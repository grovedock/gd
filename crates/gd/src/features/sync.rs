use anyhow::{Context, Result, bail};
use grovedock_core::Id;
use grovedock_core::object::FileChange;

use super::guard_dirty;
use crate::remote::Remote;
use crate::workspace::{Workspace, list_files};

#[derive(Debug)]
pub struct SyncOutcome {
    pub from: Option<Id>,
    pub to: Id,
    pub changed: usize,
}

/// Move the working copy to `target` (default: the workspace ref head). Refuses to drop
/// unlanded local changesets or to overwrite uncommitted edits.
pub async fn run(remote: &dyn Remote, ws: &mut Workspace, target: Option<&str>) -> Result<SyncOutcome> {
    let to = match target {
        Some(rev) => remote.changeset(rev).await?.id,
        None => remote
            .get_ref(&ws.config.ref_name)
            .await?
            .with_context(|| format!("ref `{}` does not exist yet", ws.config.ref_name))?,
    };
    let from = ws.base;
    if from == Some(to) {
        return Ok(SyncOutcome { from, to, changed: 0 });
    }
    if let Some(b) = from
        && !ws.base_landed
        && remote.merge_base(&b.to_hex(), &to.to_hex()).await? != Some(b)
    {
        bail!("workspace has unlanded changesets ({}); run `gd land` first", b.short());
    }
    let incoming: Vec<FileChange> = match from {
        Some(b) => remote.diff(&b.to_hex(), &to.to_hex()).await?,
        None => {
            let root = remote.changeset(&to.to_hex()).await?.root_tree;
            list_files(remote.objects().as_ref(), root, &ws.config.sparse)
                .await?
                .into_iter()
                .map(|(p, (m, id))| FileChange::upsert(p, m, id))
                .collect()
        }
    };
    guard_dirty(ws, &incoming)?;
    if remote.whoami().await?.is_full() {
        ws.apply(remote.objects(), &incoming).await?;
    } else {
        // Path-scoped readers: the server already filtered the diff to readable paths.
        for c in incoming.iter().filter(|c| c.is_delete() && crate::workspace::in_sparse(&ws.config.sparse, &c.path)) {
            crate::workspace::remove_path(&ws.root, &c.path)?;
            ws.index.remove(&c.path);
        }
        let paths: Vec<String> = incoming
            .iter()
            .filter(|c| !c.is_delete() && crate::workspace::in_sparse(&ws.config.sparse, &c.path))
            .map(|c| c.path.clone())
            .collect();
        if !paths.is_empty() {
            ws.checkout_scoped(remote, &to.to_hex(), &paths).await?;
        }
    }
    ws.base = Some(to);
    ws.base_landed = true;
    ws.save()?;
    Ok(SyncOutcome { from, to, changed: incoming.len() })
}

/// `gd checkout -b NAME`: create a ref at the workspace base and switch to it.
pub async fn create_branch(remote: &dyn Remote, ws: &mut Workspace, name: &str) -> Result<()> {
    let base = ws.base.context("nothing committed yet")?;
    if !remote.compare_and_swap(name, None, base).await? {
        bail!("ref `{name}` already exists");
    }
    ws.config.ref_name = name.to_owned();
    ws.save()
}

/// `gd checkout REV`: switch the working copy; a ref name also becomes the default ref.
pub async fn checkout(remote: &dyn Remote, ws: &mut Workspace, rev: &str) -> Result<SyncOutcome> {
    let is_ref = remote.get_ref(rev).await.ok().flatten().is_some();
    let out = run(remote, ws, Some(rev)).await?;
    if is_ref {
        ws.config.ref_name = rev.to_owned();
        ws.save()?;
    }
    Ok(out)
}
