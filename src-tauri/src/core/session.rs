use crate::core::tabs::{is_newtab, Group, GroupId, TabList};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SavedTab {
    pub url: String,
    pub title: String,
    pub pinned: bool,
    pub scroll_y: f64,
    pub group: Option<GroupId>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)] // older session files have no groups
pub struct Session {
    pub tabs: Vec<SavedTab>,
    pub active: usize,
    pub groups: Vec<Group>,
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
                group: t.group,
            })
            .collect();
        Session { tabs, active, groups: list.groups.clone() }
    }

    /// Recreate the saved tabs (all frozen) and groups in `list`; returns the new tab ids in order.
    pub fn restore_into(&self, list: &mut TabList) -> Vec<crate::core::tabs::TabId> {
        let map: std::collections::HashMap<GroupId, GroupId> =
            self.groups.iter().map(|g| (g.id, list.add_group(&g.name, Some(&g.color), g.collapsed))).collect();
        let ids = self.tabs.iter().map(|t| {
            let id = list.open(&t.url, false);
            let tab = list.get_mut(id).unwrap();
            tab.title = t.title.clone();
            tab.pinned = t.pinned;
            tab.scroll_y = t.scroll_y;
            tab.frozen = true;
            tab.group = t.group.and_then(|g| map.get(&g).copied());
            id
        }).collect();
        list.prune_groups();
        ids
    }
}

pub use crate::core::store::{load, save_atomic};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
        let s = Session { tabs: vec![SavedTab { url: "https://a.com/".into(), title: "A".into(), pinned: true, scroll_y: 120.0, group: None }], active: 0, groups: vec![] };
        save_atomic(&p, &s).unwrap();
        assert_eq!(load(&p), Some(s));
    }

    #[test]
    fn session_roundtrips_groups() {
        let mut l = TabList::default();
        let a = l.open("https://a.com/", false);
        let b = l.open("https://b.com/", false);
        let g = l.new_group(b).unwrap();
        l.group_mut(g).unwrap().name = "Work".into();
        l.set_collapsed(g, true);
        let _ = a;
        let s = Session::from_tabs(&l);
        let p = dir().join("groups.json");
        save_atomic(&p, &s).unwrap();
        let mut fresh = TabList::default();
        let ids = load::<Session>(&p).unwrap().restore_into(&mut fresh);
        let g2 = fresh.get(ids[1]).unwrap().group.unwrap();
        assert_eq!(fresh.get(ids[0]).unwrap().group, None);
        assert_eq!(fresh.groups.len(), 1);
        assert_eq!((fresh.groups[0].id, fresh.groups[0].name.as_str(), fresh.groups[0].collapsed), (g2, "Work", true));
    }

    #[test]
    fn old_session_without_groups_loads() {
        let p = dir().join("old.json");
        fs::write(&p, r#"{"tabs":[{"url":"https://a.com/","title":"A","pinned":false,"scroll_y":0.0}],"active":0}"#).unwrap();
        assert_eq!(load::<Session>(&p).unwrap().tabs.len(), 1);
    }

    #[test]
    fn load_corrupt_returns_none_and_backs_up() {
        let d = dir();
        let p = d.join("session.json");
        fs::write(&p, "{garbage").unwrap();
        assert_eq!(load::<Session>(&p), None);
        assert!(d.join("session.json.bad").exists());
        assert!(!p.exists());
    }

    #[test]
    fn load_missing_returns_none() {
        assert_eq!(load::<Session>(&dir().join("nope.json")), None);
    }
}
