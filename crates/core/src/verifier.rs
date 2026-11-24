use crate::types::{BlockSpan, CodeLang, Complexity, NormalizedRequest, SessionState};
use serde_json::Value;
use tree_sitter::{Language, Parser};
use xxhash_rust::xxh3::xxh3_64;

/// Uses Tree-sitter (Rust, TypeScript/JS, Python) to tag functions/classes in code blocks > 40
/// lines with `@block:<symbol>#<hash4>`, compute caller counts, and enforce DEEP on errors.
pub fn analyze_and_anchor(ir: &mut NormalizedRequest, state: &mut SessionState) {
    let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(last) = messages.last_mut() else {
        return;
    };
    let Some(content) = last.get("content").and_then(Value::as_str).map(str::to_owned) else {
        return;
    };

    if contains_error_signal(&content) {
        ir.complexity = Complexity::Deep;
    }

    // Hard Invariant 2: Suppress @patch:@block contracts during active tool-calling turns.
    if ir.active_tool_loop {
        return;
    }

    if let Some((anchored, spans, high_fan_in)) = anchor_code_with_treesitter(&content) {
        if high_fan_in {
            ir.complexity = Complexity::Deep;
        }
        ir.stage_stats.blocks_anchored = spans.len();
        for span in spans {
            state.known_blocks.insert(span.block_id.clone(), span);
        }
        last["content"] = Value::String(anchored);
        ir.tail_directives.push(
            "[Output Contract: Emit only modified blocks using `@patch:@block:<id>` followed by the updated code block.]"
                .to_owned(),
        );
    }
}

fn contains_error_signal(text: &str) -> bool {
    text.contains("Error:")
        || text.contains("Traceback")
        || text.contains("panic:")
        || text.contains("FAILED")
}

pub fn parse_lang_tag(tag: &str) -> CodeLang {
    match tag.trim().to_ascii_lowercase().as_str() {
        "rs" | "rust" => CodeLang::Rust,
        "ts" | "tsx" | "typescript" => CodeLang::TypeScript,
        "js" | "jsx" | "javascript" => CodeLang::JavaScript,
        "py" | "python" => CodeLang::Python,
        _ => CodeLang::Unknown,
    }
}

pub fn ts_language_for(lang: CodeLang) -> Option<Language> {
    match lang {
        CodeLang::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        CodeLang::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        CodeLang::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
        CodeLang::Python => Some(tree_sitter_python::LANGUAGE.into()),
        CodeLang::Unknown => None,
    }
}

fn anchor_code_with_treesitter(content: &str) -> Option<(String, Vec<BlockSpan>, bool)> {
    let fence_start = content.find("```")?;
    let lang_end = content[fence_start + 3..].find('\n')? + fence_start + 3;
    let lang = parse_lang_tag(&content[fence_start + 3..lang_end]);
    let body_start = lang_end + 1;
    let fence_end = content[body_start..].find("```")? + body_start;
    let code = &content[body_start..fence_end];

    if code.lines().count() <= 40 {
        return None;
    }

    let nodes = extract_symbol_ranges(code, lang);
    let comment_prefix = if lang == CodeLang::Python { "#" } else { "//" };

    let mut spans = Vec::new();
    let mut high_fan_in = false;
    let mut insertions: Vec<(usize, String)> = Vec::new();

    if nodes.is_empty() {
        let hash4 = format!("{:04x}", xxh3_64(code.as_bytes()) & 0xFFFF);
        let block_id = format!("target#{hash4}");
        insertions.push((0, format!("{comment_prefix} @block:{block_id}\n")));
        spans.push(BlockSpan {
            block_id,
            file_hint: "inline".to_owned(),
            language: lang,
            byte_start: 0,
            byte_end: code.len(),
            original_source: code.to_owned(),
        });
    } else {
        for (sym, start, end) in nodes {
            let slice = &code[start..end];
            let hash4 = format!("{:04x}", xxh3_64(slice.as_bytes()) & 0xFFFF);
            let block_id = format!("{sym}#{hash4}");
            let callers = count_symbol_references(code, &sym).saturating_sub(1);
            if callers >= 5 {
                high_fan_in = true;
            }
            insertions.push((start, format!("{comment_prefix} @block:{block_id}\n")));
            spans.push(BlockSpan {
                block_id,
                file_hint: "inline".to_owned(),
                language: lang,
                byte_start: start,
                byte_end: end,
                original_source: code.to_owned(),
            });
        }
    }

    insertions.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut tagged_code = code.to_owned();
    for (offset, marker) in insertions {
        if offset <= tagged_code.len() && tagged_code.is_char_boundary(offset) {
            tagged_code.insert_str(offset, &marker);
        }
    }

    let mut anchored = String::with_capacity(content.len() + 128);
    anchored.push_str(&content[..body_start]);
    anchored.push_str(&tagged_code);
    anchored.push_str(&content[fence_end..]);
    Some((anchored, spans, high_fan_in))
}

fn extract_symbol_ranges(code: &str, lang: CodeLang) -> Vec<(String, usize, usize)> {
    let Some(ts_lang) = ts_language_for(lang) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(code, None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut out = Vec::new();
    for child in root.children(&mut cursor) {
        let kind = child.kind();
        let is_target = matches!(
            kind,
            "function_item"
                | "impl_item"
                | "function_declaration"
                | "class_declaration"
                | "function_definition"
                | "class_definition"
        );
        if is_target {
            let name = child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(code.as_bytes()).ok())
                .unwrap_or("block")
                .to_owned();
            out.push((name, child.start_byte(), child.end_byte()));
        }
    }
    out
}

fn count_symbol_references(code: &str, sym: &str) -> usize {
    if sym.is_empty() || sym == "block" {
        return 0;
    }
    code.match_indices(sym).count()
}
