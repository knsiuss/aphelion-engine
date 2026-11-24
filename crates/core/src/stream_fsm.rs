use crate::patcher;
use crate::sse_dialect;
use crate::types::SessionState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsmState {
    Thinking,
    SniffMode,
    PatchBuffer,
    PassThrough,
}

/// Zero-panic incremental byte-level SSE state machine.
/// Supports OpenAI/DeepSeek (`reasoning_content`, `<think>`, `tool_calls`),
/// Anthropic (`thinking_delta`, `text_delta`, `input_json_delta`),
/// UTF-8 multi-byte boundary safety, and `@patch:@block` AST reconstruction.
pub struct StreamFsm {
    pub state: FsmState,
    think_tail: Vec<u8>,
    sniff_buf: Vec<u8>,
    sse_line_buf: Vec<u8>,
    utf8_carry: Vec<u8>,
    reasoning_active: bool,
}

impl Default for StreamFsm {
    fn default() -> Self {
        Self {
            state: FsmState::SniffMode,
            think_tail: Vec::with_capacity(320),
            sniff_buf: Vec::with_capacity(64),
            sse_line_buf: Vec::with_capacity(512),
            utf8_carry: Vec::with_capacity(4),
            reasoning_active: false,
        }
    }
}

impl StreamFsm {
    /// Processes arbitrarily split raw SSE (`data: {...}\n\n`) or Ollama NDJSON (`{...}\n`) chunks.
    pub fn process_sse_chunk(&mut self, chunk: &[u8], session: &mut SessionState) -> Vec<u8> {
        self.sse_line_buf.extend_from_slice(chunk);
        let mut output = Vec::new();

        loop {
            if self.sse_line_buf.first() == Some(&b'{') {
                if let Some(nl) = self.sse_line_buf.iter().position(|&b| b == b'\n') {
                    let frame: Vec<u8> = self.sse_line_buf.drain(..=nl).collect();
                    output
                        .extend_from_slice(&sse_dialect::process_sse_frame(self, &frame, session));
                    continue;
                }
                break;
            }
            if let Some(pos) = find_subslice(&self.sse_line_buf, b"\n\n") {
                let frame: Vec<u8> = self.sse_line_buf.drain(..pos + 2).collect();
                output.extend_from_slice(&sse_dialect::process_sse_frame(self, &frame, session));
            } else {
                break;
            }
        }
        output
    }

    pub fn force_passthrough(&mut self) {
        self.state = FsmState::PassThrough;
    }

    /// Feeds out-of-band reasoning deltas (`reasoning_content` / `thinking_delta`).
    pub fn ingest_reasoning_delta(&mut self, bytes: &[u8], session: &mut SessionState) {
        self.reasoning_active = true;
        self.push_think_window(bytes);
        let text = String::from_utf8_lossy(&self.think_tail);
        session.last_thought_anchor = extract_terminal_sentences(&text);
    }

    pub fn finalize_reasoning_if_active(&mut self, session: &mut SessionState) {
        if !self.reasoning_active {
            return;
        }
        let text = String::from_utf8_lossy(&self.think_tail);
        if let Some(anchor) = extract_terminal_sentences(&text) {
            session.last_thought_anchor = Some(anchor);
        }
        self.think_tail.clear();
        self.reasoning_active = false;
    }

    /// Processes raw content bytes safely across split UTF-8 multi-byte sequences.
    pub fn process_text_delta(&mut self, delta: &[u8], session: &mut SessionState) -> Vec<u8> {
        let mut combined = std::mem::take(&mut self.utf8_carry);
        combined.extend_from_slice(delta);

        let valid_len = utf8_complete_prefix_len(&combined);
        if valid_len < combined.len() {
            self.utf8_carry.extend_from_slice(&combined[valid_len..]);
            combined.truncate(valid_len);
        }
        if combined.is_empty() {
            return Vec::new();
        }

        match self.state {
            FsmState::PassThrough => combined,
            FsmState::Thinking => {
                self.push_think_window(&combined);
                if let Some(end_pos) = find_subslice(&self.think_tail, b"</think>") {
                    let valid = &self.think_tail[..end_pos];
                    let text = String::from_utf8_lossy(valid);
                    session.last_thought_anchor =
                        extract_terminal_sentences(text.trim_start_matches("<think>"));
                    self.state = FsmState::SniffMode;
                    self.think_tail.clear();
                }
                combined
            }
            FsmState::SniffMode => {
                self.sniff_buf.extend_from_slice(&combined);
                if self.sniff_buf.starts_with(b"<think>") {
                    self.state = FsmState::Thinking;
                    let out = std::mem::take(&mut self.sniff_buf);
                    self.push_think_window(&out);
                    if let Some(end_pos) = find_subslice(&self.think_tail, b"</think>") {
                        let text = String::from_utf8_lossy(&self.think_tail[..end_pos]);
                        session.last_thought_anchor =
                            extract_terminal_sentences(text.trim_start_matches("<think>"));
                        self.state = FsmState::SniffMode;
                        self.think_tail.clear();
                    }
                    return out;
                }
                if self.sniff_buf.starts_with(b"@patch:@block:") {
                    self.state = FsmState::PatchBuffer;
                    return Vec::new();
                }
                let might_be_think = b"<think>".starts_with(&self.sniff_buf);
                let might_be_patch = b"@patch:@block:".starts_with(&self.sniff_buf);
                if (!might_be_think && !might_be_patch) || self.sniff_buf.len() >= 64 {
                    self.state = FsmState::PassThrough;
                    return std::mem::take(&mut self.sniff_buf);
                }
                Vec::new()
            }
            FsmState::PatchBuffer => {
                self.sniff_buf.extend_from_slice(&combined);
                Vec::new()
            }
        }
    }

    pub fn flush_text_buffers(&mut self, session: &SessionState) -> Vec<u8> {
        let mut buf = std::mem::take(&mut self.sniff_buf);
        buf.extend_from_slice(&std::mem::take(&mut self.utf8_carry));
        if self.state == FsmState::PatchBuffer {
            self.state = FsmState::PassThrough;
            if let Ok(raw) = std::str::from_utf8(&buf) {
                if let Some(reconstructed) =
                    patcher::reconstruct_from_patch(raw, &session.known_blocks)
                {
                    return reconstructed.into_bytes();
                }
            }
        }
        buf
    }

    pub fn flush_with_session(&mut self, session: &SessionState) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.sse_line_buf);
        out.extend_from_slice(&self.flush_text_buffers(session));
        out
    }

    fn push_think_window(&mut self, bytes: &[u8]) {
        self.think_tail.extend_from_slice(bytes);
        if self.think_tail.len() > 320 {
            let mut drain_len = self.think_tail.len() - 320;
            while drain_len < self.think_tail.len() && (self.think_tail[drain_len] & 0xC0) == 0x80 {
                drain_len += 1;
            }
            self.think_tail.drain(..drain_len);
        }
    }
}

/// Returns the byte length of the longest valid UTF-8 prefix, leaving any incomplete trailing
/// multi-byte sequence (1-3 bytes) for the next chunk without panicking or dropping bytes.
fn utf8_complete_prefix_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(err) => {
            if err.error_len().is_none() {
                err.valid_up_to()
            } else {
                bytes.len()
            }
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn extract_terminal_sentences(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed
        .split(['.', '!', '?'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let take = parts.len().min(2);
    let anchor = parts[parts.len() - take..].join(". ");
    Some(format!("{anchor}."))
}
