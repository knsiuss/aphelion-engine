import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const dir = path.dirname(fileURLToPath(import.meta.url));
const bin = path.join(dir, "bin", "aphelion.js");

const help = spawnSync(process.execPath, [bin, "--help"], { encoding: "utf8" });
assert.equal(help.status, 0, `expected exit 0 for --help, got ${help.status}: ${help.stderr}`);
assert.ok(help.stderr.includes("aphelion --audit-summary"), "usage output missing --audit-summary");

const summary = spawnSync(process.execPath, [bin, "--audit-summary"], {
  encoding: "utf8",
  env: { ...process.env, APHELION_AUDIT_DB: path.join(dir, ".tmp_test_audit.db") },
});
assert.equal(summary.status, 0, `expected exit 0 for --audit-summary, got ${summary.status}`);
assert.ok(summary.stdout.includes("Total Requests"), "summary output missing Total Requests table");

console.log("packages/cli-npm test.mjs PASS");
