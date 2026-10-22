//! `gd runner`: runs Grovedock Runs jobs on this machine.
//!
//! A runner belongs to an owner (a person or an organisation) and pulls jobs for its
//! repositories; nothing connects to it. Each job gets a fresh directory, deleted afterwards,
//! and a read-only token for its one repository that dies with the job. Steps run directly on
//! this machine (like GitHub's self-hosted runners), so register runners that take outsiders'
//! code (`--outsiders`) only on machines that start clean for every job.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use grovedock_proto::runs_client::RunsClient;
use grovedock_proto::{
    Annotation, AssignedJob, ClaimJobRequest, ListRunnersRequest, RegisterRunnerRequest, RemoveRunnerRequest,
    ReportJobRequest, RunState, StepReport, UpdateRunnerRequest,
};
use grovedock_workflow::{self as wf, expr};

use crate::containers::{JobBox, Mode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tonic::transport::Channel;

const RUNNER_HEADER: &str = "x-gd-runner";

#[derive(Subcommand)]
pub enum RunnerCmd {
    /// Register this machine as a runner for an owner you administer, and save its key.
    Register {
        /// Your account or an organisation you administer.
        #[arg(long)]
        owner: String,
        /// The runner's name (default: this machine's).
        #[arg(long)]
        name: Option<String>,
        /// Labels besides those for this machine (OS, architecture), repeatable.
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Also run outsiders' code (forks), once a maintainer approves it. Only on a machine
        /// that starts clean for every job.
        #[arg(long)]
        outsiders: bool,
    },
    /// Take jobs and run them until stopped.
    Start {
        /// Which registered runner (default: the only one).
        #[arg(long)]
        name: Option<String>,
        /// Where jobs run (default: a folder beside the runner's settings).
        #[arg(long)]
        work: Option<PathBuf>,
        /// Run one job, then stop.
        #[arg(long)]
        once: bool,
        /// Run every job in a fresh Linux container (Docker), removed when the job ends: nothing
        /// a job does stays on this machine. Jobs see this runner as Linux.
        #[arg(long)]
        containers: bool,
        /// The image for jobs that don't name one, with --containers.
        #[arg(long, default_value = "buildpack-deps:bookworm")]
        image: String,
        /// CPUs each job's container may use, like 2 or 1.5.
        #[arg(long)]
        cpus: Option<String>,
        /// Memory each job's container may use, like 4g.
        #[arg(long)]
        memory: Option<String>,
        /// The containers' runtime, like runsc (gVisor) for a sandbox kernel per job.
        #[arg(long)]
        runtime: Option<String>,
    },
    /// An owner's runners.
    List {
        #[arg(long)]
        owner: String,
    },
    /// Change the labels a runner answers to (besides its OS and architecture), e.g.
    /// `gd runner labels --owner lumen ID --label gpu --label large`; none clears them.
    Labels {
        #[arg(long)]
        owner: String,
        id: String,
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    /// Remove a runner: its key stops working at once.
    Remove {
        #[arg(long)]
        owner: String,
        id: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Saved {
    server: String,
    owner: String,
    name: String,
    id: String,
    key: String,
}

impl Saved {
    /// This runner, unique on a machine that has several (for other servers or owners): what its
    /// containers are labelled with, so one never cleans up another's.
    fn identity(&self) -> String {
        format!("{}/{}/{}", self.server.trim_end_matches('/'), self.owner, self.name)
    }
}

fn runners_dir() -> Result<PathBuf> {
    let creds = gd::credentials::path().context("no home directory to keep the runner's settings in")?;
    Ok(creds.parent().context("no config directory")?.join("runners"))
}

fn save(s: &Saved) -> Result<PathBuf> {
    let dir = runners_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", s.name));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        opts.mode(0o600);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let mut f = opts.open(&path).with_context(|| format!("writing {}", path.display()))?;
    std::io::Write::write_all(&mut f, &serde_json::to_vec_pretty(s)?)?;
    Ok(path)
}

fn load(name: Option<&str>) -> Result<Saved> {
    let dir = runners_dir()?;
    let path = match name {
        Some(n) => dir.join(format!("{n}.json")),
        None => {
            let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
                .map(|d| {
                    d.filter_map(|e| e.ok().map(|e| e.path()))
                        .filter(|p| p.extension().is_some_and(|x| x == "json"))
                        .collect()
                })
                .unwrap_or_default();
            match found.len() {
                0 => bail!("no runner registered on this machine: run `gd runner register --owner OWNER` first"),
                1 => found.remove(0),
                _ => bail!("several runners are registered here; pick one with --name"),
            }
        }
    };
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn machine() -> (&'static str, &'static str) {
    (std::env::consts::OS, std::env::consts::ARCH)
}

/// `server` is needed by every command but `start`, which uses the server it registered with.
pub async fn runner(server: Option<String>, token: Option<&str>, cmd: RunnerCmd) -> Result<()> {
    let base = || -> Result<String> {
        let s = server.clone().context("no server: pass --server URL or set GD_SERVER")?;
        Ok(grovedock_proto::split_repo_url(&s).0)
    };
    match cmd {
        RunnerCmd::Register { owner, name, labels, outsiders } => {
            let name = name.unwrap_or_else(|| {
                std::env::var("HOSTNAME")
                    .ok()
                    .or_else(|| {
                        std::process::Command::new("hostname")
                            .output()
                            .ok()
                            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                    })
                    .filter(|h| !h.is_empty())
                    .map(|h| h.split('.').next().unwrap_or(&h).to_owned())
                    .unwrap_or_else(|| "runner".into())
            });
            let mut c = gd::remote::GrpcRemote::connect(&base()?, None, token)?.runs();
            let r = c
                .register_runner(RegisterRunnerRequest { owner: owner.clone(), name: name.clone(), labels, outsiders })
                .await?
                .into_inner();
            let runner = r.runner.context("the server sent no runner")?;
            let path =
                save(&Saved { server: base()?, owner, name: runner.name.clone(), id: runner.id.clone(), key: r.key })?;
            println!(
                "registered runner {} for {} (key saved to {}, readable by you only)",
                runner.name,
                runner.owner,
                path.display()
            );
            let (os, arch) = machine();
            println!(
                "it answers to: {}",
                wf::machine_labels(os, arch).into_iter().chain(runner.labels).collect::<Vec<_>>().join(", ")
            );
            println!("start it with: gd runner start");
        }
        RunnerCmd::List { owner } => {
            let mut c = gd::remote::GrpcRemote::connect(&base()?, None, token)?.runs();
            let rs = c.list_runners(ListRunnersRequest { owner }).await?.into_inner().runners;
            if rs.is_empty() {
                println!("no runners");
            }
            for r in rs {
                let state = if r.busy_with != 0 { format!("running job {}", r.busy_with) } else { "idle".into() };
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    r.id,
                    r.name,
                    if r.os.is_empty() { "never seen" } else { &r.os },
                    state,
                    r.labels.join(",")
                );
            }
        }
        RunnerCmd::Labels { owner, id, labels } => {
            let mut c = gd::remote::GrpcRemote::connect(&base()?, None, token)?.runs();
            let r = c.update_runner(UpdateRunnerRequest { owner, id, labels }).await?.into_inner();
            let what = if r.labels.is_empty() { "its OS and architecture only".into() } else { r.labels.join(", ") };
            println!("{} answers to: {what}", r.name);
        }
        RunnerCmd::Remove { owner, id } => {
            let mut c = gd::remote::GrpcRemote::connect(&base()?, None, token)?.runs();
            c.remove_runner(RemoveRunnerRequest { owner, id: id.clone() }).await?;
            println!("removed runner {id}");
        }
        RunnerCmd::Start { name, work, once, containers, image, cpus, memory, runtime } => {
            let saved = load(name.as_deref())?;
            let work = match work {
                Some(w) => w,
                None => runners_dir()?.join(format!("{}-work", saved.name)),
            };
            let mode = Mode { image: containers.then_some(image), cpus, memory, runtime };
            start(saved, work, once, mode).await?;
        }
    }
    Ok(())
}

type Client = RunsClient<tonic::service::interceptor::InterceptedService<Channel, KeyHeader>>;

#[derive(Clone)]
struct KeyHeader(tonic::metadata::AsciiMetadataValue);

impl tonic::service::Interceptor for KeyHeader {
    fn call(&mut self, mut req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        req.metadata_mut().insert(RUNNER_HEADER, self.0.clone());
        Ok(req)
    }
}

fn client(saved: &Saved) -> Result<Client> {
    let channel = grovedock_proto::endpoint(saved.server.clone())?.tcp_nodelay(true).connect_lazy();
    let key = tonic::metadata::AsciiMetadataValue::try_from(saved.key.as_str())
        .map_err(|_| anyhow!("the saved runner key is damaged"))?;
    Ok(RunsClient::with_interceptor(channel, KeyHeader(key))
        .max_decoding_message_size(grovedock_proto::MAX_MESSAGE_BYTES)
        .max_encoding_message_size(grovedock_proto::MAX_MESSAGE_BYTES))
}

async fn start(saved: Saved, work: PathBuf, once: bool, mode: Mode) -> Result<()> {
    let mut c = client(&saved)?;
    let (os, arch) = machine();
    // In containers, jobs run on Linux whatever this machine is.
    let labels = wf::machine_labels(if mode.image.is_some() { "linux" } else { os }, arch);
    crate::containers::clean_runner(&saved.identity()).await;
    eprintln!(
        "runner {} for {} on {} is waiting for jobs{} (Ctrl-C to stop)",
        saved.name,
        saved.owner,
        saved.server,
        match &mode.image {
            Some(i) => format!(", each in a fresh container ({i})"),
            None => String::new(),
        }
    );
    let mut backoff = Duration::from_secs(1);
    loop {
        let claim = c
            .claim_job(ClaimJobRequest {
                wait_ms: 30_000,
                machine_labels: labels.clone(),
                version: env!("CARGO_PKG_VERSION").into(),
                os: os.into(),
            })
            .await;
        let job = match claim {
            Ok(r) => {
                backoff = Duration::from_secs(1);
                r.into_inner().job
            }
            Err(s) if s.code() == tonic::Code::Unauthenticated => bail!("{}", s.message()),
            Err(s) => {
                eprintln!("couldn't reach the server ({}); trying again in {}s", s.message(), backoff.as_secs());
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(15));
                continue;
            }
        };
        let Some(job) = job else { continue };
        let name = format!(
            "{}/{} run #{} job {}",
            job.repository.as_ref().map(|r| r.owner.as_str()).unwrap_or(""),
            job.repository.as_ref().map(|r| r.name.as_str()).unwrap_or(""),
            job.run_number,
            job.id
        );
        eprintln!("running {name}");
        let state = run_job(&mut c, &saved, &mode, &work, job).await;
        eprintln!(
            "{name}: {}",
            match state {
                Ok(s) => format!("{s:?}").to_lowercase(),
                Err(e) => format!("couldn't finish: {e:#}"),
            }
        );
        if once {
            return Ok(());
        }
    }
}

/// Everything a job's steps write, with secrets hidden, waiting to be sent.
#[derive(Default)]
struct Output {
    pending: Mutex<Vec<u8>>,
    total: AtomicU64,
    masks: Mutex<Vec<String>>,
    cancel: AtomicBool,
    steps: Mutex<Vec<StepReport>>,
    /// The step running now, for what it flags.
    step: Mutex<String>,
    /// `::error`, `::warning`, `::notice`, and the job's summary; sent again when they change.
    annotations: Mutex<Vec<Annotation>>,
    summary: Mutex<String>,
    notes_changed: AtomicBool,
}

/// GitHub's limits: 50 annotations a job, a 1 MiB summary.
const MAX_ANNOTATIONS: usize = 50;
const MAX_SUMMARY: usize = 1 << 20;

impl Output {
    fn masked(&self, text: &str) -> String {
        let mut t = text.to_owned();
        for m in self.masks.lock().unwrap().iter().filter(|m| m.len() >= 4) {
            t = t.replace(m.as_str(), "***");
        }
        t
    }
    fn write(&self, text: &str) {
        let t = self.masked(text);
        self.total.fetch_add(t.len() as u64, Ordering::SeqCst);
        self.pending.lock().unwrap().extend_from_slice(t.as_bytes());
    }
    /// Keeps an `::error`/`::warning`/`::notice` line as an annotation.
    fn annotate(&self, line: &str) {
        let Some((level, props, message)) = workflow_command(line) else { return };
        if !matches!(level, "error" | "warning" | "notice") {
            return;
        }
        let mut a = self.annotations.lock().unwrap();
        if a.len() >= MAX_ANNOTATIONS {
            return;
        }
        let prop = |k: &str| props.iter().find(|(n, _)| n == k).map(|(_, v)| self.masked(v)).unwrap_or_default();
        a.push(Annotation {
            level: level.to_owned(),
            message: self.masked(&message),
            title: prop("title"),
            file: prop("file"),
            line: prop("line").parse().unwrap_or(0),
            step: self.step.lock().unwrap().clone(),
        });
        self.notes_changed.store(true, Ordering::SeqCst);
    }
    /// Adds a step's `$GITHUB_STEP_SUMMARY` to the job's.
    fn add_summary(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let mut s = self.summary.lock().unwrap();
        if s.len() + text.len() > MAX_SUMMARY {
            s.push_str("\n\n_The rest of the summary was over 1 MiB and was left out._\n");
        } else {
            if !s.is_empty() {
                s.push_str("\n\n");
            }
            s.push_str(&self.masked(text));
        }
        self.notes_changed.store(true, Ordering::SeqCst);
    }
    fn line(&self, text: &str) {
        self.write(&format!("{text}\n"));
    }
    fn mask(&self, secret: &str) {
        if !secret.trim().is_empty() {
            self.masks.lock().unwrap().push(secret.to_owned());
        }
    }
}

async fn report(
    c: &mut Client,
    job: u64,
    out: &Output,
    result: Option<(RunState, String, BTreeMap<String, String>)>,
) -> Result<bool> {
    let log = std::mem::take(&mut *out.pending.lock().unwrap());
    let steps = out.steps.lock().unwrap().clone();
    let (annotations, summary) = if out.notes_changed.swap(false, Ordering::SeqCst) {
        (out.annotations.lock().unwrap().clone(), out.summary.lock().unwrap().clone())
    } else {
        Default::default()
    };
    let (state, failure, outputs) = result.map(|(s, f, o)| (s as i32, f, o)).unwrap_or_default();
    let cancel = c
        .report_job(ReportJobRequest {
            job_id: job,
            log,
            steps,
            result: state,
            failure,
            outputs: outputs.into_iter().collect(),
            annotations,
            summary,
        })
        .await?
        .into_inner()
        .cancel;
    if cancel {
        out.cancel.store(true, Ordering::SeqCst);
    }
    Ok(cancel)
}

async fn run_job(c: &mut Client, saved: &Saved, mode: &Mode, work: &Path, job: AssignedJob) -> Result<RunState> {
    let out = Arc::new(Output::default());
    out.mask(&job.token);
    for v in job.secrets.values() {
        out.mask(v);
    }
    // Progress goes out about every second while the job runs; a cancel comes back the same way.
    let reporter = {
        let (mut c, out) = (c.clone(), out.clone());
        let id = job.id;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1000)).await;
                if let Err(e) = report(&mut c, id, &out, None).await {
                    eprintln!("couldn't send progress: {e:#}");
                }
            }
        })
    };
    let dir = work.join(format!("job-{}", job.id));
    let mut boxed = None;
    let result = execute(c, saved, mode, &mut boxed, &dir, &job, &out).await;
    if let Some(b) = boxed {
        b.stop(&dir).await;
    }
    reporter.abort();
    let (state, failure, outputs) = match result {
        Ok(r) => r,
        Err(e) => {
            let why = format!("The runner couldn't run this job: {e:#}");
            out.line(&why);
            (RunState::Failed, why, BTreeMap::new())
        }
    };
    // Nothing of the job stays on this machine.
    let _ = std::fs::remove_dir_all(&dir);
    for attempt in 0..5 {
        match report(c, job.id, &out, Some((state, failure.clone(), outputs.clone()))).await {
            Ok(_) => break,
            Err(e) if attempt == 4 => return Err(e),
            Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
    Ok(state)
}

/// A reusable workflow's job's `inputs`: its caller's `with:`, filled in with what the job sees.
fn called_inputs(job: &wf::Job, github: &Value, needs: &Value) -> Value {
    let scope = expr::Scope::default()
        .with("github", github.clone())
        .with("needs", needs.clone())
        .with("inputs", github["event"]["inputs"].clone());
    Value::Object(
        job.call_inputs
            .iter()
            .map(|(k, v)| (k.clone(), json!(expr::interpolate(v, &scope).unwrap_or_else(|_| v.clone()))))
            .collect(),
    )
}

/// A workflow command line, `::name key=value,…::message`, with GitHub's escapes undone.
/// A command's name, its `key=value` properties and its message.
type Command<'a> = (&'a str, Vec<(String, String)>, String);

fn workflow_command(line: &str) -> Option<Command<'_>> {
    let rest = line.trim_start().strip_prefix("::")?;
    let (head, message) = rest.split_once("::")?;
    let (name, props) = head.split_once(' ').unwrap_or((head, ""));
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let undo = |s: &str, prop: bool| {
        let s = s.replace("%0D", "\r").replace("%0A", "\n");
        let s = if prop { s.replace("%3A", ":").replace("%2C", ",") } else { s };
        s.replace("%25", "%")
    };
    let props = props
        .split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), undo(v.trim(), true)))
        .collect();
    Some((name, props, undo(message, false)))
}

/// Lines of a `$GITHUB_OUTPUT` / `$GITHUB_ENV` file: `name=value`, or `name<<DELIM` … `DELIM`.
fn key_values(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if let Some((k, delim)) = l.split_once("<<") {
            let mut v = Vec::new();
            for l in lines.by_ref() {
                if l == delim {
                    break;
                }
                v.push(l);
            }
            out.push((k.to_owned(), v.join("\n")));
        } else if let Some((k, v)) = l.split_once('=') {
            out.push((k.to_owned(), v.to_owned()));
        }
    }
    out
}

type Done = (RunState, String, BTreeMap<String, String>);

async fn execute(
    c: &mut Client,
    saved: &Saved,
    mode: &Mode,
    boxed: &mut Option<JobBox>,
    dir: &Path,
    job: &AssignedJob,
    out: &Arc<Output>,
) -> Result<Done> {
    let planned: wf::Planned = serde_json::from_str(&job.job_json).context("reading the job")?;
    let github: Value = serde_json::from_str(&job.github_json).unwrap_or(json!({}));
    let needs: Value = serde_json::from_str(&job.needs_json).unwrap_or(json!({}));
    let repo = job.repository.clone().unwrap_or_default();
    let workspace = dir.join(&repo.name);
    let temp = dir.join("_temp");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&temp)?;
    let (os, arch) = machine();
    let in_container = planned.job.container.is_some() || mode.image.is_some();
    let os = if in_container { "linux" } else { os };
    let runner_ctx = json!({
        "os": match os { "linux" => "Linux", "macos" => "macOS", "windows" => "Windows", o => o },
        "arch": match arch { "x86_64" => "X64", "aarch64" => "ARM64", a => a },
        "name": saved.name, "temp": temp, "workspace": dir,
    });
    let mut github = github;
    github["workspace"] = json!(workspace);
    let secrets: serde_json::Map<String, Value> = job.secrets.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let mut base_scope = expr::Scope::default()
        .with("github", github.clone())
        .with("needs", needs.clone())
        .with("matrix", planned.matrix.clone())
        .with("runner", runner_ctx.clone())
        .with("secrets", Value::Object(secrets))
        .with(
            "inputs",
            if planned.job.called_by.is_some() {
                called_inputs(&planned.job, &github, &needs)
            } else {
                github["event"]["inputs"].clone()
            },
        )
        .with("vars", json!(job.vars))
        .with("job", json!({ "status": "success" }));
    base_scope.workspace = Some(workspace.clone());
    if in_container || !planned.job.services.is_empty() {
        let scope = base_scope.clone().with("env", json!({}));
        let fill = |c: &wf::Container| wf::Container {
            image: expr::interpolate(&c.image, &scope).unwrap_or_else(|_| c.image.clone()),
            env: c
                .env
                .iter()
                .map(|(k, v)| (k.clone(), expr::interpolate(v, &scope).unwrap_or_else(|_| v.clone())))
                .collect(),
            ..c.clone()
        };
        let services = planned.job.services.iter().map(|(k, c)| (k.clone(), fill(c))).collect();
        let say = |l: &str| out.line(l);
        let b = JobBox::start(
            job.id,
            &saved.identity(),
            dir,
            &workspace,
            planned.job.container.as_ref().map(fill),
            &services,
            mode,
            &say,
        )
        .await?;
        base_scope = base_scope.with("job", json!({ "status": "success", "services": b.services }));
        if b.container.is_some() {
            // The checkout belongs to this machine's user; git inside runs as the container's.
            std::fs::write(temp.join("gitconfig"), "[safe]\n\tdirectory = *\n")?;
        }
        *boxed = Some(b);
    }
    let boxed = boxed.as_ref();
    let has_bash = match boxed {
        Some(b) if b.container.is_some() => b.bash,
        _ => std::process::Command::new("bash").arg("--version").output().is_ok_and(|o| o.status.success()),
    };
    let base_path = match boxed {
        Some(b) if b.container.is_some() => b.path.clone(),
        _ => std::env::var("PATH").unwrap_or_default(),
    };
    // Job env, with `${{ }}` filled in.
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    let scope0 = base_scope.clone().with("env", json!({}));
    for (k, v) in &planned.job.env {
        env.insert(k.clone(), expr::interpolate(v, &scope0).unwrap_or_else(|_| v.clone()));
    }
    let server = saved.server.trim_end_matches('/').to_owned();
    let fixed: BTreeMap<String, String> = [
        ("CI", "true".to_owned()),
        ("GROVEDOCK", "true".into()),
        ("GITHUB_ACTIONS", "true".into()),
        ("GITHUB_WORKSPACE", workspace.display().to_string()),
        ("GITHUB_SHA", github["sha"].as_str().unwrap_or("").into()),
        ("GITHUB_REF", github["ref"].as_str().unwrap_or("").into()),
        ("GITHUB_REF_NAME", github["ref_name"].as_str().unwrap_or("").into()),
        ("GITHUB_REF_TYPE", github["ref_type"].as_str().unwrap_or("").into()),
        ("GITHUB_HEAD_REF", github["head_ref"].as_str().unwrap_or("").into()),
        ("GITHUB_BASE_REF", github["base_ref"].as_str().unwrap_or("").into()),
        ("GITHUB_REPOSITORY", format!("{}/{}", repo.owner, repo.name)),
        ("GITHUB_REPOSITORY_OWNER", repo.owner.clone()),
        ("GITHUB_RUN_NUMBER", job.run_number.to_string()),
        ("GITHUB_RUN_ID", github["run_id"].to_string()),
        ("GITHUB_RUN_ATTEMPT", "1".into()),
        ("GITHUB_EVENT_NAME", github["event_name"].as_str().unwrap_or("").into()),
        ("GITHUB_ACTOR", github["actor"].as_str().unwrap_or("").into()),
        ("GITHUB_SERVER_URL", github["server_url"].as_str().unwrap_or("").into()),
        ("GITHUB_JOB", planned.job.id.clone()),
        ("GITHUB_WORKFLOW", github["workflow"].as_str().unwrap_or("").into()),
        ("RUNNER_OS", runner_ctx["os"].as_str().unwrap_or("").into()),
        ("RUNNER_ARCH", runner_ctx["arch"].as_str().unwrap_or("").into()),
        ("RUNNER_NAME", saved.name.clone()),
        ("RUNNER_TEMP", temp.display().to_string()),
        // `git config --global` writes to the job's own file, not the runner's.
        ("GIT_CONFIG_GLOBAL", temp.join("gitconfig").display().to_string()),
        ("GD_SERVER", format!("{server}/{}/{}", repo.owner, repo.name)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    // The job's token is for checkout only: steps' own code never gets it in its environment.
    let mut path_extra: Vec<String> = Vec::new();
    let mut steps_ctx = serde_json::Map::new();
    let mut status = expr::Status::Success;
    let mut failure = String::new();
    let job_started = Instant::now();
    let job_limit = Duration::from_secs(job.timeout_minutes.max(1) as u64 * 60);
    *out.steps.lock().unwrap() = planned
        .job
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| StepReport {
            index: i as u32 + 1,
            state: RunState::Queued as i32,
            name: s.title(),
            ..Default::default()
        })
        .collect();
    // Step 0 is setting up: the checkout of nothing yet, and the job's facts.
    out.line(&format!("Job {} on runner {} ({} {})", planned.name, saved.name, os, arch));
    out.line(&format!(
        "Repository {}/{} at {} ({})",
        repo.owner,
        repo.name,
        job.r#ref,
        &github["sha"].as_str().unwrap_or("")[..12.min(github["sha"].as_str().unwrap_or("").len())]
    ));
    for (i, step) in planned.job.steps.iter().enumerate() {
        let idx = i + 1;
        let cancelled = out.cancel.load(Ordering::SeqCst) || job_started.elapsed() > job_limit;
        if cancelled && status != expr::Status::Cancelled {
            status = expr::Status::Cancelled;
            if failure.is_empty() {
                failure = if job_started.elapsed() > job_limit {
                    format!("Timed out after {} minutes", job.timeout_minutes)
                } else {
                    "Cancelled".into()
                };
            }
        }
        let env_obj: serde_json::Map<String, Value> = env.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        let scope = expr::Scope { status, ..base_scope.clone() }
            .with("env", Value::Object(env_obj))
            .with("steps", Value::Object(steps_ctx.clone()));
        let title = match &step.name {
            Some(n) => expr::interpolate(n, &scope).unwrap_or_else(|_| n.clone()),
            None => {
                let t = step.title();
                expr::interpolate(&t, &scope).unwrap_or(t)
            }
        };
        let run_it = match expr::condition(step.cond.as_deref(), &scope) {
            Ok(b) => b,
            Err(e) => {
                out.line(&format!("Step {title}: its if: couldn't be evaluated: {e}"));
                false
            }
        };
        let offset = out.total.load(Ordering::SeqCst);
        set_step(out, idx, |s| {
            s.name = title.clone();
            s.log_offset = offset;
        });
        if !run_it {
            set_step(out, idx, |s| s.state = RunState::Skipped as i32);
            if let Some(id) = &step.id {
                steps_ctx.insert(id.clone(), json!({ "outcome": "skipped", "conclusion": "skipped", "outputs": {} }));
            }
            continue;
        }
        let started = now_ms();
        set_step(out, idx, |s| {
            s.state = RunState::Running as i32;
            s.started_at_ms = started;
        });
        out.line(&format!("▶ {title}"));
        *out.step.lock().unwrap() = title.clone();
        let step_out = temp.join(format!("output-{idx}"));
        let step_env_file = temp.join(format!("env-{idx}"));
        let step_path_file = temp.join(format!("path-{idx}"));
        let summary = temp.join(format!("summary-{idx}"));
        for f in [&step_out, &step_env_file, &step_path_file, &summary] {
            std::fs::write(f, b"")?;
        }
        let mut step_env = env.clone();
        for (k, v) in &step.env {
            step_env.insert(k.clone(), expr::interpolate(v, &scope).unwrap_or_else(|_| v.clone()));
        }
        step_env.extend(fixed.clone());
        step_env.insert("GITHUB_OUTPUT".into(), step_out.display().to_string());
        step_env.insert("GITHUB_ENV".into(), step_env_file.display().to_string());
        step_env.insert("GITHUB_PATH".into(), step_path_file.display().to_string());
        step_env.insert("GITHUB_STEP_SUMMARY".into(), summary.display().to_string());
        // Always set: an action's own steps add to it (`$GITHUB_PATH`) from this base.
        step_env.insert(
            "PATH".into(),
            if path_extra.is_empty() { base_path.clone() } else { format!("{}:{base_path}", path_extra.join(":")) },
        );
        let limit = step
            .timeout_minutes
            .map(|m| Duration::from_secs(m as u64 * 60))
            .unwrap_or(job_limit)
            .min(job_limit.saturating_sub(job_started.elapsed()));
        let sh = Shell { name: None, bash: has_bash, boxed };
        let outcome = match (&step.uses, &step.run) {
            (Some(uses), _) if !is_builtin(uses) => {
                let ctx = Local { saved, job, workspace: &workspace, temp: &temp, sh, limit, out };
                local_action(c, &ctx, uses, step, &scope, &step_env, &idx.to_string(), 0).await
            }
            (Some(uses), _) => builtin(c, uses, step, &scope, saved, job, &workspace, &temp, out).await,
            (None, Some(script)) => match expr::interpolate(script, &scope) {
                Err(e) => {
                    out.line(&format!("The script's ${{{{ }}}} couldn't be filled in: {e}"));
                    StepOutcome::Failed(format!("{title}: {e}"))
                }
                Ok(script) => {
                    let wd = step.working_directory.clone().or_else(|| planned.job.defaults.working_directory.clone());
                    let cwd = match wd {
                        Some(w) => workspace.join(expr::interpolate(&w, &scope).unwrap_or(w)),
                        None => workspace.clone(),
                    };
                    let shell = step.shell.clone().or_else(|| planned.job.defaults.shell.clone());
                    let shell = Shell { name: shell.as_deref(), ..sh };
                    shell_step(&script, shell, &cwd, &step_env, &temp, &idx.to_string(), limit, out).await
                }
            },
            (None, None) => StepOutcome::Failed("a step needs run or uses".into()),
        };
        out.add_summary(&std::fs::read_to_string(&summary).unwrap_or_default());
        // What the step left for later steps.
        let outputs: serde_json::Map<String, Value> =
            key_values(&std::fs::read_to_string(&step_out).unwrap_or_default())
                .into_iter()
                .map(|(k, v)| (k, json!(v)))
                .collect();
        for (k, v) in key_values(&std::fs::read_to_string(&step_env_file).unwrap_or_default()) {
            env.insert(k, v);
        }
        path_extra.extend(
            std::fs::read_to_string(&step_path_file)
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_owned),
        );
        let continue_on_error = step
            .continue_on_error
            .as_deref()
            .map(|c| expr::interpolate(c, &scope).map(|v| v.trim() == "true").unwrap_or(false))
            .unwrap_or(false);
        let (outcome_word, state) = match &outcome {
            StepOutcome::Passed => ("success", RunState::Passed),
            StepOutcome::Skipped => ("skipped", RunState::Skipped),
            StepOutcome::Failed(why) => {
                if continue_on_error {
                    out.line(&format!("(continue-on-error: the job goes on) {why}"));
                } else if status == expr::Status::Success {
                    status = expr::Status::Failure;
                    failure = why.clone();
                }
                ("failure", RunState::Failed)
            }
            StepOutcome::Cancelled(why) => {
                status = expr::Status::Cancelled;
                if failure.is_empty() {
                    failure = why.clone();
                }
                ("cancelled", RunState::Cancelled)
            }
        };
        if let Some(id) = &step.id {
            let conclusion = if continue_on_error && outcome_word == "failure" { "success" } else { outcome_word };
            steps_ctx
                .insert(id.clone(), json!({ "outcome": outcome_word, "conclusion": conclusion, "outputs": outputs }));
        }
        let finished = now_ms();
        set_step(out, idx, |s| {
            s.state = state as i32;
            s.finished_at_ms = finished;
        });
    }
    // Job outputs, from what the steps left.
    let env_obj: serde_json::Map<String, Value> = env.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let scope = expr::Scope { status, ..base_scope.clone() }
        .with("env", Value::Object(env_obj))
        .with("steps", Value::Object(steps_ctx));
    let outputs: BTreeMap<String, String> = planned
        .job
        .outputs
        .iter()
        .map(|(k, v)| (k.clone(), expr::interpolate(v, &scope).unwrap_or_default()))
        .collect();
    let state = match status {
        expr::Status::Success => RunState::Passed,
        expr::Status::Failure => RunState::Failed,
        expr::Status::Cancelled => RunState::Cancelled,
    };
    Ok((state, failure, outputs))
}

fn set_step(out: &Output, idx: usize, f: impl FnOnce(&mut StepReport)) {
    if let Some(s) = out.steps.lock().unwrap().iter_mut().find(|s| s.index == idx as u32) {
        f(s);
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

enum StepOutcome {
    Passed,
    Skipped,
    Failed(String),
    Cancelled(String),
}

/// Actions that set up a toolchain (actions/setup-node, dtolnay/rust-toolchain,
/// arduino/setup-protoc, pnpm/action-setup…): on a self-hosted runner the toolchain is already
/// there, so they're answered without running anyone's code.
/// Steps the runner does itself: checkout, artifacts, releases and caches.
fn is_builtin(uses: &str) -> bool {
    let name = uses.split('@').next().unwrap_or(uses).to_ascii_lowercase();
    cache_action(&name)
        || matches!(
            name.as_str(),
            "actions/checkout"
                | "actions/upload-artifact"
                | "actions/download-artifact"
                | "grovedock/release"
                | "softprops/action-gh-release"
                | "ncipollo/release-action"
        )
}

/// Caching actions (actions/cache, Swatinem/rust-cache…): a self-hosted runner keeps its caches.
fn cache_action(name: &str) -> bool {
    let repo = name.split('/').nth(1).unwrap_or(name);
    repo == "cache" || repo.ends_with("-cache") || name.starts_with("actions/cache/")
}

/// What a step that uses an action in the repository runs with.
struct Local<'a> {
    saved: &'a Saved,
    job: &'a AssignedJob,
    workspace: &'a Path,
    temp: &'a Path,
    sh: Shell<'a>,
    limit: Duration,
    out: &'a Arc<Output>,
}

/// `uses: ./path`: an action kept in the repository, so the repository's own code. Composite
/// actions' steps run as part of this step (their `$GITHUB_ENV` and `$GITHUB_PATH` reach the job);
/// Node actions run with `node`, given their inputs as `INPUT_*`.
#[allow(clippy::too_many_arguments)]
fn local_action<'a>(
    c: &'a mut Client,
    ctx: &'a Local<'a>,
    uses: &'a str,
    step: &'a wf::Step,
    scope: &'a expr::Scope,
    env: &'a BTreeMap<String, String>,
    tag: &'a str,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = StepOutcome> + Send + 'a>> {
    Box::pin(async move {
        if depth > 5 {
            return StepOutcome::Failed(format!("{uses}: actions use each other more than 5 deep"));
        }
        if uses.starts_with("docker://") {
            return StepOutcome::Failed(format!(
                "{uses}: Docker image steps don't run here yet; use a job `container:` instead"
            ));
        }
        let rel = uses.trim_start_matches("./").trim_end_matches('/');
        let dir = if uses.starts_with("./") {
            if rel.split('/').any(|p| p == "..") {
                return StepOutcome::Failed(format!("{uses}: an action's path stays inside the repository"));
            }
            ctx.workspace.join(rel)
        } else {
            match fetch_action(uses, ctx).await {
                Ok(d) => d,
                Err(e) => return StepOutcome::Failed(format!("{uses}: {e:#}")),
            }
        };
        let Some(source) = ["action.yml", "action.yaml"].iter().find_map(|f| std::fs::read_to_string(dir.join(f)).ok())
        else {
            return StepOutcome::Failed(if uses.starts_with("./") {
                format!("{uses}: no action.yml there (check the repository out first, with actions/checkout)")
            } else {
                format!("{uses}: no action.yml in it")
            });
        };
        let action = match wf::parse_action(&format!("{rel}/action.yml"), &source) {
            Ok(a) => a,
            Err(e) => return StepOutcome::Failed(e.0),
        };
        // Inputs: what the step gives, else the action's defaults.
        let mut inputs = serde_json::Map::new();
        for i in &action.inputs {
            let v = match step.with.get(&i.name) {
                Some(v) => expr::interpolate(v, scope).unwrap_or_else(|_| v.clone()),
                None => expr::interpolate(&i.default, scope).unwrap_or_else(|_| i.default.clone()),
            };
            if i.required && v.is_empty() {
                return StepOutcome::Failed(format!("{uses}: give the input `{}`", i.name));
            }
            inputs.insert(i.name.clone(), json!(v));
        }
        for (k, v) in &step.with {
            if !inputs.contains_key(k) {
                inputs.insert(k.clone(), json!(expr::interpolate(v, scope).unwrap_or_else(|_| v.clone())));
            }
        }
        let mut env = env.clone();
        env.insert("GITHUB_ACTION_PATH".into(), dir.display().to_string());
        if let Some((name, git_ref)) = uses.rsplit_once('@').filter(|_| !uses.starts_with("./")) {
            env.insert("GITHUB_ACTION_REPOSITORY".into(), name.splitn(3, '/').take(2).collect::<Vec<_>>().join("/"));
            env.insert("GITHUB_ACTION_REF".into(), git_ref.into());
        }
        // Where setup actions keep the tools they download (kept between jobs on a machine's own
        // runner, fresh in a container).
        let tool_cache = match ctx.sh.boxed.filter(|b| b.container.is_some()) {
            Some(_) => ctx.temp.join("_tool"),
            None => ctx
                .temp
                .parent()
                .and_then(Path::parent)
                .map(|w| w.join("_tool"))
                .unwrap_or_else(|| ctx.temp.join("_tool")),
        };
        let _ = std::fs::create_dir_all(&tool_cache);
        env.insert("RUNNER_TOOL_CACHE".into(), tool_cache.display().to_string());
        env.insert("AGENT_TOOLSDIRECTORY".into(), tool_cache.display().to_string());
        let out = ctx.out;
        match action.runs {
            wf::ActionRuns::Unsupported => StepOutcome::Failed(format!(
                "{uses}: it's a Docker action; only JavaScript and composite actions run here so far"
            )),
            wf::ActionRuns::Node { main } => {
                for (k, v) in &inputs {
                    env.insert(
                        format!("INPUT_{}", k.to_uppercase().replace(' ', "_")),
                        v.as_str().unwrap_or("").into(),
                    );
                }
                let node = match node_for(ctx).await {
                    Ok(n) => n,
                    Err(e) => return StepOutcome::Failed(format!("{uses}: no Node to run it: {e:#}")),
                };
                let q = |p: &str| format!("'{}'", p.replace('\'', r"'\''"));
                // Inputs reach the action as INPUT_<NAME> with GitHub's spelling, hyphens and all,
                // which no shell can export; Node takes the whole environment from a file instead.
                let env_file = ctx.temp.join(format!("env-{tag}.json"));
                let loader = "const f=require('fs'),p=process.argv[1];Object.assign(process.env,JSON.parse(f.readFileSync(p,'utf8')));f.rmSync(p);const m=process.argv[2];process.argv.splice(1,1);m.endsWith('.mjs')?import(require('url').pathToFileURL(m).href):require(m)";
                if let Err(e) = write_private(&env_file, &serde_json::to_string(&env).unwrap_or_default()) {
                    return StepOutcome::Failed(format!("{uses}: {e:#}"));
                }
                let script = format!(
                    "{} -e {} {} {}",
                    q(&node),
                    q(loader),
                    q(&env_file.display().to_string()),
                    q(&dir.join(main).display().to_string())
                );
                let sh = Shell { name: Some("sh"), ..ctx.sh };
                shell_step(&script, sh, ctx.workspace, &env, ctx.temp, &format!("{tag}-node"), ctx.limit, out).await
            }
            wf::ActionRuns::Composite(steps) => {
                let base_path = env.get("PATH").cloned().unwrap_or_default();
                let mut steps_ctx = serde_json::Map::new();
                let mut status = expr::Status::Success;
                let mut failure = None;
                for (n, s) in steps.iter().enumerate() {
                    let env_obj: serde_json::Map<String, Value> =
                        env.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
                    let sc = expr::Scope { status, ..scope.clone() }
                        .with("inputs", Value::Object(inputs.clone()))
                        .with("steps", Value::Object(steps_ctx.clone()))
                        .with("env", Value::Object(env_obj));
                    if !expr::condition(s.cond.as_deref(), &sc).unwrap_or(false) {
                        continue;
                    }
                    let raw = s.title();
                    let title = expr::interpolate(&raw, &sc).unwrap_or(raw);
                    out.line(&format!("  ▸ {title}"));
                    let sub_tag = format!("{tag}-{n}");
                    let output_file = ctx.temp.join(format!("output-{sub_tag}"));
                    let _ = std::fs::write(&output_file, b"");
                    let mut s_env = env.clone();
                    for (k, v) in &s.env {
                        s_env.insert(k.clone(), expr::interpolate(v, &sc).unwrap_or_else(|_| v.clone()));
                    }
                    s_env.insert("GITHUB_OUTPUT".into(), output_file.display().to_string());
                    let outcome = match (&s.uses, &s.run) {
                        (Some(u), _) if !is_builtin(u) => {
                            local_action(c, ctx, u, s, &sc, &s_env, &sub_tag, depth + 1).await
                        }
                        (Some(u), _) => builtin(c, u, s, &sc, ctx.saved, ctx.job, ctx.workspace, ctx.temp, out).await,
                        (None, Some(script)) => match expr::interpolate(script, &sc) {
                            Err(e) => StepOutcome::Failed(format!("{title}: {e}")),
                            Ok(script) => {
                                let cwd = match &s.working_directory {
                                    Some(w) => ctx.workspace.join(expr::interpolate(w, &sc).unwrap_or(w.clone())),
                                    None => ctx.workspace.to_path_buf(),
                                };
                                let sh = Shell { name: s.shell.as_deref(), ..ctx.sh };
                                shell_step(&script, sh, &cwd, &s_env, ctx.temp, &sub_tag, ctx.limit, out).await
                            }
                        },
                        (None, None) => StepOutcome::Failed("a step needs run or uses".into()),
                    };
                    // What earlier steps set reaches later ones: variables, and folders put on the
                    // PATH (newest first), like a toolchain one step installs and the next runs.
                    if let Some(f) = env.get("GITHUB_ENV") {
                        for (k, v) in key_values(&std::fs::read_to_string(f).unwrap_or_default()) {
                            env.insert(k, v);
                        }
                    }
                    if let Some(f) = env.get("GITHUB_PATH") {
                        let added: Vec<String> = std::fs::read_to_string(f)
                            .unwrap_or_default()
                            .lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty())
                            .rev()
                            .map(str::to_owned)
                            .collect();
                        if !added.is_empty() {
                            env.insert("PATH".into(), format!("{}:{base_path}", added.join(":")));
                        }
                    }
                    let outputs: serde_json::Map<String, Value> =
                        key_values(&std::fs::read_to_string(&output_file).unwrap_or_default())
                            .into_iter()
                            .map(|(k, v)| (k, json!(v)))
                            .collect();
                    let word = match &outcome {
                        StepOutcome::Passed => "success",
                        StepOutcome::Skipped => "skipped",
                        StepOutcome::Failed(_) => "failure",
                        StepOutcome::Cancelled(_) => "cancelled",
                    };
                    if let Some(id) = &s.id {
                        steps_ctx
                            .insert(id.clone(), json!({ "outcome": word, "conclusion": word, "outputs": outputs }));
                    }
                    match outcome {
                        StepOutcome::Failed(why) if failure.is_none() => {
                            status = expr::Status::Failure;
                            failure = Some(StepOutcome::Failed(why));
                        }
                        StepOutcome::Cancelled(why) => return StepOutcome::Cancelled(why),
                        _ => {}
                    }
                }
                if let Some(f) = failure {
                    return f;
                }
                // The action's outputs, for the step that used it.
                let sc = scope.clone().with("inputs", Value::Object(inputs)).with("steps", Value::Object(steps_ctx));
                if let Some(f) = env.get("GITHUB_OUTPUT") {
                    let mut text = String::new();
                    for (k, v) in &action.outputs {
                        let v = expr::interpolate(v, &sc).unwrap_or_default();
                        text.push_str(&format!("{k}<<GD_EOF_7f3a\n{v}\nGD_EOF_7f3a\n"));
                    }
                    use std::io::Write;
                    let _ =
                        std::fs::OpenOptions::new().append(true).open(f).and_then(|mut h| h.write_all(text.as_bytes()));
                }
                StepOutcome::Passed
            }
        }
    })
}

/// Actions Runs does itself, and what it says about the rest.
#[allow(clippy::too_many_arguments)]
async fn builtin(
    c: &mut Client,
    uses: &str,
    step: &wf::Step,
    scope: &expr::Scope,
    saved: &Saved,
    job: &AssignedJob,
    workspace: &Path,
    temp: &Path,
    out: &Output,
) -> StepOutcome {
    let name = uses.split('@').next().unwrap_or(uses).to_ascii_lowercase();
    let with = |k: &str| step.with.get(k).map(|v| expr::interpolate(v, scope).unwrap_or_else(|_| v.clone()));
    match name.as_str() {
        "actions/checkout" => {
            match checkout(saved, job, workspace, with("path"), with("fetch-depth"), with("persist-credentials"), out)
                .await
            {
                Ok(()) => StepOutcome::Passed,
                Err(e) => StepOutcome::Failed(format!("Checkout failed: {e:#}")),
            }
        }
        n if cache_action(n) => {
            out.line(&format!("{uses}: skipped. A self-hosted runner keeps its tool caches (cargo, npm, pnpm) between jobs on its own."));
            StepOutcome::Skipped
        }
        "grovedock/release" | "softprops/action-gh-release" | "ncipollo/release-action" => {
            match release(c, saved, job, workspace, &with, out).await {
                Ok(o) => o,
                Err(e) => StepOutcome::Failed(format!("The release failed: {e:#}")),
            }
        }
        "actions/upload-artifact" => match upload_artifact(c, saved, job, workspace, temp, &with, out).await {
            Ok(o) => o,
            Err(e) => StepOutcome::Failed(format!("Keeping the artifact failed: {e:#}")),
        },
        "actions/download-artifact" => match download_artifact(c, saved, job, workspace, temp, &with, out).await {
            Ok(o) => o,
            Err(e) => StepOutcome::Failed(format!("Getting the artifact failed: {e:#}")),
        },
        _ => {
            let why = format!("`uses: {uses}` isn't one Runs does itself");
            out.line(&why);
            StepOutcome::Failed(why)
        }
    }
}

async fn checkout(
    saved: &Saved,
    job: &AssignedJob,
    workspace: &Path,
    path: Option<String>,
    depth: Option<String>,
    persist: Option<String>,
    out: &Output,
) -> Result<()> {
    let repo = job.repository.clone().unwrap_or_default();
    let target = match path {
        Some(p) if !p.is_empty() => {
            let p = Path::new(&p);
            if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                bail!("checkout path must stay inside the workspace");
            }
            workspace.join(p)
        }
        _ => workspace.to_owned(),
    };
    std::fs::create_dir_all(&target)?;
    let server = saved.server.trim_end_matches('/');
    let has_git = std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success());
    if !job.git_commit.is_empty() && has_git {
        let url = format!("{server}/{}/{}.git", repo.owner, repo.name);
        let header = format!("Authorization: Bearer {}", job.token);
        let depth: u32 = depth.as_deref().and_then(|d| d.parse().ok()).unwrap_or(1);
        out.line(&format!(
            "Fetching {} from {url}{}",
            &job.git_commit[..12.min(job.git_commit.len())],
            if depth == 0 { " (all history)" } else { "" }
        ));
        let git = |args: &[&str]| {
            let mut c = std::process::Command::new("git");
            c.current_dir(&target).env("GIT_TERMINAL_PROMPT", "0").args(args);
            c
        };
        let run = |mut c: std::process::Command, what: &str| -> Result<()> {
            let o = c.output()?;
            let text = String::from_utf8_lossy(&o.stderr).replace(&job.token, "***");
            if !o.status.success() {
                bail!("{what}: {}", text.trim());
            }
            Ok(())
        };
        run(git(&["init", "-q"]), "git init")?;
        run(git(&["remote", "add", "origin", &url]), "git remote")?;
        let mut fetch = vec![
            "-c".to_owned(),
            format!("http.extraHeader={header}"),
            "fetch".into(),
            "-q".into(),
            "--no-tags".into(),
        ];
        if depth > 0 {
            fetch.push(format!("--depth={depth}"));
        }
        fetch.push("origin".into());
        fetch.push(job.git_commit.clone());
        if depth == 0 {
            fetch.push("+refs/heads/*:refs/remotes/origin/*".into());
        }
        run(git(&fetch.iter().map(String::as_str).collect::<Vec<_>>()), "git fetch")?;
        run(git(&["checkout", "-q", "--detach", &job.git_commit]), "git checkout")?;
        if persist.as_deref() != Some("false") {
            // Later steps' git commands work as on GitHub; the token is read-only and dies with the job.
            run(git(&["config", "--local", "http.extraHeader", &header]), "git config")?;
        }
        out.line(&format!("Checked out {} (git commit {})", job.r#ref, job.git_commit));
        return Ok(());
    }
    // No git view on the server (or no git here): gd checks out the exact change.
    let me = std::env::current_exe()?;
    let changeset = grovedock_core::Id::from_slice(&job.changeset)?.to_hex();
    out.line(&format!(
        "Checking out {} with gd (no .git: this change has no git commit yet, or the server keeps no git view)",
        &changeset[..12]
    ));
    let o = tokio::process::Command::new(me)
        .args(["clone", &format!("{server}/{}/{}", repo.owner, repo.name)])
        .arg(&target)
        .args(["--ref", &changeset])
        .env("GD_TOKEN", &job.token)
        .output()
        .await?;
    if !o.status.success() {
        bail!("{}", String::from_utf8_lossy(&o.stderr).replace(&job.token, "***").trim());
    }
    out.line(&format!("Checked out {} at {}", job.r#ref, &changeset[..12]));
    Ok(())
}

/// A workspace path as a pattern names it: `./dist/` and `dist` alike; absolute paths inside
/// the workspace become relative.
fn rel_pattern(p: &str, workspace: &Path) -> String {
    let p = p.trim();
    let p = Path::new(p).strip_prefix(workspace).map(|r| r.display().to_string()).unwrap_or_else(|_| p.to_owned());
    p.trim_start_matches("./").trim_end_matches('/').to_owned()
}

/// Whether a workspace file is named by a pattern: a glob, the file itself, or a folder holding it.
fn names(pattern: &str, file: &str) -> bool {
    if pattern.contains(['*', '?', '[']) {
        return wf::filter::glob(pattern, file);
    }
    pattern.is_empty() || file == pattern || file.starts_with(&format!("{pattern}/"))
}

/// The workspace files the `path:` lines name (`!` lines take back), sorted.
fn artifact_files(workspace: &Path, lines: &str) -> Vec<String> {
    let mut files = Vec::new();
    for e in ignore::WalkBuilder::new(workspace)
        .hidden(false)
        .git_ignore(false)
        .ignore(false)
        .parents(false)
        .build()
        .flatten()
    {
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = e.path().strip_prefix(workspace) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.starts_with(".git/") {
            continue;
        }
        let mut hit = false;
        for l in lines.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            match l.strip_prefix('!') {
                Some(neg) => {
                    if names(&rel_pattern(neg, workspace), &rel) {
                        hit = false;
                    }
                }
                None => {
                    if names(&rel_pattern(l, workspace), &rel) {
                        hit = true;
                    }
                }
            }
        }
        if hit {
            files.push(rel);
        }
    }
    files.sort();
    files
}

/// The folder every file is in (GitHub keeps paths from there).
fn common_dir(files: &[String]) -> String {
    let mut common: Option<Vec<&str>> = None;
    for f in files {
        let dirs: Vec<&str> = f.split('/').collect();
        let dirs = &dirs[..dirs.len() - 1];
        common = Some(match common {
            None => dirs.to_vec(),
            Some(c) => c.iter().zip(dirs).take_while(|(a, b)| a == b).map(|(a, _)| *a).collect(),
        });
    }
    common.unwrap_or_default().join("/")
}

/// Writes `text` readable by this user only (it may hold secrets).
fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)?.write_all(text.as_bytes())?;
    Ok(())
}

/// Publishers whose actions run at any tag by default (GitHub's own and the large vendors');
/// anyone else's must be pinned to a full commit SHA, which can't be moved to other code.
const VERIFIED: &[&str] = &[
    "actions",
    "github",
    "docker",
    "aws-actions",
    "azure",
    "google-github-actions",
    "hashicorp",
    "gradle",
    "ruby",
    "oven-sh",
    "denoland",
    "pnpm",
    "microsoft",
    "cloudflare",
    "dtolnay",
    "astral-sh",
    "golangci",
    "codecov",
];

/// Whether the owner's policy lets `owner/repo@git_ref` run; why not, when it doesn't.
fn action_allowed(
    policy: Option<&grovedock_proto::ActionsPolicy>,
    owner: &str,
    repo: &str,
    git_ref: &str,
) -> Result<(), String> {
    let pinned = git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit());
    let mode = policy.map(|p| p.mode.as_str()).unwrap_or("verified");
    let name = format!("{owner}/{repo}");
    match mode {
        "all" => Ok(()),
        "list" => {
            let allowed = policy.map(|p| p.allowed.as_slice()).unwrap_or_default();
            if allowed.iter().any(|a| {
                a.eq_ignore_ascii_case(&name) || a.strip_suffix("/*").is_some_and(|o| o.eq_ignore_ascii_case(owner))
            }) {
                Ok(())
            } else {
                Err(format!("{name} isn't on this organisation's list of allowed actions"))
            }
        }
        _ if pinned || VERIFIED.iter().any(|v| v.eq_ignore_ascii_case(owner)) => Ok(()),
        _ => Err(format!(
            "{name}@{git_ref}: actions from other publishers run only pinned to a full commit SHA ({name}@<40 hex digits>), so a moved tag can't swap in other code"
        )),
    }
}

/// `owner/repo[/path]@ref`: the action's files, downloaded from GitHub once a job.
async fn fetch_action(uses: &str, ctx: &Local<'_>) -> Result<PathBuf> {
    let (spec, git_ref) = uses.rsplit_once('@').context("name its version, like owner/repo@v4 or @<commit SHA>")?;
    let mut parts = spec.splitn(3, '/');
    let (owner, repo, sub) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let safe =
        |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && s != "..";
    if !safe(owner) || !safe(repo) || !safe(git_ref.replace('/', "-").as_str()) || sub.split('/').any(|p| p == "..") {
        bail!("`{uses}` isn't an action's name");
    }
    // A tag the organisation's runs used before is pinned to the commit it pointed at then.
    let key = format!("{owner}/{repo}@{git_ref}");
    let pin = ctx.job.actions_policy.as_ref().and_then(|p| p.pins.iter().find(|x| x.action.eq_ignore_ascii_case(&key)));
    if let Some(p) = pin
        && !p.moved_to.is_empty()
    {
        bail!(
            "{key} now points at different code ({}) than when this organisation first used it ({}), so it isn't run: a moved tag is how CI gets taken over. An administrator can accept the new commit on the Runners page.",
            &p.moved_to[..p.moved_to.len().min(12)],
            &p.sha[..p.sha.len().min(12)]
        );
    }
    let fetch = pin.map_or(git_ref, |p| p.sha.as_str());
    action_allowed(ctx.job.actions_policy.as_ref(), owner, repo, fetch).map_err(|e| anyhow!(e))?;
    let dir = ctx.temp.join("_actions").join(owner).join(repo).join(fetch.replace('/', "-"));
    if !dir.exists() {
        ctx.out.line(&match pin {
            Some(p) => format!("Getting {key}, pinned to {} since its first use", &p.sha[..p.sha.len().min(12)]),
            None => format!("Getting {key}"),
        });
        let url = format!("https://codeload.github.com/{owner}/{repo}/tar.gz/{fetch}");
        let bytes = http()?
            .get(&url)
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("{owner}/{repo}@{git_ref} isn't on GitHub"))?
            .bytes()
            .await?;
        let tarball = dir.with_extension("tar.gz");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&tarball, &bytes)?;
        let ok = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&tarball)
            .arg("-C")
            .arg(&dir)
            .arg("--strip-components=1")
            .status()?;
        anyhow::ensure!(ok.success(), "couldn't unpack {owner}/{repo}@{git_ref}");
    }
    Ok(if sub.is_empty() { dir } else { dir.join(sub) })
}

/// A Node (18 or later) to run JavaScript actions with: the job container's or this machine's,
/// else one downloaded from nodejs.org (once per machine; per job inside a container), as
/// GitHub's runner brings its own.
async fn node_for(ctx: &Local<'_>) -> Result<String> {
    let boxed = ctx.sh.boxed.filter(|b| b.container.is_some());
    if let Some(b) = boxed {
        if b.node {
            return Ok("node".into());
        }
    } else if std::process::Command::new("node")
        .args(["-e", "process.exit(+process.versions.node.split('.')[0] < 18)"])
        .status()
        .is_ok_and(|s| s.success())
    {
        return Ok("node".into());
    }
    let os = if boxed.is_some() { "linux" } else { std::env::consts::OS };
    let os = if os == "macos" { "darwin" } else { os };
    let arch = if std::env::consts::ARCH == "aarch64" { "arm64" } else { "x64" };
    let home = if boxed.is_some() {
        ctx.temp.join("_node")
    } else {
        runners_dir()?.join(format!("externals/node20-{os}-{arch}"))
    };
    let bin = home.join("bin/node");
    if !bin.exists() {
        let list: Vec<Value> = http()?.get("https://nodejs.org/dist/index.json").send().await?.json().await?;
        let full = list
            .iter()
            .filter_map(|r| r["version"].as_str())
            .find(|v| v.starts_with("v20."))
            .context("nodejs.org lists no Node 20")?
            .to_owned();
        ctx.out.line(&format!("Getting Node {full} to run actions"));
        let url = format!("https://nodejs.org/dist/{full}/node-{full}-{os}-{arch}.tar.gz");
        let bytes = http()?.get(&url).send().await?.error_for_status()?.bytes().await?;
        std::fs::create_dir_all(&home)?;
        let tarball = home.with_extension("tar.gz");
        std::fs::write(&tarball, &bytes)?;
        let ok = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&tarball)
            .arg("-C")
            .arg(&home)
            .arg("--strip-components=1")
            .status()?;
        anyhow::ensure!(ok.success(), "couldn't unpack Node");
    }
    Ok(bin.display().to_string())
}

fn http() -> Result<reqwest::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Ok(reqwest::Client::builder().build()?)
}

async fn upload_artifact(
    c: &mut Client,
    saved: &Saved,
    job: &AssignedJob,
    workspace: &Path,
    temp: &Path,
    with: &(dyn Fn(&str) -> Option<String> + Sync),
    out: &Output,
) -> Result<StepOutcome> {
    let name = with("name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "artifact".into());
    let paths = with("path").context("upload-artifact needs `path`: the files to keep")?;
    let files = artifact_files(workspace, &paths);
    if files.is_empty() {
        let why = format!("No files match {}", paths.trim().replace('\n', ", "));
        return Ok(match with("if-no-files-found").as_deref() {
            Some("error") => StepOutcome::Failed(why),
            Some("ignore") => StepOutcome::Passed,
            _ => {
                out.line(&format!("warning: {why}; nothing was kept"));
                StepOutcome::Passed
            }
        });
    }
    let root = common_dir(&files);
    let base = if root.is_empty() { workspace.to_owned() } else { workspace.join(&root) };
    let list = temp.join(format!("artifact-{name}.list"));
    let names: Vec<&str> =
        files.iter().map(|f| if root.is_empty() { f.as_str() } else { &f[root.len() + 1..] }).collect();
    std::fs::write(&list, names.join("\n") + "\n")?;
    let tarball = temp.join(format!("artifact-{name}.tar.gz"));
    let o = tokio::process::Command::new("tar")
        .arg("czf")
        .arg(&tarball)
        .arg("-C")
        .arg(&base)
        .arg("-T")
        .arg(&list)
        .output()
        .await?;
    if !o.status.success() {
        bail!("tar: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    let size = std::fs::metadata(&tarball)?.len();
    let days: u32 = with("retention-days").and_then(|d| d.trim().parse().ok()).unwrap_or(0);
    let links = c
        .runner_artifacts(grovedock_proto::RunnerArtifactsRequest {
            job_id: job.id,
            name: name.clone(),
            upload: true,
            retention_days: days,
        })
        .await?
        .into_inner()
        .links;
    let link = links.first().context("the server sent no upload link")?;
    let file = tokio::fs::File::open(&tarball).await?;
    let stream = futures::stream::unfold(file, |mut f| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 1 << 20];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(bytes::Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    });
    let r = http()?
        .put(format!("{}{}", saved.server.trim_end_matches('/'), link.url))
        .header(reqwest::header::CONTENT_LENGTH, size)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await?;
    let status = r.status();
    let body: Value = r.json().await.unwrap_or(json!({}));
    if !status.is_success() {
        bail!("{}", body["error"].as_str().unwrap_or("the server refused it"));
    }
    out.line(&format!(
        "Kept {} {} as artifact {name} ({}, sha256 {})",
        files.len(),
        if files.len() == 1 { "file" } else { "files" },
        human_size(size),
        body["sha256"].as_str().unwrap_or("")
    ));
    Ok(StepOutcome::Passed)
}

/// A file streamed to a signed upload link; the server's answer.
async fn put_file(saved: &Saved, url: &str, path: &Path) -> Result<Value> {
    let size = std::fs::metadata(path)?.len();
    let file = tokio::fs::File::open(path).await?;
    let stream = futures::stream::unfold(file, |mut f| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 1 << 20];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(bytes::Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    });
    let r = http()?
        .put(format!("{}{}", saved.server.trim_end_matches('/'), url))
        .header(reqwest::header::CONTENT_LENGTH, size)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await?;
    let status = r.status();
    let body: Value = r.json().await.unwrap_or(json!({}));
    if !status.is_success() {
        bail!("{}", body["error"].as_str().unwrap_or("the server refused it"));
    }
    Ok(body)
}

/// Makes a Grovedock release from the run: grovedock/release, or the inputs of the release
/// actions teams already use (softprops/action-gh-release, ncipollo/release-action), done by
/// Grovedock, not their code.
async fn release(
    c: &mut Client,
    saved: &Saved,
    job: &AssignedJob,
    workspace: &Path,
    with: &(dyn Fn(&str) -> Option<String> + Sync),
    out: &Output,
) -> Result<StepOutcome> {
    let first =
        |keys: &[&str]| keys.iter().find_map(|k| with(k)).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let yes = |keys: &[&str]| first(keys).is_some_and(|v| v == "true");
    let mut body = first(&["body"]).unwrap_or_default();
    if let Some(p) = first(&["body_path", "bodyFile"]) {
        body = std::fs::read_to_string(workspace.join(rel_pattern(&p, workspace)))
            .with_context(|| format!("reading {p}"))?;
    }
    // Files: newline- or comma-separated patterns.
    let patterns = first(&["files", "artifacts"]).unwrap_or_default().replace(',', "\n");
    let files = if patterns.trim().is_empty() { vec![] } else { artifact_files(workspace, &patterns) };
    if !patterns.trim().is_empty() && files.is_empty() {
        let why = format!("no files match {}", patterns.trim().replace('\n', ", "));
        if yes(&["fail_on_unmatched_files"]) {
            bail!("{why}");
        }
        out.line(&format!("warning: {why}"));
    }
    let made = c
        .runner_release(grovedock_proto::RunnerReleaseRequest {
            job_id: job.id,
            tag: first(&["tag_name", "tag"]).unwrap_or_default(),
            title: first(&["name", "title"]).unwrap_or_default(),
            body,
            draft: yes(&["draft"]),
            prerelease: yes(&["prerelease"]),
            generate_notes: yes(&["generate_release_notes", "generateReleaseNotes", "generate_notes"]),
            files: files.clone(),
        })
        .await?
        .into_inner();
    out.line(&format!("{} release {}", if made.existed { "Adding to" } else { "Made" }, made.tag));
    for (file, link) in files.iter().zip(&made.uploads) {
        let answer =
            put_file(saved, &link.url, &workspace.join(file)).await.with_context(|| format!("uploading {file}"))?;
        out.line(&format!(
            "Attached {} ({}, sha256 {})",
            link.name,
            human_size(answer["size"].as_u64().unwrap_or(0)),
            answer["sha256"].as_str().unwrap_or("")
        ));
    }
    Ok(StepOutcome::Passed)
}

async fn download_artifact(
    c: &mut Client,
    saved: &Saved,
    job: &AssignedJob,
    workspace: &Path,
    temp: &Path,
    with: &(dyn Fn(&str) -> Option<String> + Sync),
    out: &Output,
) -> Result<StepOutcome> {
    let name = with("name").unwrap_or_default().trim().to_owned();
    let dest = match with("path").map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()) {
        Some(p) => {
            let p = Path::new(&p);
            if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                bail!("download-artifact's path must stay inside the workspace");
            }
            workspace.join(p)
        }
        None => workspace.to_owned(),
    };
    let links = c
        .runner_artifacts(grovedock_proto::RunnerArtifactsRequest {
            job_id: job.id,
            name: name.clone(),
            upload: false,
            retention_days: 0,
        })
        .await?
        .into_inner()
        .links;
    if links.is_empty() {
        out.line("warning: this run has no artifacts yet");
        return Ok(StepOutcome::Passed);
    }
    for l in &links {
        // Every artifact (no name): each in a folder of its own name, as on GitHub.
        let into = if name.is_empty() { dest.join(&l.name) } else { dest.clone() };
        std::fs::create_dir_all(&into)?;
        let mut r = http()?.get(format!("{}{}", saved.server.trim_end_matches('/'), l.url)).send().await?;
        if !r.status().is_success() {
            bail!("{} couldn't be fetched ({})", l.name, r.status());
        }
        let tarball = temp.join(format!("download-{}.tar.gz", l.name));
        let mut f = tokio::fs::File::create(&tarball).await?;
        let mut size = 0u64;
        while let Some(chunk) = r.chunk().await? {
            size += chunk.len() as u64;
            tokio::io::AsyncWriteExt::write_all(&mut f, &chunk).await?;
        }
        tokio::io::AsyncWriteExt::flush(&mut f).await?;
        let o = tokio::process::Command::new("tar").arg("xzf").arg(&tarball).arg("-C").arg(&into).output().await?;
        if !o.status.success() {
            bail!("tar: {}", String::from_utf8_lossy(&o.stderr).trim());
        }
        out.line(&format!(
            "Got artifact {} ({}) into {}",
            l.name,
            human_size(size),
            into.strip_prefix(workspace)
                .map(|p| p.display().to_string())
                .ok()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| ".".into())
        ));
    }
    Ok(StepOutcome::Passed)
}

fn human_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

/// The command a `shell:` names, with `{0}` for the script file.
/// How a step's script runs: its `shell:`, whether bash is there, and where (the job's container).
#[derive(Clone, Copy)]
struct Shell<'a> {
    name: Option<&'a str>,
    bash: bool,
    boxed: Option<&'a JobBox>,
}

fn shell_command(shell: Option<&str>, bash: bool, script: &Path) -> (String, Vec<String>) {
    let file = script.display().to_string();
    let words = |s: &str| -> (String, Vec<String>) {
        let mut parts: Vec<String> = s.split_whitespace().map(|w| w.replace("{0}", &file)).collect();
        if !s.contains("{0}") {
            parts.push(file.clone());
        }
        let cmd = parts.remove(0);
        (cmd, parts)
    };
    match shell.map(str::trim) {
        None if bash => words("bash --noprofile --norc -eo pipefail {0}"),
        None => words("sh -e {0}"),
        Some("bash") => words("bash --noprofile --norc -eo pipefail {0}"),
        Some("sh") => words("sh -e {0}"),
        Some("python") => words("python3 {0}"),
        Some("pwsh") => ("pwsh".into(), vec!["-command".into(), format!(". '{file}'")]),
        Some("powershell") => ("powershell".into(), vec!["-command".into(), format!(". '{file}'")]),
        Some("cmd") => (
            "cmd".into(),
            vec!["/D".into(), "/E:ON".into(), "/V:OFF".into(), "/S".into(), "/C".into(), format!("CALL \"{file}\"")],
        ),
        Some(custom) => words(custom),
    }
}

#[allow(clippy::too_many_arguments)]
async fn shell_step(
    script: &str,
    sh: Shell<'_>,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    temp: &Path,
    idx: &str,
    limit: Duration,
    out: &Arc<Output>,
) -> StepOutcome {
    let shell = sh.name;
    let ext = match shell {
        Some("python") => "py",
        Some("pwsh" | "powershell") => "ps1",
        Some("cmd") => "cmd",
        _ => "sh",
    };
    let file = temp.join(format!("step-{idx}.{ext}"));
    if let Err(e) = std::fs::write(&file, script) {
        return StepOutcome::Failed(format!("couldn't write the script: {e}"));
    }
    if !cwd.exists() {
        return StepOutcome::Failed(format!("working-directory {} doesn't exist", cwd.display()));
    }
    let (cmd, args) = shell_command(shell, sh.bash, &file);
    let mut c = match sh.boxed.and_then(|b| b.container.as_deref().map(|id| (b, id))) {
        // ponytail: a cancelled or timed-out step stops its `docker exec`; what it started in the
        // container lingers until the job's container is removed at the job's end.
        Some((b, id)) => match b.command(id, cwd, env, &temp.join(format!("env-exec-{idx}")), &cmd, &args) {
            Ok(c) => c,
            Err(e) => return StepOutcome::Failed(format!("couldn't start the step in the container: {e:#}")),
        },
        None => {
            let mut c = tokio::process::Command::new(&cmd);
            c.args(&args).current_dir(cwd).envs(env);
            c
        }
    };
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    c.process_group(0);
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return StepOutcome::Failed(format!("couldn't start {cmd}: {e}")),
    };
    let pid = child.id();
    let tail = Arc::new(Mutex::new(Vec::<String>::new()));
    let pump = |r: Box<dyn tokio::io::AsyncRead + Unpin + Send>| {
        let (out, tail) = (out.clone(), tail.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(r).split(b'\n');
            while let Ok(Some(raw)) = lines.next_segment().await {
                let line = String::from_utf8_lossy(&raw).trim_end_matches('\r').to_owned();
                // Workflow commands GitHub's runner understands, the ones that matter here.
                if let Some(secret) = line.strip_prefix("::add-mask::") {
                    out.mask(secret);
                    continue;
                }
                out.annotate(&line);
                out.line(&line);
                let mut t = tail.lock().unwrap();
                t.push(line);
                if t.len() > 200 {
                    t.remove(0);
                }
            }
        })
    };
    let a = pump(Box::new(child.stdout.take().expect("piped")));
    let b = pump(Box::new(child.stderr.take().expect("piped")));
    let started = Instant::now();
    let status = loop {
        tokio::select! {
            s = child.wait() => break Some(s),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if out.cancel.load(Ordering::SeqCst) || started.elapsed() > limit {
                    break None;
                }
            }
        }
    };
    let status = match status {
        Some(s) => s,
        None => {
            kill_group(pid);
            let _ = child.kill().await;
            let _ = a.await;
            let _ = b.await;
            return if out.cancel.load(Ordering::SeqCst) {
                out.line("Cancelled");
                StepOutcome::Cancelled("Cancelled".into())
            } else {
                let why = format!("Timed out after {} minutes", limit.as_secs().div_ceil(60));
                out.line(&why);
                StepOutcome::Failed(why)
            };
        }
    };
    let _ = a.await;
    let _ = b.await;
    match status {
        Ok(s) if s.success() => StepOutcome::Passed,
        Ok(s) => {
            let t = tail.lock().unwrap();
            // The first line that looks like the error, else the last thing it said.
            let line = t
                .iter()
                .find(|l| {
                    let l = l.to_ascii_lowercase();
                    l.contains("error") || l.contains("failed") || l.contains("panicked")
                })
                .or_else(|| t.iter().rev().find(|l| !l.trim().is_empty()))
                .cloned()
                .unwrap_or_default();
            let code = s.code().map_or("a signal".to_owned(), |c| format!("exit code {c}"));
            StepOutcome::Failed(if line.is_empty() {
                format!("ended with {code}")
            } else {
                format!("{} ({code})", line.trim().chars().take(300).collect::<String>())
            })
        }
        Err(e) => StepOutcome::Failed(format!("couldn't wait for the step: {e}")),
    }
}

/// Stops a step and everything it started: its process group (the step was started as the
/// leader of a new one). The system call, not the `kill` program: Linux's reads `kill -TERM
/// -1234` as "every process" and stops the runner and everything else its user runs.
fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()).filter(|p| *p > 1) {
        // SAFETY: kill(2) with a negative id signals only that process group.
        unsafe { libc::kill(-pid, libc::SIGTERM) };
        std::thread::sleep(Duration::from_millis(500));
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stopping a step stops its process group, and nothing else (Linux's `kill` program once
    /// stopped every process of the user here).
    #[cfg(unix)]
    #[test]
    fn stopping_a_step_stops_only_its_group() {
        use std::os::unix::process::CommandExt;
        let mut bystander = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let mut step =
            std::process::Command::new("sh").args(["-c", "sleep 30 & wait"]).process_group(0).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        kill_group(Some(step.id()));
        assert!(!step.wait().unwrap().success());
        assert!(bystander.try_wait().unwrap().is_none(), "a process outside the group was stopped");
        bystander.kill().unwrap();
        kill_group(Some(0));
        kill_group(Some(1));
    }

    #[test]
    fn reads_output_files_like_github() {
        let kv = key_values("a=1\nmulti<<EOF\nline one\nline two\nEOF\nb=x=y\n");
        assert_eq!(
            kv,
            vec![("a".into(), "1".into()), ("multi".into(), "line one\nline two".into()), ("b".into(), "x=y".into())]
        );
    }

    #[test]
    fn shells_like_github() {
        let p = Path::new("/tmp/s.sh");
        assert_eq!(shell_command(Some("sh"), true, p), ("sh".into(), vec!["-e".into(), "/tmp/s.sh".into()]));
        assert_eq!(shell_command(Some("python"), true, p).0, "python3");
        assert_eq!(shell_command(Some("perl {0}"), true, p), ("perl".into(), vec!["/tmp/s.sh".into()]));
    }

    #[test]
    fn marketplace_actions_run_by_policy() {
        let sha = "8e5e7e5ab8b370d6c329ec480221332ada57f0ab";
        // Default: verified publishers at any tag; others only pinned to a commit.
        assert!(action_allowed(None, "actions", "setup-node", "v4").is_ok());
        assert!(action_allowed(None, "ruby", "setup-ruby", "v1").is_ok());
        let e = action_allowed(None, "tj-actions", "changed-files", "v45").unwrap_err();
        assert!(e.contains("full commit SHA"), "{e}");
        assert!(action_allowed(None, "tj-actions", "changed-files", sha).is_ok());
        let all = grovedock_proto::ActionsPolicy { mode: "all".into(), ..Default::default() };
        assert!(action_allowed(Some(&all), "someone", "thing", "main").is_ok());
        let list = grovedock_proto::ActionsPolicy {
            mode: "list".into(),
            allowed: vec!["actions/*".into(), "acme/deploy".into()],
            ..Default::default()
        };
        assert!(action_allowed(Some(&list), "actions", "setup-go", "v5").is_ok());
        assert!(action_allowed(Some(&list), "acme", "deploy", "v1").is_ok());
        assert!(action_allowed(Some(&list), "ruby", "setup-ruby", "v1").is_err(), "not on the list");
        // The runner's own steps; everything else is an action it fetches.
        for b in [
            "actions/checkout@v4",
            "actions/upload-artifact@v4",
            "actions/cache@v4",
            "Swatinem/rust-cache@v2",
            "softprops/action-gh-release@v2",
        ] {
            assert!(is_builtin(b), "{b}");
        }
        for a in ["actions/setup-node@v4", "ruby/setup-ruby@v1", "./local"] {
            assert!(!is_builtin(a), "{a}");
        }
    }

    #[test]
    fn artifact_paths_like_github() {
        let ws = tempfile::tempdir().unwrap();
        for f in ["dist/app.tar.gz", "dist/notes.txt", "dist/sub/x.bin", "src/main.rs", ".git/HEAD"] {
            std::fs::create_dir_all(ws.path().join(f).parent().unwrap()).unwrap();
            std::fs::write(ws.path().join(f), b"x").unwrap();
        }
        assert_eq!(artifact_files(ws.path(), "dist/"), ["dist/app.tar.gz", "dist/notes.txt", "dist/sub/x.bin"]);
        assert_eq!(artifact_files(ws.path(), "dist/*.tar.gz\n./dist/notes.txt"), ["dist/app.tar.gz", "dist/notes.txt"]);
        assert_eq!(artifact_files(ws.path(), "dist\n!dist/sub/**"), ["dist/app.tar.gz", "dist/notes.txt"]);
        assert!(artifact_files(ws.path(), "**").iter().all(|f| !f.starts_with(".git/")));
        assert_eq!(common_dir(&["dist/a".into(), "dist/sub/b".into()]), "dist");
        assert_eq!(common_dir(&["a".into(), "dist/b".into()]), "");
    }

    #[test]
    fn workflow_commands_become_annotations() {
        let o = Output::default();
        o.mask("hunter22");
        *o.step.lock().unwrap() = "Lint".into();
        o.annotate("::error file=src/app.ts,line=12,title=Type error::Expected a string%0Agot hunter22");
        o.annotate("::warning::Deprecated API");
        o.annotate("::group::Install");
        o.annotate("not a command");
        let a = o.annotations.lock().unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(
            (a[0].level.as_str(), a[0].file.as_str(), a[0].line, a[0].title.as_str()),
            ("error", "src/app.ts", 12, "Type error")
        );
        assert_eq!(a[0].message, "Expected a string\ngot ***", "masked, with %0A undone");
        assert_eq!(a[0].step, "Lint");
        assert_eq!((a[1].level.as_str(), a[1].message.as_str()), ("warning", "Deprecated API"));
        drop(a);
        for _ in 0..60 {
            o.annotate("::notice::x");
        }
        assert_eq!(o.annotations.lock().unwrap().len(), MAX_ANNOTATIONS);
    }

    #[test]
    fn masks_secrets_in_output() {
        let o = Output::default();
        o.mask("gdp_supersecret");
        o.line("token is gdp_supersecret here");
        assert_eq!(String::from_utf8(o.pending.lock().unwrap().clone()).unwrap(), "token is *** here\n");
    }
}
