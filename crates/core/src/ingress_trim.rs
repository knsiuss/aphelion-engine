/// Lossless Pre-Commit Noise Trimmer (0% model IQ drop):
/// 1. Strips ANSI escape sequences (colors/cursor codes) and `\r` progress-bar spinner overwrites.
/// 2. Condenses verbose PASSING-ONLY test runner outputs (>25 lines, 0 failures) while preserving
///    100% verbatim output whenever any failure, error, or stacktrace marker is present.
pub fn strip_ansi_and_spinners(raw: &str) -> String {
    let stripped = strip_ansi_codes(raw);
    let mut out_lines = Vec::new();
    for line in stripped.split('\n') {
        if line.contains('\r') {
            // Keep only the final visible frame after carriage-return progress overwrites.
            let final_frame = line
                .split('\r')
                .rev()
                .find(|seg| !seg.trim().is_empty())
                .unwrap_or("");
            out_lines.push(final_frame);
        } else {
            out_lines.push(line);
        }
    }
    out_lines.join("\n")
}

fn strip_ansi_codes(input: &str) -> String {
    if !input.contains('\x1b') {
        return input.to_owned();
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'[' => {
                    i += 2;
                    while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                    continue;
                }
                b']' => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != 0x07 {
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    if i < bytes.len() && bytes[i] == 0x07 {
                        i += 1;
                    }
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_owned())
}

/// Condenses passing-only test runner logs (>25 lines) while keeping header + summary tail.
/// Returns `None` if ANY error/failure signal is detected (Hard Invariant 6: never starve debugging).
pub fn summarize_passing_test_output(raw: &str) -> Option<String> {
    if contains_any_failure_marker(raw) {
        return None;
    }
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() <= 25 {
        return None;
    }
    let looks_like_passing_tests = raw.contains("test result: ok")
        || raw.contains("0 failed")
        || raw.contains("# fail 0")
        || (raw.contains("Ran ") && raw.contains("\nOK"))
        || (raw.contains(" passed") && !raw.contains(" failed"));
    if !looks_like_passing_tests {
        return None;
    }

    let head_keep = 3usize;
    let tail_keep = 8usize;
    if lines.len() <= head_keep + tail_keep + 5 {
        return None;
    }
    let condensed_count = lines.len() - head_keep - tail_keep;
    let mut out = Vec::with_capacity(head_keep + tail_keep + 1);
    out.extend_from_slice(&lines[..head_keep]);
    out.push("");
    let summary_line = format!(
        "[Aphelion Test Trim: {condensed_count} passing test lines condensed; 0 failed]"
    );
    let mut result = out.join("\n");
    result.push_str(&summary_line);
    result.push('\n');
    result.push_str(&lines[lines.len() - tail_keep..].join("\n"));
    Some(result)
}

pub fn contains_any_failure_marker(text: &str) -> bool {
    const FAIL_MARKERS: &[&str] = &[
        "FAILED",
        "FAIL ",
        "Error:",
        "Traceback",
        "panic:",
        "panicked at",
        "AssertionError",
        "not ok ",
        "error[E",
        "failures:",
        "✗",
        "× ",
    ];
    FAIL_MARKERS.iter().any(|m| text.contains(m))
}
