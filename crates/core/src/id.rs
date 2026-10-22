use std::fmt;
use std::str::FromStr;

use crate::{Error, Result};

/// 256-bit BLAKE3 content address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(pub [u8; 32]);

impl Id {
    pub const LEN: usize = 32;

    /// Hash `data` under a one-byte domain tag so blobs, trees and changesets with
    /// identical bytes never share an id.
    pub fn hash(tag: u8, data: &[u8]) -> Id {
        let mut h = blake3::Hasher::new();
        h.update(&[tag]);
        h.update(data);
        Id(*h.finalize().as_bytes())
    }

    /// [`Id::hash`] over a stream, without holding the content in memory.
    pub fn hash_reader(tag: u8, reader: impl std::io::Read) -> std::io::Result<Id> {
        let mut h = blake3::Hasher::new();
        h.update(&[tag]);
        h.update_reader(reader)?;
        Ok(Id(*h.finalize().as_bytes()))
    }

    pub fn from_slice(b: &[u8]) -> Result<Id> {
        <[u8; 32]>::try_from(b).map(Id).map_err(|_| Error::invalid(format!("id must be 32 bytes, got {}", b.len())))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// First 12 hex chars, for human display only.
    pub fn short(&self) -> String {
        hex::encode(&self.0[..6])
    }
}

impl FromStr for Id {
    type Err = Error;
    fn from_str(s: &str) -> Result<Id> {
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).map_err(|e| Error::invalid(format!("bad id `{s}`: {e}")))?;
        Ok(Id(out))
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.short())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_and_domain_separation() {
        let a = Id::hash(1, b"x");
        assert_eq!(a.to_hex().parse::<Id>().unwrap(), a);
        assert_ne!(a, Id::hash(2, b"x"));
        assert!("zz".parse::<Id>().is_err());
        assert!(Id::from_slice(&[0; 31]).is_err());
    }

    #[test]
    fn large_input_matches_reference_blake3() {
        let data = vec![7u8; 1 << 20];
        let mut h = blake3::Hasher::new();
        h.update(&[1]);
        h.update(&data);
        assert_eq!(Id::hash(1, &data).0, *h.finalize().as_bytes());
        assert_eq!(Id::hash_reader(1, &data[..]).unwrap(), Id::hash(1, &data));
    }
}
