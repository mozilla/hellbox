// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Binary range decoder with adaptive probabilities (see
//! `encoder/range_encoder.rs` for the encoder).
//!
//! The probability of a bit being 1 is the average of two estimates adapting
//! at different speeds, stored as 16 bits values; the coder uses 12 bits.
//! The direct bits are equiprobable, without model.

pub(crate) const TOP: u32 = 1 << 24;
const HALF: u16 = 32768;
const ONE: u32 = 65536;
const FAST_SHIFT: u32 = 4;
const SLOW_SHIFT: u32 = 8;

/// An adaptive probability: the two estimates, XOR `HALF` (so that the
/// initial state is zero, and the tables are allocated zeroed), and the
/// number of decisions, saturated at 255.
type Probability = (u16, u16, u8);

/// Adaptive probabilities, with both estimates and the decision count stored
/// together: a decision reads a single cache line (the contexts are hashed,
/// so the accesses are random).
pub struct BitModels {
    models: Vec<Probability>,
}

impl BitModels {
    pub fn new(size: usize) -> Self {
        BitModels {
            models: vec![(0, 0, 0); size],
        }
    }

    /// Probability (12 bits, never 0 or 4096) of a 1.
    #[inline]
    pub(crate) fn probability(&self, index: usize) -> u32 {
        let (fast, slow, _) = self.models[index];
        let p = ((fast ^ HALF) as u32 + (slow ^ HALF) as u32) >> 5;
        p.clamp(1, 4095)
    }

    #[inline]
    pub(crate) fn update(&mut self, index: usize, bit: u32) {
        let (fast, slow, count) = &mut self.models[index];
        let c = *count;
        // The adaptation is faster for the first decisions of a context: the
        // shift is floor(log2(count + 2)), capped at each model's limit.
        let shift = 31 - (c as u32 + 2).leading_zeros();
        let fast_shift = shift.min(FAST_SHIFT);
        let slow_shift = shift.min(SLOW_SHIFT);
        if c < 255 {
            *count = c + 1;
        }
        let f = (*fast ^ HALF) as u32;
        let s = (*slow ^ HALF) as u32;
        let (f, s) = if bit != 0 {
            (f + ((ONE - f) >> fast_shift), s + ((ONE - s) >> slow_shift))
        } else {
            (f - (f >> fast_shift), s - (s >> slow_shift))
        };
        *fast = f as u16 ^ HALF;
        *slow = s as u16 ^ HALF;
    }
}

pub struct RangeDecoder<'a> {
    bytes: &'a [u8],
    pos: usize,
    code: u32,
    range: u32,
    /// Set when the decoder needed more bytes than available: the data is
    /// truncated (the encoder flushes all the bytes the decoder needs).
    pub truncated: bool,
}

impl<'a> RangeDecoder<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        let mut decoder = RangeDecoder {
            bytes,
            pos: 0,
            code: 0,
            range: 0xffff_ffff,
            truncated: false,
        };
        // The leading byte is discarded by the 32-bit code register.
        for _ in 0..5 {
            decoder.code = (decoder.code << 8) | decoder.next();
        }
        decoder
    }

    #[inline]
    fn next(&mut self) -> u32 {
        match self.bytes.get(self.pos) {
            Some(&b) => {
                self.pos += 1;
                b as u32
            }
            None => {
                self.truncated = true;
                0
            }
        }
    }

    /// Decodes a bit with the probability `models[index]`, and updates it.
    #[inline]
    pub fn decode_bit(&mut self, models: &mut BitModels, index: usize) -> u32 {
        let bound = (self.range >> 12) * models.probability(index);
        let bit = if self.code < bound {
            self.range = bound;
            1
        } else {
            self.code -= bound;
            self.range -= bound;
            0
        };
        models.update(index, bit);
        while self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next();
        }
        bit
    }

    /// Decodes `count` (at most `MAX_DIRECT_BITS`) equiprobable bits, without
    /// model: the range is divided into 2^count equal parts.
    #[inline]
    pub fn decode_direct(&mut self, count: u32) -> u32 {
        self.range >>= count;
        let value = self.code / self.range;
        self.code -= value * self.range;
        while self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next();
        }
        // Only invalid data can give more bits.
        value & ((1 << count) - 1)
    }
}

/// The maximum number of bits of a direct decision: the range keeps at least
/// 8 bits.
pub(crate) const MAX_DIRECT_BITS: u32 = 16;
