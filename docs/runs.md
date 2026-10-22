# Runs (CI)

Runs tests every change with the workflow files a repository already has. A repository moved from
GitHub keeps its `.github/workflows/*.yml`; nothing needs rewriting. New workflows can live in
`.grovedock/workflows/`, and use Grovedock's own words (`on: change_request` for pull requests,
`on: manual` for `workflow_dispatch`).

A run is green or red on the change request, and **Land when ready** waits for it.

## Where jobs run

| | What | Cost |
|---|---|---|
| **Your runners** | Any computer you connect: a spare machine, a cloud VM, a laptop. It asks for work, so nothing has to reach it. | Free, unlimited |
| **Hosted runners** | The server's own, when it offers them: Linux ARM64, a fresh sandbox (gVisor) for each job, removed after. They take jobs that ask for `ubuntu-latest` when none of your runners can. | Minutes a month, per plan |

Connect one (an owner's administrator):

```sh
brew install kabirnarang39/tap/gd
gd runner register --server https://cloud.grovedock.com --owner acme
gd runner start --containers          # each job in a fresh Linux container (needs Docker)
```

Without `--containers` jobs run straight on the machine, in a folder made for the job and deleted
after. `--cpus 2 --memory 4g` caps each job's container; `--runtime runsc` puts it in gVisor.
A runner answers to its OS and architecture (`ubuntu-latest` on Linux, `macos-latest` on a Mac)
and `self-hosted`; add your own with `--label` when registering, or later:

```sh
gd runner labels --owner acme RUNNER_ID --label gpu --label large
```

Code from forks runs only after a maintainer approves the run, never with secrets, and only on
runners registered with `--outsiders` (or hosted runners).

## What a workflow can use

Runs reads the workflow format of `.github/workflows` files as they are. Supported:

- Triggers: `push` (branches, tags, paths), `pull_request` / `change_request`, `workflow_dispatch`
  / `manual` (with inputs), `schedule` (cron, UTC), `workflow_call`
- Jobs: `needs`, `if`, `outputs`, `strategy.matrix` (with `include`, `exclude`, `fail-fast`,
  `max-parallel`), `timeout-minutes`, `continue-on-error`, `env`, `defaults`, `concurrency`
  (workflow level), `environment`, `container`, `services`
- Steps: `run`, `uses` (see below), `with`, `env`, `if`, `id`, `shell`, `working-directory`
- Expressions: contexts `github`, `env`, `vars`, `secrets`, `inputs`, `matrix`, `needs`, `steps`,
  `job`, `runner`; functions `success()`, `failure()`, `always()`, `cancelled()`, `contains`,
  `startsWith`, `endsWith`, `format`, `join`, `toJSON`, `fromJSON`, `hashFiles` (in steps, over
  the checkout; `!pattern` excludes)
- Files a step writes: `$GITHUB_OUTPUT`, `$GITHUB_ENV`, `$GITHUB_PATH`, `$GITHUB_STEP_SUMMARY`
  (shown on the run page)
- Workflow commands: `::error`, `::warning`, `::notice` (listed on the run page, linked to the
  file and line), `::group::` / `::endgroup::` (folded in the log), `::add-mask::`
- Reusable workflows in the same repository (`uses: ./.github/workflows/build.yml`)
- Artifacts (`actions/upload-artifact`, `actions/download-artifact`), and releases from a tag's
  workflow (`softprops/action-gh-release` and alike make a Grovedock release)

Not yet: a matrix built from another job's output (`fromJSON(needs…)`), job-level `concurrency`,
Docker actions and `docker://` steps, actions' `pre`/`post` steps, Windows runners.
A workflow that uses one says so on its run, rather than doing something else.

## Actions

`uses: owner/repo@ref` runs that action: `actions/setup-node`, `ruby/setup-ruby`, or any other. It runs inside the job, with the job's own access and never more.

A tag can be moved to other code; that is how attackers got into thousands of repositories'
CI in March 2025. So by default, each tag is **pinned on first use**: the first time an owner's
runs use `owner/repo@v4`, the commit it points at is recorded, and every run after fetches exactly
that commit. If the tag later points elsewhere, runs stop at it, saying so, until an
administrator checks what changed and accepts the new commit on the owner's **Runners** page.
Workflows run as written; nobody has to pin by hand. (If GitHub can't be reached to look a tag
up, only large publishers' actions and full commit SHAs run.)

Administrators can instead allow any action at any version, or only the ones listed
(`owner/repo`, or `owner/*`). A step that isn't allowed fails and says why.

Actions in the repository itself (`uses: ./.github/actions/x`) always run. `actions/checkout`,
artifacts, releases and caches are done by the runner itself.

## Secrets, variables and environments

- **Secrets** (`${{ secrets.NAME }}`) are sealed when saved and never shown again, here or in
  logs. Only trusted runs get them: code already on a protected branch, pushed by someone who can
  write to the repository, or a tag made by a maintainer.
- **Variables** (`${{ vars.NAME }}`) are plain settings, like a region or a URL.
- Both can be set for the organisation (every repository), a repository, or one of its
  environments. The narrowest wins: environment, then repository, then organisation.
- An **environment** (`environment: production` on a job) can have reviewers, people who approve
  each deployment before the job starts, and branches it deploys from.

```sh
gd secret set NPM_TOKEN --org < token.txt      # every repository of the owner
gd secret set DEPLOY_KEY --env production < key.pem
gd var set REGION ap-south-1
gd env set production --reviewer asha --branch main
gd env approve 42 production                   # let run #42 deploy
```

## Agents

An agent's change runs like anyone's. A change that weakens CI (skips tests, lets failures
through, edits workflow files) is a blocker on its change request until a person approves it.
