# Agents and MCP

## Remote MCP: any agent, no checkout

Grovedock serves its repositories to agents anywhere over the
[Model Context Protocol](https://modelcontextprotocol.io) (2026-07-28, and the earlier versions from 2024-11-05, all served statelessly)
at `https://cloud.grovedock.com/mcp`. An agent needs that address and a token, nothing else: it
reads, searches, writes and lands files, opens and reviews change requests, reports CI checks
and reads its inbox, without cloning anything.

Give each agent a token of its own (the web app's onboarding and Tokens page do the same):

```sh
gd token create claude --as agent/claude-01 --repo acme/web --role write --server https://cloud.grovedock.com
```

Connect it with the address and its token:

```sh
claude mcp add --transport http gd https://cloud.grovedock.com/mcp \
  --header "Authorization: Bearer $GD_TOKEN"                  # Claude Code
```

Claude Desktop, Cursor, ChatGPT, Gemini CLI and other clients that speak MCP over streamable
HTTP take the same URL and header. Clients on versions before 2026-07-28 start with
`initialize` as usual; the gateway answers it without keeping a session.

- **The token decides everything.** The gateway passes each request's token to the server and
  holds no credentials of its own, so an agent can do exactly what its token allows: limited to
  some repositories, a highest role, an expiry, every action in the audit log.
- **Protected branches still hold.** When an agent writes to one, the change goes to a new
  `review/` branch and a change request opens, asking the person the agent answers to.
- **Stateless:** run as many gateways as you like behind a load balancer; nothing is shared.

| Tool | What it does |
|---|---|
| `gd_repositories` | repositories you can read, with today's activity |
| `gd_read_file`, `gd_list_directory`, `gd_find_files` | read code at any branch or change |
| `gd_history`, `gd_blame`, `gd_diff` | who changed what, and when |
| `gd_write` | write or delete files and land them as one change (or open a change request on a protected branch) |
| `gd_change_requests`, `gd_change_request` | change requests and what blocks each |
| `gd_open_change_request`, `gd_comment`, `gd_review` | propose, discuss, approve or request changes |
| `gd_land_change_request` | land once nothing blocks it |
| `gd_report_check` | report a CI result on a change request |
| `gd_issues`, `gd_issue`, `gd_open_issue`, `gd_comment_on_issue`, `gd_update_issue` | work to do: read, open, discuss, close, assign |
| `gd_search_code`, `gd_find_symbol` | search the code (words, phrases, path:, lang:) and find where something is defined, from the server's index |
| `gd_conflicts`, `gd_resolve_conflicts` | where a change request clashes with its target (three versions and a marked merge per file), and settling it by giving the files: no clone needed. Files whose edits combine aren't clashes; landing merges them |
| `gd_releases`, `gd_release_notes`, `gd_create_release` | releases: what landed since the last one, the version that fits, and a draft for a person to publish (or `publish: true`) |
| `gd_inbox` | what needs you: assigned issues, requested reviews, changes requested on yours |

### The agent loop

1. Someone assigns an issue to an agent (`gd issue assign 12 agent/fix-01`, or in the
   console). An `issue` webhook can wake the agent's runner at once.
2. The agent calls `gd_inbox`, reads the issue with `gd_issue`, and writes the fix on a
   branch with `gd_write`.
3. It opens a change request saying "Fixes #12" with `gd_open_change_request`; the person it
   answers to is asked to review.
4. After approval and passing checks it lands with `gd_land_change_request`, and the issue
   closes itself.

## Local MCP, in a workspace

`gd mcp` serves Grovedock as [Model Context Protocol](https://modelcontextprotocol.io)
tools over stdio, so coding agents read, commit and land without shelling out:

```sh
claude mcp add gd -- gd mcp      # Claude Code; other MCP clients work the same way
```

Run it inside a workspace. The tools:

| Tool | What it does |
|---|---|
| `gd_cat` | read a file at any revision |
| `gd_ls` | list a directory at any revision |
| `gd_find` | find files whose path contains a string |
| `gd_log` | history, optionally for one path |
| `gd_blame` | which change last touched each line |
| `gd_diff` | diff between two revisions |
| `gd_status` | uncommitted changes in the workspace |
| `gd_commit` | create a changeset from workspace changes |
| `gd_land` | land committed work on the branch |
| `gd_sync` | update the workspace to the branch head |

## Many agents on one branch

Agents can all land on `main` at once. The server rebases changes that touch different
files and lands them in batches, so an agent never has to pull, rebase and retry. A change
that really conflicts is refused with the conflicting paths, and the branch is untouched.
On one laptop, 1,000 agents landing at once sustain about 500 changes a second; see
[Benchmarks](comparisons/git-vs-gd.md).

## What review checks for you

Every change request's page and the review queue list what a reviewer of an agent's change would
otherwise have to look for by hand:

- **Weakened CI:** tests deleted, skipped or switched off; failures let through. Needs a person's approval.
- **Untrusted text in workflows:** a pull request's or issue's title, text or branch name pasted
  straight into a command or prompt, where whoever writes it can run code with the workflow's
  token. Needs a person's approval. Also flagged: tokens with every permission, and
  `pull_request_target`.
- **Repeats:** a new function that looks like one the code already has (`fetchUserProfile` beside
  `getUserProfile`, `normaliseCoupon` beside `normalizeCoupon`), with where the old one is.
- **No test:** code changed and no test touched.
- **Scope:** a change spread over more than five areas of the code.

## Who wrote each line

Blame names who wrote every line: a person, or an agent with its model ("Claude Code · sonnet-4",
"agent/fix-bot · opus-5-5"), and the file says how much of it agents wrote. Where that comes from:

- **Changes made with gd.** The server records the account that uploaded each change, and what an
  agent says of itself: its model, its session and the issue it's on. `gd_write` takes `model`
  and `task`; from the command line, set `GD_MODEL`, `GD_SESSION` and `GD_TASK`. None of it goes
  in the change's message.
- **History from git.** A repository that used [git-ai](https://github.com/git-ai-project/git-ai)
  keeps its attribution: `gd migrate` reads its `refs/notes/ai` notes and keeps each with the
  change its commit became.
- **Anything else** shows only who made the change. That isn't a claim a person typed it.

Every line one agent wrote, in a file or a folder: `gd blame src --by agent/fix-01`.

Attribution travels with git. `gd export-git` and `gd mirror` write git-ai notes
(`refs/notes/ai`): changes that came from git get their original notes back, and agents' changes
made with gd get notes saying which lines they added, as which agent and model. The mirror pushes
them beside the branch.

This is a record of what was declared, for review and audit. It is not a legal attestation.

## SDKs

Read files, list directories, search and inspect history from your own tools, without a
clone. Every client verifies content against its BLAKE3 id.

| Language | Package | Example |
|---|---|---|
| Python | [`sdk/python`](https://github.com/grovedock/gd/tree/main/sdk/python) | `Client(url).read_file("src/lib.rs")` |
| TypeScript | [`sdk/typescript`](https://github.com/grovedock/gd/tree/main/sdk/typescript) | `await new Client(url).readFile("src/lib.rs")` |
| Go | [`sdk/go`](https://github.com/grovedock/gd/tree/main/sdk/go) | `c.ReadFile(ctx, "main", "src/lib.rs")` |

For any other language, generate a client from
[`gd.proto`](https://github.com/grovedock/gd/blob/main/crates/proto/proto/gd.proto).
Object encodings are in
[`gd_objects.proto`](https://github.com/grovedock/gd/blob/main/crates/proto/proto/gd_objects.proto).
