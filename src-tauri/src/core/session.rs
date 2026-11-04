use crate::core::tabs::{is_newtab, TabList};
use serde::{Deserialize, Serialize};
use std::{fs, io, io::Write, path::Path};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SavedTab {
    pub url: String,
    pub title: String,
    pub pinned: bool,
    pub scroll_y: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub tabs: Vec<SavedTab>,
    pub active: usize,
}

impl Session {
    /// Snapshot of the normal (non-incognito) tabs.
    pub fn from_tabs(list: &TabList) -> Session {
        let normal: Vec<_> = list.tabs.iter().filter(|t| !t.incognito).collect();
        let active = normal.iter().position(|t| Some(t.id) == list.active).unwrap_or(0);
        let tabs = normal
            .iter()
            .map(|t| SavedTab {
                url: if is_newtab(&t.url) { String::new() } else { t.url.clone() },
                title: t.title.clone(),
                pinned: t.pinned,
                scroll_y: t.scroll_y,
            })
            .collect();
        Session { tabs, active }
    }
}

/// Write to a temp file, fsync, then rename, so a crash never leaves a half-written session.
pub fn save_atomic(path: &Path, s: &Session) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(&serde_json::to_vec(s)?)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

/// None if missing or unreadable; a corrupt file is kept as `session.json.bad` for inspection.
pub fn load(path: &Path) -> Option<Session> {
    let bytes = fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(s) => Some(s),
        Err(_) => {
            let _ = fs::rename(path, path.with_extension("json.bad"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rb-session-{}-{:?}", std::process::id(), std::thread::current().id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn incognito_tabs_not_saved() {
        let mut l = TabList::default();
        l.open("https://a.com/", false);
        l.open("https://b.com/", false);
        l.open("https://secret.com/", true);
        let s = Session::from_tabs(&l);
        assert_eq!(s.tabs.len(), 2);
        assert!(s.tabs.iter().all(|t| t.url != "https://secret.com/"));
    }

    #[test]
    fn active_index_and_newtab_blanked() {
        let mut l = TabList::default();
        l.open("https://a.com/", false);
        let b = l.open("http://127.0.0.1:1430/pages/newtab.html", false);
        l.activate(b);
        let s = Session::from_tabs(&l);
        assert_eq!(s.active, 1);
        assert_eq!(s.tabs[1].url, "");
    }

    #[test]
    fn roundtrip() {
        let p = dir().join("session.json");
        let s = Session { tabs: vec![SavedTab { url: "https://a.com/".into(), title: "A".into(), pinned: true, scroll_y: 120.0 }], active: 0 };
        save_atomic(&p, &s).unwrap();
        assert_eq!(load(&p), Some(s));
    }

    #[test]
    fn load_corrupt_returns_none_and_backs_up() {
        let d = dir();
        let p = d.join("session.json");
        fs::write(&p, "{garbage").unwrap();
        assert_eq!(load(&p), None);
        assert!(d.join("session.json.bad").exists());
        assert!(!p.exists());
    }

    #[test]
    fn load_missing_returns_none() {
        assert_eq!(load(&dir().join("nope.json")), None);
    }
}
