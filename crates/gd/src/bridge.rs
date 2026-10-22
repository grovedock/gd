//! Two-way git bridge: the `git-remote-gd` remote helper.
//!
//! With the helper on `PATH`, plain git works against a Grovedock server:
//!
//! ```text
//! git clone gd::http://server:7420 repo
//! git push / git pull / git fetch
//! ```
//!
//! Fetch streams only changesets git does not have yet (git fast-import, inline file
//! contents). Push parses git's fast-export stream into server-side changesets and lands
//! them: if the branch moved, the server rebases instead of rejecting the push
//! (`GD_PUSH=strict` restores git's reject-if-behind behaviour).
//!
//! State lives in `$GIT_DIR/gd/<remote>/`: `git.marks` (owned by git's fast-import /
//! fast-export) and `gd.marks` (mark -> Grovedock id for every commit and blob).

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use grovedock_core::Id;
use grovedock_core::chunked::read_content;
use grovedock_core::flags::{FeatureFlags, Flag, require};
use grovedock_core::object::{FileChange, Mode};

use crate::features::commit::upload;
use crate::features::git::{ImportOutcome, encode_blob, import_commit};
use crate::gitfe::{Event, InlineOp, Parser, Writer};
use crate::remote::Remote;
use crate::workspace::list_files;

/// Where tags live among a repository's refs: `tags/v1.2.0`.
pub const TAG_PREFIX: &str = "tags/";

/// What a fast-export stream brought: each ref's tip, and every commit in stream order with
/// the ref it was written for.
#[derive(Default)]
struct Pushed {
    tips: BTreeMap<String, Id>,
    order: Vec<(String, Id)>,
}

impl Pushed {
    /// The new commits written for `refname`, oldest first.
    fn chain(&self, refname: &str) -> Vec<Id> {
        self.order.iter().filter(|(r, _)| r == refname).map(|(_, id)| *id).collect()
    }

    /// The same, with commits written under `from` (a commit id, as fast-export names a
    /// commit given by id) taken as written for `to`.
    fn renamed(&self, from: &str, to: &str) -> Pushed {
        let order = self.order.iter().map(|(r, id)| (if r == from { to.to_owned() } else { r.clone() }, *id)).collect();
        Pushed { tips: BTreeMap::new(), order }
    }
}

/// mark -> ('c' changeset | 'b' blob, id), persisted next to git's own marks file.
#[derive(Default)]
struct Marks {
    entries: BTreeMap<u64, (char, Id)>,
}

fn git_marks(path: &Path) -> Result<BTreeMap<u64, String>> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    Ok(text
        .lines()
        .filter_map(|l| {
            let (m, sha) = l.split_once(' ')?;
            Some((m.strip_prefix(':')?.parse().ok()?, sha.to_owned()))
        })
        .collect())
}

impl Marks {
    /// Load our marks, dropping entries git does not know (e.g. a fast-import that failed
    /// after we recorded its marks), so we never emit `from` a mark git cannot resolve.
    fn load(path: &Path, git: &BTreeMap<u64, String>) -> Result<Marks> {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut entries = BTreeMap::new();
        for line in text.lines() {
            let mut it = line.split(' ');
            let (Some(m), Some(k), Some(id)) = (it.next(), it.next(), it.next()) else { continue };
            let Some(mark) = m.strip_prefix(':').and_then(|m| m.parse::<u64>().ok()) else { continue };
            let kind = k.chars().next().unwrap_or('?');
            if git.contains_key(&mark) || kind == 'b' {
                entries.insert(mark, (kind, id.parse()?));
            }
        }
        Ok(Marks { entries })
    }

    fn save(&self, path: &Path) -> Result<()> {
        let mut out = String::new();
        for (m, (k, id)) in &self.entries {
            out.push_str(&format!(":{m} {k} {id}\n"));
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    fn by_kind(&self, kind: char) -> HashMap<u64, Id> {
        self.entries.iter().filter(|(_, (k, _))| *k == kind).map(|(m, (_, id))| (*m, *id)).collect()
    }

    fn commit_mark(&self, id: &Id) -> Option<u64> {
        self.entries.iter().find(|(_, (k, i))| *k == 'c' && i == id).map(|(m, _)| *m)
    }
}

pub struct Bridge<'a> {
    remote: &'a dyn Remote,
    flags: &'a dyn FeatureFlags,
    alias: String,
    git_marks: PathBuf,
    our_marks: PathBuf,
    strict_push: bool,
}

fn git_mode(m: Mode) -> u32 {
    match m {
        Mode::Exec => 0o100755,
        Mode::Symlink => 0o120000,
        _ => 0o100644,
    }
}

impl<'a> Bridge<'a> {
    /// `alias` is the git remote name (or `_` for a bare URL); `git_dir` is `$GIT_DIR`.
    pub fn new(remote: &'a dyn Remote, flags: &'a dyn FeatureFlags, alias: &str, git_dir: &Path) -> Result<Self> {
        let safe: String = alias.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let dir = git_dir.join("gd").join(&safe);
        std::fs::create_dir_all(&dir)?;
        let git_marks = dir.join("git.marks");
        if !git_marks.exists() {
            std::fs::write(&git_marks, "")?;
        }
        Ok(Self {
            remote,
            flags,
            alias: safe,
            git_marks,
            our_marks: dir.join("gd.marks"),
            strict_push: std::env::var("GD_PUSH").is_ok_and(|v| v == "strict"),
        })
    }

    fn private_ref(&self, branch: &str) -> String {
        format!("refs/gd/{}/heads/{branch}", self.alias)
    }

    /// Serve the remote-helper protocol until git closes the conversation.
    pub async fn serve(&self, input: &mut impl BufRead, out: &mut impl Write) -> Result<()> {
        require(self.flags, Flag::GitBridge)?;
        let mut line = String::new();
        loop {
            line.clear();
            if input.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let cmd = line.trim_end();
            match cmd {
                "" => return Ok(()),
                "capabilities" => {
                    writeln!(out, "import\nexport")?;
                    writeln!(out, "refspec refs/heads/*:refs/gd/{}/heads/*", self.alias)?;
                    // Tags go through private refs too; git copies them to refs/tags as asked.
                    writeln!(out, "refspec refs/tags/*:refs/gd/{}/tags/*", self.alias)?;
                    writeln!(out, "*import-marks {}", self.git_marks.display())?;
                    writeln!(out, "*export-marks {}", self.git_marks.display())?;
                    writeln!(out, "option\n")?;
                }
                c if c == "list" || c.starts_with("list ") => {
                    let refs = self.remote.list_refs().await?;
                    for (name, _) in &refs {
                        match name.strip_prefix(TAG_PREFIX) {
                            Some(tag) => writeln!(out, "? refs/tags/{tag}")?,
                            None => writeln!(out, "? refs/heads/{name}")?,
                        }
                    }
                    if refs.iter().any(|(n, _)| n == "main") {
                        writeln!(out, "@refs/heads/main HEAD")?;
                    }
                    writeln!(out)?;
                }
                c if c.starts_with("option ") => writeln!(out, "unsupported")?,
                c if c.starts_with("import ") => {
                    let mut refs = vec![c["import ".len()..].to_owned()];
                    loop {
                        line.clear();
                        input.read_line(&mut line)?;
                        match line.trim_end().strip_prefix("import ") {
                            Some(r) => refs.push(r.to_owned()),
                            None => break, // the blank line ending the batch
                        }
                    }
                    self.import(&refs, out).await?;
                }
                "export" => self.export(input, out).await?,
                other => bail!("unsupported remote-helper command `{other}`"),
            }
            out.flush()?;
        }
    }

    /// Write a fast-import stream bringing each requested branch up to date.
    async fn import(&self, refs: &[String], out: &mut impl Write) -> Result<()> {
        let git = git_marks(&self.git_marks)?;
        let mut marks = Marks::load(&self.our_marks, &git)?;
        let mut next = git.keys().chain(marks.entries.keys()).max().copied().unwrap_or(0) + 1;
        let mut w = Writer { w: &mut *out };
        writeln!(w.w, "feature import-marks-if-exists={}", self.git_marks.display())?;
        writeln!(w.w, "feature export-marks={}", self.git_marks.display())?;
        writeln!(w.w, "feature done")?;
        // Private refs mirror the server. After a push the server rebased, the server's tip
        // is a sibling of the commit git recorded, so the mirror must move like a forced
        // fetch; the user's branches are never touched by this.
        writeln!(w.w, "feature force")?;
        let objects = self.remote.objects();
        for r in refs {
            // Branches and tags land in this remote's private refs, as git's refspecs say.
            let (branch, private) = match (r.strip_prefix("refs/heads/"), r.strip_prefix("refs/tags/")) {
                (Some(b), _) => (b.to_owned(), self.private_ref(b)),
                (_, Some(t)) => (format!("{TAG_PREFIX}{t}"), format!("refs/gd/{}/tags/{t}", self.alias)),
                _ => continue,
            };
            let branch = branch.as_str();
            let Some(head) = self.remote.get_ref(branch).await? else { continue };
            // What git doesn't have yet, merges included, parents first.
            let chain = {
                let known = |id: &Id| marks.commit_mark(id).is_some();
                crate::features::git::unwritten(self.remote, head, &known).await?
            };
            for rc in &chain {
                let cs = &rc.changeset;
                // On case-insensitive filesystems git's fast-import folds a case-only rename
                // (delete `A.txt`, write `a.txt`) onto the old name. For such commits emit
                // the complete tree after `deleteall`, which leaves nothing to fold.
                let case_rename = cs.changes.iter().any(|d| {
                    d.is_delete()
                        && cs
                            .changes
                            .iter()
                            .any(|u| !u.is_delete() && u.path != d.path && u.path.eq_ignore_ascii_case(&d.path))
                });
                let changes: Vec<FileChange> = if case_rename {
                    list_files(objects.as_ref(), rc.root_tree, &[])
                        .await?
                        .into_iter()
                        .map(|(p, (m, id))| FileChange::upsert(p, m, id))
                        .collect()
                } else {
                    cs.changes.clone()
                };
                let blob_ids: Vec<Id> = changes.iter().filter_map(|c| c.blob_id().ok().flatten()).collect();
                let payloads = objects.get_many(&blob_ids).await?;
                let mut contents: HashMap<Id, Vec<u8>> = HashMap::new();
                for (id, p) in blob_ids.iter().zip(payloads) {
                    let p = p.with_context(|| format!("blob {id} missing on server"))?;
                    contents.insert(*id, read_content(objects.as_ref(), id, &p).await?.1);
                }
                let ops: Vec<InlineOp> = changes
                    .iter()
                    .map(|c| {
                        Ok((
                            c.path.clone(),
                            match c.blob_id()? {
                                None => None,
                                Some(id) => Some((git_mode(Mode::parse(c.mode)?), contents[&id].clone())),
                            },
                        ))
                    })
                    .collect::<Result<_>>()?;
                let parent = |p: &Id| -> Result<String> {
                    Ok(format!(":{}", marks.commit_mark(p).with_context(|| format!("parent {p} unknown to git"))?))
                };
                let parents = cs.parent_ids()?;
                let from = parents.first().map(parent).transpose()?;
                let merges: Vec<String> = parents.iter().skip(1).map(&parent).collect::<Result<_>>()?;
                let mark = next;
                next += 1;
                let header = crate::gitfe::Header {
                    author: &cs.author,
                    timestamp_s: cs.timestamp_ms.div_euclid(1000),
                    message: &cs.message,
                    from,
                    merges: &merges,
                    git: cs.git.as_ref(),
                };
                w.commit_inline(&private, mark, &header, case_rename, &ops)?;
                marks.entries.insert(mark, ('c', rc.id));
            }
            let head_mark = marks.commit_mark(&head).context("branch head has no mark")?;
            writeln!(w.w, "reset {private}\nfrom :{head_mark}\n")?;
        }
        writeln!(w.w, "done")?;
        marks.save(&self.our_marks)?;
        Ok(())
    }

    /// Read a fast-export stream into changesets: each ref named in it with its tip, and every
    /// commit in stream order with the ref it was written for. Marks are updated in `marks`.
    async fn ingest(&self, input: &mut impl BufRead, marks: &mut Marks) -> Result<Pushed> {
        let mut blobs = marks.by_kind('b');
        let mut commits = marks.by_kind('c');
        let mut pending = Vec::new();
        let mut pushed = Pushed::default();
        let mut stats = ImportOutcome::default();
        let mut parser = Parser::new(&mut *input);
        while let Some(event) = parser.next_event()? {
            match event {
                Event::Blob { mark, data } => {
                    let (id, objects) = encode_blob(&data)?;
                    blobs.insert(mark, id);
                    marks.entries.insert(mark, ('b', id));
                    pending.extend(objects);
                }
                Event::Commit(c) => {
                    upload(self.remote, &std::mem::take(&mut pending)).await?;
                    let id = import_commit(self.remote, &c, &blobs, &commits, &mut stats).await?;
                    if let Some(m) = c.mark {
                        commits.insert(m, id);
                        marks.entries.insert(m, ('c', id));
                    }
                    pushed.order.push((c.refname.clone(), id));
                    pushed.tips.insert(c.refname.clone(), id);
                }
                Event::Reset { refname, from: Some(m) } => {
                    let id = *commits.get(&m).with_context(|| format!("unknown mark :{m} for {refname}"))?;
                    pushed.tips.insert(refname, id);
                }
                Event::Reset { .. } => {}
            }
        }
        drop(parser);
        upload(self.remote, &pending).await?;
        Ok(pushed)
    }

    /// Read git's fast-export stream, create changesets, and land each pushed branch.
    async fn export(&self, input: &mut impl BufRead, out: &mut impl Write) -> Result<()> {
        let git = git_marks(&self.git_marks)?;
        let mut marks = Marks::load(&self.our_marks, &git)?;
        let pushed = self.ingest(input, &mut marks).await?;
        marks.save(&self.our_marks)?;
        for (refname, id) in &pushed.tips {
            match self.publish_ref(refname, *id, &pushed).await {
                Ok(()) => writeln!(out, "ok {refname}")?,
                Err(e) => writeln!(out, "error {refname} {}", format!("{e:#}").replace('\n', " "))?,
            }
        }
        writeln!(out)?;
        Ok(())
    }

    async fn publish_ref(&self, refname: &str, id: Id, pushed: &Pushed) -> Result<()> {
        if let Some(tag) = refname.strip_prefix("refs/tags/") {
            return self.publish_tag(tag, id).await;
        }
        let Some(branch) = refname.strip_prefix("refs/heads/") else {
            bail!("only branches and tags can be pushed");
        };
        self.publish(branch, id, pushed.chain(refname)).await
    }

    /// A server-side pre-receive hook, for pushes over git's own HTTP protocol: the pushed
    /// commits (`old new ref` lines, as git gives them) land as they are, with every check a
    /// land makes; any refusal is said per ref and fails the push. The mapping between git
    /// commits and changesets is kept only when every ref landed, so a refused push leaves
    /// no trace of objects git is about to discard.
    pub async fn receive(
        &self,
        git_dir: &Path,
        updates: &[(String, String, String)],
    ) -> Result<Vec<(String, Result<()>)>> {
        require(self.flags, Flag::GitBridge)?;
        const ZERO: &str = "0000000000000000000000000000000000000000";
        let mut results = Vec::new();
        let mut wanted = Vec::new();
        for (_, new, refname) in updates {
            if new.trim_start_matches('0').is_empty() || new == ZERO {
                results.push((
                    refname.clone(),
                    Err(anyhow::anyhow!("deleting is done in Grovedock (gd branch -d, or the app), not by a push")),
                ));
            } else {
                wanted.push(new.clone());
            }
        }
        if !results.is_empty() {
            return Ok(results);
        }
        let exported = self.git_marks.with_extension("pushing");
        let mut cmd = std::process::Command::new("git");
        cmd.arg("--git-dir")
            .arg(git_dir)
            .args(["fast-export", "--signed-tags=strip", "--show-original-ids"])
            .args(crate::features::git::identity_args())
            .arg(format!("--import-marks={}", self.git_marks.display()))
            .arg(format!("--export-marks={}", exported.display()))
            .args(&wanted)
            .stdout(std::process::Stdio::piped());
        let mut child = cmd.spawn().context("running git fast-export")?;
        let git = git_marks(&self.git_marks)?;
        let mut marks = Marks::load(&self.our_marks, &git)?;
        let stdout = child.stdout.take().context("git fast-export output")?;
        let pushed = self.ingest(&mut std::io::BufReader::new(stdout), &mut marks).await?;
        if !child.wait()?.success() {
            bail!("git fast-export failed");
        }
        // Commits git already had (a new branch at a known commit) come from the marks.
        let known: HashMap<String, Id> = {
            let mut shas = git;
            shas.extend(git_marks(&exported)?);
            let ours = marks.by_kind('c');
            shas.into_iter().filter_map(|(m, sha)| Some((sha, *ours.get(&m)?))).collect()
        };
        let mut all_ok = true;
        for (_, new, refname) in updates {
            let id = pushed.tips.get(new).or_else(|| known.get(new)).copied();
            let r = match id {
                None => Err(anyhow::anyhow!("couldn't read the pushed commit {new}")),
                Some(id) => self.publish_ref(refname, id, &pushed.renamed(new, refname)).await,
            };
            all_ok &= r.is_ok();
            results.push((refname.clone(), r));
        }
        if all_ok {
            marks.save(&self.our_marks)?;
            // Nothing new exported (a branch at a commit already here): the marks stand as they are.
            if exported.exists() {
                std::fs::rename(&exported, &self.git_marks)?;
            }
        } else {
            let _ = std::fs::remove_file(&exported);
        }
        Ok(results)
    }

    /// A tag is made once: pushing it again to the same change is a no-op, to another is refused.
    async fn publish_tag(&self, tag: &str, id: Id) -> Result<()> {
        let name = format!("{TAG_PREFIX}{tag}");
        match self.remote.get_ref(&name).await? {
            Some(at) if at == id => Ok(()),
            Some(at) => bail!("tag {tag} already points at {}; tags don't move, so make a new one", at.short()),
            None if self.remote.compare_and_swap(&name, None, id).await? => Ok(()),
            None => bail!("tag {tag} was made by someone else meanwhile; fetch it"),
        }
    }

    /// Lands a pushed branch at `id`. As it is when it fast-forwards; otherwise (unless
    /// `GD_PUSH=strict`) the server rebases the pushed commits, in order, onto the branch.
    async fn publish(&self, branch: &str, id: Id, chain: Vec<Id>) -> Result<()> {
        match self.remote.fast_forward(branch, id).await {
            Ok(_) => return Ok(()),
            Err(e) if self.strict_push || !format!("{e:#}").contains("non-fast-forward") => return Err(e),
            Err(_) => {}
        }
        let mut landed = None;
        for c in if chain.is_empty() { vec![id] } else { chain } {
            landed = Some(self.remote.land(branch, c).await?.0);
        }
        if let Some(l) = landed {
            eprintln!(
                "gd: {branch} had moved; the server rebased your push as {}. Run `git pull --rebase` to catch up.",
                l.short()
            );
        }
        Ok(())
    }
}
