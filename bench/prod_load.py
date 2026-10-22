#!/usr/bin/env python3
"""Load test a running Grovedock server as one signed-in person, with the gd CLI.

Sign in first (the token stays in gd's own config; nothing here reads or prints it):
    gd login https://cloud.grovedock.com
Then:
    bench/prod_load.py --server https://cloud.grovedock.com --owner kabir

Makes a private repository OWNER/loadtest-STAMP, then measures, step by step:
  clone   N workspaces cloned at once (sparse, one directory)
  land    N agents landing K non-conflicting changes each on main, all at once
  read    N readers running `gd cat` on main for SECS seconds
Prints a markdown table of throughput, p50/p99 and errors per step. Stops a step early when
more than 5% of calls fail, so a struggling server isn't pushed further.
"""
import argparse
import os
import shutil
import subprocess
import tempfile
import threading
import time

GD = os.environ.get("GD", "gd")


def gd(*args, cwd=None):
    t = time.perf_counter()
    r = subprocess.run([GD, *args], cwd=cwd, capture_output=True, text=True)
    return r.returncode == 0, time.perf_counter() - t, (r.stderr or r.stdout).strip()[-300:]


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))] if xs else 0.0


def parallel(n, job):
    """Runs job(i) on n threads; returns (wall seconds, [latencies], [errors])."""
    lat, errs, lock = [], [], threading.Lock()

    def one(i):
        for ok, secs, why in job(i):
            with lock:
                (lat if ok else errs).append(secs if ok else why)

    threads = [threading.Thread(target=one, args=(i,)) for i in range(n)]
    start = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return time.perf_counter() - start, lat, errs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--owner", required=True, help="your login (the repository is made under it)")
    ap.add_argument("--agents", type=int, default=32)
    ap.add_argument("--each", type=int, default=4)
    ap.add_argument("--readers", type=int, default=32)
    ap.add_argument("--secs", type=int, default=20)
    a = ap.parse_args()
    server = a.server.rstrip("/")
    repo = f"{a.owner}/loadtest-{time.strftime('%Y%m%d%H%M%S')}"
    url = f"{server}/{repo}"
    work = tempfile.mkdtemp(prefix="gd-load-")
    rows = []
    try:
        ok, _, why = gd("repo", "create", repo, "--server", server, "--description", "load test, safe to delete")
        if not ok:
            raise SystemExit(f"couldn't create {repo}: {why}\n(signed in? run: gd login {server})")
        seed = os.path.join(work, "seed")
        ok, _, why = gd("clone", url, seed)
        assert ok, why
        os.makedirs(os.path.join(seed, "load"))
        with open(os.path.join(seed, "load", "README.md"), "w") as f:
            f.write("load test\n")
        for step in (("commit", "-m", "Seed"), ("land",)):
            ok, _, why = gd(*step, cwd=seed)
            assert ok, why

        dirs = [os.path.join(work, f"w{i}") for i in range(a.agents)]
        wall, lat, errs = parallel(a.agents, lambda i: [gd("clone", url, dirs[i], "--path", "load")])
        rows.append(("clone", a.agents, wall, lat, errs))

        def lander(i):
            out = []
            for j in range(a.each):
                with open(os.path.join(dirs[i], "load", f"a{i}-{j}.txt"), "w") as f:
                    f.write(f"{i} {j}\n")
                ok, t1, why = gd("commit", "-m", f"agent {i} change {j}", cwd=dirs[i])
                if ok:
                    ok, t2, why = gd("land", cwd=dirs[i])
                    t1 += t2
                out.append((ok, t1, why))
                if not ok:
                    break
            return out

        wall, lat, errs = parallel(a.agents, lander)
        rows.append(("land", a.agents * a.each, wall, lat, errs))

        stop = time.time() + a.secs

        def reader(i):
            out = []
            while time.time() < stop:
                out.append(gd("cat", "main", "load/README.md", cwd=dirs[i % len(dirs)]))
                if sum(1 for o in out if not o[0] and "too many calls" not in o[2]) > max(3, len(out) // 20):
                    break
            return out

        wall, lat, errs = parallel(a.readers, reader)
        rows.append(("read", len(lat) + len(errs), wall, lat, errs))
    finally:
        shutil.rmtree(work, ignore_errors=True)

    print(f"\n## {server}: {repo}\n")
    # The plan's call limit refusing calls is the server working as designed, not failing.
    limited = lambda e: "too many calls" in e
    print("| step | calls | per second | p50 | p99 | plan-limited | errors |")
    print("|---|---:|---:|---:|---:|---:|---:|")
    for name, calls, wall, lat, errs in rows:
        lim = sum(map(limited, errs))
        print(f"| {name} | {calls} | {len(lat) / wall:.1f} | {pct(lat, .5) * 1000:.0f} ms | {pct(lat, .99) * 1000:.0f} ms | {lim} | {len(errs) - lim} |")
    for name, _, _, _, errs in rows:
        for e in sorted({e for e in errs if not limited(e)})[:3]:
            print(f"- {name} error: {e}")
    print(f"\nThe repository {repo} is left in place; it is private and can be deleted from its settings.")


if __name__ == "__main__":
    main()
