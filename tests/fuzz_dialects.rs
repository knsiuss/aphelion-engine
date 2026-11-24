use aphelion_core::stream_fsm::{FsmState, StreamFsm};
use aphelion_core::types::{BlockSpan, CodeLang, SessionState};
use serde_json::json;

fn feed_chunked(wire: &[u8], sizes: &[usize], session: &mut SessionState) -> Vec<u8> {
    let mut fsm = StreamFsm::default();
    let mut out = Vec::new();
    let mut off = 0usize;
    let mut idx = 0usize;
    while off < wire.len() {
        let sz = sizes[idx % sizes.len()].max(1);
        let end = (off + sz).min(wire.len());
        out.extend_from_slice(&fsm.process_sse_chunk(&wire[off..end], session));
        off = end;
        idx += 1;
    }
    out.extend_from_slice(&fsm.flush_with_session(session));
    out
}

#[test]
fn anthropic_multiblock_thinking_and_input_json_delta_fragmentation() {
    let events = [
        format!(
            "event: content_block_delta\ndata: {}\n\n",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Memeriksa dependensi modul. Rencana edit sudah jelas."}})
        ),
        format!(
            "event: content_block_delta\ndata: {}\n\n",
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"src/lib.rs\"}"}})
        ),
        format!(
            "event: content_block_delta\ndata: {}\n\n",
            json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"Selesai memperbarui berkas 🚀"}})
        ),
        format!(
            "event: message_stop\ndata: {}\n\n",
            json!({"type":"message_stop"})
        ),
    ];
    let wire = events.concat();

    let mut s_whole = SessionState::default();
    let whole = feed_chunked(wire.as_bytes(), &[wire.len()], &mut s_whole);

    let mut s_one = SessionState::default();
    let one = feed_chunked(wire.as_bytes(), &[1], &mut s_one);

    let mut s_rand = SessionState::default();
    let rand = feed_chunked(wire.as_bytes(), &[2, 1, 5, 3, 1, 7], &mut s_rand);

    assert_eq!(whole, one);
    assert_eq!(whole, rand);
    assert!(s_one
        .last_thought_anchor
        .as_deref()
        .unwrap_or("")
        .contains("Rencana edit sudah jelas"));
}

#[test]
fn deepseek_reasoning_content_and_patch_block_reconstruction() {
    let src = "fn compute(x: u32) -> u32 {\n    x + 1\n}\n";
    let make_session = || {
        let mut s = SessionState::default();
        s.known_blocks.insert(
            "compute#abcd".to_owned(),
            BlockSpan {
                block_id: "compute#abcd".to_owned(),
                file_hint: "math.rs".to_owned(),
                language: CodeLang::Rust,
                byte_start: 0,
                byte_end: src.len(),
                original_source: src.to_owned(),
            },
        );
        s
    };

    let patch = "@patch:@block:compute#abcd\n```rust\nfn compute(x: u32) -> u32 {\n    x.saturating_add(2)\n}\n```";
    let wire = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"reasoning_content":"Analyzing overflow edge case. Use saturating_add(2)."}}]}),
        json!({"choices":[{"delta":{"content": patch}}]}),
    );

    let mut s_whole = make_session();
    let whole = feed_chunked(wire.as_bytes(), &[wire.len()], &mut s_whole);

    let mut s_one = make_session();
    let one = feed_chunked(wire.as_bytes(), &[1], &mut s_one);

    assert_eq!(whole, one);
    let out_str = String::from_utf8(one).expect("valid utf8");
    assert!(out_str.contains("saturating_add(2)"));
    assert!(s_one
        .last_thought_anchor
        .as_deref()
        .unwrap_or("")
        .contains("saturating_add(2)"));
}

#[test]
fn raw_text_delta_mid_codepoint_utf8_emoji_and_indonesian() {
    let text = "Menjalankan optimasi 🚀🔥🦀 — pengujian karakter multi-byte & aksara 🎉 selesai!";
    let mut fsm = StreamFsm::default();
    let mut session = SessionState::default();
    let mut collected = Vec::new();

    for &b in text.as_bytes() {
        let piece = fsm.process_text_delta(&[b], &mut session);
        // Every emitted slice must be strictly valid UTF-8 with zero dropped codepoints.
        assert!(std::str::from_utf8(&piece).is_ok());
        collected.extend_from_slice(&piece);
    }
    collected.extend_from_slice(&fsm.flush_with_session(&session));
    assert_eq!(String::from_utf8(collected).expect("utf8"), text);
}

#[test]
fn fragmented_tool_calls_bypass_patch_buffer() {
    let mut fsm = StreamFsm::default();
    let mut session = SessionState::default();
    let wire = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]}}]})
    );
    let out = fsm.process_sse_chunk(wire.as_bytes(), &mut session);
    assert_eq!(fsm.state, FsmState::PassThrough);
    assert!(String::from_utf8_lossy(&out).contains("read_file"));
}
