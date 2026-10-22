//! `gd mcp --http`: a remote Model Context Protocol endpoint for a whole Grovedock server, so an
//! agent anywhere (Claude, ChatGPT, Gemini, Cursor, your own) can read, change, review and land
//! code with nothing but a URL and a token: no clone, no workspace (ADR-026).
//!
//! It is a stateless gateway. Each request's `Authorization: Bearer` token is passed through to
//! the server, so the agent can do exactly what the token allows: every permission check,
//! token limit and audit record of the server applies, and the gateway holds no credentials.
//! MCP 2026-07-28 is stateless too (an older client's `initialize` is answered without a
//! session), so any number of gateways can run behind a load balancer.

use std::sync::Arc;

use axum::http::request::Parts;
use grovedock_core::object::{Changeset, FileChange, Kind, Mode, encode_object};
use grovedock_proto::{
    AddCommentRequest, ChangeRequest, ChangeRequestRef, ChangeRequestState, Check, CheckState,
    CreateChangeRequestRequest, InboxRequest, ListChangeRequestsRequest, ListRepositoriesRequest, ReportCheckRequest,
    RepositoryRef, SubmitReviewRequest, Verdict,
};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::features::{browse, diff};
use crate::remote::{GrpcRemote, Remote};

/// Largest file an agent may write in one call, and most files per call.
const MAX_FILE: usize = 4 << 20;
const MAX_FILES: usize = 200;

fn default_rev() -> String {
    String::new()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RepoArg {
    /// The repository, OWNER/NAME.
    pub repo: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListRepos {
    /// Only this owner's repositories.
    #[serde(default)]
    pub owner: String,
    /// Words to look for in names and descriptions.
    #[serde(default)]
    pub query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RepoPath {
    /// The repository, OWNER/NAME.
    pub repo: String,
    /// A branch or changeset id; empty: the default branch.
    #[serde(default = "default_rev")]
    pub rev: String,
    /// Repository-relative path; empty for the root.
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindArgs {
    pub repo: String,
    #[serde(default = "default_rev")]
    pub rev: String,
    /// Substring matched against whole paths.
    pub pattern: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArgs {
    pub repo: String,
    /// The branch; empty: the default branch.
    #[serde(default = "default_rev")]
    pub rev: String,
    /// Words (3+ characters) or "phrases", all of which a file must contain; narrow with
    /// path:dir/, -path:dir/ and lang:rs.
    pub query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SymbolArgs {
    pub repo: String,
    #[serde(default = "default_rev")]
    pub rev: String,
    /// The function, type or class name, or how it starts.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HistoryArgs {
    pub repo: String,
    #[serde(default = "default_rev")]
    pub rev: String,
    /// Only changes touching this path.
    #[serde(default)]
    pub path: String,
    /// At most this many (default 20, at most 200).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiffArgs {
    pub repo: String,
    /// Older revision: a branch or changeset id.
    pub from: String,
    /// Newer revision.
    pub to: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileWrite {
    /// Repository-relative path.
    pub path: String,
    /// The file's new content; omit (null) to delete the file.
    pub content: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteArgs {
    pub repo: String,
    /// The branch to land on. A new branch starts from `from`.
    pub branch: String,
    /// Where a new branch starts; empty: the default branch.
    #[serde(default)]
    pub from: String,
    /// Files to write or delete, all landed as one change.
    pub files: Vec<FileWrite>,
    /// What the change does and why.
    pub message: String,
    /// If the branch is protected, open a change request for review instead of failing
    /// (default true).
    #[serde(default)]
    pub propose: Option<bool>,
    /// The model making the change, e.g. claude-opus-5-5: recorded with it, so people see
    /// which model wrote what.
    #[serde(default)]
    pub model: String,
    /// The issue this change is for, e.g. 42: recorded with it and shown on the issue.
    #[serde(default)]
    pub task: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CrListArgs {
    pub repo: String,
    /// open (default), landed, closed or all.
    #[serde(default)]
    pub state: String,
    /// Only this author; `agent/*` for every agent.
    #[serde(default)]
    pub author: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CrArg {
    pub repo: String,
    /// The change request's number.
    pub number: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PathsArg {
    /// The repository, OWNER/NAME.
    pub repo: String,
    /// The files or folders you'll change, like `db/migrations` or `src/api/users.rs`.
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SettledFile {
    pub path: String,
    /// The file's whole content, settled: no conflict markers left.
    #[serde(default)]
    pub content: String,
    /// Settle the clash by deleting the file instead.
    #[serde(default)]
    pub delete: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResolveArgs {
    pub repo: String,
    pub number: u64,
    /// Every file `gd_conflicts` says clashes, settled.
    pub files: Vec<SettledFile>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenCrArgs {
    pub repo: String,
    /// The branch with the work.
    pub source: String,
    /// Where it should land; empty: the default branch.
    #[serde(default)]
    pub target: String,
    pub title: String,
    /// Why, and how to check it (Markdown).
    #[serde(default)]
    pub body: String,
    /// Logins to ask for a review.
    #[serde(default)]
    pub reviewers: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommentArgs {
    pub repo: String,
    pub number: u64,
    /// Markdown.
    pub body: String,
    /// For a line comment: the file.
    #[serde(default)]
    pub path: String,
    /// For a line comment: the line in the new version (1-based).
    #[serde(default)]
    pub line: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewArgs {
    pub repo: String,
    pub number: u64,
    /// approve, request_changes or comment.
    pub verdict: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CheckArgs {
    pub repo: String,
    pub number: u64,
    /// The check's name, e.g. `build`.
    pub name: String,
    /// pending, passed, failed or skipped.
    pub state: String,
    #[serde(default)]
    pub summary: String,
    /// A link to the run (http or https).
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Nothing {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IssuesArgs {
    pub repo: String,
    /// open (default), closed or all.
    #[serde(default)]
    pub state: String,
    /// Only issues assigned to this login.
    #[serde(default)]
    pub assignee: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IssueArg {
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReleaseNotesArgs {
    pub repo: String,
    /// The version being released, e.g. v1.4.0; empty: just say what fits.
    #[serde(default)]
    pub tag: String,
    /// Where it would be released: a branch or changeset id (default: the default branch).
    #[serde(default)]
    pub at: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateReleaseArgs {
    pub repo: String,
    /// The version, e.g. v1.4.0 (`gd_release_notes` suggests one). Made at `at` if new; tags never move.
    pub tag: String,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub title: String,
    /// Notes in Markdown; empty: written from what landed.
    #[serde(default)]
    pub notes: String,
    /// Publish now. By default an agent's release is a draft, for a person to check and publish.
    #[serde(default)]
    pub publish: bool,
    #[serde(default)]
    pub prerelease: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenIssueArgs {
    pub repo: String,
    pub title: String,
    /// What and why (Markdown).
    #[serde(default)]
    pub body: String,
    /// People or agents to work on it.
    #[serde(default)]
    pub assignees: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IssueCommentArgs {
    pub repo: String,
    pub number: u64,
    pub body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IssueUpdateArgs {
    pub repo: String,
    pub number: u64,
    /// open, completed or not_planned; empty to leave as is.
    #[serde(default)]
    pub state: String,
    /// Replace the assignees with these; omit to leave them.
    #[serde(default)]
    pub assignees: Option<Vec<String>>,
}

type ToolResult = Result<String, String>;

/// How long ago a time was, as an agent reads it: "12 minutes ago", "3 hours ago", "2 days ago".
fn ago(ms: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let m = (now - ms).max(0) / 60_000;
    match m {
        0 => "just now".into(),
        1..60 => format!("{m} minutes ago"),
        60..2880 => format!("{} hours ago", m / 60),
        _ => format!("{} days ago", m / 1440),
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn status(s: tonic::Status) -> String {
    s.message().to_owned()
}

/// The remote MCP server: one per gateway, shared by every request.
#[derive(Clone)]
pub struct RemoteMcp {
    server: Arc<String>,
}

fn bearer(parts: &Parts) -> Result<String, String> {
    parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned)
        .ok_or_else(|| "send your Grovedock token as `Authorization: Bearer gd_…`".to_owned())
}

fn split(repo: &str) -> Result<RepositoryRef, String> {
    let (owner, name) = repo.split_once('/').ok_or("give the repository as OWNER/NAME")?;
    Ok(RepositoryRef { owner: owner.into(), name: name.into() })
}

fn state_name(s: i32) -> &'static str {
    match ChangeRequestState::try_from(s).unwrap_or(ChangeRequestState::Unspecified) {
        ChangeRequestState::Open => "open",
        ChangeRequestState::Draft => "draft",
        ChangeRequestState::Landed => "landed",
        ChangeRequestState::Closed => "closed",
        ChangeRequestState::Unspecified => "unknown",
    }
}

/// A change request as an agent reads it: what it is, who said what, and what blocks it.
fn describe(cr: &ChangeRequest) -> String {
    let mut out = format!(
        "#{} {} [{}]\n{} wants to land {} into {}: {} files, +{} -{}\n",
        cr.number,
        cr.title,
        state_name(cr.state),
        cr.author,
        cr.source,
        cr.target,
        cr.files,
        cr.additions,
        cr.deletions
    );
    if !cr.body.trim().is_empty() {
        out.push_str(&format!("\n{}\n\n", cr.body.trim()));
    }
    for r in &cr.reviews {
        let v = match Verdict::try_from(r.verdict).unwrap_or(Verdict::Unspecified) {
            Verdict::Approved => "approved",
            Verdict::ChangesRequested => "requested changes",
            _ => "commented",
        };
        let stale = if r.reviewed_tip != cr.source_tip { " (an older version)" } else { "" };
        out.push_str(&format!("review: {} {v}{stale}\n", r.reviewer));
    }
    for c in &cr.checks {
        out.push_str(&format!(
            "check: {} {:?}\n",
            c.name,
            CheckState::try_from(c.state).unwrap_or(CheckState::Pending)
        ));
    }
    if !cr.conflicts.is_empty() {
        out.push_str(&format!("conflicts: {}\n", cr.conflicts.join(", ")));
    }
    if cr.state == ChangeRequestState::Open as i32 || cr.state == ChangeRequestState::Draft as i32 {
        if cr.blockers.is_empty() {
            out.push_str("ready to land: call gd_land_change_request\n");
        } else {
            for b in &cr.blockers {
                out.push_str(&format!("blocked: {b}\n"));
            }
        }
    }
    out
}

impl RemoteMcp {
    pub fn new(server: impl Into<String>) -> Self {
        Self { server: Arc::new(server.into().trim_end_matches('/').to_owned()) }
    }

    /// The caller's view of one repository, with the caller's own token.
    fn repo(&self, parts: &Parts, repo: &str) -> Result<GrpcRemote, String> {
        split(repo)?;
        GrpcRemote::connect(&format!("{}/{repo}", self.server), None, Some(&bearer(parts)?)).map_err(err)
    }

    /// The server's platform and review services, with the caller's own token.
    fn server(&self, parts: &Parts) -> Result<GrpcRemote, String> {
        GrpcRemote::connect(&self.server, None, Some(&bearer(parts)?)).map_err(err)
    }

    /// `rev`, or the repository's default branch when empty.
    async fn rev(&self, parts: &Parts, repo: &str, rev: &str) -> Result<String, String> {
        if !rev.is_empty() {
            return Ok(rev.to_owned());
        }
        let r = self.server(parts)?.platform().get_repository(split(repo)?).await.map_err(status)?.into_inner();
        Ok(if r.default_branch.is_empty() { "main".into() } else { r.default_branch })
    }

    /// Serves MCP over streamable HTTP at `/mcp` until the process stops.
    /// Browsers are refused unless their `Origin` is in `allowed_origins` (agents send none), so
    /// a web page can't drive the gateway with a visitor's network access.
    pub async fn serve_http(
        self,
        listen: std::net::SocketAddr,
        allowed_hosts: Vec<String>,
        allowed_origins: Vec<String>,
    ) -> anyhow::Result<()> {
        use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
        use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
        let mut config = StreamableHttpServerConfig::default();
        config.json_response = true;
        // Older clients (before 2026-07-28) start with `initialize`; answer it statelessly too
        // instead of trying to open a session, which this gateway never keeps.
        config.legacy_session_mode = false;
        if !allowed_hosts.is_empty() {
            config.allowed_hosts = allowed_hosts;
        }
        config = if allowed_origins.is_empty() {
            config.enforce_origin_validation()
        } else {
            config.with_allowed_origins(allowed_origins)
        };
        let me = self.clone();
        let service =
            StreamableHttpService::new(move || Ok(me.clone()), Arc::new(NeverSessionManager::default()), config);
        let app =
            axum::Router::new().nest_service("/mcp", service).route("/healthz", axum::routing::get(|| async { "ok" }));
        let listener = tokio::net::TcpListener::bind(listen).await?;
        eprintln!("gd MCP on http://{listen}/mcp, for {}", self.server);
        axum::serve(listener, app).with_graceful_shutdown(crate::shutdown_signal()).await?;
        Ok(())
    }
}

#[tool_router(server_handler)]
impl RemoteMcp {
    #[tool(
        description = "List the repositories you can read on this Grovedock server, with what landed in the last day."
    )]
    async fn gd_repositories(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ListRepos>,
    ) -> ToolResult {
        let list = self
            .server(&parts)?
            .platform()
            .list_repositories(ListRepositoriesRequest { owner: a.owner, query: a.query, ..Default::default() })
            .await
            .map_err(status)?
            .into_inner();
        Ok(list
            .repositories
            .iter()
            .map(|r| {
                format!(
                    "{}/{} ({} landed today, {} open reviews) {}",
                    r.owner, r.name, r.lands_24h, r.open_change_requests, r.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(description = "Read a file from a repository at a branch or changeset, without a checkout.")]
    async fn gd_read_file(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<RepoPath>,
    ) -> ToolResult {
        let rev = self.rev(&parts, &a.repo, &a.rev).await?;
        let bytes = browse::cat(&self.repo(&parts, &a.repo)?, &rev, &a.path).await.map_err(|e| format!("{e:#}"))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tool(description = "List a directory (d = directory, f = file, x = executable, l = link).")]
    async fn gd_list_directory(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<RepoPath>,
    ) -> ToolResult {
        let rev = self.rev(&parts, &a.repo, &a.rev).await?;
        let entries = browse::ls(&self.repo(&parts, &a.repo)?, &rev, &a.path).await.map_err(|e| format!("{e:#}"))?;
        Ok(entries.iter().map(|e| format!("{} {}", browse::mode_char(e.mode), e.name)).collect::<Vec<_>>().join("\n"))
    }

    #[tool(description = "Find files whose path contains a substring.")]
    async fn gd_find_files(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<FindArgs>,
    ) -> ToolResult {
        let rev = self.rev(&parts, &a.repo, &a.rev).await?;
        Ok(self.repo(&parts, &a.repo)?.find(&rev, &a.pattern, 1000).await.map_err(|e| format!("{e:#}"))?.join("\n"))
    }

    #[tool(
        description = "Search the code for text: every word (3+ characters) or \"phrase\", narrowed with path:dir/, -path:dir/, lang:rs. Answers path:line: text, from the server's index (no clone)."
    )]
    async fn gd_search_code(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<SearchArgs>,
    ) -> ToolResult {
        let rev = self.rev(&parts, &a.repo, &a.rev).await?;
        let r = self
            .repo(&parts, &a.repo)?
            .code_search()
            .search_code(grovedock_proto::SearchCodeRequest {
                query: a.query,
                r#ref: rev,
                limit: 30,
                page_token: String::new(),
            })
            .await
            .map_err(status)?
            .into_inner();
        if r.hits.is_empty() {
            return Ok("nothing matches".into());
        }
        let mut out = String::new();
        for h in &r.hits {
            for l in &h.lines {
                out.push_str(&format!("{}:{}: {}\n", h.path, l.line, l.text.trim_end()));
            }
            if h.more > 0 {
                out.push_str(&format!("{}: …and {} more line(s)\n", h.path, h.more));
            }
        }
        if r.files as usize > r.hits.len() {
            out.push_str(&format!("({} of about {} files; narrow with path: or lang:)\n", r.hits.len(), r.files));
        }
        Ok(out)
    }

    #[tool(
        description = "Where a function, type, class or constant is defined: kind, path:line and the defining line."
    )]
    async fn gd_find_symbol(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<SymbolArgs>,
    ) -> ToolResult {
        let rev = self.rev(&parts, &a.repo, &a.rev).await?;
        let r = self
            .repo(&parts, &a.repo)?
            .code_search()
            .find_symbol(grovedock_proto::FindSymbolRequest { name: a.name, r#ref: rev, limit: 30 })
            .await
            .map_err(status)?
            .into_inner();
        if r.symbols.is_empty() {
            return Ok("no definitions by that name".into());
        }
        Ok(r.symbols
            .iter()
            .map(|s| format!("{}:{}: {} {}  {}", s.path, s.line, s.kind, s.name, s.text))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(description = "What landed, newest first: id, author and summary, optionally for one path.")]
    async fn gd_history(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<HistoryArgs>,
    ) -> ToolResult {
        let rev = self.rev(&parts, &a.repo, &a.rev).await?;
        let log = browse::log(&self.repo(&parts, &a.repo)?, &rev, &a.path, a.limit.unwrap_or(20).min(200))
            .await
            .map_err(|e| format!("{e:#}"))?;
        Ok(log
            .iter()
            .map(|c| {
                format!(
                    "{} {} | {}",
                    c.id.short(),
                    c.changeset.author,
                    c.changeset.message.lines().next().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(
        description = "Who last changed each line of a file, and in which change: a person, or an agent with its model."
    )]
    async fn gd_blame(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<RepoPath>) -> ToolResult {
        let rev = self.rev(&parts, &a.repo, &a.rev).await?;
        let lines = self.repo(&parts, &a.repo)?.blame(&rev, &a.path).await.map_err(|e| format!("{e:#}"))?;
        Ok(lines
            .iter()
            .enumerate()
            .map(|(i, (id, l, who))| format!("{} {:<28} {:>5} {l}", id.short(), who, i + 1))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(description = "Unified diff between two revisions (branches or changeset ids).")]
    async fn gd_diff(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<DiffArgs>) -> ToolResult {
        diff::revisions(&self.repo(&parts, &a.repo)?, &a.from, &a.to, false).await.map_err(|e| format!("{e:#}"))
    }

    #[tool(description = "Write or delete files and land them on a branch as one change, without a checkout. \
        Concurrent changes to other files are merged on the server. If the branch is protected, the change \
        goes to a new review/ branch and a change request is opened instead (unless propose is false).")]
    async fn gd_write(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<WriteArgs>) -> ToolResult {
        if a.files.is_empty() || a.files.len() > MAX_FILES {
            return Err(format!("write between 1 and {MAX_FILES} files per change"));
        }
        if a.message.trim().is_empty() {
            return Err("say what the change does in `message`".into());
        }
        let r = self.repo(&parts, &a.repo)?;
        let from = self.rev(&parts, &a.repo, &a.from).await?;
        let tip = r.get_ref(&a.branch).await.map_err(|e| format!("{e:#}"))?;
        let parent = match tip {
            Some(t) => Some(t),
            None => match r.get_ref(&from).await.map_err(|e| format!("{e:#}"))? {
                Some(base) => Some(base),
                // The first change in an empty repository starts its default branch.
                None if a.branch == from => None,
                None => return Err(format!("no branch `{from}` to start from; write to `{from}` first")),
            },
        };
        let mut objects = Vec::new();
        let mut changes = Vec::new();
        for f in &a.files {
            match &f.content {
                Some(text) if text.len() > MAX_FILE => return Err(format!("{} is larger than 4 MB", f.path)),
                Some(text) => {
                    let (id, payload) = encode_object(Kind::Blob, text.as_bytes());
                    objects.push((id, payload));
                    changes.push(FileChange::upsert(f.path.trim_matches('/'), Mode::File, id));
                }
                None => changes.push(FileChange::delete(f.path.trim_matches('/'))),
            }
        }
        r.objects().put_many(objects).await.map_err(|e| format!("{e:#}"))?;
        let cs = r
            .create_changeset_declared(
                Changeset {
                    parents: parent.iter().map(|p| p.0.to_vec()).collect(),
                    changes,
                    author: String::new(), // the server records who landed it
                    message: a.message.trim().to_owned(),
                    timestamp_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0),
                    git: None,
                },
                &a.model,
                "",
                a.task.unwrap_or(0),
            )
            .await
            .map_err(|e| format!("{e:#}"))?
            .id;
        let landed = match tip {
            None => r.compare_and_swap(&a.branch, None, cs).await.map(|ok| ok.then_some(cs)),
            Some(_) => r.land(&a.branch, cs).await.map(|(id, _)| Some(id)),
        };
        match landed {
            Ok(Some(id)) => Ok(format!("landed {} on {} as {}", a.files.len(), a.branch, id.short())),
            Ok(None) => Err(format!("{} was created by someone else meanwhile; try again", a.branch)),
            Err(e) if format!("{e:#}").contains("is protected") && a.propose.unwrap_or(true) => {
                let branch = format!("review/{}", &cs.to_hex()[..12]);
                r.compare_and_swap(&branch, None, cs).await.map_err(|e| format!("{e:#}"))?;
                let (title, body) = a.message.trim().split_once('\n').unwrap_or((a.message.trim(), ""));
                let cr = self
                    .server(&parts)?
                    .reviews()
                    .create_change_request(CreateChangeRequestRequest {
                        repository: Some(split(&a.repo)?),
                        source: branch,
                        target: a.branch.clone(),
                        title: title.to_owned(),
                        body: body.trim().to_owned(),
                        ..Default::default()
                    })
                    .await
                    .map_err(status)?
                    .into_inner();
                Ok(format!("{} is protected, so this went for review.\n{}", a.branch, describe(&cr)))
            }
            Err(e) => Err(format!("{e:#}")),
        }
    }

    #[tool(description = "List change requests in a repository with what blocks each from landing.")]
    async fn gd_change_requests(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CrListArgs>,
    ) -> ToolResult {
        let states = match a.state.as_str() {
            "" | "open" => vec![ChangeRequestState::Open as i32, ChangeRequestState::Draft as i32],
            "landed" => vec![ChangeRequestState::Landed as i32],
            "closed" => vec![ChangeRequestState::Closed as i32],
            "all" => vec![],
            other => return Err(format!("`{other}` is not a state: open, landed, closed or all")),
        };
        let list = self
            .server(&parts)?
            .reviews()
            .list_change_requests(ListChangeRequestsRequest {
                repository: Some(split(&a.repo)?),
                states,
                author: a.author,
                ..Default::default()
            })
            .await
            .map_err(status)?
            .into_inner();
        if list.change_requests.is_empty() {
            return Ok("no change requests".into());
        }
        Ok(list
            .change_requests
            .iter()
            .map(|c| {
                let why = if c.blockers.is_empty() { "ready".to_owned() } else { c.blockers.join("; ") };
                format!("#{} [{}] {} by {} ({why})", c.number, state_name(c.state), c.title, c.author)
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(description = "One change request: description, reviews, checks, conflicts and what blocks it.")]
    async fn gd_change_request(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CrArg>,
    ) -> ToolResult {
        let cr = self
            .server(&parts)?
            .reviews()
            .get_change_request(ChangeRequestRef { repository: Some(split(&a.repo)?), number: a.number })
            .await
            .map_err(status)?
            .into_inner();
        Ok(describe(&cr))
    }

    #[tool(description = "Open a change request proposing a branch for review and landing.")]
    async fn gd_open_change_request(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<OpenCrArgs>,
    ) -> ToolResult {
        let cr = self
            .server(&parts)?
            .reviews()
            .create_change_request(CreateChangeRequestRequest {
                milestone: 0,
                repository: Some(split(&a.repo)?),
                source: a.source,
                target: a.target,
                title: a.title,
                body: a.body,
                draft: false,
                reviewers: a.reviewers,
                source_repository: None,
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(describe(&cr))
    }

    #[tool(description = "Comment on a change request, or on one line of one of its files.")]
    async fn gd_comment(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CommentArgs>,
    ) -> ToolResult {
        self.server(&parts)?
            .reviews()
            .add_comment(AddCommentRequest {
                r#ref: Some(ChangeRequestRef { repository: Some(split(&a.repo)?), number: a.number }),
                body: a.body,
                path: a.path,
                line: a.line,
                ..Default::default()
            })
            .await
            .map_err(status)?;
        Ok(format!("commented on #{}", a.number))
    }

    #[tool(description = "Review a change request: approve, request_changes or comment. You can't approve your own.")]
    async fn gd_review(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<ReviewArgs>) -> ToolResult {
        let verdict = match a.verdict.as_str() {
            "approve" | "approved" => Verdict::Approved,
            "request_changes" | "changes" => Verdict::ChangesRequested,
            "comment" => Verdict::Commented,
            other => return Err(format!("`{other}` is not a verdict: approve, request_changes or comment")),
        };
        let cr = self
            .server(&parts)?
            .reviews()
            .submit_review(SubmitReviewRequest {
                r#ref: Some(ChangeRequestRef { repository: Some(split(&a.repo)?), number: a.number }),
                verdict: verdict as i32,
                body: a.body,
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(describe(&cr))
    }

    #[tool(
        description = "Before starting work: who else is already changing these files or folders (open change requests, and file locks). Ask first and pick other work, or coordinate, instead of finding the clash when you land."
    )]
    async fn gd_who_is_changing(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<PathsArg>,
    ) -> ToolResult {
        let r = self
            .server(&parts)?
            .packets()
            .list_overlaps(grovedock_proto::OverlapsRequest {
                repository: Some(split(&a.repo)?),
                paths: a.paths.clone(),
                except: 0,
            })
            .await
            .map_err(status)?
            .into_inner();
        if r.items.is_empty() {
            return Ok(format!(
                "No one else is changing {} in {}: no open change request touches them, and none is locked.",
                a.paths.join(", "),
                a.repo
            ));
        }
        let mut out = format!(
            "{} other piece(s) of work touch these paths:
",
            r.items.len()
        );
        for o in &r.items {
            out.push_str(&match o.kind.as_str() {
                "lock" => format!(
                    "- {} is locked by {} ({}): changes to it are refused until they unlock it\n",
                    o.theirs,
                    o.who,
                    ago(o.since_ms)
                ),
                _ => format!("- #{} \"{}\" by {} changes {}\n", o.number, o.title, o.who, o.theirs),
            });
        }
        out.push_str(
            "Pick other work, or coordinate with them (gd_comment on their change request) before changing these.",
        );
        Ok(out)
    }

    #[tool(
        description = "Where a change request clashes with its target, file by file: the three versions (where they parted, the target now, the branch) and the merge with <<<<<<< target / ======= / >>>>>>> branch markers around each clash. Files whose edits combine aren't clashes: landing merges them."
    )]
    async fn gd_conflicts(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<CrArg>) -> ToolResult {
        let r = self
            .server(&parts)?
            .reviews()
            .conflicts(ChangeRequestRef { repository: Some(split(&a.repo)?), number: a.number })
            .await
            .map_err(status)?
            .into_inner();
        let clashing: Vec<_> = r.files.iter().filter(|f| !f.clean).collect();
        if clashing.is_empty() {
            return Ok(format!("#{} doesn't clash with {}: landing merges any files both changed", a.number, r.target));
        }
        let mut out = format!(
            "#{} clashes with {} in {} file(s). Settle each with gd_resolve_conflicts (the whole file, markers removed).\n",
            a.number,
            r.target,
            clashing.len()
        );
        for f in clashing {
            out.push_str(&format!("\n=== {} ({}) ===\n", f.path, f.kind));
            if f.merged.is_empty() {
                out.push_str(&format!(
                    "--- where they parted ---\n{}\n--- {} now ---\n{}\n--- the branch ---\n{}\n",
                    f.base, r.target, f.target, f.source
                ));
            } else {
                out.push_str(&f.merged);
            }
        }
        Ok(out)
    }

    #[tool(
        description = "Settle a change request's clashes: give each clashing file whole (markers removed) or delete it. The branch is replayed on the target's tip with them."
    )]
    async fn gd_resolve_conflicts(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ResolveArgs>,
    ) -> ToolResult {
        let cr = self
            .server(&parts)?
            .reviews()
            .resolve_conflicts(grovedock_proto::ResolveConflictsRequest {
                r#ref: Some(ChangeRequestRef { repository: Some(split(&a.repo)?), number: a.number }),
                files: a
                    .files
                    .into_iter()
                    .map(|f| grovedock_proto::FileResolution { path: f.path, content: f.content, delete: f.delete })
                    .collect(),
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(describe(&cr))
    }

    #[tool(
        description = "Make a change request land by itself, as you, once nothing blocks it (reviews, checks, conflicts); where the branch asks for it, the result is tested first and lands only if it passes. on: false turns it off."
    )]
    async fn gd_land_when_ready(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<LandWhenReadyArgs>,
    ) -> ToolResult {
        let cr = self
            .server(&parts)?
            .landing()
            .set_land_when_ready(grovedock_proto::SetLandWhenReadyRequest {
                r#ref: Some(ChangeRequestRef { repository: Some(split(&a.repo)?), number: a.number }),
                on: a.on.unwrap_or(true),
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(if cr.land_when_ready_by.is_empty() {
            format!("#{} won't land by itself", cr.number)
        } else if cr.blockers.is_empty() {
            format!("#{} lands by itself now", cr.number)
        } else {
            format!("#{} lands by itself once these clear: {}", cr.number, cr.blockers.join("; "))
        })
    }

    #[tool(description = "Land a change request once nothing blocks it.")]
    async fn gd_land_change_request(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CrArg>,
    ) -> ToolResult {
        let out = self
            .server(&parts)?
            .reviews()
            .land(ChangeRequestRef { repository: Some(split(&a.repo)?), number: a.number })
            .await
            .map_err(status)?
            .into_inner();
        Ok(format!("landed #{} as {}", a.number, hex_short(&out.landed_as)))
    }

    #[tool(description = "Report a CI check (pending, passed, failed, skipped) on a change request's current version.")]
    async fn gd_report_check(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CheckArgs>,
    ) -> ToolResult {
        let state = match a.state.as_str() {
            "pending" => CheckState::Pending,
            "passed" => CheckState::Passed,
            "failed" => CheckState::Failed,
            "skipped" => CheckState::Skipped,
            other => return Err(format!("`{other}` is not a check state: pending, passed, failed or skipped")),
        };
        let cr = self
            .server(&parts)?
            .reviews()
            .report_check(ReportCheckRequest {
                r#ref: Some(ChangeRequestRef { repository: Some(split(&a.repo)?), number: a.number }),
                check: Some(Check {
                    name: a.name,
                    state: state as i32,
                    summary: a.summary,
                    details_url: a.url,
                    ..Default::default()
                }),
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(describe(&cr))
    }

    #[tool(description = "List issues in a repository: work to do, and who it is assigned to.")]
    async fn gd_issues(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<IssuesArgs>) -> ToolResult {
        use grovedock_proto::IssueState as S;
        let states = match a.state.as_str() {
            "" | "open" => vec![S::Open as i32],
            "closed" => vec![S::Completed as i32, S::NotPlanned as i32],
            "all" => vec![S::Open as i32, S::Completed as i32, S::NotPlanned as i32],
            other => return Err(format!("`{other}` is not a state: open, closed or all")),
        };
        let list = self
            .server(&parts)?
            .issues()
            .list_issues(grovedock_proto::ListIssuesRequest {
                repository: Some(split(&a.repo)?),
                states,
                assignee: a.assignee,
                ..Default::default()
            })
            .await
            .map_err(status)?
            .into_inner();
        if list.issues.is_empty() {
            return Ok("no issues".into());
        }
        Ok(list
            .issues
            .iter()
            .map(|i| {
                format!(
                    "#{} {}{}",
                    i.number,
                    i.title,
                    if i.assignees.is_empty() {
                        String::new()
                    } else {
                        format!(" (assigned to {})", i.assignees.join(", "))
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(
        description = "CI runs (Grovedock Runs) of a repository, newest first, optionally for one branch or change request: each run's workflow, state and, when it failed, the job and the first error line. Use gd_run_failure for the details."
    )]
    async fn gd_runs(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<RunsArgs>) -> ToolResult {
        let runs = self
            .server(&parts)?
            .runs()
            .list_runs(grovedock_proto::ListRunsRequest {
                repository: Some(split(&a.repo)?),
                r#ref: a.branch.clone(),
                change_request: a.change_request,
                limit: 20,
                ..Default::default()
            })
            .await
            .map_err(status)?
            .into_inner()
            .runs;
        if runs.is_empty() {
            return Ok("no runs (does the repository have .github/workflows or .grovedock/workflows?)".into());
        }
        Ok(runs.iter().map(run_line).collect::<Vec<_>>().join("\n"))
    }

    #[tool(
        description = "Why a run failed, for fixing it: each failed job's failing step, its error line, the log lines around the error, the files they mention, and whether the job is flaky (the same tree has also passed it, so the failure may not be the change's fault)."
    )]
    async fn gd_run_failure(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<RunArg>,
    ) -> ToolResult {
        let repo = split(&a.repo)?;
        let mut runs = self.server(&parts)?.runs();
        let run = runs
            .get_run(grovedock_proto::RunRef { repository: Some(repo.clone()), number: a.number })
            .await
            .map_err(status)?
            .into_inner();
        let mut out = vec![run_line(&run)];
        if run.state == grovedock_proto::RunState::Invalid as i32 {
            out.push(format!("The workflow file {} can't be read: {}", run.file, run.problem));
            return Ok(out.join("\n"));
        }
        let failed: Vec<&grovedock_proto::RunJob> =
            run.jobs.iter().filter(|j| j.state == grovedock_proto::RunState::Failed as i32).collect();
        if failed.is_empty() {
            out.push("No job failed.".into());
            return Ok(out.join("\n"));
        }
        for j in failed {
            out.push(String::new());
            out.push(format!("Job {}: {}", j.name, j.failure));
            if j.flaky {
                out.push("Flaky: this exact tree has also passed this job, so the failure may not be caused by the change. Rerun it (gd_rerun) before changing code.".into());
            }
            let log = runs
                .read_log(grovedock_proto::ReadLogRequest {
                    repository: Some(repo.clone()),
                    job_id: j.id,
                    offset: 0,
                    limit: 1 << 20,
                })
                .await
                .map_err(status)?
                .into_inner()
                .data;
            let failing = j.steps.iter().position(|s| s.state == grovedock_proto::RunState::Failed as i32);
            let (from, to) = match failing {
                Some(i) => (
                    j.steps[i].log_offset as usize,
                    j.steps
                        .get(i + 1)
                        .map_or(log.len(), |n| (n.log_offset as usize).max(j.steps[i].log_offset as usize)),
                ),
                None => (0, log.len()),
            };
            if let Some(i) = failing {
                out.push(format!("Failing step: {}", j.steps[i].name));
            }
            let text = String::from_utf8_lossy(&log[from.min(log.len())..to.min(log.len())]).into_owned();
            let excerpt = error_window(&text);
            let files = mentioned_files(&excerpt);
            if !files.is_empty() {
                out.push(format!("Files it mentions: {}", files.join(", ")));
            }
            out.push("Log:".into());
            out.push(excerpt);
        }
        Ok(out.join("\n"))
    }

    #[tool(
        description = "Run a run's failed jobs again (and what needs them), e.g. after a flaky failure. Needs write access."
    )]
    async fn gd_rerun(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<RunArg>) -> ToolResult {
        let run = self
            .server(&parts)?
            .runs()
            .rerun_run(grovedock_proto::RerunRequest {
                r#ref: Some(grovedock_proto::RunRef { repository: Some(split(&a.repo)?), number: a.number }),
                failed_only: true,
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(format!("rerunning: {}", run_line(&run)))
    }

    #[tool(description = "A repository's releases, newest first: tag, title, state and files.")]
    async fn gd_releases(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<RepoArg>) -> ToolResult {
        let list = self
            .server(&parts)?
            .releases()
            .list_releases(grovedock_proto::ListReleasesRequest {
                repository: Some(split(&a.repo)?),
                limit: 30,
                ..Default::default()
            })
            .await
            .map_err(status)?
            .into_inner();
        if list.releases.is_empty() {
            return Ok("no releases yet".into());
        }
        Ok(list
            .releases
            .iter()
            .map(|r| {
                let state = if r.draft {
                    " [draft]"
                } else if r.prerelease {
                    " [pre-release]"
                } else if r.latest {
                    " [latest]"
                } else {
                    ""
                };
                format!("{} {}{state} ({} files)", r.tag, r.title, r.assets.len())
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(
        description = "What landed since the latest release, written up as release notes, with the next version by semantic versioning. Read before gd_create_release."
    )]
    async fn gd_release_notes(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ReleaseNotesArgs>,
    ) -> ToolResult {
        let n = self
            .server(&parts)?
            .releases()
            .generate_release_notes(grovedock_proto::ReleaseNotesRequest {
                repository: Some(split(&a.repo)?),
                tag: a.tag,
                at: a.at,
                previous: String::new(),
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(format!(
            "suggested version: {}\nchanges since {}: {}\n\n{}",
            n.suggested_version,
            if n.previous_tag.is_empty() { "the start" } else { &n.previous_tag },
            n.changes,
            n.body
        ))
    }

    #[tool(
        description = "Make a release: a tag that never moves, with notes. A draft unless publish is true, so a person can check it first."
    )]
    async fn gd_create_release(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CreateReleaseArgs>,
    ) -> ToolResult {
        let mut c = self.server(&parts)?.releases();
        let repository = Some(split(&a.repo)?);
        let body = if a.notes.trim().is_empty() {
            c.generate_release_notes(grovedock_proto::ReleaseNotesRequest {
                repository: repository.clone(),
                tag: a.tag.clone(),
                at: a.at.clone(),
                previous: String::new(),
            })
            .await
            .map_err(status)?
            .into_inner()
            .body
        } else {
            a.notes
        };
        let r = c
            .create_release(grovedock_proto::CreateReleaseRequest {
                repository,
                tag: a.tag,
                at: a.at,
                title: a.title,
                body,
                draft: !a.publish,
                prerelease: a.prerelease,
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(format!(
            "{} {} {} at {}",
            if r.draft { "drafted" } else { "released" },
            r.tag,
            if r.draft { "(a person publishes it from the release page or `gd release publish`)" } else { "" },
            hex_short(&r.changeset)
        ))
    }

    #[tool(description = "Read an issue with its discussion, to start work on it.")]
    async fn gd_issue(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<IssueArg>) -> ToolResult {
        let mut c = self.server(&parts)?.issues();
        let r = grovedock_proto::IssueRef { repository: Some(split(&a.repo)?), number: a.number };
        let i = c.get_issue(r.clone()).await.map_err(status)?.into_inner();
        let comments = c.list_issue_comments(r).await.map_err(status)?.into_inner().comments;
        Ok(issue_text(&i, &comments))
    }

    #[tool(description = "Open an issue: work to do, optionally assigned to people or agents.")]
    async fn gd_open_issue(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<OpenIssueArgs>,
    ) -> ToolResult {
        let i = self
            .server(&parts)?
            .issues()
            .create_issue(grovedock_proto::CreateIssueRequest {
                milestone: 0,
                repository: Some(split(&a.repo)?),
                title: a.title,
                body: a.body,
                labels: a.labels,
                assignees: a.assignees,
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(issue_text(&i, &[]))
    }

    #[tool(description = "Comment on an issue.")]
    async fn gd_comment_on_issue(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<IssueCommentArgs>,
    ) -> ToolResult {
        self.server(&parts)?
            .issues()
            .add_issue_comment(grovedock_proto::AddIssueCommentRequest {
                r#ref: Some(grovedock_proto::IssueRef { repository: Some(split(&a.repo)?), number: a.number }),
                body: a.body,
            })
            .await
            .map_err(status)?;
        Ok(format!("commented on #{}", a.number))
    }

    #[tool(description = "Close, reopen or reassign an issue.")]
    async fn gd_update_issue(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<IssueUpdateArgs>,
    ) -> ToolResult {
        use grovedock_proto::IssueState as S;
        let mut fields = Vec::new();
        let state = match a.state.as_str() {
            "" => S::Unspecified,
            "open" => S::Open,
            "completed" | "done" => S::Completed,
            "not_planned" => S::NotPlanned,
            other => return Err(format!("`{other}` is not a state: open, completed or not_planned")),
        };
        if state != S::Unspecified {
            fields.push("state".to_owned());
        }
        if a.assignees.is_some() {
            fields.push("assignees".to_owned());
        }
        let i = self
            .server(&parts)?
            .issues()
            .update_issue(grovedock_proto::UpdateIssueRequest {
                r#ref: Some(grovedock_proto::IssueRef { repository: Some(split(&a.repo)?), number: a.number }),
                fields,
                state: state as i32,
                assignees: a.assignees.unwrap_or_default(),
                ..Default::default()
            })
            .await
            .map_err(status)?
            .into_inner();
        Ok(issue_text(&i, &[]))
    }

    #[tool(
        description = "What needs you: issues assigned to you, reviews requested from you, changes requested on yours, your agents' work ready to land."
    )]
    async fn gd_inbox(&self, Extension(parts): Extension<Parts>, Parameters(_): Parameters<Nothing>) -> ToolResult {
        let inbox = self.server(&parts)?.reviews().inbox(InboxRequest::default()).await.map_err(status)?.into_inner();
        if inbox.items.is_empty() {
            return Ok("nothing needs you".into());
        }
        Ok(inbox
            .items
            .iter()
            .filter_map(|i| {
                if let Some(c) = &i.change_request {
                    let repo = c.repository.as_ref().map(|r| format!("{}/{}", r.owner, r.name)).unwrap_or_default();
                    return Some(format!("{}: {repo}#{} {} by {}", i.reason, c.number, c.title, c.author));
                }
                let x = i.issue.as_ref()?;
                let repo = x.repository.as_ref().map(|r| format!("{}/{}", r.owner, r.name)).unwrap_or_default();
                Some(format!("{}: issue {repo}#{} {}", i.reason, x.number, x.title))
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn issue_text(i: &grovedock_proto::Issue, comments: &[grovedock_proto::Comment]) -> String {
    let state = match grovedock_proto::IssueState::try_from(i.state).unwrap_or(grovedock_proto::IssueState::Unspecified)
    {
        grovedock_proto::IssueState::Open => "open",
        grovedock_proto::IssueState::Completed => "done",
        grovedock_proto::IssueState::NotPlanned => "not planned",
        grovedock_proto::IssueState::Unspecified => "unknown",
    };
    let mut out = format!("#{} {} [{state}]\nopened by {}", i.number, i.title, i.author);
    if !i.assignees.is_empty() {
        out.push_str(&format!(", assigned to {}", i.assignees.join(", ")));
    }
    if !i.labels.is_empty() {
        out.push_str(&format!("\nlabels: {}", i.labels.join(", ")));
    }
    if !i.fixed_by.is_empty() {
        out.push_str(&format!(
            "\nfixed by change requests: {}",
            i.fixed_by.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(", ")
        ));
    }
    if !i.body.trim().is_empty() {
        out.push_str(&format!("\n\n{}", i.body.trim()));
    }
    for c in comments {
        out.push_str(&format!("\n\n{} commented:\n{}", c.author, c.body));
    }
    out.push_str(&format!(
        "\n\nTo fix it, open a change request whose description says \"Fixes #{}\": landing it closes the issue.",
        i.number
    ));
    out
}

fn hex_short(bytes: &[u8]) -> String {
    bytes.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LandWhenReadyArgs {
    /// The repository, OWNER/NAME.
    pub repo: String,
    /// The change request's number.
    pub number: u64,
    /// false turns it off; default on.
    #[serde(default)]
    pub on: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunsArgs {
    /// The repository, OWNER/NAME.
    pub repo: String,
    /// Only runs for this branch.
    #[serde(default)]
    pub branch: String,
    /// Only runs for this change request (its number).
    #[serde(default)]
    pub change_request: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunArg {
    /// The repository, OWNER/NAME.
    pub repo: String,
    /// The run's number.
    pub number: u64,
}

fn run_state(s: i32) -> &'static str {
    use grovedock_proto::RunState as S;
    match S::try_from(s).unwrap_or(S::Unspecified) {
        S::Queued => "queued",
        S::Running => "running",
        S::Passed => "passed",
        S::Failed => "failed",
        S::Cancelled => "cancelled",
        S::Skipped => "skipped",
        S::Waiting => "waiting for a maintainer to approve",
        S::Invalid => "invalid workflow file",
        S::Unspecified => "unknown",
    }
}

fn run_line(r: &grovedock_proto::Run) -> String {
    let on = if r.change_request > 0 { format!("change request #{}", r.change_request) } else { r.r#ref.clone() };
    let mut line = format!("#{} {} on {on}: {}", r.number, r.workflow, run_state(r.state));
    if !r.problem.is_empty() {
        line.push_str(&format!(" ({})", r.problem));
    }
    let jobs: Vec<String> = r
        .jobs
        .iter()
        .map(|j| {
            format!(
                "{} {}{}{}",
                j.name,
                run_state(j.state),
                if j.reused { " (same tree passed before)" } else { "" },
                if j.flaky { " (flaky)" } else { "" }
            )
        })
        .collect();
    if !jobs.is_empty() {
        line.push_str(&format!(" [{}]", jobs.join("; ")));
    }
    line
}

/// The lines around the first error in a step's output (or its end), for a reader that
/// can't scroll.
fn error_window(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("error") || l.contains("failed") || l.contains("panicked") || l.contains("assert")
        })
        .unwrap_or(lines.len().saturating_sub(40));
    let from = at.saturating_sub(15);
    let to = (at + 45).min(lines.len());
    lines[from..to].join("\n")
}

/// `path/to/file.ext` or `path/to/file.ext:LINE` mentions in a log excerpt.
fn mentioned_files(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || "()[]'\"`,".contains(c)) {
        let w = word.trim_start_matches("./").trim_end_matches([':', '.']);
        let path = w.split(':').next().unwrap_or("");
        let looks = path.contains('.')
            && path.contains('/')
            && !path.starts_with("http")
            && !path.starts_with('/')
            && path
                .rsplit('.')
                .next()
                .is_some_and(|e| (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()));
        if looks && !out.iter().any(|o| o == w) && out.len() < 10 {
            out.push(w.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn needs_a_bearer_token_and_owner_name() {
        let req = axum::http::Request::builder().header("authorization", "Bearer gd_abc").body(()).unwrap();
        let (parts, _) = req.into_parts();
        assert_eq!(bearer(&parts).unwrap(), "gd_abc");
        let (none, _) = axum::http::Request::builder().body(()).unwrap().into_parts();
        assert!(bearer(&none).is_err());
        assert!(split("lumen/fleet").is_ok());
        assert!(split("fleet").is_err());
    }

    #[test]
    fn failures_point_at_files_and_the_error() {
        let log = "Compiling app v0.1\nwarning: unused\nerror[E0308]: mismatched types\n  --> src/billing/invoice.rs:42:9\n   |\nsee https://doc.rust-lang.org/x.html\n";
        assert_eq!(mentioned_files(log), ["src/billing/invoice.rs:42:9"]);
        assert!(error_window(log).starts_with("Compiling app"));
        assert!(error_window(log).contains("mismatched types"));
    }
}
