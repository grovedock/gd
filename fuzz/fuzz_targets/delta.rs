//! Delta payloads written by compaction, applied against an arbitrary base. Input layout:
//! a 2-byte little-endian payload length, the payload, then the base.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((len, rest)) = data.split_first_chunk::<2>() else { return };
    let n = (u16::from_le_bytes(*len) as usize).min(rest.len());
    let (payload, base) = rest.split_at(n);
    let _ = grovedock_core::delta::parse(payload);
    let _ = grovedock_core::delta::decode_with_base(payload, base);
});
