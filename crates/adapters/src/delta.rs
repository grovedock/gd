//! Read-side support for delta-encoded storage. Wrap a durable store with
//! [`DeltaObjects`] and every read returns an ordinary, hash-verified `kind || zstd`
//! payload, whether the object is stored whole or as a delta. Deltas are created only by
//! the single-writer `grovedockd compact` job; this decorator never converts anything.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt, stream};
use grovedock_core::object::encode_object;
use grovedock_core::ports::{BATCH_CONCURRENCY, ObjectStore};
use grovedock_core::{Id, Result, delta};

use crate::storage_err;

pub struct DeltaObjects {
    inner: Arc<dyn ObjectStore>,
}

impl DeltaObjects {
    pub fn new(inner: Arc<dyn ObjectStore>) -> Self {
        Self { inner }
    }

    async fn resolve(&self, id: Id, payload: Bytes) -> Result<Bytes> {
        if !delta::is_delta(&payload) {
            return Ok(payload);
        }
        let chain = delta::chain(self.inner.as_ref(), &id, payload).await?;
        // Decompressing a chain and hashing the result is CPU work: keep it off the runtime.
        tokio::task::spawn_blocking(move || chain.rebuild().map(|(kind, data)| encode_object(kind, &data).1))
            .await
            .map_err(storage_err)?
    }
}

#[async_trait]
impl ObjectStore for DeltaObjects {
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        match self.inner.get(id).await? {
            Some(p) => Ok(Some(self.resolve(*id, p).await?)),
            None => Ok(None),
        }
    }

    async fn get_many(&self, ids: &[Id]) -> Result<Vec<Option<Bytes>>> {
        let raw = self.inner.get_many(ids).await?;
        stream::iter(ids.iter().copied().zip(raw))
            .map(|(id, p)| async move {
                match p {
                    Some(p) => self.resolve(id, p).await.map(Some),
                    None => Ok(None),
                }
            })
            .buffered(BATCH_CONCURRENCY)
            .try_collect()
            .await
    }

    async fn put(&self, id: Id, payload: Bytes) -> Result<()> {
        self.inner.put(id, payload).await
    }
    async fn put_many(&self, items: Vec<(Id, Bytes)>) -> Result<()> {
        self.inner.put_many(items).await
    }
    async fn has(&self, id: &Id) -> Result<bool> {
        self.inner.has(id).await
    }
    async fn missing(&self, ids: &[Id]) -> Result<Vec<Id>> {
        self.inner.missing(ids).await
    }
    async fn replace(&self, id: Id, payload: Bytes) -> Result<bool> {
        self.inner.replace(id, payload).await
    }
}
