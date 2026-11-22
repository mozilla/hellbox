/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Run with: node --test tools/build_metadata.test.js

"use strict";

const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const { mkdtempSync, readFileSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const { test } = require("node:test");
const { readBuildMetadata } = require("./build_metadata.js");

const SCRIPT = require.resolve("./build_metadata.js");
const LOCK = `version = 4

[[package]]
name = "wasm-bindgen"
version = "0.2.129"

[[package]]
name = "wasm-bindgen-shared"
version = "0.2.128"
`;

// The wasm header and one type section.
const WASM = Buffer.from("0061736d01000000010401600000", "hex");

function tempDir(t) {
  const dir = mkdtempSync(join(tmpdir(), "hellbox-metadata-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function setupBuild(t, lock = LOCK) {
  const dir = tempDir(t);
  const lockFile = join(dir, "Cargo.lock");
  const wasmFile = join(dir, "hellbox_bg.wasm");
  writeFileSync(lockFile, lock);
  writeFileSync(wasmFile, WASM);

  function addMetadata(embeddedData = "0", ...tools) {
    execFileSync(
      process.execPath,
      [
        SCRIPT,
        wasmFile,
        lockFile,
        embeddedData,
        "rustc=1.100.0",
        "wasmPack=0.15.1",
        ...tools,
      ],
      { env: { ...process.env, REPO_REVISION: "abc" }, stdio: "pipe" }
    );
  }

  return { wasmFile, addMetadata };
}

test("the revision includes tracked and untracked changes", t => {
  const cwd = tempDir(t);
  const env = { ...process.env };
  delete env.GIT_DIR;
  delete env.GIT_INDEX_FILE;
  const options = { cwd, env, encoding: "utf8" };
  const git = (...args) => execFileSync("git", args, options).trim();
  const revision = () =>
    execFileSync(process.execPath, [SCRIPT, "--revision", "."], options).trim();

  assert.equal(revision(), "unknown");
  git("init", "--quiet");
  const tracked = join(cwd, "Cargo.lock");
  writeFileSync(tracked, LOCK);
  git("add", "Cargo.lock");
  git(
    "-c", "user.name=Test", "-c", "user.email=test@example.com",
    "commit", "--quiet", "--no-gpg-sign", "-m", "Initial commit"
  );

  const head = git("rev-parse", "HEAD");
  assert.equal(revision(), head);

  git("config", "status.showUntrackedFiles", "no");
  const untracked = join(cwd, "untracked.rs");
  writeFileSync(untracked, "untracked\n");
  assert.equal(revision(), `${head}-dirty`);
  rmSync(untracked);
  assert.equal(revision(), head);

  writeFileSync(tracked, `${LOCK}\n`);
  assert.equal(revision(), `${head}-dirty`);
  writeFileSync(join(cwd, ".git", "index"), "invalid index\n");
  assert.equal(revision(), "unknown");
});

test("build metadata records the revision, data mode and tool versions", t => {
  const { wasmFile, addMetadata } = setupBuild(t);
  const info = {
    repo: "abc",
    embeddedData: true,
    rustc: "1.100.0",
    wasmBindgen: "0.2.129",
    wasmPack: "0.15.1",
  };

  addMetadata("1");
  assert.deepEqual(readBuildMetadata(wasmFile), info);

  writeFileSync(wasmFile, WASM);
  addMetadata("0", "binaryen=133", "terser=5.51.2");
  assert.deepEqual(readBuildMetadata(wasmFile), {
    ...info,
    embeddedData: false,
    binaryen: "133",
    terser: "5.51.2",
  });
});

test("build metadata can only be added once", t => {
  const { wasmFile, addMetadata } = setupBuild(t);
  assert.equal(readBuildMetadata(wasmFile), undefined);
  addMetadata();
  assert.throws(addMetadata, /already has a build_info section/);
});

test("build metadata requires a single wasm-bindgen version", t => {
  const lock = `${LOCK}\n[[package]]\nname = "wasm-bindgen"\nversion = "0.2.100"\n`;
  const { wasmFile, addMetadata } = setupBuild(t, lock);
  assert.throws(addMetadata, /Expected exactly one version of wasm-bindgen/);
  assert.deepEqual(readFileSync(wasmFile), WASM);
});
