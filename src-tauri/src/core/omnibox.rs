use url::{form_urlencoded, Url};

pub const SEARCH_URL: &str = "https://www.google.com/search?q=";

/// Turn whatever the user typed into the URL bar into a URL to load.
pub fn resolve(input: &str) -> String {
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
    format!("{SEARCH_URL}{q}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn bare_domain_gets_https() { assert_eq!(resolve("example.com"), "https://example.com/"); }
    #[test] fn words_become_search() { assert_eq!(resolve("hello world"), "https://www.google.com/search?q=hello+world"); }
    #[test] fn full_url_kept() { assert_eq!(resolve("http://a.com/x"), "http://a.com/x"); }
    #[test] fn localhost_port() { assert_eq!(resolve("localhost:3000"), "http://localhost:3000/"); }
    #[test] fn rb_scheme_kept() { assert_eq!(resolve("rb://settings"), "rb://settings"); }
    #[test] fn single_word_is_search() { assert_eq!(resolve("rust"), "https://www.google.com/search?q=rust"); }
}
