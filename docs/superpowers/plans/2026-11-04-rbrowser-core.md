# R-Browser Core Browser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A cross-platform (macOS/Windows/Linux) security-first browser on each OS's own web engine, with session restore, incognito, unverified-site warnings, built-in blocking, tab freezing, suspicious-activity auto-close, fake location, and a password manager — using ~80% less RAM than Chrome with 20 tabs open.

**Architecture:** Tauri v2 (Rust) shell. One main window holds a small framework-free UI webview (tab strip + URL bar) and one child webview per tab. All decision logic lives in pure Rust modules under `src-tauri/src/core/` (unit-tested with `cargo test`); per-engine glue lives in `src-tauri/src/platform/`; scripts injected into pages live in `src-tauri/src/inject/`.

**Tech Stack:** Rust stable, Tauri 2 (`unstable` feature for multi-webview), adblock (adblock-rust) with `content-blocking` feature, psl, url, serde/serde_json, sha2, aes-gcm, keyring, rand, csv, tauri-plugin-updater; plain HTML/CSS/JS UI; Python 3 + psutil for the benchmark.

**Spec:** `docs/superpowers/specs/2026-11-04-rbrowser-core-design.md`

## Global Constraints

- Zero cost: no paid services, certificates or servers. GitHub + GitHub Actions free tier only.
- No telemetry or analytics, ever. The only network calls R-Browser makes on its own: filter-list / top-list refresh (weekly), Safe Browsing updates, updater check.
- No Chromium, no Electron. Engines: WKWebView (macOS 13+), WebView2 (Windows 10/11), WebKitGTK 4.1 (Ubuntu 22.04+).
- Local builds: always `-j 6` (laptop thermal limit). `export CARGO_BUILD_JOBS=6` in the dev shell.
- License MPL-2.0. Default search: Google (`https://www.google.com/search?q=`).
- Freeze idle background tabs after **300 s** (user-adjustable). Verified list = **Tranco top 10,000** registrable domains.
- Lists refresh **weekly**; bundled copies ship in `resources/` so first run works offline.
- Internal pages use the `rb://` custom scheme (`rb://blocked`, `rb://warning`, `rb://insecure`, `rb://newtab`, `rb://settings`, `rb://passwords`).
- **Remote pages get exactly one IPC command: `page_event`.** Rust derives the tab id and origin from the calling webview's label and current URL — never from the payload. No other Tauri command is reachable from remote content (enforce via capabilities file).
- Commits: plain messages, no AI attribution lines.

## Review Focus

1. **Corrupt or half-written session file** (power cut mid-save) → browser still starts with one new tab, keeps the bad file as `session.json.bad`. Test: Task 2 `load_corrupt_returns_none_and_backs_up`.
2. **Lookalike / IDN domains** (`gооgle.com` with Cyrillic о, `xn--` forms) → classified Unverified, never matched to google.com. Test: Task 6 `idn_lookalike_is_unverified`.
3. **User-content subdomains of verified sites** (`evil.github.io`, `x.blogspot.com`) → Unverified, because the PSL private section makes them their own registrable domain. Test: Task 6 `psl_private_suffix_not_verified`.
4. **Logging people out** — cookie-clearing on close applies only to *Unverified, untrusted* sites; Gmail/YouTube/banks in the top list stay logged in (anti-goal: breaking common sites). Test: Task 12 `verified_site_cookies_kept`.
5. **Freezing a tab with a half-written form** (draft email) → never frozen while a form is dirty. Test: Task 8 `dirty_form_never_frozen`.

---

### Task 1: Toolchain, scaffold, window with tabs and URL bar

**Files:**
- Create: `.gitignore`, `LICENSE` (MPL-2.0 text), `README.md`, `src-tauri/Cargo.toml`, `src-tauri/build.rs`, `src-tauri/tauri.conf.json`, `src-tauri/capabilities/ui.json`, `src-tauri/capabilities/remote.json`, `src-tauri/src/main.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/core/mod.rs`, `src-tauri/src/core/omnibox.rs`, `src-tauri/src/core/tabs.rs`, `ui/index.html`, `ui/app.js`, `ui/style.css`, `ui/pages/newtab.html`
- Test: unit tests inside `core/omnibox.rs`, `core/tabs.rs`

**Interfaces:**
- Produces:
  - `core::omnibox::resolve(input: &str) -> String` — URL or Google search URL.
  - `core::tabs::{TabId = u64, Tab { id, url: String, title: String, incognito: bool, frozen: bool, pinned: bool, audible: bool, dirty_form: bool, last_active: u64 }, TabList { tabs: Vec<Tab>, active: Option<TabId> }}` with `TabList::open(url, incognito) -> TabId`, `close(id) -> Option<TabId>` (returns new active), `activate(id)`, `move_tab(id, to: usize)`, `get(id) -> Option<&Tab>`, `get_mut`.
  - Tauri commands (UI only): `new_tab(url)`, `close_tab(id)`, `activate_tab(id)`, `navigate(id, input)`, `go_back(id)`, `go_forward(id)`, `reload(id)`. Event `tabs-changed` with the full `TabList` as JSON.
  - Child webview label format: `tab-{id}`. UI webview label: `ui`. UI bar height: 76 px; tab webviews fill the rest and resize with the window.

- [ ] **Step 1: Install toolchain**

Run: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && source ~/.cargo/env && cargo install tauri-cli --version '^2' --locked -j 6`
Expected: `cargo tauri --version` prints `tauri-cli 2.x`.

- [ ] **Step 2: Write failing tests**

```rust
// core/omnibox.rs
#[test] fn bare_domain_gets_https() { assert_eq!(resolve("example.com"), "https://example.com/"); }
#[test] fn words_become_search() { assert_eq!(resolve("hello world"), "https://www.google.com/search?q=hello+world"); }
#[test] fn full_url_kept() { assert_eq!(resolve("http://a.com/x"), "http://a.com/x"); }
#[test] fn localhost_port() { assert_eq!(resolve("localhost:3000"), "http://localhost:3000/"); }
#[test] fn rb_scheme_kept() { assert_eq!(resolve("rb://settings"), "rb://settings"); }
// core/tabs.rs
#[test] fn close_active_activates_right_neighbor_else_left() { /* open a,b,c; activate b; close b -> Some(c); close c -> Some(a) */ }
#[test] fn close_last_returns_none() { /* one tab, close -> None */ }
#[test] fn move_tab_reorders() { /* a,b,c move c to 0 -> c,a,b */ }
```

- [ ] **Step 3: Run** `cd src-tauri && cargo test -j 6` — Expected: FAIL (functions undefined).

- [ ] **Step 4: Implement `omnibox::resolve` and `TabList`.** Input with a space or no dot (except `localhost`) → search; `localhost`/IP without scheme → `http://`; otherwise `https://`; normalise via `url::Url`.

- [ ] **Step 5: Implement window + webviews in `lib.rs`.** Tauri `unstable` feature; main window built with `WindowBuilder`, UI webview via `window.add_child(WebviewBuilder::new("ui", WebviewUrl::App("index.html")), ...)`, each tab via `add_child` with label `tab-{id}`; only the active tab webview is shown (`show()`/`hide()`), all resized on `WindowEvent::Resized`. `on_page_load` / title-change hooks update `TabList` and emit `tabs-changed`. `capabilities/ui.json` grants commands to webview `ui` only; `capabilities/remote.json` grants only `page_event` to `remote.urls: ["https://*", "http://*"]`.

- [ ] **Step 6: Build `ui/`.** Tab strip (favicon, title, ×, + button), back/forward/reload, URL bar, ⚠/lock slot left of the URL (filled in Task 6), menu button. Black + electric-blue palette from `assets/logo.png`. Keyboard: ⌘/Ctrl+T, W, L, R, Tab, Shift+Tab, 1–9.

- [ ] **Step 7: Verify** `cargo test -j 6` passes; `cargo tauri dev` opens a window; typing `example.com` loads it; new/close/switch tabs works.

- [ ] **Step 8: Commit** `git add -A && git commit -m "feat: tauri shell with tabs and url bar"`

---

### Task 2: Session restore

**Files:** Create `src-tauri/src/core/session.rs`; modify `lib.rs`.

**Interfaces:**
- Consumes: `TabList`, `Tab`.
- Produces: `Session { tabs: Vec<SavedTab { url, title, pinned, scroll_y: f64 }>, active: usize }`; `Session::from_tabs(&TabList) -> Session` (drops incognito tabs); `save_atomic(path: &Path, s: &Session) -> io::Result<()>` (write `.tmp`, fsync, rename); `load(path: &Path) -> Option<Session>` (on parse error renames file to `session.json.bad`, returns None). File: `<app_data_dir>/session.json`.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn incognito_tabs_not_saved() { /* 2 normal + 1 incognito -> session.tabs.len()==2 */ }
#[test] fn roundtrip() { /* save_atomic then load == original */ }
#[test] fn load_corrupt_returns_none_and_backs_up() { /* write "{garbage"; load -> None; session.json.bad exists */ }
#[test] fn load_missing_returns_none() {}
```

- [ ] **Step 2: Run, expect FAIL. Step 3: Implement.**
- [ ] **Step 4: Wire into `lib.rs`:** save debounced 1 s after any `tabs-changed`; on startup, restore all tabs with `frozen = true` except the active one (frozen tabs have no webview until clicked — Task 8 owns the thaw path; until then create the webview on activate).
- [ ] **Step 5: Verify** tests pass; manual: open 3 sites, `kill -9` the app, relaunch → 3 tabs back, only active one loaded.
- [ ] **Step 6: Commit** `feat: crash-safe session restore`

---

### Task 3: Incognito window

**Files:** Modify `lib.rs`, `ui/app.js`, `ui/style.css`.

**Interfaces:** Command `new_incognito_window()`. Incognito tab webviews built with `WebviewBuilder::incognito(true)`; window label `incognito-{n}`; UI gets `?incognito=1` and shows the dark wolf badge.

- [ ] **Step 1: Implement**; shortcut ⌘/Ctrl+Shift+N.
- [ ] **Step 2: Verify manually:** log into a test site in incognito, close the window, reopen incognito → logged out; normal window not affected; `session.json` contains no incognito URL (`grep`).
- [ ] **Step 3: Commit** `feat: incognito windows`

---

### Task 3b: Bookmarks, history, downloads list, find, zoom, print

**Files:** Create `src-tauri/src/core/library.rs`, `ui/pages/{history,bookmarks,downloads}.html`; modify `lib.rs`, `ui/app.js`.

**Interfaces:** `Library { bookmarks: Vec<Bookmark { url, title, added: u64 }>, history: Vec<Visit { url, title, at: u64 }> }` saved as `<app_data_dir>/library.json` (atomic, same helper as session); `record_visit` skipped for incognito; `search_history(q: &str, limit: usize) -> Vec<&Visit>` (case-insensitive on url+title, newest first); history capped at 50,000 visits (drop oldest). Downloads: Tauri `on_download` saves to the OS Downloads folder; list kept in memory + `downloads.json`. Find (⌘/Ctrl+F) via injected `window.find`; zoom ⌘/Ctrl +/−/0 via `webview.set_zoom`; print via `webview.print()`. URL bar suggests from bookmarks then history.

- [ ] **Step 1: Failing tests:** `incognito_visit_not_recorded`, `search_history_newest_first`, `history_capped_at_50000`, `bookmark_toggle`.
- [ ] **Step 2: Run, FAIL. Step 3: Implement + UI pages + shortcuts (⌘/Ctrl+D bookmark, Y history, Shift+J downloads).**
- [ ] **Step 4: Verify** tests; manual check of each feature.
- [ ] **Step 5: Commit** `feat: bookmarks, history, downloads, find, zoom, print`

---

### Task 4: Ad, tracker and pop-up blocking

**Files:** Create `scripts/fetch_lists.sh`, `src-tauri/resources/lists/*.txt`, `src-tauri/src/core/blocker.rs`, `src-tauri/src/platform/{mod,macos,windows,linux}.rs`, `src-tauri/src/inject/popup.js`.

**Interfaces:**
- Produces: `Blocker::from_lists(lists: &[String]) -> Blocker`; `should_block(&self, url: &str, source_url: &str, request_type: &str) -> bool`; `cosmetic_css(&self, url: &str) -> String`; `webkit_rules_json(&self) -> String` (via adblock `content-blocking` conversion); `platform::install_blocker(webview: &Webview, blocker: &Blocker)`.
- Lists: EasyList, EasyPrivacy, uBlock filters, Peter Lowe's, NoCoin (also used by Task 10). `fetch_lists.sh` downloads them plus Tranco top-1m → `resources/top10k.txt` (first 10,000 domains).

- [ ] **Step 1: Failing tests** (use a tiny inline list, not the real files)

```rust
#[test] fn blocks_listed_third_party() { /* "||ads.example^" ; should_block("https://ads.example/a.js","https://news.com/","script") */ }
#[test] fn allows_unlisted() {}
#[test] fn exception_rule_wins() { /* "@@||ads.example/ok.js" */ }
#[test] fn cosmetic_css_contains_selector() { /* "news.com##.ad-box" -> css contains ".ad-box" */ }
#[test] fn webkit_json_is_valid_json_array() {}
```

- [ ] **Step 2: Run, FAIL. Step 3: Implement `blocker.rs`.**
- [ ] **Step 4: Platform glue.** macOS: compile `webkit_rules_json` with `WKContentRuleListStore` once (cache by list hash), add to each tab's `WKUserContentController` via `webview.with_webview`. Linux: `WebKitUserContentFilterStore` with the same JSON. Windows: `AddWebResourceRequestedFilter("*", ALL)` + `should_block` in the handler, respond with an empty 204. All: inject `cosmetic_css` at document start.
- [ ] **Step 5: `popup.js`:** wrap `window.open`; allow only if `navigator.userActivation.isActive`; otherwise call `page_event({kind:"popup-blocked", url})` → UI shows "Pop-up blocked — allow?" in the URL bar (allow = remember per site).
- [ ] **Step 6: Per-site "disable blocking" toggle** in the URL-bar shield menu (stored in settings).
- [ ] **Step 7: Verify** tests pass; manual: `https://d3ward.github.io/toolz/adblock` scores high on all 3 OSes (macOS locally; others in Task 14 CI artifacts on VMs).
- [ ] **Step 8: Commit** `feat: built-in ad, tracker and popup blocking`

---

### Task 5: HTTPS-only mode and internal pages

**Files:** Create `src-tauri/src/core/https.rs`, `ui/pages/{insecure,blocked,warning}.html`; modify `lib.rs` (register `rb://` protocol).

**Interfaces:** `https::upgrade(url: &str) -> Option<String>` (None if already https, rb://, localhost, private IP, or user-allowed host); internal page URL builders `rb://insecure?url=<enc>`, `rb://blocked?url=<enc>&reason=<code>`, `rb://warning?reason=<code>`.

- [ ] **Step 1: Failing tests:** `upgrades_http`, `keeps_https`, `skips_localhost_and_10_x`, `skips_allowed_host`.
- [ ] **Step 2: Run, FAIL. Step 3: Implement.**
- [ ] **Step 4: Wire `on_navigation`:** if `upgrade` returns Some → cancel and navigate to https; on load failure of an upgraded URL → `rb://insecure` with "Continue on insecure connection" (adds host to allowed list for this session).
- [ ] **Step 5: Verify** tests; manual `http://neverssl.com` → insecure page → continue works.
- [ ] **Step 6: Commit** `feat: https-only mode`

---

### Task 6: Verified / unverified sites and Safe Browsing

**Files:** Create `src-tauri/src/core/trust.rs`, `src-tauri/src/core/safebrowsing.rs`; modify `lib.rs`, `ui/app.js`.

**Interfaces:**
- `enum Trust { Verified, Unverified, Malicious }`; `TrustDb { top: HashSet<String>, user: HashSet<String> }`; `TrustDb::classify(&self, url: &str, sb: &SbList) -> Trust`; `registrable(host: &str) -> Option<String>` (via `psl`); `TrustDb::trust(domain)`.
- `SbList { prefixes: HashSet<[u8;4]> }`; `expressions(url: &str) -> Vec<String>` (Safe Browsing v4 host-suffix × path-prefix combos, max 5 hosts × 6 paths); `SbList::prefix_hit(&self, url) -> bool`; `confirm_full_hash(url, key) -> bool` (async, only on prefix hit, only if `RB_SAFE_BROWSING_KEY` was set at build time or in settings). No key → Safe Browsing skipped, top-list logic still runs.
- Rules: localhost/private IP/`rb://` → Verified. Registrable domain in `top` or `user` → Verified unless SB hit. SB confirmed hit → Malicious.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn top_domain_verified() { /* top={"google.com"}; "https://mail.google.com/x" -> Verified */ }
#[test] fn unknown_unverified() {}
#[test] fn user_trusted_verified() {}
#[test] fn idn_lookalike_is_unverified() { /* "https://gооgle.com" (Cyrillic о) -> Unverified */ }
#[test] fn psl_private_suffix_not_verified() { /* top has "github.io"? no: top={"github.com"}; "https://evil.github.io" -> Unverified */ }
#[test] fn localhost_verified() {}
#[test] fn sb_expressions_match_spec_example() { /* "http://a.b.c/1/2.html?param=1" yields the 8 expressions from the Safe Browsing v4 docs */ }
```

- [ ] **Step 2: Run, FAIL. Step 3: Implement.**
- [ ] **Step 4: Wire:** classify in `on_navigation`; Malicious → `rb://blocked` (Go back / Proceed anyway behind second confirm); Unverified → load + emit `trust-changed` → UI shows ⚠ left of the URL; click → popover with reason + "Trust this site". Weekly refresh task for top list + SB prefixes (stored in app data dir, bundled copy as fallback).
- [ ] **Step 5: Verify** tests; manual: `wikipedia.org` no ⚠; a random small blog shows ⚠; Trust → ⚠ gone after reload and after restart; `https://testsafebrowsing.appspot.com/s/malware.html` → blocked page (when key set).
- [ ] **Step 6: Commit** `feat: verified-site warnings and safe browsing`

---

### Task 7: JIT off on unverified sites (spike, then implement)

**Files:** Create `docs/notes/jit.md`; modify `src-tauri/src/platform/*.rs`, `lib.rs`.

**Interfaces:** `platform::set_hardened(webview: &Webview, on: bool)`; setting `hardened_mode: Auto | Always | Off` (Linux: `Always | Off` only).

- [ ] **Step 1: Spike each engine (timebox 2 h each), record results in `docs/notes/jit.md`:** macOS `WKWebpagePreferences.lockdownModeEnabled` per navigation (what breaks: fonts, WebGL, wasm?); Windows second `CoreWebView2Environment` with `--js-flags=--jitless`; Linux `JSC_useJIT=0` env (process-wide?). Measure with a JS benchmark page that the JIT is actually off (≥3× slower loop).
- [ ] **Step 2: Implement per findings:** Unverified tabs created in hardened mode; when a hardened tab navigates to a Verified site (or vice versa) recreate the webview in the right mode, keeping history-less URL. Linux: global toggle in settings.
- [ ] **Step 3: Verify** benchmark page shows the slowdown only on unverified sites (mac/Windows); Gmail unaffected.
- [ ] **Step 4: Commit** `feat: jit-off hardened mode for unverified sites`

---

### Task 8: Tab freezing and throttling

**Files:** Create `src-tauri/src/core/freezer.rs`, `src-tauri/src/inject/state.js`; modify `lib.rs`, `platform/*.rs`.

**Interfaces:**
- `freezer::should_freeze(tab: &Tab, active: Option<TabId>, now: u64, idle_secs: u64) -> bool` — false if active, pinned, audible, dirty_form, already frozen, or `now - last_active < idle_secs`.
- `state.js` reports via `page_event`: `{kind:"dirty-form", dirty: bool}`, `{kind:"scroll", y}` (throttled 1/s), `{kind:"audible", on}`.
- `platform::freeze(webview)` / thaw: Windows `ICoreWebView2_3::TrySuspend` / `Resume`; macOS & Linux close the child webview, keep `url` + `scroll_y`, recreate and restore scroll on activate.

- [ ] **Step 1: Failing tests:** `idle_background_tab_frozen`, `active_never_frozen`, `audible_never_frozen`, `pinned_never_frozen`, `dirty_form_never_frozen`, `recent_tab_not_frozen`.
- [ ] **Step 2: Run, FAIL. Step 3: Implement.**
- [ ] **Step 4: Wire:** 30 s ticker runs `should_freeze` over all tabs; frozen tabs greyed in the strip; activating thaws. Setting `freeze_after_minutes` (default 5, 0 = never).
- [ ] **Step 5: Verify** tests; manual: 10 tabs, wait 6 min, Activity Monitor shows WebContent processes dropped; clicking a frozen tab restores scroll position.
- [ ] **Step 6: Commit** `feat: freeze idle background tabs`

---

### Task 9: RAM benchmark vs Chrome

**Files:** Create `bench/bench.py`, `bench/sites.txt` (20 real sites: news, shopping, social, docs, video), `bench/README.md`; modify `lib.rs` (CLI: `r-browser <url>...` opens each as a tab).

**Interfaces:** `python3 bench/bench.py --browser chrome|rbrowser --scenario tabs20|heavy1 --wait 360` → prints `browser scenario total_mb avg_cpu_pct`; `--compare` runs both and prints a table with % saving.

- [ ] **Step 1: Implement.** Memory = sum of USS (`psutil.Process.memory_full_info().uss`) over the browser's process tree. On macOS WebKit helpers are XPC processes not parented to the app: include all `com.apple.WebKit.*` processes and require Safari to be closed (script aborts if Safari is running). Chrome launched with a fresh `--user-data-dir` in a temp dir. Install psutil in a venv at `bench/.venv`.
- [ ] **Step 2: Verify** `--compare` on macOS prints both scenarios; record numbers in `bench/README.md` with date and machine. Target: tabs20 saving ≥ 80%. If below, profile and fix before Task 14 — do not claim 80% without the number.
- [ ] **Step 3: Commit** `feat: ram benchmark vs chrome`

---

### Task 10: Suspicious-activity auto-close

**Files:** Create `src-tauri/src/core/detector.rs`, `src-tauri/src/inject/guard.js`; modify `lib.rs`, `ui/pages/warning.html`.

**Interfaces:**
- `enum Event { SbHit, DownloadNoGesture, MinerRequest, CpuHigh { secs: u64 }, Redirect { at_ms: u64 }, ScamPattern }`
- `enum Action { None, CloseTab(Reason), CloseBrowser(Reason) }`
- `Detector::default()`; `Detector::on_event(&mut self, tab: TabId, ev: Event) -> Action`.
- Rules: SbHit → CloseBrowser. Redirects: >5 within 10,000 ms on one tab → CloseTab. CpuHigh secs > 30 → CloseTab. DownloadNoGesture, MinerRequest (request matched NoCoin list), ScamPattern → CloseTab. Any second CloseTab-level trigger in the same session (any tab) → CloseBrowser.
- `guard.js` sends `{kind:"scam"}` when >3 `alert/confirm/prompt` calls within 10 s, or fullscreen requested without user activation; sends `{kind:"gesture", t}` on each user activation (used for download gesture check: a download with no gesture in the previous 1 s = DownloadNoGesture).
- CPU per tab: Windows via `GetProcessInfos`/browser process; macOS via WKWebView `_webProcessIdentifier` (private API, fine outside the App Store); Linux via `webkit_web_view_get_...` web-process pid if available, else skip CpuHigh on Linux and note it.

- [ ] **Step 1: Failing tests:** `sb_hit_closes_browser`, `five_redirects_ok_six_closes_tab`, `redirects_spread_over_11s_ok`, `cpu_30s_ok_31s_closes`, `second_trigger_closes_browser`, `download_without_gesture_closes_tab`.
- [ ] **Step 2: Run, FAIL. Step 3: Implement.**
- [ ] **Step 4: Wire:** CloseTab → replace tab with `rb://warning?reason=…`; CloseBrowser → save session **without** the offending tab, then exit. Download hook: Tauri `on_download`.
- [ ] **Step 5: Verify** tests; manual with local test pages in `tests/pages/` (redirect loop, alert spam, auto-download) served by `python3 -m http.server`.
- [ ] **Step 6: Commit** `feat: suspicious-activity auto-close`

---

### Task 11: Location prompt (real / fake) and light fingerprint protection

**Files:** Create `src-tauri/src/core/geo.rs`, `src-tauri/resources/cities.json` (~50 major cities: name, lat, lon, IANA tz), `src-tauri/src/inject/geo.js`, `src-tauri/src/inject/fp.js`; modify `lib.rs`, `ui/app.js`.

**Interfaces:**
- `geo::City { name, lat: f64, lon: f64, tz: String }`; `geo::jitter(c: &City, seed: u64) -> (f64, f64)` (±0.01°); `geo::GeoChoice { Real, Fake(String /*city*/), Block }` stored per registrable domain.
- `geo.js` replaces `navigator.geolocation.getCurrentPosition/watchPosition` at document start, sends `page_event {kind:"geo-request"}`; Rust answers by `eval` with a position, an error (Block), or by calling the saved original API (Real). Fake also overrides `Intl.DateTimeFormat().resolvedOptions().timeZone` and `Date.prototype.getTimezoneOffset` to the city's tz.
- `fp.js`: per-session seeded ±1 noise on `getImageData`/`toDataURL`/WebGL `readPixels`/AudioBuffer reads; generic UA per OS set via `WebviewBuilder::user_agent`.

- [ ] **Step 1: Failing tests:** `jitter_within_bounds`, `jitter_deterministic_per_seed`, `cities_json_valid_and_tz_known` (parse every tz with `chrono-tz`).
- [ ] **Step 2: Run, FAIL. Step 3: Implement.**
- [ ] **Step 4: UI prompt** under the URL bar: "site wants your location — Real / Fake (city ▾) / Block", remembered per site, editable in settings. If the engine refuses Real (no OS permission), show "Real location unavailable on this system".
- [ ] **Step 5: Verify** tests; manual on `https://browserleaks.com/geo` (fake city + matching tz) and `https://browserleaks.com/canvas` (hash changes per session); Gmail, YouTube, a bank login page still work (anti-goal check).
- [ ] **Step 6: Commit** `feat: fake location and fingerprint noise`

---

### Task 12: Clear cookies of closed unverified sites

**Files:** Create `src-tauri/src/core/cookies.rs`; modify `lib.rs`, `platform/*.rs`.

**Interfaces:** `cookies::should_clear(closed: &str, open_domains: &HashSet<String>, trust: Trust, user_trusted: bool, keep: &HashSet<String>) -> bool` — true only if `trust == Unverified`, not user-trusted, not in keep list, and no other open tab has the same registrable domain. `platform::clear_site_data(app, domain)` — use Tauri cookie APIs if they cover deletion, else native (`WKWebsiteDataStore.removeData`, `CookieManager.DeleteCookiesWithDomainAndPath`, `webkit_website_data_manager_remove`).

- [ ] **Step 1: Failing tests:** `unverified_closed_cleared`, `verified_site_cookies_kept`, `trusted_kept`, `still_open_elsewhere_kept`.
- [ ] **Step 2: Run, FAIL. Step 3: Implement.**
- [ ] **Step 4: Verify** tests; manual: unknown site sets cookie, close tab, reopen → cookie gone; Gmail stays logged in.
- [ ] **Step 5: Commit** `feat: clear cookies of closed unverified sites`

---

### Task 13: Password manager with Google CSV import

**Files:** Create `src-tauri/src/core/vault.rs`, `src-tauri/src/inject/login.js`, `ui/pages/passwords.html`; modify `lib.rs`.

**Interfaces:**
- `Entry { origin: String, username: String, password: String, note: String }` (origin = `scheme://host[:port]`).
- `Vault::new()`, `add(e)`, `find(origin: &str) -> Vec<&Entry>` (exact origin only), `remove(origin, username)`, `seal(&self, key: &[u8;32]) -> Vec<u8>` (AES-256-GCM, random 12-byte nonce prefixed), `Vault::open(blob, key) -> Result<Vault>`.
- `import_google_csv(path: &Path) -> Result<Vec<Entry>>` — header `name,url,username,password,note`.
- Key: 32 random bytes in OS keychain via `keyring` (service `r-browser`, user `vault-key`). Vault file `<app_data_dir>/vault.bin`.
- Autofill is **user-triggered** (key icon in URL bar → pick account → Rust evals fill into the page whose current URL origin Rust read itself). `login.js` on form submit sends `page_event {kind:"login-submitted", username, password}` → UI "Save password?" prompt.
- Unlock gate: OS biometric/system auth via `robius-authentication` before showing or filling; if unavailable on a platform, fall back to keychain-only.

- [ ] **Step 1: Failing tests:** `seal_open_roundtrip`, `wrong_key_fails`, `tampered_blob_fails`, `find_exact_origin_only` (`https://a.com` ≠ `https://sub.a.com` ≠ `http://a.com`), `import_google_csv_parses_sample` (3-row fixture in `tests/fixtures/google.csv`).
- [ ] **Step 2: Run, FAIL. Step 3: Implement.**
- [ ] **Step 4: Import flow** in `rb://passwords`: pick CSV → import → "Delete the CSV file now?" (overwrite with zeros then remove; note in UI that SSDs can't guarantee erasure).
- [ ] **Step 5: Verify** tests; manual: save on a test login page, autofill after restart, import a Google CSV.
- [ ] **Step 6: Commit** `feat: password manager with google import`

---

### Task 14: Settings page, signed updater, CI release builds

**Files:** Create `ui/pages/settings.html`, `.github/workflows/release.yml`, `.github/workflows/test.yml`; modify `tauri.conf.json`, `README.md`.

**Interfaces:** Settings stored in `<app_data_dir>/settings.json`: `search_url`, `freeze_after_minutes`, `hardened_mode`, `blocking_disabled_sites`, `trusted_sites`, `keep_logged_in`, `geo_choices`, `safe_browsing_key`.

- [ ] **Step 1: Settings page** exposing every setting above.
- [ ] **Step 2: Updater:** `cargo tauri signer generate` (private key stays local + GitHub secret, never committed); `tauri-plugin-updater` endpoint = GitHub Releases `latest.json`.
- [ ] **Step 3: CI:** `test.yml` runs `cargo test` on macos/windows/ubuntu; `release.yml` uses `tauri-apps/tauri-action` matrix → `.dmg` (ad-hoc signed), `.msi/.exe`, `.AppImage/.deb`.
- [ ] **Step 4: README:** what R-Browser does, install steps incl. macOS right-click → Open and Windows "More info → Run anyway", benchmark numbers from Task 9, MPL-2.0.
- [ ] **Step 5: Ask Rahul** before creating the public GitHub repo and pushing; then push, apply to SignPath Foundation for Windows signing.
- [ ] **Step 6: Verify** CI green on all three OSes; download each artifact and smoke-test (open tabs, restart restores) on macOS locally and Windows/Linux in VMs.
- [ ] **Step 7: Commit** `chore: settings, updater, release ci`
