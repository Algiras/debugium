#!/usr/bin/env node

const { execSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const https = require("https");
const { createWriteStream, mkdirSync } = fs;

const VERSION = "0.1.1";
const REPO = "Algiras/debugium";
const BIN_DIR = path.join(__dirname, "bin");
const BIN_NAME = process.platform === "win32" ? "debugium.exe" : "debugium";
const BIN_PATH = path.join(BIN_DIR, BIN_NAME);

function getPlatformKey() {
  const arch = process.arch === "arm64" ? "aarch64" : "x86_64";
  switch (process.platform) {
    case "darwin":
      return { name: `debugium-macos-${arch}`, ext: "tar.gz" };
    case "linux":
      return { name: `debugium-linux-${arch}`, ext: "tar.gz" };
    case "win32":
      return { name: `debugium-windows-x86_64`, ext: "zip" };
    default:
      throw new Error(`Unsupported platform: ${process.platform}-${process.arch}`);
  }
}

function download(url) {
  return new Promise((resolve, reject) => {
    https.get(url, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return download(res.headers.location).then(resolve, reject);
      }
      if (res.statusCode !== 200) {
        return reject(new Error(`Download failed: HTTP ${res.statusCode}`));
      }
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => resolve(Buffer.concat(chunks)));
      res.on("error", reject);
    }).on("error", reject);
  });
}

async function install() {
  const { name, ext } = getPlatformKey();
  const asset = `${name}.${ext}`;
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${asset}`;

  console.log(`Downloading debugium v${VERSION} for ${process.platform}-${process.arch}...`);
  console.log(`  ${url}`);

  const data = await download(url);

  mkdirSync(BIN_DIR, { recursive: true });

  if (ext === "tar.gz") {
    // Write to temp file, extract with tar
    const tmp = path.join(BIN_DIR, asset);
    fs.writeFileSync(tmp, data);
    execSync(`tar xzf "${tmp}" -C "${BIN_DIR}"`, { stdio: "inherit" });
    fs.unlinkSync(tmp);
  } else {
    // zip on Windows — write and use PowerShell to extract
    const tmp = path.join(BIN_DIR, asset);
    fs.writeFileSync(tmp, data);
    execSync(
      `powershell -command "Expand-Archive -Force '${tmp}' '${BIN_DIR}'"`,
      { stdio: "inherit" }
    );
    fs.unlinkSync(tmp);
  }

  // Make executable
  if (process.platform !== "win32") {
    fs.chmodSync(BIN_PATH, 0o755);
  }

  console.log(`Installed debugium to ${BIN_PATH}`);
}

install().catch((err) => {
  console.error(`Failed to install debugium: ${err.message}`);
  console.error("You can install manually: cargo install --git https://github.com/Algiras/debugium debugium-server");
  process.exit(1);
});
