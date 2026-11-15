// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The normalization tables of `data/unicode.bin` (see `crate::unicode`): the
//! forms of the codepoints which can be rendered with the glyph data files.

use super::range_encoder::RangeEncoder;
use crate::unicode::{code_entries, Entries};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

/// The tables, and their numbers of entries.
pub struct UnicodeTables {
    pub bytes: Vec<u8>,
    pub singletons: usize,
    pub decompositions: usize,
    pub ligatures: usize,
}

/// The codepoints of a normalization form.
fn codepoints(form: impl Iterator<Item = char>) -> Vec<u32> {
    form.map(u32::from).collect()
}

/// The codepoint of a normalization form containing exactly one character.
fn single_codepoint(mut form: impl Iterator<Item = char>) -> Option<u32> {
    let cp = form.next()? as u32;
    form.next().is_none().then_some(cp)
}

/// The entries of the codepoints whose forms are `available`: the NFC and
/// NFKC forms which are other single codepoints, the NFD and NFKD forms which
/// have several (NFKD being empty when it's NFD).
fn entries(available: &HashSet<u32>) -> Entries {
    let mut singletons = Vec::new();
    let mut decompositions = Vec::new();
    for c in (0..=0x10ffff).filter_map(char::from_u32) {
        let cp = c as u32;
        let single = |form: Option<u32>| {
            form.filter(|&f| f != cp && available.contains(&f))
                .unwrap_or(0)
        };
        let multiple = |form: Vec<u32>| {
            if form.len() > 1 && form.iter().all(|f| available.contains(f)) {
                form
            } else {
                Vec::new()
            }
        };
        let one = || std::iter::once(c);
        let nfc = single(single_codepoint(one().nfc()));
        let nfkc = single(single_codepoint(one().nfkc()));
        if nfc != 0 || nfkc != 0 {
            singletons.push((cp, nfc, nfkc));
        }
        let nfd = multiple(codepoints(one().nfd()));
        let mut nfkd = multiple(codepoints(one().nfkd()));
        if nfkd == nfd {
            nfkd.clear();
        }
        if !nfd.is_empty() || !nfkd.is_empty() {
            decompositions.push((cp, nfd, nfkd));
        }
    }
    Entries {
        singletons,
        decompositions,
    }
}

/// The coded tables of the codepoints whose forms are `available` (see
/// `entries`), including the Latin ligatures among their decompositions.
pub fn unicode_tables(available: &HashSet<u32>) -> UnicodeTables {
    let entries = entries(available);
    let mut encoder = RangeEncoder::new();
    code_entries(&mut encoder, &entries).expect("Invalid Unicode tables");
    UnicodeTables {
        bytes: encoder.finish(),
        singletons: entries.singletons.len(),
        decompositions: entries.decompositions.len(),
        ligatures: entries.ligatures().count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_decoder::RangeDecoder;

    /// Latin, Greek and their marks: runs of singletons (e.g. U+1D2C-1D6A),
    /// NFC forms (e.g. U+0340), decompositions with both forms (e.g. U+0385).
    #[test]
    fn decodes_the_coded_entries() {
        let available = (0x20..0x250).chain(0x300..0x400).collect();
        let entries = entries(&available);
        let mut encoder = RangeEncoder::new();
        code_entries(&mut encoder, &entries).unwrap();
        let bytes = encoder.finish();
        let decoded = code_entries(&mut RangeDecoder::new(&bytes), &Entries::default()).unwrap();
        assert_eq!(decoded, entries);
        let truncated = &bytes[..bytes.len() - 10];
        assert!(code_entries(&mut RangeDecoder::new(truncated), &Entries::default()).is_err());
    }
}
