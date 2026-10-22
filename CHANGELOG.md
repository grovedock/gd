# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [4.0.2] - 2026-10-22

### Changed
- `gd import-git` streams commits to the server (protocol 1.1, `Changesets/CreateMany`) instead of one round trip each. git/git's 82,542 commits import locally in about 16 minutes with debug builds; over the internet the old path managed 5.5 commits a second.
- Go, Python and TypeScript SDKs regenerated for protocol 1.1.

## [4.0.1] - 2026-10-22

### Fixed
- `gd` and self-hosted runners connect to `https://` servers. Every client was built without TLS, so the CLI could reach `http://` servers only.
- Behind a proxy, a burst of calls could make the server drop the shared connection (HTTP/2 "too many data frames"), failing every call on it. The server now uses fixed, generous flow-control windows.
- Caddy no longer grows to hundreds of megabytes under load: the web app ships precompressed (`.br`, `.gz`) and Caddy's memory is capped.
- Checkboxes and radio buttons read correctly in the light theme on a dark system.

### Changed
- Sign in with Google reaches an account made with email links when Google runs that mailbox (Gmail, or a Workspace domain); otherwise the refusal explains how to connect it.
- Emails rebuilt to render the same in every mail client, light and dark, with the Grovedock mark.
- Empty pages show where the first thing will land and what to do next; the sidebar offers *New repository* and *Bring a repository*.
- The Grovedock mark in every browser tab, including sign-in pages, Safari and home screens.
- The website and docs are served from Grovedock's own server.

## [4.0.0] - 2026-10-22

Grovedock for teams where people and AI agents ship together.

### Added
- **Runs (CI).** Workflows in `.github/workflows` run as written on runners you connect
  (`gd runner start --containers`: a fresh Linux container per job) or on hosted runners: push,
  change request, manual, schedule and reusable-workflow triggers; matrices, `needs`, services and
  job containers, concurrency, environments with reviewers, secrets and variables per
  organisation, repository and environment, artifacts, releases, annotations, job summaries,
  `hashFiles()`. Marketplace actions run; each tag is **pinned on first use**, and a tag moved
  later waits for an administrator.
- **Land when ready.** A change request lands itself once its rules are met, tested on the
  branch's current tip, so a change that breaks the branch never reaches it.
- **Review at agent volume.** A review queue ranked by risk, with the reasons, and a batch for
  low-risk changes. Every change request's packet flags weakened CI (needs a person), untrusted
  text in workflows (needs a person), new functions that repeat existing ones, code changed with
  no test, and changes spread over many areas.
- **Agents, accountable.** Every agent that can reach an organisation, who it answers to, what it
  can touch, daily budgets for lands and CI runs, and one switch that pauses them all.
- **Who wrote each line.** The server records who made each change and what an agent declared
  (model, session, issue), beside the change, not in its message. Blame names the writer of every
  line; git-ai notes come over on import and go back out on export and mirror;
  `gd blame PATH --by AGENT`.
- **Conflict foresight.** Before starting, an agent (MCP `gd_who_is_changing`) or a person sees
  who else is changing the same files.
- **Status page** at `/status`, for anyone, with incidents posted by site administrators.
- **The gd protocol, specified.** `docs/spec/protocol-v1.md`, and `gd conformance` to check any
  server against it.
- Restore drill (`restore-drill.sh`), and `from-pando.sh` to move a box set up with the old kit.

### Changed
- Commands, packages and settings use the Grovedock names: `grovedockd`, `GD_*` environment
  variables (were `PANDO_*`). `from-pando.sh` converts a box's settings.
- The web app: a new design system and redesigned Home, repository, change request, review,
  organisation, runners and settings screens.

### Security
- Untrusted text pasted into a workflow's commands or prompts needs a person's approval to land.
- Actions are pinned on first use, so a moved tag can't swap code into CI.
- The review packet's checks are bounded, so a huge repository can't exhaust a small server.

## [3.5.1] - 2026-10-19

### Added
- Website: a new landing page and a pricing page with a Team cost calculator. The product name,
  links and list prices live in `site/site.conf`, which `scripts/build-site.sh` fills in; the
  pages self-host their fonts. URLs are unchanged: the landing page at the site root, docs under
  `/docs/`, the console demo under `/console/`, and the new `/pricing.html`.
- Releases attach the web console, `pando-console-vVERSION.tar.gz` with its `.sha256`, covered by
  the release's signed provenance and SBOM attestations.
- `pandod server --console-dir DIR` (`PANDO_CONSOLE_DIR`) serves the console at `/` on the
  server's own address: links into the app fall back to `index.html`, hashed assets are cached for
  a year and everything else revalidated, with CSP, `nosniff`, framing and referrer headers
  (ADR-036).
- A cross-tenant isolation suite, a release gate: every method in the protocol is called with
  another organisation's tokens (a person, an agent, tokens limited to a repository) at an
  organisation's repositories, change requests, issues, webhooks, protections, locks, teams and
  tokens, and must be refused or show nothing of theirs; a method missing from its table fails it.

### Changed
- The product is now called Grovedock: the website, documentation, web console and sign-in pages
  use the new name and wordmark. Commands, packages, environment variables (`PANDO_*`), webhook
  headers and the protocol keep the `pando` name until 3.6.
- The website moves to https://grovedock.com/ (landing at `/`, `/pricing.html`, `/docs/`,
  `/console/`); old `kabirnarang39.github.io/pando/` links redirect. A server that lists the
  hosted console in `--web-origins` should list `https://grovedock.com`.
- Website and docs home redesigned: a night-grove hero (aspen trunks on one root, animated once and
  still under reduced motion), a benchmark panel, a reworked pricing page and footer.

### Fixed
- `docs/platform.md` said git manages 2 lands/s at 200 agents; the benchmark measured 0.5.
- `docker compose up` failed: every service listens on the network without a token, which
  pandod refuses. The compose file now requires `PANDO_TOKEN` in `.env` (with a clear error when
  it is missing) and passes it to the server, the cache and the daemon.
- A bare `PANDO_SERVER` (no `/OWNER/NAME`) naming a clone's own server no longer hides the
  clone's repository, so `pando protect` and `pando cr` work inside a clone with it set.
- The remote MCP endpoint answers clients on protocol versions before 2026-07-28 (their
  `initialize`) without a session, instead of "Session management is not supported".
- `--repos-objects file:///…/{repo_id}` no longer makes a directory literally named `{repo_id}`
  (readiness checks the template's base directory), and a `--platform` server no longer creates
  an unused `./pando-data`: its default `--metadata` is now `memory`.
- `pando admin limits suspend` says when the server doesn't enforce limits (not
  `--multi-tenant`); `pando org usage` prints dates instead of epoch milliseconds.
- `pando migrate` prints the `pando mirror` command for any git source, not only GitHub.
- The working copy reads `.pandoignore`; `.sapignore` still works, with a deprecation warning.
- `pando webhook add --help` lists the `issue` event; `pandod backup --help` points to
  `docs/backup.md`.
- The console demo on the public site no longer asks for `/login/providers` (a 404).
- `docs/platform.md` no longer claims per-path owners in repository settings.

### Security
- Creating a token limited to repositories told whether a private repository you can't see
  exists; such a name is now "no repository", like everywhere else.
- Granting a repository to a team told whether another organisation has a team of that name;
  only the owning organisation's teams are recognised.

## [3.5.0] - 2026-10-18

### Added
- `pando login [SERVER_URL]` signs the CLI in from the browser (OAuth device flow) and saves a
  90-day token in `~/.config/pando/credentials.json` (mode 0600), used when neither `--token` nor
  `PANDO_TOKEN` is given; `pando logout` revokes it and `pando whoami` shows the account (ADR-033).
- Invitations: organisation administrators invite by email (`CreateInvitation`,
  `ListInvitations`, `RevokeInvitation`); the single-use, 7-day link signs the person in (creating
  an account if needed) and adds them with the invited role (ADR-033).
- First sign-ins ask the person to confirm they are 18 or older and accept the terms
  (`--terms-url`, `--privacy-url`) before an account is created; the time is stored on the account.
- `Viewer` returns the calling token's id, so clients can sign out by revoking it.
- Console: "Sign in with …" buttons and an email form when the server has sign-in pages, sign out,
  a guided set-up for a new organisation (name it, invite teammates, connect an agent), and
  invitations on the organisation's People tab.
- `pandod repo disable|enable OWNER/NAME` takes a repository down at once without deleting it,
  and `pandod repo delete OWNER/NAME` removes it and its storage; documented with a secret purge
  procedure in docs/deploy.md.
- `--trust-proxy`: audit-log client addresses from `X-Forwarded-For` sent by a local proxy.
- Console: "New repository" on the organisation page; an empty repository shows how to clone it,
  make the first change or bring in a git repository; a new token shows ready-to-copy CLI and MCP
  setup.

### Changed
- SQLite commits are synced to disk (`synchronous=FULL`) so an acknowledged land survives power
  loss; `--sqlite-synchronous normal` restores the previous behaviour. `file://` object stores
  fsync every object and its directory (ADR-034).
- `--metrics` also serves `/healthz` and `/readyz`; gRPC health checks need no token and report
  NOT_SERVING when the registry or a data directory fails, and from the start of shutdown.
- SIGTERM drains calls in progress and stops the webhook worker before exiting.
- Platform servers limit each account to 128 calls in progress (`--max-calls-per-principal`)
  and request messages to 64 MiB (`--max-request-mb`), keep at most 256 repositories open
  (`--max-open-repos`, `--repo-idle-minutes`), and page repository listings.
- Webhooks: deliveries run concurrently, two at a time per webhook, 30 s each; delivery is at
  least once (dedupe by `X-Pando-Delivery`); finished deliveries are deleted after 30 days.
- The audit log is written to one file per UTC day and kept `--audit-retention-days` (365).
- `--web-origins` is now `--cors-origin` (the old name still works); `pando mcp --http` takes
  `--allowed-origin` and stops on SIGTERM.
- The registry records its schema version and applies ordered migrations.
- Console UX pass: a first visit to a server with sign-in pages opens sign-in instead of the demo;
  set-up asks for the organisation's name and derives its short name; the full repository header
  shows on Overview only; gold marks landings only (dark-theme buttons, selection and unread marks
  use Fishlake blue); form errors read as sentences.

### Fixed
- Console: the clone and mount commands name the repository (`SERVER/OWNER/NAME`) on servers that
  host many; Home shows an error instead of loading forever when the server is unreachable or
  refuses the token; Issues breadcrumbs; Escape closes the phone navigation drawer; wide tables no
  longer widen the page on phones; imported git authors show their name without the email; the
  Connect page's "remember" checkbox is visible.

### Security
- Production hardening (ADR-034): metric labels only for methods the protocol defines (no
  unbounded series from made-up paths); internal errors return an id, not SQL or paths; the
  registry survives a panic while locked; inbox, events, issue and repository listings scan a
  bounded number of rows and never open every repository; archived repositories refuse writes;
  `Locks/List` shows only paths the caller may read; the MCP gateway refuses browser origins unless allowed; the
  free-tier cloud-init no longer trusts a demo console origin; a stored object that fails its
  hash check is repaired by pushing it again.
- Plans and limits (ADR-035): platform servers record each organisation's active agents (agents
  that landed a change in the calendar month), people, storage (measured hourly, with the month's
  peak) and private repositories. With `--multi-tenant`, organisations' limits are enforced:
  suspended organisations and those over storage can read but not write, new agents beyond the
  active-agent limit can't land while active ones keep landing, and seats and private
  repositories are checked when added. Defaults come from `--default-org-limits`. New RPCs
  `GetUsage`, `SetLimits`, `SetSuspended`, `GetCapabilities` and `GetLicense`; commands
  `pando org usage` and `pando admin limits show|set|suspend|restore`; a **Usage & plan** tab in
  the console with a Change plan button linking to `--billing-url`, and a banner on every page of
  a suspended organisation linking to `--support-url`.
- Support licences: `pandod server --license FILE` and `pandod license verify FILE` check an
  Ed25519-signed licence offline and report it; nothing is turned off without one.

### Changed
- The sign-in pages (`/login/*`: sign in, check your email, create your account, CLI approval,
  invitations, errors) carry the console's brand: the Pando mark, its palette in light and dark,
  labelled fields, a clear primary action and plain error messages, still self-contained under
  `default-src 'none'`.

## [3.4.0] - 2026-10-18

### Added
- `pandod server --multi-tenant` (`PANDO_MULTI_TENANT`) for servers that strangers share:
  internal repositories are for the owning organisation's members, members and grants must name
  existing accounts, and any person may create up to `--max-orgs-per-account` (default 3)
  organisations, in a namespace shared with logins and with service names reserved (ADR-032).
- Browser sign-in for platform servers (`pandod server --login-listen`): Google and any OpenID
  Connect provider (`--oidc ID=ISSUER`), and single-use email links (`--smtp-url`,
  `--mail-from`, optional Cloudflare Turnstile). A sign-in creates a 30-day platform token for
  the console; first sign-ins create an account if `--signup` allows (default: a waitlist,
  managed with `pando admin signup allow|remove|list`). `pando admin identity link` lets an
  existing account, such as the first administrator, sign in from a browser (ADR-031).

### Changed
- Looking up one organisation reads only that organisation.

### Security
- Protected branches: a change's author can no longer approve it through an agent they mint,
  approve an agent's change they answer for, or report its required checks; such approvals don't
  count (ADR-030).
- Single sign-on: `--oidc-require CLAIM=VALUE` binds tokens to your tenant, and pandod refuses to
  start with GitHub Actions or Google as issuer without one. Tokens can't sign in as a platform
  administrator (unless listed with `--oidc-admin`) or as an `agent/` or `bot/` account. Unknown
  signing keys refetch the key set at most once a minute.
- Paths named `.git` or `.pando` in any case, with trailing dots or spaces, as an NTFS stream or
  as an 8.3 short name (`GIT~1`) are refused by the server, by checkouts and by imports, so a
  writer can't plant git hooks or config in other people's checkouts. Existing history holding
  such paths can no longer be read.
- Static tokens that are empty, shorter than 16 characters or a placeholder (`CHANGE-ME`) are
  refused at startup.
- Access answers are no longer re-cached from a read that raced a write, so a removed member or
  grant loses access at once.
- `pando migrate`: the GitHub token comes from `PANDO_GITHUB_TOKEN` (or `GITHUB_TOKEN`) or
  `--github-token-file`; `--github-token` warns that it is visible to other users. Source URLs
  are printed without credentials, and the mirror clone goes to a private, randomly named
  directory.

- Platform servers refuse repository calls that name no repository. Before, such a call reached
  the server's own `--metadata` repository with no access check, so any signed-in caller (or any
  token, however limited) could read and write it. A non-UTF-8 `x-pando-repo` header is refused.
- Webhooks no longer call loopback, private networks (RFC 1918, 100.64.0.0/10, fc00::/7) or cloud
  metadata (including 100.100.100.200, fd00:ec2::254 and IPv4 written as IPv6: mapped, NAT64,
  6to4). Start with `--webhook-allow-private` to let a self-hosted server call its internal
  network (metadata stays refused). The address is checked when a webhook is saved and at every
  delivery, deliveries connect only to the checked address (no DNS rebinding), DNS lookups time
  out after 5 s, proxies from the environment are ignored, and a failed delivery or ping reports
  only the HTTP status or "unreachable", never why.
### Added
- `pandod backup` snapshots a platform server's registry and every repository's database to a
  directory or object store while it runs; `docs/deploy.md` documents the restore.

### Changed
- Docs: a platform server (`--platform`) is single-node; only servers without it scale out.
- Go SDK: requires Go 1.26.9 and uses `golang.org/x/net` 0.60.0 and `golang.org/x/text`
  0.42.0, fixing six known HTTP/2 and text-parsing vulnerabilities (GO-2026-6603, 6610, 6611,
  6612, 6617, 6629).
- Releases attach their signed SLSA provenance (`*.intoto.jsonl`) next to the archives.
- CI hardening: least-privilege workflow tokens, every action pinned to a commit, CodeQL on
  every change, and Dependabot for Go, npm and Python as well as Rust and Actions.

## [3.3.0] - 2026-10-17

### Security
- On platform servers a change is attributed to whoever made it: a member can no longer record
  someone else as the author. Repository administrators may still record other authors, so
  imports and `pando migrate` keep the original history. Single-repository servers keep git's
  free-form authors.

### Fixed
- Platform servers no longer slow landing down: access answers are cached per process (cleared
  by every write that changes them) and push events are prepared off the landing path only when
  a webhook wants them. With 200 agents on one branch, p99 latency through a hosted repository
  went from 2.8–3.8 s to 0.22–0.25 s, matching a server without the platform (ADR-029).

### Added
- `pando migrate FROM --to SERVER/OWNER/NAME` (ADR-028, `docs/migrate.md`): moves a repository
  from GitHub, GitLab, Bitbucket or any git host with its full history on every branch; from
  GitHub (and GitHub Enterprise Server with `--github-api`), open pull requests become change
  requests and issues come with comments, labels and state. It prints the `pando mirror`
  command that keeps the old host current while you move.
- Issues (ADR-027): open, discuss, label and assign issues to people or agents, in the console
  (Issues tab), the CLI (`pando issue`) and over MCP (`pando_issues`, `pando_issue`,
  `pando_open_issue`, `pando_comment_on_issue`, `pando_update_issue`). Issues and change
  requests share numbers; a change request saying "fixes #N" closes the issue when it lands.
  Issues assigned to you are in your inbox; `issue` webhook events let an agent runner start work
  as soon as an issue is assigned.
- Remote MCP (ADR-026): `pando mcp --http ADDR --server URL` serves a whole server to agents
  anywhere over the stateless MCP 2026-07-28 protocol. With a URL and a token an agent reads,
  writes and lands files, opens, reviews and lands change requests, reports checks and reads its
  inbox, without a checkout; each request's token is passed through, so the server's
  permissions, token limits and audit log apply.
- An agent's change request asks the person the agent answers to for review; change requests
  nobody was asked to review show in the inbox of everyone who may approve them.
- Changesets created without an author are attributed to the authenticated caller.
- Webhooks (ADR-025, `docs/webhooks.md`): `push`, `change_request`, `review`, `comment` and
  `check` events posted to URLs and signed like GitHub's (`X-Pando-Signature-256`), retried
  with backoff from a queue that is safe across replicas. `pando webhook add|list|ping|log|remove`
  and repository Settings → Webhooks in the console.
- `pando land` on a protected branch opens a change request from a `review/` branch instead of
  failing. The console can open a change request from any branch.

### Added
- Change requests and review on platform servers (ADR-024): open a change request from a branch,
  comment on lines with suggestions, approve or request changes, report CI checks, and land it as
  one changeset once nothing blocks it; `pando cr rebase` replays it on its target. Protected
  branches (`pando protect`, or repository Settings in the console) require approvals and passing
  checks, make approvals stale on a new push, and refuse direct lands except for those allowed to
  bypass review. An inbox (review requested, changes requested, agents' changes ready) and an
  activity feed. CLI: `pando cr create|list|view|comment|review|check|land|rebase|ready|close|reopen`.
- The web console's review screens work against a platform server, and admins protect branches
  from repository Settings.

## [3.2.0] - 2026-10-17

### Added
- Accounts, organisations, members, teams, per-repository roles and access tokens on platform
  servers (ADR-023): `pando org`, `pando team`, `pando repo grant` and `pando token`, and the
  same in the web console (Tokens, organisation People, repository Settings → Access). Tokens
  start with `pando_`, are stored as a hash, can act as an agent, be limited to repositories and
  a highest role, and expire within a year.
- `pandod token --platform URL --account LOGIN` creates the first administrator's token
  without a running server.

### Changed
- Platform servers now check every repository call: reading needs the Read role and anything
  else Write; repositories are private to their organisation unless made internal or public.
  After upgrading from 3.1, add people with `pando org member ORG LOGIN ROLE`. Servers without
  any authentication stay open, for local development.

## [3.1.0] - 2026-10-17

### Added
- Console review comments on code: a "+" on every line, shift-click for a range of lines, a
  Markdown editor with preview, suggested changes shown as a small diff of current and proposed
  lines, ⌘/Ctrl+Enter to send, Escape to cancel, and resolved conversations collapsed.
- `pando mirror --to URL [--every SECONDS]`: keeps GitHub, GitLab, Bitbucket or any git
  remote up to date with a branch. Incremental, deterministic commits, never force-pushes,
  refuses to overwrite commits it did not make, and redacts credentials in messages.
- One server, many repositories: `pandod server --platform REGISTRY --repos-metadata TEMPLATE
  [--repos-objects TEMPLATE] --platform-admins NAMES`. Each repository has its own storage and
  landing queues; clients address `SERVER/OWNER/NAME`; calls without a repository reach the
  default one (ADR-022).
- `pando repo create OWNER/NAME` and `pando repo list`.
- The web console lists a server's repositories and shows only what the server supports.

### Changed
- `pandod server` and `pandod cache` listen on 127.0.0.1 by default; listening on a network is a
  deliberate `--listen 0.0.0.0:7420`.

### Fixed
- Console: the phone code view cut off line numbers and the first characters of each line;
  long titles no longer truncate to one line on phones; keys typed in a form never trigger
  page shortcuts.
- The web console kept one data cache across connections, so data from the demo or a previous
  server could appear after switching. Each connection now has its own cache.

## [3.0.0] - 2026-10-17

### Added
- The web console (`console/`): browse code with blame and history, follow changes as they land
  with word-level diffs, see who and which agent landed what on a live fleet timeline, find files
  from a command menu, and see locks. Works with any `pandod`; a demo of the full platform (several
  repositories, change requests, reviews, a land gate and rebase) runs in the browser. See
  docs/console.md.
- `pandod` accepts gRPC-Web from browsers (`web` feature flag, on by default) and allows the
  origins in `--web-origins` / `PANDO_WEB_ORIGINS` to call it.
- The platform contract, `pando_platform.proto`: organisations, repositories, accounts and agents,
  tokens, change requests, reviews, comments, protected branches (ADR-021).

### Changed
- **Security (breaking):** without `PANDO_TOKENS` or an OIDC issuer, `pandod` refuses to listen on
  a non-loopback address unless `--allow-anonymous` / `PANDO_ALLOW_ANONYMOUS=1` is set, so a server
  is never open to a network by accident. The Docker image therefore needs a token:
  `docker run -e PANDO_TOKENS=me:SECRET ...`.
- Request limits: each call may run at most `--request-timeout` (600 s); beyond `--max-in-flight`
  (4,096) calls in progress, new calls are refused with Unavailable instead of queueing; 256
  concurrent calls per connection.
- A new mark: the letter P drawn as a branch landing back on main.

## [2.4.0] - 2026-10-17

### Added
- `pando mount` on macOS and Windows. macOS serves a read-only NFSv3 view on localhost and
  mounts it with the system `mount_nfs`: no kernel extension, nothing to install, no admin
  rights. Windows uses the built-in Projected File System, loaded at run time. Both are
  tested in CI against the real kernels, file by file against `pando clone` (ADR-020).
- `pando mount --serve-only` on any Unix prints the mount command instead of mounting.
- `PANDO_LOG=debug` turns on client-side logs.

## [2.3.0] - 2026-10-17

### Added
- File locking for files that cannot be merged: `pando lock`, `pando unlock [--force]`,
  `pando locks`. A land that changes a file someone else has locked is refused. Locks
  are compare-and-swap system refs (ADR-019), need write access, are audited, and sit
  behind the `locks` feature flag.

### Fixed
- A land refused by a pre-land check or a lock showed as a garbled storage error on the
  client; it now reads `rejected: <reason>`.

## [2.2.0] - 2026-10-16

### Added
- `pandod copy`: back up, restore, or move a repository between backends (SQLite,
  Postgres, CockroachDB; disk, S3, GCS, Azure). Consistent while the source serves,
  incremental when repeated, keeps compacted objects compact and verifies whole objects.
  Storage ports gain paged listing, covered by the adapter contract tests.
- `--audit-log FILE`: an audit trail as JSON lines. One record per authenticated request
  (principal, client address, method, status, duration), per rejected request (address and
  reason, never the token), and per land, branch move, content read and history query,
  with paths and results. Off by default.
- Coverage-guided fuzzing (cargo-fuzz) of every decoder of untrusted bytes: objects,
  trees, changesets, chunk manifests, deltas and paths, with a seed corpus of valid
  inputs. Ten minutes per target nightly, one minute on pull requests.

## [2.1.3] - 2026-10-16

### Fixed
- Two replicas landing the same retried change could both apply it, leaving it in history
  twice. A change rebased onto a tip that already holds all of its changes is now reported
  as landed there instead of becoming an empty commit (as `git rebase` drops commits that
  become empty).

### Added
- `bench/chaos.sh`: three replicas on Postgres behind a connection-cutting proxy, killed
  and restarted while clients land; checks the landing invariants through the public API.
  Runs nightly with the simulation.
- Documentation: how Pando is tested.

## [2.1.2] - 2026-10-16

### Fixed
- A land retried after a lost reply could apply a rebased change a second time. Each
  rebased change now records the id it landed as, and a retry reports that landing.
- A batch flush that failed halfway could leave a derived record naming a tree or
  changeset that was never written; a later land of the same changeset then failed with
  "not found". Staged writes now go out objects first, then changesets, then derived data.
- One transient storage error failed every change in a land batch. Batches now retry
  storage errors (up to four times, with backoff) from the current head.

### Added
- A deterministic simulation of the landing protocol with fault injection, replica
  crashes and conflicting changes (`crates/pando-server/tests/simulation.rs`, ADR-018):
  200 seeds in every CI run, 50,000 new seeds nightly. It found the three bugs above.

## [2.1.1] - 2026-10-16

### Added
- A documentation site (mdBook, with search) at kabirnarang39.github.io/pando/docs:
  getting started, coming from git, deploying, and AI agents and SDKs, alongside the
  architecture, design decisions and benchmarks.
- New logo, README, landing page and social preview. Charts and diagrams are generated
  as light and dark SVGs by `scripts/charts.py`.
- `bench/ci_checkout_load.py`: concurrent CI checkouts on one machine, for git and Pando.

### Fixed
- Checkouts that write many files in parallel could fail with `File exists (os error 17)`
  when two writers created the same new directory at once (seen in about 3% of concurrent
  CI checkouts of git/git's `t/`).
- The MCP tools still used the pre-rename prefix: `sap_cat`, `sap_land` and the others are
  now `pando_cat`, `pando_land` and so on. MCP clients discover tool names, so agents pick
  up the new names on restart.
- Commands that work without a workspace (`cat`, `ls`, `log`, ...) suggested a `--server`
  flag they do not have; the error now points to `PANDO_SERVER`.

### Security
- Release archives get signed SLSA build provenance and an SPDX SBOM through GitHub
  artifact attestations (verify with `gh attestation verify`); container images carry
  provenance and SBOM attestations. See SECURITY.md.
- OpenSSF Scorecard runs weekly and on every push to `main`.

## [2.1.0] - 2026-10-16

### Added
- `examples/land_load.rs` and `bench/git_push_load.py`: landing throughput with N
  concurrent agents, for Pando and git.

### Fixed
- `bench/concurrent_land.py` failed after the rename; README links to the benchmark page
  pointed at a file that still had the old name.
- `pandod compact` on SQLite no longer leaves a write-ahead log as large as the database
  behind after VACUUM; on git/git that was 392 MB next to a 388 MB database.

### Performance
- Concurrent lands to the same branch are batched: the server rebases everything queued as
  one chain and moves the branch with one compare-and-swap. With 200 agents landing at once
  on one machine, throughput went from 10 lands/s with 237 "too contended" errors to 1,012
  lands/s with none; 1,000 agents sustain 504 lands/s (git: 0.2/s). A conflicting change
  fails without affecting the rest of its batch.
- The rebase conflict check walks only the changeset's own paths instead of diffing
  everything that landed since its base.
- SQLite writes are group-committed by one writer thread instead of one transaction per
  write behind a mutex.
- `pandod compact` is incremental: finished changesets are recorded and later runs process
  only what landed since, instead of rescanning all history.
- Compaction looks up paths in a cache of parsed trees instead of re-reading and re-parsing
  every directory level from storage for each changed file.
- Compaction caches delta chains instead of re-reading a base's whole chain from storage
  for every pair.
- SQLite runs without memory-mapped I/O and with a 64 MB page cache per connection: under
  concurrent writes SQLite remapped the file on every read transaction. On the Linux
  kernel's history, compaction went from 34-47 to 68 changesets/s and import from 78 to 89
  commits/s; clone and log times are unchanged.

## [2.0.0] - 2026-10-15

### Changed (breaking)
- **Sapwood is now Pando.** Named after the Pando aspen grove: some 47,000 trunks that share
  one root system. What changed, and what to do:
  - Binaries: `sap` → `pando`, `sapwood` → `pandod`, `git-remote-sapwood` → `git-remote-pando`
    (git URLs `pando::https://…`).
  - Environment variables: `SAPWOOD_*` → `PANDO_*`; `SAP_SERVER`, `SAP_TOKEN`, `SAP_AUTHOR`,
    `SAP_OBJECTS` → `PANDO_SERVER`, `PANDO_TOKEN`, `PANDO_AUTHOR`, `PANDO_OBJECTS`.
  - Workspaces live in `.pando/` instead of `.sap/`: clone existing workspaces again.
  - Wire protocol package `sapwood.v1` → `pando.v1`: upgrade clients and servers together.
  - Packages: crates `pando-vcs` (CLI), `pando-core`, `pando-adapters`, `pando-proto`,
    `pando-server`; PyPI `pando-vcs` (import `pando`); npm `pando-vcs`; Go module
    `github.com/kabirnarang39/pando/sdk/go/v2`; image `ghcr.io/kabirnarang39/pando`;
    Homebrew `kabirnarang39/tap/pando`.
- **Stored data carries over unchanged**: object ids, changesets and SQLite databases are
  identical, and Postgres/CockroachDB tables named `sapwood_*` are renamed on first start.

### Fixed
- `pando import-git` accepts merges that list the same parent twice, which git allows and
  the Linux kernel's history contains four of; the repeated parent is imported once.

## [1.9.0] - 2026-10-15

### Added
- **Delta storage** (`sapwood compact`, flag `delta-storage`): blob and tree versions are
  stored as zstd patch-from deltas against the previous version of the same path. On
  git/git (82k commits) server storage drops from 4.6 GB to 369 MB (git's pack: 333 MB).
  Single-writer compaction behind a lease, base pinning (chains never change after use, no
  cycles), verification before every replace, hash-verified reconstruction on every read.
  Wire protocol unchanged.
- `ObjectStore::replace` on memory, SQLite, Postgres and `object_store` backends (contract-tested).
- Install with Homebrew: `brew install kabirnarang39/tap/sapwood`.
- Release workflow publishes to crates.io (`sapwood`, `sapwood-server` and the library
  crates), PyPI (`sapwood`), npm (`sapwood-client`) and the Homebrew tap, each enabled by a
  repository variable. CI builds every crate from its packaged tarball.

### Changed
- `sap refs` / the Refs API hide system refs (`_sapwood/...`).
- The client crate is now named `sapwood` (binaries `sap` and `git-remote-sapwood` are
  unchanged): `cargo install sapwood`. The protocol files moved to
  `crates/sapwood-proto/proto/` so the crate can be published.

### Compatibility
- A store compacted by this version cannot be read by v1.8.x or older servers (they do not
  understand delta payloads). Upgrade every server before running `sapwood compact`.

### Fixed
- Linux release binaries are built on Ubuntu 22.04 and need glibc 2.35 or newer (Debian 12,
  Ubuntu 22.04, RHEL 10); they previously needed glibc 2.39.
- The release container image is assembled from the release binaries instead of compiling
  arm64 under emulation, which took over an hour and failed for v1.6.0. Images for earlier
  tags can be rebuilt with the Release workflow's manual trigger.

## [1.8.0] - 2026-10-15

### Added
- **Pre-land checks** (`sapwood server --land-hook URL [--land-hook-token T]
  [--land-hook-timeout SECS]`, flag `land-checks`): every land is POSTed as JSON to a CI gate,
  code review service or policy bot, and proceeds only on a 2xx answer. The response body is
  shown to the author as the reason; timeouts and unreachable hooks refuse the land. The check
  runs once before any server-side rebase, which happens only without path conflicts, so the
  approved changes are exactly what lands. New `LandCheck` port for in-process checks.

## [1.7.0] - 2026-10-15

### Added
- **Virtual checkouts on Linux** (`sap mount DIR [--rev main] [--objects URL]`, flag
  `virtual-checkout`): mounts a revision read-only through FUSE. Directories load when first
  listed, file sizes come from object headers without downloading content, and reads of a
  large file fetch only the chunks covering the requested range. Every object is hash-verified.

### Changed
- New objects record their uncompressed size in the zstd frame header, so a file's size is
  known without decompressing it. Ids and the storage format are unchanged; older objects are
  still read (and sized by decompressing).

## [1.6.0] - 2026-10-15

### Added
- **Daemon prefetch** (`sapwood daemon --server URL --prefetch main [--prefetch-path P]
  [--prefetch-interval SECS]`, flag `daemon-prefetch`): warms a node's cache with a revision
  in the background and keeps it warm as the branch moves; already-cached objects cost
  nothing. On git/git, a new node's first full checkout went from 4.2 s to 0.35 s.

## [1.5.0] - 2026-10-15

### Added
- **SDKs for Python, Go and TypeScript** (`sdk/`): read files, list directories, search, log,
  diff, blame, refs and merge-base without a clone, with BLAKE3 verification of changesets and
  file contents. Tested in CI against a live server.
- `proto/sapwood_objects.proto`: the canonical object encodings, published for third-party
  tools; a test proves it is byte-identical to the core's encoding.

## [1.4.0] - 2026-10-15

### Added
- **Path-scoped read permissions**: `read` grants in the access rules. Restricted principals
  are denied raw object reads and get path-filtered views instead: streaming `Checkout`, filtered
  `ListDir`/`Find`, diffs limited to readable paths, and redacted history. New `Access.WhoAmI`
  lets `sap clone/sync` switch to path-scoped checkout automatically. Rules without `read`
  keep full read access for everyone.

## [1.3.0] - 2026-10-15

### Added
- **Large files**: files of 8 MiB or more are stored as FastCDC chunks plus a manifest under
  the file's own id, removing the 256 MiB file limit. Status hashing, commit upload, checkout
  and sync all stream; chunks dedupe across files and versions; the server verifies uploaded
  manifests against the file id and clients verify reassembled content end to end. Large git
  blobs are chunked on `import-git` and on `git push` through the bridge.
- `sap commit` reports how many new objects were uploaded.

### Performance
- Tiered storage is now write-around and forwards batched writes upstream intact (one
  SQLite transaction per batch instead of per object): a 1 GiB commit went from 29 s to 14 s,
  and large uploads no longer evict the server's hot read cache.

## [1.2.0] - 2026-10-15

### Added
- **SSO via OpenID Connect** (`--oidc-issuer`, `--oidc-audience`, `--oidc-claim`, flag `oidc`):
  JWT bearer tokens verified against the issuer's JWKS with discovery and automatic key
  rotation. Asymmetric algorithms only, algorithm must match the key, `iss`/`aud`/`exp`
  required. The principal claim feeds per-directory and per-ref permissions. Static tokens
  and OIDC can be used together.

## [1.1.0] - 2026-10-15

### Added
- **Two-way git bridge**: `git-remote-sapwood` lets plain git clone, fetch, pull and push
  against a Sapwood server (`git clone sapwood::http://host:7420`). Fetches are incremental;
  pushes land server-side, so a push to a branch that moved is rebased instead of rejected
  (`SAPWOOD_PUSH=strict` restores git's behaviour). Feature flag `git-bridge`. Verified by
  cloning git/git's 24,466 mainline commits through plain `git clone` (136 s, identical tree).
- Project website (GitHub Pages) and a checksum-verifying `install.sh`.

## [1.0.1] - 2026-10-15

### Fixed
- `sap commit` failed with "`x` is both a file and a directory" when a file was replaced by a
  directory of the same name (or the reverse) in one commit. The redundant delete is now
  folded into the upsert that replaces it. Found by the compaction test suite; covered by
  `file_and_directory_swaps_commit_cleanly`.

## [1.0.0] - 2026-10-15

First stable release. The `sapwood.v1` wire protocol, storage formats and `sap` CLI are
covered by SemVer from this release on.

### Server (`sapwood`)
- Content-addressed object store: BLAKE3-256 ids, zstd-compressed payloads, verified on every write and every tier hop.
- Server-side changesets: clients send changed paths; the server builds Merkle trees (cost proportional to the change).
- Generic derived-data framework: root trees, generation numbers, skew-binary jump pointers (O(log n) ancestry).
- Lock-free landing: server-side rebase with path-level conflict detection plus compare-and-swap ref updates; no merge queue.
- Server-side history: log (path-filtered), blame, diff, merge-base, file browsing and path search, all without a client checkout.
- Tiered object serving: identical `Objects` RPC on server, regional cache (`sapwood cache`) and node daemon (`sapwood daemon`).
- Storage adapters: memory, SQLite (WAL), PostgreSQL / CockroachDB, and S3 / GCS / Azure / local disk via `object_store`.
- Security: bearer-token auth with principals, TLS, per-directory write ACLs and per-ref ACLs (JSON rules).
- Operations: feature flags for every feature, Prometheus metrics, JSON logs, gRPC health and reflection, graceful shutdown.

### Client (`sap`)
- `clone` (lazy, sparse with `--path`), `status`, `commit`, `land`, `sync`, `checkout [-b]`, `log`, `diff`, `show`, `cat`, `ls`, `find`, `blame`, `refs`, `merge-base`.
- git-compatible aliases: `push` → `land`, `pull` → `sync`, `st`, `ci`, `switch`, `branch`.
- `import-git` (deterministic, idempotent, incremental) and `export-git` (bit-identical trees; submodules are not imported).
- `sap mcp`: Model Context Protocol server so AI agents can read, commit and land natively.
- Parallel status hashing, batched and pipelined object fetches, symlink-safe working-copy writes.

[2.3.0]: https://github.com/kabirnarang39/pando/releases/tag/v2.3.0
[2.2.0]: https://github.com/kabirnarang39/pando/releases/tag/v2.2.0
[2.1.3]: https://github.com/kabirnarang39/pando/releases/tag/v2.1.3
[2.1.2]: https://github.com/kabirnarang39/pando/releases/tag/v2.1.2
[2.1.1]: https://github.com/kabirnarang39/pando/releases/tag/v2.1.1
[2.1.0]: https://github.com/kabirnarang39/pando/releases/tag/v2.1.0
[2.0.0]: https://github.com/kabirnarang39/pando/releases/tag/v2.0.0
[1.9.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.9.0
[1.8.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.8.0
[1.7.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.7.0
[1.6.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.6.0
[1.5.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.5.0
[1.4.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.4.0
[1.3.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.3.0
[1.2.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.2.0
[1.1.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.1.0
[1.0.1]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.0.1
[1.0.0]: https://github.com/kabirnarang39/sapwood/releases/tag/v1.0.0
