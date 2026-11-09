use url::{form_urlencoded, Url};

/// Settings page search engines: (key, name, query prefix).
pub const ENGINES: [(&str, &str, &str); 4] = [
    ("google", "Google", "https://www.google.com/search?q="),
    ("duckduckgo", "DuckDuckGo", "https://duckduckgo.com/?q="),
    ("bing", "Bing", "https://www.bing.com/search?q="),
    ("brave", "Brave Search", "https://search.brave.com/search?q="),
];

/// Query prefix for an engine key; unknown keys get Google.
pub fn search_url(engine: &str) -> &'static str { ENGINES.iter().find(|e| e.0 == engine).unwrap_or(&ENGINES[0]).2 }

/// Turn whatever the user typed into the URL bar into a URL to load; searches go to `engine`.
pub fn resolve(input: &str, engine: &str) -> String {
    let s = input.trim();
    if s.starts_with("rb://") {
        return s.to_string();
    }
    if let Some((scheme, _)) = s.split_once("://") {
        if matches!(scheme, "http" | "https") {
            if let Ok(u) = Url::parse(s) {
                return u.to_string();
            }
        }
    }
    if !s.is_empty() && !s.contains(char::is_whitespace) {
        let host = s.split(['/', '?', '#']).next().unwrap_or("");
        let hostname = host.split(':').next().unwrap_or("");
        let local = hostname == "localhost" || hostname.parse::<std::net::Ipv4Addr>().is_ok();
        let domainish = hostname.contains('.') && !hostname.starts_with('.') && !hostname.ends_with('.');
        if local || domainish {
            let scheme = if local { "http" } else { "https" };
            if let Ok(u) = Url::parse(&format!("{scheme}://{s}")) {
                return u.to_string();
            }
        }
    }
    let q: String = form_urlencoded::byte_serialize(s.as_bytes()).collect();
    format!("{}{q}", search_url(engine))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn bare_domain_gets_https() { assert_eq!(resolve("example.com", "google"), "https://example.com/"); }
    #[test] fn words_become_search() { assert_eq!(resolve("hello world", "google"), "https://www.google.com/search?q=hello+world"); }
    #[test] fn full_url_kept() { assert_eq!(resolve("http://a.com/x", "google"), "http://a.com/x"); }
    #[test] fn localhost_port() { assert_eq!(resolve("localhost:3000", "google"), "http://localhost:3000/"); }
    #[test] fn rb_scheme_kept() { assert_eq!(resolve("rb://settings", "google"), "rb://settings"); }
    #[test] fn engine_choice_and_unknown_falls_back() {
        assert_eq!(resolve("rust lang", "duckduckgo"), "https://duckduckgo.com/?q=rust+lang");
        assert_eq!(resolve("rust", "nope"), "https://www.google.com/search?q=rust");
        assert_eq!(resolve("example.com", "bing"), "https://example.com/");
    }
    #[test] fn single_word_is_search() { assert_eq!(resolve("rust", "google"), "https://www.google.com/search?q=rust"); }
}
