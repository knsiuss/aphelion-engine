#!/usr/bin/env node
"use strict";

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const PLATFORM_MAP = {
  "darwin-arm64": "aphelion-darwin-arm64",
  "darwin-x64": "aphelion-darwin-x64",
  "linux-x64": "aphelion-linux-x64-musl",
  "win32-x64": "aphelion-win32-x64.exe",
};

function resolvePrebuiltBinary() {
  const key = `${process.platform}-${process.arch}`;
  const binName = PLATFORM_MAP[key];
  if (!binName) return null;

  const candidates = [
    path.join(__dirname, "native", binName),
    path.join(__dirname, "..", "native", binName),
    path.resolve(__dirname, "..", "..", "..", "target", "release", process.platform === "win32" ? "aphelion.exe" : "aphelion"),
    path.resolve(__dirname, "..", "..", "..", "target", "debug", process.platform === "win32" ? "aphelion.exe" : "aphelion"),
  ];

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return null;
}

function main() {
  const args = process.argv.slice(2);
  const prebuilt = resolvePrebuiltBinary();

  if (prebuilt) {
    if (process.platform !== "win32") {
      try { fs.chmodSync(prebuilt, 0o755); } catch {}
    }
    const res = spawnSync(prebuilt, args, { stdio: "inherit" });
    if (res.signal) {
      process.exit(130);
    }
    process.exit(res.status ?? (res.error ? 1 : 0));
  }

  // Automatic development fallback to Cargo workspace binary
  const workspaceRoot = path.resolve(__dirname, "..", "..", "..");
  const cargoArgs = ["run", "--release", "-p", "aphelion-cli", "--", ...args];
  const res = spawnSync("cargo", cargoArgs, {
    cwd: workspaceRoot,
    stdio: "inherit",
  });
  if (res.error) {
    console.error(`[aphelion] failed to launch binary or cargo fallback: ${res.error.message}`);
    process.exit(1);
  }
  process.exit(res.status ?? 0);
}

main();
