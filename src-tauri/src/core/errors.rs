//! The page shown in a tab when a site can't be opened, with the reason in plain words.

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// (headline, explanation) for an engine error. `host` is already escaped.
fn reason(domain: &str, code: i64, host: &str) -> (String, String) {
    let r = |h: &str, e: &str| (h.to_string(), e.to_string());
    match (domain, code) {
        ("WebKitErrorDomain", 104) => r("R-Browser blocked this page",
            "It's on the ad and tracker block lists. If you trust it, click the shield in the address bar and switch blocking off for this site."),
        (_, -1003) => r("This site's address wasn't found",
            &format!("No server answers to <b>{host}</b>. Check the spelling, or the site may be gone. If other sites work, your network's DNS may be blocking it.")),
        (_, -1009) => r("You're offline", "This Mac isn't connected to the internet. Check Wi-Fi or your network cable."),
        (_, -1001) => r("The site took too long to answer", &format!("<b>{host}</b> didn't reply in time. It may be down or very slow, or your connection is unstable.")),
        (_, -1004) => r("The site refused to connect", &format!("<b>{host}</b> is reachable but isn't accepting connections. It may be down, or a firewall is blocking it.")),
        (_, -1005) => r("The connection was cut off",
            &format!("The connection to <b>{host}</b> was dropped. If this happens every time for this site while other sites work, your network (college or office Wi-Fi, internet provider or firewall) is probably blocking it. Mobile data or a different network will tell you.")),
        (_, -1206..=-1200) => r("This site's security certificate isn't trusted",
            &format!("R-Browser can't confirm this really is <b>{host}</b>. Something between you and the site — a network filter, antivirus, firewall, school or office network — may be intercepting the connection, or the site's certificate is wrong or expired. Don't enter passwords here.")),
        (_, -1002) => r("This kind of address can't be opened", "R-Browser opens web addresses (https://…). This link uses something else."),
        (_, -1022) => r("This page isn't encrypted", "The site only offers an unencrypted connection, which this Mac won't allow here."),
        _ => r("This site can't be opened", "The page failed to load."),
    }
}

/// HTML for a failed load, or None when nothing should be shown (load cancelled, or it turned into a download).
pub fn error_page(url: &str, domain: &str, code: i64, desc: &str) -> Option<String> {
    if code == -999 || (domain == "WebKitErrorDomain" && code == 102) { return None; }
    let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_else(|| url.to_string());
    let (head, why) = reason(domain, code, &esc(&host));
    let web = url.starts_with("https://") || url.starts_with("http://");
    let retry = if web { format!("<a class=\"btn\" href=\"{}\">Try again</a>", esc(url)) } else { String::new() };
    // Like Chrome: a connection error (offline, dropped, timed out) retries by itself once the network is back,
    // and once right away if the Mac is already online (a blip, e.g. just after waking) — at most every 30 s.
    let auto = if web && matches!(code, -1009 | -1005 | -1001) {
        let u = serde_json::to_string(url).unwrap_or_default().replace('<', "\\u003c");
        format!(r#"<script>(()=>{{const u={u},go=()=>location.replace(u);addEventListener("online",go);
try{{const k="rb-retry",t=+sessionStorage.getItem(k)||0;if(navigator.onLine&&Date.now()-t>30000){{sessionStorage.setItem(k,Date.now());setTimeout(go,3000)}}}}catch{{}}}})()</script>"#)
    } else { String::new() };
    Some(format!(r#"<!doctype html><meta charset="utf-8"><meta name="color-scheme" content="dark"><title>{t}</title>
<style>html{{background:#070b14;color:#dce6f5;font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}}
body{{max-width:600px;margin:14vh auto;padding:0 24px}}h1{{font-size:24px;font-weight:600;margin:0 0 12px}}
p{{color:#b9c6da;margin:0 0 22px}}b{{color:#dce6f5}}code{{color:#7d8ba3;font-size:12.5px;word-break:break-all}}
.btn{{display:inline-block;padding:8px 20px;border-radius:18px;background:#3b9eff;color:#04101f;font-weight:600;text-decoration:none}}
.mark{{width:44px;height:44px;margin-bottom:20px;color:#f5b54a}}</style>
<svg class="mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l7 3v5c0 4.5-3 8.3-7 10-4-1.7-7-5.5-7-10V6z"/><path d="M12 8v4M12 16h.01"/></svg>
<h1>{h}</h1><p>{why}</p>{retry}
<p style="margin-top:28px"><code>{u}<br>{d} {c}: {desc}</code></p>{auto}"#,
        t = esc(&head), h = esc(&head), u = esc(url), d = esc(domain), c = code, desc = esc(desc)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = "NSURLErrorDomain";

    #[test]
    fn cancelled_and_interrupted_loads_show_nothing() {
        assert!(error_page("https://a.com/", NS, -999, "cancelled").is_none()); // user clicked away
        assert!(error_page("https://a.com/f.zip", "WebKitErrorDomain", 102, "Frame load interrupted").is_none()); // became a download
    }

    #[test]
    fn unknown_host_explains_address_not_found() {
        let p = error_page("https://no-such-site.example/", NS, -1003, "A server with the specified hostname could not be found.").unwrap();
        assert!(p.contains("no-such-site.example") && p.contains("address"), "{p}");
    }

    #[test]
    fn certificate_error_mentions_interception() {
        let p = error_page("https://bank.com/", NS, -1202, "The certificate for this server is invalid.").unwrap();
        assert!(p.contains("certificate") && p.contains("intercept"), "{p}");
    }

    #[test]
    fn content_blocker_says_r_browser_blocked_it() {
        let p = error_page("https://ads.example/", "WebKitErrorDomain", 104, "Frame load blocked by content blocker").unwrap();
        assert!(p.contains("R-Browser blocked") && p.contains("shield"), "{p}");
    }

    #[test]
    fn cut_connection_mentions_network_blocking() {
        let p = error_page("https://www.minecraft.net/en-us", NS, -1005, "The network connection was lost.").unwrap();
        assert!(p.contains("blocking"), "{p}");
    }

    #[test]
    fn connection_errors_retry_by_themselves() {
        let p = error_page("https://www.youtube.com/watch?v=x</script>", NS, -1009, "The Internet connection appears to be offline.").unwrap();
        assert!(p.contains("addEventListener(\"online\"") && !p.contains("x</script>"), "{p}");
        assert!(!error_page("https://a.com/", NS, -1003, "").unwrap().contains("<script>")); // wrong address: retrying won't help
    }

    #[test]
    fn page_text_is_escaped() {
        let p = error_page("https://a.com/<script>alert(1)</script>", NS, -1, "<img src=x onerror=alert(1)>").unwrap();
        assert!(!p.contains("<script>alert") && !p.contains("<img"), "{p}");
    }

    #[test]
    fn try_again_link_only_for_web_urls() {
        assert!(error_page("https://a.com/", NS, -1004, "").unwrap().contains("href=\"https://a.com/\""));
        assert!(!error_page("javascript:alert(1)", NS, -1004, "").unwrap().contains("href=\"javascript"));
    }
}
