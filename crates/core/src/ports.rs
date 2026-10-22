//! Infrastructure ports. Every storage or network detail is reached only through these
//! traits, so adapters (memory, SQLite, Postgres, S3/GCS/Azure, an upstream tier...) are
//! interchangeable without touching domain code.

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt, stream};

use crate::{Error, Id, Result};

/// Concurrency used by default batch implementations.
pub const BATCH_CONCURRENCY: usize = 64;

/// Content-addressed object storage. Payloads are opaque `kind || zstd(content)` bytes.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn get(&self, id: &Id) -> Result<Option<Bytes>>;
    async fn put(&self, id: Id, payload: Bytes) -> Result<()>;
    async fn has(&self, id: &Id) -> Result<bool>;

    /// Batch fetch, results aligned with `ids`. Override when the backend can do better
    /// than bounded concurrent point reads.
    async fn get_many(&self, ids: &[Id]) -> Result<Vec<Option<Bytes>>> {
        stream::iter(ids.iter().copied())
            .map(|id| async move { self.get(&id).await })
            .buffered(BATCH_CONCURRENCY)
            .try_collect()
            .await
    }

    /// Batch write. Override to use one transaction / multi-row insert.
    async fn put_many(&self, items: Vec<(Id, Bytes)>) -> Result<()> {
        stream::iter(items).map(|(id, p)| self.put(id, p)).buffer_unordered(BATCH_CONCURRENCY).try_collect::<()>().await
    }

    /// Overwrite the stored representation of an existing object with an equivalent one
    /// (same id, same content, e.g. a delta encoding). Returns `false` when the backend
    /// cannot replace in place; callers must then leave the object as is.
    async fn replace(&self, _id: Id, _payload: Bytes) -> Result<bool> {
        Ok(false)
    }

    /// Up to `limit` ids this store holds, in ascending order, starting after `after`. Used
    /// to copy a store (backup, restore, moving between backends); caches and remote tiers
    /// cannot list and return an error.
    async fn list(&self, _after: Option<Id>, _limit: usize) -> Result<Vec<Id>> {
        Err(Error::invalid("this object store cannot list its contents"))
    }

    /// Ids from `ids` this store cannot serve.
    async fn missing(&self, ids: &[Id]) -> Result<Vec<Id>> {
        let present: Vec<bool> = stream::iter(ids.iter().copied())
            .map(|id| async move { self.has(&id).await })
            .buffered(BATCH_CONCURRENCY)
            .try_collect()
            .await?;
        Ok(ids.iter().zip(present).filter(|(_, p)| !p).map(|(id, _)| *id).collect())
    }
}

/// Append-only write-ahead log of changesets, keyed by changeset id.
#[async_trait]
pub trait ChangesetLog: Send + Sync {
    /// Idempotent: appending an existing id is a no-op.
    async fn append(&self, id: Id, encoded: Bytes) -> Result<()>;
    async fn get(&self, id: &Id) -> Result<Option<Bytes>>;
    /// Up to `limit` changeset ids in ascending order, starting after `after` (see
    /// [`ObjectStore::list`]).
    async fn list(&self, _after: Option<Id>, _limit: usize) -> Result<Vec<Id>> {
        Err(Error::invalid("this changeset log cannot list its contents"))
    }
}

/// Named pointers. The only mutable state in the system; every move is a
/// compare-and-swap on one key, so there is no global lock.
#[async_trait]
pub trait RefStore: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<Id>>;
    /// Set `name` to `new` iff it currently equals `expected` (`None` = absent).
    async fn compare_and_swap(&self, name: &str, expected: Option<Id>, new: Id) -> Result<bool>;
    async fn list(&self) -> Result<Vec<(String, Id)>>;
}

/// Storage for derived data, keyed by (derivation kind, changeset id).
#[async_trait]
pub trait DerivedStore: Send + Sync {
    async fn get(&self, kind: &str, id: &Id) -> Result<Option<Bytes>>;
    async fn put(&self, kind: &str, id: Id, value: Bytes) -> Result<()>;
    /// Up to `limit` entries of `kind` in ascending id order, starting after `after`.
    async fn list(&self, _kind: &str, _after: Option<Id>, _limit: usize) -> Result<Vec<(Id, Bytes)>> {
        Err(Error::invalid("this derived store cannot list its contents"))
    }
}

/// A change about to be landed, as seen by pre-land checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandProposal {
    pub principal: String,
    pub ref_name: String,
    pub changeset: Id,
    pub parent: Option<Id>,
    pub author: String,
    pub message: String,
    pub paths: Vec<String>,
}

/// Pre-land checks: CI status, required reviews, policy bots. Runs once per land, before
/// any rebase; a rebase only happens when no path conflicts, so what was checked is
/// exactly what lands.
#[async_trait]
pub trait LandCheck: Send + Sync {
    /// `Ok` lets the land proceed; `Err(Error::Rejected(reason))` refuses it.
    async fn check(&self, proposal: &LandProposal) -> Result<()>;
}

/// No checks: every authorised land proceeds.
pub struct NoChecks;

#[async_trait]
impl LandCheck for NoChecks {
    async fn check(&self, _: &LandProposal) -> Result<()> {
        Ok(())
    }
}
