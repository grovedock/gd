#!/usr/bin/env python3
"""git baseline for examples/land_load.rs: N agents, each with its own clone, repeatedly
commit a new file in their own directory and push to one branch, fetching and rebasing on
rejection (the standard agent loop). Fixed measurement window; pushes that finish after it
are not counted. The remote is a local bare repository, git's best case (no network).

    python3 bench/git_push_load.py --agents 1000 --secs 60 --work /tmp/gitload
"""
import argparse
import os
import shutil
import subprocess
import threading
import time


def run(cmd, cwd=None, check=True):
    return subprocess.run(cmd, cwd=cwd, check=check, capture_output=True, text=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--agents", type=int, default=1000)
    ap.add_argument("--secs", type=int, default=60)
    ap.add_argument("--work", required=True)
    a = ap.parse_args()
    shutil.rmtree(a.work, ignore_errors=True)
    os.makedirs(a.work)
    remote = os.path.join(a.work, "remote.git")
    run(["git", "init", "-q", "--bare", "-b", "main", remote])
    seed = os.path.join(a.work, "seed")
    run(["git", "clone", "-q", remote, seed])
    with open(os.path.join(seed, "seed.txt"), "w") as f:
        f.write("seed\n")
    run(["git", "add", "."], cwd=seed)
    run(["git", "-c", "user.name=a", "-c", "user.email=a@x", "commit", "-qm", "seed"], cwd=seed)
    run(["git", "push", "-q", "origin", "main"], cwd=seed)

    clones = []
    for i in range(a.agents):
        c = os.path.join(a.work, f"a{i}")
        run(["git", "clone", "-q", remote, c])
        run(["git", "config", "user.name", f"agent-{i}"], cwd=c)
        run(["git", "config", "user.email", f"agent-{i}@x"], cwd=c)
        clones.append(c)

    lock = threading.Lock()
    stats = {"landed": 0, "retries": 0, "errors": 0}
    latencies = []
    start = time.perf_counter()
    deadline = start + a.secs

    def agent(i, path):
        j = 0
        while time.perf_counter() < deadline:
            t = time.perf_counter()
            d = os.path.join(path, "bench-load", f"a{i:05}")
            os.makedirs(d, exist_ok=True)
            name = os.path.join("bench-load", f"a{i:05}", f"c{j}.txt")
            with open(os.path.join(path, name), "w") as f:
                f.write(f"agent {i} change {j}\n")
            run(["git", "add", name], cwd=path)
            run(["git", "commit", "-qm", f"agent {i} change {j}"], cwd=path)
            retries = 0
            while time.perf_counter() < deadline:
                if run(["git", "push", "-q", "origin", "HEAD:main"], cwd=path, check=False).returncode == 0:
                    with lock:
                        stats["landed"] += 1
                        stats["retries"] += retries
                        latencies.append(time.perf_counter() - t)
                    break
                retries += 1
                if run(["git", "pull", "-q", "--rebase", "origin", "main"], cwd=path, check=False).returncode != 0:
                    with lock:
                        stats["errors"] += 1
                    run(["git", "rebase", "--abort"], cwd=path, check=False)
            else:
                with lock:
                    stats["retries"] += retries
            j += 1

    threads = [threading.Thread(target=agent, args=(i, c)) for i, c in enumerate(clones)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    latencies.sort()

    def pct(p):
        return latencies[min(int(len(latencies) * p), len(latencies) - 1)] * 1000 if latencies else 0

    print(f"agents {a.agents}  window {a.secs} s  (git push / fetch+rebase retry, local bare remote)")
    print(f"landed {stats['landed']}  ({stats['landed'] / a.secs:.1f} lands/s)")
    print(f"client retries {stats['retries']}  rebase errors {stats['errors']}")
    print(f"commit+push latency ms: p50 {pct(0.5):.1f}  p90 {pct(0.9):.1f}  p99 {pct(0.99):.1f}  max {pct(1.0):.1f}")


if __name__ == "__main__":
    main()
