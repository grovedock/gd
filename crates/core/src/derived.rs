//! Generic derived-data mechanism. Anything not stored in the changeset itself is a
//! [`Derivation`]: a value defined recursively from its parents' values, computed once
//! (eagerly at write time or lazily on first read) and cached in a [`DerivedStore`].
//!
//! [`DerivedStore`]: crate::ports::DerivedStore

use async_trait::async_trait;

use crate::object::Changeset;
use crate::{Error, Id, Repo, Result, tree};

#[async_trait]
pub trait Derivation: Send + Sync + 'static {
    /// Storage namespace. Bump the suffix when the encoding or definition changes.
    const KIND: &'static str;
    type Value: Clone + Send + Sync;

    fn encode(v: &Self::Value) -> Vec<u8>;
    fn decode(b: &[u8]) -> Result<Self::Value>;
    /// `parents` are aligned with `cs.parents`. Implementations may also read derived
    /// values of other *ancestors* through `repo`; those are always already computed.
    async fn compute(repo: &Repo, cs: &Changeset, parents: &[Self::Value]) -> Result<Self::Value>;
}

fn corrupt(kind: &str) -> Error {
    Error::Corrupt(format!("derived {kind} value"))
}

fn read_u64(b: &[u8], at: usize, kind: &str) -> Result<u64> {
    b.get(at..at + 8).map(|s| u64::from_le_bytes(s.try_into().expect("8 bytes"))).ok_or_else(|| corrupt(kind))
}

/// Root tree of the changeset: first parent's root tree with `changes` applied.
pub struct RootTree;

#[async_trait]
impl Derivation for RootTree {
    const KIND: &'static str = "root_tree.v1";
    type Value = Id;

    fn encode(v: &Id) -> Vec<u8> {
        v.0.to_vec()
    }
    fn decode(b: &[u8]) -> Result<Id> {
        Id::from_slice(b).map_err(|_| corrupt(Self::KIND))
    }
    async fn compute(repo: &Repo, cs: &Changeset, parents: &[Id]) -> Result<Id> {
        tree::apply_changes(repo.objects.as_ref(), parents.first().copied(), &cs.changes).await
    }
}

/// Generation number (1 + max parent generation) plus first-parent depth and a
/// skew-binary jump pointer (Myers 1983). The jump pointer gives O(log n) first-parent
/// ancestor queries with O(1) storage per changeset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphInfo {
    pub generation: u64,
    pub depth: u64,
    pub parent: Option<Id>,
    pub jump: Option<Id>,
    pub jump_depth: u64,
}

pub struct Generation;

#[async_trait]
impl Derivation for Generation {
    const KIND: &'static str = "graph.v1";
    type Value = GraphInfo;

    fn encode(v: &GraphInfo) -> Vec<u8> {
        let mut out = Vec::with_capacity(24 + 64);
        out.extend(v.generation.to_le_bytes());
        out.extend(v.depth.to_le_bytes());
        out.extend(v.jump_depth.to_le_bytes());
        if let (Some(p), Some(j)) = (v.parent, v.jump) {
            out.extend(p.0);
            out.extend(j.0);
        }
        out
    }

    fn decode(b: &[u8]) -> Result<GraphInfo> {
        let k = Self::KIND;
        let (generation, depth, jump_depth) = (read_u64(b, 0, k)?, read_u64(b, 8, k)?, read_u64(b, 16, k)?);
        let (parent, jump) = match b.len() {
            24 => (None, None),
            88 => (Some(Id::from_slice(&b[24..56])?), Some(Id::from_slice(&b[56..88])?)),
            _ => return Err(corrupt(k)),
        };
        Ok(GraphInfo { generation, depth, parent, jump, jump_depth })
    }

    async fn compute(repo: &Repo, cs: &Changeset, parents: &[GraphInfo]) -> Result<GraphInfo> {
        let generation = 1 + parents.iter().map(|p| p.generation).max().unwrap_or(0);
        let (Some(pid), Some(p)) = (cs.first_parent()?, parents.first()) else {
            return Ok(GraphInfo { generation, depth: 0, parent: None, jump: None, jump_depth: 0 });
        };
        // jump(n) = jump(jump(p)) when the two previous jumps have equal length, else p.
        let (mut jump, mut jump_depth) = (pid, p.depth);
        if let Some(pj) = p.jump {
            let j = repo.derive::<Generation>(pj).await?;
            if let Some(jj) = j.jump
                && p.depth - p.jump_depth == p.jump_depth - j.jump_depth
            {
                (jump, jump_depth) = (jj, j.jump_depth);
            }
        }
        Ok(GraphInfo { generation, depth: p.depth + 1, parent: Some(pid), jump: Some(jump), jump_depth })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_info_roundtrip() {
        for v in [
            GraphInfo { generation: 1, depth: 0, parent: None, jump: None, jump_depth: 0 },
            GraphInfo { generation: 9, depth: 7, parent: Some(Id([1; 32])), jump: Some(Id([2; 32])), jump_depth: 3 },
        ] {
            assert_eq!(Generation::decode(&Generation::encode(&v)).unwrap(), v);
        }
        assert!(Generation::decode(&[0; 30]).is_err());
        assert!(RootTree::decode(&[0; 3]).is_err());
    }
}
