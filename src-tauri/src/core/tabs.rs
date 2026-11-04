use serde::{Deserialize, Serialize};

pub type TabId = u64;
pub type GroupId = u64;

/// Chrome's group colours; the UI maps names to shades.
pub const COLORS: [&str; 8] = ["grey", "blue", "red", "yellow", "green", "pink", "purple", "cyan"];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
    pub color: String,
    pub collapsed: bool,
}

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
    pub group: Option<GroupId>,
    /// The page has a password field (drives the key icon in the address bar).
    pub login_form: bool,
}

/// Invariant: the tabs of a group are always next to each other.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TabList {
    pub tabs: Vec<Tab>,
    pub active: Option<TabId>,
    pub groups: Vec<Group>,
    #[serde(skip)]
    next_id: TabId,
    #[serde(skip)]
    next_group: GroupId,
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
        self.prune_groups();
        if self.active == Some(id) {
            self.active = self.tabs.get(i).or_else(|| i.checked_sub(1).and_then(|j| self.tabs.get(j))).map(|t| t.id);
        }
        self.active
    }

    pub fn activate(&mut self, id: TabId) {
        if let Some(t) = self.get_mut(id) {
            t.last_active = now_secs();
            let g = t.group;
            self.active = Some(id);
            if let Some(g) = g.and_then(|g| self.group_mut(g)) { g.collapsed = false; }
        }
    }

    // ---- groups ----

    pub fn group_mut(&mut self, g: GroupId) -> Option<&mut Group> { self.groups.iter_mut().find(|x| x.id == g) }

    /// Unused colour first, then cycle.
    pub fn add_group(&mut self, name: &str, color: Option<&str>, collapsed: bool) -> GroupId {
        self.next_group += 1;
        let color = color.filter(|c| COLORS.contains(c)).map(String::from).unwrap_or_else(|| {
            COLORS.iter().find(|c| !self.groups.iter().any(|g| g.color == **c)).unwrap_or(&COLORS[self.groups.len() % 8]).to_string()
        });
        self.groups.push(Group { id: self.next_group, name: name.into(), color, collapsed });
        self.next_group
    }

    /// Index just after the group's last tab.
    fn group_end(&self, g: GroupId) -> Option<usize> { self.tabs.iter().rposition(|t| t.group == Some(g)).map(|i| i + 1) }

    /// Move a tab into group `to` (lands at its end) or out of its group (lands just right of it), so groups stay contiguous.
    fn regroup(&mut self, id: TabId, to: Option<GroupId>) {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return };
        let mut t = self.tabs.remove(i);
        let from = t.group.take();
        let at = to.or(from).and_then(|g| self.group_end(g)).unwrap_or(i);
        t.group = to;
        self.tabs.insert(at, t);
        self.prune_groups();
    }

    pub fn new_group(&mut self, id: TabId) -> Option<GroupId> {
        self.get(id)?;
        self.remove_from_group(id);
        let g = self.add_group("", None, false);
        self.get_mut(id).unwrap().group = Some(g);
        Some(g)
    }

    pub fn add_to_group(&mut self, id: TabId, g: GroupId) {
        if self.groups.iter().any(|x| x.id == g) && self.get(id).is_some_and(|t| t.group != Some(g)) { self.regroup(id, Some(g)); }
    }

    pub fn remove_from_group(&mut self, id: TabId) {
        if self.get(id).is_some_and(|t| t.group.is_some()) { self.regroup(id, None); }
    }

    pub fn ungroup(&mut self, g: GroupId) {
        self.tabs.iter_mut().filter(|t| t.group == Some(g)).for_each(|t| t.group = None);
        self.prune_groups();
    }

    pub fn set_collapsed(&mut self, g: GroupId, collapsed: bool) {
        if let Some(x) = self.group_mut(g) { x.collapsed = collapsed; }
    }

    pub fn prune_groups(&mut self) {
        let tabs = &self.tabs;
        self.groups.retain(|g| tabs.iter().any(|t| t.group == Some(g.id)));
    }

    /// Not hidden inside a collapsed group.
    pub fn visible(&self, id: TabId) -> bool {
        self.get(id).is_some_and(|t| t.group.is_none_or(|g| !self.groups.iter().any(|x| x.id == g && x.collapsed)))
    }

    /// The closest visible tab, preferring the right side (where Chrome goes).
    pub fn nearest_visible(&self, id: TabId) -> Option<TabId> {
        let i = self.tabs.iter().position(|t| t.id == id)?;
        let right = self.tabs[i + 1..].iter().find(|t| self.visible(t.id));
        right.or_else(|| self.tabs[..i].iter().rev().find(|t| self.visible(t.id))).map(|t| t.id)
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
    fn ids(l: &TabList) -> Vec<TabId> { l.tabs.iter().map(|t| t.id).collect() }
    #[test] fn group_tabs_stay_contiguous() {
        let (mut l, a, b, c) = three();
        let g = l.new_group(a).unwrap();
        l.add_to_group(c, g);
        assert_eq!(ids(&l), vec![a, c, b]);
        assert_eq!(l.get(c).unwrap().group, Some(g));
        assert_eq!(l.get(b).unwrap().group, None);
    }
    #[test] fn remove_from_group_moves_right_of_group() {
        let (mut l, a, b, c) = three();
        let g = l.new_group(a).unwrap();
        l.add_to_group(b, g);
        l.add_to_group(c, g);
        l.remove_from_group(a);
        assert_eq!(ids(&l), vec![b, c, a]);
        assert_eq!(l.get(a).unwrap().group, None);
    }
    #[test] fn empty_groups_pruned() {
        let (mut l, a, b, _) = three();
        let g = l.new_group(a).unwrap();
        l.new_group(b);
        l.close(a);
        assert!(l.groups.iter().all(|x| x.id != g));
        assert_eq!(l.groups.len(), 1);
        assert_ne!(l.groups[0].color, "", "new groups get a colour");
    }
    #[test] fn collapse_hides_and_activate_expands() {
        let (mut l, a, b, c) = three();
        let g = l.new_group(b).unwrap();
        l.activate(b);
        l.set_collapsed(g, true);
        assert!(!l.visible(b));
        assert_eq!(l.nearest_visible(b), Some(c));
        l.ungroup(g);
        let g = l.new_group(c).unwrap();
        l.set_collapsed(g, true);
        assert_eq!(l.nearest_visible(c), Some(b), "falls back to the left");
        l.activate(c);
        assert!(l.visible(c), "activating a hidden tab expands its group");
        let _ = a;
    }
    #[test] fn move_tab_reorders() {
        let (mut l, a, b, c) = three();
        l.move_tab(c, 0);
        assert_eq!(l.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), vec![c, a, b]);
    }
}
