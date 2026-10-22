//! Checkout-free reads: everything here works without a workspace.

use anyhow::Result;
use grovedock_core::object::Mode;

use crate::remote::{DirEntry, Remote, RemoteChangeset};

pub async fn cat(remote: &dyn Remote, rev: &str, path: &str) -> Result<Vec<u8>> {
    Ok(remote.read_file(rev, path).await?.1)
}

pub async fn ls(remote: &dyn Remote, rev: &str, path: &str) -> Result<Vec<DirEntry>> {
    remote.list_dir(rev, path).await
}

pub async fn log(remote: &dyn Remote, rev: &str, path: &str, limit: u32) -> Result<Vec<RemoteChangeset>> {
    remote.log(rev, path, limit).await
}

pub fn mode_char(m: Mode) -> &'static str {
    match m {
        Mode::File => "file",
        Mode::Exec => "exec",
        Mode::Symlink => "link",
        Mode::Dir => "dir ",
    }
}
