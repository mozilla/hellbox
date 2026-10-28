#!/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Builds the wasm module in the Docker image (see ../build.js). Writes
# $OUTPUT/$OUTPUT_NAME.js and $OUTPUT/${OUTPUT_NAME}_bg.wasm; defaults below.
set -e

OUTPUT=${OUTPUT:=.}
OUTPUT_NAME=${OUTPUT_NAME:=hellbox}

wasm-pack build \
          --target web \
          --release \
          --no-typescript \
          --no-pack \
          --out-name "${OUTPUT_NAME}" \
          --out-dir "${OUTPUT}"

sed -i "1 i\/* THIS FILE IS GENERATED - DO NOT EDIT */" "${OUTPUT}/${OUTPUT_NAME}.js"
# wasm-pack ignores everything in its output directory, which can be the one
# of the module in pdf.js.
rm -f "${OUTPUT}/.gitignore"
