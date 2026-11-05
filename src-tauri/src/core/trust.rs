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

impl TrustDb {
    /// `top`: one domain per line (the bundled Tranco list). `user`: domains the user trusted.
    pub fn new(top: &str, user: impl IntoIterator<Item = String>) -> TrustDb {
        TrustDb { top: top.lines().map(|l| l.trim().to_ascii_lowercase()).filter(|l| !l.is_empty()).collect(), user: user.into_iter().collect() }
    }

    pub fn classify(&self, url: &str) -> Trust {
        let Ok(u) = url::Url::parse(url) else { return Trust::Verified }; // new tab, nothing loaded
        if !matches!(u.scheme(), "http" | "https") { return Trust::Verified; } // rb://, about:, the app's own pages
        let Some(host) = u.host() else { return Trust::Unverified };
        if crate::core::https::local(&host) { return Trust::Verified; }
        match registrable(&host.to_string()) {
            Some(d) if self.top.contains(&d) || self.user.contains(&d) => Trust::Verified,
            _ => Trust::Unverified,
        }
    }

    /// Trust `url`'s site from now on; returns the domain that was added.
    pub fn trust(&mut self, url: &str) -> Option<String> {
        let d = registrable(url::Url::parse(url).ok()?.host_str()?)?;
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
}
