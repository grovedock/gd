//! FUSE adapter (Linux): translates kernel requests into [`VirtualTree`] calls.

use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use fuser::{
    BackgroundSession, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo, LockOwner,
    MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, Request,
};
use grovedock_core::object::Mode;

use super::{Attr, VirtualTree};

/// Content never changes under an inode (a mount is one immutable revision), so the
/// kernel may cache attributes and entries for as long as it likes.
const TTL: Duration = Duration::from_secs(3600);

struct Fs {
    tree: Arc<VirtualTree>,
    /// Files are owned by whoever owns the mount point.
    owner: (u32, u32),
}

fn kind(mode: Mode) -> FileType {
    match mode {
        Mode::Dir => FileType::Directory,
        Mode::Symlink => FileType::Symlink,
        Mode::File | Mode::Exec => FileType::RegularFile,
    }
}

fn file_attr(a: &Attr, (uid, gid): (u32, u32)) -> FileAttr {
    let perm = match a.mode {
        Mode::Dir | Mode::Exec => 0o555,
        Mode::Symlink => 0o777,
        Mode::File => 0o444,
    };
    FileAttr {
        ino: INodeNo(a.ino),
        size: a.size,
        blocks: a.size.div_ceil(512),
        atime: SystemTime::UNIX_EPOCH,
        mtime: SystemTime::UNIX_EPOCH,
        ctime: SystemTime::UNIX_EPOCH,
        crtime: SystemTime::UNIX_EPOCH,
        kind: kind(a.mode),
        perm,
        nlink: if a.mode == Mode::Dir { 2 } else { 1 },
        uid,
        gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn errno(e: &anyhow::Error) -> Errno {
    let msg = e.to_string();
    if msg.contains("not a directory") {
        Errno::ENOTDIR
    } else if msg.contains("is a directory") {
        Errno::EISDIR
    } else if msg.contains("not a symlink") {
        Errno::EINVAL
    } else {
        eprintln!("gd mount: {e:#}");
        Errno::EIO
    }
}

impl Filesystem for Fs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str() else { return reply.error(Errno::ENOENT) };
        match self.tree.lookup(parent.0, name) {
            Ok(Some(a)) => reply.entry(&TTL, &file_attr(&a, self.owner), Generation(0)),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.tree.attr(ino.0) {
            Ok(a) => reply.attr(&TTL, &file_attr(&a, self.owner)),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        match self.tree.readlink(ino.0) {
            Ok(t) => reply.data(&t),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.tree.read(ino.0, offset, size) {
            Ok(d) => reply.data(&d),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let entries = match self.tree.readdir(ino.0) {
            Ok(e) => e,
            Err(e) => return reply.error(errno(&e)),
        };
        // Offsets are 1-based positions: "." = 1, ".." = 2, then the entries.
        let dots = [(ino.0, ".".to_owned(), Mode::Dir), (super::ROOT, "..".to_owned(), Mode::Dir)];
        for (i, (child, name, mode)) in dots.into_iter().chain(entries).enumerate().skip(offset as usize) {
            if reply.add(INodeNo(child), i as u64 + 1, kind(mode), name) {
                break;
            }
        }
        reply.ok();
    }
}

/// Mount `tree` read-only at `dir`; unmounts when the returned session is dropped.
pub fn mount(tree: Arc<VirtualTree>, dir: &Path) -> Result<BackgroundSession> {
    let mut config = Config::default();
    config.mount_options = vec![MountOption::RO, MountOption::FSName("gd".into()), MountOption::Subtype("gd".into())];
    config.n_threads = Some(8);
    let meta = std::fs::metadata(dir)?;
    let fs = Fs { tree, owner: (meta.uid(), meta.gid()) };
    Ok(fuser::spawn_mount(fs, dir, &config)?)
}
