//! Landing under real failures: clients land changes on several `grovedockd` replicas (sharing
//! one database) while `bench/chaos.sh` kills and restarts them. Afterwards it checks, through
//! the public API only:
//! 0. no change is in `main`'s history twice;
//! 1. every land a server acknowledged is in `main`'s history;
//! 2. `main`'s tree equals replaying its history;
//! 3. no change landed over a conflicting change it never saw.
//!
//!     cargo run --release --example land_chaos -- --servers http://127.0.0.1:7441,http://127.0.0.1:7442 --secs 60

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use gd::remote::{GrpcRemote, Remote};
use grovedock_core::Id;
use grovedock_core::object::{Changeset, FileChange, Kind, Mode, encode_object};

#[derive(Parser)]
struct Args {
    /// Comma-separated replica URLs the clients land through (possibly behind a lossy proxy).
    #[arg(long)]
    servers: String,
    /// Replica URLs for seeding and the final checks; defaults to `--servers`.
    #[arg(long)]
    direct: Option<String>,
    #[arg(long, default_value_t = 32)]
    clients: usize,
    #[arg(long, default_value_t = 60)]
    secs: u64,
    #[arg(long, default_value_t = 1)]
    seed: u64,
}

struct Attempt {
    msg: String,
    path: String,
    blob: Id,
    base: Id,
    landed: Option<Id>,
}

fn rng(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let servers: Vec<String> = a.servers.split(',').map(str::to_owned).collect();
    let remotes: Vec<Arc<GrpcRemote>> =
        servers.iter().map(|s| GrpcRemote::connect(s, None, None).map(Arc::new)).collect::<Result<_>>()?;
    let direct: Vec<Arc<GrpcRemote>> = a
        .direct
        .as_deref()
        .unwrap_or(&a.servers)
        .split(',')
        .map(|s| GrpcRemote::connect(s, None, None).map(Arc::new))
        .collect::<Result<_>>()?;
    let first = up(&direct).await?;

    // Seed `main` if needed, then remember where this run starts.
    let (blob, payload) = encode_object(Kind::Blob, b"seed\n");
    first.objects().put_many(vec![(blob, payload)]).await?;
    if first.get_ref("main").await?.is_none() {
        let cs = first
            .create_changeset(Changeset {
                parents: vec![],
                changes: vec![FileChange::upsert("seed", Mode::File, blob)],
                author: "chaos".into(),
                message: "seed".into(),
                timestamp_ms: 0,
                git: None,
            })
            .await?;
        first.compare_and_swap("main", None, cs.id).await?;
    }
    let start = first.get_ref("main").await?.context("main")?;

    let attempts = Arc::new(Mutex::new(Vec::<Attempt>::new()));
    let deadline = Instant::now() + Duration::from_secs(a.secs);
    let mut tasks = Vec::new();
    for c in 0..a.clients {
        let (remotes, attempts) = (remotes.clone(), attempts.clone());
        let mut state = a.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(c as u64 + 1) | 1;
        tasks.push(tokio::spawn(async move {
            let mut j = 0u64;
            while Instant::now() < deadline {
                let r = &remotes[(rng(&mut state) % remotes.len() as u64) as usize];
                let path = if rng(&mut state).is_multiple_of(3) {
                    format!("shared/{}", rng(&mut state) % 4)
                } else {
                    format!("own/c{c}/f{j}")
                };
                let msg = format!("{c}/{j}");
                j += 1;
                // Anything before `land` returns is retried as a fresh attempt: a server that is
                // down only costs time. What matters is the land's answer.
                let Ok(Some(base)) = r.get_ref("main").await else { continue };
                let (blob, payload) = encode_object(Kind::Blob, msg.as_bytes());
                if r.objects().put_many(vec![(blob, payload)]).await.is_err() {
                    continue;
                }
                let Ok(cs) = r
                    .create_changeset(Changeset {
                        parents: vec![base.0.to_vec()],
                        changes: vec![FileChange::upsert(path.clone(), Mode::File, blob)],
                        author: format!("client-{c}"),
                        message: msg.clone(),
                        timestamp_ms: 0,
                        git: None,
                    })
                    .await
                else {
                    continue;
                };
                // A failed land is retried on other replicas, as a real client would: the first
                // try may have landed before its reply was lost, and must not land twice.
                let mut landed = None;
                for _ in 0..3 {
                    let r = &remotes[(rng(&mut state) % remotes.len() as u64) as usize];
                    match r.land("main", cs.id).await {
                        Ok((id, _)) => {
                            landed = Some(id);
                            break;
                        }
                        Err(e) if format!("{e:#}").contains("conflict") => break,
                        Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                    }
                }
                attempts.lock().unwrap().push(Attempt { msg, path, blob, base, landed });
            }
        }));
    }
    for t in tasks {
        t.await?;
    }
    let attempts = std::mem::take(&mut *attempts.lock().unwrap());
    let acked = attempts.iter().filter(|a| a.landed.is_some()).count();
    println!("attempts {}  acknowledged {}", attempts.len(), acked);

    // Give killed replicas time to come back, then check through whichever is up.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let r = up(&direct).await?;
    check(r.as_ref(), start, &attempts).await?;
    println!("all invariants hold");
    Ok(())
}

async fn up(remotes: &[Arc<GrpcRemote>]) -> Result<Arc<GrpcRemote>> {
    for _ in 0..100 {
        for r in remotes {
            if r.list_refs().await.is_ok() {
                return Ok(r.clone());
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    bail!("no replica is up")
}

async fn check(r: &GrpcRemote, start: Id, attempts: &[Attempt]) -> Result<()> {
    // History since this run started, oldest first.
    // `log` returns at most 10,000 changesets per call, so page back from each oldest one.
    let mut history = Vec::new();
    let mut rev = "main".to_owned();
    'pages: loop {
        let page = r.log(&rev, "", 10_000).await?;
        let Some(oldest) = page.last().map(|cs| cs.changeset.first_parent()) else { break };
        for cs in page {
            if cs.id == start {
                break 'pages;
            }
            history.push(cs);
        }
        match oldest? {
            Some(parent) => rev = parent.to_string(),
            None => break,
        }
    }
    history.reverse();
    let position: HashMap<Id, usize> = history.iter().enumerate().map(|(i, cs)| (cs.id, i)).collect();

    // 0. No change twice.
    let mut seen = HashSet::new();
    for cs in &history {
        ensure!(seen.insert(cs.changeset.message.clone()), "change `{}` is in history twice", cs.changeset.message);
    }
    // 1. Every acknowledged land is in history: the id the server answered with, and the
    // change itself (they differ when a retried change was already there and the server
    // answered with the tip that holds it).
    let by_msg: HashMap<&str, usize> =
        history.iter().enumerate().map(|(i, cs)| (cs.changeset.message.as_str(), i)).collect();
    let mut landed = Vec::new();
    for a in attempts {
        if let Some(id) = a.landed {
            ensure!(position.contains_key(&id), "acknowledged land `{}` ({id}) is not in history", a.msg);
            let at = *by_msg
                .get(a.msg.as_str())
                .with_context(|| format!("acknowledged change `{}` is not in history", a.msg))?;
            landed.push((at, a));
        }
    }
    // 2. The tree equals replaying history.
    let mut model: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for cs in &history {
        for c in &cs.changeset.changes {
            model.insert(c.path.clone(), c.blob.clone());
        }
    }
    let actual: BTreeMap<String, Vec<u8>> =
        r.diff(&start.to_string(), "main").await?.into_iter().map(|c| (c.path, c.blob)).collect();
    ensure!(actual == model, "tree differs from replayed history ({} vs {} paths)", actual.len(), model.len());
    // 3. A later change to a path must have been based on the earlier one.
    landed.sort_by_key(|(at, _)| *at);
    for (i, (_, b)) in landed.iter().enumerate() {
        for (at_a, a) in &landed[..i] {
            if a.path == b.path && a.blob != b.blob {
                let earlier = history[*at_a].id;
                let base_has_it = r.merge_base(&earlier.to_string(), &b.base.to_string()).await? == Some(earlier);
                ensure!(base_has_it, "`{}` landed over `{}` on `{}` without having seen it", b.msg, a.msg, b.path);
            }
        }
    }
    println!("history {}  acknowledged lands checked {}", history.len(), landed.len());
    Ok(())
}
