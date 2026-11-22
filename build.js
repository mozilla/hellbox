/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Builds the wasm module with Docker: the toolchain is in an image (see
// rust/Dockerfile), which runs rust/compile.sh.

"use strict";

const fs = require("node:fs/promises");
const { spawn } = require("node:child_process");
const { tmpdir } = require("node:os");
const { join, resolve } = require("node:path");
const { parseArgs } = require("node:util");
const { getRepoRevision } = require("./tools/build_metadata.js");

const IMAGE = "hellbox-builder";
const CRATE = join(__dirname, "rust");

function docker(...args) {
  const child = spawn("docker", args, { stdio: "inherit" });
  return new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("close", code => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`docker ${args[0]} exited with code ${code}`));
    });
  });
}

function buildImage(...options) {
  return docker("build", "-t", IMAGE, ...options, CRATE);
}

// Keep the image ID so another checkout can retag the image before we run it.
async function buildImageId() {
  const tempDir = await fs.mkdtemp(join(tmpdir(), "hellbox-build-"));
  try {
    const imageIdFile = join(tempDir, "image-id");
    await buildImage("--iidfile", imageIdFile);
    return (await fs.readFile(imageIdFile, "utf8")).trim();
  } finally {
    await fs.rm(tempDir, { recursive: true, force: true });
  }
}

async function compile(output, embeddedData, jsFallback) {
  output = resolve(output);
  await fs.mkdir(output, { recursive: true });
  const imageId = await buildImageId();
  // Read the revision on the host so Git can resolve linked worktrees.
  await docker(
    "run",
    "--rm",
    "--user",
    `${process.getuid()}:${process.getgid()}`,
    "-e",
    "HOME=/tmp",
    "-e",
    "CARGO_HOME=/tmp/cargo",
    "-e",
    "CARGO_TARGET_DIR=/tmp/target",
    "-e",
    "OUTPUT=/out",
    "-e",
    `REPO_REVISION=${getRepoRevision(__dirname)}`,
    "-e",
    `EMBEDDED_DATA=${embeddedData ? 1 : 0}`,
    "-e",
    `JS_FALLBACK=${jsFallback ? 1 : 0}`,
    "-v",
    `${__dirname}:/code`,
    "-v",
    `${output}:/out`,
    imageId,
  );
  const names = ["hellbox.js", "hellbox_bg.wasm"];
  if (jsFallback) {
    names.push("hellbox_nowasm_fallback.js");
  }
  for (const name of names) {
    const { size } = await fs.stat(join(output, name));
    console.log(`${join(output, name)}: ${size} bytes`);
  }
}

const USAGE = `Usage: node build.js [options]

  -C, --create         Only build the Docker image
  -c, --compile        Build the wasm module (hellbox.js and hellbox_bg.wasm)
                       and its JS version (hellbox_nowasm_fallback.js, for
                       when WebAssembly isn't available), after building the
                       image (from Docker's cache if it's up to date); the
                       data files are written beside them, to
                       provide at runtime (see \`set_data\` in rust/README.md)
  -o, --output <dir>   Output directory (default: build/wasm)
  --embedded-data      Embed the data files in the module instead (without
                       the JS version, which needs them outside)
  --no-js-fallback     Don't write the JS version
  -h, --help           Show this help`;

async function main() {
  const { values: args } = parseArgs({
    // --no-embedded-data and --js-fallback (the defaults) are accepted too.
    allowNegative: true,
    options: {
      create: { type: "boolean", short: "C" },
      compile: { type: "boolean", short: "c" },
      output: {
        type: "string",
        short: "o",
        default: join(__dirname, "build", "wasm"),
      },
      "embedded-data": { type: "boolean" },
      "js-fallback": { type: "boolean" },
      help: { type: "boolean", short: "h" },
    },
  });
  if (args.help || (!args.create && !args.compile)) {
    console.log(USAGE);
    return;
  }
  const embeddedData = args["embedded-data"] ?? false;
  const jsFallback = args["js-fallback"] ?? !embeddedData;
  if (embeddedData && jsFallback) {
    throw new Error("--js-fallback can't be used with --embedded-data.");
  }
  if (args.compile) {
    await compile(args.output, embeddedData, jsFallback);
  } else {
    await buildImage();
  }
}

main().catch(error => {
  console.error(error.message);
  process.exitCode = 1;
});
