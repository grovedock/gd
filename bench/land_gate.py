#!/usr/bin/env python3
"""Performance gate for landing: the base build against this change's, on one machine.

    bench/land_gate.py --base BASE_BIN_DIR --head HEAD_BIN_DIR

Each side gets its own fresh grovedockd (SQLite) per round; N agents each commit and land K small,
non-conflicting changes on one branch at once (the agent workload). Rounds alternate which side
goes first, and the medians are compared. Fails when the change lands more than --tolerance
slower (throughput) or its p99 per land grows by more than --tolerance; a failure is measured
once more before it counts, because shared CI machines are noisy.
"""
import argparse
import os
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time

ENV = dict(os.environ, GIT_AUTHOR_NAME="gate", GIT_AUTHOR_EMAIL="gate@example.com",
           GIT_COMMITTER_NAME="gate", GIT_COMMITTER_EMAIL="gate@example.com", GD_NO_KEYCHAIN="1")


def sh(cmd, cwd=None, env=ENV):
    r = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError(f"{' '.join(cmd)}: {(r.stderr or r.stdout).strip()[-400:]}")
    return r.stdout


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def one_round(bindir, agents, each):
    bindir = os.path.abspath(bindir)
    gd, server = os.path.join(bindir, "gd"), os.path.join(bindir, "grovedockd")
    work = tempfile.mkdtemp(prefix="land-gate-")
    port = free_port()
    url = f"http://127.0.0.1:{port}"
    env = dict(ENV, GD_SERVER=url, HOME=work, XDG_CONFIG_HOME=work)
    proc = subprocess.Popen([server, "server", "--listen", f"127.0.0.1:{port}", "--metadata", f"sqlite://{work}/meta.db"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
    try:
        for _ in range(100):
            if subprocess.run([gd, "refs"], env=env, capture_output=True).returncode == 0:
                break
            time.sleep(0.1)
        seed = os.path.join(work, "seed")
        sh(["git", "init", "-q", "-b", "main", seed], env=env)
        with open(os.path.join(seed, "README"), "w") as f:
            f.write("gate\n")
        sh(["git", "add", "README"], cwd=seed, env=env)
        sh(["git", "commit", "-qm", "seed"], cwd=seed, env=env)
        sh([gd, "import-git", seed], env=env)
        dirs = []
        for i in range(agents):
            d = os.path.join(work, f"a{i}")
            sh([gd, "clone", url, d], env=env)
            dirs.append(d)
        lat, errors, lock = [], [], threading.Lock()

        def agent(i):
            for j in range(each):
                with open(os.path.join(dirs[i], f"a{i}-{j}.txt"), "w") as f:
                    f.write(f"{i} {j}\n")
                t = time.perf_counter()
                try:
                    sh([gd, "commit", "-m", f"a{i} {j}"], cwd=dirs[i], env=env)
                    sh([gd, "land"], cwd=dirs[i], env=env)
                except Exception as e:  # noqa: BLE001 - every failure counts, none is lost in a thread
                    with lock:
                        errors.append(str(e))
                    continue
                with lock:
                    lat.append(time.perf_counter() - t)

        threads = [threading.Thread(target=agent, args=(i,)) for i in range(agents)]
        start = time.perf_counter()
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        wall = time.perf_counter() - start
        if errors:
            raise RuntimeError(f"{len(errors)} lands failed, first: {errors[0]}")
        lat.sort()
        return len(lat) / wall, lat[min(len(lat) - 1, int(len(lat) * 0.99))]
    finally:
        proc.terminate()
        proc.wait(timeout=30)
        shutil.rmtree(work, ignore_errors=True)


def measure(a):
    runs = {"base": [], "head": []}
    for r in range(a.rounds):
        for side in (("base", "head") if r % 2 == 0 else ("head", "base")):
            runs[side].append(one_round(getattr(a, side), a.agents, a.each))
    return {s: (statistics.median(x[0] for x in v), statistics.median(x[1] for x in v)) for s, v in runs.items()}


def verdict(m, tol):
    (btp, bp99), (htp, hp99) = m["base"], m["head"]
    problems = []
    if htp < btp * (1 - tol):
        problems.append(f"throughput {htp:.1f}/s vs {btp:.1f}/s ({(htp / btp - 1) * 100:+.0f}%)")
    if hp99 > bp99 * (1 + tol):
        problems.append(f"p99 {hp99 * 1000:.0f} ms vs {bp99 * 1000:.0f} ms ({(hp99 / bp99 - 1) * 100:+.0f}%)")
    return problems


def table(m, a):
    (btp, bp99), (htp, hp99) = m["base"], m["head"]
    print(f"\n### Land gate: {a.agents} agents x {a.each} changes, median of {a.rounds} rounds\n")
    print("| | base | this change | change |\n|---|---:|---:|---:|")
    print(f"| lands a second | {btp:.1f} | {htp:.1f} | {(htp / btp - 1) * 100:+.0f}% |")
    print(f"| p99 per land | {bp99 * 1000:.0f} ms | {hp99 * 1000:.0f} ms | {(hp99 / bp99 - 1) * 100:+.0f}% |\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", required=True, help="directory with the base build's gd and grovedockd")
    ap.add_argument("--head", required=True, help="directory with this change's gd and grovedockd")
    ap.add_argument("--agents", type=int, default=16)
    ap.add_argument("--each", type=int, default=5)
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--tolerance", type=float, default=0.10)
    a = ap.parse_args()
    m = measure(a)
    table(m, a)
    if verdict(m, a.tolerance):
        print("Slower than the base; measuring again to rule out a noisy machine.\n")
        m = measure(a)
        table(m, a)
        problems = verdict(m, a.tolerance)
        if problems:
            print("FAIL: this change makes landing slower: " + "; ".join(problems))
            sys.exit(1)
    print("PASS")


if __name__ == "__main__":
    main()
