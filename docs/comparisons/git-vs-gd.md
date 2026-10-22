# Benchmarks: gd and git

Both tools are excellent at different things. This page says where each one wins, with
measurements you can reproduce (`bench/run.sh`). The table below was recorded with gd 1.0.0
and git 2.50.1 (`bench/results/RESULTS.md`).

## Measured results

<!-- BENCH:START -->
| Scenario | git | Grovedock | Grovedock vs git |
|---|---:|---:|---|
| full-clone | 19,130 ms | 463 ms | 41.3x faster |
| sparse-clone (Documentation/) | 9,513 ms | 106 ms | 89.7x faster |
| read one file, no prior clone (CI) | 149 ms | 9 ms | 16.0x faster |
| status (clean, 4857 files) | 91 ms | 48 ms | 1.9x faster |
| log -n 50 -- builtin/add.c | 100 ms | 19 ms | 5.4x faster |
| blame Makefile | 528 ms | 352 ms | 1.5x faster |
| commit + publish 1 file | 130 ms | 77 ms | 1.7x faster |

`status` measured at parity (36 ms vs 36 ms) in an earlier run on the same machine; treat it
as parity to ~2x faster. Other rows were stable across runs.

### Footprint after full clone

| | git | Grovedock |
|---|---:|---:|
| Client metadata | 334M (`.git`, full history) | 380K (`.gd`, index only) |
| Server storage | 333M (delta-compressed pack) | 4.6G per-object zstd; 369M after `grovedockd compact` |

Full-history import: 226 s for 82,443 commits (first run). Re-import of unchanged history
uploads nothing.

### Concurrent publishing: 16 agents x 5 changes = 80 merges to one branch

| | git (push / fetch+rebase retry) | Grovedock (land) |
|---|---:|---:|
| Wall time | 42.8 s | 3.5 s |
| Throughput | 1.9 merges/s | 22.7 merges/s |
| Client retries | 456 | 0 (server rebased 79) |
<!-- BENCH:END -->

## Landing at scale: 16 to 5,000 concurrent agents

Every agent holds its own connection and loops: read the branch head, upload one new file
in its own directory, create a changeset on that head, land it. All changes are
non-conflicting, so every lost race is resolved by the server (Grovedock) or by the client's
fetch-rebase-push loop (git). Lands that finish after the 30 s window are not counted.

| Agents | Layout | git lands/s | git p99 | Grovedock lands/s | Grovedock p50 | Grovedock p99 | Grovedock errors |
|---:|---|---:|---:|---:|---:|---:|---:|
| 16 | one directory | 2.0 | 22.7 s | 758 | 18 ms | 55 ms | 0 |
| 200 | one directory | 0.5 | 25.2 s | 1,012 | 203 ms | 302 ms | 0 |
| 1,000 | one directory | 0.2 (5 lands) | 52.0 s | 504 | 2.10 s | 2.66 s | 0 |
| 1,000 | 64 directories | | | 907 | 1.17 s | 1.30 s | 0 |
| 2,000 | one directory | not run | | 333 | 6.27 s | 6.80 s | 0 |
| 5,000 | one directory | not run | | 172 | 3.36 s | 15.8 s | 0 |
| 5,000 | 64 directories | | | 531 | 9.26 s | 10.3 s | 0 |

"One directory" puts every agent's directory side by side in one parent, which every land
rewrites (5,000 entries at the largest; the Linux kernel's largest directory,
`include/linux`, has about 1,600). "64 directories" spreads them over 64 parents, closer to agents
working across a monorepo.

* Setup: Apple M4 (10 cores), macOS 26.6, release build, one `grovedockd` on SQLite, a fresh
  store per run. git pushes to a local bare repository over `file://`, its best case: no
  network and no server-side hooks. Agents and server share the machine. Measured with the
  machine otherwise idle (an earlier run with a busy background VM gave about 30% less).
* Grovedock: `cargo run --release --example land_load -- --server http://127.0.0.1:7420 --agents N --fanout F --secs 30`
  against `grovedockd server`. git: `python3 bench/git_push_load.py --agents N --secs 30 --work DIR`.
* Before batching (ADR-017), the same Grovedock build managed 105 lands/s at 16 agents and
  10 lands/s with 237 "too contended" errors at 200.
* Limits: past 1,000 agents throughput falls because the same process also serves every
  agent's uploads and changeset creation, and each agent's directory adds an entry to one
  shared directory that every land rewrites. At 5,000 agents in one directory almost every
  agent landed once in the window, so that row mostly measures the first round.
* Context: [Anthropic reported](https://www.anthropic.com/engineering/redesigning-source-control) over 30,000 merges per day by September 2026 (about
  0.35/s), [Google](https://cacm.acm.org/research/why-google-stores-billions-of-lines-of-code-in-a-single-repository/) about 40,000 commits per workday in 2015. Landing throughput of one server is
  not the constraint at those volumes; real lands also carry CI, review and larger changes,
  which this benchmark leaves out. A multi-replica deployment
  on Postgres has not been measured yet.
* git at 2,000 and 5,000 agents was not run: at 1,000 it landed 5 changes in 30 s.


### Methodology

* Corpus: [git/git](https://github.com/git/git) (82,443 commits, ~4,900 files): real,
  public, and with 20 years of history.
* git is served over `file://` (the real pack protocol, no network latency) with
  `uploadpack.allowFilter` enabled so partial clone is available to git.
* Grovedock runs as a release-build server on SQLite, over loopback gRPC.
* For each scenario git gets its best practice: partial clone + sparse checkout for sparse
  clones, `--depth 1 --filter=blob:none` for single-file CI reads, `--first-parent` for
  log/blame (Grovedock's history is first-parent, i.e. what landed).
* [hyperfine](https://github.com/sharkdp/hyperfine) median of 5 runs after a warm-up run.

## Where Grovedock wins, and why

* **Clones**: Grovedock transfers one tree's worth of objects, never history. The gap
  grows with history length; at monorepo scale git's full clone is the dominant CI cost.
* **CI reads**: `gd cat main path` is one RPC. git needs at least a shallow, filtered clone.
* **Publishing under contention**: git's push loses every race and must fetch + rebase +
  retry on the client. Grovedock rebases on the server and moves the ref by compare-and-swap,
  so concurrent non-conflicting lands all succeed without client retries.
* **Thin clients**: a Grovedock workspace's metadata is an index, not a copy of history.

## Where git wins

* **Offline work**: git has everything locally. Grovedock needs the server for history,
  commits and landing (by design).
* **Server storage (before compaction)**: uncompacted, Grovedock stores every object version
  whole (4.6 GB for git/git vs a 333 MB pack). After `grovedockd compact` (delta storage) the
  same store is **369 MB, 1.1x git's pack**.
* **Cold path-filtered history**: Grovedock's `log -- path` and `blame` are fast once the
  server's immutable metadata cache is warm (measured above); on a cold server they walk
  first-parent history from storage. A derived per-path index is on the roadmap.
* **Ecosystem**: every tool speaks git. Grovedock offers lossless `import-git` / `export-git`
  so you are never locked in.

## Semantics differences

| | git | Grovedock |
|---|---|---|
| Commit | local snapshot | server-side changeset (list of changed files vs first parent) |
| Staging area | yes | no; `gd commit [PATHS]` commits all or a subset |
| Branches | cheap, everywhere | supported (`gd checkout -b`), but `main` is the default lane; refs are global state |
| History shown by log/blame | full DAG | first-parent (what landed) |
| Submodules | yes | not supported (skipped on import) |
| Hash | SHA-1 (SHA-256 opt-in) | BLAKE3 |

## CI checkouts on one machine

Each CI job starts from an empty directory, checks out one top-level directory of git/git
(sparse, the usual CI case), checks the file count and deletes it. 16 jobs run at once on
one machine for 30 s; jobs rotate over `Documentation`, `builtin`, `t`, `contrib`, `compat`
and `po` (28 to 2,554 files). Every system checked out identical file counts.

| System | Checkouts/s | p50 | p99 |
|---|---:|---:|---:|
| git: shallow, blobless, sparse clone from `git daemon` (standard CI) | 2.7 | 3.71 s | 10.3 s |
| git: `--shared` clone from a mirror on the same machine (no network) | 4.2 | 1.87 s | 5.6 s |
| Grovedock: `gd clone --path` from the server | 5.8 | 0.35 s | 5.4 s |
| Grovedock: through the machine's node daemon | 6.1 | 0.43 s | 4.6 s |

* One checkout on an idle machine: 0.15-0.23 s for Grovedock against about 1.7 s for git, the
  largest directory included. With 16 at once, both systems are limited by writing files
  on the client machine, so the gap narrows to about 2x. Grovedock's checkout of `t/` (2,554
  files, fetched, decoded and verified) takes as long as a plain loop that only writes the
  same files (0.20 s), so the client is at the filesystem's limit; going further means not
  writing files at all, which `gd mount` does.
* Server cost: 1.57 CPU-seconds for 175 checkouts, about 9 ms each.
* The node daemon adds little here because server and jobs share one machine; it exists to
  take load off the server and the network between them.
* At 64 and 256 jobs the laptop is saturated for every system (at 256, `git daemon`
  connections time out), so those runs measure the machine rather than the tools.
* This benchmark found a bug: about 3% of Grovedock checkouts failed with `File exists` when
  two writers created the same directory at once. Fixed in 2.1.1.
* Reproduce: `bench/ci_checkout_load.py --system {git,git-mirror,gd,gd-node} --jobs 16
  --paths Documentation,builtin,t,contrib,compat,po --work DIR` plus `--server`, `--node`,
  `--git-url` or `--mirror` for the system under test.

