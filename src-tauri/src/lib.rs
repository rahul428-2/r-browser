pub mod core;

use crate::core::omnibox::resolve;
use crate::core::tabs::{TabId, TabList};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItemBuilder, SubmenuBuilder};
use tauri::webview::{PageLoadEvent, WebviewBuilder};
use tauri::window::WindowBuilder;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewUrl, Window, WindowEvent};

const BAR: f64 = 76.0; // tab strip 36 + toolbar 40
const NEWTAB: &str = "pages/newtab.html";

struct Browser(Mutex<TabList>);
/// URLs given on the command line (`r-browser example.com wikipedia.org`), opened once the UI is ready.
struct StartUrls(Mutex<Vec<String>>);

type Res = Result<(), String>;

fn label(id: TabId) -> String { format!("tab-{id}") }
fn id_of(wv: &Webview) -> Option<TabId> { wv.label().strip_prefix("tab-")?.parse().ok() }
fn tabs(app: &AppHandle) -> std::sync::MutexGuard<'_, TabList> { app.state::<Browser>().inner().0.lock().unwrap() }
fn window(app: &AppHandle) -> Window { app.get_window("main").expect("main window") }

fn content_area(win: &Window) -> (f64, f64) {
    let f = win.scale_factor().unwrap_or(1.0);
    let s = win.inner_size().map(|s| s.to_logical::<f64>(f)).unwrap_or(LogicalSize::new(1280.0, 800.0));
    (s.width, (s.height - BAR).max(0.0))
}

fn layout(app: &AppHandle) {
    let (w, h) = content_area(&window(app));
    if let Some(ui) = app.get_webview("ui") {
        let _ = ui.set_position(LogicalPosition::new(0.0, 0.0));
        let _ = ui.set_size(LogicalSize::new(w, BAR));
    }
    let ids: Vec<TabId> = tabs(app).tabs.iter().map(|t| t.id).collect();
    for id in ids {
        if let Some(wv) = app.get_webview(&label(id)) {
            let _ = wv.set_position(LogicalPosition::new(0.0, BAR));
            let _ = wv.set_size(LogicalSize::new(w, h));
        }
    }
}

fn emit(app: &AppHandle) {
    let list = tabs(app).clone();
    let _ = app.emit_to("ui", "tabs-changed", list);
}

fn target(url: &str) -> WebviewUrl {
    match Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => WebviewUrl::External(u),
        _ => WebviewUrl::App(NEWTAB.into()),
    }
}

fn create_webview(app: &AppHandle, id: TabId, url: &str) -> tauri::Result<Webview> {
    let win = window(app);
    let (w, h) = content_area(&win);
    let (a1, a2) = (app.clone(), app.clone());
    let builder = WebviewBuilder::new(label(id), target(url))
        .on_page_load(move |wv, p| {
            if p.event() == PageLoadEvent::Started {
                if let Some(id) = id_of(&wv) {
                    if let Some(t) = tabs(&a1).get_mut(id) { t.url = p.url().to_string(); }
                    emit(&a1);
                }
            }
        })
        .on_document_title_changed(move |wv, title| {
            if let Some(id) = id_of(&wv) {
                if let Some(t) = tabs(&a2).get_mut(id) { t.title = title; }
                emit(&a2);
            }
        });
    win.add_child(builder, LogicalPosition::new(0.0, BAR), LogicalSize::new(w, h))
}

fn open_tab(app: &AppHandle, url: &str) -> Res {
    let id = tabs(app).open(url, false);
    create_webview(app, id, url).map_err(|e| e.to_string())?;
    activate(app, id)
}

fn activate(app: &AppHandle, id: TabId) -> Res {
    let (url, needs_webview) = {
        let mut l = tabs(app);
        l.activate(id);
        let Some(t) = l.get_mut(id) else { return Ok(()) };
        let frozen = std::mem::replace(&mut t.frozen, false);
        (t.url.clone(), frozen)
    };
    if needs_webview || app.get_webview(&label(id)).is_none() {
        create_webview(app, id, &url).map_err(|e| e.to_string())?;
    }
    let ids: Vec<TabId> = tabs(app).tabs.iter().map(|t| t.id).collect();
    for other in ids {
        if let Some(wv) = app.get_webview(&label(other)) {
            let _ = if other == id { wv.show() } else { wv.hide() };
        }
    }
    emit(app);
    Ok(())
}

fn close(app: &AppHandle, id: TabId) -> Res {
    if let Some(wv) = app.get_webview(&label(id)) { let _ = wv.close(); }
    let next = tabs(app).close(id);
    match next {
        Some(n) => activate(app, n),
        None => { app.exit(0); Ok(()) } // last tab closed -> quit, like Chrome
    }
}

fn eval_in(app: &AppHandle, id: TabId, js: &str) -> Res {
    app.get_webview(&label(id)).ok_or("no such tab")?.eval(js).map_err(|e| e.to_string())
}

fn active(app: &AppHandle) -> Option<TabId> { tabs(app).active }

// ---- commands (UI webview only) ----

#[tauri::command]
async fn ui_ready(app: AppHandle) -> Res {
    let start: Vec<String> = std::mem::take(&mut *app.state::<StartUrls>().inner().0.lock().unwrap());
    for u in &start { open_tab(&app, &resolve(u))?; }
    if tabs(&app).tabs.is_empty() { open_tab(&app, "")?; }
    layout(&app);
    emit(&app);
    Ok(())
}

#[tauri::command]
async fn new_tab(app: AppHandle, url: Option<String>) -> Res {
    open_tab(&app, &url.map(|u| resolve(&u)).unwrap_or_default())?;
    if let Some(ui) = app.get_webview("ui") { let _ = ui.set_focus(); }
    Ok(())
}

#[tauri::command]
async fn close_tab(app: AppHandle, id: TabId) -> Res { close(&app, id) }

#[tauri::command]
async fn activate_tab(app: AppHandle, id: TabId) -> Res { activate(&app, id) }

#[tauri::command]
async fn navigate(app: AppHandle, id: TabId, input: String) -> Res {
    let url = resolve(&input);
    let parsed = Url::parse(&url).map_err(|e| e.to_string())?;
    if let Some(t) = tabs(&app).get_mut(id) { t.url = url.clone(); }
    let wv = app.get_webview(&label(id)).ok_or("no such tab")?;
    wv.navigate(parsed).map_err(|e| e.to_string())?;
    let _ = wv.set_focus();
    emit(&app);
    Ok(())
}

#[tauri::command]
async fn go_back(app: AppHandle, id: TabId) -> Res { eval_in(&app, id, "history.back()") }

#[tauri::command]
async fn go_forward(app: AppHandle, id: TabId) -> Res { eval_in(&app, id, "history.forward()") }

#[tauri::command]
async fn reload(app: AppHandle, id: TabId) -> Res {
    app.get_webview(&label(id)).ok_or("no such tab")?.reload().map_err(|e| e.to_string())
}

// ---- native menu: shortcuts that work even when a web page has focus ----

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let item = |id: &str, text: &str, acc: &str| MenuItemBuilder::with_id(id, text).accelerator(acc).build(app);
    let tab_menu = SubmenuBuilder::new(app, "Tab")
        .item(&item("new-tab", "New Tab", "CmdOrCtrl+T")?)
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
    let app = app.clone();
    let id = id.to_string();
    tauri::async_runtime::spawn(async move {
        let cur = active(&app);
        let _ = match (id.as_str(), cur) {
            ("new-tab", _) => new_tab(app.clone(), None).await,
            ("close-tab", Some(c)) => close(&app, c),
            ("focus-url", _) => {
                if let Some(ui) = app.get_webview("ui") {
                    let _ = ui.set_focus();
                    let _ = ui.eval("document.getElementById('url').focus()");
                }
                Ok(())
            }
            ("reload", Some(c)) => reload(app.clone(), c).await,
            ("back", Some(c)) => eval_in(&app, c, "history.back()"),
            ("forward", Some(c)) => eval_in(&app, c, "history.forward()"),
            ("next-tab" | "prev-tab", Some(c)) => {
                let ids: Vec<TabId> = tabs(&app).tabs.iter().map(|t| t.id).collect();
                let i = ids.iter().position(|&x| x == c).unwrap_or(0);
                let n = ids.len();
                let j = if id == "next-tab" { (i + 1) % n } else { (i + n - 1) % n };
                activate(&app, ids[j])
            }
            _ => Ok(()),
        };
    });
}

pub fn run() {
    tauri::Builder::default()
        .manage(Browser(Mutex::new(TabList::default())))
        .manage(StartUrls(Mutex::new(std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect())))
        .setup(|app| {
            let handle = app.handle().clone();
            let menu = build_menu(&handle)?;
            app.set_menu(menu)?;
            app.on_menu_event(|app, e| on_menu(app, e.id().as_ref()));

            let builder = WindowBuilder::new(app, "main")
                .title("R-Browser")
                .inner_size(1280.0, 800.0)
                .min_inner_size(480.0, 300.0);
            // macOS: draw the tab strip where the title bar was (like Chrome); keeps webview coordinates exact.
            #[cfg(target_os = "macos")]
            let builder = builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
            let win = builder.build()?;
            let (w, _) = content_area(&win);
            win.add_child(
                WebviewBuilder::new("ui", WebviewUrl::App("index.html".into())),
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(w, BAR),
            )?;
            let h2 = handle.clone();
            win.on_window_event(move |e| {
                if let WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } = e { layout(&h2); }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ui_ready, new_tab, close_tab, activate_tab, navigate, go_back, go_forward, reload
        ])
        .run(tauri::generate_context!())
        .expect("error while running R-Browser");
}
