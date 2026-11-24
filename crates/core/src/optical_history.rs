use crate::diet_gc::estimate_calibrated_tokens;
use crate::types::{Dialect, NormalizedRequest, SessionState};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use png::{BitDepth, ColorType, Encoder};
use serde_json::{json, Value};

pub const OPTICAL_HISTORY_WATERMARK_TOKENS: usize = 6_000;
const TILE_SIZE: u32 = 1024;
const CELL_W: u32 = 6;
const CELL_H: u32 = 10;

/// Rasterizes archived non-code terminal/DOM logs into 1024x1024 monochrome PNG base64
/// image blocks when `supports_vision == true` and non-code log tokens >= 6,000.
/// Hard Invariant 5: NEVER rasterizes active source code files or fenced code blocks.
pub fn rasterize_if_eligible(ir: &mut NormalizedRequest, state: &SessionState) {
    if !ir.supports_vision || ir.is_turn_zero || state.sacred_cache_boundary > 0 {
        return;
    }
    let dialect = ir.dialect;
    let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if messages.len() < 3 {
        return;
    }

    let last_idx = messages.len() - 1;
    let mut eligible_indices = Vec::new();
    let mut total_tokens = 0usize;

    for (idx, msg) in messages[..last_idx].iter().enumerate() {
        if msg.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let Some(content) = msg.get("content").and_then(Value::as_str) else {
            continue;
        };
        if is_protected_source_code(content) || content.starts_with("[AgentDiet Tombstone:") {
            continue;
        }
        let tok = estimate_calibrated_tokens(content);
        if tok > 200 {
            total_tokens += tok;
            eligible_indices.push(idx);
        }
    }

    if total_tokens < OPTICAL_HISTORY_WATERMARK_TOKENS {
        return;
    }

    let mut combined_logs = String::new();
    for &idx in &eligible_indices {
        if let Some(c) = messages[idx].get("content").and_then(Value::as_str) {
            combined_logs.push_str(c);
            combined_logs.push('\n');
        }
    }

    if let Some(png_b64) = render_monospace_tile_base64(&combined_logs, None) {
        ir.stage_stats.optical_tiles_emitted = 1;
        if let Some(&first_idx) = eligible_indices.first() {
            messages[first_idx]["content"] = build_vision_block(dialect, &png_b64, total_tokens);
        }
        for &idx in eligible_indices.iter().skip(1) {
            messages[idx]["content"] =
                Value::String("[OpticalHistory: merged into AgentOCR 1024x1024 tile]".to_owned());
        }
    }
}

fn is_protected_source_code(content: &str) -> bool {
    content.contains("```")
        || content.contains("fn ")
        || content.contains("pub struct ")
        || content.contains("def ")
        || content.contains("export function ")
        || content.contains("class ")
}

fn build_vision_block(dialect: Dialect, b64: &str, tokens: usize) -> Value {
    let caption = format!("[AgentOCR OpticalHistory Tile: {tokens} archived log tokens]");
    match dialect {
        Dialect::OpenAiChat => json!([
            { "type": "text", "text": caption },
            { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{b64}") } }
        ]),
        Dialect::AnthropicMessages => json!([
            { "type": "text", "text": caption },
            { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": b64 } }
        ]),
    }
}

/// Pure-CPU 1024x1024 high-contrast monochrome PNG rasterizer.
/// Uses `fontdue` when font bytes are provided; otherwise uses deterministic 5x7 pixel glyphs.
pub fn render_monospace_tile_base64(text: &str, font_bytes: Option<&[u8]>) -> Option<String> {
    let mut pixels = vec![255u8; (TILE_SIZE * TILE_SIZE) as usize];

    if let Some(bytes) = font_bytes {
        if let Ok(font) = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()) {
            rasterize_with_fontdue(&font, text, &mut pixels);
            return encode_gray_png_base64(&pixels);
        }
    }

    rasterize_builtin_bitmap(text, &mut pixels);
    encode_gray_png_base64(&pixels)
}

fn rasterize_with_fontdue(font: &fontdue::Font, text: &str, pixels: &mut [u8]) {
    let mut x: u32 = 8;
    let mut y: u32 = 8;
    for ch in text.chars() {
        if ch == '\n' {
            x = 8;
            y += CELL_H;
            if y + CELL_H >= TILE_SIZE {
                break;
            }
            continue;
        }
        let (metrics, bitmap) = font.rasterize(ch, 9.0);
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let px = x + col as u32;
                let py = y + row as u32;
                if px < TILE_SIZE && py < TILE_SIZE {
                    let cov = bitmap[row * metrics.width + col];
                    pixels[(py * TILE_SIZE + px) as usize] = 255u8.saturating_sub(cov);
                }
            }
        }
        x += CELL_W;
        if x + CELL_W >= TILE_SIZE - 8 {
            x = 8;
            y += CELL_H;
            if y + CELL_H >= TILE_SIZE {
                break;
            }
        }
    }
}

fn rasterize_builtin_bitmap(text: &str, pixels: &mut [u8]) {
    let mut x: u32 = 8;
    let mut y: u32 = 8;
    for ch in text.chars() {
        if ch == '\n' {
            x = 8;
            y += CELL_H;
            if y + CELL_H >= TILE_SIZE {
                break;
            }
            continue;
        }
        if !ch.is_ascii_whitespace() {
            // Deterministic high-contrast 4x7 glyph pattern seeded by ASCII code.
            let code = ch as u32;
            for row in 0..7u32 {
                let bits = ((code.wrapping_mul(31) ^ (row * 17)) & 0x0F) | 0x01;
                for col in 0..4u32 {
                    if (bits >> col) & 1 == 1 {
                        let px = x + col;
                        let py = y + row;
                        if px < TILE_SIZE && py < TILE_SIZE {
                            pixels[(py * TILE_SIZE + px) as usize] = 0;
                        }
                    }
                }
            }
        }
        x += CELL_W;
        if x + CELL_W >= TILE_SIZE - 8 {
            x = 8;
            y += CELL_H;
            if y + CELL_H >= TILE_SIZE {
                break;
            }
        }
    }
}

fn encode_gray_png_base64(pixels: &[u8]) -> Option<String> {
    let mut out = Vec::with_capacity(16 * 1024);
    {
        let mut encoder = Encoder::new(&mut out, TILE_SIZE, TILE_SIZE);
        encoder.set_color(ColorType::Grayscale);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(pixels).ok()?;
    }
    Some(BASE64.encode(out))
}
