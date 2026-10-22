use anyhow::{Context, Result, bail};
use grovedock_core::Id;
use grovedock_core::object::Changeset;

use super::guard_dirty;
use crate::remote::{Remote, RemoteChangeset};
use crate::workspace::Workspace;

/// Commits landed one after another at most; more need `--squash`.
const MAX_CHAIN: usize = 1000;

#[derive(Debug)]
pub struct LandOutcome {
    pub landed: Id,
    pub rebased: bool,
    /// Commits landed (several when there were several since the last land).
    pub commits: usize,
    /// Files updated in the working copy from changes others landed first.
    pub synced: usize,
    /// Set when the working copy could not be updated (uncommitted edits overlap).
    pub behind: Option<String>,
}

/// The commits since the last land, oldest first: first parents from `base` back to where the
/// branch's history already has them.
async fn unlanded(remote: &dyn Remote, ref_name: &str, base: Id) -> Result<Vec<RemoteChangeset>> {
    let first = remote.changeset(&base.to_hex()).await?;
    let Some(tip) = remote.get_ref(ref_name).await? else { return Ok(vec![first]) };
    let stop = remote.merge_base(&tip.to_hex(), &base.to_hex()).await?;
    let mut chain = vec![first];
    loop {
        let Some(parent) = chain.last().unwrap().changeset.first_parent()? else { break };
        if stop.is_none() || Some(parent) == stop {
            break;
        }
        if chain.len() == MAX_CHAIN {
            bail!("more than {MAX_CHAIN} commits since the last land: land them as one with `gd land --squash`");
        }
        chain.push(remote.changeset(&parent.to_hex()).await?);
    }
    chain.reverse();
    Ok(chain)
}

/// Land the workspace's commits on its ref, oldest first, each on the one before.
pub async fn run(remote: &dyn Remote, ws: &mut Workspace) -> Result<LandOutcome> {
    run_with(remote, ws, false).await
}

/// Like [`run`]; with `squash`, every commit since the last land goes as one change. The server
/// rebases when the ref moved; changes others landed first are then applied locally unless
/// they clash with uncommitted edits.
pub async fn run_with(remote: &dyn Remote, ws: &mut Workspace, squash: bool) -> Result<LandOutcome> {
    let Some(base) = ws.base else { bail!("nothing to land: commit first") };
    let ref_name = ws.config.ref_name.clone();
    let chain = if ws.base_landed {
        vec![remote.changeset(&base.to_hex()).await?]
    } else {
        unlanded(remote, &ref_name, base).await?
    };
    let mut todo: Vec<(Option<Id>, Changeset)> = chain.iter().map(|c| (Some(c.id), c.changeset.clone())).collect();
    if squash && todo.len() > 1 {
        // One change: everything from where the first started to the last.
        let from = chain[0].changeset.first_parent()?.context("the first commit has no parent to squash onto")?;
        let changes = remote.diff(&from.to_hex(), &base.to_hex()).await?;
        let message = chain.iter().map(|c| c.changeset.message.trim().to_owned()).collect::<Vec<_>>().join("\n\n");
        let last = chain.last().unwrap().changeset.clone();
        todo = vec![(None, Changeset { parents: vec![from.0.to_vec()], changes, message, git: None, ..last })];
    }
    let total = todo.len();
    let (mut landed, mut rebased) = (None::<Id>, false);
    for (i, (id, mut cs)) in todo.into_iter().enumerate() {
        // The first goes as committed; each after it on top of what the one before became.
        let id = match (landed, id) {
            (None, Some(id)) => id,
            (prev, _) => {
                if let Some(prev) = prev {
                    cs.parents = vec![prev.0.to_vec()];
                }
                remote.create_changeset(cs).await?.id
            }
        };
        match remote.land(&ref_name, id).await {
            Ok((l, r)) => {
                landed = Some(l);
                rebased |= r;
            }
            Err(e) if i > 0 => {
                // The rest stay committed, now on top of what landed, for the next `gd land`.
                ws.base = Some(id);
                ws.base_landed = false;
                ws.save()?;
                return Err(e.context(format!("landed {i} of {total} commits; the rest are still to land")));
            }
            Err(e) if total > 1 => {
                return Err(e.context(format!(
                    "the first of {total} commits can't land; if a later one fixes it, land them as one with `gd land --squash`"
                )));
            }
            Err(e) => return Err(e),
        }
    }
    let landed = landed.expect("something landed");
    ws.base_landed = true;
    let mut out = LandOutcome { landed, rebased, commits: total, synced: 0, behind: None };
    if landed != base {
        let incoming = remote.diff(&base.to_hex(), &landed.to_hex()).await?;
        match guard_dirty(ws, &incoming) {
            Ok(()) => {
                ws.apply(remote.objects(), &incoming).await?;
                ws.base = Some(landed);
                out.synced = incoming.len();
            }
            Err(e) => out.behind = Some(e.to_string()),
        }
    }
    ws.save()?;
    Ok(out)
}
