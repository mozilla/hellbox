// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The style of the generated fonts: their glyphs come from the embedded
//! style of a family closest to the requested one (regular or bold, and
//! italic for the serif family), the rest being synthesized (emboldened or
//! thinned, slanted), and the glyphs of the fallback glyph data (the symbols)
//! are synthesized to the whole style.

use crate::font_builder::{clamp, known, slant_of, FontStyle, Synthesis};
use crate::glyph_data::GlyphData;

pub(crate) const DEFAULT_WEIGHT: f64 = 400.0;
pub(crate) const BOLD_WEIGHT: f64 = 700.0;
// FreeType uses ~12 degrees for its synthetic oblique fonts.
pub(crate) const DEFAULT_ITALIC_ANGLE: f64 = -12.0;
// Keep synthetic slant away from the singularities of tan.
pub(crate) const MAX_ITALIC_ANGLE: f64 = 45.0;
/// The weights from it use the bold style, the lighter ones the regular one.
pub(crate) const BOLD_THRESHOLD: f64 = 550.0;

/// A requested style.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Target {
    /// 1 to 1000.
    pub weight: f64,
    /// In degrees, negative for a font leaning to the right (0: upright).
    pub italic_angle: f64,
}

fn weight_of(data: &GlyphData) -> f64 {
    known(data.weight_class).unwrap_or(DEFAULT_WEIGHT)
}

/// The changes of the vertical and horizontal stems per weight unit, from the
/// stems of the other weight of the family. Unknown stems use an em / 24
/// from regular to bold, with horizontal strength at 75% of vertical.
pub(crate) fn stem_slopes(data: &GlyphData) -> (f64, f64) {
    let weight = weight_of(data);
    let other_weight = if weight < 600.0 {
        BOLD_WEIGHT
    } else {
        DEFAULT_WEIGHT
    };
    let (stem_v, stem_h) = (data.stem_v as f64, data.stem_h as f64);
    let (other_v, other_h) = (data.reference_stem_v as f64, data.reference_stem_h as f64);
    let slope_v = (other_v - stem_v) / (other_weight - weight);
    if stem_v > 0.0 && other_v > 0.0 && slope_v > 0.0 {
        let slope_h = if stem_h > 0.0 && other_h > 0.0 {
            ((other_h - stem_h) / (other_weight - weight)).max(0.0)
        } else {
            slope_v * 0.75
        };
        return (slope_v, slope_h);
    }
    let slope = data.units_per_em as f64 / 24.0 / (BOLD_WEIGHT - DEFAULT_WEIGHT);
    (slope, slope * 0.75)
}

/// The weight whose vertical stems have a width (in the units of the glyph
/// data of the regular style of a family), if it can be known.
pub(crate) fn weight_of_stem(regular: &GlyphData, stem: f64) -> Option<f64> {
    let stem_v = known(regular.stem_v)?;
    let (slope, _) = stem_slopes(regular);
    Some(DEFAULT_WEIGHT + (stem - stem_v) / slope)
}

/// The style of a font whose glyphs come from `sources`, the first one having
/// the base style (italic when `base_italic_angle` is its italic angle, else
/// upright), the other ones being regular and upright:
/// - `cap_height`: target height in the first source's units; its scale is
///   clamped to 0.8..=1.25,
/// - `match_cap_height`: adjust fallback heights toward the first source's
///   cap height, with an additional scale clamped to 0.8..=1.25.
pub(crate) fn font_style(
    sources: &[&GlyphData],
    base_italic_angle: Option<f64>,
    target: &Target,
    cap_height: Option<f64>,
    match_cap_height: bool,
) -> FontStyle {
    let base = sources[0];
    let upm = base.units_per_em as f64;
    let (slope_v, slope_h) = stem_slopes(base);
    let ratio = slope_h / slope_v;
    // Limit stem changes relative to the base font's vertical stem.
    let reference = known(base.stem_v).unwrap_or(upm / 12.0);
    let synthesis = |weight_delta: f64, y_scale: f64, slant: f64| {
        let bold_x = clamp(slope_v * weight_delta, -0.4 * reference, 1.2 * reference);
        Synthesis {
            bold_x,
            bold_y: bold_x * ratio,
            y_scale,
            slant,
        }
    };
    let italic_angle = base_italic_angle.unwrap_or(clamp(
        target.italic_angle,
        -MAX_ITALIC_ANGLE,
        MAX_ITALIC_ANGLE,
    ));
    let base_slant = if base_italic_angle.is_some() {
        0.0
    } else {
        slant_of(italic_angle)
    };
    let base_cap_height = known(base.cap_height).unwrap_or(base.ascender as f64);
    let mut y_scale = 1.0;
    if let Some(target_cap_height) = cap_height {
        if target_cap_height > 0.0 && base_cap_height > 0.0 {
            y_scale = clamp(target_cap_height / base_cap_height, 0.8, 1.25);
        }
    }
    // The base glyphs only get the rest of the style.
    let mut syntheses = vec![synthesis(
        target.weight - weight_of(base),
        y_scale,
        base_slant,
    )];
    for data in &sources[1..] {
        let scale = upm / data.units_per_em as f64;
        let data_cap_height = known(data.cap_height).unwrap_or(data.ascender as f64) * scale;
        let mut data_y_scale = y_scale;
        if match_cap_height && data_cap_height > 0.0 && base_cap_height > 0.0 {
            data_y_scale *= clamp(base_cap_height / data_cap_height, 0.8, 1.25);
        }
        syntheses.push(synthesis(
            target.weight - DEFAULT_WEIGHT,
            data_y_scale,
            slant_of(italic_angle),
        ));
    }
    FontStyle {
        syntheses,
        weight: clamp(target.weight.round(), 1.0, 1000.0),
        italic_angle,
        scaled_stems: true,
    }
}
