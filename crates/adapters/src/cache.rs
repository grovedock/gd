//! Tiering building blocks.
//!
//! * [`SharedCaches`]: one memory budget for every repository of a server, kept apart by
//!   namespace.
//! * [`BoundedCache`]: memory-bounded object cache (quick_cache: sharded, CLOCK-Pro style
//!   eviction that resists scans, weighted by payload bytes).
//! * [`Tiered`]: read-through/write-around composition of a local store with an
//!   upstream store.
//! * [`CachedLog`] / [`CachedDerived`]: memory caches for immutable metadata. Server, regional cache and node daemon are all just different
//!   `(local, upstream)` pairs behind the same `Objects` RPC.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use grovedock_core::ports::{ChangesetLog, DerivedStore, ObjectStore};
use grovedock_core::{Id, Result};
use quick_cache::Weighter;
use quick_cache::sync::Cache;

/// Memory-only object store with a byte budget. Evicted objects are simply gone, so
/// use it as the `local` half of a [`Tiered`] store. Entries are keyed by a namespace too, so
/// several repositories can share one budget ([`SharedCaches`]) without seeing each other's.
pub struct BoundedCache {
    cache: Arc<Cache<(u64, Id), Bytes, BytesWeight>>,
    ns: u64,
}

impl BoundedCache {
    pub fn new(capacity_bytes: u64) -> Self {
        // Estimate ~8 KiB per object for initial table sizing.
        let est_items = (capacity_bytes / 8192).max(1024) as usize;
        Self { cache: Arc::new(Cache::with_weighter(est_items, capacity_bytes, BytesWeight)), ns: 0 }
    }
    pub fn weight(&self) -> u64 {
        self.cache.weight()
    }
}

#[async_trait]
impl ObjectStore for BoundedCache {
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        Ok(self.cache.get(&(self.ns, *id)))
    }
    async fn put(&self, id: Id, payload: Bytes) -> Result<()> {
        self.cache.insert((self.ns, id), payload);
        Ok(())
    }
    async fn has(&self, id: &Id) -> Result<bool> {
        Ok(self.cache.contains_key(&(self.ns, *id)))
    }
    async fn get_many(&self, ids: &[Id]) -> Result<Vec<Option<Bytes>>> {
        Ok(ids.iter().map(|id| self.cache.get(&(self.ns, *id))).collect())
    }
}

/// One memory budget for every repository a server hosts (objects, changesets, derived data),
/// instead of a budget each: a busy server holds hundreds of repositories open. Each repository
/// uses it under its own namespace, so one never reads another's entries.
#[derive(Clone)]
pub struct SharedCaches {
    objects: Arc<Cache<(u64, Id), Bytes, BytesWeight>>,
    log: Arc<Cache<(u64, Id), Bytes, BytesWeight>>,
    derived: Arc<Cache<(u64, String, Id), Bytes, BytesWeight>>,
}

impl SharedCaches {
    /// `total_bytes` split 60% objects, 25% changesets, 15% derived data.
    pub fn new(total_bytes: u64) -> Self {
        let (o, l, d) = (total_bytes * 60 / 100, total_bytes * 25 / 100, total_bytes * 15 / 100);
        Self {
            objects: Arc::new(Cache::with_weighter((o / 8192).max(1024) as usize, o, BytesWeight)),
            log: Arc::new(Cache::with_weighter((l / 512).max(1024) as usize, l, BytesWeight)),
            derived: Arc::new(Cache::with_weighter((d / 128).max(1024) as usize, d, BytesWeight)),
        }
    }

    pub fn objects(&self, ns: u64) -> BoundedCache {
        BoundedCache { cache: self.objects.clone(), ns }
    }
    pub fn log(&self, inner: Arc<dyn ChangesetLog>, ns: u64) -> CachedLog {
        CachedLog { inner, cache: self.log.clone(), ns }
    }
    pub fn derived(&self, inner: Arc<dyn DerivedStore>, ns: u64) -> CachedDerived {
        CachedDerived { inner, cache: self.derived.clone(), ns }
    }
    /// Bytes held across every repository.
    pub fn weight(&self) -> u64 {
        self.objects.weight() + self.log.weight() + self.derived.weight()
    }
}

/// Hit/miss counters for a tier.
#[derive(Default, Debug)]
pub struct TierStats {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
}

impl TierStats {
    pub fn hit_rate(&self) -> f64 {
        let (h, m) = (self.hits.load(Ordering::Relaxed), self.misses.load(Ordering::Relaxed));
        if h + m == 0 { 0.0 } else { h as f64 / (h + m) as f64 }
    }
}

fn is_tree(payload: &[u8]) -> bool {
    payload.first() == Some(&(grovedock_core::object::Kind::Tree as u8))
}

/// Read-through, write-around tier. Objects are immutable, so there is no invalidation:
/// a cached object is correct forever.
pub struct Tiered {
    pub local: Arc<dyn ObjectStore>,
    pub upstream: Arc<dyn ObjectStore>,
    pub stats: Arc<TierStats>,
}

impl Tiered {
    pub fn new(local: Arc<dyn ObjectStore>, upstream: Arc<dyn ObjectStore>) -> Self {
        Self { local, upstream, stats: Arc::default() }
    }
}

#[async_trait]
impl ObjectStore for Tiered {
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        Ok(self.get_many(std::slice::from_ref(id)).await?.pop().flatten())
    }

    /// Write-around for file contents: they go to the authority only and the local tier
    /// fills on reads, so a large upload never evicts the hot set other readers depend on.
    /// Trees are kept too (write-through): they are small, and the next change's tree is
    /// built from the one just written, which would otherwise come back from disk.
    async fn put(&self, id: Id, payload: Bytes) -> Result<()> {
        self.put_many(vec![(id, payload)]).await
    }

    /// Batches stay batches (one transaction / round trip upstream).
    async fn put_many(&self, items: Vec<(Id, Bytes)>) -> Result<()> {
        let trees: Vec<(Id, Bytes)> = items.iter().filter(|(_, p)| is_tree(p)).cloned().collect();
        self.upstream.put_many(items).await?;
        for (id, payload) in trees {
            self.local.put(id, payload).await?;
        }
        Ok(())
    }

    // `replace` deliberately keeps the default (unsupported): a cached copy would keep the
    // old representation. Maintenance jobs (compaction) write to the durable store directly.

    async fn has(&self, id: &Id) -> Result<bool> {
        Ok(self.local.has(id).await? || self.upstream.has(id).await?)
    }

    /// One local batch read, then one upstream batch for all misses.
    async fn get_many(&self, ids: &[Id]) -> Result<Vec<Option<Bytes>>> {
        let mut out = self.local.get_many(ids).await?;
        let miss_idx: Vec<usize> = (0..ids.len()).filter(|&i| out[i].is_none()).collect();
        self.stats.hits.fetch_add((ids.len() - miss_idx.len()) as u64, Ordering::Relaxed);
        self.stats.misses.fetch_add(miss_idx.len() as u64, Ordering::Relaxed);
        if miss_idx.is_empty() {
            return Ok(out);
        }
        let miss_ids: Vec<Id> = miss_idx.iter().map(|&i| ids[i]).collect();
        let fetched = self.upstream.get_many(&miss_ids).await?;
        for (i, payload) in miss_idx.into_iter().zip(fetched) {
            if let Some(p) = &payload {
                self.local.put(ids[i], p.clone()).await?;
            }
            out[i] = payload;
        }
        Ok(out)
    }

    async fn missing(&self, ids: &[Id]) -> Result<Vec<Id>> {
        let local_missing = self.local.missing(ids).await?;
        if local_missing.is_empty() {
            return Ok(local_missing);
        }
        self.upstream.missing(&local_missing).await
    }
}

#[derive(Clone)]
struct BytesWeight;
impl<K> Weighter<K, Bytes> for BytesWeight {
    fn weight(&self, _: &K, v: &Bytes) -> u64 {
        v.len() as u64 + 64
    }
}

/// Memory cache in front of a [`ChangesetLog`]. Changesets are immutable once appended,
/// so cached entries never go stale. History walks (log, blame, ancestry) hit memory.
pub struct CachedLog {
    inner: Arc<dyn ChangesetLog>,
    cache: Arc<Cache<(u64, Id), Bytes, BytesWeight>>,
    ns: u64,
}

impl CachedLog {
    pub fn new(inner: Arc<dyn ChangesetLog>, capacity_bytes: u64) -> Self {
        let est = (capacity_bytes / 512).max(1024) as usize;
        Self { inner, cache: Arc::new(Cache::with_weighter(est, capacity_bytes, BytesWeight)), ns: 0 }
    }
}

#[async_trait]
impl ChangesetLog for CachedLog {
    /// Write-through: a change is read again straight after it is made (deriving its tree).
    /// Only when the id is the bytes' own hash: then whatever the log already kept for that id
    /// (the first write wins) is these same bytes.
    async fn append(&self, id: Id, encoded: Bytes) -> Result<()> {
        let verified = Id::hash(grovedock_core::object::CHANGESET_TAG, &encoded) == id;
        self.inner.append(id, encoded.clone()).await?;
        if verified {
            self.cache.insert((self.ns, id), encoded);
        }
        Ok(())
    }
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        if let Some(b) = self.cache.get(&(self.ns, *id)) {
            return Ok(Some(b));
        }
        let got = self.inner.get(id).await?;
        if let Some(b) = &got {
            self.cache.insert((self.ns, *id), b.clone());
        }
        Ok(got)
    }
}

/// Memory cache in front of a [`DerivedStore`]. Derived values are pure functions of
/// immutable changesets; a re-put of the same key carries the same value, and the cache is
/// updated on every put.
pub struct CachedDerived {
    inner: Arc<dyn DerivedStore>,
    cache: Arc<Cache<(u64, String, Id), Bytes, BytesWeight>>,
    ns: u64,
}

impl CachedDerived {
    pub fn new(inner: Arc<dyn DerivedStore>, capacity_bytes: u64) -> Self {
        let est = (capacity_bytes / 128).max(1024) as usize;
        Self { inner, cache: Arc::new(Cache::with_weighter(est, capacity_bytes, BytesWeight)), ns: 0 }
    }
}

#[async_trait]
impl DerivedStore for CachedDerived {
    async fn get(&self, kind: &str, id: &Id) -> Result<Option<Bytes>> {
        let key = (self.ns, kind.to_owned(), *id);
        if let Some(b) = self.cache.get(&key) {
            return Ok(Some(b));
        }
        let got = self.inner.get(kind, id).await?;
        if let Some(b) = &got {
            self.cache.insert(key, b.clone());
        }
        Ok(got)
    }
    async fn put(&self, kind: &str, id: Id, value: Bytes) -> Result<()> {
        self.inner.put(kind, id, value.clone()).await?;
        self.cache.insert((self.ns, kind.to_owned(), id), value);
        Ok(())
    }
}

#[cfg(test)]
mod shared_tests {
    use super::*;
    use grovedock_core::object::{Kind, encode_object};

    #[tokio::test]
    async fn repositories_sharing_a_budget_never_see_each_others_entries() {
        let shared = SharedCaches::new(8 << 20);
        let (a, b) = (shared.objects(1), shared.objects(2));
        let (id, payload) = encode_object(Kind::Blob, b"only in a");
        a.put(id, payload.clone()).await.unwrap();
        assert_eq!(a.get(&id).await.unwrap(), Some(payload));
        assert_eq!(b.get(&id).await.unwrap(), None, "b must not read a's object");
        assert!(!b.has(&id).await.unwrap());
        // One budget: what a holds counts for both.
        assert!(shared.weight() > 0 && shared.weight() == a.weight());
    }
}

#[cfg(test)]
mod tier_tests {
    use super::*;
    use crate::memory::MemoryStore;
    use grovedock_core::object::{Kind, encode_object};

    #[tokio::test]
    async fn trees_are_kept_on_write_and_file_contents_are_not() {
        let local = Arc::new(BoundedCache::new(8 << 20));
        let tier = Tiered::new(local.clone(), Arc::new(MemoryStore::new()));
        let (tree, tp) = encode_object(Kind::Tree, b"a tree");
        let (blob, bp) = encode_object(Kind::Blob, b"a file");
        tier.put_many(vec![(tree, tp.clone()), (blob, bp)]).await.unwrap();
        assert_eq!(local.get(&tree).await.unwrap(), Some(tp), "the next change reads this tree");
        assert_eq!(local.get(&blob).await.unwrap(), None, "an upload doesn't flush the hot set");
        assert!(tier.has(&blob).await.unwrap(), "both are stored");
    }
}
