// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Check OTS acceptance and compare sanitized tables. The cmap is compared
//! by mappings, and head.checkSumAdjustment is ignored. This checks returned
//! bytes, not OTS diagnostic messages.

use hellbox::{
    generate_font, generate_standard_font, glyph_data, standard_glyph_data, symbols_glyph_data,
    Family, GlyphData, Kerning, Options, Request, StandardFont,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const FAMILIES: [Family; 3] = [Family::Sans, Family::Serif, Family::Mono];

/// Latin, Greek, Cyrillic, accents, a ligature, symbols and a missing glyph.
const TEXT: &str = "The quick brown fox jumps over the lazy dog 0123456789 ÀÉÎÕÜ ąčęėįšųūž \
                    Ωμέγα Привет ẞ ﬁ ∑≤≥≠ ←↑→ ■●▲ ☐☑ ✓✗ 中";

/// The tables of a font file, by tag.
fn tables(font: &[u8]) -> BTreeMap<String, &[u8]> {
    let u16_at = |pos: usize| u16::from_be_bytes([font[pos], font[pos + 1]]) as usize;
    let u32_at = |pos: usize| u32::from_be_bytes(font[pos..pos + 4].try_into().unwrap()) as usize;
    (0..u16_at(4))
        .map(|i| {
            let record = 12 + 16 * i;
            let tag = String::from_utf8_lossy(&font[record..record + 4]).into_owned();
            let offset = u32_at(record + 8);
            (tag, &font[offset..offset + u32_at(record + 12)])
        })
        .collect()
}

/// The mapping of a cmap table: its (3, 10) subtable if any, else its (3, 1)
/// one.
fn cmap(table: &[u8]) -> BTreeMap<u32, u32> {
    let u16_at = |pos: usize| u16::from_be_bytes([table[pos], table[pos + 1]]);
    let u32_at = |pos: usize| u32::from_be_bytes(table[pos..pos + 4].try_into().unwrap());
    let subtable = |platform, encoding| {
        (0..u16_at(2) as usize)
            .map(|i| 4 + 8 * i)
            .find(|&record| u16_at(record) == platform && u16_at(record + 2) == encoding)
            .map(|record| u32_at(record + 4) as usize)
    };
    let mut mapping = BTreeMap::new();
    if let Some(offset) = subtable(3, 10) {
        assert_eq!(u16_at(offset), 12, "cmap (3, 10) format");
        for i in 0..u32_at(offset + 12) as usize {
            let group = offset + 16 + 12 * i;
            let start = u32_at(group);
            for code in start..=u32_at(group + 4) {
                mapping.insert(code, u32_at(group + 8) + code - start);
            }
        }
        return mapping;
    }
    let offset = subtable(3, 1).expect("no Unicode cmap");
    assert_eq!(u16_at(offset), 4, "cmap (3, 1) format");
    let segments = u16_at(offset + 6) as usize / 2;
    let ends = offset + 14;
    let starts = ends + 2 * segments + 2;
    let deltas = starts + 2 * segments;
    let range_offsets = deltas + 2 * segments;
    for i in 0..segments {
        let start = u16_at(starts + 2 * i);
        let delta = u16_at(deltas + 2 * i);
        let range_offset = u16_at(range_offsets + 2 * i) as usize;
        for code in start..=u16_at(ends + 2 * i) {
            let gid = if range_offset == 0 {
                code.wrapping_add(delta)
            } else {
                match u16_at(range_offsets + 2 * i + range_offset + 2 * (code - start) as usize) {
                    0 => 0,
                    gid => gid.wrapping_add(delta),
                }
            };
            if gid != 0 {
                mapping.insert(code as u32, gid as u32);
            }
        }
    }
    mapping
}

/// Writes a font in `target/tmp/ots/`, to examine it when it fails (e.g.
/// with `ots-sanitize`, which prints the messages of OTS).
fn keep(name: &str, font: &[u8]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ots");
    fs::create_dir_all(&dir).unwrap();
    let file: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let path = dir.join(format!("{file}.ttf"));
    fs::write(&path, font).unwrap();
    path
}

/// Require unchanged tables, comparing cmap by mappings and ignoring the
/// head checksum adjustment.
fn check(name: &str, font: &[u8]) {
    let sanitized = fontsan::process(font).unwrap_or_else(|error| {
        panic!(
            "{name}: rejected by OTS ({error}): {}",
            keep(name, font).display()
        )
    });
    let (tables, sanitized) = (tables(font), tables(&sanitized));
    assert_eq!(
        sanitized.keys().collect::<Vec<_>>(),
        tables.keys().collect::<Vec<_>>(),
        "{name}: tables changed by OTS: {}",
        keep(name, font).display()
    );
    for (tag, &table) in &tables {
        let sanitized = sanitized[tag];
        let same = match tag.as_str() {
            "cmap" => cmap(sanitized) == cmap(table),
            // Without checkSumAdjustment, which depends on the whole file.
            "head" => sanitized[..8] == table[..8] && sanitized[12..] == table[12..],
            _ => sanitized == table,
        };
        assert!(
            same,
            "{name}: OTS changed the {tag} table: {}",
            keep(name, font).display()
        );
    }
}

fn request(unicode: &[u32]) -> Request {
    Request {
        unicode: unicode.to_vec(),
        ..Request::default()
    }
}

/// The distinct characters of `TEXT`, with various widths.
fn sample() -> Vec<Request> {
    let mut chars: Vec<char> = Vec::new();
    for c in TEXT.chars() {
        if !chars.contains(&c) {
            chars.push(c);
        }
    }
    chars
        .into_iter()
        .enumerate()
        .map(|(i, c)| Request {
            width: Some(400.0 + (i % 7) as f64 * 50.0),
            ..request(&[c as u32])
        })
        .collect()
}

fn all_the_glyphs(data: &GlyphData) -> Vec<Request> {
    data.codepoints().map(|cp| request(&[cp])).collect()
}

#[test]
fn styles() {
    let glyphs = sample();
    for family in FAMILIES {
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let options = Options {
                bold,
                italic,
                ..Options::default()
            };
            check(
                &format!("{family:?}, bold: {bold}, italic: {italic}"),
                &generate_font(family, &glyphs, &options).data,
            );
        }
    }
}

#[test]
fn all_glyphs() {
    let options = Options {
        bold: true,
        italic: true,
        ..Options::default()
    };
    for family in FAMILIES {
        let glyphs = all_the_glyphs(glyph_data(family));
        check(
            &format!("all the {family:?} glyphs"),
            &generate_font(family, &glyphs, &options).data,
        );
    }
    let glyphs = all_the_glyphs(symbols_glyph_data());
    check(
        "all the symbols",
        &generate_font(Family::Sans, &glyphs, &Options::default()).data,
    );
}

#[test]
fn standard_fonts() {
    // All their glyphs, and the sample (with glyphs of the Noto fonts).
    for font in StandardFont::ALL {
        let glyphs = all_the_glyphs(standard_glyph_data(font));
        check(
            &format!("all the {} glyphs", font.name()),
            &generate_standard_font(font, &glyphs, &Options::default()).data,
        );
        check(
            &format!("{}, sample", font.name()),
            &generate_standard_font(font, &sample(), &Options::default()).data,
        );
    }
}

#[test]
fn options() {
    let default = Options::default;
    let cases = [
        (
            "thin",
            Options {
                weight: Some(100.0),
                cap_height: Some(600.0),
                ..default()
            },
        ),
        (
            "black and slanted",
            Options {
                weight: Some(900.0),
                italic_angle: Some(-20.0),
                cap_height: Some(800.0),
                ..default()
            },
        ),
        (
            "tiny stems",
            Options {
                stem_v: Some(1.0),
                ..default()
            },
        ),
        (
            "huge stems",
            Options {
                stem_v: Some(1000.0),
                adaptive_bold: false,
                ..default()
            },
        ),
        (
            "out of range weight and cap height",
            Options {
                weight: Some(5000.0),
                cap_height: Some(1e5),
                ..default()
            },
        ),
        (
            "almost horizontal slant",
            Options {
                italic_angle: Some(89.0),
                ..default()
            },
        ),
        (
            "infinite slant",
            Options {
                italic_angle: Some(f64::NEG_INFINITY),
                ..default()
            },
        ),
        (
            "NaN everywhere",
            Options {
                units_per_em: f64::NAN,
                weight: Some(f64::NAN),
                stem_v: Some(f64::NAN),
                italic_angle: Some(f64::NAN),
                cap_height: Some(f64::NAN),
                ..default()
            },
        ),
        (
            "other width units",
            Options {
                units_per_em: 2048.0,
                italic_angle: Some(15.0),
                ..default()
            },
        ),
        (
            "an empty name",
            Options {
                name: String::new(),
                ..default()
            },
        ),
        (
            "an unusual name",
            Options {
                name: format!("Ünï (cödé) /%[]{{}}<> 😀 {}", "x".repeat(80)),
                ..default()
            },
        ),
        (
            "a huge name",
            Options {
                name: "x😀".repeat(20000),
                ..default()
            },
        ),
    ];
    let glyphs = sample();
    for (name, options) in cases {
        check(name, &generate_font(Family::Sans, &glyphs, &options).data);
    }

    // The coordinates must be bounded.
    let glyphs: Vec<Request> = glyphs
        .into_iter()
        .map(|glyph| Request {
            width: Some(1e5),
            ..glyph
        })
        .collect();
    let options = Options {
        min_scale: 0.01,
        max_scale: 100.0,
        italic: true,
        ..default()
    };
    check(
        "huge scale factors",
        &generate_font(Family::Sans, &glyphs, &options).data,
    );
}

#[test]
fn widths_and_char_codes() {
    let letter = |i: usize| request(&['A' as u32 + i as u32]);
    let widths = [0.0, 1.0, 10.0, 1e4, 1e5, 1e6, f64::INFINITY, f64::NAN, -5.0];
    let glyphs: Vec<Request> = widths
        .iter()
        .enumerate()
        .map(|(i, &width)| Request {
            width: Some(width),
            ..letter(i)
        })
        .collect();
    check(
        "extreme widths",
        &generate_font(Family::Sans, &glyphs, &Options::default()).data,
    );

    let codes = [
        0, 1, 0x20, 0x20, 0xd800, 0xdfff, 0xfffe, 0xffff, 0x10000, 0x10ffff, 0x110000, 0xffffffff,
    ];
    let glyphs: Vec<Request> = codes
        .iter()
        .enumerate()
        .map(|(i, &code)| Request {
            char_code: Some(code),
            ..letter(i)
        })
        .collect();
    check(
        "edge char codes",
        &generate_font(Family::Sans, &glyphs, &Options::default()).data,
    );

    let glyphs: Vec<Request> = (0..10000)
        .map(|i| Request {
            char_code: Some(3 * i),
            ..request(&['A' as u32])
        })
        .collect();
    let options = Options {
        italic: true,
        ..Options::default()
    };
    check(
        "a lot of scattered char codes",
        &generate_font(Family::Mono, &glyphs, &options).data,
    );
}

#[test]
fn texts() {
    // Ligatures, sequences (with symbols too: "A★" and "★A"), codepoints
    // replaced by their normalized forms (square m, bold A, fullwidth A,
    // angstrom sign), an empty text and a missing glyph.
    let texts: [&[u32]; 13] = [
        &[0x66, 0x66, 0x69],
        &[0x66, 0x69],
        &[0x73, 0x74],
        &[0x65, 0x301],
        &[0x71, 0x303],
        &[0x41, 0x2605],
        &[0x2605, 0x41],
        &[0x33a1],
        &[0x1d400],
        &[0xff21],
        &[0x212b],
        &[],
        &[0x4e00],
    ];
    let glyphs: Vec<Request> = texts.iter().map(|text| request(text)).collect();
    check(
        "ligatures, sequences and normalized forms",
        &generate_font(Family::Sans, &glyphs, &Options::default()).data,
    );
    // The symbols of the sequences are emboldened and slanted.
    let options = Options {
        bold: true,
        italic: true,
        ..Options::default()
    };
    check(
        "sequences, bold italic",
        &generate_font(Family::Serif, &glyphs, &options).data,
    );

    check(
        "no glyph",
        &generate_font(Family::Serif, &[], &Options::default()).data,
    );
    let glyphs = [request(&[0x4e00]), request(&[]), request(&[0x20])];
    check(
        "glyphs without outline",
        &generate_font(Family::Sans, &glyphs, &Options::default()).data,
    );

    // A glyph has at most 65535 points (and 32767 contours): the longer texts
    // are missing.
    let data = glyph_data(Family::Sans);
    let points = |c: char| data.outline(data.glyph_id(c as u32).unwrap()).xs.len();
    let max = 0xffff / points('A');
    let glyphs = [
        request(&vec!['A' as u32; max]),
        request(&vec!['A' as u32; max + 1]),
        request(&vec![':' as u32; 17000]),
    ];
    let font = generate_font(Family::Sans, &glyphs, &Options::default());
    assert_eq!(font.missing, vec![1, 2]);
    check("long texts", &font.data);
    // Their advances are enough without outlines.
    let options = Options {
        metrics_only: true,
        ..Options::default()
    };
    let font = generate_font(Family::Sans, &glyphs, &options);
    assert!(font.missing.is_empty());
    check("long texts, metrics only", &font.data);
}

/// A font has at most 65535 glyphs: the requests after the 65534th one are
/// missing, but without outlines the glyphs of the same advance are shared
/// (the kerned glyphs having their own glyph ids, only for the pairs which fit
/// in the `kern` table).
#[test]
fn too_many_glyphs() {
    let glyphs: Vec<Request> = (0..0x10000)
        .map(|i| Request {
            char_code: Some(i),
            ..request(&['A' as u32])
        })
        .collect();
    let num_glyphs = |font: &[u8]| {
        let maxp = tables(font)["maxp"];
        u16::from_be_bytes([maxp[4], maxp[5]])
    };
    let kern_pairs = |font: &[u8]| {
        (tables(font).get("kern")).map_or(0, |kern| u16::from_be_bytes([kern[10], kern[11]]))
    };
    let kerning = (0..glyphs.len() - 1)
        .map(|i| Kerning {
            left: i,
            right: i + 1,
            value: -10.0,
        })
        .collect();
    let metrics_only = Options {
        metrics_only: true,
        ..Options::default()
    };
    let kerned = Options {
        kerning,
        ..metrics_only.clone()
    };
    // The 10920 first pairs are kept: the 10921 first glyphs are kerned, and
    // the other ones share a glyph.
    for (name, options, missing, count, pairs) in [
        (
            "too many glyphs",
            Options::default(),
            vec![0xfffe, 0xffff],
            0xffff,
            0,
        ),
        ("a lot of glyphs, metrics only", metrics_only, vec![], 2, 0),
        (
            "a lot of kerned glyphs, metrics only",
            kerned,
            vec![],
            1 + 10921 + 1,
            10920,
        ),
    ] {
        let font = generate_font(Family::Sans, &glyphs, &options);
        assert_eq!(font.missing, missing, "{name}");
        assert_eq!(num_glyphs(&font.data), count, "{name}");
        assert_eq!(kern_pairs(&font.data), pairs, "{name}");
        let mapped = cmap(tables(&font.data)["cmap"]).len();
        assert_eq!(mapped, glyphs.len() - missing.len(), "{name}");
        check(name, &font.data);
    }
}

/// xorshift64*, for reproducible random fonts.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// In 0..n.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// In [0, max).
    fn number(&mut self, max: f64) -> f64 {
        max * (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// In [0, max), or None.
    fn option(&mut self, max: f64) -> Option<f64> {
        (self.below(2) == 0).then(|| self.number(max))
    }

    fn pick<T: Copy>(&mut self, values: &[T]) -> T {
        values[self.below(values.len())]
    }
}

#[test]
fn random_fonts() {
    const LIGATURES: [&[u32]; 6] = [
        &[0x66, 0x66],
        &[0x66, 0x69],
        &[0x66, 0x6c],
        &[0x66, 0x66, 0x69],
        &[0x66, 0x66, 0x6c],
        &[0x73, 0x74],
    ];
    let codepoints: Vec<Vec<u32>> = FAMILIES
        .iter()
        .map(|&family| glyph_data(family).codepoints().collect())
        .collect();
    let symbols: Vec<u32> = symbols_glyph_data().codepoints().collect();
    let mut random = Random(0x0123_4567_89ab_cdef);
    for i in 0..200 {
        let family = random.below(3);
        let units = random.pick(&[1000.0, 1000.0, 2048.0, 1.0]);
        let options = Options {
            units_per_em: units,
            bold: random.below(2) == 0,
            weight: random.option(1000.0),
            stem_v: random.option(0.3 * units),
            italic: random.below(2) == 0,
            italic_angle: random.option(120.0).map(|angle| angle - 60.0),
            cap_height: random.option(1.5 * units),
            adaptive_bold: random.below(4) != 0,
            ..Options::default()
        };
        let count = match random.below(20) {
            0 => 0,
            1 => 2000,
            _ => 1 + random.below(100),
        };
        let glyphs: Vec<Request> = (0..count)
            .map(|_| {
                let cp = random.pick(&codepoints[family]);
                let unicode = match random.below(20) {
                    0 => Vec::new(),
                    // Usually missing.
                    1 => vec![random.next() as u32 % 0x110000],
                    2 => vec![random.pick(&symbols)],
                    3 => vec![cp, 0x300 + random.below(0x70) as u32],
                    4 => random.pick(&LIGATURES).to_vec(),
                    _ => vec![cp],
                };
                let width = match random.below(10) {
                    0..=2 => None,
                    3 => Some(random.pick(&[0.0, -1.0, 1e5, 1e9, f64::NAN, f64::INFINITY])),
                    _ => Some(random.number(2.0 * units)),
                };
                let char_code = match random.below(10) {
                    0..=4 => None,
                    5 | 6 => Some(random.below(0x100) as u32),
                    7 => Some(random.below(0x10000) as u32),
                    8 => Some(random.below(0x110000) as u32),
                    _ => Some(random.next() as u32),
                };
                Request {
                    unicode,
                    width,
                    char_code,
                }
            })
            .collect();
        check(
            &format!("random font {i}"),
            &generate_font(FAMILIES[family], &glyphs, &options).data,
        );
        let options = Options {
            metrics_only: true,
            ..options
        };
        check(
            &format!("random font {i}, metrics only"),
            &generate_font(FAMILIES[family], &glyphs, &options).data,
        );
    }
}

/// The fonts without outlines (for a text layer), and their compact tables.
#[test]
fn metrics_only() {
    let options = Options {
        metrics_only: true,
        ..Options::default()
    };
    let font = |family, glyphs: &[Request]| generate_font(family, glyphs, &options).data;
    for family in FAMILIES {
        check(
            &format!("{family:?}, metrics only"),
            &font(family, &sample()),
        );
        let glyphs = all_the_glyphs(glyph_data(family));
        check(
            &format!("all the {family:?} glyphs, metrics only"),
            &font(family, &glyphs),
        );
    }
    check("no glyph, metrics only", &font(Family::Sans, &[]));
    let options = Options {
        ascent: Some(0.75),
        ..options.clone()
    };
    check(
        "metrics only, with an ascent",
        &generate_font(Family::Serif, &sample(), &options).data,
    );

    let letter = |i: usize| request(&['A' as u32 + (i % 26) as u32]);
    let codes = [
        0, 1, 0x20, 0x20, 0xd800, 0xdfff, 0xfffe, 0xffff, 0x10000, 0x10ffff, 0x110000, 0xffffffff,
    ];
    let glyphs: Vec<Request> = codes
        .iter()
        .enumerate()
        .map(|(i, &code)| Request {
            char_code: Some(code),
            width: Some(100.0 * i as f64),
            ..letter(i)
        })
        .collect();
    check(
        "edge char codes, metrics only",
        &font(Family::Sans, &glyphs),
    );

    // Scattered codes sharing a glyph: a big format 4 subtable.
    let glyphs: Vec<Request> = (0..10000)
        .map(|i| Request {
            char_code: Some(3 * i),
            width: Some(600.0),
            ..letter(0)
        })
        .collect();
    check(
        "a lot of scattered char codes, metrics only",
        &font(Family::Mono, &glyphs),
    );

    // Too many codes for a format 4 subtable with glyph ids arrays.
    let glyphs: Vec<Request> = (0..40000)
        .map(|i| Request {
            char_code: Some(0x20 + i),
            width: Some((i % 1000) as f64),
            ..letter(i as usize)
        })
        .collect();
    check("a lot of codes, metrics only", &font(Family::Sans, &glyphs));
}

/// The rectangles of the debug boxes, with and without outlines, and so many
/// distinct advances that `loca` needs long offsets without outlines.
#[test]
fn debug_boxes() {
    let glyphs: Vec<Request> = (0..10000)
        .map(|i| Request {
            char_code: Some(0x20 + i),
            width: Some(i as f64),
            ..request(&['A' as u32 + i % 26])
        })
        .collect();
    for metrics_only in [false, true] {
        let options = Options {
            metrics_only,
            debug_boxes: true,
            ..Options::default()
        };
        check(
            &format!("debug boxes, metrics only: {metrics_only}"),
            &generate_font(Family::Serif, &sample(), &options).data,
        );
        let options = Options {
            ascent: Some(0.8),
            ..options
        };
        let font = generate_font(Family::Sans, &glyphs, &options).data;
        check(
            &format!("a lot of debug boxes, metrics only: {metrics_only}"),
            &font,
        );
        let head = tables(&font)["head"];
        assert_eq!(&head[50..52], &[0, 1], "indexToLocFormat: long offsets");
    }
}

/// The kerning (a `kern` table), with and without outlines, up to the maximal
/// number of pairs of a subtable.
#[test]
fn kerning() {
    let glyphs = sample();
    let n = glyphs.len();
    let pairs = |count: usize| -> Vec<Kerning> {
        (0..count)
            .map(|i| Kerning {
                left: i % n,
                right: (i / n) % n,
                value: (i % 101) as f64 - 50.0,
            })
            .collect()
    };
    for metrics_only in [false, true] {
        for (name, kerning) in [("a few pairs", pairs(n)), ("all the pairs", pairs(n * n))] {
            let options = Options {
                metrics_only,
                kerning,
                ..Options::default()
            };
            check(
                &format!("kerning, {name}, metrics only: {metrics_only}"),
                &generate_font(Family::Sans, &glyphs, &options).data,
            );
        }
    }
    // More pairs than a subtable can have.
    let glyphs = all_the_glyphs(glyph_data(Family::Sans));
    let n = glyphs.len();
    let options = Options {
        metrics_only: true,
        kerning: (0..12000)
            .map(|i| Kerning {
                left: i % 110,
                right: (i / 110) % n,
                value: -10.0,
            })
            .collect(),
        ..Options::default()
    };
    check(
        "kerning, too many pairs",
        &generate_font(Family::Sans, &glyphs, &options).data,
    );
}
