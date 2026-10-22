//! Delta storage encoding: a server-side storage representation, never sent on the wire.
//!
//! A delta payload stores an object (blob or tree) as a zstd frame compressed against the
//! full content of an older version of the same kind (`base`), used as a raw-content
//! prefix ("patch-from"). The target's kind is its base's kind:
//!
//! ```text
//! 0x81 | base id (32 bytes) | depth (1 byte) | zstd frame
//! ```
//!
//! Readers reconstruct the full content, verify it hashes to the requested id, and return
//! an ordinary `kind || zstd(content)` payload, so tiers and clients never see deltas.

use std::collections::HashSet;

use bytes::Bytes;
use zstd::zstd_safe::{CCtx, CParameter, DCtx, DParameter};

use crate::object::{Kind, MAX_OBJECT_BYTES, decode_object};
use crate::ports::ObjectStore;
use crate::{Error, Id, Result};

pub const DELTA_TAG: u8 = 0x81;
/// Longest chain compaction creates (frames from an object down to its full base). Same as
/// git's default `pack.depth`; reconstruction cost is bounded and hot objects are cached.
pub const MAX_DEPTH: u8 = 50;
/// Hard limit for readers: far above what compaction creates, so an unexpected chain can
/// only make a read slower, never make data unreadable. Cycles are detected separately.
pub const READ_DEPTH_LIMIT: usize = 256;
/// Delta-encode only objects (and bases) up to this size: keeps the zstd window within the
/// decoder's default limit (2^27) for base + target.
pub const MAX_DELTA_INPUT: usize = 60 << 20;
const HEADER: usize = 1 + 32 + 1;
const LEVEL: i32 = 3;

pub fn is_delta(payload: &[u8]) -> bool {
    payload.first() == Some(&DELTA_TAG)
}

/// (base id, depth, zstd frame) of a delta payload.
pub fn parse(payload: &[u8]) -> Result<(Id, u8, &[u8])> {
    if !is_delta(payload) || payload.len() < HEADER {
        return Err(Error::Corrupt("malformed delta payload".into()));
    }
    Ok((Id::from_slice(&payload[1..33])?, payload[33], &payload[HEADER..]))
}

fn zstd_err(code: usize) -> Error {
    Error::Corrupt(format!("zstd: {}", zstd::zstd_safe::get_error_name(code)))
}

fn window_log(total: usize) -> u32 {
    (usize::BITS - total.max(1).leading_zeros()).clamp(10, 27)
}

/// Delta payload for `target` against `base_content` (the content of `base`).
pub fn encode(base: Id, depth: u8, base_content: &[u8], target: &[u8]) -> Result<Vec<u8>> {
    if base_content.len() > MAX_DELTA_INPUT || target.len() > MAX_DELTA_INPUT {
        return Err(Error::invalid("object too large for delta encoding"));
    }
    let mut cctx = CCtx::create();
    cctx.set_parameter(CParameter::CompressionLevel(LEVEL)).map_err(zstd_err)?;
    cctx.set_parameter(CParameter::WindowLog(window_log(base_content.len() + target.len()))).map_err(zstd_err)?;
    // Long-distance matching finds the base's copies of unchanged regions; measured on
    // source files it shrinks a one-line-edit delta ~30x at the same speed as level 3.
    cctx.set_parameter(CParameter::EnableLongDistanceMatching(true)).map_err(zstd_err)?;
    // A prefix is always raw content, unlike a dictionary, whose magic bytes could be
    // misinterpreted if a base file happened to start with them.
    cctx.ref_prefix(base_content).map_err(zstd_err)?;
    let mut out = Vec::with_capacity(HEADER + zstd::zstd_safe::compress_bound(target.len()));
    out.push(DELTA_TAG);
    out.extend_from_slice(&base.0);
    out.push(depth);
    let mut frame = Vec::with_capacity(zstd::zstd_safe::compress_bound(target.len()));
    cctx.compress2(&mut frame, target).map_err(zstd_err)?;
    out.extend_from_slice(&frame);
    Ok(out)
}

fn apply(frame: &[u8], base_content: &[u8]) -> Result<Vec<u8>> {
    let size = zstd::zstd_safe::get_frame_content_size(frame)
        .map_err(|_| Error::Corrupt("delta frame header".into()))?
        .ok_or_else(|| Error::Corrupt("delta frame without content size".into()))?;
    if size > MAX_OBJECT_BYTES {
        return Err(Error::Corrupt("delta target exceeds object size limit".into()));
    }
    let mut dctx = DCtx::create();
    dctx.set_parameter(DParameter::WindowLogMax(27)).map_err(zstd_err)?;
    dctx.ref_prefix(base_content).map_err(zstd_err)?;
    let mut out = Vec::with_capacity(size as usize);
    dctx.decompress(&mut out, frame).map_err(zstd_err)?;
    if out.len() as u64 != size {
        return Err(Error::Corrupt("delta frame size mismatch".into()));
    }
    Ok(out)
}

/// Decode a delta payload whose base content is already in memory (used to verify a
/// freshly encoded delta before it replaces anything).
pub fn decode_with_base(payload: &[u8], base_content: &[u8]) -> Result<Vec<u8>> {
    apply(parse(payload)?.2, base_content)
}

/// The stored payloads needed to rebuild one delta-encoded object.
pub struct Chain {
    target: Id,
    base: Id,
    base_payload: Bytes,
    /// Frames from the target down to the object just above the base.
    frames: Vec<Bytes>,
}

/// Gather the delta chain for `id` (stored as `payload`) from `store`. Detects cycles,
/// missing bases and over-long chains.
pub async fn chain(store: &dyn ObjectStore, id: &Id, payload: Bytes) -> Result<Chain> {
    let mut frames = Vec::new();
    let mut seen = HashSet::from([*id]);
    let mut cur = payload;
    loop {
        let (base, _, _) = parse(&cur)?;
        if frames.len() >= READ_DEPTH_LIMIT {
            return Err(Error::Corrupt(format!("{id}: delta chain longer than {READ_DEPTH_LIMIT}")));
        }
        frames.push(cur.slice(HEADER..));
        if !seen.insert(base) {
            return Err(Error::Corrupt(format!("{id}: delta cycle at {base}")));
        }
        cur = store.get(&base).await?.ok_or_else(|| Error::Corrupt(format!("{id}: missing delta base {base}")))?;
        if !is_delta(&cur) {
            return Ok(Chain { target: *id, base, base_payload: cur, frames });
        }
    }
}

impl Chain {
    /// CPU-bound reconstruction; verifies the result hashes to the target id under the
    /// base's kind.
    pub fn rebuild(self) -> Result<(Kind, Vec<u8>)> {
        let (kind, mut data) = decode_object(&self.base, &self.base_payload)?;
        for frame in self.frames.iter().rev() {
            data = apply(frame, &data)?;
        }
        if Id::hash(kind as u8, &data) != self.target {
            return Err(Error::Corrupt(format!("{}: reconstructed content hash mismatch", self.target)));
        }
        Ok((kind, data))
    }
}

/// Full content of `id` given its stored payload (delta or not), rebuilt inline.
pub async fn content(store: &dyn ObjectStore, id: &Id, payload: Bytes) -> Result<(Kind, Vec<u8>)> {
    if !is_delta(&payload) {
        return decode_object(id, &payload);
    }
    chain(store, id, payload).await?.rebuild()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_apply_roundtrip_and_compression() {
        // Incompressible (LCG) data, so any size win comes from the delta, not zstd alone.
        let mut x: u64 = 0x9E3779B97F4A7C15;
        let v1: Vec<u8> = (0..200_000)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 56) as u8
            })
            .collect();
        let mut v2 = v1.clone();
        v2[100_000] ^= 0xff;
        v2.extend_from_slice(b"appended");
        let base = Id::hash(Kind::Blob as u8, &v1);
        let d = encode(base, 1, &v1, &v2).unwrap();
        let (b, depth, frame) = parse(&d).unwrap();
        assert_eq!((b, depth), (base, 1));
        assert_eq!(apply(frame, &v1).unwrap(), v2);
        let full = crate::object::encode_object(Kind::Blob, &v2).1;
        assert!(full.len() > 190_000, "test data must be incompressible");
        assert!(d.len() * 100 < full.len(), "delta {} vs full {}", d.len(), full.len());
        // Wrong base must not silently produce the target.
        assert_ne!(apply(frame, &v2).ok(), Some(v2.clone()));
    }

    #[test]
    fn base_starting_with_dictionary_magic_is_raw_content() {
        let mut v1 = vec![0x37, 0xA4, 0x30, 0xEC]; // zstd dictionary magic, little-endian
        v1.extend((0..5000).map(|i| (i % 7) as u8));
        let mut v2 = v1.clone();
        v2.push(1);
        let d = encode(Id([0; 32]), 1, &v1, &v2).unwrap();
        assert_eq!(apply(parse(&d).unwrap().2, &v1).unwrap(), v2);
    }

    #[test]
    fn malformed_payloads_rejected() {
        assert!(parse(&[DELTA_TAG; 10]).is_err());
        assert!(parse(&[1, 2, 3]).is_err());
        let mut d = encode(Id([0; 32]), 1, b"base", b"target").unwrap();
        d.truncate(d.len() - 2);
        assert!(apply(parse(&d).unwrap().2, b"base").is_err());
    }
}
