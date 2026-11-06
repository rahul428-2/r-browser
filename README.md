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

## Download

| Windows 10/11 (64-bit) | macOS 13+ (Apple silicon) |
|---|---|
| **[⬇ R-Browser-Windows-Setup.exe](https://github.com/rahul428-2/r-browser/releases/latest/download/R-Browser-Windows-Setup.exe)** | **[⬇ R-Browser.app.tar.gz](https://github.com/rahul428-2/r-browser/releases/latest/download/R-Browser.app.tar.gz)** |

**Windows:** run the setup file. Until the installer is code-signed, SmartScreen may say "Windows protected your PC": click **More info** → **Run anyway**.
**macOS:** unpack it and move **R-Browser.app** to Applications. The app is not notarized (no paid certificate), so the first time: right-click it → **Open** → **Open**.

Both update themselves after that. Linux is planned. All files: [Releases](https://github.com/rahul428-2/r-browser/releases/latest).

## Code signing policy

Windows installers will be signed once the application is approved: free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

- Committers and reviewers: [rahul428-2](https://github.com/rahul428-2)
- Approvers: [rahul428-2](https://github.com/rahul428-2)

Every signed file is built from this repository by GitHub Actions ([windows.yml](.github/workflows/windows.yml)); nothing built on a personal machine is signed.

**Privacy:** this program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it. The one exception is the update check: R-Browser downloads `latest.json` from this repository's GitHub Releases (subject to [GitHub's privacy statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement)). Websites you open, and the search engine you use (Google by default), get what any browser sends them.

## Build

```sh
cd src-tauri
cargo tauri dev --no-watch          # run
cargo test                          # tests
cargo tauri build --bundles app     # release app
```

## License

[MPL-2.0](LICENSE)
