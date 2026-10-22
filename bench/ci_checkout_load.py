#!/usr/bin/env python3
"""CI checkout throughput: one CI machine running N jobs at once. Each job starts from an
empty directory, checks out one top-level directory of the repository (sparse, the usual CI
case), counts the files it got and deletes them. Fixed window; jobs that finish after it
are not counted.

Systems (one per run):
  git          shallow, blobless, sparse clone from `git daemon` (standard CI checkout)
  git-mirror   --shared clone from a mirror on the same machine (git's best case: no network)
  gd        `gd clone --path` straight from the server
  gd-node   the same, objects through the machine's `grovedockd daemon`

    python3 bench/ci_checkout_load.py --system gd --server http://127.0.0.1:7432 \\
        --paths Documentation,builtin,t --jobs 64 --secs 30 --work /tmp/ci
"""
import argparse
import os
import shutil
import subprocess
import threading
import time


def run(cmd, cwd=None):
    return subprocess.run(cmd, cwd=cwd, check=True, capture_output=True, text=True)


def count_files(root, path):
    n = 0
    for _, _, files in os.walk(os.path.join(root, path)):
        n += len(files)
    return n


def checkout(a, d, path):
    if a.system == "git":
        run(["git", "clone", "-q", "--depth", "1", "--filter=blob:none", "--sparse", "-b", a.branch, a.git_url, d])
        run(["git", "-C", d, "sparse-checkout", "set", path])
    elif a.system == "git-mirror":
        run(["git", "clone", "-q", "--shared", "--no-checkout", "-b", a.branch, a.mirror, d])
        run(["git", "-C", d, "sparse-checkout", "set", path])
        run(["git", "-C", d, "checkout", "-q", a.branch])
    else:
        cmd = ["gd", "clone", a.server, d, "--ref", a.branch, "--path", path]
        if a.system == "gd-node":
            cmd += ["--objects", a.node]
        run(cmd)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--system", required=True, choices=["git", "git-mirror", "gd", "gd-node"])
    ap.add_argument("--paths", required=True, help="comma-separated directories; job i takes path i mod n")
    ap.add_argument("--jobs", type=int, default=64, help="concurrent jobs")
    ap.add_argument("--secs", type=int, default=30)
    ap.add_argument("--work", required=True)
    ap.add_argument("--branch", default="main")
    ap.add_argument("--server", help="gd server URL")
    ap.add_argument("--node", help="grovedockd daemon URL (gd-node)")
    ap.add_argument("--git-url", help="git daemon URL (git)")
    ap.add_argument("--mirror", help="local bare mirror (git-mirror)")
    ap.add_argument("--expect", help="path=count,... file counts every checkout must match")
    a = ap.parse_args()
    paths = a.paths.split(",")
    expect = dict((k, int(v)) for k, v in (kv.split("=") for kv in a.expect.split(","))) if a.expect else {}
    shutil.rmtree(a.work, ignore_errors=True)
    os.makedirs(a.work)

    lock = threading.Lock()
    done, files, errors, latencies, counts = [0], [0], [], [], {}
    deadline = time.perf_counter() + a.secs

    def worker(w):
        k = 0
        while time.perf_counter() < deadline:
            path = paths[(w + k) % len(paths)]
            d = os.path.join(a.work, f"j{w}-{k}")
            t = time.perf_counter()
            try:
                checkout(a, d, path)
                n = count_files(d, path)
                if expect and n != expect[path]:
                    raise RuntimeError(f"{path}: {n} files, expected {expect[path]}")
                if time.perf_counter() <= deadline:
                    with lock:
                        done[0] += 1
                        files[0] += n
                        latencies.append(time.perf_counter() - t)
                        counts[path] = n
            except Exception as e:  # report, keep going
                with lock:
                    errors.append(f"{path}: {getattr(e, 'stderr', '') or e}".strip()[:300])
            shutil.rmtree(d, ignore_errors=True)
            k += 1

    threads = [threading.Thread(target=worker, args=(w,)) for w in range(a.jobs)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    latencies.sort()

    def pct(p):
        return latencies[min(int(len(latencies) * p), len(latencies) - 1)] if latencies else 0

    print(f"{a.system}  jobs {a.jobs}  window {a.secs} s")
    print(f"checkouts {done[0]}  ({done[0] / a.secs:.1f}/s, {files[0] / a.secs:,.0f} files/s)  errors {len(errors)}")
    print(f"latency s: p50 {pct(0.5):.2f}  p90 {pct(0.9):.2f}  p99 {pct(0.99):.2f}  max {pct(1.0):.2f}")
    print("files per path: " + ",".join(f"{k}={v}" for k, v in sorted(counts.items())))
    if errors:
        print(f"first error: {errors[0]}")


if __name__ == "__main__":
    main()
