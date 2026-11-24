use crate::types::{BlockSpan, CodeLang};
use crate::verifier::ts_language_for;
use std::collections::HashMap;
use tree_sitter::Parser;

/// Splices a model-emitted `@patch:@block:<id>` replacement into the cached original source
/// and validates zero Tree-sitter `ERROR` nodes.
pub fn apply_block_patch(span: &BlockSpan, patched_block: &str) -> Option<String> {
    if span.byte_end > span.original_source.len() || span.byte_start > span.byte_end {
        return None;
    }
    if !span.original_source.is_char_boundary(span.byte_start)
        || !span.original_source.is_char_boundary(span.byte_end)
    {
        return None;
    }
    let mut reconstructed = String::with_capacity(span.original_source.len() + patched_block.len());
    reconstructed.push_str(&span.original_source[..span.byte_start]);
    reconstructed.push_str(patched_block);
    reconstructed.push_str(&span.original_source[span.byte_end..]);

    if !validate_syntax(span.language, &reconstructed) {
        return None;
    }
    Some(reconstructed)
}

/// Parses `@patch:@block:<id>\n```lang\n...\n```` and reconstructs full source via `apply_block_patch`.
pub fn reconstruct_from_patch(
    raw_patch: &str,
    known_blocks: &HashMap<String, BlockSpan>,
) -> Option<String> {
    let rest = raw_patch.trim().strip_prefix("@patch:@block:")?;
    let mut lines = rest.lines();
    let block_id = lines.next()?.trim();
    let span = known_blocks.get(block_id)?;

    let body_after_id = rest.get(block_id.len()..)?.trim();
    let code_body = extract_fenced_or_raw(body_after_id);
    apply_block_patch(span, code_body)
}

fn extract_fenced_or_raw(input: &str) -> &str {
    if let Some(start) = input.find("```") {
        if let Some(nl) = input[start + 3..].find('\n') {
            let code_start = start + 3 + nl + 1;
            if let Some(end) = input[code_start..].find("```") {
                return &input[code_start..code_start + end];
            }
        }
    }
    input
}

/// Validates that Tree-sitter produces zero `ERROR` nodes on the reconstructed source.
pub fn validate_syntax(lang: CodeLang, source: &str) -> bool {
    let Some(ts_lang) = ts_language_for(lang) else {
        return true;
    };
    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return false;
    }
    let Some(tree) = parser.parse(source, None) else {
        return false;
    };
    !tree.root_node().has_error()
}

#[cfg(test)]
mod tests {
    use super::apply_block_patch;
    use crate::types::{BlockSpan, CodeLang};

    const SOURCE: &str = "fn a() {}\nfn b() {}\n";

    fn rust_span(byte_start: usize, byte_end: usize) -> BlockSpan {
        BlockSpan {
            block_id: "b#0f0f".to_owned(),
            file_hint: "inline".to_owned(),
            language: CodeLang::Rust,
            byte_start,
            byte_end,
            original_source: SOURCE.to_owned(),
        }
    }

    #[test]
    fn apply_block_patch_replaces_only_the_targeted_byte_span() {
        // Second function occupies bytes 10..20; the first must survive untouched.
        let span = rust_span(10, 20);

        let patched = apply_block_patch(&span, "fn b() { return; }\n")
            .expect("valid Rust patch that keeps the file parseable");

        assert_eq!(patched, "fn a() {}\nfn b() { return; }\n");
    }

    #[test]
    fn apply_block_patch_rejects_span_past_end_of_source() {
        let span = rust_span(0, SOURCE.len() + 1);

        assert_eq!(apply_block_patch(&span, "fn a() {}\n"), None);
    }
}
