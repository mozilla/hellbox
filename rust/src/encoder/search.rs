// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The choices of the encoder of the model codec (see `model.rs`).
//!
//! The model is the same as the decoder's, the encoder only adds the choice
//! of how each contour is coded: the candidates (the successor, the identical
//! contours and the similar ones) are ranked with an approximate cost, the
//! model cost of the best ones is computed with `CostCoder`, and the lowest
//! score is used. Parameters and IUP touched points are then refined using
//! the same cost model. CostCoder freezes probabilities while summing bit
//! costs; its score is not the final range-coded byte count.
//!
//! Candidate scores use floating-point operations: changing their order can
//! change selected references and encoded bytes. Parallel candidates are
//! merged in scan order (see `best_of`) to preserve tie-breaking.

use super::pool::Pool;
use super::range_encoder::{CostCoder, RangeEncoder};
use crate::glyph_data::MODEL_REVISION;
use crate::model::{
    interpolate_range, magnitude_class, place_deltas, predict_deltas, residual_contexts,
    round_half_up, scale_delta, ContourEntry, ContourRef, ContourState, Decision, Hooks, Model,
    Payload, BLEND_ONE, MAGNITUDE_CLASS_MIN, MODE_REF, MODE_SUCCESSOR, RESIDUAL_VALUE_CONTEXTS,
    RESIDUAL_ZERO_CONTEXTS, SCALE_ONE,
};
use crate::Error;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Limits for the contour-reference search.
#[derive(Clone, Copy, Debug)]
pub struct EncoderOptions {
    /// Number of the most recent contours with the same topology (number of
    /// points and on/off-curve pattern) considered as references.
    pub candidates: usize,
    /// Number of candidates evaluated with `CostCoder`.
    pub exact_costs: usize,
    /// Search threads, including the caller. Defaults to available
    /// parallelism capped at 8; merging preserves sequential search order.
    pub threads: usize,
}

impl Default for EncoderOptions {
    fn default() -> Self {
        EncoderOptions {
            candidates: 32768,
            exact_costs: 16,
            threads: std::thread::available_parallelism().map_or(1, |n| n.get().min(8)),
        }
    }
}

// Blends: number of the best single references which are blended, number of
// partners tried for each of them, number of blends whose cost is computed.
const BLENDED_REFERENCES: usize = 4;
const BLEND_PARTNERS: usize = 1024;
const BLEND_EXACT_COSTS: usize = 4;

const SCALE: f64 = SCALE_ONE as f64;
const BLEND: f64 = BLEND_ONE as f64;

/// Approximate log2(1 + |v|); magnitudes below 4096 are truncated for lookup.
fn log2p1(v: f64) -> f64 {
    let a = v.abs();
    if a < 4096.0 {
        log2p1_table()[a as usize]
    } else {
        (1.0 + a).log2()
    }
}

fn log2p1_table() -> &'static [f64] {
    static TABLE: OnceLock<Vec<f64>> = OnceLock::new();
    TABLE.get_or_init(|| (0..4096).map(|v| (1.0 + v as f64).log2()).collect())
}

/// `log2p1` of an integer.
#[inline]
fn log2p1_int(table: &[f64], v: i64) -> f64 {
    let a = v.unsigned_abs();
    if a < 4096 {
        table[a as usize]
    } else {
        (1.0 + a as f64).log2()
    }
}

/// Factor magnitude below which the integer prediction paths are used.
const EXACT: f64 = 4294967296.0;

#[inline]
fn scale_int(d: i64, k: i64) -> i64 {
    if k == 0 {
        d
    } else {
        (d * (SCALE_ONE + k) + SCALE_ONE / 2) >> 6
    }
}

/// `scale_delta` with floating point numbers.
fn scale_delta_f64(d: f64, k: f64) -> f64 {
    if k == 0.0 {
        d
    } else {
        ((d * (SCALE + k) + SCALE / 2.0) / SCALE).floor()
    }
}

/// `predict_deltas` with floating point numbers.
#[allow(clippy::too_many_arguments)]
fn predict_deltas_f64(
    entry: &ContourEntry,
    kx: f64,
    ky: f64,
    partner: Option<&ContourEntry>,
    bx: f64,
    by: f64,
    hx: f64,
    hy: f64,
) -> (Vec<f64>, Vec<f64>) {
    let n = entry.on.len();
    let mut dx = vec![0.0; n];
    let mut dy = vec![0.0; n];
    let blend = |e: f64, f: f64, k: f64| e + (((f - e) * k + BLEND / 2.0) / BLEND).floor();
    for i in 1..n {
        let ex = entry.dx[i] as f64;
        let ey = entry.dy[i] as f64;
        if let Some(p) = partner {
            dx[i] = blend(ex, p.dx[i] as f64, bx);
            dy[i] = blend(ey, p.dy[i] as f64, by);
        } else if hx != 0.0 || hy != 0.0 {
            dx[i] = ((ex * (SCALE + kx) + ey * hx + SCALE / 2.0) / SCALE).floor();
            dy[i] = ((ey * (SCALE + ky) + ex * hy + SCALE / 2.0) / SCALE).floor();
        } else {
            dx[i] = scale_delta_f64(ex, kx);
            dy[i] = scale_delta_f64(ey, ky);
        }
    }
    (dx, dy)
}

fn hash_contour(on: &[u8], dx: &[i64], dy: &[i64]) -> u32 {
    let n = on.len();
    let mut h = 0x811c_9dc5u32 ^ n as u32;
    for i in 1..n {
        h = (h ^ (dx[i] as u32 & 0xffff)).wrapping_mul(0x0100_0193);
        h = (h ^ (dy[i] as u32 & 0xffff)).wrapping_mul(0x0100_0193);
        h = (h ^ on[i] as u32).wrapping_mul(0x0100_0193);
    }
    h ^ on[0] as u32
}

/// Least squares scale of the reference deltas, as k (see `scale_delta`), from
/// the sums Σ reference × deltas and Σ reference².
fn fit_scale_from(num: i64, den: i64) -> f64 {
    if den == 0 {
        return 0.0;
    }
    (1.0 - SCALE).max(round_half_up((SCALE * num as f64) / den as f64) - SCALE)
}

/// Least squares 2x2 transform of the reference deltas (ex, ey), from the sums
/// Σ ex², Σ ex ey, Σ ey², Σ dx ex, Σ dx ey, Σ dy ex and Σ dy ey.
fn fit_affine_from(s: [i64; 7]) -> Option<Affine> {
    let [sxx, sxy, syy, bx1, bx2, by1, by2] = s.map(|v| v as f64);
    let det = sxx * syy - sxy * sxy;
    if det == 0.0 {
        return None;
    }
    let q = |v: f64| round_half_up(SCALE * v);
    Some(Affine {
        kx: (1.0 - SCALE).max(q((bx1 * syy - bx2 * sxy) / det) - SCALE),
        hx: q((bx2 * sxx - bx1 * sxy) / det),
        hy: q((by1 * syy - by2 * sxy) / det),
        ky: (1.0 - SCALE).max(q((by2 * sxx - by1 * sxy) / det) - SCALE),
    })
}

/// A 2x2 transform, in `SCALE_ONE` units (see `predict_deltas`).
struct Affine {
    kx: f64,
    ky: f64,
    hx: f64,
    hy: f64,
}

/// Least squares blend factor (see `blend_delta`) of a partner, or 0.
fn fit_blend(reference: &[i32], partner: &[i32], deltas: &[i64]) -> f64 {
    let (mut num, mut den) = (0i64, 0i64);
    for i in 1..deltas.len() {
        let g = partner[i] as i64 - reference[i] as i64;
        num += (deltas[i] - reference[i] as i64) * g;
        den += g * g;
    }
    if den == 0 {
        0.0
    } else {
        round_half_up((BLEND * num as f64) / den as f64)
    }
}

/// Approximate cost (in bits) of the corrections of predicted deltas.
fn predicted_residual_cost(pdx: &[f64], pdy: &[f64], dx: &[i64], dy: &[i64], limit: f64) -> f64 {
    let mut cost = 0.0;
    for i in 1..pdx.len() {
        let rx = dx[i] as f64 - pdx[i];
        let ry = dy[i] as f64 - pdy[i];
        if rx != 0.0 || ry != 0.0 {
            cost += 3.0 + 2.0 * log2p1(rx) + 2.0 * log2p1(ry);
            if cost > limit {
                return cost;
            }
        } else {
            cost += 0.3;
        }
    }
    cost
}

/// Sums integer residual costs, stopping once the candidate exceeds the limit.
fn integer_residual_cost(residuals: impl Iterator<Item = (i64, i64)>, limit: f64) -> f64 {
    let table = log2p1_table();
    let mut cost = 0.0;
    for (rx, ry) in residuals {
        if rx != 0 || ry != 0 {
            cost += 3.0 + 2.0 * log2p1_int(table, rx) + 2.0 * log2p1_int(table, ry);
            if cost > limit {
                return cost;
            }
        } else {
            cost += 0.3;
        }
    }
    cost
}

/// `predicted_residual_cost` of an affine prediction.
fn affine_residual_cost(e: &ContourEntry, a: &Affine, dx: &[i64], dy: &[i64], limit: f64) -> f64 {
    if [a.kx, a.ky, a.hx, a.hy].iter().all(|v| v.abs() < EXACT) {
        let (kx, ky, hx, hy) = (a.kx as i64, a.ky as i64, a.hx as i64, a.hy as i64);
        let residuals = (1..dx.len()).map(|i| {
            let (ex, ey) = (e.dx[i] as i64, e.dy[i] as i64);
            let rx = dx[i] - ((ex * (SCALE_ONE + kx) + ey * hx + SCALE_ONE / 2) >> 6);
            let ry = dy[i] - ((ey * (SCALE_ONE + ky) + ex * hy + SCALE_ONE / 2) >> 6);
            (rx, ry)
        });
        return integer_residual_cost(residuals, limit);
    }
    let (pdx, pdy) = predict_deltas_f64(e, a.kx, a.ky, None, 0.0, 0.0, a.hx, a.hy);
    predicted_residual_cost(&pdx, &pdy, dx, dy, limit)
}

/// `predicted_residual_cost` of a blend.
#[allow(clippy::too_many_arguments)]
fn blend_residual_cost(
    e: &ContourEntry,
    p: &ContourEntry,
    bx: f64,
    by: f64,
    dx: &[i64],
    dy: &[i64],
    limit: f64,
) -> f64 {
    if bx.abs() < EXACT && by.abs() < EXACT {
        let (bx, by) = (bx as i64, by as i64);
        let blend = |e: i64, f: i64, k: i64| e + (((f - e) * k + BLEND_ONE / 2) >> 5);
        let residuals = (1..dx.len()).map(|i| {
            let rx = dx[i] - blend(e.dx[i] as i64, p.dx[i] as i64, bx);
            let ry = dy[i] - blend(e.dy[i] as i64, p.dy[i] as i64, by);
            (rx, ry)
        });
        return integer_residual_cost(residuals, limit);
    }
    let (pdx, pdy) = predict_deltas_f64(e, 0.0, 0.0, Some(p), bx, by, 0.0, 0.0);
    predicted_residual_cost(&pdx, &pdy, dx, dy, limit)
}

/// Approximate cost (in bits) of the corrections of a reference.
fn residual_cost(
    entry: &ContourEntry,
    dx: &[i64],
    dy: &[i64],
    kx: f64,
    ky: f64,
    limit: f64,
) -> f64 {
    if kx.abs() < EXACT && ky.abs() < EXACT {
        let (kx, ky) = (kx as i64, ky as i64);
        let residuals = (1..entry.on.len()).map(|i| {
            let rx = dx[i] - scale_int(entry.dx[i] as i64, kx);
            let ry = dy[i] - scale_int(entry.dy[i] as i64, ky);
            (rx, ry)
        });
        return integer_residual_cost(residuals, limit);
    }
    let mut cost = 0.0;
    for i in 1..entry.on.len() {
        let rx = dx[i] as f64 - scale_delta_f64(entry.dx[i] as f64, kx);
        let ry = dy[i] as f64 - scale_delta_f64(entry.dy[i] as f64, ky);
        if rx != 0.0 || ry != 0.0 {
            cost += 3.0 + 2.0 * log2p1(rx) + 2.0 * log2p1(ry);
            if cost > limit {
                return cost;
            }
        } else {
            cost += 0.3;
        }
    }
    cost
}

/// The cost of coding something with the current probabilities of the model.
fn cost_with(model: &mut Model, code: impl FnOnce(&mut Model, &mut CostCoder)) -> f64 {
    let mut coder = CostCoder::new();
    code(model, &mut coder);
    coder.total
}

/// Choose touched points by shortest path over flag and displacement costs,
/// requiring exact IUP interpolation between them. Point 0 is always touched.
/// Costs use frozen model probabilities and omit untouched-point residual flags.
fn iup_touched_set(
    model: &mut Model,
    on: &[u8],
    px: &[i32],
    py: &[i32],
    dx: &[i64],
    dy: &[i64],
) -> Vec<u8> {
    let n = px.len();
    let mut tmp_x = vec![0i64; n];
    let mut tmp_y = vec![0i64; n];
    // Can the points strictly between a and b (b <= n, n being the point 0)
    // be interpolated from a and b?
    let mut interpolates = |a: usize, b: usize| {
        let end = b % n;
        tmp_x[a] = dx[a];
        tmp_x[end] = dx[end];
        tmp_y[a] = dy[a];
        tmp_y[end] = dy[end];
        interpolate_range(px, &mut tmp_x, a, end, a + 1, b);
        interpolate_range(py, &mut tmp_y, a, end, a + 1, b);
        (a + 1..b).all(|k| tmp_x[k] == dx[k] && tmp_y[k] == dy[k])
    };
    // The costs of the flags ([point][previous touched][touched]), and of the
    // displacements ([point][magnitude class of the previous one]).
    let mut flags = vec![[[0.0; 2]; 2]; n];
    let mut costs_dx = vec![[0.0; MAGNITUDE_CLASS_MIN.len()]; n];
    let mut costs_dy = costs_dx.clone();
    for i in 1..n {
        for prev in [false, true] {
            for touched in [false, true] {
                flags[i][usize::from(prev)][usize::from(touched)] = cost_with(model, |m, c| {
                    m.code_iup_flag(c, prev, on[i], i == n - 1, touched);
                });
            }
        }
        for (class, &prev) in MAGNITUDE_CLASS_MIN.iter().enumerate() {
            costs_dx[i][class] = cost_with(model, |m, c| {
                m.code_iup_dx(c, prev, dx[i]);
            });
            costs_dy[i][class] = cost_with(model, |m, c| {
                m.code_iup_dy(c, prev, dx[i] != 0, dy[i]);
            });
        }
    }
    // untouched[i]: the flags of the points 1..i, untouched after untouched
    // ones.
    let mut untouched = vec![0.0; n + 1];
    for i in 1..n {
        untouched[i + 1] = untouched[i] + flags[i][0][0];
    }
    let mut cost = vec![f64::INFINITY; n + 1];
    let mut prev = vec![0usize; n + 1];
    cost[0] = 0.0;
    for b in 1..=n {
        for a in (0..b).rev() {
            let mut step = if b == a + 1 {
                0.0
            } else {
                flags[a + 1][1][0] + untouched[b] - untouched[a + 2]
            };
            // The point n is the point 0: neither flag nor displacement.
            if b < n {
                step += flags[b][usize::from(b == a + 1)][1]
                    + costs_dx[b][magnitude_class(dx[a])]
                    + costs_dy[b][magnitude_class(dy[a])];
            }
            if cost[a] + step < cost[b] && (b == a + 1 || interpolates(a, b)) {
                cost[b] = cost[a] + step;
                prev[b] = a;
            }
        }
    }
    let mut touched = vec![0u8; n];
    touched[0] = 1;
    let mut b = prev[n];
    while b > 0 {
        touched[b] = 1;
        b = prev[b];
    }
    touched
}

/// The longest step between two touched points of `hybrid_touched_set`.
const MAX_HYBRID_STEP: usize = 64;

/// The number of the cheapest references whose touched points are also
/// chosen with `hybrid_touched_set`.
const HYBRID_REFERENCES: usize = 4;

/// Choose touched points with a shortest-path search limited by
/// MAX_HYBRID_STEP. Unlike `iup_touched_set`, steps may include residuals
/// after IUP interpolation; those costs are included in the search.
fn hybrid_touched_set(
    model: &mut Model,
    on: &[u8],
    px: &[i32],
    py: &[i32],
    dx: &[i64],
    dy: &[i64],
) -> Vec<u8> {
    let n = px.len();
    let mut flags = vec![[[0.0; 2]; 2]; n];
    let mut costs_dx = vec![[0.0; MAGNITUDE_CLASS_MIN.len()]; n];
    let mut costs_dy = costs_dx.clone();
    for i in 1..n {
        for prev in [false, true] {
            for touched in [false, true] {
                flags[i][usize::from(prev)][usize::from(touched)] = cost_with(model, |m, c| {
                    m.code_iup_flag(c, prev, on[i], i == n - 1, touched);
                });
            }
        }
        for (class, &prev) in MAGNITUDE_CLASS_MIN.iter().enumerate() {
            costs_dx[i][class] = cost_with(model, |m, c| {
                m.code_iup_dx(c, prev, dx[i]);
            });
            costs_dy[i][class] = cost_with(model, |m, c| {
                m.code_iup_dy(c, prev, dx[i] != 0, dy[i]);
            });
        }
    }
    let mut untouched = vec![0.0; n + 1];
    for i in 1..n {
        untouched[i + 1] = untouched[i] + flags[i][0][0];
    }
    // The costs of the zero flags and of the small values, cached per
    // context.
    const R: i64 = 16;
    let side = (2 * R + 1) as usize;
    let mut zero_costs = [[f64::NAN; 2]; RESIDUAL_ZERO_CONTEXTS];
    let mut value_costs = vec![f64::NAN; RESIDUAL_VALUE_CONTEXTS * side];
    let mut residual_cost = |model: &mut Model, prev: usize, on: u8, rx: i64, ry: i64| {
        let (zero_ctx, x_ctx, y_ctx) = residual_contexts(prev, on);
        let zero = rx == 0 && ry == 0;
        let z = &mut zero_costs[zero_ctx][usize::from(zero)];
        if z.is_nan() {
            *z = cost_with(model, |m, c| {
                m.code_iup_residual_zero(c, zero_ctx, zero);
            });
        }
        let mut cost = *z;
        if !zero {
            for (ctx, v) in [(x_ctx, rx), (y_ctx + usize::from(rx != 0), ry)] {
                if v.abs() > R {
                    cost += cost_with(model, |m, c| {
                        m.code_iup_residual_value(c, ctx, v);
                    });
                    continue;
                }
                let entry = &mut value_costs[ctx * side + (v + R) as usize];
                if entry.is_nan() {
                    *entry = cost_with(model, |m, c| {
                        m.code_iup_residual_value(c, ctx, v);
                    });
                }
                cost += *entry;
            }
        }
        cost
    };
    let mut tmp_x = vec![0i64; n];
    let mut tmp_y = vec![0i64; n];
    let mut cost = vec![f64::INFINITY; n + 1];
    let mut prev = vec![0usize; n + 1];
    cost[0] = 0.0;
    for b in 1..=n {
        for a in (b.saturating_sub(MAX_HYBRID_STEP)..b).rev() {
            let mut step = if b == a + 1 {
                0.0
            } else {
                flags[a + 1][1][0] + untouched[b] - untouched[a + 2]
            };
            if b < n {
                step += flags[b][usize::from(b == a + 1)][1]
                    + costs_dx[b][magnitude_class(dx[a])]
                    + costs_dy[b][magnitude_class(dy[a])];
            }
            if cost[a] + step >= cost[b] {
                continue;
            }
            if b > a + 1 {
                let end = b % n;
                tmp_x[a] = dx[a];
                tmp_x[end] = dx[end];
                tmp_y[a] = dy[a];
                tmp_y[end] = dy[end];
                interpolate_range(px, &mut tmp_x, a, end, a + 1, b);
                interpolate_range(py, &mut tmp_y, a, end, a + 1, b);
                let mut state = 0;
                for k in a + 1..b {
                    let (rx, ry) = (dx[k] - tmp_x[k], dy[k] - tmp_y[k]);
                    step += residual_cost(model, state, on[k], rx, ry);
                    if cost[a] + step >= cost[b] {
                        break;
                    }
                    state = if rx == 0 && ry == 0 { 1 } else { 2 };
                }
            }
            if cost[a] + step < cost[b] {
                cost[b] = cost[a] + step;
                prev[b] = a;
            }
        }
    }
    let mut touched = vec![0u8; n];
    touched[0] = 1;
    let mut b = prev[n];
    while b > 0 {
        touched[b] = 1;
        b = prev[b];
    }
    touched
}

/// A reference, with its approximate cost.
#[derive(Clone)]
struct Candidate {
    entry: usize,
    kx: f64,
    ky: f64,
    hx: f64,
    hy: f64,
    cost: f64,
}

#[derive(Clone)]
struct BlendCandidate {
    entry: usize,
    partner: usize,
    bx: f64,
    by: f64,
    cost: f64,
}

/// Inserts a candidate in a list sorted by cost (after the ones of the same
/// cost), keeping the `max` best ones: returns the new limit.
fn insert<T>(list: &mut Vec<T>, item: T, cost: &impl Fn(&T) -> f64, max: usize) -> Option<f64> {
    list.push(item);
    list.sort_by(|a, b| cost(a).partial_cmp(&cost(b)).unwrap());
    if list.len() > max {
        list.pop();
        return list.last().map(cost);
    }
    None
}

// Items evaluated sequentially before the parallel ones (see `best_of`), and
// the size of the chunks evaluated in parallel.
const SEQUENTIAL_PREFIX: usize = 64;
const CHUNK: usize = 128;

/// Selects the `max` best candidates of the `len` items (`item(i)`), exactly as
/// a sequential scan (`insert`ing them one after the other into `best`), the
/// candidates being evaluated in parallel.
///
/// `evaluate(item, limit)` gives the candidate of an item, if any: its score is
/// fully evaluated below the limit, else it's only known to exceed it (the
/// residuals are summed until the limit is exceeded). An item which is
/// `again` is skipped when `skip(best, item)` (e.g. its entry is already among
/// the best ones).
///
/// After a sequential prefix, chunks retain candidates entering their local
/// top `max`, subject to the prefix limit. Survivors are inserted in scan
/// order to preserve ties. Items marked `again` are evaluated without a limit
/// and checked for eligibility at insertion.
#[allow(clippy::too_many_arguments)]
fn best_of<I, T: Send + Sync>(
    pool: Option<&Pool>,
    len: usize,
    item: impl Fn(usize) -> I + Sync,
    best: &mut Vec<T>,
    limit: &mut f64,
    max: usize,
    cost: impl Fn(&T) -> f64 + Sync,
    again: impl Fn(&I) -> bool + Sync,
    skip: impl Fn(&[T], &I) -> bool,
    evaluate: impl Fn(&I, f64) -> Option<T> + Sync,
) {
    if max == 0 {
        return;
    }
    let add = |best: &mut Vec<T>, limit: &mut f64, item: &I, candidate: Option<T>| {
        if again(item) && skip(best, item) {
            return;
        }
        if let Some(candidate) = candidate.filter(|c| cost(c) < *limit) {
            if let Some(new_limit) = insert(best, candidate, &cost, max) {
                *limit = new_limit;
            }
        }
    };
    // Without a pool, scan all items sequentially.
    let prefix = if pool.is_some() {
        len.min(SEQUENTIAL_PREFIX)
    } else {
        len
    };
    for i in 0..prefix {
        let item = item(i);
        if again(&item) && skip(best, &item) {
            continue;
        }
        let candidate = evaluate(&item, *limit);
        add(best, limit, &item, candidate);
    }
    let start_limit = *limit;
    let Some(pool) = pool.filter(|_| prefix < len) else {
        return;
    };
    let chunks = (len - prefix).div_ceil(CHUNK);
    let results: Vec<OnceLock<Vec<(usize, T)>>> = (0..chunks).map(|_| OnceLock::new()).collect();
    pool.run(chunks, &|c| {
        let out = {
            let mut costs: Vec<f64> = Vec::with_capacity(max + 1);
            let mut local_limit = start_limit;
            let mut out = Vec::new();
            let start = prefix + c * CHUNK;
            for index in start..len.min(start + CHUNK) {
                let item = item(index);
                if again(&item) {
                    out.extend(evaluate(&item, f64::INFINITY).map(|t| (index, t)));
                    continue;
                }
                let Some(candidate) = evaluate(&item, local_limit) else {
                    continue;
                };
                let k = cost(&candidate);
                if k < local_limit {
                    let at = costs.partition_point(|&c| c <= k);
                    costs.insert(at, k);
                    if costs.len() > max {
                        costs.pop();
                    }
                    if costs.len() == max {
                        local_limit = local_limit.min(costs[max - 1]);
                    }
                    out.push((index, candidate));
                }
            }
            out
        };
        let _ = results[c].set(out);
    });
    let survivors = results
        .into_iter()
        .map(|r| r.into_inner().unwrap_or_default());
    for (index, candidate) in survivors.flatten() {
        add(best, limit, &item(index), Some(candidate));
    }
}

/// The contour being coded.
struct Target {
    dx: Vec<i64>,
    dy: Vec<i64>,
    x0: i64,
    y0: i64,
}

/// The encoder hooks of the model.
struct Encoder {
    options: EncoderOptions,
    /// Hash of the topology and deltas => entries.
    index: HashMap<u32, Vec<usize>>,
    /// On/off-curve pattern => entries (in the order of their creation).
    topologies: HashMap<Vec<u8>, Vec<usize>>,
    /// Number of points => number of uses.
    uses: HashMap<usize, u32>,
    /// The `uses` of the last use of each entry.
    last_use: Vec<u32>,
    /// Σ dx², Σ dx dy and Σ dy² of each entry.
    sums: Vec<[i64; 3]>,
    /// The threads of the parallel search, created when needed.
    pool: OnceLock<Pool>,
}

impl Encoder {
    fn new(options: EncoderOptions) -> Self {
        Encoder {
            options,
            index: HashMap::new(),
            topologies: HashMap::new(),
            uses: HashMap::new(),
            last_use: Vec::new(),
            sums: Vec::new(),
            pool: OnceLock::new(),
        }
    }

    /// `Model::predict_start` with floating point numbers.
    fn predict_start(
        model: &Model,
        entry: usize,
        state: &ContourState,
        kx: f64,
        ky: f64,
        together: bool,
    ) -> (f64, f64) {
        let e = &model.entries[entry];
        if !together {
            return (e.start_x as f64, e.start_y as f64);
        }
        (
            state.prev_new_x as f64 + scale_delta_f64((e.start_x - state.prev_old_x) as f64, kx),
            state.prev_new_y as f64 + scale_delta_f64((e.start_y - state.prev_old_y) as f64, ky),
        )
    }

    /// Heuristic reference score, fully evaluated when below the limit:
    /// the costs of its rank, its start point and its corrections, with the
    /// best of its transforms (none, or the scale or the 2x2 transform fitted
    /// with least squares). An `exact` reference is identical to the contour
    /// (no corrections); a `predicted` entry is one of the state's successor
    /// or same-index candidates.
    #[allow(clippy::too_many_arguments)]
    fn evaluate(
        &self,
        model: &Model,
        state: &ContourState,
        target: &Target,
        uses: u32,
        entry: usize,
        exact: bool,
        predicted: bool,
        limit: f64,
    ) -> Candidate {
        let e = &model.entries[entry];
        let (dx, dy) = (&target.dx[..], &target.dy[..]);
        let together = model.is_together(entry, state);
        let rank_cost = if predicted {
            1.0
        } else {
            4.0 + 2.0 * log2p1((uses - self.last_use[entry]) as f64)
        };
        let start_cost = |kx: f64, ky: f64| {
            let (sx, sy) = Self::predict_start(model, entry, state, kx, ky, together);
            2.0 * log2p1(target.x0 as f64 - sx) + 2.0 * log2p1(target.y0 as f64 - sy)
        };
        let base = rank_cost + start_cost(0.0, 0.0);
        let (mut kx, mut ky, mut hx, mut hy) = (0.0, 0.0, 0.0, 0.0);
        let mut cost = base
            + if exact {
                0.0
            } else {
                residual_cost(e, dx, dy, 0.0, 0.0, limit)
            };
        if !exact {
            let pkx = if together { state.kx as f64 } else { 0.0 };
            let pky = if together { state.ky as f64 } else { 0.0 };
            // Integer sums for the least-squares fits (exact, so any order gives
            // the same decisions); reference sums are cached.
            let [sxx, sxy, syy] = self.sums[entry];
            let (mut bx1, mut bx2, mut by1, mut by2) = (0i64, 0i64, 0i64, 0i64);
            for i in 1..dx.len() {
                let (x, y) = (e.dx[i] as i64, e.dy[i] as i64);
                bx1 += dx[i] * x;
                bx2 += dx[i] * y;
                by1 += dy[i] * x;
                by2 += dy[i] * y;
            }
            let fx = fit_scale_from(bx1, sxx);
            let fy = fit_scale_from(by2, syy);
            if fx != 0.0 || fy != 0.0 {
                let scale_cost = 4.0 + 2.0 * log2p1(fx - pkx) + 2.0 * log2p1(fy - pky);
                let scaled = rank_cost
                    + start_cost(fx, fy)
                    + scale_cost
                    + residual_cost(e, dx, dy, fx, fy, limit);
                if scaled < cost {
                    cost = scaled;
                    kx = fx;
                    ky = fy;
                }
            }
            // Full 2x2 transform: scale and shear.
            if let Some(a) = fit_affine_from([sxx, sxy, syy, bx1, bx2, by1, by2]) {
                if a.hx != 0.0 || a.hy != 0.0 {
                    let transformed = rank_cost
                        + start_cost(a.kx, a.ky)
                        + 8.0
                        + 2.0 * log2p1(a.kx - pkx)
                        + 2.0 * log2p1(a.ky - pky)
                        + 2.0 * log2p1(a.hx)
                        + 2.0 * log2p1(a.hy)
                        + affine_residual_cost(e, &a, dx, dy, limit);
                    if transformed < cost {
                        cost = transformed;
                        (kx, ky, hx, hy) = (a.kx, a.ky, a.hx, a.hy);
                    }
                }
            }
        }
        Candidate {
            entry,
            kx,
            ky,
            hx,
            hy,
            cost,
        }
    }

    /// The touched points for exact or hybrid IUP corrections, or None if not needed.
    fn iup_decision(
        model: &mut Model,
        contour: ContourRef,
        decision: &Decision,
        hybrid: bool,
    ) -> Option<Vec<u8>> {
        let entry = &model.entries[decision.entry?];
        let n = entry.on.len();
        if n <= 2 {
            return None;
        }
        let partner = decision.partner.map(|p| &model.entries[p]);
        let (pdx, pdy) = predict_deltas(
            entry,
            decision.kx,
            decision.ky,
            partner,
            decision.bx,
            decision.by,
            decision.hx,
            decision.hy,
        );
        let (px, py) = place_deltas(contour.x[0] as i64, contour.y[0] as i64, &pdx, &pdy);
        let dx: Vec<i64> = (0..n).map(|i| contour.x[i] as i64 - px[i] as i64).collect();
        let dy: Vec<i64> = (0..n).map(|i| contour.y[i] as i64 - py[i] as i64).collect();
        if dx.iter().chain(&dy).all(|&d| d == 0) {
            return None;
        }
        let on = entry.on.clone();
        Some(if hybrid {
            hybrid_touched_set(model, &on, &px, &py, &dx, &dy)
        } else {
            iup_touched_set(model, &on, &px, &py, &dx, &dy)
        })
    }

    /// The decision with hybrid IUP corrections, or None if not needed.
    fn hybrid_decision(
        model: &mut Model,
        contour: ContourRef,
        decision: &Decision,
    ) -> Option<Decision> {
        let touched = Self::iup_decision(model, contour, decision, true)?;
        Some(Decision {
            touched: Some(touched),
            ..decision.clone()
        })
    }

    /// A decision, and the one with IUP corrections if they're possible.
    fn with_iup(model: &mut Model, contour: ContourRef, decision: Decision) -> Vec<Decision> {
        let touched = Self::iup_decision(model, contour, &decision, false);
        let with_touched = touched.map(|t| Decision {
            touched: Some(t),
            ..decision.clone()
        });
        std::iter::once(decision).chain(with_touched).collect()
    }

    /// Refine scale/shear or blend by model cost, one unit at a time,
    /// choosing IUP touched points again for each trial.
    fn refine(
        model: &mut Model,
        contour: ContourRef,
        state: &ContourState,
        best: &mut Decision,
        best_cost: &mut f64,
    ) {
        loop {
            let mut improved = false;
            for param in 0..4 {
                for step in [-1, 1] {
                    let mut decision = Decision {
                        touched: None,
                        ..best.clone()
                    };
                    let value = match (decision.partner.is_some(), param) {
                        (false, 0) => &mut decision.kx,
                        (false, 1) => &mut decision.ky,
                        (false, 2) => &mut decision.hx,
                        (false, 3) => &mut decision.hy,
                        (true, 0) => &mut decision.bx,
                        (true, 1) => &mut decision.by,
                        _ => continue,
                    };
                    *value += step;
                    for d in Self::with_iup(model, contour, decision) {
                        let cost = Self::cost(model, contour, &d, state);
                        if cost < *best_cost {
                            *best_cost = cost;
                            *best = d;
                            improved = true;
                        }
                    }
                }
            }
            if !improved {
                break;
            }
        }
    }

    fn cost(
        model: &mut Model,
        contour: ContourRef,
        decision: &Decision,
        state: &ContourState,
    ) -> f64 {
        let mut coder = CostCoder::new();
        model
            .code_contour(&mut coder, Some((contour, decision)), state)
            .expect("Invalid encoder decision.");
        coder.total
    }
}

impl Hooks for Encoder {
    fn add_entry(&mut self, model: &Model, id: usize) {
        let e = &model.entries[id];
        let dx: Vec<i64> = e.dx.iter().map(|&d| d as i64).collect();
        let dy: Vec<i64> = e.dy.iter().map(|&d| d as i64).collect();
        self.index
            .entry(hash_contour(&e.on, &dx, &dy))
            .or_default()
            .push(id);
        self.topologies.entry(e.on.clone()).or_default().push(id);
        if self.last_use.len() <= id {
            self.last_use.resize(id + 1, 0);
            self.sums.resize(id + 1, [0; 3]);
        }
        let mut sums = [0i64; 3];
        for i in 1..e.on.len() {
            let (x, y) = (e.dx[i] as i64, e.dy[i] as i64);
            sums[0] += x * x;
            sums[1] += x * y;
            sums[2] += y * y;
        }
        self.sums[id] = sums;
    }

    fn touch_entry(&mut self, model: &Model, id: usize) {
        let uses = self.uses.entry(model.entries[id].on.len()).or_insert(0);
        *uses += 1;
        self.last_use[id] = *uses;
    }

    fn choose(&mut self, model: &mut Model, contour: ContourRef, state: &ContourState) -> Decision {
        let n = contour.on.len();
        let deltas = |c: &[i32]| -> Vec<i64> {
            (0..n)
                .map(|i| {
                    if i > 0 {
                        c[i] as i64 - c[i - 1] as i64
                    } else {
                        0
                    }
                })
                .collect()
        };
        let target = Target {
            dx: deltas(contour.x),
            dy: deltas(contour.y),
            x0: contour.x[0] as i64,
            y0: contour.y[0] as i64,
        };
        if let Some(successor) = state.successor {
            let e = &model.entries[successor];
            if e.on == contour.on {
                let together = model.is_together(successor, state);
                let (kx, ky) = if together {
                    (state.kx, state.ky)
                } else {
                    (0, 0)
                };
                let (sx, sy) = model.predict_start(successor, state, kx, ky, together);
                let same = sx == target.x0
                    && sy == target.y0
                    && (1..n).all(|i| {
                        target.dx[i] == scale_delta(e.dx[i] as i64, kx)
                            && target.dy[i] == scale_delta(e.dy[i] as i64, ky)
                    });
                if same {
                    return Decision {
                        mode: MODE_SUCCESSOR,
                        ..Decision::default()
                    };
                }
            }
        }

        let mut best = Decision::default();
        let mut best_cost = Self::cost(model, contour, &best, state);
        if model.lists.get(&(n as u64)).is_none_or(|l| l.length == 0) {
            return best;
        }
        let uses = self.uses.get(&n).copied().unwrap_or(0);

        // The best candidates, according to an approximate cost: the
        // identical entries, the predicted ones (cheap references), and the
        // recent ones with the same topology. The first ones can appear again:
        // they're skipped when they're already among the best.
        let model_ref: &Model = model;
        let mut items: Vec<(usize, bool, bool)> = Vec::new();
        let hash = hash_contour(contour.on, &target.dx, &target.dy);
        for &entry in self.index.get(&hash).into_iter().flatten() {
            let e = &model_ref.entries[entry];
            if e.on == contour.on
                && (1..n).all(|i| e.dx[i] as i64 == target.dx[i] && e.dy[i] as i64 == target.dy[i])
            {
                items.push((entry, true, false));
            }
        }
        let predicted = [state.successor, state.same_index, state.successor2];
        for entry in predicted.into_iter().flatten() {
            if model_ref.entries[entry].on == contour.on {
                items.push((entry, false, true));
            }
        }
        let first: Vec<usize> = items.iter().map(|&(entry, _, _)| entry).collect();
        // The most recent entries first.
        let same: &[usize] = self.topologies.get(contour.on).map_or(&[], |v| v);
        let recent = |i: usize| same[same.len() - 1 - i];
        let scan = |i: usize| {
            items
                .get(i)
                .copied()
                .unwrap_or_else(|| (recent(i - items.len()), false, false))
        };
        let mut top: Vec<Candidate> = Vec::new();
        let mut limit = f64::INFINITY;
        let threads = self.options.threads;
        let pool = (threads > 1).then(|| self.pool.get_or_init(|| Pool::new(threads)));
        best_of(
            pool,
            items.len() + same.len().min(self.options.candidates),
            scan,
            &mut top,
            &mut limit,
            self.options.exact_costs,
            |c| c.cost,
            |&(entry, _, _)| first.contains(&entry),
            |top, &(entry, _, _)| top.iter().any(|t| t.entry == entry),
            |&(entry, exact, predicted), limit| {
                Some(self.evaluate(
                    model_ref, state, &target, uses, entry, exact, predicted, limit,
                ))
            },
        );

        // Blends of the best references with another entry of the same
        // topology.
        let references: Vec<(usize, f64)> = top
            .iter()
            .take(BLENDED_REFERENCES)
            .map(|&Candidate { entry, .. }| {
                let together = model_ref.is_together(entry, state);
                let (sx, sy) = Self::predict_start(model_ref, entry, state, 0.0, 0.0, together);
                let base = 8.0
                    + 2.0 * log2p1((uses - self.last_use[entry]) as f64)
                    + 2.0 * log2p1(target.x0 as f64 - sx)
                    + 2.0 * log2p1(target.y0 as f64 - sy);
                (entry, base)
            })
            .collect();
        let mut blends: Vec<BlendCandidate> = Vec::new();
        let mut blend_limit = f64::INFINITY;
        for &(entry, base) in &references {
            best_of(
                pool,
                same.len().min(BLEND_PARTNERS),
                recent,
                &mut blends,
                &mut blend_limit,
                BLEND_EXACT_COSTS,
                |b| b.cost,
                |_| false,
                |_, _| false,
                |&partner, limit| {
                    if partner == entry {
                        return None;
                    }
                    let (e, p) = (&model_ref.entries[entry], &model_ref.entries[partner]);
                    let bx = fit_blend(&e.dx, &p.dx, &target.dx);
                    let by = fit_blend(&e.dy, &p.dy, &target.dy);
                    if bx == 0.0 && by == 0.0 {
                        return None;
                    }
                    let head = base
                        + 2.0 * log2p1((uses - self.last_use[partner]) as f64)
                        + 2.0 * log2p1(bx)
                        + 2.0 * log2p1(by);
                    let cost = if head >= limit {
                        head
                    } else {
                        head + blend_residual_cost(e, p, bx, by, &target.dx, &target.dy, limit)
                    };
                    Some(BlendCandidate {
                        entry,
                        partner,
                        bx,
                        by,
                        cost,
                    })
                },
            );
        }

        // Score shortlisted references with CostCoder.
        let mut ranked: Vec<(f64, Decision)> = Vec::new();
        let mut references = Vec::new();
        for c in &top {
            references.push((c.entry, c.kx, c.ky, c.hx, c.hy, None, 0.0, 0.0));
            if c.kx != 0.0 || c.ky != 0.0 || c.hx != 0.0 || c.hy != 0.0 {
                references.push((c.entry, 0.0, 0.0, 0.0, 0.0, None, 0.0, 0.0));
            }
        }
        for b in &blends {
            references.push((b.entry, 0.0, 0.0, 0.0, 0.0, Some(b.partner), b.bx, b.by));
        }
        for (entry, kx, ky, hx, hy, partner, bx, by) in references {
            let rank = model.lists[&(n as u64)].rank_of(&model.entries[entry]);
            let decision = Decision {
                mode: MODE_REF,
                entry: Some(entry),
                rank,
                kx: kx as i64,
                ky: ky as i64,
                hx: hx as i64,
                hy: hy as i64,
                partner,
                bx: bx as i64,
                by: by as i64,
                touched: None,
            };
            // The corrections can also be the displacements of a few points.
            let base = decision.clone();
            let mut base_cost = f64::INFINITY;
            for d in Self::with_iup(model, contour, decision) {
                let cost = Self::cost(model, contour, &d, state);
                base_cost = base_cost.min(cost);
                if cost < best_cost {
                    best_cost = cost;
                    best = d;
                }
            }
            ranked.push((base_cost, base));
        }
        // The cheapest references, with interpolations which needn't be exact.
        ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, decision) in ranked.into_iter().take(HYBRID_REFERENCES) {
            if let Some(d) = Self::hybrid_decision(model, contour, &decision) {
                let cost = Self::cost(model, contour, &d, state);
                if cost < best_cost {
                    best_cost = cost;
                    best = d;
                }
            }
        }
        if best.mode == MODE_REF {
            Self::refine(model, contour, state, &mut best, &mut best_cost);
            let base = Decision {
                touched: None,
                ..best.clone()
            };
            if let Some(d) = Self::hybrid_decision(model, contour, &base) {
                if Self::cost(model, contour, &d, state) < best_cost {
                    best = d;
                }
            }
        }
        best
    }
}

/// Codes a payload with the model codec: the model revision, then the range
/// coded data.
pub(crate) fn encode(payload: &Payload, options: EncoderOptions) -> Result<Vec<u8>, Error> {
    let mut coder = RangeEncoder::new();
    let mut encoder = Encoder::new(options);
    Model::new().code(&mut coder, &mut encoder, Some(payload), usize::MAX)?;
    let mut data = vec![MODEL_REVISION];
    data.extend(coder.finish());
    Ok(data)
}
