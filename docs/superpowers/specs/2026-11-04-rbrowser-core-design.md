# R-Browser — Project 1: Core Browser — Design

Date: 4 Nov 2026 · Owner: Rahul · Logo: `assets/logo.png` (electric-blue wolf + shield)

## 1. Intent

R-Browser is an open-source, security-first desktop browser for macOS, Windows and Linux.
It fills gaps found in Chrome and the privacy browsers (research, 4 Nov 2026):

| Gap in today's browsers | R-Browser answer |
|---|---|
| Zero-days fire just by visiting a page; Safe Browsing only knows *known*-bad sites | Unverified-site warning + JIT off on unverified sites + auto-close on suspicious behaviour |
| Chrome's Manifest V3 weakened ad blockers | Ad/tracker/pop-up blocking built into the browser, not an extension |
| Chrome is the heaviest on RAM and battery | System web engines + aggressive tab freezing; target **80% less RAM than Chrome** in the multi-tab case |
| Privacy browsers trade away convenience (Mullvad) or lag patches (forks) | Engine is patched by the OS vendor; normal daily-driver features kept |

**Constraint: zero money.** No paid certificates, servers or services.

## 2. The three projects (only #1 is designed here)

1. **Core browser** ← this document
2. Downloads open inside a disposable VM, never on the host (later)
3. Fake IP via built-in Tor or the user's own VPN/proxy (later)

Extension store: **dropped for now** (Rahul, 4 Nov).

## 3. Decisions made by Rahul

- Platforms: macOS, Windows, Linux.
- **No Chromium.** Use each OS's own engine to hit the RAM/power goal.
- Unverified site: page **opens**, with a ⚠ warning icon next to the URL bar (not a blocking page).
- Session restore: tabs come back after the browser is closed mid-work.
- Location: when a site asks, prompt **Real / Fake**.
- No ads, no pop-up ads, incognito mode, and the usual features of other browsers.
- Integrate Google Password Manager "if possible" → not possible (see §5.9); built-in manager with Google import instead.
- RAM: ~80% less than Chrome.
- Spend no money.

## 4. Stack (builder's/assistant's call, accepted by Rahul)

- **Tauri v2 (Rust)** shell. Engines: WKWebView (macOS), WebView2 (Windows), WebKitGTK (Linux).
- Each tab = its own child webview inside the main window; browser UI (tab strip, URL bar, menus) is a small plain HTML/TS webview — no React/framework (keeps UI RAM tiny).
- Where Tauri doesn't expose an engine feature (content blocking, data stores, lockdown/JIT, suspend), call the native engine API directly from Rust per OS (`objc2` / `webview2-com` / `webkit2gtk` crates). Expect this to be a meaningful share of the work.
- License: **MPL-2.0** (assistant's call; same as Firefox, compatible with adblock-rust).
- Builds on Rahul's laptop must be capped at ~6 parallel jobs (`cargo build -j 6`) — thermal limit.

## 5. Features

### 5.1 Standard browser features
Tabs (open/close/reorder/duplicate), back/forward/reload, URL bar with search (default engine: Google — Rahul, 4 Nov; user-changeable), bookmarks, history, downloads list, find in page, zoom, print, keyboard shortcuts matching Chrome's common ones, settings page.

### 5.2 Session restore
- Session (all windows, tabs, order, URL, scroll position, frozen/active state) written to disk as JSON, debounced ~1 s after any change, via write-to-temp-then-rename so a crash never leaves a corrupt file.
- On launch, restore everything; restored tabs start **frozen** (load on click) except the active one — saves RAM on startup.
- Incognito tabs are never written.

### 5.3 Incognito mode
Separate window using a non-persistent data store per engine (WKWebsiteDataStore.nonPersistent / WebView2 InPrivate / WebKitGTK ephemeral session). No history, cookies, cache or session entries survive closing it. Visually distinct (dark wolf badge).

### 5.4 Verified vs unverified sites
- **Verified** = domain on the bundled **Tranco top-10k list** (free) **or** user-trusted, **and** not on the Safe Browsing list.
- **Unverified** = everything else → page loads, ⚠ icon beside the URL; clicking it explains why and offers **"Trust this site"** (stored locally; e.g. college ERP).
- **Known-malicious** (Safe Browsing hit) → do not load; show R-Browser block page ("Go back" / "Proceed anyway" behind a second confirmation).
- Safe Browsing: use Google Safe Browsing **Update API v4** (free, non-commercial) — local hash-prefix list, so visited URLs are not sent to Google except for full-hash confirmation on a prefix match.
- Lists refresh weekly in the background; ship a bundled copy so it works offline / on first run.

### 5.5 JIT off on unverified sites
Most 2025–26 Chrome zero-days targeted the JS JIT. On unverified sites, disable it:
- macOS: `WKWebpagePreferences` Lockdown-mode flag per navigation (macOS 14+; verify what else it disables).
- Windows: WebView2 `--js-flags=--jitless` applies per environment → run unverified tabs in a second WebView2 environment (cost: separate cookie jar; acceptable).
- Linux: WebKitGTK JIT switch is likely process-wide → v1 ships a **global** "Hardened mode" toggle on Linux instead of per-site.
Builder must verify each mechanism early (spike) and record what actually works.

### 5.6 Blocking: ads, trackers, pop-ups
- Filter lists: EasyList, EasyPrivacy, uBlock filters, Peter Lowe's (free).
- Matching: Brave's **adblock-rust** crate. WebKit (mac/Linux): compile lists to WebKit content-blocker JSON rules (WKContentRuleList / WebKitUserContentFilterStore — native, near-zero CPU). Windows: WebView2 `WebResourceRequested` filter + adblock-rust matching.
- Cosmetic filtering (hiding leftover ad boxes) via injected CSS.
- Pop-ups: `window.open` not triggered by a user click is blocked; small "pop-up blocked — allow?" notice in the URL bar.
- Per-site "disable blocking" switch.

### 5.7 Suspicious activity → auto-close
Triggers:
1. Navigation to a Safe Browsing–listed URL (from a redirect or script)
2. A download started without a user click
3. Known crypto-miner script domains (from filter lists) or sustained 100% CPU in a background tab for >30 s
4. More than 5 forced redirects in ~10 s
5. Fake "your PC is infected" / tech-support scam patterns (fullscreen + beforeunload loop + alert spam)

Action (assistant's call): **close that tab** and show an R-Browser warning page naming what was detected. If a trigger repeats in the same session, or trigger 1 fires, **close the whole browser** (session is saved first, but the offending tab is dropped from it).

### 5.8 Location prompt (Real / Fake)
- On a geolocation request: prompt **Allow real / Use fake / Block**, remembered per site.
- Fake = user-chosen city from a built-in list (default: a random major city); coordinates get small random jitter.
- Timezone and `Intl` locale offset are overridden to match the fake city, otherwise the fake leaks.
- Implemented by injecting a script before page scripts run (override `navigator.geolocation`, `Date`/`Intl` timezone). Known limit: a determined site can detect JS overrides — document it, don't over-engineer.

### 5.9 Password manager
- Google Password Manager has no API for third-party browsers → **cannot integrate directly**.
- Built-in manager: entries encrypted (AES-256-GCM); the key lives in the OS keychain (`keyring` crate → macOS Keychain / Windows Credential Manager / Linux Secret Service); unlock gated by Touch ID / Windows Hello where available.
- Detect login forms → offer save; autofill on matching origin only (exact origin, never subdomain-guess).
- **Import from Google**: user exports CSV from passwords.google.com → R-Browser imports → offers to securely delete the CSV.
- Bitwarden sync: later, not in v1.

### 5.10 Extra hardening (assistant's suggestions, accepted)
- **HTTPS-only:** upgrade `http://` navigations; if HTTPS fails, show interstitial with "continue on insecure connection".
- **Fingerprint protection (light):** generic User-Agent per OS, small noise on canvas/WebGL/audio readback, no device-specific font list. Keep it light — breaking sites is a failure (see anti-goals).
- **Clear cookies of closed sites:** when the last tab of an **unverified** site closes, delete its cookies/storage, unless the user trusted it or pinned it "keep logged in". Verified (top-list) sites keep cookies so Gmail/YouTube/banks don't log you out (planning-stage clarification, anti-goal).
- **Signed auto-updates:** Tauri updater, minisign-signed, served from GitHub Releases (free).

### 5.11 RAM & power
- Background tabs **frozen after 5 min** idle (default, user-adjustable): on Windows use WebView2 `TrySuspend`; on WebKit destroy the webview and keep URL + scroll + form state snapshot; reload on click. Tabs playing audio or the pinned/active ones are never frozen.
- Hidden tabs: timers throttled, animations paused (engine default + `visibilitychange`).
- UI webview is framework-free; no background services beyond list updates (weekly).
- Video: rely on engine hardware decoding; don't force software paths.

### 5.12 RAM benchmark (proves the 80% claim)
Script (`bench/`, Python + psutil) that:
- Opens the same **20 real sites** in Chrome and in R-Browser, waits **6 min** (past the freeze timer), then records total memory of the whole process tree (sum of all helper/renderer processes) and average CPU.
- Second scenario: **one heavy tab** (YouTube playing) — expected saving 30–50%, reported honestly.
- Prints a comparison table; run before every release.

## 6. Goals / non-goals / anti-goals

**Goals:** everything in §5 working on all three OSes; ≥80% less RAM than Chrome in the 20-tab scenario; open source on GitHub.

**Non-goals (v1):** Chrome extensions, extension store, sync across devices, mobile apps, downloads-in-VM (project 2), fake IP/Tor (project 3), pixel-identical rendering across OSes.

**Anti-goals (failure even if it "works"):**
- Breaking common sites (Gmail, YouTube, banking, college ERP) through over-aggressive blocking or fingerprint noise.
- Silently sending browsing data anywhere (no telemetry, no analytics — ever).
- Claiming "80% less RAM" without the benchmark numbers to back it.
- Any cost to Rahul.

## 7. Zero-cost distribution

- Code + releases on GitHub (free).
- macOS: ad-hoc signed `.dmg` (free); users see "unidentified developer" → README explains right-click → Open. Not notarized.
- Windows: apply to **SignPath Foundation** (free code signing for OSS); until approved, unsigned installer → SmartScreen "More info → Run anyway", explained in README.
- Linux: AppImage + `.deb` (no signing needed).
- CI: GitHub Actions free tier builds all three OSes (avoids heavy local builds).

## 8. Assistant's calls from the design review

- Minimum OS: macOS 13+, Windows 10/11, Ubuntu 22.04+ (WebKitGTK 4.1).
- Safe Browsing needs a free Google Cloud API key; it's embedded in the build like Firefox does, overridable in settings.
- Build order inside project 1: (a) tabs + URL bar + session restore + incognito, (b) blocking + HTTPS-only, (c) verified/⚠ + JIT-off spike, (d) freezing + benchmark, (e) suspicious-activity, (f) location, fingerprint, cookie clearing, (g) password manager, (h) updater + release CI. Each step leaves a runnable browser.
- Develop and test on macOS first (Rahul's machine); Windows/Linux verified via GitHub Actions builds + VMs.
- GitHub repo: created under Rahul's account only after he says so (it's public).

## 9. Left to the builder

Tab/webview architecture details within Tauri v2, project layout, UI visual design (use the logo's black + electric-blue palette), exact filter-list update mechanics, settings storage format, test strategy, keyboard-shortcut table, block-page wording.
