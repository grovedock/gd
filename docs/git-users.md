# Coming from git

You can move to Grovedock one person at a time. git users and `gd` users work on the same
branches of the same server.

## Keep using git

On Grovedock Cloud (and any server run with `--git-dir`), stock git works with nothing to
install. Use a token as the password (Tokens, in the app's account menu):

```sh
git clone https://cloud.grovedock.com/acme/fleet.git
git push          # lands as you, as it is: protections, review rules and secret scanning apply
git pull --rebase
```

A push that Grovedock refuses fails with the reason (`remote: Grovedock refused
refs/heads/main: this change adds a secret: …`). A push lands only when it fast-forwards, as
in git; pushing deletes nothing (delete branches in the app or with `gd`). Public
repositories clone without a token. IDEs, CI systems and package managers that fetch with
git (Go modules, npm and Cargo git dependencies) use the same address.

### The `gd::` remote helper

With `git-remote-gd` on your `PATH` (every install method puts it next to `gd`), git can also
talk to a Grovedock server over its own protocol, on servers that don't serve git's:

```sh
git clone gd::https://cloud.grovedock.com/acme/fleet repo
git push          # lands on the server; if main moved, the server rebases instead of rejecting
git pull --rebase
```

Fetches are incremental. Set `GD_PUSH=strict` to get git's reject-if-behind behaviour
instead of a server-side rebase.

## Command map

| git | gd | Notes |
|---|---|---|
| `git clone URL` | `gd clone URL DIR` | add `--path services/api` to check out part of the repository |
| `git status` | `gd status` | alias `st` |
| `git add` + `git commit -m` | `gd commit -m MSG [PATHS]` | no staging area; pass paths to commit a subset |
| `git push` | `gd land` | alias `push`; the server rebases if `main` moved; several commits land in order (`--squash` lands them as one) |
| `git pull` | `gd sync` | alias `pull` |
| `git switch -c x` / `git switch x` | `gd checkout -b x` / `gd checkout x` | |
| `git log -- path` | `gd log path` | answered by the server, no local history |
| `git diff` / `git diff A B` | `gd diff` / `gd diff A B` | |
| `git show REV` | `gd show REV` | |
| `git blame f` | `gd blame f` | |
| `git show REV:path` | `gd cat REV path` | works without a clone |
| `git ls-tree REV dir` | `gd ls REV [dir]` | works without a clone |
| `git branch` | `gd refs` | alias `branch` |
| `git tag v1.2.0` / `git tag` | `gd tag v1.2.0` / `gd tag` | `--at REV` to tag something else; tags never move, and `git push --tags` makes them too |
| (none) | `gd cr undo-agent NAME --since 2h` | undo what an agent landed since a time, as one change request; people's later edits stay where they don't overlap |
| `git merge` conflicts | `gd cr conflicts N` / `gd cr resolve N --from DIR` | files both changed are merged line by line on the server; only real clashes need you, and no clone is needed |
| (GitHub: Releases) | `gd release create v1.2.0 --file app.tar.gz` | notes written from what landed since the last release; `gd release notes` previews them and suggests the version |
| `git merge-base A B` | `gd merge-base A B` | |
| (none) | `gd find PATTERN` | path search on the server |
| `git grep WORDS` | `gd search WORDS [path:dir/ lang:ts]` | from the server's index: no clone, fast on any size of repository; `gd symbol NAME` finds definitions |
| `git lfs lock f` / `git lfs unlock f` | `gd lock f` / `gd unlock f` | exclusive locks for files that cannot be merged |
| `git lfs locks` | `gd locks [dir]` | |
| (none) | `gd mount DIR --rev main` | a read-only view, files fetched on first read (Linux, macOS, Windows) |

## Moving history in and out

```sh
gd import-git ~/code/my-repo --server https://cloud.grovedock.com/acme/fleet   # incremental; rerun any time
gd export-git ./mirror.git                                     # the whole history, same commit ids for what came from git
```

## Keep GitHub, GitLab or Bitbucket up to date

`gd mirror` pushes every landed change to an existing git host, so CI, code search and issue
links keep working there. See [Mirror to a git host](mirror.md).

Submodules are not imported. Commits are imported with their authors, messages and dates,
so `gd log` and `gd blame` show the original history.

## What works differently

- **No staging area.** `gd commit` takes paths instead of `git add`.
- **No local history.** `log`, `blame` and `diff` between revisions ask the server. You
  need the server to be reachable; this is the main trade-off against git.
- **Landing replaces push.** If `main` moved, the server rebases your change when it
  touches different files. You only resolve conflicts that are real.
