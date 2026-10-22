//! Working copy: on-disk state (`.gd/`), status scanning, sparse checkout and applying
//! remote changes. Every path written to disk is validated, and no write ever follows a
//! symlink, so a hostile tree cannot escape the working copy.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt, stream};
use grovedock_core::chunked::{CHUNK_THRESHOLD, is_manifest};
use grovedock_core::object::{FileChange, Kind, Mode, decode_object, encode_object};
use grovedock_core::path::{LEGACY_META_DIR, META_DIR, components};
use grovedock_core::ports::ObjectStore;
use grovedock_core::{Id, tree};
use prost::Message;
use serde::{Deserialize, Serialize};

/// Objects requested per round trip during checkout.
const FETCH_BATCH: usize = 512;
/// Concurrent fetch batches in flight.
const FETCH_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub server: String,
    #[serde(default)]
    pub objects: Option<String>,
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// Sparse prefixes; empty = whole repository.
    #[serde(default)]
    pub sparse: Vec<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct IndexEntry {
    #[prost(string, tag = "1")]
    pub path: String,
    #[prost(bytes = "vec", tag = "2")]
    pub blob: Vec<u8>,
    #[prost(int32, tag = "3")]
    pub mode: i32,
    #[prost(uint64, tag = "4")]
    pub size: u64,
    #[prost(int64, tag = "5")]
    pub mtime_ns: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
struct IndexFile {
    #[prost(bytes = "vec", tag = "1")]
    base: Vec<u8>,
    #[prost(int64, tag = "2")]
    written_ns: i64,
    #[prost(message, repeated, tag = "3")]
    entries: Vec<IndexEntry>,
    #[prost(bool, tag = "4")]
    base_landed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalChange {
    pub path: String,
    pub kind: ChangeKind,
    /// New (mode, blob id) for added/modified files.
    pub new: Option<(Mode, Id)>,
}

pub struct Workspace {
    pub root: PathBuf,
    pub config: Config,
    pub base: Option<Id>,
    /// True when `base` was landed (possibly rebased server-side), so syncing away from it
    /// cannot lose unlanded work.
    pub base_landed: bool,
    pub index: BTreeMap<String, IndexEntry>,
    written_ns: i64,
}

fn now_ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i64)
}

fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64)
}

#[cfg(unix)]
fn is_exec(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn is_exec(_: &fs::Metadata) -> bool {
    false
}

/// Read a working file as (mode, content). Symlinks are stored as their target path.
fn read_entry(abs: &Path) -> Result<(Mode, Vec<u8>, fs::Metadata)> {
    let meta = fs::symlink_metadata(abs).with_context(|| format!("stat {}", abs.display()))?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(abs)?;
        return Ok((Mode::Symlink, target.to_string_lossy().into_owned().into_bytes(), meta));
    }
    let content = fs::read(abs).with_context(|| format!("read {}", abs.display()))?;
    Ok((if is_exec(&meta) { Mode::Exec } else { Mode::File }, content, meta))
}

/// Blob id of a working file without loading it: files are hashed as a stream, so
/// status stays memory-flat for multi-gigabyte files.
fn hash_entry(abs: &Path) -> Result<(Mode, Id)> {
    let meta = fs::symlink_metadata(abs).with_context(|| format!("stat {}", abs.display()))?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(abs)?.to_string_lossy().into_owned();
        return Ok((Mode::Symlink, Id::hash(Kind::Blob as u8, target.as_bytes())));
    }
    let file = fs::File::open(abs).with_context(|| format!("read {}", abs.display()))?;
    let id = Id::hash_reader(Kind::Blob as u8, file)?;
    Ok((if is_exec(&meta) { Mode::Exec } else { Mode::File }, id))
}

/// Is `path` a regular file large enough to be stored as chunks?
pub fn is_large(abs: &Path) -> bool {
    fs::symlink_metadata(abs).is_ok_and(|m| m.is_file() && m.len() >= CHUNK_THRESHOLD)
}

/// A file being received from a path-scoped checkout.
struct PartialFile {
    path: String,
    mode: Mode,
    id: Id,
    tmp: PathBuf,
    /// Symlink target bytes (symlinks are not written through a temporary file).
    link: Vec<u8>,
    file: Option<fs::File>,
}

impl PartialFile {
    /// Verify the received content against its id, then move it into place.
    fn finish(&self, root: &Path) -> Result<()> {
        if self.mode == Mode::Symlink {
            if Id::hash(Kind::Blob as u8, &self.link) != self.id {
                bail!("{}: content does not match {}", self.path, self.id);
            }
            return write_file(root, &self.path, self.mode, &self.link);
        }
        if Id::hash_reader(Kind::Blob as u8, fs::File::open(&self.tmp)?)? != self.id {
            let _ = fs::remove_file(&self.tmp);
            bail!("{}: content does not match {}", self.path, self.id);
        }
        let abs = prepare_target(root, &self.path)?;
        fs::rename(&self.tmp, &abs)?;
        set_exec(&abs, self.mode)
    }
}

/// Is `path` covered by the sparse prefixes?
pub fn in_sparse(sparse: &[String], path: &str) -> bool {
    sparse.is_empty()
        || sparse.iter().any(|s| path == s || (path.starts_with(s.as_str()) && path.as_bytes()[s.len()] == b'/'))
}

/// Should checkout descend into directory `dir`?
fn dir_in_sparse(sparse: &[String], dir: &str) -> bool {
    in_sparse(sparse, dir) || sparse.iter().any(|s| s.starts_with(dir) && s.as_bytes().get(dir.len()) == Some(&b'/'))
}

impl Workspace {
    pub fn meta_dir(root: &Path) -> PathBuf {
        root.join(META_DIR)
    }

    /// Find the workspace containing `start` by walking up to a `.gd` directory. A workspace made
    /// before 4.0 (`.pando`) is renamed in place the first time.
    pub fn find(start: &Path) -> Result<Workspace> {
        let start = fs::canonicalize(start)?;
        let root = start
            .ancestors()
            .find(|d| Self::meta_dir(d).is_dir() || d.join(LEGACY_META_DIR).is_dir())
            .with_context(|| format!("not a gd workspace (no {META_DIR}/ in {} or parents)", start.display()))?;
        let (old, new) = (root.join(LEGACY_META_DIR), Self::meta_dir(root));
        if !new.is_dir() {
            fs::rename(&old, &new).with_context(|| format!("renaming {} to {}", old.display(), new.display()))?;
            eprintln!("gd: upgraded this workspace ({LEGACY_META_DIR} is now {META_DIR})");
        }
        Self::open(root)
    }

    pub fn open(root: &Path) -> Result<Workspace> {
        let meta = Self::meta_dir(root);
        let config: Config =
            serde_json::from_slice(&fs::read(meta.join("config.json"))?).context("parsing config.json")?;
        let index = IndexFile::decode(&*fs::read(meta.join("index"))?).context("parsing index")?;
        let base = if index.base.is_empty() { None } else { Some(Id::from_slice(&index.base)?) };
        Ok(Workspace {
            root: root.to_owned(),
            config,
            base,
            base_landed: index.base_landed,
            index: index.entries.into_iter().map(|e| (e.path.clone(), e)).collect(),
            written_ns: index.written_ns,
        })
    }

    pub fn init(root: &Path, config: Config) -> Result<Workspace> {
        let meta = Self::meta_dir(root);
        if meta.exists() {
            bail!("{} already exists", meta.display());
        }
        fs::create_dir_all(&meta)?;
        let ws = Workspace {
            root: fs::canonicalize(root)?,
            config,
            base: None,
            base_landed: true,
            index: BTreeMap::new(),
            written_ns: 0,
        };
        ws.save()?;
        Ok(ws)
    }

    /// Atomic save: write temp files, then rename over the originals.
    pub fn save(&self) -> Result<()> {
        let meta = Self::meta_dir(&self.root);
        let write = |name: &str, data: &[u8]| -> Result<()> {
            let tmp = meta.join(format!("{name}.tmp"));
            fs::write(&tmp, data)?;
            fs::rename(&tmp, meta.join(name))?;
            Ok(())
        };
        write("config.json", &serde_json::to_vec_pretty(&self.config)?)?;
        let index = IndexFile {
            base: self.base.map(|b| b.0.to_vec()).unwrap_or_default(),
            written_ns: now_ns(),
            entries: self.index.values().cloned().collect(),
            base_landed: self.base_landed,
        };
        write("index", &index.encode_to_vec())
    }

    pub fn abs(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    fn index_entry(&self, path: &str, mode: Mode, id: Id) -> Result<IndexEntry> {
        let meta = fs::symlink_metadata(self.abs(path))?;
        Ok(IndexEntry {
            path: path.to_owned(),
            blob: id.0.to_vec(),
            mode: mode as i32,
            size: meta.len(),
            mtime_ns: mtime_ns(&meta),
        })
    }

    /// Record `path` as clean at (mode, id) using its current on-disk stat.
    pub fn mark_clean(&mut self, path: &str, mode: Mode, id: Id) -> Result<()> {
        let e = self.index_entry(path, mode, id)?;
        self.index.insert(path.to_owned(), e);
        Ok(())
    }

    /// Working-copy changes relative to the index, restricted to `pathspecs` when given.
    /// Unchanged stat (size + mtime older than the last index write) skips hashing;
    /// everything else is hashed on all cores.
    pub fn status(&self, pathspecs: &[String]) -> Result<Vec<LocalChange>> {
        let wanted = |p: &str| in_sparse(&self.config.sparse, p) && (pathspecs.is_empty() || in_sparse(pathspecs, p));
        let mut walker = ignore::WalkBuilder::new(&self.root);
        walker
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .parents(false)
            .require_git(false)
            // `.sapignore` and `.pandoignore` are old names, still read (deprecated).
            .add_custom_ignore_filename(".sapignore")
            .add_custom_ignore_filename(".pandoignore")
            .add_custom_ignore_filename(".gdignore");
        walker.filter_entry(|e| !matches!(e.file_name().to_str(), Some(META_DIR) | Some(".git")));

        let mut seen = Vec::new();
        for entry in walker.build() {
            let entry = entry?;
            if let Some(old @ (".sapignore" | ".pandoignore")) = entry.file_name().to_str() {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| eprintln!("warning: `{old}` is deprecated; rename it to `.gdignore`"));
            }
            let ft = match entry.file_type() {
                Some(ft) if ft.is_file() || ft.is_symlink() => ft,
                _ => continue,
            };
            let _ = ft;
            let rel = entry.path().strip_prefix(&self.root)?;
            let Some(rel) = rel.to_str().map(|s| s.replace(std::path::MAIN_SEPARATOR, "/")) else {
                eprintln!("warning: skipping non-UTF-8 path {}", entry.path().display());
                continue;
            };
            if components(&rel).is_err() {
                eprintln!("warning: skipping unsupported path `{rel}`");
                continue;
            }
            if wanted(&rel) {
                seen.push(rel);
            }
        }
        seen.sort();

        // Stat check on this thread; collect candidates that need hashing.
        let mut to_hash = Vec::new();
        for path in &seen {
            let meta = fs::symlink_metadata(self.abs(path))?;
            match self.index.get(path) {
                Some(e) if e.size == meta.len() && e.mtime_ns == mtime_ns(&meta) && e.mtime_ns < self.written_ns => {}
                _ => to_hash.push(path.clone()),
            }
        }
        let hashed = self.hash_files(&to_hash)?;

        let mut changes = Vec::new();
        for (path, (mode, id)) in to_hash.into_iter().zip(hashed) {
            match self.index.get(&path) {
                None => changes.push(LocalChange { path, kind: ChangeKind::Added, new: Some((mode, id)) }),
                Some(e) if e.blob != id.0 || e.mode != mode as i32 => {
                    changes.push(LocalChange { path, kind: ChangeKind::Modified, new: Some((mode, id)) })
                }
                Some(_) => {}
            }
        }
        let seen: std::collections::HashSet<&str> = seen.iter().map(String::as_str).collect();
        for path in self.index.keys() {
            if wanted(path) && !seen.contains(path.as_str()) {
                changes.push(LocalChange { path: path.clone(), kind: ChangeKind::Deleted, new: None });
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(changes)
    }

    /// Blob ids for `paths`, computed in parallel across all cores.
    fn hash_files(&self, paths: &[String]) -> Result<Vec<(Mode, Id)>> {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let chunk = paths.len().div_ceil(threads).max(1);
        std::thread::scope(|s| {
            let handles: Vec<_> = paths
                .chunks(chunk)
                .map(|part| {
                    s.spawn(move || -> Result<Vec<(Mode, Id)>> {
                        part.iter().map(|p| hash_entry(&self.abs(p))).collect()
                    })
                })
                .collect();
            let mut out = Vec::with_capacity(paths.len());
            for h in handles {
                out.extend(h.join().expect("hash thread panicked")?);
            }
            Ok(out)
        })
    }

    /// Compress small local files into object payloads (parallel), for upload. Large files
    /// (see [`is_large`]) are streamed by the commit path instead.
    pub fn encode_files(&self, paths: &[String]) -> Result<Vec<(Id, Bytes)>> {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let chunk = paths.len().div_ceil(threads).max(1);
        std::thread::scope(|s| {
            let handles: Vec<_> = paths
                .chunks(chunk)
                .map(|part| {
                    s.spawn(move || -> Result<Vec<(Id, Bytes)>> {
                        part.iter()
                            .map(|p| {
                                let (_, content, _) = read_entry(&self.abs(p))?;
                                Ok(encode_object(Kind::Blob, &content))
                            })
                            .collect()
                    })
                })
                .collect();
            let mut out = Vec::with_capacity(paths.len());
            for h in handles {
                out.extend(h.join().expect("encode thread panicked")?);
            }
            Ok(out)
        })
    }

    /// Materialise `root_tree` (restricted to the sparse set) into an empty workspace.
    pub async fn checkout(&mut self, objects: Arc<dyn ObjectStore>, base: Id, root_tree: Id) -> Result<usize> {
        let files = list_files(objects.as_ref(), root_tree, &self.config.sparse).await?;
        let changes: Vec<FileChange> = files.into_iter().map(|(p, (m, id))| FileChange::upsert(p, m, id)).collect();
        self.apply(objects, &changes).await?;
        self.base = Some(base);
        self.base_landed = true;
        self.save()?;
        Ok(self.index.len())
    }

    /// Apply remote file changes (inside the sparse set) to disk and the index.
    pub async fn apply(&mut self, objects: Arc<dyn ObjectStore>, changes: &[FileChange]) -> Result<()> {
        let changes: Vec<&FileChange> = changes.iter().filter(|c| in_sparse(&self.config.sparse, &c.path)).collect();
        for c in changes.iter().filter(|c| c.is_delete()) {
            remove_path(&self.root, &c.path)?;
            self.drop_index_overlapping(&c.path);
        }
        let upserts: Vec<(String, Mode, Id)> = changes
            .iter()
            .filter_map(|c| Some((c.path.clone(), Mode::parse(c.mode).ok()?, c.blob_id().ok()??)))
            .collect();
        if upserts.len() != changes.iter().filter(|c| !c.is_delete()).count() {
            bail!("server sent a malformed change");
        }
        let root = self.root.clone();
        let batches: Vec<Vec<(String, Mode, Id)>> = upserts.chunks(FETCH_BATCH).map(|c| c.to_vec()).collect();
        let written: Vec<(String, Mode, Id)> = stream::iter(batches)
            .map(move |batch| {
                let (objects, root) = (objects.clone(), root.clone());
                async move {
                    let ids: Vec<Id> = batch.iter().map(|(_, _, id)| *id).collect();
                    let payloads = objects.get_many(&ids).await?;
                    let mut small = Vec::new();
                    let mut large = Vec::new();
                    for (item, payload) in batch.iter().cloned().zip(payloads) {
                        let payload =
                            payload.with_context(|| format!("object {} for `{}` not found", item.2, item.0))?;
                        if is_manifest(&payload) { large.push((item, payload)) } else { small.push((item, payload)) }
                    }
                    let root2 = root.clone();
                    tokio::task::spawn_blocking(move || -> Result<()> {
                        for ((path, mode, id), payload) in &small {
                            let (kind, content) = decode_object(id, payload)?;
                            if kind != Kind::Blob {
                                bail!("{path}: expected blob");
                            }
                            write_file(&root2, path, *mode, &content)?;
                        }
                        Ok(())
                    })
                    .await??;
                    for ((path, mode, id), payload) in &large {
                        write_large_file(objects.as_ref(), &root, path, *mode, id, payload).await?;
                    }
                    Ok::<_, anyhow::Error>(batch)
                }
            })
            .buffer_unordered(FETCH_CONCURRENCY)
            .try_concat()
            .await?;
        for (path, mode, id) in written {
            self.drop_index_overlapping(&path);
            self.mark_clean(&path, mode, id)?;
        }
        Ok(())
    }

    /// Materialise `paths` at `rev` through the path-scoped checkout stream (how principals
    /// with restricted read access fetch content). Each file is written to a temporary
    /// file, verified against its id, then moved into place.
    pub async fn checkout_scoped(
        &mut self,
        remote: &dyn crate::remote::Remote,
        rev: &str,
        paths: &[String],
    ) -> Result<usize> {
        let root = self.root.clone();
        let mut written: Vec<(String, Mode, Id)> = Vec::new();
        let mut open: Option<PartialFile> = None;
        let mut sink = |p: crate::remote::Piece| -> Result<()> {
            if open.as_ref().is_none_or(|o| o.path != p.path) {
                let abs = ensure_parents(&root, &p.path)?;
                let tmp = abs.with_file_name(format!(".gd-partial-{}", p.id.short()));
                let file = if p.mode == Mode::Symlink { None } else { Some(fs::File::create(&tmp)?) };
                open = Some(PartialFile { path: p.path.clone(), mode: p.mode, id: p.id, tmp, link: Vec::new(), file });
            }
            let o = open.as_mut().expect("opened above");
            match &mut o.file {
                Some(f) => std::io::Write::write_all(f, &p.data)?,
                None => o.link.extend_from_slice(&p.data),
            }
            if p.last {
                let o = open.take().expect("open file");
                o.finish(&root)?;
                written.push((o.path, o.mode, o.id));
            }
            Ok(())
        };
        remote.checkout(rev, paths, &mut sink).await?;
        let n = written.len();
        for (path, mode, id) in written {
            self.drop_index_overlapping(&path);
            self.mark_clean(&path, mode, id)?;
        }
        Ok(n)
    }

    /// Remove index entries at `path` or nested under/above it (file <-> dir replacement).
    fn drop_index_overlapping(&mut self, path: &str) {
        self.index.retain(|p, _| !grovedock_core::paths_overlap(p, path));
    }
}

/// All files under `root_tree` within `sparse`, fetching one tree level per round trip.
pub async fn list_files(
    objects: &dyn ObjectStore,
    root_tree: Id,
    sparse: &[String],
) -> Result<Vec<(String, (Mode, Id))>> {
    let mut level = vec![(String::new(), root_tree)];
    let mut files = Vec::new();
    while !level.is_empty() {
        let mut next = Vec::new();
        for chunk in level.chunks(FETCH_BATCH) {
            let ids: Vec<Id> = chunk.iter().map(|(_, id)| *id).collect();
            let payloads = objects.get_many(&ids).await?;
            for ((prefix, id), payload) in chunk.iter().zip(payloads) {
                let payload = payload.with_context(|| format!("tree {id} not found"))?;
                let (kind, content) = decode_object(id, &payload)?;
                if kind != Kind::Tree {
                    bail!("{id}: expected tree");
                }
                for e in tree::parse_tree(id, &content)?.entries {
                    let path = if prefix.is_empty() { e.name } else { format!("{prefix}/{}", e.name) };
                    let (mode, eid) = (Mode::parse(e.mode)?, Id::from_slice(&e.id)?);
                    if mode == Mode::Dir {
                        if dir_in_sparse(sparse, &path) {
                            next.push((path, eid));
                        }
                    } else if in_sparse(sparse, &path) {
                        files.push((path, (mode, eid)));
                    }
                }
            }
        }
        level = next;
    }
    Ok(files)
}

/// Create every parent directory of `rel` under `root`, replacing any file or symlink
/// in the way. Never traverses a symlink.
fn ensure_parents(root: &Path, rel: &str) -> Result<PathBuf> {
    let parts = components(rel)?;
    let mut cur = root.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                fs::remove_file(&cur)?;
                create_dir_once(&cur)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => create_dir_once(&cur)?,
            Err(e) => return Err(e.into()),
        }
    }
    cur.push(parts[parts.len() - 1]);
    Ok(cur)
}

/// Create `dir`, tolerating a parallel writer that created it first. What exists must be
/// a real directory: a symlink or file that appeared in between is an error, never followed.
fn create_dir_once(dir: &Path) -> Result<()> {
    match fs::create_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && fs::symlink_metadata(dir)?.is_dir() => Ok(()),
        r => Ok(r?),
    }
}

/// Create parents and clear whatever occupies `rel` (file, symlink or directory) so the
/// new file is created fresh: nothing is ever written through an existing symlink.
fn prepare_target(root: &Path, rel: &str) -> Result<PathBuf> {
    let abs = ensure_parents(root, rel)?;
    match fs::symlink_metadata(&abs) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(&abs)?,
        Ok(_) => fs::remove_file(&abs)?,
        Err(_) => {}
    }
    Ok(abs)
}

#[cfg(unix)]
fn set_exec(abs: &Path, mode: Mode) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if mode == Mode::Exec {
        fs::set_permissions(abs, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}
#[cfg(not(unix))]
fn set_exec(_: &Path, _: Mode) -> Result<()> {
    Ok(())
}

/// Reassemble a chunked file into a temporary file beside its destination, verifying every
/// chunk and the whole-file id, then move it into place. Memory stays at one chunk batch.
async fn write_large_file(
    objects: &dyn ObjectStore,
    root: &Path,
    rel: &str,
    mode: Mode,
    id: &Id,
    manifest: &[u8],
) -> Result<()> {
    if mode == Mode::Symlink || mode == Mode::Dir {
        bail!("{rel}: a chunked object must be a regular file");
    }
    let abs = ensure_parents(root, rel)?;
    let tmp = abs.with_file_name(format!(".gd-partial-{}", id.short()));
    let result = async {
        let mut out = std::io::BufWriter::with_capacity(1 << 20, fs::File::create(&tmp)?);
        grovedock_core::chunked::write_content(objects, id, manifest, &mut out).await?;
        std::io::Write::flush(&mut out)?;
        drop(out);
        let abs = prepare_target(root, rel)?;
        fs::rename(&tmp, &abs)?;
        set_exec(&abs, mode)
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn write_file(root: &Path, rel: &str, mode: Mode, content: &[u8]) -> Result<()> {
    let abs = prepare_target(root, rel)?;
    match mode {
        Mode::Symlink => {
            let target = std::str::from_utf8(content).context("symlink target is not UTF-8")?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &abs)?;
            #[cfg(not(unix))]
            fs::write(&abs, target)?;
        }
        Mode::File | Mode::Exec => {
            fs::write(&abs, content)?;
            set_exec(&abs, mode)?;
        }
        Mode::Dir => bail!("{rel}: cannot write a directory as a file"),
    }
    Ok(())
}

/// Remove a file (or replaced directory) and prune now-empty parents.
pub fn remove_path(root: &Path, rel: &str) -> Result<()> {
    components(rel)?;
    let abs = root.join(rel);
    match fs::symlink_metadata(&abs) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(&abs)?,
        Ok(_) => fs::remove_file(&abs)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    let mut dir = abs.parent();
    while let Some(d) = dir {
        if d == root || fs::remove_dir(d).is_err() {
            break; // not empty, or reached the root
        }
        dir = d.parent();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_matching() {
        let s = vec!["services/api".to_owned()];
        assert!(in_sparse(&s, "services/api"));
        assert!(in_sparse(&s, "services/api/main.rs"));
        assert!(!in_sparse(&s, "services/api2/x"));
        assert!(!in_sparse(&s, "services"));
        assert!(dir_in_sparse(&s, "services"));
        assert!(!dir_in_sparse(&s, "libs"));
        assert!(in_sparse(&[], "anything"));
    }

    /// Parallel checkout writers create the same new directories at the same time; none
    /// may fail because another created a directory first.
    #[test]
    fn parallel_writers_share_new_directories() {
        for _ in 0..20 {
            let root = tempfile::tempdir().unwrap();
            std::thread::scope(|s| {
                for i in 0..16 {
                    let root = root.path();
                    s.spawn(move || ensure_parents(root, &format!("t/lib/deep/f{i}")).unwrap());
                }
            });
            assert!(root.path().join("t/lib/deep").is_dir());
        }
    }

    #[cfg(unix)]
    #[test]
    fn writes_never_follow_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        // Step 1: tree contains symlink `a` -> outside dir.
        write_file(root.path(), "a", Mode::Symlink, outside.path().to_str().unwrap().as_bytes()).unwrap();
        // Step 2: a later change writes `a/pwned`. Must replace the link, not write outside.
        write_file(root.path(), "a/pwned", Mode::File, b"x").unwrap();
        assert!(!outside.path().join("pwned").exists());
        assert!(root.path().join("a").is_dir());
        // Existing symlink at the target itself is replaced, not written through.
        let victim = outside.path().join("victim");
        fs::write(&victim, b"safe").unwrap();
        write_file(root.path(), "link", Mode::Symlink, victim.to_str().unwrap().as_bytes()).unwrap();
        write_file(root.path(), "link", Mode::File, b"evil").unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"safe");
        assert!(write_file(root.path(), "../escape", Mode::File, b"x").is_err());
        assert!(write_file(root.path(), ".gd/config.json", Mode::File, b"x").is_err());
    }

    /// Regression (security review H3): a checkout never writes git hooks or config, nor
    /// gd's own config under another spelling, whatever the server sends.
    #[test]
    fn checkout_refuses_git_and_gd_dirs() {
        let root = tempfile::tempdir().unwrap();
        for bad in [
            ".git/hooks/post-checkout",
            ".GIT/config",
            "sub/.git/config",
            "GIT~1/config",
            ".git./config",
            ".GD/config.json",
        ] {
            assert!(write_file(root.path(), bad, Mode::Exec, b"#!/bin/sh\n").is_err(), "{bad}");
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0, "nothing was written");
        write_file(root.path(), ".gitignore", Mode::File, b"target\n").unwrap();
    }

    #[test]
    fn a_workspace_from_before_4_0_upgrades_itself() {
        let root = tempfile::tempdir().unwrap();
        let config =
            Config { server: "http://127.0.0.1:1".into(), objects: None, ref_name: "main".into(), sparse: vec![] };
        Workspace::init(root.path(), config).unwrap();
        std::fs::rename(root.path().join(META_DIR), root.path().join(LEGACY_META_DIR)).unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let ws = Workspace::find(&root.path().join("src")).unwrap();
        assert_eq!(ws.config.ref_name, "main");
        assert!(root.path().join(META_DIR).is_dir() && !root.path().join(LEGACY_META_DIR).exists());
    }

    #[test]
    fn honours_gdignore_and_the_old_names() {
        let root = tempfile::tempdir().unwrap();
        let config =
            Config { server: "http://127.0.0.1:1".into(), objects: None, ref_name: "main".into(), sparse: vec![] };
        let ws = Workspace::init(root.path(), config).unwrap();
        for (path, body) in
            [(".gdignore", "build\n"), (".sapignore", "*.log\n"), ("build/out", ""), ("a.log", ""), ("src.rs", "")]
        {
            write_file(root.path(), path, Mode::File, body.as_bytes()).unwrap();
        }
        let paths: Vec<String> = ws.status(&[]).unwrap().into_iter().map(|c| c.path).collect();
        assert_eq!(paths, [".gdignore", ".sapignore", "src.rs"]);
    }

    #[test]
    fn remove_prunes_empty_dirs() {
        let root = tempfile::tempdir().unwrap();
        write_file(root.path(), "a/b/c", Mode::File, b"x").unwrap();
        write_file(root.path(), "a/keep", Mode::File, b"y").unwrap();
        remove_path(root.path(), "a/b/c").unwrap();
        assert!(!root.path().join("a/b").exists());
        assert!(root.path().join("a/keep").exists());
        remove_path(root.path(), "missing").unwrap();
    }
}
