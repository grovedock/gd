//! Client feature slices. Each returns plain data; `main.rs` owns presentation.
//! Server-side features are flag-checked by the server; client-only features
//! (sparse checkout, git import/export) check their flags here.

pub mod browse;
pub mod clone;
pub mod commit;
pub mod diff;
pub mod git;
pub mod land;
pub mod sync;

use anyhow::{Result, bail};
use grovedock_core::object::FileChange;
use grovedock_core::paths_overlap;

use crate::workspace::{LocalChange, Workspace};

/// Refuse to overwrite uncommitted local edits with incoming changes.
pub(crate) fn guard_dirty(ws: &Workspace, incoming: &[FileChange]) -> Result<()> {
    let dirty: Vec<LocalChange> = ws.status(&[])?;
    let clashes: Vec<&str> = dirty
        .iter()
        .filter(|d| incoming.iter().any(|c| paths_overlap(&c.path, &d.path)))
        .map(|d| d.path.as_str())
        .take(20)
        .collect();
    if !clashes.is_empty() {
        bail!("uncommitted changes would be overwritten: {}\ncommit them first (`gd commit`)", clashes.join(", "));
    }
    Ok(())
}
