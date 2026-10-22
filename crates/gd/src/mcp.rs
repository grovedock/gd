//! `gd mcp`: Model Context Protocol server over stdio, so any MCP-capable agent (Claude
//! Code, Codex, Cursor, custom agents) can read, commit and land through Grovedock
//! natively. Built on the official Rust MCP SDK.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServiceExt, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::features::{browse, commit, diff, land, sync};
use crate::remote::Remote;
use crate::workspace::{ChangeKind, Workspace};

fn main_ref() -> String {
    "main".into()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RevPath {
    /// Ref name (e.g. `main`) or full changeset id.
    #[serde(default = "main_ref")]
    pub rev: String,
    /// Repository-relative path, `""` for the root.
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LogArgs {
    #[serde(default = "main_ref")]
    pub rev: String,
    /// Only changesets touching this path.
    #[serde(default)]
    pub path: String,
    /// Maximum entries (default 20).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindArgs {
    #[serde(default = "main_ref")]
    pub rev: String,
    /// Substring matched against full repository paths.
    pub pattern: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiffArgs {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommitArgs {
    pub message: String,
    /// Commit only these paths (default: all changes).
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Empty {}

#[derive(Clone)]
pub struct SapMcp {
    remote: Arc<dyn Remote>,
    workspace: Option<PathBuf>,
}

type ToolResult = Result<String, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

impl SapMcp {
    pub fn new(remote: Arc<dyn Remote>, workspace: Option<PathBuf>) -> Self {
        Self { remote, workspace }
    }

    fn ws(&self) -> Result<Workspace, String> {
        let dir = self.workspace.as_ref().ok_or("no workspace: start `gd mcp` inside a gd workspace")?;
        Workspace::find(dir).map_err(err)
    }

    pub async fn serve_stdio(self) -> anyhow::Result<()> {
        self.serve(rmcp::transport::stdio()).await?.waiting().await?;
        Ok(())
    }
}

#[tool_router(server_handler)]
impl SapMcp {
    #[tool(description = "Read a file at any revision without a checkout.")]
    async fn gd_cat(&self, Parameters(a): Parameters<RevPath>) -> ToolResult {
        let bytes = browse::cat(self.remote.as_ref(), &a.rev, &a.path).await.map_err(err)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tool(description = "List a directory at any revision (mode, id, name per line).")]
    async fn gd_ls(&self, Parameters(a): Parameters<RevPath>) -> ToolResult {
        let entries = browse::ls(self.remote.as_ref(), &a.rev, &a.path).await.map_err(err)?;
        Ok(entries
            .iter()
            .map(|e| format!("{} {} {}", browse::mode_char(e.mode), e.id.map_or("-".into(), |i| i.short()), e.name))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(description = "Find files whose path contains a substring, at any revision.")]
    async fn gd_find(&self, Parameters(a): Parameters<FindArgs>) -> ToolResult {
        Ok(self.remote.find(&a.rev, &a.pattern, 1000).await.map_err(err)?.join("\n"))
    }

    #[tool(description = "Changeset history (newest first), optionally limited to a path.")]
    async fn gd_log(&self, Parameters(a): Parameters<LogArgs>) -> ToolResult {
        let log = browse::log(self.remote.as_ref(), &a.rev, &a.path, a.limit.unwrap_or(20)).await.map_err(err)?;
        Ok(log
            .iter()
            .map(|c| format!("{} {} | {}", c.id, c.changeset.author, c.changeset.message.lines().next().unwrap_or("")))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(description = "Which changeset last changed each line of a file.")]
    async fn gd_blame(&self, Parameters(a): Parameters<RevPath>) -> ToolResult {
        let lines = self.remote.blame(&a.rev, &a.path).await.map_err(err)?;
        Ok(lines
            .iter()
            .enumerate()
            .map(|(i, (id, l, who))| format!("{} {:<28} {:>5} {l}", id.short(), who, i + 1))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(description = "Unified diff between two revisions (computed server-side).")]
    async fn gd_diff(&self, Parameters(a): Parameters<DiffArgs>) -> ToolResult {
        diff::revisions(self.remote.as_ref(), &a.from, &a.to, false).await.map_err(err)
    }

    #[tool(description = "Uncommitted changes in the workspace (A/M/D per line).")]
    async fn gd_status(&self, Parameters(_): Parameters<Empty>) -> ToolResult {
        let ws = self.ws()?;
        let changes = ws.status(&[]).map_err(err)?;
        if changes.is_empty() {
            return Ok("clean".into());
        }
        Ok(changes
            .iter()
            .map(|c| {
                let t = match c.kind {
                    ChangeKind::Added => "A",
                    ChangeKind::Modified => "M",
                    ChangeKind::Deleted => "D",
                };
                format!("{t} {}", c.path)
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(description = "Commit workspace changes as a server-side changeset (not yet landed).")]
    async fn gd_commit(&self, Parameters(a): Parameters<CommitArgs>) -> ToolResult {
        let mut ws = self.ws()?;
        match commit::run(self.remote.as_ref(), &mut ws, &a.message, &crate::default_author(), &a.paths)
            .await
            .map_err(err)?
        {
            Some(c) => Ok(format!("committed {} ({} files)", c.id, c.files)),
            None => Ok("nothing to commit".into()),
        }
    }

    #[tool(description = "Land committed work on the workspace ref (server-side rebase, atomic).")]
    async fn gd_land(&self, Parameters(_): Parameters<Empty>) -> ToolResult {
        let mut ws = self.ws()?;
        let out = land::run(self.remote.as_ref(), &mut ws).await.map_err(err)?;
        Ok(format!(
            "landed {} (rebased: {}, synced files: {}){}",
            out.landed,
            out.rebased,
            out.synced,
            out.behind.map(|b| format!("; working copy not updated: {b}")).unwrap_or_default()
        ))
    }

    #[tool(description = "Update the workspace to the latest ref head.")]
    async fn gd_sync(&self, Parameters(_): Parameters<Empty>) -> ToolResult {
        let mut ws = self.ws()?;
        let out = sync::run(self.remote.as_ref(), &mut ws, None).await.map_err(err)?;
        Ok(format!("at {} ({} files changed)", out.to, out.changed))
    }
}
