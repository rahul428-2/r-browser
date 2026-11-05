//! Ad, tracker and miner blocking on Brave's adblock-rust (EasyList-style filter lists).

use adblock::lists::{FilterSet, ParseOptions};
use adblock::request::Request;
use adblock::Engine;

pub struct Blocker {
    engine: Engine,
    lists: Vec<String>,
}

impl Blocker {
    pub fn from_lists(lists: &[String]) -> Blocker {
        Blocker { engine: Engine::new_with_filter_set(filter_set(lists, false)), lists: lists.to_vec() }
    }

    /// `request_type` is a resource type as filter lists name them: "script", "image", "xmlhttprequest", ...
    pub fn should_block(&self, url: &str, source_url: &str, request_type: &str) -> bool {
        Request::new(url, source_url, request_type, "GET").is_ok_and(|r| self.engine.check_network_request(&r).should_block())
    }

    /// Element-hiding CSS for one page (site-specific rules only; generic ones are in the WebKit rule list).
    pub fn cosmetic_css(&self, url: &str) -> String {
        let r = self.engine.url_cosmetic_resources(url);
        let mut sel: Vec<&str> = r.hide_selectors.iter().map(String::as_str).collect();
        sel.sort_unstable();
        if sel.is_empty() { String::new() } else { format!("{} {{ display: none !important; }}", sel.join(",\n")) }
    }

    /// The lists as a WebKit content-blocker rule list (WKContentRuleList / WebKitUserContentFilterStore).
    pub fn webkit_rules_json(&self) -> String {
        let (mut rules, _) = filter_set(&self.lists, true).into_content_blocking().unwrap_or_default();
        rules.push(adblock::content_blocking::ignore_previous_fp_documents());
        serde_json::to_string(&rules).unwrap_or_else(|_| "[]".into())
    }
}

/// The filter lists shipped inside the app (refresh with scripts/fetch_lists.sh).
pub fn bundled() -> Vec<String> {
    [
        include_str!("../../resources/lists/easylist.txt"),
        include_str!("../../resources/lists/easyprivacy.txt"),
        include_str!("../../resources/lists/ublock.txt"),
        include_str!("../../resources/lists/peterlowe.txt"),
        include_str!("../../resources/lists/nocoin.txt"),
    ].map(String::from).to_vec()
}

/// `debug` keeps the rule text, which the WebKit conversion needs.
fn filter_set(lists: &[String], debug: bool) -> FilterSet {
    let mut set = FilterSet::new(debug);
    for l in lists { set.add_filter_list(l.clone(), ParseOptions::default()); }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(list: &str) -> Blocker { Blocker::from_lists(&[list.to_string()]) }

    #[test]
    fn blocks_listed_third_party() {
        assert!(b("||ads.example^").should_block("https://ads.example/a.js", "https://news.com/", "script"));
    }

    #[test]
    fn allows_unlisted() {
        assert!(!b("||ads.example^").should_block("https://cdn.news.com/app.js", "https://news.com/", "script"));
    }

    #[test]
    fn exception_rule_wins() {
        let bl = b("||ads.example^\n@@||ads.example/ok.js");
        assert!(!bl.should_block("https://ads.example/ok.js", "https://news.com/", "script"));
        assert!(bl.should_block("https://ads.example/bad.js", "https://news.com/", "script"));
    }

    #[test]
    fn cosmetic_css_contains_selector() {
        let css = b("news.com##.ad-box").cosmetic_css("https://news.com/story");
        assert!(css.contains(".ad-box") && css.contains("display: none"), "{css}");
        assert!(b("news.com##.ad-box").cosmetic_css("https://other.com/").is_empty());
    }

    #[test]
    fn webkit_json_is_valid_json_array() {
        let json = b("||ads.example^\n@@||ads.example/ok.js\nnews.com##.ad-box").webkit_rules_json();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rules = v.as_array().unwrap();
        assert!(rules.iter().any(|r| r["action"]["type"] == "block"), "{json}");
        assert!(rules.iter().any(|r| r["action"]["type"] == "ignore-previous-rules"), "{json}");
        assert!(rules.iter().any(|r| r["action"]["type"] == "css-display-none"), "{json}");
    }
}

#[cfg(test)]
mod real_lists {
    use super::*;

    #[test]
    #[ignore] // stats only: cargo test --release real_lists -- --ignored --nocapture
    fn stats() {
        let t = std::time::Instant::now();
        let b = Blocker::from_lists(&bundled());
        println!("engine {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let j = b.webkit_rules_json();
        let n = serde_json::from_str::<Vec<serde_json::Value>>(&j).unwrap().len();
        println!("webkit {n} rules, {} KB, {:?}", j.len() / 1024, t.elapsed());
    }
}
