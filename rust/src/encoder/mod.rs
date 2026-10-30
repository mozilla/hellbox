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

pub use build::{build_glyph_data, BuildOptions, BuildStats, Codec, FontSpec, Metrics, Stems};
pub use search::EncoderOptions;
pub use unicode::{unicode_tables, UnicodeTables};

// Fallback subsets for punctuation, math, arrows, technical symbols and dingbats.
// Supplemental arrows and math operators (U+2900-2AFF) are outside these subsets.
const MATH_RANGES: &str = "2000-206F,2100-214F,2190-21FF,2200-22FF,2300-23FF,25A0-25FF,27C0-27FF";
const SYMBOLS_RANGES: &str = "2100-214F,2190-21FF,2300-23FF,2400-24FF,25A0-25FF,2600-27BF";
const SYMBOLS2_RANGES: &str = "2190-21FF,2300-23FF,2400-24FF,25A0-25FF,2600-27BF,2B00-2BFF";

// AFM glyph names and Unicode codepoints used by this data ("name:codepoint").
// These are pdf.js's Symbol and ZapfDingbats mappings, with braceex and
// radicalex at U+23AA and U+203E rather than at their private use codepoints
// (U+F8F4 and U+F8E5), which pdf.js must map like this. The sans-serif ®, ©
// and ™ share codepoints with the serif variants and are omitted.
const SYMBOL_GLYPHS: &str =
    "space:20 exclam:21 numbersign:23 percent:25 ampersand:26 parenleft:28 parenright:29 plus:2B \
    comma:2C period:2E slash:2F zero:30 one:31 two:32 three:33 four:34 five:35 six:36 seven:37 \
    eight:38 nine:39 colon:3A semicolon:3B less:3C equal:3D greater:3E question:3F \
    bracketleft:5B bracketright:5D underscore:5F braceleft:7B bar:7C braceright:7D \
    copyrightserif:A9 logicalnot:AC registerserif:AE degree:B0 plusminus:B1 mu:B5 multiply:D7 \
    divide:F7 florin:192 Alpha:391 Beta:392 Gamma:393 Epsilon:395 Zeta:396 Eta:397 Theta:398 \
    Iota:399 Kappa:39A Lambda:39B Mu:39C Nu:39D Xi:39E Omicron:39F Pi:3A0 Rho:3A1 Sigma:3A3 \
    Tau:3A4 Upsilon:3A5 Phi:3A6 Chi:3A7 Psi:3A8 alpha:3B1 beta:3B2 gamma:3B3 delta:3B4 \
    epsilon:3B5 zeta:3B6 eta:3B7 theta:3B8 iota:3B9 kappa:3BA lambda:3BB nu:3BD xi:3BE \
    omicron:3BF pi:3C0 rho:3C1 sigma1:3C2 sigma:3C3 tau:3C4 upsilon:3C5 phi:3C6 chi:3C7 psi:3C8 \
    omega:3C9 theta1:3D1 Upsilon1:3D2 phi1:3D5 omega1:3D6 bullet:2022 ellipsis:2026 minute:2032 \
    second:2033 radicalex:203E fraction:2044 Euro:20AC Ifraktur:2111 weierstrass:2118 \
    Rfraktur:211C trademarkserif:2122 Omega:2126 aleph:2135 arrowleft:2190 arrowup:2191 \
    arrowright:2192 arrowdown:2193 arrowboth:2194 carriagereturn:21B5 arrowdblleft:21D0 \
    arrowdblup:21D1 arrowdblright:21D2 arrowdbldown:21D3 arrowdblboth:21D4 universal:2200 \
    partialdiff:2202 existential:2203 emptyset:2205 Delta:2206 gradient:2207 element:2208 \
    notelement:2209 suchthat:220B product:220F summation:2211 minus:2212 asteriskmath:2217 \
    radical:221A proportional:221D infinity:221E angle:2220 logicaland:2227 logicalor:2228 \
    intersection:2229 union:222A integral:222B therefore:2234 similar:223C congruent:2245 \
    approxequal:2248 notequal:2260 equivalence:2261 lessequal:2264 greaterequal:2265 \
    propersubset:2282 propersuperset:2283 notsubset:2284 reflexsubset:2286 reflexsuperset:2287 \
    circleplus:2295 circlemultiply:2297 perpendicular:22A5 dotmath:22C5 integraltp:2320 \
    integralbt:2321 angleleft:2329 angleright:232A parenlefttp:239B parenleftex:239C \
    parenleftbt:239D parenrighttp:239E parenrightex:239F parenrightbt:23A0 bracketlefttp:23A1 \
    bracketleftex:23A2 bracketleftbt:23A3 bracketrighttp:23A4 bracketrightex:23A5 \
    bracketrightbt:23A6 bracelefttp:23A7 braceleftmid:23A8 braceleftbt:23A9 braceex:23AA \
    bracerighttp:23AB bracerightmid:23AC bracerightbt:23AD integralex:23AE arrowhorizex:23AF \
    arrowvertex:23D0 lozenge:25CA spade:2660 club:2663 heart:2665 diamond:2666";
const DINGBATS_GLYPHS: &str =
    "space:20 a161:2192 a163:2194 a164:2195 a120:2460 a121:2461 a122:2462 a123:2463 a124:2464 \
    a125:2465 a126:2466 a127:2467 a128:2468 a129:2469 a73:25A0 a76:25B2 a77:25BC a78:25C6 \
    a71:25CF a81:25D7 a35:2605 a4:260E a11:261B a12:261E a109:2660 a112:2663 a110:2665 a111:2666 \
    a1:2701 a2:2702 a202:2703 a3:2704 a5:2706 a119:2707 a118:2708 a117:2709 a13:270C a14:270D \
    a15:270E a16:270F a105:2710 a17:2711 a18:2712 a19:2713 a20:2714 a21:2715 a22:2716 a23:2717 \
    a24:2718 a25:2719 a26:271A a27:271B a28:271C a6:271D a7:271E a8:271F a9:2720 a10:2721 \
    a29:2722 a30:2723 a31:2724 a32:2725 a33:2726 a34:2727 a36:2729 a37:272A a38:272B a39:272C \
    a40:272D a41:272E a42:272F a43:2730 a44:2731 a45:2732 a46:2733 a47:2734 a48:2735 a49:2736 \
    a50:2737 a51:2738 a52:2739 a53:273A a54:273B a55:273C a56:273D a57:273E a58:273F a59:2740 \
    a60:2741 a61:2742 a62:2743 a63:2744 a64:2745 a65:2746 a66:2747 a67:2748 a68:2749 a69:274A \
    a70:274B a72:274D a74:274F a203:2750 a75:2751 a204:2752 a79:2756 a82:2758 a83:2759 a84:275A \
    a97:275B a98:275C a99:275D a100:275E a101:2761 a102:2762 a103:2763 a104:2764 a106:2765 \
    a107:2766 a108:2767 a89:2768 a90:2769 a93:276A a94:276B a91:276C a92:276D a205:276E a85:276F \
    a206:2770 a86:2771 a87:2772 a88:2773 a95:2774 a96:2775 a130:2776 a131:2777 a132:2778 \
    a133:2779 a134:277A a135:277B a136:277C a137:277D a138:277E a139:277F a140:2780 a141:2781 \
    a142:2782 a143:2783 a144:2784 a145:2785 a146:2786 a147:2787 a148:2788 a149:2789 a150:278A \
    a151:278B a152:278C a153:278D a154:278E a155:278F a156:2790 a157:2791 a158:2792 a159:2793 \
    a160:2794 a196:2798 a165:2799 a192:279A a166:279B a167:279C a168:279D a169:279E a170:279F \
    a171:27A0 a172:27A1 a173:27A2 a162:27A3 a174:27A4 a175:27A5 a176:27A6 a177:27A7 a178:27A8 \
    a179:27A9 a193:27AA a180:27AB a199:27AC a181:27AD a200:27AE a182:27AF a201:27B1 a183:27B2 \
    a184:27B3 a197:27B4 a185:27B5 a194:27B6 a198:27B7 a186:27B8 a195:27B9 a187:27BA a188:27BB \
    a189:27BC a190:27BD a191:27BE";
/// The pieces of the big delimiters, integrals and arrows of Symbol, and its
/// radical and its extension: they're moved exactly onto their boxes, to join
/// (see `Metrics`).
const SYMBOL_PIECES: [&str; 26] = [
    "parenlefttp",
    "parenleftex",
    "parenleftbt",
    "parenrighttp",
    "parenrightex",
    "parenrightbt",
    "bracketlefttp",
    "bracketleftex",
    "bracketleftbt",
    "bracketrighttp",
    "bracketrightex",
    "bracketrightbt",
    "bracelefttp",
    "braceleftmid",
    "braceleftbt",
    "bracerighttp",
    "bracerightmid",
    "bracerightbt",
    "braceex",
    "integraltp",
    "integralex",
    "integralbt",
    "arrowvertex",
    "arrowhorizex",
    "radical",
    "radicalex",
];
/// Adobe's phi is the stroked form and its phi1 the loopy one, the reverse of
/// the glyphs of U+03C6 and U+03D5 (where pdf.js maps them) in the fonts: use
/// U+03D5 as the source for phi and U+03C6 for phi1.
const SYMBOL_GLYPHS_OF: [(u32, u32); 2] = [(0x3c6, 0x3d5), (0x3d5, 0x3c6)];
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
    // having them. Symbol and ZapfDingbats also use AFM widths and fitting.
    let metrics = |file: &str, glyphs: &str, exact: &[&str], glyphs_of: &[(u32, u32)]| Metrics {
        path: sources.join(file),
        names: glyph_names(glyphs),
        exact: exact.iter().map(|name| name.to_string()).collect(),
        glyphs_of: glyphs_of.to_vec(),
    };
    for (file, family, fonts, metrics) in [
        (
            "Symbol.hbgd",
            "Hellbox Symbol",
            // Times-like glyphs first.
            &[
                "Tinos-Regular",
                "NotoSansMath-Regular",
                "NotoSansSymbols-Regular",
            ][..],
            Some(metrics(
                "Symbol.afm",
                SYMBOL_GLYPHS,
                &SYMBOL_PIECES,
                &SYMBOL_GLYPHS_OF,
            )),
        ),
        (
            "ZapfDingbats.hbgd",
            "Hellbox Dingbats",
            &["NotoSansSymbols2-Regular", "NotoSansSymbols-Regular"][..],
            Some(metrics("ZapfDingbats.afm", DINGBATS_GLYPHS, &[], &[])),
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
            None,
        ),
    ] {
        let codepoints = match &metrics {
            Some(metrics) => {
                let cps: Vec<String> = metrics
                    .names
                    .iter()
                    .map(|(cp, _)| format!("{cp:X}"))
                    .collect();
                cps.join(",")
            }
            None => WINGDINGS_CODEPOINTS.to_string(),
        };
        let mut specs = fonts.iter().map(|font| FontSpec {
            path: src(font),
            ranges: Some(codepoints.clone()),
        });
        let font = specs.next().unwrap();
        builds.push((
            file.to_string(),
            BuildOptions {
                font,
                fallback: specs.collect(),
                family: Some(family.to_string()),
                serif: file == "Symbol.hbgd",
                metrics,
                grid,
                ..BuildOptions::new("")
            },
        ));
    }
    builds
}

/// The codepoints and the glyph names of a table of glyphs ("name:codepoint",
/// see `SYMBOL_GLYPHS`).
fn glyph_names(table: &str) -> Vec<(u32, String)> {
    table
        .split_whitespace()
        .map(|glyph| {
            let (name, cp) = glyph.split_once(':').expect("Invalid glyph table");
            let cp = u32::from_str_radix(cp, 16).expect("Invalid glyph table");
            (cp, name.to_string())
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_glyphs_of_symbol_and_zapfdingbats() {
        for (table, count) in [(SYMBOL_GLYPHS, 186), (DINGBATS_GLYPHS, 202)] {
            let glyphs = glyph_names(table);
            let mut codepoints: Vec<u32> = glyphs.iter().map(|&(cp, _)| cp).collect();
            codepoints.sort_unstable();
            codepoints.dedup();
            assert_eq!((glyphs.len(), codepoints.len()), (count, count));
        }
        let symbol = glyph_names(SYMBOL_GLYPHS);
        for piece in SYMBOL_PIECES {
            assert!(symbol.iter().any(|(_, name)| name == piece), "{piece}");
        }
        for (cp, from) in SYMBOL_GLYPHS_OF {
            assert!(symbol.iter().any(|&(c, _)| c == cp) && symbol.iter().any(|&(c, _)| c == from));
        }
    }
}
