//! SQLite adapter for single-node deployments and local development.
//!
//! WAL mode, one writer connection plus a pool of reader connections so reads never
//! block on writes. Reads run on tokio's blocking pool. Writes go through a queue to one
//! writer thread that commits whatever has queued up as a single transaction (group
//! commit): at 1000 concurrent agents, one transaction per write left the land path waiting
//! behind hundreds of threads contending for the writer.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use async_trait::async_trait;
use bytes::Bytes;
use grovedock_core::ports::{ChangesetLog, DerivedStore, ObjectStore, RefStore};
use grovedock_core::{Id, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::storage_err;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS objects    (id BLOB PRIMARY KEY, payload BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS changesets (id BLOB PRIMARY KEY, data BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS refs       (name TEXT PRIMARY KEY, id BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS derived    (kind TEXT NOT NULL, id BLOB NOT NULL, value BLOB NOT NULL,
                                       PRIMARY KEY (kind, id)) WITHOUT ROWID;
";

/// Most writes committed in one transaction.
const MAX_BATCH: usize = 512;

/// Runs one write; returns whether it succeeded and how to report the outcome once the
/// batch's commit is known.
type Job = Box<dyn FnOnce(&mut Connection) -> (bool, Deliver) + Send>;
type Deliver = Box<dyn FnOnce(bool) + Send>;

/// `alone`: run outside any transaction (VACUUM cannot run inside one).
struct Queued {
    alone: bool,
    job: Job,
}

pub struct SqliteStore {
    writer: mpsc::Sender<Queued>,
    readers: Arc<Vec<Mutex<Connection>>>,
    next: AtomicUsize,
}

/// `synchronous=FULL` (the default): a write is on disk before its caller sees Ok, so an
/// acknowledged land survives power loss. NORMAL (`--sqlite-synchronous normal`) is durable
/// across application crashes only: an OS crash or power loss can drop the last transactions,
/// never corrupt the database.
static SYNC_FULL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Chooses FULL (true) or NORMAL for stores opened from now on.
pub fn set_synchronous_full(full: bool) {
    SYNC_FULL.store(full, Ordering::Relaxed);
}

fn tune(c: &Connection) -> rusqlite::Result<()> {
    let sync = if SYNC_FULL.load(Ordering::Relaxed) { "FULL" } else { "NORMAL" };
    c.pragma_update(None, "synchronous", sync)?;
    // No mmap: SQLite remaps the file on every read transaction after the file grows, which
    // dominated reads under concurrent writes (measured: compaction 1.4-2x faster without
    // it, clones unchanged). ponytail: 64 MB page cache per connection, so at most ~1.1 GB
    // across 16 readers and the writer on a busy, large store; make it configurable if
    // small hosts need less.
    c.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
         PRAGMA cache_size=-65536; PRAGMA mmap_size=0; PRAGMA temp_store=MEMORY;",
    )
}

fn id_of(b: Vec<u8>) -> rusqlite::Result<Id> {
    Id::from_slice(&b).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
}

impl SqliteStore {
    /// Return free pages to the filesystem (after compaction shrank stored objects). In WAL
    /// mode VACUUM writes the whole database through the log, so truncate the log afterwards
    /// or it keeps that space until the next checkpoint.
    pub async fn vacuum(&self) -> Result<()> {
        self.enqueue(true, |c| c.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")).await
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let writer = Connection::open(path).map_err(storage_err)?;
        tune(&writer).map_err(storage_err)?;
        writer.execute_batch(SCHEMA).map_err(storage_err)?;
        let n = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16);
        let readers = (0..n)
            .map(|_| {
                let c = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                tune(&c)?;
                Ok(Mutex::new(c))
            })
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err)?;
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("sqlite-writer".into())
            .spawn(move || write_loop(writer, rx))
            .map_err(storage_err)?;
        Ok(Self { writer: tx, readers: Arc::new(readers), next: AtomicUsize::new(0) })
    }

    async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let readers = self.readers.clone();
        let i = self.next.fetch_add(1, Ordering::Relaxed) % readers.len();
        tokio::task::spawn_blocking(move || f(&readers[i].lock().expect("sqlite reader poisoned")))
            .await
            .map_err(storage_err)?
            .map_err(storage_err)
    }

    async fn list_ids(&self, sql: &'static str, after: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        let after = after.map(|a| a.0.to_vec()).unwrap_or_default();
        self.read(move |c| {
            c.prepare_cached(sql)?.query_map(params![after, limit as i64], |r| id_of(r.get(0)?))?.collect()
        })
        .await
    }

    async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        self.enqueue(false, f).await
    }

    async fn enqueue<T, F>(&self, alone: bool, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move |c| {
            let r = f(c);
            (
                r.is_ok(),
                Box::new(move |committed| {
                    let r = r.and_then(|v| if committed { Ok(v) } else { Err(failed("group commit failed")) });
                    let _ = tx.send(r);
                }),
            )
        });
        self.writer.send(Queued { alone, job }).map_err(|_| storage_err(failed("sqlite writer stopped")))?;
        rx.await.map_err(|_| storage_err(failed("sqlite writer stopped")))?.map_err(storage_err)
    }
}

fn failed(msg: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT), Some(msg.to_owned()))
}

/// The writer thread: take whatever has queued (up to [`MAX_BATCH`]), run each write in its
/// own savepoint so a failing one is rolled back alone, commit once, then report. Results
/// are reported only after the commit, so a write is durable when its caller sees Ok.
fn write_loop(mut c: Connection, rx: mpsc::Receiver<Queued>) {
    let mut held: Option<Queued> = None;
    while let Some(first) = held.take().or_else(|| rx.recv().ok()) {
        let mut batch = vec![first];
        if !batch[0].alone {
            while batch.len() < MAX_BATCH {
                match rx.try_recv() {
                    Ok(q) if q.alone => {
                        held = Some(q);
                        break;
                    }
                    Ok(q) => batch.push(q),
                    Err(_) => break,
                }
            }
        }
        if batch.len() == 1 {
            let (_, deliver) = (batch.pop().unwrap().job)(&mut c);
            deliver(true);
            continue;
        }
        let mut delivers = Vec::with_capacity(batch.len());
        let committed = c.execute_batch("BEGIN IMMEDIATE").is_ok() && {
            for q in batch {
                let sp = c.execute_batch("SAVEPOINT w").is_ok();
                let (ok, deliver) = (q.job)(&mut c);
                if sp {
                    let _ = c.execute_batch(if ok { "RELEASE w" } else { "ROLLBACK TO w; RELEASE w" });
                }
                delivers.push(deliver);
            }
            c.execute_batch("COMMIT").is_ok()
        };
        if !committed {
            let _ = c.execute_batch("ROLLBACK");
        }
        for d in delivers {
            d(committed);
        }
    }
}

#[async_trait]
impl ObjectStore for SqliteStore {
    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        let id = *id;
        self.read(move |c| {
            c.prepare_cached("SELECT payload FROM objects WHERE id = ?1")?
                .query_row([&id.0[..]], |r| r.get::<_, Vec<u8>>(0))
                .optional()
                .map(|o| o.map(Bytes::from))
        })
        .await
    }

    async fn put(&self, id: Id, payload: Bytes) -> Result<()> {
        self.put_many(vec![(id, payload)]).await
    }

    async fn put_many(&self, items: Vec<(Id, Bytes)>) -> Result<()> {
        self.write(move |c| {
            // A savepoint, not a transaction: it nests inside the writer's batch.
            let tx = c.savepoint()?;
            {
                let mut stmt = tx.prepare_cached("INSERT OR IGNORE INTO objects (id, payload) VALUES (?1, ?2)")?;
                for (id, p) in &items {
                    stmt.execute(params![&id.0[..], &p[..]])?;
                }
            }
            tx.commit()
        })
        .await
    }

    async fn has(&self, id: &Id) -> Result<bool> {
        Ok(self.missing(std::slice::from_ref(id)).await?.is_empty())
    }

    async fn replace(&self, id: Id, payload: Bytes) -> Result<bool> {
        self.write(move |c| {
            c.prepare_cached("UPDATE objects SET payload = ?1 WHERE id = ?2")?
                .execute(params![&payload[..], &id.0[..]])
                .map(|n| n == 1)
        })
        .await
    }

    async fn get_many(&self, ids: &[Id]) -> Result<Vec<Option<Bytes>>> {
        let ids = ids.to_vec();
        self.read(move |c| {
            let mut stmt = c.prepare_cached("SELECT payload FROM objects WHERE id = ?1")?;
            ids.iter()
                .map(|id| {
                    stmt.query_row([&id.0[..]], |r| r.get::<_, Vec<u8>>(0)).optional().map(|o| o.map(Bytes::from))
                })
                .collect()
        })
        .await
    }

    async fn missing(&self, ids: &[Id]) -> Result<Vec<Id>> {
        let ids = ids.to_vec();
        self.read(move |c| {
            let mut stmt = c.prepare_cached("SELECT 1 FROM objects WHERE id = ?1")?;
            let mut out = Vec::new();
            for id in ids {
                if !stmt.exists([&id.0[..]])? {
                    out.push(id);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn list(&self, after: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        self.list_ids("SELECT id FROM objects WHERE id > ?1 ORDER BY id LIMIT ?2", after, limit).await
    }
}

#[async_trait]
impl ChangesetLog for SqliteStore {
    async fn append(&self, id: Id, encoded: Bytes) -> Result<()> {
        self.write(move |c| {
            c.prepare_cached("INSERT OR IGNORE INTO changesets (id, data) VALUES (?1, ?2)")?
                .execute(params![&id.0[..], &encoded[..]])
                .map(drop)
        })
        .await
    }

    async fn get(&self, id: &Id) -> Result<Option<Bytes>> {
        let id = *id;
        self.read(move |c| {
            c.prepare_cached("SELECT data FROM changesets WHERE id = ?1")?
                .query_row([&id.0[..]], |r| r.get::<_, Vec<u8>>(0))
                .optional()
                .map(|o| o.map(Bytes::from))
        })
        .await
    }

    async fn list(&self, after: Option<Id>, limit: usize) -> Result<Vec<Id>> {
        self.list_ids("SELECT id FROM changesets WHERE id > ?1 ORDER BY id LIMIT ?2", after, limit).await
    }
}

#[async_trait]
impl RefStore for SqliteStore {
    async fn get(&self, name: &str) -> Result<Option<Id>> {
        let name = name.to_owned();
        // Autocommit reads in WAL mode see every transaction committed before they start.
        self.read(move |c| {
            c.prepare_cached("SELECT id FROM refs WHERE name = ?1")?
                .query_row([&name], |r| r.get::<_, Vec<u8>>(0))
                .optional()?
                .map(id_of)
                .transpose()
        })
        .await
    }

    async fn compare_and_swap(&self, name: &str, expected: Option<Id>, new: Id) -> Result<bool> {
        let name = name.to_owned();
        self.write(move |c| {
            let changed = match expected {
                None => c
                    .prepare_cached("INSERT OR IGNORE INTO refs (name, id) VALUES (?1, ?2)")?
                    .execute(params![name, &new.0[..]])?,
                Some(old) => c
                    .prepare_cached("UPDATE refs SET id = ?1 WHERE name = ?2 AND id = ?3")?
                    .execute(params![&new.0[..], name, &old.0[..]])?,
            };
            Ok(changed == 1)
        })
        .await
    }

    async fn list(&self) -> Result<Vec<(String, Id)>> {
        self.read(|c| {
            c.prepare_cached("SELECT name, id FROM refs ORDER BY name")?
                .query_map([], |r| Ok((r.get::<_, String>(0)?, id_of(r.get(1)?)?)))?
                .collect()
        })
        .await
    }
}

#[async_trait]
impl DerivedStore for SqliteStore {
    async fn get(&self, kind: &str, id: &Id) -> Result<Option<Bytes>> {
        let (kind, id) = (kind.to_owned(), *id);
        self.read(move |c| {
            c.prepare_cached("SELECT value FROM derived WHERE kind = ?1 AND id = ?2")?
                .query_row(params![kind, &id.0[..]], |r| r.get::<_, Vec<u8>>(0))
                .optional()
                .map(|o| o.map(Bytes::from))
        })
        .await
    }

    async fn put(&self, kind: &str, id: Id, value: Bytes) -> Result<()> {
        let kind = kind.to_owned();
        self.write(move |c| {
            c.prepare_cached("INSERT OR REPLACE INTO derived (kind, id, value) VALUES (?1, ?2, ?3)")?
                .execute(params![kind, &id.0[..], &value[..]])
                .map(drop)
        })
        .await
    }

    async fn list(&self, kind: &str, after: Option<Id>, limit: usize) -> Result<Vec<(Id, Bytes)>> {
        let kind = kind.to_owned();
        let after = after.map(|a| a.0.to_vec()).unwrap_or_default();
        self.read(move |c| {
            c.prepare_cached("SELECT id, value FROM derived WHERE kind = ?1 AND id > ?2 ORDER BY id LIMIT ?3")?
                .query_map(params![kind, after, limit as i64], |r| {
                    Ok((id_of(r.get(0)?)?, Bytes::from(r.get::<_, Vec<u8>>(1)?)))
                })?
                .collect()
        })
        .await
    }
}
