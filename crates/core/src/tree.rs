//! Merkle tree operations. Both `apply_changes` and `diff_trees` only descend into
//! subtrees whose ids differ, so cost scales with the size of the change, never with
//! the size of the repository.

use std::collections::BTreeMap;

use futures::future::BoxFuture;
use prost::Message;

use crate::object::{FileChange, Kind, Mode, Tree, TreeEntry, encode_object};
use crate::path::{components, components_or_root, validate_name};
use crate::ports::ObjectStore;
use crate::{Error, Id, Result};

pub async fn put_blob(store: &dyn ObjectStore, content: &[u8]) -> Result<Id> {
    let (id, payload) = encode_object(Kind::Blob, content);
    store.put(id, payload).await?;
    Ok(id)
}

pub async fn read_object(store: &dyn ObjectStore, id: &Id, expect: Kind) -> Result<Vec<u8>> {
    let payload = store.get(id).await?.ok_or_else(|| Error::not_found(format!("object {id}")))?;
    // Large files are stored as chunk manifests; reassemble (bounded) transparently.
    let (kind, content) = crate::chunked::read_content(store, id, &payload).await?;
    if kind != expect {
        return Err(Error::Corrupt(format!("{id}: expected {expect:?}, found {kind:?}")));
    }
    Ok(content)
}

/// Decode and validate a tree: names valid, strictly sorted, ids well formed.
pub fn parse_tree(id: &Id, content: &[u8]) -> Result<Tree> {
    let tree = Tree::decode(content).map_err(|e| Error::Corrupt(format!("tree {id}: {e}")))?;
    for (i, e) in tree.entries.iter().enumerate() {
        validate_name(&e.name).map_err(|_| Error::Corrupt(format!("tree {id}: bad name {:?}", e.name)))?;
        Mode::parse(e.mode).map_err(|_| Error::Corrupt(format!("tree {id}: bad mode")))?;
        Id::from_slice(&e.id).map_err(|_| Error::Corrupt(format!("tree {id}: bad entry id")))?;
        if i > 0 && tree.entries[i - 1].name >= e.name {
            return Err(Error::Corrupt(format!("tree {id}: entries not strictly sorted")));
        }
    }
    Ok(tree)
}

pub async fn read_tree(store: &dyn ObjectStore, id: &Id) -> Result<Tree> {
    parse_tree(id, &read_object(store, id, Kind::Tree).await?)
}

type Entries = BTreeMap<String, (Mode, Id)>;

fn to_entries(tree: Tree) -> Result<Entries> {
    tree.entries.into_iter().map(|e| Ok((e.name, (Mode::parse(e.mode)?, Id::from_slice(&e.id)?)))).collect()
}

pub fn encode_tree(entries: &Entries) -> (Id, bytes::Bytes) {
    let tree = Tree {
        entries: entries
            .iter()
            .map(|(name, (mode, id))| TreeEntry { name: name.clone(), mode: *mode as i32, id: id.0.to_vec() })
            .collect(),
    };
    encode_object(Kind::Tree, &tree.encode_to_vec())
}

async fn load_entries(store: &dyn ObjectStore, id: Option<Id>) -> Result<Entries> {
    match id {
        Some(id) => to_entries(read_tree(store, &id).await?),
        None => Ok(Entries::new()),
    }
}

/// The id of the empty root tree.
pub fn empty_tree() -> (Id, bytes::Bytes) {
    encode_tree(&Entries::new())
}

type Pending = Vec<(Vec<String>, Option<(Mode, Id)>)>;

/// Apply file changes to `base` (None = empty) and return the new root tree id.
/// Callers must reject changesets where one path is a directory prefix of another.
pub async fn apply_changes(store: &dyn ObjectStore, base: Option<Id>, changes: &[FileChange]) -> Result<Id> {
    let mut pending: Pending = Vec::with_capacity(changes.len());
    for c in changes {
        let parts = components(&c.path)?.into_iter().map(str::to_owned).collect();
        let target = match c.blob_id()? {
            None => None,
            Some(id) => {
                let mode = Mode::parse(c.mode)?;
                if mode == Mode::Dir {
                    return Err(Error::invalid(format!("{}: file change cannot have dir mode", c.path)));
                }
                Some((mode, id))
            }
        };
        pending.push((parts, target));
    }
    match apply(store, base, pending).await? {
        Some(id) => Ok(id),
        None => {
            let (id, payload) = empty_tree();
            store.put(id, payload).await?;
            Ok(id)
        }
    }
}

fn apply(store: &dyn ObjectStore, tree: Option<Id>, changes: Pending) -> BoxFuture<'_, Result<Option<Id>>> {
    Box::pin(async move {
        let mut entries = load_entries(store, tree).await?;
        let mut sub: BTreeMap<String, Pending> = BTreeMap::new();
        for (mut parts, target) in changes {
            if parts.len() == 1 {
                let name = parts.pop().expect("len 1");
                match target {
                    Some(t) => entries.insert(name, t),
                    None => entries.remove(&name),
                };
            } else {
                let head = parts.remove(0);
                sub.entry(head).or_default().push((parts, target));
            }
        }
        // Independent subtrees are rebuilt concurrently.
        let names: Vec<String> = sub.keys().cloned().collect();
        let results = futures::future::try_join_all(sub.into_iter().map(|(name, changes)| {
            let existing = match entries.get(&name) {
                Some((Mode::Dir, id)) => Some(*id),
                _ => None,
            };
            apply(store, existing, changes)
        }))
        .await?;
        for (name, result) in names.into_iter().zip(results) {
            match result {
                Some(id) => entries.insert(name, (Mode::Dir, id)),
                None => entries.remove(&name),
            };
        }
        if entries.is_empty() {
            return Ok(None);
        }
        let (id, payload) = encode_tree(&entries);
        store.put(id, payload).await?;
        Ok(Some(id))
    })
}

/// Minimal list of file changes turning tree `from` into tree `to`, in depth-first tree
/// order with unique paths.
/// Invariant (property-tested): `apply_changes(from, diff_trees(from, to)) == to`.
/// A file replacing a directory (or vice versa) is a single upsert/addition set; the
/// replaced subtree is implied.
pub async fn diff_trees(store: &dyn ObjectStore, from: Option<Id>, to: Option<Id>) -> Result<Vec<FileChange>> {
    let mut out = Vec::new();
    diff(store, from, to, String::new(), &mut out).await?;
    Ok(out)
}

fn diff<'a>(
    store: &'a dyn ObjectStore,
    a: Option<Id>,
    b: Option<Id>,
    prefix: String,
    out: &'a mut Vec<FileChange>,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        if a == b {
            return Ok(());
        }
        let (ea, eb) = futures::try_join!(load_entries(store, a), load_entries(store, b))?;
        let mut names: Vec<&String> = ea.keys().chain(eb.keys()).collect();
        names.sort();
        names.dedup();
        for name in names {
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            match (ea.get(name), eb.get(name)) {
                (x, y) if x == y => {}
                (Some((Mode::Dir, x)), Some((Mode::Dir, y))) => diff(store, Some(*x), Some(*y), path, out).await?,
                (_, Some((Mode::Dir, y))) => diff(store, None, Some(*y), path, out).await?,
                (_, Some((mode, id))) => out.push(FileChange::upsert(path, *mode, *id)),
                (Some((Mode::Dir, x)), None) => diff(store, Some(*x), None, path, out).await?,
                (Some(_), None) => out.push(FileChange::delete(path)),
                (None, None) => unreachable!("name came from one side"),
            }
        }
        Ok(())
    })
}

/// Look up `path` ("" = root) under `root`. Returns (mode, id) or None.
pub async fn lookup(store: &dyn ObjectStore, root: Id, path: &str) -> Result<Option<(Mode, Id)>> {
    let mut cur = (Mode::Dir, root);
    for part in components_or_root(path)? {
        if cur.0 != Mode::Dir {
            return Ok(None);
        }
        match to_entries(read_tree(store, &cur.1).await?)?.remove(part) {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(Some(cur))
}

/// The entry at each prefix of `path` under `root`: element k is the entry for the first
/// k+1 components, None from the first missing component or non-directory on.
pub async fn trail(store: &dyn ObjectStore, root: Id, path: &str) -> Result<Vec<Option<(Mode, Id)>>> {
    let parts = components_or_root(path)?;
    let mut out = Vec::with_capacity(parts.len());
    let mut cur = Some((Mode::Dir, root));
    for part in parts {
        cur = match cur {
            Some((Mode::Dir, id)) => to_entries(read_tree(store, &id).await?)?.remove(part),
            _ => None,
        };
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use bytes::Bytes;
    use proptest::prelude::*;

    use super::*;

    #[derive(Default)]
    struct Mem(Mutex<HashMap<Id, Bytes>>);
    #[async_trait]
    impl ObjectStore for Mem {
        async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
            Ok(self.0.lock().unwrap().get(id).cloned())
        }
        async fn put(&self, id: Id, p: Bytes) -> Result<()> {
            self.0.lock().unwrap().insert(id, p);
            Ok(())
        }
        async fn has(&self, id: &Id) -> Result<bool> {
            Ok(self.0.lock().unwrap().contains_key(id))
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().build().unwrap()
    }

    /// Materialise a flat path -> content map into a tree.
    async fn build(store: &Mem, files: &BTreeMap<String, u8>) -> Id {
        let mut changes = Vec::new();
        for (p, c) in files {
            changes.push(FileChange::upsert(p.clone(), Mode::File, put_blob(store, &[*c]).await.unwrap()));
        }
        apply_changes(store, None, &changes).await.unwrap()
    }

    #[test]
    fn apply_then_lookup_and_delete_prunes_empty_dirs() {
        rt().block_on(async {
            let s = Mem::default();
            let blob = put_blob(&s, b"x").await.unwrap();
            let root = apply_changes(&s, None, &[FileChange::upsert("a/b/c.txt", Mode::File, blob)]).await.unwrap();
            assert_eq!(lookup(&s, root, "a/b/c.txt").await.unwrap(), Some((Mode::File, blob)));
            assert_eq!(lookup(&s, root, "a/b/c.txt/x").await.unwrap(), None);
            let root2 = apply_changes(&s, Some(root), &[FileChange::delete("a/b/c.txt")]).await.unwrap();
            assert_eq!(root2, empty_tree().0);
            assert_eq!(lookup(&s, root2, "a").await.unwrap(), None);
        });
    }

    #[test]
    fn file_dir_replacement_roundtrips() {
        rt().block_on(async {
            let s = Mem::default();
            let a = build(&s, &BTreeMap::from([("x".into(), 1), ("y/z".into(), 2)])).await;
            let b = build(&s, &BTreeMap::from([("x/inner".into(), 3), ("y".into(), 4)])).await;
            for (from, to) in [(a, b), (b, a)] {
                let d = diff_trees(&s, Some(from), Some(to)).await.unwrap();
                assert_eq!(apply_changes(&s, Some(from), &d).await.unwrap(), to);
            }
        });
    }

    #[test]
    fn corrupt_tree_rejected() {
        let unsorted = Tree {
            entries: vec![
                TreeEntry { name: "b".into(), mode: 0, id: vec![0; 32] },
                TreeEntry { name: "a".into(), mode: 0, id: vec![0; 32] },
            ],
        };
        assert!(parse_tree(&Id([0; 32]), &unsorted.encode_to_vec()).is_err());
        let traversal = Tree { entries: vec![TreeEntry { name: "..".into(), mode: 0, id: vec![0; 32] }] };
        assert!(parse_tree(&Id([0; 32]), &traversal.encode_to_vec()).is_err());
    }

    fn arb_files() -> impl Strategy<Value = BTreeMap<String, u8>> {
        // Small alphabet + shallow depth maximises file/dir collisions.
        proptest::collection::btree_map("[ab]{1,2}(/[ab]{1,2}){0,2}", 0u8..4, 0..12).prop_map(|m| {
            // Drop paths that are a directory prefix of another (not representable).
            let keys: Vec<String> = m.keys().cloned().collect();
            m.into_iter().filter(|(k, _)| !keys.iter().any(|o| o.starts_with(&format!("{k}/")))).collect()
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn diff_apply_roundtrip(a in arb_files(), b in arb_files()) {
            rt().block_on(async {
                let s = Mem::default();
                let (ta, tb) = (build(&s, &a).await, build(&s, &b).await);
                let d = diff_trees(&s, Some(ta), Some(tb)).await.unwrap();
                assert_eq!(apply_changes(&s, Some(ta), &d).await.unwrap(), tb);
                // Canonical: identical content => identical root id; diff of equal trees is empty.
                assert_eq!(build(&s, &b).await, tb);
                assert!(diff_trees(&s, Some(tb), Some(tb)).await.unwrap().is_empty());
                // Unique paths.
                let mut paths: Vec<_> = d.iter().map(|c| c.path.clone()).collect();
                let n = paths.len();
                paths.sort();
                paths.dedup();
                assert_eq!(paths.len(), n);
            });
        }
    }
}
