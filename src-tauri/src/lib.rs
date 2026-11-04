pub mod core;

use crate::core::omnibox::resolve;
use crate::core::session::{self, Session};
use crate::core::tabs::{TabId, TabList};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItemBuilder, SubmenuBuilder};
use tauri::webview::{PageLoadEvent, WebviewBuilder};
use tauri::window::WindowBuilder;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewUrl, Window, WindowEvent};

const BAR: f64 = 80.0; // tab strip 36 + toolbar 43 + guard line 1
const NEWTAB: &str = "pages/newtab.html";
const MAIN: &str = "main";

/// One tab list per browser window, keyed by window label ("main", "incognito-1", ...).
struct Browser(Mutex<HashMap<String, TabList>>);
/// URLs given on the command line (`r-browser example.com wikipedia.org`), opened once the UI is ready.
struct StartUrls(Mutex<Vec<String>>);

type Res = Result<(), String>;

static SAVE_GEN: AtomicU64 = AtomicU64::new(0);
static RESTORED: AtomicBool = AtomicBool::new(false);
static NEXT_INCOGNITO: AtomicU64 = AtomicU64::new(1);

fn is_incognito(win: &str) -> bool { win.starts_with("incognito-") }
fn ui_label(win: &str) -> String { format!("{win}-ui") }
fn label(win: &str, id: TabId) -> String { format!("{win}-tab-{id}") }
fn parse_label(wv: &Webview) -> Option<(String, TabId)> {
    let (win, id) = wv.label().rsplit_once("-tab-")?;
    Some((win.to_string(), id.parse().ok()?))
}

/// Run `f` on a window's tab list (no-op default if the window is gone).
fn with<R: Default>(app: &AppHandle, win: &str, f: impl FnOnce(&mut TabList) -> R) -> R {
    let state = app.state::<Browser>();
    let mut map = state.inner().0.lock().unwrap();
    map.get_mut(win).map(f).unwrap_or_default()
}
fn tab_ids(app: &AppHandle, win: &str) -> Vec<TabId> { with(app, win, |l| l.tabs.iter().map(|t| t.id).collect()) }

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

fn emit(app: &AppHandle, win: &str) {
    let list = with(app, win, |l| l.clone());
    let _ = app.emit_to(ui_label(win), "tabs-changed", list);
    if win == MAIN { schedule_save(app); }
}

// ---- session (main window only; incognito is never saved) ----

fn session_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("session.json"))
}

/// Save ~1 s after the last change (debounced); never before the old session was restored.
fn schedule_save(app: &AppHandle) {
    if !RESTORED.load(Ordering::SeqCst) { return; }
    let gen = SAVE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if SAVE_GEN.load(Ordering::SeqCst) != gen { return; }
        let snap = with(&app, MAIN, |l| Session::from_tabs(l));
        if let Some(p) = session_path(&app) {
            if let Err(e) = session::save_atomic(&p, &snap) { eprintln!("session save failed: {e}"); }
        }
    });
}

/// Reopen last session's tabs; all start frozen (no webview) except the active one.
fn restore(app: &AppHandle) -> Res {
    let Some(saved) = session_path(app).and_then(|p| session::load(&p)) else { return Ok(()) };
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
        WebviewBuilder::new(ui_label(win), WebviewUrl::App("index.html".into())),
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

fn target(url: &str) -> WebviewUrl {
    match Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => WebviewUrl::External(u),
        _ => WebviewUrl::App(NEWTAB.into()),
    }
}

fn create_webview(app: &AppHandle, win: &str, id: TabId, url: &str) -> tauri::Result<Webview> {
    let w = app.get_window(win).ok_or(tauri::Error::WindowNotFound)?;
    let (width, h) = content_area(&w);
    let (a1, a2) = (app.clone(), app.clone());
    let builder = WebviewBuilder::new(label(win, id), target(url))
        // Incognito: non-persistent data store — nothing written to disk, gone when the tab closes.
        .incognito(is_incognito(win))
        .on_page_load(move |wv, p| {
            if p.event() != PageLoadEvent::Started { return; }
            if let Some((win, id)) = parse_label(&wv) {
                with(&a1, &win, |l| if let Some(t) = l.get_mut(id) { t.url = p.url().to_string(); });
                emit(&a1, &win);
            }
        })
        .on_document_title_changed(move |wv, title| {
            if let Some((win, id)) = parse_label(&wv) {
                with(&a2, &win, |l| if let Some(t) = l.get_mut(id) { t.title = title; });
                emit(&a2, &win);
            }
        });
    w.add_child(builder, LogicalPosition::new(0.0, BAR), LogicalSize::new(width, h))
}

fn open_tab(app: &AppHandle, win: &str, url: &str) -> Res {
    let id = with(app, win, |l| Some(l.open(url, is_incognito(win)))).ok_or("no such window")?;
    create_webview(app, win, id, url).map_err(|e| e.to_string())?;
    activate(app, win, id)
}

fn activate(app: &AppHandle, win: &str, id: TabId) -> Res {
    let found = with(app, win, |l| {
        l.activate(id);
        l.get_mut(id).map(|t| (t.url.clone(), std::mem::replace(&mut t.frozen, false)))
    });
    let Some((url, was_frozen)) = found else { return Ok(()) };
    if was_frozen || app.get_webview(&label(win, id)).is_none() {
        create_webview(app, win, id, &url).map_err(|e| e.to_string())?;
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
    match with(app, win, |l| l.close(id)) {
        Some(n) => activate(app, win, n),
        None => {
            // Last tab closed -> close the window, like Chrome.
            if win == MAIN {
                // Save the empty session now so it isn't restored next time.
                SAVE_GEN.fetch_add(1, Ordering::SeqCst);
                if let Some(p) = session_path(app) { let _ = session::save_atomic(&p, &Session::default()); }
            }
            if app.windows().len() <= 1 { app.exit(0); } else if let Some(w) = app.get_window(win) { let _ = w.destroy(); }
            Ok(())
        }
    }
}

fn eval_in(app: &AppHandle, win: &str, id: TabId, js: &str) -> Res {
    app.get_webview(&label(win, id)).ok_or("no such tab")?.eval(js).map_err(|e| e.to_string())
}

fn new_incognito(app: &AppHandle) -> Res {
    let win = format!("incognito-{}", NEXT_INCOGNITO.fetch_add(1, Ordering::SeqCst));
    create_window(app, &win).map_err(|e| e.to_string())
}

/// The window the user is looking at (for menu shortcuts).
fn focused(app: &AppHandle) -> String {
    app.windows().into_iter().find(|(_, w)| w.is_focused().unwrap_or(false)).map(|(l, _)| l).unwrap_or_else(|| MAIN.into())
}

// ---- commands (browser UI webviews only; `window` = the window the UI lives in) ----

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
    let parsed = Url::parse(&url).map_err(|e| e.to_string())?;
    with(&app, win, |l| if let Some(t) = l.get_mut(id) { t.url = url.clone(); });
    let wv = app.get_webview(&label(win, id)).ok_or("no such tab")?;
    wv.navigate(parsed).map_err(|e| e.to_string())?;
    let _ = wv.set_focus();
    emit(&app, win);
    Ok(())
}

#[tauri::command]
async fn go_back(app: AppHandle, window: Window, id: TabId) -> Res { eval_in(&app, window.label(), id, "history.back()") }

#[tauri::command]
async fn go_forward(app: AppHandle, window: Window, id: TabId) -> Res { eval_in(&app, window.label(), id, "history.forward()") }

#[tauri::command]
async fn reload(app: AppHandle, window: Window, id: TabId) -> Res {
    app.get_webview(&label(window.label(), id)).ok_or("no such tab")?.reload().map_err(|e| e.to_string())
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
        .separator()
        .item(&item("next-tab", "Next Tab", "Ctrl+Tab")?)
        .item(&item("prev-tab", "Previous Tab", "Ctrl+Shift+Tab")?)
        .build()?;
    let menu = Menu::default(app)?;
    menu.append(&tab_menu)?;
    Ok(menu)
}

fn on_menu(app: &AppHandle, id: &str) {
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        let win = focused(&app);
        let cur = with(&app, &win, |l| l.active);
        let _ = match (id.as_str(), cur) {
            ("new-tab", _) => {
                let r = open_tab(&app, &win, "");
                if let Some(ui) = app.get_webview(&ui_label(&win)) { let _ = ui.set_focus(); }
                r
            }
            ("new-incognito", _) => new_incognito(&app),
            ("close-tab", Some(c)) => close(&app, &win, c),
            ("focus-url", _) => {
                if let Some(ui) = app.get_webview(&ui_label(&win)) {
                    let _ = ui.set_focus();
                    let _ = ui.eval("document.getElementById('url').focus()");
                }
                Ok(())
            }
            ("reload", Some(c)) => app.get_webview(&label(&win, c)).map(|w| w.reload().map_err(|e| e.to_string())).unwrap_or(Ok(())),
            ("back", Some(c)) => eval_in(&app, &win, c, "history.back()"),
            ("forward", Some(c)) => eval_in(&app, &win, c, "history.forward()"),
            ("next-tab" | "prev-tab", Some(c)) => {
                let ids = tab_ids(&app, &win);
                let i = ids.iter().position(|&x| x == c).unwrap_or(0);
                let n = ids.len();
                let j = if id == "next-tab" { (i + 1) % n } else { (i + n - 1) % n };
                activate(&app, &win, ids[j])
            }
            _ => Ok(()),
        };
    });
}

pub fn run() {
    tauri::Builder::default()
        .manage(Browser(Mutex::new(HashMap::new())))
        .manage(StartUrls(Mutex::new(std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect())))
        .setup(|app| {
            let handle = app.handle().clone();
            app.set_menu(build_menu(&handle)?)?;
            app.on_menu_event(|app, e| on_menu(app, e.id().as_ref()));
            create_window(&handle, MAIN)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ui_ready, new_tab, new_incognito_window, close_tab, activate_tab, navigate, go_back, go_forward, reload
        ])
        .run(tauri::generate_context!())
        .expect("error while running R-Browser");
}
