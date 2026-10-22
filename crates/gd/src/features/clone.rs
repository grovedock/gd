use std::path::Path;

use anyhow::{Result, bail};
use grovedock_core::acl::ReadScope;
use grovedock_core::flags::{FeatureFlags, Flag, require};
use grovedock_core::path::components;

use crate::remote::Remote;
use crate::workspace::{Config, Workspace};

/// Create a workspace in `dir` and materialise the ref head (only `config.sparse`
/// paths when given). Cloning a ref that does not exist yet yields an empty workspace.
pub async fn run(remote: &dyn Remote, flags: &dyn FeatureFlags, dir: &Path, config: Config) -> Result<Workspace> {
    if !config.sparse.is_empty() {
        require(flags, Flag::SparseCheckout)?;
    }
    for p in &config.sparse {
        components(p)?;
    }
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} exists and is not empty", dir.display());
    }
    std::fs::create_dir_all(dir)?;
    let mut config = config;
    let scope = remote.whoami().await?;
    if let ReadScope::Prefixes(readable) = &scope {
        // Path-scoped readers: the workspace covers only what they may read.
        config.sparse = restrict(&config.sparse, readable);
    }
    let mut ws = Workspace::init(dir, config)?;
    if let Some(head) = remote.get_ref(&ws.config.ref_name).await? {
        let cs = remote.changeset(&head.to_hex()).await?;
        if scope.is_full() {
            ws.checkout(remote.objects(), cs.id, cs.root_tree).await?;
        } else {
            let paths = ws.config.sparse.clone();
            ws.checkout_scoped(remote, &cs.id.to_hex(), &paths).await?;
            ws.base = Some(cs.id);
            ws.base_landed = true;
            ws.save()?;
        }
    }
    Ok(ws)
}

/// Sparse prefixes limited to readable ones: requested prefixes that are readable, plus
/// readable prefixes nested inside requested ones (all readable prefixes when none requested).
pub fn restrict(requested: &[String], readable: &[String]) -> Vec<String> {
    if requested.is_empty() {
        return readable.to_vec();
    }
    let scope = ReadScope::Prefixes(readable.to_vec());
    let want = ReadScope::Prefixes(requested.to_vec());
    let mut out: Vec<String> = requested.iter().filter(|r| scope.allows(r)).cloned().collect();
    let nested: Vec<String> = readable.iter().filter(|p| want.allows(p) && !out.contains(p)).cloned().collect();
    out.extend(nested);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn restrict_to_readable() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(super::restrict(&[], &s(&["a", "b/c"])), s(&["a", "b/c"]));
        assert_eq!(super::restrict(&s(&["a/x", "b"]), &s(&["a", "b/c"])), s(&["a/x", "b/c"]));
        assert!(super::restrict(&s(&["z"]), &s(&["a"])).is_empty());
    }
}
