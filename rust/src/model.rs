// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Context model of the glyph data payload (the model codec, see FORMAT.md),
//! used by both the encoder and the decoder: the same code runs in both
//! directions, the coder either coding the given values (the encoder, and its
//! cost estimation) or returning the decoded ones (the given ones being
//! `None`). Changes to the bitstream format or probability updates require a
//! new `MODEL_REVISION`.
//!
//! The contours are kept in a dictionary (one move-to-front list per number
//! of points), and each contour is coded as:
//! - SUCCESSOR: the contour which followed the previous one the last time it
//!   was used (or the first contour of the previous glyph), at its predicted
//!   position and scale,
//! - REF: a previous contour with the same number of points and the same
//!   on/off-curve pattern, translated and with optional corrections of its
//!   deltas,
//! - NEW: point by point.
//!
//! The encoder chooses the mode and the reference (see `encoder/search.rs`),
//! the decoder just follows.

use crate::range_decoder::{BitModels, RangeDecoder, MAX_DIRECT_BITS};
use crate::Error;
#[cfg(target_arch = "wasm32")]
use std::cell::Cell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A non-cryptographic hasher for integer keys.
#[derive(Default)]
pub(crate) struct IntHasher(u64);

impl Hasher for IntHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

pub(crate) type IntMap<K, V> = HashMap<K, V, BuildHasherDefault<IntHasher>>;

pub(crate) const FIXED_HEADER_SIZE: usize = 28;
pub(crate) const NUM_STRINGS: usize = 5;
const MAX_BITS: usize = 32;
// Number of mantissa bits coded with a binary tree, the other ones having a
// context per position (the lengths and positions above these limits share
// their contexts).
const TREE_BITS: usize = 3;
const MAX_LENGTH_CONTEXT: usize = 20;
const MAX_POSITION_CONTEXT: usize = 16;
const MANTISSA_CONTEXTS: usize = (1 << TREE_BITS) + MAX_POSITION_CONTEXT;

// Per-field decoding limits; these do not bound total allocations.
const MAX_GLYPHS: u64 = 1 << 21;
const MAX_STRING_LENGTH: u64 = 1 << 20;
const MAX_CONTOURS: u64 = 0xffff;
const MAX_POINTS: u64 = 0xffff;
const MAX_LIST_LENGTH: u64 = 0x110000;

pub(crate) const MODE_NEW: u8 = 0;
pub(crate) const MODE_REF: u8 = 1;
pub(crate) const MODE_SUCCESSOR: u8 = 2;

const GLYPH_EMPTY: usize = 0;
const GLYPH_SIMPLE: usize = 1;
const GLYPH_COMPOSITE: usize = 2;

/// The deltas of a reference can be scaled by (SCALE_ONE + k) / SCALE_ONE.
pub(crate) const SCALE_ONE: i64 = 64;
/// A reference can be blended with a second entry (same topology): the
/// deltas are e + (f - e) * k / BLEND_ONE, k being possibly negative or
/// greater than BLEND_ONE.
pub(crate) const BLEND_ONE: i64 = 32;
// Orders (number of previous points) of the point contexts.
const MAX_ORDER: usize = 4;
// Context of a correction: magnitude of the predicted delta, previous
// correction (none, opposite sign of the displacement...) and accumulated
// displacement.
const RESIDUAL_CONTEXTS: usize = 6 * 3 * 3;

/// The payload (see FORMAT.md and `payload.rs`).
#[derive(Clone, Debug, PartialEq)]
pub struct Payload {
    pub fixed: [u8; FIXED_HEADER_SIZE],
    pub strings: Vec<Vec<u8>>,
    /// The glyph id of the first codepoint of the runs (the glyphs before are
    /// only used as components).
    pub first_gid: u64,
    /// Pairs (start delta, modulo 0x110000, length - 1).
    pub runs: Vec<u64>,
    /// Pairs (codepoint delta, glyph id).
    pub aliases: Vec<u64>,
    pub glyphs: Vec<DecodedGlyph>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedGlyph {
    pub shape: Shape,
    pub advance: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Empty,
    /// Absolute coordinates (in grid units), the contours being concatenated.
    Simple {
        sizes: Vec<u16>,
        on: Vec<u8>,
        x: Vec<i32>,
        y: Vec<i32>,
    },
    Composite(Vec<Component>),
}

impl Shape {
    fn kind(&self) -> usize {
        match self {
            Shape::Empty => GLYPH_EMPTY,
            Shape::Simple { .. } => GLYPH_SIMPLE,
            Shape::Composite(_) => GLYPH_COMPOSITE,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub gid: i64,
    pub transform: u32,
    pub dx: i64,
    pub dy: i64,
    /// F2Dot14 values (0, 1, 2 or 4, depending on the transform).
    pub values: Vec<i64>,
}

/// A binary coder: the range decoder, or the range encoder and the cost
/// estimation of the encoder.
pub(crate) trait Coder {
    /// Whether the values are given (encoding) or decoded.
    const ENCODING: bool;

    /// Codes a bit with the probability `models[index]`: returns the given
    /// bit when encoding, the decoded one when decoding.
    fn bit(&mut self, models: &mut BitModels, index: usize, bit: u32) -> u32;

    /// Codes `count` (at most `MAX_DIRECT_BITS`) equiprobable bits, without
    /// model: returns the given value when encoding, the decoded one when
    /// decoding.
    fn direct(&mut self, count: u32, value: u32) -> u32;

    /// Whether the data is truncated (when decoding).
    fn failed(&self) -> bool {
        false
    }
}

impl Coder for RangeDecoder<'_> {
    const ENCODING: bool = false;

    #[inline]
    fn bit(&mut self, models: &mut BitModels, index: usize, _bit: u32) -> u32 {
        self.decode_bit(models, index)
    }

    #[inline]
    fn direct(&mut self, count: u32, _value: u32) -> u32 {
        self.decode_direct(count)
    }

    fn failed(&self) -> bool {
        self.truncated
    }
}

/// How the encoder codes a contour (see `encoder/search.rs`).
#[derive(Clone, Debug, Default)]
pub(crate) struct Decision {
    pub mode: u8,
    /// The reference, and its rank in the list of its number of points.
    pub entry: Option<usize>,
    pub rank: usize,
    pub kx: i64,
    pub ky: i64,
    pub hx: i64,
    pub hy: i64,
    pub partner: Option<usize>,
    pub bx: i64,
    pub by: i64,
    /// The touched points of the IUP corrections.
    pub touched: Option<Vec<u8>>,
}

/// A contour as absolute points.
#[derive(Clone, Copy)]
pub(crate) struct ContourRef<'a> {
    pub on: &'a [u8],
    pub x: &'a [i32],
    pub y: &'a [i32],
}

/// The given outline of a simple glyph.
#[derive(Clone, Copy)]
struct OutlineRef<'a> {
    sizes: &'a [u16],
    on: &'a [u8],
    x: &'a [i32],
    y: &'a [i32],
}

/// The choices of the encoder, which also indexes the entries of the
/// dictionary (the decoder has none).
pub(crate) trait Hooks {
    /// A new entry has been added to the dictionary.
    fn add_entry(&mut self, _model: &Model, _id: usize) {}

    /// An entry has been used (added, or moved to the end of its list).
    fn touch_entry(&mut self, _model: &Model, _id: usize) {}

    /// How to code a contour.
    fn choose(
        &mut self,
        _model: &mut Model,
        _contour: ContourRef,
        _state: &ContourState,
    ) -> Decision {
        Decision::default()
    }
}

/// The decoder's hooks.
pub(crate) struct NoHooks;

impl Hooks for NoHooks {}

fn check_limit(value: u64, max: u64) -> Result<u64, Error> {
    if value > max {
        Err(Error::InvalidData)
    } else {
        Ok(value)
    }
}

/// Unsigned integers: adaptive length and mantissa coding of value + 1.
struct UIntModels {
    unary: BitModels,
    mantissa: BitModels,
    /// The length is coded with a binary tree (see `TREE_LENGTHS`) rather
    /// than in unary, followed by a unary extension for longer lengths.
    length_tree: bool,
    /// The number of mantissa bits coded with the models, the next ones being
    /// direct (equiprobable) bits.
    modeled_bits: usize,
    /// The number of mantissa models per length of a context.
    mantissa_contexts: usize,
}

/// The lengths coded by the tree of a `UIntModels` (a power of two), the
/// last one meaning "this one or more" (the next ones are in unary).
const TREE_LENGTHS: usize = 16;

impl UIntModels {
    fn new(num_contexts: usize) -> Self {
        UIntModels {
            unary: BitModels::new(num_contexts * MAX_BITS),
            mantissa: BitModels::new(num_contexts * (MAX_LENGTH_CONTEXT + 1) * MANTISSA_CONTEXTS),
            length_tree: false,
            modeled_bits: MAX_BITS,
            mantissa_contexts: MANTISSA_CONTEXTS,
        }
    }

    fn with_length_tree(num_contexts: usize) -> Self {
        UIntModels {
            length_tree: true,
            ..Self::new(num_contexts)
        }
    }

    /// New-contour delta magnitudes: tree-coded length, one modeled mantissa
    /// bit, then direct bits. The other bits are close to random, where
    /// adaptive models cost more than one bit each.
    fn for_magnitudes(num_contexts: usize) -> Self {
        // Only node 1 is used; node 0 is unused.
        let mantissa_contexts = 2;
        UIntModels {
            unary: BitModels::new(num_contexts * MAX_BITS),
            mantissa: BitModels::new(num_contexts * (MAX_LENGTH_CONTEXT + 1) * mantissa_contexts),
            length_tree: true,
            modeled_bits: 1,
            mantissa_contexts,
        }
    }

    fn code<C: Coder>(&mut self, c: &mut C, ctx: usize, value: u64) -> u64 {
        let v = if C::ENCODING { value + 1 } else { 0 };
        let n = if C::ENCODING {
            64 - v.leading_zeros() as usize
        } else {
            0
        };
        let base = ctx * MAX_BITS;
        let mut length = 1;
        if self.length_tree {
            // The nodes of the tree are the first TREE_LENGTHS - 1 models of
            // the context, the next ones code the longer lengths in unary.
            let index = n.clamp(1, TREE_LENGTHS) - 1;
            let mut node = 1;
            for k in (0..TREE_LENGTHS.trailing_zeros()).rev() {
                let bit = ((index >> k) & 1) as u32;
                node = node * 2 + c.bit(&mut self.unary, base + node, bit) as usize;
            }
            length = node - TREE_LENGTHS + 1;
        }
        if length == TREE_LENGTHS || !self.length_tree {
            while length < MAX_BITS
                && c.bit(&mut self.unary, base + length, u32::from(length < n)) != 0
            {
                length += 1;
            }
        }
        let mantissa_base = (ctx * (MAX_LENGTH_CONTEXT + 1) + length.min(MAX_LENGTH_CONTEXT))
            * self.mantissa_contexts;
        let mut result: u64 = 1;
        let modeled = (length - 1).min(self.modeled_bits);
        for j in 0..modeled {
            // Position of the bit, from the end.
            let k = length - 2 - j;
            let index = if j < TREE_BITS {
                mantissa_base + result as usize
            } else {
                mantissa_base + (1 << TREE_BITS) + k.min(MAX_POSITION_CONTEXT - 1)
            };
            result = result * 2 + c.bit(&mut self.mantissa, index, ((v >> k) & 1) as u32) as u64;
        }
        let mut remaining = length - 1 - modeled;
        while remaining > 0 {
            let count = remaining.min(MAX_DIRECT_BITS as usize);
            remaining -= count;
            let given = ((v >> remaining) & ((1 << count) - 1)) as u32;
            result = (result << count) | c.direct(count as u32, given) as u64;
        }
        result - 1
    }
}

/// Signed integers: zero flag, sign and magnitude.
struct SIntModels {
    zero: BitModels,
    sign: BitModels,
    magnitude: UIntModels,
}

impl SIntModels {
    fn new(num_contexts: usize) -> Self {
        SIntModels {
            zero: BitModels::new(num_contexts),
            sign: BitModels::new(num_contexts),
            magnitude: UIntModels::new(num_contexts),
        }
    }

    /// The magnitude's length being coded with a tree (see `UIntModels`).
    fn with_length_tree(num_contexts: usize) -> Self {
        SIntModels {
            magnitude: UIntModels::with_length_tree(num_contexts),
            ..Self::new(num_contexts)
        }
    }

    fn code<C: Coder>(&mut self, c: &mut C, ctx: usize, value: i64) -> i64 {
        if c.bit(&mut self.zero, ctx, u32::from(value == 0)) != 0 {
            return 0;
        }
        let negative = c.bit(&mut self.sign, ctx, u32::from(value < 0)) != 0;
        let given = if C::ENCODING {
            value.unsigned_abs() - 1
        } else {
            0
        };
        let magnitude = self.magnitude.code(c, ctx, given) as i64 + 1;
        if negative {
            -magnitude
        } else {
            magnitude
        }
    }
}

/// Bytes, with a context.
struct ByteModels {
    models: BitModels,
}

impl ByteModels {
    fn new(num_contexts: usize) -> Self {
        ByteModels {
            models: BitModels::new(num_contexts * 256),
        }
    }

    fn code<C: Coder>(&mut self, c: &mut C, ctx: usize, value: u8) -> u8 {
        let base = ctx * 256;
        let mut node = 1;
        for k in (0..8).rev() {
            node =
                node * 2 + c.bit(&mut self.models, base + node, ((value >> k) & 1) as u32) as usize;
        }
        (node - 256) as u8
    }
}

/// The number of contexts of the zero flags and of the values of the
/// residuals of the IUP corrections (see `residual_contexts`).
pub(crate) const RESIDUAL_ZERO_CONTEXTS: usize = 6;
pub(crate) const RESIDUAL_VALUE_CONTEXTS: usize = 9;

/// The contexts of the residual of an untouched point of IUP corrections,
/// after the state of the previous point (0 touched, 1 untouched without
/// residual, 2 with one): of the zero flag, of the x value and of the y value
/// (to which 1 is added when the x value isn't 0).
pub(crate) fn residual_contexts(prev: usize, on: u8) -> (usize, usize, usize) {
    (prev * 2 + on as usize, prev, 3 + prev * 2)
}

/// The smallest magnitude of each class of `magnitude_class` (for the costs
/// of the encoder).
#[cfg(any(test, feature = "encoder"))]
pub(crate) const MAGNITUDE_CLASS_MIN: [i64; 6] = [0, 1, 3, 9, 33, 129];

pub(crate) fn magnitude_class(v: i64) -> usize {
    match v.unsigned_abs() {
        0 => 0,
        1..=2 => 1,
        3..=8 => 2,
        9..=32 => 3,
        33..=128 => 4,
        _ => 5,
    }
}

fn sign3(v: i64) -> usize {
    match v {
        v if v > 0 => 2,
        v if v < 0 => 0,
        _ => 1,
    }
}

fn residual_context(predicted: i64, previous: i64, displacement: i64) -> usize {
    let p = if previous == 0 {
        0
    } else if previous.abs() <= 2 {
        1
    } else {
        2
    };
    let d = if displacement == 0 {
        0
    } else if (displacement > 0) == (predicted >= 0) {
        1
    } else {
        2
    };
    (magnitude_class(predicted) * 3 + p) * 3 + d
}

pub(crate) fn scale_delta(d: i64, k: i64) -> i64 {
    if k == 0 {
        d
    } else {
        (d.wrapping_mul(SCALE_ONE + k) + SCALE_ONE / 2).div_euclid(SCALE_ONE)
    }
}

fn blend_delta(e: i64, f: i64, k: i64) -> i64 {
    e + ((f - e).wrapping_mul(k) + BLEND_ONE / 2).div_euclid(BLEND_ONE)
}

/// The rounding of the model: the closest integer, ties towards +infinity, as
/// in its integer computations (`scale_delta`, `blend_delta`). The encoder
/// and the decoder must round the same way.
pub(crate) fn round_half_up(v: f64) -> f64 {
    let f = v.floor();
    if v - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

const HASH_SEED: u32 = 0x9e37_79b1;

/// Mixes a delta, with the on-curve state of its end, into a hash.
#[inline]
fn hash_item(h: u32, (dx, dy, on): (i32, i32, u8)) -> u32 {
    let h = (h ^ dx as u32).wrapping_mul(0x85eb_ca6b);
    let h = (h ^ dy as u32).wrapping_mul(0xc2b2_ae35);
    (h ^ on as u32).wrapping_mul(0x27d4_eb2f)
}

/// The hash of a context: the hash of its deltas, with its order and the
/// on-curve state of the point before them.
#[inline]
fn finish_hash(h: u32, order: usize, first_on: u8) -> u32 {
    let h = hash_item(h, (order as i32, 0, first_on));
    h ^ (h >> 15)
}

/// The hash of a context: its deltas, from the last one (so that the
/// contexts of increasing orders extend each other, see `context_hashes`),
/// with the on-curve state of their ends, and the on-curve state of the point
/// before them.
fn hash_items(order: usize, items: impl Iterator<Item = (i32, i32, u8)>, first_on: u8) -> u32 {
    finish_hash(items.fold(HASH_SEED, hash_item), order, first_on)
}

/// The hashes of the contexts of the point i, for the orders 1 to
/// min(MAX_ORDER, i - 1) (the other ones are 0).
fn context_hashes(on: &[u8], x: &[i32], y: &[i32], i: usize) -> [u32; MAX_ORDER] {
    let mut hashes = [0; MAX_ORDER];
    let mut h = HASH_SEED;
    for order in 1..=MAX_ORDER.min(i.saturating_sub(1)) {
        let k = i - order;
        let delta = (x[k].wrapping_sub(x[k - 1]), y[k].wrapping_sub(y[k - 1]));
        h = hash_item(h, (delta.0, delta.1, on[k]));
        hashes[order - 1] = finish_hash(h, order, on[k - 1]);
    }
    hashes
}

/// The transforms of the symmetric predictions: a rotation by 180° (the
/// deltas in the same order), and the left-right and up-down mirrors (in the
/// reverse order).
const SYMMETRIES: [(i32, i32, bool); 3] = [(-1, -1, false), (1, -1, true), (-1, 1, true)];
/// The number of deltas of the contexts of the symmetric predictions.
const SYMMETRY_ORDER: usize = 2;

/// All the points coded so far (as deltas), and the position of the point
/// which followed the last occurrence of each context (hashed sequence of the
/// previous points of the contour), to predict the next point (LZP).
struct PointHistory {
    on: Vec<u8>,
    dx: Vec<i32>,
    dy: Vec<i32>,
    /// The index of each point in its contour.
    index: Vec<u16>,
    mask: u32,
    /// The position + 1 of the point which followed the context of each slot
    /// (0 for none: the table is allocated zeroed) in the low `POSITION_BITS`,
    /// and the high bits of its hash (the slot being given by the low bits)
    /// in the other ones.
    slots: Vec<u32>,
}

/// The bits of the positions of the slots of `PointHistory`: the points after
/// the first 2^POSITION_BITS - 1 ones aren't added to the lookup table.
const POSITION_BITS: u32 = 20;
const POSITION_MASK: u32 = (1 << POSITION_BITS) - 1;

#[cfg(target_arch = "wasm32")]
impl Drop for PointHistory {
    fn drop(&mut self) {
        // The biggest table is kept.
        let slots = std::mem::take(&mut self.slots);
        let kept = HISTORY_SLOTS.take();
        HISTORY_SLOTS.set(if slots.capacity() >= kept.capacity() {
            slots
        } else {
            kept
        });
    }
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// Reuses the largest dropped slot buffer on this wasm thread.
    /// `PointHistory::new` clears it before use. A new table wouldn't reuse
    /// the freed memory (smaller allocations take it in between) and wasm
    /// memory never shrinks; natively, a freed table goes back to the system.
    static HISTORY_SLOTS: Cell<Vec<u32>> = const { Cell::new(Vec::new()) };
}

impl PointHistory {
    fn new(num_glyphs: u64) -> Self {
        let lz = ((num_glyphs * 128) as u32).leading_zeros();
        let bits = (32 - lz).clamp(14, 21);
        #[cfg(target_arch = "wasm32")]
        let mut slots = HISTORY_SLOTS.take();
        #[cfg(not(target_arch = "wasm32"))]
        let mut slots = Vec::new();
        slots.clear();
        slots.resize(1 << bits, 0);
        PointHistory {
            on: Vec::new(),
            dx: Vec::new(),
            dy: Vec::new(),
            index: Vec::new(),
            mask: (1 << bits) - 1,
            slots,
        }
    }

    /// A history without table (before the number of glyphs is known).
    fn empty() -> Self {
        PointHistory {
            on: Vec::new(),
            dx: Vec::new(),
            dy: Vec::new(),
            index: Vec::new(),
            mask: 0,
            slots: Vec::new(),
        }
    }

    fn lookup(&self, hash: u32) -> i64 {
        let slot = self.slots[(hash & self.mask) as usize];
        if (slot ^ hash) & !POSITION_MASK == 0 {
            (slot & POSITION_MASK) as i64 - 1
        } else {
            -1
        }
    }

    /// Adds a contour, with the hashes of the contexts of its points when
    /// they're known (else they're computed).
    fn add(&mut self, on: &[u8], x: &[i32], y: &[i32], hashes: &[[u32; MAX_ORDER]]) {
        for i in 0..on.len() {
            let pos = self.on.len() as i32;
            // Each contour has at most MAX_POINTS points.
            self.index.push(i as u16);
            self.on.push(on[i]);
            self.dx.push(if i > 0 {
                x[i].wrapping_sub(x[i - 1])
            } else {
                0
            });
            self.dy.push(if i > 0 {
                y[i].wrapping_sub(y[i - 1])
            } else {
                0
            });
            let point_hashes = match hashes.get(i) {
                Some(&hashes) => hashes,
                None => context_hashes(on, x, y, i),
            };
            let mut order = 1;
            while order <= MAX_ORDER && order < i {
                let hash = point_hashes[order - 1];
                if (pos as u32) < POSITION_MASK {
                    self.slots[(hash & self.mask) as usize] =
                        (hash & !POSITION_MASK) | (pos as u32 + 1);
                }
                order += 1;
            }
        }
    }
}

/// Maps (order, hash) to the last point following that context in the current
/// contour. Stamps let the open-addressing table be reused across contours.
#[derive(Default)]
struct LocalContexts {
    slots: Vec<LocalSlot>,
    mask: usize,
    stamp: u32,
}

#[derive(Clone, Copy, Default)]
struct LocalSlot {
    hash: u32,
    order: u32,
    stamp: u32,
    position: u32,
}

impl LocalContexts {
    /// Starts a contour with room for MAX_ORDER contexts per point at a load
    /// factor of at most 1/2. Clears stale entries only on stamp overflow.
    fn reset(&mut self, n: usize) {
        let size = (2 * MAX_ORDER * n).next_power_of_two();
        if size > self.slots.len() {
            self.slots.resize(size, LocalSlot::default());
        }
        self.mask = size - 1;
        if self.stamp == u32::MAX {
            self.slots.fill(LocalSlot::default());
            self.stamp = 0;
        }
        self.stamp += 1;
    }

    fn first_slot(&self, order: usize, hash: u32) -> usize {
        (hash ^ (order as u32).wrapping_mul(HASH_SEED)) as usize & self.mask
    }

    fn get(&self, order: usize, hash: u32) -> Option<usize> {
        let mut i = self.first_slot(order, hash);
        loop {
            let slot = &self.slots[i];
            if slot.stamp != self.stamp {
                return None;
            }
            if slot.hash == hash && slot.order == order as u32 {
                return Some(slot.position as usize);
            }
            i = (i + 1) & self.mask;
        }
    }

    fn insert(&mut self, order: usize, hash: u32, position: usize) {
        let mut i = self.first_slot(order, hash);
        loop {
            let slot = &mut self.slots[i];
            if slot.stamp != self.stamp || (slot.hash == hash && slot.order == order as u32) {
                *slot = LocalSlot {
                    hash,
                    order: order as u32,
                    stamp: self.stamp,
                    position: position as u32,
                };
                return;
            }
            i = (i + 1) & self.mask;
        }
    }
}

/// A contour of the dictionary: its points as deltas (dx[0] = dy[0] = 0), the
/// start of its last occurrence, the entry of the contour which followed it
/// and the glyph of its last occurrence.
pub(crate) struct ContourEntry {
    pub on: Vec<u8>,
    pub dx: Vec<i32>,
    pub dy: Vec<i32>,
    pub start_x: i64,
    pub start_y: i64,
    next: Option<usize>,
    /// The previous successor, when `next` changed.
    next2: Option<usize>,
    pub last_glyph: i64,
    slot: usize,
}

/// Entries in last-use order, with rank 0 for the most recent. A Fenwick tree
/// counts occupied slots for rank lookups. Full slot arrays are compacted.
pub(crate) struct RecencyList {
    slots: Vec<Option<usize>>,
    tree: Vec<i32>,
    capacity: usize,
    next: usize,
    pub length: usize,
}

/// Initial slot capacity. Compaction reserves room beyond the live entries.
const RECENCY_CAPACITY: usize = 16;

impl RecencyList {
    fn new() -> Self {
        RecencyList {
            slots: vec![None; RECENCY_CAPACITY],
            tree: vec![0; RECENCY_CAPACITY + 1],
            capacity: RECENCY_CAPACITY,
            next: 0,
            length: 0,
        }
    }

    #[allow(clippy::manual_isolate_lowest_one)]
    fn add(&mut self, slot: usize, delta: i32) {
        let mut i = slot + 1;
        while i <= self.capacity {
            self.tree[i] += delta;
            // The lowest set bit (Fenwick tree).
            i += i & i.wrapping_neg();
        }
    }

    /// Number of used slots in [0, slot].
    #[allow(clippy::manual_isolate_lowest_one)]
    fn prefix(&self, slot: usize) -> usize {
        let mut sum = 0;
        let mut i = slot + 1;
        while i > 0 {
            sum += self.tree[i];
            i -= i & i.wrapping_neg();
        }
        sum as usize
    }

    fn compact(&mut self, entries: &mut [ContourEntry]) {
        let ids: Vec<usize> = self.slots.iter().flatten().copied().collect();
        let lz = (ids.len() as u32).leading_zeros();
        self.capacity = (1usize << (33 - lz)).max(RECENCY_CAPACITY);
        self.tree = vec![0; self.capacity + 1];
        self.slots = vec![None; self.capacity];
        self.next = 0;
        for id in ids {
            entries[id].slot = self.next;
            self.slots[self.next] = Some(id);
            self.add(self.next, 1);
            self.next += 1;
        }
    }

    fn push(&mut self, id: usize, entries: &mut [ContourEntry]) {
        if self.next == self.capacity {
            self.compact(entries);
        }
        entries[id].slot = self.next;
        self.slots[self.next] = Some(id);
        self.add(self.next, 1);
        self.next += 1;
        self.length += 1;
    }

    fn move_to_end(&mut self, id: usize, entries: &mut [ContourEntry]) {
        let slot = entries[id].slot;
        self.add(slot, -1);
        self.slots[slot] = None;
        self.length -= 1;
        self.push(id, entries);
    }

    /// The rank of an entry (0 for the most recently used).
    pub fn rank_of(&self, entry: &ContourEntry) -> usize {
        self.length - self.prefix(entry.slot)
    }

    /// The entry with the given rank (0 for the most recently used).
    fn at(&self, rank: usize) -> Option<usize> {
        let mut k = (self.length - rank) as i32;
        let mut pos = 0;
        let mut step = self.capacity;
        while step > 0 {
            if pos + step <= self.capacity && self.tree[pos + step] < k {
                pos += step;
                k -= self.tree[pos];
            }
            step >>= 1;
        }
        self.slots.get(pos).copied().flatten()
    }
}

/// State of the outline being coded.
pub(crate) struct ContourState {
    /// Last point of the previous contour.
    px: i64,
    py: i64,
    /// Start of the previous contour, and of the occurrence of its reference.
    pub prev_new_x: i64,
    pub prev_new_y: i64,
    pub prev_old_x: i64,
    pub prev_old_y: i64,
    /// Scale of the previous contour (if it was a reference).
    pub kx: i64,
    pub ky: i64,
    prev_mode: u8,
    /// Glyph of the previous use of the entry of the previous contour.
    prev_glyph: i64,
    /// The predicted references: the entry which followed the one of the
    /// previous contour (or the first one of the previous glyph), and the
    /// contour at the same index in the previous glyph.
    pub successor: Option<usize>,
    pub same_index: Option<usize>,
    /// The contour which followed the previous contour's entry before its
    /// successor.
    pub successor2: Option<usize>,
    /// The number of points of the previous contour, 0 for the first one.
    prev_n: usize,
    index: usize,
}

pub(crate) struct Contour {
    mode: u8,
    entry: Option<usize>,
    exact: bool,
    kx: i64,
    ky: i64,
    on: Vec<u8>,
    x: Vec<i32>,
    y: Vec<i32>,
    /// The hashes of the contexts of its points, when they were coded one by
    /// one (see `PointHistory::add`).
    hashes: Vec<[u32; MAX_ORDER]>,
}

/// The prediction of the deltas of a reference: transformed (scaled, and
/// optionally sheared: hx is the contribution of dy to dx and hy the one of dx
/// to dy, in `SCALE_ONE` units), or blended with a partner.
#[derive(Clone, Copy)]
struct DeltaPrediction<'a> {
    entry: &'a ContourEntry,
    partner: Option<&'a ContourEntry>,
    kx: i64,
    ky: i64,
    bx: i64,
    by: i64,
    hx: i64,
    hy: i64,
}

impl DeltaPrediction<'_> {
    /// The predicted delta i (from the point i - 1, i > 0).
    #[inline]
    fn delta(&self, i: usize) -> (i64, i64) {
        let ex = self.entry.dx[i] as i64;
        let ey = self.entry.dy[i] as i64;
        if let Some(p) = self.partner {
            (
                blend_delta(ex, p.dx[i] as i64, self.bx),
                blend_delta(ey, p.dy[i] as i64, self.by),
            )
        } else if self.hx != 0 || self.hy != 0 {
            (
                (ex.wrapping_mul(SCALE_ONE + self.kx) + ey.wrapping_mul(self.hx) + SCALE_ONE / 2)
                    .div_euclid(SCALE_ONE),
                (ey.wrapping_mul(SCALE_ONE + self.ky) + ex.wrapping_mul(self.hy) + SCALE_ONE / 2)
                    .div_euclid(SCALE_ONE),
            )
        } else {
            (scale_delta(ex, self.kx), scale_delta(ey, self.ky))
        }
    }

    /// Accumulates predicted deltas from (x0, y0).
    fn points(&self, x0: i64, y0: i64) -> (Vec<i32>, Vec<i32>) {
        let n = self.entry.on.len();
        let mut x = Vec::with_capacity(n);
        let mut y = Vec::with_capacity(n);
        let (mut px, mut py) = (x0, y0);
        x.push(px as i32);
        y.push(py as i32);
        for i in 1..n {
            let (dx, dy) = self.delta(i);
            px = px.wrapping_add(dx);
            py = py.wrapping_add(dy);
            x.push(px as i32);
            y.push(py as i32);
        }
        (x, y)
    }
}

/// Predicted deltas of a reference (see `DeltaPrediction`).
#[cfg(feature = "encoder")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_deltas(
    entry: &ContourEntry,
    kx: i64,
    ky: i64,
    partner: Option<&ContourEntry>,
    bx: i64,
    by: i64,
    hx: i64,
    hy: i64,
) -> (Vec<i64>, Vec<i64>) {
    let prediction = DeltaPrediction {
        entry,
        partner,
        kx,
        ky,
        bx,
        by,
        hx,
        hy,
    };
    let n = entry.on.len();
    let mut dx = vec![0; n];
    let mut dy = vec![0; n];
    for i in 1..n {
        (dx[i], dy[i]) = prediction.delta(i);
    }
    (dx, dy)
}

#[cfg(feature = "encoder")]
pub(crate) fn place_deltas(x0: i64, y0: i64, dx: &[i64], dy: &[i64]) -> (Vec<i32>, Vec<i32>) {
    let n = dx.len();
    let mut x = vec![0; n];
    let mut y = vec![0; n];
    let mut px = x0;
    let mut py = y0;
    x[0] = px as i32;
    y[0] = py as i32;
    for i in 1..n {
        px = px.wrapping_add(dx[i]);
        py = py.wrapping_add(dy[i]);
        x[i] = px as i32;
        y[i] = py as i32;
    }
    (x, y)
}

/// Interpolates the deltas of the points from `start` to `end` (excluded)
/// from the ones of the points a and b (IUP).
pub(crate) fn interpolate_range(
    coords: &[i32],
    deltas: &mut [i64],
    a: usize,
    b: usize,
    start: usize,
    end: usize,
) {
    let mut x1 = coords[a] as i64;
    let mut x2 = coords[b] as i64;
    let mut d1 = deltas[a];
    let mut d2 = deltas[b];
    if x1 > x2 {
        std::mem::swap(&mut x1, &mut x2);
        std::mem::swap(&mut d1, &mut d2);
    }
    for k in start..end {
        let x = coords[k] as i64;
        deltas[k] = if x1 == x2 {
            if d1 == d2 {
                d1
            } else {
                0
            }
        } else if x <= x1 {
            d1
        } else if x >= x2 {
            d2
        } else {
            round_half_up(d1 as f64 + ((x - x1) as f64 * (d2 - d1) as f64) / (x2 - x1) as f64)
                as i64
        };
    }
}

/// IUP (as in the TrueType variations): the displacements (`deltas`) of the
/// untouched points are interpolated from the touched ones (the point 0
/// always is), from the coordinates of the points (here the predicted ones):
/// linearly between the coordinates of the previous and next touched points
/// (in the contour order), or the displacement of the closest of them when
/// outside.
// The loop goes up to n, to close the contour.
#[allow(clippy::needless_range_loop)]
fn interpolate_untouched(coords: &[i32], deltas: &mut [i64], touched: &[u8]) {
    let n = coords.len();
    let mut last = 0;
    for i in 1..=n {
        if i < n && touched[i] == 0 {
            continue;
        }
        if i - last > 1 {
            interpolate_range(coords, deltas, last, i % n, last + 1, i);
        }
        last = i;
    }
}

pub(crate) struct Model {
    header: ByteModels,
    text: ByteModels,
    counts: UIntModels,
    cmap: UIntModels,
    glyph_type: BitModels,
    same_advance: BitModels,
    advance: SIntModels,
    mode: BitModels,
    predicted_ref: BitModels,
    rank: UIntModels,
    translation: SIntModels,
    scaled: BitModels,
    blended: BitModels,
    sheared: BitModels,
    shear: SIntModels,
    blend: SIntModels,
    scale: SIntModels,
    exact: BitModels,
    point_residual: BitModels,
    iup_mode: BitModels,
    iup_touched: BitModels,
    iup_delta: SIntModels,
    iup_residual_zero: BitModels,
    iup_residual: SIntModels,
    residual_x: SIntModels,
    residual_y: SIntModels,
    on_curve: BitModels,
    zero_x: BitModels,
    zero_y: BitModels,
    sign_x: BitModels,
    sign_y: BitModels,
    mag_x: UIntModels,
    mag_y: UIntModels,
    component_gid: SIntModels,
    transform: BitModels,
    offset: SIntModels,
    transform_value: SIntModels,
    last_base_gid: i64,
    prediction_hit: BitModels,
    symmetry_hit: BitModels,
    /// The contour dictionary: the entries, and their lists by number of
    /// points.
    pub entries: Vec<ContourEntry>,
    pub lists: IntMap<u64, RecencyList>,
    prev_glyph_entries: Vec<usize>,
    glyph_index: i64,
    history: PointHistory,
    local: LocalContexts,
    /// The number of glyphs of the payload.
    num_glyphs: usize,
}

impl Model {
    /// The model of the encoder (the decoder needs the glyph models only when
    /// it decodes glyphs, see `decode_glyphs`).
    #[cfg(feature = "encoder")]
    pub(crate) fn new() -> Self {
        Self::with_glyph_models(true)
    }

    /// A model whose models of the glyphs are empty when they aren't needed
    /// (to decode only the header, strings and cmap: they are big, and
    /// initializing them is a good part of the decoding of a few glyphs).
    fn with_glyph_models(glyphs: bool) -> Self {
        let n = |size: usize| if glyphs { size } else { 0 };
        Model {
            header: ByteModels::new(FIXED_HEADER_SIZE),
            text: ByteModels::new(256),
            counts: UIntModels::new(16),
            cmap: UIntModels::new(4),
            glyph_type: BitModels::new(n(3 * 2)),
            same_advance: BitModels::new(n(3)),
            advance: SIntModels::with_length_tree(n(3)),
            mode: BitModels::new(n(64)),
            predicted_ref: BitModels::new(n(6)),
            rank: UIntModels::with_length_tree(n(8)),
            translation: SIntModels::new(n(8)),
            scaled: BitModels::new(n(16)),
            blended: BitModels::new(n(4)),
            sheared: BitModels::new(n(4)),
            shear: SIntModels::new(n(3)),
            blend: SIntModels::new(n(3)),
            scale: SIntModels::new(n(6)),
            exact: BitModels::new(n(8)),
            point_residual: BitModels::new(n(16)),
            iup_mode: BitModels::new(n(8)),
            iup_touched: BitModels::new(n(8)),
            iup_delta: SIntModels::new(n(18)),
            iup_residual_zero: BitModels::new(n(RESIDUAL_ZERO_CONTEXTS)),
            iup_residual: SIntModels::new(n(RESIDUAL_VALUE_CONTEXTS)),
            residual_x: SIntModels::new(n(RESIDUAL_CONTEXTS)),
            residual_y: SIntModels::new(n(RESIDUAL_CONTEXTS * 2)),
            on_curve: BitModels::new(n(8)),
            zero_x: BitModels::new(n(24 * 4)),
            zero_y: BitModels::new(n(24 * 4)),
            sign_x: BitModels::new(n(24 * 3 * 3)),
            sign_y: BitModels::new(n(24 * 3 * 3)),
            mag_x: UIntModels::for_magnitudes(n(24 * 6)),
            mag_y: UIntModels::for_magnitudes(n(24 * 6)),
            component_gid: SIntModels::new(n(2)),
            transform: BitModels::new(n(4)),
            offset: SIntModels::new(n(4)),
            transform_value: SIntModels::new(n(1)),
            last_base_gid: 0,
            prediction_hit: BitModels::new(n(MAX_ORDER * 8 * 2)),
            symmetry_hit: BitModels::new(n(SYMMETRIES.len() * 2 * 2 * 2)),
            entries: Vec::new(),
            lists: IntMap::default(),
            prev_glyph_entries: Vec::new(),
            glyph_index: 0,
            history: PointHistory::empty(),
            local: LocalContexts::default(),
            num_glyphs: 0,
        }
    }

    /// Codes a payload: the given one when encoding, else the decoded one,
    /// which is returned, with only its first `limit` glyphs (the glyphs are
    /// coded in order, so that a decoder can stop after the ones it needs).
    pub(crate) fn code<C: Coder, H: Hooks>(
        &mut self,
        c: &mut C,
        hooks: &mut H,
        input: Option<&Payload>,
        limit: usize,
    ) -> Result<Payload, Error> {
        let input = if C::ENCODING { input } else { None };
        let mut fixed = input.map_or([0; FIXED_HEADER_SIZE], |p| p.fixed);
        for (i, byte) in fixed.iter_mut().enumerate() {
            *byte = self.header.code(c, i, *byte);
        }
        let given = input.map_or(0, |p| p.glyphs.len() as u64);
        let num_glyphs = check_limit(self.counts.code(c, 0, given), MAX_GLYPHS)?;
        self.num_glyphs = num_glyphs as usize;
        let limit = limit.min(self.num_glyphs);
        if limit > 0 {
            self.history = PointHistory::new(num_glyphs);
        }
        let mut strings = Vec::with_capacity(NUM_STRINGS);
        for i in 0..NUM_STRINGS {
            let given = input.map(|p| &p.strings[i][..]);
            let length = check_limit(
                self.counts.code(c, 1, given.map_or(0, |s| s.len() as u64)),
                MAX_STRING_LENGTH,
            )? as usize;
            let mut string = Vec::with_capacity(length);
            let mut prev = 0;
            for j in 0..length {
                prev = self.text.code(c, prev as usize, given.map_or(0, |s| s[j]));
                string.push(prev);
            }
            strings.push(string);
        }
        let given = input.map_or(0, |p| p.first_gid);
        let first_gid = check_limit(self.counts.code(c, 6, given), MAX_GLYPHS)?;
        let runs = self.code_list(c, 0, input.map(|p| &p.runs[..]))?;
        let aliases = self.code_list(c, 2, input.map(|p| &p.aliases[..]))?;

        let mut glyphs: Vec<DecodedGlyph> = Vec::with_capacity(limit);
        let mut prev_type = GLYPH_SIMPLE;
        let mut prev_advance = 0;
        for gid in 0..limit {
            if c.failed() {
                return Err(Error::InvalidData);
            }
            let given = input.map(|p| &p.glyphs[gid]);
            let glyph = self.code_glyph(c, hooks, given, prev_type, prev_advance, &glyphs, gid)?;
            prev_type = glyph.shape.kind();
            prev_advance = glyph.advance;
            glyphs.push(glyph);
        }
        if c.failed() {
            return Err(Error::InvalidData);
        }
        Ok(Payload {
            fixed,
            strings,
            first_gid,
            runs,
            aliases,
            glyphs,
        })
    }

    /// A list of pairs of unsigned integers.
    fn code_list<C: Coder>(
        &mut self,
        c: &mut C,
        ctx: usize,
        input: Option<&[u64]>,
    ) -> Result<Vec<u64>, Error> {
        let input = if C::ENCODING { input } else { None };
        let given = input.map_or(0, |l| l.len() as u64 / 2);
        let count = check_limit(self.counts.code(c, 2, given), MAX_LIST_LENGTH)? as usize;
        let mut list = Vec::with_capacity(count * 2);
        for i in 0..count * 2 {
            let given = input.map_or(0, |l| l[i]);
            list.push(self.cmap.code(c, ctx + i % 2, given));
        }
        Ok(list)
    }

    #[allow(clippy::too_many_arguments)]
    fn code_glyph<C: Coder, H: Hooks>(
        &mut self,
        c: &mut C,
        hooks: &mut H,
        input: Option<&DecodedGlyph>,
        prev_type: usize,
        prev_advance: i64,
        glyphs: &[DecodedGlyph],
        gid: usize,
    ) -> Result<DecodedGlyph, Error> {
        let input = if C::ENCODING { input } else { None };
        let given_type = input.map_or(GLYPH_EMPTY, |g| g.shape.kind());
        let mut glyph_type = GLYPH_EMPTY;
        if c.bit(
            &mut self.glyph_type,
            prev_type * 2,
            u32::from(given_type != GLYPH_EMPTY),
        ) != 0
        {
            glyph_type = if c.bit(
                &mut self.glyph_type,
                prev_type * 2 + 1,
                u32::from(given_type == GLYPH_COMPOSITE),
            ) != 0
            {
                GLYPH_COMPOSITE
            } else {
                GLYPH_SIMPLE
            };
        }
        let mut x_min = 0i64;
        let mut x_max = 0i64;
        let shape = match glyph_type {
            GLYPH_SIMPLE => {
                let given = input.and_then(|g| match &g.shape {
                    Shape::Simple { sizes, on, x, y } => Some(OutlineRef { sizes, on, x, y }),
                    _ => None,
                });
                let (sizes, on, x, y) = self.code_outline(c, hooks, given)?;
                x_min = x.iter().copied().min().unwrap_or(0) as i64;
                x_max = x.iter().copied().max().unwrap_or(0) as i64;
                Shape::Simple { sizes, on, x, y }
            }
            GLYPH_COMPOSITE => {
                let given = input.and_then(|g| match &g.shape {
                    Shape::Composite(components) => Some(&components[..]),
                    _ => None,
                });
                Shape::Composite(self.code_components(c, given)?)
            }
            _ => Shape::Empty,
        };

        // Advance: same as the previous glyph, or predicted from the outline.
        let given_advance = input.map_or(0, |g| g.advance);
        let advance = if c.bit(
            &mut self.same_advance,
            glyph_type,
            u32::from(C::ENCODING && given_advance == prev_advance),
        ) != 0
        {
            prev_advance
        } else {
            let mut predicted = prev_advance;
            match &shape {
                Shape::Simple { .. } => predicted = x_max + x_min.max(0),
                Shape::Composite(components) => {
                    let base = components[0].gid;
                    if base < 0 {
                        return Err(Error::InvalidData);
                    }
                    if (base as usize) < gid {
                        predicted = glyphs[base as usize].advance;
                    }
                }
                Shape::Empty => {}
            }
            predicted + self.advance.code(c, glyph_type, given_advance - predicted)
        };
        Ok(DecodedGlyph { shape, advance })
    }

    #[allow(clippy::type_complexity)]
    fn code_outline<C: Coder, H: Hooks>(
        &mut self,
        c: &mut C,
        hooks: &mut H,
        input: Option<OutlineRef>,
    ) -> Result<(Vec<u16>, Vec<u8>, Vec<i32>, Vec<i32>), Error> {
        let input = if C::ENCODING { input } else { None };
        let given = input.map_or(0, |o| o.sizes.len().saturating_sub(1) as u64);
        let num_contours = check_limit(self.counts.code(c, 3, given) + 1, MAX_CONTOURS)? as usize;
        let mut state = ContourState {
            px: 0,
            py: 0,
            prev_new_x: 0,
            prev_new_y: 0,
            prev_old_x: 0,
            prev_old_y: 0,
            kx: 0,
            ky: 0,
            prev_mode: MODE_NEW,
            prev_glyph: -1,
            successor: None,
            same_index: None,
            successor2: None,
            prev_n: 0,
            index: 0,
        };
        let glyph_index = self.glyph_index;
        self.glyph_index += 1;
        let mut prev_entry: Option<usize> = None;
        let mut entries = Vec::with_capacity(num_contours);
        let mut sizes = Vec::with_capacity(num_contours);
        let mut all_on = Vec::new();
        let mut all_x = Vec::new();
        let mut all_y = Vec::new();
        let mut offset = 0;
        for index in 0..num_contours {
            if c.failed() {
                return Err(Error::InvalidData);
            }
            let given = input.map(|o| {
                let n = o.sizes[index] as usize;
                let range = offset..offset + n;
                offset += n;
                ContourRef {
                    on: &o.on[range.clone()],
                    x: &o.x[range.clone()],
                    y: &o.y[range],
                }
            });
            state.successor = if index == 0 {
                self.prev_glyph_entries.first().copied()
            } else {
                prev_entry.and_then(|e| self.entries[e].next)
            };
            state.same_index = self.prev_glyph_entries.get(index).copied();
            state.successor2 = if index == 0 {
                None
            } else {
                prev_entry.and_then(|e| self.entries[e].next2)
            };
            state.index = index;
            let decision = given.map(|contour| hooks.choose(self, contour, &state));
            let contour = self.code_contour(c, given.zip(decision.as_ref()), &state)?;
            let n = contour.on.len();
            self.history
                .add(&contour.on, &contour.x, &contour.y, &contour.hashes);
            sizes.push(n as u16);
            all_on.extend_from_slice(&contour.on);
            all_x.extend_from_slice(&contour.x);
            all_y.extend_from_slice(&contour.y);

            // Update the dictionary.
            let list_key = n as u64;
            let (previous_glyph, old_x, old_y) = match contour.entry {
                Some(e) if contour.mode != MODE_NEW => (
                    self.entries[e].last_glyph,
                    self.entries[e].start_x,
                    self.entries[e].start_y,
                ),
                _ => (-1, contour.x[0] as i64, contour.y[0] as i64),
            };
            let new_entry = if contour.mode == MODE_NEW || !contour.exact {
                let deltas = |v: &[i32]| -> Vec<i32> {
                    std::iter::once(0)
                        .chain(v.windows(2).map(|w| w[1].wrapping_sub(w[0])))
                        .collect()
                };
                let id = self.entries.len();
                self.entries.push(ContourEntry {
                    on: contour.on,
                    dx: deltas(&contour.x),
                    dy: deltas(&contour.y),
                    start_x: 0,
                    start_y: 0,
                    next: None,
                    next2: None,
                    last_glyph: -1,
                    slot: 0,
                });
                hooks.add_entry(self, id);
                self.lists
                    .entry(list_key)
                    .or_insert_with(RecencyList::new)
                    .push(id, &mut self.entries);
                id
            } else {
                let id = contour.entry.ok_or(Error::InvalidData)?;
                self.lists
                    .entry(list_key)
                    .or_insert_with(RecencyList::new)
                    .move_to_end(id, &mut self.entries);
                id
            };
            hooks.touch_entry(self, new_entry);
            {
                let entry = &mut self.entries[new_entry];
                entry.start_x = contour.x[0] as i64;
                entry.start_y = contour.y[0] as i64;
                entry.last_glyph = glyph_index;
            }
            state.prev_glyph = previous_glyph;
            if let Some(p) = prev_entry {
                let e = &mut self.entries[p];
                if e.next != Some(new_entry) {
                    e.next2 = e.next;
                    e.next = Some(new_entry);
                }
            }
            prev_entry = Some(new_entry);
            entries.push(new_entry);
            state.px = contour.x[n - 1] as i64;
            state.py = contour.y[n - 1] as i64;
            state.prev_new_x = contour.x[0] as i64;
            state.prev_new_y = contour.y[0] as i64;
            state.prev_old_x = old_x;
            state.prev_old_y = old_y;
            state.kx = contour.kx;
            state.ky = contour.ky;
            state.prev_mode = contour.mode;
            state.prev_n = n;
        }
        self.prev_glyph_entries = entries;
        Ok((sizes, all_on, all_x, all_y))
    }

    /// Whether an entry was used in the same glyph as the previous contour.
    pub(crate) fn is_together(&self, entry: usize, state: &ContourState) -> bool {
        state.prev_mode != MODE_NEW && self.entries[entry].last_glyph == state.prev_glyph
    }

    /// Predicted start of a reference: same place as its last occurrence,
    /// or, when it was used in the same glyph as the previous contour, at the
    /// same (scaled) offset from it.
    pub(crate) fn predict_start(
        &self,
        entry: usize,
        state: &ContourState,
        kx: i64,
        ky: i64,
        together: bool,
    ) -> (i64, i64) {
        let e = &self.entries[entry];
        if !together {
            return (e.start_x, e.start_y);
        }
        (
            state.prev_new_x + scale_delta(e.start_x - state.prev_old_x, kx),
            state.prev_new_y + scale_delta(e.start_y - state.prev_old_y, ky),
        )
    }

    /// Codes a contour, without modifying the dictionary (so the encoder can
    /// also use it to compute the cost of the different possibilities).
    pub(crate) fn code_contour<C: Coder>(
        &mut self,
        c: &mut C,
        input: Option<(ContourRef, &Decision)>,
        state: &ContourState,
    ) -> Result<Contour, Error> {
        let input = if C::ENCODING { input } else { None };
        let mode_ctx = state.prev_mode as usize * 2 + usize::from(state.index == 0);
        let given_mode = input.map_or(MODE_NEW, |(_, d)| d.mode);
        let mut mode = MODE_NEW;
        if state.successor.is_some()
            && c.bit(
                &mut self.mode,
                mode_ctx,
                u32::from(given_mode == MODE_SUCCESSOR),
            ) != 0
        {
            mode = MODE_SUCCESSOR;
        } else if c.bit(
            &mut self.mode,
            8 + mode_ctx * 2 + usize::from(state.successor.is_some()),
            u32::from(given_mode == MODE_REF),
        ) != 0
        {
            mode = MODE_REF;
        }

        match mode {
            MODE_SUCCESSOR => {
                // Same scale as the previous contour, when both come from the
                // same glyph.
                let entry = state.successor.ok_or(Error::InvalidData)?;
                let together = self.is_together(entry, state);
                let (kx, ky) = if together {
                    (state.kx, state.ky)
                } else {
                    (0, 0)
                };
                let (x0, y0) = self.predict_start(entry, state, kx, ky, together);
                let e = &self.entries[entry];
                let prediction = DeltaPrediction {
                    entry: e,
                    partner: None,
                    kx,
                    ky,
                    bx: 0,
                    by: 0,
                    hx: 0,
                    hy: 0,
                };
                let (x, y) = prediction.points(x0, y0);
                Ok(Contour {
                    mode,
                    entry: Some(entry),
                    exact: kx == 0 && ky == 0,
                    kx,
                    ky,
                    on: e.on.clone(),
                    x,
                    y,
                    hashes: Vec::new(),
                })
            }
            MODE_REF => self.code_reference(c, input, state),
            _ => {
                let (on, x, y, hashes) = self.code_new_contour(c, input.map(|(k, _)| k), state)?;
                Ok(Contour {
                    mode,
                    entry: None,
                    exact: false,
                    kx: 0,
                    ky: 0,
                    on,
                    x,
                    y,
                    hashes,
                })
            }
        }
    }

    fn code_reference<C: Coder>(
        &mut self,
        c: &mut C,
        input: Option<(ContourRef, &Decision)>,
        state: &ContourState,
    ) -> Result<Contour, Error> {
        let input = if C::ENCODING { input } else { None };
        let contour = input.map(|(k, _)| k);
        let decision = input.map(|(_, d)| d);
        // The reference is one of the predicted entries, or given by its
        // number of points and its rank in the list.
        let mut predicted = [0; 3];
        let mut num_predicted = 0;
        for entry in [state.successor, state.same_index, state.successor2]
            .into_iter()
            .flatten()
        {
            if !predicted[..num_predicted].contains(&entry) {
                predicted[num_predicted] = entry;
                num_predicted += 1;
            }
        }
        let predictions = &predicted[..num_predicted];
        let given_entry = decision.and_then(|d| d.entry);
        let first_ctx = if predictions.len() == 3 {
            3
        } else {
            predictions.len().saturating_sub(1)
        };
        let entry = if !predictions.is_empty()
            && c.bit(
                &mut self.predicted_ref,
                first_ctx,
                u32::from(given_entry.is_some_and(|e| predictions.contains(&e))),
            ) != 0
        {
            match predictions.len() {
                1 => predictions[0],
                2 => {
                    if c.bit(
                        &mut self.predicted_ref,
                        2,
                        u32::from(given_entry == Some(predictions[1])),
                    ) == 0
                    {
                        predictions[0]
                    } else {
                        predictions[1]
                    }
                }
                _ => {
                    if c.bit(
                        &mut self.predicted_ref,
                        4,
                        u32::from(given_entry != Some(predictions[0])),
                    ) == 0
                    {
                        predictions[0]
                    } else if c.bit(
                        &mut self.predicted_ref,
                        5,
                        u32::from(given_entry == Some(predictions[2])),
                    ) == 0
                    {
                        predictions[1]
                    } else {
                        predictions[2]
                    }
                }
            }
        } else {
            // The number of points, after the one of the previous contour.
            let given = contour.map_or(0, |k| k.on.len() as u64 - 1);
            let count_ctx = 8 + match state.prev_n {
                0 => 0,
                1..=4 => 1,
                5..=8 => 2,
                9..=12 => 3,
                13..=16 => 4,
                17..=24 => 5,
                25..=40 => 6,
                _ => 7,
            };
            let n = self.counts.code(c, count_ctx, given) + 1;
            let given = decision.map_or(0, |d| d.rank as u64);
            let rank = self.rank.code(c, (n >> 2).min(3) as usize, given);
            let list = self.lists.get(&n).ok_or(Error::InvalidData)?;
            if rank >= list.length as u64 {
                return Err(Error::InvalidData);
            }
            list.at(rank as usize).ok_or(Error::InvalidData)?
        };
        let n = self.entries[entry].on.len();
        let size_class = (n >> 2).min(3);
        // References last used in the same glyph share scale and position predictors.
        let together = self.is_together(entry, state);
        let together_ctx = usize::from(together);
        let prev_scaled = usize::from(state.kx != 0 || state.ky != 0);

        // Optional blending with a second entry with the same topology (e.g.
        // an interpolation between a short and a long version of a stroke).
        let same_size_length = self.lists.get(&(n as u64)).map_or(0, |l| l.length);
        let mut partner = None;
        let mut bx = 0;
        let mut by = 0;
        let given_partner = decision.and_then(|d| d.partner);
        if same_size_length > 1
            && c.bit(
                &mut self.blended,
                size_class,
                u32::from(given_partner.is_some()),
            ) != 0
        {
            let given = given_partner.map_or(0, |p| {
                self.lists[&(n as u64)].rank_of(&self.entries[p]) as u64
            });
            let rank = self.rank.code(c, 4 + size_class, given);
            if rank >= same_size_length as u64 {
                return Err(Error::InvalidData);
            }
            let p = self.lists[&(n as u64)]
                .at(rank as usize)
                .ok_or(Error::InvalidData)?;
            if p == entry || self.entries[p].on != self.entries[entry].on {
                return Err(Error::InvalidData);
            }
            partner = Some(p);
            bx = self.blend.code(c, 0, decision.map_or(0, |d| d.bx));
            let ctx = if bx == 0 { 1 } else { 2 };
            by = self.blend.code(c, ctx, decision.map_or(0, |d| d.by));
        }

        // Optional scaling (and shear) of the deltas (around the start point).
        let scaled = if partner.is_some() {
            0
        } else {
            let given = decision.is_some_and(|d| d.kx != 0 || d.ky != 0 || d.hx != 0 || d.hy != 0);
            c.bit(
                &mut self.scaled,
                (together_ctx * 2 + prev_scaled) * 4 + size_class,
                u32::from(given),
            ) as usize
        };
        let (mut kx, mut ky, mut hx, mut hy) = (0, 0, 0, 0);
        if scaled != 0 {
            let pkx = if together { state.kx } else { 0 };
            let pky = if together { state.ky } else { 0 };
            kx = pkx
                + self
                    .scale
                    .code(c, together_ctx * 3, decision.map_or(0, |d| d.kx - pkx));
            let ctx = together_ctx * 3 + if kx == pkx { 1 } else { 2 };
            ky = pky + self.scale.code(c, ctx, decision.map_or(0, |d| d.ky - pky));
            // Optional shear (e.g. for slanted strokes).
            let given = decision.is_some_and(|d| d.hx != 0 || d.hy != 0);
            if c.bit(&mut self.sheared, size_class, u32::from(given)) != 0 {
                hx = self.shear.code(c, 0, decision.map_or(0, |d| d.hx));
                let ctx = if hx == 0 { 1 } else { 2 };
                hy = self.shear.code(c, ctx, decision.map_or(0, |d| d.hy));
            }
        }

        let (px0, py0) = self.predict_start(entry, state, kx, ky, together);
        let given = contour.map_or(0, |k| k.x[0] as i64 - px0);
        let rx = self.translation.code(c, together_ctx * 2, given);
        let x0 = px0 + rx;
        let given = contour.map_or(0, |k| k.y[0] as i64 - py0);
        // Knowing whether the x one is wrong.
        let y_ctx = 4 + together_ctx * 2 + usize::from(rx != 0);
        let y0 = py0 + self.translation.code(c, y_ctx, given);
        let prediction = DeltaPrediction {
            entry: &self.entries[entry],
            partner: partner.map(|p| &self.entries[p]),
            kx,
            ky,
            bx,
            by,
            hx,
            hy,
        };
        let (mut x, mut y) = prediction.points(x0, y0);
        let on = self.entries[entry].on.clone();
        let is_exact = contour.is_none_or(|k| k.x == x && k.y == y);
        let exact = c.bit(
            &mut self.exact,
            size_class * 2 + scaled,
            u32::from(is_exact),
        ) != 0;
        let touched = decision.and_then(|d| d.touched.as_deref());
        let iup = !exact
            && n > 2
            && c.bit(
                &mut self.iup_mode,
                size_class * 2 + scaled,
                u32::from(touched.is_some()),
            ) != 0;
        if iup {
            self.code_iup_residual(c, contour.zip(touched), &on, &mut x, &mut y);
        } else if !exact {
            let mut prev_zero = 1;
            let mut cx = x[0] as i64;
            let mut cy = y[0] as i64;
            // Previous corrections and accumulated displacement, per axis.
            let (mut prx, mut pry, mut acc_x, mut acc_y) = (0i64, 0i64, 0i64, 0i64);
            for i in 1..n {
                let (mut dx, mut dy) = prediction.delta(i);
                let rx = contour.map_or(0, |k| k.x[i] as i64 - k.x[i - 1] as i64 - dx);
                let ry = contour.map_or(0, |k| k.y[i] as i64 - k.y[i - 1] as i64 - dy);
                let zero = c.bit(
                    &mut self.point_residual,
                    ((scaled * 2 + prev_zero) * 2 + on[i] as usize) * 2
                        + usize::from(acc_x != 0 || acc_y != 0),
                    u32::from(rx == 0 && ry == 0),
                ) as usize;
                let mut decoded_x = 0;
                let mut decoded_y = 0;
                if zero == 0 {
                    decoded_x = self
                        .residual_x
                        .code(c, residual_context(dx, prx, acc_x), rx);
                    let ctx = residual_context(dy, pry, acc_y) * 2 + usize::from(decoded_x != 0);
                    decoded_y = self.residual_y.code(c, ctx, ry);
                    dx += decoded_x;
                    dy += decoded_y;
                }
                prx = decoded_x;
                pry = decoded_y;
                acc_x += decoded_x;
                acc_y += decoded_y;
                prev_zero = zero;
                cx = cx.wrapping_add(dx);
                cy = cy.wrapping_add(dy);
                x[i] = cx as i32;
                y[i] = cy as i32;
            }
        }
        Ok(Contour {
            mode: MODE_REF,
            entry: Some(entry),
            exact: exact && scaled == 0 && partner.is_none(),
            kx,
            ky,
            on,
            x,
            y,
            hashes: Vec::new(),
        })
    }

    /// Codes whether the point of IUP corrections after a touched (or an
    /// untouched) point is touched (the encoder also uses it for the costs).
    pub(crate) fn code_iup_flag<C: Coder>(
        &mut self,
        c: &mut C,
        prev_touched: bool,
        on: u8,
        last: bool,
        touched: bool,
    ) -> bool {
        let ctx = (usize::from(prev_touched) * 2 + on as usize) * 2 + usize::from(last);
        c.bit(&mut self.iup_touched, ctx, u32::from(touched)) != 0
    }

    /// Codes the x displacement of a touched point, after the one of the
    /// previous touched point (the encoder also uses it for the costs).
    pub(crate) fn code_iup_dx<C: Coder>(&mut self, c: &mut C, prev: i64, dx: i64) -> i64 {
        self.iup_delta.code(c, magnitude_class(prev), dx)
    }

    /// Codes the y displacement of a touched point, after the one of the
    /// previous touched point, knowing whether its x displacement is 0.
    pub(crate) fn code_iup_dy<C: Coder>(
        &mut self,
        c: &mut C,
        prev: i64,
        dx_nonzero: bool,
        dy: i64,
    ) -> i64 {
        let ctx = 6 + magnitude_class(prev) * 2 + usize::from(dx_nonzero);
        self.iup_delta.code(c, ctx, dy)
    }

    /// Codes whether the residual of an untouched point of IUP corrections is
    /// zero (see `residual_contexts`; the encoder also uses it for the costs).
    pub(crate) fn code_iup_residual_zero<C: Coder>(
        &mut self,
        c: &mut C,
        ctx: usize,
        zero: bool,
    ) -> bool {
        c.bit(&mut self.iup_residual_zero, ctx, u32::from(zero)) != 0
    }

    /// Codes a residual value of an untouched point of IUP corrections (see
    /// `residual_contexts`; the encoder also uses it for the costs).
    pub(crate) fn code_iup_residual_value<C: Coder>(
        &mut self,
        c: &mut C,
        ctx: usize,
        v: i64,
    ) -> i64 {
        self.iup_residual.code(c, ctx, v)
    }

    /// Codes the residual of an untouched point of IUP corrections, after the
    /// state `prev` of the previous point (see `residual_contexts`).
    fn code_iup_point_residual<C: Coder>(
        &mut self,
        c: &mut C,
        prev: usize,
        on: u8,
        rx: i64,
        ry: i64,
    ) -> (i64, i64) {
        let (zero_ctx, x_ctx, y_ctx) = residual_contexts(prev, on);
        if self.code_iup_residual_zero(c, zero_ctx, rx == 0 && ry == 0) {
            return (0, 0);
        }
        let rx = self.code_iup_residual_value(c, x_ctx, rx);
        let ry = self.code_iup_residual_value(c, y_ctx + usize::from(rx != 0), ry);
        (rx, ry)
    }

    /// Corrections as the displacements of a few touched points, the other
    /// ones being interpolated, then corrected by a residual.
    fn code_iup_residual<C: Coder>(
        &mut self,
        c: &mut C,
        input: Option<(ContourRef, &[u8])>,
        on: &[u8],
        x: &mut [i32],
        y: &mut [i32],
    ) {
        let n = on.len();
        let mut touched = vec![0u8; n];
        touched[0] = 1;
        let mut prev_touched = true;
        for i in 1..n {
            let given = input.is_some_and(|(_, t)| t[i] != 0);
            prev_touched = self.code_iup_flag(c, prev_touched, on[i], i == n - 1, given);
            touched[i] = u8::from(prev_touched);
        }
        let mut deltas_x = vec![0i64; n];
        let mut deltas_y = vec![0i64; n];
        let mut pdx = 0;
        let mut pdy = 0;
        for i in 1..n {
            if touched[i] == 0 {
                continue;
            }
            let given = input.map_or(0, |(k, _)| k.x[i] as i64 - x[i] as i64);
            pdx = self.code_iup_dx(c, pdx, given);
            deltas_x[i] = pdx;
            let given = input.map_or(0, |(k, _)| k.y[i] as i64 - y[i] as i64);
            pdy = self.code_iup_dy(c, pdy, pdx != 0, given);
            deltas_y[i] = pdy;
        }
        interpolate_untouched(x, &mut deltas_x, &touched);
        interpolate_untouched(y, &mut deltas_y, &touched);
        let mut prev = 0;
        for i in 1..n {
            if touched[i] != 0 {
                prev = 0;
                continue;
            }
            let (gx, gy) = input.map_or((0, 0), |(k, _)| {
                (
                    k.x[i] as i64 - x[i] as i64 - deltas_x[i],
                    k.y[i] as i64 - y[i] as i64 - deltas_y[i],
                )
            });
            let (rx, ry) = self.code_iup_point_residual(c, prev, on[i], gx, gy);
            deltas_x[i] += rx;
            deltas_y[i] += ry;
            prev = if rx == 0 && ry == 0 { 1 } else { 2 };
        }
        for i in 1..n {
            x[i] = (x[i] as i64).wrapping_add(deltas_x[i]) as i32;
            y[i] = (y[i] as i64).wrapping_add(deltas_y[i]) as i32;
        }
    }

    #[allow(clippy::type_complexity)]
    fn code_new_contour<C: Coder>(
        &mut self,
        c: &mut C,
        input: Option<ContourRef>,
        state: &ContourState,
    ) -> Result<(Vec<u8>, Vec<i32>, Vec<i32>, Vec<[u32; MAX_ORDER]>), Error> {
        let input = if C::ENCODING { input } else { None };
        let ctx = if state.index == 0 { 4 } else { 5 };
        let given = input.map_or(0, |k| k.on.len() as u64 - 1);
        let n = check_limit(self.counts.code(c, ctx, given) + 1, MAX_POINTS)? as usize;
        // The given points, or the decoded ones.
        let (mut on, mut x, mut y) = match input {
            Some(k) => (k.on.to_vec(), k.x.to_vec(), k.y.to_vec()),
            None => (vec![0u8; n], vec![0i32; n], vec![0i32; n]),
        };
        let mut px = state.px;
        let mut py = state.py;
        let mut pdx = 0i64;
        let mut pdy = 0i64;
        let mut prev_on = 1usize;
        let mut prev_prev_on = 1usize;
        self.local.reset(n);
        let mut hits = 0usize;
        let mut prev_symmetric = 0usize;
        let mut contour_hashes = Vec::with_capacity(n);
        for i in 0..n {
            let start = usize::from(i == 0);
            // The prediction of the LZP, when it missed.
            let mut missed = None;

            // The hash of the context of each order (the previous points).
            let max_order = MAX_ORDER.min(i.saturating_sub(1));
            let hashes = context_hashes(&on, &x, &y, i);
            contour_hashes.push(hashes);
            // Prediction from the longest previous occurrence of the context.
            let mut order = max_order;
            let mut history_pos = -1i64;
            let mut local_prediction = None;
            while order >= 1 {
                let hash = hashes[order - 1];
                local_prediction = self.local.get(order, hash);
                if local_prediction.is_some() {
                    break;
                }
                history_pos = self.history.lookup(hash);
                if history_pos >= 0 {
                    break;
                }
                order -= 1;
            }
            for k in 1..=max_order {
                self.local.insert(k, hashes[k - 1], i);
            }
            if order >= 1 {
                let (p_on, pdxp, pdyp) = match local_prediction {
                    Some(lp) => (
                        on[lp],
                        x[lp].wrapping_sub(x[lp - 1]) as i64,
                        y[lp].wrapping_sub(y[lp - 1]) as i64,
                    ),
                    None => {
                        let hp = history_pos as usize;
                        (
                            self.history.on[hp],
                            self.history.dx[hp] as i64,
                            self.history.dy[hp] as i64,
                        )
                    }
                };
                let given = C::ENCODING
                    && on[i] == p_on
                    && x[i] as i64 - px == pdxp
                    && y[i] as i64 - py == pdyp;
                let hit = c.bit(
                    &mut self.prediction_hit,
                    ((order - 1) * 8 + hits.min(7)) * 2 + prev_on,
                    u32::from(given),
                );
                if hit != 0 {
                    hits += 1;
                    on[i] = p_on;
                    px = px.wrapping_add(pdxp);
                    py = py.wrapping_add(pdyp);
                    x[i] = px as i32;
                    y[i] = py as i32;
                    pdx = pdxp;
                    pdy = pdyp;
                    prev_prev_on = prev_on;
                    prev_on = p_on as usize;
                    continue;
                }
                hits = 0;
                missed = Some((p_on, pdxp, pdyp));
            }

            // Else, the last deltas may be a symmetric copy of earlier ones
            // (mirrored or rotated), in the contour or in the history.
            if let Some((kind, p_on, sdx, sdy)) = self
                .symmetric_prediction(&on, &x, &y, i)
                .filter(|&(_, p_on, sdx, sdy)| missed != Some((p_on, sdx, sdy)))
            {
                let given = C::ENCODING
                    && on[i] == p_on
                    && x[i] as i64 - px == sdx
                    && y[i] as i64 - py == sdy;
                let hit = c.bit(
                    &mut self.symmetry_hit,
                    (kind * 2 + usize::from(missed.is_some())) * 2 + prev_symmetric,
                    u32::from(given),
                );
                prev_symmetric = hit as usize;
                if hit != 0 {
                    on[i] = p_on;
                    px = px.wrapping_add(sdx);
                    py = py.wrapping_add(sdy);
                    x[i] = px as i32;
                    y[i] = py as i32;
                    pdx = sdx;
                    pdy = sdy;
                    prev_prev_on = prev_on;
                    prev_on = p_on as usize;
                    continue;
                }
            }

            let is_on = c.bit(
                &mut self.on_curve,
                start * 4 + prev_on * 2 + prev_prev_on,
                on[i] as u32,
            ) as usize;
            on[i] = is_on as u8;
            // Position in the contour (start, second point, others) and
            // on/off-curve state of the last three points.
            let position = i.min(2);
            let point_state = position * 8 + prev_prev_on * 4 + is_on * 2 + prev_on;
            // Predict an off-curve point's delta from the previous delta
            // when either of the preceding two points is off-curve.
            let predicted = position == 2 && is_on == 0 && !(prev_prev_on != 0 && prev_on != 0);
            let ex = if predicted { pdx } else { 0 };
            let ey = if predicted { pdy } else { 0 };
            let rx = x[i] as i64 - px - ex;
            let ry = y[i] as i64 - py - ey;
            let zx = c.bit(
                &mut self.zero_x,
                (point_state * 2 + usize::from(pdx == 0)) * 2 + usize::from(pdy == 0),
                u32::from(C::ENCODING && rx == 0),
            ) as usize;
            let zy = c.bit(
                &mut self.zero_y,
                (point_state * 2 + zx) * 2 + usize::from(pdy == 0),
                u32::from(C::ENCODING && ry == 0),
            );
            let mut dx = ex;
            let mut dy = ey;
            if zx == 0 {
                let negative = c.bit(
                    &mut self.sign_x,
                    (point_state * 3 + sign3(pdx)) * 3 + sign3(pdy),
                    u32::from(rx < 0),
                ) != 0;
                let given = if C::ENCODING {
                    rx.unsigned_abs() - 1
                } else {
                    0
                };
                let m = self
                    .mag_x
                    .code(c, point_state * 6 + magnitude_class(pdx), given)
                    as i64
                    + 1;
                dx += if negative { -m } else { m };
            }
            if zy == 0 {
                let negative = c.bit(
                    &mut self.sign_y,
                    (point_state * 3 + sign3(pdy)) * 3 + sign3(dx),
                    u32::from(ry < 0),
                ) != 0;
                let given = if C::ENCODING {
                    ry.unsigned_abs() - 1
                } else {
                    0
                };
                let m = self
                    .mag_y
                    .code(c, point_state * 6 + magnitude_class(pdy), given)
                    as i64
                    + 1;
                dy += if negative { -m } else { m };
            }
            px = px.wrapping_add(dx);
            py = py.wrapping_add(dy);
            x[i] = px as i32;
            y[i] = py as i32;
            // The start delta isn't part of the contour.
            pdx = if start != 0 { 0 } else { dx };
            pdy = if start != 0 { 0 } else { dy };
            prev_prev_on = prev_on;
            prev_on = is_on;
        }
        Ok((on, x, y, contour_hashes))
    }

    /// A prediction of the point i of a new contour (its kind, on-curve state
    /// and delta) from a symmetric copy (see `SYMMETRIES`) of the context of
    /// its last deltas, in the contour (`local`) or in the history: its kind
    /// is (index of the symmetry, in the history).
    fn symmetric_prediction(
        &self,
        on: &[u8],
        x: &[i32],
        y: &[i32],
        i: usize,
    ) -> Option<(usize, u8, i64, i64)> {
        let d = |k: usize| (x[k].wrapping_sub(x[k - 1]), y[k].wrapping_sub(y[k - 1]));
        let order = SYMMETRY_ORDER;
        if i < order + 1 || d(i - 1) == (0, 0) {
            return None;
        }
        let history = &self.history;
        for (s, &(fx, fy, reversed)) in SYMMETRIES.iter().enumerate() {
            let kind = s * 2;
            let f = |(dx, dy): (i32, i32)| (dx.wrapping_mul(fx), dy.wrapping_mul(fy));
            // The context at a position p of a copy: the deltas p - order to
            // p - 1, the on-curve states of their ends and of the point
            // p - order - 1. A mirror is in the reverse order: its delta p - t
            // is the copy of the delta i - (order + 1 - t).
            let hash = if reversed {
                hash_items(
                    order,
                    (1..=order).rev().map(|t| {
                        let (a, b) = f(d(i - t));
                        (a, b, on[i - t - 1])
                    }),
                    on[i - 1],
                )
            } else {
                hash_items(
                    order,
                    (1..=order).map(|t| {
                        let (a, b) = f(d(i - t));
                        (a, b, on[i - t])
                    }),
                    on[i - order - 1],
                )
            };
            // The prediction: the delta p - order - 1 (mirrors), whose start
            // is the copy of the point i, or the delta p (rotation).
            let back = order + 1;
            if let Some(p) = self.local.get(order, hash) {
                if reversed && p > back {
                    let (dx, dy) = f(d(p - back));
                    return Some((kind, on[p - back - 1], dx as i64, dy as i64));
                }
                if !reversed && p < i {
                    let (dx, dy) = f(d(p));
                    return Some((kind, on[p], dx as i64, dy as i64));
                }
            }
            let p = history.lookup(hash);
            if p >= 0 {
                let p = p as usize;
                if reversed && history.index[p] as usize > back {
                    let (dx, dy) = f((history.dx[p - back], history.dy[p - back]));
                    return Some((kind + 1, history.on[p - back - 1], dx as i64, dy as i64));
                }
                if !reversed {
                    let (dx, dy) = f((history.dx[p], history.dy[p]));
                    return Some((kind + 1, history.on[p], dx as i64, dy as i64));
                }
            }
        }
        None
    }

    fn code_components<C: Coder>(
        &mut self,
        c: &mut C,
        input: Option<&[Component]>,
    ) -> Result<Vec<Component>, Error> {
        let input = if C::ENCODING { input } else { None };
        let given = input.map_or(0, |l| l.len().saturating_sub(1) as u64);
        let count = check_limit(self.counts.code(c, 7, given) + 1, MAX_CONTOURS)? as usize;
        let mut components = Vec::with_capacity(count);
        let mut prev_gid = self.last_base_gid;
        for k in 0..count {
            let given = input.map(|l| &l[k]);
            let gid = prev_gid
                + self.component_gid.code(
                    c,
                    usize::from(k > 0),
                    given.map_or(0, |g| g.gid - prev_gid),
                );
            if k == 0 {
                self.last_base_gid = gid;
            }
            prev_gid = gid;
            let t = given.map_or(0, |g| g.transform);
            let hi = c.bit(&mut self.transform, 0, t >> 1);
            let transform = hi * 2 + c.bit(&mut self.transform, 1 + hi as usize, t & 1);
            let ctx = if k > 0 { 2 } else { 0 };
            let dx = self.offset.code(c, ctx, given.map_or(0, |g| g.dx));
            let ctx = if k > 0 { 3 } else { 1 };
            let dy = self.offset.code(c, ctx, given.map_or(0, |g| g.dy));
            let num_values = match transform {
                1 => 1,
                2 => 2,
                3 => 4,
                _ => 0,
            };
            let values = (0..num_values)
                .map(|j| {
                    let value = given.map_or(0, |g| g.values[j]);
                    self.transform_value.code(c, 0, value)
                })
                .collect();
            components.push(Component {
                gid,
                transform,
                dx,
                dy,
                values,
            });
        }
        Ok(components)
    }
}

/// Decodes a payload coded with the model codec (without the revision byte).
pub fn decode(bytes: &[u8]) -> Result<Payload, Error> {
    decode_glyphs(bytes, usize::MAX).map(|(payload, _)| payload)
}

/// Decodes a payload coded with the model codec (without the revision byte),
/// with only its first `limit` glyphs: returns it with its number of glyphs.
pub fn decode_glyphs(bytes: &[u8], limit: usize) -> Result<(Payload, usize), Error> {
    let mut decoder = RangeDecoder::new(bytes);
    let mut model = Model::with_glyph_models(limit > 0);
    let payload = model.code(&mut decoder, &mut NoHooks, None, limit)?;
    Ok((payload, model.num_glyphs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_the_magnitude_classes() {
        for (class, &min) in MAGNITUDE_CLASS_MIN.iter().enumerate() {
            assert_eq!(magnitude_class(min), class);
            assert_eq!(magnitude_class(-min), class);
            if class > 0 {
                assert_eq!(magnitude_class(min - 1), class - 1);
            }
        }
    }

    #[test]
    fn rounds_half_up() {
        assert_eq!(round_half_up(2.5), 3.0);
        assert_eq!(round_half_up(-2.5), -2.0);
        assert_eq!(round_half_up(0.49999999999999994), 0.0);
        assert_eq!(round_half_up(-0.4), 0.0);
        assert_eq!(round_half_up(1.4999), 1.0);
    }

    #[test]
    fn floor_divisions() {
        assert_eq!(scale_delta(-5, 1), (-5 * 65 + 32_i64).div_euclid(64));
        assert_eq!(scale_delta(-100, -10), -84);
        assert_eq!(blend_delta(10, -10, 16), 0);
    }
}
