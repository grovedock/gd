# Architecture

Grovedock is a client/server source control system designed around one observation: when
humans and AI agents merge tens of thousands of changes a day into one monorepo, the
expensive operations of git (full clones, client-side history, pushing whole packfiles,
merge queues behind a global lock) become the bottleneck. Grovedock moves history, commits,
merges and rebases to the server, keeps clients thin, and makes every read an API call.

## Topology

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/charts/architecture-dark.svg">
  <img src="assets/charts/architecture-light.svg" width="840" alt="Clients fetch file contents through a node daemon and a regional cache from the grovedockd server; commits, lands and history queries go straight to the server, which stores metadata in SQLite, Postgres or CockroachDB and objects in S3, GCS, Azure or disk.">
</picture>

* **Server**: every feature. Stateless apart from its storage adapters, so several replicas
  can share one Postgres/CockroachDB + object store. A server that hosts many repositories
  (`--platform`) keeps its registry in one process and runs as a single node.
* **Cache** and **daemon**: serve only the `Objects` service. Objects are immutable and
  content-addressed, so a cached object is correct forever: no invalidation, no coherence
  protocol. Tiers speak the identical RPC, so a client cannot tell which tier answered.
* **Client**: holds no history. A workspace is the materialised tree of one changeset plus
  an index of file stats.

## Data model

| Concept | Representation |
|---|---|
| Object id | BLAKE3-256 of `kind byte ‖ content` (domain-separated) |
| Blob | file bytes; stored as `kind ‖ zstd(content)` |
| Tree | sorted `(name, mode, id)` entries (prost-encoded, canonical) |
| Changeset | `parents`, `changes` (path → blob or delete) **relative to first parent**, author, message, timestamp. Id = BLAKE3 of the canonical encoding |
| Ref | name → changeset id; moved only by compare-and-swap |
| Derived data | any value defined recursively from parents: root tree, generation number + skew-binary jump pointer |

A changeset stores *what changed*, not a tree. The root tree is derived data, computed
once by applying the changes to the parent's tree. Because trees are Merkle trees, applying
`k` changes touches `O(k · depth)` tree objects regardless of repository size, and diffing
two trees only descends into subtrees whose ids differ.

## Feature slices (vertical slicing)

Each feature owns its use case, its transport adapter and its feature flag, and depends
only on the domain kernel (`grovedock_core::Repo`), never on another slice.

| Slice (server) | RPCs | Flag |
|---|---|---|
| `objects` | Get / Put / Missing | `objects` |
| `changesets` | Create / Get | `changesets` |
| `refs` | Get / CompareAndSwap / List | `refs` |
| `land` | Land (server-side rebase + CAS) | `land` |
| `history` | Log, Blame | `history`, `blame` |
| `diff` | Diff | `diff` |
| `merge_base` | MergeBase | `merge-base` |
| `browse` | ListDir, ReadFile, Find | `browse`, `search` |
| (cross-cutting) | per-directory write ACLs, per-ref ACLs | `permissions` |

Client slices: `clone`, `commit`, `land`, `sync`/`checkout`, `diff`, `browse`, `git`
(import/export, flags `git-import` / `git-export`), `mcp`; sparse checkout is flagged by
`sparse-checkout`.

Flags are read from `GD_FLAGS`, e.g. `GD_FLAGS="land=off,git-import=on"`. All
flags default to on. The flag source is a port (`FeatureFlags`), so a flag service can
replace the env adapter.

## Ports and adapters (clean architecture)

```
grovedock-core      domain + ports (no I/O, no transport)       <- depends on nothing
grovedock-adapters  memory, SQLite, Postgres, object_store, caches, gRPC upstream, flags, ACL rules
grovedock-proto     wire protocol (gRPC)
grovedock-server    slices + composition root (config.rs) + binary
gd               client slices + Remote port + CLI + MCP
```

| Port | Purpose | Adapters |
|---|---|---|
| `ObjectStore` | content-addressed bytes | memory, SQLite, Postgres, S3/GCS/Azure/local (object_store), `BoundedCache`, `Tiered`, `GrpcObjects` |
| `ChangesetLog` | append-only changeset log | memory, SQLite, Postgres |
| `RefStore` | CAS refs | memory, SQLite, Postgres |
| `DerivedStore` | derived data cache | memory, SQLite, Postgres |
| `FeatureFlags` | flags | `SpecFlags` (env) |
| `AccessPolicy` | write/ref permissions | `RulesPolicy` (JSON), `AllowAllPolicy` |
| `Authenticator` | identity | `StaticTokens`, `OidcAuthenticator` (JWT/JWKS), `FirstMatch`, `AllowAll` |
| `Remote` (client) | server access | `GrpcRemote` |

Every storage adapter passes the same contract test suite
(`crates/adapters/tests/contract.rs`; GCS and Azure are not run in CI), including a 16-writer CAS linearizability
test, which is what makes adapters substitutable rather than merely type-compatible.

## Landing without a global lock

Lands to the same ref queue up inside a server process. Whoever takes the ref's turn lands
everything queued as one batch:

```
head = refs.get(ref)
for each queued changeset, concurrently:          (reads only; depends on head alone)
  already in head's history?                      -> lands as is
  require parent is an ancestor of head           (O(log n) jump pointers)
  record the entries along each changed path in root(parent)
tip = head
for each queued changeset, in order:              (reads staged, in-memory data)
  if parent == tip: candidate = changeset
  else:
    conflict if, along any of our paths, root(parent) and root(tip) differ
      (unless tip already holds exactly our change)
    candidate = create_changeset(parents=[tip], same changes)   (staged in memory)
  tip = candidate                                 (a conflicting change fails alone)
write everything staged; refs.compare_and_swap(ref, head, tip)
  (lost only to another server replica: redo the batch on its head)
```

There is no merge queue to wait in and no client retry loop: concurrent lands that touch
disjoint paths all succeed. The conflict check walks only the changeset's own paths, so its
cost does not grow with the number of changes that landed since the changeset's base. One
compare-and-swap moves the ref for the whole batch; between server replicas that swap is
still the only coordination. On SQLite, one writer thread commits whatever writes have
queued as a single transaction (group commit).

## Integrity and safety

* Every object is verified (decompress + hash) when a server accepts it, when a tier
  receives it from upstream, and when a client materialises it. A tier never caches a
  corrupt object.
* Decompression is capped (256 MiB per object) against zip bombs.
* All paths are validated at every trust boundary (`..`, `.gd`, separators, NUL).
* The client never writes through a symlink: every parent directory is checked with
  `lstat`, so a hostile tree cannot escape the working copy (the class of git CVE-2024-32002).
* Auth tokens are compared in constant time.

## Land checks

`--land-hook URL` makes the server POST every land to a hook before moving the ref, so CI
results and required reviews gate `main` without a merge queue:

```json
{"ref": "main", "changeset": "<hex id>", "parent": "<hex id or null>",
 "principal": "alice", "author": "Alice <alice@example.com>", "message": "...",
 "paths": ["src/lib.rs"], "paths_truncated": false}
```

A 2xx answer lands the change. Any other answer refuses it, with the response body (first 500
characters) shown to the author; a timeout or unreachable hook also refuses (fail closed).
`--land-hook-token` is sent as `Authorization: Bearer …`. Approve by changeset id: the id
covers the parents and every changed path, so an approval cannot be reused for different
content. The hook runs once per land, before any server-side rebase, and a rebase only
happens when no path conflicts, so what was approved is what lands. Other integrations
implement the `LandCheck` port.

## Virtual checkouts

`gd mount DIR` exposes one revision as a read-only file system on Linux, macOS and Windows.
The platform-independent `VirtualTree` maps inodes to tree entries and loads each directory on
first access; one thin adapter per platform translates operating-system requests:

| Platform | Mechanism | Needs |
|---|---|---|
| Linux | FUSE | `fuse3` |
| macOS | a read-only NFSv3 server on localhost, mounted with the system `mount_nfs` | nothing to install, no kernel extension, no admin rights |
| Windows | the Projected File System (ProjFS) | the built-in optional feature `Client-ProjFS` |

On any Unix, `--serve-only` runs the NFS server and prints the mount command instead, for
machines where only an administrator may mount. On Windows, ProjFS shows every file at once as
a placeholder and fills it in on first read; files already read stay on disk after the mount
stops. A file's size comes from its zstd frame
header or large-file manifest, so `ls -l` downloads no content, and a read of a chunked file
fetches only the chunks it overlaps. Pointing `--objects` at a node daemon makes first reads local.

## Warm build nodes

A node daemon can prefetch a revision at startup and on an interval
(`--prefetch main --prefetch-interval 300`), walking the tree through its own tiered store so
every tree, file and chunk lands on local disk. Objects already cached are not refetched, so
following a moving branch costs only what changed. The prefetched content keeps serving even
if the upstream cache is unreachable.

## Delta storage

`grovedockd compact` re-stores each blob version as a zstd delta against the previous version
of the same path (single writer, lease-protected, safe to run while serving). Reads go
through `DeltaObjects`, which reconstructs chains on the blocking pool and verifies the
result's BLAKE3 id, so nothing above the storage layer ever sees a delta. See ADR-014 for
the safety argument (base pinning, verification before replace, bounded chains).

SQLite returns freed pages to the filesystem with `VACUUM`, which `compact` runs at the end
and which temporarily needs free disk space roughly equal to the database size.

## Large files

Files of 8 MiB or more are split with FastCDC (content-defined, ~1 MiB average chunks) into
ordinary blob objects and stored under the file's own id as a manifest
(`0x03 | size | [(chunk id, length)]`). The file id is still BLAKE3 of the whole content, so
trees, diffs and the git bridge are unaware of chunking. Editing part of a large file
re-uploads only the chunks it touches, and identical chunks dedupe across files and versions.

Nothing holds a whole large file in memory: `gd status` hashes files as streams, `gd commit`
chunks on a worker thread and uploads through a bounded channel, and checkout reassembles
into a temporary file (verifying every chunk and the whole-file id) before an atomic rename.
The server verifies an uploaded manifest by streaming its chunks against the id; caches can
only check structure, and clients verify end to end. Measured on a 1 GiB file (v3.5.0, macOS
arm64, maximum resident set size): `gd commit` peaks at 163 MB, `gd clone` at 139 MB.

## Performance notes

* BLAKE3 (SIMD) for ids, zstd level 3 for storage, mimalloc in both binaries.
* Object fetches are batched (512-1024 ids per round trip) and pipelined (8 batches in
  flight); servers stream responses with bounded buffers (backpressure).
* `gd status` uses size+mtime against the index and hashes only candidates, in parallel
  on all cores; the walker is ripgrep's (`ignore`) and honours `.gitignore` and `.gdignore` (and the deprecated `.pandoignore` and `.sapignore`).
* SQLite runs in WAL mode with one writer and a pool of readers; Postgres batches use
  `UNNEST`/`ANY` for single-round-trip multi-row operations.
