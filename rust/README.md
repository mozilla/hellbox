# hellbox

A Rust library (compiled to wasm for pdf.js) which generates TrueType fonts
with exactly the requested glyphs, to replace the fonts which aren't embedded
in PDF files. The glyph data (see `../FORMAT.md`) is embedded in the library
(729 KB in total):

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

Four optional last arguments of `generate_font` (and of
`generate_standard_font`), e.g. for a text layer:

- `metricsOnly` (`false` by default) generates glyphs without outlines: the
  smallest font with the advances (less than 1 KB for a text), the glyphs
  having the same advance sharing a glyph id (the cmap gives them),
- `ascent`, the ascent as a fraction of the em: the descent is the rest,
  without line gap (the lines are an em high),
- `kerning`, a `Float64Array` of triples (index of the left glyph, index of the
  right one, adjustment of the advance of the left one in 1/1000 em): a `kern`
  table,
- `debugBoxes` (`false` by default), for debugging: each glyph with an advance
  is the outline (1/32 em thick, inside) of a rectangle covering it, from the
  descender to the ascender (e.g. to see the glyphs of a `metricsOnly` font).

The text of a glyph is usually a single codepoint, but it can be a ligature
(`ffi` gives U+FB03) or a letter followed by combining marks (which are
composed if possible, else rendered as a sequence). A codepoint which isn't in
the data is replaced by its normalized forms when possible (e.g. 𝐀 by A, ㎡ by
m and 2).

The first font of a style decodes its glyphs, from the first one up to the
last one it needs: the glyphs of the characters of the 8-bit encodings of PDF
come first in the files of the families, so that a font with them takes about
6 ms with Node.js 24, a font with all the glyphs about 25 ms (more for the
first call, which also decodes the Unicode tables, and when the symbols are
needed). A font needing glyphs which aren't decoded yet decodes again from the
first glyph (the state of the decoder isn't kept): up to the last needed one,
or all of them beyond half of the glyphs. Then a font with ~50 glyphs takes
less than 1 ms (about 1 ms for the 244 glyphs of the 8-bit encodings, 6 ms for
the 3010 glyphs of Arimo).

## Rust API

```rust
use hellbox::{generate_font, generate_standard_font, Family, Options, Request, StandardFont};

let glyphs = [Request { unicode: vec!['A' as u32], width: Some(667.0), char_code: None }];
let options = Options { bold: true, italic: true, ..Options::default() };
let font = generate_font(Family::Serif, &glyphs, &options);
// font.data: the TrueType font, font.missing: the glyphs which can't be rendered.
let font = generate_standard_font(StandardFont::TimesBoldItalic, &glyphs, &Options::default());
```

## Building the wasm module

From the root of the repository:

```sh
node build.js --compile --output my_output_dir
```

It builds a Docker image with the toolchain (Rust, the wasm target and
wasm-pack, see `Dockerfile`) if it doesn't exist yet, then generates
`hellbox.js` and `hellbox_bg.wasm` (884 KB) in the output directory
(`build/wasm` by default). After a change of the `Dockerfile` (e.g. to update
the toolchain), `--create` builds the image again: `node build.js -Cco
my_output_dir` does both. With a local toolchain: `OUTPUT=<dir> ./compile.sh`.

## Encoder

With the `encoder` feature, the crate also builds the glyph data files, the
fonts being read with [read-fonts](https://crates.io/crates/read-fonts): all
the files of `data/` are built in 1.1 s (in parallel).

The search of the references of each contour is split between threads
(`--threads`, 8 at most by default), the result being exactly the one of the
sequential search: it's for the big searches (e.g. thousands of ideographs),
which are several times faster. The threads spin between the searches
(they're short, and waking threads up costs more than them).

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
needed) accepts the generated fonts (all the glyphs, extreme options, edge char
codes, 200 random fonts...) without fixing anything: the sanitized font must be
the same, except the cmap which OTS always rebuilds (with the same mapping).
fontsan doesn't give the messages of OTS, but its warnings come with a fix
(except a few ones about the layout of the cmap). The fonts which fail are
written in `target/tmp/ots/`: `ots-sanitize` (`pip install opentype-sanitizer`)
prints the messages of OTS.

The arithmetic of the font builder is deterministic (no `hypot`, the slant
rounded to 16.16 since `tan` isn't the same everywhere), so that the fonts are
the same everywhere: changing it changes the fonts (see the snapshots).

## Benchmarks

`benches/glyph_data.rs` measures the decoding of the glyph data files (once
per style in a document), and `benches/fonts.rs` the generation of the fonts
from decoded data: a text and the Latin-1 glyphs in each style (embedded or
synthesized), all the glyphs of the families and of some standard fonts, the
fallbacks (symbols, sequences, normalized forms), the fonts of a text layer
(with kerning, debug boxes, or a CJK text), and the lookup of the missing
glyphs. They use [divan](https://docs.rs/divan), through the compatibility
layer of [CodSpeed](https://codspeed.io), which measures them (simulated CPU,
deterministic) to catch the regressions:

```sh
cargo bench  # The times of the native code (the wasm module is slower).
cargo bench --bench fonts -- text_layer  # The benchmarks matching a filter.

# With CodSpeed (cargo install cargo-codspeed, codspeed auth login):
cargo codspeed build -m simulation
codspeed run -m simulation -- cargo codspeed run
```

## Continuous integration

The workflows of `../.github/workflows/` run on the pushes to `main` and the
pull requests:

- `ci.yml`: rustfmt, clippy (without features, with the encoder, and for
  wasm32) and the docs, the warnings being errors; the tests on Linux, macOS
  and Windows (the fonts must be the same everywhere); the files of `data/`
  built from the fonts of `../sources/` (cached); the wasm module (its sizes
  are in the summary of the run, and it's an artifact).
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
