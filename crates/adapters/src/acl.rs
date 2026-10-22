//! JSON rules for the `AccessPolicy` port.
//!
//! ```json
//! {
//!   "default":    { "write": ["*"], "refs": ["user/*"] },
//!   "principals": {
//!     "release-bot": { "refs": ["main", "release/*"] },
//!     "api-team":    { "write": ["services/api", "libs/api-client"], "refs": ["main"] }
//!   }
//! }
//! ```
//! A principal's grants are the union of `default` and its own entry. Read and write
//! grants are path prefixes (`*` = everything); ref grants are exact names or `prefix/*`.
//! When no entry mentions `read`, everyone may read everything (the pre-1.4 behaviour).

use std::collections::HashMap;

use grovedock_core::acl::{AccessPolicy, Principal, ReadScope};
use grovedock_core::{Error, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grants {
    /// Path prefixes this principal may read. Absent everywhere = everyone reads everything.
    #[serde(default)]
    read: Option<Vec<String>>,
    #[serde(default)]
    write: Vec<String>,
    #[serde(default)]
    refs: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesPolicy {
    #[serde(default)]
    default: Grants,
    #[serde(default)]
    principals: HashMap<String, Grants>,
}

fn path_matches(grant: &str, path: &str) -> bool {
    let g = grant.trim_end_matches('/');
    g == "*" || g.is_empty() || path == g || (path.starts_with(g) && path.as_bytes()[g.len()] == b'/')
}

fn ref_matches(grant: &str, name: &str) -> bool {
    match grant.strip_suffix("/*") {
        _ if grant == "*" => true,
        Some(prefix) => {
            name.len() > prefix.len() + 1 && name.starts_with(prefix) && name.as_bytes()[prefix.len()] == b'/'
        }
        None => grant == name,
    }
}

impl RulesPolicy {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|e| Error::invalid(format!("access rules: {e}")))
    }

    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
        Self::parse(&text)
    }

    fn grants<'a>(&'a self, who: &Principal) -> impl Iterator<Item = &'a Grants> {
        std::iter::once(&self.default).chain(self.principals.get(&who.0))
    }
}

impl AccessPolicy for RulesPolicy {
    fn read_scope(&self, who: &Principal) -> ReadScope {
        let configured: Vec<&Vec<String>> = self.grants(who).filter_map(|g| g.read.as_ref()).collect();
        let any_rules = self.default.read.is_some() || self.principals.values().any(|g| g.read.is_some());
        if !any_rules {
            return ReadScope::All;
        }
        let prefixes: Vec<String> =
            configured.into_iter().flatten().map(|p| p.trim_end_matches('/').to_owned()).collect();
        if prefixes.iter().any(|p| p == "*" || p.is_empty()) { ReadScope::All } else { ReadScope::Prefixes(prefixes) }
    }

    fn can_write(&self, who: &Principal, path: &str) -> bool {
        self.grants(who).any(|g| g.write.iter().any(|w| path_matches(w, path)))
    }
    fn can_update_ref(&self, who: &Principal, ref_name: &str) -> bool {
        self.grants(who).any(|g| g.refs.iter().any(|r| ref_matches(r, ref_name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        let p = RulesPolicy::parse(
            r#"{"default":{"write":[],"refs":["user/*"]},
                "principals":{"api":{"write":["services/api/"],"refs":["main"]},
                              "admin":{"write":["*"],"refs":["*"]}}}"#,
        )
        .unwrap();
        let (api, admin, anon) = (Principal("api".into()), Principal("admin".into()), Principal::anonymous());
        assert!(p.can_write(&api, "services/api/main.rs"));
        assert!(p.can_write(&api, "services/api"));
        assert!(!p.can_write(&api, "services/api2/x"));
        assert!(!p.can_write(&api, "libs/x"));
        assert!(p.can_write(&admin, "anything/at/all"));
        assert!(!p.can_write(&anon, "x"));
        assert!(p.can_update_ref(&api, "main"));
        assert!(p.can_update_ref(&anon, "user/kabir/wip"));
        assert!(!p.can_update_ref(&anon, "user"));
        assert!(!p.can_update_ref(&anon, "user/"));
        assert!(!p.can_update_ref(&anon, "main"));
        assert!(!p.can_update_ref(&api, "release/1"));
        assert!(RulesPolicy::parse(r#"{"defualt":{}}"#).is_err(), "typos rejected");
        assert_eq!(p.read_scope(&api), ReadScope::All, "no read rules anywhere: full read");
        let r = RulesPolicy::parse(
            r#"{"default":{"read":["docs/"]},"principals":{"api":{"read":["services/api"]},"admin":{"read":["*"]}}}"#,
        )
        .unwrap();
        assert_eq!(r.read_scope(&api), ReadScope::Prefixes(vec!["docs".into(), "services/api".into()]));
        assert_eq!(r.read_scope(&admin), ReadScope::All);
        assert_eq!(r.read_scope(&anon), ReadScope::Prefixes(vec!["docs".into()]));
    }
}
