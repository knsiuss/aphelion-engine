use serde_json::Value;

/// Infers minimal active tools from `tools[]` matching the current turn context.
pub fn infer_active_tools(root: &Value, context: &str) -> Vec<String> {
    let Some(tools) = root.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    let ctx_lower = context.to_ascii_lowercase();
    let mut matched = Vec::new();
    for tool in tools {
        let name = tool
            .get("function")
            .and_then(|f| f.get("name"))
            .or_else(|| tool.get("name"))
            .and_then(Value::as_str);
        if let Some(n) = name {
            let n_lower = n.to_ascii_lowercase();
            let relevant = ctx_lower.contains(&n_lower)
                || (ctx_lower.contains("test") && n_lower.contains("run"))
                || (ctx_lower.contains("error")
                    && (n_lower.contains("read") || n_lower.contains("edit")))
                || (ctx_lower.contains("file")
                    && (n_lower.contains("read") || n_lower.contains("edit")));
            if relevant {
                matched.push(n.to_owned());
            }
        }
    }
    matched.truncate(4);
    matched
}

/// Matches deterministic procedural Micro-Skills (20-50 tokens) on error/language signatures.
pub fn match_micro_skill(text: &str) -> Option<(&'static str, &'static str)> {
    if text.contains("error[E0382]") || text.contains("error[E0502]") || text.contains("borrow") {
        Some((
            "rust-borrow",
            "[Micro-Skill:rust-borrow] Check ownership move or overlapping &mut borrow; narrow borrow scope or clone minimal key before mutating.",
        ))
    } else if text.contains("TS2322") || text.contains("TS2339") {
        Some((
            "ts-type",
            "[Micro-Skill:ts-type] Inspect declared interface/type definition first; narrow union types before property access.",
        ))
    } else if text.contains("Traceback")
        && (text.contains("ImportError") || text.contains("KeyError"))
    {
        Some((
            "py-runtime",
            "[Micro-Skill:py-runtime] Verify module path or dict key presence with .get() before indexing.",
        ))
    } else {
        None
    }
}
