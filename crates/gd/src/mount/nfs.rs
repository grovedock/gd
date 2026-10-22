//! Serve a [`VirtualTree`] as a read-only NFSv3 server on localhost, for the system's own
//! NFS client to mount. On macOS this needs no kernel extension and nothing to install,
//! which is why EdenFS moved to NFS there as Apple deprecated kernel extensions.
//!
//! NFS never tells the server when the client drops a file handle, so every inode handed
//! out stays valid for the life of the mount; [`VirtualTree`] already never forgets one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use grovedock_core::object::Mode;
use nfsserve::nfs::{fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfsstring, nfstime3, sattr3, specdata3};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};

use super::{Attr, ROOT, VirtualTree};

pub struct NfsTree {
    tree: Arc<VirtualTree>,
    /// Directory each inode was found in, so `..` resolves.
    parents: Mutex<HashMap<u64, u64>>,
    uid: u32,
    gid: u32,
    mounted_at: nfstime3,
}

impl NfsTree {
    pub fn new(tree: Arc<VirtualTree>, uid: u32, gid: u32) -> Self {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        Self {
            tree,
            parents: Mutex::new(HashMap::from([(ROOT, ROOT)])),
            uid,
            gid,
            mounted_at: nfstime3 { seconds: now.as_secs() as u32, nseconds: now.subsec_nanos() },
        }
    }

    /// Run blocking tree work (it may fetch from the server) off the async workers.
    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&VirtualTree) -> anyhow::Result<T> + Send + 'static,
    ) -> Result<T, nfsstat3> {
        let tree = self.tree.clone();
        match tokio::task::spawn_blocking(move || f(&tree)).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) if e.to_string().contains("stale inode") => Err(nfsstat3::NFS3ERR_STALE),
            _ => Err(nfsstat3::NFS3ERR_IO),
        }
    }

    fn fattr(&self, a: &Attr) -> fattr3 {
        let (ftype, mode, nlink) = match a.mode {
            Mode::Dir => (ftype3::NF3DIR, 0o555, 2),
            Mode::Exec => (ftype3::NF3REG, 0o555, 1),
            Mode::Symlink => (ftype3::NF3LNK, 0o777, 1),
            Mode::File => (ftype3::NF3REG, 0o444, 1),
        };
        fattr3 {
            ftype,
            mode,
            nlink,
            uid: self.uid,
            gid: self.gid,
            size: a.size,
            used: a.size,
            rdev: specdata3::default(),
            fsid: 0,
            fileid: a.ino,
            atime: self.mounted_at,
            mtime: self.mounted_at,
            ctime: self.mounted_at,
        }
    }
}

fn name(n: &filename3) -> Result<String, nfsstat3> {
    String::from_utf8(n.0.clone()).map_err(|_| nfsstat3::NFS3ERR_NOENT)
}

#[async_trait]
impl NFSFileSystem for NfsTree {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    fn root_dir(&self) -> fileid3 {
        ROOT
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let n = name(filename)?;
        match n.as_str() {
            "." => return Ok(dirid),
            ".." => return Ok(*self.parents.lock().expect("parents").get(&dirid).unwrap_or(&ROOT)),
            _ => {}
        }
        let dir = self.run(move |t| t.attr(dirid)).await?;
        if dir.mode != Mode::Dir {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        match self.run(move |t| t.lookup(dirid, &n)).await? {
            Some(a) => {
                self.parents.lock().expect("parents").insert(a.ino, dirid);
                Ok(a.ino)
            }
            None => Err(nfsstat3::NFS3ERR_NOENT),
        }
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let a = self.run(move |t| t.attr(id)).await?;
        Ok(self.fattr(&a))
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
        let a = self.run(move |t| t.attr(id)).await?;
        if a.mode == Mode::Dir {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }
        let data = self.run(move |t| t.read(id, offset, count)).await?;
        let eof = offset + data.len() as u64 >= a.size;
        Ok((data, eof))
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let dir = self.run(move |t| t.attr(dirid)).await?;
        if dir.mode != Mode::Dir {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let all = self.run(move |t| t.readdir(dirid)).await?;
        let from = if start_after == 0 {
            0
        } else {
            all.iter().position(|(ino, _, _)| *ino == start_after).map(|i| i + 1).ok_or(nfsstat3::NFS3ERR_BAD_COOKIE)?
        };
        let page: Vec<u64> = all[from..].iter().take(max_entries).map(|(ino, _, _)| *ino).collect();
        let attrs = self.run(move |t| page.iter().map(|ino| t.attr(*ino)).collect::<anyhow::Result<Vec<_>>>()).await?;
        let mut parents = self.parents.lock().expect("parents");
        let entries: Vec<DirEntry> = attrs
            .iter()
            .zip(&all[from..])
            .map(|(a, (_, n, _))| {
                parents.insert(a.ino, dirid);
                DirEntry { fileid: a.ino, name: nfsstring(n.as_bytes().to_vec()), attr: self.fattr(a) }
            })
            .collect();
        let end = from + entries.len() >= all.len();
        Ok(ReadDirResult { entries, end })
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        Ok(nfsstring(self.run(move |t| t.readlink(id)).await?))
    }

    async fn setattr(&self, _: fileid3, _: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
    async fn write(&self, _: fileid3, _: u64, _: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
    async fn create(&self, _: fileid3, _: &filename3, _: sattr3) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
    async fn create_exclusive(&self, _: fileid3, _: &filename3) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
    async fn mkdir(&self, _: fileid3, _: &filename3) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
    async fn remove(&self, _: fileid3, _: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
    async fn rename(&self, _: fileid3, _: &filename3, _: fileid3, _: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
    async fn symlink(
        &self,
        _: fileid3,
        _: &filename3,
        _: &nfspath3,
        _: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
}

/// Start the NFS server for `tree` on a free localhost port; returns the port. The server
/// runs until the process exits.
pub async fn serve(tree: Arc<VirtualTree>, uid: u32, gid: u32) -> anyhow::Result<u16> {
    let listener = NFSTcpListener::bind("127.0.0.1:0", NfsTree::new(tree, uid, gid)).await?;
    let port = listener.get_listen_port();
    tokio::spawn(async move { listener.handle_forever().await });
    Ok(port)
}

/// Mount the server on `port` at `dir` with the system NFS client (macOS `mount_nfs`).
pub fn mount(port: u16, dir: &std::path::Path) -> anyhow::Result<()> {
    let status =
        std::process::Command::new("mount_nfs").args(["-o", &options(port), "localhost:/"]).arg(dir).status()?;
    anyhow::ensure!(status.success(), "mount_nfs failed ({status}); is {} an empty directory you own?", dir.display());
    Ok(())
}

/// `mount_nfs` options for the server on `port`: read-only, no lock daemon, and the flags
/// an unprivileged mount needs (no reserved port, no setuid or device files).
pub fn options(port: u16) -> String {
    format!("nolocks,noresvport,nosuid,nodev,vers=3,tcp,rdonly,rsize=1048576,actimeo=120,port={port},mountport={port}")
}

/// The command that mounts the server on `port` at `dir` on this platform.
pub fn mount_command(port: u16, dir: &std::path::Path) -> String {
    if cfg!(target_os = "macos") {
        format!("mount_nfs -o {} localhost:/ {}", options(port), dir.display())
    } else {
        format!("sudo mount -t nfs -o ro,nolock,vers=3,tcp,port={port},mountport={port} 127.0.0.1:/ {}", dir.display())
    }
}

pub fn unmount(dir: &std::path::Path) -> anyhow::Result<()> {
    let status = std::process::Command::new("umount").arg(dir).status()?;
    anyhow::ensure!(status.success(), "umount {} failed ({status})", dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use grovedock_adapters::memory::MemoryStore;
    use grovedock_core::object::FileChange;
    use grovedock_core::ports::ObjectStore;
    use grovedock_core::tree::{apply_changes, put_blob};

    use super::*;

    fn n(s: &str) -> filename3 {
        nfsstring(s.as_bytes().to_vec())
    }

    /// The NFS adapter over a small tree: lookups (including `.` and `..`), attributes,
    /// paged directory listings, reads with end-of-file, symlinks, and read-only errors.
    #[tokio::test(flavor = "multi_thread")]
    async fn serves_a_tree_read_only() {
        let store = Arc::new(MemoryStore::new());
        let mut changes = Vec::new();
        for i in 0..25 {
            let blob = put_blob(store.as_ref(), format!("file {i}\n").as_bytes()).await.unwrap();
            changes.push(FileChange::upsert(format!("docs/f{i:02}.txt"), Mode::File, blob));
        }
        let run = put_blob(store.as_ref(), b"#!/bin/sh\necho hi\n").await.unwrap();
        changes.push(FileChange::upsert("bin/run", Mode::Exec, run));
        let target = put_blob(store.as_ref(), b"../docs/f00.txt").await.unwrap();
        changes.push(FileChange::upsert("bin/link", Mode::Symlink, target));
        let root = apply_changes(store.as_ref() as &dyn ObjectStore, None, &changes).await.unwrap();
        let tree = Arc::new(VirtualTree::new(store, tokio::runtime::Handle::current(), root, 1 << 20));
        let fs = NfsTree::new(tree, 501, 20);

        let docs = fs.lookup(ROOT, &n("docs")).await.unwrap();
        assert_eq!(fs.lookup(docs, &n(".")).await.unwrap(), docs);
        assert_eq!(fs.lookup(docs, &n("..")).await.unwrap(), ROOT);
        assert!(matches!(fs.lookup(ROOT, &n("nope")).await, Err(nfsstat3::NFS3ERR_NOENT)));
        let d = fs.getattr(docs).await.unwrap();
        assert!(matches!(d.ftype, ftype3::NF3DIR));
        assert_eq!((d.mode, d.uid, d.gid), (0o555, 501, 20));

        // Paging: 25 entries in pages of 10, resuming after the last id of each page.
        let (mut names, mut after) = (Vec::new(), 0);
        loop {
            let page = fs.readdir(docs, after, 10).await.unwrap();
            assert!(page.entries.len() <= 10);
            names.extend(page.entries.iter().map(|e| String::from_utf8(e.name.0.clone()).unwrap()));
            after = page.entries.last().map_or(after, |e| e.fileid);
            if page.end {
                break;
            }
        }
        assert_eq!(names, (0..25).map(|i| format!("f{i:02}.txt")).collect::<Vec<_>>());

        let f = fs.lookup(docs, &n("f07.txt")).await.unwrap();
        assert_eq!(fs.getattr(f).await.unwrap().size, 7);
        assert_eq!(fs.read(f, 0, 4).await.unwrap(), (b"file".to_vec(), false));
        assert_eq!(fs.read(f, 4, 100).await.unwrap(), (b" 7\n".to_vec(), true));
        assert!(matches!(fs.read(docs, 0, 1).await, Err(nfsstat3::NFS3ERR_ISDIR)));
        assert!(matches!(fs.lookup(f, &n("x")).await, Err(nfsstat3::NFS3ERR_NOTDIR)));

        let bin = fs.lookup(ROOT, &n("bin")).await.unwrap();
        let run = fs.lookup(bin, &n("run")).await.unwrap();
        assert_eq!(fs.getattr(run).await.unwrap().mode, 0o555);
        let link = fs.lookup(bin, &n("link")).await.unwrap();
        assert!(matches!(fs.getattr(link).await.unwrap().ftype, ftype3::NF3LNK));
        assert_eq!(fs.readlink(link).await.unwrap().0, b"../docs/f00.txt");

        assert!(matches!(fs.write(f, 0, b"x").await, Err(nfsstat3::NFS3ERR_ROFS)));
        assert!(matches!(fs.remove(docs, &n("f00.txt")).await, Err(nfsstat3::NFS3ERR_ROFS)));
        assert!(matches!(fs.getattr(999_999).await, Err(nfsstat3::NFS3ERR_STALE)));
    }
}
