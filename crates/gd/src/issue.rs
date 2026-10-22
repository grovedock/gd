//! `gd issue`: work to do, for people and agents.

use anyhow::{Result, anyhow};
use clap::{Subcommand, ValueEnum};
use gd::remote::Ch;
use grovedock_proto::issues_client::IssuesClient;
use grovedock_proto::{
    AddIssueCommentRequest, CreateIssueRequest, Issue, IssueRef, IssueState, ListIssuesRequest, RepositoryRef,
    UpdateIssueRequest,
};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum IssueStateArg {
    Open,
    Closed,
    All,
}

#[derive(Subcommand)]
pub enum IssueCmd {
    /// Open an issue.
    Create {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        body: String,
        /// Who should work on it, a person or an agent (repeatable).
        #[arg(long = "assignee")]
        assignees: Vec<String>,
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    List {
        #[arg(long, value_enum, default_value = "open")]
        state: IssueStateArg,
        #[arg(long, default_value = "")]
        assignee: String,
        #[arg(long, default_value = "")]
        label: String,
        /// Search, e.g. `coupon label:bug assignee:@me -label:wontfix`.
        #[arg(long, short, default_value = "")]
        query: String,
        /// How many to show (at most 200); the total is printed after.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    View {
        number: u64,
    },
    Comment {
        number: u64,
        #[arg(short, long)]
        message: String,
    },
    /// Give an issue to people or agents (replaces the assignees).
    Assign {
        number: u64,
        assignees: Vec<String>,
    },
    /// Close an issue as done (or `--not-planned`).
    Close {
        number: u64,
        #[arg(long)]
        not_planned: bool,
    },
    Reopen {
        number: u64,
    },
}

fn explain(s: tonic::Status) -> anyhow::Error {
    if s.code() == tonic::Code::Unimplemented {
        anyhow!("this server has no issues; they come with `grovedockd server --platform`")
    } else {
        anyhow!("{}", s.message())
    }
}

fn state(s: i32) -> &'static str {
    match IssueState::try_from(s).unwrap_or(IssueState::Unspecified) {
        IssueState::Open => "open",
        IssueState::Completed => "done",
        IssueState::NotPlanned => "not planned",
        IssueState::Unspecified => "?",
    }
}

fn print(i: &Issue) {
    println!("#{} {} [{}]", i.number, i.title, state(i.state));
    println!(
        "  opened by {}{}",
        i.author,
        if i.assignees.is_empty() { String::new() } else { format!(", assigned to {}", i.assignees.join(", ")) }
    );
    if !i.labels.is_empty() {
        println!("  labels: {}", i.labels.join(", "));
    }
    if !i.fixed_by.is_empty() {
        println!("  fixed by: {}", i.fixed_by.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(", "));
    }
    if !i.body.trim().is_empty() {
        println!("\n{}", i.body.trim());
    }
}

pub async fn issue(mut client: IssuesClient<Ch>, repo: RepositoryRef, cmd: IssueCmd) -> Result<()> {
    let r = |number| IssueRef { repository: Some(repo.clone()), number };
    let update = |number, fields: &[&str]| UpdateIssueRequest {
        r#ref: Some(r(number)),
        fields: fields.iter().map(|f| f.to_string()).collect(),
        ..Default::default()
    };
    match cmd {
        IssueCmd::Create { title, body, assignees, labels } => {
            let i = client
                .create_issue(CreateIssueRequest {
                    milestone: 0,
                    repository: Some(repo.clone()),
                    title,
                    body,
                    labels,
                    assignees,
                })
                .await
                .map_err(explain)?
                .into_inner();
            print(&i);
        }
        IssueCmd::List { state: s, assignee, label, query, limit } => {
            let states = match s {
                IssueStateArg::Open => vec![IssueState::Open as i32],
                IssueStateArg::Closed => vec![IssueState::Completed as i32, IssueState::NotPlanned as i32],
                IssueStateArg::All => {
                    vec![IssueState::Open as i32, IssueState::Completed as i32, IssueState::NotPlanned as i32]
                }
            };
            let list = client
                .list_issues(ListIssuesRequest {
                    repository: Some(repo.clone()),
                    states,
                    assignee,
                    label,
                    query,
                    limit,
                    ..Default::default()
                })
                .await
                .map_err(explain)?
                .into_inner();
            let shown = list.issues.len() as u64;
            for i in list.issues {
                println!("#{}\t{}\t{}\t{}", i.number, state(i.state), i.assignees.join(","), i.title);
            }
            if list.total > shown {
                eprintln!("{shown} of {} shown; --limit shows more, --query narrows", list.total);
            }
        }
        IssueCmd::View { number } => print(&client.get_issue(r(number)).await.map_err(explain)?.into_inner()),
        IssueCmd::Comment { number, message } => {
            client
                .add_issue_comment(AddIssueCommentRequest { r#ref: Some(r(number)), body: message })
                .await
                .map_err(explain)?;
            println!("commented on #{number}");
        }
        IssueCmd::Assign { number, assignees } => {
            let req = UpdateIssueRequest { assignees, ..update(number, &["assignees"]) };
            print(&client.update_issue(req).await.map_err(explain)?.into_inner());
        }
        IssueCmd::Close { number, not_planned } => {
            let s = if not_planned { IssueState::NotPlanned } else { IssueState::Completed };
            let req = UpdateIssueRequest { state: s as i32, ..update(number, &["state"]) };
            print(&client.update_issue(req).await.map_err(explain)?.into_inner());
        }
        IssueCmd::Reopen { number } => {
            let req = UpdateIssueRequest { state: IssueState::Open as i32, ..update(number, &["state"]) };
            print(&client.update_issue(req).await.map_err(explain)?.into_inner());
        }
    }
    Ok(())
}
