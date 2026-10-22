# Prior art

Grovedock builds on ideas that large engineering organisations have described in public.
No code from any of these systems was used. This page credits them, records what each one
proved, and sets the bar Grovedock measures itself against.

| System | Organisation | Published scale | Ideas Grovedock shares | Open source? |
|---|---|---|---|---|
| Piper and Clients in the Cloud (CitC) | Google | ~1 billion files, ~35 million commits, 86 TB (2015) [^piper] | Server holds history; workspaces show the whole tree but store only what you change | No |
| Sapling, Mononoke, EdenFS | Meta | Repositories with millions of files and very long histories [^sapling] | Operations scale with the files you use, not the repository; lazy virtual checkouts | Client yes; Mononoke and EdenFS not supported for outside use [^sapling-readme] |
| VFS for Git (GVFS) and Scalar | Microsoft | Windows: ~3.5 million files, ~300 GB repository (2017) [^gvfs] | Fetch only what is used; virtualize the working directory | Yes (MIT), git-based |
| Redesigning source control | Anthropic | Tens of thousands of merges a day from humans and agents [^anthropic] | Server-side commits and rebases, lock-free landing, tiered caches, API-first reads | No |

## What Grovedock adds

* **Open source and self-hosted** under Apache-2.0: one binary on a laptop with SQLite, or
  stateless replicas on Postgres/CockroachDB and S3/GCS/Azure.
* **Agent-native interfaces**: a built-in MCP server and SDKs in Python, Go and TypeScript.
* **Plain git keeps working** through a two-way remote helper; import and export round-trip
  with identical trees, so there is no lock-in.
* **Pluggable everything**: every infrastructure piece is a port with a shared contract suite,
  and every source-control feature has a runtime flag.
* **Measured in public**: the benchmarks are reproducible from public data with the scripts in
  `bench/` and the examples in `crates/gd/examples/`.

## Where these systems are ahead

They run in production at a scale Grovedock has not yet been tested at, with years of
operating experience and integrated code review, search and build tooling. Grovedock is new.
The benchmarks in [docs/comparisons/git-vs-gd.md](comparisons/git-vs-gd.md) and
larger-scale runs still to come are how it earns comparison, one public dataset at a time.

[^piper]: Potvin and Levenberg, [*Why Google Stores Billions of Lines of Code in a Single Repository*](https://cacm.acm.org/research/why-google-stores-billions-of-lines-of-code-in-a-single-repository/), Communications of the ACM, July 2016. Figures from January 2015; the file count includes release branches and deleted files.
[^sapling]: Meta Engineering, [*Sapling: Source control that's user-friendly and scalable*](https://engineering.fb.com/2022/11/15/open-source/sapling-source-control/), November 2022.
[^sapling-readme]: [facebook/sapling README](https://github.com/facebook/sapling).
[^gvfs]: Brian Harry, [*The largest Git repo on the planet*](https://devblogs.microsoft.com/bharry/the-largest-git-repo-on-the-planet/), May 2017.
[^anthropic]: Anthropic Engineering, [*Redesigning source control at Anthropic*](https://www.anthropic.com/engineering/redesigning-source-control).
