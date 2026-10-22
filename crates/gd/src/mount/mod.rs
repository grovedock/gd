//! Virtual checkout: a read-only view of one revision whose directories and files are
//! fetched on first access. [`VirtualTree`] holds all the logic and is platform-independent;
//! the FUSE adapter (Linux), the NFS adapter (macOS, or any Unix with `--serve-only`) and the
//! ProjFS adapter (Windows) only translate operating-system requests.
//!
//! * Directories are loaded when first looked up or listed.
//! * File sizes come from object headers or large-file manifests, without decompressing.
//! * Reads fetch only the chunks covering the requested byte range of a large file.
//! * Every object fetched is hash-verified; content is cached in a bounded LRU.

#[cfg(target_os = "linux")]
pub mod fuse;
#[cfg(unix)]
pub mod nfs;
#[cfg(windows)]
pub mod projfs;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use grovedock_core::Id;
use grovedock_core::chunked::{is_manifest, parse_manifest};
use grovedock_core::object::{Kind, Mode, content_size, decode_object};
use grovedock_core::ports::ObjectStore;
use grovedock_core::tree::parse_tree;
use quick_cache::Weighter;
use quick_cache::sync::Cache;

pub const ROOT: u64 = 1;

#[derive(Debug, Clone)]
pub struct Attr {
    pub ino: u64,
    pub mode: Mode,
    pub size: u64,
}

#[derive(Debug, Clone)]
struct Node {
    mode: Mode,
    id: Id,
    size: Option<u64>,
    /// name -> inode, once the directory has been loaded.
    children: Option<Vec<(String, u64)>>,
}

#[derive(Clone)]
struct ByteWeight;
impl Weighter<Id, Bytes> for ByteWeight {
    fn weight(&self, _: &Id, v: &Bytes) -> u64 {
        v.len() as u64 + 64
    }
}

pub struct VirtualTree {
    objects: Arc<dyn ObjectStore>,
    rt: tokio::runtime::Handle,
    nodes: Mutex<HashMap<u64, Node>>,
    next_ino: Mutex<u64>,
    /// Decompressed, verified blob contents (whole small files and large-file chunks).
    content: Cache<Id, Bytes, ByteWeight>,
}

impl VirtualTree {
    /// `rt` runs fetches; callers (FUSE worker threads) must not be runtime threads.
    pub fn new(objects: Arc<dyn ObjectStore>, rt: tokio::runtime::Handle, root_tree: Id, cache_bytes: u64) -> Self {
        let root = Node { mode: Mode::Dir, id: root_tree, size: Some(0), children: None };
        Self {
            objects,
            rt,
            nodes: Mutex::new(HashMap::from([(ROOT, root)])),
            next_ino: Mutex::new(ROOT + 1),
            content: Cache::with_weighter(4096, cache_bytes, ByteWeight),
        }
    }

    fn fetch(&self, id: &Id) -> Result<Bytes> {
        self.rt.block_on(self.objects.get(id))?.with_context(|| format!("object {id} not found"))
    }

    fn node(&self, ino: u64) -> Result<Node> {
        self.nodes.lock().expect("nodes").get(&ino).cloned().context("stale inode")
    }

    fn load_dir(&self, ino: u64) -> Result<Vec<(String, u64)>> {
        let node = self.node(ino)?;
        if node.mode != Mode::Dir {
            bail!("not a directory");
        }
        if let Some(c) = node.children {
            return Ok(c);
        }
        let payload = self.fetch(&node.id)?;
        let (_, content) = decode_object(&node.id, &payload)?;
        let tree = parse_tree(&node.id, &content)?;
        let mut nodes = self.nodes.lock().expect("nodes");
        // Another thread may have loaded it meanwhile.
        if let Some(c) = nodes.get(&ino).and_then(|n| n.children.clone()) {
            return Ok(c);
        }
        let mut next = self.next_ino.lock().expect("ino");
        let mut children = Vec::with_capacity(tree.entries.len());
        for e in tree.entries {
            let child = Node { mode: Mode::parse(e.mode)?, id: Id::from_slice(&e.id)?, size: None, children: None };
            let child_ino = *next;
            *next += 1;
            nodes.insert(child_ino, child);
            children.push((e.name, child_ino));
        }
        nodes.get_mut(&ino).expect("dir").children = Some(children.clone());
        Ok(children)
    }

    pub fn attr(&self, ino: u64) -> Result<Attr> {
        let node = self.node(ino)?;
        let size = match (node.mode, node.size) {
            (_, Some(s)) => s,
            (Mode::Dir, None) => 0,
            (_, None) => {
                let s = content_size(&self.fetch(&node.id)?)?;
                if let Some(n) = self.nodes.lock().expect("nodes").get_mut(&ino) {
                    n.size = Some(s);
                }
                s
            }
        };
        Ok(Attr { ino, mode: node.mode, size })
    }

    pub fn lookup(&self, parent: u64, name: &str) -> Result<Option<Attr>> {
        match self.load_dir(parent)?.into_iter().find(|(n, _)| n == name) {
            Some((_, ino)) => Ok(Some(self.attr(ino)?)),
            None => Ok(None),
        }
    }

    /// (inode, name, mode) for each entry; modes need no fetch.
    pub fn readdir(&self, ino: u64) -> Result<Vec<(u64, String, Mode)>> {
        let children = self.load_dir(ino)?;
        let nodes = self.nodes.lock().expect("nodes");
        Ok(children.into_iter().map(|(name, i)| (i, name, nodes[&i].mode)).collect())
    }

    fn blob(&self, id: &Id) -> Result<Bytes> {
        if let Some(b) = self.content.get(id) {
            return Ok(b);
        }
        let payload = self.fetch(id)?;
        let (kind, data) = decode_object(id, &payload)?;
        if kind != Kind::Blob {
            bail!("{id} is not a blob");
        }
        let data = Bytes::from(data);
        self.content.insert(*id, data.clone());
        Ok(data)
    }

    /// Bytes `[offset, offset + len)` of a file (or a symlink's target), fetching only the
    /// chunks of a large file that overlap the range.
    pub fn read(&self, ino: u64, offset: u64, len: u32) -> Result<Vec<u8>> {
        let node = self.node(ino)?;
        if node.mode == Mode::Dir {
            bail!("is a directory");
        }
        let end = offset.saturating_add(len as u64);
        if let Some(b) = self.content.get(&node.id) {
            return Ok(slice(&b, offset, end));
        }
        let payload = self.fetch(&node.id)?;
        if !is_manifest(&payload) {
            return Ok(slice(&self.blob(&node.id)?, offset, end));
        }
        let m = parse_manifest(&payload)?;
        let mut out = Vec::with_capacity(len as usize);
        let mut pos = 0u64;
        for c in &m.chunks {
            let (start, stop) = (pos, pos + c.len);
            pos = stop;
            if stop <= offset || start >= end {
                continue;
            }
            let data = self.blob(&Id::from_slice(&c.id)?)?;
            if data.len() as u64 != c.len {
                bail!("chunk length does not match its manifest");
            }
            out.extend_from_slice(&slice(&data, offset.saturating_sub(start), end - start));
            if stop >= end {
                break;
            }
        }
        Ok(out)
    }

    pub fn readlink(&self, ino: u64) -> Result<Vec<u8>> {
        let node = self.node(ino)?;
        if node.mode != Mode::Symlink {
            bail!("not a symlink");
        }
        Ok(self.blob(&node.id)?.to_vec())
    }
}

fn slice(data: &[u8], from: u64, to: u64) -> Vec<u8> {
    let len = data.len() as u64;
    let (from, to) = (from.min(len) as usize, to.min(len) as usize);
    data[from..to].to_vec()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use grovedock_adapters::memory::MemoryStore;
    use grovedock_core::object::{FileChange, encode_object};
    use grovedock_core::tree::{apply_changes, put_blob};

    use super::*;

    fn noise(n: usize) -> Vec<u8> {
        let mut x = 99u64;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 56) as u8
            })
            .collect()
    }

    #[test]
    fn browse_and_read_lazily() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let store = Arc::new(MemoryStore::new());
        let big = noise(12 << 20);
        let root = rt.block_on(async {
            let mut changes = Vec::new();
            for (path, data) in BTreeMap::from([("a/small.txt", b"hello".to_vec()), ("b.txt", b"b".to_vec())]) {
                changes.push(FileChange::upsert(path, Mode::File, put_blob(store.as_ref(), &data).await.unwrap()));
            }
            let mut objs = Vec::new();
            let (big_id, manifest) = grovedock_core::chunked::chunk_reader(&big[..], |id, p| {
                objs.push((id, p));
                Ok(())
            })
            .unwrap();
            objs.push((big_id, manifest));
            store.put_many(objs).await.unwrap();
            changes.push(FileChange::upsert("a/big.bin", Mode::Exec, big_id));
            let (link_id, link) = encode_object(Kind::Blob, b"small.txt");
            store.put(link_id, link).await.unwrap();
            changes.push(FileChange::upsert("a/link", Mode::Symlink, link_id));
            apply_changes(store.as_ref(), None, &changes).await.unwrap()
        });
        let vt = VirtualTree::new(store, rt.handle().clone(), root, 64 << 20);
        let names: Vec<String> = vt.readdir(ROOT).unwrap().into_iter().map(|(_, n, _)| n).collect();
        assert_eq!(names, vec!["a", "b.txt"]);
        let a = vt.lookup(ROOT, "a").unwrap().unwrap();
        assert_eq!(a.mode, Mode::Dir);
        assert!(vt.lookup(ROOT, "missing").unwrap().is_none());
        let small = vt.lookup(a.ino, "small.txt").unwrap().unwrap();
        assert_eq!(small.size, 5);
        assert_eq!(vt.read(small.ino, 1, 3).unwrap(), b"ell");
        assert_eq!(vt.read(small.ino, 10, 3).unwrap(), b"");
        let bigf = vt.lookup(a.ino, "big.bin").unwrap().unwrap();
        assert_eq!((bigf.size, bigf.mode), (big.len() as u64, Mode::Exec));
        // A range spanning chunk boundaries, read without fetching the whole file.
        let (off, len) = (5_000_000u64, 3_000_000u32);
        assert_eq!(vt.read(bigf.ino, off, len).unwrap(), &big[off as usize..off as usize + len as usize]);
        assert_eq!(vt.read(bigf.ino, big.len() as u64 - 10, 100).unwrap(), &big[big.len() - 10..]);
        let link = vt.lookup(a.ino, "link").unwrap().unwrap();
        assert_eq!(vt.readlink(link.ino).unwrap(), b"small.txt");
        assert!(vt.read(a.ino, 0, 1).is_err());
    }
}
