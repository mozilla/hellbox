// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The fonts the glyph data is built from (TrueType, glyf-based), read with
//! read-fonts: the metrics, the names, the Unicode cmap and the glyphs (the
//! components being resolved by `build.rs`).

use read_fonts::tables::cmap::{Cmap, CmapSubtable};
use read_fonts::tables::glyf::{Anchor, CompositeGlyphFlags, Glyph};
use read_fonts::types::{GlyphId, Tag};
use read_fonts::{FontRef, TableProvider};
use std::collections::{HashMap, HashSet};

/// A point of a source outline (the transformed components have fractional
/// coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
    pub on: bool,
}

pub(crate) struct SourceComponent {
    pub glyph_index: u32,
    /// Linear transform [a, b, c, d]: x' = a x + c y, y' = b x + d y.
    /// Component offsets and point matching are resolved in `build.rs`.
    pub matrix: [f64; 4],
    /// The arguments are an offset (else point numbers).
    pub xy_values: bool,
    pub scaled_offset: bool,
    pub arg1: i32,
    pub arg2: i32,
}

pub(crate) enum SourceGlyph {
    Empty,
    Simple(Vec<Vec<Point>>),
    Composite(Vec<SourceComponent>),
}

#[derive(Default)]
pub(crate) struct SourceFont {
    pub units_per_em: u16,
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    pub weight_class: u16,
    /// 0 when unknown (old OS/2 tables).
    pub x_height: i16,
    pub cap_height: i16,
    pub underline_position: i16,
    pub underline_thickness: i16,
    pub is_fixed_pitch: bool,
    /// Name id => string (Windows, Unicode BMP, English US).
    pub names: HashMap<u16, String>,
    /// The Unicode mappings, in the order of the subtable.
    pub cmap: Vec<(u32, u32)>,
    pub cmap_index: HashMap<u32, u32>,
    pub advances: Vec<u16>,
    pub glyphs: Vec<SourceGlyph>,
}

impl SourceFont {
    pub fn glyph_id(&self, cp: u32) -> Option<u32> {
        self.cmap_index.get(&cp).copied()
    }
}

fn err(e: impl std::fmt::Display) -> String {
    format!("Invalid font: {e}")
}

/// The Unicode mappings of a cmap table: the first subtable giving some, the
/// full repertoire ones first (without .notdef).
fn read_cmap(cmap: &Cmap) -> Result<Vec<(u32, u32)>, String> {
    let rank = |platform: u16, encoding: u16| match (platform, encoding) {
        (3, 10) => 4,
        (0, 4 | 6) => 3,
        (3, 1) => 2,
        (0, _) => 1,
        _ => 0,
    };
    let mut records: Vec<_> = cmap.encoding_records().iter().collect();
    // A stable sort: the records of the same rank keep their order.
    records.sort_by_key(|r| std::cmp::Reverse(rank(r.platform_id() as u16, r.encoding_id())));
    let mut map = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |c: u32, gid: u32, map: &mut Vec<(u32, u32)>| {
        if seen.insert(c) {
            map.push((c, gid));
        }
    };
    for record in records {
        if rank(record.platform_id() as u16, record.encoding_id()) == 0 {
            continue;
        }
        match record.subtable(cmap.offset_data()).map_err(err)? {
            CmapSubtable::Format4(s) => {
                let data = s.offset_data();
                let ranges_pos = s.id_range_offsets_byte_range().start;
                let ends = s.end_code();
                let starts = s.start_code();
                let deltas = s.id_delta();
                let range_offsets = s.id_range_offsets();
                for i in 0..(s.seg_count_x2() / 2) as usize {
                    let end = ends[i].get() as u32;
                    let start = starts[i].get() as u32;
                    let delta = deltas[i].get() as u16 as u32;
                    let range_offset = range_offsets[i].get() as usize;
                    let mut c = start;
                    while c <= end && c != 0xffff {
                        let gid = if range_offset == 0 {
                            (c + delta) & 0xffff
                        } else {
                            let pos = ranges_pos + i * 2 + range_offset + (c - start) as usize * 2;
                            match data.read_at::<u16>(pos).map_err(err)? as u32 {
                                0 => 0,
                                gid => (gid + delta) & 0xffff,
                            }
                        };
                        if gid != 0 {
                            add(c, gid, &mut map);
                        }
                        c += 1;
                    }
                }
            }
            CmapSubtable::Format12(s) => {
                for group in s.groups() {
                    // Bound codepoints and skip .notdef or overflowing glyph ids.
                    let start = group.start_char_code();
                    for c in start..=group.end_char_code().min(0x10ffff) {
                        match group.start_glyph_id().checked_add(c - start) {
                            Some(0) | None => {}
                            Some(gid) => add(c, gid, &mut map),
                        }
                    }
                }
            }
            _ => {}
        }
        if !map.is_empty() {
            break;
        }
    }
    Ok(map)
}

/// The Windows English names (UTF-16BE).
fn read_names(font: &FontRef) -> HashMap<u16, String> {
    let mut names = HashMap::new();
    let Ok(name) = font.name() else {
        return names;
    };
    let storage = name.string_data().as_bytes();
    for record in name.name_record() {
        if record.platform_id() != 3 || record.encoding_id() != 1 || record.language_id() != 0x409 {
            continue;
        }
        let start = (record.string_offset().to_u32() as usize).min(storage.len());
        let end = (start + record.length() as usize).min(storage.len());
        let units: Vec<u16> = storage[start..end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| u16::from_be_bytes(b))
            .collect();
        names.insert(record.name_id().to_u16(), String::from_utf16_lossy(&units));
    }
    names
}

/// Reads a glyph of a font with `num_glyphs` glyphs.
fn read_glyph(glyph: Option<Glyph>, num_glyphs: u32) -> Result<SourceGlyph, String> {
    Ok(match glyph {
        None => SourceGlyph::Empty,
        Some(Glyph::Simple(s)) => {
            let end_pts = s.end_pts_of_contours();
            if end_pts.is_empty() {
                return Ok(SourceGlyph::Empty);
            }
            let points: Vec<Point> = s
                .points()
                .map(|p| Point {
                    x: p.x as f64,
                    y: p.y as f64,
                    on: p.on_curve,
                })
                .collect();
            if points.len() != end_pts.last().unwrap().get() as usize + 1 {
                return Err(err("bad simple glyph"));
            }
            let mut contours = Vec::with_capacity(end_pts.len());
            let mut start = 0;
            for end in end_pts {
                let end = end.get() as usize;
                contours.push(
                    points
                        .get(start..=end)
                        .ok_or_else(|| err("bad contour"))?
                        .to_vec(),
                );
                start = end + 1;
            }
            SourceGlyph::Simple(contours)
        }
        Some(Glyph::Composite(c)) => {
            let components = c
                .components()
                .map(|component| {
                    let glyph_index = component.glyph.to_u32();
                    if glyph_index >= num_glyphs {
                        return Err(err(format!("bad component glyph id {glyph_index}")));
                    }
                    let t = component.transform;
                    let (xy_values, arg1, arg2) = match component.anchor {
                        Anchor::Offset { x, y } => (true, x as i32, y as i32),
                        Anchor::Point { base, component } => (false, base as i32, component as i32),
                    };
                    Ok(SourceComponent {
                        glyph_index,
                        matrix: [t.xx, t.yx, t.xy, t.yy].map(|v| v.to_bits() as f64 / 16384.0),
                        xy_values,
                        scaled_offset: component
                            .flags
                            .contains(CompositeGlyphFlags::SCALED_COMPONENT_OFFSET),
                        arg1,
                        arg2,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if components.is_empty() {
                return Err(err("composite glyph without components"));
            }
            SourceGlyph::Composite(components)
        }
    })
}

/// Reads a TrueType (glyf-based) font.
pub(crate) fn read(bytes: &[u8]) -> Result<SourceFont, String> {
    let font = FontRef::new(bytes).map_err(err)?;
    for tag in ["head", "hhea", "hmtx", "maxp", "cmap", "loca", "glyf"] {
        if font
            .table_data(Tag::new_checked(tag.as_bytes()).unwrap())
            .is_none()
        {
            return Err(format!("Missing \"{tag}\" table."));
        }
    }
    let units_per_em = font.head().map_err(err)?.units_per_em();
    let num_glyphs = font.maxp().map_err(err)?.num_glyphs() as u32;
    let hhea = font.hhea().map_err(err)?;
    let hmtx = font.hmtx().map_err(err)?;
    let advances = (0..num_glyphs)
        .map(|gid| hmtx.advance(GlyphId::new(gid)).unwrap_or(0))
        .collect();

    let (mut weight_class, mut x_height, mut cap_height) = (400, 0, 0);
    if let Ok(os2) = font.os2() {
        weight_class = os2.us_weight_class();
        if os2.version() >= 2 {
            x_height = os2.sx_height().unwrap_or(0);
            cap_height = os2.s_cap_height().unwrap_or(0);
        }
    }
    let (mut underline_position, mut underline_thickness, mut is_fixed_pitch) = (0, 0, false);
    if let Ok(post) = font.post() {
        underline_position = post.underline_position().to_i16();
        underline_thickness = post.underline_thickness().to_i16();
        is_fixed_pitch = post.is_fixed_pitch() != 0;
    }

    let loca = font.loca(None).map_err(err)?;
    let glyf = font.glyf().map_err(err)?;
    let glyphs = (0..num_glyphs)
        .map(|gid| {
            read_glyph(
                loca.get_glyf(GlyphId::new(gid), &glyf).map_err(err)?,
                num_glyphs,
            )
        })
        .collect::<Result<_, _>>()?;

    let cmap = read_cmap(&font.cmap().map_err(err)?)?;
    Ok(SourceFont {
        units_per_em,
        ascender: hhea.ascender().to_i16(),
        descender: hhea.descender().to_i16(),
        line_gap: hhea.line_gap().to_i16(),
        weight_class,
        x_height,
        cap_height,
        underline_position,
        underline_thickness,
        is_fixed_pitch,
        names: read_names(&font),
        cmap_index: cmap.iter().copied().collect(),
        cmap,
        advances,
        glyphs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use read_fonts::tables::glyf::CompositeGlyph;
    use read_fonts::{FontData, FontRead};

    /// Reads a composite glyph (in a font of two glyphs) with these
    /// components (offsets of 0).
    fn read_composite(gids: &[u16]) -> Result<SourceGlyph, String> {
        let mut bytes = (-1i16).to_be_bytes().to_vec();
        bytes.extend([0; 8]); // Bounds.
        for (i, gid) in gids.iter().enumerate() {
            // ARGS_ARE_XY_VALUES, and MORE_COMPONENTS but for the last one.
            let flags: u16 = if i + 1 < gids.len() { 0x22 } else { 0x02 };
            bytes.extend(flags.to_be_bytes());
            bytes.extend(gid.to_be_bytes());
            bytes.extend([0, 0]);
        }
        let glyph = CompositeGlyph::read(FontData::new(&bytes)).map_err(err)?;
        read_glyph(Some(Glyph::Composite(glyph)), 2)
    }

    #[test]
    fn checks_the_components() {
        assert!(read_composite(&[1, 0]).is_ok());
        assert_eq!(
            read_composite(&[1, 0xffff]).err().as_deref(),
            Some("Invalid font: bad component glyph id 65535")
        );
        assert!(read_composite(&[]).is_err());
    }

    #[test]
    fn reads_the_format_12_cmaps() {
        // Groups (start, end, glyph id): .notdef, two codes, a glyph id
        // overflowing and codepoints after U+10FFFF.
        let groups = [
            (0x41, 0x41, 0),
            (0x42, 0x43, 5),
            (0x45, 0x46, u32::MAX),
            (0x10fffe, 0x110005, 9),
        ];
        // A (3, 10) subtable.
        let mut bytes: Vec<u8> = [0u16, 1, 3, 10]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let length = 16 + 12 * groups.len() as u32;
        bytes.extend(12u32.to_be_bytes());
        bytes.extend([0, 12, 0, 0]);
        for v in [length, 0, groups.len() as u32] {
            bytes.extend(v.to_be_bytes());
        }
        for (start, end, gid) in groups {
            for v in [start, end, gid] {
                bytes.extend(v.to_be_bytes());
            }
        }
        let cmap = Cmap::read(FontData::new(&bytes)).unwrap();
        let expected = [
            (0x42, 5),
            (0x43, 6),
            (0x45, u32::MAX),
            (0x10fffe, 9),
            (0x10ffff, 10),
        ];
        assert_eq!(read_cmap(&cmap), Ok(expected.to_vec()));
    }
}
