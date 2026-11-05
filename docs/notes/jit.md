# JIT-off spike (Task 7)

## macOS — `WKWebpagePreferences.lockdownModeEnabled` per navigation (macOS 13+)

Tested 5 Nov 2026 on macOS 27.0.1 with an offscreen WKWebView harness (scratch `spike.swift`): one page loaded with
lockdown off, then on, set from `webView:decidePolicyForNavigationAction:preferences:decisionHandler:`.

| | lockdown off | lockdown on |
|---|---|---|
| 30M-iteration JS loop | 203 ms | 2298 ms (**11× slower → JIT is off**) |
| WebAssembly | yes | **no** |
| WebGL | yes | **no** |
| WebRTC (video calls) | yes | **no** |
| Web Audio | yes | **no** |
| Speech synthesis | yes | **no** |
| Gamepad | yes | **no** |
| IndexedDB (offline storage) | yes | **no** |
| Service workers (offline / push) | yes | **no** |
| MathML | yes | **no** |
| Notifications API | yes | **no** |
| FileReader | yes | **no** |
| FontFace API / SharedWorker | yes | yes |

Works per navigation, so it can be on for unverified sites only — but it is Apple's full Lockdown Mode web
profile, not just "JIT off": sites that need any of the **no** rows break while it's on.

## Windows / Linux
Not spiked (no machine): WebView2 `--js-flags=--jitless` per environment, WebKitGTK `JSC_useJIT=0`. → Task 14.
