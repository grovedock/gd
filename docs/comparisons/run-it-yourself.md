# Run the comparison yourself

The numbers in [Benchmarks](git-vs-gd.md) come from `bench/hosted_compare.py`. It measures GitHub
and a Grovedock server the same way, from your machine, over the internet, so you can check them
with your own account, network and repository. It prints a markdown table; nothing is sent
anywhere else.

## What it measures

| Test | GitHub | Grovedock |
|---|---|---|
| **land**: N agents each land K small changes on `main` at once, none touching the same file | commit, push; on rejection fetch, rebase, push again (the usual agent loop) | `gd commit` then `gd land` |
| **clone**: full clone of git/git | GitHub's copy of `git/git` | your import of git/git |
| **read**: CI reads one file without a clone | the contents API (`gh api`) and `raw.githubusercontent.com` | `gd cat` |

Defaults: 16 agents, 5 changes each, 3 clones, 30 reads. Change them with `--agents`, `--each`,
`--clones` and `--reads`.

## Before you start

- Python 3.9 or later, git, and both command-line tools signed in: `gh auth login` and
  `gd login https://cloud.grovedock.com` (or your own server). The script never reads your
  credentials; `gh` and `gd` use their own stores.
- A small repository on each side for the agents to land on, with a `main` branch. Use a scratch
  repository: the land test adds small files under `bench/` on `main`.
- On Grovedock, git/git imported once for the clone test:

  ```sh
  git clone --mirror https://github.com/git/git git.git
  gd import-git git.git --server https://cloud.grovedock.com/ORG/git
  ```

  This is about 1 GB of storage. Check your plan's storage limit on the organisation's
  **Plan and usage** tab first, and delete the repository afterwards if you don't need it.

## Run it

```sh
bench/hosted_compare.py github    --repo YOU/gd-bench
bench/hosted_compare.py grovedock --repo ORG/gd-bench --server https://cloud.grovedock.com --big ORG/git
```

Run both from the same machine, one after the other, so they share a network. Run each twice and
keep the second: the first clone warms caches on both sides.

## Reading the results

- **land**: changes landed, changes a second, p50 and p99 time for one change, client retries
  and failures. Retries are the cost of git's push model under contention; gd lands
  on the server and never rejects a change that doesn't conflict.
- **clone**: median time. **read**: median and p99.

Your numbers will differ from ours with distance to each service and your connection. If GitHub
wins a test on your setup, that is a real result: please open an issue with the table, so the
benchmarks page can show it.
