# Get started

From nothing to a landed change in about five minutes.

## 1. Sign up

Go to [cloud.grovedock.com](https://cloud.grovedock.com) and continue with Google (or an email
link). You choose a username, then a plan: Free is enough for this page. Name your
organisation, for example `acme`.

## 2. Install gd

```sh
brew install kabirnarang39/tap/gd
```

Other ways:

```sh
# macOS and Linux, verifies the release checksum
curl -fsSL https://raw.githubusercontent.com/grovedock/gd/main/install.sh | sh

# From source
cargo install grovedock
```

Windows: download the `.zip` from the [releases page](https://github.com/grovedock/gd/releases).
You get `gd` and `git-remote-gd` (which lets plain git talk to Grovedock).

## 3. Sign in from the terminal

```sh
gd login https://cloud.grovedock.com     # opens the browser; the token is saved for this server
gd whoami
```

Commands use `--token`, then `GD_TOKEN`, then the token `gd login` saved. `gd logout` revokes it.

## 4. Bring a git repository

Create a repository in the web app (or in the onboarding's last steps), then:

```sh
gd import-git ~/code/my-repo --server https://cloud.grovedock.com/acme/web
```

Import is deterministic and incremental: run it again later and it uploads only new commits.
`gd export-git` goes the other way, with identical trees, and `gd mirror` keeps a git host such
as GitHub or Bitbucket up to date continuously.

## 5. Make and land a change

```sh
gd clone https://cloud.grovedock.com/acme/web web
cd web
echo hello > hello.txt
gd commit -m "Say hello"
gd land
```

`gd commit` creates the commit on the server from the files you changed. `gd land`
moves `main` to include it. If someone landed first and your change touches different
files, the server rebases it for you; there is nothing to pull and retry.

Files matched by `.gitignore` or `.gdignore` (same syntax) are left out. `.sapignore`, the
old name, still works but is deprecated.

To check out part of a large repository, add `--path`:

```sh
gd clone https://cloud.grovedock.com/acme/web api --path services/api
```

## Read without a clone

```sh
export GD_SERVER=https://cloud.grovedock.com/acme/web
gd cat main services/api/config.yaml
gd log services/api
gd ls main services
```

Inside a workspace these commands use the workspace's server when `GD_SERVER` is unset, so
it is only needed elsewhere. `--server` always wins; a `GD_SERVER` that names the workspace's
own server without a repository keeps the workspace's repository.

## Mount instead of checking out

A mount shows a whole revision at once and downloads a file only when something reads it,
which suits big repositories and CI jobs that touch a few files:

```sh
mkdir ~/main && gd mount ~/main --rev main      # Ctrl-C to unmount
```

It works on Linux (FUSE), macOS (built in, nothing to install) and Windows (the built-in
Projected File System; enable it once with
`Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS`). Mounts are read-only;
use `gd clone` to make changes. If your Mac's management policy refuses the mount, run
`gd mount ~/main --serve-only` and have an administrator run the command it prints.

## Lock files that cannot be merged

Art, models, video and other binaries cannot be merged, so two people editing one at the
same time lose someone's work. Lock the file first:

```sh
gd lock art/hero.psd          # others can still read it, but cannot land changes to it
gd locks art                  # who holds what
gd unlock art/hero.psd        # or --force to release someone else's lock
```

A land that changes a file someone else has locked is refused with
``rejected: `art/hero.psd` is locked by alice``. Locks need write access to the path, and
every lock and unlock is recorded in the audit log.

## Next

- Point agents at it: [Agents and MCP](agents.md).
- Keep using git against the same server: [Coming from git](git-users.md).
- Keep GitHub current while you try it: [Mirror to a git host](mirror.md).
- Bring your team: invite people from the organisation's People tab ([Organisations and access](orgs.md)).
