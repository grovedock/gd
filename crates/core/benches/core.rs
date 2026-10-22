//! Micro-benchmarks for the claims in the README:
//! * hashing/compression throughput,
//! * commit (apply_changes) and diff cost scale with the CHANGE, not the repo size,
//! * ancestry queries are O(log n) in history length.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use grovedock_core::object::{Changeset, FileChange, Kind, Mode, encode_object};
use grovedock_core::ports::{ChangesetLog, DerivedStore, ObjectStore, RefStore};
use grovedock_core::{Id, Repo, Result, tree};

#[derive(Default)]
struct Mem {
    objects: RwLock<HashMap<Id, Bytes>>,
    log: RwLock<HashMap<Id, Bytes>>,
    derived: RwLock<HashMap<(String, Id), Bytes>>,
}

#[async_trait]
impl ObjectStore for Mem {
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        Ok(self.objects.read().unwrap().get(id).cloned())
    }
    async fn put(&self, id: Id, p: Bytes) -> Result<()> {
        self.objects.write().unwrap().insert(id, p);
        Ok(())
    }
    async fn has(&self, id: &Id) -> Result<bool> {
        Ok(self.objects.read().unwrap().contains_key(id))
    }
}
#[async_trait]
impl ChangesetLog for Mem {
    async fn append(&self, id: Id, b: Bytes) -> Result<()> {
        self.log.write().unwrap().insert(id, b);
        Ok(())
    }
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        Ok(self.log.read().unwrap().get(id).cloned())
    }
}
#[async_trait]
impl RefStore for Mem {
    async fn get(&self, _: &str) -> Result<Option<Id>> {
        Ok(None)
    }
    async fn compare_and_swap(&self, _: &str, _: Option<Id>, _: Id) -> Result<bool> {
        Ok(true)
    }
    async fn list(&self) -> Result<Vec<(String, Id)>> {
        Ok(vec![])
    }
}
#[async_trait]
impl DerivedStore for Mem {
    async fn get(&self, k: &str, id: &Id) -> Result<Option<Bytes>> {
        Ok(self.derived.read().unwrap().get(&(k.to_owned(), *id)).cloned())
    }
    async fn put(&self, k: &str, id: Id, v: Bytes) -> Result<()> {
        self.derived.write().unwrap().insert((k.to_owned(), id), v);
        Ok(())
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().build().unwrap()
}

/// A repo of `n` files spread over a 3-level directory hierarchy (like a monorepo).
fn build_tree(rt: &tokio::runtime::Runtime, store: &Mem, n: usize) -> Id {
    rt.block_on(async {
        let blob = tree::put_blob(store, b"fn main() {}\n").await.unwrap();
        let changes: Vec<FileChange> = (0..n)
            .map(|i| {
                FileChange::upsert(format!("svc{}/mod{}/pkg{}/file{i}.rs", i % 20, i % 400, i % 4000), Mode::File, blob)
            })
            .collect();
        tree::apply_changes(store, None, &changes).await.unwrap()
    })
}

fn bench_encoding(c: &mut Criterion) {
    let mut g = c.benchmark_group("object");
    let data: Vec<u8> = (0..1 << 20).map(|i| (i % 251) as u8 ^ (i >> 7) as u8).collect();
    g.throughput(Throughput::Bytes(data.len() as u64));
    g.bench_function("blake3_id_1MiB", |b| b.iter(|| Id::hash(Kind::Blob as u8, &data)));
    g.bench_function("encode_zstd3_1MiB", |b| b.iter(|| encode_object(Kind::Blob, &data)));
    let (id, payload) = encode_object(Kind::Blob, &data);
    g.bench_function("decode_verify_1MiB", |b| {
        b.iter(|| grovedock_core::object::decode_object(&id, &payload).unwrap())
    });
    g.finish();
}

fn bench_commit_scaling(c: &mut Criterion) {
    let rt = rt();
    let mut g = c.benchmark_group("commit_10_files");
    for n in [1_000usize, 10_000, 100_000] {
        let store = Mem::default();
        let root = build_tree(&rt, &store, n);
        let blob = rt.block_on(tree::put_blob(&store, b"changed\n")).unwrap();
        let changes: Vec<FileChange> =
            (0..10).map(|i| FileChange::upsert(format!("svc{i}/mod{i}/pkg{i}/file{i}.rs"), Mode::File, blob)).collect();
        g.bench_with_input(BenchmarkId::new("repo_files", n), &n, |b, _| {
            b.iter(|| rt.block_on(tree::apply_changes(&store, Some(root), &changes)).unwrap())
        });
    }
    g.finish();

    let mut g = c.benchmark_group("diff_10_files");
    for n in [1_000usize, 10_000, 100_000] {
        let store = Mem::default();
        let root = build_tree(&rt, &store, n);
        let blob = rt.block_on(tree::put_blob(&store, b"changed\n")).unwrap();
        let changes: Vec<FileChange> =
            (0..10).map(|i| FileChange::upsert(format!("svc{i}/mod{i}/pkg{i}/file{i}.rs"), Mode::File, blob)).collect();
        let after = rt.block_on(tree::apply_changes(&store, Some(root), &changes)).unwrap();
        g.bench_with_input(BenchmarkId::new("repo_files", n), &n, |b, _| {
            b.iter(|| rt.block_on(tree::diff_trees(&store, Some(root), Some(after))).unwrap())
        });
    }
    g.finish();
}

fn bench_ancestry(c: &mut Criterion) {
    let rt = rt();
    let mut g = c.benchmark_group("is_ancestor_root_of_head");
    for depth in [1_000usize, 10_000, 100_000] {
        let m = Arc::new(Mem::default());
        let repo = Repo { objects: m.clone(), log: m.clone(), refs: m.clone(), derived: m };
        let (root, head) = rt.block_on(async {
            let mut parent: Option<Id> = None;
            let mut first = None;
            for i in 0..depth {
                let cs = Changeset {
                    parents: parent.map(|p| vec![p.0.to_vec()]).unwrap_or_default(),
                    message: format!("{i}"),
                    ..Default::default()
                };
                let id = repo.create_changeset(cs).await.unwrap();
                first.get_or_insert(id);
                parent = Some(id);
            }
            (first.unwrap(), parent.unwrap())
        });
        g.bench_with_input(BenchmarkId::new("history_len", depth), &depth, |b, _| {
            b.iter(|| assert!(rt.block_on(repo.is_ancestor(root, head)).unwrap()))
        });
    }
    g.finish();
}

criterion_group!(benches, bench_encoding, bench_commit_scaling, bench_ancestry);
criterion_main!(benches);
