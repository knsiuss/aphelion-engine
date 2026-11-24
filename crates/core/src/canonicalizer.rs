use crate::types::{NormalizedRequest, SessionState};
use crate::CoreError;
use serde_json::{Map, Value};

const PREFIX_KEYS: &[&str] = &["anthropic_version", "model", "system", "tools"];

/// Canonicalizes JSON keys on Turn-0 with static prefix keys (`anthropic_version`, `model`,
/// `system`, `tools`) serialized FIRST so `out[..frozen_prefix_len]` is byte-for-byte
/// identical across all subsequent turns (Hard Invariant 1: Zero Prefix Mutation).
pub fn apply_prefix_lock(
    ir: &mut NormalizedRequest,
    state: &mut SessionState,
) -> Result<(), CoreError> {
    state.sacred_cache_boundary = state.sacred_cache_boundary.max(ir.sacred_boundary);

    // Explicit `cache_control` breakpoints are Sacred Read-Only Zones: zero reordering.
    if state.sacred_cache_boundary > 0 {
        if ir.is_turn_zero {
            let wire = serde_json::to_vec(&ir.root)?;
            let bound = state.sacred_cache_boundary.min(wire.len());
            state.frozen_prefix_len = bound;
            state.frozen_prefix_bytes = wire[..bound].to_vec();
        }
        return Ok(());
    }

    if ir.is_turn_zero {
        extract_volatile_system_timestamp(ir);
        ir.root = canonicalize_root(&ir.root);
        let prefix_len = compute_wire_prefix_len(&ir.root);
        let wire = serde_json::to_vec(&ir.root)?;
        let bound = prefix_len.min(wire.len());
        state.frozen_prefix_len = bound;
        state.frozen_prefix_bytes = wire[..bound].to_vec();
    } else if state.frozen_prefix_len > 0 {
        // Restore Turn-0 static prefix keys before canonicalizing so prefix bytes never drift.
        ir.root = canonicalize_root(&ir.root);
        let wire = serde_json::to_vec(&ir.root)?;
        if !verify_prefix_invariant(&state.frozen_prefix_bytes, &wire, state.frozen_prefix_len) {
            restore_frozen_prefix_fields(&mut ir.root, &state.frozen_prefix_bytes);
            ir.root = canonicalize_root(&ir.root);
        }
    }

    Ok(())
}

/// Verifies Hard Invariant 1 prefix immutability across two serialized wire payloads.
pub fn verify_prefix_invariant(prev: &[u8], curr: &[u8], frozen_len: usize) -> bool {
    if frozen_len == 0 {
        return true;
    }
    prev.len() >= frozen_len
        && curr.len() >= frozen_len
        && prev[..frozen_len] == curr[..frozen_len]
}

/// Orders top-level static prefix keys first, then all remaining keys lexicographically.
pub fn canonicalize_root(val: &Value) -> Value {
    let Value::Object(map) = val else {
        return sort_json_keys(val);
    };
    let mut ordered = Map::new();
    for &pk in PREFIX_KEYS {
        if let Some(v) = map.get(pk) {
            ordered.insert(pk.to_owned(), sort_json_keys(v));
        }
    }
    let mut rest: Vec<&String> = map
        .keys()
        .filter(|k| !PREFIX_KEYS.contains(&k.as_str()))
        .collect();
    rest.sort();
    for k in rest {
        if let Some(v) = map.get(k) {
            ordered.insert(k.clone(), sort_json_keys(v));
        }
    }
    Value::Object(ordered)
}

pub fn sort_json_keys(val: &Value) -> Value {
    match val {
        Value::Object(map) => {
            let mut sorted = Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                if let Some(v) = map.get(k) {
                    sorted.insert(k.clone(), sort_json_keys(v));
                }
            }
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sort_json_keys).collect()),
        other => other.clone(),
    }
}

fn extract_volatile_system_timestamp(ir: &mut NormalizedRequest) {
    let Some(sys) = ir
        .root
        .get("system")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    for marker in ["Time:", "Current time:", "Date:", "Timestamp:"] {
        if let Some(idx) = sys.find(marker) {
            let clean = sys[..idx].trim_end().to_owned();
            let env_note = format!("[Env: {}]", sys[idx..].trim());
            ir.root["system"] = Value::String(clean);
            ir.tail_directives.push(env_note);
            return;
        }
    }
}

/// Computes the exact wire byte length covering all leading static prefix keys.
fn compute_wire_prefix_len(root: &Value) -> usize {
    let mut prefix_only = Map::new();
    for &pk in PREFIX_KEYS {
        if let Some(v) = root.get(pk) {
            prefix_only.insert(pk.to_owned(), v.clone());
        }
    }
    if prefix_only.is_empty() {
        return 0;
    }
    // Wire JSON of prefix_only is `{...}`; drop the closing `}` to get the exact shared prefix.
    serde_json::to_vec(&Value::Object(prefix_only))
        .map(|b| b.len().saturating_sub(1))
        .unwrap_or(0)
}

fn restore_frozen_prefix_fields(root: &mut Value, frozen_prefix: &[u8]) {
    let mut closed = Vec::with_capacity(frozen_prefix.len() + 1);
    closed.extend_from_slice(frozen_prefix);
    closed.push(b'}');
    let Ok(Value::Object(prefix_map)) = serde_json::from_slice::<Value>(&closed) else {
        return;
    };
    if let Some(obj) = root.as_object_mut() {
        for (k, v) in prefix_map {
            obj.insert(k, v);
        }
    }
}
