/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Compares fonts and missing-glyph indices from wasm and JS. Build with:
// node build.js -co <dir> --no-embedded-data --js-fallback
//
// Usage: node tools/check_js_fallback.mjs <dir>

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const dir = resolve(process.argv[2] ?? "build/wasm");
const url = name => pathToFileURL(join(dir, name)).href;

// Two instances of the glue: one for each version.
const wasm = await import(`${url("hellbox.js")}?wasm`);
wasm.initSync({ module: readFileSync(join(dir, "hellbox_bg.wasm")) });
const js = await import(`${url("hellbox.js")}?js`);
js.initJs((await import(url("hellbox_nowasm_fallback.js"))).default);

const provide = (glue, files) => {
  for (const file of files) {
    if (!glue.has_data(file)) {
      glue.set_data(file, readFileSync(join(dir, file)));
    }
  }
};
const ones = n => new Uint32Array(n).fill(1);
const none = new Uint32Array(0);
const check = (label, run) => {
  const [a, b] = [run(wasm), run(js)];
  assert.ok(a.length > 0, `${label}: empty result`);
  assert.deepEqual(b, a, label);
  console.log(`ok: ${label} (${b.length})`);
};

// All primary-source codepoints of selected standard fonts, with custom widths.
for (const name of ["Helvetica", "Times-BoldItalic", "Courier", "Symbol", "ZapfDingbats"]) {
  provide(wasm, wasm.standard_font_data_files(name));
  provide(js, js.standard_font_data_files(name));
  const codepoints = wasm.standard_font_codepoints(name);
  const widths = Float64Array.from(codepoints, (_, i) => 300 + (i % 9) * 60);
  check(name, glue =>
    glue.generate_standard_font(codepoints, ones(codepoints.length), widths, none, name)
  );
}
// A synthesized style, with fallbacks and a missing glyph.
const text = Uint32Array.from("Hello, ffi é ∑ ★ 一", c => c.codePointAt(0));
const style = [1, false, false, 900, NaN, -12];
provide(wasm, wasm.font_data_files(...style));
provide(js, js.font_data_files(...style));
check("serif, weight 900, slanted", glue =>
  glue.generate_font(text, ones(text.length), new Float64Array(0), none, ...style, NaN)
);
check("missing glyphs", glue => glue.missing_glyphs(text, ones(text.length), 1));
