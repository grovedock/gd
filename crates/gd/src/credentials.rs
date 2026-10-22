//! Tokens saved by `gd login`, one per server, in `~/.config/gd/credentials.json`
//! (`$XDG_CONFIG_HOME/gd` when set; under the user profile on Windows). The file is readable
//! by its owner only. A token given with `--token` or `GD_TOKEN` always comes first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    servers: BTreeMap<String, Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    token: String,
}

/// Where the file lives.
pub fn path() -> Option<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?).join(".config"),
    };
    Some(in_config(&config))
}

/// The credentials file under a config directory. Sign-ins saved before 4.0 carry over: copied
/// once from `pando/` (the copy keeps the owner-only mode).
fn in_config(config: &Path) -> PathBuf {
    let path = config.join("gd/credentials.json");
    let old = config.join("pando/credentials.json");
    if !path.exists() && old.exists() {
        let _ = std::fs::create_dir_all(config.join("gd")).and_then(|()| std::fs::copy(&old, &path));
    }
    path
}

/// What credentials are kept under: the server's origin (`https://host[:port]`), so any
/// repository URL on that server finds them.
pub fn key(server: &str) -> String {
    let s = server.trim();
    let (scheme, rest) = s.split_once("://").unwrap_or(("https", s));
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    format!("{}://{}", scheme.to_ascii_lowercase(), host.to_ascii_lowercase())
}

fn read(path: &Path) -> Result<File> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Replaces the file, readable by its owner only (written aside, then renamed into place).
fn write(path: &Path, f: &File) -> Result<()> {
    let dir = path.parent().context("credentials path has no directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        opts.mode(0o600);
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let mut file = opts.open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        // A file left over from before keeps its old mode when opened; tighten it.
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(f)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The saved token for `server`, if any.
pub fn load(path: &Path, server: &str) -> Option<String> {
    read(path).ok()?.servers.remove(&key(server)).map(|e| e.token)
}

pub fn save(path: &Path, server: &str, token: &str) -> Result<()> {
    let mut f = read(path)?;
    f.servers.insert(key(server), Entry { token: token.to_owned() });
    write(path, &f)
}

/// Forgets `server`'s token; returns it if there was one.
pub fn remove(path: &Path, server: &str) -> Result<Option<String>> {
    let mut f = read(path)?;
    let old = f.servers.remove(&key(server)).map(|e| e.token);
    if old.is_some() {
        write(path, &f)?;
    }
    Ok(old)
}

/// The token to use for `server`: the one given (`--token`, `GD_TOKEN`), else the saved one.
pub fn resolve(given: Option<&str>, server: &str) -> Option<String> {
    given.map(str::to_owned).or_else(|| load(&path()?, server))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_ins_from_before_4_0_carry_over() {
        let dir = tempfile::tempdir().unwrap();
        save(&dir.path().join("pando/credentials.json"), "https://a.example", "pando_a").unwrap();
        let p = in_config(dir.path());
        assert_eq!(p, dir.path().join("gd/credentials.json"));
        assert_eq!(load(&p, "https://a.example").as_deref(), Some("pando_a"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        save(&p, "https://a.example", "gd_b").unwrap();
        assert_eq!(load(&in_config(dir.path()), "https://a.example").as_deref(), Some("gd_b"), "copied once");
    }

    #[test]
    fn keyed_by_origin_and_private_to_the_owner() {
        assert_eq!(key("HTTPS://Grovedock.Example.com/acme/app"), "https://grovedock.example.com");
        assert_eq!(key("http://127.0.0.1:7420/"), "http://127.0.0.1:7420");

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gd/credentials.json");
        assert_eq!(load(&p, "https://a.example"), None);
        save(&p, "https://a.example", "gd_a").unwrap();
        save(&p, "https://b.example", "gd_b").unwrap();
        assert_eq!(load(&p, "https://a.example/acme/app").as_deref(), Some("gd_a"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(p.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert_eq!(remove(&p, "https://a.example").unwrap().as_deref(), Some("gd_a"));
        assert_eq!(remove(&p, "https://a.example").unwrap(), None);
        assert_eq!(load(&p, "https://b.example").as_deref(), Some("gd_b"));
        assert_eq!(resolve(Some("gd_given"), "https://b.example").as_deref(), Some("gd_given"));
    }
}
