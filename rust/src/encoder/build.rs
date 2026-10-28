// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Builds a glyph data file (see FORMAT.md) from TrueType fonts.
//!
//! A codepoint is taken from the first font having it (and having it in its
//! ranges). Only the glyphs reachable from the cmap are kept (plus the
//! components they use), without any hinting, glyph names or layout tables.
//! The stems are measured, to embolden the outlines at runtime.

use super::font::{self, Point, SourceComponent, SourceFont, SourceGlyph};
use super::search::EncoderOptions;
use crate::glyph_data::{
    GlyphData, COMPRESSION_DEFLATE, COMPRESSION_MODEL, COMPRESSION_NONE, FLAG_FIXED_PITCH,
    FLAG_SERIF, MAGIC, VERSION,
};
use crate::payload::{self, write_triplet, Writer};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};

/// The compression of the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Adaptive contour model (see `model.rs`).
    Model,
    /// Zlib compression.
    Deflate,
    None,
}

/// The glyphs measured to get the stems.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stems {
    /// "l", "I" and the crossbar of "H".
    Latin,
    /// The vertical and horizontal strokes of the ideographs.
    Cjk,
}

/// A font and the ranges of the codepoints taken from it.
#[derive(Clone, Debug)]
pub struct FontSpec {
    pub path: PathBuf,
    /// Comma-separated hexadecimal ranges ("0-24F,370-3FF"): all the
    /// codepoints of the font when None.
    pub ranges: Option<String>,
}

impl FontSpec {
    /// Parses `path[:ranges]` (the `--fallback` option of `build_glyph_data`).
    pub fn parse(spec: &str) -> Self {
        let is_range = |c: char| c.is_ascii_hexdigit() || c == ',' || c == '-';
        for (i, c) in spec.char_indices() {
            if c == ':' && spec[i + 1..].chars().all(is_range) {
                return FontSpec {
                    path: spec[..i].into(),
                    ranges: Some(spec[i + 1..].to_string()),
                };
            }
        }
        FontSpec {
            path: spec.into(),
            ranges: None,
        }
    }
}

/// The options of a build (see also the `build_glyph_data` tool).
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// The primary font: a codepoint is taken from the first font having it
    /// (and having it in its ranges), the metrics from the primary one.
    pub font: FontSpec,
    pub fallback: Vec<FontSpec>,
    /// The font of the other weight of the family (the bold one of a regular
    /// font, else the regular one): the difference of their stems gives the
    /// emboldening strength.
    pub reference: Option<PathBuf>,
    /// Coordinate quantization step in primary-font units (1..=255).
    pub grid: u8,
    pub family: Option<String>,
    pub serif: bool,
    /// From the `post` table of the primary font when None.
    pub fixed_pitch: Option<bool>,
    /// The codepoints covered by all these fonts are skipped.
    pub exclude_covered_by: Vec<PathBuf>,
    /// The codepoints whose glyphs (and their components) come first, so that
    /// a decoder needing only them can stop after them (e.g. the characters
    /// of the common 8-bit encodings).
    pub front: Vec<u32>,
    pub stems: Stems,
    pub codec: Codec,
    pub encoder: EncoderOptions,
    /// Verify decoded outlines and advances against the source fonts.
    pub check: bool,
}

impl BuildOptions {
    pub fn new(font: impl Into<PathBuf>) -> Self {
        BuildOptions {
            font: FontSpec {
                path: font.into(),
                ranges: None,
            },
            fallback: Vec::new(),
            reference: None,
            grid: 1,
            family: None,
            serif: false,
            fixed_pitch: None,
            exclude_covered_by: Vec::new(),
            front: Vec::new(),
            stems: Stems::Latin,
            codec: Codec::Model,
            encoder: EncoderOptions::default(),
            check: true,
        }
    }
}

/// Statistics of a build.
#[derive(Clone, Debug, Default)]
pub struct BuildStats {
    pub glyphs: usize,
    pub codepoints: usize,
    pub aliases: usize,
    pub points: usize,
    pub stem_v: i64,
    pub stem_h: i64,
    pub reference_stem_v: i64,
    pub reference_stem_h: i64,
    /// Sizes of the streams of the payload.
    pub streams: [usize; 7],
    pub payload: usize,
    pub file: usize,
}

struct Font {
    source: SourceFont,
    ranges: Option<Vec<(u32, u32)>>,
}

fn load(path: &Path) -> Result<SourceFont, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    font::read(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse_ranges(ranges: Option<&str>) -> Result<Option<Vec<(u32, u32)>>, String> {
    let Some(ranges) = ranges.filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    let parse = |part: &str| -> Option<(u32, u32)> {
        let mut values = part.split('-').map(|v| u32::from_str_radix(v, 16).ok());
        let start = values.next()??;
        let end = values.next().unwrap_or(Some(start))?;
        (end >= start && values.next().is_none()).then_some((start, end))
    };
    ranges
        .split(',')
        .map(|part| parse(part).ok_or_else(|| format!("Invalid range: {part}")))
        .collect::<Result<_, _>>()
        .map(Some)
}

/// Require copyright and license names, and check whether the license text
/// mentions the Open Font License.
fn is_under_ofl(names: &HashMap<u16, String>) -> Result<bool, String> {
    let name = |id| names.get(&id).filter(|value| !value.is_empty());
    match (name(0), name(13)) {
        (Some(_), Some(license)) => Ok(license.to_lowercase().contains("open font license")),
        _ => Err("No copyright notice or license (Windows English names).".to_string()),
    }
}

/// A component normalized to: point' = M * point + d.
struct Resolved {
    glyph_index: u32,
    matrix: [f64; 4],
    dx: f64,
    dy: f64,
}

const MAX_DEPTH: usize = 8;

/// Resolves the point matching and the scaled component offset flag.
fn resolve_components(
    font: &SourceFont,
    components: &[SourceComponent],
    depth: usize,
) -> Result<Vec<Resolved>, String> {
    let mut resolved = Vec::with_capacity(components.len());
    let mut parent_points: Vec<(f64, f64)> = Vec::new();
    for component in components {
        let [a, b, c, d] = component.matrix;
        let child_points: Vec<Point> = source_outline(font, component.glyph_index, depth)?
            .into_iter()
            .flatten()
            .collect();
        let (dx, dy) = if component.xy_values {
            let (x, y) = (component.arg1 as f64, component.arg2 as f64);
            if component.scaled_offset {
                (a * x + c * y, b * x + d * y)
            } else {
                (x, y)
            }
        } else {
            let parent = parent_points.get(component.arg1 as usize);
            let child = child_points.get(component.arg2 as usize);
            let (Some(&(px, py)), Some(child)) = (parent, child) else {
                return Err("Invalid point matching in composite glyph.".to_string());
            };
            (
                px - (a * child.x + c * child.y),
                py - (b * child.x + d * child.y),
            )
        };
        for p in &child_points {
            parent_points.push((a * p.x + c * p.y + dx, b * p.x + d * p.y + dy));
        }
        resolved.push(Resolved {
            glyph_index: component.glyph_index,
            matrix: [a, b, c, d],
            dx,
            dy,
        });
    }
    Ok(resolved)
}

/// Flattens composite components into contours for point matching,
/// metric measurements and round-trip verification.
fn source_outline(font: &SourceFont, gid: u32, depth: usize) -> Result<Vec<Vec<Point>>, String> {
    let glyph = match font.glyphs.get(gid as usize) {
        Some(glyph) if depth <= MAX_DEPTH => glyph,
        _ => return Ok(Vec::new()),
    };
    Ok(match glyph {
        SourceGlyph::Empty => Vec::new(),
        SourceGlyph::Simple(contours) => contours.clone(),
        SourceGlyph::Composite(components) => {
            let mut result = Vec::new();
            for component in resolve_components(font, components, depth + 1)? {
                let [a, b, c, d] = component.matrix;
                for mut contour in source_outline(font, component.glyph_index, depth + 1)? {
                    for p in &mut contour {
                        let (x, y) = (p.x, p.y);
                        p.x = a * x + c * y + component.dx;
                        p.y = b * x + d * y + component.dy;
                    }
                    result.push(contour);
                }
            }
            result
        }
    })
}

/// Intersections of an outline with the line `axis = position`, the outline
/// being flattened.
fn intersections(contours: &[Vec<Point>], vertical_axis: bool, position: f64) -> Vec<f64> {
    let coord = |p: &Point| if vertical_axis { p.y } else { p.x };
    let other = |p: &Point| if vertical_axis { p.x } else { p.y };
    let mut result = Vec::new();
    let mut add_segment = |p0: &Point, p1: &Point| {
        let a = coord(p0) - position;
        let b = coord(p1) - position;
        if (a < 0.0 && b >= 0.0) || (a >= 0.0 && b < 0.0) {
            result.push(other(p0) + ((other(p1) - other(p0)) * a) / (a - b));
        }
    };
    for contour in contours {
        // Insert the on-curve midpoints implied by consecutive off-curve points.
        let n = contour.len();
        let mut pts = Vec::with_capacity(n * 2);
        for i in 0..n {
            let (p, next) = (contour[i], contour[(i + 1) % n]);
            pts.push(p);
            if !p.on && !next.on {
                pts.push(Point {
                    x: (p.x + next.x) / 2.0,
                    y: (p.y + next.y) / 2.0,
                    on: true,
                });
            }
        }
        let Some(start) = pts.iter().position(|p| p.on) else {
            continue;
        };
        let m = pts.len();
        let mut prev = pts[start];
        let mut i = 1;
        while i <= m {
            let p = pts[(start + i) % m];
            if p.on {
                add_segment(&prev, &p);
                prev = p;
                i += 1;
                continue;
            }
            let end = pts[(start + i + 1) % m];
            let steps = 32;
            let mut last = prev;
            for s in 1..=steps {
                let t = s as f64 / steps as f64;
                let u = 1.0 - t;
                let q = Point {
                    x: u * u * prev.x + 2.0 * u * t * p.x + t * t * end.x,
                    y: u * u * prev.y + 2.0 * u * t * p.y + t * t * end.y,
                    on: true,
                };
                add_segment(&last, &q);
                last = q;
            }
            prev = end;
            i += 2;
        }
    }
    result.sort_by(|a, b| a.partial_cmp(b).unwrap());
    result
}

/// Measures the vertical and horizontal stems: these values are used at
/// runtime to compute how much the outlines must be emboldened. The vertical
/// stems are measured in the middle of the glyph height, the horizontal ones
/// in the middle of the advance (see `Stems` for the glyphs).
fn measure_stems(font: &SourceFont, kind: Stems) -> Result<(i64, i64), String> {
    let (vertical, horizontal): (&[u32], &[u32]) = match kind {
        Stems::Latin => (&[0x6c, 0x49], &[0x48]),
        Stems::Cjk => (&[0x4e28], &[0x4e00]),
    };
    let span = |cps: &[u32], vertical_axis: bool| -> Result<i64, String> {
        for &cp in cps {
            let Some(gid) = font.glyph_id(cp) else {
                continue;
            };
            let contours = source_outline(font, gid, 0)?;
            let position = if vertical_axis {
                let ys = contours.iter().flatten().map(|p| p.y);
                let min = ys.clone().fold(f64::INFINITY, f64::min);
                let max = ys.fold(f64::NEG_INFINITY, f64::max);
                (min + max) / 2.0
            } else {
                font.advances.get(gid as usize).copied().unwrap_or(0) as f64 / 2.0
            };
            let values = intersections(&contours, vertical_axis, position);
            if values.len() >= 2 {
                return Ok(f64::round(values[1] - values[0]) as i64);
            }
        }
        Ok(0)
    };
    Ok((span(vertical, true)?, span(horizontal, false)?))
}

/// The x-height and cap height, measured when the OS/2 table doesn't have
/// them.
fn source_metrics(font: &SourceFont) -> Result<(f64, f64), String> {
    let y_max_of = |cp: u32| -> Result<f64, String> {
        let Some(gid) = font.glyph_id(cp) else {
            return Ok(0.0);
        };
        let contours = source_outline(font, gid, 0)?;
        Ok(contours.iter().flatten().map(|p| p.y).fold(0.0, f64::max))
    };
    let mut x_height = font.x_height as f64;
    let mut cap_height = font.cap_height as f64;
    if x_height == 0.0 {
        x_height = y_max_of('x' as u32)?;
    }
    if cap_height == 0.0 {
        cap_height = y_max_of('H' as u32)?;
    }
    Ok((x_height, cap_height))
}

/// Hash integer coordinates and flags, with contour separators.
fn hash_contours(contours: &[Vec<Point>]) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for contour in contours {
        for p in contour {
            for v in [p.x as i64, p.y as i64, i64::from(p.on), 0x5f] {
                h = (h ^ (v as u32 & 0xffff)).wrapping_mul(0x0100_0193);
            }
        }
        h = (h ^ 0x7c).wrapping_mul(0x0100_0193);
    }
    h
}

/// An int16 of the header (the measured heights can be fractional: they're
/// truncated towards zero).
fn header_i16(w: &mut Writer, v: f64) -> Result<(), String> {
    if !(-32768.0..=32767.0).contains(&v) {
        return Err(format!("Value out of int16 range: {v}"));
    }
    w.u16(v.trunc() as i64 as u16);
    Ok(())
}

/// A glyph in one of the source fonts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Key {
    font: usize,
    gid: u32,
}

struct Built {
    data: Vec<u8>,
    stats: BuildStats,
    fonts: Vec<Font>,
    cmap: HashMap<u32, Key>,
}

fn build(options: &BuildOptions) -> Result<Built, String> {
    let fonts = std::iter::once(&options.font)
        .chain(&options.fallback)
        .map(|spec| {
            Ok(Font {
                source: load(&spec.path)?,
                ranges: parse_ranges(spec.ranges.as_deref())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let primary = &fonts[0].source;
    let units_per_em = primary.units_per_em;
    let grid = options.grid;
    if grid == 0 {
        return Err("The grid must be in [1, 255].".to_string());
    }
    let glyph = |key: Key| &fonts[key.font].source.glyphs[key.gid as usize];
    let scale_of = |key: Key| units_per_em as f64 / fonts[key.font].source.units_per_em as f64;

    // Collect the codepoints: the first font mapping a codepoint wins.
    let mut cmap: HashMap<u32, Key> = HashMap::new();
    for (index, font) in fonts.iter().enumerate() {
        for &(cp, gid) in &font.source.cmap {
            let in_ranges = font
                .ranges
                .as_ref()
                .is_none_or(|ranges| ranges.iter().any(|&(a, b)| cp >= a && cp <= b));
            if in_ranges && (gid as usize) < font.source.glyphs.len() {
                cmap.entry(cp).or_insert(Key { font: index, gid });
            }
        }
    }
    // Skip the codepoints covered by all the given fonts (useful for a fallback
    // font, e.g. with symbols, used along with some text fonts).
    if !options.exclude_covered_by.is_empty() {
        let covering = options
            .exclude_covered_by
            .iter()
            .map(|path| load(path))
            .collect::<Result<Vec<_>, _>>()?;
        cmap.retain(|cp, _| !covering.iter().all(|f| f.cmap_index.contains_key(cp)));
    }
    let mut codepoints: Vec<u32> = cmap.keys().copied().collect();
    codepoints.sort_unstable();

    // Identical simple glyphs (same outline and advance) are merged.
    let mut canonical: HashMap<Key, Key> = HashMap::new();
    let mut shapes: HashMap<(u32, u64, u64), Vec<Key>> = HashMap::new();
    let no_contours: Vec<Vec<Point>> = Vec::new();
    let contours_of = |key: Key| match glyph(key) {
        SourceGlyph::Simple(contours) => contours,
        _ => &no_contours,
    };
    for cp in &codepoints {
        let key = cmap[cp];
        if canonical.contains_key(&key) {
            continue;
        }
        if let SourceGlyph::Composite(_) = glyph(key) {
            canonical.insert(key, key);
            continue;
        }
        let scale = scale_of(key);
        let font = &fonts[key.font].source;
        let advance = font.advances[key.gid as usize] as f64 * scale;
        let contours = contours_of(key);
        let hash = (hash_contours(contours), advance.to_bits(), scale.to_bits());
        let candidates = shapes.entry(hash).or_default();
        let same = candidates
            .iter()
            .copied()
            .find(|&other| contours == contours_of(other));
        if same.is_none() {
            candidates.push(key);
        }
        canonical.insert(key, same.unwrap_or(key));
    }

    // The front: the glyphs of the front codepoints and their components,
    // the latter adding their codepoints (if they have some) to the front.
    let mut first_cp: HashMap<Key, u32> = HashMap::new();
    for &cp in &codepoints {
        first_cp.entry(canonical[&cmap[&cp]]).or_insert(cp);
    }
    let mut front: HashSet<u32> = options
        .front
        .iter()
        .copied()
        .filter(|cp| cmap.contains_key(cp))
        .collect();
    let mut front_components: Vec<Key> = Vec::new();
    let mut pending: Vec<Key> = front.iter().map(|cp| canonical[&cmap[cp]]).collect();
    let mut visited: HashSet<Key> = HashSet::new();
    while let Some(key) = pending.pop() {
        if !visited.insert(key) {
            continue;
        }
        match first_cp.get(&key) {
            Some(&cp) => {
                front.insert(cp);
            }
            None => front_components.push(key),
        }
        if let SourceGlyph::Composite(components) = glyph(key) {
            let font = &fonts[key.font].source;
            for component in resolve_components(font, components, 0)? {
                pending.push(Key {
                    gid: component.glyph_index,
                    ..key
                });
            }
        }
    }
    front_components.sort_unstable();
    // The codepoints in the order of their glyphs: the front, then the other
    // ones.
    let (mut front_cps, other_cps): (Vec<u32>, Vec<u32>) =
        codepoints.iter().partition(|cp| front.contains(cp));
    front_cps.extend(other_cps);
    let ordered_cps = front_cps;

    // New glyph ids: the components of the front without codepoint, the
    // glyphs by their first codepoint (in the order above), then the other
    // glyphs only used as components.
    let mut new_ids: HashMap<Key, usize> = HashMap::new();
    let mut order: Vec<Key> = Vec::new();
    for key in front_components.iter().copied() {
        new_ids.insert(key, order.len());
        order.push(key);
    }
    let first_gid = order.len();
    for cp in &ordered_cps {
        let key = canonical[&cmap[cp]];
        if let Entry::Vacant(e) = new_ids.entry(key) {
            e.insert(order.len());
            order.push(key);
        }
    }
    let mut resolved: HashMap<Key, Vec<Resolved>> = HashMap::new();
    let mut i = 0;
    while i < order.len() {
        let key = order[i];
        i += 1;
        let SourceGlyph::Composite(components) = glyph(key) else {
            continue;
        };
        let font = &fonts[key.font].source;
        let components = resolve_components(font, components, 0)?;
        for component in &components {
            let component_key = Key {
                gid: component.glyph_index,
                ..key
            };
            if let Entry::Vacant(e) = new_ids.entry(component_key) {
                e.insert(order.len());
                order.push(component_key);
            }
        }
        resolved.insert(key, components);
    }

    // cmap: runs of consecutive codepoints mapped to new consecutive glyphs
    // (from first_gid) and a list of codepoints mapped to an already seen
    // glyph.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    let mut aliases: Vec<(u32, usize)> = Vec::new();
    let mut next_fresh = first_gid;
    for &cp in &ordered_cps {
        let gid = new_ids[&canonical[&cmap[&cp]]];
        if gid == next_fresh {
            next_fresh += 1;
            match runs.last_mut() {
                Some(last) if last.1 + 1 == cp => last.1 = cp,
                _ => runs.push((cp, cp)),
            }
        } else {
            aliases.push((cp, gid));
        }
    }
    let mut streams: [Writer; 7] = Default::default();
    let [cmap_stream, glyph_infos, advances, contours, flags, coordinates, components] =
        &mut streams;
    aliases.sort_unstable();
    cmap_stream.varint(first_gid as u64)?;
    cmap_stream.varint(runs.len() as u64)?;
    let mut prev_end = 0;
    for &(start, end) in &runs {
        // The starts wrap around (after the front).
        cmap_stream.varint((start + 0x110000 - prev_end) as u64 % 0x110000)?;
        cmap_stream.varint((end - start) as u64)?;
        prev_end = end + 1;
    }
    cmap_stream.varint(aliases.len() as u64)?;
    let mut prev_cp = 0;
    for &(cp, gid) in &aliases {
        cmap_stream.varint((cp - prev_cp) as u64)?;
        cmap_stream.varint(gid as u64)?;
        prev_cp = cp;
    }

    let to_grid = |v: f64, scale: f64| f64::round((v * scale) / grid as f64) as i64;
    let mut num_points = 0;
    for &key in &order {
        let font = &fonts[key.font].source;
        let gid = key.gid;
        let scale = scale_of(key);
        advances.varint(f64::round(font.advances[gid as usize] as f64 * scale) as u64)?;
        match glyph(key) {
            SourceGlyph::Empty => glyph_infos.varint(0)?,
            SourceGlyph::Simple(list) => {
                glyph_infos.varint(list.len() as u64 * 2)?;
                let (mut x, mut y) = (0, 0);
                for contour in list {
                    contours.varint(contour.len() as u64 - 1)?;
                    for p in contour {
                        let (px, py) = (to_grid(p.x, scale), to_grid(p.y, scale));
                        write_triplet(flags, coordinates, p.on, px - x, py - y)?;
                        (x, y) = (px, py);
                        num_points += 1;
                    }
                }
            }
            SourceGlyph::Composite(_) => {
                let list = &resolved[&key];
                glyph_infos.varint(list.len() as u64 * 2 + 1)?;
                for component in list {
                    let [a, b, c, d] = component.matrix.map(|v| f64::round(v * 16384.0) as i64);
                    let transform = if b == 0 && c == 0 {
                        if a == d {
                            u64::from(a != 16384)
                        } else {
                            2
                        }
                    } else {
                        3
                    };
                    let component_key = Key {
                        gid: component.glyph_index,
                        ..key
                    };
                    components.varint(new_ids[&component_key] as u64 * 4 + transform)?;
                    components.zigzag(to_grid(component.dx, scale))?;
                    components.zigzag(to_grid(component.dy, scale))?;
                    let values: &[i64] = match transform {
                        1 => &[a],
                        2 => &[a, d],
                        3 => &[a, b, c, d],
                        _ => &[],
                    };
                    for &v in values {
                        components.i16(v)?;
                    }
                }
            }
        }
    }

    let (x_height, cap_height) = source_metrics(primary)?;
    let (stem_v, stem_h) = measure_stems(primary, options.stems)?;
    let (mut reference_stem_v, mut reference_stem_h) = (0, 0);
    if let Some(path) = &options.reference {
        let reference = load(path)?;
        let scale = units_per_em as f64 / reference.units_per_em as f64;
        let (v, h) = measure_stems(&reference, options.stems)?;
        reference_stem_v = f64::round(v as f64 * scale) as i64;
        reference_stem_h = f64::round(h as f64 * scale) as i64;
    }
    let mut header_flags = 0;
    if options.serif {
        header_flags |= FLAG_SERIF;
    }
    if options.fixed_pitch.unwrap_or(primary.is_fixed_pitch) {
        header_flags |= FLAG_FIXED_PITCH;
    }

    // The copyright notices of all the fonts providing glyphs must be kept.
    let used: HashSet<usize> = order.iter().map(|&key| key.font).collect();
    let unique = |name_id: u16| {
        let mut values: Vec<&str> = Vec::new();
        for (index, font) in fonts.iter().enumerate() {
            if let Some(value) = font.source.names.get(&name_id) {
                if used.contains(&index) && !value.is_empty() && !values.contains(&value.as_str()) {
                    values.push(value);
                }
            }
        }
        values
    };
    let name = |id: u16| primary.names.get(&id).cloned();
    let strings = [
        options
            .family
            .clone()
            .or_else(|| name(16))
            .or_else(|| name(1))
            .unwrap_or_default(),
        unique(5).join(", "),
        unique(0).join("\n"),
        name(13).unwrap_or_default(),
        name(14).unwrap_or_default(),
    ];
    let mut used_paths: Vec<_> = used.iter().copied().collect();
    used_paths.sort_unstable();
    for index in used_paths {
        let spec = if index == 0 {
            &options.font
        } else {
            &options.fallback[index - 1]
        };
        let path = spec.path.display();
        if !is_under_ofl(&fonts[index].source.names).map_err(|e| format!("{path}: {e}"))? {
            eprintln!("Warning: {path} isn't explicitly under the OFL.");
        }
    }

    let mut payload_writer = Writer::default();
    payload_writer.u16(units_per_em);
    payload_writer.u8(grid);
    payload_writer.u8(header_flags);
    for v in [
        primary.ascender as f64,
        primary.descender as f64,
        primary.line_gap as f64,
        cap_height,
        x_height,
        primary.underline_position as f64,
        primary.underline_thickness as f64,
    ] {
        header_i16(&mut payload_writer, v)?;
    }
    payload_writer.u16(primary.weight_class);
    for v in [stem_v, stem_h, reference_stem_v, reference_stem_h] {
        payload_writer.u16(v as u16);
    }
    let payload =
        payload::serialize_streams(&payload_writer.data, order.len(), &strings, &streams)?;

    let mut file = MAGIC.to_vec();
    file.push(VERSION);
    match options.codec {
        Codec::Model => {
            file.push(COMPRESSION_MODEL);
            let coded = super::encode_model_payload(&payload, options.encoder);
            file.extend(coded.map_err(|e| e.to_string())?);
        }
        Codec::Deflate => {
            file.push(COMPRESSION_DEFLATE);
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
            encoder.write_all(&payload).map_err(|e| e.to_string())?;
            file.extend(encoder.finish().map_err(|e| e.to_string())?);
        }
        Codec::None => {
            file.push(COMPRESSION_NONE);
            file.extend_from_slice(&payload);
        }
    }

    let stats = BuildStats {
        glyphs: order.len(),
        codepoints: codepoints.len(),
        aliases: aliases.len(),
        points: num_points,
        stem_v,
        stem_h,
        reference_stem_v,
        reference_stem_h,
        streams: std::array::from_fn(|i| streams[i].data.len()),
        payload: payload.len(),
        file: file.len(),
    };
    Ok(Built {
        data: file,
        stats,
        fonts,
        cmap,
    })
}

/// Compare decoded outlines and advances with the sources, allowing the
/// quantization tolerance below.
fn verify(built: &Built, grid: u8) -> Result<(), String> {
    let data = &built.data;
    let glyph_data = if data[5] == COMPRESSION_DEFLATE {
        let mut payload = Vec::new();
        flate2::read::ZlibDecoder::new(&data[6..])
            .read_to_end(&mut payload)
            .map_err(|e| e.to_string())?;
        GlyphData::from_payload(payload::parse(&payload).map_err(|e| e.to_string())?)
    } else {
        GlyphData::new(data)
    }
    .map_err(|e| e.to_string())?;
    let fonts = &built.fonts;
    let units_per_em = fonts[0].source.units_per_em as f64;
    let mut codepoints: Vec<_> = built.cmap.iter().map(|(&cp, &key)| (cp, key)).collect();
    codepoints.sort_unstable();
    let mut errors = 0;
    for (cp, key) in codepoints {
        let gid = glyph_data
            .glyph_id(cp)
            .ok_or_else(|| format!("U+{cp:X} is missing."))?;
        let font = &fonts[key.font].source;
        let scale = units_per_em / font.units_per_em as f64;
        let expected = source_outline(font, key.gid, 0)?;
        let outline = glyph_data.outline(gid);
        let tolerance = if scale == 1.0 && grid == 1 {
            0.0
        } else {
            grid as f64 * 2.0
        };
        let mut ok = expected.len() == outline.end_pts.len();
        let mut i = 0;
        for (c, contour) in expected.iter().enumerate() {
            if !ok || outline.end_pts[c] != i + contour.len() - 1 {
                ok = false;
                break;
            }
            for p in contour {
                ok &= (outline.xs[i] - p.x * scale).abs() <= tolerance
                    && (outline.ys[i] - p.y * scale).abs() <= tolerance
                    && outline.on_curve[i] == u8::from(p.on);
                i += 1;
            }
        }
        let advance = f64::round(font.advances[key.gid as usize] as f64 * scale);
        if !ok || glyph_data.advance(gid) as f64 != advance {
            if errors < 10 {
                eprintln!("Mismatch for U+{cp:X}");
            }
            errors += 1;
        }
    }
    if errors > 0 {
        return Err(format!("{errors} glyphs don't round-trip."));
    }
    Ok(())
}

/// Build glyph data and, when `check` is true, verify it against the sources
/// with the quantization tolerance in `verify`.
pub fn build_glyph_data(options: &BuildOptions) -> Result<(Vec<u8>, BuildStats), String> {
    let built = build(options)?;
    if options.check {
        verify(&built, options.grid)?;
    }
    Ok((built.data, built.stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_font_specs() {
        let spec = FontSpec::parse("fonts/a:b.ttf:2500-259F,25a0");
        assert_eq!(spec.path, Path::new("fonts/a:b.ttf"));
        assert_eq!(spec.ranges.as_deref(), Some("2500-259F,25a0"));
        let spec = FontSpec::parse("fonts/a.ttf");
        assert_eq!(
            (spec.path.to_str(), spec.ranges),
            (Some("fonts/a.ttf"), None)
        );
        let spec = FontSpec::parse("a.ttf:");
        assert_eq!(spec.ranges.as_deref(), Some(""));
    }

    #[test]
    fn parses_the_ranges() {
        assert_eq!(parse_ranges(None), Ok(None));
        assert_eq!(parse_ranges(Some("")), Ok(None));
        assert_eq!(
            parse_ranges(Some("0-24F,370,3a0-3FF")),
            Ok(Some(vec![(0, 0x24f), (0x370, 0x370), (0x3a0, 0x3ff)]))
        );
        assert!(parse_ranges(Some("20-10")).is_err());
        assert!(parse_ranges(Some("1,,2")).is_err());
        assert!(parse_ranges(Some("41-42-43")).is_err());
    }

    #[test]
    fn requires_the_copyright_notice_and_the_license() {
        let names = |pairs: &[(u16, &str)]| -> HashMap<u16, String> {
            pairs.iter().map(|&(id, s)| (id, s.to_string())).collect()
        };
        let ofl = "This Font Software is licensed under the SIL Open Font License, Version 1.1.";
        assert_eq!(is_under_ofl(&names(&[(0, "©"), (13, ofl)])), Ok(true));
        assert_eq!(
            is_under_ofl(&names(&[(0, "©"), (13, "Apache 2.0")])),
            Ok(false)
        );
        assert!(is_under_ofl(&names(&[(13, ofl)])).is_err());
        assert!(is_under_ofl(&names(&[(0, "©"), (13, "")])).is_err());
    }
}
