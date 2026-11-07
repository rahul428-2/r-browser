pub mod core;
mod platform;

use crate::core::blocker;
use crate::core::library::{site, unique_path, Download, Library, Suggestion, Visit, Bookmark};
use crate::core::omnibox::resolve;
use crate::core::session::{self, Session};
use crate::core::store;
use crate::core::tabs::{now_secs, rb_page, to_rb, GroupId, TabId, TabList, COLORS};
use crate::core::vault::{self, origin_of, Entry, Vault};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuItemBuilder, SubmenuBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri::webview::{DownloadEvent, PageLoadEvent, WebviewBuilder};
use tauri::window::WindowBuilder;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewUrl, Window, WindowEvent};

const BAR: f64 = 84.0; // tab strip 36 + toolbar 47 + guard line 1
const MAIN: &str = "main";
const ZOOM_STEPS: [f64; 9] = [0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 2.0];

struct Live { win: String, name: String, start: std::time::Instant, got: u64, pct: Option<u8>, eta: Option<u64> }

/// One tab list per browser window, keyed by window label ("main", "incognito-1", ...).
struct Browser(Mutex<HashMap<String, TabList>>);
/// Bookmarks, history, downloads (normal windows only).
struct Lib(Mutex<Library>);
/// Verified-site lists: bundled top sites + the user's trusted sites + sites that passed the safety check.
struct Trusted(Mutex<core::trust::TrustDb>);
/// One-time safety checks for unverified sites (core::check).
struct Checks {
    checker: core::check::Checker,
    /// Sites being watched on their first visit: site -> (tab label, when it started, what the site did).
    watching: Mutex<HashMap<String, (String, std::time::Instant, core::check::Visit)>>,
    /// Sites that failed this session, and why (shown in the ⚠ menu; checked again next session).
    failed: Mutex<HashMap<String, core::check::Fail>>,
}
/// URLs given on the command line (`r-browser example.com wikipedia.org`), opened once the UI is ready.
struct StartUrls(Mutex<Vec<String>>);
/// Per-webview zoom level and in-flight download paths (macOS doesn't report where a download landed).
struct Runtime {
    zoom: Mutex<HashMap<String, f64>>,
    downloads: Mutex<HashMap<String, PathBuf>>,
    /// Downloads in flight by URL, for progress: (window, file name, started, last percent/seconds left/bytes).
    live: Mutex<HashMap<String, Live>>,
    /// Last fully loaded URL per tab webview: a download starts like a navigation, so we roll back to this.
    committed: Mutex<HashMap<String, String>>,
    /// A tab being dragged outside the strips: (window it lives in now, tab, grab offset x, y in that window).
    tear: Mutex<Option<(String, TabId, f64, f64)>>,
    /// Window whose strip currently shows a "drop here" mark for the torn tab.
    hint: Mutex<Option<String>>,
    /// Pop-up waiting for the user's Allow, per window: (page that tried it, pop-up URL).
    popups: Mutex<HashMap<String, (String, String)>>,
    /// WKWebView pointer -> tab webview label, to find the tab a load error belongs to.
    engine: Mutex<HashMap<usize, String>>,
    /// Tabs showing a "can't open this site" page: label -> the URL that failed (Reload retries it).
    failed: Mutex<HashMap<String, String>>,
    /// HTTPS-only: hosts we just switched to https -> (the http URL asked for, when), to spot an https attempt
    /// that fails or redirects back to http.
    upgraded: Mutex<HashMap<String, (String, std::time::Instant)>>,
    /// Hosts the user chose to open over plain http, this session only.
    insecure_ok: Mutex<std::collections::HashSet<String>>,
    /// Addresses the user just asked for (typed / picked), so the safety check doesn't count them as the site
    /// redirecting by itself.
    user_nav: Mutex<std::collections::HashSet<String>>,
    /// App-page addresses the browser itself is about to open (see `page_url`); anything else is refused.
    app_nav: Mutex<std::collections::HashSet<String>>,
    /// Tabs just thawed: label -> where to scroll once the page has loaded again.
    scroll_to: Mutex<HashMap<String, f64>>,
}

/// Password vault runtime state. Plaintext only exists inside `with_vault`.
#[derive(Default)]
struct VaultRt {
    /// One vault operation at a time (read-modify-write of vault.bin).
    lock: Mutex<()>,
    unlocked_until: AtomicU64,
    /// Login waiting for the user's Save / Never answer, per window.
    pending: Mutex<HashMap<String, Entry>>,
    /// Hashes (random per run) of logins we just filled, so submitting them doesn't ask to save again.
    filled: Mutex<std::collections::HashSet<u64>>,
    hasher: std::collections::hash_map::RandomState,
}
const UNLOCK_SECS: u64 = 5 * 60;

type Res = Result<(), String>;

static SESSION_GEN: AtomicU64 = AtomicU64::new(0);
static LIBRARY_GEN: AtomicU64 = AtomicU64::new(0);
static RESTORED: AtomicBool = AtomicBool::new(false);
static NEXT_INCOGNITO: AtomicU64 = AtomicU64::new(1);
static NEXT_WINDOW: AtomicU64 = AtomicU64::new(1);

fn is_incognito(win: &str) -> bool { win.starts_with("incognito-") }
fn ui_label(win: &str) -> String { format!("{win}-ui") }
/// Tab webview labels don't name their window (`tab-7`, incognito `itab-7`): a torn-off tab moves to another
/// window with its live webview. Tab ids are unique across windows. Capabilities match these prefixes.
fn label(win: &str, id: TabId) -> String { if is_incognito(win) { format!("itab-{id}") } else { format!("tab-{id}") } }
fn is_incognito_label(label: &str) -> bool { label.starts_with("itab-") }
/// (window, tab) for a tab webview: the id is in the label, the window is whichever one holds that tab now.
fn parse_label(app: &AppHandle, label: &str) -> Option<(String, TabId)> {
    let id: TabId = label.strip_prefix("tab-").or_else(|| label.strip_prefix("itab-"))?.parse().ok()?;
    Some((owner(app, id)?, id))
}
/// The window that holds tab `id` right now.
fn owner(app: &AppHandle, id: TabId) -> Option<String> {
    let map = app.state::<Browser>().inner().0.lock().unwrap();
    map.iter().find(|(_, l)| l.get(id).is_some()).map(|(w, _)| w.clone())
}

fn err(e: impl std::fmt::Display) -> String { e.to_string() }

/// Run `f` on a window's tab list (default value if the window is gone).
fn with<R: Default>(app: &AppHandle, win: &str, f: impl FnOnce(&mut TabList) -> R) -> R {
    let state = app.state::<Browser>();
    let mut map = state.inner().0.lock().unwrap();
    map.get_mut(win).map(f).unwrap_or_default()
}
fn tab_ids(app: &AppHandle, win: &str) -> Vec<TabId> { with(app, win, |l| l.tabs.iter().map(|t| t.id).collect()) }
fn active_url(app: &AppHandle, win: &str) -> Option<(String, String)> {
    with(app, win, |l| l.active.and_then(|a| l.get(a)).map(|t| (t.url.clone(), t.title.clone())))
}
fn lib<R>(app: &AppHandle, f: impl FnOnce(&mut Library) -> R) -> R {
    f(&mut app.state::<Lib>().inner().0.lock().unwrap())
}

fn content_area(win: &Window) -> (f64, f64) {
    let f = win.scale_factor().unwrap_or(1.0);
    let s = win.inner_size().map(|s| s.to_logical::<f64>(f)).unwrap_or(LogicalSize::new(1280.0, 800.0));
    (s.width, (s.height - BAR).max(0.0))
}

fn layout(app: &AppHandle, win: &str) {
    let Some(w) = app.get_window(win) else { return };
    let (width, h) = content_area(&w);
    if let Some(ui) = app.get_webview(&ui_label(win)) {
        let _ = ui.set_position(LogicalPosition::new(0.0, 0.0));
        let _ = ui.set_size(LogicalSize::new(width, BAR));
    }
    for id in tab_ids(app, win) {
        if let Some(wv) = app.get_webview(&label(win, id)) {
            let _ = wv.set_position(LogicalPosition::new(0.0, BAR));
            let _ = wv.set_size(LogicalSize::new(width, h));
        }
    }
}

#[derive(Serialize, Clone)]
struct UiState {
    win: String,
    #[serde(flatten)]
    list: TabList,
    bookmarked: bool,
    /// The active tab's site isn't a well-known or user-trusted one (⚠ beside the address).
    unverified: bool,
    /// ...or it only passed the automatic safety check (soft "Checked" badge).
    checked: bool,
}

fn emit(app: &AppHandle, win: &str) {
    let list = with(app, win, |l| l.clone());
    let url = list.active.and_then(|a| list.get(a)).map(|t| t.url.clone()).unwrap_or_default();
    let bookmarked = lib(app, |l| l.is_bookmarked(&url));
    let trust = trust_of(app, &url);
    let (unverified, checked) = (trust == core::trust::Trust::Unverified, trust == core::trust::Trust::Checked);
    let _ = app.emit_to(ui_label(win), "tabs-changed", UiState { win: win.to_string(), list, bookmarked, unverified, checked });
    if !is_incognito(win) { save_session_soon(app); }
}

// ---- persistence (debounced ~1 s; incognito is never saved) ----

fn data_file(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(name))
}

fn debounce(gen: &'static AtomicU64, app: &AppHandle, save: fn(&AppHandle)) {
    let mine = gen.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if gen.load(Ordering::SeqCst) == mine { save(&app); }
    });
}

fn save_session_soon(app: &AppHandle) {
    if !RESTORED.load(Ordering::SeqCst) { return; } // never overwrite the old session before restoring it
    debounce(&SESSION_GEN, app, save_session_now);
}

/// All normal windows, main first. Incognito windows are never saved.
fn save_session_now(app: &AppHandle) {
    SESSION_GEN.fetch_add(1, Ordering::SeqCst); // supersede a pending debounced save
    let snap = {
        let map = app.state::<Browser>().inner().0.lock().unwrap();
        let mut wins: Vec<&String> = map.keys().filter(|w| !is_incognito(w)).collect();
        wins.sort_by_key(|w| (w.as_str() != MAIN, w.len(), (*w).clone()));
        Session::from_windows(&wins.iter().map(|w| &map[*w]).collect::<Vec<_>>())
    };
    if let Some(p) = data_file(app, "session.json") {
        if let Err(e) = session::save_atomic(&p, &snap) { eprintln!("session save failed: {e}"); }
    }
}

fn save_library_soon(app: &AppHandle) {
    debounce(&LIBRARY_GEN, app, |app| {
        let snap = lib(app, |l| l.clone());
        if let Some(p) = data_file(app, "library.json") {
            if let Err(e) = store::save_atomic(&p, &snap) { eprintln!("library save failed: {e}"); }
        }
    });
}

/// Reopen last session's tabs; all start frozen (no webview) except the active one.
fn restore(app: &AppHandle) -> Res {
    let Some(saved) = data_file(app, "session.json").and_then(|p| session::load::<Session>(&p)) else { return Ok(()) };
    let ids: Vec<TabId> = with(app, MAIN, |l| saved.restore_into(l));
    if let Some(&id) = ids.get(saved.active).or(ids.first()) { activate(app, MAIN, id)?; }
    for extra in &saved.extra {
        let win = format!("window-{}", NEXT_WINDOW.fetch_add(1, Ordering::SeqCst));
        create_window(app, &win, None).map_err(err)?;
        let ids = with(app, &win, |l| extra.restore_into(l));
        if let Some(&id) = ids.get(extra.active).or(ids.first()) { activate(app, &win, id)?; }
    }
    Ok(())
}

// ---- windows & tabs ----

/// `at` = top-left in screen points (torn-off tabs open where they were dropped).
fn create_window(app: &AppHandle, win: &str, at: Option<(f64, f64)>) -> tauri::Result<()> {
    let title = if is_incognito(win) { "R-Browser — Incognito" } else { "R-Browser" };
    let mut builder = WindowBuilder::new(app, win).title(title).inner_size(1280.0, 800.0).min_inner_size(480.0, 300.0);
    if let Some((x, y)) = at { builder = builder.position(x, y); }
    // macOS: draw the tab strip where the title bar was (like Chrome); keeps webview coordinates exact.
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
    // Windows: no system title bar; the tab strip is the title bar, with its own minimize/maximize/close (like Chrome).
    #[cfg(target_os = "windows")]
    let builder = builder.decorations(false);
    let w = builder.build()?;
    // Re-setting resizable makes Tauri add its edge-resize borders, which it skips for windows without a page of their own.
    #[cfg(target_os = "windows")]
    w.set_resizable(true)?;
    // Linux: no File/Edit bar under the title; the menu stays attached so its shortcuts still work.
    #[cfg(target_os = "linux")]
    w.hide_menu()?;
    app.state::<Browser>().inner().0.lock().unwrap().insert(win.to_string(), TabList::default());
    let (width, _) = content_area(&w);
    let ui = w.add_child(
        // Incognito windows: the toolbar itself is non-persistent too (it may load favicons).
        {
            let b = WebviewBuilder::new(ui_label(win), WebviewUrl::App("index.html".into())).incognito(is_incognito(win));
            #[cfg(target_os = "macos")]
            let b = b.user_agent(user_agent());
            b
        },
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(width, BAR),
    )?;
    // The toolbar loads site icons, so it gets the ad/tracker block list too (no-op until the list is loaded).
    platform::set_blocking(&ui, true);
    shortcuts(app, &ui);
    platform::raise_resize_borders(&w);
    let (app2, win2) = (app.clone(), win.to_string());
    w.on_window_event(move |e| match e {
        WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => layout(&app2, &win2),
        WindowEvent::Destroyed => {
            app2.state::<Browser>().inner().0.lock().unwrap().remove(&win2);
        }
        _ => {}
    });
    Ok(())
}

/// What a tab webview loads: http(s) as-is, `rb://name` -> the app's own page, anything else -> new tab.
fn target(url: &str) -> WebviewUrl {
    if let Some(page) = rb_page(url) { return WebviewUrl::App(page.into()); }
    match Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => WebviewUrl::External(u),
        _ => WebviewUrl::App("pages/newtab.html".into()),
    }
}

/// While anything downloads, twice a second: read the byte counts and tell the toolbar (percent, time left).
fn watch_progress(app: &AppHandle) {
    static ON: AtomicBool = AtomicBool::new(false);
    if ON.swap(true, Ordering::SeqCst) { return; }
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        {
            let rt = app.state::<Runtime>();
            let live = rt.live.lock().unwrap(); // under the lock, so a new download can't slip past
            if live.is_empty() { ON.store(false, Ordering::SeqCst); return; }
        }
        let a = app.clone();
        let _ = app.run_on_main_thread(move || {
            for (url, got, total) in platform::download_progress() {
                let rt = a.state::<Runtime>();
                let mut live = rt.live.lock().unwrap();
                let Some(l) = live.get_mut(&url) else { continue };
                (l.pct, l.eta) = core::library::progress(got, total, l.start.elapsed().as_secs_f64());
                l.got = got;
                let _ = a.emit_to(ui_label(&l.win), "download", (l.win.clone(), l.name.clone(), "progress", got, l.pct, l.eta));
            }
        });
    });
}

fn on_download(app: &AppHandle, wv: &Webview, ev: DownloadEvent<'_>) -> bool {
    let win = parse_label(app, wv.label()).map(|(w, _)| w).unwrap_or_else(|| MAIN.into());
    let incognito = is_incognito(&win) || is_incognito_label(wv.label());
    let rt = app.state::<Runtime>();
    match ev {
        DownloadEvent::Requested { url, destination } => {
            // Always into the Downloads folder, never auto-opened (project 2 will open files inside a VM).
            let dir = app.path().download_dir().unwrap_or_else(|_| std::env::temp_dir());
            let suggested = destination.file_name().and_then(|n| n.to_str()).map(String::from)
                .or_else(|| url.path_segments().and_then(|mut s| s.next_back()).map(String::from))
                .unwrap_or_default();
            let path = unique_path(&dir, &suggested);
            *destination = path.clone();
            rt.downloads.lock().unwrap().insert(url.to_string(), path.clone());
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            rt.live.lock().unwrap().insert(url.to_string(), Live { win: win.clone(), name, start: std::time::Instant::now(), got: 0, pct: None, eta: None });
            watch_progress(app);
            if let (Some((w, id)), Some(prev)) = (parse_label(app, wv.label()), rt.committed.lock().unwrap().get(wv.label()).cloned()) {
                with(app, &w, |l| if let Some(t) = l.get_mut(id) { t.url = prev; t.loading = false; });
                emit(app, &w);
            }
            if !incognito {
                lib(app, |l| l.add_download(Download { url: url.to_string(), path: path.display().to_string(), at: now_secs(), done: false, ok: false }));
                save_library_soon(app);
            }
            let _ = app.emit_to(ui_label(&win), "download", (win.clone(), path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(), "started"));
        }
        DownloadEvent::Finished { url, success, .. } => {
            let path = rt.downloads.lock().unwrap().remove(url.as_str());
            rt.live.lock().unwrap().remove(url.as_str());
            platform::download_finished(url.as_str());
            if let (Some(p), true) = (&path, success) { mark_from_internet(p, url.as_str()); }
            if !incognito {
                lib(app, |l| if let Some(d) = l.downloads.iter_mut().rev().find(|d| d.url == url.as_str()) { d.done = true; d.ok = success; });
                save_library_soon(app);
            }
            let name = path.as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("").to_string();
            let _ = app.emit_to(ui_label(&win), "download", (win.clone(), name, if success { "done" } else { "failed" }));
        }
        _ => {}
    }
    true
}

/// Identify as Safari (Rahul's call, 5 Nov): Gmail rejects the engine's bare user agent. Used by tabs and the
/// toolbar alike (it fetches favicons), so a site never sees two different identities.
#[cfg(target_os = "macos")]
fn user_agent() -> &'static str {
    static UA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    UA.get_or_init(|| core::tabs::safari_ua(&std::fs::read_to_string("/Applications/Safari.app/Contents/Info.plist").unwrap_or_default()))
}

fn create_webview(app: &AppHandle, win: &str, id: TabId, url: &str) -> tauri::Result<Webview> {
    let w = app.get_window(win).ok_or(tauri::Error::WindowNotFound)?;
    platform::hook_delegate();
    let (width, h) = content_area(&w);
    let (a1, a2, a3, a4) = (app.clone(), app.clone(), app.clone(), app.clone());
    let me = label(win, id);
    let mut builder = WebviewBuilder::new(label(win, id), target(url))
        // Incognito: non-persistent data store — nothing written to disk.
        .incognito(is_incognito(win))
        .on_page_load(move |wv, p| {
            let Some((win, id)) = parse_label(&a1, wv.label()) else { return };
            let url = to_rb(p.url().as_str());
            let started = p.event() == PageLoadEvent::Started;
            if started { sync_blocking(&a1, &wv, p.url().as_str()); }
            let failed = {
                let rt = a1.state::<Runtime>();
                let mut f = rt.failed.lock().unwrap();
                if started && f.get(wv.label()).is_some_and(|u| u != p.url().as_str()) { f.remove(wv.label()); }
                f.contains_key(wv.label())
            };
            let restoring = a1.state::<Runtime>().scroll_to.lock().unwrap().contains_key(wv.label());
            let title = with(&a1, &win, |l| l.get_mut(id).map(|t| {
                if started {
                    t.login_form = false;
                    t.dirty_form = false;
                    t.audible = false;
                    if !restoring { t.scroll_y = 0.0; }
                    // A different site: forget the old site's icon (same site keeps it, so tabs don't flicker).
                    if url::Url::parse(&t.url).ok().map(|u| u.origin()) != url::Url::parse(&url).ok().map(|u| u.origin()) { t.icon.clear(); }
                }
                t.url = url.clone();
                t.loading = started;
                t.title.clone()
            })).unwrap_or_default();
            if p.event() == PageLoadEvent::Finished {
                if let Some(h) = p.url().host_str() { a1.state::<Runtime>().upgraded.lock().unwrap().remove(h); }
                a1.state::<Runtime>().committed.lock().unwrap().insert(wv.label().to_string(), url.clone());
                if let Some(y) = a1.state::<Runtime>().scroll_to.lock().unwrap().remove(wv.label()) {
                    // Lazy pages grow after load: retry briefly until the old position exists.
                    let _ = wv.eval(format!("(y=>{{let n=0;const go=()=>{{scrollTo(0,y);if(scrollY<y-2&&++n<6)setTimeout(go,300)}};go()}})({y})"));
                }
            }
            if p.event() == PageLoadEvent::Finished && !is_incognito(&win) && !failed {
                lib(&a1, |l| l.record_visit(&url, &title, now_secs(), false));
                save_library_soon(&a1);
                start_check(&a1, wv.label(), &url);
            }
            emit(&a1, &win);
        })
        .on_document_title_changed(move |wv, title| {
            let Some((win, id)) = parse_label(&a2, wv.label()) else { return };
            let url = with(&a2, &win, |l| l.get_mut(id).map(|t| { t.title = title.clone(); t.url.clone() })).unwrap_or_default();
            if !is_incognito(&win) { lib(&a2, |l| l.set_title(&url, &title)); }
            emit(&a2, &win);
        })
        .on_download(move |wv, ev| on_download(&a3, &wv, ev))
        // Clicked target=_blank links and window.open (WebKit already refuses ones without a click) open as tabs.
        .on_new_window(move |url, _| {
            let (app, me) = (a4.clone(), me.clone());
            if matches!(url.scheme(), "http" | "https") {
                tauri::async_runtime::spawn(async move {
                    if let Some((win, _)) = parse_label(&app, &me) { let _ = open_tab(&app, &win, url.as_str()); }
                });
            }
            tauri::webview::NewWindowResponse::Deny
        });
    #[cfg(target_os = "macos")]
    { builder = builder.user_agent(user_agent()); }
    builder = builder.initialization_script(include_str!("inject/dark.js"))
        .initialization_script(include_str!("inject/popup.js"))
        .initialization_script(include_str!("inject/icon.js"))
        .initialization_script(include_str!("inject/state.js"));
    if !is_incognito(win) { builder = builder.initialization_script(include_str!("inject/login.js")); }
    let wv = w.add_child(builder, LogicalPosition::new(0.0, BAR), LogicalSize::new(width, h))?;
    shortcuts(app, &wv);
    platform::raise_resize_borders(&w);
    platform::watch_downloads(&wv);
    sync_blocking(app, &wv, url);
    if let Some(&z) = app.state::<Runtime>().zoom.lock().unwrap().get(wv.label()) { let _ = wv.set_zoom(z); } // thawed tab keeps its zoom
    #[cfg(target_os = "macos")]
    {
        let (a, l) = (app.clone(), wv.label().to_string());
        let _ = wv.with_webview(move |p| { a.state::<Runtime>().engine.lock().unwrap().insert(p.inner() as usize, l); });
    }
    Ok(wv)
}

// ---- sites that can't be opened ----

/// Full address of an app page (`rb://name?query`): same origin as the toolbar UI. Only addresses made here
/// may be opened from a website tab, so call this only for navigations the browser itself starts.
fn page_url(app: &AppHandle, rb: &str) -> Option<Url> {
    let page = rb_page(rb)?;
    let base = app.webviews().into_values().find(|w| w.label().ends_with("-ui"))?.url().ok()?;
    let u = base.join(&format!("/{page}")).ok()?;
    app.state::<Runtime>().app_nav.lock().unwrap().insert(u.to_string());
    Some(u)
}

/// HTTPS-only, for every top-level navigation: try http:// sites over https first. If the https attempt
/// comes straight back to http (the site redirects to http), stop and show the "no secure version" page.
fn on_main_navigation(app: &AppHandle, url: &str, current: &str, kind: isize) -> Option<String> {
    let rt = app.state::<Runtime>();
    // A watched site sending the tab to another site by itself (script or redirect, not a click or typing).
    let typed = rt.user_nav.lock().unwrap().remove(url);
    if kind == -1 && !typed && core::trust::site_of(current).is_some_and(|c| core::trust::site_of(url).as_deref() != Some(&c)) {
        note_bad(app, current, |v| v.redirects += 1);
    }
    if core::tabs::is_app_page(url) {
        let expected = rt.app_nav.lock().unwrap().remove(url);
        return (!core::tabs::app_nav_allowed(current, kind, expected)).then(String::new);
    }
    let to = core::https::upgrade(url, &rt.insecure_ok.lock().unwrap())?;
    let host = Url::parse(url).ok()?.host_str()?.to_string();
    let mut up = rt.upgraded.lock().unwrap();
    if up.get(&host).is_some_and(|(_, at)| at.elapsed().as_secs() < 15) {
        up.remove(&host);
        return page_url(app, &core::https::insecure_page(url)).map(String::from);
    }
    up.insert(host, (url.to_string(), std::time::Instant::now()));
    Some(to)
}

/// The http URL to offer, if `url` is an https attempt we made that just failed for a reason http might not share.
fn failed_upgrade(app: &AppHandle, url: &str, domain: &str, code: i64) -> Option<String> {
    let host = Url::parse(url).ok().filter(|u| u.scheme() == "https")?.host_str()?.to_string();
    let (http, at) = app.state::<Runtime>().upgraded.lock().unwrap().remove(&host)?;
    // No such host / offline: http won't work either, so show the normal reason.
    let hopeless = domain == "NSURLErrorDomain" && matches!(code, -1003 | -1009);
    (at.elapsed().as_secs() < 60 && !hopeless).then_some(http)
}

#[tauri::command]
async fn insecure_continue(webview: Webview, app: AppHandle, url: String) -> Res { trusted(&webview)?;
    let u = Url::parse(&url).map_err(err)?;
    let host = u.host_str().filter(|_| u.scheme() == "http").ok_or("not an http address")?;
    app.state::<Runtime>().insecure_ok.lock().unwrap().insert(host.to_string());
    webview.navigate(u).map_err(err)
}

/// A tab's page failed to load: show why, in the tab, instead of a blank page.
fn on_load_failed(app: &AppHandle, engine: usize, url: String, domain: String, code: i64, desc: String) {
    let Some(label) = app.state::<Runtime>().engine.lock().unwrap().get(&engine).cloned() else { return };
    let url = if url.is_empty() {
        parse_label(app, &label).and_then(|(w, id)| with(app, &w, |l| l.get(id).map(|t| t.url.clone()))).unwrap_or_default()
    } else { url };
    if !url.starts_with("http") { return; }
    if let Some(http) = failed_upgrade(app, &url, &domain, code) {
        if let (Some(wv), Some(page)) = (app.get_webview(&label), page_url(app, &core::https::insecure_page(&http))) { let _ = wv.navigate(page); }
        return;
    }
    let (Some(wv), Some(html)) = (app.get_webview(&label), core::errors::error_page(&url, &domain, code, &desc)) else { return };
    app.state::<Runtime>().failed.lock().unwrap().insert(label, url.clone());
    platform::show_html(&wv, html, url);
}

/// Reload, or retry the site if the tab is showing a "can't open" page (reloading that would just reload the page).
fn reload_tab(app: &AppHandle, win: &str, id: TabId) -> Res {
    let wv = tab_webview(app, win, id)?;
    match app.state::<Runtime>().failed.lock().unwrap().remove(wv.label()).and_then(|u| Url::parse(&u).ok()) {
        Some(u) => wv.navigate(u).map_err(err),
        None => wv.reload().map_err(err),
    }
}

// ---- verified / unverified sites ----

fn trust_of(app: &AppHandle, url: &str) -> core::trust::Trust { app.state::<Trusted>().0.lock().unwrap().classify(url) }

/// Unverified sites load locked down (Lockdown Mode: JIT and risky web features off) until they pass the check.
fn needs_lockdown(app: &AppHandle, url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) && trust_of(app, url) == core::trust::Trust::Unverified
}

/// First visit to an unverified site (normal windows): check its address now, then watch it for 30 s.
fn start_check(app: &AppHandle, label: &str, url: &str) {
    if trust_of(app, url) != core::trust::Trust::Unverified { return; }
    let Some(site) = core::trust::site_of(url) else { return };
    let ck = app.state::<Checks>();
    if ck.failed.lock().unwrap().contains_key(&site) || ck.watching.lock().unwrap().contains_key(&site) { return; }
    if let Err(f) = ck.checker.domain_checks(url) { ck.failed.lock().unwrap().insert(site, f); return; }
    ck.watching.lock().unwrap().insert(site.clone(), (label.to_string(), std::time::Instant::now(), core::check::Visit::default()));
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(core::check::WATCH_SECS));
        finish_check(&app, &site);
    });
}

/// The watched site did something bad: count it, and fail the check at once if that's enough.
fn note_bad(app: &AppHandle, page: &str, f: impl FnOnce(&mut core::check::Visit)) {
    let Some(site) = core::trust::site_of(page) else { return };
    let ck = app.state::<Checks>();
    let mut w = ck.watching.lock().unwrap();
    let Some((_, at, v)) = w.get_mut(&site) else { return };
    f(v);
    if let Some(Err(fail)) = v.verdict(at.elapsed().as_secs()) {
        w.remove(&site);
        ck.failed.lock().unwrap().insert(site, fail);
    }
}

/// 30 s after the first visit: if the tab is still on the site and nothing bad happened, it's safe from now on.
fn finish_check(app: &AppHandle, site: &str) {
    let ck = app.state::<Checks>();
    let Some((label, at, v)) = ck.watching.lock().unwrap().remove(site) else { return };
    let still_there = app.get_webview(&label).and_then(|w| w.url().ok()).is_some_and(|u| core::trust::site_of(u.as_str()).as_deref() == Some(site));
    if !still_there { return; } // left early: check again next visit
    match v.verdict(at.elapsed().as_secs()) {
        Some(Ok(())) => {
            app.state::<Trusted>().0.lock().unwrap().add_checked(site);
            lib(app, |l| if !l.checked.iter().any(|d| d == site) { l.checked.push(site.to_string()) });
            save_library_soon(app);
            if let Some((win, _)) = parse_label(app, &label) { emit(app, &win); }
        }
        Some(Err(f)) => { ck.failed.lock().unwrap().insert(site.to_string(), f); }
        None => {}
    }
}

/// The ⚠ beside the address: why the site is unverified, and "Trust this site" (not offered in incognito,
/// where it would be saved to disk and reveal the visit).
#[tauri::command]
async fn trust_menu(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?;
    let win = window.label().to_string();
    let Some((id, url)) = with(&app, &win, |l| l.active.and_then(|a| l.get(a).map(|t| (a, t.url.clone())))) else { return Ok(()) };
    let Some(domain) = core::trust::site_of(&url) else { return Ok(()) };
    let info = |text: &str| MenuItemBuilder::new(text).enabled(false).build(&app);
    let trust = if is_incognito(&win) { format!("Trust {domain} (not in incognito)") } else { format!("Trust {domain}") };
    let ck = app.state::<Checks>();
    let status = if let Some(f) = ck.failed.lock().unwrap().get(&domain) {
        format!("Safety check failed: {}.", f.reason())
    } else if let Some((_, at, _)) = ck.watching.lock().unwrap().get(&domain) {
        format!("Safety check running: about {} s left.", core::check::WATCH_SECS.saturating_sub(at.elapsed().as_secs()))
    } else if is_incognito(&win) {
        "Sites aren't safety-checked in incognito.".to_string()
    } else {
        "Safety check starts when the page finishes loading.".to_string()
    };
    if trust_of(&app, &url) == core::trust::Trust::Checked {
        let menu = tauri::menu::MenuBuilder::new(&app)
            .item(&info(&format!("{domain} passed R-Browser's safety check, so it runs at full speed")).map_err(err)?)
            .item(&info("But it isn't a well-known site, and a brand-new phishing site can pass the check too.").map_err(err)?)
            .item(&info("Check the address before you sign in or pay.").map_err(err)?)
            .separator()
            .item(&MenuItemBuilder::with_id(format!("tr|{win}|trust|{id}|{domain}"), trust).enabled(!is_incognito(&win)).build(&app).map_err(err)?)
            .build().map_err(err)?;
        return window.popup_menu(&menu).map_err(err);
    }
    let menu = tauri::menu::MenuBuilder::new(&app)
        .item(&info(&format!("R-Browser doesn't know {domain}")).map_err(err)?)
        .item(&info("It isn't one of the 10,000 most-visited sites and you haven't trusted it.").map_err(err)?)
        .item(&info("Until it passes a safety check it runs locked down: JavaScript is slower, and video calls, games and offline storage are off.").map_err(err)?)
        .item(&info(&status).map_err(err)?)
        .separator()
        // The domain shown is in the id: if the tab moves to another site before the click, nothing is trusted.
        .item(&MenuItemBuilder::with_id(format!("tr|{win}|trust|{id}|{domain}"), trust).enabled(!is_incognito(&win)).build(&app).map_err(err)?)
        .build().map_err(err)?;
    window.popup_menu(&menu).map_err(err)
}

fn trust_site(app: &AppHandle, win: &str, id: TabId, shown: &str) -> Res {
    if is_incognito(win) { return Ok(()); }
    let url = with(app, win, |l| l.get(id).map(|t| t.url.clone())).ok_or("no such tab")?;
    if core::trust::site_of(&url).as_deref() != Some(shown) { return Ok(()); }
    let Some(d) = app.state::<Trusted>().0.lock().unwrap().trust(&url) else { return Ok(()) };
    lib(app, |l| if !l.trusted.contains(&d) { l.trusted.push(d) });
    save_library_soon(app);
    emit(app, win);
    Ok(())
}

// ---- ad / tracker blocking ----

fn is_tab_label(l: &str) -> bool { l.starts_with("tab-") || l.starts_with("itab-") }

/// Blocking is on for every tab except sites the user switched it off for (never off in incognito).
fn sync_blocking(app: &AppHandle, wv: &Webview, url: &str) {
    platform::set_blocking(wv, is_incognito_label(wv.label()) || lib(app, |l| l.blocking_on(url)));
}

/// Load (or, once per list version, compile) the bundled filter lists, then switch blocking on in every open tab.
fn start_blocker(app: &AppHandle) {
    use std::hash::{Hash, Hasher};
    let lists = blocker::bundled();
    let mut h = std::hash::DefaultHasher::new();
    (blocker::FORMAT, &lists).hash(&mut h); // a new FORMAT recompiles lists that are already on disk
    let id = format!("rb-{:016x}", h.finish());
    let a = app.clone();
    platform::load_rules(app.clone(), id, move || blocker::Blocker::from_lists(&lists).webkit_rules_json(), move || {
        for wv in a.webviews().into_values() {
            if wv.label().ends_with("-ui") { platform::set_blocking(&wv, true); continue; } // it loads site icons
            if !is_tab_label(wv.label()) { continue; }
            // The tab's own record, not wv.url(): wry panics on a web view that never committed a page (a tab
            // whose first load became a download), and this runs right at startup.
            let Some((win, id)) = parse_label(&a, wv.label()) else { continue };
            let url = with(&a, &win, |l| l.get_mut(id).map(|t| t.url.clone())).unwrap_or_default();
            sync_blocking(&a, &wv, &url);
        }
    });
}

#[tauri::command]
async fn shield_menu(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?;
    let win = window.label().to_string();
    let Some((id, url)) = with(&app, &win, |l| l.active.and_then(|a| l.get(a).map(|t| (a, t.url.clone())))) else { return Ok(()) };
    let Some(host) = site(&url) else { return Ok(()) };
    let on = is_incognito(&win) || lib(&app, |l| l.blocking_on(&url));
    // Incognito can't change site settings: they'd be saved to disk and reveal where you went.
    let text = if is_incognito(&win) { format!("Block ads and trackers on {host} (always on in incognito)") } else { format!("Block ads and trackers on {host}") };
    let menu = tauri::menu::MenuBuilder::new(&app)
        .item(&CheckMenuItemBuilder::with_id(format!("b|{win}|toggle|{id}"), text).checked(on).enabled(!is_incognito(&win)).build(&app).map_err(err)?)
        .build().map_err(err)?;
    window.popup_menu(&menu).map_err(err)
}

/// Turn blocking on/off for the tab's site, then reload so the page loads with the new setting.
fn toggle_blocking(app: &AppHandle, win: &str, id: TabId) -> Res {
    if is_incognito(win) { return Ok(()); }
    let url = with(app, win, |l| l.get(id).map(|t| t.url.clone())).ok_or("no such tab")?;
    lib(app, |l| l.toggle_blocking(&url));
    save_library_soon(app);
    let wv = tab_webview(app, win, id)?;
    sync_blocking(app, &wv, &url);
    reload_tab(app, win, id)
}

#[tauri::command]
async fn popup_allow(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?;
    let Some((page, url)) = app.state::<Runtime>().popups.lock().unwrap().remove(window.label()) else { return Ok(()) };
    // The prompt named the site in the current tab; if that changed since, Allow doesn't apply to it.
    let cur = active_url(&app, window.label()).map(|(u, _)| u).unwrap_or_default();
    if site(&cur).is_none() || site(&cur) != site(&page) { return Ok(()); }
    if !is_incognito(window.label()) {
        lib(&app, |l| l.allow_popups(&page));
        save_library_soon(&app);
    }
    open_tab(&app, window.label(), &url)
}

#[tauri::command]
async fn popup_dismiss(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?;
    app.state::<Runtime>().popups.lock().unwrap().remove(window.label());
    Ok(())
}

fn open_tab(app: &AppHandle, win: &str, url: &str) -> Res {
    let id = with(app, win, |l| Some(l.open(url, is_incognito(win)))).ok_or("no such window")?;
    create_webview(app, win, id, url).map_err(err)?;
    activate(app, win, id)
}

fn activate(app: &AppHandle, win: &str, id: TabId) -> Res {
    let found = with(app, win, |l| {
        l.activate(id);
        l.get_mut(id).map(|t| (t.url.clone(), std::mem::replace(&mut t.frozen, false), t.scroll_y))
    });
    let Some((url, was_frozen, y)) = found else { return Ok(()) };
    if was_frozen || app.get_webview(&label(win, id)).is_none() {
        if y > 0.0 { app.state::<Runtime>().scroll_to.lock().unwrap().insert(label(win, id), y); }
        create_webview(app, win, id, &url).map_err(err)?;
    }
    // Show the new page before hiding the old one, so there's never a frame with no page (or the wrong one) on top.
    if let Some(wv) = app.get_webview(&label(win, id)) { let _ = wv.show(); }
    for other in tab_ids(app, win).into_iter().filter(|&o| o != id) {
        if let Some(wv) = app.get_webview(&label(win, other)) { let _ = wv.hide(); }
    }
    throttle(app, win);
    emit(app, win);
    Ok(())
}

/// Background tabs keep their page but get fewer resources (platform::set_background). Applied on every tab
/// switch; this 30 s pass catches tabs that started or stopped playing sound.
fn start_throttler(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(30));
        let wins: Vec<String> = app.state::<Browser>().0.lock().unwrap().keys().cloned().collect();
        for win in wins { throttle(&app, &win); }
    });
}

fn throttle(app: &AppHandle, win: &str) {
    let plan: Vec<(TabId, bool)> = with(app, win, |l| l.tabs.iter().map(|t| (t.id, core::tabs::should_throttle(t, l.active))).collect());
    // Background first, the tab on screen last: if WebKit ever shares one process between them, it ends up normal.
    for (id, bg) in plan.iter().filter(|p| p.1).chain(plan.iter().filter(|p| !p.1)) {
        if let Some(wv) = app.get_webview(&label(win, *id)) { platform::set_background(&wv, *bg); }
    }
}

// ---- auto-update (signed builds from GitHub Releases; see tauri.conf.json plugins.updater) ----

/// A downloaded, signature-checked update waiting for "Restart to update".
struct UpdateReady(Mutex<Option<(tauri_plugin_updater::Update, Vec<u8>)>>);

fn update_pill(app: &AppHandle, ui: &str) {
    let v = app.state::<UpdateReady>().0.lock().unwrap().as_ref().map(|(u, _)| u.version.clone());
    if let Some(v) = v { let _ = app.emit_to(ui, "update-ready", v); }
}

/// Check, download and verify in the background; the toolbar then shows the pill. `manual` = from the menu:
/// also say "up to date" or what went wrong.
async fn check_update(app: AppHandle, manual: bool) {
    use tauri_plugin_updater::UpdaterExt;
    let say = |msg: String| { app.dialog().message(msg).title("R-Browser updates").blocking_show(); };
    if app.state::<UpdateReady>().0.lock().unwrap().is_some() {
        if manual { ask_restart(&app); }
        return;
    }
    let found = match app.updater() { Ok(u) => u.check().await, Err(e) => Err(e) };
    let r = match found {
        Ok(Some(u)) => match u.download(|_, _| {}, || {}).await.and_then(|b| u.verify(&b).map(|_| b)) {
            Ok(bytes) => {
                *app.state::<UpdateReady>().0.lock().unwrap() = Some((u, bytes));
                for ui in app.webviews().into_keys().filter(|l| l.ends_with("-ui")) { update_pill(&app, &ui); }
                if manual { ask_restart(&app); }
                return;
            }
            Err(e) => format!("The update couldn't be downloaded: {e}"),
        },
        Ok(None) => format!("R-Browser {} is up to date.", app.package_info().version),
        Err(e) => format!("Couldn't check for updates: {e}"),
    };
    if manual { say(r); } else { eprintln!("update: {r}"); }
}

fn ask_restart(app: &AppHandle) {
    let v = app.state::<UpdateReady>().0.lock().unwrap().as_ref().map(|(u, _)| u.version.clone()).unwrap_or_default();
    let yes = app.dialog().message(format!("R-Browser {v} is ready. Restart now? Your tabs come back."))
        .title("Update ready").buttons(MessageDialogButtons::OkCancelCustom("Restart".into(), "Later".into())).blocking_show();
    if yes { let _ = install_update(app); }
}

fn install_update(app: &AppHandle) -> Res {
    let Some((u, bytes)) = app.state::<UpdateReady>().0.lock().unwrap().take() else { return Ok(()) };
    save_session_now(app);
    // The signature was checked when it downloaded. Windows: on success install() starts the installer and exits.
    let mut r = u.install(&bytes);
    // Windows: a just-written installer is often still locked by the antivirus scan ("Access is denied"): retry.
    for _ in 0..3 {
        if !cfg!(target_os = "windows") || r.is_ok() { break; }
        std::thread::sleep(std::time::Duration::from_secs(2));
        r = u.install(&bytes);
    }
    #[cfg(target_os = "windows")]
    if let Err(e) = &r {
        // Still blocked (usually antivirus, or a declined permission prompt): offer the same installer from the website.
        let go = app.dialog().message(format!("Windows wouldn't start the update installer ({e}). This is usually the antivirus. Download the installer from the website instead? Your tabs and data are kept."))
            .title("R-Browser updates").buttons(MessageDialogButtons::OkCancelCustom("Download".into(), "Not now".into())).blocking_show();
        if go { let _ = std::process::Command::new("explorer.exe").arg("https://github.com/rahul428-2/r-browser/releases/latest/download/R-Browser-Windows-Setup.exe").spawn(); }
        return Ok(());
    }
    r.map_err(err)?;
    app.restart()
}

#[tauri::command]
async fn update_restart(webview: Webview, app: AppHandle) -> Res { trusted(&webview)?;
    install_update(&app).inspect_err(|e| { app.dialog().message(format!("The update couldn't be installed: {e}")).title("R-Browser updates").blocking_show(); })
}

/// 10 s after launch, then daily.
fn start_updater(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(10));
        loop {
            tauri::async_runtime::block_on(check_update(app.clone(), false));
            std::thread::sleep(std::time::Duration::from_secs(24 * 3600));
        }
    });
}

fn close(app: &AppHandle, win: &str, id: TabId) -> Res {
    if let Some(wv) = app.get_webview(&label(win, id)) { let _ = wv.close(); }
    let rt = app.state::<Runtime>();
    rt.zoom.lock().unwrap().remove(&label(win, id));
    rt.committed.lock().unwrap().remove(&label(win, id));
    match with(app, win, |l| l.close(id)) {
        Some(n) => activate(app, win, n),
        None => {
            // Last tab closed -> close the window, like Chrome. Save now: this window drops out of the session.
            if !is_incognito(win) { save_session_now(app); }
            if app.windows().len() <= 1 { app.exit(0); } else if let Some(w) = app.get_window(win) { let _ = w.destroy(); }
            Ok(())
        }
    }
}

/// Tear a tab off into a new window (top-left at `at`), moving the live page: no reload, forms and video keep going.
/// Incognito tabs go to a new incognito window. Groups don't travel.
fn detach(app: &AppHandle, from: &str, id: TabId, at: Option<(f64, f64)>) -> Result<String, String> {
    if tab_ids(app, from).len() < 2 { return Ok(from.to_string()); } // the only tab: the window itself is what moves
    let to = if is_incognito(from) {
        format!("incognito-{}", NEXT_INCOGNITO.fetch_add(1, Ordering::SeqCst))
    } else {
        format!("window-{}", NEXT_WINDOW.fetch_add(1, Ordering::SeqCst))
    };
    let at = at.or_else(|| {
        let w = app.get_window(from)?;
        let p = w.outer_position().ok()?.to_logical::<f64>(w.scale_factor().unwrap_or(1.0));
        Some((p.x + 40.0, p.y + 40.0))
    });
    create_window(app, &to, at).map_err(err)?;
    let tab = with(app, from, |l| l.take(id)).ok_or("no such tab")?;
    with(app, &to, |l| { l.adopt(tab); });
    if let (Some(wv), Some(w)) = (app.get_webview(&label(from, id)), app.get_window(&to)) { wv.reparent(&w).map_err(err)?; }
    match with(app, from, |l| l.active) {
        Some(n) => activate(app, from, n)?,
        None => emit(app, from),
    }
    activate(app, &to, id)?;
    layout(app, &to);
    Ok(to)
}

/// Move tab `id` (from whichever window has it) into window `to`, before `before` / into `group`, live page and all.
/// A window left with no tabs closes.
fn attach(app: &AppHandle, to: &str, id: TabId, before: Option<TabId>, group: Option<GroupId>) -> Res {
    let from = owner(app, id).ok_or("no such tab")?;
    if from != to {
        if is_incognito(&from) != is_incognito(to) { return Err("incognito and normal tabs don't mix".into()); }
        let tab = with(app, &from, |l| l.take(id)).ok_or("no such tab")?;
        with(app, to, |l| { l.adopt(tab); });
        if let (Some(wv), Some(w)) = (app.get_webview(&label(to, id)), app.get_window(to)) { wv.reparent(&w).map_err(err)?; }
        match with(app, &from, |l| l.active) {
            Some(n) => activate(app, &from, n)?,
            None => { if let Some(w) = app.get_window(&from) { let _ = w.destroy(); } }
        }
    }
    with(app, to, |l| l.move_to(id, before, group));
    activate(app, to, id)?;
    layout(app, to);
    if let Some(w) = app.get_window(to) { let _ = w.set_focus(); }
    Ok(())
}

/// Strip height that counts as "drop here" when a torn tab is released over a window (plus a little slack).
const STRIP: f64 = 36.0;

/// The browser window (other than `skip`, same incognito-ness) whose tab strip is under screen point (x, y),
/// with x relative to that window.
fn strip_at(app: &AppHandle, x: f64, y: f64, skip: &str, incognito: bool) -> Option<(String, f64)> {
    app.windows().into_iter().find_map(|(lbl, w)| {
        if lbl == skip || is_incognito(&lbl) != incognito || !w.is_visible().unwrap_or(false) || w.is_minimized().unwrap_or(false) { return None; }
        let f = w.scale_factor().unwrap_or(1.0);
        let (p, s) = (w.inner_position().ok()?.to_logical::<f64>(f), w.inner_size().ok()?.to_logical::<f64>(f));
        (x >= p.x && x < p.x + s.width && y >= p.y - 8.0 && y < p.y + STRIP + 12.0).then(|| (lbl, x - p.x))
    })
}

fn hint(app: &AppHandle, target: Option<(String, f64)>) {
    let rt = app.state::<Runtime>();
    let mut cur = rt.hint.lock().unwrap();
    if let Some(old) = cur.take() {
        if target.as_ref().map(|t| &t.0) != Some(&old) { if let Some(ui) = app.get_webview(&ui_label(&old)) { let _ = ui.eval("dropHint(null)"); } }
    }
    if let Some((w, x)) = target {
        if let Some(ui) = app.get_webview(&ui_label(&w)) { let _ = ui.eval(&format!("dropHint({x})")); }
        *cur = Some(w);
    }
}

/// Dragged out of the strip: the tab gets its own window that follows the cursor (Chrome-style).
/// `ox, oy` = where the cursor should sit inside that window. Returns true if a new window was made.
#[tauri::command]
async fn tear_start(webview: Webview, app: AppHandle, window: Window, id: TabId, x: f64, y: f64, ox: f64, oy: f64) -> Result<bool, String> { trusted(&webview)?;
    let from = window.label().to_string();
    let to = detach(&app, &from, id, Some((x - ox, y - oy)))?;
    *app.state::<Runtime>().tear.lock().unwrap() = Some((to.clone(), id, ox, oy));
    Ok(to != from)
}

#[tauri::command]
async fn tear_move(webview: Webview, app: AppHandle, x: f64, y: f64) -> Res { trusted(&webview)?;
    let Some((to, _, ox, oy)) = app.state::<Runtime>().tear.lock().unwrap().clone() else { return Ok(()) };
    if let Some(w) = app.get_window(&to) { let _ = w.set_position(LogicalPosition::new(x - ox, y - oy)); }
    hint(&app, strip_at(&app, x, y, &to, is_incognito(&to)));
    Ok(())
}

/// Released: over another window's strip -> that window's toolbar picks the exact slot and calls attach_tab;
/// anywhere else -> the tab stays in its own window.
#[tauri::command]
async fn tear_end(webview: Webview, app: AppHandle, x: f64, y: f64) -> Res { trusted(&webview)?;
    let Some((to, id, _, _)) = app.state::<Runtime>().tear.lock().unwrap().take() else { return Ok(()) };
    hint(&app, None);
    match strip_at(&app, x, y, &to, is_incognito(&to)) {
        Some((target, cx)) => if let Some(ui) = app.get_webview(&ui_label(&target)) { ui.eval(&format!("dropForeign({id}, {cx})")).map_err(err)?; },
        None => if let Some(w) = app.get_window(&to) { let _ = w.set_focus(); },
    }
    Ok(())
}

/// From a toolbar: put tab `id` (possibly from another window) into this window.
#[tauri::command]
async fn attach_tab(webview: Webview, app: AppHandle, window: Window, id: TabId, before: Option<TabId>, group: Option<GroupId>) -> Res { trusted(&webview)?;
    attach(&app, window.label(), id, before, group)
}

fn tab_webview(app: &AppHandle, win: &str, id: TabId) -> Result<Webview, String> {
    app.get_webview(&label(win, id)).ok_or_else(|| "no such tab".into())
}
fn eval_in(app: &AppHandle, win: &str, id: TabId, js: &str) -> Res { tab_webview(app, win, id)?.eval(js).map_err(err) }

fn new_incognito(app: &AppHandle) -> Res {
    let win = format!("incognito-{}", NEXT_INCOGNITO.fetch_add(1, Ordering::SeqCst));
    create_window(app, &win, None).map_err(err)
}

/// Keyboard shortcuts typed into this web view (Windows; elsewhere the app menu has them).
fn shortcuts(app: &AppHandle, wv: &Webview) {
    let app = app.clone();
    platform::on_shortcut(wv, move |id| on_menu(&app, id));
}

/// The window the user is looking at (for menu shortcuts).
fn focused(app: &AppHandle) -> String {
    app.windows().into_iter().find(|(_, w)| w.is_focused().unwrap_or(false)).map(|(l, _)| l).unwrap_or_else(|| MAIN.into())
}

fn zoom(app: &AppHandle, win: &str, id: TabId, step: i32) -> Res {
    let lbl = label(win, id);
    let rt = app.state::<Runtime>();
    let mut z = rt.zoom.lock().unwrap();
    let cur = *z.get(&lbl).unwrap_or(&1.0);
    let i = ZOOM_STEPS.iter().position(|&s| (s - cur).abs() < 1e-6).unwrap_or(4) as i32;
    let next = if step == 0 { 1.0 } else { ZOOM_STEPS[(i + step).clamp(0, ZOOM_STEPS.len() as i32 - 1) as usize] };
    z.insert(lbl, next);
    tab_webview(app, win, id)?.set_zoom(next).map_err(err)
}

fn toggle_bookmark_active(app: &AppHandle, win: &str) -> Res {
    let Some((url, title)) = active_url(app, win) else { return Ok(()) };
    if !(url.starts_with("http://") || url.starts_with("https://")) { return Ok(()); }
    lib(app, |l| l.toggle_bookmark(&url, &title, now_secs()));
    save_library_soon(app);
    emit(app, win);
    Ok(())
}

fn focus_ui(app: &AppHandle, win: &str, js: &str) {
    if let Some(ui) = app.get_webview(&ui_label(win)) {
        let _ = ui.set_focus();
        let _ = ui.eval(js);
    }
}

/// Defense in depth on top of Tauri's ACL (which already blocks remote origins): only the browser
/// toolbar and the app's own rb:// pages may call commands. A website must never reach them.
fn trusted(wv: &Webview) -> Result<(), String> {
    if wv.label().ends_with("-ui") { return Ok(()); }
    let url = wv.url().map(|u| u.to_string()).unwrap_or_default();
    if to_rb(&url).is_empty() || to_rb(&url).starts_with("rb://") { Ok(()) } else { Err("not allowed".into()) }
}

/// Tell the OS this file came from the internet, so Gatekeeper / SmartScreen check it before it opens.
fn mark_from_internet(path: &std::path::Path, url: &str) {
    #[cfg(target_os = "macos")]
    {
        let secs = now_secs();
        let _ = std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.quarantine", &format!("0081;{secs:08x};R-Browser;")])
            .arg(path)
            .status();
    }
    #[cfg(target_os = "windows")]
    {
        let zone = format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={url}\r\n");
        let _ = std::fs::write(format!("{}:Zone.Identifier", path.display()), zone);
    }
    let _ = (path, url);
}

// ---- commands (`window` = the window the calling page lives in) ----

#[tauri::command]
async fn ui_ready(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?;
    let win = window.label().to_string();
    if win == MAIN {
        if !RESTORED.load(Ordering::SeqCst) {
            restore(&app)?;
            RESTORED.store(true, Ordering::SeqCst);
        }
        let start: Vec<String> = std::mem::take(&mut *app.state::<StartUrls>().inner().0.lock().unwrap());
        for u in &start { open_tab(&app, &win, &resolve(u))?; }
    }
    if tab_ids(&app, &win).is_empty() { open_tab(&app, &win, "")?; }
    layout(&app, &win);
    emit(&app, &win);
    update_pill(&app, &ui_label(&win));
    Ok(())
}

#[tauri::command]
async fn new_tab(webview: Webview, app: AppHandle, window: Window, url: Option<String>) -> Res { trusted(&webview)?;
    let win = window.label();
    open_tab(&app, win, &url.map(|u| resolve(&u)).unwrap_or_default())?;
    if let Some(ui) = app.get_webview(&ui_label(win)) { let _ = ui.set_focus(); }
    Ok(())
}

#[tauri::command]
async fn new_incognito_window(webview: Webview, app: AppHandle) -> Res { trusted(&webview)?; new_incognito(&app) }

#[tauri::command]
async fn close_tab(webview: Webview, app: AppHandle, window: Window, id: TabId) -> Res { trusted(&webview)?; close(&app, window.label(), id) }

#[tauri::command]
async fn activate_tab(webview: Webview, app: AppHandle, window: Window, id: TabId) -> Res { trusted(&webview)?; activate(&app, window.label(), id) }

#[tauri::command]
async fn navigate(webview: Webview, app: AppHandle, window: Window, id: TabId, input: String) -> Res { trusted(&webview)?;
    let win = window.label();
    let url = resolve(&input);
    with(&app, win, |l| if let Some(t) = l.get_mut(id) { t.url = url.clone(); });
    let wv = tab_webview(&app, win, id)?;
    let dest = match target(&url) {
        WebviewUrl::External(u) => u,
        WebviewUrl::App(_) => page_url(&app, &url).ok_or("bad page")?,
        _ => return Err("bad url".into()),
    };
    app.state::<Runtime>().user_nav.lock().unwrap().insert(dest.to_string());
    wv.navigate(dest).map_err(err)?;
    let _ = wv.set_focus();
    emit(&app, win);
    Ok(())
}

#[tauri::command]
async fn go_back(webview: Webview, app: AppHandle, window: Window, id: TabId) -> Res { trusted(&webview)?; eval_in(&app, window.label(), id, "history.back()") }

#[tauri::command]
async fn go_forward(webview: Webview, app: AppHandle, window: Window, id: TabId) -> Res { trusted(&webview)?; eval_in(&app, window.label(), id, "history.forward()") }

#[tauri::command]
async fn reload(webview: Webview, app: AppHandle, window: Window, id: TabId) -> Res { trusted(&webview)?; reload_tab(&app, window.label(), id) }

#[tauri::command]
async fn toggle_bookmark(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?; toggle_bookmark_active(&app, window.label()) }

#[tauri::command]
async fn find_in_page(webview: Webview, app: AppHandle, window: Window, id: TabId, query: String, backwards: bool) -> Res { trusted(&webview)?;
    // JSON-encode the query so it can't break out of the JS string.
    let js = format!("window.find({}, false, {backwards}, true)", serde_json::to_string(&query).map_err(err)?);
    eval_in(&app, window.label(), id, &js)
}

#[tauri::command]
fn suggest(webview: Webview, app: AppHandle, query: String) -> Result<Vec<Suggestion>, String> { trusted(&webview)?; Ok(lib(&app, |l| l.suggest(&query, 8))) }

#[tauri::command]
fn history_search(webview: Webview, app: AppHandle, query: String) -> Result<Vec<Visit>, String> { trusted(&webview)?; Ok(lib(&app, |l| l.search_history(&query, 500).into_iter().cloned().collect())) }

#[tauri::command]
fn history_clear(webview: Webview, app: AppHandle) -> Res { trusted(&webview)?;
    lib(&app, |l| l.clear_history());
    app.state::<Trusted>().0.lock().unwrap().clear_checked();
    save_library_soon(&app);
    Ok(())
}

#[tauri::command]
fn bookmarks_list(webview: Webview, app: AppHandle) -> Result<Vec<Bookmark>, String> { trusted(&webview)?; Ok(lib(&app, |l| l.bookmarks.iter().rev().cloned().collect())) }

#[tauri::command]
fn bookmark_remove(webview: Webview, app: AppHandle, url: String) -> Res {
    trusted(&webview)?;
    lib(&app, |l| l.bookmarks.retain(|b| b.url != url));
    save_library_soon(&app);
    Ok(())
}

#[tauri::command]
fn downloads_list(webview: Webview, app: AppHandle) -> Result<Vec<DownloadRow>, String> { trusted(&webview)?;
    let rt = app.state::<Runtime>();
    let live = rt.live.lock().unwrap();
    Ok(lib(&app, |l| l.downloads.iter().rev().map(|d| {
        let now = if d.done { None } else { live.get(&d.url) };
        DownloadRow { d: d.clone(), got: now.map(|l| l.got), pct: now.and_then(|l| l.pct), eta: now.and_then(|l| l.eta) }
    }).collect()))
}

#[derive(serde::Serialize)]
struct DownloadRow { #[serde(flatten)] d: Download, got: Option<u64>, pct: Option<u8>, eta: Option<u64> }

/// Reveal a finished download in the file manager. Never opens/executes the file itself.
#[tauri::command]
fn show_download(webview: Webview, app: AppHandle, path: String) -> Res { trusted(&webview)?;
    let known = lib(&app, |l| l.downloads.iter().any(|d| d.path == path));
    if !known { return Err("not a download".into()); }
    let p = PathBuf::from(&path);
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(&p).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
    #[cfg(target_os = "linux")]
    let r = std::process::Command::new("xdg-open").arg(p.parent().unwrap_or(&p)).spawn();
    r.map(|_| ()).map_err(err)
}


// ---- tab groups ----

fn group_label(l: &TabList, g: GroupId) -> String {
    l.groups.iter().find(|x| x.id == g).map(|x| {
        let n = l.tabs.iter().filter(|t| t.group == Some(g)).count();
        if x.name.is_empty() { format!("{} group · {n} tab{}", cap(&x.color), if n == 1 { "" } else { "s" }) } else { x.name.clone() }
    }).unwrap_or_default()
}
fn cap(s: &str) -> String { let mut c = s.chars(); c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default() }

/// After collapsing, the active tab may be hidden: move to the nearest visible tab, or a new one.
fn ensure_visible_active(app: &AppHandle, win: &str) -> Res {
    let (active, next) = with(app, win, |l| match l.active {
        Some(a) if !l.visible(a) => (Some(a), l.nearest_visible(a)),
        _ => (None, None),
    });
    match (active, next) {
        (Some(_), Some(n)) => activate(app, win, n),
        (Some(_), None) => open_tab(app, win, ""),
        _ => { emit(app, win); Ok(()) }
    }
}

fn close_group(app: &AppHandle, win: &str, g: GroupId) -> Res {
    let ids: Vec<TabId> = with(app, win, |l| l.tabs.iter().filter(|t| t.group == Some(g)).map(|t| t.id).collect());
    for id in ids { close(app, win, id)?; }
    Ok(())
}

#[tauri::command]
async fn tab_menu(webview: Webview, app: AppHandle, window: Window, id: TabId) -> Res { trusted(&webview)?;
    let win = window.label().to_string();
    let (cur, groups) = with(&app, &win, |l| (l.get(id).map(|t| t.group), l.groups.iter().map(|g| (g.id, group_label(l, g.id))).collect::<Vec<_>>()));
    let Some(cur) = cur else { return Ok(()) };
    let item = |act: &str, text: &str| MenuItemBuilder::with_id(format!("t|{win}|{act}|{id}"), text).build(&app);
    let mut m = tauri::menu::MenuBuilder::new(&app).item(&item("new", "Add Tab to New Group").map_err(err)?);
    let others: Vec<_> = groups.into_iter().filter(|(g, _)| Some(*g) != cur).collect();
    if !others.is_empty() {
        let mut sub = SubmenuBuilder::new(&app, "Add Tab to Group");
        for (g, name) in others { sub = sub.item(&MenuItemBuilder::with_id(format!("t|{win}|add|{id}|{g}"), name).build(&app).map_err(err)?); }
        m = m.item(&sub.build().map_err(err)?);
    }
    if cur.is_some() { m = m.item(&item("remove", "Remove from Group").map_err(err)?); }
    if tab_ids(&app, &win).len() > 1 { m = m.item(&item("newwin", "Move Tab to New Window").map_err(err)?); }
    let menu = m.separator().item(&item("close", "Close Tab").map_err(err)?).build().map_err(err)?;
    window.popup_menu(&menu).map_err(err)
}

#[tauri::command]
async fn group_menu(webview: Webview, app: AppHandle, window: Window, gid: GroupId) -> Res { trusted(&webview)?;
    let win = window.label().to_string();
    let Some(color) = with(&app, &win, |l| l.groups.iter().find(|g| g.id == gid).map(|g| g.color.clone())) else { return Ok(()) };
    let item = |act: &str, text: &str| MenuItemBuilder::with_id(format!("g|{win}|{act}|{gid}"), text).build(&app);
    let mut colors = SubmenuBuilder::new(&app, "Colour");
    for c in COLORS {
        colors = colors.item(&CheckMenuItemBuilder::with_id(format!("g|{win}|color|{gid}|{c}"), cap(c)).checked(c == color).build(&app).map_err(err)?);
    }
    let menu = tauri::menu::MenuBuilder::new(&app)
        .item(&item("rename", "Rename Group…").map_err(err)?)
        .item(&colors.build().map_err(err)?)
        .separator()
        .item(&item("ungroup", "Ungroup").map_err(err)?)
        .item(&item("close", "Close Group").map_err(err)?)
        .build().map_err(err)?;
    window.popup_menu(&menu).map_err(err)
}

#[tauri::command]
async fn group_toggle(webview: Webview, app: AppHandle, window: Window, gid: GroupId) -> Res { trusted(&webview)?;
    let win = window.label();
    with(&app, win, |l| { let c = l.groups.iter().any(|g| g.id == gid && g.collapsed); l.set_collapsed(gid, !c); });
    ensure_visible_active(&app, win)
}

/// Drag & drop in the tab strip. The core keeps groups contiguous whatever the UI asks for.
#[tauri::command]
async fn move_tab(webview: Webview, app: AppHandle, window: Window, id: TabId, before: Option<TabId>, group: Option<GroupId>) -> Res { trusted(&webview)?;
    let win = window.label();
    with(&app, win, |l| l.move_to(id, before, group));
    emit(&app, win);
    Ok(())
}

#[tauri::command]
async fn group_rename(webview: Webview, app: AppHandle, window: Window, gid: GroupId, name: String) -> Res { trusted(&webview)?;
    let win = window.label();
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(40).collect();
    with(&app, win, |l| if let Some(g) = l.group_mut(gid) { g.name = name; });
    emit(&app, win);
    Ok(())
}

/// macOS gives focus back to the page as the popup menu closes, so open the name box a moment later.
fn rename_soon(app: &AppHandle, win: &str, g: GroupId) {
    let (app, win) = (app.clone(), win.to_string());
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(250));
        focus_ui(&app, &win, &format!("startRename({g})"));
    });
}

/// Ids of our native popup menus: `t|win|action|tab[|group]`, `g|win|action|group[|colour]`, `pw|win|tab|username`.
fn on_popup(app: &AppHandle, id: &str) -> Res {
    let p: Vec<&str> = id.splitn(5, '|').collect();
    let (kind, win) = (p[0], p.get(1).copied().unwrap_or_default());
    let num = |i: usize| p.get(i).and_then(|x| x.parse::<u64>().ok()).ok_or("bad menu id");
    match (kind, p.get(2).copied().unwrap_or_default()) {
        ("b", "toggle") => toggle_blocking(app, win, num(3)?)?,
        ("tr", "trust") => trust_site(app, win, num(3)?, p.get(4).copied().unwrap_or_default())?,
        ("pw", _) => return fill_password(app, win, num(2)?, id.splitn(4, '|').nth(3).unwrap_or_default()),
        ("t", "new") => {
            let g = with(app, win, |l| l.new_group(num(3).unwrap_or(0)));
            emit(app, win);
            if let Some(g) = g { rename_soon(app, win, g); }
        }
        ("t", "add") => { let (t, g) = (num(3)?, num(4)?); with(app, win, |l| l.add_to_group(t, g)); emit(app, win); }
        ("t", "remove") => { let t = num(3)?; with(app, win, |l| l.remove_from_group(t)); emit(app, win); }
        ("t", "close") => close(app, win, num(3)?)?,
        ("t", "newwin") => {
            let to = detach(app, win, num(3)?, None)?;
            if let Some(w) = app.get_window(&to) { let _ = w.set_focus(); }
        }
        ("g", "rename") => rename_soon(app, win, num(3)?),
        ("g", "color") => {
            let (g, c) = (num(3)?, p.get(4).copied().unwrap_or_default());
            if COLORS.contains(&c) { with(app, win, |l| if let Some(x) = l.group_mut(g) { x.color = c.into(); }); }
            emit(app, win);
        }
        ("g", "ungroup") => { let g = num(3)?; with(app, win, |l| l.ungroup(g)); emit(app, win); }
        ("g", "close") => close_group(app, win, num(3)?)?,
        _ => {}
    }
    Ok(())
}

// ---- password vault ----

/// Run `f` on the decrypted vault. Asks for Touch ID / the login password if the 5-minute unlock has expired.
/// `f` returns (result, changed); a changed vault is re-sealed with a fresh nonce and written atomically.
fn with_vault<R>(app: &AppHandle, reason: &str, f: impl FnOnce(&mut Vault) -> (R, bool)) -> Result<R, String> {
    let rt = app.state::<VaultRt>();
    let _one = rt.lock.lock().unwrap();
    if now_secs() >= rt.unlocked_until.load(Ordering::SeqCst) {
        if !platform::authenticate(reason) { return Err("locked".into()); }
        rt.unlocked_until.store(now_secs() + UNLOCK_SECS, Ordering::SeqCst);
    }
    let path = data_file(app, "vault.bin").ok_or("no app data folder")?;
    let blob = std::fs::read(&path).ok();
    let key = platform::vault_key(blob.is_none())?;
    let mut v = match &blob { Some(b) => Vault::open(b, &key)?, None => Vault::default() };
    let (r, changed) = f(&mut v);
    if changed { store::write_atomic(&path, &v.seal(&key)).map_err(err)?; }
    Ok(r)
}

fn login_hash(app: &AppHandle, origin: &str, user: &str, pass: &str) -> u64 {
    use std::hash::BuildHasher;
    app.state::<VaultRt>().hasher.hash_one((origin, user, pass))
}

/// The only command a website can call. Which tab and which site come from the webview, never from `ev`.
#[derive(Deserialize)]
struct PageEvent { kind: String, #[serde(default)] username: String, #[serde(default)] password: String, #[serde(default)] url: String,
    #[serde(default)] dirty: bool, #[serde(default)] on: bool, #[serde(default)] y: f64 }

#[tauri::command]
async fn page_event(webview: Webview, app: AppHandle, ev: PageEvent) -> Res {
    let (win, id) = parse_label(&app, webview.label()).ok_or("not a tab")?;
    let page = webview.url().map(|u| u.to_string()).unwrap_or_default();
    if ev.kind == "popup-blocked" {
        let ok = ev.url.len() < 4096 && url::Url::parse(&ev.url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
        if ok { note_bad(&app, &page, |v| v.popups += 1); }
        // Only the tab you're looking at can raise the prompt, so a background tab can't swap its pop-up in behind it.
        if !ok || with(&app, &win, |l| l.active) != Some(id) { return Ok(()); }
        if lib(&app, |l| l.popups_ok(&page)) { return open_tab(&app, &win, &ev.url); }
        let host = site(&page).unwrap_or_default();
        app.state::<Runtime>().popups.lock().unwrap().insert(win.clone(), (page, ev.url));
        let _ = app.emit_to(ui_label(&win), "popup-blocked", (win, host));
        return Ok(());
    }
    // Freezing inputs (state.js). Not emitted: nothing on screen changes.
    match ev.kind.as_str() {
        "dirty-form" => { with(&app, &win, |l| if let Some(t) = l.get_mut(id) { t.dirty_form = ev.dirty; }); return Ok(()); }
        "audible" => { with(&app, &win, |l| if let Some(t) = l.get_mut(id) { t.audible = ev.on; }); return Ok(()); }
        "scroll" if ev.y.is_finite() => {
            with(&app, &win, |l| if let Some(t) = l.get_mut(id) { t.scroll_y = ev.y.clamp(0.0, 1e7); });
            if !is_incognito(&win) { save_session_soon(&app); }
            return Ok(());
        }
        _ => {}
    }
    if is_incognito(&win) { return Ok(()); } // incognito tabs show no site icons (no fetches that outlive the window)
    if ev.kind == "icon" {
        // Only from a finished page that is the site the tab is on: while a tab is loading, the page being left
        // can still send events, and its icon must not end up on the next site's tab.
        let changed = core::tabs::valid_icon(&ev.url) && with(&app, &win, |l| l.get_mut(id).is_some_and(|t| {
            let ok = !t.loading && origin_of(&t.url).is_some() && origin_of(&t.url) == origin_of(&page) && t.icon != ev.url;
            if ok { t.icon = ev.url.clone(); }
            ok
        }));
        if changed { emit(&app, &win); }
        return Ok(());
    }
    let Some(origin) = origin_of(&page) else { return Ok(()) };
    match ev.kind.as_str() {
        "login-form" => {
            with(&app, &win, |l| if let Some(t) = l.get_mut(id) { t.login_form = true; });
            emit(&app, &win);
        }
        "login-submitted" if !ev.password.is_empty() && ev.password.len() < 1024 && ev.username.len() < 1024 => {
            if lib(&app, |l| l.never_save.contains(&origin)) { return Ok(()); }
            if app.state::<VaultRt>().filled.lock().unwrap().contains(&login_hash(&app, &origin, &ev.username, &ev.password)) { return Ok(()); }
            let host = url::Url::parse(&origin).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
            let user = ev.username.clone();
            app.state::<VaultRt>().pending.lock().unwrap().insert(win.clone(), Entry { origin, username: ev.username, password: ev.password, note: String::new() });
            let _ = app.emit_to(ui_label(&win), "save-password", (win, host, user));
        }
        _ => {}
    }
    Ok(())
}

fn take_pending(app: &AppHandle, win: &str) -> Option<Entry> { app.state::<VaultRt>().pending.lock().unwrap().remove(win) }

#[tauri::command]
async fn password_save(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?;
    let Some(e) = take_pending(&app, window.label()) else { return Ok(()) };
    with_vault(&app, "save a password", |v| { v.add(e); ((), true) })
}

#[tauri::command]
async fn password_never(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?;
    let Some(e) = take_pending(&app, window.label()) else { return Ok(()) };
    lib(&app, |l| if !l.never_save.contains(&e.origin) { l.never_save.push(e.origin.clone()); });
    save_library_soon(&app);
    Ok(())
}

#[tauri::command]
async fn password_dismiss(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?; take_pending(&app, window.label()); Ok(()) }

/// Key icon: unlock, then a native menu of saved usernames for this exact origin.
#[tauri::command]
async fn password_menu(webview: Webview, app: AppHandle, window: Window) -> Res { trusted(&webview)?;
    let win = window.label().to_string();
    let id = with(&app, &win, |l| l.active).ok_or("no tab")?;
    let origin = tab_webview(&app, &win, id)?.url().ok().and_then(|u| origin_of(u.as_str())).ok_or("not a website")?;
    let users = with_vault(&app, "fill a saved password", |v| (v.find(&origin).iter().map(|e| e.username.clone()).collect::<Vec<_>>(), false))?;
    let mut m = tauri::menu::MenuBuilder::new(&app);
    if users.is_empty() {
        m = m.item(&MenuItemBuilder::new("No saved passwords for this site").enabled(false).build(&app).map_err(err)?);
    }
    for u in users {
        let text = if u.is_empty() { "(no username)".to_string() } else { u.clone() };
        m = m.item(&MenuItemBuilder::with_id(format!("pw|{win}|{id}|{u}"), text).build(&app).map_err(err)?);
    }
    window.popup_menu(&m.build().map_err(err)?).map_err(err)
}

fn fill_password(app: &AppHandle, win: &str, id: TabId, username: &str) -> Res {
    let wv = tab_webview(app, win, id)?;
    let origin = wv.url().ok().and_then(|u| origin_of(u.as_str())).ok_or("not a website")?;
    let Some(e) = with_vault(app, "fill a saved password", |v| (v.find(&origin).into_iter().find(|e| e.username == username).cloned(), false))? else { return Ok(()) };
    app.state::<VaultRt>().filled.lock().unwrap().insert(login_hash(app, &e.origin, &e.username, &e.password));
    let args = serde_json::to_string(&(&e.origin, &e.username, &e.password)).map_err(err)?;
    // The page re-checks its own origin, so a navigation between our check and this script can't leak the password.
    let js = format!(r#"(([o, u, p]) => {{
  if (location.origin !== o) return;
  const vis = (e) => !!(e.offsetWidth || e.offsetHeight || e.getClientRects().length);
  const set = (el, v) => {{ Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(el, v);
    el.dispatchEvent(new Event("input", {{ bubbles: true }})); el.dispatchEvent(new Event("change", {{ bubbles: true }})); }};
  const all = [...document.querySelectorAll("input")];
  const pw = all.find((e) => e.type === "password" && vis(e)) || all.find((e) => e.type === "password");
  const user = (pw ? all.slice(0, all.indexOf(pw)).reverse() : all).find((e) => vis(e) && /^(text|email|tel)$/.test(e.type));
  if (user && u) set(user, u);
  if (pw) set(pw, p);
  (pw || user)?.focus();
}})({args})"#);
    wv.eval(&js).map_err(err)?;
    let _ = wv.set_focus();
    Ok(())
}

#[derive(Serialize)]
struct Login { origin: String, username: String, note: String }

#[tauri::command]
async fn passwords_list(webview: Webview, app: AppHandle) -> Result<Vec<Login>, String> { trusted(&webview)?;
    with_vault(&app, "see your saved passwords", |v| {
        let mut out: Vec<Login> = v.entries.iter().map(|e| Login { origin: e.origin.clone(), username: e.username.clone(), note: e.note.clone() }).collect();
        out.sort_by(|a, b| (&a.origin, &a.username).cmp(&(&b.origin, &b.username)));
        (out, false)
    })
}

#[tauri::command]
async fn password_reveal(webview: Webview, app: AppHandle, origin: String, username: String) -> Result<String, String> { trusted(&webview)?;
    with_vault(&app, "show a saved password", |v| (v.find(&origin).into_iter().find(|e| e.username == username).map(|e| e.password.clone()).unwrap_or_default(), false))
}

#[tauri::command]
async fn password_delete(webview: Webview, app: AppHandle, origin: String, username: String) -> Res { trusted(&webview)?;
    with_vault(&app, "delete a saved password", |v| { v.remove(&origin, &username); ((), true) })
}

/// Pick a Google Passwords CSV, import it, then offer to wipe the CSV (it holds every password in plain text).
#[tauri::command]
async fn passwords_import(webview: Webview, app: AppHandle) -> Result<usize, String> { trusted(&webview)?;
    let Some(file) = app.dialog().file().set_title("Choose your Google Passwords CSV").add_filter("CSV", &["csv"]).blocking_pick_file() else { return Ok(0) };
    let path = file.into_path().map_err(err)?;
    let mut text = std::fs::read_to_string(&path).map_err(err)?;
    let parsed = vault::import_google_csv(&text);
    zeroize::Zeroize::zeroize(&mut text);
    let entries = parsed?;
    let n = entries.len();
    with_vault(&app, "import passwords", |v| { for e in entries { v.add(e); } ((), true) })?;
    let wipe = app.dialog()
        .message(format!("Imported {n} passwords.\n\nThe CSV file still holds every password in plain text. Delete it now?\n\n(It's overwritten before deleting, but SSDs and backups like Time Machine can still keep old copies.)"))
        .title("Delete the CSV file?")
        .buttons(MessageDialogButtons::OkCancelCustom("Delete CSV".into(), "Keep".into()))
        .blocking_show();
    if wipe { vault::shred(&path).map_err(err)?; }
    Ok(n)
}

// ---- native menu: shortcuts that work even when a web page has focus ----

#[cfg_attr(target_os = "windows", allow(dead_code))]
fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let item = |id: &str, text: &str, acc: &str| MenuItemBuilder::with_id(id, text).accelerator(acc).build(app);
    let tab_menu = SubmenuBuilder::new(app, "Tab")
        .item(&item("new-tab", "New Tab", "CmdOrCtrl+T")?)
        .item(&item("new-window", "New Window", "CmdOrCtrl+N")?)
        .item(&item("new-incognito", "New Incognito Window", "CmdOrCtrl+Shift+N")?)
        .item(&item("close-tab", "Close Tab", "CmdOrCtrl+W")?)
        .separator()
        .item(&item("focus-url", "Open Location", "CmdOrCtrl+L")?)
        .item(&item("reload", "Reload", "CmdOrCtrl+R")?)
        .item(&item("back", "Back", "CmdOrCtrl+[")?)
        .item(&item("forward", "Forward", "CmdOrCtrl+]")?)
        .item(&item("find", "Find…", "CmdOrCtrl+F")?)
        .item(&item("print", "Print…", "CmdOrCtrl+P")?)
        .separator()
        .item(&item("zoom-in", "Zoom In", "CmdOrCtrl+=")?)
        .item(&item("zoom-out", "Zoom Out", "CmdOrCtrl+-")?)
        .item(&item("zoom-reset", "Actual Size", "CmdOrCtrl+0")?)
        .separator()
        .item(&item("next-tab", "Next Tab", "Ctrl+Tab")?)
        .item(&item("prev-tab", "Previous Tab", "Ctrl+Shift+Tab")?)
        .build()?;
    let library_menu = SubmenuBuilder::new(app, "Library")
        .item(&item("bookmark", "Bookmark This Page", "CmdOrCtrl+D")?)
        .item(&item("bookmarks", "Bookmarks", "CmdOrCtrl+Shift+B")?)
        .item(&item("history", "History", "CmdOrCtrl+Y")?)
        .item(&item("downloads", "Downloads", "CmdOrCtrl+Shift+J")?)
        .item(&item("passwords", "Passwords", "CmdOrCtrl+Alt+P")?)
        .separator()
        .item(&MenuItemBuilder::with_id("check-update", "Check for Updates…").build(app)?)
        .build()?;
    let menu = Menu::default(app)?;
    menu.append(&tab_menu)?;
    menu.append(&library_menu)?;
    Ok(menu)
}

fn on_menu(app: &AppHandle, id: &str) {
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        if id.contains('|') {
            if let Err(e) = on_popup(&app, &id) { eprintln!("menu: {e}"); }
            return;
        }
        let win = focused(&app);
        let cur = with(&app, &win, |l| l.active);
        let r = match (id.as_str(), cur) {
            ("new-tab", _) => {
                let r = open_tab(&app, &win, "");
                focus_ui(&app, &win, "document.getElementById('url').focus()");
                r
            }
            ("new-incognito", _) => new_incognito(&app),
            ("new-window", _) => create_window(&app, &format!("window-{}", NEXT_WINDOW.fetch_add(1, Ordering::SeqCst)), None).map_err(err),
            ("close-window", _) => app.get_window(&win).map_or(Ok(()), |w| w.close().map_err(err)),
            ("check-update", _) => { check_update(app.clone(), true).await; Ok(()) }
            ("close-tab", Some(c)) => close(&app, &win, c),
            ("focus-url", _) => { focus_ui(&app, &win, "document.getElementById('url').focus()"); Ok(()) }
            ("find", _) => { focus_ui(&app, &win, "openFind()"); Ok(()) }
            ("reload", Some(c)) => reload_tab(&app, &win, c),
            ("back", Some(c)) => eval_in(&app, &win, c, "history.back()"),
            ("forward", Some(c)) => eval_in(&app, &win, c, "history.forward()"),
            ("print", Some(c)) => tab_webview(&app, &win, c).and_then(|w| w.print().map_err(err)),
            ("zoom-in", Some(c)) => zoom(&app, &win, c, 1),
            ("zoom-out", Some(c)) => zoom(&app, &win, c, -1),
            ("zoom-reset", Some(c)) => zoom(&app, &win, c, 0),
            ("bookmark", _) => toggle_bookmark_active(&app, &win),
            ("bookmarks" | "history" | "downloads" | "passwords", _) => open_tab(&app, &win, &format!("rb://{id}")),
            ("next-tab" | "prev-tab", Some(c)) => {
                let ids: Vec<TabId> = with(&app, &win, |l| l.tabs.iter().filter(|t| t.id == c || l.visible(t.id)).map(|t| t.id).collect());
                let i = ids.iter().position(|&x| x == c).unwrap_or(0);
                let n = ids.len();
                let j = if id == "next-tab" { (i + 1) % n } else { (i + n - 1) % n };
                activate(&app, &win, ids[j])
            }
            // tab-1..tab-8: that tab in strip order (tabs hidden in a collapsed group don't count); tab-last: the last one.
            (s, Some(c)) if s.starts_with("tab-") => {
                let ids: Vec<TabId> = with(&app, &win, |l| l.tabs.iter().filter(|t| t.id == c || l.visible(t.id)).map(|t| t.id).collect());
                let pick = match s[4..].parse::<usize>() { Ok(n) => n.checked_sub(1).and_then(|i| ids.get(i)), Err(_) => ids.last() };
                pick.map_or(Ok(()), |&t| activate(&app, &win, t))
            }
            _ => Ok(()),
        };
        if let Err(e) = r { eprintln!("menu {id}: {e}"); }
    });
}

pub fn run() {
    tauri::Builder::default()
        .manage(Browser(Mutex::new(HashMap::new())))
        .manage(Runtime { zoom: Mutex::default(), downloads: Mutex::default(), live: Mutex::default(), committed: Mutex::default(), tear: Mutex::default(), hint: Mutex::default(), popups: Mutex::default(), engine: Mutex::default(), failed: Mutex::default(), upgraded: Mutex::default(), insecure_ok: Mutex::default(), app_nav: Mutex::default(), user_nav: Mutex::default(), scroll_to: Mutex::default() })
        .manage(VaultRt::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(UpdateReady(Mutex::default()))
        .manage(StartUrls(Mutex::new(std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect())))
        .setup(|app| {
            let handle = app.handle().clone();
            let library = data_file(&handle, "library.json").and_then(|p| store::load::<Library>(&p)).unwrap_or_default();
            let top = include_str!("../resources/top10k.txt");
            let mut trusted = core::trust::TrustDb::new(top, library.trusted.iter().cloned());
            for d in &library.checked { trusted.add_checked(d); }
            app.manage(Trusted(Mutex::new(trusted)));
            let lists = [include_str!("../resources/lists/peterlowe.txt"), include_str!("../resources/lists/nocoin.txt")].concat();
            let top: Vec<String> = top.lines().map(String::from).collect();
            app.manage(Checks { checker: core::check::Checker::new(&top, &lists), watching: Mutex::default(), failed: Mutex::default() });
            app.manage(Lib(Mutex::new(library)));
            // Windows: no app menu (it would only be hidden); shortcuts come from the web views (platform::on_shortcut).
            #[cfg(not(target_os = "windows"))]
            app.set_menu(build_menu(&handle)?)?;
            app.on_menu_event(|app, e| on_menu(app, e.id().as_ref()));
            create_window(&handle, MAIN, None)?;
            start_blocker(&handle);
            start_throttler(&handle);
            start_updater(&handle);
            let h = handle.clone();
            platform::on_load_failed(move |e, url, domain, code, desc| on_load_failed(&h, e, url, domain, code, desc));
            let h = handle.clone();
            platform::on_main_navigation(move |url, cur, kind| on_main_navigation(&h, &url, &cur, kind));
            let h = handle.clone();
            platform::on_lockdown(move |url| needs_lockdown(&h, &url));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ui_ready, new_tab, new_incognito_window, close_tab, activate_tab, navigate, go_back, go_forward, reload,
            toggle_bookmark, find_in_page, suggest, history_search, history_clear, bookmarks_list, bookmark_remove,
            downloads_list, show_download,
            tab_menu, group_menu, group_toggle, group_rename, move_tab, tear_start, tear_move, tear_end, attach_tab,
            page_event, password_save, password_never, password_dismiss, password_menu,
            passwords_list, password_reveal, password_delete, passwords_import, shield_menu, popup_allow, popup_dismiss, insecure_continue, trust_menu, update_restart
        ])
        .run(tauri::generate_context!())
        .expect("error while running R-Browser");
}
