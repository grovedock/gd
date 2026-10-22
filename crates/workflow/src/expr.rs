//! Expressions, as GitHub Actions writes them: `${{ matrix.os }}`, `if: failure()`,
//! `contains(github.ref, 'release')`. Values are JSON; comparisons follow GitHub's rules
//! (strings compare case-insensitively, mixed types are coerced to numbers).

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};

/// How the steps (or jobs) before this one went, for `success()`, `failure()` and friends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Success,
    Failure,
    Cancelled,
}

/// What an expression can see: contexts by name (`github`, `env`, `matrix`, `steps`, `needs`,
/// `runner`, `inputs`, `secrets`, `job`) and the status so far.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub contexts: BTreeMap<String, Value>,
    pub status: Status,
    /// The job's checkout on this machine, for `hashFiles()`; `None` where there are no files yet
    /// (the server deciding a job's `if:`).
    pub workspace: Option<std::path::PathBuf>,
}

impl Scope {
    pub fn with(mut self, name: &str, value: Value) -> Self {
        self.contexts.insert(name.to_owned(), value);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExprError(pub String);

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ExprError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ExprError> {
    Err(ExprError(msg.into()))
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
}

fn lex(src: &str) -> Result<Vec<Tok>, ExprError> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if c == '\'' {
            let mut s = String::new();
            i += 1;
            loop {
                match b.get(i) {
                    None => return err(format!("unclosed string in `{src}`")),
                    Some(b'\'') if b.get(i + 1) == Some(&b'\'') => {
                        s.push('\'');
                        i += 2;
                    }
                    Some(b'\'') => {
                        i += 1;
                        break;
                    }
                    Some(_) => {
                        let ch = src[i..].chars().next().unwrap_or_default();
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
            out.push(Tok::Str(s));
        } else if c.is_ascii_digit() || (c == '-' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                i += 1;
            }
            let t = &src[start..i];
            let n = if let Some(h) = t.strip_prefix("0x") {
                i64::from_str_radix(h, 16).ok().map(|v| v as f64)
            } else {
                t.parse().ok()
            };
            out.push(Tok::Num(n.ok_or_else(|| ExprError(format!("`{t}` isn't a number")))?));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-') {
                i += 1;
            }
            out.push(Tok::Ident(src[start..i].to_owned()));
        } else {
            let two = src.get(i..i + 2).unwrap_or("");
            let op = match two {
                "==" => Some("=="),
                "!=" => Some("!="),
                "<=" => Some("<="),
                ">=" => Some(">="),
                "&&" => Some("&&"),
                "||" => Some("||"),
                _ => None,
            };
            if let Some(op) = op {
                out.push(Tok::Op(op));
                i += 2;
                continue;
            }
            let op = match c {
                '(' => "(",
                ')' => ")",
                '[' => "[",
                ']' => "]",
                '.' => ".",
                ',' => ",",
                '!' => "!",
                '<' => "<",
                '>' => ">",
                '*' => "*",
                _ => return err(format!("unexpected `{c}` in `{src}`")),
            };
            out.push(Tok::Op(op));
            i += 1;
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
enum Node {
    Lit(Value),
    Ctx(String),
    Index(Box<Node>, Box<Node>),
    /// `a.*.b`: every element's property.
    Star(Box<Node>),
    Not(Box<Node>),
    Bin(&'static str, Box<Node>, Box<Node>),
    Call(String, Vec<Node>),
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
    src: String,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }
    fn eat(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Op(o)) if *o == op) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, op: &str) -> Result<(), ExprError> {
        if self.eat(op) { Ok(()) } else { err(format!("expected `{op}` in `{}`", self.src)) }
    }
    fn or(&mut self) -> Result<Node, ExprError> {
        let mut l = self.and()?;
        while self.eat("||") {
            l = Node::Bin("||", Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Node, ExprError> {
        let mut l = self.cmp()?;
        while self.eat("&&") {
            l = Node::Bin("&&", Box::new(l), Box::new(self.cmp()?));
        }
        Ok(l)
    }
    fn cmp(&mut self) -> Result<Node, ExprError> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(o @ ("==" | "!=" | "<" | "<=" | ">" | ">="))) => *o,
                _ => return Ok(l),
            };
            self.at += 1;
            l = Node::Bin(op, Box::new(l), Box::new(self.unary()?));
        }
    }
    fn unary(&mut self) -> Result<Node, ExprError> {
        if self.eat("!") {
            return Ok(Node::Not(Box::new(self.unary()?)));
        }
        self.postfix()
    }
    fn postfix(&mut self) -> Result<Node, ExprError> {
        let mut n = self.primary()?;
        loop {
            if self.eat(".") {
                if self.eat("*") {
                    n = Node::Star(Box::new(n));
                    continue;
                }
                match self.toks.get(self.at).cloned() {
                    Some(Tok::Ident(p)) => {
                        self.at += 1;
                        n = Node::Index(Box::new(n), Box::new(Node::Lit(Value::String(p))));
                    }
                    _ => return err(format!("expected a property name in `{}`", self.src)),
                }
            } else if self.eat("[") {
                if self.eat("*") {
                    self.expect("]")?;
                    n = Node::Star(Box::new(n));
                    continue;
                }
                let i = self.or()?;
                self.expect("]")?;
                n = Node::Index(Box::new(n), Box::new(i));
            } else {
                return Ok(n);
            }
        }
    }
    fn primary(&mut self) -> Result<Node, ExprError> {
        let Some(t) = self.toks.get(self.at).cloned() else { return err(format!("`{}` ends too soon", self.src)) };
        self.at += 1;
        Ok(match t {
            Tok::Num(n) => Node::Lit(num(n)),
            Tok::Str(s) => Node::Lit(Value::String(s)),
            Tok::Op("(") => {
                let n = self.or()?;
                self.expect(")")?;
                n
            }
            Tok::Ident(id) => match id.as_str() {
                "true" => Node::Lit(Value::Bool(true)),
                "false" => Node::Lit(Value::Bool(false)),
                "null" => Node::Lit(Value::Null),
                _ if self.eat("(") => {
                    let mut args = Vec::new();
                    if !self.eat(")") {
                        loop {
                            args.push(self.or()?);
                            if self.eat(")") {
                                break;
                            }
                            self.expect(",")?;
                        }
                    }
                    Node::Call(id.to_ascii_lowercase(), args)
                }
                _ => Node::Ctx(id),
            },
            Tok::Op(o) => return err(format!("unexpected `{o}` in `{}`", self.src)),
        })
    }
}

fn num(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        Value::Number(Number::from(n as i64))
    } else {
        Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
    }
}

fn parse(src: &str) -> Result<Node, ExprError> {
    let mut p = Parser { toks: lex(src)?, at: 0, src: src.to_owned() };
    let n = p.or()?;
    if p.at != p.toks.len() {
        return err(format!("unexpected text after the expression in `{src}`"));
    }
    Ok(n)
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn to_num(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => *b as i32 as f64,
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) if s.trim().is_empty() => 0.0,
        Value::String(s) => {
            let t = s.trim();
            t.strip_prefix("0x").map_or_else(
                || t.parse().unwrap_or(f64::NAN),
                |h| i64::from_str_radix(h, 16).map_or(f64::NAN, |v| v as f64),
            )
        }
        _ => f64::NAN,
    }
}

/// A value as text, the way `${{ }}` writes it into a script.
pub fn to_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn compare(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::String(x), Value::String(y)) => Some(x.to_lowercase().cmp(&y.to_lowercase())),
        (Value::Array(_) | Value::Object(_), _) | (_, Value::Array(_) | Value::Object(_)) => None,
        _ => to_num(a).partial_cmp(&to_num(b)),
    }
}

fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(_) | Value::Object(_), _) | (_, Value::Array(_) | Value::Object(_)) => {
            std::ptr::eq(a, b) || a == b
        }
        _ => compare(a, b) == Some(std::cmp::Ordering::Equal),
    }
}

fn index(v: &Value, i: &Value) -> Value {
    match (v, i) {
        (Value::Object(m), _) => {
            let k = to_text(i);
            m.get(&k)
                .or_else(|| m.iter().find(|(key, _)| key.eq_ignore_ascii_case(&k)).map(|(_, v)| v))
                .cloned()
                .unwrap_or(Value::Null)
        }
        (Value::Array(a), Value::Number(n)) => {
            n.as_u64().and_then(|n| a.get(n as usize)).cloned().unwrap_or(Value::Null)
        }
        // `steps.*.outcome` after a star: each element's property.
        _ => Value::Null,
    }
}

fn eval(n: &Node, s: &Scope) -> Result<Value, ExprError> {
    Ok(match n {
        Node::Lit(v) => v.clone(),
        Node::Ctx(name) => match s.contexts.get(name) {
            Some(v) => v.clone(),
            None => return err(format!("`{name}` isn't available here")),
        },
        Node::Index(base, i) => {
            let b = eval(base, s)?;
            let i = eval(i, s)?;
            if let Node::Star(_) = **base
                && let Value::Array(items) = &b
            {
                return Ok(Value::Array(items.iter().map(|x| index(x, &i)).collect()));
            }
            index(&b, &i)
        }
        Node::Star(base) => match eval(base, s)? {
            Value::Object(m) => Value::Array(m.into_iter().map(|(_, v)| v).collect()),
            Value::Array(a) => Value::Array(a),
            _ => Value::Array(Vec::new()),
        },
        Node::Not(x) => Value::Bool(!truthy(&eval(x, s)?)),
        Node::Bin("&&", l, r) => {
            let l = eval(l, s)?;
            if !truthy(&l) { l } else { eval(r, s)? }
        }
        Node::Bin("||", l, r) => {
            let l = eval(l, s)?;
            if truthy(&l) { l } else { eval(r, s)? }
        }
        Node::Bin(op, l, r) => {
            let (l, r) = (eval(l, s)?, eval(r, s)?);
            Value::Bool(match *op {
                "==" => equal(&l, &r),
                "!=" => !equal(&l, &r),
                "<" => compare(&l, &r) == Some(std::cmp::Ordering::Less),
                "<=" => matches!(compare(&l, &r), Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)),
                ">" => compare(&l, &r) == Some(std::cmp::Ordering::Greater),
                ">=" => matches!(compare(&l, &r), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)),
                _ => unreachable!(),
            })
        }
        Node::Call(f, args) => call(f, args, s)?,
    })
}

fn call(f: &str, args: &[Node], s: &Scope) -> Result<Value, ExprError> {
    let want = |n: usize| -> Result<(), ExprError> {
        if args.len() == n { Ok(()) } else { err(format!("{f}() takes {n} argument{}", if n == 1 { "" } else { "s" })) }
    };
    let arg = |i: usize| eval(&args[i], s);
    Ok(match f {
        "success" => {
            want(0)?;
            Value::Bool(s.status == Status::Success)
        }
        "failure" => {
            want(0)?;
            Value::Bool(s.status == Status::Failure)
        }
        "cancelled" => {
            want(0)?;
            Value::Bool(s.status == Status::Cancelled)
        }
        "always" => {
            want(0)?;
            Value::Bool(true)
        }
        "contains" => {
            want(2)?;
            let (hay, needle) = (arg(0)?, arg(1)?);
            Value::Bool(match &hay {
                Value::Array(items) => items.iter().any(|x| equal(x, &needle)),
                _ => to_text(&hay).to_lowercase().contains(&to_text(&needle).to_lowercase()),
            })
        }
        "startswith" => {
            want(2)?;
            Value::Bool(to_text(&arg(0)?).to_lowercase().starts_with(&to_text(&arg(1)?).to_lowercase()))
        }
        "endswith" => {
            want(2)?;
            Value::Bool(to_text(&arg(0)?).to_lowercase().ends_with(&to_text(&arg(1)?).to_lowercase()))
        }
        "format" => {
            if args.is_empty() {
                return err("format() needs a format string");
            }
            let fmt = to_text(&arg(0)?);
            let vals: Vec<String> = (1..args.len()).map(|i| arg(i).map(|v| to_text(&v))).collect::<Result<_, _>>()?;
            let mut out = String::new();
            let mut chars = fmt.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '{' if chars.peek() == Some(&'{') => {
                        chars.next();
                        out.push('{');
                    }
                    '}' if chars.peek() == Some(&'}') => {
                        chars.next();
                        out.push('}');
                    }
                    '{' => {
                        let mut d = String::new();
                        for c in chars.by_ref() {
                            if c == '}' {
                                break;
                            }
                            d.push(c);
                        }
                        let i: usize = d
                            .parse()
                            .map_err(|_| ExprError(format!("format(): `{{{d}}}` isn't an argument number")))?;
                        out.push_str(vals.get(i).ok_or_else(|| ExprError(format!("format(): no argument {i}")))?);
                    }
                    c => out.push(c),
                }
            }
            Value::String(out)
        }
        "join" => {
            if args.is_empty() || args.len() > 2 {
                return err("join() takes 1 or 2 arguments");
            }
            let sep = if args.len() == 2 { to_text(&arg(1)?) } else { ",".into() };
            match arg(0)? {
                Value::Array(a) => Value::String(a.iter().map(to_text).collect::<Vec<_>>().join(&sep)),
                v => Value::String(to_text(&v)),
            }
        }
        "tojson" => {
            want(1)?;
            Value::String(serde_json::to_string_pretty(&arg(0)?).unwrap_or_default())
        }
        "fromjson" => {
            want(1)?;
            let t = to_text(&arg(0)?);
            serde_json::from_str(&t).map_err(|e| ExprError(format!("fromJSON(): {e}")))?
        }
        "hashfiles" => {
            if args.is_empty() {
                return err("hashFiles() needs a pattern, like hashFiles('**/package-lock.json')");
            }
            let Some(root) = &s.workspace else {
                return err("hashFiles() works in steps, where the files are, not in a job's if:");
            };
            let patterns: Vec<String> =
                (0..args.len()).map(|i| arg(i).map(|v| to_text(&v))).collect::<Result<_, _>>()?;
            Value::String(hash_files(root, &patterns)?)
        }
        _ => return err(format!("{f}() isn't a function Runs knows")),
    })
}

/// Evaluates an expression (without `${{ }}`).
/// `hashFiles()`: the files under `root` matching any pattern and no `!pattern`, in path order; the
/// SHA-256 of their SHA-256s, in hex. Empty when nothing matches, as the workflow format says.
fn hash_files(root: &std::path::Path, patterns: &[String]) -> Result<String, ExprError> {
    use sha2::{Digest, Sha256};
    let set = |neg: bool| {
        let mut b = globset::GlobSetBuilder::new();
        for p in patterns.iter().filter(|p| p.starts_with('!') == neg) {
            let p = p.trim_start_matches('!').trim_start_matches("./");
            let g = globset::GlobBuilder::new(p)
                .literal_separator(true)
                .build()
                .map_err(|e| ExprError(format!("hashFiles(): `{p}`: {e}")))?;
            b.add(g);
        }
        b.build().map_err(|e| ExprError(format!("hashFiles(): {e}")))
    };
    let (yes, no) = (set(false)?, set(true)?);
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let Ok(t) = e.file_type() else { continue };
            let path = e.path();
            if t.is_dir() {
                if e.file_name() != ".git" {
                    dirs.push(path);
                }
            } else if t.is_file() {
                let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                if yes.is_match(&rel) && !no.is_match(&rel) {
                    files.push((rel, path));
                }
            }
        }
    }
    if files.is_empty() {
        return Ok(String::new());
    }
    files.sort();
    let mut all = Sha256::new();
    for (rel, path) in &files {
        let body = std::fs::read(path).map_err(|e| ExprError(format!("hashFiles(): {rel}: {e}")))?;
        all.update(Sha256::digest(&body));
    }
    Ok(all.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn evaluate(src: &str, s: &Scope) -> Result<Value, ExprError> {
    eval(&parse(src)?, s)
}

/// Checks an expression parses, without evaluating it.
pub fn check(src: &str) -> Result<(), ExprError> {
    parse(src).map(|_| ())
}

fn calls_status(n: &Node) -> bool {
    match n {
        Node::Call(f, args) => {
            matches!(f.as_str(), "success" | "failure" | "cancelled" | "always") || args.iter().any(calls_status)
        }
        Node::Index(a, b) | Node::Bin(_, a, b) => calls_status(a) || calls_status(b),
        Node::Star(a) | Node::Not(a) => calls_status(a),
        _ => false,
    }
}

/// An `if:` condition: may be written with or without `${{ }}`, and without a status function
/// it means `success() && (…)`, as on GitHub.
pub fn condition(src: Option<&str>, s: &Scope) -> Result<bool, ExprError> {
    let Some(src) = src.map(str::trim).filter(|x| !x.is_empty()) else { return Ok(s.status == Status::Success) };
    let inner = match src.strip_prefix("${{").and_then(|r| r.strip_suffix("}}")) {
        Some(i) if !i.contains("${{") => i.trim().to_owned(),
        _ if src.contains("${{") => interpolate(src, s)?,
        _ => src.to_owned(),
    };
    let n = parse(&inner)?;
    if !calls_status(&n) && s.status != Status::Success {
        return Ok(false);
    }
    Ok(truthy(&eval(&n, s)?))
}

/// Replaces every `${{ expr }}` in `text` with its value.
pub fn interpolate(text: &str, s: &Scope) -> Result<String, ExprError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("${{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 3..];
        let Some(j) = find_close(after) else { return err(format!("`${{{{` without `}}}}` in `{text}`")) };
        out.push_str(&to_text(&evaluate(after[..j].trim(), s)?));
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Where the `}}` closing an expression is, skipping any inside strings.
fn find_close(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i + 1 < b.len() + 1 && i < b.len() {
        match b[i] {
            b'\'' => in_str = !in_str,
            b'}' if !in_str && b.get(i + 1) == Some(&b'}') => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The names of contexts an interpolated text reads (`secrets`, `matrix`…), to say early
/// which ones a step needs.
pub fn contexts_used(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("${{") {
        let after = &rest[i + 3..];
        let Some(j) = find_close(after) else { break };
        if let Ok(toks) = lex(&after[..j]) {
            let mut prev_dot = false;
            for t in toks {
                if let Tok::Ident(id) = &t
                    && !prev_dot
                    && !out.contains(id)
                {
                    out.push(id.clone());
                }
                prev_dot = t == Tok::Op(".");
            }
        }
        rest = &after[j + 2..];
    }
    out
}

/// A JSON object from string pairs.
pub fn object<'a>(pairs: impl IntoIterator<Item = (&'a str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect::<Map<_, _>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> Scope {
        Scope::default()
            .with("github", json!({"ref": "refs/heads/main", "event_name": "push", "actor": "asha"}))
            .with("matrix", json!({"os": "linux", "node": 20}))
            .with("steps", json!({"build": {"outcome": "success", "outputs": {"v": "1.2"}}}))
    }

    #[test]
    fn hash_files_matches_globs_under_the_workspace() {
        let dir = std::env::temp_dir().join(format!("gd-hashfiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, body) in [
            ("package-lock.json", "a"),
            ("web/package-lock.json", "b"),
            ("vendor/package-lock.json", "c"),
            ("README.md", "d"),
        ] {
            std::fs::create_dir_all(dir.join(p).parent().unwrap()).unwrap();
            std::fs::write(dir.join(p), body).unwrap();
        }
        let s = Scope { workspace: Some(dir.clone()), ..Scope::default() };
        let h = |e: &str| to_text(&evaluate(e, &s).unwrap());
        let all = h("hashFiles('**/package-lock.json')");
        assert_eq!(all.len(), 64);
        assert_ne!(all, h("hashFiles('**/package-lock.json', '!vendor/**')"), "excludes drop vendor/");
        assert_eq!(
            h("hashFiles('**/package-lock.json', '!vendor/**')"),
            h("hashFiles('package-lock.json', 'web/*.json')")
        );
        assert_eq!(h("hashFiles('**/*.lock')"), "", "nothing matched");
        std::fs::write(dir.join("web/package-lock.json"), "changed").unwrap();
        assert_ne!(all, h("hashFiles('**/package-lock.json')"), "a changed file changes the hash");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn evaluates_like_github() {
        let s = scope();
        let e = |x: &str| evaluate(x, &s).unwrap();
        assert_eq!(e("github.ref == 'refs/heads/main'"), json!(true));
        assert_eq!(e("github.REF == 'REFS/HEADS/MAIN'"), json!(true), "case-insensitive");
        assert_eq!(e("matrix.node >= 18 && matrix.os"), json!("linux"));
        assert_eq!(e("github['event_name'] != 'pull_request' || 'x'"), json!(true));
        assert_eq!(e("contains(fromJSON('[\"a\",\"b\"]'), 'B')"), json!(true));
        assert_eq!(e("format('{0}-{1}{{x}}', matrix.os, matrix.node)"), json!("linux-20{x}"));
        assert_eq!(e("steps.build.outputs.v"), json!("1.2"));
        assert_eq!(e("steps.*.outcome"), json!(["success"]));
        assert_eq!(e("startsWith(github.ref, 'refs/tags/')"), json!(false));
        assert_eq!(e("'1' == 1"), json!(true));
        assert_eq!(e("!github.missing"), json!(true));
        assert!(evaluate("hashFiles('x')", &s).is_err(), "no workspace, no files");
        assert!(evaluate("secrets.x", &s).is_err(), "unknown contexts are errors, not empty");
    }

    #[test]
    fn conditions_assume_success_unless_they_ask() {
        let mut s = scope();
        assert!(condition(None, &s).unwrap());
        assert!(condition(Some("${{ matrix.os == 'linux' }}"), &s).unwrap());
        s.status = Status::Failure;
        assert!(!condition(Some("matrix.os == 'linux'"), &s).unwrap());
        assert!(condition(Some("failure()"), &s).unwrap());
        assert!(condition(Some("always() && matrix.os == 'linux'"), &s).unwrap());
        assert!(!condition(None, &s).unwrap());
    }

    #[test]
    fn interpolates_and_lists_contexts() {
        let s = scope();
        assert_eq!(
            interpolate("node ${{ matrix.node }} on ${{matrix.os}}; '}}'", &s).unwrap(),
            "node 20 on linux; '}}'"
        );
        assert_eq!(interpolate("${{ format('}}{0}', 'x') }}", &s).unwrap(), "}x");
        assert_eq!(
            contexts_used("${{ secrets.TOKEN }} ${{ matrix.os }} ${{ github.ref }}"),
            ["secrets", "matrix", "github"]
        );
    }
}
