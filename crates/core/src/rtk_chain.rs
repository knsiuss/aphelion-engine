use crate::ingress_trim::contains_any_failure_marker;
use std::collections::BTreeMap;

/// Extracts individual lowercase subcommands from a chained shell command
/// (`cd repo && git status; ls -la`), stripping leading `cd` / `pushd` / env assignments.
pub fn extract_chain_commands(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in split_shell_chain(cmd) {
        let trimmed = strip_env_prefixes(part.trim());
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        let first = lower.split_whitespace().next().unwrap_or_default();
        if matches!(first, "cd" | "pushd" | "popd" | "export" | "set" | "echo") {
            continue;
        }
        out.push(lower);
    }
    out
}

fn split_shell_chain(cmd: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let bytes = cmd.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b';' | b'\n' if !in_single && !in_double => {
                parts.push(&cmd[start..i]);
                start = i + 1;
            }
            b'&' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'&' => {
                parts.push(&cmd[start..i]);
                start = i + 2;
                i += 1;
            }
            b'|' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'|' => {
                parts.push(&cmd[start..i]);
                start = i + 2;
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start <= cmd.len() {
        parts.push(&cmd[start..]);
    }
    parts
}

fn strip_env_prefixes(mut s: &str) -> &str {
    loop {
        let Some((tok, rest)) = s.split_once(char::is_whitespace) else {
            return s;
        };
        if tok.contains('=') && !tok.starts_with('-') && !tok.starts_with('/') {
            s = rest.trim_start();
        } else {
            return s;
        }
    }
}

/// Groups long file-path listings (e.g. `Glob` / `find`) by shared parent directory.
pub fn filter_glob_paths(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() < 10 {
        return None;
    }
    let mut grouped: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut matched = 0usize;
    for line in &lines {
        if line.contains("  ") || line.contains(": ") {
            return None;
        }
        if let Some(pos) = line.rfind(['/', '\\']) {
            grouped.entry(&line[..=pos]).or_default().push(&line[pos + 1..]);
            matched += 1;
        } else {
            grouped.entry("").or_default().push(line);
        }
    }
    if matched < 6 || grouped.len() * 2 >= lines.len() {
        return None;
    }
    let mut out = Vec::with_capacity(lines.len() + grouped.len());
    for (dir, files) in grouped {
        if dir.is_empty() {
            for f in files {
                out.push((*f).to_owned());
            }
        } else {
            out.push(format!("{dir} [{}]", files.join(", ")));
        }
    }
    let joined = out.join("\n");
    (joined.len() < raw.len()).then_some(joined)
}

/// Lossless composite condensation for chained shell outputs (`git status && git log && ls -la`).
/// Preserves all error/failure lines intact and only strips known boilerplate columns/hints.
pub fn filter_chained_output(subcmds: &[String], raw: &str) -> Option<String> {
    if contains_any_failure_marker(raw) {
        return None;
    }
    let has_known = subcmds.iter().any(|c| {
        c.contains("git ")
            || c.starts_with("ls")
            || c.starts_with("dir")
            || c.contains("grep")
            || c.contains("rg ")
            || c.contains("cargo ")
            || c.contains("npm ")
    });
    if !has_known {
        return None;
    }
    let mut out = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty()
            || t.starts_with("(use \"git ")
            || t.starts_with("nothing added to commit")
            || t.starts_with("no changes added to commit")
            || t.starts_with("total ")
            || t.starts_with("Author:")
            || t.starts_with("Merge:")
            || (t.starts_with("index ") && t.contains(".."))
        {
            continue;
        }
        if let Some(rest) = line.strip_prefix("commit ") {
            let short: String = rest.trim().chars().take(7).collect();
            out.push(format!("commit {short}"));
            continue;
        }
        let cols: Vec<&str> = t.split_whitespace().collect();
        if cols.len() >= 9 && (cols[0].starts_with('-') || cols[0].starts_with('d')) {
            out.push(format!("{} {}", cols[4], cols[8..].join(" ")));
        } else {
            out.push(line.trim_end().to_owned());
        }
    }
    let joined = out.join("\n");
    (!joined.trim().is_empty() && joined.len() < raw.len()).then_some(joined)
}
