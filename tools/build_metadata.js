/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Adds a "build_info" custom section, a JSON object describing the build, to
// the wasm module (see "Build metadata" in rust/README.md), and reads it.

"use strict";

const { execFileSync } = require("node:child_process");
const { readFileSync, writeFileSync } = require("node:fs");

const SECTION_NAME = "build_info";

function git(dir, args) {
  return execFileSync("git", args, {
    cwd: dir,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  }).trim();
}

// The revision of the repository, suffixed with -dirty if there are local
// changes, untracked files included.
function getRepoRevision(dir, gitOptions = []) {
  try {
    const revision = git(dir, [...gitOptions, "rev-parse", "HEAD"]);
    const status = git(dir, [
      ...gitOptions,
      "status",
      "--porcelain",
      "--untracked-files=normal",
    ]);
    return status ? `${revision}-dirty` : revision;
  } catch {
    return "unknown";
  }
}

function getWasmBindgenVersion(lockFile) {
  const lock = readFileSync(lockFile, "utf8");
  const packages = lock.matchAll(
    /^\[\[package\]\]\r?\nname = "wasm-bindgen"\r?\nversion = "([^"]+)"$/gm
  );
  const versions = new Set(Array.from(packages, ([, version]) => version));
  if (versions.size !== 1) {
    throw new Error(`Expected exactly one version of wasm-bindgen in ${lockFile}`);
  }
  return [...versions][0];
}

function encodeU32(value) {
  const bytes = [];
  do {
    let byte = value & 0x7f;
    value >>>= 7;
    if (value) {
      byte |= 0x80;
    }
    bytes.push(byte);
  } while (value);
  return Buffer.from(bytes);
}

function addCustomSection(wasm, name, payload) {
  const encodedName = Buffer.from(name, "utf8");
  const content = Buffer.concat([
    encodeU32(encodedName.length),
    encodedName,
    payload,
  ]);
  // A custom section has the id 0 and can be anywhere after the header.
  return Buffer.concat([
    wasm,
    Buffer.from([0]),
    encodeU32(content.length),
    content,
  ]);
}

function getBuildInfoSection(wasm) {
  const module = new WebAssembly.Module(wasm);
  const [section] = WebAssembly.Module.customSections(module, SECTION_NAME);
  return section;
}

function readBuildMetadata(wasmFile) {
  const section = getBuildInfoSection(readFileSync(wasmFile));
  return section ? JSON.parse(new TextDecoder().decode(section)) : undefined;
}

function writeBuildMetadata(wasmFile, info) {
  const wasm = readFileSync(wasmFile);
  if (getBuildInfoSection(wasm)) {
    throw new Error(`${wasmFile} already has a ${SECTION_NAME} section`);
  }
  const json = JSON.stringify(info);
  writeFileSync(wasmFile, addCustomSection(wasm, SECTION_NAME, Buffer.from(json)));
  console.log(`Added ${json} to ${wasmFile}`);
}

if (require.main === module) {
  const args = process.argv.slice(2);
  if (args[0] === "--revision") {
    // The mounted checkout can belong to a different user inside Docker.
    console.log(getRepoRevision(args[1], ["-c", "safe.directory=*"]));
  } else {
    const [wasmFile, lockFile, embeddedData, ...tools] = args;
    const { rustc, wasmPack, binaryen, terser } = Object.fromEntries(
      tools.map(tool => tool.split("=", 2))
    );
    writeBuildMetadata(wasmFile, {
      repo: process.env.REPO_REVISION || "unknown",
      embeddedData: embeddedData !== "0",
      rustc,
      wasmBindgen: getWasmBindgenVersion(lockFile),
      wasmPack,
      binaryen: binaryen || undefined,
      terser: terser || undefined,
    });
  }
}

module.exports = { getRepoRevision, readBuildMetadata };
