//! Chunk manifests describe large files and are read before any chunk is fetched.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = grovedock_core::chunked::parse_manifest(data);
});
