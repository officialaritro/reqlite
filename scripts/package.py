#!/usr/bin/env python3
"""Builds the release archives for one platform into dist/, then checks them.

Usage:
  scripts/package.py --target TARGET
  scripts/package.py --check-tag vX.Y.Z

TARGET is a Rust target triple, or `macos-universal`, which builds for Apple
silicon and Intel and joins the two with `lipo`. On macOS the DMG is built too,
by scripts/macos-app.sh.

Each archive holds `reqlite` and `reqlite-gui`, the README and the licences.
After writing it, the script unpacks it and runs both binaries with --version;
each must print the workspace version.
"""

import argparse
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"
BINARIES = ("reqlite", "reqlite-gui")
DOCS = ("README.md", "LICENSE-MIT", "LICENSE-APACHE")
# The name each platform has in a file name.
PLATFORMS = {
    "macos-universal": "macos-universal",
    "aarch64-apple-darwin": "macos-arm64",
    "x86_64-apple-darwin": "macos-x86_64",
    "x86_64-unknown-linux-gnu": "linux-x86_64",
    "aarch64-unknown-linux-gnu": "linux-arm64",
    "x86_64-pc-windows-msvc": "windows-x86_64",
}


def version() -> str:
    with open(ROOT / "Cargo.toml", "rb") as f:
        return tomllib.load(f)["workspace"]["package"]["version"]


def run(*cmd: str | Path) -> None:
    print("+", " ".join(str(c) for c in cmd), flush=True)
    subprocess.run([str(c) for c in cmd], cwd=ROOT, check=True)


def build(target: str) -> Path:
    """Builds both binaries for `target` and returns the folder that holds them."""
    run(
        "cargo",
        "build",
        "--release",
        "--locked",
        "-p",
        "reqlite",
        "-p",
        "reqlite-gui",
        "--target",
        target,
    )
    return ROOT / "target" / target / "release"


def universal() -> Path:
    """Builds for both Mac CPUs and joins each binary into one."""
    arm, intel = build("aarch64-apple-darwin"), build("x86_64-apple-darwin")
    out = ROOT / "target" / "universal-apple-darwin" / "release"
    out.mkdir(parents=True, exist_ok=True)
    for name in BINARIES:
        run("lipo", "-create", arm / name, intel / name, "-output", out / name)
    return out


def archive(bins: Path, platform: str, windows: bool) -> Path:
    name = f"reqlite-{version()}-{platform}"
    stage = DIST / name
    shutil.rmtree(stage, ignore_errors=True)
    stage.mkdir(parents=True)
    exe = ".exe" if windows else ""
    for b in BINARIES:
        shutil.copy2(bins / f"{b}{exe}", stage / f"{b}{exe}")
    for doc in DOCS:
        shutil.copy2(ROOT / doc, stage / doc)
    if windows:
        path = DIST / f"{name}.zip"
        with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
            for f in sorted(stage.iterdir()):
                z.write(f, f"{name}/{f.name}")
    else:
        path = DIST / f"{name}.tar.gz"
        with tarfile.open(path, "w:gz") as t:
            t.add(stage, arcname=name)
    shutil.rmtree(stage)
    return path


def check(path: Path, windows: bool) -> None:
    """Unpacks `path` and checks that each binary prints the version."""
    want = version()
    with tempfile.TemporaryDirectory() as tmp:
        if windows:
            with zipfile.ZipFile(path) as z:
                z.extractall(tmp)
        else:
            with tarfile.open(path) as t:
                t.extractall(tmp, filter="data")
        (folder,) = Path(tmp).iterdir()
        missing = [d for d in DOCS if not (folder / d).is_file()]
        if missing:
            raise SystemExit(f"{path.name} lacks {', '.join(missing)}")
        for b in BINARIES:
            exe = folder / (f"{b}.exe" if windows else b)
            out = subprocess.run(
                [str(exe), "--version"], capture_output=True, text=True, check=True
            ).stdout.strip()
            if out != f"{b} {want}":
                raise SystemExit(
                    f"{path.name}: {b} --version printed {out!r}, not {b} {want!r}"
                )
            print(f"ok   {path.name}: {out}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target")
    parser.add_argument("--check-tag", metavar="TAG")
    args = parser.parse_args()

    if args.check_tag:
        if args.check_tag != f"v{version()}":
            raise SystemExit(
                f"tag {args.check_tag} does not match the workspace version {version()}; set it in Cargo.toml first"
            )
        print(f"ok   tag {args.check_tag} matches the workspace version")
        return
    if args.target not in PLATFORMS:
        raise SystemExit(f"--target must be one of: {', '.join(PLATFORMS)}")

    DIST.mkdir(exist_ok=True)
    windows = "windows" in args.target
    bins = universal() if args.target == "macos-universal" else build(args.target)
    path = archive(bins, PLATFORMS[args.target], windows)
    check(path, windows)
    if "apple" in args.target or args.target == "macos-universal":
        dmg = DIST / f"reqlite-{version()}-{PLATFORMS[args.target]}.dmg"
        run("sh", "scripts/macos-app.sh", bins / "reqlite-gui", dmg)
        print(f"ok   {dmg.name}")
    for f in sorted(DIST.iterdir()):
        print(f"     {f.name} {f.stat().st_size / 1048576:.1f} MB")


if __name__ == "__main__":
    sys.exit(main())
