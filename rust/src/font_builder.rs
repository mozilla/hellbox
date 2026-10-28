// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Builds a TrueType font with only the requested glyphs, from glyph data.
//!
//! Each glyph is horizontally scaled to fit the expected advance width, its
//! stems getting the scale of the font (see `font_scale`), and possibly
//! emboldened (or thinned) and slanted, to synthesize a style (see
//! `style.rs`). There is no hinting. Lengths use `sqrt` and the slant is
//! rounded to 16.16, rather than `hypot` and the raw `tan`, which differ
//! between platforms: the fonts are the same everywhere.

use crate::glyph_data::{GlyphData, Outline};
use crate::outline;
use crate::unicode::{Resolver, UnicodeTables};

const DEFAULT_NAME: &str = "HellboxSubstitute";
/// The coordinates are bounded so that the difference of two of them fits in
/// the int16 deltas of the glyf table. It's also the maximal advance width.
const MAX_COORDINATE: f64 = 16383.0;
/// The number of pairs of a `kern` subtable, whose length is a uint16.
const MAX_KERNING_PAIRS: usize = (0xffff - 14) / 6;
/// The number of glyphs of a font, .notdef included (a uint16 in `maxp`).
const MAX_GLYPHS: usize = 0xffff;
// Without outlines, a font has a glyph per advance (they're bounded by
// MAX_COORDINATE) and per kerned glyph (two per pair at most): they fit.
const _: () = assert!(1 + (MAX_COORDINATE as usize + 1) + 2 * MAX_KERNING_PAIRS <= MAX_GLYPHS);
/// The numbers of points and contours of a glyph (a uint16 and an int16 in
/// `glyf`).
const MAX_POINTS: usize = 0xffff;
const MAX_CONTOURS: usize = 0x7fff;
/// The name table has 16-bit offsets: the name is truncated.
const MAX_NAME_LENGTH: usize = 255;
/// The thickness of the outlines of `Options::debug_boxes`, in em.
const DEBUG_BOX_STROKE: f64 = 1.0 / 32.0;
/// The limits of the stem compensation of the fitting (see `build_font`): the
/// thinning, in horizontal stems (the thin strokes of a serif font must
/// survive), and the thickening, in vertical stems.
const MAX_THINNING: f64 = 0.25;
const MAX_THICKENING: f64 = 0.4;
/// Smaller compensations, in font units, are skipped: the rounded points
/// barely move, and the glyphs of a font with the widths of the family (e.g.
/// Helvetica) aren't emboldened, which is the slowest part of the fitting.
const MIN_COMPENSATION: f64 = 0.5;

/// A requested glyph.
#[derive(Clone, Debug, Default)]
pub struct Request {
    /// The text of the glyph: usually a single codepoint, but it can be a
    /// ligature (e.g. "ffi") or a letter followed by combining marks.
    pub unicode: Vec<u32>,
    /// Expected advance width in `Options::units_per_em` units, or None for
    /// the natural width.
    pub width: Option<f64>,
    /// The code mapped to the glyph in the cmap, or None for the first
    /// codepoint of `unicode`.
    pub char_code: Option<u32>,
}

/// The kerning of a pair of requested glyphs (e.g. to apply the kerning of a
/// PDF file in a text layer).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Kerning {
    /// The indices of the requests of the glyphs.
    pub left: usize,
    pub right: usize,
    /// Adjustment of the left glyph's advance in `Options::units_per_em`
    /// units; negative values bring the glyphs closer.
    pub value: f64,
}

/// Options of the generated font. The style ones (`bold`, `weight`, `stem_v`,
/// `italic`, `italic_angle`, `cap_height`) are the ones of the fonts of a
/// family (see `generate_font`).
#[derive(Clone, Debug)]
pub struct Options {
    /// Units of widths, `stem_v` and `cap_height` (default: 1000, PDF glyph space).
    pub units_per_em: f64,
    pub bold: bool,
    /// Positive target weight, clamped to 1..=1000; overrides `bold`.
    pub weight: Option<f64>,
    /// Expected vertical stem width, it has precedence over `weight` and
    /// `bold`.
    pub stem_v: Option<f64>,
    pub italic: bool,
    /// In degrees, counter-clockwise from the vertical (negative for a font
    /// leaning to the right). Synthetic slant is limited to +/-45 degrees.
    /// A negative finite serif angle selects the embedded italic style at
    /// -16.33 degrees; positive serif angles are synthesized from upright.
    pub italic_angle: Option<f64>,
    /// Target cap height; the requested scale is clamped to 0.8..=1.25.
    pub cap_height: Option<f64>,
    /// Limits of the horizontal scale factor (the stems getting the scale of
    /// the font, see `build_font`), the glyph being centered when the expected
    /// width can't be reached.
    pub min_scale: f64,
    pub max_scale: f64,
    /// Embolden the dense glyphs less.
    pub adaptive_bold: bool,
    /// The font name ("HellboxSubstitute" when empty), truncated to 255
    /// characters.
    pub name: String,
    /// Omit outlines and use minimal names, e.g. for a text layer. Requests
    /// of equal advance share glyph ids unless used by an emitted kerning
    /// pair. Use the cmap to find their ids.
    pub metrics_only: bool,
    /// The ascent as a fraction of the em (between 0 and 1), the descent being
    /// the rest, without line gap: e.g. for a text layer, whose lines are an
    /// em high.
    pub ascent: Option<f64>,
    /// The kerning of some pairs of glyphs (a `kern` table): the pairs of
    /// glyphs which aren't in the cmap are ignored, and a subtable has at most
    /// 10920 pairs (the first ones in the order of the requests).
    pub kerning: Vec<Kerning>,
    /// For debugging (e.g. with `metrics_only`): each glyph with an advance,
    /// .notdef excepted, is the outline (1/32 em thick, inside) of a
    /// rectangle covering it, from the descender to the ascender.
    pub debug_boxes: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            units_per_em: 1000.0,
            bold: false,
            weight: None,
            stem_v: None,
            italic: false,
            italic_angle: None,
            cap_height: None,
            min_scale: 0.5,
            // Stretched more, the stems would get thicker (the thinning which
            // keeps them is bounded, see `MAX_THINNING`): the glyph gets space
            // around it instead.
            max_scale: 1.25,
            adaptive_bold: true,
            name: DEFAULT_NAME.to_string(),
            metrics_only: false,
            ascent: None,
            kerning: Vec::new(),
            debug_boxes: false,
        }
    }
}

impl Options {
    /// The units of the widths (1000 when unknown).
    pub(crate) fn width_units(&self) -> f64 {
        if self.units_per_em > 0.0 {
            self.units_per_em
        } else {
            1000.0
        }
    }
}

/// Clamp without the argument checks in `f64::clamp`.
pub(crate) fn clamp(v: f64, min: f64, max: f64) -> f64 {
    v.max(min).min(max)
}

/// A metric of the glyph data, 0 meaning unknown.
pub(crate) fn known(value: impl Into<f64>) -> Option<f64> {
    let value = value.into();
    (value != 0.0).then_some(value)
}

/// A value in font units, rounded (and saturated).
fn fword(v: f64) -> i16 {
    v.round() as i16
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { bytes: Vec::new() }
    }

    fn with_capacity(capacity: usize) -> Self {
        Writer {
            bytes: Vec::with_capacity(capacity),
        }
    }

    fn u16(&mut self, v: u16) {
        self.bytes.extend_from_slice(&v.to_be_bytes());
    }

    fn i16(&mut self, v: i16) {
        self.bytes.extend_from_slice(&v.to_be_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.bytes.extend_from_slice(&v.to_be_bytes());
    }

    /// A value in font units (FWORD), rounded.
    fn fword(&mut self, v: f64) {
        self.i16(fword(v));
    }

    /// A 16.16 fixed point number.
    fn fixed(&mut self, v: f64) {
        self.u32((v * 65536.0).round() as i32 as u32);
    }

    fn bytes(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
    }

    fn zeros(&mut self, n: usize) {
        self.bytes.resize(self.bytes.len() + n, 0);
    }

    fn pad(&mut self) {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }
}

/// Reduce emboldening as control-polygon perimeter grows relative to the
/// advance and em size. The factor is clamped to 0.35..=1.
fn density_factor<'a>(parts: impl Iterator<Item = &'a Outline>, advance: f64, upm: f64) -> f64 {
    let mut perimeter = 0.0;
    for outline in parts {
        let mut start = 0;
        for &end in &outline.end_pts {
            let mut px = outline.xs[end];
            let mut py = outline.ys[end];
            for i in start..=end {
                let dx = outline.xs[i] - px;
                let dy = outline.ys[i] - py;
                perimeter += (dx * dx + dy * dy).sqrt();
                px = outline.xs[i];
                py = outline.ys[i];
            }
            start = end + 1;
        }
    }
    let density = perimeter / (advance.max(upm / 4.0) * upm).sqrt();
    clamp(1.0 - 0.04 * (density - 4.0), 0.35, 1.0)
}

struct Metrics {
    units_per_em: u16,
    ascender: i16,
    descender: i16,
    line_gap: i16,
    cap_height: i16,
    x_height: i16,
    underline_position: i16,
    underline_thickness: i16,
    weight: u16,
    bold: bool,
    italic: bool,
    italic_angle: f64,
    slant: f64,
    fixed_pitch: bool,
}

struct OutGlyph {
    outline: Option<Outline>,
    advance: u16,
}

/// A glyph of the text of a requested glyph (e.g. a letter of a sequence, or
/// a combining mark).
struct Part {
    outline: Outline,
    /// Its natural position.
    offset: f64,
    /// The index of its glyph data.
    source: usize,
    /// Whether it has an advance (else it's a mark over the previous part).
    spacing: bool,
    /// The vertical and horizontal stems of its glyph data (0 when unknown).
    stems: (f64, f64),
    /// The horizontal emboldening which gives its stems the scale of the font
    /// (see `build_font`).
    compensation: f64,
}

/// How the glyphs of some glyph data are transformed, besides their
/// horizontal scale: emboldened (or thinned, with negative strengths),
/// vertically scaled and slanted.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Synthesis {
    pub bold_x: f64,
    pub bold_y: f64,
    pub y_scale: f64,
    pub slant: f64,
}

/// The style of a generated font (see `style.rs`).
#[derive(Clone, Debug)]
pub(crate) struct FontStyle {
    /// The synthesis of the glyphs of each source.
    pub syntheses: Vec<Synthesis>,
    pub weight: f64,
    /// In degrees, negative for a font leaning to the right.
    pub italic_angle: f64,
    /// Whether the stems follow the horizontal scale of the font (see
    /// `font_scale`), else they keep the width of the style (e.g. the
    /// expected `Options::stem_v`).
    pub scaled_stems: bool,
}

/// The scale of a font: the median of the horizontal scales of its glyphs
/// with an expected width (1 without any).
fn font_scale(
    sources: &[&GlyphData],
    syntheses: &[Synthesis],
    requests: &[Request],
    resolved: &[Vec<(usize, usize)>],
    to_font_units: impl Fn(f64) -> f64,
) -> f64 {
    let upm = sources[0].units_per_em as f64;
    // The bits of positive numbers sort like them: the sort of the cmap and
    // the kerning pairs is reused (another one would make the wasm module
    // bigger).
    let mut scales: Vec<(u64, usize)> = Vec::new();
    for (request, glyphs) in requests.iter().zip(resolved) {
        let Some(width) = request.width.filter(|&width| width > 0.0) else {
            continue;
        };
        let (mut natural_advance, mut strength) = (0.0, 0.0);
        for &(source, gid) in glyphs {
            let data = sources[source];
            let advance = data.advance(gid) as f64 * upm / data.units_per_em as f64;
            if advance > 0.0 {
                natural_advance += advance;
                strength += syntheses[source].bold_x;
            }
        }
        let scale = (to_font_units(width) - strength) / natural_advance;
        if scale > 0.0 && scale.is_finite() {
            scales.push((scale.to_bits(), 0));
        }
    }
    if scales.is_empty() {
        return 1.0;
    }
    sort_by_code(&mut scales);
    f64::from_bits(scales[scales.len() / 2].0)
}

/// The horizontal bounds of an outline (None if it's empty).
fn x_bounds(outline: &Outline) -> Option<(f64, f64)> {
    let min = outline.xs.iter().copied().reduce(f64::min)?;
    let max = outline.xs.iter().copied().reduce(f64::max)?;
    Some((min, max))
}

/// The shear of an italic angle, rounded to 16.16.
pub(crate) fn slant_of(italic_angle: f64) -> f64 {
    (((-italic_angle * std::f64::consts::PI) / 180.0).tan() * 65536.0).round() / 65536.0
}

/// Builds the font, preferring canonical resolution from the first source,
/// then resolving against all sources. Metrics come from the first source.
/// Returns the font and indices of unresolved or over-limit requests.
pub fn build_font(
    sources: &[&GlyphData],
    style: &FontStyle,
    tables: &UnicodeTables,
    requests: &[Request],
    options: &Options,
) -> (Vec<u8>, Vec<usize>) {
    let primary = sources[0];
    let width_units = options.width_units();
    let upm = primary.units_per_em as f64;
    let to_font_units = |v: f64| (v * upm) / width_units;
    let cap_height = known(primary.cap_height).unwrap_or(primary.ascender as f64);
    let syntheses = &style.syntheses;
    let italic = style.italic_angle != 0.0;
    // The slant of the font, and the vertical scale of its metrics.
    let slant = slant_of(style.italic_angle);
    let y_scale = syntheses[0].y_scale;
    // The other requests don't fit in the font (without outlines, they all
    // fit: the glyphs are shared, see MAX_GLYPHS).
    let count = if options.metrics_only {
        requests.len()
    } else {
        requests.len().min(MAX_GLYPHS - 1)
    };
    let (requests, excess) = (&requests[..count], requests.len() - count);

    let resolver = Resolver::new(sources, tables);
    // Prefer a complete canonical resolution from the primary source before
    // resolving against all sources with compatibility fallbacks.
    let primary_resolver = (sources.len() > 1).then(|| Resolver::canonical(&sources[..1], tables));
    let mut used_sources = vec![0];
    let mut missing = Vec::new();
    let mut out_glyphs = Vec::with_capacity(count + 1);
    out_glyphs.push(OutGlyph {
        outline: None,
        advance: (upm / 2.0).round() as u16,
    });
    let mut cmap: Vec<(u32, usize)> = Vec::new();
    let mut mapped = std::collections::HashSet::new();
    // Without outlines, a given width is the advance: the glyph data only
    // matters for a natural width (its advance and, for the emboldening, the
    // outline, see `density_factor`).
    let uses_data = |request: &Request| {
        !options.metrics_only || !matches!(request.width, Some(width) if width >= 0.0)
    };
    // The glyphs of the requests, which are decoded together (see
    // `GlyphData::prepare`).
    let resolved: Vec<Vec<(usize, usize)>> = requests
        .iter()
        .map(|request| {
            let mut glyphs = primary_resolver
                .as_ref()
                .map(|resolver| resolver.resolve(&request.unicode))
                .unwrap_or_default();
            if glyphs.is_empty() {
                glyphs = resolver.resolve(&request.unicode);
            }
            glyphs
        })
        .collect();
    for (source, data) in sources.iter().enumerate() {
        let gids: Vec<usize> = requests
            .iter()
            .zip(&resolved)
            .filter(|(request, _)| uses_data(request))
            .flat_map(|(_, glyphs)| glyphs)
            .filter(|&&(s, _)| s == source)
            .map(|&(_, gid)| gid)
            .collect();
        if !gids.is_empty() {
            data.prepare(&gids);
        }
    }
    // Without outlines the scale doesn't matter, and the expected stems
    // (`Options::stem_v`) don't follow it.
    let font_scale = if options.metrics_only || !style.scaled_stems {
        1.0
    } else {
        clamp(
            font_scale(sources, syntheses, requests, &resolved, to_font_units),
            options.min_scale,
            options.max_scale,
        )
    };
    for (i, (request, glyphs)) in requests.iter().zip(resolved).enumerate() {
        if glyphs.is_empty() {
            missing.push(i);
        }
        if let Some(code) = request
            .char_code
            .or_else(|| request.unicode.first().copied())
        {
            if mapped.insert(code) {
                cmap.push((code, i + 1));
            }
        }

        let mut natural_advance = 0.0;
        let used_glyphs = if uses_data(request) { &glyphs[..] } else { &[] };
        let mut parts: Vec<Part> = Vec::with_capacity(used_glyphs.len());
        for &(source, gid) in used_glyphs {
            if !used_sources.contains(&source) {
                used_sources.push(source);
            }
            let data = sources[source];
            let mut glyph_outline = data.outline(gid);
            let scale = upm / data.units_per_em as f64;
            if scale != 1.0 {
                outline::transform(&mut glyph_outline, [scale, 0.0, 0.0, scale, 0.0, 0.0]);
            }
            let advance = data.advance(gid) as f64 * scale;
            let mut offset = natural_advance;
            // Center zero-advance marks on the previous part's horizontal bounds;
            // this builder does not apply mark-positioning tables.
            if let (0.0, Some(previous)) = (advance, parts.last()) {
                let base = x_bounds(&previous.outline);
                if let (Some(mark), Some(base)) = (x_bounds(&glyph_outline), base) {
                    offset = previous.offset + (base.0 + base.1 - mark.0 - mark.1) / 2.0;
                }
            }
            parts.push(Part {
                outline: glyph_outline,
                offset,
                source,
                spacing: advance > 0.0,
                stems: (data.stem_v as f64 * scale, data.stem_h as f64 * scale),
                compensation: 0.0,
            });
            natural_advance += advance;
        }
        // A long text (e.g. a repeated letter) can't be a single glyph, but
        // its advance is enough without outlines.
        let points: usize = parts.iter().map(|part| part.outline.xs.len()).sum();
        let contours: usize = parts.iter().map(|part| part.outline.end_pts.len()).sum();
        if !options.metrics_only && (points > MAX_POINTS || contours > MAX_CONTOURS) {
            missing.push(i);
            parts.clear();
            natural_advance = 0.0;
        }
        for part in &mut parts {
            let offset = part.offset;
            if offset != 0.0 {
                part.outline.xs.iter_mut().for_each(|x| *x += offset);
            }
        }

        let empty = parts.iter().all(|part| part.outline.xs.is_empty());
        let factor = if !options.adaptive_bold || empty {
            1.0
        } else {
            density_factor(parts.iter().map(|part| &part.outline), natural_advance, upm)
        };
        // The emboldening widens each part with an advance (with the strength
        // of its glyph data), as the advance of a single glyph.
        let strength = parts
            .iter()
            .filter(|part| part.spacing)
            .map(|part| syntheses[part.source].bold_x * factor)
            .sum::<f64>();
        let stem_width = parts
            .iter()
            .filter(|part| part.spacing)
            .map(|part| part.stems.0 * factor)
            .sum::<f64>();
        let (mut advance, stem_scale) = match request.width {
            Some(width) if width >= 0.0 => (to_font_units(width), font_scale),
            _ => (natural_advance + strength, 1.0),
        };
        // Bound the advance as well as the outline coordinates.
        advance = advance.min(MAX_COORDINATE);
        // The stems get the scale of the font (e.g. a condensed font has
        // thinner stems), whatever the one of the glyph: the glyph scaled by
        // `x_scale` is emboldened by `stem * (stem_scale - x_scale)`, so its
        // own scale only changes its counters and its side bearings (else the
        // glyphs stretched more than the other ones, e.g. an "i" or a "J",
        // look bolder).
        let mut x_scale = 1.0;
        if natural_advance > 0.0 {
            let target = advance - strength;
            let fit = if natural_advance > stem_width {
                (target - stem_width * stem_scale) / (natural_advance - stem_width)
            } else {
                target / natural_advance
            };
            x_scale = clamp(fit, options.min_scale, options.max_scale);
        }
        let mut compensation_width = 0.0;
        for part in &mut parts {
            let (stem_v, stem_h) = (part.stems.0 * factor, part.stems.1 * factor);
            part.compensation = clamp(
                stem_v * (stem_scale - x_scale),
                -MAX_THINNING * stem_h,
                MAX_THICKENING * stem_v,
            );
            if part.compensation.abs() < MIN_COMPENSATION {
                part.compensation = 0.0;
            }
            if part.spacing {
                compensation_width += part.compensation;
            }
        }
        // Center the glyph when it can't fit exactly.
        let shift = (advance - (natural_advance * x_scale + strength + compensation_width)) / 2.0;
        if options.metrics_only {
            parts.clear();
        }
        // Apply each source's synthesis and add preceding advance growth.
        // Marks inherit their base's offset rather than advancing the pen.
        let (mut growth, mut part_growth) = (0.0, 0.0);
        for part in &mut parts {
            let synthesis = syntheses[part.source];
            let bold_x = synthesis.bold_x * factor + part.compensation;
            let bold_y = synthesis.bold_y * factor;
            if part.spacing {
                part_growth = growth;
                growth += bold_x;
            }
            let outline = &mut part.outline;
            outline::transform(outline, [x_scale, 0.0, 0.0, synthesis.y_scale, 0.0, 0.0]);
            outline::embolden(outline, bold_x, bold_y);
            let bold_y_scale = if cap_height > 0.0 {
                cap_height / (cap_height + bold_y)
            } else {
                1.0
            };
            let dx = shift + part_growth;
            outline::transform(outline, [1.0, 0.0, synthesis.slant, bold_y_scale, dx, 0.0]);
        }
        let glyph_outline = outline::concat(parts.into_iter().map(|part| part.outline));
        out_glyphs.push(OutGlyph {
            outline: (!glyph_outline.xs.is_empty()).then_some(glyph_outline),
            advance: advance.round().max(0.0) as u16,
        });
    }
    missing.extend(count..count + excess);

    // The glyph id of the requests mapped in the cmap (0 for the other ones).
    let mut request_gids = vec![0; requests.len()];
    for &(_, gid) in &cmap {
        request_gids[gid - 1] = gid;
    }
    let kerned_pairs = prepare_kerning(&options.kerning, &request_gids, to_font_units);
    if options.metrics_only {
        share_glyphs(&mut out_glyphs, &mut cmap, &mut request_gids, &kerned_pairs);
    }

    // Kerned requests retain their order when assigned glyph ids, so this
    // conversion preserves the pair ordering required by the kern table.
    let kerning: Vec<(u32, usize)> = kerned_pairs
        .iter()
        .map(|&(pair, value)| {
            let [left, right] = kerning_requests(pair).map(|i| request_gids[i] as u32);
            ((left << 16) | right, value)
        })
        .collect();

    let (ascender, descender, line_gap) = match options.ascent {
        Some(ascent) if ascent > 0.0 && ascent < 1.0 => {
            let ascender = (ascent * upm).round();
            (fword(ascender), fword(ascender - upm), 0)
        }
        _ => (
            fword(primary.ascender as f64 * y_scale),
            fword(primary.descender as f64 * y_scale),
            primary.line_gap,
        ),
    };
    if options.debug_boxes {
        add_debug_boxes(&mut out_glyphs[1..], ascender, descender, upm);
    }
    let metrics = Metrics {
        units_per_em: primary.units_per_em,
        ascender,
        descender,
        line_gap,
        cap_height: fword(cap_height * y_scale),
        x_height: fword(primary.x_height as f64 * y_scale),
        underline_position: primary.underline_position,
        underline_thickness: primary.underline_thickness,
        weight: style.weight as u16,
        bold: style.weight >= 600.0,
        italic,
        italic_angle: style.italic_angle,
        slant,
        fixed_pitch: primary.fixed_pitch,
    };
    let used: Vec<&GlyphData> = used_sources.iter().map(|&i| sources[i]).collect();
    let name: String = if options.name.is_empty() {
        DEFAULT_NAME
    } else {
        &options.name
    }
    .chars()
    .take(MAX_NAME_LENGTH)
    .collect();
    let names = if options.metrics_only {
        // Keep the five name IDs that OTS would otherwise synthesize.
        [(1, "P"), (2, "R"), (4, "P"), (5, "1"), (6, "P")]
            .map(|(id, value)| (id, value.to_string()))
            .to_vec()
    } else {
        make_names(&used, &name, &metrics)
    };
    let font = write_font(
        &out_glyphs,
        &cmap,
        &kerning,
        &metrics,
        &names,
        options.metrics_only,
    );
    (font, missing)
}

/// Replaces the outlines of the glyphs with the outlines of boxes covering
/// their advance, from the descender to the ascender (see
/// `Options::debug_boxes`).
fn add_debug_boxes(glyphs: &mut [OutGlyph], ascender: i16, descender: i16, upm: f64) {
    let (top, bottom) = (ascender as f64, descender as f64);
    let stroke = (upm * DEBUG_BOX_STROKE).round();
    for glyph in glyphs {
        if glyph.advance == 0 {
            glyph.outline = None;
            continue;
        }
        let right = glyph.advance as f64;
        // Clockwise outer contour and counter-clockwise inner contour.
        // Glyphs too narrow for the inner contour are filled.
        let mut outline = Outline {
            xs: vec![0.0, 0.0, right, right],
            ys: vec![bottom, top, top, bottom],
            on_curve: vec![1; 4],
            end_pts: vec![3],
        };
        let (left, right) = (stroke, right - stroke);
        let (bottom, top) = (bottom + stroke, top - stroke);
        if left < right && bottom < top {
            outline.xs.extend([left, right, right, left]);
            outline.ys.extend([bottom, bottom, top, top]);
            outline.on_curve.extend([1; 4]);
            outline.end_pts.push(7);
        }
        glyph.outline = Some(outline);
    }
}

/// Unpack the two 32-bit request indices.
fn kerning_requests(pair: u64) -> [usize; 2] {
    [(pair >> 32) as usize, pair as u32 as usize]
}

/// Keep the first nonzero rounded value per mapped pair, sorted by request
/// indices and limited before reserving glyph ids. Values store FWORD bits.
fn prepare_kerning(
    pairs: &[Kerning],
    request_gids: &[usize],
    to_font_units: impl Fn(f64) -> f64,
) -> Vec<(u64, usize)> {
    let is_mapped = |i: usize| request_gids.get(i).is_some_and(|&gid| gid != 0);
    let mut pairs: Vec<_> = pairs
        .iter()
        .filter_map(|pair| {
            let value = fword(to_font_units(pair.value));
            (value != 0 && is_mapped(pair.left) && is_mapped(pair.right)).then_some((
                ((pair.left as u64) << 32) | pair.right as u64,
                value as u16 as usize,
            ))
        })
        .collect();
    sort_by_code(&mut pairs);
    pairs.dedup_by_key(|pair| pair.0);
    pairs.truncate(MAX_KERNING_PAIRS);
    pairs
}

/// Share empty glyphs by advance, excluding .notdef and kerned requests.
fn share_glyphs(
    glyphs: &mut Vec<OutGlyph>,
    cmap: &mut [(u32, usize)],
    request_gids: &mut [usize],
    kerning: &[(u64, usize)],
) {
    let mut kerned = vec![false; request_gids.len()];
    for &(pair, _) in kerning {
        for request in kerning_requests(pair) {
            kerned[request] = true;
        }
    }
    // Each output glyph's advance and whether it can be shared.
    let mut advances: Vec<(u16, bool)> = Vec::new();
    for (_, gid) in cmap {
        let request = *gid - 1;
        let advance = glyphs[*gid].advance;
        let shareable = !kerned[request];
        *gid = 1 + match advances
            .iter()
            .position(|&(a, shared)| shared && a == advance)
        {
            Some(index) if shareable => index,
            _ => {
                advances.push((advance, shareable));
                advances.len() - 1
            }
        };
        request_gids[request] = *gid;
    }
    glyphs.truncate(1);
    glyphs.extend(advances.into_iter().map(|(advance, _)| OutGlyph {
        outline: None,
        advance,
    }));
}

fn make_names(sources: &[&GlyphData], name: &str, metrics: &Metrics) -> Vec<(u16, String)> {
    let subfamily = match (metrics.bold, metrics.italic) {
        (true, true) => "Bold Italic",
        (true, false) => "Bold",
        (false, true) => "Italic",
        _ => "Regular",
    };
    // PostScript names are restricted to printable ASCII, minus some chars.
    let ps_name: String = format!("{name}-{subfamily}")
        .chars()
        .filter(|&c| ('!'..='~').contains(&c) && !"[](){}<>/%".contains(c))
        .collect();
    let unique = |get: fn(&GlyphData) -> &String| {
        let mut values: Vec<&str> = Vec::new();
        for source in sources {
            let value = get(source).as_str();
            if !value.is_empty() && !values.contains(&value) {
                values.push(value);
            }
        }
        values
    };
    let copyright = unique(|s| &s.names.copyright).join("\n");
    let family = unique(|s| &s.names.family).join(", ");
    let names = &sources[0].names;
    [
        (0, copyright),
        (1, name.to_string()),
        (2, subfamily.to_string()),
        (3, format!("{ps_name};{family}")),
        (4, format!("{name} {subfamily}")),
        (5, "Version 1.0".to_string()),
        (6, ps_name.chars().take(63).collect()),
        (10, format!("Generated from: {family}.")),
        (13, names.license.clone()),
        (14, names.license_url.clone()),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .collect()
}

struct EncodedGlyph {
    x_min: i16,
    y_min: i16,
    x_max: i16,
    y_max: i16,
    num_points: usize,
}

/// A rounded coordinate clamped to +/-MAX_COORDINATE.
fn to_coordinate(v: f64) -> i64 {
    clamp(v.round(), -MAX_COORDINATE, MAX_COORDINATE) as i64
}

/// Writes a glyph in `w` (the glyf table), without padding.
fn encode_glyph(outline: &Outline, w: &mut Writer) -> EncodedGlyph {
    let xs: Vec<i64> = outline.xs.iter().map(|&v| to_coordinate(v)).collect();
    let ys: Vec<i64> = outline.ys.iter().map(|&v| to_coordinate(v)).collect();
    let n = xs.len();
    // The coordinates are bounded by MAX_COORDINATE.
    let x_min = *xs.iter().min().unwrap() as i16;
    let x_max = *xs.iter().max().unwrap() as i16;
    let y_min = *ys.iter().min().unwrap() as i16;
    let y_max = *ys.iter().max().unwrap() as i16;

    let mut flags = Vec::with_capacity(n);
    let mut x_bytes = Vec::new();
    let mut y_bytes = Vec::new();
    let (mut px, mut py) = (0, 0);
    for i in 0..n {
        let mut flag: u8 = if outline.on_curve[i] != 0 { 0x01 } else { 0 };
        let dx = xs[i] - px;
        let dy = ys[i] - py;
        px = xs[i];
        py = ys[i];
        for (d, bytes, short, same) in [
            (dx, &mut x_bytes, 0x02, 0x10),
            (dy, &mut y_bytes, 0x04, 0x20),
        ] {
            if d == 0 {
                flag |= same;
            } else if d.abs() < 256 {
                flag |= short | if d > 0 { same } else { 0 };
                bytes.push(d.unsigned_abs() as u8);
            } else {
                bytes.extend_from_slice(&(d as i16).to_be_bytes());
            }
        }
        flags.push(flag);
    }
    // Compress the flags using the repeat bit.
    let mut packed = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let flag = flags[i];
        let mut repeat = 0;
        while i + repeat + 1 < n && flags[i + repeat + 1] == flag && repeat < 255 {
            repeat += 1;
        }
        if repeat > 0 {
            packed.push(flag | 0x08);
            packed.push(repeat as u8);
        } else {
            packed.push(flag);
        }
        i += repeat + 1;
    }

    // The points and contours are bounded by MAX_POINTS and MAX_CONTOURS.
    w.i16(outline.end_pts.len() as i16);
    w.i16(x_min);
    w.i16(y_min);
    w.i16(x_max);
    w.i16(y_max);
    for &end in &outline.end_pts {
        w.u16(end as u16);
    }
    w.u16(0); // instructionLength
    w.bytes(&packed);
    w.bytes(&x_bytes);
    w.bytes(&y_bytes);
    EncodedGlyph {
        x_min,
        y_min,
        x_max,
        y_max,
        num_points: n,
    }
}

/// Stable sort by packed code.
fn sort_by_code(entries: &mut [(u64, usize)]) {
    entries.sort_by_key(|&(code, _)| code);
}

/// Valid cmap mappings, sorted by code using the same `(u64, usize)` sort
/// as kerning pairs (another instance of the stable sort would make the wasm
/// module 1 KB bigger).
fn cmap_entries(cmap: &[(u32, usize)]) -> Vec<(u32, usize)> {
    let mut entries: Vec<(u64, usize)> = cmap
        .iter()
        .filter(|&&(code, _)| code <= 0x10ffff)
        .map(|&(code, gid)| (code as u64, gid))
        .collect();
    sort_by_code(&mut entries);
    entries
        .into_iter()
        .map(|(code, gid)| (code as u32, gid))
        .collect()
}

/// Groups of consecutive codes mapped to consecutive glyphs.
fn cmap_groups(entries: &[(u32, usize)]) -> Vec<(u32, u32, usize)> {
    let mut groups: Vec<(u32, u32, usize)> = Vec::new();
    for &(code, gid) in entries {
        if let Some(last) = groups.last_mut() {
            if code == last.1 + 1 && gid == last.2 + (code - last.0) as usize {
                last.1 = code;
                continue;
            }
        }
        groups.push((code, code, gid));
    }
    groups
}

fn write_format12(groups: &[(u32, u32, usize)]) -> Vec<u8> {
    let length = 16 + 12 * groups.len();
    let mut w = Writer::new();
    w.u16(12);
    w.u16(0);
    w.u32(length as u32);
    w.u32(0); // language
    w.u32(groups.len() as u32);
    for &(start, end, gid) in groups {
        w.u32(start);
        w.u32(end);
        w.u32(gid as u32);
    }
    w.bytes
}

/// A format 4 subtable, from its segments (start, end, idDelta and the index
/// of their glyph ids, if any) without the final 0xFFFF one.
fn write_format4(segments: &[(u32, u32, i64, Option<usize>)], glyph_ids: &[usize]) -> Vec<u8> {
    let seg_count = segments.len() + 1;
    let mut w = Writer::new();
    w.u16(4);
    w.u16((16 + 8 * seg_count + 2 * glyph_ids.len()) as u16);
    w.u16(0); // language
    w.u16(2 * seg_count as u16);
    for value in search_fields(seg_count as u16, 2) {
        w.u16(value);
    }
    for &(_, end, _, _) in segments {
        w.u16(end as u16);
    }
    w.u16(0xffff);
    w.u16(0); // reservedPad
    for &(start, _, _, _) in segments {
        w.u16(start as u16);
    }
    w.u16(0xffff);
    for &(_, _, delta, _) in segments {
        w.u16(delta as u16); // Modulo 65536.
    }
    w.u16(1);
    // From each idRangeOffset to the glyph ids of its segment.
    for (i, &(_, _, _, index)) in segments.iter().enumerate() {
        w.u16(index.map_or(0, |index| 2 * (seg_count - i) + 2 * index) as u16);
    }
    w.u16(0);
    for &gid in glyph_ids {
        w.u16(gid as u16);
    }
    w.bytes
}

/// The cmap table, from its encoding records (platform, encoding and index of
/// the subtable) and its subtables.
fn write_cmap_table(records: &[(u16, u16, usize)], subtables: &[Vec<u8>]) -> Vec<u8> {
    let mut offsets = Vec::with_capacity(subtables.len());
    let mut offset = 4 + 8 * records.len();
    for subtable in subtables {
        offsets.push(offset);
        offset += subtable.len();
    }
    let mut w = Writer::new();
    w.u16(0);
    w.u16(records.len() as u16);
    for &(platform, encoding, index) in records {
        w.u16(platform);
        w.u16(encoding);
        w.u32(offsets[index] as u32);
    }
    for subtable in subtables {
        w.bytes(subtable);
    }
    w.bytes
}

fn write_cmap(cmap: &[(u32, usize)]) -> Vec<u8> {
    let groups = cmap_groups(&cmap_entries(cmap));

    // Format 4, for the BMP: with too many segments only the first ones are
    // kept and format 12 has all of them.
    let max_segments = (0xffff - 16) / 8 - 1;
    let mut segments = Vec::new();
    let mut truncated = false;
    for &(start, end, gid) in &groups {
        if start > 0xfffe {
            break;
        }
        if segments.len() == max_segments {
            truncated = true;
            break;
        }
        segments.push((start, end.min(0xfffe), gid as i64 - start as i64, None));
    }
    let mut subtables = vec![write_format4(&segments, &[])];

    // Format 12, only when format 4 isn't enough.
    if truncated || groups.last().is_some_and(|g| g.1 > 0xfffe) {
        subtables.push(write_format12(&groups));
        write_cmap_table(&[(0, 3, 0), (0, 4, 1), (3, 1, 0), (3, 10, 1)], &subtables)
    } else {
        write_cmap_table(&[(0, 3, 0), (3, 1, 0)], &subtables)
    }
}

/// A smaller cmap when many codes share a few glyphs (see
/// `Options::metrics_only`): format 4 with glyph ids arrays (and format 12
/// for the codes beyond the BMP), in Windows subtables only. None when the
/// format 4 subtable would be too big.
fn write_compact_cmap(cmap: &[(u32, usize)]) -> Option<Vec<u8>> {
    let entries = cmap_entries(cmap);
    // The segments of codes: a new one when more than 4 codes are skipped (a
    // segment takes 8 bytes, a glyph id 2).
    let mut segments: Vec<(u32, u32, i64, Option<usize>)> = Vec::new();
    let mut glyph_ids = Vec::new();
    for &(code, gid) in entries.iter().take_while(|&&(code, _)| code <= 0xfffe) {
        match segments.last_mut() {
            Some(last) if code - last.1 <= 5 => {
                glyph_ids.extend((last.1 + 1..code).map(|_| 0));
                last.1 = code;
            }
            _ => segments.push((code, code, 0, Some(glyph_ids.len()))),
        }
        glyph_ids.push(gid);
    }
    if 16 + 8 * (segments.len() + 1) + 2 * glyph_ids.len() > 0xffff {
        return None;
    }
    let mut subtables = vec![write_format4(&segments, &glyph_ids)];
    let groups = cmap_groups(&entries);
    Some(if groups.last().is_some_and(|g| g.1 > 0xfffe) {
        subtables.push(write_format12(&groups));
        write_cmap_table(&[(3, 1, 0), (3, 10, 1)], &subtables)
    } else {
        write_cmap_table(&[(3, 1, 0)], &subtables)
    })
}

fn write_name(names: &[(u16, String)]) -> Vec<u8> {
    let records: Vec<(u16, Vec<u8>)> = names
        .iter()
        .map(|(id, value)| {
            (
                *id,
                value.encode_utf16().flat_map(|c| c.to_be_bytes()).collect(),
            )
        })
        .collect();
    let string_offset = 6 + 12 * records.len();
    let mut w = Writer::new();
    w.u16(0);
    w.u16(records.len() as u16);
    w.u16(string_offset as u16);
    let mut offset = 0;
    for (id, data) in &records {
        w.u16(3); // Windows
        w.u16(1); // Unicode BMP
        w.u16(0x409); // English (US)
        w.u16(*id);
        w.u16(data.len() as u16);
        w.u16(offset as u16);
        offset += data.len();
    }
    for (_, data) in &records {
        w.bytes(data);
    }
    w.bytes
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for chunk in data.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

/// The fields of a binary search (searchRange, entrySelector, rangeShift) in
/// `n` entries of `size` bytes.
fn search_fields(n: u16, size: u16) -> [u16; 3] {
    let entry_selector = 15 - n.max(1).leading_zeros() as u16;
    let search_range = size << entry_selector;
    [search_range, entry_selector, n * size - search_range]
}

/// The `kern` table (version 0) of some pairs of glyph ids (left << 16 |
/// right), sorted, and their kerning (as a uint16): a format 0 subtable.
fn write_kern(pairs: &[(u32, usize)]) -> Vec<u8> {
    let n = pairs.len() as u16;
    let mut w = Writer::new();
    w.u16(0); // version
    w.u16(1); // nTables
    w.u16(0); // subtable version
    w.u16(14 + 6 * n); // length
    w.u16(1); // coverage: horizontal, format 0
    w.u16(n);
    for value in search_fields(n, 6) {
        w.u16(value);
    }
    for &(gids, value) in pairs {
        w.u32(gids);
        w.u16(value as u16);
    }
    w.bytes
}

/// Writes the font: `compact` is for a font without outlines (see
/// `Options::metrics_only`), with short offsets in `loca` (if they fit) and a
/// compact cmap.
fn write_font(
    glyphs: &[OutGlyph],
    cmap: &[(u32, usize)],
    kerning: &[(u32, usize)],
    metrics: &Metrics,
    names: &[(u16, String)],
    compact: bool,
) -> Vec<u8> {
    let upm = metrics.units_per_em as f64;
    let num_glyphs = glyphs.len();

    // glyf, loca and hmtx, with the bounds of the outlines and the extrema of
    // their side bearings.
    // At most a header, the contour ends, and 5 bytes per point (a flag and
    // two coordinates), with the padding.
    let glyf_capacity = (glyphs.iter().filter_map(|glyph| glyph.outline.as_ref()))
        .map(|outline| 12 + 2 * outline.end_pts.len() + 5 * outline.xs.len() + 3)
        .sum();
    let mut glyf = Writer::with_capacity(glyf_capacity);
    let mut loca = Writer::new();
    let mut hmtx = Writer::new();
    let (mut x_min, mut y_min, mut x_max, mut y_max) = (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
    let (mut min_lsb, mut min_rsb, mut x_max_extent) = (i16::MAX, i16::MAX, i16::MIN);
    let mut advance_width_max = 0;
    let (mut max_points, mut max_contours) = (0, 0);
    let (mut width_sum, mut width_count) = (0, 0);
    let mut offsets = Vec::with_capacity(num_glyphs + 1);
    for glyph in glyphs {
        let advance = glyph.advance;
        offsets.push(glyf.len());
        advance_width_max = advance_width_max.max(advance);
        if advance > 0 {
            width_sum += u32::from(advance);
            width_count += 1;
        }
        hmtx.u16(advance);
        let Some(outline) = &glyph.outline else {
            hmtx.i16(0);
            continue;
        };
        let encoded = encode_glyph(outline, &mut glyf);
        glyf.pad();
        hmtx.i16(encoded.x_min);
        x_min = x_min.min(encoded.x_min);
        y_min = y_min.min(encoded.y_min);
        x_max = x_max.max(encoded.x_max);
        y_max = y_max.max(encoded.y_max);
        min_lsb = min_lsb.min(encoded.x_min);
        // The advances and the coordinates are bounded by MAX_COORDINATE.
        min_rsb = min_rsb.min(advance as i16 - encoded.x_max);
        x_max_extent = x_max_extent.max(encoded.x_max);
        max_points = max_points.max(encoded.num_points);
        max_contours = max_contours.max(outline.end_pts.len());
    }
    offsets.push(glyf.len());
    // The offsets are even (the glyphs are padded).
    let short_loca = compact && glyf.len() / 2 <= 0xffff;
    for offset in offsets {
        if short_loca {
            loca.u16((offset / 2) as u16);
        } else {
            loca.u32(offset as u32);
        }
    }
    // Keep glyf nonempty even when every loca offset is zero.
    if glyf.len() == 0 {
        glyf.zeros(1);
    }
    if x_min > x_max {
        // No outline.
        (x_min, y_min, x_max, y_max) = (0, 0, 0, 0);
        (min_lsb, min_rsb, x_max_extent) = (0, 0, 0);
    }

    let mut head = Writer::new();
    head.u16(1);
    head.u16(0);
    head.fixed(1.0);
    head.u32(0); // checksumAdjustment, set below.
    head.u32(0x5f0f_3cf5);
    head.u16(0x0003); // Baseline at y=0; each glyph's lsb equals xMin.
    head.u16(metrics.units_per_em);
    // created and modified: 2024-01-01 (seconds since 1904-01-01).
    for _ in 0..2 {
        head.u32(0);
        head.u32(3_786_912_000);
    }
    head.i16(x_min);
    head.i16(y_min);
    head.i16(x_max);
    head.i16(y_max);
    head.u16(u16::from(metrics.bold) | (u16::from(metrics.italic) << 1));
    head.u16(8); // lowestRecPPEM
    head.i16(2); // fontDirectionHint
    head.i16(if short_loca { 0 } else { 1 }); // indexToLocFormat
    head.i16(0);

    let mut hhea = Writer::new();
    hhea.u16(1);
    hhea.u16(0);
    hhea.i16(metrics.ascender);
    hhea.i16(metrics.descender);
    hhea.i16(metrics.line_gap);
    hhea.u16(advance_width_max);
    hhea.i16(min_lsb);
    hhea.i16(min_rsb);
    hhea.i16(x_max_extent);
    hhea.fword(if metrics.italic { upm } else { 1.0 }); // caretSlopeRise
    hhea.fword(if metrics.italic {
        upm * metrics.slant
    } else {
        0.0
    });
    hhea.i16(0); // caretOffset
    hhea.zeros(8); // reserved
    hhea.i16(0); // metricDataFormat
    hhea.u16(num_glyphs as u16); // Bounded by MAX_GLYPHS.

    let mut maxp = Writer::new();
    maxp.u32(0x0001_0000);
    maxp.u16(num_glyphs as u16);
    maxp.u16(max_points as u16);
    maxp.u16(max_contours as u16);
    maxp.u16(0); // maxCompositePoints
    maxp.u16(0); // maxCompositeContours
    maxp.u16(2); // maxZones
                 // maxTwilightPoints, maxStorage, maxFunctionDefs, maxInstructionDefs,
                 // maxStackElements, maxSizeOfInstructions, maxComponentElements and
                 // maxComponentDepth are all 0.
    maxp.zeros(16);

    let mut os2 = Writer::new();
    os2.u16(4); // version
    os2.fword(if width_count > 0 {
        width_sum as f64 / width_count as f64
    } else {
        0.0
    });
    os2.u16(metrics.weight);
    os2.u16(5); // usWidthClass: normal
    os2.u16(0); // fsType: installable
    for v in [0.65, 0.6, 0.0, 0.075, 0.65, 0.6, 0.0, 0.35] {
        os2.fword(v * upm); // Sub/superscript sizes and offsets.
    }
    // yStrikeoutSize and yStrikeoutPosition.
    os2.fword(known(metrics.underline_thickness).unwrap_or(upm / 20.0));
    os2.fword(known(metrics.x_height as f64 / 2.0).unwrap_or(upm / 4.0));
    os2.i16(0); // sFamilyClass
    os2.zeros(10); // panose
    os2.zeros(16); // ulUnicodeRange1-4
    os2.bytes(b"PDFJ"); // achVendID
    let mut fs_selection = 0x80; // USE_TYPO_METRICS
    if metrics.italic {
        fs_selection |= 0x01;
    }
    if metrics.bold {
        fs_selection |= 0x20;
    }
    if !metrics.italic && !metrics.bold {
        fs_selection |= 0x40;
    }
    os2.u16(fs_selection);
    let first = cmap.iter().map(|&(code, _)| code).min();
    let last = cmap.iter().map(|&(code, _)| code).max();
    os2.u16(first.map_or(0, |c| c.min(0xffff) as u16));
    os2.u16(last.map_or(0, |c| c.min(0xffff) as u16));
    os2.i16(metrics.ascender);
    os2.i16(metrics.descender);
    os2.i16(metrics.line_gap);
    // usWinAscent and usWinDescent.
    os2.u16(metrics.ascender.max(y_max).max(0) as u16);
    os2.u16(
        (-i32::from(metrics.descender))
            .max(-i32::from(y_min))
            .max(0) as u16,
    );
    os2.u32(1); // ulCodePageRange1: Latin 1
    os2.u32(0);
    os2.i16(metrics.x_height);
    os2.i16(metrics.cap_height);
    os2.u16(0); // usDefaultChar
    os2.u16(0x20); // usBreakChar
    os2.u16(0); // usMaxContext

    let mut post = Writer::new();
    post.u32(0x0003_0000);
    post.fixed(metrics.italic_angle);
    post.i16(metrics.underline_position);
    post.i16(metrics.underline_thickness);
    post.u32(u32::from(metrics.fixed_pitch));
    post.zeros(16);

    // Sorted by tag, without the empty ones (only `kern` can be).
    let tables: [(&[u8; 4], Vec<u8>); 11] = [
        (b"OS/2", os2.bytes),
        (
            b"cmap",
            (compact.then(|| write_compact_cmap(cmap)).flatten())
                .unwrap_or_else(|| write_cmap(cmap)),
        ),
        (b"glyf", glyf.bytes),
        (b"head", head.bytes),
        (b"hhea", hhea.bytes),
        (b"hmtx", hmtx.bytes),
        (
            b"kern",
            if kerning.is_empty() {
                Vec::new()
            } else {
                write_kern(kerning)
            },
        ),
        (b"loca", loca.bytes),
        (b"maxp", maxp.bytes),
        (b"name", write_name(names)),
        (b"post", post.bytes),
    ];

    let num_tables = tables.iter().filter(|(_, data)| !data.is_empty()).count();
    let length = tables
        .iter()
        .map(|(_, data)| (data.len() + 3) & !3)
        .sum::<usize>();
    let mut w = Writer::with_capacity(12 + 16 * num_tables + length);
    w.u32(0x0001_0000);
    w.u16(num_tables as u16);
    for value in search_fields(num_tables as u16, 16) {
        w.u16(value);
    }
    let mut offset = 12 + 16 * num_tables;
    let mut head_offset = 0;
    for (tag, data) in &tables {
        if data.is_empty() {
            continue;
        }
        w.bytes(*tag);
        w.u32(checksum(data));
        w.u32(offset as u32);
        w.u32(data.len() as u32);
        if *tag == b"head" {
            head_offset = offset;
        }
        offset += (data.len() + 3) & !3;
    }
    for (_, data) in &tables {
        if !data.is_empty() {
            w.bytes(data);
            w.pad();
        }
    }
    let mut font = w.bytes;
    let adjustment = 0xb1b0_afbau32.wrapping_sub(checksum(&font));
    font[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
    font
}
