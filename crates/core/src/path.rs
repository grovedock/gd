//! Repository path rules. Paths cross trust boundaries (client -> server, server ->
//! client filesystem), so every component is validated before use.

use crate::{Error, Result};

/// Directory reserved for client metadata; never tracked.
pub const META_DIR: &str = ".gd";
/// Its name before 4.0. Still reserved, so no repository can plant one that a 4.0 client would
/// adopt as an old workspace's metadata.
pub const LEGACY_META_DIR: &str = ".pando";

pub fn validate_name(name: &str) -> Result<()> {
    let bad = name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']);
    if bad {
        return Err(Error::invalid(format!("invalid path component `{name}`")));
    }
    if reserved(name) {
        return Err(Error::invalid(reserved_why(name)));
    }
    Ok(())
}

fn reserved_why(name: &str) -> String {
    format!("`{name}` is reserved: a file under `.git` or `.gd` would change the tools of whoever checks it out")
}

/// Is `name`, as some filesystem would see it, `.git` or `.gd`? Like git's `verify_path`:
/// case-insensitive (macOS, Windows), ignoring trailing dots and spaces and an NTFS stream
/// suffix (`.git. `, `.git::$INDEX_ALLOCATION`), the 8.3 short names (`GIT~1`), and the
/// characters HFS+ ignores (`.g\u{200c}it`).
fn reserved(name: &str) -> bool {
    let ignorable =
        |c: char| matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}');
    let n: String = name.chars().filter(|c| !ignorable(*c)).collect::<String>().to_lowercase();
    let n = n.split(':').next().unwrap_or("").trim_end_matches(['.', ' ']);
    matches!(n, ".git" | "git~1" | META_DIR | "gd~1" | LEGACY_META_DIR | "pando~1")
}

/// Split a repo-relative path (`a/b/c`) into validated components.
pub fn components(path: &str) -> Result<Vec<&str>> {
    if path.is_empty() {
        return Err(Error::invalid("empty path"));
    }
    let parts: Vec<&str> = path.split('/').collect();
    for p in &parts {
        validate_name(p).map_err(|_| match reserved(p) {
            true => Error::invalid(format!("invalid path `{path}`: {}", reserved_why(p))),
            false => Error::invalid(format!("invalid path `{path}`")),
        })?;
    }
    Ok(parts)
}

/// `""` denotes the root; otherwise same rules as [`components`].
pub fn components_or_root(path: &str) -> Result<Vec<&str>> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() { Ok(Vec::new()) } else { components(trimmed) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rejects_traversal_and_reserved() {
        for bad in ["", "/a", "a/", "a//b", "../x", "a/../b", "./a", ".gd/x", "a\\b", "a\0b"] {
            assert!(components(bad).is_err(), "{bad:?} accepted");
        }
        assert_eq!(components("src/main.rs").unwrap(), ["src", "main.rs"]);
        // Paths a filesystem treats as `.git` / `.gd` (hooks or config planted in a checkout).
        for bad in [
            ".git/config",
            "a/.git/hooks/post-checkout",
            ".GIT/config",
            ".Git./config",
            ".git /x",
            ".git::$INDEX_ALLOCATION/x",
            "GIT~1/config",
            ".g\u{200c}it/config",
            ".GD/config.json",
            "x/.gd.",
            "GD~1",
            ".pando/workspace.json",
            "a/.Pando./x",
            "PANDO~1",
        ] {
            let e = components(bad).unwrap_err().to_string();
            assert!(e.contains("reserved"), "{bad:?}: {e}");
        }
        for ok in [".gitignore", ".github/ci.yml", ".gitattributes", "git~2", "gd", "x.git"] {
            assert!(components(ok).is_ok(), "{ok:?} refused");
        }
        assert!(components_or_root("/").unwrap().is_empty());
    }

    proptest! {
        // Any accepted path joined under a root stays under that root.
        #[test]
        fn accepted_paths_never_escape(path in "[a-z./\\\\]{0,24}") {
            if let Ok(parts) = components(&path) {
                let joined = std::path::Path::new("/root").join(parts.join("/"));
                prop_assert!(joined.starts_with("/root"));
                prop_assert!(!parts.iter().any(|p| *p == ".." || *p == "."));
            }
        }
    }
}
