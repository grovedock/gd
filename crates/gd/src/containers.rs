//! Jobs in containers: a job's `container:` and `services:`, and every job when the runner was
//! started with `--containers`. Docker's own CLI does the work; nothing else is needed on the
//! machine. Each job gets its own network, its containers are labelled with the job, and all of
//! it is removed when the job ends (or when the runner next starts, if it died mid-job).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use grovedock_workflow::Container;
use serde_json::{Value, json};

/// What `gd runner start --containers` asked for.
#[derive(Clone, Debug, Default)]
pub struct Mode {
    /// Run every job in a fresh container, this image unless the job names one.
    pub image: Option<String>,
    pub cpus: Option<String>,
    pub memory: Option<String>,
    /// A container runtime, like gVisor's `runsc` (each job's system calls to a sandbox kernel).
    pub runtime: Option<String>,
}

/// A job's containers while it runs.
pub struct JobBox {
    docker: PathBuf,
    /// Names and labels this job's Docker resources: the job and a random part, since two
    /// runners on one machine (for different servers or owners) can both run a "job 1".
    tag: String,
    /// The job's own container, when steps run in one.
    pub container: Option<String>,
    /// The container's PATH, which `$GITHUB_PATH` adds to.
    pub path: String,
    pub bash: bool,
    /// A `node` the job's container has (an action's runtime), when it has one.
    pub node: bool,
    /// `job.services`, for `${{ }}`.
    pub services: Value,
}

fn docker_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("docker")).find(|p| p.is_file())
}

/// Runs `docker ARGS`; a Docker that doesn't answer in 5 minutes (a wedged daemon) is an error,
/// not a job that waits forever.
async fn run(docker: &Path, args: &[&str], env: &[(String, String)]) -> Result<String> {
    let cmd = tokio::process::Command::new(docker)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(300), cmd)
        .await
        .map_err(|_| {
            anyhow::anyhow!("docker {} didn't answer in 5 minutes; is Docker running?", args.first().unwrap_or(&""))
        })?
        .with_context(|| format!("running docker {}", args.first().unwrap_or(&"")))?;
    if !out.status.success() {
        bail!("docker {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Options a workflow may pass to `docker create`. Anything else (`--privileged`, `--network`,
/// `--volume`, `--cap-add`…) could reach the runner's machine, so it's refused.
const ALLOWED: &[(&str, bool)] = &[
    ("--health-cmd", true),
    ("--health-interval", true),
    ("--health-timeout", true),
    ("--health-retries", true),
    ("--health-start-period", true),
    ("--entrypoint", true),
    ("--user", true),
    ("-u", true),
    ("--env", true),
    ("-e", true),
    ("--hostname", true),
    ("--shm-size", true),
    ("--tmpfs", true),
    ("--ulimit", true),
    ("--memory", true),
    ("-m", true),
    ("--cpus", true),
    ("--init", false),
];

fn options(text: &str) -> Result<Vec<String>> {
    let words = shlex::split(text).with_context(|| format!("options `{text}` aren't quoted properly"))?;
    let mut out = Vec::new();
    let mut it = words.into_iter();
    while let Some(w) = it.next() {
        let flag = w.split('=').next().unwrap_or(&w).to_owned();
        let Some((_, takes)) = ALLOWED.iter().find(|(f, _)| *f == flag) else {
            bail!("`{flag}` isn't allowed in options: it could reach the runner's machine");
        };
        out.push(w.clone());
        if *takes && !w.contains('=') {
            out.push(it.next().with_context(|| format!("`{flag}` needs a value"))?);
        }
    }
    Ok(out)
}

/// A port published on this machine only: `5432:5432`, or `5432` for any free port.
fn publish(p: &str) -> String {
    if p.contains(':') { format!("127.0.0.1:{p}") } else { format!("127.0.0.1::{p}") }
}

/// `name:/path` volumes only, each private to the job; a path on the runner's machine is refused.
fn volume(tag: &str, v: &str) -> Result<(String, String)> {
    let (name, rest) = v.split_once(':').with_context(|| format!("volume `{v}` should be name:/path"))?;
    if !name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        bail!("volume `{v}`: only named volumes, not paths on the runner's machine");
    }
    let private = format!("gd-job-{tag}-{name}");
    Ok((private.clone(), format!("{private}:{rest}")))
}

impl JobBox {
    /// Starts a job's services and, if it has one (or the runner runs every job in one), its
    /// container, with `dir` (the job's folder) at the same path inside.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        job: u64,
        runner: &str,
        dir: &Path,
        workspace: &Path,
        container: Option<Container>,
        services: &BTreeMap<String, Container>,
        mode: &Mode,
        say: &(dyn Fn(&str) + Sync),
    ) -> Result<JobBox> {
        let docker = docker_path().context(
            "this job needs Docker (it has `container:` or `services:`), and the runner's machine has no `docker`",
        )?;
        run(&docker, &["version", "--format", "{{.Server.Version}}"], &[])
            .await
            .context("this job needs Docker, and Docker isn't running on the runner's machine")?;
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let tag = format!("{job}-{:08x}", nanos ^ std::process::id().rotate_left(16));
        let network = format!("gd-job-{tag}");
        let mut b = JobBox {
            docker,
            tag: tag.clone(),
            container: None,
            path: String::new(),
            bash: false,
            node: false,
            services: json!({}),
        };
        let label = format!("gd.job={tag}");
        let runner_label = format!("gd.runner={runner}");
        run(&b.docker, &["network", "create", "--label", &label, "--label", &runner_label, &network], &[]).await?;
        let in_container = container.is_some() || mode.image.is_some();
        for (name, s) in services {
            say(&format!("Starting service {name} ({})", s.image));
            b.pull(&s.image, say).await?;
            let mut args: Vec<String> =
                ["create", "--label", &label, "--label", &runner_label, "--network", &network].map(String::from).into();
            args.extend(["--network-alias".into(), name.clone(), "--name".into(), format!("gd-job-{tag}-{name}")]);
            if let Some(r) = &mode.runtime {
                args.extend(["--runtime".into(), r.clone()]);
            }
            let env: Vec<(String, String)> = s.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            for (k, _) in &env {
                args.extend(["-e".into(), k.clone()]);
            }
            for p in &s.ports {
                // Published on this machine only; a job in a container reaches services by name.
                args.extend(["-p".into(), publish(p)]);
            }
            for v in &s.volumes {
                let (vol, spec) = volume(&tag, v)?;
                run(&b.docker, &["volume", "create", "--label", &label, "--label", &runner_label, &vol], &[]).await?;
                args.extend(["-v".into(), spec]);
            }
            args.extend(options(&s.options).with_context(|| format!("service {name}"))?);
            args.push(s.image.clone());
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let id = run(&b.docker, &refs, &env).await?;
            run(&b.docker, &["start", &id], &[]).await?;
            b.wait_healthy(name, &id, say).await?;
            let mut ports = serde_json::Map::new();
            for line in run(&b.docker, &["port", &id], &[]).await.unwrap_or_default().lines() {
                // 5432/tcp -> 127.0.0.1:49153
                if let Some((inner, outer)) = line.split_once(" -> ") {
                    let inner = inner.split('/').next().unwrap_or(inner);
                    let outer = outer.rsplit(':').next().unwrap_or(outer);
                    ports.insert(inner.to_owned(), json!(outer));
                }
            }
            b.services[name] = json!({ "id": id, "network": network, "ports": ports });
        }
        if in_container {
            let c = container
                .unwrap_or_else(|| Container { image: mode.image.clone().unwrap_or_default(), ..Default::default() });
            say(&format!("Starting the job's container ({})", c.image));
            b.pull(&c.image, say).await?;
            let d = dir.display().to_string();
            let mut args: Vec<String> =
                ["create", "--label", &label, "--label", &runner_label, "--network", &network].map(String::from).into();
            args.extend([
                "--name".into(),
                format!("gd-job-{tag}"),
                "-v".into(),
                format!("{d}:{d}"),
                "-w".into(),
                workspace.display().to_string(),
                "--pids-limit".into(),
                "4096".into(),
            ]);
            if let Some(r) = &mode.runtime {
                args.extend(["--runtime".into(), r.clone()]);
            }
            if let Some(c) = &mode.cpus {
                args.extend(["--cpus".into(), c.clone()]);
            }
            if let Some(m) = &mode.memory {
                args.extend(["--memory".into(), m.clone()]);
            }
            let env: Vec<(String, String)> = c.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            for (k, _) in &env {
                args.extend(["-e".into(), k.clone()]);
            }
            for p in &c.ports {
                args.extend(["-p".into(), publish(p)]);
            }
            for v in &c.volumes {
                let (vol, spec) = volume(&tag, v)?;
                run(&b.docker, &["volume", "create", "--label", &label, "--label", &runner_label, &vol], &[]).await?;
                args.extend(["-v".into(), spec]);
            }
            args.extend(options(&c.options).context("container")?);
            // Kept alive while steps run in it, as GitHub's runner does.
            args.extend(["--entrypoint".into(), "tail".into(), c.image.clone(), "-f".into(), "/dev/null".into()]);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let id = run(&b.docker, &refs, &env).await?;
            run(&b.docker, &["start", &id], &[]).await?;
            b.path = run(&b.docker, &["exec", &id, "sh", "-c", "echo $PATH"], &[]).await?;
            b.bash =
                run(&b.docker, &["exec", &id, "sh", "-c", "command -v bash"], &[]).await.is_ok_and(|s| !s.is_empty());
            b.node = run(
                &b.docker,
                &["exec", &id, "sh", "-c", "node -e 'process.exit(+process.versions.node.split(\".\")[0] < 18)'"],
                &[],
            )
            .await
            .is_ok();
            b.container = Some(id);
        }
        Ok(b)
    }

    async fn pull(&self, image: &str, say: &(dyn Fn(&str) + Sync)) -> Result<()> {
        if run(&self.docker, &["image", "inspect", "--format", "{{.Id}}", image], &[]).await.is_ok() {
            return Ok(());
        }
        say(&format!("Pulling {image}"));
        run(&self.docker, &["pull", "--quiet", image], &[]).await.map(|_| ())
    }

    async fn wait_healthy(&self, name: &str, id: &str, say: &(dyn Fn(&str) + Sync)) -> Result<()> {
        let started = Instant::now();
        loop {
            let state = run(
                &self.docker,
                &["inspect", "--format", "{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}}", id],
                &[],
            )
            .await?;
            match state.split_whitespace().collect::<Vec<_>>()[..] {
                ["running"] | ["running", "healthy"] => {
                    say(&format!("Service {name} is ready"));
                    return Ok(());
                }
                ["running", "starting"] if started.elapsed() < Duration::from_secs(300) => {}
                _ => {
                    let logs = run(&self.docker, &["logs", "--tail", "20", id], &[]).await.unwrap_or_default();
                    for l in logs.lines() {
                        say(l);
                    }
                    bail!("service {name} didn't become ready ({state})");
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// `docker exec` of `cmd args` in the job's container, in `cwd`, with `env` read from a file
    /// in the job's folder: values never appear in a process list or in Docker's own environment.
    pub fn command(
        &self,
        container: &str,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_file: &Path,
        cmd: &str,
        args: &[String],
    ) -> Result<tokio::process::Command> {
        let mut text = String::new();
        for (k, v) in env {
            if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                text.push_str(&format!("export {k}='{}'\n", v.replace('\'', r"'\''")));
            }
        }
        write_private(env_file, &text)?;
        let mut c = tokio::process::Command::new(&self.docker);
        c.args(["exec", "-w"])
            .arg(cwd)
            .arg(container)
            .args(["sh", "-c", ". \"$0\" && exec \"$@\""])
            .arg(env_file)
            .arg(cmd)
            .args(args);
        Ok(c)
    }

    /// Removes everything the job started.
    pub async fn stop(self, dir: &Path) {
        // Files the container's root made stay removable by the runner.
        if let Some(id) = &self.container {
            let (u, g) = ids();
            let me = format!("{u}:{g}");
            let _ = run(&self.docker, &["exec", id, "chown", "-R", &me, &dir.display().to_string()], &[]).await;
        }
        clean(&self.docker, &format!("gd.job={}", self.tag)).await;
    }
}

/// This user's uid and gid.
fn ids() -> (String, String) {
    let id = |f: &str| {
        std::process::Command::new("id")
            .arg(f)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    (id("-u"), id("-g"))
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)?.write_all(text.as_bytes())?;
    Ok(())
}

async fn clean(docker: &Path, label: &str) {
    let filter = format!("label={label}");
    if let Ok(ids) = run(docker, &["ps", "-aq", "--filter", &filter], &[]).await
        && !ids.is_empty()
    {
        let mut args = vec!["rm", "-f", "-v"];
        args.extend(ids.lines());
        let _ = run(docker, &args, &[]).await;
    }
    for kind in ["network", "volume"] {
        if let Ok(ids) = run(docker, &[kind, "ls", "-q", "--filter", &filter], &[]).await
            && !ids.is_empty()
        {
            let mut args = vec![kind, "rm"];
            args.extend(ids.lines());
            let _ = run(docker, &args, &[]).await;
        }
    }
}

/// What a runner that died mid-job left behind.
pub async fn clean_runner(runner: &str) {
    if let Some(docker) = docker_path() {
        clean(&docker, &format!("gd.runner={runner}")).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_harmless_options_and_named_volumes() {
        assert_eq!(
            options(r#"--health-cmd "pg_isready -U postgres" --health-interval=2s --init"#).unwrap(),
            ["--health-cmd", "pg_isready -U postgres", "--health-interval=2s", "--init"]
        );
        for bad in ["--privileged", "--network host", "-v /:/host", "--cap-add SYS_ADMIN", "--pid=host"] {
            assert!(options(bad).is_err(), "{bad}");
        }
        assert_eq!((publish("5432").as_str(), publish("1:2").as_str()), ("127.0.0.1::5432", "127.0.0.1:1:2"));
        assert_eq!(volume("7", "cache:/root/.cache").unwrap().1, "gd-job-7-cache:/root/.cache");
        for bad in ["/var/run/docker.sock:/var/run/docker.sock", "./x:/x", "~/x:/x", "nocolon"] {
            assert!(volume("7", bad).is_err(), "{bad}");
        }
    }
}
