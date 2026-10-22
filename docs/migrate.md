# Migrate from GitHub

One command moves a repository from GitHub, GitLab, Bitbucket or any git host:

```sh
export GD_GITHUB_TOKEN=$(gh auth token)   # private repositories, and GitHub's rate limit
gd migrate https://github.com/acme/fleet --to https://cloud.grovedock.com/acme/fleet
gd migrate git@gitlab.com:acme/fleet.git --to https://cloud.grovedock.com/acme/fleet
gd migrate https://github.example.com/acme/fleet --to https://cloud.grovedock.com/acme/fleet \
  --github-api https://github.example.com/api/v3          # GitHub Enterprise Server
```

## What comes across

| | |
|---|---|
| **History** | every commit on every branch, with its author, message and date, and merges; the files at each branch tip are byte-identical to git's |
| **Pull requests** (GitHub) | each open pull request becomes a change request from a `github/pr-N` branch into its base, with its title, description and a link back |
| **Issues** (GitHub) | open and closed, oldest first, with every comment, labels, and closed as done or not planned |
| **The repository** | created if it doesn't exist, with its GitHub description |

Git commit ids don't change: every change remembers what git hashes (author and committer with
their time zones, the message's encoding, signatures, submodule entries), so the git view of the
repository (clones over HTTPS, `gd mirror`, `gd export-git`) is the same commits, merges included.
Existing clones keep working: point them at Grovedock with one line,

```sh
git remote set-url origin https://cloud.grovedock.com/acme/fleet.git
```

and links to commits keep their ids. (Signed commits keep theirs with git 2.48 or later on the
machine that imports; with an older git they're imported without the signature, and get new ids.) One more exception: git's
own first months (2005) stored some files with mode `100664`, which git no longer writes and
can't re-import; Grovedock keeps them as ordinary files, so a repository whose history has them
(git/git does, from its first commit) comes back with the same files and messages but new
commit ids from that commit on. A repository started after 2005 is unaffected. Issues, pull requests and comments are posted by whoever runs the migration, each
naming its original GitHub author with a link to the original. Run it again later to bring
across new commits; history is imported incrementally.

## Keep the old host current while you move

From GitHub, `migrate` ends by printing the `gd mirror` command for the repository you came
from (from other hosts, write it the same way):

```sh
gd mirror --to https://github.com/acme/fleet --server https://cloud.grovedock.com/acme/fleet --every 10
```

With the mirror running, CI, code search and issue links on the old host keep working while
people and agents move to Grovedock at their own pace. Stopping the mirror is the whole rollback.

## Tokens

- A GitHub token is needed for private repositories and to stay under GitHub's rate limit (60
  requests an hour without one). Give it in `GD_GITHUB_TOKEN` (or `GITHUB_TOKEN`), or in a file
  with `--github-token-file`. `--github-token` works but warns, since other users of the machine
  can see command lines. The token is sent only to the repository's own host and passed to git
  through its environment; source URLs are printed without credentials.
- `--no-issues` and `--no-pull-requests` skip those.
- The destination uses your Grovedock token (`GD_TOKEN`), which needs to be allowed to create
  repositories in the organisation (an organisation administrator, or a site administrator for a
  new organisation).
