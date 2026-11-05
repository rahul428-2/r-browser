<p align="center"><img src="assets/logo.png" width="120" alt="R-Browser logo"></p>

# R-Browser

A security-first, lightweight desktop browser. No Chromium: built on [Tauri](https://tauri.app) and the web engine your OS already ships (WebKit on macOS).

## What it does

- **Built-in ad, tracker and pop-up blocking** (EasyList, EasyPrivacy, Peter Lowe), switchable per site.
- **HTTPS-only**: plain-http addresses are upgraded; if a site has no secure version you're asked first.
- **Unverified-site warnings**: sites outside the top 10,000 get an "Unverified" badge and open in a locked-down mode (no JIT, WebAssembly, WebGL or WebRTC) until a one-time safety check passes.
- **Idle tab freezing**: background tabs idle for 5 minutes are unloaded to free memory (never the one playing audio or with a half-typed form).
- **Password manager**: AES-256-GCM vault, key in the system keychain, Touch ID unlock, Google Passwords CSV import.
- Tabs, tab groups, drag/tear-off windows, incognito, bookmarks, history, downloads, session restore.
- **Signed auto-updates** from this repo's releases.

## Memory vs Chrome

20 real sites, measured after 6 minutes (`python3 bench/bench.py --compare`, macOS, Apple silicon):

| | RAM | CPU |
|---|---|---|
| R-Browser | 1,276 MB | 2.4 % |
| Chrome | 3,780 MB | 23.3 % |

Details in [bench/README.md](bench/README.md).

## Install (macOS 13+)

Download `R-Browser.app.tar.gz` from [Releases](../../releases/latest), unpack it and move **R-Browser.app** to Applications.
The app is not notarized (no paid certificate), so the first time: right-click it → **Open** → **Open**.
After that it updates itself.

Windows and Linux builds are planned.

## Build

```sh
cd src-tauri
cargo tauri dev --no-watch          # run
cargo test                          # tests
cargo tauri build --bundles app     # release app
```

## License

[MPL-2.0](LICENSE)
