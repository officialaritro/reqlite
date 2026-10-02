#!/usr/bin/env python3
"""Checks the GUI idle memory budgets (issue #1) and cold start on macOS.
Exits 1 on a miss.

Usage: scripts/gui_idle.py EXECUTABLE [--runs N]

The app must open its default 1000x800 window. After 5 s of idle the script reads:
  footprint  physical footprint, what Activity Monitor shows (`footprint -p`)
  drawables  dirty IOSurface memory, the window's frame buffers (`vmmap --summary`)

Budgets:
  footprint              under 60 MB, on a 2x display only (it scales with the display)
  footprint - drawables  under 35 MB, on any display (memory Reqlite controls)
  first frame            under 300 ms, on a Mac with a real GPU only. A virtual
                         machine (a CI runner) has a paravirtual GPU, so the time
                         is printed but not checked there. The app prints
                         `first-frame <ms>` and exits when
                         REQLITE_GUI_EXIT_ON_FIRST_FRAME is set.
Set REQLITE_BUDGET_SCALE=0.01 to shrink both budgets and watch the check fail.
"""

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time

MB = 1024 * 1024
SCALE = float(os.environ.get("REQLITE_BUDGET_SCALE", "1"))
UNITS = {"K": 1024, "KB": 1024, "M": MB, "MB": MB, "G": 1024 * MB, "GB": 1024 * MB}


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


def measure(exe: str) -> tuple[int, int]:
    p = subprocess.Popen([exe], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        time.sleep(5)
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
        return size(fp.group(1)), size(surf.group(3))
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
    env = dict(os.environ, REQLITE_GUI_EXIT_ON_FIRST_FRAME="1")
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
    footprint = statistics.median(f for f, _ in runs)
    own = statistics.median(f - d for f, d in runs)
    drawables = statistics.median(d for _, d in runs)
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
        check("idle footprint", footprint, 60 * MB)
    else:
        print(
            f"skip idle footprint: {footprint / MB:.1f} MB; its budget is defined on a 2x display only"
        )
    check("idle footprint minus drawables", own, 35 * MB)

    starts = sorted(first_frame(args.exe) for _ in range(5))
    start = starts[2]
    runs_ms = ", ".join(f"{t:.0f}" for t in starts)
    model = gpu()
    if "paravirtual" in model.lower():
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
