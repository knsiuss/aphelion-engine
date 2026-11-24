use std::sync::OnceLock;
use tiktoken_rs::{cl100k_base, o200k_base, CoreBPE};

static CL100K: OnceLock<Option<CoreBPE>> = OnceLock::new();
static O200K: OnceLock<Option<CoreBPE>> = OnceLock::new();

fn get_cl100k() -> Option<&'static CoreBPE> {
    CL100K.get_or_init(|| cl100k_base().ok()).as_ref()
}

fn get_o200k() -> Option<&'static CoreBPE> {
    O200K.get_or_init(|| o200k_base().ok()).as_ref()
}

/// Counts exact BPE tokens calibrated to the target model family.
/// Falls back to `aphelion_core::diet_gc::estimate_calibrated_tokens` if tokenizer init fails.
pub fn count_tokens_for_model(model: &str, text: &str) -> usize {
    let m = model.to_ascii_lowercase();
    let bpe =
        if m.contains("gpt-4o") || m.contains("gpt-4.1") || m.contains("o1") || m.contains("o3") {
            get_o200k().or_else(get_cl100k)
        } else {
            get_cl100k()
        };

    if let Some(encoder) = bpe {
        let raw = encoder.encode_with_special_tokens(text).len();
        // Anthropic/Claude BPE vocabulary is ~4% denser on code than cl100k_base.
        if m.contains("claude") {
            ((raw as f64) * 1.04).ceil() as usize
        } else {
            raw
        }
    } else {
        aphelion_core::diet_gc::estimate_calibrated_tokens(text)
    }
}

/// Counts exact BPE tokens on raw UTF-8 JSON payload bytes.
pub fn count_payload_tokens(model: &str, payload: &[u8]) -> usize {
    match std::str::from_utf8(payload) {
        Ok(s) => count_tokens_for_model(model, s),
        Err(_) => {
            let lossy = String::from_utf8_lossy(payload);
            count_tokens_for_model(model, &lossy)
        }
    }
}
