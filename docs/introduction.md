<div class="gd-hero">
<div class="gd-hero-copy">
<h1 class="gd-hero-title">One root. Thousands of agents.</h1>
<p class="gd-lede">Grovedock is where people and AI agents build software together, built on gd,
an open source-control protocol. Commits, rebases and history run on the server, so hundreds of
agents land on one branch at once with no merge queue, a clone fetches only the files you ask
for, and CI reads files without checking anything out.</p>
</div>
<svg class="gd-trunk-art" viewBox="0 0 260 300" aria-hidden="true" focusable="false"><path class="t-branch" d="M60 250 C60 205 118 200 130 170"/><path class="t-branch" d="M205 230 C205 180 140 160 130 128"/><path class="t-branch" d="M48 150 C48 112 116 104 130 86"/><path class="t-main" d="M130 292 V36"/><circle class="t-person" cx="60" cy="250" r="7"/><rect class="t-agent" x="198" y="223" width="14" height="14" rx="2"/><rect class="t-agent" x="41" y="143" width="14" height="14" rx="2"/><circle class="t-node" cx="130" cy="214" r="5"/><circle class="t-node" cx="130" cy="170" r="5"/><circle class="t-node" cx="130" cy="128" r="5"/><circle class="t-node" cx="130" cy="86" r="5"/><circle class="t-tip" cx="130" cy="36" r="9"/><text class="t-label" x="150" y="40">newest landing</text><text class="t-label" x="60" y="276" text-anchor="middle">a person</text><text class="t-label" x="205" y="258" text-anchor="middle">an agent</text><text class="t-label" x="48" y="178" text-anchor="middle">an agent</text></svg>
</div>

Grovedock was called Pando until October 2026 (`pando` became `gd` in 4.0). A grove is many trees
on one root, like many agents on one history; a dock is where their changes land.

## Start here

<ul class="gd-paths">
<li><a href="getting-started.html"><span class="gd-path-title">Get started</span><span class="gd-path-text">Sign up free, install gd, bring a git repository and land your first change.</span></a></li>
<li><a href="agents.html"><span class="gd-path-title">Run agents on it</span><span class="gd-path-text">Give each agent an address and a token over MCP. It reads, writes and lands without a clone.</span></a></li>
<li><a href="mirror.html"><span class="gd-path-title">Beside GitHub</span><span class="gd-path-text">Mirror every landed change to GitHub, GitLab or Bitbucket, or move history, pull requests and issues across in one command.</span></a></li>
<li><a href="orgs.html"><span class="gd-path-title">Organisations and access</span><span class="gd-path-text">People, teams, roles, protected branches and reviews.</span></a></li>
<li><a href="reference/protocol.html"><span class="gd-path-title">The gd protocol</span><span class="gd-path-text">Every call in <code>gd.v1</code>, for clients and servers in any language. Open source.</span></a></li>
<li><a href="limits.html"><span class="gd-path-title">Plans and limits</span><span class="gd-path-text">Free, Team and Enterprise, and what happens at each limit.</span></a></li>
</ul>

Also: [migrate from GitHub](migrate.md) and the [CLI reference](reference/cli.md).

## How it differs from git

| | git | Grovedock |
|---|---|---|
| Clone | downloads all history | downloads one tree, lazily, only the paths you ask for |
| Commit | built locally, pushed as packs | the server builds it from the list of changed files |
| Concurrent pushes to `main` | one wins, the rest fetch, rebase and retry | the server rebases changes that touch different files and lands them all ([how](landing.md)) |
| Reading a file in CI | clone first | one call: `gd cat main path` |
| `log`, `blame`, `diff` | need local history | server APIs; ancestry in O(log n) |
| Integrity | SHA-1 | BLAKE3, verified at every cache tier |

Where git wins: working offline with full history. Grovedock needs the server for history,
commits and landing. The [benchmarks](comparisons/git-vs-gd.md) give the measured numbers,
the method and the limits.

Plain git keeps working against Grovedock through `git-remote-gd`; see
[Coming from git](git-users.md).
