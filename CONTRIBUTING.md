# Contributing to gd

Thanks for helping. gd aims to stay small, fast and correct, so contributions are
held to a few concrete rules.

## Setup

```sh
# Rust stable (see rust-version in Cargo.toml) and protoc
brew install protobuf        # or: apt-get install protobuf-compiler
cargo test --workspace       # unit, property, contract and end-to-end tests
```

Optional, for the database adapters:

```sh
docker run -d -p 55432:5432 -e POSTGRES_PASSWORD=gd -e POSTGRES_DB=gd postgres:17-alpine
GD_TEST_POSTGRES_URL=postgres://postgres:gd@localhost:55432/gd \
  cargo test -p grovedock-adapters --test contract
```

## Where code goes

* **Domain logic** → `crates/core`. No I/O, no transport.
* **Infrastructure** → a new adapter in `crates/adapters` implementing a port.
  It must pass `tests/contract.rs`; add your adapter to that file.
* **A server feature** → the Grovedock server is not open source; propose the protocol change
  (below) in an issue, and any server can implement it.
* **A client feature** → `crates/gd/src/features/<name>.rs`, returning data; printing
  belongs in `main.rs`.
* **Protocol changes** → `crates/proto/proto/` (`gd.proto`, `gd_platform.proto`, `gd_issues.proto`, `gd_releases.proto`). v1 is stable: add fields and RPCs, never
  renumber or change the meaning of existing ones.

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

* Every behaviour change comes with a test that fails without it.
* Performance-sensitive changes include before/after numbers (`cargo bench -p grovedock-core`
  or `bench/run.sh`, which runs gd against a Grovedock server).
* Keep commits focused; use [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `perf:`, `docs:`...).

## Tests for new functionality

Every new feature and every behaviour change lands with automated tests in the same pull
request: unit tests next to the code, and an end-to-end test in `crates/*/tests/` for anything
a user can run. A bug fix includes a test that fails without the fix. Changes are not merged
without them. See [How Grovedock is tested](https://grovedock.com/docs/testing.html).

## Questions and discussion

Ask questions and discuss ideas in
[GitHub Discussions](https://github.com/grovedock/gd/discussions); track bugs and
feature requests as [issues](https://github.com/grovedock/gd/issues). Both are public,
searchable and open to anyone with a GitHub account. Issues, pull requests and code comments
are in English.

## Reporting bugs

Open an issue with the `gd --version` and `grovedockd --version` output, the command, what
you expected and what happened. Security issues: see [SECURITY.md](https://github.com/grovedock/gd/blob/main/SECURITY.md).
