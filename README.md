# Hellbox

[![CI](https://github.com/mozilla/hellbox/actions/workflows/ci.yml/badge.svg)](https://github.com/mozilla/hellbox/actions/workflows/ci.yml)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://codspeed.io/mozilla/hellbox)
[![codecov](https://codecov.io/gh/mozilla/hellbox/graph/badge.svg)](https://codecov.io/gh/mozilla/hellbox)

Glyph data and a font generator for the fonts which aren't embedded in PDF
files, made for pdf.js.

In letterpress printing, the hellbox is where the worn and broken type was
thrown, to be melted and cast again. Hellbox keeps fonts melted down into
compact glyph data, and casts on demand the fonts a PDF needs, with exactly
their glyphs.

When a PDF doesn't embed a font, pdf.js has to rely on whatever the system
provides, with the wrong widths, weights and shapes. Shipping full Unicode
fonts would be far too big, so this repository provides:

- compact glyph data files (`rust/data/*.hbgd`) containing only the outlines
  and advance widths of the glyphs, plus a few metrics (no hinting, glyph names
  or layout tables). See [FORMAT.md](FORMAT.md).
- a Rust library, compiled to wasm for pdf.js, which embeds the data and
  synthesizes a TrueType font containing only the requested glyphs, for the
  standard fonts of PDF and for any other font of the three families
  (sans-serif, serif, monospace):
  - the glyphs come from the style of the family closest to the requested one
    (regular or bold, and italic for the serif family),
  - each glyph is horizontally scaled to fit the width given by the PDF,
  - the other weights are obtained by emboldening (or thinning) the outlines
    (FreeType's algorithm), the stems being linear in the weight between the
    regular and bold styles, and the emboldening being reduced for the dense
    glyphs (e.g. complex ideographs) like real bold fonts do,
  - the italic of the sans-serif and monospace families is obtained with a
    shear (as the oblique styles of Helvetica and Courier),
  - the glyphs can be vertically scaled to match the PDF `CapHeight`.

  (The glyphs of the standard fonts are only fitted to their widths, and
  slanted for the oblique styles of Courier and Helvetica.)
- the encoder of the data files (the `encoder` feature of the library), which
  builds them from TrueType fonts.

See [rust/](rust/README.md) for the API, the wasm module and the tests. This is
the version 0.0: the format and the API may change.

## Data files

The families come from fonts which have the metrics of the standard fonts of
PDF: [Arimo](https://github.com/googlefonts/arimo) and
[Tinos](https://github.com/googlefonts/tinos) (by Google, the Liberation fonts
2 come from them) for Helvetica (Arial) and Times (New Roman), and
[Liberation Mono](https://github.com/liberationfonts/liberation-fonts) 2.1.5
for Courier (New): 96-98% of the widths of the AFM files of Helvetica and Times
are the same, and all the ones of Courier. The symbols come from some unhinted
[Noto](https://notofonts.github.io/) fonts.

| File                    | Size    | Codepoints | Content                                                  |
| ----------------------- | ------- | ---------- | -------------------------------------------------------- |
| `Sans-Regular.hbgd`     | 76.9 KB | 3010       | Arimo: Latin, Greek, Cyrillic, Hebrew, IPA, punctuation, currencies... (also Helvetica and Helvetica-Oblique) |
| `Sans-Bold.hbgd`        | 77.2 KB | 3010       | Arimo Bold (also Helvetica-Bold and Helvetica-BoldOblique) |
| `Serif-Regular.hbgd`    | 76.9 KB | 3011       | Tinos, the same coverage (also Times-Roman)               |
| `Serif-Bold.hbgd`       | 77.5 KB | 3011       | Tinos Bold (also Times-Bold)                              |
| `Serif-Italic.hbgd`     | 87.3 KB | 3011       | Tinos Italic (also Times-Italic)                          |
| `Serif-BoldItalic.hbgd` | 87.4 KB | 3011       | Tinos Bold Italic (also Times-BoldItalic)                 |
| `Mono-Regular.hbgd`     | 49.5 KB | 2305       | Liberation Mono (also Courier and Courier-Oblique)        |
| `Mono-Bold.hbgd`        | 49.8 KB | 2305       | Liberation Mono Bold (also Courier-Bold and Courier-BoldOblique) |
| `NotoSymbols.hbgd`      | 71.0 KB | 1757       | Arrows, math operators, technical symbols, geometric shapes, dingbats... (from Noto Sans Math, Symbols, Symbols 2 and Mono), only what's missing in the 3 families |
| `Symbol.hbgd`           | 8.5 KB  | 186        | The glyphs of Symbol, as pdf.js maps them to Unicode: from Tinos (Greek, ...), Noto Sans Math and Noto Sans Symbols |
| `ZapfDingbats.hbgd`     | 17.0 KB | 202        | The glyphs of ZapfDingbats, from Noto Sans Symbols 2 and Noto Sans Symbols |
| `Wingdings.hbgd`        | 35.6 KB | 646        | The glyphs of Wingdings, Wingdings 2 and Wingdings 3 at their Unicode codepoints (pdf.js maps their codes), from Noto Sans Symbols 2, Noto Sans Symbols, Noto Sans Math and Tinos |

They're compressed with a dedicated codec (see [Compression](#compression)):
715 KB in total. `NotoSymbols.hbgd` is used as a fallback for all the fonts.

The data is lossless (the decoded outlines are checked against the source
fonts when the files are built).

## The generated fonts

The style of a font is the closest one of its family: bold from a weight of
550, italic for the serif family (when the font leans to the right: the font
has the angle of Tinos Italic, -16.33°, whatever the requested one). The rest
is synthesized: the stems are linear in the weight (from the stems of the
regular and bold styles, measured on `l`, `I` and `H`), e.g. a weight of 900
is the bold style emboldened by two thirds of the difference between the
styles, and the sans-serif and monospace italics are slanted. The glyphs of
the symbols get the whole style (from the regular weight, upright), and the
cap height of the family, also in a sequence with other glyphs (e.g. "A★").

A glyph is found from its text: the codepoint itself, its NFC/NFKC forms, a
Latin ligature (`ff`, `fi`, `fl`, `ffi`, `ffl`, `st`), or else its NFD/NFKD
decomposition rendered as a sequence (e.g. a letter followed by combining
marks, centered on it). The glyphs of the family come first, with the
canonical forms (NFC/NFD), then the exact codepoints of the symbols, then the
compatibility forms (e.g. "1" for "①" only when no symbol has it).

The fitting is done as follows: the outline is scaled horizontally by `s` and
emboldened horizontally by `stem × (font scale − s)`, so that `s × advance +
compensation + bold strength = width`. The stems get the scale of the font
(the median of the scales of its glyphs: e.g. a condensed font has thinner
stems), whatever the one of the glyph, which only changes its counters and its
side bearings: else the glyphs stretched more than the other ones (e.g. the
`i`, `l` and `J` of a font narrower than the family) look bolder. The scale is
limited to `[min_scale, max_scale]` (`[0.5, 1.25]` by default), the
compensation to a thinning of a quarter of the horizontal stem and to a
thickening of 0.4 vertical stem, and the glyph is centered when it can't fit
exactly. Then it's emboldened, vertically rescaled to compensate the height
added by the emboldening, and slanted. Unreasonable values (e.g. garbage in a
PDF) are bounded: the advance to 16383 units, the italic angle to ±45°, and the
coordinates to ±16383 (so that the int16 deltas of the glyf table can't
overflow).

The emboldening strength is multiplied by `clamp(1 - 0.04 (D - 4), 0.35, 1)`
where `D` is the length of the contours divided by `sqrt(advance * em)`:
fitted on Noto Sans CJK, where the effective strength of the real bold goes
from ~48 units for 一 to ~18 for 鬱 (D ≈ 18); D is below 6 for almost all the
Latin, Greek and Cyrillic glyphs, so they're barely affected.

The tests check that OTS (the font sanitizer of Firefox and Chrome) accepts
the generated fonts without any error or warning, and without fixing anything
(it must keep the tables as they are, except the cmap which it always
rebuilds).

## Building

```sh
sh tools/fetch_sources.sh  # Download the fonts (pinned commits, sha256 checked).
cd rust
# Build data/ (the glyph data files, their licenses and the Unicode tables).
cargo run --release --features encoder --bin build_all
cargo test --release --features encoder -- --include-ignored
```

`build_glyph_data` can be used on any glyf-based TrueType font:

```sh
cargo run --release --features encoder --bin build_glyph_data -- \
  --font Some-Regular.ttf --reference Some-Bold.ttf \
  --ranges 0-24F,370-3FF --fallback Other.ttf:2190-21FF -o some.hbgd
```

`--grid N` rounds the coordinates to multiples of N (lossy, the files being a
bit smaller). `--stems cjk` measures the stems on 丨 and 一 instead of `l`, `I`
and `H` (for a CJK file). `--codec deflate` is much faster to encode than the
default model codec (which can take minutes for big files), and
`--candidates N` (32768 by default) trades encoding time for size.

To visually check the generated fonts, the reference tests draw their glyphs
in SVG files (`rust/tests/refs/`, see [rust/](rust/README.md)).

## Compression

The payload (WOFF2-like triplets for the points, see [FORMAT.md](FORMAT.md))
is compressed with a dedicated codec: an adaptive binary range coder driven by
a context model of the glyph data, `rust/src/model.rs` being shared by the
encoder and the decoder. The encoder can be slow (it searches for the best
references), the decoder just follows its decisions
(`rust/src/range_decoder.rs` and `rust/src/model.rs`).

| Files of `data/`              | Payload | deflate | brotli 11 | xz -9e | model  |
| ----------------------------- | ------- | ------- | --------- | ------ | ------ |
| Sans (2 styles)               | 303 kB  | 192 kB  | 178 kB    | 178 kB | 154 kB |
| Serif (4 styles)              | 709 kB  | 410 kB  | 374 kB    | 373 kB | 329 kB |
| Mono (2 styles)               | 199 kB  | 119 kB  | 113 kB    | 112 kB | 99 kB  |
| Symbols, Symbol, ZapfDingbats | 203 kB  | 125 kB  | 119 kB    | 118 kB | 96 kB  |
| Wingdings                     | 80 kB   | 48 kB   | 45 kB     | 45 kB  | 36 kB  |
| All                           | 1.49 MB | 894 kB  | 828 kB    | 826 kB | 715 kB |

(kB = 1000 bytes.) The same glyphs as fonts (the same codepoints from the same
source fonts, subset with fontTools: unhinted, without glyph names and layout
tables, i.e. only the cmap, glyf, loca, hmtx, hhea, head, maxp, OS/2, name and
post tables):

| Files of `data/`              | TrueType | WOFF    | WOFF2  | model  |
| ----------------------------- | -------- | ------- | ------ | ------ |
| Sans (2 styles)               | 463 kB   | 280 kB  | 198 kB | 154 kB |
| Serif (4 styles)              | 1.03 MB  | 602 kB  | 414 kB | 329 kB |
| Mono (2 styles)               | 313 kB   | 179 kB  | 126 kB | 99 kB  |
| Symbols, Symbol, ZapfDingbats | 281 kB   | 155 kB  | 126 kB | 96 kB  |
| Wingdings                     | 113 kB   | 59 kB   | 50 kB  | 36 kB  |
| All                           | 2.20 MB  | 1.27 MB | 914 kB | 715 kB |

WOFF is compressed with zopfli (100 iterations, 3% smaller than zlib level
9), WOFF2 with brotli 11 and its glyf, loca and hmtx transforms: the files of
`data/` are 22% smaller than WOFF2 (the source fonts, hinted, with their layout
tables and all their glyphs, take 5.7 MB). CFF (`.otf`) isn't smaller: the
same subset of Noto Sans and Noto Serif (version 2.015, which has both) is
23-32% smaller in CFF than in TrueType, but about the same size in WOFF (from
-0.1% to +2%) and 21-23% bigger in WOFF2 (whose transforms don't apply to
CFF), and its outlines are cubic.

What the model exploits, by order of importance:

- **Contours are reused**: all the coded contours are kept in a dictionary,
  and a contour can be a reference to a previous one (with the same number of
  points and on/off-curve pattern), translated, transformed (per-axis scale,
  shear) or blended with a second reference (e.g. between a short and a long
  version of a stroke), with corrections: per delta, or IUP-like (only a few
  touched points, the other ones being interpolated, then corrected). The
  encoder looks for the best reference among thousands of candidates,
  computing the exact cost of the best ones by running the model without
  coding, then refines its parameters (scale, shear or blend) by their exact
  cost; the touched points are the cheapest ones with the current
  probabilities.
- **Components are predicted**: after a contour, the contour which followed it
  the last time, or the time before (and, for a glyph's first contour, the
  first contour of the previous glyph) is predicted, at the same relative
  position and scale: a component made of several contours (e.g. a radical)
  costs a few bits per contour.
- **Repeated parts of the outlines** (serifs, stem ends, bowls...) are
  predicted with the previous occurrence of the same context, i.e. the last
  points of the contour (LZP): one decision per point when the prediction is
  right. It's what makes the model beat xz on Latin, where most contours are
  new.
- **Symmetric parts of the outlines** (the halves of an o, b and d, p and q,
  arrows...): when LZP has no prediction or a wrong one, the last two deltas
  are looked up rotated by 180° or mirrored, in the contour and in the
  history, which predicts the next point.
- **The geometry**: in curves the control polygon continues (the delta is
  predicted to be the previous one), the contexts are made of the on/off-curve
  state of the last points and of the zero/sign/magnitude of the previous
  deltas, the advance is predicted from the bounding box...
- **Adaptive probabilities** with two speeds, adapting faster for the first
  decisions of a context.

The format also supports deflate and no compression at all: the encoder
builds such files (`--codec`), but the library only reads the model codec (and
the uncompressed files with the `encoder` feature).

Other decisions:

- composite glyphs are kept (about a third of the glyphs, mostly accented
  letters, are just references to other glyphs),
- the glyphs are sorted by codepoint, those of the characters of the 8-bit
  encodings first (pdf.js requests them for most fonts, and the decoder stops
  after them), so the cmap is just a few runs,
- only the glyphs reachable from the cmap are kept, identical glyphs are
  merged.

## License

The code is under the Mozilla Public License 2.0 (see [LICENSE](LICENSE)).

All the source fonts are under the SIL Open Font License 1.1 (OFL). Arimo,
Tinos and the Noto fonts have no Reserved Font Name, so derived data can be
distributed under any name. The `.hbgd` files (and the drawings of their
glyphs in `rust/tests/refs/`) are "Modified Versions" of the fonts in the OFL
terms: they can be bundled with software, but must not be sold by themselves
and must keep the copyright notices and the license. Hence the licenses of
`rust/data/` (`LICENSE_ARIMO`, `LICENSE_TINOS`, `LICENSE_LIBERATION` and
`LICENSE_NOTO`, which has the notices of all the Noto fonts used) must be
shipped along with them, and the copyright notices of all the source fonts are
stored in the data and copied into the `name` table of the generated fonts.

`rust/data/unicode.bin` is derived from the Unicode Character Database (by
[unicode-normalization](https://crates.io/crates/unicode-normalization)),
under the Unicode License v3 (`rust/data/LICENSE_UNICODE`), which must be
shipped along with it too. The wasm module also contains the Rust standard
library and wasm-bindgen, under the MIT or the Apache License 2.0.

Liberation Mono has the Reserved Font Names "Liberation", "Arimo", "Tinos" and
"Cousine": a Modified Version can't use them as the name of the font presented
to the users, hence the files and the generated fonts don't (their family is
"Hellbox Mono", ...).

## Use in pdf.js

For a font without file, pdf.js loads the wasm module (as qcms or OpenJPEG),
and generates a font with the code points it draws for the char codes of the
font (with their widths):

- for a standard font (or a font mapped to one, e.g. Arial), with its style:
  the generated font replaces the font when the system fonts can't be used,
  and always for Symbol, ZapfDingbats and Wingdings (it's then also drawn as
  paths, e.g.
  without the font loading API); an XFA form gets all the glyphs of Helvetica,
  with the widths of its font,
- else from the family selected from the `FontDescriptor` flags (`FixedPitch`
  => mono, `Serif` => serif, else sans), with the style of the descriptor and
  of the font name: it's used when the system fonts substituting the font
  aren't available.

## Limitations

- CJK isn't in the data: it would be several MB, so it'd have to be split in
  chunks (e.g. by frequency) loaded on demand.
- Scripts which need shaping (Arabic, Indic, ...) can't work without the
  embedded font anyway, since the PDF char codes are glyphs.
- The synthetic italic of the sans-serif and monospace families is an oblique
  (as the ones of Helvetica and Courier).
