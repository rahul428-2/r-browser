//! Verified vs unverified sites: a site is verified when its registrable domain is one of the most-visited
//! sites (Tranco top 10k, bundled) or one the user chose to trust. Everything else gets the ⚠.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust { Verified, Unverified }

pub struct TrustDb {
    top: HashSet<String>,
    user: HashSet<String>,
}

/// The part of a host someone actually registered: `mail.google.com` -> `google.com`, `evil.github.io` ->
/// `evil.github.io` (github.io is a public suffix, so every subdomain is a different owner). Hosts are compared
/// in their punycode (xn--) form, so a lookalike in another alphabet never equals the real name.
pub fn registrable(host: &str) -> Option<String> {
    psl::domain_str(&host.to_ascii_lowercase()).map(String::from)
}

/// What a site is trusted under: the exact address for an IP, the registrable domain for a name. A `blob:`
/// URL counts as the site that made it. None for anything that isn't a web page.
pub fn site_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    if u.scheme() == "blob" { return site_of(u.path()); }
    if !matches!(u.scheme(), "http" | "https") { return None; }
    match u.host()? {
        url::Host::Domain(d) => registrable(d),
        ip => Some(ip.to_string()),
    }
}

impl TrustDb {
    /// `top`: one domain per line (the bundled Tranco list). `user`: domains the user trusted.
    pub fn new(top: &str, user: impl IntoIterator<Item = String>) -> TrustDb {
        TrustDb { top: top.lines().map(|l| l.trim().to_ascii_lowercase()).filter(|l| !l.is_empty()).collect(), user: user.into_iter().collect() }
    }

    /// Fails closed: only the new tab, the browser's own pages and known web sites are Verified; data:, file:,
    /// unknown schemes and anything unparsable are Unverified.
    pub fn classify(&self, url: &str) -> Trust {
        if url.is_empty() || url == "about:blank" || url.starts_with("rb://") || crate::core::tabs::is_app_page(url) { return Trust::Verified; }
        let Ok(u) = url::Url::parse(url) else { return Trust::Unverified };
        if u.scheme() == "blob" { return self.classify(u.path()); }
        if !matches!(u.scheme(), "http" | "https") { return Trust::Unverified; }
        if u.host().is_some_and(|h| crate::core::https::local(&h)) { return Trust::Verified; }
        match site_of(url) {
            Some(d) if self.top.contains(&d) || self.user.contains(&d) => Trust::Verified,
            _ => Trust::Unverified,
        }
    }

    /// Trust `url`'s site from now on; returns the domain that was added.
    pub fn trust(&mut self, url: &str) -> Option<String> {
        let d = site_of(url)?;
        self.user.insert(d.clone());
        Some(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> TrustDb { TrustDb::new("google.com\ngithub.com\nwikipedia.org\n", ["klh.edu.in".to_string()]) }

    #[test]
    fn top_domain_verified() {
        assert_eq!(db().classify("https://mail.google.com/x"), Trust::Verified);
        assert_eq!(db().classify("https://en.wikipedia.org/wiki/Rust"), Trust::Verified);
    }

    #[test]
    fn unknown_unverified() {
        assert_eq!(db().classify("https://some-small-blog.net/post"), Trust::Unverified);
    }

    #[test]
    fn user_trusted_verified() {
        assert_eq!(db().classify("https://erp.klh.edu.in/login"), Trust::Verified);
    }

    #[test]
    fn idn_lookalike_is_unverified() {
        assert_eq!(db().classify("https://gооgle.com/"), Trust::Unverified); // Cyrillic о
        assert_eq!(db().classify("https://xn--ggle-55da.com/"), Trust::Unverified);
    }

    #[test]
    fn psl_private_suffix_not_verified() {
        assert_eq!(db().classify("https://evil.github.io/"), Trust::Unverified); // github.io is its own suffix
        assert_eq!(registrable("evil.github.io").as_deref(), Some("evil.github.io"));
        assert_eq!(registrable("a.b.google.co.uk").as_deref(), Some("google.co.uk"));
    }

    #[test]
    fn localhost_and_own_pages_verified() {
        for u in ["http://localhost:3000/", "http://192.168.1.1/", "rb://history", "", "about:blank", "tauri://localhost/pages/newtab.html"] {
            assert_eq!(db().classify(u), Trust::Verified, "{u}");
        }
    }

    #[test]
    fn trust_adds_registrable_domain() {
        let mut d = db();
        assert_eq!(d.trust("https://www.some-small-blog.net/a").as_deref(), Some("some-small-blog.net"));
        assert_eq!(d.classify("https://cdn.some-small-blog.net/"), Trust::Verified);
    }

    #[test]
    fn trusting_an_ip_trusts_only_that_ip() {
        let mut d = db();
        assert_eq!(d.trust("http://203.0.113.7/admin").as_deref(), Some("203.0.113.7"));
        assert_eq!(d.classify("http://203.0.113.7/x"), Trust::Verified);
        assert_eq!(d.classify("http://198.51.113.7/"), Trust::Unverified);
        assert_eq!(d.classify("http://10.113.7.1/"), Trust::Verified); // private network, always fine
    }

    #[test]
    fn unknown_schemes_fail_closed() {
        for u in ["data:text/html,<h1>Sign in to Google</h1>", "file:///Users/x/Downloads/invoice.html", "javascript:alert(1)", "ftp://x.com/", "not a url"] {
            assert_eq!(db().classify(u), Trust::Unverified, "{u}");
        }
    }

    #[test]
    fn blob_url_uses_its_real_site() {
        assert_eq!(db().classify("blob:https://evil-login.net/1b2c"), Trust::Unverified);
        assert_eq!(db().classify("blob:https://mail.google.com/1b2c"), Trust::Verified);
    }
}
