#!/usr/bin/env node

const { execFileSync } = require("child_process");
const path = require("path");

const BIN_NAME = process.platform === "win32" ? "debugium.exe" : "debugium";
const BIN_PATH = path.join(__dirname, "bin", BIN_NAME);

try {
  execFileSync(BIN_PATH, process.argv.slice(2), { stdio: "inherit" });
} catch (err) {
  if (err.status != null) process.exit(err.status);
  console.error(`Failed to run debugium: ${err.message}`);
  process.exit(1);
}
