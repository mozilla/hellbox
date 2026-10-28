// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The normalization tables of `data/unicode.bin` (see `crate::unicode`): the
//! forms of the codepoints which can be rendered with the glyph data files.

use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

/// The Latin ligatures (ff, fi, fl, ffi, ffl, long s t, st): a text with the
/// compatibility decomposition of one of them is rendered with its glyph (e.g.
/// "ffi" gives U+FB03).
const LIGATURES: std::ops::RangeInclusive<u32> = 0xfb00..=0xfb06;

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

fn varint(bytes: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        bytes.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    bytes.push(v as u8);
}

/// The tables of the codepoints whose forms are `available`: the NFC and NFKC
/// forms which are other single codepoints, the NFD and NFKD forms which have
/// several (NFKD being empty when it's NFD), and the ligatures (among these
/// decompositions). The codepoints are coded as differences with the previous
/// ones.
pub fn unicode_tables(available: &HashSet<u32>) -> UnicodeTables {
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
    let ligatures: Vec<u32> = LIGATURES
        .filter(|cp| decompositions.iter().any(|(c, _, _)| c == cp))
        .collect();
    let mut bytes = Vec::new();
    varint(&mut bytes, singletons.len() as u32);
    let mut prev = 0;
    for &(cp, nfc, nfkc) in &singletons {
        varint(&mut bytes, cp - prev);
        varint(&mut bytes, nfc);
        varint(&mut bytes, nfkc);
        prev = cp;
    }
    varint(&mut bytes, decompositions.len() as u32);
    prev = 0;
    for (cp, nfd, nfkd) in &decompositions {
        varint(&mut bytes, cp - prev);
        for form in [nfd, nfkd] {
            varint(&mut bytes, form.len() as u32);
            for &c in form {
                varint(&mut bytes, c);
            }
        }
        prev = *cp;
    }
    varint(&mut bytes, ligatures.len() as u32);
    prev = 0;
    for &cp in &ligatures {
        varint(&mut bytes, cp - prev);
        prev = cp;
    }
    UnicodeTables {
        bytes,
        singletons: singletons.len(),
        decompositions: decompositions.len(),
        ligatures: ligatures.len(),
    }
}
