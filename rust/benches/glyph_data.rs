// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Benchmarks full decoding and lazy decoding of an encoding subset.

use divan::{black_box, Bencher};
use hellbox::GlyphData;

fn main() {
    divan::main();
}

/// Representative family, fallback-symbol and standard-symbol files.
const FILES: [&str; 6] = [
    "Sans-Regular",
    "Serif-BoldItalic",
    "Mono-Regular",
    "NotoSymbols",
    "Symbol",
    "ZapfDingbats",
];

fn bytes(name: &str) -> &'static [u8] {
    match name {
        "Sans-Regular" => include_bytes!("../data/Sans-Regular.hbgd"),
        "Serif-BoldItalic" => include_bytes!("../data/Serif-BoldItalic.hbgd"),
        "Mono-Regular" => include_bytes!("../data/Mono-Regular.hbgd"),
        "NotoSymbols" => include_bytes!("../data/NotoSymbols.hbgd"),
        "Symbol" => include_bytes!("../data/Symbol.hbgd"),
        "ZapfDingbats" => include_bytes!("../data/ZapfDingbats.hbgd"),
        _ => unreachable!("{name}"),
    }
}

#[divan::bench(args = FILES, sample_count = 20)]
fn decode(bencher: Bencher, name: &str) {
    let bytes = bytes(name);
    bencher.bench(|| GlyphData::new(black_box(bytes)).unwrap());
}

/// Latin-1 and extra codepoints matching the encoder's front-glyph selection.
fn encodings() -> Vec<u32> {
    let extra = [
        0x131, 0x141, 0x142, 0x152, 0x153, 0x160, 0x161, 0x178, 0x17d, 0x17e, 0x192, 0x2c6, 0x2c7,
        0x2d8, 0x2d9, 0x2da, 0x2db, 0x2dc, 0x2dd, 0x3c0, 0x2013, 0x2014, 0x2018, 0x2019, 0x201a,
        0x201c, 0x201d, 0x201e, 0x2020, 0x2021, 0x2022, 0x2026, 0x2030, 0x2039, 0x203a, 0x2044,
        0x20ac, 0x2122, 0x2126, 0x2202, 0x2206, 0x220f, 0x2211, 0x221a, 0x221e, 0x222b, 0x2248,
        0x2260, 0x2264, 0x2265, 0x25ca, 0xf8ff, 0xfb01, 0xfb02,
    ];
    (0x20..0x7f).chain(0xa0..0x100).chain(extra).collect()
}

/// The decoding of the glyphs of the 8-bit encodings only (see
/// `GlyphData::lazy`).
#[divan::bench(args = ["Sans-Regular", "Serif-BoldItalic", "Mono-Regular"], sample_count = 20)]
fn decode_encodings(bencher: Bencher, name: &str) {
    let bytes = bytes(name);
    let codepoints = encodings();
    bencher.bench(|| {
        let data = GlyphData::lazy(black_box(bytes)).unwrap();
        let gids: Vec<usize> = codepoints
            .iter()
            .filter_map(|&cp| data.glyph_id(cp))
            .collect();
        data.prepare(&gids);
        data
    });
}
