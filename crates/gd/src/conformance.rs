//! `gd conformance`: the checks of docs/spec/protocol-v1.md, run against any server. Each check
//! names its requirement ([OBJ-1], [LAND-3], …) and says what it saw. Everything it writes is
//! under refs `conformance/<run>/…` and `tags/conformance-<run>` in the repository it's given.

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use grovedock_core::Id;
use grovedock_core::flags::FeatureFlags;
use grovedock_core::object::{CHANGESET_TAG, Changeset, FileChange, Kind, Mode, decode_object, encode_object};

use crate::remote::{Remote, RemoteChangeset};

/// One requirement's result.
pub struct Outcome {
    pub id: &'static str,
    pub what: &'static str,
    /// `None`: passed. `Some(Err)`: failed, why. `Some(Ok)`: skipped, why.
    pub failed: Option<std::result::Result<String, String>>,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.failed.is_none()
    }
    pub fn skipped(&self) -> bool {
        matches!(self.failed, Some(Ok(_)))
    }
}

struct Ctx<'a> {
    r: &'a dyn Remote,
    run: String,
}

impl Ctx<'_> {
    fn branch(&self, name: &str) -> String {
        format!("conformance/{}/{name}", self.run)
    }

    async fn blob(&self, body: &str) -> Result<Id> {
        let (id, payload) = encode_object(Kind::Blob, body.as_bytes());
        self.r.objects().put_many(vec![(id, payload)]).await?;
        Ok(id)
    }

    async fn change(&self, parent: Option<Id>, files: &[(&str, &str)], message: &str) -> Result<RemoteChangeset> {
        let mut changes = Vec::new();
        for (path, body) in files {
            changes.push(FileChange::upsert(*path, Mode::File, self.blob(body).await?));
        }
        self.r
            .create_changeset(Changeset {
                parents: parent.iter().map(|p| p.0.to_vec()).collect(),
                changes,
                author: "conformance".into(),
                message: message.into(),
                timestamp_ms: 1_700_000_000_000,
                git: None,
            })
            .await
    }
}

macro_rules! check {
    ($out:ident, $id:literal, $what:literal, $body:expr) => {{
        let r: Result<()> = async { $body }.await;
        $out.push(Outcome { id: $id, what: $what, failed: r.err().map(|e| Err(format!("{e:#}"))) });
    }};
}

/// Runs every check. `git_dir_root` is a scratch folder for the git round trip; `None` skips it.
pub async fn run(r: &dyn Remote, flags: &dyn FeatureFlags, git_dir_root: Option<&Path>) -> Vec<Outcome> {
    let run =
        format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    let c = Ctx { r, run };
    let mut out = Vec::new();

    // Objects
    let body = format!("conformance {}\n", c.run);
    let (blob, payload) = encode_object(Kind::Blob, body.as_bytes());
    check!(out, "OBJ-1", "an object's id is BLAKE3(kind || content)", {
        let mut h = blake3::Hasher::new();
        h.update(&[1]);
        h.update(body.as_bytes());
        ensure!(blob.0 == *h.finalize().as_bytes(), "the client library disagrees with the spec");
        c.r.objects().put_many(vec![(blob, payload.clone())]).await?;
        let got = c.r.objects().get(&blob).await?.context("the server doesn't have what it was just given")?;
        let (kind, content) = decode_object(&blob, &got)?;
        ensure!(kind == Kind::Blob && content == body.as_bytes(), "the server gave back different content");
        Ok(())
    });
    check!(out, "OBJ-2", "a payload that doesn't match its id is refused", {
        let (other, _) = encode_object(Kind::Blob, b"something else");
        match c.r.objects().put_many(vec![(other, payload.clone())]).await {
            Err(_) => Ok(()),
            Ok(()) => match c.r.objects().get(&other).await? {
                None => Ok(()),
                Some(_) => bail!("the server stored a payload under an id it doesn't hash to"),
            },
        }
    });
    check!(out, "OBJ-3", "Missing returns exactly the ids the server can't serve", {
        let (absent, _) = encode_object(Kind::Blob, format!("never uploaded {}", c.run).as_bytes());
        let missing = c.r.objects().missing(&[blob, absent]).await?;
        ensure!(missing == vec![absent], "asked about one present and one absent object, got {missing:?}");
        Ok(())
    });
    check!(out, "OBJ-4", "Get streams the payloads asked for", {
        let got = c.r.objects().get_many(&[blob]).await?;
        ensure!(got.len() == 1 && got[0].is_some(), "asked for one object, got {}", got.iter().flatten().count());
        Ok(())
    });

    // Changesets
    let mut first = None;
    check!(out, "CS-1", "a changeset's id is BLAKE3(3 || its canonical encoding)", {
        let cs = c.change(None, &[("README.md", "conformance\n"), ("src/a.txt", "a\n")], "First").await?;
        let (_, bytes) = cs.changeset.encode_canonical();
        let mut h = blake3::Hasher::new();
        h.update(&[CHANGESET_TAG]);
        h.update(&bytes);
        ensure!(cs.id.0 == *h.finalize().as_bytes(), "the id doesn't hash from the encoding returned");
        first = Some(cs);
        Ok(())
    });
    let first = match first {
        Some(f) => f,
        None => return out, // nothing further can be checked without a changeset
    };
    check!(out, "CS-2", "creating a changeset derives its root tree and generation", {
        let t = c.r.objects().get(&first.root_tree).await?.context("its root tree isn't on the server")?;
        let (kind, content) = decode_object(&first.root_tree, &t)?;
        let tree = grovedock_core::tree::parse_tree(&first.root_tree, &content)?;
        ensure!(kind == Kind::Tree, "the root tree isn't a tree");
        let names: Vec<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();
        ensure!(names == ["README.md", "src"], "the root tree lists {names:?}");
        let child = c.change(Some(first.id), &[("b.txt", "b\n")], "Second").await?;
        ensure!(
            child.generation > first.generation,
            "a child's generation ({}) isn't above its parent's ({})",
            child.generation,
            first.generation
        );
        Ok(())
    });
    check!(out, "CS-3", "creating the same changeset twice gives the same id", {
        let again = c.change(None, &[("README.md", "conformance\n"), ("src/a.txt", "a\n")], "First").await?;
        ensure!(again.id == first.id, "{} then {}", first.id, again.id);
        Ok(())
    });
    check!(out, "CS-4", "paths under .git or .gd are refused", {
        for bad in [".git/config", "x/.gd/hooks"] {
            ensure!(c.change(Some(first.id), &[(bad, "x")], "Bad").await.is_err(), "`{bad}` was accepted");
        }
        Ok(())
    });
    check!(out, "CS-5", "a changeset whose parent the server doesn't have is refused", {
        let ghost = Id::hash(CHANGESET_TAG, c.run.as_bytes());
        ensure!(c.change(Some(ghost), &[("a", "a")], "Orphan").await.is_err(), "it was accepted");
        Ok(())
    });

    // Refs
    let main = c.branch("main");
    check!(out, "REF-1", "compare-and-swap moves a ref only from what it holds", {
        ensure!(c.r.compare_and_swap(&main, None, first.id).await?, "creating a new ref didn't swap");
        ensure!(c.r.get_ref(&main).await? == Some(first.id), "Get doesn't show the new ref");
        let other = c.change(Some(first.id), &[("x.txt", "x\n")], "X").await?;
        ensure!(!c.r.compare_and_swap(&main, None, other.id).await?, "it swapped although the ref exists");
        ensure!(
            !c.r.compare_and_swap(&main, Some(other.id), other.id).await?,
            "it swapped from a value the ref doesn't hold"
        );
        ensure!(c.r.get_ref(&main).await? == Some(first.id), "a failed swap changed the ref");
        ensure!(c.r.list_refs().await?.iter().any(|(n, id)| *n == main && *id == first.id), "List doesn't show it");
        Ok(())
    });
    check!(out, "REF-2", "a tag is made once and never moves", {
        let tag = format!("tags/conformance-{}", c.run);
        ensure!(c.r.compare_and_swap(&tag, None, first.id).await?, "the tag wasn't made");
        let other = c.change(Some(first.id), &[("t.txt", "t\n")], "T").await?;
        let moved = c.r.compare_and_swap(&tag, Some(first.id), other.id).await.unwrap_or(false);
        ensure!(!moved && c.r.get_ref(&tag).await? == Some(first.id), "the tag moved");
        ensure!(c.r.land(&tag, other.id).await.is_err(), "a change landed on a tag");
        Ok(())
    });

    // Landing
    check!(out, "LAND-1", "a changeset on the ref's head lands as it is", {
        let head = c.r.get_ref(&main).await?.context("no main")?;
        let next = c.change(Some(head), &[("l1.txt", "1\n")], "L1").await?;
        let (landed, rebased) = c.r.land(&main, next.id).await?;
        ensure!(landed == next.id && !rebased, "landed as {landed} (rebased: {rebased})");
        Ok(())
    });
    let mut a_landed = None;
    check!(out, "LAND-2", "a changeset on an older base is rebased when its paths didn't change", {
        let base = c.r.get_ref(&main).await?.context("no main")?;
        let a = c.change(Some(base), &[("l2-a.txt", "a\n")], "L2 A").await?;
        let b = c.change(Some(base), &[("l2-b.txt", "b\n")], "L2 B").await?;
        c.r.land(&main, a.id).await?;
        let (landed, rebased) = c.r.land(&main, b.id).await?;
        ensure!(rebased && landed != b.id, "landed as {landed} (rebased: {rebased})");
        let (_, text) = c.r.read_file(&main, "l2-a.txt").await?;
        ensure!(text == b"a\n", "the other change was lost");
        a_landed = Some(a.id);
        Ok(())
    });
    check!(out, "LAND-3", "a land whose paths changed on both sides fails, changing nothing", {
        let base = c.r.get_ref(&main).await?.context("no main")?;
        let a = c.change(Some(base), &[("clash.txt", "a\n")], "Clash A").await?;
        let b = c.change(Some(base), &[("clash.txt", "b\n")], "Clash B").await?;
        c.r.land(&main, a.id).await?;
        let head = c.r.get_ref(&main).await?;
        let e = c.r.land(&main, b.id).await.err().context("the clashing change landed")?;
        ensure!(format!("{e:#}").contains("clash.txt"), "the refusal doesn't name the path: {e:#}");
        ensure!(c.r.get_ref(&main).await? == head, "the ref moved");
        Ok(())
    });
    check!(out, "LAND-4", "concurrent lands of different paths all succeed", {
        let base = c.r.get_ref(&main).await?.context("no main")?;
        let mut ids = Vec::new();
        for i in 0..8 {
            ids.push(
                c.change(Some(base), &[(&format!("many/{i}.txt"), &format!("{i}\n"))], &format!("Many {i}")).await?.id,
            );
        }
        let lands = futures::future::join_all(ids.iter().map(|id| c.r.land(&main, *id))).await;
        let failed = lands.iter().filter(|l| l.is_err()).count();
        ensure!(failed == 0, "{failed} of 8 failed");
        let files = c.r.list_dir(&main, "many").await?;
        ensure!(files.len() == 8, "{} of 8 files are on the branch", files.len());
        Ok(())
    });
    check!(out, "LAND-5", "landing a changeset that already landed doesn't apply it twice", {
        let a = a_landed.context("LAND-2 didn't run")?;
        let before = c.r.log(&main, "", 1000).await?.len();
        c.r.land(&main, a).await?;
        let after = c.r.log(&main, "", 1000).await?.len();
        ensure!(before == after, "history grew from {before} to {after} changes");
        Ok(())
    });

    // History and reading
    check!(out, "HIST-1", "Log follows first parents, newest first, filtered by path", {
        let all = c.r.log(&main, "", 1000).await?;
        ensure!(all.len() >= 10, "{} changes in the log", all.len());
        ensure!(all.windows(2).all(|w| w[0].generation > w[1].generation), "not newest first");
        let one = c.r.log(&main, "l1.txt", 1000).await?;
        ensure!(one.len() == 1 && one[0].changeset.message == "L1", "filtered to a path: {} changes", one.len());
        Ok(())
    });
    check!(out, "HIST-2", "Blame gives each line's last changeset and its line there", {
        let base = c.r.get_ref(&main).await?.context("no main")?;
        let one = c.change(Some(base), &[("blame.txt", "one\ntwo\n")], "Blame 1").await?;
        c.r.land(&main, one.id).await?;
        let two = c.change(Some(one.id), &[("blame.txt", "zero\none\ntwo\n")], "Blame 2").await?;
        c.r.land(&main, two.id).await?;
        let lines = c.r.blame(&main, "blame.txt").await?;
        let owners: Vec<(Id, &str)> = lines.iter().map(|(id, t, _)| (*id, t.as_str())).collect();
        ensure!(owners == [(two.id, "zero"), (one.id, "one"), (one.id, "two")], "got {owners:?}");
        Ok(())
    });
    check!(out, "HIST-3", "Diff lists the files that differ", {
        let changes = c.r.diff(&first.id.to_hex(), &main).await?;
        ensure!(
            changes.iter().any(|f| f.path == "l1.txt") && !changes.iter().any(|f| f.path == "README.md"),
            "{} changes",
            changes.len()
        );
        Ok(())
    });
    check!(out, "HIST-4", "MergeBase finds the common ancestor", {
        let head = c.r.get_ref(&main).await?.context("no main")?;
        let side = c.change(Some(first.id), &[("side.txt", "s\n")], "Side").await?;
        let mb = c.r.merge_base(&head.to_hex(), &side.id.to_hex()).await?;
        ensure!(mb == Some(first.id), "got {mb:?}");
        Ok(())
    });
    check!(out, "READ-1", "ReadFile and ListDir read a revision", {
        let (mode, text) = c.r.read_file(&main, "src/a.txt").await?;
        ensure!(text == b"a\n" && mode == Mode::File, "read {:?}", String::from_utf8_lossy(&text));
        let root = c.r.list_dir(&main, "").await?;
        ensure!(root.iter().any(|e| e.name == "src" && e.mode == Mode::Dir), "the root doesn't list src/");
        Ok(())
    });

    // Locks
    check!(out, "LOCK-1", "a file can be locked, listed and unlocked", {
        let path = format!("conformance-{}.psd", c.run);
        c.r.lock(&path).await?;
        ensure!(c.r.locks("").await?.iter().any(|(p, _, _)| *p == path), "the lock isn't listed");
        c.r.unlock(&path, false).await?;
        ensure!(!c.r.locks("").await?.iter().any(|(p, _, _)| *p == path), "it's still locked");
        Ok(())
    });

    // Git
    match git_dir_root {
        Some(root) if std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success()) => {
            check!(out, "GIT-1", "a git history comes back as the very same commits", {
                git_round_trip(&c, flags, root).await
            });
            check!(out, "GIT-2", "gd's own changes become the same git commits every time", {
                let x = root.join("export-1");
                let y = root.join("export-2");
                crate::features::git::export(c.r, flags, &main, &x, "main").await?;
                crate::features::git::export(c.r, flags, &main, &y, "main").await?;
                let (a, b) = (git(&x, &["rev-parse", "main"])?, git(&y, &["rev-parse", "main"])?);
                ensure!(a == b, "two exports gave {a} and {b}");
                Ok(())
            });
        }
        _ => {
            for (id, what) in [
                ("GIT-1", "a git history comes back as the very same commits"),
                ("GIT-2", "gd's own changes become the same git commits every time"),
            ] {
                out.push(Outcome { id, what, failed: Some(Ok("needs git on this machine".into())) });
            }
        }
    }
    out
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Ada Lovelace")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada Lovelace")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-02T03:04:05+05:30")
        .env("GIT_COMMITTER_DATE", "2026-01-02T03:04:05+05:30")
        .output()?;
    ensure!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

/// A small git history with a merge, an executable, a symlink and a non-UTF-8 message, imported
/// and exported again: every commit id must come back unchanged.
async fn git_round_trip(c: &Ctx<'_>, flags: &dyn FeatureFlags, root: &Path) -> Result<()> {
    let src = root.join("source");
    std::fs::create_dir_all(&src)?;
    git(&src, &["init", "-q", "-b", "main"])?;
    std::fs::write(src.join("a.txt"), "a\n")?;
    std::fs::write(src.join("run.sh"), "#!/bin/sh\necho hi\n")?;
    git(&src, &["add", "."])?;
    git(&src, &["update-index", "--chmod=+x", "run.sh"])?;
    git(&src, &["commit", "-qm", "Start"])?;
    git(&src, &["checkout", "-qb", "side"])?;
    std::fs::write(src.join("b.txt"), "b\n")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", src.join("link"))?;
    git(&src, &["add", "."])?;
    git(&src, &["commit", "-qm", "Side"])?;
    git(&src, &["checkout", "-q", "main"])?;
    std::fs::write(src.join("c.txt"), "c\n")?;
    git(&src, &["add", "."])?;
    git(&src, &["-c", "i18n.commitEncoding=ISO-8859-1", "commit", "-qm", "Caf\u{e9}"])?;
    git(&src, &["merge", "-q", "--no-ff", "side", "-m", "Merge side"])?;
    let want = git(&src, &["rev-list", "--all"])?;

    let branch = c.branch("git");
    crate::features::git::import(c.r, flags, &src, "refs/heads/main", &branch, false).await?;
    let back = root.join("back");
    crate::features::git::export(c.r, flags, &branch, &back, "main").await?;
    let got = git(&back, &["rev-list", "--all"])?;
    let (mut w, mut g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
    w.sort_unstable();
    g.sort_unstable();
    ensure!(w == g, "sent {} commits ({}), got back {} ({})", w.len(), w.join(" "), g.len(), g.join(" "));
    Ok(())
}
