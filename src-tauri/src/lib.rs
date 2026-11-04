pub mod core;
mod platform;

use crate::core::library::{unique_path, Download, Library, Suggestion, Visit, Bookmark};
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

const BAR: f64 = 80.0; // tab strip 36 + toolbar 43 + guard line 1
const MAIN: &str = "main";
const ZOOM_STEPS: [f64; 9] = [0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 2.0];

/// One tab list per browser window, keyed by window label ("main", "incognito-1", ...).
struct Browser(Mutex<HashMap<String, TabList>>);
/// Bookmarks, history, downloads (normal windows only).
struct Lib(Mutex<Library>);
/// URLs given on the command line (`r-browser example.com wikipedia.org`), opened once the UI is ready.
struct StartUrls(Mutex<Vec<String>>);
/// Per-webview zoom level and in-flight download paths (macOS doesn't report where a download landed).
struct Runtime {
    zoom: Mutex<HashMap<String, f64>>,
    downloads: Mutex<HashMap<String, PathBuf>>,
    /// Last fully loaded URL per tab webview: a download starts like a navigation, so we roll back to this.
    committed: Mutex<HashMap<String, String>>,
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
    let map = app.state::<Browser>().inner().0.lock().unwrap();
    map.iter().find(|(_, l)| l.get(id).is_some()).map(|(w, _)| (w.clone(), id))
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
}

fn emit(app: &AppHandle, win: &str) {
    let list = with(app, win, |l| l.clone());
    let url = list.active.and_then(|a| list.get(a)).map(|t| t.url.clone()).unwrap_or_default();
    let bookmarked = lib(app, |l| l.is_bookmarked(&url));
    let _ = app.emit_to(ui_label(win), "tabs-changed", UiState { win: win.to_string(), list, bookmarked });
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
    let w = builder.build()?;
    app.state::<Browser>().inner().0.lock().unwrap().insert(win.to_string(), TabList::default());
    let (width, _) = content_area(&w);
    w.add_child(
        // Incognito windows: the toolbar itself is non-persistent too (it may load favicons).
        WebviewBuilder::new(ui_label(win), WebviewUrl::App("index.html".into())).incognito(is_incognito(win)),
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(width, BAR),
    )?;
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

fn create_webview(app: &AppHandle, win: &str, id: TabId, url: &str) -> tauri::Result<Webview> {
    let w = app.get_window(win).ok_or(tauri::Error::WindowNotFound)?;
    let (width, h) = content_area(&w);
    let (a1, a2, a3) = (app.clone(), app.clone(), app.clone());
    let mut builder = WebviewBuilder::new(label(win, id), target(url))
        // Incognito: non-persistent data store — nothing written to disk.
        .incognito(is_incognito(win))
        .on_page_load(move |wv, p| {
            let Some((win, id)) = parse_label(&a1, wv.label()) else { return };
            let url = to_rb(p.url().as_str());
            let started = p.event() == PageLoadEvent::Started;
            let title = with(&a1, &win, |l| l.get_mut(id).map(|t| {
                t.url = url.clone();
                if started { t.login_form = false; }
                t.loading = started;
                t.title.clone()
            })).unwrap_or_default();
            if p.event() == PageLoadEvent::Finished {
                a1.state::<Runtime>().committed.lock().unwrap().insert(wv.label().to_string(), url.clone());
            }
            if p.event() == PageLoadEvent::Finished && !is_incognito(&win) {
                lib(&a1, |l| l.record_visit(&url, &title, now_secs(), false));
                save_library_soon(&a1);
            }
            emit(&a1, &win);
        })
        .on_document_title_changed(move |wv, title| {
            let Some((win, id)) = parse_label(&a2, wv.label()) else { return };
            let url = with(&a2, &win, |l| l.get_mut(id).map(|t| { t.title = title.clone(); t.url.clone() })).unwrap_or_default();
            if !is_incognito(&win) { lib(&a2, |l| l.set_title(&url, &title)); }
            emit(&a2, &win);
        })
        .on_download(move |wv, ev| on_download(&a3, &wv, ev));
    if !is_incognito(win) { builder = builder.initialization_script(include_str!("inject/login.js")); }
    w.add_child(builder, LogicalPosition::new(0.0, BAR), LogicalSize::new(width, h))
}

fn open_tab(app: &AppHandle, win: &str, url: &str) -> Res {
    let id = with(app, win, |l| Some(l.open(url, is_incognito(win)))).ok_or("no such window")?;
    create_webview(app, win, id, url).map_err(err)?;
    activate(app, win, id)
}

fn activate(app: &AppHandle, win: &str, id: TabId) -> Res {
    let found = with(app, win, |l| {
        l.activate(id);
        l.get_mut(id).map(|t| (t.url.clone(), std::mem::replace(&mut t.frozen, false)))
    });
    let Some((url, was_frozen)) = found else { return Ok(()) };
    if was_frozen || app.get_webview(&label(win, id)).is_none() {
        create_webview(app, win, id, &url).map_err(err)?;
    }
    for other in tab_ids(app, win) {
        if let Some(wv) = app.get_webview(&label(win, other)) {
            let _ = if other == id { wv.show() } else { wv.hide() };
        }
    }
    emit(app, win);
    Ok(())
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
fn detach(app: &AppHandle, from: &str, id: TabId, at: Option<(f64, f64)>) -> Res {
    if tab_ids(app, from).len() < 2 { return Ok(()); } // ponytail: Chrome drags the whole window here; we leave it be
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
    if let Some(w) = app.get_window(&to) { let _ = w.set_focus(); }
    Ok(())
}

#[tauri::command]
async fn detach_tab(webview: Webview, app: AppHandle, window: Window, id: TabId, x: f64, y: f64) -> Res { trusted(&webview)?;
    // Put the new window's tab strip under the cursor.
    detach(&app, window.label(), id, Some((x - 120.0, (y - 18.0).max(0.0))))
}

fn tab_webview(app: &AppHandle, win: &str, id: TabId) -> Result<Webview, String> {
    app.get_webview(&label(win, id)).ok_or_else(|| "no such tab".into())
}
fn eval_in(app: &AppHandle, win: &str, id: TabId, js: &str) -> Res { tab_webview(app, win, id)?.eval(js).map_err(err) }

fn new_incognito(app: &AppHandle) -> Res {
    let win = format!("incognito-{}", NEXT_INCOGNITO.fetch_add(1, Ordering::SeqCst));
    create_window(app, &win, None).map_err(err)
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
        // app page: same origin as the UI, resolve against it
        WebviewUrl::App(p) => app.get_webview(&ui_label(win)).and_then(|ui| ui.url().ok()).and_then(|b| b.join(&format!("/{}", p.display())).ok()).ok_or("bad page")?,
        _ => return Err("bad url".into()),
    };
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
async fn reload(webview: Webview, app: AppHandle, window: Window, id: TabId) -> Res { trusted(&webview)?; tab_webview(&app, window.label(), id)?.reload().map_err(err) }

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
fn history_clear(webview: Webview, app: AppHandle) -> Res { trusted(&webview)?; lib(&app, |l| l.clear_history()); save_library_soon(&app); Ok(()) }

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
fn downloads_list(webview: Webview, app: AppHandle) -> Result<Vec<Download>, String> { trusted(&webview)?; Ok(lib(&app, |l| l.downloads.iter().rev().cloned().collect())) }

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
        ("pw", _) => return fill_password(app, win, num(2)?, id.splitn(4, '|').nth(3).unwrap_or_default()),
        ("t", "new") => {
            let g = with(app, win, |l| l.new_group(num(3).unwrap_or(0)));
            emit(app, win);
            if let Some(g) = g { rename_soon(app, win, g); }
        }
        ("t", "add") => { let (t, g) = (num(3)?, num(4)?); with(app, win, |l| l.add_to_group(t, g)); emit(app, win); }
        ("t", "remove") => { let t = num(3)?; with(app, win, |l| l.remove_from_group(t)); emit(app, win); }
        ("t", "close") => close(app, win, num(3)?)?,
        ("t", "newwin") => detach(app, win, num(3)?, None)?,
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
struct PageEvent { kind: String, #[serde(default)] username: String, #[serde(default)] password: String }

#[tauri::command]
async fn page_event(webview: Webview, app: AppHandle, ev: PageEvent) -> Res {
    let (win, id) = parse_label(&app, webview.label()).ok_or("not a tab")?;
    if is_incognito(&win) { return Ok(()); }
    let Some(origin) = webview.url().ok().and_then(|u| origin_of(u.as_str())) else { return Ok(()) };
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

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let item = |id: &str, text: &str, acc: &str| MenuItemBuilder::with_id(id, text).accelerator(acc).build(app);
    let tab_menu = SubmenuBuilder::new(app, "Tab")
        .item(&item("new-tab", "New Tab", "CmdOrCtrl+T")?)
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
            ("close-tab", Some(c)) => close(&app, &win, c),
            ("focus-url", _) => { focus_ui(&app, &win, "document.getElementById('url').focus()"); Ok(()) }
            ("find", _) => { focus_ui(&app, &win, "openFind()"); Ok(()) }
            ("reload", Some(c)) => tab_webview(&app, &win, c).and_then(|w| w.reload().map_err(err)),
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
            _ => Ok(()),
        };
        if let Err(e) = r { eprintln!("menu {id}: {e}"); }
    });
}

pub fn run() {
    tauri::Builder::default()
        .manage(Browser(Mutex::new(HashMap::new())))
        .manage(Runtime { zoom: Mutex::default(), downloads: Mutex::default(), committed: Mutex::default() })
        .manage(VaultRt::default())
        .plugin(tauri_plugin_dialog::init())
        .manage(StartUrls(Mutex::new(std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect())))
        .setup(|app| {
            let handle = app.handle().clone();
            let library = data_file(&handle, "library.json").and_then(|p| store::load::<Library>(&p)).unwrap_or_default();
            app.manage(Lib(Mutex::new(library)));
            app.set_menu(build_menu(&handle)?)?;
            app.on_menu_event(|app, e| on_menu(app, e.id().as_ref()));
            create_window(&handle, MAIN, None)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ui_ready, new_tab, new_incognito_window, close_tab, activate_tab, navigate, go_back, go_forward, reload,
            toggle_bookmark, find_in_page, suggest, history_search, history_clear, bookmarks_list, bookmark_remove,
            downloads_list, show_download,
            tab_menu, group_menu, group_toggle, group_rename, move_tab, detach_tab,
            page_event, password_save, password_never, password_dismiss, password_menu,
            passwords_list, password_reveal, password_delete, passwords_import
        ])
        .run(tauri::generate_context!())
        .expect("error while running R-Browser");
}
