//! In-process adapter for every port. Used for tests, local demos, and as the
//! reference behaviour the contract tests hold every other adapter to.

use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

use async_trait::async_trait;
use bytes::Bytes;
use grovedock_core::ports::{ChangesetLog, DerivedStore, ObjectStore, RefStore};
use grovedock_core::{Id, Result};

#[derive(Default)]
pub struct MemoryStore {
    objects: RwLock<HashMap<Id, Bytes>>,
    changesets: RwLock<HashMap<Id, Bytes>>,
    refs: RwLock<BTreeMap<String, Id>>,
    derived: RwLock<HashMap<(String, Id), Bytes>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ObjectStore for MemoryStore {
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        Ok(self.objects.read().unwrap().get(id).cloned())
    }
    async fn put(&self, id: Id, payload: Bytes) -> Result<()> {
        self.objects.write().unwrap().entry(id).or_insert(payload);
        Ok(())
    }
    async fn has(&self, id: &Id) -> Result<bool> {
        Ok(self.objects.read().unwrap().contains_key(id))
    }
    async fn replace(&self, id: Id, payload: Bytes) -> Result<bool> {
        Ok(self.objects.write().unwrap().get_mut(&id).map(|p| *p = payload).is_some())
    }
    async fn get_many(&self, ids: &[Id]) -> Result<Vec<Option<Bytes>>> {
        let map = self.objects.read().unwrap();
        Ok(ids.iter().map(|id| map.get(id).cloned()).collect())
    }
    async fn missing(&self, ids: &[Id]) -> Result<Vec<Id>> {
        let map = self.objects.read().unwrap();
        Ok(ids.iter().filter(|id| !map.contains_key(id)).copied().collect())
    }
    async fn list(&self, after: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        Ok(page(self.objects.read().unwrap().keys().copied(), after, limit))
    }
}

/// Up to `limit` ids greater than `after`, ascending.
fn page(ids: impl Iterator<Item = Id>, after: Option<Id>, limit: usize) -> Vec<Id> {
    let mut ids: Vec<Id> = ids.filter(|id| after.is_none_or(|a| *id > a)).collect();
    ids.sort_unstable();
    ids.truncate(limit);
    ids
}

#[async_trait]
impl ChangesetLog for MemoryStore {
    async fn append(&self, id: Id, encoded: Bytes) -> Result<()> {
        self.changesets.write().unwrap().entry(id).or_insert(encoded);
        Ok(())
    }
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        Ok(self.changesets.read().unwrap().get(id).cloned())
    }
    async fn list(&self, after: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        Ok(page(self.changesets.read().unwrap().keys().copied(), after, limit))
    }
}

#[async_trait]
impl RefStore for MemoryStore {
    async fn get(&self, name: &str) -> Result<Option<Id>> {
        Ok(self.refs.read().unwrap().get(name).copied())
    }
    async fn compare_and_swap(&self, name: &str, expected: Option<Id>, new: Id) -> Result<bool> {
        let mut refs = self.refs.write().unwrap();
        if refs.get(name).copied() != expected {
            return Ok(false);
        }
        refs.insert(name.to_owned(), new);
        Ok(true)
    }
    async fn list(&self) -> Result<Vec<(String, Id)>> {
        Ok(self.refs.read().unwrap().iter().map(|(k, v)| (k.clone(), *v)).collect())
    }
}

#[async_trait]
impl DerivedStore for MemoryStore {
    async fn get(&self, kind: &str, id: &Id) -> Result<Option<Bytes>> {
        Ok(self.derived.read().unwrap().get(&(kind.to_owned(), *id)).cloned())
    }
    async fn put(&self, kind: &str, id: Id, value: Bytes) -> Result<()> {
        self.derived.write().unwrap().insert((kind.to_owned(), id), value);
        Ok(())
    }
    async fn list(&self, kind: &str, after: Option<Id>, limit: usize) -> Result<Vec<(Id, Bytes)>> {
        let map = self.derived.read().unwrap();
        let ids = page(map.keys().filter(|(k, _)| k == kind).map(|(_, id)| *id), after, limit);
        Ok(ids.into_iter().map(|id| (id, map[&(kind.to_owned(), id)].clone())).collect())
    }
}
