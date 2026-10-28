// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Reference tests: the glyphs of some generated fonts (Noto and standard
//! fonts), read and drawn by
//! skrifa (the font scaler of Google's Fontations), are written in SVG files
//! which must be the same as the references in `tests/refs/`.
//!
//! The SVG files which differ from their references are written in
//! `target/tmp/reftest/`, with an `index.html` to compare them (hovering an
//! image shows its reference). To update the references, after reviewing the
//! differences:
//!
//! ```sh
//! UPDATE_REFS=1 cargo test --release --test reftest
//! ```

use hellbox::{
    generate_font, generate_standard_font, glyph_data, standard_glyph_data, Family, GeneratedFont,
    Options, Request, StandardFont,
};
use skrifa::font::glyph::outline::{ControlBoundsSink, SvgSink};
use skrifa::font::name::NameId;
use skrifa::{DrawMemory, Font, GlyphId, ScalerFont};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

/// The glyph data of a font.
#[derive(Clone, Copy)]
enum Source {
    Family(Family),
    Standard(StandardFont),
}

struct Case {
    name: String,
    source: Source,
    options: Options,
    glyphs: Vec<Request>,
}

fn request(unicode: &[u32], width: Option<f64>) -> Request {
    Request {
        unicode: unicode.to_vec(),
        width,
        char_code: None,
    }
}

/// The requests for the distinct characters of a text, at their natural
/// widths.
fn chars(text: &str) -> Vec<Request> {
    let mut chars: Vec<char> = Vec::new();
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        if !chars.contains(&c) {
            chars.push(c);
        }
    }
    chars.iter().map(|&c| request(&[c as u32], None)).collect()
}

/// The classic text to test a typeface, punctuation, digits, accents, Greek,
/// Cyrillic, a ligature and some symbols (from the fallback glyph data).
const SAMPLE: &str = "Hamburgefonstiv HOQ &?! 0123 ÀÉçñßął ΩαβЖя ﬁ ∑→■✓";

fn cases() -> Vec<Case> {
    let case = |name: &str, family, options, glyphs| Case {
        name: name.to_string(),
        source: Source::Family(family),
        options,
        glyphs,
    };
    let default = Options::default;
    let mut cases = Vec::new();
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
            let options = Options {
                bold,
                italic,
                ..default()
            };
            cases.push(case(
                &format!("{name}-{style}"),
                family,
                options,
                chars(SAMPLE),
            ));
        }
    }

    // The horizontal fitting: scaled between 0.5 and 1.25, else centered.
    let widths = [
        None,
        Some(0.0),
        Some(250.0),
        Some(500.0),
        Some(1000.0),
        Some(2000.0),
    ];
    let mut glyphs = Vec::new();
    for c in ['M', 'i', 'o'] {
        for width in widths {
            glyphs.push(request(&[c as u32], width));
        }
    }
    cases.push(case("sans-widths", Family::Sans, default(), glyphs));

    // A font narrower than the family, whose "i", "j", "l" and "J" are less
    // condensed than the other glyphs: the stems have the scale of the font.
    let sans = glyph_data(Family::Sans);
    let glyphs = chars("Ljubljana illicit million Jill")
        .into_iter()
        .map(|mut glyph| {
            let c = glyph.unicode[0];
            let advance = sans.advance(sans.glyph_id(c).unwrap()) as f64;
            let natural = advance * 1000.0 / sans.units_per_em as f64;
            let narrow = "ijlJ".contains(char::from_u32(c).unwrap());
            glyph.width = Some(natural * if narrow { 1.0 } else { 0.85 });
            glyph
        })
        .collect();
    cases.push(case("sans-narrow-widths", Family::Sans, default(), glyphs));

    let options = [
        (
            "sans-weight-100",
            Options {
                weight: Some(100.0),
                ..default()
            },
        ),
        (
            "sans-weight-900",
            Options {
                weight: Some(900.0),
                ..default()
            },
        ),
        (
            "sans-stem-v-200",
            Options {
                stem_v: Some(200.0),
                ..default()
            },
        ),
        (
            "sans-italic-angle-20",
            Options {
                italic_angle: Some(-20.0),
                ..default()
            },
        ),
        (
            "sans-backslant-12",
            Options {
                italic_angle: Some(12.0),
                ..default()
            },
        ),
        (
            "sans-cap-height-600",
            Options {
                cap_height: Some(600.0),
                ..default()
            },
        ),
        (
            "sans-cap-height-850",
            Options {
                cap_height: Some(850.0),
                ..default()
            },
        ),
    ];
    for (name, options) in options {
        cases.push(case(name, Family::Sans, options, chars("Hamburg HOxg")));
    }

    // Ligatures, compositions, sequences, codepoints replaced by their
    // normalized forms (square m, bold A, fullwidth A, angstrom sign), an
    // empty text and a missing glyph.
    let texts: [&[u32]; 13] = [
        &[0x66, 0x66, 0x69],
        &[0x66, 0x69],
        &[0x66, 0x6c],
        &[0x73, 0x74],
        &[0x65, 0x301],
        &[0x41, 0x30a],
        &[0x71, 0x303],
        &[0x33a1],
        &[0x1d400],
        &[0xff21],
        &[0x212b],
        &[],
        &[0x4e00],
    ];
    let glyphs = texts.iter().map(|text| request(text, None)).collect();
    cases.push(case("sans-texts", Family::Sans, default(), glyphs));

    let symbols = "←↑→↓↔⇒ ∑∏∫√∞≈≠≤≥ ■□●○▲►◆★☆ ☐☑☒✓✔✗✘ ♠♣♥♦ ①②③ ⌘⏎";
    cases.push(case(
        "sans-symbols",
        Family::Sans,
        default(),
        chars(symbols),
    ));
    // Compare standalone glyphs with sequences mixing embedded styles,
    // synthesized symbols, and the compatibility decomposition of "㎡".
    let texts = |texts: &[&str]| {
        texts
            .iter()
            .map(|text| request(&text.chars().map(u32::from).collect::<Vec<_>>(), None))
            .collect()
    };
    let options = Options {
        bold: true,
        italic: true,
        ..default()
    };
    cases.push(case(
        "serif-bold-italic-sequences",
        Family::Serif,
        options,
        texts(&["A", "★", "A★", "★A", "H✓H"]),
    ));
    let options = Options {
        weight: Some(900.0),
        ..default()
    };
    cases.push(case(
        "sans-weight-900-sequences",
        Family::Sans,
        options,
        texts(&["m", "2", "\u{33a1}", "st", "H✓H"]),
    ));
    let code = "┌─┬┐│├┼┤└┴┘═║╔╗╚╝ fn main() { x[0] = 42; }";
    cases.push(case("mono-code", Family::Mono, default(), chars(code)));

    // The standard fonts: the sample, whose symbols come from the Noto fonts
    // (synthesized to the style), all the glyphs of Symbol and ZapfDingbats,
    // and the bullets and check boxes of Wingdings with a fifth of its glyphs
    // (all of them would make a 600 KB reference).
    for font in StandardFont::ALL {
        let glyphs = match font {
            StandardFont::Symbol | StandardFont::ZapfDingbats => (standard_glyph_data(font)
                .codepoints())
            .map(|cp| request(&[cp], None))
            .collect(),
            StandardFont::Wingdings => {
                let mut glyphs = chars("▪➢✓✔☐☑☒⮚❖");
                let sample: Vec<Request> = (standard_glyph_data(font).codepoints().step_by(5))
                    .filter(|&cp| !glyphs.iter().any(|glyph| glyph.unicode == [cp]))
                    .map(|cp| request(&[cp], None))
                    .collect();
                glyphs.extend(sample);
                glyphs
            }
            _ => chars(SAMPLE),
        };
        cases.push(Case {
            name: format!("standard-{}", font.name().to_lowercase()),
            source: Source::Standard(font),
            options: default(),
            glyphs,
        });
    }
    cases
}

/// Escapes a text for XML.
fn escape(text: &str) -> String {
    let mut escaped = String::new();
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// A glyph, as read by skrifa.
struct Glyph {
    advance: f32,
    path: String,
    /// x_min, y_min, x_max, y_max.
    bounds: [f32; 4],
}

/// Draws the glyphs of a font (except .notdef) in a grid: each glyph with its
/// advance box (from the descender to the ascender), its baseline, and a
/// label with its id, its text, its codes in the cmap and its advance.
fn draw(case: &Case, font: &GeneratedFont) -> String {
    const SCALE: i32 = 8; // Font units per pixel.
    const PAD: i32 = 100;
    const LABEL: i32 = 150;
    const HEADER: i32 = 320;
    let scaler = ScalerFont::unhinted(Font::new(font.data.clone(), 0).unwrap(), None);
    let font_ref = scaler.font();
    let metrics = scaler.metrics();
    let line = metrics.h_line().unwrap();
    let style = scaler.style_metrics();
    let attributes = font_ref.default_attributes();
    let full_name: String = font_ref
        .names()
        .get(NameId::FULL_NAME)
        .and_then(|name| name.english_or_first())
        .map(|name| name.name().chars().collect())
        .unwrap_or_default();
    let mut codes: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (code, gid) in font_ref.charmap().unicodes() {
        codes.entry(gid.to_u32()).or_default().push(code);
    }

    let glyph_metrics = scaler.linear_glyph_metrics();
    let glyphs: Vec<Glyph> = (1..font_ref.num_glyphs())
        .map(|gid| {
            let glyph = GlyphId::new(gid);
            let mut path = SvgSink::new();
            let mut bounds = ControlBoundsSink::new();
            if let Some(outline) = scaler.outline(glyph) {
                outline.draw(DrawMemory::Scratch, &mut path).unwrap();
                outline.draw(DrawMemory::Scratch, &mut bounds).unwrap();
            }
            let b = bounds.bounding_box().unwrap_or_default();
            Glyph {
                advance: glyph_metrics.h_advance(glyph),
                path: path.to_string(),
                bounds: [b.x_min, b.y_min, b.x_max, b.y_max],
            }
        })
        .collect();

    // The cells are large enough for all the glyphs and their advance boxes.
    // Their sizes are rounded, so that a change of a glyph usually doesn't
    // move the other ones (and change all the lines of the file).
    let max = |f: &dyn Fn(&Glyph) -> f32, step: i32| {
        let max = glyphs.iter().map(f).fold(0.0, f32::max).ceil() as i32;
        (max + step - 1) / step * step
    };
    let left = max(&|g| -g.bounds[0], 250);
    let right = max(&|g| g.advance.max(g.bounds[2]), 250);
    let top = max(&|g| g.bounds[3].max(line.ascender), 100);
    let bottom = max(&|g| -g.bounds[1].min(line.descender), 100);
    let cell_width = (left + right + 2 * PAD).max(1000);
    let cell_height = top + bottom + LABEL + 2 * PAD;
    let columns = glyphs.len().clamp(1, 10) as i32;
    let rows = glyphs.len().div_ceil(10) as i32;
    let width = (columns * cell_width).max(8000);
    let height = HEADER + rows * cell_height;

    let mut svg = String::new();
    let _ = writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {width} {height}" font-family="sans-serif" font-size="100">"#,
        width / SCALE,
        height / SCALE
    );
    svg.push_str(concat!(
        "<style>.title { font-size: 120px; } .label { fill: #555; } .missing { fill: #c00; }",
        " .box { fill: #e8eef8; } .baseline { fill: #9ab; }</style>\n",
        "<rect width=\"100%\" height=\"100%\" fill=\"#fff\"/>\n",
    ));
    let _ = writeln!(
        svg,
        r#"<text class="title" x="{PAD}" y="140">{}: {}</text>"#,
        case.name,
        escape(&full_name)
    );
    let _ = writeln!(
        svg,
        r#"<text class="label" x="{PAD}" y="270">ascender {}, descender {}, line gap {}, cap height {}, x-height {}, italic angle {}, weight {}, style {:?}, fixed pitch {}</text>"#,
        line.ascender,
        line.descender,
        line.line_gap,
        metrics.cap_height().unwrap_or_default(),
        metrics.x_height().unwrap_or_default(),
        style.italic_angle().unwrap_or_default().to_f32(),
        attributes.weight.value(),
        attributes.style,
        style.is_fixed_pitch().unwrap_or_default(),
    );
    for (i, glyph) in glyphs.iter().enumerate() {
        let x = (i as i32 % 10) * cell_width + PAD + left;
        let y = HEADER + (i as i32 / 10) * cell_height + PAD + top;
        let text: String = (case.glyphs[i].unicode.iter())
            .map(|&cp| char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        let codes: Vec<String> = (codes.get(&(i as u32 + 1)).into_iter().flatten())
            .map(|code| format!("U+{code:04X}"))
            .collect();
        let missing = font.missing.contains(&i);
        let _ = writeln!(
            svg,
            concat!(
                r#"<g transform="translate({},{})">"#,
                r#"<rect class="box" y="{}" width="{}" height="{}"/>"#,
                r#"<rect class="baseline" y="-3" width="{}" height="6"/>"#,
                r#"<path transform="scale(1,-1)" d="{}"/>"#,
                r#"<text class="{}" y="{}">{} {} {} {}{}</text></g>"#
            ),
            x,
            y,
            -line.ascender,
            glyph.advance,
            line.ascender - line.descender,
            glyph.advance,
            glyph.path,
            if missing { "missing" } else { "label" },
            bottom + 120,
            i + 1,
            escape(&text),
            if codes.is_empty() {
                "no code".to_string()
            } else {
                codes.join(",")
            },
            glyph.advance,
            if missing { " missing" } else { "" },
        );
    }
    svg.push_str("</svg>\n");
    svg
}

#[test]
fn draws_the_glyphs_like_the_references() {
    let refs = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/refs");
    let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reftest");
    let update = std::env::var_os("UPDATE_REFS").is_some();
    let _ = fs::remove_dir_all(&report);
    fs::create_dir_all(&refs).unwrap();

    let cases = cases();
    let mut failures = Vec::new();
    for case in &cases {
        let font = match case.source {
            Source::Family(family) => generate_font(family, &case.glyphs, &case.options),
            Source::Standard(font) => generate_standard_font(font, &case.glyphs, &case.options),
        };
        let svg = draw(case, &font);
        let file = format!("{}.svg", case.name);
        let reference = fs::read_to_string(refs.join(&file)).ok();
        if reference.as_ref() == Some(&svg) {
            continue;
        }
        if update {
            fs::write(refs.join(&file), &svg).unwrap();
            continue;
        }
        fs::create_dir_all(&report).unwrap();
        fs::write(report.join(&file), &svg).unwrap();
        if let Some(reference) = &reference {
            fs::write(report.join(format!("{}.ref.svg", case.name)), reference).unwrap();
        }
        failures.push((case.name.as_str(), reference.is_some()));
    }

    // The references of the cases which don't exist anymore.
    let mut stale = Vec::new();
    for entry in fs::read_dir(&refs).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        if !cases.iter().any(|case| case.name == name) {
            if update {
                fs::remove_file(&path).unwrap();
            } else {
                stale.push(name);
            }
        }
    }

    let mut errors = Vec::new();
    if !stale.is_empty() {
        errors.push(format!(
            "References without test case: {} (UPDATE_REFS=1 removes them).",
            stale.join(", ")
        ));
    }
    if !failures.is_empty() {
        let index = write_report(&report, &failures);
        let names: Vec<&str> = failures.iter().map(|&(name, _)| name).collect();
        errors.push(format!(
            "{} drawings differ from their references: {}.\nCompare them in {}, and run with UPDATE_REFS=1 to update the references.",
            names.len(),
            names.join(", "),
            index.display()
        ));
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// Writes an HTML page to compare the drawings which differ from their
/// references (written in `report`), and returns its path.
fn write_report(report: &Path, failures: &[(&str, bool)]) -> PathBuf {
    let mut html = String::from(concat!(
        "<!DOCTYPE html>\n<meta charset=\"utf-8\">\n<title>Reference test failures</title>\n",
        "<style>body { font-family: sans-serif; } img { max-width: 100%; border: 1px solid #ccc; }\n",
        ".flip > .ref, .flip:hover > .new { display: none; } .flip:hover > .ref { display: inline; }</style>\n",
        "<p>The new drawings: hovering one shows its reference.</p>\n",
    ));
    for &(name, has_reference) in failures {
        if has_reference {
            let _ = writeln!(
                html,
                r#"<h2>{name}</h2><div class="flip"><img class="new" src="{name}.svg"><img class="ref" src="{name}.ref.svg"></div>"#
            );
        } else {
            let _ = writeln!(
                html,
                r#"<h2>{name} (no reference)</h2><img src="{name}.svg">"#
            );
        }
    }
    let index = report.join("index.html");
    fs::write(&index, html).unwrap();
    index
}
