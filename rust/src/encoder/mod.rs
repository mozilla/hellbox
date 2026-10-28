// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The encoder of the glyph data files (with the `encoder` feature): it reads
//! TrueType fonts (`font.rs`), keeps and transforms their glyphs (`build.rs`)
//! and compresses them (`search.rs`). It also generates the normalization
//! tables (`unicode.rs`).

mod build;
mod font;
mod pool;
mod range_encoder;
mod search;
mod unicode;

use crate::{model, payload, Error};
use std::path::Path;

pub use build::{build_glyph_data, BuildOptions, BuildStats, Codec, FontSpec, Stems};
pub use search::EncoderOptions;
pub use unicode::{unicode_tables, UnicodeTables};

// Fallback subsets for punctuation, math, arrows, technical symbols and dingbats.
// Supplemental arrows and math operators (U+2900-2AFF) are outside these subsets.
const MATH_RANGES: &str = "2000-206F,2100-214F,2190-21FF,2200-22FF,2300-23FF,25A0-25FF,27C0-27FF";
const SYMBOLS_RANGES: &str = "2100-214F,2190-21FF,2300-23FF,2400-24FF,25A0-25FF,2600-27BF";
const SYMBOLS2_RANGES: &str = "2190-21FF,2300-23FF,2400-24FF,25A0-25FF,2600-27BF,2B00-2BFF";

// The codepoints drawn by pdf.js for the glyphs of the Symbol and
// ZapfDingbats fonts (their encodings and AFM files), after the mapping of
// their private use codepoints (e.g. U+F8F4 braceex to U+23AA).
const SYMBOL_CODEPOINTS: &str = "20-21,23,25-26,28-29,2B-2C,2E-3F,5B,5D,5F,7B-7D,A9,AC,AE,\
    B0-B1,B5,D7,F7,192,391-393,395-3A1,3A3-3A8,3B1-3BB,3BD-3C9,3D1-3D2,3D5-3D6,2022,2026,\
    2032-2033,203E,2044,20AC,2111,2118,211C,2122,2126,2135,2190-2194,21B5,21D0-21D4,2200,\
    2202-2203,2205-2209,220B,220F,2211-2212,2217,221A,221D-221E,2220,2227-222B,2234,223C,\
    2245,2248,2260-2261,2264-2265,2282-2284,2286-2287,2295,2297,22A5,22C5,2320-2321,\
    2329-232A,239B-23AF,23D0,25CA,2660,2663,2665-2666";
const DINGBATS_CODEPOINTS: &str = "20,2192,2194-2195,2460-2469,25A0,25B2,25BC,25C6,25CF,25D7,\
    2605,260E,261B,261E,2660,2663,2665-2666,2701-2704,2706-2709,270C-2727,2729-274B,274D,\
    274F-2752,2756,2758-275E,2761-2794,2798-27AF,27B1-27BE";
// The characters of Wingdings, Wingdings 2 and Wingdings 3, as pdf.js maps
// their codes to Unicode (Unicode 7 encoded them, see the proposal L2/11-344).
const WINGDINGS_CODEPOINTS: &str = "20,2022,203B,203D,2042,21E6-21EA,21F3,2219,2299,22C5,2303,\
    2316,2318,2324-2327,232B,237D,238B,23F3,2423,2460-2469,24EA,24FF,25A0-25A1,25A3,25AA,\
    25B2-25B3,25B6-25B7,25BC-25BD,25C0-25C1,25C6-25C8,25CA-25CB,25CF,25D6-25D7,25E2-25E5,25FC,\
    25FE,2605,2609,2611-2612,261C-261E,262A,262F,2638-263A,263C-263E,2648-2653,26AB,2700-2702,\
    2704,2706-2708,270C-270D,2713,271D-271E,2720-2721,272A,272F-2730,2736,2739,2744,2751-2752,\
    2756,2776-2793,2981,29B8,29BF,29EB,2B00-2B04,2B1D,2B1F,2B22-2B23,2B25,2B27,2B29-2B2A,2B4D,\
    2B58,2B60-2B73,2B76,2B78,2B7B,2B7D-2B80,2B83-2B93,2B98-2BB9,2BBD-2BC8,2BCA-2BD1,2E3F,\
    1F10B-1F10C,1F315,1F322,1F3F1,1F3F3,1F3F5-1F3F6,1F446-1F449,1F44D-1F44E,1F453,1F4A3,1F4CB,\
    1F4EA-1F4ED,1F53E-1F53F,1F546-1F549,1F550-1F567,1F56D-1F56F,1F571,1F57D-1F57F,1F582-1F583,\
    1F589-1F593,1F597-1F5A3,1F5AB-1F5B8,1F5C0-1F5C1,1F5C4-1F5D1,1F5D4,1F5F4-1F5F9,1F610,\
    1F650-1F66B,1F670-1F677,1F679-1F67B,1F6C7,1F780-1F7C7,1F7C9-1F7D4,1F800-1F80B,1F810-1F820,\
    1F822,1F824,1F826,1F828,1F82A,1F82C,1F82E,1F830,1F832,1F834,1F836,1F838-1F847,1F850-1F859,\
    1F860-1F887,1F890-1F8AD,1FA9F";

/// Output file, source font, reference font of the other weight, and output
/// family name for each embedded style. Sans-serif and monospace italics are
/// synthesized at runtime. Output names avoid the Reserved Font Names listed
/// in `data/LICENSE_LIBERATION`.
const FAMILY_FONTS: [(&str, &str, &str, &str); 8] = [
    (
        "Sans-Regular",
        "Arimo-Regular",
        "Arimo-Bold",
        "Hellbox Sans",
    ),
    ("Sans-Bold", "Arimo-Bold", "Arimo-Regular", "Hellbox Sans"),
    (
        "Serif-Regular",
        "Tinos-Regular",
        "Tinos-Bold",
        "Hellbox Serif",
    ),
    ("Serif-Bold", "Tinos-Bold", "Tinos-Regular", "Hellbox Serif"),
    (
        "Serif-Italic",
        "Tinos-Italic",
        "Tinos-BoldItalic",
        "Hellbox Serif",
    ),
    (
        "Serif-BoldItalic",
        "Tinos-BoldItalic",
        "Tinos-Italic",
        "Hellbox Serif",
    ),
    (
        "Mono-Regular",
        "LiberationMono-Regular",
        "LiberationMono-Bold",
        "Hellbox Mono",
    ),
    (
        "Mono-Bold",
        "LiberationMono-Bold",
        "LiberationMono-Regular",
        "Hellbox Mono",
    ),
];

/// The characters of the 8-bit font encodings of PDF beyond Latin-1
/// (WinAnsiEncoding, StandardEncoding and MacRomanEncoding, as pdf.js maps
/// their glyph names).
const ENCODINGS_EXTRA: [u32; 54] = [
    0x131, 0x141, 0x142, 0x152, 0x153, 0x160, 0x161, 0x178, 0x17d, 0x17e, 0x192, 0x2c6, 0x2c7,
    0x2d8, 0x2d9, 0x2da, 0x2db, 0x2dc, 0x2dd, 0x3c0, 0x2013, 0x2014, 0x2018, 0x2019, 0x201a,
    0x201c, 0x201d, 0x201e, 0x2020, 0x2021, 0x2022, 0x2026, 0x2030, 0x2039, 0x203a, 0x2044, 0x20ac,
    0x2122, 0x2126, 0x2202, 0x2206, 0x220f, 0x2211, 0x221a, 0x221e, 0x222b, 0x2248, 0x2260, 0x2264,
    0x2265, 0x25ca, 0xf8ff, 0xfb01, 0xfb02,
];

/// Prioritizes the characters of the 8-bit encodings (Latin-1 and
/// `ENCODINGS_EXTRA`) in family files, allowing requests for those characters
/// to decode a prefix: pdf.js requests every character of a font's encoding.
fn front_codepoints() -> Vec<u32> {
    (0x20..0x7f)
        .chain(0xa0..0x100)
        .chain(ENCODINGS_EXTRA)
        .collect()
}

/// The builds of the files of `data/`, from the fonts of `sources/` (see
/// `tools/fetch_sources.sh`): the families, the symbols used as a fallback
/// for all of them, and the glyphs of Symbol, ZapfDingbats and Wingdings.
pub fn data_builds(sources: &Path, grid: u8) -> Vec<(String, BuildOptions)> {
    let src = |name: &str| sources.join(format!("{name}.ttf"));
    let mut builds = Vec::new();
    for (file, font, reference, family) in FAMILY_FONTS {
        builds.push((
            format!("{file}.hbgd"),
            BuildOptions {
                reference: Some(src(reference)),
                family: Some(family.to_string()),
                serif: file.starts_with("Serif"),
                fixed_pitch: Some(file.starts_with("Mono")),
                front: front_codepoints(),
                grid,
                ..BuildOptions::new(src(font))
            },
        ));
    }

    let mut symbols = BuildOptions {
        family: Some("Noto Symbols (subset)".to_string()),
        grid,
        ..BuildOptions::new(src("NotoSansMath-Regular"))
    };
    symbols.font.ranges = Some(MATH_RANGES.to_string());
    symbols.fallback = [
        ("NotoSansMono-Regular", "2500-259F"),
        ("NotoSansSymbols-Regular", SYMBOLS_RANGES),
        ("NotoSansSymbols2-Regular", SYMBOLS2_RANGES),
    ]
    .map(|(font, ranges)| FontSpec {
        path: src(font),
        ranges: Some(ranges.to_string()),
    })
    .to_vec();
    symbols.exclude_covered_by = ["Arimo-Regular", "Tinos-Regular", "LiberationMono-Regular"]
        .map(src)
        .to_vec();
    builds.push(("NotoSymbols.hbgd".to_string(), symbols));

    // The glyphs of Symbol, ZapfDingbats and Wingdings, from the first font
    // having them.
    for (file, family, fonts, codepoints) in [
        (
            "Symbol.hbgd",
            "Hellbox Symbol",
            // Times-like glyphs first.
            &[
                "Tinos-Regular",
                "NotoSansMath-Regular",
                "NotoSansSymbols-Regular",
            ][..],
            SYMBOL_CODEPOINTS,
        ),
        (
            "ZapfDingbats.hbgd",
            "Hellbox Dingbats",
            &["NotoSansSymbols2-Regular", "NotoSansSymbols-Regular"][..],
            DINGBATS_CODEPOINTS,
        ),
        (
            "Wingdings.hbgd",
            // Not the name of the font it replaces, a trademark.
            "Hellbox Pictographs",
            &[
                "NotoSansSymbols2-Regular",
                "NotoSansSymbols-Regular",
                "NotoSansMath-Regular",
                "Tinos-Regular",
            ][..],
            WINGDINGS_CODEPOINTS,
        ),
    ] {
        let mut specs = fonts.iter().map(|font| FontSpec {
            path: src(font),
            ranges: Some(codepoints.to_string()),
        });
        let font = specs.next().unwrap();
        builds.push((
            file.to_string(),
            BuildOptions {
                font,
                fallback: specs.collect(),
                family: Some(family.to_string()),
                serif: file == "Symbol.hbgd",
                grid,
                ..BuildOptions::new("")
            },
        ));
    }
    builds
}

/// Model-compresses a payload, returning the revision followed by range-coded
/// bytes. Returns `InvalidData` if parsing or byte-for-byte round-trip
/// validation fails.
pub fn encode_model_payload(payload: &[u8], options: EncoderOptions) -> Result<Vec<u8>, Error> {
    let coded = search::encode(&payload::parse(payload)?, options)?;
    if decode_model_payload(&coded)? != payload {
        return Err(Error::InvalidData);
    }
    Ok(coded)
}

/// Decodes a payload compressed with the model codec (the model revision,
/// then the range coded data) to the uncompressed payload.
pub fn decode_model_payload(coded: &[u8]) -> Result<Vec<u8>, Error> {
    if coded.first() != Some(&crate::glyph_data::MODEL_REVISION) {
        return Err(Error::UnsupportedCompression);
    }
    payload::serialize(&model::decode(&coded[1..])?).map_err(|_| Error::InvalidData)
}
