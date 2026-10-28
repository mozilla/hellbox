// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Font generation and missing-glyph lookup. Warm-up calls initialize the
//! embedded data before timing.

use divan::{black_box, Bencher};
use hellbox::{
    generate_font, generate_standard_font, glyph_data, missing_glyphs, standard_glyph_data, Family,
    Kerning, Options, Request, StandardFont,
};

fn main() {
    divan::main();
}

/// Sample text for font-generation benchmarks.
const TEXT: &str = "Trace-based Just-in-Time Type Specialization for Dynamic Languages. \
                    Dynamic languages such as JavaScript are more difficult to compile than \
                    statically typed ones (2009): “quotes”, café, naïve, über, ﬁ.";

const FAMILIES: [Family; 3] = [Family::Sans, Family::Serif, Family::Mono];

/// The distinct characters of a text.
fn chars(text: &str) -> Vec<Vec<u32>> {
    let mut chars = Vec::new();
    for c in text.chars() {
        let c = vec![c as u32];
        if !chars.contains(&c) {
            chars.push(c);
        }
    }
    chars
}

fn latin_1() -> Vec<Vec<u32>> {
    (0x20..0x7f).chain(0xa0..0x100).map(|c| vec![c]).collect()
}

/// Requests with varied synthetic widths.
fn requests(texts: Vec<Vec<u32>>) -> Vec<Request> {
    texts
        .into_iter()
        .enumerate()
        .map(|(i, unicode)| Request {
            unicode,
            width: Some(400.0 + (i % 7) as f64 * 50.0),
            char_code: None,
        })
        .collect()
}

/// Requests of all the codepoints of some glyph data, with natural widths.
fn all_the_glyphs(codepoints: impl Iterator<Item = u32>) -> Vec<Request> {
    codepoints
        .map(|cp| Request {
            unicode: vec![cp],
            ..Request::default()
        })
        .collect()
}

fn bench_font(bencher: Bencher, family: Family, requests: &[Request], options: &Options) {
    generate_font(family, requests, options);
    bencher.bench(|| generate_font(family, black_box(requests), black_box(options)));
}

/// The font of a text, in each family.
#[divan::bench(args = FAMILIES)]
fn text(bencher: Bencher, family: Family) {
    bench_font(bencher, family, &requests(chars(TEXT)), &Options::default());
}

/// The styles: embedded or synthesized (emboldening, thinning, slant).
#[derive(Clone, Copy, Debug)]
enum Style {
    Regular,
    Bold,
    Black,
    Light,
    Oblique,
    SerifBoldItalic,
}

/// The Latin-1 glyphs, in each style.
#[divan::bench(args = [
    Style::Regular,
    Style::Bold,
    Style::Black,
    Style::Light,
    Style::Oblique,
    Style::SerifBoldItalic,
])]
fn latin_1_style(bencher: Bencher, style: Style) {
    let mut family = Family::Sans;
    let options = match style {
        Style::Regular => Options::default(),
        Style::Bold => Options {
            bold: true,
            ..Options::default()
        },
        Style::Black => Options {
            weight: Some(900.0),
            ..Options::default()
        },
        Style::Light => Options {
            weight: Some(300.0),
            ..Options::default()
        },
        Style::Oblique => Options {
            italic: true,
            ..Options::default()
        },
        Style::SerifBoldItalic => {
            family = Family::Serif;
            Options {
                bold: true,
                italic: true,
                ..Options::default()
            }
        }
    };
    bench_font(bencher, family, &requests(latin_1()), &options);
}

/// All the glyphs of each family.
#[divan::bench(args = FAMILIES, sample_count = 20)]
fn all_glyphs(bencher: Bencher, family: Family) {
    let requests = all_the_glyphs(glyph_data(family).codepoints());
    bench_font(bencher, family, &requests, &Options::default());
}

/// Symbols, ligatures, combining sequences and normalization fallbacks.
#[divan::bench]
fn fallbacks(bencher: Bencher) {
    let mut texts = chars("∑≤≥≠∞ ←↑→↓ ■●▲◆ ☐☑ ✓✗ ★♠♣♥♦ 𝐀𝐁𝐂 ㎡㎏ ①②③");
    texts.extend(
        [
            "ffi",
            "ffl",
            "e\u{301}",
            "q\u{301}",
            "a\u{308}\u{304}",
            "n\u{303}",
        ]
        .map(|text| text.chars().map(u32::from).collect()),
    );
    let options = Options {
        bold: true,
        ..Options::default()
    };
    bench_font(bencher, Family::Sans, &requests(texts), &options);
}

/// All glyphs of selected standard fonts.
#[divan::bench(args = [
    StandardFont::Helvetica,
    StandardFont::TimesBoldItalic,
    StandardFont::CourierOblique,
    StandardFont::Symbol,
    StandardFont::ZapfDingbats,
], sample_count = 20)]
fn standard_font(bencher: Bencher, font: StandardFont) {
    let requests = all_the_glyphs(standard_glyph_data(font).codepoints());
    let options = Options::default();
    generate_standard_font(font, &requests, &options);
    bencher.bench(|| generate_standard_font(font, black_box(&requests), black_box(&options)));
}

/// The fonts of a text layer, without outlines.
#[derive(Clone, Copy, Debug)]
enum TextLayer {
    Latin1,
    Kerning,
    DebugBoxes,
    /// Codepoints which aren't in the glyph data (e.g. a CJK text).
    Cjk,
}

#[divan::bench(args = [
    TextLayer::Latin1,
    TextLayer::Kerning,
    TextLayer::DebugBoxes,
    TextLayer::Cjk,
])]
fn text_layer(bencher: Bencher, case: TextLayer) {
    let mut options = Options {
        metrics_only: true,
        ascent: Some(0.8),
        ..Options::default()
    };
    let requests = match case {
        TextLayer::Cjk => (0x4e00..0x4e00 + 5000)
            .map(|cp| Request {
                unicode: vec![cp],
                width: Some(1000.0),
                char_code: None,
            })
            .collect(),
        _ => requests(latin_1()),
    };
    match case {
        // 1000 pairs of the first 32 glyphs.
        TextLayer::Kerning => {
            options.kerning = (0..1000)
                .map(|i| Kerning {
                    left: i / 32,
                    right: i % 32,
                    value: -5.0 - (i % 50) as f64,
                })
                .collect();
        }
        TextLayer::DebugBoxes => options.debug_boxes = true,
        _ => {}
    }
    bench_font(bencher, Family::Sans, &requests, &options);
}

/// The lookup of the glyphs which can't be rendered: Latin-1, symbols, and a
/// CJK text.
#[divan::bench]
fn missing(bencher: Bencher) {
    let mut texts = latin_1();
    texts.extend(chars("∑≤≥≠∞ ←↑→↓ ■●▲◆ ☐☑ ✓✗ ★♠♣♥♦ 𝐀𝐁𝐂 ㎡㎏ ①②③"));
    texts.extend((0x4e00..0x4e00 + 2000).map(|cp| vec![cp]));
    let texts: Vec<&[u32]> = texts.iter().map(Vec::as_slice).collect();
    missing_glyphs(Family::Sans, &texts);
    bencher.bench(|| missing_glyphs(Family::Sans, black_box(&texts)));
}
