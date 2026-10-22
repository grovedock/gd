//! Object storage via the `object_store` crate (Apache Arrow project): one adapter for
//! Amazon S3 (and S3-compatible: MinIO, R2, Ceph), Google Cloud Storage, Azure Blob,
//! local filesystem and memory, selected by URL.
//!
//! Layout: `<prefix>/objects/<2 hex>/<62 hex>`. Two-char fan-out keeps local directories
//! small and spreads S3 request-rate partitions.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt, stream};
use grovedock_core::ports::{BATCH_CONCURRENCY, ObjectStore};
use grovedock_core::{Id, Result};
use object_store::path::Path;
use object_store::{ObjectStore as Backend, ObjectStoreExt, PutMode, PutOptions, PutPayload};

use crate::storage_err;

pub struct CloudObjects {
    backend: Arc<dyn Backend>,
    prefix: Path,
}

impl CloudObjects {
    pub fn new(backend: Arc<dyn Backend>, prefix: Path) -> Self {
        Self { backend, prefix }
    }

    /// `file:///var/lib/gd`, `s3://bucket/prefix`, `gs://bucket`, `az://container`,
    /// `memory:///`. Credentials come from the provider's standard environment variables.
    /// Build `file://` URLs with [`file_url`] so Windows paths are encoded correctly.
    /// A local directory is durable: each object file and its directory are fsynced before a
    /// put returns.
    pub fn from_url(url: &str) -> Result<Self> {
        let url = url::Url::parse(url).map_err(storage_err)?;
        if url.scheme() == "file" {
            let dir = url.to_file_path().map_err(|_| storage_err(format!("bad file URL `{url}`")))?;
            return Self::local(&dir, true);
        }
        let env = std::env::vars().map(|(k, v)| (k.to_ascii_lowercase(), v));
        let (backend, prefix) = object_store::parse_url_opts(&url, env).map_err(storage_err)?;
        Ok(Self::new(Arc::from(backend), prefix))
    }

    /// Objects in a local directory; `fsync` makes every put durable before it returns (off
    /// only for caches, which can always refetch).
    pub fn local(dir: &std::path::Path, fsync: bool) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(storage_err)?;
        let fs = object_store::local::LocalFileSystem::new_with_prefix(dir).map_err(storage_err)?.with_fsync(fsync);
        Ok(Self::new(Arc::new(fs), Path::default()))
    }

    /// Whether a stored object fails its hash check (a file torn by a crash, say), so a put of
    /// the right bytes may replace it. Deltas and large-file manifests are checked by readers.
    async fn corrupt(&self, id: &Id) -> bool {
        match self.get(id).await {
            Ok(Some(p)) => {
                !grovedock_core::delta::is_delta(&p)
                    && !grovedock_core::chunked::is_manifest(&p)
                    && grovedock_core::object::decode_object(id, &p).is_err()
            }
            _ => false,
        }
    }

    fn path(&self, id: &Id) -> Path {
        let hex = id.to_hex();
        self.prefix.clone().join("objects").join(&hex[..2]).join(&hex[2..])
    }

    /// Bytes stored under this store's prefix (one listing; for usage metering).
    pub async fn size_bytes(&self) -> Result<u64> {
        self.backend
            .list(Some(&self.prefix))
            .map_err(storage_err)
            .try_fold(0, |n, m| async move { Ok(n + m.size) })
            .await
    }
}

#[async_trait]
impl ObjectStore for CloudObjects {
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        match self.backend.get(&self.path(id)).await {
            Ok(r) => Ok(Some(r.bytes().await.map_err(storage_err)?)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(storage_err(e)),
        }
    }

    async fn put(&self, id: Id, payload: Bytes) -> Result<()> {
        // Create-only: content addressing means an existing object is already correct,
        // and backends that support conditional puts skip the redundant upload.
        let opts = PutOptions { mode: PutMode::Create, ..Default::default() };
        match self.backend.put_opts(&self.path(&id), PutPayload::from_bytes(payload.clone()), opts).await {
            Ok(_) => Ok(()),
            // Already stored: normally identical, but a copy that fails its hash check is
            // repaired by this put when the new bytes check out (M4).
            Err(object_store::Error::AlreadyExists { .. }) => {
                if self.corrupt(&id).await && grovedock_core::object::decode_object(&id, &payload).is_ok() {
                    self.backend.put(&self.path(&id), PutPayload::from_bytes(payload)).await.map_err(storage_err)?;
                }
                Ok(())
            }
            Err(object_store::Error::NotImplemented { .. }) => {
                self.backend.put(&self.path(&id), PutPayload::from_bytes(payload)).await.map_err(storage_err)?;
                Ok(())
            }
            Err(e) => Err(storage_err(e)),
        }
    }

    async fn has(&self, id: &Id) -> Result<bool> {
        match self.backend.head(&self.path(id)).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(storage_err(e)),
        }
    }

    /// Object PUTs are atomic on every `object_store` backend, so readers see either the
    /// old or the new representation, never a torn write.
    async fn replace(&self, id: Id, payload: Bytes) -> Result<bool> {
        if !self.has(&id).await? {
            return Ok(false);
        }
        self.backend.put(&self.path(&id), PutPayload::from_bytes(payload)).await.map_err(storage_err)?;
        Ok(true)
    }

    async fn get_many(&self, ids: &[Id]) -> Result<Vec<Option<Bytes>>> {
        stream::iter(ids.iter().copied())
            .map(|id| async move { self.get(&id).await })
            .buffered(BATCH_CONCURRENCY)
            .try_collect()
            .await
    }

    /// Pages through the 256 two-hex-digit fan-out directories in order and sorts each one,
    /// because not every backend lists keys in order (the local filesystem does not).
    async fn list(&self, after: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        let first = after.map_or(0, |a| a.0[0] as usize);
        for fan in first..256 {
            let dir = self.prefix.clone().join("objects").join(format!("{fan:02x}"));
            let mut ids: Vec<Id> = self
                .backend
                .list(Some(&dir))
                .map_err(storage_err)
                .try_filter_map(|meta| async move {
                    let hex = format!("{fan:02x}{}", meta.location.filename().unwrap_or_default());
                    Ok(hex.parse::<Id>().ok())
                })
                .try_collect()
                .await?;
            ids.retain(|id| after.is_none_or(|a| *id > a));
            ids.sort_unstable();
            out.extend(ids.into_iter().take(limit - out.len()));
            if out.len() == limit {
                break;
            }
        }
        Ok(out)
    }
}

/// `file://` URL for a local directory, correct on every platform (`C:\\x` included).
pub fn file_url(dir: &std::path::Path) -> Result<String> {
    let abs = std::path::absolute(dir).map_err(storage_err)?;
    url::Url::from_directory_path(&abs)
        .map(String::from)
        .map_err(|_| storage_err(format!("bad path {}", abs.display())))
}

#[cfg(test)]
mod tests {
    use grovedock_core::object::{Kind, encode_object};

    use super::*;

    /// A torn object file (power loss without fsync) is repaired by pushing the object again.
    #[tokio::test]
    async fn a_corrupt_object_is_replaced_by_a_good_put() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudObjects::from_url(&file_url(dir.path()).unwrap()).unwrap();
        let (id, payload) = encode_object(Kind::Blob, b"hello, durable world");
        store.put(id, payload.clone()).await.unwrap();
        let hex = id.to_hex();
        let file = dir.path().join("objects").join(&hex[..2]).join(&hex[2..]);
        std::fs::write(&file, &payload[..5]).unwrap();
        // The wrong bytes for this id never replace anything.
        let (_, other) = encode_object(Kind::Blob, b"something else");
        store.put(id, other).await.unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), &payload[..5]);
        store.put(id, payload.clone()).await.unwrap();
        assert_eq!(store.get(&id).await.unwrap().unwrap(), payload);
    }
}
