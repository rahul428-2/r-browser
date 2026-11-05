#!/usr/bin/env python3
"""RAM/CPU benchmark: R-Browser vs Chrome (macOS).

  python3 bench/bench.py --browser rbrowser|chrome --scenario tabs20|heavy1 [--wait 360]
  python3 bench/bench.py --compare            # both browsers, both scenarios, table with % saving

Memory = sum of phys_footprint (Activity Monitor's "Memory" column) over every process the browser is
responsible for. WebKit's helpers (WebContent, GPU, Networking) are XPC services parented to launchd, not to
the app, so processes are matched by macOS's "responsible pid" instead of the process tree — other WebKit apps
(Safari, Mail, a running R-Browser) are not counted. CPU = average % of one core over the last 60 s of the wait.
Each browser runs with a throwaway profile, default settings, so neither has an advantage from cached data.
"""
import argparse, ctypes, os, shutil, signal, subprocess, sys, tempfile, time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RB = ROOT.parent / "src-tauri/target/release/bundle/macos/R-Browser.app/Contents/MacOS/r-browser"
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
HEAVY = ["https://www.youtube.com/"]

libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
libc.responsibility_get_pid_responsible_for_pid.argtypes = [ctypes.c_int]

class RUsage(ctypes.Structure):  # rusage_info_v0
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(n, ctypes.c_uint64) for n in
        "user system pkg_idle_wkups interrupt_wkups pageins wired resident phys_footprint start abstime".split()]

class Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]
tb = Timebase(); libc.mach_timebase_info(ctypes.byref(tb))
TICK_NS = tb.numer / tb.denom  # rusage cpu times are mach ticks on Apple silicon

def usage(pid):
    r = RUsage()
    return r if libc.proc_pid_rusage(pid, 0, ctypes.byref(r)) == 0 else None

def pids():
    return set(map(int, subprocess.run(["ps", "-axo", "pid="], capture_output=True, text=True).stdout.split()))

def procs(main, before):
    """pid -> rusage for the browser and the helpers started for it. A browser launched from a script shares the
    script's "responsible" process, and so do its WebKit XPC helpers; anything that already ran before the launch
    (e.g. the user's own R-Browser or Chrome) is excluded."""
    who = libc.responsibility_get_pid_responsible_for_pid(main)
    res = {}
    for p in pids() - before:
        if p == main or libc.responsibility_get_pid_responsible_for_pid(p) == who:
            u = usage(p)  # None for processes already gone (ps itself)
            if u: res[p] = u
    return res

def launch(browser, urls, tmp):
    if browser == "chrome":
        args = [CHROME, f"--user-data-dir={tmp}", "--no-first-run", "--no-default-browser-check", *urls]
        env = os.environ
    else:
        if not RB.exists(): sys.exit(f"build first: cd src-tauri && cargo tauri build --bundles app ({RB} missing)")
        args = [str(RB), *urls]
        env = {**os.environ, "HOME": tmp}  # own session/library files, the user's R-Browser data is untouched
    return subprocess.Popen(args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)

def run(browser, scenario, wait):
    urls = [l.strip() for l in (ROOT / "sites.txt").read_text().splitlines() if l.strip() and not l.startswith("#")]
    urls = urls[:20] if scenario == "tabs20" else HEAVY
    tmp = tempfile.mkdtemp(prefix=f"bench-{browser}-")
    before = pids()
    p = launch(browser, urls, tmp)
    try:
        time.sleep(max(wait - 60, 0))
        a, t0 = procs(p.pid, before), time.monotonic()
        time.sleep(min(wait, 60))
        b, t1 = procs(p.pid, before), time.monotonic()
        if p.poll() is not None: sys.exit(f"{browser} exited early (code {p.returncode})")
        mb = sum(u.phys_footprint for u in b.values()) / 2**20
        cpu_ns = sum((u.user + u.system) - (a[pid].user + a[pid].system) for pid, u in b.items() if pid in a) * TICK_NS
        cpu = cpu_ns / ((t1 - t0) * 1e9) * 100
        return mb, cpu, len(b)
    finally:
        os.killpg(p.pid, signal.SIGTERM)
        try: p.wait(10)
        except subprocess.TimeoutExpired: os.killpg(p.pid, signal.SIGKILL)
        time.sleep(3)
        shutil.rmtree(tmp, ignore_errors=True)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--browser", choices=["rbrowser", "chrome"])
    ap.add_argument("--scenario", choices=["tabs20", "heavy1"], default="tabs20")
    ap.add_argument("--wait", type=int, help="seconds before measuring (default 360 for tabs20, 90 for heavy1)")
    ap.add_argument("--compare", action="store_true")
    a = ap.parse_args()
    if not a.compare and not a.browser: ap.error("--browser or --compare")
    jobs = [(b, s) for s in ("tabs20", "heavy1") for b in ("rbrowser", "chrome")] if a.compare else [(a.browser, a.scenario)]
    res = {}
    print("browser   scenario  total_mb  avg_cpu_pct  procs", flush=True)
    for b, s in jobs:
        mb, cpu, n = run(b, s, a.wait or (360 if s == "tabs20" else 90))
        res[b, s] = mb
        print(f"{b:<9} {s:<9} {mb:8.0f}  {cpu:11.1f}  {n:5}", flush=True)
    for s in ("tabs20", "heavy1"):
        if ("rbrowser", s) in res and ("chrome", s) in res:
            print(f"{s}: R-Browser uses {(1 - res['rbrowser', s] / res['chrome', s]) * 100:.0f}% less RAM than Chrome")

if __name__ == "__main__":
    main()
