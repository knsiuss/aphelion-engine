//! C-ABI and WASM-compatible bindings for Aphelion (`ExecutionProfile::LongLived`).
//! Designed for embedding into 9Router, LiteLLM, Node.js, Python, and WASM runtimes.

use aphelion_core::stream_fsm::StreamFsm;
use aphelion_core::types::{ExecutionProfile, SessionState};
use std::slice;

/// Opaque session handle for C-ABI / WASM hosts.
pub struct AphelionHandle {
    state: SessionState,
    fsm: StreamFsm,
}

#[repr(C)]
pub struct AphelionBuffer {
    pub ptr: *mut u8,
    pub len: usize,
    pub cap: usize,
}

impl AphelionBuffer {
    fn empty() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        }
    }

    fn from_vec(mut v: Vec<u8>) -> Self {
        let ptr = v.as_mut_ptr();
        let len = v.len();
        let cap = v.capacity();
        std::mem::forget(v);
        Self { ptr, len, cap }
    }
}

/// Allocates a new `AphelionHandle` for a long-lived session.
#[no_mangle]
pub extern "C" fn aphelion_session_new() -> *mut AphelionHandle {
    let handle = Box::new(AphelionHandle {
        state: SessionState::default(),
        fsm: StreamFsm::default(),
    });
    Box::into_raw(handle)
}

/// Frees an `AphelionHandle` previously created by `aphelion_session_new`.
///
/// # Safety
/// `handle` must be a valid pointer returned by `aphelion_session_new` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn aphelion_session_free(handle: *mut AphelionHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// Transforms an outbound JSON request under `ExecutionProfile::LongLived`.
///
/// # Safety
/// `handle` must be valid; `input_ptr` must point to `input_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn aphelion_prepare_request(
    handle: *mut AphelionHandle,
    input_ptr: *const u8,
    input_len: usize,
) -> AphelionBuffer {
    if handle.is_null() || input_ptr.is_null() || input_len == 0 {
        return AphelionBuffer::empty();
    }
    let handle_ref = unsafe { &mut *handle };
    let raw = unsafe { slice::from_raw_parts(input_ptr, input_len) };
    match aphelion_core::prepare_request(raw, ExecutionProfile::LongLived, &mut handle_ref.state) {
        Ok(transformed) => AphelionBuffer::from_vec(transformed.body),
        Err(_) => AphelionBuffer::from_vec(raw.to_vec()),
    }
}

/// Processes an incremental SSE wire chunk through `StreamFsm`.
///
/// # Safety
/// `handle` must be valid; `chunk_ptr` must point to `chunk_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn aphelion_process_sse_chunk(
    handle: *mut AphelionHandle,
    chunk_ptr: *const u8,
    chunk_len: usize,
) -> AphelionBuffer {
    if handle.is_null() || chunk_ptr.is_null() || chunk_len == 0 {
        return AphelionBuffer::empty();
    }
    let handle_ref = unsafe { &mut *handle };
    let chunk = unsafe { slice::from_raw_parts(chunk_ptr, chunk_len) };
    let out = aphelion_core::process_sse_chunk(&mut handle_ref.fsm, chunk, &mut handle_ref.state);
    AphelionBuffer::from_vec(out)
}

/// Releases a buffer returned by `aphelion_prepare_request` or `aphelion_process_sse_chunk`.
///
/// # Safety
/// `buf` must have been returned by an `aphelion_*` function and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn aphelion_free_buffer(buf: AphelionBuffer) {
    if !buf.ptr.is_null() && buf.cap > 0 {
        drop(unsafe { Vec::from_raw_parts(buf.ptr, buf.len, buf.cap) });
    }
}
