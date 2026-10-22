//! Landing throughput under heavy concurrency: N agents, each on its own connection,
//! repeatedly sync to the branch head, upload a small new file, create a changeset and
//! land it. Every agent writes its own directory, so all lands are non-conflicting and
//! any lost race is resolved by the server (rebase + compare-and-swap), not the client.
//!
//!     cargo run --release --example land_load -- --server http://127.0.0.1:7431 --agents 1000 --secs 60
//!
//! Raise the open-file limit first (`ulimit -n 65536`): each agent holds one connection.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use gd::remote::{GrpcRemote, Remote};
use grovedock_core::object::{Changeset, FileChange, Kind, Mode, encode_object};
use tokio::sync::Mutex;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:7431")]
    server: String,
    #[arg(long, env = "GD_TOKEN")]
    token: Option<String>,
    #[arg(long = "ref", default_value = "main")]
    ref_name: String,
    #[arg(long, default_value_t = 1000)]
    agents: usize,
    /// Measurement window; agents keep landing until it ends.
    #[arg(long, default_value_t = 60)]
    secs: u64,
    /// Directory the agents write under (one subdirectory per agent).
    #[arg(long, default_value = "bench-load")]
    dir: String,
    /// Spread agent directories over this many subdirectories (1 = all agents side by side
    /// in one directory, which every land then rewrites).
    #[arg(long, default_value_t = 1)]
    fanout: usize,
}

#[derive(Default)]
struct Stats {
    landed: AtomicU64,
    rebased: AtomicU64,
    errors: AtomicU64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let stats = Arc::new(Stats::default());
    let latencies = Arc::new(Mutex::new(Vec::<u32>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let first_error = Arc::new(Mutex::new(None::<String>));

    // Connect every agent before the clock starts.
    let mut remotes = Vec::with_capacity(a.agents);
    for _ in 0..a.agents {
        let r = GrpcRemote::connect(&a.server, None, a.token.as_deref())?;
        r.get_ref(&a.ref_name).await?;
        remotes.push(r);
    }

    // An empty ref accepts only a root changeset; seed it so every agent has a parent.
    if remotes[0].get_ref(&a.ref_name).await?.is_none() {
        one(&remotes[0], &a.ref_name, &format!("{}/seed.txt", a.dir), 0, 0).await?;
    }

    let start = Instant::now();
    let deadline = start + Duration::from_secs(a.secs);
    let mut tasks = Vec::new();
    for (i, remote) in remotes.into_iter().enumerate() {
        let (stats, latencies, stop, first_error) =
            (stats.clone(), latencies.clone(), stop.clone(), first_error.clone());
        let (ref_name, dir) = (
            a.ref_name.clone(),
            match a.fanout {
                0 | 1 => a.dir.clone(),
                n => format!("{}/g{:03}", a.dir, i % n),
            },
        );
        tasks.push(tokio::spawn(async move {
            let mut mine = Vec::new();
            let mut j = 0u64;
            while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                let t = Instant::now();
                match one(&remote, &ref_name, &format!("{dir}/a{i:05}/c{j}.txt"), i, j).await {
                    Ok(rebased) => {
                        if Instant::now() > deadline {
                            break; // finished after the window: not counted
                        }
                        stats.landed.fetch_add(1, Ordering::Relaxed);
                        if rebased {
                            stats.rebased.fetch_add(1, Ordering::Relaxed);
                        }
                        mine.push(t.elapsed().as_micros() as u32);
                    }
                    Err(e) => {
                        stats.errors.fetch_add(1, Ordering::Relaxed);
                        first_error.lock().await.get_or_insert_with(|| format!("{e:#}"));
                    }
                }
                j += 1;
            }
            latencies.lock().await.extend(mine);
        }));
    }
    tokio::time::sleep_until(deadline.into()).await;
    stop.store(true, Ordering::Relaxed);
    for t in tasks {
        t.await?;
    }

    let mut lat = std::mem::take(&mut *latencies.lock().await);
    lat.sort_unstable();
    let pct = |p: f64| {
        lat.get(((lat.len() as f64 * p) as usize).min(lat.len().saturating_sub(1)))
            .map_or(0.0, |&us| us as f64 / 1000.0)
    };
    let landed = stats.landed.load(Ordering::Relaxed);
    println!("agents {}  window {} s  ref {}  fanout {}", a.agents, a.secs, a.ref_name, a.fanout.max(1));
    println!("landed {landed}  ({:.1} lands/s)", landed as f64 / a.secs as f64);
    println!(
        "server rebases {}  errors {}",
        stats.rebased.load(Ordering::Relaxed),
        stats.errors.load(Ordering::Relaxed)
    );
    println!(
        "sync+upload+commit+land latency ms: p50 {:.1}  p90 {:.1}  p99 {:.1}  max {:.1}",
        pct(0.5),
        pct(0.9),
        pct(0.99),
        pct(1.0)
    );
    if let Some(e) = first_error.lock().await.as_ref() {
        println!("first error: {e}");
    }
    Ok(())
}

/// One agent change: read the head, upload a new file, create a changeset on it, land.
async fn one(remote: &GrpcRemote, ref_name: &str, path: &str, i: usize, j: u64) -> Result<bool> {
    let head = remote.get_ref(ref_name).await?;
    let (blob, payload) = encode_object(Kind::Blob, format!("agent {i} change {j}\n").as_bytes());
    remote.objects().put_many(vec![(blob, payload)]).await?;
    let cs = Changeset {
        parents: head.map(|h| vec![h.0.to_vec()]).unwrap_or_default(),
        changes: vec![FileChange::upsert(path, Mode::File, blob)],
        author: format!("agent-{i}"),
        message: format!("agent {i} change {j}"),
        timestamp_ms: 0,
        git: None,
    };
    let created = remote.create_changeset(cs).await?;
    Ok(remote.land(ref_name, created.id).await?.1)
}
