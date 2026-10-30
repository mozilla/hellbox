// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The wasm interface, for pdf.js (see the README).

use crate::{Family, Kerning, Options, Request, StandardFont};
use wasm_bindgen::prelude::*;

/// A char code meaning "the first codepoint of the glyph text".
const NO_CHAR_CODE: u32 = u32::MAX;

fn family(value: u8) -> Result<Family, JsError> {
    match value {
        0 => Ok(Family::Sans),
        1 => Ok(Family::Serif),
        2 => Ok(Family::Mono),
        _ => Err(JsError::new("Invalid font family.")),
    }
}

/// Splits the codepoints of all the glyphs in the texts of each glyph.
fn split_texts<'a>(codepoints: &'a [u32], lengths: &[u32]) -> Result<Vec<&'a [u32]>, JsError> {
    let mut texts = Vec::with_capacity(lengths.len());
    let mut start = 0usize;
    for &length in lengths {
        let end = start
            .checked_add(length as usize)
            .filter(|&end| end <= codepoints.len())
            .ok_or_else(|| JsError::new("Invalid lengths."))?;
        texts.push(&codepoints[start..end]);
        start = end;
    }
    if start != codepoints.len() {
        return Err(JsError::new("Invalid lengths."));
    }
    Ok(texts)
}

fn standard_font(name: &str) -> Result<StandardFont, JsError> {
    StandardFont::from_name(name).ok_or_else(|| JsError::new("Invalid standard font."))
}

fn optional(value: f64) -> Option<f64> {
    (value.is_finite() && value != 0.0).then_some(value)
}

fn style_options(bold: bool, italic: bool, weight: f64, stem_v: f64, italic_angle: f64) -> Options {
    Options {
        bold,
        italic,
        weight: optional(weight),
        stem_v: optional(stem_v),
        italic_angle: optional(italic_angle),
        ..Options::default()
    }
}

/// Returns a JS error listing missing data files.
fn check_data(files: Vec<&str>) -> Result<(), JsError> {
    let missing: Vec<&str> = files
        .into_iter()
        .filter(|name| !crate::has_data(name))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(JsError::new(&format!(
            "Missing data files: {}.",
            missing.join(", ")
        )))
    }
}

/// Registers a data file for the module lifetime. Load the files listed by
/// `standard_font_data_files` or `font_data_files` before generating a font.
#[cfg(not(feature = "embedded-data"))]
#[wasm_bindgen]
pub fn set_data(name: &str, bytes: Vec<u8>) -> Result<(), JsError> {
    crate::set_data(name, bytes).map_err(|e| JsError::new(&e.to_string()))
}

/// Whether a named data file is embedded or registered.
#[wasm_bindgen]
pub fn has_data(name: &str) -> bool {
    crate::has_data(name)
}

/// The data files needed by a standard font (see `generate_standard_font`).
#[wasm_bindgen]
pub fn standard_font_data_files(name: &str) -> Result<Vec<String>, JsError> {
    let files = crate::standard_font_data_files(standard_font(name)?);
    Ok(files.into_iter().map(String::from).collect())
}

/// Data files needed for the family and style passed to `generate_font`.
#[wasm_bindgen]
pub fn font_data_files(
    family: u8,
    bold: bool,
    italic: bool,
    weight: f64,
    stem_v: f64,
    italic_angle: f64,
) -> Result<Vec<String>, JsError> {
    let options = style_options(bold, italic, weight, stem_v, italic_angle);
    let files = crate::font_data_files(self::family(family)?, &options);
    Ok(files.into_iter().map(String::from).collect())
}

/// The requests of `generate_font` and `generate_standard_font`.
fn requests(
    codepoints: &[u32],
    lengths: &[u32],
    widths: &[f64],
    char_codes: &[u32],
) -> Result<Vec<Request>, JsError> {
    let texts = split_texts(codepoints, lengths)?;
    let n = texts.len();
    if (!widths.is_empty() && widths.len() != n)
        || (!char_codes.is_empty() && char_codes.len() != n)
    {
        return Err(JsError::new("Invalid number of widths or char codes."));
    }
    Ok(texts
        .iter()
        .enumerate()
        .map(|(i, text)| Request {
            unicode: text.to_vec(),
            width: widths
                .get(i)
                .copied()
                .filter(|w| w.is_finite() && *w >= 0.0),
            char_code: char_codes.get(i).copied().filter(|&c| c != NO_CHAR_CODE),
        })
        .collect())
}

/// The kerning triples of `generate_font` and `generate_standard_font`, for
/// `n` glyphs.
fn kerning(triples: Option<Vec<f64>>, n: usize) -> Result<Vec<Kerning>, JsError> {
    let triples = triples.unwrap_or_default();
    let (triples, remainder) = triples.as_chunks::<3>();
    if !remainder.is_empty() {
        return Err(JsError::new("Invalid kerning."));
    }
    let index = |value: f64| {
        (value >= 0.0 && value < n as f64 && value.fract() == 0.0).then_some(value as usize)
    };
    let mut kerning = Vec::with_capacity(triples.len());
    for &[left, right, value] in triples {
        let (Some(left), Some(right), true) = (index(left), index(right), value.is_finite()) else {
            return Err(JsError::new("Invalid kerning."));
        };
        kerning.push(Kerning { left, right, value });
    }
    Ok(kerning)
}

/// Generates a TrueType font from the requested glyphs (the glyph id
/// of the i-th one is i + 1, except with `metrics_only`): a font has at most
/// 65535 glyphs, the requests after the 65534th one are ignored (except with
/// `metrics_only`, whose glyphs are shared).
///
/// - `codepoints`: the codepoints of the texts of all the glyphs, one after
///   the other (a text is usually a single codepoint, but it can be e.g. a
///   ligature "ffi" or a letter followed by combining marks),
/// - `lengths`: the number of codepoints of each glyph,
/// - `widths`: the advance width of each glyph, in 1/1000 em (PDF glyph space
///   units); NaN for the natural width. Can be empty (natural widths).
/// - `char_codes`: the code mapped to each glyph in the cmap, 0xFFFFFFFF for
///   the first codepoint of its text. Can be empty.
/// - `family`: 0 for sans-serif, 1 for serif, 2 for monospace.
/// - `bold`, `italic`: the style.
/// - `weight` (positive values override `bold`, clamped to 1..=1000), `stem_v` (the
///   expected vertical stem width, it has precedence over `weight`),
///   `italic_angle` (degrees, negative for a font leaning to the right, -16.33
///   for any negative angle with the serif family, which has an italic style),
///   `cap_height` (the glyphs are vertically scaled to match it): 0 or NaN
///   when unknown.
/// - `metrics_only` (optional, `false` by default): omit outlines and use
///   minimal names. Equal advances share glyph ids unless the requests are
///   used by an emitted kerning pair (see the cmap).
/// - `ascent` (optional): the ascent as a fraction of the em (between 0 and
///   1), the descent being the rest, without line gap (e.g. for a text layer,
///   whose lines are an em high).
/// - `kerning` (optional): the kerning of some pairs of glyphs, as triples
///   (index of the left glyph, index of the right one, adjustment of the
///   advance of the left one in 1/1000 em): at most 10920 pairs, the first
///   ones in the order of the glyphs.
/// - `debug_boxes` (optional, `false` by default): for debugging, each glyph
///   with an advance is the outline (1/32 em thick, inside) of a rectangle
///   covering it, from the descender to the ascender.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn generate_font(
    codepoints: &[u32],
    lengths: &[u32],
    widths: &[f64],
    char_codes: &[u32],
    family: u8,
    bold: bool,
    italic: bool,
    weight: f64,
    stem_v: f64,
    italic_angle: f64,
    cap_height: f64,
    metrics_only: Option<bool>,
    ascent: Option<f64>,
    kerning: Option<Vec<f64>>,
    debug_boxes: Option<bool>,
) -> Result<Vec<u8>, JsError> {
    let glyphs = requests(codepoints, lengths, widths, char_codes)?;
    let kerning = self::kerning(kerning, glyphs.len())?;
    let options = Options {
        cap_height: optional(cap_height),
        metrics_only: metrics_only.unwrap_or(false),
        ascent,
        kerning,
        debug_boxes: debug_boxes.unwrap_or(false),
        ..style_options(bold, italic, weight, stem_v, italic_angle)
    };
    let family = self::family(family)?;
    check_data(crate::font_data_files(family, &options))?;
    Ok(crate::generate_font(family, &glyphs, &options).data)
}

/// The indices of the glyphs (same `codepoints` and `lengths` as for
/// `generate_font`) which can't be rendered with a family.
#[wasm_bindgen]
pub fn missing_glyphs(
    codepoints: &[u32],
    lengths: &[u32],
    family: u8,
) -> Result<Vec<u32>, JsError> {
    let texts = split_texts(codepoints, lengths)?;
    let family = self::family(family)?;
    check_data(crate::font_data_files(family, &Options::default()))?;
    Ok(crate::missing_glyphs(family, &texts)
        .into_iter()
        .map(|i| i as u32)
        .collect())
}

/// Generates a standard font of PDF (e.g. "Helvetica-Bold", see
/// `StandardFont`) from the requested glyphs (the glyph id of the
/// i-th one is i + 1, except with `metrics_only`): the arguments are the ones
/// of `generate_font`, without the style (it's the one of the standard font).
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn generate_standard_font(
    codepoints: &[u32],
    lengths: &[u32],
    widths: &[f64],
    char_codes: &[u32],
    name: &str,
    metrics_only: Option<bool>,
    ascent: Option<f64>,
    kerning: Option<Vec<f64>>,
    debug_boxes: Option<bool>,
) -> Result<Vec<u8>, JsError> {
    let font = standard_font(name)?;
    check_data(crate::standard_font_data_files(font))?;
    let glyphs = requests(codepoints, lengths, widths, char_codes)?;
    let kerning = self::kerning(kerning, glyphs.len())?;
    let options = Options {
        metrics_only: metrics_only.unwrap_or(false),
        ascent,
        kerning,
        debug_boxes: debug_boxes.unwrap_or(false),
        ..Options::default()
    };
    Ok(crate::generate_standard_font(font, &glyphs, &options).data)
}

/// The indices of the glyphs (same `codepoints` and `lengths` as for
/// `generate_font`) which can't be rendered with a standard font.
#[wasm_bindgen]
pub fn missing_standard_glyphs(
    codepoints: &[u32],
    lengths: &[u32],
    name: &str,
) -> Result<Vec<u32>, JsError> {
    let font = standard_font(name)?;
    check_data(crate::standard_font_data_files(font))?;
    let texts = split_texts(codepoints, lengths)?;
    Ok(crate::missing_standard_glyphs(font, &texts)
        .into_iter()
        .map(|i| i as u32)
        .collect())
}

/// The codepoints of the glyph data of a standard font (e.g. to generate a
/// font with all its glyphs).
#[wasm_bindgen]
pub fn standard_font_codepoints(name: &str) -> Result<Vec<u32>, JsError> {
    let font = standard_font(name)?;
    check_data(crate::standard_font_data_files(font))?;
    Ok(crate::standard_glyph_data(font).codepoints().collect())
}
