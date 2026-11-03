// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Each integration test compiles this module with the helpers it uses.
#![allow(dead_code)]

use hellbox::{
    generate_font, generate_standard_font, Family, GeneratedFont, Options, Request, StandardFont,
};
use skrifa::raw::tables::cmap::Cmap;
use std::collections::BTreeMap;

/// The selected cmap's mappings, excluding .notdef.
pub fn cmap(table: Cmap<'_>) -> BTreeMap<u32, u32> {
    let (_, _, subtable) = table.best_subtable().unwrap();
    subtable
        .iter()
        .map(|(code, gid)| (code, gid.to_u32()))
        .filter(|&(_, gid)| gid != 0)
        .collect()
}

#[derive(Clone, Copy)]
pub enum Source {
    Family(Family),
    Standard(StandardFont),
}

impl Source {
    pub fn generate(self, requests: &[Request], options: &Options) -> GeneratedFont {
        match self {
            Self::Family(family) => generate_font(family, requests, options),
            Self::Standard(font) => generate_standard_font(font, requests, options),
        }
    }
}

/// Distinct characters in their original order, including spaces.
pub fn characters(text: &str) -> Vec<char> {
    let mut chars = Vec::new();
    for c in text.chars() {
        if !chars.contains(&c) {
            chars.push(c);
        }
    }
    chars
}

pub fn text_requests(text: &str, width: impl Fn(usize) -> Option<f64>) -> Vec<Request> {
    characters(text)
        .into_iter()
        .enumerate()
        .map(|(i, c)| Request {
            unicode: vec![c as u32],
            width: width(i),
            ..Request::default()
        })
        .collect()
}
