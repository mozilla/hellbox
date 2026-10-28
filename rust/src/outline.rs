// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Transformations of the outlines returned by `GlyphData::outline`. An
//! outline is made of quadratic contours (TrueType conventions), so all the
//! affine transformations can be applied on the points directly.

use crate::glyph_data::Outline;

/// Applies x' = a * x + c * y + e, y' = b * x + d * y + f, in place.
pub fn transform(outline: &mut Outline, [a, b, c, d, e, f]: [f64; 6]) {
    for (x, y) in outline.xs.iter_mut().zip(outline.ys.iter_mut()) {
        let (px, py) = (*x, *y);
        *x = a * px + c * py + e;
        *y = b * px + d * py + f;
    }
}

/// Twice the control polygon's signed area; clockwise contours contribute
/// negatively.
fn orientation(outline: &Outline) -> f64 {
    let Outline {
        xs, ys, end_pts, ..
    } = outline;
    let mut area = 0.0;
    let mut start = 0;
    for &end in end_pts {
        let mut px = xs[end];
        let mut py = ys[end];
        for i in start..=end {
            area += (ys[i] - py) * (xs[i] + px);
            px = xs[i];
            py = ys[i];
        }
        start = end + 1;
    }
    area
}

/// Embolden in place using a floating-point adaptation of FreeType's
/// FT_Outline_EmboldenXY. Adjacent-edge bisectors determine point shifts;
/// sharp turns and short edges limit them. Negative strengths thin outlines.
/// Edge lengths use `sqrt(dx * dx + dy * dy)`, not `hypot`, which differs
/// between platforms.
pub fn embolden(outline: &mut Outline, x_strength: f64, y_strength: f64) {
    let x_str = x_strength / 2.0;
    let y_str = y_strength / 2.0;
    if (x_str == 0.0 && y_str == 0.0) || outline.xs.is_empty() {
        return;
    }
    let area = orientation(outline);
    if area == 0.0 {
        return;
    }
    let true_type = area < 0.0;

    let xs = &outline.xs;
    let ys = &outline.ys;
    let mut new_xs = xs.clone();
    let mut new_ys = ys.clone();
    let mut start = 0;
    for &end in &outline.end_pts {
        let n = end - start + 1;
        for i in 0..n {
            let cur = start + i;
            let x = xs[cur];
            let y = ys[cur];
            // Previous and next points at a non-null distance.
            let different = |&k: &usize| xs[k] != x || ys[k] != y;
            let Some(prev) = (1..n).map(|j| start + (i + n - j) % n).find(different) else {
                break; // Degenerate contour.
            };
            let next = (1..n)
                .map(|j| start + (i + j) % n)
                .find(different)
                .unwrap_or(cur);
            let mut in_x = x - xs[prev];
            let mut in_y = y - ys[prev];
            let l_in = (in_x * in_x + in_y * in_y).sqrt();
            in_x /= l_in;
            in_y /= l_in;
            let mut out_x = xs[next] - x;
            let mut out_y = ys[next] - y;
            let l_out = (out_x * out_x + out_y * out_y).sqrt();
            out_x /= l_out;
            out_y /= l_out;

            let mut shift_x = 0.0;
            let mut shift_y = 0.0;
            let mut d = in_x * out_x + in_y * out_y;
            // Shift only if the turn is less than ~160 degrees.
            if d > -0.9375 {
                d += 1.0;
                shift_x = in_y + out_y;
                shift_y = in_x + out_x;
                let mut q = out_x * in_y - out_y * in_x;
                if true_type {
                    shift_x = -shift_x;
                    q = -q;
                } else {
                    shift_y = -shift_y;
                }
                // Restrict the shift magnitude to better handle collapsing
                // segments.
                let l = l_in.min(l_out);
                shift_x *= if x_str * q <= l * d { x_str / d } else { l / q };
                shift_y *= if y_str * q <= l * d { y_str / d } else { l / q };
            }
            new_xs[cur] = x + x_str + shift_x;
            new_ys[cur] = y + y_str + shift_y;
        }
        start = end + 1;
    }
    outline.xs = new_xs;
    outline.ys = new_ys;
}

/// Concatenates outlines.
pub fn concat(parts: impl IntoIterator<Item = Outline>) -> Outline {
    let mut parts = parts.into_iter();
    let mut result = parts.next().unwrap_or_default();
    for outline in parts {
        let offset = result.xs.len();
        result.xs.extend_from_slice(&outline.xs);
        result.ys.extend_from_slice(&outline.ys);
        result.on_curve.extend_from_slice(&outline.on_curve);
        result
            .end_pts
            .extend(outline.end_pts.iter().map(|end| end + offset));
    }
    result
}
