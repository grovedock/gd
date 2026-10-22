//! Git interop: import a git history into Grovedock and export Grovedock history back to
//! git. Import is deterministic (same git history -> same changeset ids), so re-running
//! it is idempotent and importing new git commits fast-forwards the ref.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use grovedock_core::chunked::{CHUNK_THRESHOLD, chunk_reader, read_content};
use grovedock_core::flags::{FeatureFlags, Flag, require};
use grovedock_core::object::{Changeset, FileChange, GitOrigin, Gitlink, Kind, Mode, encode_object};
use grovedock_core::path::components;
use grovedock_core::{Id, paths_overlap};

use super::commit::upload;
use crate::gitfe::{DataRef, Event, GitCommit, Op, Parser, Writer};
use crate::remote::{Parent, Remote};
use crate::workspace::list_files;

const UPLOAD_FLUSH_BYTES: usize = 32 << 20;
/// Commits sent to the server in one stream (the server takes at most 10,000 per call).
pub(crate) const COMMIT_BATCH: usize = 2_000;

#[derive(Debug, Default)]
pub struct ImportOutcome {
    pub commits: usize,
    pub blobs: usize,
    pub uploaded: usize,
    pub skipped_submodules: usize,
    pub tip: Option<Id>,
    /// Each imported git commit (full sha) and the change it became.
    pub changes: HashMap<String, Id>,
}

/// Parsed stream events with blobs already compressed and hashed (on the parser thread).
enum Item {
    /// `objects`: the blob itself, or its chunks followed by its manifest.
    Blob {
        mark: u64,
        id: Id,
        objects: Vec<(Id, Bytes)>,
    },
    Commit(Box<GitCommit>),
}

/// Objects that store one file's content: a single blob, or (for large files) its
/// chunks followed by the manifest, in upload order.
pub fn encode_blob(data: &[u8]) -> Result<(Id, Vec<(Id, Bytes)>)> {
    if (data.len() as u64) < CHUNK_THRESHOLD {
        let (id, payload) = encode_object(Kind::Blob, data);
        return Ok((id, vec![(id, payload)]));
    }
    let mut objects = Vec::new();
    let (id, manifest) = chunk_reader(data, |cid, payload| {
        objects.push((cid, payload));
        Ok(())
    })?;
    objects.push((id, manifest));
    Ok((id, objects))
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

fn git_mode(mode: u32) -> Result<Option<Mode>> {
    Ok(match mode {
        0o100644 | 0o100664 => Some(Mode::File),
        0o100755 => Some(Mode::Exec),
        0o120000 => Some(Mode::Symlink),
        0o160000 => None, // submodule (gitlink): not imported
        m => bail!("unsupported git mode {m:o}"),
    })
}

pub async fn import(
    remote: &dyn Remote,
    flags: &dyn FeatureFlags,
    git_dir: &Path,
    git_ref: &str,
    target_ref: &str,
    force: bool,
) -> Result<ImportOutcome> {
    require(flags, Flag::GitImport)?;
    let tip_sha = git(git_dir, &["rev-parse", "--verify", &format!("{git_ref}^{{commit}}")])?;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(git_dir)
        .args(["fast-export", "--signed-tags=strip", "--tag-of-filtered-object=drop", "--show-original-ids"])
        .args(identity_args())
        .arg(git_ref)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawning git fast-export")?;
    let stdout = child.stdout.take().expect("piped");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Item>>(256);
    let parser = std::thread::spawn(move || {
        let mut p = Parser::new(BufReader::with_capacity(1 << 20, stdout));
        loop {
            let item = match p.next_event() {
                Ok(None) => return,
                Ok(Some(Event::Blob { mark, data })) => {
                    encode_blob(&data).map(|(id, objects)| Item::Blob { mark, id, objects })
                }
                Ok(Some(Event::Commit(c))) => Ok(Item::Commit(c)),
                Ok(Some(Event::Reset { .. })) => continue,
                Err(e) => Err(e),
            };
            let stop = item.is_err();
            if tx.blocking_send(item).is_err() || stop {
                return;
            }
        }
    });

    let mut out = ImportOutcome::default();
    let mut blob_marks: HashMap<u64, Id> = HashMap::new();
    let mut commit_marks: HashMap<u64, Id> = HashMap::new();
    let mut by_oid: HashMap<String, Id> = HashMap::new();
    let mut pending: Vec<(Id, Bytes)> = Vec::new();
    let mut pending_bytes = 0;
    // Commits wait here and go to the server together (one stream, no round trip each);
    // `queued` maps a mark to its position in the batch, for parents inside it.
    let mut batch = Batch::default();

    while let Some(item) = rx.recv().await {
        match item? {
            Item::Blob { mark, id, objects } => {
                blob_marks.insert(mark, id);
                pending_bytes += objects.iter().map(|(_, p)| p.len()).sum::<usize>();
                pending.extend(objects);
                out.blobs += 1;
                if pending_bytes >= UPLOAD_FLUSH_BYTES {
                    out.uploaded += upload(remote, &std::mem::take(&mut pending)).await?;
                    pending_bytes = 0;
                }
            }
            Item::Commit(c) => {
                let marks = parent_marks(&c);
                // Replacing the whole tree reads the first parent's: it must exist by then.
                let replaces_all = c.ops.iter().any(|op| matches!(op, Op::DeleteAll));
                if replaces_all && marks.first().is_some_and(|m| batch.queued.contains_key(m)) {
                    batch.send(remote, &mut pending, &mut out, &mut commit_marks, &mut by_oid).await?;
                    pending_bytes = 0;
                }
                let parents = marks
                    .iter()
                    .map(|m| match (commit_marks.get(m), batch.queued.get(m)) {
                        (Some(id), _) => Ok(Parent::Id(*id)),
                        (None, Some(i)) => Ok(Parent::Earlier(*i)),
                        _ => Err(anyhow::anyhow!("unknown parent mark :{m}")),
                    })
                    .collect::<Result<Vec<_>>>()?;
                let first = marks.first().and_then(|m| commit_marks.get(m)).copied();
                let cs = changeset_of(remote, &c, &blob_marks, first, &mut out).await?;
                batch.push(parents, cs, c.mark, c.original_oid.clone());
                if batch.items.len() >= COMMIT_BATCH {
                    batch.send(remote, &mut pending, &mut out, &mut commit_marks, &mut by_oid).await?;
                    pending_bytes = 0;
                }
            }
        }
    }
    batch.send(remote, &mut pending, &mut out, &mut commit_marks, &mut by_oid).await?;
    parser.join().map_err(|_| anyhow::anyhow!("fast-export parser panicked"))?;
    let status = child.wait()?;
    if !status.success() {
        bail!("git fast-export failed: {status}");
    }
    let tip = *by_oid.get(&tip_sha).context("tip commit missing from fast-export stream")?;
    out.tip = Some(tip);
    out.changes = by_oid;
    move_ref(remote, target_ref, tip, force).await?;
    Ok(out)
}

async fn move_ref(remote: &dyn Remote, name: &str, tip: Id, force: bool) -> Result<()> {
    let current = remote.get_ref(name).await?;
    match current {
        Some(c) if c == tip => return Ok(()),
        Some(c) if !force && remote.merge_base(&c.to_hex(), &tip.to_hex()).await? != Some(c) => {
            bail!("ref `{name}` has diverged from the imported history; pass --force to overwrite");
        }
        _ => {}
    }
    if !remote.compare_and_swap(name, current, tip).await? {
        bail!("ref `{name}` moved concurrently; re-run the import");
    }
    Ok(())
}

/// Turn one fast-export commit into a server-side changeset. `blobs` and `commits` map
/// fast-export marks to Grovedock ids. Shared by `import-git` and the git remote helper.
pub async fn import_commit(
    remote: &dyn Remote,
    c: &GitCommit,
    blobs: &HashMap<u64, Id>,
    commits: &HashMap<u64, Id>,
    out: &mut ImportOutcome,
) -> Result<Id> {
    let parents = parent_marks(c)
        .iter()
        .map(|m| commits.get(m).copied().with_context(|| format!("unknown parent mark :{m}")))
        .collect::<Result<Vec<_>>>()?;
    let mut cs = changeset_of(remote, c, blobs, parents.first().copied(), out).await?;
    cs.parents = parents.iter().map(|p| p.0.to_vec()).collect();
    Ok(remote.create_changeset(cs).await?.id)
}

/// The changeset for one fast-export commit, without its parents (the caller sets those).
/// `first_parent` is read only when the commit replaces the whole tree.
pub(crate) async fn changeset_of(
    remote: &dyn Remote,
    c: &GitCommit,
    blobs: &HashMap<u64, Id>,
    first_parent: Option<Id>,
    out: &mut ImportOutcome,
) -> Result<Changeset> {
    let mut files: BTreeMap<String, Option<(Mode, Id)>> = BTreeMap::new();
    let mut gitlinks = Vec::new();
    let mut delete_all = false;
    for op in &c.ops {
        match op {
            Op::Modify { mode, data, path } => {
                let Some(mode) = git_mode(*mode)? else {
                    // A submodule: gd keeps none, but the git view writes it back (same trees).
                    out.skipped_submodules += 1;
                    if let DataRef::Sha(sha) = data {
                        gitlinks.push(Gitlink { path: path.clone(), commit: sha.clone() });
                    }
                    continue;
                };
                let DataRef::Mark(m) = data else { bail!("{path}: blob by sha is unsupported") };
                // `.git` / `.gd` paths (any case or Windows alias) are refused by name, not skipped.
                components(path).map_err(|e| anyhow::anyhow!("can't import git path `{path}`: {e}"))?;
                files.insert(
                    path.clone(),
                    Some((mode, *blobs.get(m).with_context(|| format!("unknown blob mark :{m}"))?)),
                );
            }
            Op::Delete(p) => {
                files.insert(p.clone(), None);
            }
            Op::DeleteAll => {
                files.clear();
                delete_all = true;
            }
        }
    }
    if delete_all && let Some(p) = first_parent {
        let root = remote.changeset(&p.to_hex()).await?.root_tree;
        for (path, _) in list_files(remote.objects().as_ref(), root, &[]).await? {
            files.entry(path).or_insert(None);
        }
    }
    // git may delete `a` and add `a/b` in one commit; the upsert alone already replaces.
    let upserts: Vec<String> = files.iter().filter(|(_, v)| v.is_some()).map(|(k, _)| k.clone()).collect();
    files.retain(|p, v| v.is_some() || !upserts.iter().any(|u| paths_overlap(u, p)));

    Ok(Changeset {
        parents: vec![],
        changes: files
            .into_iter()
            .map(|(p, v)| match v {
                Some((m, id)) => FileChange::upsert(p, m, id),
                None => FileChange::delete(p),
            })
            .collect(),
        author: c.author.clone(),
        message: c.message.clone(),
        timestamp_ms: c.timestamp_s * 1000,
        git: git_origin(c, gitlinks),
    })
}

/// A commit's parent marks, first parent first. git accepts a parent listed twice (the Linux
/// kernel has four such merges); the repeat adds no ancestry, so it's kept once.
pub(crate) fn parent_marks(c: &GitCommit) -> Vec<u64> {
    let mut marks: Vec<u64> = c.from.iter().copied().collect();
    for m in &c.merges {
        if !marks.contains(m) {
            marks.push(*m);
        }
    }
    marks
}

/// Commits on their way to the server: sent together, after the objects they need.
#[derive(Default)]
struct Batch {
    items: Vec<(Vec<Parent>, Changeset)>,
    meta: Vec<(Option<u64>, Option<String>)>,
    queued: HashMap<u64, usize>,
}

impl Batch {
    fn push(&mut self, parents: Vec<Parent>, cs: Changeset, mark: Option<u64>, oid: Option<String>) {
        if let Some(m) = mark {
            self.queued.insert(m, self.items.len());
        }
        self.items.push((parents, cs));
        self.meta.push((mark, oid));
    }

    async fn send(
        &mut self,
        remote: &dyn Remote,
        pending: &mut Vec<(Id, Bytes)>,
        out: &mut ImportOutcome,
        commit_marks: &mut HashMap<u64, Id>,
        by_oid: &mut HashMap<String, Id>,
    ) -> Result<()> {
        out.uploaded += upload(remote, &std::mem::take(pending)).await?;
        if self.items.is_empty() {
            return Ok(());
        }
        let ids = remote.create_changesets(std::mem::take(&mut self.items)).await?;
        for (id, (mark, oid)) in ids.into_iter().zip(self.meta.drain(..)) {
            if let Some(m) = mark {
                commit_marks.insert(m, id);
            }
            if let Some(oid) = oid {
                by_oid.insert(oid, id);
            }
            out.commits += 1;
        }
        self.queued.clear();
        eprintln!("  imported {} commits", out.commits);
        Ok(())
    }
}

/// What git needs to make `c` again, when it came from git (it has an original id). A deleted
/// submodule needs nothing here: the changeset's own delete writes it back.
fn git_origin(c: &GitCommit, gitlinks: Vec<Gitlink>) -> Option<GitOrigin> {
    let commit = c.original_oid.clone()?;
    let utf8 = c.encoding.is_empty() && std::str::from_utf8(&c.message_raw).is_ok();
    Some(GitOrigin {
        author: c.author_raw.clone(),
        committer: c.committer_raw.clone(),
        encoding: c.encoding.clone(),
        message: if utf8 { Vec::new() } else { c.message_raw.clone() },
        signature_algorithm: c.signature.as_ref().map(|s| s.0.clone()).unwrap_or_default(),
        signature: c.signature.as_ref().map(|s| s.1.clone()).unwrap_or_default(),
        gitlinks,
        commit,
    })
}

/// fast-export options that keep what git hashes: the message's own encoding, and commit
/// signatures (git 2.48+; older git strips them, so those commits get new ids).
pub fn identity_args() -> Vec<&'static str> {
    static SIGNED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let signed = *SIGNED.get_or_init(|| {
        std::process::Command::new("git").args(["fast-export", "-h"]).output().is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout).contains("signed-commits")
                || String::from_utf8_lossy(&o.stderr).contains("signed-commits")
        })
    });
    let mut args = vec!["--reencode=no"];
    if signed {
        args.push("--signed-commits=verbatim");
    }
    args
}

/// The changesets reachable from `tip` that `known` doesn't cover, merges included, parents
/// before children: what a git view of the history still has to write.
pub async fn unwritten(
    remote: &dyn Remote,
    tip: Id,
    known: &dyn Fn(&Id) -> bool,
) -> Result<Vec<crate::remote::RemoteChangeset>> {
    let mut found: HashMap<Id, crate::remote::RemoteChangeset> = HashMap::new();
    let mut todo = vec![tip];
    while let Some(start) = todo.pop() {
        // Down the first-parent line a page at a time; other parents are walked later.
        let mut next = Some(start);
        'line: while let Some(r) = next.take() {
            if known(&r) || found.contains_key(&r) {
                break;
            }
            for rc in remote.log(&r.to_hex(), "", 1000).await? {
                if known(&rc.id) || found.contains_key(&rc.id) {
                    break 'line;
                }
                todo.extend(rc.changeset.parent_ids()?.into_iter().skip(1));
                next = rc.changeset.first_parent()?;
                found.insert(rc.id, rc);
            }
        }
    }
    // Parents first: a depth-first walk, each changeset after all its parents.
    let mut order = Vec::with_capacity(found.len());
    let mut done = std::collections::HashSet::new();
    let mut ids: Vec<Id> = found.keys().copied().collect();
    ids.sort();
    for root in ids {
        let mut stack = vec![(root, false)];
        while let Some((id, expanded)) = stack.pop() {
            if done.contains(&id) || !found.contains_key(&id) {
                continue;
            }
            if expanded {
                done.insert(id);
                order.push(id);
                continue;
            }
            stack.push((id, true));
            for p in found[&id].changeset.parent_ids()?.into_iter().rev() {
                if !done.contains(&p) {
                    stack.push((p, false));
                }
            }
        }
    }
    Ok(order.into_iter().map(|id| found.remove(&id).expect("found")).collect())
}

/// Export the history of `rev` into git repository `git_dir` (created if missing) as branch
/// `git_ref`, merges included; changes that came from git become their original commits.
/// Returns the number of commits written.
pub async fn export(
    remote: &dyn Remote,
    flags: &dyn FeatureFlags,
    rev: &str,
    git_dir: &Path,
    git_ref: &str,
) -> Result<usize> {
    require(flags, Flag::GitExport)?;
    init_git(git_dir)?;
    let tip = remote.changeset(rev).await?.id;
    let nodes = unwritten(remote, tip, &|_| false).await?;
    let written = write_commits(remote, &nodes, &HashMap::new(), git_dir, git_ref).await?;
    report_identity(&nodes, &written);
    write_notes(remote, &nodes, &written, git_dir).await?;
    Ok(nodes.len())
}

/// Says (on stderr) how many changes from git didn't come out as their original commits:
/// what git hashes but gd couldn't keep (an old git that strips signatures, for one).
fn report_identity(nodes: &[crate::remote::RemoteChangeset], written: &HashMap<Id, String>) -> usize {
    let differ = nodes
        .iter()
        .filter(|rc| {
            rc.changeset.git.as_ref().is_some_and(|g| !g.commit.is_empty() && written.get(&rc.id) != Some(&g.commit))
        })
        .count();
    if differ > 0 {
        eprintln!(
            "gd: {differ} commit(s) from git came out with new ids (their git identity couldn't be reproduced exactly)"
        );
    }
    differ
}

fn init_git(git_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(git_dir)?;
    if !git_dir.join(".git").exists() && !git_dir.join("HEAD").exists() {
        git(git_dir, &["init", "-q"])?;
    }
    Ok(())
}

/// Writes `nodes` (parents first) as commits, `git_ref` ending at the last; parents outside
/// `nodes` are git commits already there (`known`). Returns the git commit each became. The
/// same changesets always make the same commits; those that came from git make their original.
async fn write_commits(
    remote: &dyn Remote,
    nodes: &[crate::remote::RemoteChangeset],
    known: &HashMap<Id, String>,
    git_dir: &Path,
    git_ref: &str,
) -> Result<HashMap<Id, String>> {
    if nodes.is_empty() {
        return Ok(HashMap::new());
    }
    let marks_file = git_dir.join("gd-export.marks");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(git_dir)
        // On case-insensitive filesystems git defaults to core.ignorecase=true, which makes
        // fast-import fold case-only renames onto the old name. History must be exact.
        .args(["-c", "core.ignorecase=false", "fast-import", "--quiet", "--force"])
        .arg(format!("--export-marks={}", marks_file.display()))
        .stdin(Stdio::piped())
        .spawn()
        .context("spawning git fast-import")?;
    let mut w = Writer { w: BufWriter::with_capacity(1 << 20, child.stdin.take().expect("piped")) };
    let objects = remote.objects();
    let mut blob_marks: HashMap<Id, u64> = HashMap::new();
    let mut commit_marks: HashMap<Id, u64> = HashMap::new();
    let mut next_mark = 1u64;
    let refname = format!("refs/heads/{git_ref}");
    let parent_ref = |p: &Id, marks: &HashMap<Id, u64>| -> Result<String> {
        match (marks.get(p), known.get(p)) {
            (Some(m), _) => Ok(format!(":{m}")),
            (None, Some(sha)) => Ok(sha.clone()),
            _ => bail!("parent {p} is neither written nor known to git"),
        }
    };
    for rc in nodes {
        let cs = &rc.changeset;
        let new_blobs: Vec<Id> = cs
            .changes
            .iter()
            .filter_map(|c| c.blob_id().ok().flatten())
            .filter(|id| !blob_marks.contains_key(id))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for chunk in new_blobs.chunks(512) {
            for (id, payload) in chunk.iter().zip(objects.get_many(chunk).await?) {
                let payload = payload.with_context(|| format!("blob {id} missing"))?;
                let (_, content) = read_content(objects.as_ref(), id, &payload).await?;
                w.blob(next_mark, &content)?;
                blob_marks.insert(*id, next_mark);
                next_mark += 1;
            }
        }
        let ops: Vec<(String, Option<(u32, u64)>)> = cs
            .changes
            .iter()
            .map(|c| {
                let op = match c.blob_id()? {
                    None => None,
                    Some(id) => {
                        let mode = match Mode::parse(c.mode)? {
                            Mode::Exec => 0o100755,
                            Mode::Symlink => 0o120000,
                            _ => 0o100644,
                        };
                        Some((mode, blob_marks[&id]))
                    }
                };
                Ok((c.path.clone(), op))
            })
            .collect::<Result<_>>()?;
        let parents = cs.parent_ids()?;
        let from = parents.first().map(|p| parent_ref(p, &commit_marks)).transpose()?;
        let merges: Vec<String> =
            parents.iter().skip(1).map(|p| parent_ref(p, &commit_marks)).collect::<Result<_>>()?;
        let mark = next_mark;
        next_mark += 1;
        let header = crate::gitfe::Header {
            author: &cs.author,
            timestamp_s: cs.timestamp_ms.div_euclid(1000),
            message: &cs.message,
            from,
            merges: &merges,
            git: cs.git.as_ref(),
        };
        w.commit(&refname, mark, &header, &ops)?;
        commit_marks.insert(rc.id, mark);
    }
    // The branch ends at the last changeset written (a merge's other parents came before it).
    let last = commit_marks[&nodes.last().expect("nodes").id];
    w.reset(&refname, &format!(":{last}"))?;
    w.w.flush()?;
    drop(w);
    let status = child.wait()?;
    if !status.success() {
        bail!("git fast-import failed: {status}");
    }
    let shas: HashMap<u64, String> = std::fs::read_to_string(&marks_file)?
        .lines()
        .filter_map(|l| {
            let (m, sha) = l.split_once(' ')?;
            Some((m.strip_prefix(':')?.parse().ok()?, sha.to_owned()))
        })
        .collect();
    let _ = std::fs::remove_file(&marks_file);
    Ok(commit_marks.into_iter().filter_map(|(id, m)| Some((id, shas.get(&m)?.clone()))).collect())
}

/// Where a mirror has got to: the last changeset exported and the git commit it became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorState {
    pub changeset: Id,
    pub commit: String,
}

const MIRROR_STATE: &str = "gd-mirror";

pub fn mirror_state(git_dir: &Path) -> Result<Option<MirrorState>> {
    let path = git_dir.join(MIRROR_STATE);
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let (cs, commit) = text.trim().split_once(' ').with_context(|| format!("{} is damaged", path.display()))?;
    Ok(Some(MirrorState { changeset: cs.parse::<Id>()?, commit: commit.to_owned() }))
}

/// Every changeset a mirror has written, and the git commit it became (`gd-mirror.map`).
const MIRROR_MAP: &str = "gd-mirror.map";

fn mirror_map(git_dir: &Path) -> Result<HashMap<Id, String>> {
    let mut map = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(git_dir.join(MIRROR_MAP)) {
        for l in text.lines() {
            if let Some((cs, sha)) = l.split_once(' ') {
                map.insert(cs.parse::<Id>()?, sha.to_owned());
            }
        }
    } else if let Some(s) = mirror_state(git_dir)? {
        // A mirror from before the map: what it recorded is where it continues from.
        map.insert(s.changeset, s.commit);
    }
    Ok(map)
}

/// Brings `git_ref` in `git_dir` up to date with `rev`, appending only changes not yet
/// exported, merges included. Returns how many commits were added (0 when already current).
pub async fn mirror_once(
    remote: &dyn Remote,
    flags: &dyn FeatureFlags,
    rev: &str,
    git_dir: &Path,
    git_ref: &str,
) -> Result<usize> {
    require(flags, Flag::GitExport)?;
    init_git(git_dir)?;
    let Some(tip) = remote.get_ref(rev).await? else { return Ok(0) };
    let state = mirror_state(git_dir)?;
    if state.as_ref().is_some_and(|s| s.changeset == tip) {
        return Ok(0);
    }
    let mut map = mirror_map(git_dir)?;
    if let Some(s) = &state
        && remote.merge_base(&s.changeset.to_hex(), &tip.to_hex()).await? != Some(s.changeset)
    {
        bail!("{rev} no longer contains the last mirrored change; refusing to rewrite the mirror");
    }
    let nodes = unwritten(remote, tip, &|id| map.contains_key(id)).await?;
    let written = write_commits(remote, &nodes, &map, git_dir, git_ref).await?;
    report_identity(&nodes, &written);
    let mut parents = map.clone();
    parents.extend(written.iter().map(|(k, v)| (*k, v.clone())));
    write_notes(remote, &nodes, &parents, git_dir).await?;
    map.extend(written);
    let commit = git(git_dir, &["rev-parse", &format!("refs/heads/{git_ref}")])?;
    // Written last: if anything above fails, the next run redoes the same work.
    let tmp = git_dir.join(format!("{MIRROR_MAP}.tmp"));
    std::fs::write(&tmp, map.iter().map(|(id, sha)| format!("{} {sha}\n", id.to_hex())).collect::<String>())?;
    std::fs::rename(tmp, git_dir.join(MIRROR_MAP))?;
    let tmp = git_dir.join(format!("{MIRROR_STATE}.tmp"));
    std::fs::write(&tmp, format!("{} {commit}\n", tip.to_hex()))?;
    std::fs::rename(tmp, git_dir.join(MIRROR_STATE))?;
    Ok(nodes.len())
}

/// Who wrote what, for git (`refs/notes/ai`, git-ai's authorship notes): a change that came
/// from git gets its own note back as it was; a change an agent made with gd gets a note saying
/// which lines it added, as which agent and model. Changes by people get none.
async fn write_notes(
    remote: &dyn Remote,
    nodes: &[crate::remote::RemoteChangeset],
    commits: &HashMap<Id, String>,
    git_dir: &Path,
) -> Result<()> {
    use grovedock_core::gitai::{self, Author, Note};
    for n in nodes {
        let Some(sha) = commits.get(&n.id) else { continue };
        let text = if n.changeset.git.is_some() {
            match remote.git_ai_note(n.id).await? {
                Some(t) => t,
                None => continue,
            }
        } else {
            let Some(p) = n.provenance.as_ref().filter(|p| p.agent) else { continue };
            let parent = n.changeset.first_parent()?;
            let conversation = if p.session.is_empty() { n.id.to_hex() } else { p.session.clone() };
            let (key, _) = gitai::session_key(&p.by, &conversation, &n.id.to_hex());
            let mut files = Vec::new();
            for c in n.changeset.changes.iter().filter(|c| !c.is_delete()).take(500) {
                let new = match remote.read_file(&n.id.to_hex(), &c.path).await {
                    Ok((_, b)) => String::from_utf8_lossy(&b).into_owned(),
                    Err(_) => continue,
                };
                let old = match parent {
                    Some(par) => remote
                        .read_file(&par.to_hex(), &c.path)
                        .await
                        .map(|(_, b)| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default(),
                    None => String::new(),
                };
                let added: Vec<u32> = similar::TextDiff::from_lines(&old, &new)
                    .iter_all_changes()
                    .filter(|ch| ch.tag() == similar::ChangeTag::Insert)
                    .filter_map(|ch| ch.new_index().map(|i| i as u32 + 1))
                    .collect();
                if !added.is_empty() {
                    files.push((c.path.clone(), vec![(key.clone(), gitai::ranges(&added))]));
                }
            }
            if files.is_empty() {
                continue;
            }
            let note = Note {
                files,
                authors: [(
                    key,
                    Author::Agent {
                        tool: p.by.clone(),
                        model: p.model.clone(),
                        session: conversation,
                        human: String::new(),
                    },
                )]
                .into_iter()
                .collect(),
            };
            let base = parent.and_then(|par| commits.get(&par).cloned()).unwrap_or_default();
            gitai::write(&note, &base)
        };
        let file = git_dir.join("gd-note.tmp");
        std::fs::write(&file, text)?;
        let r =
            git(git_dir, &["notes", "--ref", gitai::NOTES_REF, "add", "-f", "-F", &file.display().to_string(), sha]);
        let _ = std::fs::remove_file(&file);
        r?;
    }
    Ok(())
}

/// A URL with any password or token replaced, for messages and logs.
pub fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let host_end = rest.find('/').unwrap_or(rest.len());
    match rest[..host_end].rfind('@') {
        Some(at) => format!("{scheme}://***@{}", &rest[at + 1..]),
        None => url.to_owned(),
    }
}

/// Where a mirror keeps its local copy when no directory is given: a bare repository in the
/// user's cache, one per destination.
pub fn default_mirror_dir(to: &str) -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let name: String =
        redact(to).chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    base.join("gd").join("mirror").join(name)
}

/// Brings the mirror's local copy up to date with `rev` and pushes `git_ref` to `to`.
/// The push never forces: if the destination has commits the mirror didn't make, it fails
/// and says so. Pushing every time also retries a push that failed last time.
pub async fn mirror(
    remote: &dyn Remote,
    flags: &dyn FeatureFlags,
    rev: &str,
    dir: &Path,
    git_ref: &str,
    to: &str,
) -> Result<usize> {
    std::fs::create_dir_all(dir)?;
    if !dir.join("HEAD").exists() {
        git(dir, &["init", "-q", "--bare"])?;
    }
    let n = mirror_once(remote, flags, rev, dir, git_ref).await?;
    if git(dir, &["rev-parse", "-q", "--verify", &format!("refs/heads/{git_ref}")]).is_err() {
        return Ok(0); // nothing has landed yet
    }
    let spec = format!("refs/heads/{git_ref}:refs/heads/{git_ref}");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["push", "--quiet", "--porcelain", to, &spec])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("running git push")?;
    if out.status.success() && git(dir, &["rev-parse", "-q", "--verify", grovedock_core::gitai::NOTES_REF]).is_ok() {
        let notes = format!("{0}:{0}", grovedock_core::gitai::NOTES_REF);
        let pushed = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["push", "--quiet", to, &notes])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output();
        if !pushed.is_ok_and(|o| o.status.success()) {
            eprintln!(
                "note: who-wrote-what notes ({}) weren't pushed to {}; the branch was",
                grovedock_core::gitai::NOTES_REF,
                redact(to)
            );
        }
    }
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).replace(to, &redact(to));
        let why = String::from_utf8_lossy(&out.stdout).replace(to, &redact(to)) + &why;
        if why.contains("[rejected]") || why.contains("non-fast-forward") || why.contains("fetch first") {
            bail!(
                "{} has commits on {git_ref} that did not come from this mirror; the mirror never \
                 overwrites them. Mirror into an empty repository or branch, or one this mirror made.",
                redact(to)
            );
        }
        bail!("pushing to {} failed: {}", redact(to), why.trim());
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redact_hides_credentials_only() {
        assert_eq!(
            redact("https://x-access-token:ghp_secret@github.com/acme/fleet.git"),
            "https://***@github.com/acme/fleet.git"
        );
        assert_eq!(redact("https://github.com/acme/fleet.git"), "https://github.com/acme/fleet.git");
        assert_eq!(redact("git@github.com:acme/fleet.git"), "git@github.com:acme/fleet.git");
        assert_eq!(redact("https://github.com/a/b@c"), "https://github.com/a/b@c");
    }
}
