use std::fmt::Write as _;

use anyhow::Result;
use grovedock_core::object::{FileChange, Kind};
use grovedock_core::tree::read_object;
use similar::TextDiff;

use crate::remote::Remote;
use crate::workspace::{ChangeKind, Workspace};

fn is_binary(b: &[u8]) -> bool {
    b.iter().take(8000).any(|&c| c == 0)
}

/// Render a unified diff for one file. Binary content is summarised.
pub fn unified(path: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> String {
    let (a, b) = (old.unwrap_or_default(), new.unwrap_or_default());
    let (from, to) = (
        if old.is_some() { format!("a/{path}") } else { "/dev/null".into() },
        if new.is_some() { format!("b/{path}") } else { "/dev/null".into() },
    );
    if is_binary(a) || is_binary(b) {
        return format!("Binary files {from} and {to} differ\n");
    }
    let (a, b) = (String::from_utf8_lossy(a), String::from_utf8_lossy(b));
    let mut out = format!("diff --gd a/{path} b/{path}\n");
    let _ = write!(out, "{}", TextDiff::from_lines(&a, &b).unified_diff().context_radius(3).header(&from, &to));
    out
}

/// Working copy vs the workspace base.
pub async fn working(remote: &dyn Remote, ws: &Workspace, pathspecs: &[String], names_only: bool) -> Result<String> {
    let mut out = String::new();
    let objects = remote.objects();
    for c in ws.status(pathspecs)? {
        if names_only {
            let tag = match c.kind {
                ChangeKind::Added => 'A',
                ChangeKind::Modified => 'M',
                ChangeKind::Deleted => 'D',
            };
            let _ = writeln!(out, "{tag}\t{}", c.path);
            continue;
        }
        let old = match ws.index.get(&c.path) {
            Some(e) => {
                Some(read_object(objects.as_ref(), &grovedock_core::Id::from_slice(&e.blob)?, Kind::Blob).await?)
            }
            None => None,
        };
        let new = if c.kind == ChangeKind::Deleted {
            None
        } else {
            Some(std::fs::read(ws.abs(&c.path)).or_else(|_| {
                std::fs::read_link(ws.abs(&c.path)).map(|t| t.to_string_lossy().into_owned().into_bytes())
            })?)
        };
        out.push_str(&unified(&c.path, old.as_deref(), new.as_deref()));
    }
    Ok(out)
}

/// Between two revisions, computed by the server; contents fetched by blob id.
pub async fn revisions(remote: &dyn Remote, from: &str, to: &str, names_only: bool) -> Result<String> {
    let changes: Vec<FileChange> = remote.diff(from, to).await?;
    let mut out = String::new();
    let objects = remote.objects();
    for c in changes {
        let old = remote.read_file(from, &c.path).await.ok().map(|(_, b)| b);
        if names_only {
            let tag = if c.is_delete() {
                'D'
            } else if old.is_some() {
                'M'
            } else {
                'A'
            };
            let _ = writeln!(out, "{tag}\t{}", c.path);
            continue;
        }
        let new = match c.blob_id()? {
            Some(id) => Some(read_object(objects.as_ref(), &id, Kind::Blob).await?),
            None => None,
        };
        out.push_str(&unified(&c.path, old.as_deref(), new.as_deref()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_text_and_binary() {
        let d = unified("f.txt", Some(b"a\nb\n"), Some(b"a\nc\n"));
        assert!(d.contains("--- a/f.txt") && d.contains("-b") && d.contains("+c"), "{d}");
        assert!(unified("new", None, Some(b"x\n")).contains("--- /dev/null"));
        assert!(unified("bin", Some(b"\0\x01"), Some(b"\0\x02")).starts_with("Binary files"));
    }
}
