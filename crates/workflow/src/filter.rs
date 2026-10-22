//! Branch, tag and path filters, as GitHub writes them: `*` within one segment, `**` across
//! segments, `?` and `+` after a character, `[...]` classes, and `!pattern` to take back what
//! an earlier pattern matched (the last matching pattern decides).

use regex::Regex;

fn to_regex(pattern: &str) -> Option<Regex> {
    let mut re = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                re.push_str(".*");
                i += 2;
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push('?'),
            '+' => re.push('+'),
            '[' => {
                let end = chars[i..].iter().position(|&c| c == ']')? + i;
                re.push('[');
                re.extend(chars[i + 1..end].iter());
                re.push(']');
                i = end + 1;
                continue;
            }
            '\\' if i + 1 < chars.len() => {
                re.push_str(&regex::escape(&chars[i + 1].to_string()));
                i += 2;
                continue;
            }
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    Regex::new(&re).ok()
}

/// Whether `name` matches `pattern`.
pub fn glob(pattern: &str, name: &str) -> bool {
    to_regex(pattern).is_some_and(|r| r.is_match(name))
}

/// Whether `name` passes a list of patterns (with `!` negations, last match wins).
pub fn matches_list(patterns: &[String], name: &str) -> bool {
    let mut hit = false;
    for p in patterns {
        if let Some(neg) = p.strip_prefix('!') {
            if glob(neg, name) {
                hit = false;
            }
        } else if glob(p, name) {
            hit = true;
        }
    }
    hit
}

/// Include and ignore lists together: GitHub allows one or the other per kind.
pub fn passes(include: &[String], ignore: &[String], name: &str) -> bool {
    if !include.is_empty() {
        return matches_list(include, name);
    }
    if !ignore.is_empty() {
        return !matches_list(ignore, name);
    }
    true
}

/// Path filters: a run happens when any changed path passes (`paths`), or when not every
/// changed path is ignored (`paths-ignore`). Unknown paths (`None`) always run.
pub fn paths_pass(include: &[String], ignore: &[String], changed: Option<&[String]>) -> bool {
    let Some(changed) = changed else { return true };
    if !include.is_empty() {
        return changed.iter().any(|p| matches_list(include, p));
    }
    if !ignore.is_empty() {
        return changed.iter().any(|p| !matches_list(ignore, p));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: &[&str]) -> Vec<String> {
        x.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn patterns_like_githubs() {
        assert!(glob("main", "main"));
        assert!(glob("release/*", "release/1.2"));
        assert!(!glob("release/*", "release/1/hotfix"));
        assert!(glob("release/**", "release/1/hotfix"));
        assert!(glob("v[12].[0-9]+.[0-9]+", "v1.10.3"));
        assert!(glob("docs/**/*.md", "docs/a/b/c.md"));
        assert!(glob("*.js", "app.js"));
        assert!(!glob("*.js", "src/app.js"));
        assert!(glob("**.js", "src/app.js"));
        // `?`: zero or one of the character before it.
        assert!(glob("feature?", "featur") && glob("feature?", "feature") && !glob("feature?", "features"));
    }

    #[test]
    fn negation_and_paths() {
        let l = v(&["release/**", "!release/**-alpha"]);
        assert!(matches_list(&l, "release/2.0"));
        assert!(!matches_list(&l, "release/2.0-alpha"));
        assert!(passes(&[], &v(&["wip/*"]), "main"));
        assert!(!passes(&[], &v(&["wip/*"]), "wip/x"));
        let changed = v(&["docs/a.md", "README.md"]);
        assert!(!paths_pass(&v(&["src/**"]), &[], Some(&changed)));
        assert!(!paths_pass(&[], &v(&["docs/**", "*.md"]), Some(&changed)));
        assert!(paths_pass(&[], &v(&["docs/**"]), Some(&changed)));
        assert!(paths_pass(&v(&["src/**"]), &[], None));
    }
}
