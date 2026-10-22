//! `gd org`, `gd team`, `gd token`, `gd repo grant` and `gd admin`: who may use a server that
//! hosts many repositories (`grovedockd server --platform`).

use anyhow::{Context, Result, anyhow};
use clap::{Subcommand, ValueEnum};
use gd::remote::Ch;
use grovedock_proto::platform_client::PlatformClient;
use grovedock_proto::{
    CreateOrganizationRequest, CreateTokenRequest, Empty, GetUsageRequest, LinkIdentityRequest, OrgName, RepositoryRef,
    RevokeTokenRequest, Role, SetGrantRequest, SetLimitsRequest, SetMemberRequest, SetSuspendedRequest, Team, TeamRef,
};

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// A role, as typed on the command line.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RoleArg {
    Read,
    Triage,
    Write,
    Maintain,
    Admin,
    /// Removes the member or grant.
    None,
}

impl From<RoleArg> for Role {
    fn from(r: RoleArg) -> Self {
        match r {
            RoleArg::Read => Role::Read,
            RoleArg::Triage => Role::Triage,
            RoleArg::Write => Role::Write,
            RoleArg::Maintain => Role::Maintain,
            RoleArg::Admin => Role::Admin,
            RoleArg::None => Role::Unspecified,
        }
    }
}

fn role_name(r: i32) -> &'static str {
    match Role::try_from(r).unwrap_or(Role::Unspecified) {
        Role::Unspecified => "-",
        Role::Read => "read",
        Role::Triage => "triage",
        Role::Write => "write",
        Role::Maintain => "maintain",
        Role::Admin => "admin",
    }
}

#[derive(Subcommand)]
pub enum OrgCmd {
    /// Organisations you belong to (all of them, for site administrators).
    List,
    /// Create an organisation; you become its administrator. Site administrators only.
    Create {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// List an organisation's members and their roles.
    Members { org: String },
    /// Add a member, change their role, or remove them with `none`.
    Member { org: String, login: String, role: RoleArg },
    /// The organisation's plan, limits and use: active agents this month, people, storage.
    /// Organisation administrators.
    Usage {
        org: String,
        /// A past month, YYYY-MM (default: this month, UTC).
        #[arg(long, default_value = "")]
        month: String,
    },
}

#[derive(Subcommand)]
pub enum TeamCmd {
    /// List an organisation's teams.
    List { org: String },
    /// Create a team, or replace its members: `gd team set lumen/platform --member mira`.
    Set {
        /// ORG/TEAM
        team: String,
        /// A member of the organisation (repeatable).
        #[arg(long = "member")]
        members: Vec<String>,
        #[arg(long, default_value = "")]
        name: String,
    },
    /// Delete a team and its grants.
    Delete {
        /// ORG/TEAM
        team: String,
    },
}

#[derive(Subcommand)]
pub enum WebhookCmd {
    /// A repository's webhooks: `gd webhook list lumen/fleet`.
    List {
        repo: String,
    },
    /// Call URL when something happens in the repository. Prints the signing secret once.
    Add {
        repo: String,
        url: String,
        /// Only these events (repeatable): push, change_request, review, comment, check, issue.
        #[arg(long = "event")]
        events: Vec<String>,
    },
    /// Send a ping now and show how it went.
    Ping {
        id: String,
    },
    /// Recent deliveries and their answers.
    Log {
        id: String,
    },
    Remove {
        id: String,
    },
}

#[derive(Subcommand)]
pub enum TokenCmd {
    /// Your tokens.
    List,
    /// Create a token. Prints it once: store it as GD_TOKEN.
    Create {
        name: String,
        /// Act as an agent you answer for, e.g. `agent/refactor-07` (created if new).
        #[arg(long = "as")]
        acts_as: Option<String>,
        /// Only these repositories, OWNER/NAME or OWNER/* for all of an organisation's (repeatable); default: all you can reach.
        #[arg(long = "repo")]
        repositories: Vec<String>,
        /// Highest role the token may use.
        #[arg(long, value_enum, default_value = "write")]
        role: RoleArg,
        /// Days until it expires (1–366).
        #[arg(long, default_value_t = 90)]
        days: i64,
    },
    /// Revoke a token by its id.
    Revoke { id: String },
}

#[derive(Subcommand)]
pub enum AdminCmd {
    /// Ways people sign in from a browser.
    Identity {
        #[command(subcommand)]
        action: IdentityCmd,
    },
    /// Organisations' plans and limits (enforced with `grovedockd server --multi-tenant`).
    Limits {
        #[command(subcommand)]
        action: LimitsCmd,
    },
    /// Incidents on the public status page (/status).
    Incident {
        #[command(subcommand)]
        action: IncidentCmd,
    },
}

#[derive(Subcommand)]
pub enum IncidentCmd {
    /// Open one: `gd admin incident open "Runs are slow" --impact degraded --part Runs --text "Jobs wait 10 minutes for a runner; we're adding capacity."`
    Open {
        title: String,
        /// outage, degraded or maintenance.
        #[arg(long)]
        impact: String,
        /// A part it touches (repeatable): Web and API, Git over HTTPS, Runs, Hosted runners, Sign-in and email.
        #[arg(long = "part")]
        parts: Vec<String>,
        /// What's happening.
        #[arg(long)]
        text: String,
    },
    /// Add an update; --resolve when it's over.
    Update {
        id: u64,
        #[arg(long, default_value = "")]
        text: String,
        #[arg(long)]
        resolve: bool,
    },
    /// What /status shows now.
    Show,
}

#[derive(Subcommand)]
pub enum LimitsCmd {
    /// The limits that apply to an organisation and what it uses.
    Show { org: String },
    /// Change some limits; the others stay. A number, or `unlimited`.
    Set {
        org: String,
        #[arg(long)]
        seats: Option<String>,
        /// Agents that land a change in a calendar month.
        #[arg(long)]
        agents: Option<String>,
        /// e.g. 10GiB.
        #[arg(long)]
        storage: Option<String>,
        #[arg(long)]
        private_repos: Option<String>,
        /// Calls a minute per person or agent, e.g. 300.
        #[arg(long)]
        rate: Option<String>,
        /// Minutes a month on the server's hosted runners, e.g. 2000.
        #[arg(long)]
        hosted_minutes: Option<String>,
        /// Jobs at once on hosted runners, e.g. 1.
        #[arg(long)]
        hosted_jobs: Option<String>,
        /// The plan's name as people see it.
        #[arg(long)]
        plan: Option<String>,
    },
    /// Stop an organisation writing (it can still read).
    Suspend { org: String },
    /// Let a suspended organisation write again.
    Restore { org: String },
}

#[derive(Subcommand)]
pub enum IdentityCmd {
    /// Let an identity sign in as an existing account, e.g. the first administrator:
    /// `gd admin identity link kabir --provider email --subject kabir@example.com`.
    Link {
        login: String,
        /// `email`, or a sign-in provider's id such as `google`.
        #[arg(long)]
        provider: String,
        /// The email address, or the provider's `sub` for the person.
        #[arg(long)]
        subject: String,
    },
}

/// `unlimited`, a number, or (for storage) a size such as `10GiB`.
fn limit_arg(v: &str, size: bool) -> Result<Option<u64>> {
    if v == "unlimited" {
        return Ok(None);
    }
    let split = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
    let (n, unit) = v.split_at(split);
    let n: u64 = n.parse().with_context(|| format!("`{v}` is not a number or `unlimited`"))?;
    let mul: u64 = match (unit, size) {
        ("", _) | ("B", true) => 1,
        ("KiB", true) => 1 << 10,
        ("MiB", true) => 1 << 20,
        ("GiB", true) => 1 << 30,
        ("TiB", true) => 1 << 40,
        ("KB", true) => 1_000,
        ("MB", true) => 1_000_000,
        ("GB", true) => 1_000_000_000,
        ("TB", true) => 1_000_000_000_000,
        _ => return Err(anyhow!("`{v}` is not a number{}", if size { " or size such as 10GiB" } else { "" })),
    };
    Ok(Some(n.checked_mul(mul).with_context(|| format!("`{v}` is too large"))?))
}

fn show_limit(v: Option<u64>, size: bool) -> String {
    match v {
        None => "unlimited".into(),
        Some(n) if size => bytes(n),
        Some(n) => n.to_string(),
    }
}

fn bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64),
        n => format!("{n} B"),
    }
}

/// `gd org usage` and `gd admin limits show`.
fn print_usage(u: &grovedock_proto::Usage) {
    let l = u.limits.clone().unwrap_or_default();
    let plan = if l.plan_label.is_empty() { "no plan".to_owned() } else { l.plan_label.clone() };
    println!(
        "{}: {plan}{}{}",
        u.org,
        if l.suspended { ", suspended" } else { "" },
        if u.enforced { "" } else { " (limits not enforced on this server)" }
    );
    println!("  active agents in {}: {} of {}", u.month, u.active_agents.len(), show_limit(l.active_agents, false));
    println!("  people: {} of {}", u.seats, show_limit(l.seats, false));
    println!(
        "  storage: {} of {} (peak this month {})",
        bytes(u.storage_bytes),
        show_limit(l.storage_bytes, true),
        bytes(u.peak_storage_bytes)
    );
    println!("  private repositories: {} of {}", u.private_repos, show_limit(l.private_repos, false));
    println!("  calls a minute, per person or agent: {}", show_limit(l.calls_per_minute, false));
    println!(
        "  hosted runner minutes in {}: {} of {} ({} job(s) at once)",
        u.month,
        u.hosted_minutes_used,
        show_limit(l.hosted_minutes, false),
        show_limit(l.hosted_jobs, false)
    );
    for a in &u.active_agents {
        println!("  {}\tfirst landed {}", a.login, crate::fmt_time(a.first_at_ms));
    }
}

async fn incident(mut p: grovedock_proto::status_client::StatusClient<Ch>, cmd: IncidentCmd) -> Result<()> {
    let show = |i: &grovedock_proto::Incident| {
        println!(
            "#{} {} ({}; {}){}",
            i.id,
            i.title,
            i.impact,
            i.components.join(", "),
            if i.resolved_at_ms > 0 { ", resolved" } else { "" }
        );
        for u in &i.updates {
            println!("  {}  {}", crate::fmt_time(u.at_ms), u.text);
        }
    };
    match cmd {
        IncidentCmd::Open { title, impact, parts, text } => {
            let i = p
                .open_incident(grovedock_proto::OpenIncidentRequest { title, impact, components: parts, text })
                .await
                .map_err(explain)?
                .into_inner();
            show(&i);
        }
        IncidentCmd::Update { id, text, resolve } => {
            let i = p
                .update_incident(grovedock_proto::UpdateIncidentRequest { id, text, resolve })
                .await
                .map_err(explain)?
                .into_inner();
            show(&i);
        }
        IncidentCmd::Show => {
            let s = p.get_service_status(grovedock_proto::Empty {}).await.map_err(explain)?.into_inner();
            for c in &s.components {
                println!(
                    "{:<18} {}{}",
                    c.name,
                    c.state.replace('_', " "),
                    if c.detail.is_empty() { String::new() } else { format!(": {}", c.detail) }
                );
            }
            for i in &s.incidents {
                show(i);
            }
        }
    }
    Ok(())
}

fn explain(s: tonic::Status) -> anyhow::Error {
    if s.code() == tonic::Code::Unimplemented {
        anyhow!("this server hosts one repository; start grovedockd with --platform to manage organisations and tokens")
    } else {
        anyhow!("{}", s.message())
    }
}

fn split(s: &str, what: &str) -> Result<(String, String)> {
    let (a, b) = s.split_once('/').with_context(|| format!("give the {what} as OWNER/NAME"))?;
    Ok((a.to_owned(), b.to_owned()))
}

pub async fn org(mut p: PlatformClient<Ch>, cmd: OrgCmd) -> Result<()> {
    match cmd {
        OrgCmd::List => {
            for o in p.list_organizations(Empty {}).await.map_err(explain)?.into_inner().organizations {
                println!(
                    "{}\t{} repositories\t{} people\t{} agents\t{}",
                    o.login,
                    o.repositories,
                    o.members,
                    o.agents,
                    role_name(o.viewer_role)
                );
            }
        }
        OrgCmd::Create { name, description } => {
            let o = p
                .create_organization(CreateOrganizationRequest {
                    login: name,
                    display_name: String::new(),
                    description,
                })
                .await
                .map_err(explain)?
                .into_inner();
            println!("created {}; add people with: gd org member {} LOGIN write", o.login, o.login);
        }
        OrgCmd::Members { org } => {
            for m in p.list_members(OrgName { login: org }).await.map_err(explain)?.into_inner().members {
                let login = m.account.map(|a| a.login).unwrap_or_default();
                println!("{login}\t{}\t{}", role_name(m.role), m.teams.join(","));
            }
        }
        OrgCmd::Member { org, login, role } => {
            p.set_member(SetMemberRequest { org: org.clone(), login: login.clone(), role: Role::from(role) as i32 })
                .await
                .map_err(explain)?;
            match role {
                RoleArg::None => println!("removed {login} from {org}"),
                r => println!("{login} is now {} in {org}", role_name(Role::from(r) as i32)),
            }
        }
        OrgCmd::Usage { org, month } => {
            print_usage(&p.get_usage(GetUsageRequest { org, month }).await.map_err(explain)?.into_inner());
        }
    }
    Ok(())
}

async fn limits(mut p: PlatformClient<Ch>, cmd: LimitsCmd) -> Result<()> {
    let usage = |org: &str| GetUsageRequest { org: org.to_owned(), month: String::new() };
    match cmd {
        LimitsCmd::Show { org } => print_usage(&p.get_usage(usage(&org)).await.map_err(explain)?.into_inner()),
        LimitsCmd::Set { org, seats, agents, storage, private_repos, rate, hosted_minutes, hosted_jobs, plan } => {
            let mut l = p.get_usage(usage(&org)).await.map_err(explain)?.into_inner().limits.unwrap_or_default();
            if let Some(v) = seats {
                l.seats = limit_arg(&v, false)?;
            }
            if let Some(v) = agents {
                l.active_agents = limit_arg(&v, false)?;
            }
            if let Some(v) = storage {
                l.storage_bytes = limit_arg(&v, true)?;
            }
            if let Some(v) = private_repos {
                l.private_repos = limit_arg(&v, false)?;
            }
            if let Some(v) = rate {
                l.calls_per_minute = limit_arg(&v, false)?;
            }
            if let Some(v) = hosted_minutes {
                l.hosted_minutes = limit_arg(&v, false)?;
            }
            if let Some(v) = hosted_jobs {
                l.hosted_jobs = limit_arg(&v, false)?;
            }
            if let Some(v) = plan {
                l.plan_label = v;
            }
            p.set_limits(SetLimitsRequest { org: org.clone(), limits: Some(l) }).await.map_err(explain)?;
            print_usage(&p.get_usage(usage(&org)).await.map_err(explain)?.into_inner());
        }
        LimitsCmd::Suspend { org } => {
            p.set_suspended(SetSuspendedRequest { org: org.clone(), suspended: true }).await.map_err(explain)?;
            if p.get_usage(usage(&org)).await.map_err(explain)?.into_inner().enforced {
                println!("{org} is suspended: it can read but not write");
            } else {
                println!(
                    "{org} is marked suspended, but this server doesn't enforce limits (--multi-tenant): it can still write"
                );
            }
        }
        LimitsCmd::Restore { org } => {
            p.set_suspended(SetSuspendedRequest { org: org.clone(), suspended: false }).await.map_err(explain)?;
            println!("{org} can write again");
        }
    }
    Ok(())
}

pub async fn team(mut p: PlatformClient<Ch>, cmd: TeamCmd) -> Result<()> {
    match cmd {
        TeamCmd::List { org } => {
            for t in p.list_teams(OrgName { login: org }).await.map_err(explain)?.into_inner().teams {
                println!("{}/{}\t{}\t{}", t.org, t.slug, t.name, t.members.join(","));
            }
        }
        TeamCmd::Set { team, members, name } => {
            let (org, slug) = split(&team, "team")?;
            let t = p
                .set_team(Team { org, slug, name, description: String::new(), members })
                .await
                .map_err(explain)?
                .into_inner();
            println!(
                "{}/{}: {}",
                t.org,
                t.slug,
                if t.members.is_empty() { "no members".into() } else { t.members.join(", ") }
            );
        }
        TeamCmd::Delete { team } => {
            let (org, slug) = split(&team, "team")?;
            p.delete_team(TeamRef { org, slug }).await.map_err(explain)?;
            println!("deleted {team}");
        }
    }
    Ok(())
}

/// `gd repo grant OWNER/NAME PRINCIPAL ROLE`; without a principal, lists the grants.
pub async fn grant(
    mut p: PlatformClient<Ch>,
    repo: &str,
    principal: Option<String>,
    role: Option<RoleArg>,
) -> Result<()> {
    let (owner, name) = split(repo, "repository")?;
    let r = RepositoryRef { owner, name };
    match (principal, role) {
        (Some(principal), Some(role)) => {
            p.set_grant(SetGrantRequest {
                repository: Some(r),
                principal: principal.clone(),
                role: Role::from(role) as i32,
            })
            .await
            .map_err(explain)?;
            match role {
                RoleArg::None => println!("removed {principal}'s access to {repo}"),
                r => println!("{principal} has {} access to {repo}", role_name(Role::from(r) as i32)),
            }
        }
        (None, None) => {
            for g in p.list_grants(r).await.map_err(explain)?.into_inner().grants {
                println!("{}\t{}", g.principal, role_name(g.role));
            }
        }
        _ => return Err(anyhow!("give both a principal and a role, or neither to list access")),
    }
    Ok(())
}

pub async fn token(mut p: PlatformClient<Ch>, cmd: TokenCmd) -> Result<()> {
    match cmd {
        TokenCmd::List => {
            let now = now_ms();
            for t in p.list_tokens(Empty {}).await.map_err(explain)?.into_inner().tokens {
                let days = ((t.expires_at_ms - now).max(0) + 86_399_999) / 86_400_000;
                let scope = if t.repositories.is_empty() { "all".to_owned() } else { t.repositories.join(",") };
                println!(
                    "{}\t{}\tas {}\t{}\t{scope}\texpires in {days} days",
                    t.id,
                    t.name,
                    t.acts_as,
                    role_name(t.max_role)
                );
            }
        }
        TokenCmd::Create { name, acts_as, repositories, role, days } => {
            anyhow::ensure!((1..=366).contains(&days), "--days must be between 1 and 366");
            let expires_at_ms = now_ms() + days * 86_400_000;
            let r = p
                .create_token(CreateTokenRequest {
                    name,
                    acts_as: acts_as.unwrap_or_default(),
                    repositories,
                    max_role: Role::from(role) as i32,
                    expires_at_ms,
                })
                .await
                .map_err(explain)?
                .into_inner();
            let t = r.token.unwrap_or_default();
            eprintln!("token {} acts as {}, expires in {days} days; it is shown once:", t.id, t.acts_as);
            println!("{}", r.secret);
        }
        TokenCmd::Revoke { id } => {
            p.revoke_token(RevokeTokenRequest { id: id.clone() }).await.map_err(explain)?;
            println!("revoked {id}");
        }
    }
    Ok(())
}

pub async fn webhook(mut p: PlatformClient<Ch>, cmd: WebhookCmd) -> Result<()> {
    use grovedock_proto::{CreateWebhookRequest, WebhookRef};
    let status = |s: i32| match s {
        0 => "not sent yet".to_owned(),
        -1 => "no answer".to_owned(),
        s => format!("HTTP {s}"),
    };
    match cmd {
        WebhookCmd::List { repo } => {
            let (owner, name) = split(&repo, "repository")?;
            for h in p.list_webhooks(RepositoryRef { owner, name }).await.map_err(explain)?.into_inner().webhooks {
                let events = if h.events.is_empty() { "every event".to_owned() } else { h.events.join(",") };
                println!("{}\t{}\t{events}\tlast: {}", h.id, h.url, status(h.last_status));
            }
        }
        WebhookCmd::Add { repo, url, events } => {
            let (owner, name) = split(&repo, "repository")?;
            let r = p
                .create_webhook(CreateWebhookRequest {
                    repository: Some(RepositoryRef { owner, name }),
                    url,
                    events,
                    secret: String::new(),
                })
                .await
                .map_err(explain)?
                .into_inner();
            let h = r.webhook.unwrap_or_default();
            eprintln!(
                "webhook {} calls {}; verify X-Grovedock-Signature-256 with this secret, shown once:",
                h.id, h.url
            );
            println!("{}", r.secret);
        }
        WebhookCmd::Ping { id } => {
            let d = p.ping_webhook(WebhookRef { id }).await.map_err(explain)?.into_inner();
            println!(
                "{}{}",
                status(d.status),
                if d.error.is_empty() { String::new() } else { format!(": {}", d.error) }
            );
        }
        WebhookCmd::Log { id } => {
            for d in p.list_webhook_deliveries(WebhookRef { id }).await.map_err(explain)?.into_inner().deliveries {
                let state = if d.delivered {
                    "delivered"
                } else if d.attempts >= 8 {
                    "gave up"
                } else {
                    "retrying"
                };
                println!("{}\t{}\t{state}\t{}\t{} attempts\t{}", d.id, d.event, status(d.status), d.attempts, d.error);
            }
        }
        WebhookCmd::Remove { id } => {
            p.delete_webhook(WebhookRef { id: id.clone() }).await.map_err(explain)?;
            println!("removed webhook {id}");
        }
    }
    Ok(())
}

pub async fn admin(remote: gd::remote::GrpcRemote, cmd: AdminCmd) -> Result<()> {
    let mut p = remote.platform();
    match cmd {
        AdminCmd::Limits { action } => limits(p, action).await,
        AdminCmd::Incident { action } => incident(remote.status(), action).await,
        AdminCmd::Identity { action: IdentityCmd::Link { login, provider, subject } } => {
            p.link_identity(LinkIdentityRequest {
                login: login.clone(),
                provider: provider.clone(),
                subject: subject.clone(),
            })
            .await
            .map_err(explain)?;
            println!("{provider} {subject} now signs in as {login}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn limits_are_numbers_sizes_or_unlimited() {
        assert_eq!(super::limit_arg("12", false).unwrap(), Some(12));
        assert_eq!(super::limit_arg("unlimited", false).unwrap(), None);
        assert_eq!(super::limit_arg("10GiB", true).unwrap(), Some(10 << 30));
        assert!(super::limit_arg("10GiB", false).is_err(), "a size is not a count");
        assert!(super::limit_arg("lots", false).is_err());
    }
}
