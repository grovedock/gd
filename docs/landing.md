# How landing works

Landing is how a change reaches a branch. In git you push, and if someone pushed first you
fetch, rebase and try again. In Grovedock the server does that part: you send a changeset, and the
server either lands it or tells you exactly why not.

## What happens when you land

`gd land` (or `git push` through `git-remote-gd`, or `gd_write` over MCP) asks the server
to move a branch to include your changeset. There are three outcomes:

| Outcome | When | What you see |
|---|---|---|
| Landed as is | the branch has not moved since your base | `landed c9bdddcbb14c on main` |
| Landed, rebased | the branch moved, but nothing that landed since your base touched your paths | the server creates the rebased changeset and lands that; nothing to pull or retry |
| Refused | someone changed one of your paths since your base, a file you changed is locked, a pre-land check said no, or the branch is protected | the reason and the paths; the branch is untouched |

A refusal is never partial: either your whole changeset lands or none of it does.

## Why there is no merge queue

Lands to the same branch join a queue inside the server. Whichever land takes the branch's
turn lands everything queued as one batch: each changeset is checked against the tip only along
its own paths, rebased in memory if it needs to be, and the branch moves once, by a single
compare-and-swap, for the whole batch. A conflicting change fails alone and the rest of its
batch lands.

Because the conflict check walks only the changeset's own paths, its cost doesn't grow with
the number of changes that landed since your base. Measured on one laptop with every agent
landing on `main` ([method and limits](comparisons/git-vs-gd.md#landing-at-scale-16-to-5000-concurrent-agents)):

| Agents | git lands/s | Grovedock lands/s | Grovedock p99 | Grovedock errors |
|---:|---:|---:|---:|---:|
| 16 | 2.0 | 758 | 55 ms | 0 |
| 200 | 0.5 | 1,012 | 302 ms | 0 |
| 1,000 | 0.2 | 504 | 2.66 s | 0 |

Between server replicas the compare-and-swap is still the only coordination, so correctness
doesn't depend on the queue. Landing is tested by
[deterministic simulation and chaos runs](testing.md) against four invariants, the first being
that no acknowledged land is ever lost.

## What can stop a land

- **A real conflict.** Someone changed one of your paths since your base. Sync, resolve, land
  again. For change requests, `gd cr rebase` replays the change on the target's tip when the
  two touch different files.
- **A lock.** `gd lock` reserves files that can't be merged; a land that changes someone
  else's locked file is refused with who holds it.
- **A pre-land check.** With `--land-hook`, the server asks your URL before every land and
  refuses on anything but a 2xx answer ([Architecture](ARCHITECTURE.md#land-checks)).
- **Branch protection.** A protected branch accepts changes only through a reviewed change
  request; an agent's direct write becomes a change request automatically
  ([Organisations, people and access](orgs.md#change-requests-and-protected-branches)).
- **Access.** Access rules can limit who writes which directories and branches.

The full algorithm is in [Architecture](ARCHITECTURE.md#landing-without-a-global-lock), and the
reasoning in [ADR-006 and ADR-017](DECISIONS.md#adr-017-batch-concurrent-lands-per-ref-instead-of-retrying-compare-and-swap).
