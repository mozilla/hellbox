#!/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Builds the wasm module in the Docker image (see ../build.js). Writes
# $OUTPUT/$OUTPUT_NAME.js and $OUTPUT/${OUTPUT_NAME}_bg.wasm; defaults below.
# EMBEDDED_DATA=0 copies data files to $OUTPUT for runtime loading.
#
# JS_FALLBACK=1 also writes ${OUTPUT_NAME}_nowasm_fallback.js using wasm2js.
# It uses the same glue (via initJs) and data files. Binaryen tools come from
# $BINARYEN, or PATH when unset.
set -e

OUTPUT=${OUTPUT:=.}
OUTPUT_NAME=${OUTPUT_NAME:=hellbox}
EMBEDDED_DATA=${EMBEDDED_DATA:=1}
JS_FALLBACK=${JS_FALLBACK:=0}

set --
if [ "${EMBEDDED_DATA}" = 0 ]; then
  set -- -- --no-default-features
fi

if [ "${JS_FALLBACK}" = 1 ]; then
  if [ "${EMBEDDED_DATA}" != 0 ]; then
    echo "The JS version needs the data files outside the module (EMBEDDED_DATA=0)." >&2
    exit 1
  fi
  # Disable reference types and multivalue for wasm2js, including in the
  # linker's feature list passed to wasm-bindgen.
  RUSTFLAGS="${RUSTFLAGS} -C target-feature=-reference-types,-multivalue \
    -C link-arg=--no-check-features \
    -C link-arg=--features=mutable-globals,sign-ext,nontrapping-fptoint,bulk-memory"
  export RUSTFLAGS
fi

wasm-pack build \
          --target web \
          --release \
          --no-typescript \
          --no-pack \
          --out-name "${OUTPUT_NAME}" \
          --out-dir "${OUTPUT}" \
          "$@"

if [ "${EMBEDDED_DATA}" = 0 ]; then
  cp data/*.hbgd data/unicode.bin "${OUTPUT}/"
fi

if [ "${JS_FALLBACK}" = 1 ]; then
  GLUE="${OUTPUT}/${OUTPUT_NAME}.js"
  TMP=$(mktemp -d)
  # Check buffer identity for both wasm and JS memory growth.
  sed -i -E \
    -e 's/(cached(Float64Array|Uint32Array|Uint8Array)Memory0)\.byteLength === 0/\1.buffer !== wasm.memory.buffer/' \
    -e 's/cachedDataViewMemory0\.buffer\.detached === true \|\| \(cachedDataViewMemory0\.buffer\.detached === undefined && (cachedDataViewMemory0\.buffer !== wasm\.memory\.buffer)\)/\1/' \
    "${GLUE}"
  VIEWS=$(grep -c '^let cached[A-Za-z0-9]*Memory0 = null;' "${GLUE}")
  RENEWED=$(grep -c 'Memory0\.buffer !== wasm\.memory\.buffer' "${GLUE}")
  if [ "${VIEWS}" != "${RENEWED}" ]; then
    echo "Unexpected glue: ${RENEWED} of its ${VIEWS} memory views renewed." >&2
    exit 1
  fi
  cat >> "${GLUE}" <<'EOF'

/**
 * Initializes the JS fallback using its default export as `instantiate`.
 */
function initJs(instantiate) {
    if (wasm !== undefined) return wasm;
    return __wbg_finalize_init({ exports: instantiate(__wbg_get_imports()) }, undefined);
}
export { initJs };
EOF
  # Lower instructions for wasm2js without changing the shipped wasm module.
  WASM_OPT="${BINARYEN:+${BINARYEN}/}wasm-opt"
  WASM2JS="${BINARYEN:+${BINARYEN}/}wasm2js"
  "${WASM_OPT}" "${OUTPUT}/${OUTPUT_NAME}_bg.wasm" \
           --enable-mutable-globals --enable-sign-ext \
           --enable-nontrapping-float-to-int --enable-bulk-memory \
           --llvm-nontrapping-fptoint-lowering --llvm-memory-copy-fill-lowering \
           --signext-lowering -o "${TMP}/lowered.wasm"
  "${WASM2JS}" "${TMP}/lowered.wasm" -Oz -o "${TMP}/fallback.js"
  # Export an instantiator that accepts the glue's imports.
  {
    echo "/* THIS FILE IS GENERATED - DO NOT EDIT */"
    sed -e "/^import \* as .* from '\.\/${OUTPUT_NAME}_bg\.js';\$/d" \
        -e '/^var retasmFunc = asmFunc(/,$d' "${TMP}/fallback.js"
    echo "export default function instantiate(imports) {"
    echo "  return asmFunc(imports);"
    echo "}"
  } > "${OUTPUT}/${OUTPUT_NAME}_nowasm_fallback.js"
  rm -rf "${TMP}"
fi

sed -i "1 i\/* THIS FILE IS GENERATED - DO NOT EDIT */" "${OUTPUT}/${OUTPUT_NAME}.js"
# wasm-pack ignores everything in its output directory, which can be the one
# of the module in pdf.js.
rm -f "${OUTPUT}/.gitignore"
