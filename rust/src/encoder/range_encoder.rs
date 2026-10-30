// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Binary range encoder (the counterpart of `range_decoder.rs`), and the
//! coder of the encoder which only sums the costs of the decisions.

use crate::model::Coder;
use crate::range_decoder::{BitModels, TOP};
use std::sync::OnceLock;

pub(crate) struct RangeEncoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    bytes: Vec<u8>,
}

impl RangeEncoder {
    pub fn new() -> Self {
        RangeEncoder {
            low: 0,
            range: 0xffff_ffff,
            cache: 0,
            cache_size: 1,
            bytes: Vec::new(),
        }
    }

    fn shift_low(&mut self) {
        let high = u8::from(self.low >= 1 << 32);
        let low32 = self.low & 0xffff_ffff;
        if low32 < 0xff00_0000 || high != 0 {
            let mut temp = self.cache;
            loop {
                self.bytes.push(temp.wrapping_add(high));
                temp = 0xff;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (low32 >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (low32 & 0x00ff_ffff) << 8;
    }

    /// The coded data.
    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.bytes
    }
}

impl Coder for RangeEncoder {
    const ENCODING: bool = true;

    fn bit(&mut self, models: &mut BitModels, index: usize, bit: u32) -> u32 {
        let bound = (self.range >> 12) * models.probability(index);
        if bit != 0 {
            self.range = bound;
        } else {
            self.low += bound as u64;
            self.range -= bound;
        }
        models.update(index, bit);
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
        bit
    }

    fn direct(&mut self, count: u32, value: u32) -> u32 {
        self.range >>= count;
        self.low += value as u64 * self.range as u64;
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
        value
    }
}

/// -log2(p / 4096): the cost in bits of a decision of probability p / 4096.
fn costs() -> &'static [f64; 4097] {
    static COSTS: OnceLock<[f64; 4097]> = OnceLock::new();
    COSTS.get_or_init(|| std::array::from_fn(|p| -(p as f64 / 4096.0).log2()))
}

/// A coder which doesn't code but sums the costs, without adapting.
pub(crate) struct CostCoder {
    pub total: f64,
    costs: &'static [f64; 4097],
}

impl CostCoder {
    pub fn new() -> Self {
        CostCoder {
            total: 0.0,
            costs: costs(),
        }
    }
}

impl Coder for CostCoder {
    const ENCODING: bool = true;

    #[inline]
    fn bit(&mut self, models: &mut BitModels, index: usize, bit: u32) -> u32 {
        let p = models.probability(index) as usize;
        self.total += self.costs[if bit != 0 { p } else { 4096 - p }];
        bit
    }

    #[inline]
    fn direct(&mut self, count: u32, value: u32) -> u32 {
        self.total += count as f64;
        value
    }
}
