#!/usr/bin/env python3
"""Checks Reqlite's resource budgets and crate boundaries. Exits 1 on any miss.

Usage: scripts/budgets.py   (builds the release binaries and examples first)
Set REQLITE_BUDGET_SCALE=0.01 to shrink every budget and watch the check fail.
"""

import http.server
import json
import os
import subprocess
import sys
import tempfile
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = ".exe" if os.name == "nt" else ""
CLI = os.path.join(ROOT, "target", "release", "reqlite" + EXE)
GUI = os.path.join(ROOT, "target", "release", "reqlite-gui" + EXE)
OPEN = os.path.join(ROOT, "target", "release", "examples", "open" + EXE)
SCALE = float(os.environ.get("REQLITE_BUDGET_SCALE", "1"))
MB = 1024 * 1024

# Crate -> workspace crates it may depend on (Out of the Tar Pit, Figure 1).
ALLOWED: dict[str, set[str]] = {
    "reqlite-format": set(),
    "reqlite-engine": {"reqlite-format"},
    "reqlite-import": {"reqlite-format"},
    "reqlite-viewer": set(),
    "reqlite-store": {"reqlite-format", "reqlite-engine"},
    "reqlite-gui": {
        "reqlite-format",
        "reqlite-import",
        "reqlite-engine",
        "reqlite-store",
        "reqlite-viewer",
    },
    "reqlite": {
        "reqlite-format",
        "reqlite-engine",
        "reqlite-store",
        "reqlite-import",
        "reqlite-viewer",
    },
}

failures: list[str] = []


def check(name: str, value: float, budget: float, unit: str) -> None:
    budget *= SCALE
    ok = value <= budget
    shown = (
        (lambda v: f"{v / MB:.1f} MB") if unit == "bytes" else (lambda v: f"{v:.0f} ms")
    )
    print(f"{'ok  ' if ok else 'MISS'} {name}: {shown(value)} (budget {shown(budget)})")
    if not ok:
        failures.append(name)


def peak_rss(cmd: list[str], **kw: object) -> int:
    """Runs cmd in a fresh child of a fresh Python, so ru_maxrss covers only cmd."""
    probe = (
        "import resource, subprocess, sys, json;"
        "r = subprocess.run(sys.argv[1:], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE);"
        "m = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss;"
        "print(json.dumps([r.returncode, m, r.stderr.decode(errors='replace')[-500:]]))"
    )
    out = subprocess.run(
        [sys.executable, "-c", probe, *cmd],
        capture_output=True,
        text=True,
        check=True,
        **kw,
    )
    code, maxrss, err = json.loads(out.stdout)
    if code != 0:
        raise SystemExit(f"{cmd} exited {code}: {err}")
    return maxrss if sys.platform == "darwin" else maxrss * 1024


def crate_boundaries() -> None:
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )
    names = {p["name"] for p in meta["packages"]}
    for pkg in meta["packages"]:
        deps = {
            d["name"]
            for d in pkg["dependencies"]
            if d["name"] in names and d["kind"] is None
        }
        allowed = ALLOWED.get(pkg["name"])
        if allowed is None:
            print(f"MISS crate {pkg['name']} has no entry in ALLOWED")
            failures.append(pkg["name"])
        elif not deps <= allowed:
            print(f"MISS crate {pkg['name']} depends on {sorted(deps - allowed)}")
            failures.append(pkg["name"])
    print(
        f"ok   crate boundaries checked for {len(meta['packages'])} crates"
        if not failures
        else ""
    )


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format, *args):
        pass


def build() -> None:
    """Builds what the checks measure. `--examples` alone skips the binaries,
    which leaves a missing or stale CLI, so both target kinds are named."""
    subprocess.run(
        ["cargo", "build", "--release", "--workspace", "--bins", "--examples"],
        cwd=ROOT,
        check=True,
    )


def main() -> None:
    build()
    crate_boundaries()
    check("CLI binary size", os.path.getsize(CLI), 25 * MB, "bytes")
    check("GUI binary size", os.path.getsize(GUI), 25 * MB, "bytes")

    runs = []
    for _ in range(5):
        t = time.perf_counter()
        subprocess.run([CLI, "--version"], check=True, capture_output=True)
        runs.append((time.perf_counter() - t) * 1000)
    check("CLI cold start", sorted(runs)[2], 300, "ms")

    with tempfile.TemporaryDirectory() as fixtures:
        subprocess.run(
            [sys.executable, os.path.join(ROOT, "scripts", "fixtures.py"), fixtures],
            check=True,
            capture_output=True,
        )
        for name in ("array.json", "nested.json", "string.json", "markup.xml"):
            path = os.path.join(fixtures, name)
            check(
                f"viewer peak RAM, 50 MB {name}",
                peak_rss([OPEN, path]),
                50 * MB,
                "bytes",
            )

        server = http.server.ThreadingHTTPServer(
            ("127.0.0.1", 0), lambda *a: Quiet(*a, directory=fixtures)
        )
        threading.Thread(target=server.serve_forever, daemon=True).start()
        port = server.server_address[1]
        with tempfile.TemporaryDirectory() as work:
            req = os.path.join(work, "big.toml")
            with open(req, "w") as f:
                f.write(
                    f'version = 1\nname = "big"\nurl = "http://127.0.0.1:{port}/array.json"\n'
                )
            env = dict(os.environ, REQLITE_DATA_DIR=os.path.join(work, "data"))
            check(
                "CLI peak RAM, send 50 MB",
                peak_rss([CLI, "send", req], env=env),
                50 * MB,
                "bytes",
            )
        server.shutdown()

    if failures:
        print(f"\n{len(failures)} budget(s) missed: {', '.join(failures)}")
        sys.exit(1)
    print("\nall budgets met")


if __name__ == "__main__":
    main()
