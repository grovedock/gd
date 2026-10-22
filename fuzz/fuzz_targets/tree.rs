//! Tree objects name every path a checkout writes; parsing hostile bytes must fail cleanly.
#![no_main]
use libfuzzer_sys::fuzz_target;
use grovedock_core::Id;
use grovedock_core::tree::parse_tree;

fuzz_target!(|data: &[u8]| {
    if data.len() < 32 {
        return;
    }
    let (id, content) = data.split_at(32);
    let _ = parse_tree(&Id::from_slice(id).unwrap(), content);
});
