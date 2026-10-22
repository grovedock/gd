# Roadmap

Two products, one protocol:

- **gd** (open source, Apache-2.0): the protocol (`gd.v1`), the `gd` command-line tool, the git
  bridge (`gd::` URLs) and the SDKs. Anyone can implement the protocol or build on it, like git.
- **Grovedock** (cloud.grovedock.com, and Enterprise on your own servers): the platform built on
  it: accounts, organisations, repositories, reviews, issues, agents, plans.

Roughly in order. `gd.v1` stays backward compatible within 4.x.

## Shipped in 4.0

The Grovedock name throughout (`gd`, `grovedockd`, `git-remote-gd`, `gd.v1`); open sign-up with
chosen usernames; Free, Team and Enterprise plans with limits on people, active agents,
storage, private repositories and calls a minute; payment by Razorpay that turns Team on at
once; onboarding from plan to first agent; activity for every land.

## Next

| Item | Product | Why |
|---|---|---|
| Credentials in the OS keychain | gd | `gd login` saves to macOS Keychain, Windows Credential Manager or the Secret Service, falling back to the 0600 file; `gd auth token` prints it for scripts (like git's credential helpers and `gh auth token`) |
| Explore, profiles, stars and forks | Grovedock | public repositories and communities, for open-source projects as much as companies |
| Code search across repositories | Grovedock | find anything without a checkout |
| Email notifications | Grovedock | review requested, assigned, mentioned, with per-person settings |
| Releases and tags pages | Grovedock | ship from where the code lives |
| Import a repository from GitHub in the browser | Grovedock | `gd migrate` without a terminal |
| Platform registry on Postgres | Grovedock | several `grovedockd` replicas for high availability |
| Path-history index | gd | `log -- path` walks first-parent history today |
| Writable virtual checkouts | gd | `gd mount` is read-only today |

## Later

SAML and SCIM, audit-log streaming, hosted CI runners, package registry, and regions outside
India, each when customers ask for them.
