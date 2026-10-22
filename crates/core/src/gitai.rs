//! git-ai authorship notes (`refs/notes/ai`, schema `authorship/3.0.0`): which lines of a commit
//! an agent wrote, which a person was seen typing, and which nobody knows. Read on import so
//! that history from a git repository keeps its attribution. Format:
//! <https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md>

use std::collections::BTreeMap;

use serde::Deserialize;

/// The notes ref git-ai writes.
pub const NOTES_REF: &str = "refs/notes/ai";

/// One file's attestations: each key with the line ranges (1-based, inclusive) it covers.
pub type Attested = (String, Vec<(String, Vec<(u32, u32)>)>);

/// One commit's note.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Note {
    /// Per file, each key with the lines (1-based, inclusive ranges, in that commit's version of
    /// the file) it covers. Lines under no key are untracked: nothing is known about them.
    pub files: Vec<Attested>,
    /// Who each key is.
    pub authors: BTreeMap<String, Author>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Author {
    /// An agent session: `tool` like "claude_code" or "cursor", and its model.
    Agent { tool: String, model: String, session: String, human: String },
    /// A person an editor saw typing: "Name <email>".
    Person(String),
}

impl Note {
    /// The key covering `line` (1-based) of `path`, if any.
    pub fn key_for(&self, path: &str, line: u32) -> Option<&str> {
        let (_, keys) = self.files.iter().find(|(p, _)| p == path)?;
        keys.iter().find(|(_, r)| r.iter().any(|&(a, b)| a <= line && line <= b)).map(|(k, _)| k.as_str())
    }
}

#[derive(Deserialize)]
struct Meta {
    schema_version: String,
    #[serde(default)]
    prompts: BTreeMap<String, Session>,
    #[serde(default)]
    sessions: BTreeMap<String, Session>,
    #[serde(default)]
    humans: BTreeMap<String, Human>,
}

#[derive(Deserialize)]
struct Session {
    agent_id: AgentId,
    #[serde(default)]
    human_author: String,
}

#[derive(Deserialize)]
struct AgentId {
    #[serde(default)]
    tool: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
}

#[derive(Deserialize)]
struct Human {
    author: String,
}

/// Notes larger than this aren't read: a commit's attribution is a few kilobytes.
pub const MAX_NOTE_BYTES: usize = 1 << 20;

pub fn parse(note: &str) -> Result<Note, String> {
    if note.len() > MAX_NOTE_BYTES {
        return Err("the note is larger than 1 MiB".into());
    }
    let (attest, meta) = match note.split_once("\n---\n") {
        Some(x) => x,
        None => note.strip_prefix("---\n").map(|m| ("", m)).ok_or("no `---` line between attestations and metadata")?,
    };
    let meta: Meta = serde_json::from_str(meta.trim()).map_err(|e| format!("metadata: {e}"))?;
    if !meta.schema_version.starts_with("authorship/3.") {
        return Err(format!("schema {} isn't one this reads (authorship/3.x)", meta.schema_version));
    }
    let mut files: Vec<Attested> = Vec::new();
    for (n, line) in attest.lines().enumerate() {
        let n = n + 1;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(entry) = line.strip_prefix("  ") {
            let (key, ranges) = entry.rsplit_once(' ').ok_or(format!("line {n}: expected `KEY RANGES`"))?;
            let file = files.last_mut().ok_or(format!("line {n}: an attestation before any file"))?;
            file.1.push((key.to_owned(), ranges_of(ranges).map_err(|e| format!("line {n}: {e}"))?));
        } else {
            files.push((path_of(line).map_err(|e| format!("line {n}: {e}"))?, Vec::new()));
        }
    }
    let mut authors = BTreeMap::new();
    for (k, s) in meta.prompts.into_iter().chain(meta.sessions) {
        let session = k.split("::").next().unwrap_or(&k).to_owned();
        authors.insert(
            k,
            Author::Agent {
                tool: s.agent_id.tool,
                model: s.agent_id.model,
                session: if s.agent_id.id.is_empty() { session } else { s.agent_id.id },
                human: s.human_author,
            },
        );
    }
    for (k, h) in meta.humans {
        authors.insert(k, Author::Person(h.author));
    }
    // A session key in the attestations is `s_<id>::t_<trace>`; its record is under `s_<id>`.
    for (_, keys) in &files {
        for (k, _) in keys {
            if !authors.contains_key(k)
                && let Some((s, _)) = k.split_once("::")
                && let Some(a) = authors.get(s).cloned()
            {
                authors.insert(k.clone(), a);
            }
        }
    }
    Ok(Note { files, authors })
}

fn ranges_of(s: &str) -> Result<Vec<(u32, u32)>, String> {
    s.split(',')
        .map(|r| {
            let (a, b) = r.split_once('-').unwrap_or((r, r));
            let (a, b): (u32, u32) = (
                a.parse().map_err(|_| format!("`{r}` isn't a line range"))?,
                b.parse().map_err(|_| format!("`{r}` isn't a line range"))?,
            );
            if a == 0 || b < a { Err(format!("`{r}` isn't a line range")) } else { Ok((a, b)) }
        })
        .collect()
}

fn path_of(line: &str) -> Result<String, String> {
    let Some(q) = line.strip_prefix('"') else { return Ok(line.to_owned()) };
    let body = q.strip_suffix('"').ok_or("a quoted path without its closing quote")?;
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        out.push(match (c, c == '\\') {
            (_, true) => match chars.next() {
                Some('n') => '\n',
                Some('t') => '\t',
                Some(c @ ('"' | '\\')) => c,
                _ => return Err("an unknown escape in a quoted path".into()),
            },
            (c, false) => c,
        });
    }
    Ok(out)
}

/// A session key for an agent's work in the standard's form: `s_` and the first 14 hex digits
/// of SHA-256 of `tool:conversation`, then `::t_` and a trace (here, of the change).
pub fn session_key(tool: &str, conversation: &str, trace: &str) -> (String, String) {
    use sha2::{Digest, Sha256};
    let h = |s: &str| Sha256::digest(s.as_bytes()).iter().take(7).map(|b| format!("{b:02x}")).collect::<String>();
    let session = format!("s_{}", h(&format!("{tool}:{conversation}")));
    (format!("{session}::t_{}", h(trace)), session)
}

/// One commit's note as the standard writes it: the files' attestations, `---`, then the
/// metadata. `base` is the commit's first parent (empty for a root commit).
pub fn write(note: &Note, base: &str) -> String {
    let path = |p: &str| {
        if p.contains([' ', '\t', '\n', '"', '\\']) {
            format!("\"{}\"", p.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\t', "\\t"))
        } else {
            p.to_owned()
        }
    };
    let mut out = String::new();
    for (file, keys) in &note.files {
        out.push_str(&path(file));
        out.push('\n');
        for (key, ranges) in keys {
            let r: Vec<String> =
                ranges.iter().map(|&(a, b)| if a == b { a.to_string() } else { format!("{a}-{b}") }).collect();
            out.push_str(&format!("  {key} {}\n", r.join(",")));
        }
    }
    let mut sessions = serde_json::Map::new();
    let mut humans = serde_json::Map::new();
    for (key, author) in &note.authors {
        match author {
            Author::Agent { tool, model, session, human } => {
                let mut rec = serde_json::json!({ "agent_id": { "tool": tool, "id": session, "model": model } });
                if !human.is_empty() {
                    rec["human_author"] = serde_json::json!(human);
                }
                sessions.insert(key.split("::").next().unwrap_or(key).to_owned(), rec);
            }
            Author::Person(p) => {
                humans.insert(key.clone(), serde_json::json!({ "author": p }));
            }
        }
    }
    let meta = serde_json::json!({
        "schema_version": "authorship/3.0.0",
        "git_ai_version": "grovedock",
        "base_commit_sha": base,
        "prompts": {},
        "sessions": sessions,
        "humans": humans,
    });
    out.push_str("---\n");
    out.push_str(&serde_json::to_string_pretty(&meta).unwrap_or_default());
    out.push('\n');
    out
}

/// Line ranges (1-based, inclusive) from sorted line numbers.
pub fn ranges(lines: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &l in lines {
        match out.last_mut() {
            Some((_, end)) if *end + 1 == l => *end = l,
            _ => out.push((l, l)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn what_is_written_reads_back() {
        let (key, _) = session_key("agent/fix-01", "conv-9", "abc");
        assert!(key.starts_with("s_") && key.contains("::t_") && key.len() == 2 + 14 + 4 + 14);
        let note = Note {
            files: vec![("src/a b.rs".into(), vec![(key.clone(), ranges(&[1, 2, 3, 7]))])],
            authors: [(
                key.clone(),
                Author::Agent {
                    tool: "agent/fix-01".into(),
                    model: "claude-opus-5-5".into(),
                    session: "conv-9".into(),
                    human: String::new(),
                },
            )]
            .into_iter()
            .collect(),
        };
        let text = write(&note, "0123abcd");
        let back = parse(&text).unwrap();
        assert_eq!(back.files, note.files);
        assert!(matches!(back.authors.get(&key), Some(Author::Agent { model, .. }) if model == "claude-opus-5-5"));
        assert_eq!(back.key_for("src/a b.rs", 7).unwrap(), key);
        assert!(back.key_for("src/a b.rs", 5).is_none());
    }

    use super::*;

    const NOTE: &str = r#"src/auth/login.ts
  s_c9883b05a2487d::t_9f8e7d6c5b4a32 1-10,15-20
  h_1a2b3c4d5e6f70 11-14
"docs/read me.md"
  0123456789abcdef 3
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.4.2",
  "base_commit_sha": "9f2c1e0",
  "prompts": {
    "0123456789abcdef": {"agent_id": {"tool": "cursor", "id": "c-1", "model": "gpt-5"}, "total_additions": 1, "total_deletions": 0, "accepted_lines": 1, "overriden_lines": 0}
  },
  "sessions": {
    "s_c9883b05a2487d": {"agent_id": {"tool": "claude_code", "id": "conv-42", "model": "claude-sonnet-4"}, "human_author": "Asha Rao <asha@acme.test>"}
  },
  "humans": {"h_1a2b3c4d5e6f70": {"author": "Asha Rao <asha@acme.test>"}}
}"#;

    #[test]
    fn reads_sessions_people_legacy_prompts_and_untracked_lines() {
        let n = parse(NOTE).unwrap();
        let agent = |path, line| match n.key_for(path, line).and_then(|k| n.authors.get(k)) {
            Some(Author::Agent { tool, model, .. }) => format!("{tool}/{model}"),
            Some(Author::Person(p)) => p.clone(),
            None => "untracked".into(),
        };
        assert_eq!(agent("src/auth/login.ts", 1), "claude_code/claude-sonnet-4");
        assert_eq!(agent("src/auth/login.ts", 12), "Asha Rao <asha@acme.test>");
        assert_eq!(agent("src/auth/login.ts", 20), "claude_code/claude-sonnet-4");
        assert_eq!(agent("src/auth/login.ts", 21), "untracked");
        assert_eq!(agent("docs/read me.md", 3), "cursor/gpt-5");
        assert_eq!(agent("other.rs", 1), "untracked");
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        assert!(parse("a.rs\n  k 1-2\n").is_err(), "no metadata");
        assert!(parse("a.rs\n  k 3-1\n---\n{\"schema_version\":\"authorship/3.0.0\"}").is_err(), "backwards range");
        assert!(parse("---\n{\"schema_version\":\"authorship/2.0.0\"}").is_err(), "older schema");
        assert_eq!(parse("---\n{\"schema_version\":\"authorship/3.0.0\"}").unwrap(), Note::default());
    }
}
