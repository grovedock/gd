mod admin;
mod containers;
mod cr;
mod deploy;
mod issue;
mod login;
mod migrate;
mod release;
mod runner;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gd::features::{browse, clone, commit, diff, git, land, sync};
use gd::remote::{GrpcRemote, Remote};
use gd::workspace::{ChangeKind, Config, Workspace};
use grovedock_adapters::flags::SpecFlags;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(
    name = "gd",
    version,
    about = "Grovedock: source control for monorepos at agent scale",
    propagate_version = true
)]
struct Cli {
    /// Bearer token for the server ($GD_TOKEN); without one, the token `gd login` saved.
    #[arg(long, global = true, env = "GD_TOKEN", hide_env_values = true)]
    token: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a workspace and check out a ref (lazily, and sparsely with --path).
    Clone {
        server: String,
        dir: PathBuf,
        #[arg(long = "ref", default_value = "main")]
        ref_name: String,
        /// Only materialise these paths (repeatable).
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Fetch objects through a node daemon or regional cache.
        #[arg(long, env = "GD_OBJECTS")]
        objects: Option<String>,
    },
    /// Show working-copy changes.
    #[command(visible_alias = "st")]
    Status { paths: Vec<String> },
    /// Create a changeset on the server from working-copy changes (all, or PATHS).
    #[command(visible_alias = "ci")]
    Commit {
        #[arg(short, long)]
        message: String,
        /// Accepted for git muscle memory; Grovedock always commits all changes (or PATHS).
        #[arg(short = 'a', long = "all", hide = true)]
        _all: bool,
        paths: Vec<String>,
    },
    /// Land the committed work on the ref (server-side rebase + atomic ref update). Several
    /// commits land one after another, each on the one before.
    #[command(visible_alias = "push")]
    Land {
        /// Land every commit since the last land as one change.
        #[arg(long)]
        squash: bool,
    },
    /// Update the working copy to the ref head (or REV).
    #[command(visible_alias = "pull")]
    Sync { rev: Option<String> },
    /// Switch to a ref or changeset; -b creates a ref at the current base.
    #[command(visible_alias = "switch")]
    Checkout {
        target: String,
        #[arg(short = 'b')]
        create: bool,
    },
    /// Changeset history (server-side; no local history needed).
    Log {
        /// Limit to a path.
        path: Option<String>,
        #[arg(long)]
        rev: Option<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        oneline: bool,
        /// Only what agents landed; with a name, only that agent's (`--agent fix-01`).
        #[arg(long, num_args = 0..=1, default_missing_value = "*")]
        agent: Option<String>,
        /// Only changes written with this model (its `Model:` trailer contains MODEL).
        #[arg(long)]
        model: Option<String>,
    },
    /// Diff the working copy, or two revisions (`gd diff A B`).
    Diff {
        revs: Vec<String>,
        #[arg(long = "name-only")]
        names_only: bool,
    },
    /// Show a changeset and its patch against its first parent.
    Show { rev: Option<String> },
    /// Print a file at a revision without checking anything out.
    Cat { rev: String, path: String },
    /// Lock files for exclusive editing (for files that cannot be merged). Others cannot land
    /// changes to them until you unlock.
    Lock {
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Release your locks; --force releases someone else's (needs write access).
    Unlock {
        #[arg(required = true)]
        paths: Vec<String>,
        #[arg(long)]
        force: bool,
    },
    /// List locks, optionally under a directory.
    Locks {
        #[arg(default_value = "")]
        prefix: String,
    },
    /// List a directory at a revision without checking anything out.
    Ls {
        rev: String,
        #[arg(default_value = "")]
        path: String,
    },
    /// Find files by path substring at a revision (server-side).
    Find {
        pattern: String,
        #[arg(long, default_value = "main")]
        rev: String,
    },
    /// Search the code on the server: every word (3+ characters) or "phrase", with path:DIR,
    /// -path:DIR and lang:rs to narrow it. Answered from the server's index; no clone needed.
    Search {
        query: Vec<String>,
        #[arg(long, default_value = "main")]
        rev: String,
        /// Files to show.
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
        /// Only the paths of matching files.
        #[arg(short = 'l', long)]
        files_only: bool,
    },
    /// Where a function, type or class is defined (by name or how it starts).
    Symbol {
        name: String,
        #[arg(long, default_value = "main")]
        rev: String,
    },
    /// Check a server against the gd protocol specification (version 1): each requirement,
    /// passed or failed, with what was seen. Use an empty repository made for it:
    /// `gd conformance --server https://HOST/OWNER/conformance`.
    Conformance {
        #[arg(long)]
        server: Option<String>,
    },
    /// Show which changeset last changed each line of a file, and who wrote it. With --by, every
    /// line one agent (or person) wrote, in a file or a whole folder: `gd blame src --by agent/fix-01`.
    Blame {
        path: String,
        #[arg(long, default_value = "main")]
        rev: String,
        /// Only lines this writer wrote (an account like agent/fix-01, or a tool like claude_code).
        #[arg(long)]
        by: Option<String>,
    },
    /// List branches.
    #[command(visible_alias = "branch")]
    Refs,
    /// Make a tag at a revision (default: your workspace's branch), or list tags. A tag is made
    /// once and never moves.
    Tag {
        /// The tag, e.g. v1.2.0; none: list tags.
        name: Option<String>,
        /// What it names: a branch or change id.
        #[arg(long)]
        at: Option<String>,
    },
    /// Best common ancestor of two revisions.
    MergeBase { a: String, b: String },
    /// No-op: Grovedock has no staging area. Use `gd commit [PATHS]`.
    #[command(hide = true)]
    Add { paths: Vec<String> },
    /// Import a git repository's history.
    ImportGit {
        git_dir: PathBuf,
        #[arg(long)]
        server: Option<String>,
        #[arg(long = "git-ref", default_value = "HEAD")]
        git_ref: String,
        #[arg(long = "ref", default_value = "main")]
        ref_name: String,
        #[arg(long)]
        force: bool,
    },
    /// Mount a revision read-only; files are fetched on first read (Linux, macOS, Windows).
    Mount {
        dir: PathBuf,
        #[arg(long)]
        server: Option<String>,
        /// Daemon or cache serving objects.
        #[arg(long)]
        objects: Option<String>,
        #[arg(long, default_value = "main")]
        rev: String,
        /// In-memory cache for file contents.
        #[arg(long, default_value_t = 256)]
        cache_mb: u64,
        /// Serve the checkout over NFS on localhost and print the command that mounts it,
        /// instead of mounting (for machines where only an administrator may mount, or
        /// Linux machines without FUSE).
        #[arg(long)]
        serve_only: bool,
    },
    /// Serve Grovedock as MCP tools over stdio for AI agents (Claude Code, Codex, Cursor...).
    Mcp {
        #[arg(long)]
        server: Option<String>,
        /// Serve the whole server to remote agents over HTTP at this address (e.g.
        /// 0.0.0.0:7430), instead of one workspace over stdio. Each request's bearer token is
        /// passed through to the server.
        #[arg(long, value_name = "ADDR")]
        http: Option<std::net::SocketAddr>,
        /// Host names clients use to reach the HTTP endpoint (repeatable); default: loopback
        /// only, against DNS rebinding.
        #[arg(long = "allowed-host")]
        allowed_hosts: Vec<String>,
        /// Browser origins allowed to call the HTTP endpoint (repeatable), e.g.
        /// https://app.example.com:443; default: none (requests with an Origin are refused).
        #[arg(long = "allowed-origin")]
        allowed_origins: Vec<String>,
    },
    /// Sign in to a server from the browser; the token is saved in ~/.config/gd/credentials.json.
    Login {
        /// The server, e.g. https://gd.example.com; default $GD_SERVER or the workspace's.
        server: Option<String>,
    },
    /// Revoke the token `gd login` saved for a server, and forget it.
    Logout { server: Option<String> },
    /// Show who you are signed in as.
    Whoami { server: Option<String> },
    /// Create and list repositories on a server that hosts many (`grovedockd server --platform`).
    Repo {
        #[command(subcommand)]
        action: RepoCmd,
    },
    /// Organisations and their members.
    Org {
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: admin::OrgCmd,
    },
    /// Teams within an organisation, for granting access to many people at once.
    Team {
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: admin::TeamCmd,
    },
    /// Change requests: propose a branch, review it, land it.
    Cr {
        /// The repository, SERVER/OWNER/NAME; defaults to the workspace's.
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: cr::CrCmd,
    },
    /// Issues: work to do, assigned to people or agents; "fixes #N" in a change request closes
    /// one when it lands.
    Issue {
        /// The repository, SERVER/OWNER/NAME; defaults to the workspace's.
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: issue::IssueCmd,
    },
    /// Releases: a tag with notes written from what landed, and files.
    Release {
        /// The repository, SERVER/OWNER/NAME; defaults to the workspace's.
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: release::ReleaseCmd,
    },
    /// Runners: machines of yours that run Grovedock Runs jobs (CI) for an owner's repositories.
    Runner {
        /// The server (default: $GD_SERVER, or the workspace's).
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: runner::RunnerCmd,
    },
    /// Secrets for Runs: only runs of code already on a protected branch get them.
    Secret {
        /// The repository, SERVER/OWNER/NAME; defaults to the workspace's.
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: deploy::SecretCmd,
    },
    /// Variables for Runs (`${{ vars.NAME }}`): a repository's, an environment's, or an owner's.
    Var {
        /// The repository, SERVER/OWNER/NAME; defaults to the workspace's.
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: deploy::VarCmd,
    },
    /// Environments a repository deploys to: who approves, and from which branches.
    Env {
        /// The repository, SERVER/OWNER/NAME; defaults to the workspace's.
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: deploy::EnvCmd,
    },
    /// Protected branches: changes land on them only through a reviewed change request.
    Protect {
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: cr::ProtectCmd,
    },
    /// Webhooks: call a URL (CI, chat, trackers) when something happens in a repository.
    Webhook {
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: admin::WebhookCmd,
    },
    /// Access tokens for you and for agents you answer for.
    Token {
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: admin::TokenCmd,
    },
    /// Server administration (site administrators only).
    Admin {
        #[arg(long, global = true)]
        server: Option<String>,
        #[command(subcommand)]
        action: admin::AdminCmd,
    },
    /// Move a repository from any git host to Grovedock: full history, and from GitHub, open pull
    /// requests (as change requests) and issues with their comments.
    Migrate {
        /// Where it is now: any URL git can clone.
        from: String,
        /// Where it goes: SERVER/OWNER/NAME.
        #[arg(long)]
        to: String,
        /// For private GitHub repositories, and to avoid GitHub's rate limit. Prefer the
        /// GD_GITHUB_TOKEN (or GITHUB_TOKEN) environment variable or --github-token-file: a
        /// value given here is visible to other users of the machine (`ps`).
        #[arg(long, env = "GD_GITHUB_TOKEN", hide_env_values = true)]
        github_token: Option<String>,
        /// Read the GitHub token from this file.
        #[arg(long, conflicts_with = "github_token")]
        github_token_file: Option<std::path::PathBuf>,
        /// Skip issues.
        #[arg(long)]
        no_issues: bool,
        /// Skip pull requests.
        #[arg(long)]
        no_pull_requests: bool,
        /// GitHub Enterprise Server's API, e.g. https://github.example.com/api/v3.
        #[arg(long)]
        github_api: Option<String>,
        /// Report progress as JSON lines (for the server's importer) instead of text.
        #[arg(long, hide = true)]
        progress_json: bool,
    },
    /// Keep a git repository (GitHub, GitLab, Bitbucket, any git remote) up to date with a
    /// branch: every landed change becomes the same git commit every time, pushed without force.
    Mirror {
        /// Where to push: any URL git accepts. Credentials come from git (SSH keys or a
        /// credential helper).
        #[arg(long)]
        to: String,
        #[arg(long)]
        server: Option<String>,
        #[arg(long, default_value = "main")]
        rev: String,
        #[arg(long = "git-ref", default_value = "main")]
        git_ref: String,
        /// The mirror's local copy (a bare git repository); defaults to the user cache.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Mirror again every SECONDS until interrupted; without it, mirror once and exit.
        #[arg(long, value_name = "SECONDS")]
        every: Option<u64>,
    },
    /// Export the history to a git repository: merges included, and changes that came from git as their original commits.
    ExportGit {
        git_dir: PathBuf,
        #[arg(long)]
        server: Option<String>,
        #[arg(long, default_value = "main")]
        rev: String,
        #[arg(long = "git-ref", default_value = "main")]
        git_ref: String,
    },
}

#[derive(Subcommand)]
enum RepoCmd {
    /// Create OWNER/NAME. Then: `gd clone SERVER/OWNER/NAME`.
    Create {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        server: Option<String>,
    },
    /// List repositories, optionally of one owner.
    List {
        #[arg(long, default_value = "")]
        owner: String,
        #[arg(long)]
        server: Option<String>,
    },
    /// Give a person, agent or team (ORG/TEAM) a role on a repository; `none` removes it.
    /// With only the repository, lists who has access.
    Grant {
        repo: String,
        principal: Option<String>,
        #[arg(value_enum)]
        role: Option<admin::RoleArg>,
        #[arg(long)]
        server: Option<String>,
    },
}

/// A path argument as a repository path: inside a workspace it is taken relative to the
/// current directory, elsewhere it is already repository-relative.
fn repo_path(arg: &str) -> Result<String> {
    let Ok(ws) = workspace() else { return Ok(arg.trim_matches('/').to_owned()) };
    let abs = std::env::current_dir()?.join(arg);
    let mut parts: Vec<String> = Vec::new();
    for c in abs.strip_prefix(std::fs::canonicalize(&ws.root)?).or_else(|_| abs.strip_prefix(&ws.root))?.components() {
        match c {
            std::path::Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            std::path::Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    Ok(parts.join("/"))
}

fn workspace() -> Result<Workspace> {
    Workspace::find(Path::new("."))
}

fn connect(server: &str, objects: Option<&str>, token: Option<&str>) -> Result<Arc<dyn Remote>> {
    Ok(Arc::new(GrpcRemote::connect(server, objects, token)?))
}

fn ws_remote(ws: &Workspace, token: Option<&str>) -> Result<Arc<dyn Remote>> {
    connect(&ws.config.server, ws.config.objects.as_deref(), token)
}

/// The platform service of a server (its base URL, without any OWNER/NAME).
fn platform_client(
    server: Option<String>,
    token: Option<&str>,
) -> Result<grovedock_proto::platform_client::PlatformClient<gd::remote::Ch>> {
    let base = grovedock_proto::split_repo_url(&server_for(server)?).0;
    Ok(GrpcRemote::connect(&base, None, token)?.platform())
}

/// The review service and repository for `gd cr` and `gd protect`.
fn review_target(server: Option<String>, token: Option<&str>) -> Result<cr::Target> {
    let url = server_for(server)?;
    let (base, repo) = grovedock_proto::split_repo_url(&url);
    let repo = repo.context("name the repository: --server SERVER/OWNER/NAME")?;
    let (owner, name) = repo.split_once('/').context("give the repository as OWNER/NAME")?;
    Ok(cr::Target {
        client: GrpcRemote::connect(&base, None, token)?.reviews(),
        issues: GrpcRemote::connect(&base, None, token)?.issues(),
        repo: grovedock_proto::RepositoryRef { owner: owner.into(), name: name.into() },
        branch: workspace().ok().map(|ws| ws.config.ref_name),
    })
}

/// Server for workspace-free commands: `--server`, else $GD_SERVER, else the current workspace.
fn server_for(explicit: Option<String>) -> Result<String> {
    let ws = || workspace().ok().map(|w| w.config.server);
    pick_server(explicit, std::env::var("GD_SERVER").ok(), ws)
        .context("no server: set GD_SERVER=URL or run inside a workspace")
}

/// A bare $GD_SERVER naming the workspace's own server keeps the workspace's repository
/// (`SERVER/OWNER/NAME`), so `gd protect` or `gd cr` in a clone need no `--server`.
fn pick_server(explicit: Option<String>, env: Option<String>, ws: impl FnOnce() -> Option<String>) -> Option<String> {
    use grovedock_proto::split_repo_url;
    if explicit.is_some() {
        return explicit;
    }
    let Some(env) = env else { return ws() };
    match (split_repo_url(&env), ws()) {
        ((base, None), Some(ws)) if split_repo_url(&ws).0 == base => Some(ws),
        _ => Some(env),
    }
}

fn short_message(m: &str) -> &str {
    m.lines().next().unwrap_or("")
}

fn fmt_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant), UTC.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", tod / 3600, tod % 3600 / 60)
}

fn main() {
    grovedock_core::env::adopt_legacy();
    // Library logs (mounts, transports) only when asked for, e.g. GD_LOG=debug.
    if let Ok(filter) = std::env::var("GD_LOG") {
        let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).try_init();
    }
    // The commands run on a thread with a roomy stack, and so do the runtime's workers: Windows
    // gives the main thread only 1 MB, which deep imports and migrations can exceed.
    let code = std::thread::Builder::new()
        .name("gd".into())
        .stack_size(16 << 20)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(8 << 20)
                .build()
                .expect("starting the async runtime");
            match runtime.block_on(Box::pin(run())) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    1
                }
            }
        })
        .expect("starting the main thread")
        .join()
        .unwrap_or(101);
    std::process::exit(code);
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let flags = SpecFlags::from_env()?;
    let token = cli.token.as_deref();
    let mut stdout = std::io::stdout().lock();
    match cli.cmd {
        Cmd::Clone { server, dir, ref_name, paths, objects } => {
            let remote = connect(&server, objects.as_deref(), token)?;
            let config = Config { server, objects, ref_name, sparse: paths };
            let ws = clone::run(remote.as_ref(), &flags, &dir, config).await?;
            match ws.base {
                Some(b) => println!("checked out {} files at {} into {}", ws.index.len(), b.short(), dir.display()),
                None => println!("cloned empty ref into {}", dir.display()),
            }
        }
        Cmd::Status { paths } => {
            let ws = workspace()?;
            let changes = ws.status(&paths)?;
            for c in &changes {
                let tag = match c.kind {
                    ChangeKind::Added => "A",
                    ChangeKind::Modified => "M",
                    ChangeKind::Deleted => "D",
                };
                writeln!(stdout, "{tag} {}", c.path)?;
            }
            if changes.is_empty() {
                writeln!(stdout, "clean (base {})", ws.base.map_or("none".into(), |b| b.short()))?;
            }
        }
        Cmd::Commit { message, paths, .. } => {
            let mut ws = workspace()?;
            let remote = ws_remote(&ws, token)?;
            match commit::run(remote.as_ref(), &mut ws, &message, &gd::default_author(), &paths).await? {
                Some(c) => println!(
                    "committed {} ({} files, {} new objects uploaded); run `gd land` to land on {}",
                    c.id.short(),
                    c.files,
                    c.uploaded,
                    ws.config.ref_name
                ),
                None => println!("nothing to commit"),
            }
        }
        Cmd::Land { squash } => {
            let mut ws = workspace()?;
            let remote = ws_remote(&ws, token)?;
            let out = match land::run_with(remote.as_ref(), &mut ws, squash).await {
                Ok(out) => out,
                // A protected branch takes changes through review: propose this one instead.
                Err(e) if format!("{e:#}").contains("is protected") && ws.base.is_some() => {
                    let target = review_target(Some(ws.config.server.clone()), token).map_err(|_| e)?;
                    let (number, branch) = cr::propose(target, remote.as_ref(), &ws).await?;
                    println!(
                        "{} is protected, so this went for review: change request #{number} from {branch}.\n\
                         It lands once approved and its checks pass: gd cr view {number}",
                        ws.config.ref_name
                    );
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            println!(
                "landed {}{} on {}{}",
                if out.commits > 1 { format!("{} commits, ending at ", out.commits) } else { String::new() },
                out.landed.short(),
                ws.config.ref_name,
                if out.rebased { format!(" (rebased; {} incoming files synced)", out.synced) } else { String::new() }
            );
            if let Some(why) = out.behind {
                eprintln!("warning: working copy not updated: {why}");
            }
        }
        Cmd::Sync { rev } => {
            let mut ws = workspace()?;
            let remote = ws_remote(&ws, token)?;
            let out = sync::run(remote.as_ref(), &mut ws, rev.as_deref()).await?;
            println!("at {} ({} files changed)", out.to.short(), out.changed);
        }
        Cmd::Checkout { target, create } => {
            let mut ws = workspace()?;
            let remote = ws_remote(&ws, token)?;
            if create {
                sync::create_branch(remote.as_ref(), &mut ws, &target).await?;
                println!("created ref {target} at {}", ws.base.map_or("none".into(), |b| b.short()));
            } else {
                let out = sync::checkout(remote.as_ref(), &mut ws, &target).await?;
                println!("at {} ({} files changed)", out.to.short(), out.changed);
            }
        }
        Cmd::Log { path, rev, limit, oneline, agent, model } => {
            let (remote, rev) = match workspace() {
                Ok(ws) => {
                    let rev = rev.or(ws.base.map(|b| b.to_hex())).unwrap_or(ws.config.ref_name.clone());
                    (ws_remote(&ws, token)?, rev)
                }
                Err(_) => (connect(&server_for(None)?, None, token)?, rev.unwrap_or("main".into())),
            };
            let filtered = agent.is_some() || model.is_some();
            // ponytail: filters run here over the most recent 10,000 changes; a server-side
            // filter when histories with sparse agent work get longer.
            let fetch = if filtered { 10_000 } else { limit };
            let wanted = |c: &gd::remote::RemoteChangeset| {
                let who = &c.changeset.author;
                let by_agent = match agent.as_deref() {
                    None => true,
                    Some("*") => who.starts_with("agent/"),
                    Some(a) => who == a || *who == format!("agent/{}", a.trim_start_matches("agent/")),
                };
                // The server's record; older changes said it in a `Model:` trailer.
                let with_model = model.as_deref().is_none_or(|m| {
                    let m = m.to_lowercase();
                    c.provenance.as_ref().is_some_and(|p| p.model.to_lowercase().contains(&m))
                        || c.changeset
                            .message
                            .lines()
                            .any(|l| l.strip_prefix("Model:").is_some_and(|v| v.trim().to_lowercase().contains(&m)))
                });
                by_agent && with_model
            };
            let entries = browse::log(remote.as_ref(), &rev, path.as_deref().unwrap_or(""), fetch).await?;
            for c in entries.into_iter().filter(wanted).take(limit as usize) {
                if oneline {
                    writeln!(stdout, "{} {}", c.id.short(), short_message(&c.changeset.message))?;
                } else {
                    writeln!(stdout, "changeset {}", c.id)?;
                    writeln!(stdout, "Author: {}", c.changeset.author)?;
                    writeln!(stdout, "Date:   {}\n", fmt_time(c.changeset.timestamp_ms))?;
                    for l in c.changeset.message.trim_end().lines() {
                        writeln!(stdout, "    {l}")?;
                    }
                    writeln!(stdout)?;
                }
            }
        }
        Cmd::Diff { revs, names_only } => {
            let text = match revs.as_slice() {
                [] => {
                    let ws = workspace()?;
                    diff::working(ws_remote(&ws, token)?.as_ref(), &ws, &[], names_only).await?
                }
                [a, b] => diff::revisions(connect(&server_for(None)?, None, token)?.as_ref(), a, b, names_only).await?,
                _ => anyhow::bail!("usage: gd diff [FROM TO]"),
            };
            stdout.write_all(text.as_bytes())?;
        }
        Cmd::Show { rev } => {
            let ws = workspace().ok();
            let remote = match &ws {
                Some(w) => ws_remote(w, token)?,
                None => connect(&server_for(None)?, None, token)?,
            };
            let rev = rev.or(ws.and_then(|w| w.base.map(|b| b.to_hex()))).context("no revision")?;
            let c = remote.changeset(&rev).await?;
            writeln!(
                stdout,
                "changeset {}\nAuthor: {}\nDate:   {}\n",
                c.id,
                c.changeset.author,
                fmt_time(c.changeset.timestamp_ms)
            )?;
            for l in c.changeset.message.trim_end().lines() {
                writeln!(stdout, "    {l}")?;
            }
            writeln!(stdout)?;
            if let Some(p) = c.changeset.first_parent()? {
                stdout.write_all(
                    diff::revisions(remote.as_ref(), &p.to_hex(), &c.id.to_hex(), false).await?.as_bytes(),
                )?;
            }
        }
        Cmd::Lock { paths } => {
            let remote = connect(&server_for(None)?, None, token)?;
            for p in paths {
                let (path, owner, _) = remote.lock(&repo_path(&p)?).await?;
                writeln!(stdout, "locked {path} ({owner})")?;
            }
        }
        Cmd::Unlock { paths, force } => {
            let remote = connect(&server_for(None)?, None, token)?;
            for p in paths {
                let path = repo_path(&p)?;
                remote.unlock(&path, force).await?;
                writeln!(stdout, "unlocked {path}")?;
            }
        }
        Cmd::Locks { prefix } => {
            let remote = connect(&server_for(None)?, None, token)?;
            let prefix = if prefix.is_empty() { prefix } else { repo_path(&prefix)? };
            for (path, owner, at_ms) in remote.locks(&prefix).await? {
                let age = (gd::now_ms() - at_ms).max(0) / 60_000;
                writeln!(
                    stdout,
                    "{path}\t{owner}\t{}",
                    if age < 60 { format!("{age}m ago") } else { format!("{}h ago", age / 60) }
                )?;
            }
        }
        Cmd::Cat { rev, path } => {
            let remote = connect(&server_for(None)?, None, token)?;
            stdout.write_all(&browse::cat(remote.as_ref(), &rev, &path).await?)?;
        }
        Cmd::Ls { rev, path } => {
            let remote = connect(&server_for(None)?, None, token)?;
            for e in browse::ls(remote.as_ref(), &rev, &path).await? {
                writeln!(
                    stdout,
                    "{} {} {}",
                    browse::mode_char(e.mode),
                    e.id.map_or("-".into(), |i| i.short()),
                    e.name
                )?;
            }
        }
        Cmd::Find { pattern, rev } => {
            let remote = connect(&server_for(None)?, None, token)?;
            for p in remote.find(&rev, &pattern, 10_000).await? {
                writeln!(stdout, "{p}")?;
            }
        }
        Cmd::Search { query, rev, limit, files_only } => {
            let r = GrpcRemote::connect(&server_for(None)?, None, token)?;
            let res = r
                .code_search()
                .search_code(grovedock_proto::SearchCodeRequest {
                    query: query.join(" "),
                    r#ref: rev,
                    limit,
                    page_token: String::new(),
                })
                .await
                .map_err(|s| anyhow::anyhow!("{}", s.message()))?
                .into_inner();
            for h in &res.hits {
                if files_only {
                    writeln!(stdout, "{}", h.path)?;
                    continue;
                }
                for l in &h.lines {
                    writeln!(stdout, "{}:{}: {}", h.path, l.line, l.text.trim_end())?;
                }
                if h.more > 0 {
                    writeln!(stdout, "{}: …and {} more line(s)", h.path, h.more)?;
                }
            }
            let shown = res.hits.len();
            if res.files as usize > shown {
                eprintln!("{shown} of about {} files shown; -n shows more, path:/lang: narrow it", res.files);
            }
            if res.catching_up {
                eprintln!("the index is still catching up with the branch's latest changes");
            }
        }
        Cmd::Symbol { name, rev } => {
            let r = GrpcRemote::connect(&server_for(None)?, None, token)?;
            let res = r
                .code_search()
                .find_symbol(grovedock_proto::FindSymbolRequest { name, r#ref: rev, limit: 50 })
                .await
                .map_err(|s| anyhow::anyhow!("{}", s.message()))?
                .into_inner();
            if res.symbols.is_empty() {
                eprintln!("no definitions by that name");
            }
            for s in res.symbols {
                writeln!(stdout, "{}:{}: {} {}  {}", s.path, s.line, s.kind, s.name, s.text)?;
            }
        }
        Cmd::Conformance { server } => {
            let remote = connect(&server_for(server)?, None, token)?;
            let scratch = tempfile::tempdir()?;
            let results = gd::conformance::run(remote.as_ref(), &flags, Some(scratch.path())).await;
            let (mut passed, mut failed, mut skipped) = (0, 0, 0);
            for r in &results {
                match &r.failed {
                    None => {
                        passed += 1;
                        writeln!(stdout, "  pass  {:<7} {}", r.id, r.what)?;
                    }
                    Some(Ok(why)) => {
                        skipped += 1;
                        writeln!(stdout, "  skip  {:<7} {} ({why})", r.id, r.what)?;
                    }
                    Some(Err(why)) => {
                        failed += 1;
                        writeln!(stdout, "  FAIL  {:<7} {}\n        {why}", r.id, r.what)?;
                    }
                }
            }
            writeln!(stdout, "\ngd protocol v1: {passed} passed, {failed} failed, {skipped} skipped")?;
            if failed > 0 {
                std::process::exit(1);
            }
        }
        Cmd::Blame { path, rev, by: Some(by) } => {
            let remote = connect(&server_for(None)?, None, token)?;
            // The files: the one named, or every file under the folder named (up to 5,000).
            let mut files = Vec::new();
            let mut dirs = vec![path.trim_matches('/').to_owned()];
            if remote.read_file(&rev, &path).await.is_ok() {
                files.push(path.clone());
                dirs.clear();
            }
            while let Some(d) = dirs.pop() {
                for e in remote.list_dir(&rev, &d).await? {
                    let p = if d.is_empty() { e.name.clone() } else { format!("{d}/{}", e.name) };
                    match e.mode {
                        grovedock_core::object::Mode::Dir => dirs.push(p),
                        grovedock_core::object::Mode::Symlink => {}
                        _ if files.len() < 5000 => files.push(p),
                        _ => {}
                    }
                }
            }
            let want = by.to_lowercase();
            let (mut lines, mut in_files) = (0usize, 0usize);
            for f in &files {
                let Ok(blame) = remote.blame(&rev, f).await else { continue };
                let mut here = 0;
                for (i, (_, text, who)) in blame.iter().enumerate() {
                    if who.to_lowercase().split(" · ").next().is_some_and(|w| {
                        w == want || w.trim_start_matches("agent/") == want.trim_start_matches("agent/")
                    }) {
                        writeln!(stdout, "{f}:{}: {text}", i + 1)?;
                        here += 1;
                    }
                }
                lines += here;
                in_files += usize::from(here > 0);
            }
            eprintln!("{by} wrote {lines} lines in {in_files} of {} files", files.len());
        }
        Cmd::Blame { path, rev, by: None } => {
            let remote = connect(&server_for(None)?, None, token)?;
            let lines = remote.blame(&rev, &path).await?;
            let width = lines.iter().map(|(_, _, w)| w.chars().count()).max().unwrap_or(0).min(36);
            for (i, (id, line, who)) in lines.into_iter().enumerate() {
                writeln!(stdout, "{} {who:<width$} {:>5} {line}", id.short(), i + 1)?;
            }
        }
        Cmd::Refs => {
            let remote = connect(&server_for(None)?, None, token)?;
            let current = workspace().ok().map(|w| w.config.ref_name);
            for (name, id) in remote.list_refs().await? {
                if name.starts_with(gd::bridge::TAG_PREFIX) {
                    continue;
                }
                let mark = if current.as_deref() == Some(&name) { "*" } else { " " };
                writeln!(stdout, "{mark} {name} {}", id.short())?;
            }
        }
        Cmd::Tag { name: None, .. } => {
            let remote = connect(&server_for(None)?, None, token)?;
            for (name, id) in remote.list_refs().await? {
                if let Some(tag) = name.strip_prefix(gd::bridge::TAG_PREFIX) {
                    writeln!(stdout, "{tag} {}", id.short())?;
                }
            }
        }
        Cmd::Tag { name: Some(name), at } => {
            let remote = connect(&server_for(None)?, None, token)?;
            let rev = at.or_else(|| workspace().ok().map(|w| w.config.ref_name)).unwrap_or_else(|| "main".into());
            let id = remote
                .log(&rev, "", 1)
                .await?
                .first()
                .map(|c| c.id)
                .with_context(|| format!("nothing at `{rev}` to tag"))?;
            let tag = format!("{}{name}", gd::bridge::TAG_PREFIX);
            if !remote.compare_and_swap(&tag, None, id).await? {
                anyhow::bail!("tag {name} already exists; tags don't move, so pick a new name");
            }
            writeln!(stdout, "tagged {} as {name}", id.short())?;
        }
        Cmd::MergeBase { a, b } => {
            let remote = connect(&server_for(None)?, None, token)?;
            match remote.merge_base(&a, &b).await? {
                Some(id) => writeln!(stdout, "{id}")?,
                None => anyhow::bail!("no common ancestor"),
            }
        }
        Cmd::Add { .. } => {
            println!("gd has no staging area: `gd commit -m MSG [PATHS]` commits all changes (or just PATHS)");
        }
        Cmd::ImportGit { git_dir, server, git_ref, ref_name, force } => {
            let remote = connect(&server_for(server)?, None, token)?;
            let started = std::time::Instant::now();
            let out = git::import(remote.as_ref(), &flags, &git_dir, &git_ref, &ref_name, force).await?;
            println!(
                "imported {} commits, {} blobs ({} uploaded) in {:.1}s; {ref_name} -> {}{}",
                out.commits,
                out.blobs,
                out.uploaded,
                started.elapsed().as_secs_f64(),
                out.tip.map_or("none".into(), |t| t.short()),
                if out.skipped_submodules > 0 {
                    format!(" ({} submodule entries skipped)", out.skipped_submodules)
                } else {
                    String::new()
                }
            );
        }
        Cmd::Mount { dir, server, objects, rev, cache_mb, serve_only } => {
            grovedock_core::flags::require(&flags, grovedock_core::flags::Flag::VirtualCheckout)?;
            let remote = connect(&server_for(server)?, objects.as_deref(), token)?;
            let cs = remote.changeset(&rev).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let meta = std::fs::metadata(&dir)
                    .with_context(|| format!("{} must be an existing directory", dir.display()))?;
                let tree = Arc::new(gd::mount::VirtualTree::new(
                    remote.objects(),
                    tokio::runtime::Handle::current(),
                    cs.root_tree,
                    cache_mb << 20,
                ));
                if serve_only || cfg!(target_os = "macos") {
                    let port = gd::mount::nfs::serve(tree, meta.uid(), meta.gid()).await?;
                    if serve_only {
                        eprintln!("serving {rev} over NFS on 127.0.0.1:{port}; mount it with:");
                        println!("{}", gd::mount::nfs::mount_command(port, &dir));
                        tokio::signal::ctrl_c().await?;
                        return Ok(());
                    }
                    let target = dir.clone();
                    tokio::task::spawn_blocking(move || gd::mount::nfs::mount(port, &target)).await??;
                    eprintln!("mounted {rev} at {}; Ctrl-C to unmount", dir.display());
                    tokio::signal::ctrl_c().await?;
                    let target = dir.clone();
                    tokio::task::spawn_blocking(move || gd::mount::nfs::unmount(&target)).await??;
                } else {
                    #[cfg(target_os = "linux")]
                    {
                        let session = gd::mount::fuse::mount(tree, &dir)?;
                        eprintln!("mounted {rev} at {}; Ctrl-C to unmount", dir.display());
                        tokio::signal::ctrl_c().await?;
                        session.umount_and_join()?;
                    }
                }
            }
            #[cfg(windows)]
            {
                anyhow::ensure!(!serve_only, "--serve-only is for macOS and Linux; Windows projects files directly");
                let tree = Arc::new(gd::mount::VirtualTree::new(
                    remote.objects(),
                    tokio::runtime::Handle::current(),
                    cs.root_tree,
                    cache_mb << 20,
                ));
                let target = dir.clone();
                let session = tokio::task::spawn_blocking(move || gd::mount::projfs::mount(tree, &target)).await??;
                eprintln!("projecting {rev} into {}; Ctrl-C to stop (files already read stay on disk)", dir.display());
                tokio::signal::ctrl_c().await?;
                drop(session);
            }
            #[cfg(not(any(unix, windows)))]
            {
                let _ = (dir, cs, cache_mb, serve_only);
                anyhow::bail!(
                    "`gd mount` is not supported on this platform; use `gd clone --path` for a sparse checkout"
                );
            }
        }
        Cmd::Mcp { server, http: Some(listen), allowed_hosts, allowed_origins } => {
            let base = grovedock_proto::split_repo_url(&server_for(server)?).0;
            gd::mcp_remote::RemoteMcp::new(base).serve_http(listen, allowed_hosts, allowed_origins).await?;
        }
        Cmd::Mcp { server, .. } => {
            let ws = workspace().ok();
            let remote = match &ws {
                Some(w) if server.is_none() => ws_remote(w, token)?,
                _ => connect(&server_for(server)?, None, token)?,
            };
            drop(stdout);
            gd::mcp::SapMcp::new(remote, ws.map(|w| w.root)).serve_stdio().await?;
        }
        Cmd::Login { server } => login::login(&server_for(server)?).await?,
        Cmd::Logout { server } => login::logout(&server_for(server)?).await?,
        Cmd::Whoami { server } => login::whoami(&server_for(server)?, token).await?,
        Cmd::Org { server, action } => admin::org(platform_client(server, token)?, action).await?,
        Cmd::Team { server, action } => admin::team(platform_client(server, token)?, action).await?,
        Cmd::Cr { server, action } => cr::cr(review_target(server, token)?, action).await?,
        Cmd::Issue { server, action } => {
            let t = review_target(server, token)?;
            issue::issue(t.issues, t.repo, action).await?
        }
        Cmd::Release { server, action } => {
            let url = server_for(server)?;
            let t = review_target(Some(url.clone()), token)?;
            let base = grovedock_proto::split_repo_url(&url).0;
            let releases = GrpcRemote::connect(&base, None, token)?.releases();
            release::release(releases, &base, connect(&url, None, token)?.as_ref(), t.repo, action).await?
        }
        Cmd::Runner { server, action } => runner::runner(server_for(server).ok(), token, action).await?,
        Cmd::Secret { server, action } => deploy::secret(server_for(server)?, token, action).await?,
        Cmd::Var { server, action } => deploy::var(server_for(server)?, token, action).await?,
        Cmd::Env { server, action } => deploy::env(server_for(server)?, token, action).await?,
        Cmd::Protect { server, action } => cr::protect(review_target(server, token)?, action).await?,
        Cmd::Webhook { server, action } => admin::webhook(platform_client(server, token)?, action).await?,
        Cmd::Token { server, action } => admin::token(platform_client(server, token)?, action).await?,
        Cmd::Admin { server, action } => {
            let base = grovedock_proto::split_repo_url(&server_for(server)?).0;
            admin::admin(GrpcRemote::connect(&base, None, token)?, action).await?
        }
        Cmd::Repo { action } => {
            let server = match &action {
                RepoCmd::Create { server, .. } | RepoCmd::List { server, .. } | RepoCmd::Grant { server, .. } => {
                    server.clone()
                }
            };
            let base = grovedock_proto::split_repo_url(&server_for(server)?).0;
            let mut platform = GrpcRemote::connect(&base, None, token)?.platform();
            let unimplemented = |s: tonic::Status| {
                if s.code() == tonic::Code::Unimplemented {
                    anyhow::anyhow!("this server hosts one repository; start grovedockd with --platform to host many")
                } else {
                    anyhow::anyhow!("{}", s.message())
                }
            };
            match action {
                RepoCmd::Create { name, description, .. } => {
                    let (owner, repo) = name.split_once('/').context("give the repository as OWNER/NAME")?;
                    let r = platform
                        .create_repository(grovedock_proto::CreateRepositoryRequest {
                            owner: owner.into(),
                            name: repo.into(),
                            description,
                            ..Default::default()
                        })
                        .await
                        .map_err(unimplemented)?
                        .into_inner();
                    println!("created {}/{}; clone it with: gd clone {base}/{}/{}", r.owner, r.name, r.owner, r.name);
                }
                RepoCmd::List { owner, .. } => {
                    let rs = platform
                        .list_repositories(grovedock_proto::ListRepositoriesRequest { owner, ..Default::default() })
                        .await
                        .map_err(unimplemented)?
                        .into_inner()
                        .repositories;
                    for r in rs {
                        println!("{}/{}\t{} landed today\t{}", r.owner, r.name, r.lands_24h, r.description);
                    }
                }
                RepoCmd::Grant { repo, principal, role, .. } => admin::grant(platform, &repo, principal, role).await?,
            }
        }
        Cmd::Migrate {
            from,
            to,
            github_token,
            github_token_file,
            no_issues,
            no_pull_requests,
            github_api,
            progress_json,
        } => {
            if std::env::args().any(|a| a == "--github-token" || a.starts_with("--github-token=")) {
                eprintln!(
                    "warning: --github-token is visible to other users of this machine; set GD_GITHUB_TOKEN or use --github-token-file"
                );
            }
            let github_token = match github_token_file {
                Some(f) => Some(
                    std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?.trim().to_owned(),
                ),
                None => github_token.or_else(|| std::env::var("GITHUB_TOKEN").ok()),
            }
            .filter(|t| !t.is_empty());
            let m = migrate::Migration {
                from,
                to,
                github_token,
                issues: !no_issues,
                pull_requests: !no_pull_requests,
                github_api,
                json: progress_json,
            };
            migrate::run(m, token, &flags).await?;
        }
        Cmd::Mirror { to, server, rev, git_ref, dir, every } => {
            let remote = connect(&server_for(server)?, None, token)?;
            let dir = dir.unwrap_or_else(|| git::default_mirror_dir(&to));
            let shown = git::redact(&to);
            loop {
                match git::mirror(remote.as_ref(), &flags, &rev, &dir, &git_ref, &to).await {
                    Ok(n) if n > 0 || every.is_none() => {
                        println!("mirrored {n} changes from {rev} to {shown} ({git_ref})")
                    }
                    Ok(_) => {}
                    // Keep running through network and server blips; the next round retries.
                    Err(e) if every.is_some() => eprintln!("gd: mirror to {shown}: {e:#}; retrying"),
                    Err(e) => return Err(e),
                }
                let Some(secs) = every else { break };
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(secs.max(1))) => {}
                    _ = tokio::signal::ctrl_c() => break,
                }
            }
        }
        Cmd::ExportGit { git_dir, server, rev, git_ref } => {
            let remote = connect(&server_for(server)?, None, token)?;
            let n = git::export(remote.as_ref(), &flags, &rev, &git_dir, &git_ref).await?;
            println!("exported {n} commits to {} ({git_ref})", git_dir.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::pick_server;

    #[test]
    fn a_bare_server_address_keeps_the_workspace_repository() {
        let ws = || Some("https://p.example.com/lumen/fleet".to_owned());
        let pick = |explicit: Option<&str>, env: Option<&str>| {
            pick_server(explicit.map(Into::into), env.map(Into::into), ws).unwrap()
        };
        assert_eq!(pick(None, Some("https://p.example.com/")), "https://p.example.com/lumen/fleet");
        assert_eq!(pick(None, None), "https://p.example.com/lumen/fleet");
        assert_eq!(pick(None, Some("https://p.example.com/acme/web")), "https://p.example.com/acme/web");
        assert_eq!(pick(None, Some("https://other.example.com")), "https://other.example.com");
        assert_eq!(pick(Some("https://p.example.com"), Some("https://x")), "https://p.example.com");
    }
}
