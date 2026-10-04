#!/usr/bin/env python3
"""Checks the GUI idle memory budgets (issue #1) and cold start on macOS.
Exits 1 on a miss.

Usage: scripts/gui_idle.py EXECUTABLE [--runs N]

The app must open its default 1000x800 window. After 5 s of idle the script reads:
  footprint  physical footprint, what Activity Monitor shows (`footprint -p`)
  drawables  dirty IOSurface memory, the window's frame buffers (`vmmap --summary`)

Budgets:
  footprint              under 70 MB, on a 2x display only (it scales with the display)
  footprint - drawables  under 45 MB, on any display (memory Reqlite controls)
  idle CPU               under 0.05 s of CPU time over the next 5 s, on a Mac with
                         a real GPU only. More means a redraw loop or an animation
                         that never stops.
  first frame            under 300 ms, on a Mac with a real GPU only. The app prints
                         `first-frame <ms>` and exits when
                         REQLITE_GUI_EXIT_ON_FIRST_FRAME is set.
A virtual machine (a CI runner) lists no GPU or a paravirtual one. It renders in
software and its CPU times include the hypervisor, so idle CPU and first frame are
printed but not checked there.
Set REQLITE_BUDGET_SCALE=0.01 to shrink every budget and watch the check fail.
"""

import argparse
import atexit
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

MB = 1024 * 1024
SCALE = float(os.environ.get("REQLITE_BUDGET_SCALE", "1"))
UNITS = {"K": 1024, "KB": 1024, "M": MB, "MB": MB, "G": 1024 * MB, "GB": 1024 * MB}
IDLE_SAMPLE = 5.0
# An empty data folder, so the app opens its first-start window every time
# instead of the folder you used last.
DATA_DIR = tempfile.mkdtemp(prefix="reqlite-gui-idle-")
atexit.register(shutil.rmtree, DATA_DIR, ignore_errors=True)


def size(text: str) -> int:
    m = re.fullmatch(r"([\d.]+)\s*([KMG]B?)", text.strip())
    if not m:
        raise SystemExit(f"cannot read size {text!r}")
    return int(float(m.group(1)) * UNITS[m.group(2)])


def display_scale() -> float:
    out = subprocess.run(
        [
            "osascript",
            "-l",
            "JavaScript",
            "-e",
            'ObjC.import("AppKit"); $.NSScreen.mainScreen.backingScaleFactor',
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return float(out.strip())


def cpu_time(pid: int) -> float:
    """Total CPU seconds the process used. `ps` prints [hh:]mm:ss.ss."""
    out = subprocess.run(
        ["ps", "-o", "time=", "-p", str(pid)],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    seconds = 0.0
    for part in out.split(":"):
        seconds = seconds * 60 + float(part)
    return seconds


def measure(exe: str) -> tuple[int, int, float]:
    p = subprocess.Popen(
        [exe],
        env=dict(os.environ, REQLITE_DATA_DIR=DATA_DIR),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(5)
        before = cpu_time(p.pid)
        time.sleep(IDLE_SAMPLE)
        idle_cpu = cpu_time(p.pid) - before
        if p.poll() is not None:
            raise SystemExit(
                f"{exe} exited with code {p.returncode} before the measurement"
            )
        fp_out = subprocess.run(
            ["footprint", "-p", str(p.pid)], capture_output=True, text=True, check=True
        ).stdout
        fp = re.search(r"Footprint: ([\d.]+ [KMG]B)", fp_out)
        vm_out = subprocess.run(
            ["vmmap", "--summary", str(p.pid)],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        # Columns: VIRTUAL RESIDENT DIRTY ... The third size is the dirty size.
        surf = re.search(r"^IOSurface\s+(\S+)\s+(\S+)\s+(\S+)", vm_out, re.MULTILINE)
        if not fp or not surf:
            raise SystemExit(f"cannot read footprint or IOSurface for {exe}")
        return size(fp.group(1)), size(surf.group(3)), idle_cpu
    finally:
        p.terminate()
        p.wait()


def gpu() -> str:
    out = subprocess.run(
        ["system_profiler", "SPDisplaysDataType", "-json"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    models = [
        g.get("sppci_model", "unknown") for g in json.loads(out)["SPDisplaysDataType"]
    ]
    return ", ".join(models) or "none"


def first_frame(exe: str) -> float:
    env = dict(
        os.environ, REQLITE_GUI_EXIT_ON_FIRST_FRAME="1", REQLITE_DATA_DIR=DATA_DIR
    )
    out = subprocess.run(
        [exe], env=env, capture_output=True, text=True, timeout=60, check=False
    ).stdout
    m = re.search(r"first-frame ([\d.]+)", out)
    if not m:
        raise SystemExit(f"{exe} did not print first-frame; output: {out[-300:]!r}")
    return float(m.group(1))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("exe")
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()

    if sys.platform != "darwin":
        raise SystemExit("gui_idle.py measures with macOS tools; run it on macOS")
    scale = display_scale()
    runs = [measure(args.exe) for _ in range(args.runs)]
    footprint = statistics.median(f for f, _, _ in runs)
    own = statistics.median(f - d for f, d, _ in runs)
    drawables = statistics.median(d for _, d, _ in runs)
    # The median, like the other figures: a redraw loop shows in every run, while
    # the first launch of a fresh binary can do one-off work.
    idle_cpu = statistics.median(c for _, _, c in runs)
    print(f"display scale {scale:g}, {args.runs} runs, medians")
    print(f"     drawables (window frame buffers): {drawables / MB:.1f} MB")

    failures: list[str] = []

    def check(name: str, value: float, budget: float) -> None:
        budget *= SCALE
        ok = value <= budget
        print(
            f"{'ok  ' if ok else 'MISS'} {name}: {value / MB:.1f} MB (budget {budget / MB:.1f} MB)"
        )
        if not ok:
            failures.append(name)

    if scale == 2:
        check("idle footprint", footprint, 70 * MB)
    else:
        print(
            f"skip idle footprint: {footprint / MB:.1f} MB; its budget is defined on a 2x display only"
        )
    check("idle footprint minus drawables", own, 45 * MB)

    model = gpu()
    real_gpu = model != "none" and "paravirtual" not in model.lower()

    cpu_runs = ", ".join(f"{c:.2f}" for _, _, c in runs)
    if real_gpu:
        cpu_budget = 0.05 * SCALE
        ok = idle_cpu <= cpu_budget
        print(
            f"{'ok  ' if ok else 'MISS'} idle CPU: {idle_cpu:.2f} s over {IDLE_SAMPLE:g} s "
            f"(budget {cpu_budget:.2f} s; runs {cpu_runs})"
        )
        if not ok:
            failures.append("idle CPU")
    else:
        print(
            f"skip idle CPU: {idle_cpu:.2f} s over {IDLE_SAMPLE:g} s ({cpu_runs}); "
            f"GPU is {model}, the budget assumes a real GPU"
        )

    starts = sorted(first_frame(args.exe) for _ in range(5))
    start = starts[2]
    runs_ms = ", ".join(f"{t:.0f}" for t in starts)
    if not real_gpu:
        print(
            f"skip first frame: {start:.0f} ms ({runs_ms}); GPU is {model}, the budget assumes a real GPU"
        )
    else:
        start_budget = 300 * SCALE
        ok = start <= start_budget
        print(
            f"{'ok  ' if ok else 'MISS'} first frame: {start:.0f} ms "
            f"(budget {start_budget:.0f} ms; runs {runs_ms}; GPU {model})"
        )
        if not ok:
            failures.append("first frame")

    if failures:
        sys.exit(1)


if __name__ == "__main__":
    main()
