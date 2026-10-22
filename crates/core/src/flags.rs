//! Feature flags. Every feature slice checks its flag at its entry point; the flag
//! source is a port so it can be backed by env vars, a file, or a flag service.

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flag {
    Objects,
    Changesets,
    Refs,
    Land,
    History,
    Diff,
    MergeBase,
    Browse,
    SparseCheckout,
    GitImport,
    GitExport,
    Blame,
    Search,
    Permissions,
    DeltaStorage,
    GitBridge,
    Oidc,
    DaemonPrefetch,
    VirtualCheckout,
    LandChecks,
    Locks,
    Web,
}

impl Flag {
    pub const ALL: [Flag; 22] = [
        Flag::Objects,
        Flag::Changesets,
        Flag::Refs,
        Flag::Land,
        Flag::History,
        Flag::Diff,
        Flag::MergeBase,
        Flag::Browse,
        Flag::SparseCheckout,
        Flag::GitImport,
        Flag::GitExport,
        Flag::Blame,
        Flag::Search,
        Flag::Permissions,
        Flag::DeltaStorage,
        Flag::GitBridge,
        Flag::Oidc,
        Flag::DaemonPrefetch,
        Flag::VirtualCheckout,
        Flag::LandChecks,
        Flag::Locks,
        Flag::Web,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Flag::Objects => "objects",
            Flag::Changesets => "changesets",
            Flag::Refs => "refs",
            Flag::Land => "land",
            Flag::History => "history",
            Flag::Diff => "diff",
            Flag::MergeBase => "merge-base",
            Flag::Browse => "browse",
            Flag::SparseCheckout => "sparse-checkout",
            Flag::GitImport => "git-import",
            Flag::GitExport => "git-export",
            Flag::Blame => "blame",
            Flag::Search => "search",
            Flag::Permissions => "permissions",
            Flag::DeltaStorage => "delta-storage",
            Flag::GitBridge => "git-bridge",
            Flag::Oidc => "oidc",
            Flag::DaemonPrefetch => "daemon-prefetch",
            Flag::VirtualCheckout => "virtual-checkout",
            Flag::LandChecks => "land-checks",
            Flag::Locks => "locks",
            Flag::Web => "web",
        }
    }

    pub fn from_key(key: &str) -> Option<Flag> {
        Flag::ALL.into_iter().find(|f| f.key() == key)
    }
}

pub trait FeatureFlags: Send + Sync {
    fn enabled(&self, flag: Flag) -> bool;
}

/// Guard used at the top of every feature entry point.
pub fn require(flags: &dyn FeatureFlags, flag: Flag) -> Result<()> {
    if flags.enabled(flag) { Ok(()) } else { Err(Error::Disabled(flag.key())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_roundtrip_and_are_unique() {
        let mut keys: Vec<_> = Flag::ALL.iter().map(|f| f.key()).collect();
        for f in Flag::ALL {
            assert_eq!(Flag::from_key(f.key()), Some(f));
        }
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), Flag::ALL.len());
    }

    #[test]
    fn require_maps_to_disabled() {
        struct Off;
        impl FeatureFlags for Off {
            fn enabled(&self, _: Flag) -> bool {
                false
            }
        }
        assert!(matches!(require(&Off, Flag::Land), Err(Error::Disabled("land"))));
    }
}
