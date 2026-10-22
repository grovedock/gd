use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;

use crate::derived::{Derivation, Generation, GraphInfo, RootTree};
use crate::object::{Changeset, Mode};
use crate::path::components;
use crate::ports::{ChangesetLog, DerivedStore, ObjectStore, RefStore};
use crate::{Error, Id, Result, tree};

/// Hard cap on changes in one changeset, bounding server work per request.
pub const MAX_CHANGES: usize = 1_000_000;

/// Repository service: the shared domain kernel every feature slice is built on.
/// Holds only ports, so it is identical whether backed by memory, SQLite, Postgres or
/// cloud object storage.
#[derive(Clone)]
pub struct Repo {
    pub objects: Arc<dyn ObjectStore>,
    pub log: Arc<dyn ChangesetLog>,
    pub refs: Arc<dyn RefStore>,
    pub derived: Arc<dyn DerivedStore>,
}

/// True when two repo paths are equal or one is a directory prefix of the other.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    let nested = |x: &str, y: &str| x.len() > y.len() && x.starts_with(y) && x.as_bytes()[y.len()] == b'/';
    a == b || nested(a, b) || nested(b, a)
}

impl Repo {
    pub async fn changeset(&self, id: &Id) -> Result<Changeset> {
        let bytes = self.log.get(id).await?.ok_or_else(|| Error::not_found(format!("changeset {id}")))?;
        Changeset::decode_verified(id, &bytes)
    }

    /// Resolve a full hex changeset id or a ref name.
    pub async fn resolve(&self, rev: &str) -> Result<Id> {
        if rev.len() == 64
            && let Ok(id) = rev.parse::<Id>()
        {
            return match self.log.get(&id).await? {
                Some(_) => Ok(id),
                None => Err(Error::not_found(format!("changeset {id}"))),
            };
        }
        validate_ref_name(rev)?;
        self.refs.get(rev).await?.ok_or_else(|| Error::not_found(format!("ref `{rev}`")))
    }

    /// Validate, append, and eagerly derive. Cost is O(changes x depth), independent of
    /// repository size. Idempotent for identical input.
    pub async fn create_changeset(&self, cs: Changeset) -> Result<Id> {
        if cs.changes.len() > MAX_CHANGES {
            return Err(Error::invalid(format!("more than {MAX_CHANGES} changes")));
        }
        let parents = cs.parent_ids()?;
        let unique: HashSet<_> = parents.iter().collect();
        if unique.len() != parents.len() {
            return Err(Error::invalid("duplicate parent"));
        }
        for p in &parents {
            if self.log.get(p).await?.is_none() {
                return Err(Error::not_found(format!("parent {p}")));
            }
        }
        let mut paths = HashSet::with_capacity(cs.changes.len());
        let mut blobs = Vec::new();
        for c in &cs.changes {
            components(&c.path)?;
            if !paths.insert(c.path.as_str()) {
                return Err(Error::invalid(format!("duplicate path `{}`", c.path)));
            }
            if let Some(blob) = c.blob_id()? {
                if Mode::parse(c.mode)? == Mode::Dir {
                    return Err(Error::invalid(format!("{}: dir mode on file change", c.path)));
                }
                blobs.push(blob);
            }
        }
        // A path may not also be a directory of another changed path.
        for p in &paths {
            let mut cut = p.len();
            while let Some(i) = p[..cut].rfind('/') {
                if paths.contains(&p[..i]) {
                    return Err(Error::invalid(format!("`{}` is both a file and a directory", &p[..i])));
                }
                cut = i;
            }
        }
        let missing = self.objects.missing(&blobs).await?;
        if let Some(m) = missing.first() {
            return Err(Error::not_found(format!("blob {m} ({} missing; upload first)", missing.len())));
        }
        let (id, bytes) = cs.encode_canonical();
        self.log.append(id, bytes).await?;
        futures::try_join!(self.derive::<RootTree>(id), self.derive::<Generation>(id))?;
        Ok(id)
    }

    pub async fn root_tree(&self, id: Id) -> Result<Id> {
        self.derive::<RootTree>(id).await
    }

    pub async fn graph(&self, id: Id) -> Result<GraphInfo> {
        self.derive::<Generation>(id).await
    }

    /// Compute (or fetch) a derived value. Underived ancestors are processed iteratively
    /// in post-order, so arbitrarily deep histories never overflow the stack.
    pub async fn derive<D: Derivation>(&self, id: Id) -> Result<D::Value> {
        if let Some(v) = self.derived.get(D::KIND, &id).await? {
            return D::decode(&v);
        }
        let mut done: HashMap<Id, D::Value> = HashMap::new();
        let mut stack = vec![(id, false)];
        while let Some((cur, expanded)) = stack.pop() {
            if done.contains_key(&cur) {
                continue;
            }
            let cs = self.changeset(&cur).await?;
            let parents = cs.parent_ids()?;
            if !expanded {
                stack.push((cur, true));
                for p in parents {
                    if done.contains_key(&p) {
                        continue;
                    }
                    match self.derived.get(D::KIND, &p).await? {
                        Some(v) => {
                            done.insert(p, D::decode(&v)?);
                        }
                        None => stack.push((p, false)),
                    }
                }
            } else {
                let values: Vec<D::Value> = parents.iter().map(|p| done[p].clone()).collect();
                let v = D::compute(self, &cs, &values).await?;
                self.derived.put(D::KIND, cur, D::encode(&v).into()).await?;
                done.insert(cur, v);
            }
        }
        Ok(done.remove(&id).expect("derived target"))
    }

    /// First-parent ancestor of `id` at first-parent `depth`, in O(log n) jumps.
    pub async fn first_parent_ancestor_at(&self, id: Id, depth: u64) -> Result<Option<Id>> {
        let mut cur = id;
        let mut info = self.graph(cur).await?;
        if depth > info.depth {
            return Ok(None);
        }
        while info.depth > depth {
            cur = match (info.jump, info.parent) {
                (Some(j), _) if info.jump_depth >= depth => j,
                (_, Some(p)) => p,
                _ => return Ok(None),
            };
            info = self.graph(cur).await?;
        }
        Ok(Some(cur))
    }

    /// Is `a` an ancestor of (or equal to) `b`? O(log n) on first-parent history, with a
    /// generation-pruned walk as fallback for merge histories.
    pub async fn is_ancestor(&self, a: Id, b: Id) -> Result<bool> {
        let ga = self.graph(a).await?;
        if self.first_parent_ancestor_at(b, ga.depth).await? == Some(a) {
            return Ok(true);
        }
        let mut seen = HashSet::new();
        let mut heap = BinaryHeap::from([(self.graph(b).await?.generation, b)]);
        while let Some((g, cur)) = heap.pop() {
            if cur == a {
                return Ok(true);
            }
            if g <= ga.generation || !seen.insert(cur) {
                continue;
            }
            for p in self.changeset(&cur).await?.parent_ids()? {
                heap.push((self.graph(p).await?.generation, p));
            }
        }
        Ok(false)
    }

    /// Best common ancestor (highest generation). Nodes are popped in decreasing
    /// generation, so a node's paint is final when popped.
    pub async fn merge_base(&self, a: Id, b: Id) -> Result<Option<Id>> {
        if self.is_ancestor(a, b).await? {
            return Ok(Some(a));
        }
        if self.is_ancestor(b, a).await? {
            return Ok(Some(b));
        }
        let mut paint: HashMap<Id, u8> = HashMap::from([(a, 1), (b, 2)]);
        let mut heap = BinaryHeap::from([(self.graph(a).await?.generation, a), (self.graph(b).await?.generation, b)]);
        let mut popped = HashSet::new();
        while let Some((_, cur)) = heap.pop() {
            if !popped.insert(cur) {
                continue;
            }
            let f = paint[&cur];
            if f == 3 {
                return Ok(Some(cur));
            }
            for p in self.changeset(&cur).await?.parent_ids()? {
                let e = paint.entry(p).or_insert(0);
                if *e | f != *e {
                    *e |= f;
                    heap.push((self.graph(p).await?.generation, p));
                }
            }
        }
        Ok(None)
    }

    pub async fn lookup(&self, rev: Id, path: &str) -> Result<Option<(Mode, Id)>> {
        tree::lookup(self.objects.as_ref(), self.root_tree(rev).await?, path).await
    }
}

/// Prefix of refs reserved for the system (leases, checkpoints), such as
/// `_pando/compaction-lease` (and `_sapwood/...` in stores created before the rename).
/// User-facing names cannot contain `_`, so they can never collide with or move these.
pub const SYSTEM_REF_PREFIX: &str = "_";

/// Ref names: `[A-Za-z0-9._/-]`, no empty components, no `..`, at most 255 bytes.
pub fn validate_ref_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 255
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
        && name.split('/').all(|c| !c.is_empty() && c != "." && c != "..");
    if ok { Ok(()) } else { Err(Error::invalid(format!("invalid ref name `{name}`"))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap() {
        assert!(paths_overlap("a", "a"));
        assert!(paths_overlap("a", "a/b"));
        assert!(paths_overlap("a/b/c", "a/b"));
        assert!(!paths_overlap("a", "ab"));
        assert!(!paths_overlap("a-b", "a/b"));
    }

    #[test]
    fn ref_names() {
        for ok in ["main", "release/1.2", "user/kabir/feat-x"] {
            assert!(validate_ref_name(ok).is_ok());
        }
        for bad in ["", "a//b", "../x", "a b", "/main", "main/"] {
            assert!(validate_ref_name(bad).is_err(), "{bad}");
        }
    }
}
