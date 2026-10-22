//! Stored and transferred object payloads (zstd frames, chunk manifests, deltas) come from
//! the network and disk: decoding any bytes must return an error, never panic or blow up.
#![no_main]
use libfuzzer_sys::fuzz_target;
use grovedock_core::Id;
use grovedock_core::object::{content_size, decode_object};

fuzz_target!(|data: &[u8]| {
    if data.len() < 32 {
        return;
    }
    let (id, payload) = data.split_at(32);
    let id = Id::from_slice(id).unwrap();
    let _ = content_size(payload);
    let _ = decode_object(&id, payload);
});
