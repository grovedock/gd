use anyhow::{Result, bail};
use bytes::Bytes;
use grovedock_core::object::{Changeset, FileChange};
use std::collections::HashMap;

use grovedock_core::{Id, paths_overlap};

use crate::remote::Remote;
use crate::workspace::{ChangeKind, LocalChange, Workspace, is_large};

/// Upload batch ceiling (compressed bytes) per Put stream.
const UPLOAD_BATCH_BYTES: usize = 32 << 20;

#[derive(Debug)]
pub struct Committed {
    pub id: Id,
    pub files: usize,
    /// Objects sent: a small commit goes whole; a larger one only with what the server lacks.
    pub uploaded: usize,
}

/// Upload changed blobs (only those the server lacks) and create a changeset on the
/// server with the workspace base as parent. `pathspecs` limits the commit to matching
/// paths (git's staging, without a staging area).
pub async fn run(
    remote: &dyn Remote,
    ws: &mut Workspace,
    message: &str,
    author: &str,
    pathspecs: &[String],
) -> Result<Option<Committed>> {
    if message.trim().is_empty() {
        bail!("empty commit message (use -m)");
    }
    let changes = ws.status(pathspecs)?;
    if changes.is_empty() {
        return Ok(None);
    }
    let upserts: Vec<String> =
        changes.iter().filter(|c| c.kind != ChangeKind::Deleted).map(|c| c.path.clone()).collect();
    // Re-read and encode now: the encoded id is authoritative even if a file changed
    // since the status scan. Large files stream through the chunker instead.
    let (large, small): (Vec<String>, Vec<String>) = upserts.iter().cloned().partition(|p| is_large(&ws.abs(p)));
    let mut ids: HashMap<String, Id> = HashMap::new();
    let encoded = ws.encode_files(&small)?;
    for (p, (id, _)) in small.iter().zip(&encoded) {
        ids.insert(p.clone(), *id);
    }
    // A small commit (an agent's usual edit) is sent whole: that costs less than a round trip
    // to ask which objects the server lacks, and the server keeps one copy of what it had.
    let mut uploaded = if encoded.iter().map(|(_, p)| p.len()).sum::<usize>() <= SEND_WITHOUT_ASKING {
        remote.objects().put_many(encoded.clone()).await?;
        encoded.len()
    } else {
        upload(remote, &encoded).await?
    };
    for p in &large {
        let (id, n) = upload_large(remote, &ws.abs(p)).await?;
        ids.insert(p.clone(), id);
        uploaded += n;
    }

    let file_changes = file_changes(&changes, &ids);
    let cs = changeset(ws, &file_changes, message, author);
    let created = remote.create_changeset(cs).await?;
    record(ws, &changes, &file_changes, created.id)?;
    Ok(Some(Committed { id: created.id, files: file_changes.len(), uploaded }))
}

/// `gd commit --land` in one round trip (protocol 1.2) for an agent's usual edit: a small change
/// on landed history, its files, the change and the land in one call. `None`: commit and land
/// the usual way instead (a server before 1.2, a larger change, or one the server refused,
/// whose refusal the usual way then reports, protected branches going to review included).
pub async fn run_and_land(
    remote: &dyn Remote,
    ws: &mut Workspace,
    message: &str,
    author: &str,
    pathspecs: &[String],
) -> Result<Option<(Committed, super::land::LandOutcome)>> {
    if message.trim().is_empty() || !(ws.base_landed || ws.base.is_none()) {
        return Ok(None);
    }
    let changes = ws.status(pathspecs)?;
    let upserts: Vec<String> =
        changes.iter().filter(|c| c.kind != ChangeKind::Deleted).map(|c| c.path.clone()).collect();
    if changes.is_empty() || upserts.iter().any(|p| is_large(&ws.abs(p))) {
        return Ok(None);
    }
    let encoded = ws.encode_files(&upserts)?;
    if encoded.len() > ONE_CALL_OBJECTS || encoded.iter().map(|(_, p)| p.len()).sum::<usize>() > ONE_CALL_BYTES {
        return Ok(None);
    }
    let ids: HashMap<String, Id> = upserts.iter().cloned().zip(encoded.iter().map(|(id, _)| *id)).collect();
    let file_changes = file_changes(&changes, &ids);
    let cs = changeset(ws, &file_changes, message, author);
    let sent = encoded.len();
    let Some((created, landed, rebased)) = remote.create_and_land(&ws.config.ref_name, encoded, cs).await? else {
        return Ok(None);
    };
    record(ws, &changes, &file_changes, created)?;
    let landed = super::land::settle(remote, ws, created, landed, rebased, 1).await?;
    Ok(Some((Committed { id: created, files: file_changes.len(), uploaded: sent }, landed)))
}

/// What a commit sends in one call at most (the server takes 1,024 objects and 4 MiB).
const ONE_CALL_OBJECTS: usize = 1024;
const ONE_CALL_BYTES: usize = 3 << 20;

fn file_changes(changes: &[LocalChange], ids: &HashMap<String, Id>) -> Vec<FileChange> {
    let mut file_changes = Vec::with_capacity(changes.len());
    for c in changes {
        if c.kind == ChangeKind::Deleted {
            file_changes.push(FileChange::delete(c.path.clone()));
        } else {
            let (mode, _) = c.new.expect("upsert has mode");
            file_changes.push(FileChange::upsert(c.path.clone(), mode, ids[&c.path]));
        }
    }
    // A file replaced by a directory of the same name (or the reverse) shows up as a delete
    // plus an upsert above or below it. The upsert alone already implies the replacement,
    // and sending both would be ambiguous, so drop the redundant delete.
    let upsert_paths: Vec<String> = file_changes.iter().filter(|c| !c.is_delete()).map(|c| c.path.clone()).collect();
    file_changes.retain(|c| !c.is_delete() || !upsert_paths.iter().any(|u| paths_overlap(u, &c.path)));
    file_changes
}

fn changeset(ws: &Workspace, file_changes: &[FileChange], message: &str, author: &str) -> Changeset {
    Changeset {
        parents: ws.base.map(|b| vec![b.0.to_vec()]).unwrap_or_default(),
        changes: file_changes.to_vec(),
        author: author.to_owned(),
        message: message.to_owned(),
        timestamp_ms: crate::now_ms(),
        git: None,
    }
}

/// The working copy after `created` exists: its files are clean, and it's the new base.
fn record(ws: &mut Workspace, changes: &[LocalChange], file_changes: &[FileChange], created: Id) -> Result<()> {
    for c in changes.iter().filter(|c| c.kind == ChangeKind::Deleted) {
        ws.index.remove(&c.path);
    }
    for c in file_changes {
        if let Some(id) = c.blob_id()? {
            ws.mark_clean(&c.path, grovedock_core::object::Mode::parse(c.mode)?, id)?;
        }
    }
    // On landed history (or the first commit): it's the only change `gd land` has to send.
    ws.parent_landed = ws.base_landed || ws.base.is_none();
    ws.base = Some(created);
    ws.base_landed = false;
    ws.save()
}

/// Chunk a large file on a blocking thread and upload chunks as they are produced, then
/// the manifest (after all of its chunks, so the server can verify it). Memory stays at
/// a few chunks plus one upload batch, whatever the file size. Returns the file's id and
/// the number of objects sent.
pub async fn upload_large(remote: &dyn Remote, abs: &std::path::Path) -> Result<(Id, usize)> {
    let file = std::fs::File::open(abs)?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Id, Bytes)>(8);
    let chunker = tokio::task::spawn_blocking(move || {
        grovedock_core::chunked::chunk_reader(std::io::BufReader::with_capacity(1 << 20, file), |id, payload| {
            tx.blocking_send((id, payload)).map_err(|_| grovedock_core::Error::invalid("upload stopped"))
        })
    });
    let (mut batch, mut size, mut sent) = (Vec::new(), 0, 0);
    while let Some(item) = rx.recv().await {
        size += item.1.len();
        batch.push(item);
        if size >= UPLOAD_BATCH_BYTES {
            sent += upload(remote, &std::mem::take(&mut batch)).await?;
            size = 0;
        }
    }
    sent += upload(remote, &batch).await?;
    let (id, manifest) = chunker.await??;
    sent += upload(remote, &[(id, manifest)]).await?;
    Ok((id, sent))
}

/// A commit's files this small go without first asking what the server lacks.
const SEND_WITHOUT_ASKING: usize = 64 << 10;

/// Upload only what the server is missing, in bounded batches.
pub async fn upload(remote: &dyn Remote, objects: &[(Id, Bytes)]) -> Result<usize> {
    if objects.is_empty() {
        return Ok(0);
    }
    let store = remote.objects();
    let ids: Vec<Id> = objects.iter().map(|(i, _)| *i).collect();
    // Principals with path-scoped read access may not probe object existence; they upload
    // everything and the server deduplicates.
    let missing: std::collections::HashSet<Id> = match store.missing(&ids).await {
        Ok(m) => m.into_iter().collect(),
        Err(grovedock_core::Error::PermissionDenied(_)) => ids.iter().copied().collect(),
        Err(e) => return Err(e.into()),
    };
    let mut batch = Vec::new();
    let (mut size, mut sent) = (0, 0);
    for (id, payload) in objects.iter().filter(|(i, _)| missing.contains(i)) {
        size += payload.len();
        batch.push((*id, payload.clone()));
        if size >= UPLOAD_BATCH_BYTES {
            sent += batch.len();
            store.put_many(std::mem::take(&mut batch)).await?;
            size = 0;
        }
    }
    sent += batch.len();
    store.put_many(batch).await?;
    Ok(sent)
}
