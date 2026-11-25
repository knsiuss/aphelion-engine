use crate::ingress_trim::contains_any_failure_marker;
use crate::rtk_chain::{extract_chain_commands, filter_chained_output, filter_glob_paths};
use std::collections::BTreeMap;

/// RTK-Style Command-Aware Lossless Tool Output Filter (0% model IQ drop):
/// Detects noisy CLI/tool outputs (`git status/diff/log`, `grep/rg`, `ls/dir`, `glob`, `cargo/npm`)
/// including chained commands (`cd repo && ls -la`), and compresses boilerplate strictly before
/// first cache commit. Falls back to raw text if not strictly smaller or on critical error context.
pub fn apply_rtk_filter(tool_name: &str, hint_cmd: Option<&str>, raw: &str) -> Option<String> {
    if raw.len() < 80 {
        return None;
    }
    let subcmds = extract_chain_commands(hint_cmd.unwrap_or_default());
    if subcmds.len() > 1 {
        if let Some(chained) = filter_chained_output(&subcmds, raw) {
            return Some(chained);
        }
    }
    let cmd = subcmds
        .first()
        .map(String::as_str)
        .unwrap_or_default();
    let tname = tool_name.to_ascii_lowercase();
    let head = &raw[..raw.len().min(1024)];

    let candidate = if cmd.contains("git status") || head.starts_with("On branch ") {
        filter_git_status(raw)
    } else if cmd.contains("git diff") || head.starts_with("diff --git ") {
        filter_git_diff(raw)
    } else if cmd.contains("git log")
        || (head.starts_with("commit ") && head.contains("\nAuthor:"))
    {
        filter_git_log(raw)
    } else if tname.contains("grep")
        || cmd.contains("rg ")
        || cmd.contains("grep ")
        || looks_like_grep(raw)
    {
        filter_grep_output(raw)
    } else if tname.contains("glob") || cmd.starts_with("find ") {
        filter_glob_paths(raw)
    } else if cmd.starts_with("ls")
        || cmd.starts_with("dir")
        || cmd.contains("get-childitem")
        || head.starts_with("total ")
        || head.contains("LastWriteTime")
    {
        filter_ls_dir_output(raw)
    } else if cmd.contains("cargo ")
        || cmd.contains("npm ")
        || cmd.contains("pnpm ")
        || head.contains("   Compiling ")
    {
        filter_build_noise(raw)
    } else {
        None
    };

    candidate.filter(|out| !out.trim().is_empty() && out.len() < raw.len())
}

fn filter_git_status(raw: &str) -> Option<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty()
            || t.starts_with("(use \"git ")
            || t.starts_with("nothing added to commit")
            || t.starts_with("no changes added to commit")
        {
            continue;
        }
        out.push(line.trim_end());
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

fn filter_git_diff(raw: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    let mut unchanged_run: Vec<&str> = Vec::new();

    let flush_unchanged = |run: &mut Vec<&str>, dst: &mut Vec<String>| {
        if run.len() > 10 {
            dst.push(run[0].to_owned());
            dst.push(run[1].to_owned());
            dst.push(format!(" ... ({} unchanged lines) ...", run.len() - 4));
            dst.push(run[run.len() - 2].to_owned());
            dst.push(run[run.len() - 1].to_owned());
        } else {
            for l in run.iter() {
                dst.push((*l).to_owned());
            }
        }
        run.clear();
    };

    for line in raw.lines() {
        if line.starts_with("index ") && line.contains("..") {
            continue;
        }
        if line.starts_with("--- a/") || line.starts_with("+++ b/") {
            continue;
        }
        if line.starts_with(' ') {
            unchanged_run.push(line);
        } else {
            flush_unchanged(&mut unchanged_run, &mut out);
            out.push(line.to_owned());
        }
    }
    flush_unchanged(&mut unchanged_run, &mut out);
    Some(out.join("\n"))
}

fn filter_git_log(raw: &str) -> Option<String> {
    let mut entries = Vec::new();
    let mut hash = String::new();
    let mut date = String::new();
    let mut msg = String::new();

    let push_entry = |h: &mut String, d: &mut String, m: &mut String, dst: &mut Vec<String>| {
        if !h.is_empty() {
            dst.push(format!("{} {} {}", h, d, m.trim()));
            h.clear();
            d.clear();
            m.clear();
        }
    };

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("commit ") {
            push_entry(&mut hash, &mut date, &mut msg, &mut entries);
            hash = rest.trim().chars().take(7).collect();
        } else if let Some(rest) = line.strip_prefix("Date:") {
            date = rest.split_whitespace().take(4).collect::<Vec<_>>().join(" ");
        } else if line.starts_with("Author:") || line.starts_with("Merge:") || line.trim().is_empty()
        {
            continue;
        } else if msg.is_empty() {
            msg = line.trim().to_owned();
        }
    }
    push_entry(&mut hash, &mut date, &mut msg, &mut entries);
    (entries.len() >= 2).then(|| entries.join("\n"))
}

fn looks_like_grep(raw: &str) -> bool {
    let mut matches = 0usize;
    for line in raw.lines().take(12) {
        let mut parts = line.splitn(3, ':');
        if let (Some(path), Some(num), Some(_)) = (parts.next(), parts.next(), parts.next()) {
            if (path.contains('/') || path.contains('\\') || path.contains('.'))
                && !num.is_empty()
                && num.chars().all(|c| c.is_ascii_digit())
            {
                matches += 1;
            }
        }
    }
    matches >= 3
}

fn filter_grep_output(raw: &str) -> Option<String> {
    let mut grouped: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut total = 0usize;
    for line in raw.lines() {
        let mut parts = line.splitn(3, ':');
        if let (Some(path), Some(num), Some(rest)) = (parts.next(), parts.next(), parts.next()) {
            if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                grouped
                    .entry(path)
                    .or_default()
                    .push(format!("  {num}:{}", rest.trim_end()));
                total += 1;
            }
        }
    }
    if total < 3 || grouped.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(total + grouped.len());
    for (file, hits) in grouped {
        out.push(format!("{file}:"));
        out.extend(hits);
    }
    Some(out.join("\n"))
}

fn filter_ls_dir_output(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() < 6 {
        return None;
    }
    let mut out = Vec::new();
    for line in lines {
        let t = line.trim();
        if t.is_empty()
            || t.starts_with("total ")
            || t.starts_with("Mode ")
            || t.starts_with("----")
        {
            continue;
        }
        // Collapse Unix `ls -la` permission/owner/date columns into `<size> <name>`.
        let cols: Vec<&str> = t.split_whitespace().collect();
        if cols.len() >= 9 && (cols[0].starts_with('-') || cols[0].starts_with('d')) {
            out.push(format!("{} {}", cols[4], cols[8..].join(" ")));
        } else {
            out.push(t.to_owned());
        }
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

fn filter_build_noise(raw: &str) -> Option<String> {
    if contains_any_failure_marker(raw) || raw.contains("error:") || raw.contains("TS2") {
        return None;
    }
    let mut kept = Vec::new();
    let mut skipped = 0usize;
    for line in raw.lines() {
        let t = line.trim_start();
        if t.starts_with("Compiling ")
            || t.starts_with("Downloading ")
            || t.starts_with("Downloaded ")
            || t.starts_with("Fresh ")
            || t.starts_with("Locking ")
            || t.starts_with("Checking ")
        {
            skipped += 1;
        } else if !t.is_empty() {
            kept.push(line.trim_end());
        }
    }
    if skipped < 5 {
        return None;
    }
    let mut out = format!("[Aphelion Build Trim: {skipped} build progress lines condensed]\n");
    out.push_str(&kept.join("\n"));
    Some(out)
}
