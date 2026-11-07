//! OS glue: password vault (user presence check + keychain-held key) and native content blocking.

/// Ask the OS who's there: Touch ID, falling back to the login password. Blocks — never call on the main thread.
#[cfg(target_os = "macos")]
pub fn authenticate(reason: &str) -> bool {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    let (tx, rx) = std::sync::mpsc::channel();
    let reply = RcBlock::new(move |ok: Bool, _err: *mut NSError| { let _ = tx.send(ok.as_bool()); });
    unsafe {
        let ctx = LAContext::new();
        ctx.evaluatePolicy_localizedReason_reply(LAPolicy::DeviceOwnerAuthentication, &NSString::from_str(reason), &reply);
        rx.recv().unwrap_or(false)
    }
}

// ponytail: no Windows Hello / polkit prompt yet; the keychain (Credential Manager / Secret Service) is the only gate there.
#[cfg(not(target_os = "macos"))]
pub fn authenticate(_reason: &str) -> bool { true }

/// The 32-byte vault key from the OS keychain. Only created when `create` (no vault file yet),
/// so a missing key can never cause an existing vault to be overwritten with a new one.
pub fn vault_key(create: bool) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
    let entry = keyring::Entry::new("r-browser", "vault-key").map_err(|e| e.to_string())?;
    match entry.get_secret() {
        Ok(mut k) => {
            let key = <[u8; 32]>::try_from(k.as_slice()).map_err(|_| "vault key in the keychain is damaged".to_string());
            zeroize::Zeroize::zeroize(&mut k);
            key.map(zeroize::Zeroizing::new)
        }
        Err(keyring::Error::NoEntry) if create => {
            let k = crate::core::vault::new_key();
            entry.set_secret(&k).map_err(|e| e.to_string())?;
            Ok(zeroize::Zeroizing::new(k))
        }
        Err(keyring::Error::NoEntry) => Err("the vault key is missing from the keychain, so saved passwords can't be opened".into()),
        Err(e) => Err(e.to_string()),
    }
}

// ---- content blocking: one compiled WebKit content rule list shared by every tab ----

#[cfg(target_os = "macos")]
thread_local! {
    /// Main thread only (WebKit objects aren't Send).
    static RULES: std::cell::RefCell<Option<objc2::rc::Retained<objc2_web_kit::WKContentRuleList>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "macos")]
fn rule_store() -> Option<objc2::rc::Retained<objc2_web_kit::WKContentRuleListStore>> {
    unsafe { objc2_web_kit::WKContentRuleListStore::defaultStore(objc2::MainThreadMarker::new()?) }
}

/// Keep a list WebKit handed us; false if it's null (not found / failed to compile).
#[cfg(target_os = "macos")]
fn keep(list: *mut objc2_web_kit::WKContentRuleList) -> bool {
    let Some(l) = (unsafe { objc2::rc::Retained::retain(list) }) else { return false };
    RULES.with(|r| *r.borrow_mut() = Some(l));
    true
}

/// Load rule list `id` from WebKit's on-disk store, or, the first time this list version is seen, build `json()`
/// off the main thread and compile it (seconds; tabs load unblocked meanwhile). Then delete older versions and
/// call `ready`. Call on the main thread.
#[cfg(target_os = "macos")]
pub fn load_rules(app: tauri::AppHandle, id: String, json: impl FnOnce() -> String + Send + 'static, ready: impl Fn() + Send + Sync + 'static) {
    use block2::RcBlock;
    use objc2_foundation::{NSError, NSString};
    use objc2_web_kit::WKContentRuleList;
    use std::sync::{Arc, Mutex};

    let Some(store) = rule_store() else { return };
    let ready = Arc::new(ready);
    let json = Mutex::new(Some(json));
    let key = id.clone();
    let found = RcBlock::new(move |list: *mut WKContentRuleList, _: *mut NSError| {
        if keep(list) { prune(&key); return ready(); }
        let Some(json) = json.lock().unwrap().take() else { return };
        let (app, key, ready) = (app.clone(), key.clone(), ready.clone());
        std::thread::spawn(move || {
            let text = json();
            let _ = app.run_on_main_thread(move || {
                let Some(store) = rule_store() else { return };
                let k2 = key.clone();
                let done = RcBlock::new(move |list: *mut WKContentRuleList, err: *mut NSError| {
                    if keep(list) { prune(&k2); ready(); }
                    else if let Some(e) = unsafe { err.as_ref() } { eprintln!("content blocker: {}", e.localizedDescription()); }
                });
                unsafe { store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(Some(&NSString::from_str(&key)), Some(&NSString::from_str(&text)), Some(&done)) };
            });
        });
    });
    unsafe { store.lookUpContentRuleListForIdentifier_completionHandler(Some(&NSString::from_str(&id)), Some(&found)) };
}

/// Drop compiled lists other than `id` (each is ~10 MB on disk).
#[cfg(target_os = "macos")]
fn prune(id: &str) {
    use block2::RcBlock;
    use objc2_foundation::{NSArray, NSError, NSString};
    let Some(store) = rule_store() else { return };
    let (cur, s2) = (id.to_string(), store.clone());
    let got = RcBlock::new(move |ids: *mut NSArray<NSString>| {
        let Some(ids) = (unsafe { ids.as_ref() }) else { return };
        for old in ids.iter().filter(|i| i.to_string() != cur) {
            unsafe { s2.removeContentRuleListForIdentifier_completionHandler(Some(&old), Some(&RcBlock::new(|_: *mut NSError| {}))) };
        }
    });
    unsafe { store.getAvailableContentRuleListIdentifiers(Some(&got)) };
}

/// Turn blocking on or off for one tab (off for sites the user exempted). No-op until the list is loaded.
#[cfg(target_os = "macos")]
pub fn set_blocking(wv: &tauri::Webview, on: bool) {
    let _ = wv.with_webview(move |p| unsafe {
        let Some(c) = (p.controller() as *const objc2_web_kit::WKUserContentController).as_ref() else { return };
        c.removeAllContentRuleLists();
        if on { RULES.with(|r| if let Some(l) = &*r.borrow() { c.addContentRuleList(l) }); }
    });
}

// ponytail: WebView2 (WebResourceRequested + Blocker::should_block) and WebKitGTK (UserContentFilterStore) glue
// lands with Task 14, when CI can compile and test those targets; until then Windows/Linux load pages unblocked.
#[cfg(not(target_os = "macos"))]
pub fn load_rules(_: tauri::AppHandle, _: String, _: impl FnOnce() -> String + Send + 'static, _: impl Fn() + Send + Sync + 'static) {}
#[cfg(not(target_os = "macos"))]
pub fn set_blocking(_: &tauri::Webview, _: bool) {}

// ---- failed page loads: wry ignores WebKit's "navigation failed" callbacks, so pages just stayed blank ----

/// (WKWebView pointer, failing URL, error domain, error code, error text)
#[cfg(target_os = "macos")]
type LoadFailed = Box<dyn Fn(usize, String, String, i64, String) + Send + Sync>;
#[cfg(target_os = "macos")]
static LOAD_FAILED: std::sync::OnceLock<LoadFailed> = std::sync::OnceLock::new();

/// Call `f` whenever a page fails to load (takes effect once `hook_delegate` succeeds).
#[cfg(target_os = "macos")]
pub fn on_load_failed(f: impl Fn(usize, String, String, i64, String) + Send + Sync + 'static) {
    let _ = LOAD_FAILED.set(Box::new(f));
}

/// Called for every main-frame navigation with (target URL, the tab's current URL, WKNavigationType); returns a
/// URL to load instead (https upgrade or an rb:// page), or "" to cancel it.
#[cfg(target_os = "macos")]
type Redirect = Box<dyn Fn(String, String, isize) -> Option<String> + Send + Sync>;
#[cfg(target_os = "macos")]
static REDIRECT: std::sync::OnceLock<Redirect> = std::sync::OnceLock::new();
#[cfg(target_os = "macos")]
static WRY_POLICY: std::sync::OnceLock<objc2::runtime::Imp> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
pub fn on_main_navigation(f: impl Fn(String, String, isize) -> Option<String> + Send + Sync + 'static) {
    let _ = REDIRECT.set(Box::new(f));
}

/// Whether a main-frame navigation should load locked down (Lockdown Mode: JIT off, risky web features off).
#[cfg(target_os = "macos")]
static LOCKDOWN: std::sync::OnceLock<Box<dyn Fn(String) -> bool + Send + Sync>> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
pub fn on_lockdown(f: impl Fn(String) -> bool + Send + Sync + 'static) {
    let _ = LOCKDOWN.set(Box::new(f));
}

/// webView:decidePolicyForNavigationAction:preferences:decisionHandler: — once the delegate has this, WebKit calls
/// it instead of the plain one. Sets Lockdown Mode for the page, then decides exactly as the plain one does.
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn decide_policy_prefs(this: *mut objc2::runtime::AnyObject, _cmd: objc2::runtime::Sel, wv: *mut objc2::runtime::AnyObject,
                                                action: *mut objc2::runtime::AnyObject, prefs: *mut objc2::runtime::AnyObject,
                                                handler: *mut block2::Block<dyn Fn(isize, *mut objc2::runtime::AnyObject)>) {
    use objc2::{msg_send, runtime::{AnyObject, Bool}, sel};
    use objc2_foundation::NSURLRequest;
    unsafe {
        let frame: *mut AnyObject = msg_send![action, targetFrame];
        let main = !frame.is_null() && { let m: Bool = msg_send![frame, isMainFrame]; m.as_bool() };
        if main && !prefs.is_null() {
            let req: *mut NSURLRequest = msg_send![action, request];
            let url = req.as_ref().and_then(|r| r.URL()).and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            let lock = LOCKDOWN.get().is_some_and(|f| f(url));
            let _: () = msg_send![prefs, setLockdownModeEnabled: Bool::new(lock)];
        }
        // Hand the plain decision (cancel / allow / redirect) back to WebKit with these preferences. The bridge owns
        // its own references, so it's safe even if the decision is made after this method returns.
        let Some(h) = block2::RcBlock::copy(handler) else { return }; // no handler: nothing to answer
        let p = objc2::rc::Retained::retain(prefs);
        let bridge = block2::RcBlock::new(move |policy: isize| {
            h.call((policy, p.as_ref().map_or(std::ptr::null_mut(), |p| objc2::rc::Retained::as_ptr(p) as *mut AnyObject)))
        });
        decide_policy(this, sel!(webView:decidePolicyForNavigationAction:decisionHandler:), wv, action, &*bridge as *const _ as *mut _);
    }
}

/// Our webView:decidePolicyForNavigationAction:decisionHandler: — wry's own handler sees every frame, so the
/// https upgrade lives here where we can tell a top-level page from an iframe; everything else goes to wry's.
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn decide_policy(this: *mut objc2::runtime::AnyObject, cmd: objc2::runtime::Sel, wv: *mut objc2::runtime::AnyObject,
                                          action: *mut objc2::runtime::AnyObject, handler: *mut block2::Block<dyn Fn(isize)>) {
    use objc2::{msg_send, runtime::{AnyObject, Bool}};
    use objc2_foundation::{NSString, NSURL, NSURLRequest};
    let instead = unsafe {
        let frame: *mut AnyObject = msg_send![action, targetFrame];
        let main = !frame.is_null() && { let m: Bool = msg_send![frame, isMainFrame]; m.as_bool() };
        let req: *mut NSURLRequest = msg_send![action, request];
        let url = req.as_ref().and_then(|r| r.URL()).and_then(|u| u.absoluteString()).map(|s| s.to_string());
        let cur: *mut NSURL = msg_send![wv, URL];
        let cur = cur.as_ref().and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
        let kind: isize = msg_send![action, navigationType];
        match (main, url, REDIRECT.get()) { (true, Some(u), Some(f)) => f(u, cur, kind), _ => None }
    };
    if instead.as_deref() == Some("") {
        unsafe { (*handler).call((0,)) }; // cancel
        return;
    }
    if let Some(to) = instead.and_then(|t| NSURL::URLWithString(&NSString::from_str(&t))) {
        unsafe {
            (*handler).call((0,)); // WKNavigationActionPolicyCancel
            let _: *mut AnyObject = msg_send![wv, loadRequest: &*NSURLRequest::requestWithURL(&to)];
        }
        return;
    }
    let Some(orig) = WRY_POLICY.get() else { unsafe { (*handler).call((1,)) }; return }; // Allow
    let orig: unsafe extern "C-unwind" fn(*mut AnyObject, objc2::runtime::Sel, *mut AnyObject, *mut AnyObject, *mut block2::Block<dyn Fn(isize)>) = unsafe { std::mem::transmute(*orig) };
    unsafe { orig(this, cmd, wv, action, handler) }
}

/// Teach wry's navigation delegate class the two "failed" callbacks and wrap its navigation policy. Call before creating each tab webview:
/// the class only exists once wry has made its first webview, and WebKit checks the delegate's methods when
/// it's attached, so this must run before the tab is created. Does the work once.
#[cfg(target_os = "macos")]
pub fn hook_delegate() {
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{msg_send, sel};
    use objc2_foundation::{NSError, NSString};

    unsafe extern "C-unwind" fn failed(_this: *mut AnyObject, _cmd: Sel, wv: *mut AnyObject, _nav: *mut AnyObject, err: *mut NSError) {
        let (Some(cb), Some(e)) = (LOAD_FAILED.get(), unsafe { err.as_ref() }) else { return };
        // The failing URL is in userInfo as an NSURL ("NSErrorFailingURLKey"); older errors use a string key.
        let url = unsafe {
            let info: *mut AnyObject = msg_send![e, userInfo];
            let get = |k: &str| -> *mut AnyObject { if info.is_null() { std::ptr::null_mut() } else { msg_send![info, objectForKey: &*NSString::from_str(k)] } };
            let u = get("NSErrorFailingURLKey");
            let s: *mut NSString = if u.is_null() { get("NSErrorFailingURLStringKey").cast() } else { msg_send![u, absoluteString] };
            s.as_ref().map(|s| s.to_string()).unwrap_or_default()
        };
        cb(wv as usize, url, e.domain().to_string(), e.code() as i64, e.localizedDescription().to_string());
    }

    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.load(std::sync::atomic::Ordering::Relaxed) { return; }
    // wry names it "<module path>::WryNavigationDelegate<wry version>", so look it up instead of hard-coding.
    let Some(cls) = AnyClass::classes().iter().copied().find(|c| c.name().to_str().is_ok_and(|n| n.contains("::WryNavigationDelegate"))) else {
        eprintln!("load errors: wry delegate class not found yet");
        return;
    };
    DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    let imp: Imp = unsafe { std::mem::transmute(failed as unsafe extern "C-unwind" fn(_, _, _, _, _)) };
    let cls_mut = cls as *const AnyClass as *mut AnyClass;
    for s in [sel!(webView:didFailProvisionalNavigation:withError:), sel!(webView:didFailNavigation:withError:)] {
        unsafe { objc2::ffi::class_addMethod(cls_mut, s, imp, c"v@:@@@".as_ptr()) };
    }
    unsafe {
        let m = objc2::ffi::class_getInstanceMethod(cls_mut, sel!(webView:decidePolicyForNavigationAction:decisionHandler:));
        if !m.is_null() {
            let ours: Imp = std::mem::transmute(decide_policy as unsafe extern "C-unwind" fn(_, _, _, _, _));
            if let Some(old) = objc2::ffi::method_setImplementation(m as *mut _, ours) { let _ = WRY_POLICY.set(old); }
        }
        let prefs: Imp = std::mem::transmute(decide_policy_prefs as unsafe extern "C-unwind" fn(_, _, _, _, _, _));
        objc2::ffi::class_addMethod(cls_mut, sel!(webView:decidePolicyForNavigationAction:preferences:decisionHandler:), prefs, c"v@:@@@@".as_ptr());
    }
}

/// Show `html` in the tab, with `url` as its address (so links and the address bar still point at the site).
#[cfg(target_os = "macos")]
pub fn show_html(wv: &tauri::Webview, html: String, url: String) {
    let _ = wv.with_webview(move |p| unsafe {
        use objc2::{msg_send, runtime::AnyObject};
        use objc2_foundation::{NSString, NSURL};
        let base = NSURL::URLWithString(&NSString::from_str(&url));
        let _: *mut AnyObject = msg_send![p.inner() as *mut AnyObject, loadHTMLString: &*NSString::from_str(&html), baseURL: base.as_deref()];
    });
}

// ponytail: WebView2 has its own error page; WebKitGTK's load-failed hook comes with the Task 14 Linux glue.
#[cfg(not(target_os = "macos"))]
pub fn on_load_failed(_: impl Fn(usize, String, String, i64, String) + Send + Sync + 'static) {}
#[cfg(not(target_os = "macos"))]
pub fn hook_delegate() {}
// ponytail: HTTPS upgrade on WebView2/WebKitGTK comes with their Task 14 glue (WebView2 NavigationStarting).
// ponytail: WebView2 jitless environment / WebKitGTK JIT switch come with the Task 14 glue.
#[cfg(not(target_os = "macos"))]
pub fn on_lockdown(_: impl Fn(String) -> bool + Send + Sync + 'static) {}
#[cfg(not(target_os = "macos"))]
pub fn on_main_navigation(_: impl Fn(String, String, isize) -> Option<String> + Send + Sync + 'static) {}
#[cfg(not(target_os = "macos"))]
pub fn show_html(_: &tauri::Webview, _: String, _: String) {}

/// Windows: the focused web view (WebView2) swallows keys before the hidden menu's accelerators can see them,
/// so take Chrome's shortcuts from WebView2 itself. Runs before the page sees the key; others pass through.
#[cfg(target_os = "windows")]
pub fn on_shortcut(wv: &tauri::Webview, f: impl Fn(&'static str) + Send + 'static) {
    use webview2_com::{AcceleratorKeyPressedEventHandler, Microsoft::Web::WebView2::Win32::*};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT};
    let _ = wv.with_webview(move |p| unsafe {
        let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
            args.KeyEventKind(&mut kind)?;
            if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN { return Ok(()); }
            let mut vk = 0u32;
            args.VirtualKey(&mut vk)?;
            let down = |k: VIRTUAL_KEY| GetKeyState(k.0 as i32) < 0;
            let Some(id) = crate::core::keys::shortcut(vk, down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU)) else { return Ok(()) };
            args.SetHandled(true)?;
            let mut st = COREWEBVIEW2_PHYSICAL_KEY_STATUS::default();
            args.PhysicalKeyStatus(&mut st)?;
            if !st.WasKeyDown.as_bool() || crate::core::keys::repeats(id) { f(id); }
            Ok(())
        }));
        let mut token = 0i64;
        let _ = p.controller().add_AcceleratorKeyPressed(&handler, &mut token);
    });
}
#[cfg(not(target_os = "windows"))]
pub fn on_shortcut(_: &tauri::Webview, _: impl Fn(&'static str) + Send + 'static) {}

/// Windows, frameless window: Tauri's invisible resize-border window must stay above the tab web views,
/// which are added later and would otherwise cover it (no resizing from the edges until the next resize).
#[cfg(target_os = "windows")]
pub fn raise_resize_borders(w: &tauri::Window) {
    use windows::{core::w, Win32::UI::WindowsAndMessaging::*};
    let Ok(hwnd) = w.hwnd() else { return };
    unsafe {
        if let Ok(h) = FindWindowExW(Some(hwnd), None, w!("TAURI_DRAG_RESIZE_BORDERS"), w!("TAURI_DRAG_RESIZE_WINDOW")) {
            let _ = SetWindowPos(h, Some(HWND_TOP), 0, 0, 0, 0, SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER);
        }
    }
}
#[cfg(not(target_os = "windows"))]
pub fn raise_resize_borders(_: &tauri::Window) {}

// ---- download progress: wry only reports start and finish. Keep each native download object (main thread
// only) and read its byte counts when asked: WKDownload.progress on macOS, the WebView2 download operation on Windows. ----

/// Same spelling as the URL wry reports to on_download (it goes through url::Url too).
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn norm(u: &str) -> String { url::Url::parse(u).map(|u| u.to_string()).unwrap_or_else(|_| u.to_string()) }

#[cfg(target_os = "macos")]
thread_local! {
    static DOWNLOADS: std::cell::RefCell<Vec<(String, objc2::rc::Retained<objc2::runtime::AnyObject>)>> = Default::default();
}
#[cfg(target_os = "windows")]
thread_local! {
    static DOWNLOADS: std::cell::RefCell<Vec<(String, webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2DownloadOperation)>> = Default::default();
}

/// macOS: wrap wry's download:decideDestinationUsingResponse:… to keep the WKDownload, then let wry decide.
#[cfg(target_os = "macos")]
static WRY_DEST: std::sync::OnceLock<objc2::runtime::Imp> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn decide_destination(this: *mut objc2::runtime::AnyObject, cmd: objc2::runtime::Sel, download: *mut objc2::runtime::AnyObject,
                                               response: *mut objc2::runtime::AnyObject, name: *mut objc2::runtime::AnyObject, handler: *mut objc2::runtime::AnyObject) {
    use objc2::{msg_send, runtime::AnyObject};
    use objc2_foundation::NSURLRequest;
    unsafe {
        let req: *mut NSURLRequest = msg_send![download, originalRequest];
        let url = req.as_ref().and_then(|r| r.URL()).and_then(|u| u.absoluteString()).map(|s| norm(&s.to_string()));
        if let (Some(url), Some(d)) = (url, objc2::rc::Retained::retain(download)) { DOWNLOADS.with(|v| v.borrow_mut().push((url, d))); }
        let Some(orig) = WRY_DEST.get() else { return };
        let orig: unsafe extern "C-unwind" fn(*mut AnyObject, objc2::runtime::Sel, *mut AnyObject, *mut AnyObject, *mut AnyObject, *mut AnyObject) = std::mem::transmute(*orig);
        orig(this, cmd, download, response, name, handler)
    }
}

/// Start following downloads from this tab. macOS: hooks wry's download delegate class once it exists (wry makes it
/// with the first tab, so call this after creating each tab). Windows: listens on the tab's WebView2.
#[cfg(target_os = "macos")]
pub fn watch_downloads(_: &tauri::Webview) {
    use objc2::{runtime::{AnyClass, Imp}, sel};
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.load(std::sync::atomic::Ordering::Relaxed) { return; }
    let Some(cls) = AnyClass::classes().iter().copied().find(|c| c.name().to_str().is_ok_and(|n| n.contains("::WryDownloadDelegate"))) else { return };
    DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        let m = objc2::ffi::class_getInstanceMethod(cls, sel!(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:));
        if m.is_null() { return; }
        let ours: Imp = std::mem::transmute(decide_destination as unsafe extern "C-unwind" fn(_, _, _, _, _, _));
        if let Some(old) = objc2::ffi::method_setImplementation(m as *mut _, ours) { let _ = WRY_DEST.set(old); }
    }
}

#[cfg(target_os = "windows")]
pub fn watch_downloads(wv: &tauri::Webview) {
    use webview2_com::{take_pwstr, DownloadStartingEventHandler, Microsoft::Web::WebView2::Win32::*};
    use windows::core::{Interface, PWSTR};
    let _ = wv.with_webview(|p| unsafe {
        let Ok(core) = p.controller().CoreWebView2() else { return };
        let Ok(wv4) = core.cast::<ICoreWebView2_4>() else { return };
        let handler = DownloadStartingEventHandler::create(Box::new(|_, args| {
            let Some(args) = args else { return Ok(()) };
            let op = args.DownloadOperation()?;
            let mut uri = PWSTR::null();
            op.Uri(&mut uri)?;
            let url = norm(&take_pwstr(uri));
            DOWNLOADS.with(|v| v.borrow_mut().push((url, op)));
            Ok(())
        }));
        let mut token = 0i64;
        let _ = wv4.add_DownloadStarting(&handler, &mut token);
    });
}

/// (url, bytes so far, total bytes if known) for each download in flight. Main thread only.
#[cfg(target_os = "macos")]
pub fn download_progress() -> Vec<(String, u64, Option<u64>)> {
    use objc2::{msg_send, runtime::AnyObject};
    DOWNLOADS.with(|v| v.borrow().iter().filter_map(|(u, d)| unsafe {
        let p: *mut AnyObject = msg_send![&**d, progress];
        if p.is_null() { return None; }
        let (got, total): (i64, i64) = (msg_send![p, completedUnitCount], msg_send![p, totalUnitCount]);
        Some((u.clone(), got.max(0) as u64, (total > 0).then_some(total as u64)))
    }).collect())
}

#[cfg(target_os = "windows")]
pub fn download_progress() -> Vec<(String, u64, Option<u64>)> {
    DOWNLOADS.with(|v| v.borrow().iter().filter_map(|(u, op)| unsafe {
        let (mut got, mut total) = (0i64, 0i64);
        op.BytesReceived(&mut got).ok()?;
        op.TotalBytesToReceive(&mut total).ok()?;
        Some((u.clone(), got.max(0) as u64, (total > 0).then_some(total as u64)))
    }).collect())
}

/// The download ended (either way): stop following it. Main thread only.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn download_finished(url: &str) { DOWNLOADS.with(|v| v.borrow_mut().retain(|(u, _)| u != url)); }

// ponytail: no progress on Linux yet (WebKitGTK's WebKitDownload has it); comes with the Task 14 Linux glue.
#[cfg(target_os = "linux")]
pub fn watch_downloads(_: &tauri::Webview) {}
#[cfg(target_os = "linux")]
pub fn download_progress() -> Vec<(String, u64, Option<u64>)> { Vec::new() }
#[cfg(target_os = "linux")]
pub fn download_finished(_: &str) {}

// ---- background tabs: fewer resources, page kept (never put to sleep) ----

/// macOS: the tab's web content process gets background priority (Darwin "background" band: lowest CPU priority,
/// throttled disk and network I/O); WebKit already slows timers in hidden views. Back to normal when shown.
#[cfg(target_os = "macos")]
pub fn set_background(wv: &tauri::Webview, bg: bool) {
    extern "C" { fn setpriority(which: i32, who: u32, prio: i32) -> i32; }
    const PRIO_DARWIN_PROCESS: i32 = 4;
    const PRIO_DARWIN_BG: i32 = 0x1000;
    let _ = wv.with_webview(move |p| unsafe {
        use objc2::{msg_send, runtime::{AnyObject, Bool}, sel};
        let v = p.inner() as *mut AnyObject;
        // Private but long-standing (WKWebViewPrivate.h); skipped if a WebKit ever drops it.
        let has: Bool = msg_send![v, respondsToSelector: sel!(_webProcessIdentifier)];
        if !has.as_bool() { return; }
        let pid: i32 = msg_send![v, _webProcessIdentifier];
        if pid > 0 { setpriority(PRIO_DARWIN_PROCESS, pid as u32, if bg { PRIO_DARWIN_BG } else { 0 }); }
    });
}

/// Windows: WebView2 keeps a background tab's memory low (it trims caches and pages memory out); hidden web views
/// already get throttled timers. Needs WebView2 runtime 114+; older ones skip it.
#[cfg(target_os = "windows")]
pub fn set_background(wv: &tauri::Webview, bg: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use windows::core::Interface;
    let _ = wv.with_webview(move |p| unsafe {
        let Ok(core) = p.controller().CoreWebView2() else { return };
        let Ok(w19) = core.cast::<ICoreWebView2_19>() else { return };
        let _ = w19.SetMemoryUsageTargetLevel(if bg { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL });
    });
}

#[cfg(target_os = "linux")]
pub fn set_background(_: &tauri::Webview, _: bool) {}
