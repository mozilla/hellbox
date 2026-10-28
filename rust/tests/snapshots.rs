// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Snapshot tests: the decoded glyph data and some generated fonts must be the
//! same as the ones in `tests/snapshots/` (their lengths and hashes).
//!
//! To update the snapshots, after checking the changes (e.g. with the
//! reference tests):
//!
//! ```sh
//! UPDATE_SNAPSHOTS=1 cargo test --release --test snapshots
//! ```
//!
//! With `HELLBOX_OUTPUT=<dir>`, the fonts are written in `<dir>`.

use hellbox::{
    generate_font, generate_standard_font, standard_glyph_data, style_glyph_data,
    symbols_glyph_data, Family, GlyphData, Options, Request, StandardFont,
};
use std::fs;
use std::path::Path;

/// FNV-1a (32 bits).
fn fnv(bytes: &[u8]) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for &b in bytes {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h
}

/// A canonical dump of the content of glyph data: its length and hash.
fn dump(data: &GlyphData) -> (usize, u32) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(data.units_per_em as u32).to_le_bytes());
    for v in [
        data.ascender as i32,
        data.descender as i32,
        data.line_gap as i32,
        data.cap_height as i32,
        data.x_height as i32,
        data.underline_position as i32,
        data.underline_thickness as i32,
        data.weight_class as i32,
        data.stem_v as i32,
        data.stem_h as i32,
        data.reference_stem_v as i32,
        data.reference_stem_h as i32,
        data.serif as i32,
        data.fixed_pitch as i32,
    ] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let names = &data.names;
    for name in [
        &names.family,
        &names.version,
        &names.copyright,
        &names.license,
        &names.license_url,
    ] {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
    }
    for cp in data.codepoints() {
        bytes.extend_from_slice(&cp.to_le_bytes());
        bytes.extend_from_slice(&(data.glyph_id(cp).unwrap() as u32).to_le_bytes());
    }
    for gid in 0..data.num_glyphs() {
        let outline = data.outline(gid);
        for v in [
            data.advance(gid) as u32,
            outline.xs.len() as u32,
            outline.end_pts.len() as u32,
        ] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for &end in &outline.end_pts {
            bytes.extend_from_slice(&(end as u32).to_le_bytes());
        }
        bytes.extend_from_slice(&outline.on_curve);
        for v in outline.xs.iter().chain(&outline.ys) {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    (bytes.len(), fnv(&bytes))
}

/// Compares the lines of a snapshot with `tests/snapshots/<name>`, or writes
/// them with `UPDATE_SNAPSHOTS=1`.
fn check_snapshot(name: &str, header: &str, lines: &[String]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name);
    let mut snapshot = format!("# {header} (see tests/snapshots.rs).\n");
    for line in lines {
        snapshot += line;
        snapshot += "\n";
    }
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, snapshot).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_default();
    if expected != snapshot {
        let changed = |a: &str, b: &str| -> Vec<String> {
            let b: Vec<&str> = b.lines().collect();
            a.lines()
                .filter(|line| !b.contains(line))
                .map(|line| format!("\n  {line}"))
                .collect()
        };
        panic!(
            "{} differs from the results.\nExpected:{}\nResults:{}\nRun with UPDATE_SNAPSHOTS=1 to update it.",
            path.display(),
            changed(&expected, &snapshot).concat(),
            changed(&snapshot, &expected).concat()
        );
    }
}

#[test]
fn decodes_the_glyph_data() {
    let mut sources = Vec::new();
    for (family, name) in [
        (Family::Sans, "sans"),
        (Family::Serif, "serif"),
        (Family::Mono, "mono"),
    ] {
        for (bold, italic, style) in [
            (false, false, "regular"),
            (true, false, "bold"),
            (false, true, "italic"),
            (true, true, "bold-italic"),
        ] {
            // Only the serif family has italic styles.
            if !italic || family == Family::Serif {
                sources.push((
                    format!("{name}-{style}"),
                    style_glyph_data(family, bold, italic),
                ));
            }
        }
    }
    sources.push(("symbols".to_string(), symbols_glyph_data()));
    for font in [
        StandardFont::Symbol,
        StandardFont::ZapfDingbats,
        StandardFont::Wingdings,
    ] {
        sources.push((font.name().to_string(), standard_glyph_data(font)));
    }
    let lines: Vec<String> = sources
        .into_iter()
        .map(|(name, data)| {
            let (length, hash) = dump(data);
            format!("{name} {length} {hash:08x}")
        })
        .collect();
    check_snapshot(
        "glyph_data.txt",
        "Glyph data: name, length and hash of its dump",
        &lines,
    );
}

/// The glyph data of a font.
#[derive(Clone, Copy)]
enum Source {
    Family(Family),
    Standard(StandardFont),
}

struct Case {
    name: &'static str,
    source: Source,
    options: Options,
    requests: Vec<Request>,
}

fn request(unicode: &[u32], width: Option<f64>, char_code: Option<u32>) -> Request {
    Request {
        unicode: unicode.to_vec(),
        width,
        char_code,
    }
}

/// The requests for the distinct characters of a text (in order, the spaces
/// included), the i-th one having the width `width(i)`.
fn text(text: &str, width: impl Fn(usize) -> Option<f64>) -> Vec<Request> {
    let mut chars: Vec<char> = Vec::new();
    for c in text.chars() {
        if !chars.contains(&c) {
            chars.push(c);
        }
    }
    chars
        .iter()
        .enumerate()
        .map(|(i, &c)| request(&[c as u32], width(i), None))
        .collect()
}

fn cases() -> Vec<Case> {
    let case = |name, family, options, requests| Case {
        name,
        source: Source::Family(family),
        options,
        requests,
    };
    let standard = |name, font, requests| Case {
        name,
        source: Source::Standard(font),
        options: Options::default(),
        requests,
    };
    let default = Options::default;
    let latin = "The quick brown fox jumps over the lazy dog 0123456789 ÀÉÎÕÜ";
    let widths = |i| Some(400.0 + (i % 7) as f64 * 50.0);
    let natural = |_| None;
    vec![
        case("sans", Family::Sans, default(), text(latin, widths)),
        case(
            "sans-bold",
            Family::Sans,
            Options {
                bold: true,
                ..default()
            },
            text(latin, widths),
        ),
        case(
            "sans-italic",
            Family::Sans,
            Options {
                italic: true,
                ..default()
            },
            text(latin, widths),
        ),
        case(
            "sans-options",
            Family::Sans,
            Options {
                weight: Some(900.0),
                italic_angle: Some(-20.0),
                cap_height: Some(650.0),
                ..default()
            },
            text(latin, widths),
        ),
        case(
            "sans-stem",
            Family::Sans,
            Options {
                stem_v: Some(120.0),
                ..default()
            },
            text(latin, natural),
        ),
        case(
            "serif",
            Family::Serif,
            Options {
                bold: true,
                italic: true,
                ..default()
            },
            text("Ægir, ŒUVRE, final — €19.99 Ωμέγα Привет ẞ", natural),
        ),
        case(
            "mono",
            Family::Mono,
            default(),
            text("fn main() { 42 }", |_| Some(600.0)),
        ),
        // The symbols come from the fallback glyph data.
        case(
            "symbols",
            Family::Sans,
            Options {
                bold: true,
                ..default()
            },
            text(
                "• ∑ ∞ ≤ ≥ ≠ √ ∫ ← → ⇒ ■ □ ● ○ ▲ ► ◆ ☐ ☑ ☒ ★ ☆ ♠ ♥ ✓ ✔ ✗ ✘ ❖ ➢ ① ② ┌─┐ ⌘ ⏎",
                natural,
            ),
        ),
        // Ligatures, normalization fallbacks, missing U+E000, empty text,
        // and supplementary or duplicate cmap codes.
        case(
            "texts",
            Family::Sans,
            default(),
            [
                (&[0x66, 0x66, 0x69][..], Some(830.0), 1),
                (&[0x66, 0x69], Some(560.0), 2),
                (&[0x65, 0x301], None, 3),
                (&[0x71, 0x303], None, 4),
                (&[0x33a1], None, 5),
                (&[0x1d400], None, 6),
                (&[0xff21], Some(700.0), 7),
                (&[0x212b], None, 8),
                (&[0xfb06], None, 9),
                (&[0xe000], Some(300.0), 10),
                (&[], Some(250.0), 11),
                (&[0x41], Some(2000.0), 0x1f600),
                (&[0x42], Some(10.0), 0x41),
                (&[0x43], None, 0x41),
            ]
            .iter()
            .map(|&(unicode, width, char_code)| request(unicode, width, Some(char_code)))
            .collect(),
        ),
        // Many glyphs with scattered char codes.
        case(
            "scattered",
            Family::Mono,
            Options {
                italic: true,
                ..default()
            },
            (0..9000)
                .map(|i| request(&[0x41], Some(600.0), Some(3 * i)))
                .collect(),
        ),
        // Bounded italic angle and advances.
        case(
            "extremes",
            Family::Sans,
            Options {
                bold: true,
                italic_angle: Some(-89.0),
                ..default()
            },
            [Some(0.0), Some(1.0), Some(1e5), Some(1e6), None]
                .iter()
                .zip(0x41..)
                .map(|(&width, cp)| request(&[cp], width, None))
                .collect(),
        ),
        case("empty", Family::Serif, default(), Vec::new()),
        // Without outlines (for a text layer).
        case(
            "sans-metrics-only",
            Family::Sans,
            Options {
                metrics_only: true,
                ..default()
            },
            text(latin, widths),
        ),
        // The standard fonts: their glyphs, slanted for the oblique ones, and
        // the Noto glyphs synthesized to their style.
        standard("helvetica", StandardFont::Helvetica, text(latin, widths)),
        standard(
            "helvetica-boldoblique",
            StandardFont::HelveticaBoldOblique,
            text(latin, natural),
        ),
        standard(
            "times-bolditalic",
            StandardFont::TimesBoldItalic,
            text("Ægir, ŒUVRE, final — €19.99 Ωμέγα Привет ẞ", natural),
        ),
        standard(
            "courier-oblique",
            StandardFont::CourierOblique,
            text("fn main() { 42 }", |_| Some(600.0)),
        ),
        standard(
            "times-bold-fallback",
            StandardFont::TimesBold,
            text("Ͱ ★ ✓ ⌘ A", natural),
        ),
        standard(
            "symbol",
            StandardFont::Symbol,
            text("αβγ ∑∏∫ ←→⇒ ♠♣♥♦ ⎛⎞⎪", widths),
        ),
        standard(
            "zapfdingbats",
            StandardFont::ZapfDingbats,
            text("✓✗❖➢①➔", natural),
        ),
        standard(
            "wingdings",
            StandardFont::Wingdings,
            text("▪➢✓☑☒🖂🕿⮚", widths),
        ),
    ]
}

#[test]
fn generates_the_fonts() {
    let output = std::env::var("HELLBOX_OUTPUT").ok();
    let lines: Vec<String> = cases()
        .iter()
        .map(|case| {
            let font = match case.source {
                Source::Family(family) => generate_font(family, &case.requests, &case.options),
                Source::Standard(font) => {
                    generate_standard_font(font, &case.requests, &case.options)
                }
            };
            if let Some(dir) = &output {
                fs::write(format!("{dir}/{}.ttf", case.name), &font.data).unwrap();
            }
            let mut line = format!("{} {} {:08x}", case.name, font.data.len(), fnv(&font.data));
            if !font.missing.is_empty() {
                let missing: Vec<String> = font.missing.iter().map(|i| i.to_string()).collect();
                line += &format!(" missing {}", missing.join(","));
            }
            line
        })
        .collect();
    check_snapshot(
        "fonts.txt",
        "Fonts: name, length and hash, and the indices of the missing glyphs",
        &lines,
    );
}
