#!/usr/bin/env python3
"""Hosted against hosted, from the same machine over the internet: GitHub and a Grovedock server.

    bench/hosted_compare.py github --repo OWNER/gd-bench           (gh auth login first)
    bench/hosted_compare.py grovedock --server URL --repo ORG/gd-bench --big ORG/git
                                                                   (gd login URL first)

Three measurements, the same on both sides:
  land   N agents each land K small non-conflicting changes on main at once. git: commit, push,
         and on rejection fetch + rebase + push again (the standard agent loop). gd: commit, land.
  clone  a full clone of git/git (GitHub's own copy; on Grovedock an import of it).
  read   CI reading one file without cloning: GitHub's contents API (what CI tools call, with
         the token) and raw.githubusercontent.com (its CDN); `gd cat` on Grovedock.
Prints a markdown table. Credentials stay in gh's and gd's own stores; nothing here reads them.
"""
import argparse
import os
import shutil
import subprocess
import tempfile
import threading
import time

GD = os.environ.get("GD", "gd")


def run(cmd, cwd=None):
    t = time.perf_counter()
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    return r.returncode == 0, time.perf_counter() - t, (r.stderr or r.stdout).strip()[-200:]


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))] if xs else float("nan")


def threads(n, fn):
    ts = [threading.Thread(target=fn, args=(i,)) for i in range(n)]
    start = time.perf_counter()
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    return time.perf_counter() - start


def land_test(n, k, prepare, change):
    """prepare(i) -> workspace; change(ws, i, j) -> (ok, seconds, retries)."""
    dirs = [prepare(i) for i in range(n)]
    lat, retries, errs, lock = [], [0], [], threading.Lock()

    def agent(i):
        for j in range(k):
            ok, secs, tries = change(dirs[i], i, j)
            with lock:
                (lat.append(secs) if ok else errs.append(i))
                retries[0] += tries

    wall = threads(n, agent)
    return {"changes": len(lat), "per_s": len(lat) / wall, "p50": pct(lat, .5), "p99": pct(lat, .99),
            "retries": retries[0], "errors": len(errs)}


def write(ws, i, j):
    os.makedirs(os.path.join(ws, "bench"), exist_ok=True)
    with open(os.path.join(ws, "bench", f"a{i}-{j}-{time.time_ns()}.txt"), "w") as f:
        f.write(f"{i} {j}\n")


def github(a, work):
    url = f"https://github.com/{a.repo}.git"

    def prepare(i):
        d = os.path.join(work, f"g{i}")
        ok, _, why = run(["git", "clone", "-q", "--depth", "1", url, d])
        assert ok, why
        return d

    def change(ws, i, j):
        write(ws, i, j)
        t = time.perf_counter()
        run(["git", "add", "-A"], cwd=ws)
        run(["git", "commit", "-qm", f"agent {i} change {j}"], cwd=ws)
        tries = 0
        while True:
            ok, _, _ = run(["git", "push", "-q", "origin", "HEAD:main"], cwd=ws)
            if ok or tries >= 50:
                return ok, time.perf_counter() - t, tries
            tries += 1
            run(["git", "pull", "-q", "--rebase", "origin", "main"], cwd=ws)

    rows = {"land": land_test(a.agents, a.each, prepare, change)}
    clones = []
    for n in range(a.clones):
        d = os.path.join(work, f"clone{n}")
        ok, secs, why = run(["git", "clone", "-q", "https://github.com/git/git.git", d])
        assert ok, why
        clones.append(secs)
        shutil.rmtree(d)
    rows["clone"] = {"p50": pct(clones, .5)}
    api = [run(["gh", "api", "repos/git/git/contents/Makefile", "-H", "Accept: application/vnd.github.raw"])[1] for _ in range(a.reads)]
    raw = [run(["curl", "-fsS", "-o", "/dev/null", "https://raw.githubusercontent.com/git/git/master/Makefile"])[1] for _ in range(a.reads)]
    rows["read (API)"] = {"p50": pct(api, .5), "p99": pct(api, .99)}
    rows["read (CDN)"] = {"p50": pct(raw, .5), "p99": pct(raw, .99)}
    return rows


def grovedock(a, work):
    server = a.server.rstrip("/")

    def prepare(i):
        d = os.path.join(work, f"s{i}")
        ok, _, why = run([GD, "clone", f"{server}/{a.repo}", d, "--path", "bench"])
        assert ok, why
        return d

    def change(ws, i, j):
        write(ws, i, j)
        t = time.perf_counter()
        ok, _, _ = run([GD, "commit", "-m", f"agent {i} change {j}"], cwd=ws)
        if ok:
            ok, _, _ = run([GD, "land"], cwd=ws)
        return ok, time.perf_counter() - t, 0

    rows = {"land": land_test(a.agents, a.each, prepare, change)}
    clones = []
    for n in range(a.clones):
        d = os.path.join(work, f"clone{n}")
        ok, secs, why = run([GD, "clone", f"{server}/{a.big}", d])
        assert ok, why
        clones.append(secs)
        shutil.rmtree(d)
    rows["clone"] = {"p50": pct(clones, .5)}
    d = os.path.join(work, "reader")
    run([GD, "clone", f"{server}/{a.big}", d, "--path", "Documentation"])
    reads = [run([GD, "cat", "main", "Makefile"], cwd=d)[1] for _ in range(a.reads)]
    rows["read"] = {"p50": pct(reads, .5), "p99": pct(reads, .99)}
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("side", choices=["github", "grovedock"])
    ap.add_argument("--repo", required=True, help="small repository the agents land on (main must exist)")
    ap.add_argument("--server", help="Grovedock server URL")
    ap.add_argument("--big", help="Grovedock: the git/git import, ORG/NAME")
    ap.add_argument("--agents", type=int, default=16)
    ap.add_argument("--each", type=int, default=5)
    ap.add_argument("--clones", type=int, default=3)
    ap.add_argument("--reads", type=int, default=30)
    a = ap.parse_args()
    work = tempfile.mkdtemp(prefix="gd-compare-")
    try:
        rows = github(a, work) if a.side == "github" else grovedock(a, work)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    ms = lambda s: f"{s * 1000:.0f} ms" if s < 10 else f"{s:.1f} s"
    l = rows.pop("land")
    print(f"\n## {a.side}: {a.agents} agents x {a.each} changes on one branch\n")
    print(f"| landed | per second | p50 | p99 | client retries | failed |")
    print(f"|---:|---:|---:|---:|---:|---:|")
    print(f"| {l['changes']} | {l['per_s']:.2f} | {ms(l['p50'])} | {ms(l['p99'])} | {l['retries']} | {l['errors']} |\n")
    for name, r in rows.items():
        print(f"- {name}: p50 {ms(r['p50'])}" + (f", p99 {ms(r['p99'])}" if "p99" in r else ""))


if __name__ == "__main__":
    main()
