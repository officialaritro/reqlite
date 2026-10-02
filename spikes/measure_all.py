#!/usr/bin/env python3
"""Measures the three GUI spikes one after another with one method (macOS only).

Usage: python3 spikes/measure_all.py FIXTURE_DIR
Build each spike in release mode first. RAM counts the app plus any WebKit
helper processes that appeared after launch (Tauri's webview runs in them).
"""

import http.server
import os
import re
import statistics
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
APPS = {
    "iced": "iced/target/release/reqlite-spike-iced",
    "slint": "slint/target/release/reqlite-slint-spike",
    "tauri": "tauri/src-tauri/target/release/reqlite-tauri-spike",
}
PORT = 8710
MB = 1024 * 1024


def webkit_pids() -> set[int]:
    out = subprocess.run(["pgrep", "-f", "com.apple.WebKit"], capture_output=True, text=True).stdout
    return {int(p) for p in out.split()}


def rss(pid: int) -> int:
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return int(out) * 1024 if out else 0


def footprint(pid: int) -> int:
    out = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True).stdout
    m = re.search(r"Footprint: ([\d.]+) (KB|MB|GB)", out)
    if not m:
        return 0
    scale = {"KB": 1024, "MB": MB, "GB": 1024 * MB}[m.group(2)]
    return int(float(m.group(1)) * scale)


def first_frame(exe: str) -> tuple[float, float]:
    t = time.perf_counter()
    out = subprocess.run([exe], env=dict(os.environ, REQLITE_SPIKE_EXIT_ON="first-frame"),
                         capture_output=True, text=True, timeout=60).stdout
    wall = (time.perf_counter() - t) * 1000
    m = re.search(r"first-frame ([\d.]+)", out)
    return (float(m.group(1)) if m else float("nan")), wall


def idle(exe: str) -> dict[str, int]:
    before = webkit_pids()
    p = subprocess.Popen([exe], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(5)
    helpers = webkit_pids() - before
    result = {
        "app_rss": rss(p.pid),
        "total_rss": rss(p.pid) + sum(rss(h) for h in helpers),
        "app_footprint": footprint(p.pid),
        "total_footprint": footprint(p.pid) + sum(footprint(h) for h in helpers),
    }
    p.terminate()
    p.wait()
    return result


def big_load(exe: str) -> dict[str, float]:
    before = webkit_pids()
    env = dict(os.environ, REQLITE_SPIKE_URL=f"http://127.0.0.1:{PORT}/array.json",
               REQLITE_SPIKE_EXIT_ON="viewer-ready")
    p = subprocess.Popen([exe], env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    peak = 0
    while p.poll() is None:
        helpers = webkit_pids() - before
        peak = max(peak, rss(p.pid) + sum(rss(h) for h in helpers))
        time.sleep(0.05)
    out = p.stdout.read() if p.stdout else ""
    ready = re.search(r"viewer-ready ([\d.]+)", out)
    gap = re.search(r"max-frame-gap ([\d.]+)", out)
    return {
        "ready_ms": float(ready.group(1)) if ready else float("nan"),
        "gap_ms": float(gap.group(1)) if gap else float("nan"),
        "peak_total_rss": peak,
    }


def med(xs: list[float]) -> str:
    return f"{statistics.median(xs):.0f} ({min(xs):.0f} to {max(xs):.0f})"


def main() -> None:
    fixtures = sys.argv[1]
    handler = lambda *a: http.server.SimpleHTTPRequestHandler(*a, directory=fixtures)  # noqa: E731
    http.server.SimpleHTTPRequestHandler.log_message = lambda *a: None  # type: ignore[method-assign]
    server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    print("| | " + " | ".join(APPS) + " |")
    print("|---|" + "---|" * len(APPS))
    rows: dict[str, list[str]] = {}
    for name, rel in APPS.items():
        exe = os.path.join(HERE, rel)
        rows.setdefault("Binary", []).append(f"{os.path.getsize(exe) / MB:.1f} MB")
        frames = [first_frame(exe) for _ in range(5)]
        rows.setdefault("first-frame ms", []).append(med([f[0] for f in frames]))
        rows.setdefault("launch to exit ms", []).append(med([f[1] for f in frames]))
        idles = [idle(exe) for _ in range(3)]
        for key, label in [("app_rss", "Idle RSS, app"), ("total_rss", "Idle RSS, total"),
                           ("app_footprint", "Idle footprint, app"), ("total_footprint", "Idle footprint, total")]:
            rows.setdefault(label + " MB", []).append(med([i[key] / MB for i in idles]))
        loads = [big_load(exe) for _ in range(3)]
        rows.setdefault("50 MB viewer-ready ms", []).append(med([load["ready_ms"] for load in loads]))
        rows.setdefault("50 MB max-frame-gap ms", []).append(med([load["gap_ms"] for load in loads]))
        rows.setdefault("50 MB peak RSS, total MB", []).append(med([load["peak_total_rss"] / MB for load in loads]))
        print(f"measured {name}", file=sys.stderr)
    for label, values in rows.items():
        print(f"| {label} | " + " | ".join(values) + " |")
    server.shutdown()


if __name__ == "__main__":
    main()
