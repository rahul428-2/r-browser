use crate::core::tabs::is_newtab;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const HISTORY_CAP: usize = 50_000; // ponytail: whole-file JSON rewrite; move to SQLite if saves get slow
pub const DOWNLOADS_CAP: usize = 1_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Bookmark { pub url: String, pub title: String, pub added: u64 }

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Visit { pub url: String, pub title: String, pub at: u64 }

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Download { pub url: String, pub path: String, pub at: u64, pub done: bool, pub ok: bool }

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Suggestion { pub url: String, pub title: String, pub bookmarked: bool }

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Library {
    pub bookmarks: Vec<Bookmark>,
    pub history: Vec<Visit>,
    pub downloads: Vec<Download>,
}

fn matches(q: &str, url: &str, title: &str) -> bool {
    q.is_empty() || url.to_lowercase().contains(q) || title.to_lowercase().contains(q)
}

impl Library {
    /// Incognito and new-tab visits are never recorded.
    pub fn record_visit(&mut self, url: &str, title: &str, at: u64, incognito: bool) {
        if incognito || is_newtab(url) || !(url.starts_with("http://") || url.starts_with("https://")) { return; }
        self.history.push(Visit { url: url.into(), title: title.into(), at });
        if self.history.len() > HISTORY_CAP {
            let extra = self.history.len() - HISTORY_CAP;
            self.history.drain(..extra);
        }
    }

    /// Titles arrive after the page starts loading: fill in the newest visit of that URL.
    pub fn set_title(&mut self, url: &str, title: &str) {
        if let Some(v) = self.history.iter_mut().rev().find(|v| v.url == url) { v.title = title.into(); }
        if let Some(b) = self.bookmarks.iter_mut().find(|b| b.url == url && b.title.is_empty()) { b.title = title.into(); }
    }

    /// Newest first, case-insensitive on URL and title.
    pub fn search_history(&self, q: &str, limit: usize) -> Vec<&Visit> {
        let q = q.trim().to_lowercase();
        self.history.iter().rev().filter(|v| matches(&q, &v.url, &v.title)).take(limit).collect()
    }

    pub fn clear_history(&mut self) { self.history.clear(); }

    pub fn is_bookmarked(&self, url: &str) -> bool { self.bookmarks.iter().any(|b| b.url == url) }

    /// Returns true if the URL is bookmarked afterwards.
    pub fn toggle_bookmark(&mut self, url: &str, title: &str, at: u64) -> bool {
        if let Some(i) = self.bookmarks.iter().position(|b| b.url == url) {
            self.bookmarks.remove(i);
            false
        } else {
            self.bookmarks.push(Bookmark { url: url.into(), title: title.into(), added: at });
            true
        }
    }

    /// Address-bar suggestions: bookmarks first, then history, one entry per URL.
    pub fn suggest(&self, q: &str, limit: usize) -> Vec<Suggestion> {
        let q = q.trim().to_lowercase();
        if q.is_empty() { return vec![]; }
        let mut out: Vec<Suggestion> = vec![];
        let marks = self.bookmarks.iter().rev().map(|b| (&b.url, &b.title, true));
        let hist = self.history.iter().rev().map(|v| (&v.url, &v.title, false));
        for (url, title, bookmarked) in marks.chain(hist) {
            if out.len() >= limit { break; }
            if matches(&q, url, title) && !out.iter().any(|s| &s.url == url) {
                out.push(Suggestion { url: url.clone(), title: title.clone(), bookmarked });
            }
        }
        out
    }

    pub fn add_download(&mut self, d: Download) {
        self.downloads.push(d);
        if self.downloads.len() > DOWNLOADS_CAP { self.downloads.remove(0); }
    }
}

/// Make a server-suggested filename safe on every OS: no path parts, no characters that are
/// reserved on Windows or that could break a command line (`,` `"` etc.), no control chars,
/// no Windows device names, no trailing dots/spaces.
pub fn safe_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut out: String = base
        .chars()
        .map(|c| if c.is_control() || "<>:\"|?*,;".contains(c) { '_' } else { c })
        .collect();
    out = out.trim_end_matches(['.', ' ']).trim_start_matches(['.', ' ']).to_string();
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if reserved { out = format!("_{out}"); }
    if out.is_empty() { "download".into() } else { out.chars().take(200).collect() }
}

/// `dir/name`, or `dir/name (1).ext`, `(2)`... if taken. The name is cleaned with `safe_filename`.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let name = safe_filename(name);
    let first = dir.join(&name);
    if !first.exists() { return first; }
    let p = Path::new(&name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(&name);
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    (1..).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|c| !c.exists()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incognito_visit_not_recorded() {
        let mut l = Library::default();
        l.record_visit("https://a.com/", "A", 1, true);
        l.record_visit("http://127.0.0.1:1430/pages/newtab.html", "New tab", 2, false);
        assert!(l.history.is_empty());
    }

    #[test]
    fn search_history_newest_first() {
        let mut l = Library::default();
        l.record_visit("https://rust-lang.org/", "Rust", 1, false);
        l.record_visit("https://example.com/", "Example", 2, false);
        l.record_visit("https://doc.rust-lang.org/", "Docs", 3, false);
        let r: Vec<_> = l.search_history("RUST", 10).iter().map(|v| v.at).collect();
        assert_eq!(r, vec![3, 1]);
    }

    #[test]
    fn history_capped_at_50000() {
        let mut l = Library::default();
        for i in 0..HISTORY_CAP as u64 + 5 { l.record_visit("https://a.com/", "", i, false); }
        assert_eq!(l.history.len(), HISTORY_CAP);
        assert_eq!(l.history[0].at, 5);
    }

    #[test]
    fn bookmark_toggle() {
        let mut l = Library::default();
        assert!(l.toggle_bookmark("https://a.com/", "A", 1));
        assert!(l.is_bookmarked("https://a.com/"));
        assert!(!l.toggle_bookmark("https://a.com/", "A", 2));
        assert!(!l.is_bookmarked("https://a.com/"));
    }

    #[test]
    fn suggest_bookmarks_first_and_deduped() {
        let mut l = Library::default();
        l.record_visit("https://github.com/", "GitHub", 1, false);
        l.record_visit("https://github.com/", "GitHub", 2, false);
        l.record_visit("https://gist.github.com/", "Gist", 3, false);
        l.toggle_bookmark("https://github.com/", "GitHub", 4);
        let s = l.suggest("git", 5);
        assert_eq!(s.iter().map(|s| s.url.as_str()).collect::<Vec<_>>(), vec!["https://github.com/", "https://gist.github.com/"]);
        assert!(s[0].bookmarked);
    }

    #[test]
    fn safe_filename_cleans_dangerous_names() {
        assert_eq!(safe_filename("report.pdf"), "report.pdf");
        assert_eq!(safe_filename("a,/select,C:\\Windows\\x.exe"), "x.exe");
        assert_eq!(safe_filename("evil\",name.exe"), "evil__name.exe");
        assert_eq!(safe_filename("file.txt:Zone.Identifier"), "file.txt_Zone.Identifier");
        assert_eq!(safe_filename("CON.txt"), "_CON.txt");
        assert_eq!(safe_filename("...hidden. "), "hidden");
        assert_eq!(safe_filename("a\u{0}b\nc"), "a_b_c");
        assert_eq!(safe_filename(".."), "download");
    }

    #[test]
    fn unique_path_adds_counter_and_strips_dirs() {
        let d = std::env::temp_dir().join(format!("rb-dl-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.pdf"), "x").unwrap();
        assert_eq!(unique_path(&d, "a.pdf"), d.join("a (1).pdf"));
        assert_eq!(unique_path(&d, "../../etc/passwd"), d.join("passwd"));
        assert_eq!(unique_path(&d, ""), d.join("download"));
    }
}
