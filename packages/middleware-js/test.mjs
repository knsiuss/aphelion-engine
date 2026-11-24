import assert from "node:assert/strict";
import { createAphelionMiddleware } from "./src/index.mjs";

const dec = new TextDecoder();
const enc = new TextEncoder();

const mw = createAphelionMiddleware({ profile: "LongLived" });
assert.equal(mw.profile, "LongLived");

// Multi-turn prefix ordering & tool schema preservation
const tools = [{ type: "function", function: { name: "read_file", description: "Read file" } }];
const t0 = dec.decode(
  mw.transformRequest(
    JSON.stringify({ messages: [{ role: "user", content: "hi" }], tools, model: "gpt-4o", system: "sys" }),
    { sessionId: "s1" }
  )
);
const t1 = dec.decode(
  mw.transformRequest(
    JSON.stringify({
      messages: [
        { role: "user", content: "hi" },
        { role: "assistant", content: "ok" },
        { role: "user", content: "next" },
      ],
      tools,
      model: "gpt-4o",
      system: "sys",
    }),
    { sessionId: "s1" }
  )
);
assert.ok(t0.startsWith('{"model":"gpt-4o","system":"sys","tools":'));
const prefixLen = t0.indexOf(',"messages":');
assert.equal(t0.slice(0, prefixLen), t1.slice(0, prefixLen), "frozen prefix mismatch across turns");
assert.deepEqual(JSON.parse(t1).tools, tools, "tools[] schema must remain immutable");

// SSE stream pass-through preserving tool_calls and [DONE]
const sseStream = new ReadableStream({
  start(c) {
    c.enqueue(enc.encode('data: {"choices":[{"delta":{"tool_calls":[{"function":{"name":"read_file"}}]}}]}\n\n'));
    c.enqueue(enc.encode("data: [DONE]\n\n"));
    c.close();
  },
});
const reader = mw.transformSseStream(sseStream, { sessionId: "s1" }).getReader();
let combined = "";
while (true) {
  const { done, value } = await reader.read();
  if (done) break;
  combined += dec.decode(value);
}
assert.ok(combined.includes("read_file"));
assert.ok(combined.includes("[DONE]"));

mw.dispose();
console.log("packages/middleware-js test.mjs PASS");
