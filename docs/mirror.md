# Mirror to a git host

Run Grovedock beside GitHub, GitLab or Bitbucket instead of replacing them. `gd mirror` keeps a git repository current with a branch, so CI, code search, issue links
and anything else attached to your git host keep working while agents land in Grovedock:

```sh
gd mirror --to git@github.com:acme/fleet.git --every 5        # runs until stopped
gd mirror --to https://bitbucket.org/acme/fleet.git           # once, for cron or CI
```

- Every landed change becomes one git commit with its author, message and date, merges
  included. Changes that came from git become their original commits (same ids), so mirroring
  back to the host you migrated from simply adds what landed since. The same history always makes
  the same commits, so a mirror can be rebuilt from scratch at any time.
- It is incremental: each round pushes only what landed since the last one.
- It never force-pushes. If the destination branch has commits the mirror did not make, it
  stops and says so; protect the branch on your git host so only the mirror writes to it.
- Credentials are git's own: an SSH key, or a credential helper or deploy token. Tokens in
  the URL are never printed.
- The local copy lives in your cache directory (`--dir` to choose).

## Options

| Option | Default | Meaning |
|---|---|---|
| `--to URL` | (required) | where to push: any URL git accepts |
| `--server URL` | `$GD_SERVER`, or the workspace's server | the Grovedock server, with `/OWNER/NAME` on a server that hosts many repositories |
| `--rev REV` | `main` | the Grovedock branch to mirror |
| `--git-ref REF` | `main` | the branch to update on the git host |
| `--every SECONDS` | once | mirror again every SECONDS until interrupted |
| `--dir DIR` | your cache directory | the mirror's local bare git repository |

## Rolling back

Stopping the mirror is the whole rollback: the git host has every change that landed in Grovedock,
as ordinary commits, and people can go back to pushing there. To move a repository the other way,
from a git host into Grovedock with its pull requests and issues, see [Migrate from GitHub](migrate.md).
