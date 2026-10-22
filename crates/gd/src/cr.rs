//! `gd cr`: change requests from the command line, for people, agents and CI; and
//! `gd protect` for protected branches.

use anyhow::{Context, Result, anyhow};
use clap::{Subcommand, ValueEnum};
use gd::remote::Ch;
use grovedock_proto::reviews_client::ReviewsClient;
use grovedock_proto::{
    AddCommentRequest, ChangeRequest, ChangeRequestRef, ChangeRequestState, Check, CheckState,
    CreateChangeRequestRequest, ListChangeRequestsRequest, Protection, ProtectionRef, ReportCheckRequest,
    RepositoryRef, SubmitReviewRequest, UpdateChangeRequestRequest, Verdict,
};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ReviewArg {
    Approve,
    /// Request changes.
    Changes,
    Comment,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CheckArg {
    Pending,
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum StateArg {
    Open,
    Draft,
    Landed,
    Closed,
    All,
}

#[derive(Subcommand)]
pub enum CrCmd {
    /// Propose a branch for landing on another: `gd cr create --title "…"`.
    Create {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        body: String,
        /// The branch to land; defaults to the workspace's branch.
        #[arg(long)]
        source: Option<String>,
        /// Where to land it; defaults to the repository's default branch.
        #[arg(long, default_value = "")]
        target: String,
        #[arg(long)]
        draft: bool,
        /// Ask someone to review (repeatable).
        #[arg(long = "reviewer")]
        reviewers: Vec<String>,
        /// Propose it to the repository this one was forked from (OWNER/NAME); reading that
        /// repository is enough.
        #[arg(long, value_name = "OWNER/NAME")]
        upstream: Option<String>,
    },
    /// List change requests.
    List {
        #[arg(long, value_enum, default_value = "open")]
        state: StateArg,
        /// Only this author; `agent/*` for every agent.
        #[arg(long, default_value = "")]
        author: String,
        /// Search, e.g. `is:ready reviewer:@me into:main tax`.
        #[arg(long, short, default_value = "")]
        query: String,
        /// How many to show (at most 200); the total is printed after.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Show one, with what still blocks it.
    View {
        number: u64,
    },
    /// Comment, on the change or on a line of a file.
    Comment {
        number: u64,
        #[arg(short, long)]
        message: String,
        #[arg(long, requires = "line")]
        path: Option<String>,
        #[arg(long)]
        line: Option<u32>,
    },
    /// Approve, request changes, or comment.
    Review {
        number: u64,
        #[arg(value_enum)]
        verdict: ReviewArg,
        #[arg(short, long, default_value = "")]
        message: String,
    },
    /// Report a CI check on the current version: `gd cr check 12 build passed --url …`.
    Check {
        number: u64,
        name: String,
        #[arg(value_enum)]
        state: CheckArg,
        #[arg(long, default_value = "")]
        url: String,
        #[arg(long, default_value = "")]
        summary: String,
    },
    /// Land it, once nothing blocks it.
    Land {
        number: u64,
    },
    /// Replay it on the target's tip; files both changed merge line by line.
    Rebase {
        number: u64,
    },
    /// Where it clashes with its target: each file's merge, with conflict markers. `--write DIR`
    /// saves them as DIR/PATH to edit, for `gd cr resolve --from DIR`.
    Conflicts {
        number: u64,
        #[arg(long)]
        write: Option<std::path::PathBuf>,
    },
    /// Undo everything an agent landed on a branch since a time, as one change request. Later
    /// edits by people stay where they don't overlap; files where they do are left and listed.
    UndoAgent {
        /// The agent, e.g. agent/acme-claude (or just acme-claude).
        agent: String,
        /// How far back: 30m, 2h, 1d, or a UTC time like 2026-10-20 or "2026-10-20 14:05".
        #[arg(long)]
        since: String,
        /// The branch (default: the default branch).
        #[arg(long, default_value = "")]
        target: String,
        /// Only say what it would undo.
        #[arg(long)]
        dry_run: bool,
    },
    /// Settle its clashes: each clashing file is read from DIR/PATH (markers removed), then the
    /// branch is replayed on the target's tip.
    Resolve {
        number: u64,
        #[arg(long)]
        from: std::path::PathBuf,
        /// Settle a clash by deleting the file (repeatable).
        #[arg(long = "delete")]
        deletes: Vec<String>,
    },
    /// Mark a draft ready for review.
    Ready {
        number: u64,
    },
    Close {
        number: u64,
    },
    Reopen {
        number: u64,
    },
}

#[derive(Subcommand)]
pub enum ProtectCmd {
    /// List protected branches.
    List,
    /// Protect a branch or prefix (`main`, `release/*`).
    Set {
        pattern: String,
        /// Approvals needed before a change request lands.
        #[arg(long, default_value_t = 1)]
        approvals: u32,
        /// A check that must pass (repeatable).
        #[arg(long = "check")]
        checks: Vec<String>,
        /// Keep approvals when the branch changes after them.
        #[arg(long)]
        keep_stale: bool,
        /// Let agents land on it directly, without review.
        #[arg(long)]
        agents_direct: bool,
        /// Who may land without review: a login, a team `org/team`, or `admins` (repeatable).
        #[arg(long = "bypass")]
        bypass: Vec<String>,
        /// Stricter review for some paths or for agents' changes: `[agents:]N[:path,path]`,
        /// e.g. `agents:2:auth` (two people approve when an agent changes auth/). Repeatable.
        #[arg(long = "rule")]
        rules: Vec<String>,
        /// A change request landing by itself is first tested (Runs) as it would land, on the
        /// branch's current tip; the branch moves to exactly what passed.
        #[arg(long)]
        test_before_landing: bool,
    },
    /// Stop protecting a branch or prefix.
    Remove { pattern: String },
}

fn explain(s: tonic::Status) -> anyhow::Error {
    if s.code() == tonic::Code::Unimplemented {
        anyhow!("this server has no change requests; they come with `grovedockd server --platform`")
    } else {
        anyhow!("{}", s.message())
    }
}

fn state_name(s: i32) -> &'static str {
    match ChangeRequestState::try_from(s).unwrap_or(ChangeRequestState::Unspecified) {
        ChangeRequestState::Open => "open",
        ChangeRequestState::Draft => "draft",
        ChangeRequestState::Landed => "landed",
        ChangeRequestState::Closed => "closed",
        ChangeRequestState::Unspecified => "?",
    }
}

fn print(cr: &ChangeRequest) {
    println!("#{} {} [{}]", cr.number, cr.title, state_name(cr.state));
    println!(
        "  {} wants to land {} into {}: {} files, +{} -{}",
        cr.author, cr.source, cr.target, cr.files, cr.additions, cr.deletions
    );
    for r in &cr.reviews {
        let v = match Verdict::try_from(r.verdict).unwrap_or(Verdict::Unspecified) {
            Verdict::Approved => "approved",
            Verdict::ChangesRequested => "requested changes",
            _ => "commented",
        };
        let stale = if r.reviewed_tip != cr.source_tip { " (on an older version)" } else { "" };
        println!("  {} {v}{stale}", r.reviewer);
    }
    for c in &cr.checks {
        let s = match CheckState::try_from(c.state).unwrap_or(CheckState::Unspecified) {
            CheckState::Passed => "passed",
            CheckState::Failed => "failed",
            CheckState::Skipped => "skipped",
            _ => "pending",
        };
        println!("  check {}: {s}", c.name);
    }
    if cr.state == ChangeRequestState::Open as i32 || cr.state == ChangeRequestState::Draft as i32 {
        if cr.blockers.is_empty() {
            println!("  ready to land: gd cr land {}", cr.number);
        } else {
            for b in &cr.blockers {
                println!("  blocked: {b}");
            }
        }
    }
}

/// Opens a change request for the workspace's committed work on a protected branch: pushes it
/// to a new branch `review/ID` and proposes that branch for the protected one. Returns the
/// change request's number and the branch.
pub async fn propose(
    t: Target,
    remote: &dyn gd::remote::Remote,
    ws: &gd::workspace::Workspace,
) -> Result<(u64, String)> {
    let Target { mut client, repo, .. } = t;
    let tip = ws.base.context("nothing to propose: commit first")?;
    let branch = format!("review/{}", &tip.to_hex()[..12]);
    if !remote.compare_and_swap(&branch, None, tip).await? && remote.get_ref(&branch).await? != Some(tip) {
        anyhow::bail!("branch {branch} already exists with other work");
    }
    let message =
        remote.log(&tip.to_hex(), "", 1).await?.into_iter().next().map(|c| c.changeset.message).unwrap_or_default();
    let (title, body) = message.split_once('\n').unwrap_or((&message, ""));
    let title =
        if title.trim().is_empty() { format!("Changes for {}", ws.config.ref_name) } else { title.trim().to_owned() };
    let cr = client
        .create_change_request(CreateChangeRequestRequest {
            repository: Some(repo),
            source: branch.clone(),
            target: ws.config.ref_name.clone(),
            title,
            body: body.trim().to_owned(),
            ..Default::default()
        })
        .await
        .map_err(explain)?
        .into_inner();
    Ok((cr.number, branch))
}

pub struct Target {
    pub client: ReviewsClient<Ch>,
    pub issues: grovedock_proto::issues_client::IssuesClient<Ch>,
    pub repo: RepositoryRef,
    /// The workspace's branch, for `cr create` without `--source`.
    pub branch: Option<String>,
}

pub async fn cr(t: Target, cmd: CrCmd) -> Result<()> {
    let Target { mut client, repo, branch, .. } = t;
    let r = |number| ChangeRequestRef { repository: Some(repo.clone()), number };
    let update = |number, state: ChangeRequestState| UpdateChangeRequestRequest {
        r#ref: Some(ChangeRequestRef { repository: Some(repo.clone()), number }),
        fields: vec!["state".into()],
        state: state as i32,
        ..Default::default()
    };
    match cmd {
        CrCmd::Create { title, body, source, target, draft, reviewers, upstream } => {
            let source = source.or(branch).context("give --source BRANCH (or run this in a workspace)")?;
            // To the repository it was forked from: there, from this one.
            let (into, from) = match upstream {
                Some(u) => {
                    let (owner, name) = u.split_once('/').context("give --upstream as OWNER/NAME")?;
                    (RepositoryRef { owner: owner.into(), name: name.into() }, Some(repo.clone()))
                }
                None => (repo.clone(), None),
            };
            let cr = client
                .create_change_request(CreateChangeRequestRequest {
                    milestone: 0,
                    repository: Some(into),
                    source,
                    target,
                    title,
                    body,
                    draft,
                    reviewers,
                    source_repository: from,
                })
                .await
                .map_err(explain)?
                .into_inner();
            print(&cr);
        }
        CrCmd::List { state, author, query, limit } => {
            let states = match state {
                StateArg::Open => vec![ChangeRequestState::Open as i32, ChangeRequestState::Draft as i32],
                StateArg::Draft => vec![ChangeRequestState::Draft as i32],
                StateArg::Landed => vec![ChangeRequestState::Landed as i32],
                StateArg::Closed => vec![ChangeRequestState::Closed as i32],
                StateArg::All => vec![],
            };
            let list = client
                .list_change_requests(ListChangeRequestsRequest {
                    repository: Some(repo.clone()),
                    states,
                    author,
                    query,
                    limit,
                    ..Default::default()
                })
                .await
                .map_err(explain)?
                .into_inner();
            let shown = list.change_requests.len() as u64;
            if list.total > shown {
                eprintln!("{shown} of {} shown; --limit shows more, --query narrows", list.total);
            }
            for c in list.change_requests {
                let open = c.state == ChangeRequestState::Open as i32 || c.state == ChangeRequestState::Draft as i32;
                let status = match (open, c.blockers.is_empty()) {
                    (false, _) => String::new(),
                    (true, true) => "ready to land".to_owned(),
                    (true, false) => c.blockers.join("; "),
                };
                println!("#{}\t{}\t{}\t{}\t{status}", c.number, state_name(c.state), c.author, c.title);
            }
        }
        CrCmd::View { number } => print(&client.get_change_request(r(number)).await.map_err(explain)?.into_inner()),
        CrCmd::Comment { number, message, path, line } => {
            client
                .add_comment(AddCommentRequest {
                    r#ref: Some(r(number)),
                    body: message,
                    path: path.unwrap_or_default(),
                    line: line.unwrap_or(0),
                    ..Default::default()
                })
                .await
                .map_err(explain)?;
            println!("commented on #{number}");
        }
        CrCmd::Review { number, verdict, message } => {
            let v = match verdict {
                ReviewArg::Approve => Verdict::Approved,
                ReviewArg::Changes => Verdict::ChangesRequested,
                ReviewArg::Comment => Verdict::Commented,
            };
            let cr = client
                .submit_review(SubmitReviewRequest { r#ref: Some(r(number)), verdict: v as i32, body: message })
                .await
                .map_err(explain)?
                .into_inner();
            print(&cr);
        }
        CrCmd::Check { number, name, state, url, summary } => {
            let s = match state {
                CheckArg::Pending => CheckState::Pending,
                CheckArg::Passed => CheckState::Passed,
                CheckArg::Failed => CheckState::Failed,
                CheckArg::Skipped => CheckState::Skipped,
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let check = Check {
                name,
                state: s as i32,
                summary,
                details_url: url,
                started_at_ms: now,
                finished_at_ms: if s == CheckState::Pending { 0 } else { now },
            };
            let cr = client
                .report_check(ReportCheckRequest { r#ref: Some(r(number)), check: Some(check) })
                .await
                .map_err(explain)?
                .into_inner();
            print(&cr);
        }
        CrCmd::Land { number } => {
            let out = client.land(r(number)).await.map_err(explain)?.into_inner();
            let id = out.landed_as.iter().map(|b| format!("{b:02x}")).collect::<String>();
            println!("landed #{number} as {}", &id[..id.len().min(12)]);
        }
        CrCmd::Rebase { number } => print(&client.rebase(r(number)).await.map_err(explain)?.into_inner()),
        CrCmd::Conflicts { number, write } => {
            let report = client.conflicts(r(number)).await.map_err(explain)?.into_inner();
            if report.files.is_empty() {
                println!("#{number} doesn't clash with {}", report.target);
            }
            for f in &report.files {
                let state = if f.clean {
                    "merges cleanly".to_owned()
                } else if f.clashes > 0 {
                    format!("{} clash(es)", f.clashes)
                } else {
                    "choose a side".to_owned()
                };
                println!("{} ({}, {state})", f.path, f.kind);
                match &write {
                    Some(dir) => {
                        let out = dir.join(&f.path);
                        if let Some(parent) = out.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&out, if f.merged.is_empty() { &f.source } else { &f.merged })?;
                        println!("  wrote {}", out.display());
                    }
                    None if !f.clean && !f.merged.is_empty() => println!("{}", f.merged),
                    None => {}
                }
            }
            if write.is_some() && report.files.iter().any(|f| !f.clean) {
                eprintln!("edit the files, remove the markers, then `gd cr resolve {number} --from DIR`");
            }
        }
        CrCmd::UndoAgent { agent, since, target, dry_run } => {
            let agent = if agent.starts_with("agent/") { agent } else { format!("agent/{agent}") };
            let since_ms = since_ms(&since)?;
            let plan = client
                .revert_agent(grovedock_proto::RevertAgentRequest {
                    repository: Some(repo.clone()),
                    agent: agent.clone(),
                    since_ms,
                    target,
                    dry_run,
                })
                .await
                .map_err(explain)?
                .into_inner();
            if plan.changes.is_empty() {
                println!("{agent} landed nothing there since then");
                return Ok(());
            }
            println!("{} change(s) by {agent}, newest first:", plan.changes.len());
            for c in &plan.changes {
                println!(
                    "  {} {}",
                    grovedock_core::Id::from_slice(&c.id).map(|i| i.short()).unwrap_or_default(),
                    c.title
                );
            }
            println!(
                "undoes: {}",
                if plan.files.is_empty() { "nothing (already undone)".into() } else { plan.files.join(", ") }
            );
            if !plan.clashes.is_empty() {
                println!("left as they are (changed in the same places since): {}", plan.clashes.join(", "));
            }
            match plan.change_request {
                Some(cr) => print(&cr),
                None if dry_run => eprintln!("run again without --dry-run to open the change request"),
                None => {}
            }
        }
        CrCmd::Resolve { number, from, deletes } => {
            let report = client.conflicts(r(number)).await.map_err(explain)?.into_inner();
            let mut files = Vec::new();
            for f in report.files.iter().filter(|f| !f.clean) {
                if deletes.contains(&f.path) {
                    files.push(grovedock_proto::FileResolution {
                        path: f.path.clone(),
                        content: String::new(),
                        delete: true,
                    });
                    continue;
                }
                let path = from.join(&f.path);
                let content = std::fs::read_to_string(&path).map_err(|e| {
                    anyhow!(
                        "reading {}: {e}; write the settled file there, or pass --delete {}",
                        path.display(),
                        f.path
                    )
                })?;
                files.push(grovedock_proto::FileResolution { path: f.path.clone(), content, delete: false });
            }
            if files.is_empty() {
                print(&client.rebase(r(number)).await.map_err(explain)?.into_inner());
            } else {
                let req = grovedock_proto::ResolveConflictsRequest { r#ref: Some(r(number)), files };
                print(&client.resolve_conflicts(req).await.map_err(explain)?.into_inner());
            }
        }
        CrCmd::Ready { number } => print(
            &client
                .update_change_request(update(number, ChangeRequestState::Open))
                .await
                .map_err(explain)?
                .into_inner(),
        ),
        CrCmd::Close { number } => print(
            &client
                .update_change_request(update(number, ChangeRequestState::Closed))
                .await
                .map_err(explain)?
                .into_inner(),
        ),
        CrCmd::Reopen { number } => print(
            &client
                .update_change_request(update(number, ChangeRequestState::Open))
                .await
                .map_err(explain)?
                .into_inner(),
        ),
    }
    Ok(())
}

pub async fn protect(t: Target, cmd: ProtectCmd) -> Result<()> {
    let Target { mut client, repo, .. } = t;
    match cmd {
        ProtectCmd::List => {
            for p in client.list_protections(repo).await.map_err(explain)?.into_inner().protections {
                let checks = if p.required_checks.is_empty() {
                    "no checks".to_owned()
                } else {
                    format!("checks: {}", p.required_checks.join(", "))
                };
                let bypass =
                    if p.bypass.is_empty() { String::new() } else { format!("; bypass: {}", p.bypass.join(", ")) };
                let agents = if p.agents_land_directly { "; agents land directly" } else { "" };
                let rules: String = p
                    .rules
                    .iter()
                    .map(|r| {
                        let who = if r.agents_only { "an agent" } else { "anyone" };
                        let what = if r.paths.is_empty() { "anything".to_owned() } else { r.paths.join(", ") };
                        format!("; {} from people when {who} changes {what}", r.human_approvals)
                    })
                    .collect();
                println!("{}\t{} approvals, {checks}{agents}{bypass}{rules}", p.pattern, p.required_approvals);
            }
        }
        ProtectCmd::Set {
            pattern,
            approvals,
            checks,
            keep_stale,
            agents_direct,
            bypass,
            rules,
            test_before_landing,
        } => {
            let rules = rules.iter().map(|r| parse_rule(r)).collect::<Result<Vec<_>>>()?;
            let p = client
                .set_protection(Protection {
                    repository: Some(repo),
                    pattern,
                    required_approvals: approvals,
                    required_checks: checks,
                    dismiss_stale: !keep_stale,
                    agents_land_directly: agents_direct,
                    bypass,
                    rules,
                    test_before_landing,
                })
                .await
                .map_err(explain)?
                .into_inner();
            let n = p.required_approvals;
            println!(
                "protected {}: changes land through a change request with {n} {}",
                p.pattern,
                if n == 1 { "approval" } else { "approvals" }
            );
        }
        ProtectCmd::Remove { pattern } => {
            client
                .delete_protection(ProtectionRef { repository: Some(repo), pattern: pattern.clone() })
                .await
                .map_err(explain)?;
            println!("{pattern} is no longer protected");
        }
    }
    Ok(())
}

/// `[agents:]N[:path,path]` as a review rule.
fn parse_rule(s: &str) -> Result<grovedock_proto::ReviewRule> {
    let (agents_only, rest) = match s.strip_prefix("agents:") {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (n, paths) = rest.split_once(':').unwrap_or((rest, ""));
    let human_approvals: u32 =
        n.parse().map_err(|_| anyhow::anyhow!("`{s}`: a rule is [agents:]N[:path,path], e.g. agents:2:auth"))?;
    Ok(grovedock_proto::ReviewRule {
        agents_only,
        human_approvals,
        paths: paths.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect(),
    })
}

#[cfg(test)]
mod rule_tests {
    #[test]
    fn rules_parse() {
        let r = super::parse_rule("agents:2:auth, billing").unwrap();
        assert!(r.agents_only && r.human_approvals == 2 && r.paths == ["auth", "billing"]);
        let r = super::parse_rule("1").unwrap();
        assert!(!r.agents_only && r.human_approvals == 1 && r.paths.is_empty());
        assert!(super::parse_rule("agents:two").is_err());
    }
}

/// A time to look back to: `30m`, `2h`, `1d`, `2026-10-20` or `2026-10-20 14:05` (UTC).
fn since_ms(s: &str) -> Result<i64> {
    let s = s.trim();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64;
    if let Some(n) =
        s.strip_suffix('m').or(s.strip_suffix('h')).or(s.strip_suffix('d')).and_then(|n| n.parse::<i64>().ok())
    {
        let unit = match s.chars().last() {
            Some('m') => 60_000,
            Some('h') => 3_600_000,
            _ => 86_400_000,
        };
        return Ok(now - n * unit);
    }
    let (date, time) = s.split_once([' ', 'T']).unwrap_or((s, "00:00"));
    let d: Vec<i64> = date.split('-').filter_map(|x| x.parse().ok()).collect();
    let t: Vec<i64> = time.trim_end_matches('Z').split(':').filter_map(|x| x.parse().ok()).collect();
    let [y, m, day] = d[..] else { anyhow::bail!("`{s}` isn't a time: use 30m, 2h, 1d or 2026-10-20 14:05") };
    let (hh, mm) = (t.first().copied().unwrap_or(0), t.get(1).copied().unwrap_or(0));
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok(days * 86_400_000 + hh * 3_600_000 + mm * 60_000)
}

#[cfg(test)]
mod since_tests {
    #[test]
    fn times_to_look_back_to() {
        assert_eq!(super::since_ms("2026-10-20").unwrap(), 1_792_454_400_000);
        assert_eq!(super::since_ms("2026-10-20 14:05").unwrap(), 1_792_454_400_000 + (14 * 60 + 5) * 60_000);
        assert_eq!(super::since_ms("1970-01-01T00:01Z").unwrap(), 60_000);
        assert!(super::since_ms("2h").unwrap() > super::since_ms("1d").unwrap());
        assert!(super::since_ms("yesterday").is_err());
    }
}
