//! Writes valid inputs for every fuzz target to `fuzz/seeds/<target>/`, so coverage-guided
//! fuzzing starts past the checksums and headers instead of rediscovering them.
//!
//!     cargo run --manifest-path fuzz/Cargo.toml --example seed_corpus

use std::fs;
use std::path::Path;

use grovedock_core::object::{Changeset, FileChange, Kind, Mode, Tree, TreeEntry, encode_object};
use grovedock_core::{Id, chunked, delta};
use prost::Message;

fn write(target: &str, name: &str, bytes: &[u8]) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds").join(target);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), bytes).unwrap();
}

fn with_id(id: &Id, payload: &[u8]) -> Vec<u8> {
    [&id.0[..], payload].concat()
}

fn main() {
    let small = b"fn main() {\n    println!(\"hello\");\n}\n".repeat(3);
    let big: Vec<u8> = (0..300_000u32).flat_map(|i| (i.wrapping_mul(2_654_435_761) >> 13).to_le_bytes()).collect();

    // Objects: a blob, a tree, a chunk manifest and a delta, each with its real id.
    let (id, payload) = encode_object(Kind::Blob, &small);
    write("object", "blob", &with_id(&id, &payload));
    let tree = Tree {
        entries: vec![
            TreeEntry { name: "README.md".into(), mode: Mode::File as i32, id: id.0.to_vec() },
            TreeEntry { name: "src".into(), mode: Mode::Dir as i32, id: id.0.to_vec() },
            TreeEntry { name: "run".into(), mode: Mode::Exec as i32, id: id.0.to_vec() },
            TreeEntry { name: "link".into(), mode: Mode::Symlink as i32, id: id.0.to_vec() },
        ],
    }
    .encode_to_vec();
    let (tid, tpayload) = encode_object(Kind::Tree, &tree);
    write("object", "tree", &with_id(&tid, &tpayload));
    write("tree", "tree", &with_id(&tid, &tree));
    let (mid, manifest) = chunked::chunk_reader(&big[..], |_, _| Ok(())).unwrap();
    write("object", "manifest", &with_id(&mid, &manifest));
    write("manifest", "manifest", &manifest);
    let mut edited = small.clone();
    edited.extend_from_slice(b"// edited\n");
    let d = delta::encode(id, 1, &small, &edited).unwrap();
    let (eid, _) = encode_object(Kind::Blob, &edited);
    write("object", "delta", &with_id(&eid, &d));
    let framed = [&(d.len() as u16).to_le_bytes()[..], &d, &small].concat();
    write("delta", "delta", &framed);

    // Changesets: canonical encodings with and without parents.
    let cs = Changeset {
        parents: vec![id.0.to_vec()],
        changes: vec![
            FileChange::upsert("src/main.rs", Mode::File, id),
            FileChange::upsert("bin/run", Mode::Exec, id),
            FileChange::delete("old.txt"),
        ],
        author: "agent-7".into(),
        message: "Add a binary".into(),
        timestamp_ms: 1_760_000_000_000,
    };
    let (cid, bytes) = cs.encode_canonical();
    write("changeset", "with-id", &with_id(&cid, &bytes));
    write("changeset", "plain", &bytes);

    // Paths, accepted and rejected.
    for (name, p) in [("ok", "src/lib/mod.rs"), ("dotdot", "a/../b"), ("abs", "/etc/passwd"), ("meta", ".gd/index"), ("deep", "a/b/c/d/e/f/g/h")] {
        write("path", name, p.as_bytes());
    }
    println!("seeds written to fuzz/seeds/");
}
