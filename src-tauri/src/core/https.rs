//! HTTPS-only mode: plain-http addresses are tried over https first.

use std::collections::HashSet;

/// Home networks, this machine and intranet names: these rarely have https and never need it to be safe.
pub(crate) fn local(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Ipv4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        url::Host::Ipv6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00 || (ip.segments()[0] & 0xffc0) == 0xfe80,
        url::Host::Domain(d) => !d.contains('.') || [".local", ".localhost", ".internal", ".lan", ".home.arpa"].iter().any(|s| d.ends_with(s)),
    }
}

/// The https address to try instead of `url`, or None to load it as it is: already https / not web, a local
/// address, an explicit port, or a host the user chose to open insecurely (`allowed`) this session.
pub fn upgrade(url: &str, allowed: &HashSet<String>) -> Option<String> {
    let mut u = url::Url::parse(url).ok()?;
    if u.scheme() != "http" || u.port().is_some() { return None; }
    let host = u.host()?;
    if local(&host) || allowed.contains(&host.to_string()) { return None; }
    u.set_scheme("https").ok()?;
    Some(u.to_string())
}

/// The "this site has no secure version" page for `http_url`.
pub fn insecure_page(http_url: &str) -> String {
    format!("rb://insecure?url={}", url::form_urlencoded::byte_serialize(http_url.as_bytes()).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn none() -> HashSet<String> { HashSet::new() }

    #[test]
    fn upgrades_http() {
        assert_eq!(upgrade("http://example.com/a?b=1#c", &none()).as_deref(), Some("https://example.com/a?b=1#c"));
        assert_eq!(upgrade("http://Example.COM", &none()).as_deref(), Some("https://example.com/"));
    }

    #[test]
    fn keeps_https() {
        assert_eq!(upgrade("https://example.com/", &none()), None);
        assert_eq!(upgrade("rb://history", &none()), None);
        assert_eq!(upgrade("about:blank", &none()), None);
    }

    #[test]
    fn skips_localhost_and_10_x() {
        for u in ["http://localhost:1430/pages/newtab.html", "http://127.0.0.1:1430/", "http://10.0.0.5/", "http://192.168.1.1/",
                  "http://172.16.0.1/", "http://169.254.1.1/", "http://[::1]/", "http://router.local/", "http://printer/", "http://app.localhost/"] {
            assert_eq!(upgrade(u, &none()), None, "{u}");
        }
        assert!(upgrade("http://172.32.0.1/", &none()).is_some()); // public, outside 172.16/12
    }

    #[test]
    fn skips_explicit_port() {
        assert_eq!(upgrade("http://example.com:8080/", &none()), None); // https on a custom port almost never exists
    }

    #[test]
    fn skips_allowed_host() {
        let ok: HashSet<String> = ["neverssl.com".to_string()].into();
        assert_eq!(upgrade("http://neverssl.com/x", &ok), None);
        assert!(upgrade("http://other.com/", &ok).is_some());
    }

    #[test]
    fn insecure_page_round_trips_url() {
        let p = insecure_page("http://a.com/x?y=1&z=2");
        assert!(p.starts_with("rb://insecure?url="), "{p}");
        assert!(!p["rb://insecure?url=".len()..].contains('&'), "{p}");
        assert_eq!(crate::core::tabs::rb_page(&p).as_deref(), Some(&*format!("pages/insecure.html?{}", p.split_once('?').unwrap().1)));
    }
}
