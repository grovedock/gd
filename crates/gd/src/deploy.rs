//! `gd secret`, `gd var` and `gd env`: what Runs' jobs get (secrets and variables, for a
//! repository, one of its environments, or the whole owner) and where they deploy.

use anyhow::{Context, Result};
use clap::Subcommand;
use grovedock_proto::{
    ApproveDeploymentRequest, Environment, EnvironmentRef, RepositoryRef, SecretRef, SetSecretRequest,
    SetVariableRequest, VariableRef,
};

#[derive(Subcommand)]
pub enum SecretCmd {
    /// Set a secret from standard input (so it stays out of shell history), e.g.
    /// `gd secret set DEPLOY_KEY < key.txt`. Only runs of code on a protected branch get it.
    Set {
        name: String,
        /// For jobs that deploy to this environment only.
        #[arg(long)]
        env: Option<String>,
        /// For every repository of the owner.
        #[arg(long)]
        org: bool,
    },
    /// Secrets by name (values are never shown).
    List {
        #[arg(long)]
        org: bool,
    },
    Delete {
        name: String,
        #[arg(long)]
        env: Option<String>,
        #[arg(long)]
        org: bool,
    },
}

#[derive(Subcommand)]
pub enum VarCmd {
    /// Set a variable (`${{ vars.NAME }}`); not secret, so shown to writers.
    Set {
        name: String,
        value: String,
        #[arg(long)]
        env: Option<String>,
        #[arg(long)]
        org: bool,
    },
    List {
        #[arg(long)]
        org: bool,
    },
    Delete {
        name: String,
        #[arg(long)]
        env: Option<String>,
        #[arg(long)]
        org: bool,
    },
}

#[derive(Subcommand)]
pub enum EnvCmd {
    /// Make or change an environment: who approves its deployments, and from which branches.
    Set {
        name: String,
        /// A person who approves (repeatable); none: deployments go at once.
        #[arg(long = "reviewer")]
        reviewers: Vec<String>,
        /// A branch pattern it deploys from, like main or release/* (repeatable); none: any.
        #[arg(long = "branch")]
        branches: Vec<String>,
    },
    List,
    /// Remove an environment with its secrets and variables.
    Delete {
        name: String,
    },
    /// Let run NUMBER's jobs waiting to deploy to NAME go ahead.
    Approve {
        number: u64,
        name: String,
    },
}

/// The repository from `--server SERVER/OWNER/NAME`, or only its owner with `org`.
fn target(server: &str, org: bool) -> Result<(String, RepositoryRef)> {
    let (base, repo) = grovedock_proto::split_repo_url(server);
    let repo = repo.context("name the repository: --server SERVER/OWNER/NAME")?;
    let (owner, name) = repo.split_once('/').context("the repository is OWNER/NAME")?;
    let name = if org { String::new() } else { name.to_owned() };
    Ok((base, RepositoryRef { owner: owner.into(), name }))
}

fn place(env: &Option<String>, r: &RepositoryRef) -> String {
    match env {
        Some(e) => format!("{}/{} ({e})", r.owner, r.name),
        None if r.name.is_empty() => format!("all of {}", r.owner),
        None => format!("{}/{}", r.owner, r.name),
    }
}

pub async fn secret(server: String, token: Option<&str>, cmd: SecretCmd) -> Result<()> {
    let org = matches!(
        cmd,
        SecretCmd::Set { org: true, .. } | SecretCmd::List { org: true } | SecretCmd::Delete { org: true, .. }
    );
    let (base, r) = target(&server, org)?;
    let mut c = gd::remote::GrpcRemote::connect(&base, None, token)?.runs();
    match cmd {
        SecretCmd::Set { name, env, .. } => {
            let mut value = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut value)?;
            let value = value.trim_end_matches(['\n', '\r']).to_owned();
            let at = place(&env, &r);
            c.set_secret(SetSecretRequest {
                repository: Some(r),
                name: name.clone(),
                value,
                environment: env.unwrap_or_default(),
            })
            .await?;
            println!("set {name} for {at}");
        }
        SecretCmd::List { .. } => {
            for x in c.list_secrets(r).await?.into_inner().secrets {
                let env = if x.environment.is_empty() { String::new() } else { format!("\t({})", x.environment) };
                println!("{}{env}\tset by {}", x.name, x.updated_by);
            }
        }
        SecretCmd::Delete { name, env, .. } => {
            c.delete_secret(SecretRef {
                repository: Some(r),
                name: name.clone(),
                environment: env.unwrap_or_default(),
            })
            .await?;
            println!("deleted {name}");
        }
    }
    Ok(())
}

pub async fn var(server: String, token: Option<&str>, cmd: VarCmd) -> Result<()> {
    let org =
        matches!(cmd, VarCmd::Set { org: true, .. } | VarCmd::List { org: true } | VarCmd::Delete { org: true, .. });
    let (base, r) = target(&server, org)?;
    let mut c = gd::remote::GrpcRemote::connect(&base, None, token)?.runs();
    match cmd {
        VarCmd::Set { name, value, env, .. } => {
            let at = place(&env, &r);
            c.set_variable(SetVariableRequest {
                repository: Some(r),
                name: name.clone(),
                value,
                environment: env.unwrap_or_default(),
            })
            .await?;
            println!("set {name} for {at}");
        }
        VarCmd::List { .. } => {
            for v in c.list_variables(r).await?.into_inner().variables {
                let env = if v.environment.is_empty() { String::new() } else { format!(" ({})", v.environment) };
                println!("{}{env}={}", v.name, v.value);
            }
        }
        VarCmd::Delete { name, env, .. } => {
            c.delete_variable(VariableRef {
                repository: Some(r),
                name: name.clone(),
                environment: env.unwrap_or_default(),
            })
            .await?;
            println!("deleted {name}");
        }
    }
    Ok(())
}

pub async fn env(server: String, token: Option<&str>, cmd: EnvCmd) -> Result<()> {
    let (base, r) = target(&server, false)?;
    let mut c = gd::remote::GrpcRemote::connect(&base, None, token)?.runs();
    match cmd {
        EnvCmd::Set { name, reviewers, branches } => {
            let e =
                c.set_environment(Environment { repository: Some(r), name, reviewers, branches }).await?.into_inner();
            println!(
                "{}: {}; {}",
                e.name,
                if e.reviewers.is_empty() {
                    "deploys without approval".to_owned()
                } else {
                    format!("approved by {}", e.reviewers.join(" or "))
                },
                if e.branches.is_empty() {
                    "from any branch".to_owned()
                } else {
                    format!("from {}", e.branches.join(", "))
                }
            );
        }
        EnvCmd::List => {
            for e in c.list_environments(r).await?.into_inner().environments {
                println!("{}\treviewers: {}\tbranches: {}", e.name, e.reviewers.join(","), e.branches.join(","));
            }
        }
        EnvCmd::Delete { name } => {
            c.delete_environment(EnvironmentRef { repository: Some(r), name: name.clone() }).await?;
            println!("deleted {name}");
        }
        EnvCmd::Approve { number, name } => {
            c.approve_deployment(ApproveDeploymentRequest { repository: Some(r), number, environment: name.clone() })
                .await?;
            println!("approved run #{number} to deploy to {name}");
        }
    }
    Ok(())
}
