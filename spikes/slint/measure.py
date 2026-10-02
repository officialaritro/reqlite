import os, re, statistics, subprocess, sys, time

B = "target/release/reqlite-slint-spike"
URL = "http://127.0.0.1:8702/array.json"
BODY = sys.argv[1] if len(sys.argv) > 1 else "body-1mb.json"

def run(env):
    e = dict(os.environ, **env)
    t = time.perf_counter()
    p = subprocess.run(["/usr/bin/time", "-l", B], env=e, capture_output=True, text=True)
    wall = (time.perf_counter() - t) * 1000
    rss = int(re.search(r"(\d+)\s+maximum resident set size", p.stderr).group(1))
    vals = dict((m[0], float(m[1])) for m in re.findall(r"^([\w-]+) ([\d.]+)$", p.stdout, re.M))
    return p.returncode, wall, rss, vals, p.stdout

def stats(name, xs, unit):
    print(f"{name}: median {statistics.median(xs):.1f} {unit}, min {min(xs):.1f}, max {max(xs):.1f}  {[round(x,1) for x in xs]}")

print("binary bytes", os.path.getsize(B))

ff, wall = [], []
for _ in range(5):
    rc, w, _, v, _ = run({"REQLITE_SPIKE_EXIT_ON": "first-frame"})
    assert rc == 0
    ff.append(v["first-frame"]); wall.append(w)
stats("first-frame", ff, "ms"); stats("first-frame wall launch-to-exit", wall, "ms")

idle = []
for _ in range(5):
    p = subprocess.Popen([B], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(5)
    idle.append(int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(p.pid)])) / 1024)
    p.kill(); p.wait()
stats("idle RSS", idle, "MiB")

vr, gap, gap2, peak = [], [], [], []
for _ in range(5):
    rc, _, rss, v, _ = run({"REQLITE_SPIKE_EXIT_ON": "viewer-ready", "REQLITE_SPIKE_URL": URL})
    assert rc == 0
    vr.append(v["viewer-ready"]); gap.append(v["max-frame-gap"]); gap2.append(v["max-frame-gap-after-first-frame"]); peak.append(rss / 1048576)
stats("viewer-ready", vr, "ms"); stats("max-frame-gap (from send)", gap, "ms")
stats("max-frame-gap (ticks after first frame only)", gap2, "ms"); stats("50MB peak RSS", peak, "MiB")

for _ in range(5):
    rc, _, rss, _, out = run({"REQLITE_SPIKE_EXIT_ON": "editor-bench", "REQLITE_SPIKE_BODY_FILE": BODY})
    print("editor-bench rc", rc, out.strip(), f"peak RSS {rss/1048576:.1f} MiB")
