//! macOS: a local HTTP CONNECT relay that every tab's traffic goes through. Through a CONNECT proxy WebKit
//! can't use HTTP/3, and Apple's HTTP/3 breaks on lines with an MTU under 1500 (PPPoE fibre, common in India):
//! its 1500-byte packets are refused ("Message too long"), the QUIC connection is dropped and sites like
//! YouTube say "You're offline". HTTP/2 over TCP fits any MTU. Only CONNECT is served (web pages can't send
//! CONNECT) and only on 127.0.0.1.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static PORT: OnceLock<u16> = OnceLock::new();
/// host -> why the relay couldn't reach it. WebKit only ever reports a failed CONNECT as -1005 ("connection
/// lost"), so the error page asks here for the real reason.
static FAILS: OnceLock<Mutex<HashMap<String, (i64, Instant)>>> = OnceLock::new();

fn fails() -> &'static Mutex<HashMap<String, (i64, Instant)>> { FAILS.get_or_init(Default::default) }

/// Starts the relay (once) and returns its address for `WebviewBuilder::proxy_url`.
pub fn url() -> Option<tauri::Url> {
    let port = match PORT.get() {
        Some(p) => *p,
        None => {
            let l = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
            l.set_nonblocking(true).ok()?;
            let port = l.local_addr().ok()?.port();
            tauri::async_runtime::spawn(async move {
                let Ok(l) = TcpListener::from_std(l) else { return };
                while let Ok((c, _)) = l.accept().await { tauri::async_runtime::spawn(serve(c)); }
            });
            *PORT.get_or_init(|| port)
        }
    };
    format!("http://127.0.0.1:{port}").parse().ok()
}

/// The real failure behind WebKit's -1005 for `url`, if the relay saw one in the last 30 s.
pub fn real_code(url: &str, code: i64) -> i64 {
    if code != -1005 { return code; }
    let Some(host) = tauri::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)) else { return code };
    match fails().lock().unwrap().remove(&host) {
        Some((c, at)) if at.elapsed() < Duration::from_secs(30) => c,
        _ => code,
    }
}

/// "CONNECT host:port HTTP/1.1" -> "host:port".
fn connect_target(head: &str) -> Option<&str> {
    let mut p = head.lines().next()?.split(' ');
    let (m, t) = (p.next()?, p.next()?);
    (m == "CONNECT" && t.rsplit_once(':').is_some_and(|(h, port)| !h.is_empty() && port.parse::<u16>().is_ok())).then_some(t)
}

async fn serve(mut c: TcpStream) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") { break i + 4; }
        let n = c.read(&mut chunk).await?;
        if n == 0 || buf.len() > 16 * 1024 { return Ok(()); }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let Some(target) = connect_target(&head) else {
        return c.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    };
    let mut up = match dial(target).await {
        Ok(u) => u,
        Err(code) => {
            let host = target.rsplit_once(':').map_or(target, |(h, _)| h).trim_matches(['[', ']']).to_ascii_lowercase();
            fails().lock().unwrap().insert(host, (code, Instant::now()));
            return c.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        }
    };
    let _ = (c.set_nodelay(true), up.set_nodelay(true));
    c.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    if buf.len() > end { up.write_all(&buf[end..]).await?; }
    tokio::io::copy_bidirectional(&mut c, &mut up).await.map(|_| ())
}

/// Connects to the first address that answers; Err = the NSURLError code WebKit would have shown.
// ponytail: tries addresses one by one (4 s each), not Happy Eyeballs; race them if broken-IPv6 networks complain.
async fn dial(target: &str) -> Result<TcpStream, i64> {
    let addrs: Vec<_> = tokio::net::lookup_host(target).await.map_err(|_| if offline() { -1009 } else { -1003 })?.collect();
    let mut last = -1003;
    for a in addrs {
        match tokio::time::timeout(Duration::from_secs(4), TcpStream::connect(a)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => last = -1004,
            Ok(Err(_)) => last = if offline() { -1009 } else { -1004 },
            Err(_) => last = -1001,
        }
    }
    Err(last)
}

/// No route to the internet at all (a UDP connect sends nothing, it only asks the routing table).
fn offline() -> bool {
    let routed = |bind: &str, to: &str| std::net::UdpSocket::bind(bind).and_then(|s| s.connect(to)).is_ok();
    !routed("0.0.0.0:0", "1.1.1.1:53") && !routed("[::]:0", "[2606:4700:4700::1111]:53")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn only_connect_with_host_and_port() {
        assert_eq!(connect_target("CONNECT www.youtube.com:443 HTTP/1.1\r\nHost: x\r\n\r\n"), Some("www.youtube.com:443"));
        assert_eq!(connect_target("CONNECT [2001:db8::1]:443 HTTP/1.1\r\n\r\n"), Some("[2001:db8::1]:443"));
        assert_eq!(connect_target("GET http://a.com/ HTTP/1.1\r\n\r\n"), None);
        assert_eq!(connect_target("CONNECT a.com HTTP/1.1\r\n\r\n"), None);
        assert_eq!(connect_target("CONNECT :443 HTTP/1.1\r\n\r\n"), None);
    }

    fn ask(port: u16, req: &str) -> (std::net::TcpStream, String) {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut b = [0u8; 256];
        let n = s.read(&mut b).unwrap();
        (s, String::from_utf8_lossy(&b[..n]).into_owned())
    }

    #[test]
    fn tunnels_bytes_and_explains_failures() {
        let port = url().unwrap().port().unwrap();
        // upstream echo server
        let up = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let up_port = up.local_addr().unwrap().port();
        std::thread::spawn(move || { let (mut s, _) = up.accept().unwrap(); let mut b = [0u8; 5]; s.read_exact(&mut b).unwrap(); s.write_all(&b).unwrap(); });
        let (mut s, head) = ask(port, &format!("CONNECT 127.0.0.1:{up_port} HTTP/1.1\r\n\r\n"));
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        s.write_all(b"hello").unwrap();
        let mut b = [0u8; 5];
        s.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"hello");

        assert!(ask(port, "GET http://127.0.0.1/ HTTP/1.1\r\n\r\n").1.starts_with("HTTP/1.1 405"));

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port(); // dropped: refuses
        assert!(ask(port, &format!("CONNECT 127.0.0.1:{closed} HTTP/1.1\r\n\r\n")).1.starts_with("HTTP/1.1 502"));
        assert_eq!(real_code("https://127.0.0.1/x", -1005), -1004);
        assert_eq!(real_code("https://127.0.0.1/x", -1005), -1005); // used once
        assert_eq!(real_code("https://a.com/", -1202), -1202);

        assert!(ask(port, "CONNECT no-such-host.invalid:443 HTTP/1.1\r\n\r\n").1.starts_with("HTTP/1.1 502"));
        assert_eq!(real_code("https://no-such-host.invalid/", -1005), -1003);
    }
}
