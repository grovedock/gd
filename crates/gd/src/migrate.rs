//! `gd migrate`: move a repository from any git host to Grovedock in one command (ADR-028).
//! Full history comes from git itself; from GitHub, open pull requests become change requests
//! and issues come across with their discussion, labels and state. The old host can be kept
//! current afterwards with `gd mirror`, so the move is reversible.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use gd::features::git;
use gd::remote::GrpcRemote;
use grovedock_core::flags::FeatureFlags;
use grovedock_proto::{
    AddIssueCommentRequest, CreateChangeRequestRequest, CreateIssueRequest, CreateRepositoryRequest, IssueRef,
    IssueState, RepositoryRef, UpdateIssueRequest,
};
use serde_json::Value;

pub struct Migration {
    /// Where the repository is now: any URL git can clone.
    pub from: String,
    /// Where it goes: SERVER/OWNER/NAME.
    pub to: String,
    /// A GitHub token, for private repositories and for pull requests and issues.
    pub github_token: Option<String>,
    pub issues: bool,
    pub pull_requests: bool,
    /// GitHub's API: https://api.github.com, or GitHub Enterprise Server's
    /// (https://github.example.com/api/v3). Setting it treats `from` as a GitHub repository.
    pub github_api: Option<String>,
    /// Progress as JSON lines on stdout (`{"event": …}`), for the server's importer.
    pub json: bool,
}

/// Progress, as text for a person or a JSON line for the server's importer.
struct Say(bool);

impl Say {
    fn tell(&self, event: Value, text: impl FnOnce() -> String) {
        if self.0 {
            println!("{event}");
        } else {
            println!("{}", text());
        }
    }

    /// Progress only the importer shows (a person sees the totals).
    fn progress(&self, event: Value) {
        if self.0 {
            println!("{event}");
        }
    }

    /// Something skipped: on stderr as text, or as a JSON note.
    fn note(&self, text: String) {
        if self.0 {
            println!("{}", serde_json::json!({ "event": "note", "text": text }));
        } else {
            eprintln!("{text}");
        }
    }
}

/// `owner/repo` from the last two path segments of any repository URL.
fn last_two(url: &str) -> Option<(String, String)> {
    let path = url.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.rsplit(['/', ':', '\\']);
    let repo = parts.next()?;
    let owner = parts.next()?;
    (!owner.is_empty() && !repo.is_empty()).then(|| (owner.to_owned(), repo.to_owned()))
}

/// `owner/repo` if `url` is a GitHub repository.
pub fn github_repo(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, repo) = rest.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then(|| (owner.to_owned(), repo.to_owned()))
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

struct GitHub {
    http: reqwest::Client,
    api: String,
    repo: String,
    token: Option<String>,
}

impl GitHub {
    /// Every page of a list endpoint.
    async fn all(&self, path: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for page in 1.. {
            let sep = if path.contains('?') { '&' } else { '?' };
            let url = format!("{}/repos/{}/{path}{sep}per_page=100&page={page}", self.api, self.repo);
            let mut req =
                self.http.get(&url).header("accept", "application/vnd.github+json").header("user-agent", "gd-migrate");
            if let Some(t) = &self.token {
                req = req.bearer_auth(t);
            }
            let resp = req.send().await.with_context(|| format!("calling {url}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let hint = if status.as_u16() == 403 || status.as_u16() == 429 {
                    " (rate limited: set GD_GITHUB_TOKEN or pass --github-token-file)"
                } else if status.as_u16() == 404 && self.token.is_none() {
                    // GitHub answers 404, not 401, for a private repository read without a token.
                    " (a private repository? GitHub needs a token to show its pull requests and issues)"
                } else {
                    ""
                };
                bail!("GitHub answered {status} for {path}{hint}");
            }
            let items: Vec<Value> = resp.json().await?;
            let n = items.len();
            out.extend(items);
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }

    async fn one(&self, path: &str) -> Result<Value> {
        let mut req =
            self.http.get(format!("{}/repos/{}{path}", self.api, self.repo)).header("user-agent", "gd-migrate");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        Ok(req.send().await?.error_for_status()?.json().await?)
    }
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}

fn attribution(item: &Value, what: &str) -> String {
    format!("\n\n---\n_Moved from {} where @{} opened this {what}._", s(item, "html_url"), s(&item["user"], "login"))
}

/// Attaches each `refs/notes/ai` note in the clone to the change its commit became. A note
/// that can't be read is reported and skipped: attribution is worth having, not worth failing a
/// move over. Returns how many were kept.
async fn keep_git_ai_notes(
    remote: &GrpcRemote,
    dir: &Path,
    changes: &std::collections::HashMap<String, grovedock_core::Id>,
    say: &Say,
) -> usize {
    let Ok(list) = git(dir, &["notes", "--ref", grovedock_core::gitai::NOTES_REF, "list"]) else {
        return 0; // no notes ref
    };
    let mut kept = 0;
    for line in list.lines() {
        let Some((blob, commit)) = line.split_once(' ') else { continue };
        let Some(&change) = changes.get(commit) else { continue };
        let note = match git(dir, &["cat-file", "blob", blob]) {
            Ok(n) => n,
            Err(_) => continue,
        };
        match remote.attach_git_ai_note(change, &note).await {
            Ok(()) => kept += 1,
            Err(e) => say.note(format!("git-ai note on {}: {e:#}; skipped", &commit[..commit.len().min(12)])),
        }
    }
    kept
}

pub async fn run(m: Migration, token: Option<&str>, flags: &dyn FeatureFlags) -> Result<()> {
    let say = Say(m.json);
    let (base, repo) = grovedock_proto::split_repo_url(&m.to);
    let repo = repo.context("give the destination as SERVER/OWNER/NAME")?;
    let (owner, name) = repo.split_once('/').context("give the destination as SERVER/OWNER/NAME")?;
    let server = GrpcRemote::connect(&base, None, token)?;
    let found = match &m.github_api {
        Some(_) => last_two(&m.from),
        None => github_repo(&m.from),
    };
    let api = m.github_api.clone().unwrap_or_else(|| "https://api.github.com".into()).trim_end_matches('/').to_owned();
    let gh = found.map(|(o, r)| GitHub {
        api,
        http: {
            let _ = rustls::crypto::ring::default_provider().install_default();
            reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().expect("an HTTP client")
        },
        repo: format!("{o}/{r}"),
        token: m.github_token.clone(),
    });

    // 1. The repository, described as it was.
    let description = match &gh {
        Some(g) => g.one("").await.map(|v| s(&v, "description")).unwrap_or_default(),
        None => String::new(),
    };
    // One already there (the importer makes it first, and its token reaches only it) is added to.
    let there = server
        .platform()
        .get_repository(grovedock_proto::RepositoryRef { owner: owner.into(), name: name.into() })
        .await
        .is_ok();
    if there {
        say.tell(serde_json::json!({ "event": "repository", "created": false }), || {
            format!("{owner}/{name} exists; adding to it")
        });
    } else {
        match server
            .platform()
            .create_repository(CreateRepositoryRequest {
                owner: owner.into(),
                name: name.into(),
                description,
                ..Default::default()
            })
            .await
        {
            Ok(_) => say.tell(serde_json::json!({ "event": "repository", "created": true }), || {
                format!("created {owner}/{name}")
            }),
            Err(e) if e.code() == tonic::Code::AlreadyExists => say
                .tell(serde_json::json!({ "event": "repository", "created": false }), || {
                    format!("{owner}/{name} exists; adding to it")
                }),
            Err(e) => bail!("creating {owner}/{name}: {}", e.message()),
        }
    }

    // 2. History: a bare mirror clone (in a fresh, private, randomly named directory), then
    // every branch.
    let work = tempfile::Builder::new().prefix("gd-migrate-").tempdir().context("creating a scratch directory")?;
    let dir = work.path().join("mirror.git");
    let shown = git::redact(&m.from);
    say.tell(serde_json::json!({ "event": "cloning" }), || format!("cloning {shown} ..."));
    let mut clone = Command::new("git");
    clone.env("GIT_TERMINAL_PROMPT", "0");
    if let (Some(_), Some(t)) = (&gh, &m.github_token)
        && let Some(host) = m.from.strip_prefix("https://").and_then(|r| r.split('/').next())
    {
        // The token goes only to the repository's own host, and through git's environment
        // rather than its command line, which other users of the machine can see.
        clone.env("GIT_CONFIG_COUNT", "1");
        clone.env("GIT_CONFIG_KEY_0", format!("http.https://{host}/.extraheader"));
        clone.env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: bearer {t}"));
    }
    let status =
        clone.args(["clone", "--mirror", "--quiet", &m.from]).arg(&dir).status().context("running git clone")?;
    if !status.success() {
        bail!("git could not clone {shown}");
    }
    let default = git(&dir, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|_| "main".into());
    let remote = GrpcRemote::connect(&format!("{base}/{owner}/{name}"), None, token)?;
    let branches: Vec<String> =
        git(&dir, &["for-each-ref", "--format=%(refname:short)", "refs/heads"])?.lines().map(str::to_owned).collect();
    let mut ordered = vec![default.clone()];
    ordered.extend(branches.into_iter().filter(|b| b != &default));
    say.progress(serde_json::json!({ "event": "history", "total": ordered.len() }));
    let mut changes = std::collections::HashMap::new();
    for (i, b) in ordered.iter().enumerate() {
        let out = git::import(&remote, flags, &dir, &format!("refs/heads/{b}"), b, false)
            .await
            .with_context(|| format!("importing {b}"))?;
        changes.extend(out.changes.iter().map(|(k, v)| (k.clone(), *v)));
        say.tell(
            serde_json::json!({ "event": "branch", "branch": b, "commits": out.commits, "done": i + 1, "total": ordered.len() }),
            || format!("  {b}: {} commits ({} files uploaded)", out.commits, out.uploaded),
        );
    }

    // 2b. git-ai notes: which lines of each commit an agent wrote, kept with its change.
    let attributed = keep_git_ai_notes(&remote, &dir, &changes, &say).await;
    if attributed > 0 {
        say.tell(serde_json::json!({ "event": "attribution", "commits": attributed }), || {
            format!("  kept git-ai attribution for {attributed} commits")
        });
    }

    let done = || {
        say.tell(serde_json::json!({ "event": "done", "default_branch": default }), || {
            format!(
                "done. Clone it: gd clone {base}/{owner}/{name}\n\
                 to keep {shown} current while you move over: gd mirror --to {shown} --server {base}/{owner}/{name} --every 10"
            )
        });
    };
    let Some(gh) = gh else {
        done();
        return Ok(());
    };
    let dest = || Some(RepositoryRef { owner: owner.into(), name: name.into() });

    // 3. Open pull requests become change requests from github/pr-N branches.
    if m.pull_requests {
        // The history is in: a host that won't list them leaves a note, not a failed move.
        let prs = match gh.all("pulls?state=open").await {
            Ok(p) => p,
            Err(e) => {
                say.note(format!(
                    "open pull requests couldn't be read ({e:#}); a token that can read them brings them later"
                ));
                Vec::new()
            }
        };
        say.tell(serde_json::json!({ "event": "pull_requests", "total": prs.len() }), || {
            format!("moving {} open pull requests ...", prs.len())
        });
        for pr in &prs {
            let n = pr["number"].as_u64().unwrap_or(0);
            let branch = format!("github/pr-{n}");
            let head = format!("refs/pull/{n}/head");
            if git(&dir, &["rev-parse", "--verify", "-q", &head]).is_err() {
                say.note(format!("pull request #{n}: its commits are not in the repository; skipped"));
                continue;
            }
            if let Err(e) = git::import(&remote, flags, &dir, &head, &branch, false).await {
                say.note(format!("pull request #{n}: {e:#}; skipped"));
                continue;
            }
            let target = s(&pr["base"], "ref");
            let target = if ordered.contains(&target) { target } else { default.clone() };
            let created = server
                .reviews()
                .create_change_request(CreateChangeRequestRequest {
                    milestone: 0,
                    repository: dest(),
                    source: branch,
                    target,
                    title: s(pr, "title"),
                    body: format!("{}{}", s(pr, "body"), attribution(pr, "pull request")),
                    draft: pr["draft"].as_bool().unwrap_or(false),
                    reviewers: vec![],
                    source_repository: None,
                })
                .await;
            match created {
                Ok(c) => {
                    let to = c.into_inner().number;
                    say.tell(serde_json::json!({ "event": "pull_request", "number": n, "change_request": to }), || {
                        format!("  pull request #{n} -> change request #{to}")
                    })
                }
                Err(e) => say.note(format!("pull request #{n}: {}; skipped", e.message())),
            }
        }
    }

    // 4. Issues, oldest first, with their comments and state.
    if m.issues {
        let items = match gh.all("issues?state=all&sort=created&direction=asc").await {
            Ok(i) => i,
            Err(e) => {
                say.note(format!("issues couldn't be read ({e:#}); a token that can read them brings them later"));
                Vec::new()
            }
        };
        let issues: Vec<&Value> = items.iter().filter(|i| i.get("pull_request").is_none()).collect();
        say.tell(serde_json::json!({ "event": "issues", "total": issues.len() }), || {
            format!("moving {} issues ...", issues.len())
        });
        let mut issues_client = server.issues();
        for i in issues {
            let labels = i["labels"]
                .as_array()
                .map(|l| l.iter().map(|x| s(x, "name").replace(',', " ")).collect())
                .unwrap_or_default();
            let created = issues_client
                .create_issue(CreateIssueRequest {
                    milestone: 0,
                    repository: dest(),
                    title: s(i, "title"),
                    body: format!("{}{}", s(i, "body"), attribution(i, "issue")),
                    labels,
                    assignees: vec![],
                })
                .await
                .map_err(|e| anyhow::anyhow!("{}", e.message()))?
                .into_inner();
            let r = IssueRef { repository: dest(), number: created.number };
            if i["comments"].as_u64().unwrap_or(0) > 0 {
                let n = i["number"].as_u64().unwrap_or(0);
                let comments = gh.all(&format!("issues/{n}/comments")).await.unwrap_or_else(|e| {
                    say.note(format!("issue #{n}: its comments couldn't be read ({e:#})"));
                    Vec::new()
                });
                for c in comments {
                    let body = format!("**@{}** wrote on GitHub:\n\n{}", s(&c["user"], "login"), s(&c, "body"));
                    issues_client
                        .add_issue_comment(AddIssueCommentRequest { r#ref: Some(r.clone()), body })
                        .await
                        .map_err(|e| anyhow::anyhow!("{}", e.message()))?;
                }
            }
            if s(i, "state") == "closed" {
                let state =
                    if s(i, "state_reason") == "not_planned" { IssueState::NotPlanned } else { IssueState::Completed };
                issues_client
                    .update_issue(UpdateIssueRequest {
                        r#ref: Some(r),
                        fields: vec!["state".into()],
                        state: state as i32,
                        ..Default::default()
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e.message()))?;
            }
            say.progress(serde_json::json!({ "event": "issue", "number": created.number }));
        }
    }

    done();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_github_urls() {
        for url in [
            "https://github.com/acme/fleet",
            "https://github.com/acme/fleet.git",
            "git@github.com:acme/fleet.git",
            "ssh://git@github.com/acme/fleet",
        ] {
            assert_eq!(github_repo(url), Some(("acme".into(), "fleet".into())), "{url}");
        }
        assert_eq!(github_repo("https://gitlab.com/acme/fleet"), None);
        assert_eq!(github_repo("https://github.com/acme"), None);
        assert_eq!(last_two("https://github.example.com/acme/fleet.git"), Some(("acme".into(), "fleet".into())));
        assert_eq!(last_two("/srv/git/acme/fleet"), Some(("acme".into(), "fleet".into())));
        assert_eq!(last_two(r"C:\git\acme\fleet"), Some(("acme".into(), "fleet".into())));
    }
}
