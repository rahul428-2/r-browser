# RAM benchmark: R-Browser vs Chrome

`python3 bench/bench.py --compare` (stdlib only). Memory = phys_footprint (Activity Monitor "Memory") summed over
every process the browser started, WebKit XPC helpers included. Fresh profiles, default settings, `sites.txt`.

## 5 Nov 2026 — MacBook (Apple silicon, 16 GB), macOS 27, Chrome default settings

| scenario | browser | RAM | CPU (avg, last 60 s) | processes |
|---|---|---|---|---|
| tabs20 (wait 360 s) | R-Browser | 1,276 MB | 2.4 % | 12 |
| tabs20 (wait 360 s) | Chrome | 3,780 MB | 23.3 % | 63 |

**tabs20: R-Browser uses 66 % less RAM and ~10x less CPU.** Target was 80 % — not met yet.
heavy1 not measured (run stopped); a 25 s smoke test with YouTube gave 421 MB vs 643 MB (-35 %).
