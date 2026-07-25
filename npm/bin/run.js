#!/usr/bin/env node
// npm shim for the `hypermile-audit` Rust binary (spec 01 §2, WP-A4).
//
// On first run it downloads the prebuilt binary for this platform from the
// GitHub release matching this package's version, caches it under the package
// directory, then execs it with all arguments passed through. The audit itself
// makes zero network calls; the only download is this one-time binary fetch
// from GitHub Releases (skipped entirely when the binary is already cached).

"use strict";

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const VERSION = require("../package.json").version;
const REPO = "adrianph98/hypermile-audit";

// Release asset naming convention — the publish runbook (docs/release.md /
// todo 0.10-B) must attach assets with exactly these names.
const TARGETS = {
  "win32-x64": { asset: "hypermile-audit-x86_64-pc-windows-msvc.exe", ext: ".exe" },
  "darwin-x64": { asset: "hypermile-audit-x86_64-apple-darwin", ext: "" },
  "darwin-arm64": { asset: "hypermile-audit-aarch64-apple-darwin", ext: "" },
  "linux-x64": { asset: "hypermile-audit-x86_64-unknown-linux-gnu", ext: "" },
  "linux-arm64": { asset: "hypermile-audit-aarch64-unknown-linux-gnu", ext: "" },
};

async function main() {
  const key = `${process.platform}-${process.arch}`;
  const target = TARGETS[key];
  if (!target) {
    console.error(
      `hypermile-audit: no prebuilt binary for ${key}.\n` +
        `Build from source instead: cargo install --git https://github.com/${REPO}\n` +
        `(https://github.com/${REPO})`
    );
    process.exit(1);
  }

  const cacheDir = path.join(__dirname, "..", ".bin");
  const binPath = path.join(cacheDir, `hypermile-audit-v${VERSION}${target.ext}`);

  if (!fs.existsSync(binPath)) {
    const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${target.asset}`;
    console.error(`hypermile-audit: first run — fetching ${url}`);
    const res = await fetch(url, { redirect: "follow" });
    if (!res.ok) {
      console.error(
        `hypermile-audit: download failed (HTTP ${res.status}).\n` +
          `Check https://github.com/${REPO}/releases or build from source:\n` +
          `  cargo install --git https://github.com/${REPO}`
      );
      process.exit(1);
    }
    fs.mkdirSync(cacheDir, { recursive: true });
    const tmp = `${binPath}.tmp-${process.pid}`;
    fs.writeFileSync(tmp, Buffer.from(await res.arrayBuffer()), { mode: 0o755 });
    fs.renameSync(tmp, binPath);
  }

  const child = spawnSync(binPath, process.argv.slice(2), { stdio: "inherit" });
  if (child.error) {
    console.error(`hypermile-audit: failed to launch binary: ${child.error.message}`);
    process.exit(1);
  }
  process.exit(child.status ?? 1);
}

main().catch((err) => {
  console.error(`hypermile-audit: ${err.message}`);
  process.exit(1);
});
