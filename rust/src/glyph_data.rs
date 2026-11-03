// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Glyph data files (see FORMAT.md): metrics, cmap and outlines, for the
//! files compressed with the model codec (and the uncompressed ones, with the
//! `encoder` feature).

use crate::model::{self, DecodedGlyph, Payload, Shape};
#[cfg(feature = "encoder")]
use crate::payload;
use crate::Error;
use std::sync::{Mutex, MutexGuard};

/// Stable sort by code, shared by the cmap entries, the kerning pairs and the
/// scale bits: a merge sort, as the one of the standard library is
/// 6 KB of the wasm module and these sorts are small (a few thousand entries).
pub(crate) fn sort_by_code(entries: &mut [(u64, usize)]) {
    if entries.is_sorted_by_key(|&(code, _)| code) {
        return;
    }
    // Both buffers start with the same entries, so recursive calls can swap
    // their roles and merge the sorted halves directly into the destination.
    merge_sort(&mut entries.to_vec(), entries);
}

fn merge_sort(src: &mut [(u64, usize)], dst: &mut [(u64, usize)]) {
    if src.len() < 2 {
        return;
    }
    let mid = src.len() / 2;
    let (left, right) = src.split_at_mut(mid);
    let (dst_left, dst_right) = dst.split_at_mut(mid);
    merge_sort(dst_left, left);
    merge_sort(dst_right, right);

    let (mut i, mut j) = (0, 0);
    for entry in dst {
        // Take the left entry on ties to preserve the original order.
        if j < right.len() && (i == left.len() || right[j].0 < left[i].0) {
            *entry = right[j];
            j += 1;
        } else {
            *entry = left[i];
            i += 1;
        }
    }
}

pub(crate) const MAGIC: [u8; 4] = *b"HBGD";
pub(crate) const VERSION: u8 = 1;
#[cfg(feature = "encoder")]
pub(crate) const COMPRESSION_NONE: u8 = 0;
#[cfg(feature = "encoder")]
pub(crate) const COMPRESSION_DEFLATE: u8 = 1;
pub(crate) const COMPRESSION_MODEL: u8 = 2;
// Revision of the model (see `model.rs`): the coded data can only be decoded
// with the same model, so it must be incremented when the model changes.
pub(crate) const MODEL_REVISION: u8 = 1;
pub(crate) const FLAG_SERIF: u8 = 0x01;
pub(crate) const FLAG_FIXED_PITCH: u8 = 0x02;
const MAX_COMPONENT_DEPTH: usize = 8;
/// The number of codepoints (the starts of the runs of the cmap wrap around).
const CODEPOINTS: u64 = 0x110000;
const MAX_VISITED_GLYPHS: i32 = 1024;

/// A glyph of `GlyphData`, whose contours, points and components are in its
/// arrays.
#[derive(Clone, Copy, Debug)]
enum Glyph {
    Empty,
    /// The contours `contours..contours + num_contours` of `contour_sizes`,
    /// whose points start at `points`.
    Simple {
        contours: u32,
        num_contours: u32,
        points: u32,
    },
    /// The components `start..start + count`.
    Composite {
        start: u32,
        count: u32,
    },
}

/// A component of a composite glyph (see `model::Component`): its values are
/// F2Dot14 (the unused ones are 0).
#[derive(Clone, Copy, Debug)]
struct GlyphComponent {
    gid: u32,
    transform: u8,
    dx: i16,
    dy: i16,
    values: [i16; 4],
}

/// The strings of a glyph data file.
#[derive(Clone, Debug, Default)]
pub struct Names {
    pub family: String,
    pub version: String,
    pub copyright: String,
    pub license: String,
    pub license_url: String,
}

/// Flattened quadratic contours using TrueType points. Consecutive off-curve
/// points imply an on-curve midpoint; it is not stored explicitly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
    /// 1 for the on-curve points.
    pub on_curve: Vec<u8>,
    /// Index of the last point of each contour.
    pub end_pts: Vec<usize>,
}

impl Outline {
    /// Removes the points and contours, keeping the buffers.
    pub fn clear(&mut self) {
        self.xs.clear();
        self.ys.clear();
        self.on_curve.clear();
        self.end_pts.clear();
    }
}

pub struct GlyphData {
    pub units_per_em: u16,
    grid: u8,
    pub serif: bool,
    pub fixed_pitch: bool,
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    pub cap_height: i16,
    pub x_height: i16,
    pub underline_position: i16,
    pub underline_thickness: i16,
    pub weight_class: u16,
    pub stem_v: u16,
    pub stem_h: u16,
    /// The stems of the other weight of the family: the bold one of a regular
    /// font (`weight_class` below 600), else the regular one (0: unknown).
    pub reference_stem_v: u16,
    pub reference_stem_h: u16,
    pub names: Names,
    /// Sorted (codepoint, glyph id).
    cmap: Vec<(u32, u32)>,
    num_glyphs: usize,
    /// The decoded glyphs: all of them, or only the first ones (see
    /// `prepare`).
    glyphs: Mutex<Glyphs>,
    /// The coded data (without the model revision), when the glyphs are
    /// decoded on demand.
    coded: Option<&'static [u8]>,
}

/// The first glyphs of some glyph data: their contours, points (on-curve
/// flags and coordinates in grid units, which fit in 16 bits as in TrueType)
/// and components are in arrays, one glyph after the other, so that the
/// outlines are contiguous, in a few allocations.
#[derive(Default)]
struct Glyphs {
    glyphs: Vec<Glyph>,
    advances: Vec<u16>,
    contour_sizes: Vec<u16>,
    on: Vec<u8>,
    xs: Vec<i16>,
    ys: Vec<i16>,
    components: Vec<GlyphComponent>,
}

fn u16_at(bytes: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([bytes[pos], bytes[pos + 1]])
}

fn i16_at(bytes: &[u8], pos: usize) -> i16 {
    i16::from_be_bytes([bytes[pos], bytes[pos + 1]])
}

impl GlyphData {
    /// Parses a glyph data file compressed with the model codec (or
    /// uncompressed, with the `encoder` feature), decoding all its glyphs.
    pub fn new(bytes: &[u8]) -> Result<Self, Error> {
        match Self::compression(bytes)? {
            #[cfg(feature = "encoder")]
            COMPRESSION_NONE => Self::from_payload(payload::parse(&bytes[6..])?),
            _ => Self::from_payload(model::decode(&bytes[7..])?),
        }
    }

    /// Parses a file, decoding glyphs on demand (see `prepare`). Family files
    /// put 8-bit encoding glyphs first so they can be decoded as a prefix.
    pub fn lazy(bytes: &'static [u8]) -> Result<Self, Error> {
        match Self::compression(bytes)? {
            #[cfg(feature = "encoder")]
            COMPRESSION_NONE => Self::new(bytes),
            _ => {
                let coded = &bytes[7..];
                let (payload, num_glyphs) = model::decode_glyphs(coded, 0)?;
                Self::from_parts(payload, num_glyphs, Some(coded))
            }
        }
    }

    /// The compression of a glyph data file (`COMPRESSION_MODEL` with the
    /// right revision, or `COMPRESSION_NONE` with the `encoder` feature).
    fn compression(bytes: &[u8]) -> Result<u8, Error> {
        if bytes.len() < 6 || bytes[..4] != MAGIC || bytes[4] != VERSION {
            return Err(Error::InvalidData);
        }
        match bytes[5] {
            #[cfg(feature = "encoder")]
            COMPRESSION_NONE => Ok(COMPRESSION_NONE),
            COMPRESSION_MODEL if bytes.get(6) == Some(&MODEL_REVISION) => Ok(COMPRESSION_MODEL),
            _ => Err(Error::UnsupportedCompression),
        }
    }

    pub(crate) fn from_payload(payload: Payload) -> Result<Self, Error> {
        let num_glyphs = payload.glyphs.len();
        Self::from_parts(payload, num_glyphs, None)
    }

    /// The glyph data of a payload which may have only the first glyphs (the
    /// other ones being decoded from `coded` when they're needed).
    fn from_parts(
        payload: Payload,
        num_glyphs: usize,
        coded: Option<&'static [u8]>,
    ) -> Result<Self, Error> {
        let f = &payload.fixed;
        let string = |i: usize| String::from_utf8_lossy(&payload.strings[i]).into_owned();
        let names = Names {
            family: string(0),
            version: string(1),
            copyright: string(2),
            license: string(3),
            license_url: string(4),
        };

        // cmap: runs of codepoints mapped to consecutive glyph ids (from
        // first_gid, the starts wrapping around), then the aliases.
        let mut cmap: Vec<(u64, usize)> = Vec::new();
        let mut gid = payload.first_gid;
        let mut start: u64 = 0;
        for &[gap, length] in payload.runs.as_chunks::<2>().0 {
            if gap >= CODEPOINTS {
                return Err(Error::InvalidData);
            }
            start = (start + gap) % CODEPOINTS;
            let end = start + length;
            if end > 0x10ffff {
                return Err(Error::InvalidData);
            }
            for cp in start..=end {
                cmap.push((cp, gid as u32 as usize));
                gid += 1;
            }
            start = end + 1;
        }
        let mut cp: u64 = 0;
        for &[gap, gid] in payload.aliases.as_chunks::<2>().0 {
            cp += gap;
            if cp > 0x10ffff {
                return Err(Error::InvalidData);
            }
            cmap.push((cp, gid as u32 as usize));
        }
        // The aliases replace the previous mappings.
        cmap.reverse();
        sort_by_code(&mut cmap);
        cmap.dedup_by_key(|entry| entry.0);
        if cmap.iter().any(|&(_, gid)| gid >= num_glyphs) {
            return Err(Error::InvalidData);
        }
        let cmap = cmap
            .into_iter()
            .map(|(cp, gid)| (cp as u32, gid as u32))
            .collect();

        Ok(GlyphData {
            units_per_em: u16_at(f, 0),
            grid: f[2],
            serif: f[3] & FLAG_SERIF != 0,
            fixed_pitch: f[3] & FLAG_FIXED_PITCH != 0,
            ascender: i16_at(f, 4),
            descender: i16_at(f, 6),
            line_gap: i16_at(f, 8),
            cap_height: i16_at(f, 10),
            x_height: i16_at(f, 12),
            underline_position: i16_at(f, 14),
            underline_thickness: i16_at(f, 16),
            weight_class: u16_at(f, 18),
            stem_v: u16_at(f, 20),
            stem_h: u16_at(f, 22),
            reference_stem_v: u16_at(f, 24),
            reference_stem_h: u16_at(f, 26),
            names,
            cmap,
            num_glyphs,
            glyphs: Mutex::new(Glyphs::new(payload.glyphs, num_glyphs)?),
            coded,
        })
    }

    pub fn num_glyphs(&self) -> usize {
        self.num_glyphs
    }

    /// The glyph id of a codepoint.
    pub fn glyph_id(&self, codepoint: u32) -> Option<usize> {
        self.cmap
            .binary_search_by_key(&codepoint, |&(cp, _)| cp)
            .ok()
            .map(|i| self.cmap[i].1 as usize)
    }

    pub fn has_codepoint(&self, codepoint: u32) -> bool {
        self.glyph_id(codepoint).is_some()
    }

    /// The supported codepoints, sorted.
    pub fn codepoints(&self) -> impl Iterator<Item = u32> + '_ {
        self.cmap.iter().map(|&(cp, _)| cp)
    }

    /// Decodes some glyphs and their components, if they aren't yet (see
    /// `decoded`).
    pub fn prepare(&self, gids: &[usize]) {
        drop(self.decoded(gids));
    }

    /// Decodes requested glyphs and their components. Each restart decodes
    /// from the beginning, at least doubling the prefix. Beyond half of the
    /// glyphs, decodes the whole file, including trailing component glyphs.
    fn decoded(&self, gids: &[usize]) -> MutexGuard<'_, Glyphs> {
        let mut glyphs = self.glyphs.lock().unwrap();
        let Some(coded) = self.coded else {
            return glyphs;
        };
        loop {
            let needed = glyphs.needed(gids, self.num_glyphs);
            let decoded = glyphs.glyphs.len();
            if needed <= decoded {
                return glyphs;
            }
            let mut limit = needed.max(2 * decoded);
            if limit > self.num_glyphs / 2 {
                limit = self.num_glyphs;
            }
            let (payload, _) =
                model::decode_glyphs(coded, limit).expect("Invalid embedded glyph data");
            *glyphs =
                Glyphs::new(payload.glyphs, self.num_glyphs).expect("Invalid embedded glyph data");
        }
    }

    pub fn advance(&self, gid: usize) -> u16 {
        self.decoded(&[gid]).advances[gid]
    }

    /// The outline of a glyph, composite glyphs being flattened.
    pub fn outline(&self, gid: usize) -> Outline {
        let mut outline = Outline::default();
        self.outline_into(gid, &mut outline);
        outline
    }

    /// The outline of a glyph in `outline`, whose buffers are reused.
    pub fn outline_into(&self, gid: usize, outline: &mut Outline) {
        outline.clear();
        let mut budget = MAX_VISITED_GLYPHS;
        self.decoded(&[gid]).collect_contours(
            self.grid as f64,
            gid as i64,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            outline,
            0,
            &mut budget,
        );
    }
}

impl Glyphs {
    /// The glyphs of a payload (the first ones of the glyph data, which has
    /// `num_glyphs` glyphs).
    fn new(payload_glyphs: Vec<DecodedGlyph>, num_glyphs: usize) -> Result<Self, Error> {
        let (mut num_contours, mut num_points, mut num_components) = (0, 0, 0);
        for glyph in &payload_glyphs {
            match &glyph.shape {
                Shape::Empty => {}
                Shape::Simple { sizes, x, .. } => {
                    num_contours += sizes.len();
                    num_points += x.len();
                }
                Shape::Composite(components) => num_components += components.len(),
            }
        }
        let mut glyphs = Glyphs {
            glyphs: Vec::with_capacity(payload_glyphs.len()),
            advances: Vec::with_capacity(payload_glyphs.len()),
            contour_sizes: Vec::with_capacity(num_contours),
            on: Vec::with_capacity(num_points),
            xs: Vec::with_capacity(num_points),
            ys: Vec::with_capacity(num_points),
            components: Vec::with_capacity(num_components),
        };
        let short = |v: i64| i16::try_from(v).map_err(|_| Error::InvalidData);
        for glyph in payload_glyphs {
            if glyph.advance < 0 || glyph.advance > u32::MAX as i64 {
                return Err(Error::InvalidData);
            }
            // Stored on 16 bits, as in the hmtx table (larger values wrap
            // around).
            glyphs.advances.push(glyph.advance as u16);
            let glyph = match glyph.shape {
                Shape::Empty => Glyph::Empty,
                Shape::Simple {
                    sizes,
                    on: flags,
                    x,
                    y,
                } => {
                    let n: usize = sizes.iter().map(|&size| size as usize).sum();
                    if n != x.len() || n != y.len() || n != flags.len() {
                        return Err(Error::InvalidData);
                    }
                    let glyph = Glyph::Simple {
                        contours: glyphs.contour_sizes.len() as u32,
                        num_contours: sizes.len() as u32,
                        points: glyphs.xs.len() as u32,
                    };
                    if x.iter().chain(&y).any(|&c| i16::try_from(c).is_err()) {
                        return Err(Error::InvalidData);
                    }
                    glyphs.contour_sizes.extend_from_slice(&sizes);
                    glyphs.on.extend_from_slice(&flags);
                    glyphs.xs.extend(x.iter().map(|&c| c as i16));
                    glyphs.ys.extend(y.iter().map(|&c| c as i16));
                    glyph
                }
                Shape::Composite(list) => {
                    let start = glyphs.components.len() as u32;
                    for component in &list {
                        if component.gid < 0 || component.gid as usize >= num_glyphs {
                            return Err(Error::InvalidData);
                        }
                        let mut values = [0; 4];
                        for (value, &v) in values.iter_mut().zip(&component.values) {
                            *value = short(v)?;
                        }
                        glyphs.components.push(GlyphComponent {
                            gid: component.gid as u32,
                            transform: component.transform as u8,
                            dx: short(component.dx)?,
                            dy: short(component.dy)?,
                            values,
                        });
                    }
                    Glyph::Composite {
                        start,
                        count: list.len() as u32,
                    }
                }
            };
            glyphs.glyphs.push(glyph);
        }
        Ok(glyphs)
    }

    /// The number of glyphs to decode for some glyphs and their components:
    /// one more than the greatest id found (the components of the glyphs
    /// which aren't decoded being unknown yet). Traversal is bounded by
    /// `MAX_COMPONENT_DEPTH` and `MAX_VISITED_GLYPHS`.
    fn needed(&self, gids: &[usize], num_glyphs: usize) -> usize {
        let decoded = self.glyphs.len();
        let mut needed = 0;
        let mut pending: Vec<(usize, usize)> = Vec::new();
        for &gid in gids {
            if gid >= num_glyphs {
                continue;
            }
            needed = needed.max(gid + 1);
            // Only decoded composite glyphs have components to visit (skipping
            // the others avoids an allocation per `outline` or `advance` call).
            if gid >= decoded || !matches!(self.glyphs[gid], Glyph::Composite { .. }) {
                continue;
            }
            pending.push((gid, 0));
            let mut budget = MAX_VISITED_GLYPHS;
            while let Some((gid, depth)) = pending.pop() {
                needed = needed.max(gid + 1);
                budget -= 1;
                if gid >= decoded || depth >= MAX_COMPONENT_DEPTH || budget <= 0 {
                    continue;
                }
                if let Glyph::Composite { start, count } = self.glyphs[gid] {
                    let components = &self.components[start as usize..][..count as usize];
                    pending.extend(components.iter().map(|c| (c.gid as usize, depth + 1)));
                }
            }
        }
        needed
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_contours(
        &self,
        grid: f64,
        gid: i64,
        [a, b, c, d, e, f]: [f64; 6],
        output: &mut Outline,
        depth: usize,
        budget: &mut i32,
    ) {
        if gid < 0 || gid as usize >= self.glyphs.len() || depth > MAX_COMPONENT_DEPTH {
            return;
        }
        let remaining = *budget;
        *budget -= 1;
        if remaining <= 0 {
            return;
        }
        match self.glyphs[gid as usize] {
            Glyph::Empty => {}
            Glyph::Simple {
                contours,
                num_contours,
                points,
            } => {
                let sizes = &self.contour_sizes[contours as usize..][..num_contours as usize];
                let n: usize = sizes.iter().map(|&size| size as usize).sum();
                output.xs.reserve(n);
                output.ys.reserve(n);
                output.on_curve.reserve(n);
                output.end_pts.reserve(sizes.len());
                let mut start = points as usize;
                for &size in sizes {
                    let end = start + size as usize;
                    for i in start..end {
                        let px = self.xs[i] as f64 * grid;
                        let py = self.ys[i] as f64 * grid;
                        output.xs.push(a * px + c * py + e);
                        output.ys.push(b * px + d * py + f);
                    }
                    output.on_curve.extend_from_slice(&self.on[start..end]);
                    output.end_pts.push(output.xs.len() - 1);
                    start = end;
                }
            }
            Glyph::Composite { start, count } => {
                for component in &self.components[start as usize..][..count as usize] {
                    let v = |i: usize| component.values[i] as f64 / 16384.0;
                    let m = match component.transform {
                        1 => [v(0), 0.0, 0.0, v(0)],
                        2 => [v(0), 0.0, 0.0, v(1)],
                        3 => [v(0), v(1), v(2), v(3)],
                        _ => [1.0, 0.0, 0.0, 1.0],
                    };
                    let dx = component.dx as f64 * grid;
                    let dy = component.dy as f64 * grid;
                    // Compose: parent(component(p)).
                    self.collect_contours(
                        grid,
                        component.gid as i64,
                        [
                            a * m[0] + c * m[1],
                            b * m[0] + d * m[1],
                            a * m[2] + c * m[3],
                            b * m[2] + d * m[3],
                            a * dx + c * dy + e,
                            b * dx + d * dy + f,
                        ],
                        output,
                        depth + 1,
                        budget,
                    );
                }
            }
        }
    }
}

#[cfg(all(test, feature = "embedded-data"))]
mod tests {
    use super::{sort_by_code, Glyph};
    use crate::{glyph_data, Family};

    #[test]
    fn sorts_by_code_like_the_stable_sort() {
        for n in (0..300).chain([1000, 4097, 65535]) {
            // Equal, repeated and descending codes; indices check stability.
            for codes in [1, 17, n + 1] {
                let mut entries: Vec<_> = (0..n).map(|i| (((n - i) % codes) as u64, i)).collect();
                let mut expected = entries.clone();
                expected.sort_by_key(|&(code, _)| code);
                sort_by_code(&mut entries);
                assert_eq!(entries, expected, "{n} entries, {codes} codes");
            }
        }
    }

    #[test]
    fn has_the_font_metrics() {
        let (sans, serif) = (glyph_data(Family::Sans), glyph_data(Family::Serif));
        assert_eq!(sans.units_per_em, 2048);
        assert_eq!((sans.cap_height, sans.x_height), (1409, 1082));
        // The reference stems are the ones of the bold style.
        assert_eq!((sans.stem_v, sans.reference_stem_v), (180, 281));
        assert!(serif.serif && !sans.serif);
        assert!(sans.names.copyright.contains("Arimo Project Authors"));
        assert!(sans.names.license.contains("Open Font License"));
    }

    #[test]
    fn flattens_the_composite_glyphs() {
        let sans = glyph_data(Family::Sans);
        let (e, e_acute) = (sans.glyph_id(0x65).unwrap(), sans.glyph_id(0xe9).unwrap());
        sans.prepare(&[e_acute]);
        let glyph = sans.glyphs.lock().unwrap().glyphs[e_acute];
        assert!(matches!(glyph, Glyph::Composite { .. }));
        let (e, e_acute) = (sans.outline(e), sans.outline(e_acute));
        assert!(e_acute.end_pts.len() > e.end_pts.len());
        // The base glyph is the first component, without any offset.
        assert_eq!(e_acute.xs[..e.xs.len()], e.xs);
        assert_eq!(e_acute.ys[..e.ys.len()], e.ys);
    }
}
