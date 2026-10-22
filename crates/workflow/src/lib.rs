//! Grovedock Runs workflow files: the GitHub Actions syntax teams already have, read from
//! `.grovedock/workflows/` or, failing that, `.github/workflows/`, so an imported repository's
//! CI runs unchanged where it can. What isn't supported is said plainly when the file is read,
//! not discovered halfway through a run.
//!
//! The server parses and plans (which jobs, in what order, on which runners); the runner
//! evaluates steps (conditions, `${{ }}`, outputs). Both use this crate.

pub mod cron;
pub mod expr;
pub mod filter;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yaml_rust2::{Yaml, YamlLoader};

/// Where workflows live: both are read, and a file name in both is the first's.
pub const DIRS: [&str; 2] = [".grovedock/workflows", ".github/workflows"];

/// Most jobs one matrix may make (GitHub's limit too).
pub const MAX_MATRIX: usize = 256;

/// Default job timeout, as on GitHub.
pub const DEFAULT_TIMEOUT_MINUTES: u32 = 360;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Defaults {
    pub shell: Option<String>,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub id: Option<String>,
    pub name: Option<String>,
    pub run: Option<String>,
    pub uses: Option<String>,
    pub with: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    #[serde(rename = "if")]
    pub cond: Option<String>,
    pub shell: Option<String>,
    pub working_directory: Option<String>,
    /// A boolean, or an expression evaluated by the runner.
    pub continue_on_error: Option<String>,
    pub timeout_minutes: Option<u32>,
}

impl Step {
    /// What the step is called in logs and on the run page.
    pub fn title(&self) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        if let Some(u) = &self.uses {
            // A pinned commit reads as git shows one: its first 7 characters.
            return match u.split_once('@') {
                Some((name, rev)) if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) => {
                    format!("Run {name}@{}", &rev[..7])
                }
                _ => format!("Run {u}"),
            };
        }
        let first = self.run.as_deref().unwrap_or("").lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        format!("Run {}", first.chars().take(80).collect::<String>())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Matrix {
    /// Each dimension and its values, in file order.
    pub dims: Vec<(String, Vec<Value>)>,
    pub include: Vec<BTreeMap<String, Value>>,
    pub exclude: Vec<BTreeMap<String, Value>>,
    pub fail_fast: bool,
    pub max_parallel: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: Option<String>,
    pub needs: Vec<String>,
    /// Runner labels, every one of which a runner must have. May use `${{ matrix.* }}`.
    pub runs_on: Vec<String>,
    #[serde(rename = "if")]
    pub cond: Option<String>,
    pub env: BTreeMap<String, String>,
    pub defaults: Defaults,
    pub timeout_minutes: u32,
    pub continue_on_error: Option<String>,
    pub strategy: Option<Matrix>,
    pub steps: Vec<Step>,
    pub outputs: BTreeMap<String, String>,
    /// `container`: the steps run inside this image (needs Docker on the runner).
    pub container: Option<Container>,
    /// `services`: containers beside the job (a database), reachable by name from a job
    /// container, or on the ports they publish otherwise.
    pub services: BTreeMap<String, Container>,
    /// `environment`: where it deploys. Its reviewers approve the job before it starts, and its
    /// secrets and variables join the repository's. May use `${{ }}`.
    pub environment: Option<String>,
    /// `uses: ./.github/workflows/x.yml`: a job that is another workflow of the repository, and
    /// its `with:`. Replaced by that workflow's jobs before planning (see [`expand`]).
    pub uses: Option<String>,
    pub with: BTreeMap<String, String>,
    /// For a job that came from a reusable workflow: the job that called it, the inputs it was
    /// given (may use `${{ }}`), and the workflow's outputs (`needs.CALLER.outputs`).
    pub called_by: Option<String>,
    pub call_inputs: BTreeMap<String, String>,
    pub call_outputs: BTreeMap<String, String>,
}

/// A `container:` or a service. Values may use `${{ }}`; the runner fills them in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Container {
    pub image: String,
    pub env: BTreeMap<String, String>,
    /// `5432:5432`, or `5432` for any free port on the runner.
    pub ports: Vec<String>,
    /// Docker volumes, `name:/path`.
    pub volumes: Vec<String>,
    /// Extra `docker create` options, like `--health-cmd pg_isready`.
    pub options: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub branches: Vec<String>,
    pub branches_ignore: Vec<String>,
    pub tags: Vec<String>,
    pub tags_ignore: Vec<String>,
    pub paths: Vec<String>,
    pub paths_ignore: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub name: String,
    pub description: String,
    pub default: String,
    pub required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Triggers {
    pub push: Option<Filter>,
    /// `change_request` (GitHub's files say `pull_request`, or `pull_request_target`, which
    /// runs the same way here: with no secrets, so it can't be turned against the repository).
    pub change_request: Option<Filter>,
    /// `manual` (GitHub's `workflow_dispatch`), with its inputs.
    pub manual: Option<Vec<Input>>,
    /// `schedule`: cron lines, in UTC, run on the default branch.
    pub schedule: Vec<String>,
    /// `workflow_call`: other workflows of the repository use this one as a job.
    pub call: Option<Call>,
    /// Events in the file that Runs doesn't start runs for (yet): said on the run page.
    pub ignored: Vec<String>,
}

/// What a reusable workflow takes and gives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub inputs: Vec<Input>,
    /// Output name to its value, like `${{ jobs.build.outputs.version }}`.
    pub outputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    /// The file it came from, like `.github/workflows/ci.yml`.
    pub file: String,
    pub name: String,
    pub on: Triggers,
    pub env: BTreeMap<String, String>,
    pub defaults: Defaults,
    pub jobs: Vec<Job>,
    /// Keys read but not acted on (`permissions`…), for the run page.
    pub notes: Vec<String>,
    /// `concurrency`: one run at a time per group.
    pub concurrency: Option<Concurrency>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Concurrency {
    /// May use `${{ }}` (github, inputs, vars).
    pub group: String,
    /// `true`, `false` or an expression: a new run stops the one in progress, rather than waiting.
    pub cancel_in_progress: String,
}

fn concurrency(y: Option<&Yaml>, at: &str) -> R<Option<Concurrency>> {
    match y {
        None | Some(Yaml::Null) => Ok(None),
        Some(Yaml::Hash(h)) => Ok(Some(Concurrency {
            group: text(key(h, "group"), &format!("{at}.group"))?
                .ok_or_else(|| ParseError(format!("{at} needs a group")))?,
            cancel_in_progress: bool_or_expr(key(h, "cancel-in-progress"), &format!("{at}.cancel-in-progress"))?
                .unwrap_or_else(|| "false".into()),
        })),
        other => {
            Ok(Some(Concurrency { group: text(other, at)?.unwrap_or_default(), cancel_in_progress: "false".into() }))
        }
    }
}

/// What started a run.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A branch or tag moved. `name` is the ref as Grovedock names it (`main`, `tags/v1.0`).
    Push {
        name: String,
        changed: Option<Vec<String>>,
    },
    /// A change request's source changed (or it opened): `target` is the branch it lands on.
    ChangeRequest {
        target: String,
        changed: Option<Vec<String>>,
    },
    Manual {
        name: String,
    },
    /// A `schedule` line came due, on the default branch.
    Schedule {
        cron: String,
    },
}

impl Event {
    /// The event's name in Grovedock: `push`, `change_request`, `manual` or `schedule`.
    pub fn name(&self) -> &'static str {
        match self {
            Event::Push { .. } => "push",
            Event::ChangeRequest { .. } => "change_request",
            Event::Manual { .. } => "manual",
            Event::Schedule { .. } => "schedule",
        }
    }

    /// The name a workflow sees as `github.event_name`: GitHub's for files in
    /// `.github/workflows`, so their conditions keep working, and Grovedock's otherwise.
    pub fn name_for(&self, file: &str) -> &'static str {
        if file.starts_with(".github/") { self.github_name() } else { self.name() }
    }

    pub fn github_name(&self) -> &'static str {
        match self {
            Event::Push { .. } => "push",
            Event::ChangeRequest { .. } => "pull_request",
            Event::Manual { .. } => "workflow_dispatch",
            Event::Schedule { .. } => "schedule",
        }
    }
}

impl Workflow {
    /// Whether this event starts the workflow.
    pub fn triggered_by(&self, event: &Event) -> bool {
        match event {
            Event::Push { name, changed } => {
                let Some(f) = &self.on.push else { return false };
                let ref_ok = match name.strip_prefix("tags/") {
                    // A filter naming only branches doesn't run for tags, and the other way round.
                    Some(tag) => {
                        (f.branches.is_empty() && f.branches_ignore.is_empty()
                            || !f.tags.is_empty()
                            || !f.tags_ignore.is_empty())
                            && filter::passes(&f.tags, &f.tags_ignore, tag)
                    }
                    None => {
                        (f.tags.is_empty() && f.tags_ignore.is_empty()
                            || !f.branches.is_empty()
                            || !f.branches_ignore.is_empty())
                            && filter::passes(&f.branches, &f.branches_ignore, name)
                    }
                };
                ref_ok && filter::paths_pass(&f.paths, &f.paths_ignore, changed.as_deref())
            }
            Event::ChangeRequest { target, changed } => {
                let Some(f) = &self.on.change_request else { return false };
                filter::passes(&f.branches, &f.branches_ignore, target)
                    && filter::paths_pass(&f.paths, &f.paths_ignore, changed.as_deref())
            }
            Event::Manual { .. } => self.on.manual.is_some(),
            Event::Schedule { cron } => self.on.schedule.iter().any(|c| c == cron),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

type R<T> = Result<T, ParseError>;

fn bad<T>(msg: impl Into<String>) -> R<T> {
    Err(ParseError(msg.into()))
}

fn key<'a>(h: &'a yaml_rust2::yaml::Hash, k: &str) -> Option<&'a Yaml> {
    h.get(&Yaml::String(k.to_owned()))
}

fn scalar(y: &Yaml) -> Option<String> {
    match y {
        Yaml::String(s) | Yaml::Real(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

fn text(y: Option<&Yaml>, what: &str) -> R<Option<String>> {
    match y {
        None | Some(Yaml::Null) => Ok(None),
        Some(v) => scalar(v).map(Some).ok_or_else(|| ParseError(format!("{what} should be text"))),
    }
}

fn list(y: Option<&Yaml>, what: &str) -> R<Vec<String>> {
    match y {
        None | Some(Yaml::Null) => Ok(Vec::new()),
        Some(Yaml::Array(a)) => {
            a.iter().map(|v| scalar(v).ok_or_else(|| ParseError(format!("{what} should be a list of text")))).collect()
        }
        Some(v) => Ok(vec![scalar(v).ok_or_else(|| ParseError(format!("{what} should be text or a list")))?]),
    }
}

fn map(y: Option<&Yaml>, what: &str) -> R<BTreeMap<String, String>> {
    match y {
        None | Some(Yaml::Null) => Ok(BTreeMap::new()),
        Some(Yaml::Hash(h)) => h
            .iter()
            .map(|(k, v)| match (scalar(k), scalar(v).or_else(|| matches!(v, Yaml::Null).then(String::new))) {
                (Some(k), Some(v)) => Ok((k, v)),
                _ => bad(format!("{what} should map names to text")),
            })
            .collect(),
        Some(Yaml::String(s)) if s.contains("${{") => {
            bad(format!("{what} as one expression isn't supported yet; list the values"))
        }
        _ => bad(format!("{what} should map names to text")),
    }
}

fn to_json(y: &Yaml) -> Value {
    match y {
        Yaml::Real(s) => {
            s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(Value::String(s.clone()), Value::Number)
        }
        Yaml::Integer(i) => Value::Number((*i).into()),
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Array(a) => Value::Array(a.iter().map(to_json).collect()),
        Yaml::Hash(h) => Value::Object(h.iter().filter_map(|(k, v)| scalar(k).map(|k| (k, to_json(v)))).collect()),
        _ => Value::Null,
    }
}

fn defaults(y: Option<&Yaml>) -> R<Defaults> {
    let Some(Yaml::Hash(h)) = y else { return Ok(Defaults::default()) };
    let Some(Yaml::Hash(run)) = key(h, "run") else { return Ok(Defaults::default()) };
    Ok(Defaults {
        shell: text(key(run, "shell"), "defaults.run.shell")?,
        working_directory: text(key(run, "working-directory"), "defaults.run.working-directory")?,
    })
}

fn bool_or_expr(y: Option<&Yaml>, what: &str) -> R<Option<String>> {
    match y {
        None | Some(Yaml::Null) => Ok(None),
        Some(Yaml::Boolean(b)) => Ok(Some(b.to_string())),
        Some(Yaml::String(s)) if s.contains("${{") || s == "true" || s == "false" => Ok(Some(s.clone())),
        _ => bad(format!("{what} should be true, false or an expression")),
    }
}

fn minutes(y: Option<&Yaml>, what: &str) -> R<Option<u32>> {
    match y {
        None | Some(Yaml::Null) => Ok(None),
        Some(Yaml::Integer(i)) if *i > 0 && *i <= 72 * 60 => Ok(Some(*i as u32)),
        Some(Yaml::Real(r)) => r
            .parse::<f64>()
            .ok()
            .filter(|f| *f > 0.0)
            .map(|f| Some(f.ceil() as u32))
            .ok_or_else(|| ParseError(format!("{what} should be a number of minutes"))),
        _ => bad(format!("{what} should be a number of minutes, up to 4320")),
    }
}

fn filter_of(y: Option<&Yaml>, event: &str) -> R<Filter> {
    let Some(Yaml::Hash(h)) = y else { return Ok(Filter::default()) };
    let f = |k: &str| list(key(h, k), &format!("on.{event}.{k}"));
    let out = Filter {
        branches: f("branches")?,
        branches_ignore: f("branches-ignore")?,
        tags: f("tags")?,
        tags_ignore: f("tags-ignore")?,
        paths: f("paths")?,
        paths_ignore: f("paths-ignore")?,
    };
    if !out.branches.is_empty() && !out.branches_ignore.is_empty() {
        return bad(format!("on.{event}: use branches or branches-ignore, not both"));
    }
    if !out.paths.is_empty() && !out.paths_ignore.is_empty() {
        return bad(format!("on.{event}: use paths or paths-ignore, not both"));
    }
    Ok(out)
}

/// The `inputs:` of `workflow_dispatch` or `workflow_call`.
fn inputs_of(body: Option<&Yaml>) -> R<Vec<Input>> {
    let mut inputs = Vec::new();
    if let Some(Yaml::Hash(h)) = body
        && let Some(Yaml::Hash(ins)) = key(h, "inputs")
    {
        for (k, v) in ins {
            let Some(k) = scalar(k) else { continue };
            let Yaml::Hash(v) = v else {
                inputs.push(Input { name: k, ..Default::default() });
                continue;
            };
            inputs.push(Input {
                name: k,
                description: text(key(v, "description"), "input description")?.unwrap_or_default(),
                default: text(key(v, "default"), "input default")?.unwrap_or_default(),
                required: matches!(key(v, "required"), Some(Yaml::Boolean(true))),
            });
        }
    }
    Ok(inputs)
}

fn triggers(y: Option<&Yaml>) -> R<Triggers> {
    let mut t = Triggers::default();
    let events: Vec<(String, Option<&Yaml>)> = match y {
        None => return bad("the workflow says nothing under `on:`, so nothing would start it"),
        Some(Yaml::String(s)) => vec![(s.clone(), None)],
        Some(Yaml::Array(a)) => a.iter().filter_map(scalar).map(|s| (s, None)).collect(),
        Some(Yaml::Hash(h)) => h.iter().filter_map(|(k, v)| scalar(k).map(|k| (k, Some(v)))).collect(),
        _ => return bad("`on:` should name events"),
    };
    for (name, body) in events {
        match name.as_str() {
            "push" => t.push = Some(filter_of(body, "push")?),
            "change_request" | "pull_request" | "pull_request_target" => {
                let f = filter_of(body, &name)?;
                if let Some(Yaml::Hash(h)) = body
                    && let Some(types) = key(h, "types")
                {
                    let types = list(Some(types), "types")?;
                    if !types.iter().any(|x| matches!(x.as_str(), "opened" | "synchronize" | "reopened")) {
                        t.ignored.push(format!("{name} ({})", types.join(", ")));
                        continue;
                    }
                }
                t.change_request = Some(f);
            }
            "schedule" => {
                let Some(Yaml::Array(items)) = body else { return bad("`schedule:` lists `- cron: '…'` lines") };
                for i in items {
                    let Yaml::Hash(h) = i else { return bad("`schedule:` lists `- cron: '…'` lines") };
                    let line = text(key(h, "cron"), "cron")?.unwrap_or_default();
                    cron::Cron::parse(&line).map_err(|e| ParseError(format!("schedule: {e}")))?;
                    t.schedule.push(line);
                }
            }
            "manual" | "workflow_dispatch" => t.manual = Some(inputs_of(body)?),
            "workflow_call" => {
                let mut outputs = BTreeMap::new();
                if let Some(Yaml::Hash(h)) = body
                    && let Some(Yaml::Hash(outs)) = key(h, "outputs")
                {
                    for (k, v) in outs {
                        let Some(name) = scalar(k) else { continue };
                        let value = match v {
                            Yaml::Hash(v) => text(key(v, "value"), &format!("workflow_call.outputs.{name}.value"))?,
                            _ => None,
                        };
                        outputs.insert(name, value.unwrap_or_default());
                    }
                }
                t.call = Some(Call { inputs: inputs_of(body)?, outputs });
            }
            other => t.ignored.push(other.to_owned()),
        }
    }
    Ok(t)
}

fn matrix(y: Option<&Yaml>, job: &str) -> R<Option<Matrix>> {
    let Some(Yaml::Hash(s)) = y else { return Ok(None) };
    let mut m = Matrix { fail_fast: !matches!(key(s, "fail-fast"), Some(Yaml::Boolean(false))), ..Default::default() };
    m.max_parallel = match key(s, "max-parallel") {
        Some(Yaml::Integer(i)) if *i > 0 => Some(*i as u32),
        _ => None,
    };
    match key(s, "matrix") {
        None | Some(Yaml::Null) => return Ok(Some(m)),
        Some(Yaml::Hash(h)) => {
            for (k, v) in h {
                let Some(k) = scalar(k) else { continue };
                let rows = |v: &Yaml| -> R<Vec<BTreeMap<String, Value>>> {
                    match v {
                        Yaml::Array(a) => a
                            .iter()
                            .map(|r| match to_json(r) {
                                Value::Object(o) => Ok(o.into_iter().collect()),
                                _ => bad(format!("jobs.{job}.strategy.matrix.{k} should list maps")),
                            })
                            .collect(),
                        _ => bad(format!("jobs.{job}.strategy.matrix.{k} should list maps")),
                    }
                };
                match k.as_str() {
                    "include" => m.include = rows(v)?,
                    "exclude" => m.exclude = rows(v)?,
                    _ => match v {
                        Yaml::Array(a) => m.dims.push((k, a.iter().map(to_json).collect())),
                        _ => {
                            return bad(format!(
                                "jobs.{job}.strategy.matrix.{k} should be a list (an expression for the whole matrix isn't supported yet)"
                            ));
                        }
                    },
                }
            }
        }
        _ => return bad(format!("jobs.{job}.strategy.matrix as one expression isn't supported yet; list the values")),
    }
    Ok(Some(m))
}

fn step(y: &Yaml, job: &str, i: usize) -> R<Step> {
    let Yaml::Hash(h) = y else { return bad(format!("jobs.{job}.steps[{i}] should be a map")) };
    let at = format!("jobs.{job}.steps[{i}]");
    let s = Step {
        id: text(key(h, "id"), &format!("{at}.id"))?,
        name: text(key(h, "name"), &format!("{at}.name"))?,
        run: text(key(h, "run"), &format!("{at}.run"))?,
        uses: text(key(h, "uses"), &format!("{at}.uses"))?,
        with: map(key(h, "with"), &format!("{at}.with"))?,
        env: map(key(h, "env"), &format!("{at}.env"))?,
        cond: text(key(h, "if"), &format!("{at}.if"))?,
        shell: text(key(h, "shell"), &format!("{at}.shell"))?,
        working_directory: text(key(h, "working-directory"), &format!("{at}.working-directory"))?,
        continue_on_error: bool_or_expr(key(h, "continue-on-error"), &format!("{at}.continue-on-error"))?,
        timeout_minutes: minutes(key(h, "timeout-minutes"), &format!("{at}.timeout-minutes"))?,
    };
    match (&s.run, &s.uses) {
        (Some(_), Some(_)) => return bad(format!("{at} has both run and uses; a step does one")),
        (None, None) => return bad(format!("{at} needs run or uses")),
        _ => {}
    }
    if let Some(c) = &s.cond {
        check_condition(c, &at)?;
    }
    Ok(s)
}

fn check_condition(c: &str, at: &str) -> R<()> {
    let c = c.trim();
    let inner = c.strip_prefix("${{").and_then(|r| r.strip_suffix("}}")).unwrap_or(c);
    if inner.contains("${{") {
        return Ok(());
    }
    expr::check(inner).map_err(|e| ParseError(format!("{at}.if: {e}")))
}

fn container(y: &Yaml, at: &str) -> R<Container> {
    let h = match y {
        Yaml::Hash(h) => h,
        other => return Ok(Container { image: text(Some(other), at)?.unwrap_or_default(), ..Default::default() }),
    };
    if key(h, "credentials").is_some() {
        return bad(format!(
            "{at}.credentials isn't supported yet: use a public image, or sign the runner's machine in to the registry (`docker login`)"
        ));
    }
    let c = Container {
        image: text(key(h, "image"), &format!("{at}.image"))?.unwrap_or_default(),
        env: map(key(h, "env"), &format!("{at}.env"))?,
        ports: list(key(h, "ports"), &format!("{at}.ports"))?,
        volumes: list(key(h, "volumes"), &format!("{at}.volumes"))?,
        options: text(key(h, "options"), &format!("{at}.options"))?.unwrap_or_default(),
    };
    if c.image.trim().is_empty() {
        return bad(format!("{at} needs an image"));
    }
    Ok(c)
}

fn job(id: &str, y: &Yaml) -> R<Job> {
    let Yaml::Hash(h) = y else { return bad(format!("jobs.{id} should be a map")) };
    let job_container = match key(h, "container") {
        Some(c) => Some(container(c, &format!("jobs.{id}.container"))?),
        None => None,
    };
    let environment = match key(h, "environment") {
        Some(Yaml::Hash(e)) => text(key(e, "name"), &format!("jobs.{id}.environment.name"))?,
        other => text(other, &format!("jobs.{id}.environment"))?,
    };
    let mut services = BTreeMap::new();
    match key(h, "services") {
        None => {}
        Some(Yaml::Hash(sv)) => {
            for (k, v) in sv {
                let Some(name) = scalar(k) else { continue };
                let c = container(v, &format!("jobs.{id}.services.{name}"))?;
                services.insert(name, c);
            }
        }
        Some(_) => return bad(format!("jobs.{id}.services should map names to containers")),
    }
    if let Some(uses) = text(key(h, "uses"), &format!("jobs.{id}.uses"))? {
        let path = uses.trim_start_matches("./");
        if !uses.starts_with("./") || !DIRS.iter().any(|d| path.starts_with(&format!("{d}/"))) || path.contains("..") {
            return bad(format!(
                "jobs.{id}.uses: only workflows of this repository can be used as a job, like ./.github/workflows/build.yml"
            ));
        }
        if key(h, "strategy").is_some() {
            return bad(format!("jobs.{id}: a matrix of a reusable workflow isn't supported yet"));
        }
        let cond = text(key(h, "if"), &format!("jobs.{id}.if"))?;
        if let Some(c) = &cond {
            check_condition(c, &format!("jobs.{id}"))?;
        }
        return Ok(Job {
            id: id.to_owned(),
            name: text(key(h, "name"), &format!("jobs.{id}.name"))?,
            needs: list(key(h, "needs"), &format!("jobs.{id}.needs"))?,
            cond,
            uses: Some(uses),
            with: map(key(h, "with"), &format!("jobs.{id}.with"))?,
            ..Default::default()
        });
    }
    let runs_on = match key(h, "runs-on") {
        Some(Yaml::Hash(r)) => {
            let mut l = list(key(r, "labels"), &format!("jobs.{id}.runs-on.labels"))?;
            if let Some(g) = text(key(r, "group"), &format!("jobs.{id}.runs-on.group"))? {
                l.push(g);
            }
            l
        }
        other => list(other, &format!("jobs.{id}.runs-on"))?,
    };
    if runs_on.is_empty() {
        return bad(format!("jobs.{id} needs runs-on: the labels of the runner it runs on"));
    }
    let steps = match key(h, "steps") {
        Some(Yaml::Array(a)) if !a.is_empty() => {
            a.iter().enumerate().map(|(i, s)| step(s, id, i)).collect::<R<Vec<_>>>()?
        }
        _ => return bad(format!("jobs.{id} has no steps")),
    };
    let mut seen = BTreeSet::new();
    for s in &steps {
        if let Some(sid) = &s.id
            && !seen.insert(sid.clone())
        {
            return bad(format!("jobs.{id}: two steps have the id `{sid}`"));
        }
    }
    let cond = text(key(h, "if"), &format!("jobs.{id}.if"))?;
    if let Some(c) = &cond {
        check_condition(c, &format!("jobs.{id}"))?;
    }
    Ok(Job {
        id: id.to_owned(),
        name: text(key(h, "name"), &format!("jobs.{id}.name"))?,
        needs: list(key(h, "needs"), &format!("jobs.{id}.needs"))?,
        runs_on,
        cond,
        env: map(key(h, "env"), &format!("jobs.{id}.env"))?,
        defaults: defaults(key(h, "defaults"))?,
        timeout_minutes: minutes(key(h, "timeout-minutes"), &format!("jobs.{id}.timeout-minutes"))?
            .unwrap_or(DEFAULT_TIMEOUT_MINUTES),
        continue_on_error: bool_or_expr(key(h, "continue-on-error"), &format!("jobs.{id}.continue-on-error"))?,
        strategy: matrix(key(h, "strategy"), id)?,
        steps,
        outputs: map(key(h, "outputs"), &format!("jobs.{id}.outputs"))?,
        container: job_container,
        services,
        environment: environment.filter(|e| !e.trim().is_empty()),
        ..Default::default()
    })
}

/// An action kept in the repository (`uses: ./path`): its `action.yml`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Action {
    pub name: String,
    pub inputs: Vec<Input>,
    /// Output name to its `value:` (composite actions).
    pub outputs: BTreeMap<String, String>,
    pub runs: ActionRuns,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum ActionRuns {
    /// `using: composite`: steps run as part of the step that uses it.
    Composite(Vec<Step>),
    /// `using: node20` (or another node): `main` is the script, relative to the action.
    Node { main: String },
    #[default]
    Unsupported,
}

/// Reads an `action.yml`. `at` names it in errors.
pub fn parse_action(at: &str, source: &str) -> R<Action> {
    let docs = YamlLoader::load_from_str(source).map_err(|e| ParseError(format!("{at} isn't valid YAML: {e}")))?;
    let Some(Yaml::Hash(top)) = docs.first() else { return bad(format!("{at} should be a map with `runs:`")) };
    let mut inputs = Vec::new();
    if let Some(Yaml::Hash(ins)) = key(top, "inputs") {
        for (k, v) in ins {
            let Some(name) = scalar(k) else { continue };
            let Yaml::Hash(v) = v else {
                inputs.push(Input { name, ..Default::default() });
                continue;
            };
            inputs.push(Input {
                name,
                description: text(key(v, "description"), "input description")?.unwrap_or_default(),
                default: text(key(v, "default"), "input default")?.unwrap_or_default(),
                required: matches!(key(v, "required"), Some(Yaml::Boolean(true))),
            });
        }
    }
    let mut outputs = BTreeMap::new();
    if let Some(Yaml::Hash(outs)) = key(top, "outputs") {
        for (k, v) in outs {
            let Some(name) = scalar(k) else { continue };
            let value = match v {
                Yaml::Hash(v) => text(key(v, "value"), &format!("{at} outputs.{name}.value"))?.unwrap_or_default(),
                _ => String::new(),
            };
            outputs.insert(name, value);
        }
    }
    let Some(Yaml::Hash(runs)) = key(top, "runs") else { return bad(format!("{at} has no `runs:`")) };
    let using = text(key(runs, "using"), &format!("{at} runs.using"))?.unwrap_or_default();
    let runs = match using.as_str() {
        "composite" => {
            let Some(Yaml::Array(a)) = key(runs, "steps") else { return bad(format!("{at} has no steps")) };
            ActionRuns::Composite(a.iter().enumerate().map(|(i, s)| step(s, "action", i)).collect::<R<Vec<_>>>()?)
        }
        u if u.starts_with("node") => ActionRuns::Node {
            main: text(key(runs, "main"), &format!("{at} runs.main"))?
                .ok_or_else(|| ParseError(format!("{at} has no runs.main")))?,
        },
        _ => ActionRuns::Unsupported,
    };
    Ok(Action { name: text(key(top, "name"), "name")?.unwrap_or_default(), inputs, outputs, runs })
}

/// Reads one workflow file. `file` is its path in the repository.
pub fn parse(file: &str, source: &str) -> R<Workflow> {
    let docs = YamlLoader::load_from_str(source).map_err(|e| ParseError(format!("{file} isn't valid YAML: {e}")))?;
    let Some(Yaml::Hash(top)) = docs.first() else {
        return bad(format!("{file} should be a map with `on:` and `jobs:`"));
    };
    let wrap = |e: ParseError| ParseError(format!("{file}: {}", e.0));
    let mut notes = Vec::new();
    for (k, note) in [
        ("permissions", "permissions: each job's token is read-only for this repository, whatever the file asks"),
        ("run-name", "run-name: runs are named after the change"),
    ] {
        if key(top, k).is_some() {
            notes.push(note.to_owned());
        }
    }
    let on = triggers(key(top, "on").or_else(|| key(top, "true"))).map_err(wrap)?;
    let Some(Yaml::Hash(jobs_y)) = key(top, "jobs") else { return bad(format!("{file} has no jobs")) };
    let mut jobs = Vec::new();
    for (k, v) in jobs_y {
        let Some(id) = scalar(k) else { continue };
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || id.starts_with(|c: char| c.is_ascii_digit() || c == '-')
        {
            return bad(format!(
                "{file}: `{id}` isn't a valid job id (letters, digits, - and _, starting with a letter or _)"
            ));
        }
        if let Yaml::Hash(h) = v
            && key(h, "concurrency").is_some()
        {
            notes.push(format!("jobs.{id}.concurrency: not applied yet (the workflow's concurrency is)"));
        }
        jobs.push(job(&id, v).map_err(wrap)?);
    }
    let ids: BTreeSet<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
    for j in &jobs {
        for n in &j.needs {
            if !ids.contains(n.as_str()) {
                return bad(format!("{file}: jobs.{} needs `{n}`, which isn't a job here", j.id));
            }
        }
    }
    // A cycle in needs would never start.
    let mut done: BTreeSet<String> = BTreeSet::new();
    while done.len() < jobs.len() {
        let ready: Vec<String> = jobs
            .iter()
            .filter(|j| !done.contains(&j.id) && j.needs.iter().all(|n| done.contains(n)))
            .map(|j| j.id.clone())
            .collect();
        if ready.is_empty() {
            let stuck: Vec<&str> = jobs.iter().filter(|j| !done.contains(&j.id)).map(|j| j.id.as_str()).collect();
            return bad(format!("{file}: jobs {} need each other, so none could start", stuck.join(", ")));
        }
        done.extend(ready);
    }
    Ok(Workflow {
        file: file.to_owned(),
        name: text(key(top, "name"), "name")
            .map_err(wrap)?
            .unwrap_or_else(|| file.rsplit('/').next().unwrap_or(file).to_owned()),
        on,
        env: map(key(top, "env"), "env").map_err(wrap)?,
        defaults: defaults(key(top, "defaults")).map_err(wrap)?,
        jobs,
        notes,
        concurrency: concurrency(key(top, "concurrency"), "concurrency").map_err(wrap)?,
    })
}

/// One job to run: a workflow job, once per matrix combination.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Planned {
    /// Unique in the run: the job id, with its matrix values for matrix jobs.
    pub key: String,
    pub name: String,
    /// The workflow job ids it waits for (every combination of each).
    pub needs: Vec<String>,
    pub matrix: Value,
    pub runs_on: Vec<String>,
    /// Ready to run: workflow `env` and `defaults` folded in, `continue-on-error` decided.
    pub job: Job,
}

/// Replaces each job that uses another workflow of the repository with that workflow's jobs,
/// as `CALLER.JOB`: they wait for what the caller waited for, run only if it would, and get
/// its `with:` as `inputs`; a job that needed the caller needs all of them. `read` gives a
/// file's text by its path in the repository.
pub fn expand(mut w: Workflow, read: &dyn Fn(&str) -> Option<String>) -> R<Workflow> {
    if !w.jobs.iter().any(|j| j.uses.is_some()) {
        return Ok(w);
    }
    let mut jobs = Vec::new();
    let mut made: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for caller in std::mem::take(&mut w.jobs) {
        let Some(uses) = &caller.uses else {
            jobs.push(caller);
            continue;
        };
        let path = uses.trim_start_matches("./");
        let at = format!("{}: jobs.{}", w.file, caller.id);
        let source =
            read(path).ok_or_else(|| ParseError(format!("{at} uses {uses}, which isn't in the repository")))?;
        let called = parse(path, &source).map_err(|e| ParseError(format!("{at}: {}", e.0)))?;
        let Some(call) = &called.on.call else {
            return bad(format!("{at}: {path} can't be used as a job: it has no `on: workflow_call`"));
        };
        if called.jobs.iter().any(|j| j.uses.is_some()) {
            return bad(format!("{at}: {path} uses another workflow itself, which isn't supported yet"));
        }
        let mut inputs = BTreeMap::new();
        for i in &call.inputs {
            match caller.with.get(&i.name) {
                Some(v) => inputs.insert(i.name.clone(), v.clone()),
                None if i.required => return bad(format!("{at}: {path} needs the input `{}`", i.name)),
                None => inputs.insert(i.name.clone(), i.default.clone()),
            };
        }
        let strip = |c: &str| {
            let c = c.trim();
            c.strip_prefix("${{").and_then(|r| r.strip_suffix("}}")).unwrap_or(c).trim().to_owned()
        };
        let label = caller.name.clone().unwrap_or_else(|| caller.id.clone());
        let mut ids = Vec::new();
        for j in &called.jobs {
            let mut n = j.clone();
            n.id = format!("{}.{}", caller.id, j.id);
            n.name = Some(format!("{label} / {}", j.name.clone().unwrap_or_else(|| j.id.clone())));
            n.needs = if j.needs.is_empty() {
                caller.needs.clone()
            } else {
                j.needs.iter().map(|x| format!("{}.{x}", caller.id)).collect()
            };
            n.cond = match (&caller.cond, &j.cond) {
                (Some(a), Some(b)) => Some(format!("${{{{ ({}) && ({}) }}}}", strip(a), strip(b))),
                (Some(a), None) => Some(a.clone()),
                (None, b) => b.clone(),
            };
            // The called workflow's own env and defaults, not the caller's (as on GitHub).
            n.env = called.env.clone().into_iter().chain(j.env.clone()).collect();
            n.defaults = Defaults {
                shell: j.defaults.shell.clone().or_else(|| called.defaults.shell.clone()),
                working_directory: j
                    .defaults
                    .working_directory
                    .clone()
                    .or_else(|| called.defaults.working_directory.clone()),
            };
            n.called_by = Some(caller.id.clone());
            n.call_inputs = inputs.clone();
            n.call_outputs = call.outputs.clone();
            ids.push(n.id.clone());
            jobs.push(n);
        }
        made.insert(caller.id.clone(), ids);
    }
    for j in &mut jobs {
        j.needs = j.needs.iter().flat_map(|n| made.get(n).cloned().unwrap_or_else(|| vec![n.clone()])).collect();
    }
    w.jobs = jobs;
    Ok(w)
}

/// Every combination of a matrix, after `exclude` and `include`.
pub fn combinations(m: &Matrix) -> Result<Vec<BTreeMap<String, Value>>, String> {
    let mut combos: Vec<BTreeMap<String, Value>> = vec![BTreeMap::new()];
    for (k, vals) in &m.dims {
        let mut next = Vec::with_capacity(combos.len() * vals.len());
        for c in &combos {
            for v in vals {
                let mut c = c.clone();
                c.insert(k.clone(), v.clone());
                next.push(c);
            }
        }
        combos = next;
        if combos.len() > MAX_MATRIX {
            return Err(format!("the matrix makes more than {MAX_MATRIX} jobs"));
        }
    }
    if m.dims.is_empty() {
        combos.clear();
    }
    combos.retain(|c| !m.exclude.iter().any(|e| e.iter().all(|(k, v)| c.get(k) == Some(v))));
    let dims: BTreeSet<&String> = m.dims.iter().map(|(k, _)| k).collect();
    let original = combos.clone();
    for inc in &m.include {
        // Added to every original combination it agrees with, without changing their values;
        // otherwise it's a combination of its own.
        let mut placed = false;
        for (i, c) in original.iter().enumerate() {
            if inc.iter().filter(|(k, _)| dims.contains(k)).all(|(k, v)| c.get(k) == Some(v)) {
                // Values an earlier include added may be replaced; the matrix's own may not.
                for (k, v) in inc {
                    combos[i].insert(k.clone(), v.clone());
                }
                placed = true;
            }
        }
        if !placed && !original.iter().any(|c| inc.iter().all(|(k, v)| c.get(k) == Some(v))) {
            combos.push(inc.clone());
        }
    }
    if combos.len() > MAX_MATRIX {
        return Err(format!("the matrix makes more than {MAX_MATRIX} jobs"));
    }
    Ok(combos)
}

/// The jobs a run of `w` has, in file order, matrices expanded. `github` is the `github`
/// context (for `runs-on` and names that use it).
pub fn plan(w: &Workflow, github: &Value) -> Result<Vec<Planned>, String> {
    let mut out = Vec::new();
    for j in &w.jobs {
        let combos = match &j.strategy {
            Some(m) if !m.dims.is_empty() || !m.include.is_empty() => {
                combinations(m).map_err(|e| format!("jobs.{}: {e}", j.id))?
            }
            _ => Vec::new(),
        };
        let one = combos.is_empty();
        for c in if one { vec![BTreeMap::new()] } else { combos } {
            let matrix = Value::Object(c.clone().into_iter().collect());
            let inputs = match &github["event"]["inputs"] {
                Value::Object(o) => Value::Object(o.clone()),
                _ => serde_json::json!({}),
            };
            let inputs = if j.called_by.is_some() { call_inputs(j, github, &inputs) } else { inputs };
            let scope = expr::Scope::default()
                .with("matrix", matrix.clone())
                .with("github", github.clone())
                .with("inputs", inputs)
                .with("vars", serde_json::json!({}))
                .with("env", serde_json::json!({}));
            let fill = |s: &str| expr::interpolate(s, &scope).map_err(|e| format!("jobs.{}: {e}", j.id));
            let mut job = j.clone();
            job.env = w.env.clone().into_iter().chain(j.env.clone()).collect();
            job.defaults = Defaults {
                shell: j.defaults.shell.clone().or_else(|| w.defaults.shell.clone()),
                working_directory: j
                    .defaults
                    .working_directory
                    .clone()
                    .or_else(|| w.defaults.working_directory.clone()),
            };
            job.continue_on_error = j.continue_on_error.as_deref().map(fill).transpose()?;
            let runs_on: Vec<String> = j.runs_on.iter().map(|l| fill(l)).collect::<Result<_, _>>()?;
            let base = match &j.name {
                Some(n) => fill(n)?,
                None => j.id.clone(),
            };
            // Named by the matrix's own values in file order, then any an include added.
            let order: Vec<&String> = j.strategy.iter().flat_map(|m| m.dims.iter().map(|(k, _)| k)).collect();
            let values: Vec<String> = order
                .iter()
                .filter_map(|k| c.get(*k))
                .chain(c.iter().filter(|(k, _)| !order.contains(k)).map(|(_, v)| v))
                .map(expr::to_text)
                .collect();
            let name = if one || j.name.as_deref().is_some_and(|n| n.contains("${{")) {
                base
            } else {
                format!("{base} ({})", values.join(", "))
            };
            let key = if one { j.id.clone() } else { format!("{} ({})", j.id, values.join(", ")) };
            out.push(Planned { key, name, needs: j.needs.clone(), matrix, runs_on, job });
        }
    }
    Ok(out)
}

/// The `inputs` of a job from a reusable workflow: its caller's `with:`, filled in where they
/// can be now; one that reads `needs` stays as written for the runner to fill in.
pub fn call_inputs(j: &Job, github: &Value, outer: &Value) -> Value {
    let scope = expr::Scope::default()
        .with("github", github.clone())
        .with("inputs", outer.clone())
        .with("vars", serde_json::json!({}));
    Value::Object(
        j.call_inputs
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(expr::interpolate(v, &scope).unwrap_or_else(|_| v.clone()))))
            .collect(),
    )
}

/// The labels a runner on this machine answers to without being told: `self-hosted`, its OS
/// and architecture as GitHub names them, and the GitHub-hosted image names for its OS, so
/// `runs-on: ubuntu-latest` finds a Linux runner.
pub fn machine_labels(os: &str, arch: &str) -> Vec<String> {
    let mut l = vec!["self-hosted".to_owned()];
    let (name, images): (&str, &[&str]) = match os {
        "linux" => ("Linux", &["ubuntu-latest", "ubuntu-24.04", "ubuntu-22.04", "ubuntu-20.04"]),
        "macos" => ("macOS", &["macos-latest", "macos-15", "macos-14", "macos-13"]),
        "windows" => ("Windows", &["windows-latest", "windows-2025", "windows-2022", "windows-2019"]),
        other => (other, &[]),
    };
    l.push(name.to_owned());
    l.push(
        match arch {
            "x86_64" => "X64",
            "aarch64" => "ARM64",
            "x86" => "X86",
            "arm" => "ARM",
            other => other,
        }
        .to_owned(),
    );
    l.extend(images.iter().map(|s| s.to_string()));
    l
}

/// Whether a runner with `have` labels can take a job that `wants` them: every label wanted,
/// compared without case.
pub fn labels_fit(wants: &[String], have: &[String]) -> bool {
    wants.iter().all(|w| have.iter().any(|h| h.eq_ignore_ascii_case(w)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CI: &str = r#"
name: CI
on:
  push:
    branches: [main, 'release/**']
    paths-ignore: ['docs/**']
  pull_request:
    branches: [main]
  workflow_dispatch:
    inputs:
      level: { description: Log level, default: info }
permissions:
  contents: read
env:
  CARGO_TERM_COLOR: always
jobs:
  test:
    name: Test
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
        rust: [stable, beta]
        exclude:
          - os: macos-latest
            rust: beta
        include:
          - os: ubuntu-latest
            rust: nightly
            experimental: true
    continue-on-error: ${{ matrix.experimental == true }}
    steps:
      - uses: actions/checkout@v4
      - run: cargo test --all
        env: { RUST: "${{ matrix.rust }}" }
  release:
    needs: test
    if: github.ref == 'refs/heads/main'
    runs-on: [self-hosted, linux]
    outputs:
      version: ${{ steps.v.outputs.version }}
    steps:
      - id: v
        run: echo "version=1.0" >> "$GITHUB_OUTPUT"
"#;

    #[test]
    fn reads_a_real_github_workflow() {
        let w = parse(".github/workflows/ci.yml", CI).unwrap();
        assert_eq!(w.name, "CI");
        assert_eq!(w.jobs.len(), 2);
        assert_eq!(w.env["CARGO_TERM_COLOR"], "always");
        assert_eq!(w.on.manual.as_ref().unwrap()[0].default, "info");
        assert_eq!(w.notes.len(), 1);
        assert_eq!(w.jobs[1].outputs["version"], "${{ steps.v.outputs.version }}");
        assert_eq!(w.jobs[0].steps[0].title(), "Run actions/checkout@v4");
        let pinned = Step {
            uses: Some("actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1".into()),
            ..Default::default()
        };
        assert_eq!(pinned.title(), "Run actions/checkout@3d3c42e");
        assert_eq!(w.jobs[0].steps[1].title(), "Run cargo test --all");
    }

    #[test]
    fn triggers_follow_the_filters() {
        let w = parse("ci.yml", CI).unwrap();
        let push = |n: &str, changed: Option<&[&str]>| Event::Push {
            name: n.into(),
            changed: changed.map(|c| c.iter().map(|s| s.to_string()).collect()),
        };
        assert!(w.triggered_by(&push("main", None)));
        assert!(w.triggered_by(&push("release/2.0", Some(&["src/a.rs"]))));
        assert!(!w.triggered_by(&push("main", Some(&["docs/a.md"]))), "only ignored paths changed");
        assert!(!w.triggered_by(&push("feature/x", None)));
        assert!(!w.triggered_by(&push("tags/v1.0", None)), "a branch filter doesn't run for tags");
        assert!(w.triggered_by(&Event::ChangeRequest { target: "main".into(), changed: None }));
        assert!(!w.triggered_by(&Event::ChangeRequest { target: "dev".into(), changed: None }));
        assert!(w.triggered_by(&Event::Manual { name: "main".into() }));
        let y = |c: &str| {
            format!("on:\n  schedule:\n    - cron: '{c}'\njobs:\n  a:\n    runs-on: x\n    steps: [{{run: x}}]")
        };
        let s = parse("n.yml", &y("0 3 * * *")).unwrap();
        assert!(s.triggered_by(&Event::Schedule { cron: "0 3 * * *".into() }));
        assert!(!s.triggered_by(&Event::Schedule { cron: "0 4 * * *".into() }));
        assert!(parse("n.yml", &y("0 25 * * *")).unwrap_err().0.contains("0–23"));
    }

    #[test]
    fn grovedock_files_use_grovedock_words() {
        let w = parse(
            ".grovedock/workflows/ci.yml",
            "on: [change_request, manual]\njobs:\n  a:\n    runs-on: x\n    steps: [{run: x}]",
        )
        .unwrap();
        let cr = Event::ChangeRequest { target: "main".into(), changed: None };
        assert!(w.triggered_by(&cr) && w.triggered_by(&Event::Manual { name: "main".into() }));
        assert_eq!(cr.name_for(&w.file), "change_request");
        assert_eq!(cr.name_for(".github/workflows/ci.yml"), "pull_request", "GitHub's files see GitHub's names");
    }

    #[test]
    fn plans_the_matrix_like_github() {
        let w = parse("ci.yml", CI).unwrap();
        let p = plan(&w, &json!({"ref": "refs/heads/main"})).unwrap();
        let keys: Vec<&str> = p.iter().map(|j| j.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "test (ubuntu-latest, stable)",
                "test (ubuntu-latest, beta)",
                "test (macos-latest, stable)",
                "test (ubuntu-latest, nightly, true)",
                "release"
            ],
            "2×2 − 1 excluded + 1 included, then release"
        );
        let nightly = p.iter().find(|j| j.matrix["rust"] == "nightly").unwrap();
        assert_eq!(nightly.runs_on, ["ubuntu-latest"]);
        assert_eq!(nightly.job.continue_on_error.as_deref(), Some("true"));
        assert_eq!(
            p.iter()
                .find(|j| j.matrix["rust"] == "stable" && j.matrix["os"] == "ubuntu-latest")
                .unwrap()
                .job
                .continue_on_error
                .as_deref(),
            Some("false")
        );
        assert!(p.iter().all(|j| j.job.env["CARGO_TERM_COLOR"] == "always"));
        assert_eq!(p[4].needs, ["test"]);
        assert_eq!(p[0].name.split(' ').next(), Some("Test"));
    }

    #[test]
    fn names_can_use_inputs() {
        let w = parse(
            "r.yml",
            "on: manual\njobs:\n  build:\n    name: Build ${{ inputs.version }}\n    runs-on: x\n    steps: [{run: x}]",
        )
        .unwrap();
        let p = plan(&w, &json!({"event": {"inputs": {"version": "1.4.0"}}})).unwrap();
        assert_eq!(p[0].name, "Build 1.4.0");
        assert_eq!(plan(&w, &json!({})).unwrap()[0].name, "Build ", "no inputs: empty, not an error");
    }

    #[test]
    fn include_extends_matching_combinations() {
        let m = Matrix {
            dims: vec![
                ("fruit".into(), vec![json!("apple"), json!("pear")]),
                ("animal".into(), vec![json!("cat"), json!("dog")]),
            ],
            include: vec![
                [("color".to_string(), json!("green"))].into(),
                [("color".to_string(), json!("pink")), ("animal".to_string(), json!("cat"))].into(),
                [("fruit".to_string(), json!("apple")), ("shape".to_string(), json!("circle"))].into(),
                [("fruit".to_string(), json!("banana"))].into(),
                [("fruit".to_string(), json!("banana")), ("animal".to_string(), json!("cat"))].into(),
            ],
            ..Default::default()
        };
        // GitHub's documented example: the 4 combinations extended (pink replaces the green an
        // earlier include added), then banana, and banana with a cat.
        let c = combinations(&m).unwrap();
        assert_eq!(c.len(), 6);
        assert_eq!(
            c[0],
            [("fruit", "apple"), ("animal", "cat"), ("color", "pink"), ("shape", "circle")]
                .map(|(k, v)| (k.to_string(), json!(v)))
                .into()
        );
        assert_eq!(c[3]["color"], "green");
        assert_eq!(c[1]["color"], "green");
        assert_eq!(c[2]["color"], "pink");
        assert_eq!(c[4], [("fruit".to_string(), json!("banana"))].into());
        assert_eq!(c[5]["animal"], "cat");
    }

    #[test]
    fn a_job_that_uses_a_workflow_becomes_its_jobs() {
        let caller = "on: push\njobs:\n  setup:\n    runs-on: x\n    steps: [{run: x}]\n  build:\n    needs: setup\n    if: github.ref == 'refs/heads/main'\n    uses: ./.github/workflows/b.yml\n    with: {target: arm}\n  after:\n    needs: build\n    runs-on: x\n    steps: [{run: 'echo ${{ needs.build.outputs.artifact }}'}]\n";
        let called = "on:\n  workflow_call:\n    inputs:\n      target: {required: true}\n      mode: {default: release}\n    outputs:\n      artifact: {value: '${{ jobs.compile.outputs.file }}'}\njobs:\n  compile:\n    runs-on: x\n    steps: [{run: x}]\n  test:\n    needs: compile\n    runs-on: x\n    steps: [{run: x}]\n";
        let read = |p: &str| (p == ".github/workflows/b.yml").then(|| called.to_owned());
        let w = expand(parse("ci.yml", caller).unwrap(), &read).unwrap();
        let ids: Vec<_> = w.jobs.iter().map(|j| (j.id.as_str(), j.needs.join(","))).collect();
        assert_eq!(
            ids,
            [
                ("setup", "".into()),
                ("build.compile", "setup".into()),
                ("build.test", "build.compile".into()),
                ("after", "build.compile,build.test".into())
            ]
        );
        let c = &w.jobs[1];
        assert_eq!(c.name.as_deref(), Some("build / compile"));
        assert_eq!((c.call_inputs["target"].as_str(), c.call_inputs["mode"].as_str()), ("arm", "release"));
        assert_eq!(c.cond.as_deref(), Some("github.ref == 'refs/heads/main'"));
        assert_eq!(c.call_outputs["artifact"], "${{ jobs.compile.outputs.file }}");
        let missing = "on: push\njobs:\n  b:\n    uses: ./.github/workflows/b.yml\n";
        assert!(expand(parse("m.yml", missing).unwrap(), &read).unwrap_err().0.contains("needs the input `target`"));
        assert!(
            parse("o.yml", "on: push\njobs:\n  b:\n    uses: org/repo/.github/workflows/b.yml@main\n")
                .unwrap_err()
                .0
                .contains("this repository")
        );
    }

    #[test]
    fn reads_actions_in_the_repository() {
        let a = parse_action(
            "a/action.yml",
            "name: Greet\ninputs:\n  who: {default: world}\noutputs:\n  said: {value: '${{ steps.s.outputs.text }}'}\nruns:\n  using: composite\n  steps:\n    - id: s\n      run: echo text=hi >> $GITHUB_OUTPUT\n      shell: bash\n",
        )
        .unwrap();
        assert_eq!(a.inputs[0].default, "world");
        assert_eq!(a.outputs["said"], "${{ steps.s.outputs.text }}");
        assert!(matches!(&a.runs, ActionRuns::Composite(s) if s.len() == 1));
        let n = parse_action("b", "runs:\n  using: node20\n  main: dist/index.js\n").unwrap();
        assert_eq!(n.runs, ActionRuns::Node { main: "dist/index.js".into() });
        assert_eq!(
            parse_action("c", "runs:\n  using: docker\n  image: Dockerfile\n").unwrap().runs,
            ActionRuns::Unsupported
        );
    }

    #[test]
    fn reads_containers_and_services() {
        let w = parse(
            "t.yml",
            r#"on: push
jobs:
  test:
    runs-on: ubuntu-latest
    container: node:${{ matrix.node }}
    strategy: {matrix: {node: [20]}}
    services:
      db:
        image: postgres:16
        env: {POSTGRES_PASSWORD: pw}
        ports: ["5432:5432"]
        options: --health-cmd "pg_isready -U postgres" --health-interval 2s
    steps: [{run: x}]
"#,
        )
        .unwrap();
        let j = &w.jobs[0];
        assert_eq!(j.container.as_ref().unwrap().image, "node:${{ matrix.node }}");
        let db = &j.services["db"];
        assert_eq!((db.image.as_str(), db.ports[0].as_str()), ("postgres:16", "5432:5432"));
        assert_eq!(db.env["POSTGRES_PASSWORD"], "pw");
        assert!(db.options.starts_with("--health-cmd"));
        let e = |y: &str| {
            parse(
                "e.yml",
                &format!("on: push\njobs:\n  d:\n    runs-on: x\n    environment: {y}\n    steps: [{{run: x}}]"),
            )
            .unwrap()
            .jobs[0]
                .environment
                .clone()
        };
        assert_eq!(e("production").as_deref(), Some("production"));
        assert_eq!(e("{name: staging, url: 'https://x'}").as_deref(), Some("staging"));
    }

    #[test]
    fn says_plainly_what_it_cant_run() {
        let e = |src: &str| parse("w.yml", src).unwrap_err().0;
        assert!(
            e("on: push\njobs:\n  a:\n    runs-on: x\n    container: {image: x, credentials: {username: u}}\n    steps: [{run: x}]")
                .contains("credentials")
        );
        assert!(
            e("on: push\njobs:\n  a:\n    runs-on: x\n    steps: [{run: x, uses: y}]").contains("both run and uses")
        );
        assert!(
            e("on: push\njobs:\n  a:\n    runs-on: x\n    needs: b\n    steps: [{run: x}]").contains("isn't a job")
        );
        assert!(e("on: push\njobs:\n  a:\n    runs-on: x\n    needs: b\n    steps: [{run: x}]\n  b:\n    runs-on: x\n    needs: a\n    steps: [{run: x}]").contains("need each other"));
        assert!(e("on: push\njobs:\n  a:\n    steps: [{run: x}]").contains("runs-on"));
        assert!(e("on: push\njobs:\n  a:\n    runs-on: x\n    steps: [{run: x, if: \"a ==\"}]").contains(".if"));
        assert!(e("jobs: {}").contains("on:"));
    }

    #[test]
    fn labels_match_github_images() {
        let have = machine_labels("linux", "x86_64");
        assert!(labels_fit(&["ubuntu-latest".into()], &have));
        assert!(labels_fit(&["self-hosted".into(), "linux".into(), "x64".into()], &have));
        assert!(!labels_fit(&["macos-latest".into()], &have));
        assert!(!labels_fit(&["self-hosted".into(), "gpu".into()], &have));
    }
}
