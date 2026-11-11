//! File modes git no longer writes. In git's first months trees held `100664` (group-writable
//! files), and some old tools wrote directories as `040000`. Git still reads them, but shows
//! them (and `fast-export` sends them) as `100644` and `40000`, so a history rebuilt from
//! fast-export gets other tree ids, and every commit after them other ids too. git/git has
//! 1,373 such entries in 98 of its 164,334 trees.
//!
//! The same holds for commit headers fast-export drops: `mergetag` (a merge of a signed tag
//! carries the tag) and others such as `change-id` (133 and 1 commits in git/git), and, with git
//! before 2.48, signatures (`gpgsig`).
//!
//! Import finds both by reading every tree's and commit's raw bytes ([`raw_modes`], a few
//! seconds for git/git, nothing stored when there are none) and records them with each
//! commit. Export puts them back ([`patch_commit`]): `fast-import` writes the commit as usual,
//! then only the trees on the way to those entries, and the commit, are rewritten.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;

use anyhow::{Context, Result, bail};
use grovedock_core::object::GitMode;

/// What a commit has that a git import can't carry.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Raw {
    pub modes: Vec<GitMode>,
    /// Its header lines after `committer`, verbatim, when any is one fast-export drops.
    pub headers: Vec<u8>,
}

/// Whether this machine's git carries commit signatures through `fast-export` and `fast-import`
/// (git 2.48+); older git strips them, so a signature is then one more header to keep.
pub fn fast_export_carries_signatures() -> bool {
    static SIGNED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SIGNED.get_or_init(|| {
        std::process::Command::new("git").args(["fast-export", "-h"]).output().is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout).contains("signed-commits")
                || String::from_utf8_lossy(&o.stderr).contains("signed-commits")
        })
    })
}

/// The header lines after `committer` of a raw commit, when one of them is a header that
/// fast-export drops (anything but encoding, and signatures where it carries them).
fn dropped_headers(commit: &[u8]) -> Vec<u8> {
    dropped_headers_with(commit, fast_export_carries_signatures())
}

/// A commit's header lines after `committer` as git writes them, for a commit whose signature
/// fast-import can't take: its encoding, then the signature folded into a `gpgsig` header.
pub fn signature_headers(encoding: &str, algorithm: &str, signature: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if !encoding.is_empty() {
        out.extend_from_slice(format!("encoding {encoding}\n").as_bytes());
    }
    let name: &[u8] = if algorithm.starts_with("sha256") { b"gpgsig-sha256" } else { b"gpgsig" };
    let body = signature.strip_suffix(b"\n").unwrap_or(signature);
    for (i, line) in body.split(|&b| b == b'\n').enumerate() {
        out.extend_from_slice(if i == 0 { name } else { b"" });
        out.push(b' ');
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

fn dropped_headers_with(commit: &[u8], signatures_carried: bool) -> Vec<u8> {
    let end = commit.windows(2).position(|w| w == b"\n\n").map_or(commit.len(), |i| i + 1);
    let head = &commit[..end];
    let Some(at) = find(head, b"\ncommitter ") else { return Vec::new() };
    let tail_start = head[at + 1..].iter().position(|&b| b == b'\n').map_or(head.len(), |i| at + 1 + i + 1);
    let tail = &head[tail_start..];
    let dropped = tail.split(|&b| b == b'\n').filter(|l| !l.is_empty() && !l.starts_with(b" ")).any(|l| {
        let name = l.split(|&b| b == b' ').next().unwrap_or_default();
        !(name == b"encoding" || signatures_carried && matches!(name, b"gpgsig" | b"gpgsig-sha256"))
    });
    if dropped { tail.to_vec() } else { Vec::new() }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The modes git writes today.
fn canonical(mode: &[u8]) -> bool {
    matches!(mode, b"100644" | b"100755" | b"120000" | b"160000" | b"40000")
}

/// How git writes `mode` today: what a raw mode reads as.
pub fn canonical_of(mode: &str) -> &'static str {
    let v = u32::from_str_radix(mode, 8).unwrap_or(0o100644);
    match v & 0o170000 {
        0o040000 => "40000",
        0o120000 => "120000",
        0o160000 => "160000",
        _ if v & 0o111 != 0 => "100755",
        _ => "100644",
    }
}

fn is_tree_mode(mode: &[u8]) -> bool {
    std::str::from_utf8(mode)
        .ok()
        .and_then(|m| u32::from_str_radix(m, 8).ok())
        .is_some_and(|v| v & 0o170000 == 0o040000)
}

/// One tree entry: mode, name and object id, as written.
type Entry<'a> = (&'a [u8], &'a [u8], &'a [u8]);

fn entries(data: &[u8]) -> Result<Vec<Entry<'_>>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let sp = data[i..].iter().position(|&b| b == b' ').context("damaged tree")? + i;
        let nul = data[sp..].iter().position(|&b| b == 0).context("damaged tree")? + sp;
        let end = nul + 1 + 20;
        if end > data.len() {
            // A sha256 repository: 32-byte ids.
            bail!("only sha1 repositories are read for raw modes");
        }
        out.push((&data[i..sp], &data[sp + 1..nul], &data[nul + 1..end]));
        i = end;
    }
    Ok(out)
}

/// Every commit and tree reachable from `rev`, with its raw bytes: `rev-list` into
/// `cat-file --batch`, streamed.
fn each_object(git_dir: &Path, rev: &str, mut f: impl FnMut(&str, &[u8], &[u8]) -> Result<bool>) -> Result<()> {
    let mut list = Command::new("git")
        .arg("-C")
        .arg(git_dir)
        .args(["rev-list", "--objects", "--no-object-names", "--filter=blob:none", rev])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("running git rev-list")?;
    let mut cat = Command::new("git")
        .arg("-C")
        .arg(git_dir)
        .args(["cat-file", "--batch"])
        .stdin(list.stdout.take().expect("piped"))
        .stdout(Stdio::piped())
        .spawn()
        .context("running git cat-file")?;
    let mut r = BufReader::with_capacity(1 << 20, cat.stdout.take().expect("piped"));
    let mut header = String::new();
    let mut data = Vec::new();
    let mut stopped = false;
    loop {
        header.clear();
        if r.read_line(&mut header)? == 0 {
            break;
        }
        let mut parts = header.split_whitespace();
        let (Some(_id), Some(kind), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            bail!("unexpected git cat-file output: {header}");
        };
        let size: usize = size.parse()?;
        data.resize(size, 0);
        r.read_exact(&mut data)?;
        r.read_exact(&mut [0u8])?;
        if !f(kind, header.split_whitespace().next().unwrap_or_default().as_bytes(), &data)? {
            stopped = true;
            break;
        }
    }
    if stopped {
        let _ = cat.kill();
        let _ = list.kill();
    }
    let _ = cat.wait();
    let _ = list.wait();
    Ok(())
}

/// Each commit reachable from `rev` that a git import can't carry as it is: its tree holds
/// modes git no longer writes (full paths, modes as written), or its header lines fast-export
/// drops. Empty, after one quick pass, for nearly every repository.
pub fn raw_modes(git_dir: &Path, rev: &str) -> Result<HashMap<String, Raw>> {
    // First pass: is there anything at all? Stops at the first one.
    let mut any = false;
    each_object(git_dir, rev, |kind, _, data| {
        if (kind == "tree" && entries(data)?.iter().any(|(m, _, _)| !canonical(m)))
            || (kind == "commit" && !dropped_headers(data).is_empty())
        {
            any = true;
            return Ok(false);
        }
        Ok(true)
    })?;
    if !any {
        return Ok(HashMap::new());
    }
    // Second pass: what each tree holds that matters here (subtrees, odd modes), and each
    // commit's root tree.
    type Node = (Vec<(Box<str>, [u8; 20])>, Vec<(Box<str>, Box<str>)>);
    let mut trees: HashMap<[u8; 20], Node> = HashMap::new();
    let mut commits: Vec<(String, [u8; 20], Vec<u8>)> = Vec::new();
    each_object(git_dir, rev, |kind, id, data| {
        match kind {
            "tree" => {
                let key = hex_id(std::str::from_utf8(id)?)?;
                let mut node: Node = (Vec::new(), Vec::new());
                for (mode, name, oid) in entries(data)? {
                    let name: Box<str> = String::from_utf8_lossy(name).into();
                    if is_tree_mode(mode) {
                        node.0.push((name.clone(), oid.try_into()?));
                    }
                    if !canonical(mode) {
                        node.1.push((name, String::from_utf8_lossy(mode).into()));
                    }
                }
                trees.insert(key, node);
            }
            "commit" => {
                let first = data.split(|&b| b == b'\n').next().unwrap_or_default();
                let root = std::str::from_utf8(first.strip_prefix(b"tree ").context("commit without a tree")?)?;
                commits.push((String::from_utf8_lossy(id).into_owned(), hex_id(root)?, dropped_headers(data)));
            }
            _ => {}
        }
        Ok(true)
    })?;
    let mut memo: Memo = HashMap::new();
    let mut out = HashMap::new();
    for (commit, root, headers) in commits {
        let found = odd_entries(&root, &trees, &mut memo);
        if !found.is_empty() || !headers.is_empty() {
            let modes = found.iter().map(|(path, mode)| GitMode { path: path.clone(), mode: mode.clone() }).collect();
            out.insert(commit, Raw { modes, headers });
        }
    }
    Ok(out)
}

/// Each tree's odd entries, as paths from it, worked out once.
type Memo = HashMap<[u8; 20], Rc<Vec<(String, String)>>>;

/// The odd entries under a tree, as paths from it; each tree worked out once.
type Trees = HashMap<[u8; 20], (Vec<(Box<str>, [u8; 20])>, Vec<(Box<str>, Box<str>)>)>;
fn odd_entries(tree: &[u8; 20], trees: &Trees, memo: &mut Memo) -> Rc<Vec<(String, String)>> {
    if let Some(found) = memo.get(tree) {
        return found.clone();
    }
    let mut found: Vec<(String, String)> = Vec::new();
    if let Some((subtrees, odd)) = trees.get(tree) {
        found.extend(odd.iter().map(|(n, m)| (n.to_string(), m.to_string())));
        for (name, child) in subtrees {
            for (path, mode) in odd_entries(child, trees, memo).iter() {
                found.push((format!("{name}/{path}"), mode.clone()));
            }
        }
    }
    found.sort();
    let found = Rc::new(found);
    memo.insert(*tree, found.clone());
    found
}

fn hex_id(s: &str) -> Result<[u8; 20]> {
    let s = s.trim();
    if s.len() != 40 {
        bail!("only sha1 repositories are read for raw modes");
    }
    let mut out = [0u8; 20];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)?;
    }
    Ok(out)
}

/// The commit `commit` (just written by `fast-import`, so with canonical modes) with the given
/// entries' modes as written, any entry its first parent had written oddly (`inherited`;
/// `fast-import` keeps those) back to how git writes it today unless `want` names it too, and
/// `headers` (when given) as its header lines after `committer`. Only the trees on the way to
/// those entries are rewritten. Returns the commit to use (the same one when nothing changed).
pub fn patch_commit(
    git_dir: &Path,
    commit: &str,
    want: &[GitMode],
    inherited: &[GitMode],
    headers: &[u8],
) -> Result<String> {
    let mut modes: BTreeMap<String, Option<String>> = BTreeMap::new();
    for m in inherited {
        modes.insert(m.path.clone(), None);
    }
    for m in want {
        modes.insert(m.path.clone(), Some(m.mode.clone()));
    }
    let raw = git_bytes(git_dir, &["cat-file", "commit", commit])?;
    let first_end = raw.iter().position(|&b| b == b'\n').context("damaged commit")?;
    let tree = std::str::from_utf8(raw[..first_end].strip_prefix(b"tree ").context("commit without a tree")?)?;
    let new_tree = patch_tree(git_dir, tree, "", &modes)?;
    if new_tree == tree && headers.is_empty() {
        return Ok(commit.to_owned());
    }
    let mut bytes = format!("tree {new_tree}").into_bytes();
    bytes.extend_from_slice(&raw[first_end..]);
    if !headers.is_empty() {
        // The original header lines after `committer` replace what fast-import wrote there.
        let end = find(&bytes, b"\n\n").context("damaged commit")? + 1;
        let at = find(&bytes[..end], b"\ncommitter ").context("commit without a committer")?;
        let after = bytes[at + 1..].iter().position(|&b| b == b'\n').context("damaged commit")? + at + 2;
        bytes = [&bytes[..after], headers, &bytes[end..]].concat();
    }
    write_object(git_dir, "commit", &bytes)
}

fn patch_tree(git_dir: &Path, tree: &str, prefix: &str, modes: &BTreeMap<String, Option<String>>) -> Result<String> {
    let raw = git_bytes(git_dir, &["cat-file", "tree", tree])?;
    let mut changed = false;
    let mut out = Vec::with_capacity(raw.len());
    for (mode, name, oid) in entries(&raw)? {
        let path = format!("{prefix}{}", String::from_utf8_lossy(name));
        let mut mode = String::from_utf8_lossy(mode).into_owned();
        let mut oid = oid.to_vec();
        if let Some(set) = modes.get(&path) {
            let next = set.clone().unwrap_or_else(|| canonical_of(&mode).to_owned());
            changed |= next != mode;
            mode = next;
        }
        let under = format!("{path}/");
        if is_tree_mode(mode.as_bytes())
            && modes.range(under.clone()..).next().is_some_and(|(k, _)| k.starts_with(&under))
        {
            let child: String = oid.iter().map(|b| format!("{b:02x}")).collect();
            let next = patch_tree(git_dir, &child, &under, modes)?;
            if next != child {
                oid = (0..20).map(|i| u8::from_str_radix(&next[2 * i..2 * i + 2], 16)).collect::<Result<_, _>>()?;
                changed = true;
            }
        }
        out.extend_from_slice(mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(name);
        out.push(0);
        out.extend_from_slice(&oid);
    }
    if !changed {
        return Ok(tree.to_owned());
    }
    write_object(git_dir, "tree", &out)
}

fn git_bytes(git_dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git").arg("-C").arg(git_dir).args(args).output().context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// Writes an object as given: `--literally`, because git's checks refuse the very modes this
/// exists to keep.
fn write_object(git_dir: &Path, kind: &str, bytes: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(git_dir)
        .args(["hash-object", "-w", "--literally", "--stdin", "-t", kind])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git hash-object")?;
    child.stdin.take().expect("piped").write_all(bytes)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("git hash-object: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_fast_export_drops_are_kept_verbatim() {
        let plain = b"tree t\nparent p\nauthor a 1 +0000\ncommitter c 1 +0000\n\nmsg\n";
        assert!(dropped_headers(plain).is_empty());
        let signed = b"tree t\nauthor a 1 +0000\ncommitter c 1 +0000\ngpgsig -----BEGIN\n x\n -----END\n\nmsg\n";
        assert!(dropped_headers_with(signed, true).is_empty(), "git 2.48+ fast-export carries signatures");
        assert_eq!(dropped_headers_with(signed, false), b"gpgsig -----BEGIN\n x\n -----END\n", "older git strips them");
        // Written back, a signature folds into the header it came from.
        assert_eq!(signature_headers("", "sha1", b"-----BEGIN\nx\n-----END\n"), b"gpgsig -----BEGIN\n x\n -----END\n");
        assert_eq!(
            signature_headers("ISO-8859-1", "sha256 openpgp", b"a\n\nb\n"),
            b"encoding ISO-8859-1\ngpgsig-sha256 a\n \n b\n"
        );
        let merged = b"tree t\nauthor a 1 +0000\ncommitter c 1 +0000\nmergetag object o\n type commit\n\nmsg\n";
        assert_eq!(dropped_headers(merged), b"mergetag object o\n type commit\n");
    }

    #[test]
    fn modes_read_as_git_writes_them_now() {
        assert_eq!(canonical_of("100664"), "100644");
        assert_eq!(canonical_of("100775"), "100755");
        assert_eq!(canonical_of("040000"), "40000");
        assert!(canonical(b"100644") && !canonical(b"100664") && !canonical(b"040000"));
        assert!(is_tree_mode(b"040000") && is_tree_mode(b"40000") && !is_tree_mode(b"100644"));
    }
}
