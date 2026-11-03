# hellbox

A Rust library (compiled to wasm for pdf.js) which generates TrueType fonts
with exactly the requested glyphs, to replace the fonts which aren't embedded
in PDF files. Glyph data (see `../FORMAT.md`) is embedded by default,
or provided at runtime without the `embedded-data` feature:

- three families (see `Family`), from Arimo and Tinos, which have the metrics
  of Helvetica and Times, and Liberation Mono, which has the ones of Courier:
  regular and bold, and italic for the serif family,
- the symbols used as a fallback for all the fonts, from Noto fonts,
- the glyphs of Symbol, ZapfDingbats and Wingdings (Wingdings, Wingdings 2
  and Wingdings 3, at their Unicode codepoints, see `StandardFont`).

Each glyph is horizontally scaled to the width given by the PDF, its stems
getting the scale of the whole font (e.g. the stretched `i` of a font narrower
than the family doesn't look bolder than the other letters). The standard
fonts have their styles (the oblique ones are slanted); the other fonts get
the style of their family closest to the requested one, the rest being
synthesized (emboldening or thinning, and slanting the outlines).

With the `encoder` feature, it also has the encoder of the glyph data files
(see below).

## JS (wasm) API

```js
import { initSync, generate_font, missing_glyphs } from "./hellbox.js";

// Synchronously, as for qcms in pdf.js:
initSync({ module: fetchSync(`${wasmUrl}hellbox_bg.wasm`) });

const font = generate_font(
  codepoints, // Uint32Array: the codepoints of all the glyphs, one after the other
  lengths, // Uint32Array: the number of codepoints of each glyph
  widths, // Float64Array: the widths (1/1000 em), NaN for the natural width (or empty)
  charCodes, // Uint32Array: the code of each glyph in the cmap, 0xFFFFFFFF for its
  //            first codepoint (or empty)
  family, // 0: sans-serif, 1: serif, 2: monospace
  bold,
  italic,
  weight, // 100 to 900, it has precedence over `bold` (0: unknown)
  stemV, // the expected vertical stem, it has precedence over `weight` (0: unknown)
  italicAngle, // degrees, negative for a font leaning to the right (0: default)
  capHeight // the glyphs are vertically scaled to match it (0: unknown)
); // Uint8Array: a TrueType font, the glyph id of the i-th glyph being i + 1.

// The glyphs which can't be rendered (they're empty in the font):
const missing = missing_glyphs(codepoints, lengths, family); // Uint32Array

// A standard font: the same arguments, without the style.
const standard = generate_standard_font(
  codepoints,
  lengths,
  widths,
  charCodes,
  "Helvetica-Bold" // The name of one of the 14 standard fonts.
);
missing_standard_glyphs(codepoints, lengths, "Helvetica-Bold");
// All the codepoints of a standard font (e.g. for a font with all of them):
standard_font_codepoints("Helvetica"); // Uint32Array
```

The glyphs which aren't in a family are taken from the symbols (which are only
decoded then), and synthesized to the style of the font.

By default, `build.js` builds the module without the data files (see below):
the ones a font needs must then be provided first, they're kept by the module
(the functions throw an error, "Missing data files: ...", without them):

```js
// Load the style, fallbacks and Unicode tables.
for (const file of standard_font_data_files("Helvetica-Bold")) {
  if (!has_data(file)) {
    set_data(file, fetchSync(`${dataUrl}${file}`)); // Uint8Array
  }
}
// For a font of a family: the arguments of generate_font about the style.
font_data_files(family, bold, italic, weight, stemV, italicAngle);
```

Without WebAssembly (e.g. disabled in the browser), the JS version of the
module (also built by default, see below) uses the same glue and the same data
files:

```js
import { initJs } from "./hellbox.js";

const { default: instantiate } = await import("./hellbox_nowasm_fallback.js");
initJs(instantiate); // Instead of initSync: the other functions are the same.
```

Two optional last arguments of `generate_font` and `generate_standard_font`:

- `ascent`, the ascent as a fraction of the em: the descent is the rest,
  without line gap (the lines are an em high),
- `kerning`, a `Float64Array` of triples (index of the left glyph, index of the
  right one, adjustment of the advance of the left one in 1/1000 em): a `kern`
  table.

The text of a glyph is usually a single codepoint, but it can be a ligature
(`ffi` gives U+FB03) or a letter followed by combining marks (which are
composed if possible, else rendered as a sequence). A codepoint which isn't in
the data is replaced by its normalized forms when possible (e.g. 𝐀 by A, ㎡ by
m and 2).

Glyphs are decoded on demand and cached. Family files put 8-bit encoding
glyphs first. When more glyphs are needed, decoding restarts from the first
glyph and at least doubles the cached prefix. Beyond half of the glyphs,
the whole file is decoded.

## Rust API

```rust
use hellbox::{generate_font, generate_standard_font, Family, Options, Request, StandardFont};

let glyphs = [Request { unicode: vec!['A' as u32], width: Some(667.0), char_code: None }];
let options = Options { bold: true, italic: true, ..Options::default() };
let font = generate_font(Family::Serif, &glyphs, &options);
// font.data: the TrueType font, font.missing: the glyphs which can't be rendered.
let font = generate_standard_font(StandardFont::TimesBoldItalic, &glyphs, &Options::default());
```

Without the `embedded-data` feature (on by default), the data files needed by
a font (`standard_font_data_files` and `font_data_files`, among the ones of
`data_file_names`) must be given to `set_data` first: a function needing a
file which isn't there panics.

## Building the wasm module

From the root of the repository:

```sh
node build.js --compile --output my_output_dir
```

It builds a Docker image with the toolchain (Rust, the wasm target, wasm-pack,
binaryen and terser, see `Dockerfile`) if it doesn't exist yet, then generates
in the output directory (`build/wasm` by default):

- `hellbox.js` and `hellbox_bg.wasm`, the module without the data files,
- the data files (`data/*.hbgd` and `data/unicode.bin`), to provide at
  runtime with `set_data`,
- `hellbox_nowasm_fallback.js`, the JS version of the module, translated by
  binaryen's `wasm2js` and minified by terser, to load with `initJs` (see
  above). The `js-fallback` profile in `Cargo.toml` optimizes for size; the
  build checks that its glue matches the release build. The build disables
  reference types and multivalue, lowers instructions for the JS translation,
  and adapts the glue's memory views for JS memory growth.
  `node tools/check_js_fallback.mjs <dir>` compares selected fonts and
  missing-glyph indices from the two versions.

`--no-js-fallback` skips the JS version, and `--embedded-data` embeds the data
files in the module instead (without the JS version, which needs them
outside). After a change of the `Dockerfile` (e.g. to update the toolchain),
`--create` builds the image again: `node build.js -Cco my_output_dir` does
both.

With a local toolchain: `EMBEDDED_DATA=0 JS_FALLBACK=1 OUTPUT=<dir>
./compile.sh` (binaryen's tools from `$BINARYEN`, or `PATH`, and terser from
`$TERSER`, or `PATH`); without these variables, `compile.sh` embeds the data
files and doesn't write the JS version.

## Encoder

The `encoder` feature builds glyph data files from fonts read with
[read-fonts](https://crates.io/crates/read-fonts).

Contour reference searches use up to 8 threads by default (`--threads`).
Results are merged in scan order to preserve sequential tie-breaking.
Workers spin while waiting for the next search.

```sh
# The files of data/, from the fonts of ../sources/ (see
# ../tools/fetch_sources.sh):
cargo run --release --features encoder --bin build_all [-- --out <dir>]
# A file (--help gives the options):
cargo run --release --features encoder --bin build_glyph_data -- \
  --font Some-Regular.ttf --reference Some-Bold.ttf -o some.hbgd
```

```rust
use hellbox::encoder::{build_glyph_data, BuildOptions};

let options = BuildOptions {
    reference: Some("Some-Bold.ttf".into()),
    ..BuildOptions::new("Some-Regular.ttf")
};
let (data, stats) = build_glyph_data(&options)?;
```

The model (`src/model.rs`) runs in both directions: the decoder and the
encoder share it, the encoder adding the choice of how each contour is coded.
These choices depend on floating point costs, so their operations must stay
the same, else the files change. The last bit of `f64::log2` may differ
between platforms: it doesn't change the files of `data/`, but it can change
bigger ones (e.g. CJK) by a few bytes. The model rounds ties towards
+infinity, in its integer computations and its floating point ones. With the `deflate` codec, the files depend on flate2.

## Data and tests

`data/` is built by `build_all` (see above):

- the glyph data files and the licenses of their fonts (`data/LICENSE_ARIMO`,
  `LICENSE_TINOS`, `LICENSE_LIBERATION`, and `LICENSE_NOTO` with the notices of
  all the Noto fonts used),
- `data/unicode.bin`: the normalization forms of the codepoints which can be
  rendered, and the Latin ligatures (U+FB00-FB06, e.g. "ffi" gives U+FB03)
  (14 KB), generated with
  [unicode-normalization](https://crates.io/crates/unicode-normalization),
  whose version is pinned (another Unicode version changes the tables): its
  license, the one of the Unicode data, is `data/LICENSE_UNICODE`.

```sh
cargo test --release
# Runtime data loading (tests/data_files.rs).
cargo test --release --no-default-features
```

`tests/snapshots.rs` checks that the decoded glyph data and some generated
fonts are the same as their snapshots in `tests/snapshots/` (lengths and
hashes). With `HELLBOX_OUTPUT=<dir>`, the fonts are
written in `<dir>`. After checking the changes (e.g. with the reference
tests):

```sh
UPDATE_SNAPSHOTS=1 cargo test --release --test snapshots
```

`tests/font_builder.rs` checks the generated fonts, read with read-fonts: the
char codes and widths, the centering, the styles of the families and the
weights synthesized from them (linear stems), the slant, the ligatures and
sequences, the fallback to the symbols (with the style of the font), the
glyphs of the standard fonts (those of Symbol and ZapfDingbats on the boxes of
their AFM files, the pieces of the big delimiters joining)...

`tests/encoder.rs` (with the `encoder` feature) checks that encoding the
embedded files again gives the same bytes, that the model codec gives back the
payloads, and that the Unicode tables are the ones of `data/`. With
`-- --include-ignored`, it also builds the files from the fonts of
`../sources/` (they must be the ones of `data/`), with a grid and with the
three codecs:

```sh
cargo test --release --features encoder -- --include-ignored
```

`tests/reftest.rs` draws the glyphs of some generated fonts (each family and
style, the width fitting, weights, slants, cap heights, ligatures and
compositions, symbols, the standard fonts...) as read by [skrifa](https://docs.rs/skrifa) (the font
scaler of Google's Fontations), in SVG files which must be the same as the
references in `tests/refs/`: each glyph with its advance box, baseline, text,
codes in the cmap and advance. When some differ, the new drawings and their
references are written in `target/tmp/reftest/`, with an `index.html` to
compare them (hovering a drawing shows its reference). After reviewing the
differences:

```sh
UPDATE_REFS=1 cargo test --release --test reftest  # Updates tests/refs/.
```

`tests/ots.rs` checks that OTS (the font sanitizer of Firefox and Chrome, built
by Servo's [fontsan](https://crates.io/crates/fontsan), hence a C++ compiler is
needed) accepts the generated fonts, including extreme options, edge char
codes and 200 random cases. Tests compare sanitized tables, comparing cmap
mappings and ignoring head.checkSumAdjustment. Failing fonts are written in
`target/tmp/ots/` for inspection with `ots-sanitize`.

The font builder uses `sqrt` for lengths and rounds slant to 16.16, as `hypot`
and `tan` differ between platforms: the fonts must be the same everywhere. CI
checks font snapshots and reference drawings on Linux, macOS and Windows.

## Benchmarks

`benches/glyph_data.rs` measures the decoding of the glyph data files (once
per style in a document), and `benches/fonts.rs` the generation of the fonts
from decoded data: a text and the Latin-1 glyphs in each style (embedded or
synthesized), all the glyphs of the families and of some standard fonts, the
fallbacks (symbols, sequences, normalized forms), and the lookup of the missing
glyphs. They use [divan](https://docs.rs/divan), through the compatibility
layer of [CodSpeed](https://codspeed.io), which measures them (simulated CPU,
deterministic) to catch the regressions:

```sh
cargo bench  # The times of the native code (the wasm module is slower).
cargo bench --bench fonts -- standard_font  # The benchmarks matching a filter.

# With CodSpeed (cargo install cargo-codspeed, codspeed auth login):
cargo codspeed build -m simulation
codspeed run -m simulation -- cargo codspeed run
```

## Continuous integration

The workflows of `../.github/workflows/` run on the pushes to `main` and the
pull requests:

- `ci.yml`: rustfmt, clippy (with and without the embedded data, with the
  encoder, and for wasm32) and the docs, the warnings being errors; the tests
  on Linux, macOS and Windows (the fonts must be the same everywhere), also
  with the data provided at runtime; the files of `data/` built from the fonts
  of `../sources/` (cached); the wasm module, with and without the data, and
  with its JS version (built with `build.js`, and checked against the wasm
  module): their sizes are in the summary of the run, and they're artifacts.
- `codspeed.yml`: the benchmarks, measured by CodSpeed (the repository must
  be enabled on [codspeed.io](https://codspeed.io), which authenticates the
  runs with OIDC).
- `coverage.yml`: the coverage of all the tests with
  [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) (its summary is
  in the summary of the run, the lcov file is an artifact), uploaded to
  [Codecov](https://codecov.io) by a job of the `code-coverage` environment,
  which has the `CODECOV_TOKEN` secret (not for the pull requests from forks).

Dependabot (`../.github/dependabot.yml`) updates the actions of the workflows
every week; the Cargo dependencies are updated by hand (some pins are
deliberate, and an update can change the generated files or the wasm module).

## License

The code is under the Mozilla Public License 2.0. The glyph data is derived
from Arimo, Tinos, Liberation Mono and the Noto fonts, under the SIL Open Font
License 1.1, and `data/unicode.bin` from the Unicode Character Database, under
the Unicode License v3: the licenses of `data/` (`LICENSE_ARIMO`,
`LICENSE_TINOS`, `LICENSE_LIBERATION`, `LICENSE_NOTO` and `LICENSE_UNICODE`)
must be distributed with the library (the copyright notices are in the
generated fonts, which don't use the Reserved Font Names of Liberation Mono).
