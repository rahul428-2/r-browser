pub mod core;

use crate::core::library::{unique_path, Download, Library, Suggestion, Visit, Bookmark};
use crate::core::omnibox::resolve;
use crate::core::session::{self, Session};
use crate::core::store;
use crate::core::tabs::{now_secs, rb_page, to_rb, TabId, TabList};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItemBuilder, SubmenuBuilder};
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

type Res = Result<(), String>;

static SESSION_GEN: AtomicU64 = AtomicU64::new(0);
static LIBRARY_GEN: AtomicU64 = AtomicU64::new(0);
static RESTORED: AtomicBool = AtomicBool::new(false);
static NEXT_INCOGNITO: AtomicU64 = AtomicU64::new(1);

fn is_incognito(win: &str) -> bool { win.starts_with("incognito-") }
fn ui_label(win: &str) -> String { format!("{win}-ui") }
fn label(win: &str, id: TabId) -> String { format!("{win}-tab-{id}") }
fn parse_label(label: &str) -> Option<(String, TabId)> {
    let (win, id) = label.rsplit_once("-tab-")?;
    Some((win.to_string(), id.parse().ok()?))
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
    if win == MAIN { save_session_soon(app); }
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
    debounce(&SESSION_GEN, app, |app| {
        let snap = with(app, MAIN, |l| Session::from_tabs(l));
        if let Some(p) = data_file(app, "session.json") {
            if let Err(e) = session::save_atomic(&p, &snap) { eprintln!("session save failed: {e}"); }
        }
    });
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
    let ids: Vec<TabId> = with(app, MAIN, |l| {
        saved.tabs.iter().map(|t| {
            let id = l.open(&t.url, false);
            let tab = l.get_mut(id).unwrap();
            tab.title = t.title.clone();
            tab.pinned = t.pinned;
            tab.scroll_y = t.scroll_y;
            tab.frozen = true;
            id
        }).collect()
    });
    match ids.get(saved.active).or(ids.first()) {
        Some(&id) => activate(app, MAIN, id),
        None => Ok(()),
    }
}

// ---- windows & tabs ----

fn create_window(app: &AppHandle, win: &str) -> tauri::Result<()> {
    let title = if is_incognito(win) { "R-Browser — Incognito" } else { "R-Browser" };
    let builder = WindowBuilder::new(app, win).title(title).inner_size(1280.0, 800.0).min_inner_size(480.0, 300.0);
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
        WindowEvent::Destroyed => { app2.state::<Browser>().inner().0.lock().unwrap().remove(&win2); }
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
    let win = parse_label(wv.label()).map(|(w, _)| w).unwrap_or_else(|| MAIN.into());
    let incognito = is_incognito(&win);
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
            if let (Some((w, id)), Some(prev)) = (parse_label(wv.label()), rt.committed.lock().unwrap().get(wv.label()).cloned()) {
                with(app, &w, |l| if let Some(t) = l.get_mut(id) { t.url = prev; });
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
    let builder = WebviewBuilder::new(label(win, id), target(url))
        // Incognito: non-persistent data store — nothing written to disk.
        .incognito(is_incognito(win))
        .on_page_load(move |wv, p| {
            let Some((win, id)) = parse_label(wv.label()) else { return };
            let url = to_rb(p.url().as_str());
            let title = with(&a1, &win, |l| l.get_mut(id).map(|t| { t.url = url.clone(); t.title.clone() })).unwrap_or_default();
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
            let Some((win, id)) = parse_label(wv.label()) else { return };
            let url = with(&a2, &win, |l| l.get_mut(id).map(|t| { t.title = title.clone(); t.url.clone() })).unwrap_or_default();
            if !is_incognito(&win) { lib(&a2, |l| l.set_title(&url, &title)); }
            emit(&a2, &win);
        })
        .on_download(move |wv, ev| on_download(&a3, &wv, ev));
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
            // Last tab closed -> close the window, like Chrome.
            if win == MAIN {
                SESSION_GEN.fetch_add(1, Ordering::SeqCst); // cancel a pending save, then save empty now
                if let Some(p) = data_file(app, "session.json") { let _ = session::save_atomic(&p, &Session::default()); }
            }
            if app.windows().len() <= 1 { app.exit(0); } else if let Some(w) = app.get_window(win) { let _ = w.destroy(); }
            Ok(())
        }
    }
}

fn tab_webview(app: &AppHandle, win: &str, id: TabId) -> Result<Webview, String> {
    app.get_webview(&label(win, id)).ok_or_else(|| "no such tab".into())
}
fn eval_in(app: &AppHandle, win: &str, id: TabId, js: &str) -> Res { tab_webview(app, win, id)?.eval(js).map_err(err) }

fn new_incognito(app: &AppHandle) -> Res {
    let win = format!("incognito-{}", NEXT_INCOGNITO.fetch_add(1, Ordering::SeqCst));
    create_window(app, &win).map_err(err)
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

// ---- commands (`window` = the window the calling page lives in) ----

#[tauri::command]
async fn ui_ready(app: AppHandle, window: Window) -> Res {
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
async fn new_tab(app: AppHandle, window: Window, url: Option<String>) -> Res {
    let win = window.label();
    open_tab(&app, win, &url.map(|u| resolve(&u)).unwrap_or_default())?;
    if let Some(ui) = app.get_webview(&ui_label(win)) { let _ = ui.set_focus(); }
    Ok(())
}

#[tauri::command]
async fn new_incognito_window(app: AppHandle) -> Res { new_incognito(&app) }

#[tauri::command]
async fn close_tab(app: AppHandle, window: Window, id: TabId) -> Res { close(&app, window.label(), id) }

#[tauri::command]
async fn activate_tab(app: AppHandle, window: Window, id: TabId) -> Res { activate(&app, window.label(), id) }

#[tauri::command]
async fn navigate(app: AppHandle, window: Window, id: TabId, input: String) -> Res {
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
async fn go_back(app: AppHandle, window: Window, id: TabId) -> Res { eval_in(&app, window.label(), id, "history.back()") }

#[tauri::command]
async fn go_forward(app: AppHandle, window: Window, id: TabId) -> Res { eval_in(&app, window.label(), id, "history.forward()") }

#[tauri::command]
async fn reload(app: AppHandle, window: Window, id: TabId) -> Res { tab_webview(&app, window.label(), id)?.reload().map_err(err) }

#[tauri::command]
async fn toggle_bookmark(app: AppHandle, window: Window) -> Res { toggle_bookmark_active(&app, window.label()) }

#[tauri::command]
async fn find_in_page(app: AppHandle, window: Window, id: TabId, query: String, backwards: bool) -> Res {
    // JSON-encode the query so it can't break out of the JS string.
    let js = format!("window.find({}, false, {backwards}, true)", serde_json::to_string(&query).map_err(err)?);
    eval_in(&app, window.label(), id, &js)
}

#[tauri::command]
fn suggest(app: AppHandle, query: String) -> Vec<Suggestion> { lib(&app, |l| l.suggest(&query, 8)) }

#[tauri::command]
fn history_search(app: AppHandle, query: String) -> Vec<Visit> { lib(&app, |l| l.search_history(&query, 500).into_iter().cloned().collect()) }

#[tauri::command]
fn history_clear(app: AppHandle) { lib(&app, |l| l.clear_history()); save_library_soon(&app); }

#[tauri::command]
fn bookmarks_list(app: AppHandle) -> Vec<Bookmark> { lib(&app, |l| l.bookmarks.iter().rev().cloned().collect()) }

#[tauri::command]
fn bookmark_remove(app: AppHandle, url: String) {
    lib(&app, |l| l.bookmarks.retain(|b| b.url != url));
    save_library_soon(&app);
}

#[tauri::command]
fn downloads_list(app: AppHandle) -> Vec<Download> { lib(&app, |l| l.downloads.iter().rev().cloned().collect()) }

/// Reveal a finished download in the file manager. Never opens/executes the file itself.
#[tauri::command]
fn show_download(app: AppHandle, path: String) -> Res {
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
        .build()?;
    let menu = Menu::default(app)?;
    menu.append(&tab_menu)?;
    menu.append(&library_menu)?;
    Ok(menu)
}

fn on_menu(app: &AppHandle, id: &str) {
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
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
            ("bookmarks" | "history" | "downloads", _) => open_tab(&app, &win, &format!("rb://{id}")),
            ("next-tab" | "prev-tab", Some(c)) => {
                let ids = tab_ids(&app, &win);
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
        .manage(StartUrls(Mutex::new(std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect())))
        .setup(|app| {
            let handle = app.handle().clone();
            let library = data_file(&handle, "library.json").and_then(|p| store::load::<Library>(&p)).unwrap_or_default();
            app.manage(Lib(Mutex::new(library)));
            app.set_menu(build_menu(&handle)?)?;
            app.on_menu_event(|app, e| on_menu(app, e.id().as_ref()));
            create_window(&handle, MAIN)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ui_ready, new_tab, new_incognito_window, close_tab, activate_tab, navigate, go_back, go_forward, reload,
            toggle_bookmark, find_in_page, suggest, history_search, history_clear, bookmarks_list, bookmark_remove,
            downloads_list, show_download
        ])
        .run(tauri::generate_context!())
        .expect("error while running R-Browser");
}
