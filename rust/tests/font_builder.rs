// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Behaviors of the generated fonts, read with read-fonts (through skrifa):
//! the char codes and the widths, the emboldening, the slant, the ligatures
//! and sequences, the fallback glyph data, the standard fonts.

use hellbox::{
    generate_font, generate_standard_font, glyph_data, missing_standard_glyphs,
    standard_glyph_data, style_glyph_data, symbols_glyph_data, Family, GlyphData, Kerning, Options,
    Request, StandardFont,
};
use skrifa::raw::tables::glyf::Glyph;
use skrifa::raw::types::Tag;
use skrifa::raw::{FontRef, TableProvider};
use skrifa::GlyphId;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    x: f64,
    y: f64,
    on: bool,
}

type Contours = Vec<Vec<Point>>;

/// What the tests check in a generated font.
struct Font {
    /// The contours of the glyphs (none for the empty ones).
    glyphs: Vec<Contours>,
    advances: Vec<u16>,
    cmap: BTreeMap<u32, u32>,
    weight_class: u16,
    italic_angle: f64,
}

fn read(data: &[u8]) -> Font {
    let font = FontRef::new(data).unwrap();
    let num_glyphs = font.maxp().unwrap().num_glyphs() as u32;
    let hmtx = font.hmtx().unwrap();
    let (loca, glyf) = (font.loca(None).unwrap(), font.glyf().unwrap());
    let glyphs = (0..num_glyphs)
        .map(
            |gid| match loca.get_glyf(GlyphId::new(gid), &glyf).unwrap() {
                None => Vec::new(),
                Some(Glyph::Simple(glyph)) => {
                    let points: Vec<Point> = glyph
                        .points()
                        .map(|p| Point {
                            x: p.x as f64,
                            y: p.y as f64,
                            on: p.on_curve,
                        })
                        .collect();
                    let mut start = 0;
                    (glyph.end_pts_of_contours().iter())
                        .map(|end| {
                            let end = end.get() as usize;
                            let contour = points[start..=end].to_vec();
                            start = end + 1;
                            contour
                        })
                        .collect()
                }
                Some(Glyph::Composite(_)) => panic!("Composite glyph {gid}."),
            },
        )
        .collect();
    let (_, _, subtable) = font.cmap().unwrap().best_subtable().unwrap();
    Font {
        glyphs,
        advances: (0..num_glyphs)
            .map(|gid| hmtx.advance(GlyphId::new(gid)).unwrap())
            .collect(),
        // Without the mappings to .notdef (e.g. 0xFFFF in a format 4 subtable).
        cmap: subtable
            .iter()
            .map(|(c, gid)| (c, gid.to_u32()))
            .filter(|&(_, gid)| gid != 0)
            .collect(),
        weight_class: font.os2().unwrap().us_weight_class(),
        italic_angle: font.post().unwrap().italic_angle().to_f64(),
    }
}

fn request(text: &str, width: Option<f64>, char_code: Option<u32>) -> Request {
    Request {
        unicode: text.chars().map(u32::from).collect(),
        width,
        char_code,
    }
}

/// A width in 1/1000 em, in the units of the glyph data of a family.
fn units(family: Family, width: f64) -> f64 {
    (width * glyph_data(family).units_per_em as f64 / 1000.0).round()
}

/// The font of the requests for some texts, at their natural widths.
fn font_of(texts: &[&str], options: &Options) -> Font {
    let requests: Vec<Request> = texts.iter().map(|t| request(t, None, None)).collect();
    read(&generate_font(Family::Sans, &requests, options).data)
}

struct Bounds {
    x_min: f64,
    x_max: f64,
    y_max: f64,
}

/// The bounds of the points.
fn bounds(contours: &Contours) -> Bounds {
    let points = || contours.iter().flatten();
    Bounds {
        x_min: points().map(|p| p.x).fold(f64::INFINITY, f64::min),
        x_max: points().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max),
        y_max: points().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max),
    }
}

/// The abscissas where the outline crosses the horizontal line at y, sorted
/// (the curves being flattened).
fn crossings(contours: &Contours, y: f64) -> Vec<f64> {
    let mut xs = Vec::new();
    let mut segment = |(x0, y0): (f64, f64), (x1, y1): (f64, f64)| {
        let (a, b) = (y0 - y, y1 - y);
        if (a < 0.0) != (b < 0.0) {
            xs.push(x0 + (x1 - x0) * a / (a - b));
        }
    };
    for contour in contours {
        // The on-curve points implied between the off-curve ones.
        let mut points = Vec::new();
        for (i, &p) in contour.iter().enumerate() {
            let next = contour[(i + 1) % contour.len()];
            points.push(p);
            if !p.on && !next.on {
                points.push(Point {
                    x: (p.x + next.x) / 2.0,
                    y: (p.y + next.y) / 2.0,
                    on: true,
                });
            }
        }
        let Some(start) = points.iter().position(|p| p.on) else {
            continue;
        };
        points.rotate_left(start);
        points.push(points[0]);
        let mut prev = (points[0].x, points[0].y);
        let mut i = 1;
        while i < points.len() {
            let p = points[i];
            if p.on {
                segment(prev, (p.x, p.y));
                prev = (p.x, p.y);
                i += 1;
                continue;
            }
            let end = points[i + 1];
            let mut last = prev;
            for step in 1..=32 {
                let t = step as f64 / 32.0;
                let u = 1.0 - t;
                let q = (
                    u * u * prev.0 + 2.0 * u * t * p.x + t * t * end.x,
                    u * u * prev.1 + 2.0 * u * t * p.y + t * t * end.y,
                );
                segment(last, q);
                last = q;
            }
            prev = (end.x, end.y);
            i += 2;
        }
    }
    xs.sort_by(f64::total_cmp);
    xs
}

/// The width of the first stem crossed by the horizontal line at y.
fn stem_width(contours: &Contours, y: f64) -> f64 {
    let xs = crossings(contours, y);
    xs[1] - xs[0]
}

#[test]
fn maps_the_char_codes_and_uses_the_expected_widths() {
    let requests = [
        request("A", Some(667.0), Some(0x21)),
        request("B", Some(500.0), Some(0x22)),
        request(" ", Some(278.0), None),
        request("\u{e000}", Some(300.0), Some(0x23)),
    ];
    let generated = generate_font(Family::Sans, &requests, &Options::default());
    assert_eq!(generated.missing, [3]);
    let font = read(&generated.data);
    assert_eq!(font.glyphs.len(), 5);
    assert_eq!(
        font.cmap,
        BTreeMap::from([(0x20, 3), (0x21, 1), (0x22, 2), (0x23, 4)])
    );
    let widths = [667.0, 500.0, 278.0, 300.0].map(|width| units(Family::Sans, width) as u16);
    assert_eq!(font.advances[1..], widths);
    assert!(font.glyphs[4].is_empty());

    // "B" is compressed to fit in 500/1000 em instead of 667/1000, its stem
    // keeping its width: the scale doesn't apply to it.
    let sans = glyph_data(Family::Sans);
    let natural_advance = sans.advance(sans.glyph_id(0x42).unwrap()) as f64;
    assert_eq!(natural_advance, units(Family::Sans, 667.0));
    let natural_glyph = &font_of(&["B"], &Options::default()).glyphs[1];
    let (b, natural) = (bounds(&font.glyphs[2]), bounds(natural_glyph));
    let stem = sans.stem_v as f64;
    let scale = (units(Family::Sans, 500.0) - stem) / (natural_advance - stem);
    assert!((b.x_max - (natural.x_max * scale + stem * (1.0 - scale))).abs() <= 1.0);
    assert_eq!(b.y_max, natural.y_max);
    // The compensation is the one of the stems of the font (measured on "l",
    // "I" and "H"), which are close to the one of "B" (scaled, it would be
    // 29% thinner).
    let y = natural.y_max / 4.0;
    let (b_stem, natural_stem) = (stem_width(&font.glyphs[2], y), stem_width(natural_glyph, y));
    assert!((b_stem - natural_stem).abs() <= 0.05 * natural_stem);
}

#[test]
fn gives_the_scale_of_the_font_to_the_stems() {
    // A condensed font: its glyphs at 80% of their widths, but "H" at 90%.
    let sans = glyph_data(Family::Sans);
    let natural = |c: char| {
        let advance = sans.advance(sans.glyph_id(c as u32).unwrap()) as f64;
        advance * 1000.0 / sans.units_per_em as f64
    };
    let requests = |options: &Options| {
        let requests: Vec<Request> = ["n", "o", "e", "H"]
            .iter()
            .map(|&text| {
                let c = text.chars().next().unwrap();
                let scale = if c == 'H' { 0.9 } else { 0.8 };
                request(text, Some(natural(c) * scale), None)
            })
            .collect();
        read(&generate_font(Family::Sans, &requests, options).data)
    };
    let y = sans.x_height as f64 / 2.0;
    let stem_of = |font: &Font, i: usize| stem_width(&font.glyphs[i], y);
    let natural_font = font_of(&["n", "H"], &Options::default());
    let condensed = requests(&Options::default());
    // The stems of "n" are scaled with it, like the ones of "H", whose own
    // scale would make them 12% thicker.
    let n_stem = stem_of(&natural_font, 1) * 0.8;
    assert!((stem_of(&condensed, 1) - n_stem).abs() <= 1.0);
    // The compensation is the one of the stems of the font (those of "l"),
    // which are 11 units thinner than the ones of "H".
    let h_stem = stem_of(&natural_font, 2) * 0.8;
    assert!((stem_of(&condensed, 4) - h_stem).abs() <= 3.0);

    // The expected stems are the ones of all the glyphs, whatever the scale.
    let stem_v = 100.0;
    let font = requests(&Options {
        stem_v: Some(stem_v),
        ..Options::default()
    });
    let strength = units(Family::Sans, stem_v) - sans.stem_v as f64;
    let n_stem = stem_of(&natural_font, 1) + strength;
    assert!((stem_of(&font, 1) - n_stem).abs() <= 2.0);
    let h_stem = stem_of(&natural_font, 2) + strength;
    assert!((stem_of(&font, 4) - h_stem).abs() <= 3.0);
}

#[test]
fn uses_the_units_per_em_of_the_widths() {
    let options = Options {
        units_per_em: 2000.0,
        ..Options::default()
    };
    let generated = generate_font(Family::Sans, &[request("A", Some(1334.0), None)], &options);
    assert_eq!(
        read(&generated.data).advances[1] as f64,
        units(Family::Sans, 667.0)
    );
}

#[test]
fn centers_a_glyph_which_cant_be_scaled_enough() {
    let requests = [request("i", Some(2000.0), None)];
    let font = read(&generate_font(Family::Sans, &requests, &Options::default()).data);
    let advance = units(Family::Sans, 2000.0);
    assert_eq!(font.advances[1] as f64, advance);
    let b = bounds(&font.glyphs[1]);
    assert!((b.x_min - (advance - b.x_max)).abs() <= 2.0);
}

#[test]
fn uses_the_bold_style_and_synthesizes_the_other_weights() {
    let (sans, sans_bold) = (
        glyph_data(Family::Sans),
        style_glyph_data(Family::Sans, true, false),
    );
    let (stem, bold_stem) = (sans.stem_v as f64, sans_bold.stem_v as f64);
    assert_eq!(sans.reference_stem_v, sans_bold.stem_v);
    let y = sans.x_height as f64 / 2.0;
    let l = |options: Options| font_of(&["l"], &options);
    let weight = |weight| {
        l(Options {
            weight: Some(weight),
            ..Options::default()
        })
    };
    let (regular, bold) = (
        l(Options::default()),
        l(Options {
            bold: true,
            ..Options::default()
        }),
    );
    let stem_of = |font: &Font| stem_width(&font.glyphs[1], y);
    // The glyphs of the bold style.
    assert_eq!(stem_of(&regular).round(), stem);
    assert_eq!(stem_of(&bold).round(), bold_stem);
    let l_gid = sans_bold.glyph_id('l' as u32).unwrap();
    assert_eq!(bold.advances[1], sans_bold.advance(l_gid));
    assert_eq!(bold.weight_class, 700);
    // The other weights, from the closest style: the stems are linear in the
    // weight, and the natural advance grows like the stem.
    let step = (bold_stem - stem) / 3.0;
    assert!((stem_of(&weight(300.0)) - (stem - step)).abs() <= 2.0);
    assert!((stem_of(&weight(600.0)) - (bold_stem - step)).abs() <= 2.0);
    let black = weight(900.0);
    assert!((stem_of(&black) - (bold_stem + 2.0 * step)).abs() <= 2.0);
    assert!((black.advances[1] as f64 - bold.advances[1] as f64 - 2.0 * step).abs() <= 1.0);
    assert_eq!(black.weight_class, 900);
    // An expected stem of 120/1000 em.
    let stem_120 = l(Options {
        stem_v: Some(120.0),
        ..Options::default()
    });
    assert!((stem_of(&stem_120) - units(Family::Sans, 120.0)).abs() <= 2.0);
}

#[test]
fn uses_the_italic_style_of_the_serif_family() {
    let italic = style_glyph_data(Family::Serif, false, true);
    for options in [
        Options {
            italic: true,
            ..Options::default()
        },
        Options {
            italic_angle: Some(-10.0),
            ..Options::default()
        },
    ] {
        let requests = [request("g", None, None)];
        let font = read(&generate_font(Family::Serif, &requests, &options).data);
        assert_eq!(font.glyphs[1], contours_of(italic, 'g'));
        assert!((font.italic_angle + 16.33).abs() < 1e-3);
    }
}

#[test]
fn keeps_the_expected_width_when_emboldening() {
    let options = Options {
        bold: true,
        ..Options::default()
    };
    let generated = generate_font(Family::Sans, &[request("H", Some(722.0), None)], &options);
    let font = read(&generated.data);
    let advance = units(Family::Sans, 722.0);
    assert_eq!(font.advances[1] as f64, advance);
    assert!(bounds(&font.glyphs[1]).x_max < advance);
}

#[test]
fn slants_the_glyphs() {
    let upright = font_of(&["l"], &Options::default());
    let italic = font_of(
        &["l"],
        &Options {
            italic_angle: Some(-12.0),
            ..Options::default()
        },
    );
    assert_eq!(italic.italic_angle, -12.0);
    let (slanted, reference) = (bounds(&italic.glyphs[1]), bounds(&upright.glyphs[1]));
    let shift = reference.y_max * 12f64.to_radians().tan();
    assert!((slanted.x_max - reference.x_max - shift).abs() <= 1.0);
    // The bottom of the glyph is on the baseline, so it doesn't move.
    assert_eq!(slanted.x_min, reference.x_min);
}

#[test]
fn uses_the_ligatures_and_composes_the_sequences() {
    let requests = [
        request("ffi", None, Some(1)),
        request("\u{fb03}", None, Some(2)),
        // Not in the glyph data: a sequence of a base and a combining mark.
        request("q\u{303}", None, Some(3)),
        // Not in the glyph data, decomposed with NFKD: "m2".
        request("\u{33a1}", None, Some(4)),
    ];
    let generated = generate_font(Family::Sans, &requests, &Options::default());
    assert!(generated.missing.is_empty());
    let font = read(&generated.data);
    let sans = glyph_data(Family::Sans);
    let gid = |cp| sans.glyph_id(cp).unwrap();
    let contours = |cp| sans.outline(gid(cp)).end_pts.len();
    assert_eq!(font.glyphs[1], font.glyphs[2]);
    assert_eq!(font.advances[1], sans.advance(gid(0xfb03)));
    assert_eq!(font.glyphs[3].len(), contours(0x71) + contours(0x303));
    assert!(!sans.has_codepoint(0x33a1));
    assert_eq!(font.glyphs[4].len(), contours(0x6d) + contours(0x32));
}

#[test]
fn falls_back_to_the_symbols() {
    let sans = glyph_data(Family::Sans);
    assert!(!sans.has_codepoint(0x2605) && !sans.has_codepoint(0x2713));
    let requests = [
        request("A", None, None),
        request("★", None, None),
        request("✓", None, None),
    ];
    let generated = generate_font(Family::Sans, &requests, &Options::default());
    assert!(generated.missing.is_empty());
    let font = read(&generated.data);
    assert!(font.glyphs[1..].iter().all(|glyph| !glyph.is_empty()));
    let copyright = &symbols_glyph_data().names.copyright;
    assert!(copyright.contains("Google LLC") && copyright.contains("Noto Project Authors"));
}

#[test]
fn synthesizes_the_style_of_the_symbols() {
    // A vertical stroke, from the symbols.
    let divides = "\u{2223}";
    assert!(!glyph_data(Family::Sans).has_codepoint(0x2223));
    let (sans, sans_bold) = (
        glyph_data(Family::Sans),
        style_glyph_data(Family::Sans, true, false),
    );
    let y = sans.cap_height as f64 / 2.0;
    let stem_of = |options: &Options| stem_width(&font_of(&[divides], options).glyphs[1], y);
    let regular = stem_of(&Options::default());
    let bold = stem_of(&Options {
        bold: true,
        ..Options::default()
    });
    // Emboldened like the family (from its regular style to its bold one).
    let bold_strength = (sans_bold.stem_v - sans.stem_v) as f64;
    assert!((bold - regular - bold_strength).abs() <= 4.0);
}

/// With density-based emboldening disabled, a sequence matches its separately
/// synthesized parts, including their summed advances (within rounding).
#[test]
fn synthesizes_each_part_of_a_sequence_with_its_style() {
    // The bold italic serif glyphs only need the symbols to be synthesized
    // (emboldened, slanted and scaled to the cap height), and with a weight of
    // 900 all the glyphs are emboldened.
    assert!(!style_glyph_data(Family::Serif, true, true).has_codepoint(0x2605));
    assert!(!glyph_data(Family::Sans).has_codepoint(0x33a1));
    let options = |bold, italic, weight| Options {
        bold,
        italic,
        weight,
        // The emboldening of the glyphs doesn't depend on their density.
        adaptive_bold: false,
        ..Options::default()
    };
    let bold_italic = options(true, true, None);
    let black = options(false, false, Some(900.0));
    let cases = [
        (Family::Serif, &bold_italic, "A★", ["A", "★"]),
        (Family::Serif, &bold_italic, "★A", ["★", "A"]),
        // Decomposed with NFKD: "m2".
        (Family::Sans, &black, "\u{33a1}", ["m", "2"]),
    ];
    for (family, options, text, glyphs) in cases {
        let requests = [glyphs[0], glyphs[1], text].map(|text| request(text, None, None));
        let generated = generate_font(family, &requests, options);
        assert!(generated.missing.is_empty());
        let font = read(&generated.data);
        let (first, second, sequence) = (&font.glyphs[1], &font.glyphs[2], &font.glyphs[3]);
        assert_eq!(sequence.len(), first.len() + second.len(), "{text}");
        // The advances are rounded.
        let advances = font.advances[1] + font.advances[2];
        assert!(font.advances[3].abs_diff(advances) <= 1, "{text}");
        let after_first = font.advances[1] as f64;
        let (first_part, second_part) = sequence.split_at(first.len());
        for (part, glyph, dx) in [(first_part, first, 0.0), (second_part, second, after_first)] {
            for (p, q) in part.iter().flatten().zip(glyph.iter().flatten()) {
                let close = (p.x - q.x - dx).abs() <= 1.0 && (p.y - q.y).abs() <= 1.0;
                assert!(close, "{text}: {p:?} and {q:?}");
            }
        }
    }
}

#[test]
fn supports_a_lot_of_scattered_codes() {
    let requests: Vec<Request> = (0..10000)
        .map(|i| request("A", None, Some(3 * i)))
        .collect();
    let font = read(&generate_font(Family::Sans, &requests, &Options::default()).data);
    assert_eq!(font.cmap.len(), 10000);
    assert_eq!(font.cmap[&0], 1);
    assert_eq!(font.cmap[&29997], 10000);
}

#[test]
fn supports_supplementary_codes() {
    let requests = [request("A", None, Some(0x1f600)), request("B", None, None)];
    let font = read(&generate_font(Family::Sans, &requests, &Options::default()).data);
    assert_eq!(font.cmap, BTreeMap::from([(0x42, 2), (0x1f600, 1)]));
}

#[test]
fn generates_fonts_without_outlines() {
    // Natural widths (emboldened) and given ones, some of them equal, and
    // dense glyphs with natural widths (their emboldening depends on their
    // outlines, see `density_factor`).
    let mut requests: Vec<Request> = "Hamburgefonstiv ffi"
        .chars()
        .enumerate()
        .map(|(i, c)| request(&c.to_string(), (i % 3 != 0).then_some(500.0), None))
        .collect();
    requests.extend(
        "@%&WMm‰"
            .chars()
            .map(|c| request(&c.to_string(), None, None)),
    );
    // The bold style, and a weight emboldening it.
    for options in [
        Options {
            bold: true,
            ..Options::default()
        },
        Options {
            weight: Some(900.0),
            ..Options::default()
        },
    ] {
        let full = generate_font(Family::Serif, &requests, &options);
        let generated = generate_font(
            Family::Serif,
            &requests,
            &Options {
                metrics_only: true,
                ..options
            },
        );
        assert_eq!(generated.missing, full.missing);
        let (font, full_font) = (read(&generated.data), read(&full.data));
        assert!(font.glyphs.iter().all(|glyph| glyph.is_empty()));
        // The same advances, with a glyph per advance (and .notdef).
        let advance = |font: &Font, code| font.advances[font.cmap[&code] as usize];
        assert_eq!(
            font.cmap.keys().collect::<Vec<_>>(),
            full_font.cmap.keys().collect::<Vec<_>>()
        );
        for &code in full_font.cmap.keys() {
            assert_eq!(advance(&font, code), advance(&full_font, code), "{code}");
        }
        let mut advances: Vec<u16> = (full_font.cmap.values())
            .map(|&gid| full_font.advances[gid as usize])
            .collect();
        advances.sort_unstable();
        advances.dedup();
        assert_eq!(font.glyphs.len(), advances.len() + 1);
        assert!(generated.data.len() < full.data.len() / 4);
    }
}

#[test]
fn uses_the_given_ascent() {
    let options = Options {
        metrics_only: true,
        ascent: Some(0.8),
        ..Options::default()
    };
    let generated = generate_font(Family::Sans, &[request("A", Some(600.0), None)], &options);
    let font = FontRef::new(&generated.data).unwrap();
    let hhea = font.hhea().unwrap();
    let os2 = font.os2().unwrap();
    // An em high, without line gap.
    let upm = glyph_data(Family::Sans).units_per_em as i16;
    let ascender = units(Family::Sans, 800.0) as i16;
    let descender = ascender - upm;
    assert_eq!(
        (hhea.ascender().to_i16(), hhea.descender().to_i16()),
        (ascender, descender)
    );
    assert_eq!(hhea.line_gap().to_i16(), 0);
    assert_eq!(
        (
            os2.s_typo_ascender(),
            os2.s_typo_descender(),
            os2.s_typo_line_gap()
        ),
        (ascender, descender, 0)
    );
    assert_eq!(
        (os2.us_win_ascent(), os2.us_win_descent()),
        (ascender as u16, -descender as u16)
    );
}

#[test]
fn draws_debug_boxes() {
    let requests = [
        request("A", Some(600.0), None),
        request("B", Some(600.0), None),
        request("i", None, None),
        // Narrower than two strokes.
        request(".", Some(50.0), None),
        request("\u{301}", Some(0.0), None),
    ];
    let upm = glyph_data(Family::Sans).units_per_em as f64;
    let stroke = (upm / 32.0).round();
    let point = |x, y| Point { x, y, on: true };
    for metrics_only in [false, true] {
        let options = Options {
            metrics_only,
            ascent: Some(0.8),
            debug_boxes: true,
            ..Options::default()
        };
        let font = read(&generate_font(Family::Sans, &requests, &options).data);
        let (top, bottom) = (units(Family::Sans, 800.0), units(Family::Sans, 800.0) - upm);
        let gid = |c: char| font.cmap[&(c as u32)] as usize;
        let filled = |right| {
            vec![
                point(0.0, bottom),
                point(0.0, top),
                point(right, top),
                point(right, bottom),
            ]
        };
        assert!(font.glyphs[0].is_empty(), ".notdef");
        for c in ['A', 'B', 'i'] {
            let right = font.advances[gid(c)] as f64;
            let (left, right_inside) = (stroke, right - stroke);
            let (bottom, top) = (bottom + stroke, top - stroke);
            assert_eq!(
                font.glyphs[gid(c)],
                vec![
                    filled(right),
                    vec![
                        point(left, bottom),
                        point(right_inside, bottom),
                        point(right_inside, top),
                        point(left, top)
                    ]
                ],
                "{c}, metrics only: {metrics_only}"
            );
        }
        let right = font.advances[gid('.')] as f64;
        assert!(right < 2.0 * stroke);
        assert_eq!(font.glyphs[gid('.')], vec![filled(right)]);
        // Nothing to draw without advance.
        assert!(font.glyphs[gid('\u{301}')].is_empty());
        assert_eq!(gid('A') == gid('B'), metrics_only);
    }
}

/// The pairs of glyph ids of the `kern` table of a font, with their kerning.
fn kern_pairs(data: &[u8]) -> BTreeMap<(u16, u16), i16> {
    let font = FontRef::new(data).unwrap();
    let Some(table) = font.table_data(Tag::new(b"kern")) else {
        return BTreeMap::new();
    };
    let table = table.as_bytes();
    let u16_at = |pos: usize| u16::from_be_bytes([table[pos], table[pos + 1]]);
    assert_eq!((u16_at(0), u16_at(2)), (0, 1), "kern version and nTables");
    assert_eq!(u16_at(8), 1, "coverage: horizontal, format 0");
    let n = u16_at(10) as usize;
    assert_eq!(u16_at(6) as usize, 14 + 6 * n, "subtable length");
    let power = 1 << (usize::BITS - 1 - n.leading_zeros());
    assert_eq!(u16_at(12) as usize, 6 * power, "searchRange");
    assert_eq!(1 << u16_at(14), power, "entrySelector");
    assert_eq!(u16_at(16) as usize, 6 * (n - power), "rangeShift");
    let pairs: Vec<((u16, u16), i16)> = (0..n)
        .map(|i| {
            let pos = 18 + 6 * i;
            let value = i16::from_be_bytes([table[pos + 4], table[pos + 5]]);
            ((u16_at(pos), u16_at(pos + 2)), value)
        })
        .collect();
    assert!(pairs.windows(2).all(|w| w[0].0 < w[1].0), "sorted pairs");
    pairs.into_iter().collect()
}

#[test]
fn kerns_the_pairs_of_glyphs() {
    let requests = [
        request("W", Some(944.0), None),
        request("a", Some(444.0), None),
        request("e", Some(444.0), None),
        request("c", Some(444.0), None),
        // A code already mapped: its kerning is ignored.
        request("x", Some(500.0), Some('a' as u32)),
    ];
    let kerning = vec![
        Kerning {
            left: 0,
            right: 1,
            value: -80.0,
        },
        Kerning {
            left: 1,
            right: 0,
            value: 40.4,
        },
        // Too small, the first one of a pair, a glyph which isn't mapped.
        Kerning {
            left: 2,
            right: 3,
            value: 0.2,
        },
        Kerning {
            left: 0,
            right: 1,
            value: -30.0,
        },
        Kerning {
            left: 4,
            right: 0,
            value: -50.0,
        },
    ];
    for metrics_only in [false, true] {
        let options = Options {
            metrics_only,
            kerning: kerning.clone(),
            ..Options::default()
        };
        let data = generate_font(Family::Serif, &requests, &options).data;
        let font = read(&data);
        let gid = |c: char| font.cmap[&(c as u32)] as u16;
        let value = |value: f64| units(Family::Serif, value) as i16;
        assert_eq!(
            kern_pairs(&data),
            BTreeMap::from([
                ((gid('W'), gid('a')), value(-80.0)),
                ((gid('a'), gid('W')), value(40.4))
            ]),
            "metrics only: {metrics_only}"
        );
        // Without outlines, the kerned glyphs have their own glyph ids.
        assert_ne!(gid('a'), gid('e'));
        assert_eq!(gid('e') == gid('c'), metrics_only);
    }
    // No table without kerning.
    let data = generate_font(Family::Serif, &requests, &Options::default()).data;
    assert!(kern_pairs(&data).is_empty());
}

/// The font of the requests for some texts with a standard font, at their
/// natural widths.
fn standard_font_of(font: StandardFont, texts: &[&str]) -> Font {
    let requests: Vec<Request> = texts.iter().map(|t| request(t, None, None)).collect();
    read(&generate_standard_font(font, &requests, &Options::default()).data)
}

/// The contours of the glyph of a codepoint in glyph data.
fn contours_of(data: &GlyphData, cp: char) -> Contours {
    let outline = data.outline(data.glyph_id(cp as u32).unwrap());
    let mut start = 0;
    outline
        .end_pts
        .iter()
        .map(|&end| {
            let contour = (start..=end)
                .map(|i| Point {
                    x: outline.xs[i],
                    y: outline.ys[i],
                    on: outline.on_curve[i] != 0,
                })
                .collect();
            start = end + 1;
            contour
        })
        .collect()
}

#[test]
fn names_the_standard_fonts() {
    for font in StandardFont::ALL {
        assert_eq!(StandardFont::from_name(font.name()), Some(font));
    }
    assert_eq!(StandardFont::from_name("Arial"), None);
    assert_eq!(
        StandardFont::HelveticaBoldOblique.name(),
        "Helvetica-BoldOblique"
    );
}

#[test]
fn keeps_the_glyphs_of_the_standard_fonts() {
    for (font, c, weight, italic_angle) in [
        (StandardFont::HelveticaBold, 'A', 700, 0.0),
        (StandardFont::TimesItalic, 'g', 400, -16.33),
        (StandardFont::CourierBold, '{', 700, 0.0),
        (StandardFont::Symbol, '∑', 400, 0.0),
        (StandardFont::ZapfDingbats, '✓', 400, 0.0),
        (StandardFont::Wingdings, '🖂', 400, 0.0),
    ] {
        let data = standard_glyph_data(font);
        let generated = standard_font_of(font, &[&c.to_string()]);
        assert_eq!(generated.glyphs[1], contours_of(data, c), "{font:?}");
        let gid = data.glyph_id(c as u32).unwrap();
        assert_eq!(generated.advances[1], data.advance(gid));
        assert_eq!(generated.weight_class, weight);
        assert!((generated.italic_angle - italic_angle).abs() < 1e-3);
    }
    // Helvetica A at 667/1000 em retains the embedded Arimo outline.
    let requests = [request("A", Some(667.0), None)];
    let generated = generate_standard_font(StandardFont::Helvetica, &requests, &Options::default());
    let font = read(&generated.data);
    assert_eq!(font.advances[1], 1366);
    let data = standard_glyph_data(StandardFont::Helvetica);
    assert_eq!(font.glyphs[1], contours_of(data, 'A'));
}

#[test]
fn slants_the_oblique_standard_fonts() {
    let helvetica = standard_glyph_data(StandardFont::Helvetica);
    let upright = standard_font_of(StandardFont::Helvetica, &["l"]);
    let oblique = standard_font_of(StandardFont::HelveticaOblique, &["l"]);
    assert_eq!(oblique.italic_angle, -12.0);
    assert_eq!(oblique.advances, upright.advances);
    let (slanted, reference) = (bounds(&oblique.glyphs[1]), bounds(&upright.glyphs[1]));
    let shift = reference.y_max * 12f64.to_radians().tan();
    assert!((slanted.x_max - reference.x_max - shift).abs() <= 1.0);
    assert_eq!(slanted.x_min, reference.x_min);
    // The bold oblique has the bold glyphs.
    let bold = standard_font_of(StandardFont::HelveticaBoldOblique, &["l"]);
    // A shear keeps the horizontal widths.
    let y = helvetica.x_height as f64 / 2.0;
    let stem = |font: &Font| stem_width(&font.glyphs[1], y);
    let bold_data = standard_glyph_data(StandardFont::HelveticaBold);
    assert!((stem(&bold) - bold_data.stem_v as f64).abs() <= 1.0);
    assert!((stem(&oblique) - stem(&upright)).abs() <= 1.0);
}

#[test]
fn falls_back_to_the_symbols_in_the_standard_fonts() {
    let helvetica = standard_glyph_data(StandardFont::Helvetica);
    // A star and a vertical stroke.
    assert!(!helvetica.has_codepoint(0x2605) && !helvetica.has_codepoint(0x2223));
    let texts = ["H", "★", "\u{2223}"];
    let requests: Vec<Request> = texts.iter().map(|t| request(t, None, None)).collect();
    let generated = generate_standard_font(StandardFont::Helvetica, &requests, &Options::default());
    assert!(generated.missing.is_empty());
    let regular = read(&generated.data);
    let bold = standard_font_of(StandardFont::HelveticaBold, &texts);
    assert!(regular.glyphs[1..].iter().all(|glyph| !glyph.is_empty()));
    // The symbols are emboldened like the family.
    let y = helvetica.cap_height as f64 / 2.0;
    let stem = |font: &Font| stem_width(&font.glyphs[3], y);
    let bold_data = standard_glyph_data(StandardFont::HelveticaBold);
    let bold_strength = (bold_data.stem_v - helvetica.stem_v) as f64;
    assert!((stem(&bold) - stem(&regular) - bold_strength).abs() <= 4.0);
}

#[test]
fn finds_the_missing_glyphs_of_the_standard_fonts() {
    let texts: [&[u32]; 5] = [&[0x41], &[0xe000], &[0x2605], &[0x41, 0x301], &[]];
    assert_eq!(
        missing_standard_glyphs(StandardFont::Helvetica, &texts),
        vec![1, 4]
    );
    // The glyphs of Symbol, as pdf.js maps them (e.g. braceex to U+23AA).
    let symbols: [&[u32]; 4] = [&[0x3b1], &[0x23aa], &[0x203e], &[0x2329]];
    assert!(missing_standard_glyphs(StandardFont::Symbol, &symbols).is_empty());
    let dingbats: [&[u32]; 3] = [&[0x2701], &[0x2460], &[0x27be]];
    assert!(missing_standard_glyphs(StandardFont::ZapfDingbats, &dingbats).is_empty());
    // The square bullet, the check box and the envelope of Wingdings, its
    // arrow-head bullet (Wingdings 3) and its ballot box with an X
    // (Wingdings 2).
    let wingdings: [&[u32]; 5] = [&[0x25aa], &[0x2611], &[0x1f582], &[0x2b9a], &[0x2612]];
    assert!(missing_standard_glyphs(StandardFont::Wingdings, &wingdings).is_empty());
}

/// Approximate curve bounds [x_min, y_min, x_max, y_max], sampling each
/// quadratic segment at 65 evenly spaced parameter values.
fn outline_bounds(contours: &Contours) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut add = |x: f64, y: f64| b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    for contour in contours {
        let n = contour.len();
        for (i, &p) in contour.iter().enumerate() {
            if p.on {
                add(p.x, p.y);
                continue;
            }
            // The curve of an off-curve point (flattened), between the
            // on-curve points around it (implied between two off-curve ones).
            let end = |q: Point| {
                if q.on {
                    (q.x, q.y)
                } else {
                    ((p.x + q.x) / 2.0, (p.y + q.y) / 2.0)
                }
            };
            let (a, c) = (end(contour[(i + n - 1) % n]), end(contour[(i + 1) % n]));
            for step in 0..=64 {
                let t = step as f64 / 64.0;
                let u = 1.0 - t;
                add(
                    u * u * a.0 + 2.0 * u * t * p.x + t * t * c.0,
                    u * u * a.1 + 2.0 * u * t * p.y + t * t * c.1,
                );
            }
        }
    }
    b
}

/// Generates a standard font with the supplied widths in 1/1000 em.
/// Also returns the font units per 1/1000 em.
fn font_with_widths(font: StandardFont, glyphs: &[(char, f64)]) -> (Font, f64) {
    let requests: Vec<Request> = glyphs
        .iter()
        .map(|&(c, width)| request(&c.to_string(), Some(width), None))
        .collect();
    let generated = generate_standard_font(font, &requests, &Options::default());
    assert!(generated.missing.is_empty());
    let units = standard_glyph_data(font).units_per_em as f64 / 1000.0;
    (read(&generated.data), units)
}

/// How a glyph of Symbol or ZapfDingbats is on its box in the AFM file.
#[derive(Clone, Copy, Debug)]
enum OnBox {
    /// All of it.
    Exactly,
    /// Its bottom and top (scaled to its height, keeping its proportions).
    Height,
    /// Its vertical center (fitting in its width).
    Centered,
}

#[test]
fn puts_the_glyphs_of_symbol_and_zapfdingbats_on_the_boxes_of_adobe() {
    use OnBox::*;
    // Glyph, width and box in the AFM files of Adobe.
    for (font, c, width, afm_box, on_box) in [
        // Tinos's summation and product are about 100/1000 em below these boxes.
        (
            StandardFont::Symbol,
            '∑',
            713.0,
            [14.0, -108.0, 695.0, 752.0],
            Height,
        ),
        (
            StandardFont::Symbol,
            '∏',
            823.0,
            [25.0, -101.0, 803.0, 751.0],
            Height,
        ),
        (
            StandardFont::Symbol,
            '+',
            549.0,
            [10.0, 0.0, 539.0, 533.0],
            Height,
        ),
        // The radical, and its extension past its advance, at its top.
        (
            StandardFont::Symbol,
            '√',
            549.0,
            [10.0, -38.0, 515.0, 917.0],
            Exactly,
        ),
        (
            StandardFont::Symbol,
            '\u{203e}',
            500.0,
            [480.0, 881.0, 1090.0, 917.0],
            Exactly,
        ),
        // Check the AFM heights after swapping the source glyphs for phi and phi1.
        (
            StandardFont::Symbol,
            '\u{3c6}',
            521.0,
            [28.0, -224.0, 492.0, 673.0],
            Height,
        ),
        (
            StandardFont::Symbol,
            '\u{3d5}',
            603.0,
            [36.0, -224.0, 587.0, 499.0],
            Height,
        ),
        // Height fit preserves the baseline.
        (
            StandardFont::Symbol,
            '1',
            500.0,
            [117.0, 0.0, 390.0, 673.0],
            Height,
        ),
        // Width-limited arrows remain vertically centered.
        (
            StandardFont::Symbol,
            '←',
            987.0,
            [32.0, -15.0, 942.0, 511.0],
            Centered,
        ),
        (
            StandardFont::ZapfDingbats,
            '✁',
            974.0,
            [35.0, 72.0, 939.0, 621.0],
            Height,
        ),
        (
            StandardFont::ZapfDingbats,
            '→',
            838.0,
            [35.0, 152.0, 803.0, 540.0],
            Centered,
        ),
    ] {
        let (generated, units) = font_with_widths(font, &[(c, width)]);
        let [x_min, y_min, x_max, y_max] = outline_bounds(&generated.glyphs[1]);
        let expected = afm_box.map(|v| v * units);
        let near = |a: f64, b: f64| (a - b).abs() <= 2.0;
        let message = format!("{c} {:?}", [x_min, y_min, x_max, y_max].map(|v| v / units));
        let matches_box = match on_box {
            Exactly => {
                near(x_min, expected[0])
                    && near(y_min, expected[1])
                    && near(x_max, expected[2])
                    && near(y_max, expected[3])
            }
            Height => near(y_min, expected[1]) && near(y_max, expected[3]),
            Centered => {
                near(y_min + y_max, expected[1] + expected[3])
                    && y_min >= expected[1]
                    && y_max <= expected[3]
            }
        };
        assert!(matches_box, "{message}");
        // The widths of the AFM files are the natural ones.
        let data = standard_glyph_data(font);
        let gid = data.glyph_id(c as u32).unwrap();
        assert_eq!(data.advance(gid) as f64, (width * units).round(), "{c}");
    }
}

#[test]
fn joins_the_pieces_of_the_big_delimiters_of_symbol() {
    // Extension, AFM stem box, advance, and top/middle/bottom pieces with
    // their AFM boxes. Middle pieces must join at both ends.
    type Pieces<'a> = &'a [(char, [f64; 4])];
    let families: [(char, [f64; 4], f64, Pieces, Pieces, Pieces); 6] = [
        (
            '\u{239c}',
            [24.0, -85.0, 108.0, 925.0],
            384.0,
            &[('\u{239b}', [24.0, -293.0, 436.0, 926.0])],
            &[],
            &[('\u{239d}', [24.0, -293.0, 436.0, 926.0])],
        ),
        (
            '\u{239f}',
            [382.0, -85.0, 466.0, 925.0],
            384.0,
            &[('\u{239e}', [54.0, -293.0, 466.0, 926.0])],
            &[],
            &[('\u{23a0}', [54.0, -293.0, 466.0, 926.0])],
        ),
        (
            '\u{23a2}',
            [0.0, -79.0, 77.0, 925.0],
            384.0,
            &[('\u{23a1}', [0.0, -80.0, 349.0, 926.0])],
            &[],
            &[('\u{23a3}', [0.0, -80.0, 349.0, 926.0])],
        ),
        (
            '\u{23a5}',
            [294.0, -79.0, 371.0, 925.0],
            384.0,
            &[('\u{23a4}', [22.0, -80.0, 371.0, 926.0])],
            &[],
            &[('\u{23a6}', [22.0, -80.0, 371.0, 926.0])],
        ),
        // The braces: the same stem on both sides.
        (
            '\u{23aa}',
            [209.0, -85.0, 284.0, 935.0],
            494.0,
            &[
                ('\u{23a7}', [209.0, -85.0, 445.0, 925.0]),
                ('\u{23ab}', [48.0, -85.0, 284.0, 925.0]),
            ],
            &[
                ('\u{23a8}', [20.0, -85.0, 284.0, 935.0]),
                ('\u{23ac}', [209.0, -85.0, 473.0, 935.0]),
            ],
            &[
                ('\u{23a9}', [209.0, -75.0, 445.0, 935.0]),
                ('\u{23ad}', [48.0, -75.0, 284.0, 935.0]),
            ],
        ),
        // In Noto Sans Math, the halves of the integral aren't aligned with its
        // extension, so the encoder measures the stem of each piece.
        (
            '\u{23ae}',
            [308.0, -88.0, 378.0, 975.0],
            686.0,
            &[('\u{2320}', [308.0, -88.0, 675.0, 920.0])],
            &[],
            &[('\u{2321}', [11.0, -87.0, 378.0, 921.0])],
        ),
    ];
    for (extension, stem, width, tops, middles, bottoms) in families {
        let mut pieces = vec![(extension, stem, true, true)];
        pieces.extend(tops.iter().map(|&(c, b)| (c, b, true, false)));
        pieces.extend(middles.iter().map(|&(c, b)| (c, b, true, true)));
        pieces.extend(bottoms.iter().map(|&(c, b)| (c, b, false, true)));
        let glyphs: Vec<(char, f64)> = pieces.iter().map(|&(c, ..)| (c, width)).collect();
        let (generated, units) = font_with_widths(StandardFont::Symbol, &glyphs);
        let near = |a: f64, b: f64| (a - b).abs() <= 2.0;
        for (i, &(c, afm_box, at_bottom, at_top)) in pieces.iter().enumerate() {
            let contours = &generated.glyphs[i + 1];
            let [_, y_min, _, y_max] = outline_bounds(contours);
            assert!(
                near(y_min, afm_box[1] * units) && near(y_max, afm_box[3] * units),
                "{c}"
            );
            // Compare stems 15/1000 em inside each joining edge.
            for (joins, y) in [(at_bottom, afm_box[1] + 15.0), (at_top, afm_box[3] - 15.0)] {
                if !joins {
                    continue;
                }
                let xs = crossings(contours, y * units);
                let (x0, x1) = (xs[0], xs[xs.len() - 1]);
                assert!(
                    near(x0, stem[0] * units) && near(x1, stem[2] * units),
                    "{c} at {y}: {:.0}-{:.0}",
                    x0 / units,
                    x1 / units
                );
            }
        }
    }
}
