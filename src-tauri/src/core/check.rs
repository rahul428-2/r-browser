//! One-time safety check for an unverified site (Rahul's design, 5 Nov): it opens locked down (JIT off) and
//! is checked; if it passes, it's remembered as safe and never checked again. These checks catch known-bad
//! and lookalike sites and pushy behaviour — they cannot see a new (zero-day) attack.

use std::collections::HashSet;

/// Why a site didn't pass (shown in the ⚠ menu).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail { NotHttps, Idn, Lookalike(String), Listed, Popups, Redirects }

impl Fail {
    pub fn reason(&self) -> String {
        match self {
            Fail::NotHttps => "it isn't encrypted (http), so what it sends can be changed on the way".into(),
            Fail::Idn => "its address uses letters from another alphabet, a common lookalike trick".into(),
            Fail::Lookalike(d) => format!("its address looks like {d} but isn't it"),
            Fail::Listed => "it's on a list of ad, tracker or crypto-miner servers".into(),
            Fail::Popups => "it tried to open pop-ups by itself".into(),
            Fail::Redirects => "it sent you to another site by itself".into(),
        }
    }
}

pub struct Checker {
    /// (name without the public suffix, full domain) for well-known sites, most popular first.
    top: Vec<(String, String)>,
    /// Domains on the malware / miner / ad-server domain lists.
    listed: HashSet<String>,
    known: HashSet<String>,
}

/// `paypal.com` -> `paypal`, `amazon.co.in` -> `amazon`.
fn name_of(domain: &str) -> Option<&str> {
    let suffix = psl::suffix_str(domain)?;
    domain.strip_suffix(suffix)?.strip_suffix('.')
}

/// Levenshtein distance, stopping early once it's over 1 (all we need).
fn within_one(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 1 { return false; }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()] <= 1
}

impl Checker {
    /// `top`: well-known domains, most popular first. `lists`: ABP-style domain lists (`||host^`).
    pub fn new(top: &[String], lists: &str) -> Checker {
        let known = top.iter().cloned().collect();
        let top: Vec<(String, String)> = top.iter().filter_map(|d| Some((name_of(d)?.to_string(), d.clone()))).filter(|(n, _)| n.len() >= 4).collect();
        let listed = lists.lines().filter_map(|l| l.strip_prefix("||")).map(|l| l.split(['^', '/', '$']).next().unwrap_or(""))
            .filter(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')).map(str::to_ascii_lowercase).collect();
        Checker { top, listed, known }
    }

    /// The checks that only need the address. Err says which one failed.
    pub fn domain_checks(&self, url: &str) -> Result<(), Fail> {
        let u = url::Url::parse(url).map_err(|_| Fail::NotHttps)?;
        if u.scheme() != "https" { return Err(Fail::NotHttps); }
        // "coinminer.net." is the same host as "coinminer.net": compare without the trailing dot.
        let host = u.host_str().ok_or(Fail::NotHttps)?.trim_end_matches('.').to_ascii_lowercase();
        if host.split('.').any(|l| l.starts_with("xn--")) { return Err(Fail::Idn); }
        let mut h = host.as_str();
        loop {
            if self.listed.contains(h) { return Err(Fail::Listed); }
            match h.split_once('.') { Some((_, rest)) => h = rest, None => break }
        }
        let Some(reg) = crate::core::trust::registrable(&host) else { return Ok(()) };
        if self.known.contains(&reg) { return Ok(()); } // a real well-known site, even if it resembles another
        let Some(name) = name_of(&reg) else { return Ok(()) };
        for (tn, td) in &self.top {
            let typo = tn.len() >= 5 && name != tn && within_one(name, tn);
            let word = name.split('-').any(|w| w == tn) && name != tn;
            let prefix = host.starts_with(&format!("{td}.")) || host.contains(&format!(".{td}."));
            if typo || word || prefix { return Err(Fail::Lookalike(td.clone())); }
        }
        Ok(())
    }
}

/// What the site did during the watched first visit.
#[derive(Default, Clone, Copy, Debug)]
pub struct Visit { pub popups: u32, pub redirects: u32 }

/// Seconds of good behaviour needed before a site passes.
pub const WATCH_SECS: u64 = 30;

impl Visit {
    /// None while still watching; a bad sign fails at once.
    pub fn verdict(&self, secs: u64) -> Option<Result<(), Fail>> {
        if self.redirects > 0 { return Some(Err(Fail::Redirects)); }
        if self.popups > 0 { return Some(Err(Fail::Popups)); }
        (secs >= WATCH_SECS).then_some(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top() -> Vec<String> { ["google.com", "paypal.com", "github.com", "amazon.in", "wikipedia.org", "wikimedia.org"].map(String::from).to_vec() }
    fn lists() -> String { "! comment\n||adserver.example^\n||coinminer.net^$third-party\n||1.2.3.4^$third-party\n".into() }

    #[test]
    fn plain_https_site_passes() {
        let c = Checker::new(&top(), &lists());
        assert_eq!(c.domain_checks("https://jvns.ca/"), Ok(()));
        assert_eq!(c.domain_checks("https://klh.edu.in/erp"), Ok(()));
    }

    #[test]
    fn http_fails() {
        assert_eq!(Checker::new(&top(), &lists()).domain_checks("http://jvns.ca/"), Err(Fail::NotHttps));
    }

    #[test]
    fn idn_fails() {
        assert_eq!(Checker::new(&top(), &lists()).domain_checks("https://gооgle.com/"), Err(Fail::Idn));
    }

    #[test]
    fn lookalikes_fail() {
        let c = Checker::new(&top(), &lists());
        for (u, real) in [("https://paypa1.com/", "paypal.com"), ("https://gooogle.com/", "google.com"), ("https://githb.com/", "github.com"),
                          ("https://paypal-login.com/", "paypal.com"), ("https://secure-amazon.in/", "amazon.in"), ("https://paypal.com.verify-account.net/", "paypal.com")] {
            assert_eq!(c.domain_checks(u), Err(Fail::Lookalike(real.into())), "{u}");
        }
        assert_eq!(c.domain_checks("https://wikihow.com/"), Ok(())); // a different word, not a typo
    }

    #[test]
    fn real_top_site_never_a_lookalike() {
        // wikimedia is one letter from wikipedia, but it's a real top site itself
        assert_eq!(Checker::new(&top(), &lists()).domain_checks("https://commons.wikimedia.org/"), Ok(()));
    }

    #[test]
    fn listed_domain_fails() {
        let c = Checker::new(&top(), &lists());
        assert_eq!(c.domain_checks("https://cdn.coinminer.net/"), Err(Fail::Listed));
        assert_eq!(c.domain_checks("https://adserver.example/x"), Err(Fail::Listed));
    }

    #[test]
    fn trailing_dot_is_the_same_host() {
        let c = Checker::new(&top(), &lists());
        assert_eq!(c.domain_checks("https://cdn.coinminer.net./"), Err(Fail::Listed));
        assert_eq!(c.domain_checks("https://paypa1.com./"), Err(Fail::Lookalike("paypal.com".into())));
        assert_eq!(crate::core::trust::site_of("https://jvns.ca./x").as_deref(), Some("jvns.ca"));
    }

    #[test]
    fn behaviour_verdict_after_30s() {
        let mut v = Visit::default();
        assert_eq!(v.verdict(10), None); // still watching
        assert_eq!(v.verdict(30), Some(Ok(())));
        v.popups = 1;
        assert_eq!(v.verdict(30), Some(Err(Fail::Popups)));
        let v = Visit { redirects: 1, ..Visit::default() };
        assert_eq!(v.verdict(5), Some(Err(Fail::Redirects))); // fails at once, no need to wait
    }
}

#[cfg(test)]
mod real_lists {
    use super::*;

    /// Real small sites must pass with the bundled lists (no false "lookalike" alarms).
    #[test]
    fn rahuls_sites_pass_with_real_lists() {
        let top: Vec<String> = include_str!("../../resources/top10k.txt").lines().map(String::from).collect();
        let lists = [include_str!("../../resources/lists/peterlowe.txt"), include_str!("../../resources/lists/nocoin.txt")].concat();
        let c = Checker::new(&top, &lists);
        let mut bad = vec![];
        for u in ["https://kluniversity.in/", "https://erp.kluniversity.in/", "https://klh.edu.in/", "https://upstox.com/", "https://nivirafashions.com/",
                  "https://rvinfradevelopers.com/", "https://jvns.ca/", "https://danluu.com/", "https://httpforever.com/", "https://tauri.app/",
                  "https://docs.rs/", "https://crates.io/", "https://news.ycombinator.com/", "https://zerodha.com/", "https://irctc.co.in/"] {
            if let Err(f) = c.domain_checks(u) { bad.push(format!("{u}: {f:?}")); }
        }
        assert!(bad.is_empty(), "false alarms:\n{}", bad.join("\n"));
        assert_eq!(c.domain_checks("https://paypa1.com/"), Err(Fail::Lookalike("paypal.com".into())));
    }
}
