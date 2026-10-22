# The gd protocol, version 1

Status: **stable**. Published 2026-10-22. Wire package: `gd.v1`.

This document says what a gd server must do so that any gd client works with it, and what a
client may rely on. It covers the core: content-addressed objects, changesets, refs, landing,
history and reading files. The platform services (change requests, issues, runs) are described
in the [protocol reference](../reference/protocol.md) and are not part of conformance.

The key words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119. Each requirement a
conformance run checks has an id in brackets, like **[OBJ-1]**; `gd conformance` runs them all
(see [Conformance](#9-conformance)).

## 1. Transport

gRPC over HTTP/2, with gRPC-Web accepted on the same port for browsers. Messages are defined in
`proto/gd.proto` (package `gd.v1`). A client authenticates with `authorization: Bearer TOKEN`; a
server that hosts many repositories reads the repository from the `x-gd-repo: OWNER/NAME`
header.

Errors use gRPC status codes with these meanings, and a server MUST use them for these cases:

| Code | Meaning |
|---|---|
| `NOT_FOUND` | No such object, changeset, ref or path. |
| `ABORTED` | A conflict: a compare-and-swap lost, or a land's paths clash with what landed meanwhile. Retrying after syncing may succeed. |
| `INVALID_ARGUMENT` | The request is malformed (a bad path, a bad ref name). Retrying won't help. |
| `DATA_LOSS` | An object or changeset doesn't match its id. |
| `PERMISSION_DENIED` | The caller may not do this. |
| `FAILED_PRECONDITION` | A rule refused it (a protected branch, a policy). |
| `UNIMPLEMENTED` | The server has this feature turned off. |

## 2. Objects

Files and directories are immutable objects named by their content.

- An object is a **blob** (kind 1, a file's bytes) or a **tree** (kind 2, a directory).
- Its id is the 32-byte **BLAKE3** hash of one byte holding its kind followed by its content:
  `id = BLAKE3(kind || content)`. **[OBJ-1]**
- On the wire and at rest, an object's payload is its kind byte followed by its content
  compressed as one zstd frame: `kind || zstd(content)`. Ids hash the *uncompressed* content,
  so compression settings never change an id.
- A receiver MUST verify a payload against its id before trusting it, and a server MUST refuse
  to store a payload that doesn't match its id. **[OBJ-2]**
- An object is at most 256 MiB uncompressed.
- `Objects.Missing(ids)` returns exactly the ids the server can't serve, so a client uploads
  only those. **[OBJ-3]** `Objects.Get` streams the payloads asked for. **[OBJ-4]**

**Trees.** A tree's content is a protobuf `Tree { repeated TreeEntry entries }` with
`TreeEntry { name, mode, id }`. Entries MUST be strictly sorted by `name` (byte order), names
unique, and each a single path component (not empty, not `.` or `..`, no `/` or NUL). Modes:
`0` file, `1` executable, `2` symlink (its blob holds the target), `3` directory (its id names a
tree). A tree that breaks these rules is corrupt.

**Large files** of 8 MiB or more MAY be stored as a manifest: payload byte `3` followed by
`Manifest { size, repeated ChunkRef { id, len } }`, whose chunks are blobs cut with FastCDC
(256 KiB minimum, 1 MiB average, 4 MiB maximum). The file keeps its ordinary blob id, the hash of
its whole content.

## 3. Changesets

A changeset is the unit of history: a list of changed files on top of its parents, not a tree.

```proto
Changeset { repeated bytes parents = 1; repeated FileChange changes = 2;
            string author = 3; string message = 4; int64 timestamp_ms = 5; GitOrigin git = 6; }
FileChange { string path = 1; bytes blob = 2; Mode mode = 3; }   // empty blob: deleted
```

- Its id is `BLAKE3(3 || encoded)`, where `encoded` is its protobuf encoding with fields in
  tag order (prost's canonical form). **[CS-1]**
- `Changesets.Create` builds the changeset from the request, stores it and returns its
  `ChangesetInfo`: the id, the root tree it produces (derived by the server by applying the
  changes to the first parent's tree), its generation (greater than each of its parents') and
  the canonical encoding. **[CS-2]** A client MAY re-hash `encoded` to check the id.
- Every parent MUST already exist on the server, and no parent may be listed twice; otherwise
  the server refuses it (`INVALID_ARGUMENT` or `NOT_FOUND`). **[CS-5]**
- Creating the same changeset twice yields the same id; changesets are never changed or deleted.
  **[CS-3]**
- Paths are `/`-separated, relative, and their components follow the tree-name rules. A path
  under `.git` or `.gd` MUST be refused (`INVALID_ARGUMENT`), so no repository can plant files
  that change a checkout's tools. **[CS-4]**
- `git`, when present, holds what is needed to recreate the exact git commit the changeset came
  from (author and committer lines, raw message, signature, submodule entries, and the commit id
  it must reproduce). Changesets made with gd don't have it, so their ids don't depend on git.
- `Changesets.CreateMany` (since 1.1) takes a stream of the same requests and answers each, in
  order, with its id, so a client sends a whole history without a round trip per change. A
  parent in `parent_refs` is either an id or `earlier: N`, the change made at position N
  (from 0) on the same stream. The result is exactly what the same requests sent one by one to
  `Create` would give, ids included. **[CS-6]** A server MAY answer `UNIMPLEMENTED`; clients
  then send the changes one by one. At most 10,000 changes per call.
- A server MAY keep a **provenance** record beside a changeset made with gd: the account that
  uploaded it and what an agent declared (model, session, issue). It is not part of the
  changeset and never changes its id.

## 4. Refs

Refs name changesets: branches, and tags under `tags/`.

- `Refs.Get` returns whether a ref exists and its changeset. `Refs.List` lists them all.
- `Refs.CompareAndSwap(name, expect_exists, expected, new)` moves a ref only if it currently
  holds `expected` (or doesn't exist, when `expect_exists` is false), atomically; otherwise it
  returns `swapped = false` and changes nothing. **[REF-1]**
- A tag is made once and never moves: a compare-and-swap of an existing tag MUST fail, and
  nothing lands on a tag. **[REF-2]**

## 5. Landing

`Land.Land(ref, changeset)` puts a changeset on a branch, rebasing it on the server when the
branch has moved since the changeset's base. There is no merge queue and no lock a client holds.

- If the branch's head is the changeset's first parent, the branch moves to the changeset as it
  is (`rebased = false`). **[LAND-1]**
- Otherwise the server rebases: if none of the changeset's paths changed on the branch since
  its base, it creates a new changeset with the same changes on the head and moves the branch to
  it (`rebased = true`, a new id). **[LAND-2]**
- If any of its paths changed on both sides, nothing moves and the land fails with `ABORTED`,
  naming the paths. **[LAND-3]**
- Concurrent lands of changes to different paths MUST all succeed, none lost: each lands on top
  of the others in some order. **[LAND-4]**
- Landing a changeset that already landed (a retry after a lost reply) reports where it landed
  instead of applying it twice. **[LAND-5]**
- With `fast_forward`, the ref moves to the changeset as it is only if the ref's head is its
  ancestor; nothing is rebased (how `git push` works).

## 6. History and reading

- `History.Log(rev, path, limit)` streams changesets along first parents from `rev`, newest
  first, only those touching `path` when one is given. **[HIST-1]**
- `History.Blame(rev, path)` returns each line of the file with the changeset that last changed
  it along first parents, and the line's number in that changeset's version of the file.
  **[HIST-2]**
- `Diff.Diff(from, to)` returns the files that differ between two revisions. **[HIST-3]**
- `MergeBase.MergeBase(a, b)` returns their best common ancestor. **[HIST-4]**
- `Browse.ReadFile(rev, path)` returns a file's content; `Browse.ListDir(rev, path)` a
  directory's entries. **[READ-1]**
- A revision (`rev`) is a ref name, or a changeset id as 64 hex characters.

## 7. Locks

For files that can't be merged. `Locks.Lock(path)` gives the caller the lock if nobody else holds
it; while locked, the server MUST refuse anyone else's land touching that path. `Locks.Unlock`
releases it; `Locks.List(prefix)` lists them. **[LOCK-1]**

## 8. Git compatibility

gd and git describe the same history. A server that accepts git (`git push`) and serves it
(`git clone`) MUST keep these guarantees:

- **Round trip with the same ids.** A git history imported into gd and exported again yields
  the very same commits: the same commit ids, trees and blobs, signatures, encodings and
  submodule entries included. **[GIT-1]**
- **Stable ids for gd's own changes.** Changesets made with gd become git commits
  deterministically: the same history always makes the same commits, so a mirror can be
  rebuilt from nothing and match. **[GIT-2]**
- Where gd has no equivalent (submodules), what git needs is kept in `GitOrigin` and given back.

## 9. Conformance

`gd conformance --server URL` runs every check marked **[…]** above against a server and an
empty repository it can write to, and reports each as passed or failed with what it saw:

```sh
gd conformance --server https://gd.example.com/acme/conformance --token "$TOKEN"
```

A server conforms to version 1 when every check passes. The git checks need `git` on the
machine running them and a server that serves git over HTTPS; without either they're reported as
skipped, not passed.

The checks only create objects, changesets and refs under `conformance/` in the repository they
are given. Use an empty repository made for it.

## 10. Versions

This is version 1. Within version 1, fields and methods are only added: a client written for
1.0 keeps working with any 1.x server, and unknown fields are ignored. A change that breaks that
gets a new package (`gd.v2`), served beside `gd.v1` for at least a year.

### Changes

- **1.0** (2026-10-22): first published.
- **1.1** (2026-10-22): `Changesets.CreateMany`, a stream of changes with parents among them
  (imports and git pushes without a round trip per change). [CS-6]
