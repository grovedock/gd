# v1.0 release review

A written self-review before sign-off: every defect found while dogfooding and testing,
its root cause, the fix, and the test that now guards it. Known limitations are listed
separately so nobody discovers them in production.

## Defects found and fixed before release

| # | Found by | Symptom | Root cause | Fix | Guarded by |
|---|---|---|---|---|---|
| 1 | Importing git/git (82,443 commits) | Import aborted at commit ~1,000: "non-UTF-8 line" | 2005-era commits carry Latin-1 author names that `--reencode` does not touch | Identity lines decode lossily; **paths stay strict** (a non-UTF-8 path fails loudly instead of renaming a file) | `latin1_identity_is_tolerated_but_paths_are_strict` |
| 2 | Exporting git/git back to git and comparing tree hashes | 2 files had the wrong case (`zh_CN.po` vs `zh_cn.po`) | On case-insensitive filesystems `git init` sets `core.ignorecase=true`; `git fast-import` then folds case-only renames onto the old name | Run fast-import with `-c core.ignorecase=false` | case-rename step in `git_import_export_roundtrip_preserves_trees_exactly` (verified to fail without the fix) |
| 3 | Unit test while writing the fast-export parser | Commit message lost its trailing newline in the expectation | Test expectation wrong (`data 9` includes the newline); parser correct | Fixed the test | `parses_real_fast_export_shape` |
| 4 | Profiling the import server | 1.3 GB RSS | 64 MiB SQLite page cache on each of ~11 connections | 16 MiB per connection (mmap pages are shared) | n/a (resource tuning) |
| 5 | Code review for Windows | `file://` URLs built by string formatting break on `C:\` paths | Manual URL construction | `Url::from_directory_path` / `to_file_path` | contract + e2e tests on the Windows CI runner |
| 6 | Code review | `find` fetched trees one by one despite its doc comment | Implementation drift | Batched `get_many` per tree level | `clone_commit_land_sync_roundtrip` |
| 7 | Running the docker-compose topology | Server logged the Postgres URL **including its password**; `--help` could print env values | Raw URLs passed to the logger; clap shows env values by default | `config::redact` on every logged URL; `hide_env_values` on URL args | `redacts_credentials` |
| 8 | First CI run | S3 job could not start | MinIO no longer publishes community images | SeaweedFS (Apache-2.0) for CI and compose; adapter unchanged | CI `s3` job |
| 10 | Delta-storage test suite (after v1.0.0) | `gd commit` rejected a file replaced by a same-named directory | Client sent delete + upsert for overlapping paths, which the server rightly treats as ambiguous | Drop the redundant delete on the client (fixed in **v1.0.1**) | `file_and_directory_swaps_commit_cleanly` |
| 9 | Benchmarks | `log -- path` and `blame` slower than git | Every history step re-read and decoded the changeset from storage | Memory caches for immutable changesets and derived data (`CachedLog`, `CachedDerived`) | `cached_metadata_decorators`; log 6x faster than git after fix |

## Delta storage (unreleased, PR) review

| # | Found by | Symptom | Root cause | Fix | Guarded by |
|---|---|---|---|---|---|
| D1 | Design review before coding | Write-time delta encoding on several replicas could form A→B / B→A cycles (unreadable data) | No cross-replica ordering | Conversion only in a single-writer, lease-protected `grovedockd compact`; live servers only read | `compaction_preserves_every_version_and_shrinks_storage` (lease) |
| D2 | Unit test | "10x smaller" assertion passed for the wrong reason | Test data compressible on its own | Incompressible LCG data; assertion now proves the delta effect (>100x) | `encode_apply_roundtrip_and_compression` |
| D3 | e2e test | Deltas only 2x smaller | zstd prefix matching without long-distance matching | Measured 7 settings; level 3 + LDM: 1,405 → 41 bytes at the same speed | ADR-014 measurements |
| D4 | Compacting git/git (82k commits) | "delta chain longer than the maximum depth" | A base converted *after* something depended on it made depth headers stale (reverts/copies) | Pin bases before use; pinned objects are never converted; readers tolerate 5x the cap | `compaction_never_converts_a_base_that_others_depend_on` |
| D5 | Compacting git/git | Machine ran out of disk | SQLite VACUUM rewrites the whole file; test copies accumulated | Cleaned own artifacts only; disk watchdog during runs; documented VACUUM space need | docs (ARCHITECTURE: delta storage) |
| D6 | Storage breakdown after blob deltas | Trees were 90% of remaining storage | Each changeset writes a new root tree | Delta-encode trees along changed paths; chain depth 50 (git's `pack.depth`) | `trees_converted` assertions in e2e |

## v1.1.0 git bridge review

| # | Found by | Symptom | Root cause | Fix | Guarded by |
|---|---|---|---|---|---|
| B1 | e2e (concurrent push) | `git pull` after a server-rebased push failed: "Not updating refs/gd/... does not contain ..." | git records the pushed commit in the helper's private ref; the server's rebased tip is its sibling | Import stream declares `feature force` (private refs mirror the server; user branches untouched) | `git_bridge_clone_push_pull_and_concurrent_push` |
| B2 | Cloning git/git's history through the bridge | Two files had the wrong case | macOS `core.ignorecase=true` makes fast-import fold case-only renames | Commits containing a case-only rename are emitted as `deleteall` + full tree | case-rename step in the bridge e2e (fails without the fix) |

## Verified properties

* Full git/git history (82,443 commits) imports, and its tip exports back to git with a
  **bit-identical tree hash** (submodule pointers excluded: unsupported, reported on import).
* Re-import is idempotent (0 blobs re-uploaded) and incremental (fast-forwards the ref).
* 8 concurrent lands on one ref produce a linear history with no lost changes (CAS + rebase).
* CAS linearizability under 16 concurrent writers on every metadata adapter.
* Property tests: `apply_changes(a, diff(a, b)) == b` for random trees including file↔dir
  replacement; decoders never panic on arbitrary bytes; accepted paths never escape the root.
* No write follows a symlink, even when a later change turns a symlink into a directory.

## Known limitations in v1.0 (documented, not bugs)

* **Server storage** is several times git's (no delta compression): ~4.5 GB for git/git
  vs a 333 MB pack. Clients are unaffected. Roadmap: delta/dictionary storage.
* **Read permissions** are repository-wide; write and ref permissions are per path/ref.
* **Submodules** are not supported.
* **History** commands show first-parent (landed) history.
* `log -- path` and `blame` walk first-parent history server-side (bounded); very old,
  rarely-touched paths are slower than git with commit-graph Bloom filters.
* Objects are capped at 256 MiB.
