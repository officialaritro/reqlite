#!/usr/bin/env python3
"""Reruns every SPEC.md measurement for the Tauri spike. Needs the fixture server on 8703."""
import os, re, statistics, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.join(HERE, "src-tauri/target/release/reqlite-tauri-spike")
URL = "http://127.0.0.1:8703/array.json"
RUNS = int(os.environ.get("RUNS", "5"))


def procs():
    out = subprocess.run(["ps", "-axo", "pid=,rss=,comm="], capture_output=True, text=True).stdout
    rows = {}
    for line in out.splitlines():
        pid, rss, comm = line.strip().split(None, 2)
        rows[int(pid)] = (int(rss), comm)
    return rows


def webkit(rows, baseline):
    return {p: r for p, (r, c) in rows.items() if "com.apple.WebKit." in c and p not in baseline}


def settle():
    time.sleep(3)


def summary(xs):
    return f"median {statistics.median(xs):.0f}, min {min(xs):.0f}, max {max(xs):.0f}  (runs: {', '.join(f'{x:.0f}' for x in xs)})"


def env(**kw):
    e = {k: v for k, v in os.environ.items() if not k.startswith("REQLITE_SPIKE_")}
    e.update(kw)
    return e


def first_frame():
    ff, wall = [], []
    for _ in range(RUNS):
        t = time.perf_counter()
        out = subprocess.run([BIN], env=env(REQLITE_SPIKE_EXIT_ON="first-frame"), capture_output=True, text=True, timeout=60).stdout
        wall.append((time.perf_counter() - t) * 1000)
        ff.append(float(re.search(r"first-frame (\d+)", out).group(1)))
        settle()
    print("first-frame ms:", summary(ff))
    print("launch-to-exit wall ms:", summary(wall))


def idle():
    app, total = [], []
    for _ in range(RUNS):
        base = set(procs())
        p = subprocess.Popen([BIN], env=env(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(5)
        rows = procs()
        a = rows[p.pid][0]
        helpers = webkit(rows, base)
        app.append(a / 1024)
        total.append((a + sum(helpers.values())) / 1024)
        names = sorted(rows[h][1].rsplit("/", 1)[-1] for h in helpers)
        p.terminate(); p.wait()
        print(f"  idle run: app {a/1024:.1f} MB, helpers {[(n, round(rows[h][0]/1024,1)) for h, n in zip(sorted(helpers, key=lambda h: rows[h][1].rsplit('/',1)[-1]), names)]}")
        settle()
    print("idle RSS app MB:", summary(app))
    print("idle RSS app+WebKit MB:", summary(total))


def big(extra=None):
    ready, gap, time_peak, app_peak, total_peak, sum_of_maxes = [], [], [], [], [], []
    for _ in range(RUNS):
        base = set(procs())
        p = subprocess.Popen(["/usr/bin/time", "-l", BIN], env=env(REQLITE_SPIKE_URL=URL, REQLITE_SPIKE_EXIT_ON="viewer-ready", **(extra or {})),
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        best_total, best_app, per = 0, 0, {}
        while p.poll() is None:
            rows = procs()
            kids = [q for q, (r, c) in rows.items() if c.endswith("reqlite-tauri-spike") and q not in base]
            a = sum(rows[k][0] for k in kids)
            helpers = webkit(rows, base)
            for h, r in helpers.items():
                per[h] = max(per.get(h, 0), r)
            best_app = max(best_app, a)
            best_total = max(best_total, a + sum(helpers.values()))
            time.sleep(0.05)
        out, err = p.communicate()
        ready.append(float(re.search(r"viewer-ready (\d+)", out).group(1)))
        gap.append(float(re.search(r"max-frame-gap ([\d.]+)", out).group(1)))
        tp = int(re.search(r"(\d+)\s+maximum resident set size", err).group(1)) / 1048576
        time_peak.append(tp)
        app_peak.append(best_app / 1024)
        total_peak.append(best_total / 1024)
        sum_of_maxes.append(tp + sum(per.values()) / 1024)
        settle()
    print("viewer-ready ms:", summary(ready))
    print("max-frame-gap ms:", summary(gap))
    print("peak RSS app, /usr/bin/time -l MB:", summary(time_peak))
    print("peak RSS app, 50 ms sampler MB:", summary(app_peak))
    print("peak RSS app+WebKit, 50 ms sampler (max of summed sample) MB:", summary(total_peak))
    print("peak RSS app(time -l) + per-helper peaks MB (upper bound):", summary(sum_of_maxes))


def big_after_paint():
    big({"REQLITE_SPIKE_SEND_AFTER_PAINT": "1"})


def editor():
    for _ in range(RUNS):
        out = subprocess.run([BIN], env=env(REQLITE_SPIKE_EXIT_ON="editor"), capture_output=True, text=True, timeout=120).stdout
        print("  editor run:", " | ".join(out.strip().splitlines()))
        settle()


def scroll():
    for _ in range(RUNS):
        out = subprocess.run([BIN], env=env(REQLITE_SPIKE_URL=URL, REQLITE_SPIKE_EXIT_ON="scroll"), capture_output=True, text=True, timeout=120).stdout
        print("  scroll run:", " | ".join(out.strip().splitlines()))
        settle()


if __name__ == "__main__":
    for name in sys.argv[1:] or ["first_frame", "idle", "big", "editor", "scroll"]:
        print(f"== {name}", flush=True)
        globals()[name]()
        sys.stdout.flush()
