//! Large files as content-defined chunks.
//!
//! A file of at least [`CHUNK_THRESHOLD`] bytes is split with FastCDC into chunks that are
//! stored as ordinary blob objects. The file itself keeps its normal id
//! (`BLAKE3(blob tag || whole content)`) and is stored as a *manifest*:
//!
//! ```text
//! 0x03 | prost(Manifest { size, chunks: [(chunk id, length)] })
//! ```
//!
//! Content-defined boundaries mean an edit re-uploads only the chunks it touches, and
//! identical chunks dedupe across files and versions. Nothing ever holds a whole large
//! file in memory: chunking, verification and reassembly all stream.

use std::io::{Read, Write};

use bytes::Bytes;
use prost::Message;

use crate::object::{Kind, MAX_OBJECT_BYTES, decode_object, encode_object};
use crate::ports::ObjectStore;
use crate::{Error, Id, Result};

/// Files at least this large are chunked.
pub const CHUNK_THRESHOLD: u64 = 8 << 20;
pub const MIN_CHUNK: usize = 256 << 10;
pub const AVG_CHUNK: usize = 1 << 20;
pub const MAX_CHUNK: usize = 4 << 20;
/// Payload tag of a manifest (stored and on the wire).
pub const MANIFEST_TAG: u8 = 3;
/// Largest file that server-side features may load into memory whole (read, blame, diff).
pub const READ_IN_MEMORY_LIMIT: u64 = MAX_OBJECT_BYTES;
/// Chunks fetched per round trip while reassembling.
const FETCH_BATCH: usize = 32;

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ChunkRef {
    #[prost(bytes = "vec", tag = "1")]
    pub id: Vec<u8>,
    #[prost(uint64, tag = "2")]
    pub len: u64,
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Manifest {
    #[prost(uint64, tag = "1")]
    pub size: u64,
    #[prost(message, repeated, tag = "2")]
    pub chunks: Vec<ChunkRef>,
}

pub fn is_manifest(payload: &[u8]) -> bool {
    payload.first() == Some(&MANIFEST_TAG)
}

pub fn encode_manifest(m: &Manifest) -> Bytes {
    let mut out = Vec::with_capacity(1 + m.encoded_len());
    out.push(MANIFEST_TAG);
    m.encode(&mut out).expect("Vec has capacity");
    out.into()
}

/// Structural validation: well-formed ids, non-empty chunks within the object cap, and
/// lengths that add up. (Content is verified separately, against the file id.)
pub fn parse_manifest(payload: &[u8]) -> Result<Manifest> {
    if !is_manifest(payload) {
        return Err(Error::Corrupt("not a manifest".into()));
    }
    let m = Manifest::decode(&payload[1..]).map_err(|e| Error::Corrupt(format!("manifest: {e}")))?;
    let mut total = 0u64;
    for c in &m.chunks {
        Id::from_slice(&c.id).map_err(|_| Error::Corrupt("manifest chunk id".into()))?;
        if c.len == 0 || c.len > MAX_OBJECT_BYTES {
            return Err(Error::Corrupt("manifest chunk length".into()));
        }
        total = total.checked_add(c.len).ok_or_else(|| Error::Corrupt("manifest size overflow".into()))?;
    }
    if total != m.size {
        return Err(Error::Corrupt("manifest size does not match its chunks".into()));
    }
    Ok(m)
}

/// Stream `reader` into chunk objects, handing each to `sink` as it is produced, and
/// return the file id plus its manifest payload. Memory use is bounded by one chunk.
pub fn chunk_reader<R: Read>(reader: R, mut sink: impl FnMut(Id, Bytes) -> Result<()>) -> Result<(Id, Bytes)> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[Kind::Blob as u8]);
    let mut manifest = Manifest::default();
    for chunk in fastcdc::v2020::StreamCDC::new(reader, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK) {
        let chunk = chunk.map_err(|e| Error::invalid(format!("chunking: {e}")))?;
        hasher.update(&chunk.data);
        let (id, payload) = encode_object(Kind::Blob, &chunk.data);
        manifest.size += chunk.length as u64;
        manifest.chunks.push(ChunkRef { id: id.0.to_vec(), len: chunk.length as u64 });
        sink(id, payload)?;
    }
    Ok((Id(*hasher.finalize().as_bytes()), encode_manifest(&manifest)))
}

/// Stream the content of the object stored as `payload` under `id` into `out`, verifying
/// every chunk and the whole file against `id`. On error, `out` may hold partial data;
/// callers write to a temporary file and rename only on success.
pub async fn write_content(store: &dyn ObjectStore, id: &Id, payload: &[u8], out: &mut impl Write) -> Result<Kind> {
    if !is_manifest(payload) {
        let (kind, data) = decode_object(id, payload)?;
        out.write_all(&data).map_err(|e| Error::Storage(e.into()))?;
        return Ok(kind);
    }
    let m = parse_manifest(payload)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[Kind::Blob as u8]);
    for batch in m.chunks.chunks(FETCH_BATCH) {
        let ids: Vec<Id> = batch.iter().map(|c| Id::from_slice(&c.id)).collect::<Result<_>>()?;
        let payloads = store.get_many(&ids).await?;
        for ((c, cid), p) in batch.iter().zip(&ids).zip(payloads) {
            let p = p.ok_or_else(|| Error::not_found(format!("chunk {cid} of {id}")))?;
            let (kind, data) = decode_object(cid, &p)?;
            if kind != Kind::Blob || data.len() as u64 != c.len {
                return Err(Error::Corrupt(format!("chunk {cid} of {id} does not match its manifest")));
            }
            hasher.update(&data);
            out.write_all(&data).map_err(|e| Error::Storage(e.into()))?;
        }
    }
    if Id(*hasher.finalize().as_bytes()) != *id {
        return Err(Error::Corrupt(format!("{id}: reassembled content does not match its id")));
    }
    Ok(Kind::Blob)
}

/// Verify a manifest against its id by streaming its chunks (server-side, on upload).
pub async fn verify_manifest(store: &dyn ObjectStore, id: &Id, payload: &[u8]) -> Result<()> {
    write_content(store, id, payload, &mut std::io::sink()).await.map(drop)
}

/// Whole content in memory, for features that need it (read, blame, diff). Refuses files
/// larger than [`READ_IN_MEMORY_LIMIT`].
pub async fn read_content(store: &dyn ObjectStore, id: &Id, payload: &[u8]) -> Result<(Kind, Vec<u8>)> {
    if is_manifest(payload) {
        let size = parse_manifest(payload)?.size;
        if size > READ_IN_MEMORY_LIMIT {
            return Err(Error::invalid(format!("{id} is {size} bytes, too large to load in memory")));
        }
        let mut out = Vec::with_capacity(size as usize);
        let kind = write_content(store, id, payload, &mut out).await?;
        return Ok((kind, out));
    }
    decode_object(id, payload)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

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

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 56) as u8
            })
            .collect()
    }

    fn chunk(store: &Mem, data: &[u8]) -> (Id, Bytes, Vec<Id>) {
        let mut ids = Vec::new();
        let (id, manifest) = chunk_reader(data, |cid, p| {
            ids.push(cid);
            store.0.lock().unwrap().insert(cid, p);
            Ok(())
        })
        .unwrap();
        (id, manifest, ids)
    }

    #[tokio::test]
    async fn roundtrip_and_id_matches_whole_content() {
        let store = Mem::default();
        let data = noise(20 << 20, 1);
        let (id, manifest, ids) = chunk(&store, &data);
        assert_eq!(id, Id::hash(Kind::Blob as u8, &data), "file id is independent of chunking");
        assert!(ids.len() >= 5, "{} chunks", ids.len());
        let m = parse_manifest(&manifest).unwrap();
        assert_eq!(m.size, data.len() as u64);
        let mut out = Vec::new();
        write_content(&store, &id, &manifest, &mut out).await.unwrap();
        assert_eq!(out, data);
        verify_manifest(&store, &id, &manifest).await.unwrap();
    }

    #[tokio::test]
    async fn local_edit_changes_few_chunks() {
        let store = Mem::default();
        let data = noise(24 << 20, 2);
        let (_, _, before) = chunk(&store, &data);
        let mut edited = data.clone();
        edited.splice(12 << 20..12 << 20, b"inserted in the middle".iter().copied());
        let (_, _, after) = chunk(&store, &edited);
        let new: Vec<_> = after.iter().filter(|c| !before.contains(c)).collect();
        assert!(new.len() <= 2, "{} of {} chunks changed", new.len(), after.len());
    }

    #[tokio::test]
    async fn verification_catches_lies() {
        let store = Mem::default();
        let data = noise(10 << 20, 3);
        let (id, manifest, ids) = chunk(&store, &data);
        // Wrong id for a valid manifest.
        assert!(verify_manifest(&store, &Id([7; 32]), &manifest).await.is_err());
        // Missing chunk.
        store.0.lock().unwrap().remove(&ids[1]);
        assert!(verify_manifest(&store, &id, &manifest).await.is_err());
        // Reordered chunks: structurally valid, content wrong.
        let (id2, manifest2, _) = chunk(&store, &data);
        let mut m = parse_manifest(&manifest2).unwrap();
        m.chunks.swap(0, 1);
        assert!(verify_manifest(&store, &id2, &encode_manifest(&m)).await.is_err());
        // Structural errors.
        let mut bad = parse_manifest(&manifest2).unwrap();
        bad.size += 1;
        assert!(parse_manifest(&encode_manifest(&bad)).is_err());
        assert!(parse_manifest(&[MANIFEST_TAG, 0xff, 0xff]).is_err());
    }

    #[tokio::test]
    async fn in_memory_reads_are_capped() {
        let n = (READ_IN_MEMORY_LIMIT / MAX_OBJECT_BYTES + 1) as usize;
        let big = Manifest {
            size: MAX_OBJECT_BYTES * n as u64,
            chunks: (0..n).map(|i| ChunkRef { id: vec![i as u8; 32], len: MAX_OBJECT_BYTES }).collect(),
        };
        let err = read_content(&Mem::default(), &Id([0; 32]), &encode_manifest(&big)).await.unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
    }
}
