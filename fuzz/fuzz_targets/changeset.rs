//! Changesets arrive from clients and from other replicas' stores.
#![no_main]
use libfuzzer_sys::fuzz_target;
use grovedock_core::Id;
use grovedock_core::object::Changeset;

fuzz_target!(|data: &[u8]| {
    if let Ok(cs) = <Changeset as prost::Message>::decode(data) {
        let _ = cs.parent_ids();
        let _ = cs.first_parent();
        for c in &cs.changes {
            let _ = c.blob_id();
        }
        let (id, bytes) = cs.encode_canonical();
        assert!(Changeset::decode_verified(&id, &bytes).is_ok(), "canonical encoding must verify");
    }
    if data.len() >= 32 {
        let (id, bytes) = data.split_at(32);
        let _ = Changeset::decode_verified(&Id::from_slice(id).unwrap(), bytes);
    }
});
