/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Builds the wasm module with Docker: the toolchain is in an image (see
// rust/Dockerfile), which runs rust/compile.sh.

"use strict";

const fs = require("fs");
const { spawn } = require("child_process");
const { join, resolve } = require("path");
const { parseArgs } = require("util");

const IMAGE = "hellbox-builder";
const CRATE = join(__dirname, "rust");

function exec(command, args, stdio = "inherit") {
  const child = spawn(command, args, { stdio });
  return new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("close", code => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`${command} exited with code ${code}`));
    });
  });
}

function create() {
  return exec("docker", ["build", "-t", IMAGE, CRATE]);
}

async function hasImage() {
  try {
    await exec("docker", ["image", "inspect", IMAGE], "ignore");
    return true;
  } catch {
    return false;
  }
}

async function compile(output) {
  output = resolve(output);
  await fs.promises.mkdir(output, { recursive: true });
  if (!(await hasImage())) {
    await create();
  }
  // The crate is mounted in /code and the output in /out: the build files and
  // the Cargo downloads go under /tmp in the disposable container.
  await exec("docker", [
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
    "-v",
    `${CRATE}:/code`,
    "-v",
    `${output}:/out`,
    IMAGE,
  ]);
  for (const name of ["hellbox.js", "hellbox_bg.wasm"]) {
    const { size } = await fs.promises.stat(join(output, name));
    console.log(`${join(output, name)}: ${size} bytes`);
  }
}

const USAGE = `Usage: node build.js [options]

  -C, --create         Create the Docker image (again, e.g. after a change of
                       rust/Dockerfile)
  -c, --compile        Build the wasm module (hellbox.js and
                       hellbox_bg.wasm), creating the image if needed
  -o, --output <dir>   Output directory (default: build/wasm)
  -h, --help           Show this help`;

async function main() {
  const { values: args } = parseArgs({
    options: {
      create: { type: "boolean", short: "C" },
      compile: { type: "boolean", short: "c" },
      output: {
        type: "string",
        short: "o",
        default: join(__dirname, "build", "wasm"),
      },
      help: { type: "boolean", short: "h" },
    },
  });
  if (args.help || (!args.create && !args.compile)) {
    console.log(USAGE);
    return;
  }
  if (args.create) {
    await create();
  }
  if (args.compile) {
    await compile(args.output);
  }
}

main().catch(error => {
  console.error(error.message);
  process.exitCode = 1;
});
