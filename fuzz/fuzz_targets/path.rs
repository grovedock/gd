//! Paths come from trees and clients and decide where files are written: anything accepted
//! must be a plain relative path that stays inside the working copy.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|path: &str| {
    if let Ok(parts) = grovedock_core::path::components(path) {
        for p in parts {
            assert!(!p.is_empty() && p != "." && p != ".." && !p.contains('/') && !p.contains('\0'), "accepted `{path}`");
        }
    }
});
