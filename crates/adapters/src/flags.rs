//! Feature flag source parsed from a spec string such as `land=off,git-import=on`.
//! Every flag defaults to on; the binaries read the spec from `GD_FLAGS`.

use std::collections::HashMap;

use grovedock_core::flags::{FeatureFlags, Flag};
use grovedock_core::{Error, Result};

#[derive(Debug, Default, Clone)]
pub struct SpecFlags {
    overrides: HashMap<Flag, bool>,
}

impl SpecFlags {
    pub fn all_on() -> Self {
        Self::default()
    }

    pub fn parse(spec: &str) -> Result<Self> {
        let mut overrides = HashMap::new();
        for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (key, value) = item.split_once('=').unwrap_or((item, "on"));
            let flag = Flag::from_key(key.trim()).ok_or_else(|| Error::invalid(format!("unknown flag `{key}`")))?;
            let on = match value.trim() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                v => return Err(Error::invalid(format!("flag `{key}`: bad value `{v}`"))),
            };
            overrides.insert(flag, on);
        }
        Ok(Self { overrides })
    }

    pub fn from_env() -> Result<Self> {
        Self::parse(&std::env::var("GD_FLAGS").unwrap_or_default())
    }
}

impl FeatureFlags for SpecFlags {
    fn enabled(&self, flag: Flag) -> bool {
        self.overrides.get(&flag).copied().unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spec() {
        let f = SpecFlags::parse(" land=off, history=0,diff ").unwrap();
        assert!(!f.enabled(Flag::Land));
        assert!(!f.enabled(Flag::History));
        assert!(f.enabled(Flag::Diff));
        assert!(f.enabled(Flag::Objects));
        assert!(SpecFlags::parse("nope=on").is_err());
        assert!(SpecFlags::parse("land=maybe").is_err());
        assert!(Flag::ALL.iter().all(|f| SpecFlags::parse("").unwrap().enabled(*f)));
    }
}
