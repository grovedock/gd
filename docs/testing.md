# How Grovedock is tested

Source control is trusted for not losing work. These are the checks that back that up,
from the smallest to the most realistic. All of them run in CI.

## Contract tests for every storage adapter

Memory, SQLite, Postgres and CockroachDB, and the local-disk, in-memory and S3 object stores
(GCS and Azure use the same `object_store` code but are not run in CI), pass the same suite
(`crates/adapters/tests/contract.rs`), including a 16-writer compare-and-swap test.
That is what makes adapters interchangeable rather than merely type-compatible.

## Deterministic simulation of landing

`crates/server/tests/simulation.rs` runs three server replicas over one shared
store that fails operations, slows them down, and applies writes but loses the reply.
Replicas crash mid-batch and restart. Clients land changes on random replicas, give up
waiting and retry elsewhere, and a third of the changes conflict. One seed drives a
single-threaded runtime with a paused clock, so any failure replays exactly:

```sh
GD_SIM_SEED=1234 cargo test -p grovedock-server --test simulation
```

After every run four invariants must hold:

1. no change is in the branch's history twice;
2. every land the server acknowledged is in the branch's history;
3. the branch's tree equals replaying its history;
4. no change landed over a conflicting change it never saw.

Every CI build runs 200 seeds; a nightly job explores 50,000 new ones. Deliberately
breaking the conflict check, or acknowledging a land whose ref update failed, is caught
at the first seed.

## Chaos against real binaries

`bench/chaos.sh` starts three `grovedockd` replicas on one Postgres and puts a proxy in front
of them that cuts connections while replies are in flight. Clients land through the proxy
and retry failures on other replicas, while the script kills replicas with `SIGKILL`,
restarts them, and pauses the database. At the end the same four invariants are checked
through the public API. A nightly job runs it for five minutes; a typical two-minute run
kills replicas about 30 times and lands about 9,000 changes.

## Fuzzing the decoders

Everything Grovedock reads from the network or disk goes through a handful of decoders:
objects (zstd frames, chunk manifests, deltas), trees, changesets, and path validation.
Each has a [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) target in `fuzz/`,
seeded with valid inputs from `fuzz/examples/seed_corpus.rs` so the fuzzer starts past
checksums and headers. Any input must produce a value or an error, never a panic; the path
target also checks that every accepted path stays inside the working copy. Each target
runs for ten minutes nightly and one minute on pull requests that touch the decoders.

```sh
cargo +nightly fuzz run tree fuzz/seeds/tree
```

## What these found

| Bug | Found by | Fixed in |
|---|---|---|
| Parallel checkout writers creating the same directory failed with `File exists` | CI checkout benchmark | 2.1.1 |
| One transient storage error failed every change in a land batch | simulation | 2.1.2 |
| A flush that failed halfway left a record pointing at data never written | simulation | 2.1.2 |
| A land retried after a lost reply applied the change twice | simulation | 2.1.2 |
| Two replicas landing the same retried change could still apply it twice | chaos test | 2.1.3 |

## What is not covered yet

The simulation covers the landing protocol over in-memory stores, not the whole server.
The gRPC message decoders are generated code (prost) and are not fuzzed separately. There
has been no independent (Jepsen) analysis.
