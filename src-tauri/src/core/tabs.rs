use serde::{Deserialize, Serialize};

pub type TabId = u64;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Tab {
    pub id: TabId,
    pub url: String,
    pub title: String,
    pub incognito: bool,
    pub frozen: bool,
    pub pinned: bool,
    pub audible: bool,
    pub dirty_form: bool,
    pub last_active: u64,
    pub scroll_y: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TabList {
    pub tabs: Vec<Tab>,
    pub active: Option<TabId>,
    #[serde(skip)]
    next_id: TabId,
}

fn app_page(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let app_origin = u.scheme() == "tauri" || matches!(u.host_str(), Some("tauri.localhost" | "localhost" | "127.0.0.1"));
    let name = u.path().strip_prefix("/pages/")?.strip_suffix(".html")?;
    (app_origin && !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric())).then(|| name.to_string())
}

/// The built-in new-tab page. Only counts when served by the app itself (tauri://localhost,
/// http://tauri.localhost, or the local dev server) — a website serving /pages/newtab.html does not.
pub fn is_newtab(url: &str) -> bool {
    url.is_empty() || url == "rb://newtab" || app_page(url).as_deref() == Some("newtab")
}

/// Show/store the app's own pages as `rb://name` (new tab as ""); everything else unchanged.
pub fn to_rb(url: &str) -> String {
    match app_page(url) {
        Some(n) if n == "newtab" => String::new(),
        Some(n) => format!("rb://{n}"),
        None => url.to_string(),
    }
}

/// `rb://history` -> `pages/history.html` (only plain names, so no path tricks).
pub fn rb_page(url: &str) -> Option<String> {
    let name = url.strip_prefix("rb://")?.trim_end_matches('/');
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric())).then(|| format!("pages/{name}.html"))
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl TabList {
    pub fn open(&mut self, url: &str, incognito: bool) -> TabId {
        self.next_id += 1;
        let id = self.next_id;
        self.tabs.push(Tab { id, url: url.into(), incognito, last_active: now_secs(), ..Default::default() });
        id
    }

    /// Removes the tab and returns the (possibly new) active tab.
    pub fn close(&mut self, id: TabId) -> Option<TabId> {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return self.active };
        self.tabs.remove(i);
        if self.active == Some(id) {
            self.active = self.tabs.get(i).or_else(|| i.checked_sub(1).and_then(|j| self.tabs.get(j))).map(|t| t.id);
        }
        self.active
    }

    pub fn activate(&mut self, id: TabId) {
        if let Some(t) = self.get_mut(id) {
            t.last_active = now_secs();
            self.active = Some(id);
        }
    }

    pub fn move_tab(&mut self, id: TabId, to: usize) {
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            let t = self.tabs.remove(i);
            self.tabs.insert(to.min(self.tabs.len()), t);
        }
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> { self.tabs.iter().find(|t| t.id == id) }
    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> { self.tabs.iter_mut().find(|t| t.id == id) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn three() -> (TabList, TabId, TabId, TabId) {
        let mut l = TabList::default();
        let (a, b, c) = (l.open("a", false), l.open("b", false), l.open("c", false));
        (l, a, b, c)
    }
    #[test] fn close_active_activates_right_neighbor_else_left() {
        let (mut l, a, b, c) = three();
        l.activate(b);
        assert_eq!(l.close(b), Some(c));
        assert_eq!(l.close(c), Some(a));
    }
    #[test] fn close_inactive_keeps_active() {
        let (mut l, a, b, _) = three();
        l.activate(a);
        assert_eq!(l.close(b), Some(a));
    }
    #[test] fn close_last_returns_none() {
        let mut l = TabList::default();
        let a = l.open("a", false);
        l.activate(a);
        assert_eq!(l.close(a), None);
    }
    #[test] fn newtab_only_from_app_origin() {
        assert!(is_newtab(""));
        assert!(is_newtab("tauri://localhost/pages/newtab.html"));
        assert!(is_newtab("http://127.0.0.1:1430/pages/newtab.html"));
        assert!(!is_newtab("https://evil.com/pages/newtab.html"));
    }
    #[test] fn app_pages_map_to_rb_scheme() {
        assert_eq!(to_rb("http://127.0.0.1:1430/pages/history.html"), "rb://history");
        assert_eq!(to_rb("tauri://localhost/pages/newtab.html"), "");
        assert_eq!(to_rb("https://evil.com/pages/history.html"), "https://evil.com/pages/history.html");
        assert_eq!(rb_page("rb://downloads"), Some("pages/downloads.html".into()));
        assert_eq!(rb_page("rb://../secret"), None);
    }
    #[test] fn move_tab_reorders() {
        let (mut l, a, b, c) = three();
        l.move_tab(c, 0);
        assert_eq!(l.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), vec![c, a, b]);
    }
}
