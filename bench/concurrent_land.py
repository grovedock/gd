#!/usr/bin/env python3
"""Concurrent publish throughput: N agents each publish K small, non-conflicting changes
to the same branch at the same time.

git:     commit, push; on rejection fetch + rebase and retry (the standard agent loop).
Grovedock: commit, land (the server rebases; no client retry loop).

Prints a markdown section with wall time, merges/second and client-side retries.
"""
import argparse
import os
import shutil
import subprocess
import threading
import time


def run(cmd, cwd=None, check=True):
    return subprocess.run(cmd, cwd=cwd, check=check, capture_output=True, text=True)


def git_worker(path, wid, k, stats, lock):
    for j in range(k):
        name = f"bench-concurrency/w{wid}-{j}.txt"
        os.makedirs(os.path.join(path, "bench-concurrency"), exist_ok=True)
        with open(os.path.join(path, name), "w") as f:
            f.write(f"{wid} {j}\n")
        run(["git", "add", name], cwd=path)
        run(["git", "commit", "-qm", f"w{wid} {j}"], cwd=path)
        while True:
            if run(["git", "push", "-q", "origin", "HEAD:master"], cwd=path, check=False).returncode == 0:
                break
            with lock:
                stats["retries"] += 1
            run(["git", "pull", "-q", "--rebase", "origin", "master"], cwd=path)


def sap_worker(path, wid, k, stats, lock):
    for j in range(k):
        name = f"bench-concurrency/w{wid}-{j}.txt"
        os.makedirs(os.path.join(path, "bench-concurrency"), exist_ok=True)
        with open(os.path.join(path, name), "w") as f:
            f.write(f"{wid} {j}\n")
        run(["gd", "commit", "-m", f"w{wid} {j}"], cwd=path)
        out = run(["gd", "land"], cwd=path).stdout
        if "rebased" in out:
            with lock:
                stats["server_rebases"] += 1


def timed(worker, paths, k):
    stats = {"retries": 0, "server_rebases": 0}
    lock = threading.Lock()
    threads = [threading.Thread(target=worker, args=(p, i, k, stats, lock)) for i, p in enumerate(paths)]
    start = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return time.perf_counter() - start, stats


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--git-url", required=True)
    ap.add_argument("--gd-url", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--agents", type=int, default=16)
    ap.add_argument("--each", type=int, default=5)
    a = ap.parse_args()
    base = os.path.join(a.work, "concurrency")
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)

    run(["git", "clone", "-q", "--bare", a.git_url, os.path.join(base, "remote.git")])
    remote = "file://" + os.path.join(base, "remote.git")
    git_paths, sap_paths = [], []
    for i in range(a.agents):
        g = os.path.join(base, f"g{i}")
        run(["git", "clone", "-q", "--depth", "1", remote, g])
        git_paths.append(g)
        s = os.path.join(base, f"s{i}")
        run(["gd", "clone", a.gd_url, s, "--path", "bench-concurrency"])
        sap_paths.append(s)

    merges = a.agents * a.each
    g_time, g_stats = timed(git_worker, git_paths, a.each)
    s_time, s_stats = timed(sap_worker, sap_paths, a.each)
    print()
    print(f"## Concurrent publishing: {a.agents} agents x {a.each} changes = {merges} merges to one branch")
    print()
    print("| | git (push / fetch+rebase retry) | Grovedock (land) |")
    print("|---|---:|---:|")
    print(f"| Wall time | {g_time:.1f} s | {s_time:.1f} s |")
    print(f"| Throughput | {merges / g_time:.1f} merges/s | {merges / s_time:.1f} merges/s |")
    print(f"| Client retries | {g_stats['retries']} | 0 (server rebased {s_stats['server_rebases']}) |")


if __name__ == "__main__":
    main()
