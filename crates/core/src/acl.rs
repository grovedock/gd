//! Access policy port: per-directory read and write permissions and per-ref permissions.
//!
//! Objects are content-addressed and parent trees list their children's ids, so a principal
//! with a restricted [`ReadScope`] never reads objects by id: the server denies raw object
//! reads to them and serves path-scoped, filtered views instead (checkout, browse, redacted
//! history).

use crate::{Error, Result};

/// Authenticated identity of the caller. `anonymous` when auth is disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal(pub String);

impl Principal {
    pub fn anonymous() -> Self {
        Self("anonymous".into())
    }
}

/// What a principal may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadScope {
    All,
    /// Repo-relative path prefixes (a prefix covers itself and everything below it).
    Prefixes(Vec<String>),
}

fn under(path: &str, prefix: &str) -> bool {
    path == prefix || (path.len() > prefix.len() && path.starts_with(prefix) && path.as_bytes()[prefix.len()] == b'/')
}

impl ReadScope {
    pub fn is_full(&self) -> bool {
        matches!(self, ReadScope::All)
    }

    /// May the content at `path` (file or directory) be read?
    pub fn allows(&self, path: &str) -> bool {
        match self {
            ReadScope::All => true,
            ReadScope::Prefixes(ps) => ps.iter().any(|p| under(path, p)),
        }
    }

    /// May directory `dir` ("" = root) be listed? True when it is readable or leads to
    /// something readable; listings then show only entries that pass this check or
    /// [`ReadScope::allows`].
    pub fn traversable(&self, dir: &str) -> bool {
        match self {
            ReadScope::All => true,
            ReadScope::Prefixes(ps) => dir.is_empty() || ps.iter().any(|p| under(dir, p) || under(p, dir)),
        }
    }
}

pub trait AccessPolicy: Send + Sync {
    fn can_write(&self, who: &Principal, path: &str) -> bool;
    fn can_update_ref(&self, who: &Principal, ref_name: &str) -> bool;
    /// Why `who` may not update `ref_name`, when the policy can say more than "not allowed".
    fn why_not_ref(&self, _who: &Principal, _ref_name: &str) -> Option<String> {
        None
    }
    /// The author recorded for a change `who` creates claiming `claimed`. By default the claim
    /// stands, as in git; a policy can insist on the authenticated identity.
    fn author_for(&self, _who: &Principal, claimed: &str) -> String {
        claimed.to_owned()
    }
    /// Defaults to the whole repository, which is what policies without read rules mean.
    fn read_scope(&self, _who: &Principal) -> ReadScope {
        ReadScope::All
    }
}

/// The scope to enforce: everything when the `permissions` feature is off.
pub fn effective_scope(
    flags: &dyn crate::flags::FeatureFlags,
    policy: &dyn AccessPolicy,
    who: &Principal,
) -> ReadScope {
    if flags.enabled(crate::flags::Flag::Permissions) { policy.read_scope(who) } else { ReadScope::All }
}

/// A changeset with the changes outside `scope` removed. Returns the original (and
/// `false`) when nothing had to be hidden.
pub fn redact(cs: &crate::object::Changeset, scope: &ReadScope) -> (crate::object::Changeset, bool) {
    if scope.is_full() || cs.changes.iter().all(|c| scope.allows(&c.path)) {
        return (cs.clone(), false);
    }
    let mut out = cs.clone();
    out.changes.retain(|c| scope.allows(&c.path));
    (out, true)
}

pub fn require_read(scope: &ReadScope, who: &Principal, path: &str) -> Result<()> {
    if scope.allows(path) { Ok(()) } else { Err(Error::PermissionDenied(format!("{} may not read `{path}`", who.0))) }
}

/// Raw object reads by id are for principals who may read everything.
pub fn require_full_read(scope: &ReadScope, who: &Principal) -> Result<()> {
    if scope.is_full() {
        Ok(())
    } else {
        Err(Error::PermissionDenied(format!(
            "{} has path-scoped read access; use the path-scoped APIs (gd clone/sync use them automatically)",
            who.0
        )))
    }
}

/// Allows everything.
pub struct AllowAllPolicy;
impl AccessPolicy for AllowAllPolicy {
    fn can_write(&self, _: &Principal, _: &str) -> bool {
        true
    }
    fn can_update_ref(&self, _: &Principal, _: &str) -> bool {
        true
    }
}

pub fn require_write<'a>(
    policy: &dyn AccessPolicy,
    who: &Principal,
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let denied: Vec<&str> = paths.into_iter().filter(|p| !policy.can_write(who, p)).take(10).collect();
    if denied.is_empty() {
        Ok(())
    } else {
        Err(Error::PermissionDenied(format!("{} may not write {}", who.0, denied.join(", "))))
    }
}

pub fn require_ref(policy: &dyn AccessPolicy, who: &Principal, ref_name: &str) -> Result<()> {
    if policy.can_update_ref(who, ref_name) {
        Ok(())
    } else {
        Err(Error::PermissionDenied(
            policy.why_not_ref(who, ref_name).unwrap_or_else(|| format!("{} may not update ref `{ref_name}`", who.0)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_rules() {
        let s = ReadScope::Prefixes(vec!["services/api".into(), "docs".into()]);
        assert!(s.allows("services/api") && s.allows("services/api/main.rs") && s.allows("docs/x/y.md"));
        assert!(!s.allows("services/api2/x") && !s.allows("services") && !s.allows("secrets/key"));
        assert!(s.traversable("") && s.traversable("services") && s.traversable("services/api/sub"));
        assert!(!s.traversable("secrets") && !s.traversable("services/web"));
        assert!(ReadScope::All.allows("anything") && ReadScope::All.traversable("x"));
    }

    #[test]
    fn redaction() {
        use crate::object::{Changeset, FileChange, Mode};
        let cs = Changeset {
            changes: vec![
                FileChange::upsert("docs/a", Mode::File, crate::Id([1; 32])),
                FileChange::delete("secrets/b"),
            ],
            ..Default::default()
        };
        let (r, hidden) = redact(&cs, &ReadScope::Prefixes(vec!["docs".into()]));
        assert!(hidden);
        assert_eq!(r.changes.len(), 1);
        assert_eq!(redact(&cs, &ReadScope::All), (cs.clone(), false));
    }
}
