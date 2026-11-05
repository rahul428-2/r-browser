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

/// Call `f` whenever a page fails to load (takes effect once `hook_load_failed` succeeds).
#[cfg(target_os = "macos")]
pub fn on_load_failed(f: impl Fn(usize, String, String, i64, String) + Send + Sync + 'static) {
    let _ = LOAD_FAILED.set(Box::new(f));
}

/// Teach wry's navigation delegate class the two "failed" callbacks. Call before creating each tab webview:
/// the class only exists once wry has made its first webview, and WebKit checks the delegate's methods when
/// it's attached, so this must run before the tab is created. Does the work once.
#[cfg(target_os = "macos")]
pub fn hook_load_failed() {
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
    for s in [sel!(webView:didFailProvisionalNavigation:withError:), sel!(webView:didFailNavigation:withError:)] {
        unsafe { objc2::ffi::class_addMethod(cls as *const AnyClass as *mut AnyClass, s, imp, c"v@:@@@".as_ptr()) };
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
pub fn hook_load_failed() {}
#[cfg(not(target_os = "macos"))]
pub fn show_html(_: &tauri::Webview, _: String, _: String) {}
