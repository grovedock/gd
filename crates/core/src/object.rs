//! Canonical object model and storage encoding.
//!
//! Stored/wire payload = `kind byte || zstd(content)`; id = `BLAKE3(kind || content)`.
//! Hashing the *uncompressed* content keeps ids stable if compression settings change,
//! and shipping the compressed bytes lets every tier stream objects without recompressing.

use std::io::Read;

use bytes::Bytes;
use prost::Message;

use crate::{Error, Id, Result};

/// Tags for [`Id::hash`] domain separation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Blob = 1,
    Tree = 2,
}
pub const CHANGESET_TAG: u8 = 3;

impl Kind {
    fn from_byte(b: u8) -> Result<Kind> {
        match b {
            1 => Ok(Kind::Blob),
            2 => Ok(Kind::Tree),
            _ => Err(Error::Corrupt(format!("unknown object kind {b}"))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum Mode {
    File = 0,
    Exec = 1,
    Symlink = 2,
    Dir = 3,
}

impl Mode {
    pub fn parse(v: i32) -> Result<Mode> {
        Mode::try_from(v).map_err(|_| Error::invalid(format!("unknown mode {v}")))
    }
}

/// One directory entry. Trees are sorted by `name` and names are unique.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct TreeEntry {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(enumeration = "Mode", tag = "2")]
    pub mode: i32,
    #[prost(bytes = "vec", tag = "3")]
    pub id: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Tree {
    #[prost(message, repeated, tag = "1")]
    pub entries: Vec<TreeEntry>,
}

/// A file-level change relative to the changeset's first parent. Empty `blob` = delete.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct FileChange {
    #[prost(string, tag = "1")]
    pub path: String,
    #[prost(bytes = "vec", tag = "2")]
    pub blob: Vec<u8>,
    #[prost(enumeration = "Mode", tag = "3")]
    pub mode: i32,
}

impl FileChange {
    pub fn upsert(path: impl Into<String>, mode: Mode, blob: Id) -> Self {
        Self { path: path.into(), blob: blob.0.to_vec(), mode: mode as i32 }
    }
    pub fn delete(path: impl Into<String>) -> Self {
        Self { path: path.into(), blob: Vec::new(), mode: 0 }
    }
    pub fn is_delete(&self) -> bool {
        self.blob.is_empty()
    }
    pub fn blob_id(&self) -> Result<Option<Id>> {
        if self.is_delete() { Ok(None) } else { Id::from_slice(&self.blob).map(Some) }
    }
}

/// The unit of work: an append-only log entry listing changed files, not a tree.
/// The resulting root tree is *derived data* (see [`crate::RootTree`]).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Changeset {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub parents: Vec<Vec<u8>>,
    #[prost(message, repeated, tag = "2")]
    pub changes: Vec<FileChange>,
    #[prost(string, tag = "3")]
    pub author: String,
    #[prost(string, tag = "4")]
    pub message: String,
    #[prost(int64, tag = "5")]
    pub timestamp_ms: i64,
    /// For a changeset that came from git: what git needs to make the very same commit again.
    /// Absent for changes made with gd (so their ids don't change).
    #[prost(message, optional, tag = "6")]
    pub git: Option<GitOrigin>,
}

/// A git commit's identity beyond what a changeset holds (`proto/gd_objects.proto`).
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct GitOrigin {
    /// As git wrote them: "Name <email> SECONDS +ZZZZ".
    #[prost(string, tag = "1")]
    pub author: String,
    #[prost(string, tag = "2")]
    pub committer: String,
    /// The message's encoding when it isn't UTF-8 (then `message` holds its bytes).
    #[prost(string, tag = "3")]
    pub encoding: String,
    #[prost(bytes = "vec", tag = "4")]
    pub message: Vec<u8>,
    /// A signed commit's signature, verbatim, and its hash algorithm ("sha1").
    #[prost(string, tag = "5")]
    pub signature_algorithm: String,
    #[prost(bytes = "vec", tag = "6")]
    pub signature: Vec<u8>,
    /// Submodule entries: gd keeps no submodules; git's trees do.
    #[prost(message, repeated, tag = "7")]
    pub gitlinks: Vec<Gitlink>,
    /// The commit id this must reproduce: checked when written, never trusted.
    #[prost(string, tag = "8")]
    pub commit: String,
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Gitlink {
    #[prost(string, tag = "1")]
    pub path: String,
    #[prost(string, tag = "2")]
    pub commit: String,
}

impl Changeset {
    pub fn parent_ids(&self) -> Result<Vec<Id>> {
        self.parents.iter().map(|p| Id::from_slice(p)).collect()
    }
    pub fn first_parent(&self) -> Result<Option<Id>> {
        self.parents.first().map(|p| Id::from_slice(p)).transpose()
    }
    pub fn encode_canonical(&self) -> (Id, Bytes) {
        let bytes = self.encode_to_vec();
        (Id::hash(CHANGESET_TAG, &bytes), bytes.into())
    }
    pub fn decode_verified(id: &Id, bytes: &[u8]) -> Result<Changeset> {
        if Id::hash(CHANGESET_TAG, bytes) != *id {
            return Err(Error::Corrupt(format!("changeset {id} hash mismatch")));
        }
        Changeset::decode(bytes).map_err(|e| Error::Corrupt(format!("changeset {id}: {e}")))
    }
}

/// zstd level 3: the library default, ~350 MB/s compress on one core with ratio within a
/// few percent of level 9. Source code compresses 3-5x.
const ZSTD_LEVEL: i32 = 3;

/// Upper bound on one object (decoded), so a hostile tier cannot zip-bomb a client and a
/// single object always fits one gRPC message. Larger files belong in chunked storage
/// (see ROADMAP: content-defined chunking).
pub const MAX_OBJECT_BYTES: u64 = 256 << 20;

pub fn encode_object(kind: Kind, content: &[u8]) -> (Id, Bytes) {
    let id = Id::hash(kind as u8, content);
    // The bulk encoder records the content size in the frame header, which lets readers
    // size an object (e.g. for `stat` on a mounted checkout) without decompressing it.
    let frame = zstd::bulk::compress(content, ZSTD_LEVEL).expect("zstd into memory");
    let mut payload = Vec::with_capacity(1 + frame.len());
    payload.push(kind as u8);
    payload.extend_from_slice(&frame);
    (id, payload.into())
}

/// Decompress and verify a payload against its claimed id.
pub fn decode_object(id: &Id, payload: &[u8]) -> Result<(Kind, Vec<u8>)> {
    let (&tag, body) = payload.split_first().ok_or_else(|| Error::Corrupt(format!("{id}: empty payload")))?;
    let kind = Kind::from_byte(tag)?;
    let mut content = Vec::new();
    zstd::stream::Decoder::new(body)
        .and_then(|d| d.take(MAX_OBJECT_BYTES + 1).read_to_end(&mut content))
        .map_err(|e| Error::Corrupt(format!("{id}: {e}")))?;
    if content.len() as u64 > MAX_OBJECT_BYTES {
        return Err(Error::Corrupt(format!("{id}: exceeds {MAX_OBJECT_BYTES} bytes")));
    }
    if Id::hash(tag, &content) != *id {
        return Err(Error::Corrupt(format!("{id}: hash mismatch")));
    }
    Ok((kind, content))
}

/// Content size of a stored object without decompressing it: from the zstd frame header,
/// or from the manifest of a chunked large file.
pub fn content_size(payload: &[u8]) -> Result<u64> {
    if crate::chunked::is_manifest(payload) {
        return Ok(crate::chunked::parse_manifest(payload)?.size);
    }
    let body = payload.get(1..).ok_or_else(|| Error::Corrupt("empty payload".into()))?;
    match zstd::zstd_safe::get_frame_content_size(body) {
        Ok(Some(n)) => Ok(n),
        // Objects written before sizes were recorded: measure by decompressing.
        _ => {
            let mut n = 0u64;
            let mut dec = zstd::stream::Decoder::new(body).map_err(|e| Error::Corrupt(e.to_string()))?;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let k = dec.read(&mut buf).map_err(|e| Error::Corrupt(e.to_string()))?;
                if k == 0 {
                    return Ok(n);
                }
                n += k as u64;
            }
        }
    }
}

/// Cheap kind check without decompressing.
pub fn payload_kind(payload: &[u8]) -> Result<Kind> {
    Kind::from_byte(*payload.first().ok_or_else(|| Error::Corrupt("empty payload".into()))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn object_roundtrip(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let (id, payload) = encode_object(Kind::Blob, &data);
            let (kind, out) = decode_object(&id, &payload).unwrap();
            prop_assert_eq!(kind, Kind::Blob);
            prop_assert_eq!(out, data);
        }

        // Fuzz: arbitrary bytes must never panic the decoder.
        #[test]
        fn decode_never_panics(junk in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = decode_object(&Id([0; 32]), &junk);
            let _ = Changeset::decode_verified(&Id::hash(CHANGESET_TAG, &junk), &junk);
        }
    }

    #[test]
    fn content_size_of_legacy_frames_without_size() {
        let mut p = vec![Kind::Blob as u8];
        zstd::stream::copy_encode(&b"legacy"[..], &mut p, 3).unwrap();
        assert_eq!(content_size(&p).unwrap(), 6);
    }

    #[test]
    fn content_size_from_header() {
        for n in [0usize, 1, 1000, 1 << 20] {
            let (_, p) = encode_object(Kind::Blob, &vec![7u8; n]);
            assert_eq!(content_size(&p).unwrap(), n as u64);
        }
    }

    #[test]
    fn tampered_payload_rejected() {
        let (id, payload) = encode_object(Kind::Blob, b"hello");
        let (other, _) = encode_object(Kind::Blob, b"world");
        assert!(decode_object(&other, &payload).is_err());
        let mut retag = payload.to_vec();
        retag[0] = Kind::Tree as u8;
        assert!(decode_object(&id, &retag).is_err());
    }

    #[test]
    fn changeset_id_is_stable_and_verified() {
        let cs = Changeset { message: "m".into(), ..Default::default() };
        let (id, bytes) = cs.encode_canonical();
        assert_eq!(Changeset::decode_verified(&id, &bytes).unwrap(), cs);
        assert!(Changeset::decode_verified(&Id([1; 32]), &bytes).is_err());
    }
}
