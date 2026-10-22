//! The published object schema (`proto/gd_objects.proto`) must encode exactly as the
//! core's types do, since SDKs decode and verify ids with it.

use grovedock_core::Id;
use grovedock_core::chunked::{ChunkRef, Manifest};
use grovedock_core::object::{Changeset, FileChange, Mode, Tree, TreeEntry};
use grovedock_proto::objects as published;
use prost::Message;

#[test]
fn changeset_bytes_are_identical() {
    let core = Changeset {
        parents: vec![vec![1; 32], vec![2; 32]],
        changes: vec![FileChange::upsert("a/b.rs", Mode::Exec, Id([3; 32])), FileChange::delete("gone")],
        author: "dev <d@x>".into(),
        message: "msg\n".into(),
        timestamp_ms: -1_700_000_000_000,
        git: None,
    };
    let bytes = core.encode_to_vec();
    let decoded = published::Changeset::decode(&bytes[..]).unwrap();
    assert_eq!(decoded.encode_to_vec(), bytes);
    assert_eq!(decoded.changes[0].mode, published::Mode::Exec as i32);
    assert_eq!(Changeset::decode(&decoded.encode_to_vec()[..]).unwrap(), core);
}

#[test]
fn tree_and_manifest_bytes_are_identical() {
    let tree = Tree {
        entries: vec![
            TreeEntry { name: "a".into(), mode: Mode::Dir as i32, id: vec![4; 32] },
            TreeEntry { name: "b".into(), mode: Mode::Symlink as i32, id: vec![5; 32] },
        ],
    };
    let bytes = tree.encode_to_vec();
    assert_eq!(published::Tree::decode(&bytes[..]).unwrap().encode_to_vec(), bytes);
    let m = Manifest { size: 10, chunks: vec![ChunkRef { id: vec![6; 32], len: 10 }] };
    let bytes = m.encode_to_vec();
    assert_eq!(published::Manifest::decode(&bytes[..]).unwrap().encode_to_vec(), bytes);
}
