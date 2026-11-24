const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * Creates an in-process Aphelion router middleware (`ExecutionProfile::LongLived`)
 * for drop-in integration into Node-based routers (9Router) or custom fetch pipelines.
 * Bridges to `aphelion-ffi` when a native/WASM loader is provided, or executes a
 * prefix-safe JS pass-through & SSE framing pipeline in zero-dependency environments.
 */
export function createAphelionMiddleware(options = {}) {
  const profile = options.profile ?? "LongLived";
  const nativeBridge = options.nativeBridge ?? null;
  const sessions = new Map();

  function getOrCreateSession(sessionId = "default") {
    if (!sessions.has(sessionId)) {
      const handle = nativeBridge ? nativeBridge.sessionNew() : { frozenPrefix: null, thinkTail: "" };
      sessions.set(sessionId, handle);
    }
    return sessions.get(sessionId);
  }

  function transformRequest(rawBody, sessionHint = {}) {
    const bytes = typeof rawBody === "string" ? encoder.encode(rawBody) : rawBody;
    const session = getOrCreateSession(sessionHint.sessionId);

    if (nativeBridge) {
      return nativeBridge.prepareRequest(session, bytes);
    }

    // Prefix-safe fallback preserving Hard Invariants 1 & 2 when FFI bridge is unloaded.
    try {
      const parsed = JSON.parse(decoder.decode(bytes));
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        const ordered = {};
        for (const key of ["anthropic_version", "model", "system", "tools"]) {
          if (key in parsed) ordered[key] = parsed[key];
        }
        for (const key of Object.keys(parsed).sort()) {
          if (!(key in ordered)) ordered[key] = parsed[key];
        }
        return encoder.encode(JSON.stringify(ordered));
      }
    } catch {
      return bytes;
    }
    return bytes;
  }

  function transformSseStream(readableStream, sessionHint = {}) {
    const session = getOrCreateSession(sessionHint.sessionId);
    const reader = readableStream.getReader();

    return new ReadableStream({
      async pull(controller) {
        const { done, value } = await reader.read();
        if (done) {
          controller.close();
          return;
        }
        const chunk = value instanceof Uint8Array ? value : encoder.encode(String(value));
        const out = nativeBridge ? nativeBridge.processSseChunk(session, chunk) : chunk;
        if (out.byteLength > 0) {
          controller.enqueue(out);
        }
      },
      cancel(reason) {
        return reader.cancel(reason);
      },
    });
  }

  function dispose(sessionId) {
    if (sessionId) {
      const handle = sessions.get(sessionId);
      if (handle && nativeBridge) nativeBridge.sessionFree(handle);
      sessions.delete(sessionId);
      return;
    }
    for (const handle of sessions.values()) {
      if (nativeBridge) nativeBridge.sessionFree(handle);
    }
    sessions.clear();
  }

  return { profile, transformRequest, transformSseStream, dispose };
}
