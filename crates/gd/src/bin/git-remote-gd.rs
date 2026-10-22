//! git remote helper: `git clone gd::http://host:7420 repo`.
//!
//! git invokes `git-remote-gd <remote> <url>` with `GIT_DIR` set and speaks the
//! remote-helper protocol on stdin/stdout. All logic lives in `gd::bridge`.
//!
//! `git-remote-gd --pre-receive` is the server's hook for pushes over git's HTTP protocol:
//! it lands what was pushed (lines `old new ref` on stdin) in the repository at `GD_HOOK_URL`
//! as the pusher (`GD_TOKEN`), and fails the push with the reason when Grovedock refuses.

use std::io::{BufWriter, stdin, stdout};
use std::path::PathBuf;

use anyhow::Context;
use gd::bridge::Bridge;
use gd::remote::GrpcRemote;
use grovedock_adapters::flags::SpecFlags;

fn main() {
    grovedock_core::env::adopt_legacy();
    helper();
}

#[tokio::main]
async fn helper() {
    if let Err(e) = run().await {
        eprintln!("git-remote-gd: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--pre-receive") {
        return pre_receive().await;
    }
    let mut args = std::env::args().skip(1);
    let name = args.next().context("usage: git-remote-gd <remote> <url>")?;
    let url = args.next().unwrap_or_else(|| name.clone());
    let url = url.strip_prefix("gd::").unwrap_or(&url).to_owned();
    // A bare URL (no configured remote) gets a generic alias, as git's own helpers do.
    let alias = if name.contains("://") { "_".to_owned() } else { name };
    let git_dir = PathBuf::from(std::env::var("GIT_DIR").context("GIT_DIR is not set; run through git")?);
    let token = std::env::var("GD_TOKEN").ok();
    let objects = std::env::var("GD_OBJECTS").ok();
    let remote = GrpcRemote::connect(&url, objects.as_deref(), token.as_deref())?;
    let flags = SpecFlags::from_env()?;
    let bridge = Bridge::new(&remote, &flags, &alias, &git_dir)?;
    let mut input = stdin().lock();
    let mut out = BufWriter::new(stdout().lock());
    bridge.serve(&mut input, &mut out).await
}

async fn pre_receive() -> anyhow::Result<()> {
    use std::io::BufRead;
    let url = std::env::var("GD_HOOK_URL").context("GD_HOOK_URL is not set")?;
    let token = std::env::var("GD_TOKEN").ok();
    let git_dir = std::fs::canonicalize(std::env::var("GIT_DIR").unwrap_or_else(|_| ".".into()))?;
    let mut updates = Vec::new();
    for line in stdin().lock().lines() {
        let line = line?;
        let mut it = line.split_whitespace();
        if let (Some(old), Some(new), Some(r)) = (it.next(), it.next(), it.next()) {
            updates.push((old.to_owned(), new.to_owned(), r.to_owned()));
        }
    }
    let remote = GrpcRemote::connect(&url, None, token.as_deref())?;
    let flags = SpecFlags::from_env()?;
    let bridge = Bridge::new(&remote, &flags, "gd", &git_dir)?;
    let mut refused = false;
    for (refname, r) in bridge.receive(&git_dir, &updates).await? {
        if let Err(e) = r {
            refused = true;
            // git shows these lines to the person pushing, prefixed "remote:".
            eprintln!("Grovedock refused {refname}: {}", format!("{e:#}").replace('\n', " "));
        }
    }
    if refused {
        std::process::exit(1);
    }
    Ok(())
}
