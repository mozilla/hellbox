// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Checks the encoder (the `encoder` feature) with the embedded files
//! (`data/`): it must give exactly the same ones.
//!
//! `cargo test --release --features encoder -- --include-ignored` also
//! builds them from the fonts of `sources/` (see `tools/fetch_sources.sh`).
#![cfg(feature = "encoder")]

use hellbox::encoder::{
    build_glyph_data, data_builds, decode_model_payload, encode_model_payload, unicode_tables,
    BuildOptions, Codec, EncoderOptions,
};
use hellbox::{Error, GlyphData};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

const FILES: [(&str, &[u8]); 12] = [
    (
        "Sans-Regular.hbgd",
        include_bytes!("../data/Sans-Regular.hbgd"),
    ),
    ("Sans-Bold.hbgd", include_bytes!("../data/Sans-Bold.hbgd")),
    (
        "Serif-Regular.hbgd",
        include_bytes!("../data/Serif-Regular.hbgd"),
    ),
    ("Serif-Bold.hbgd", include_bytes!("../data/Serif-Bold.hbgd")),
    (
        "Serif-Italic.hbgd",
        include_bytes!("../data/Serif-Italic.hbgd"),
    ),
    (
        "Serif-BoldItalic.hbgd",
        include_bytes!("../data/Serif-BoldItalic.hbgd"),
    ),
    (
        "Mono-Regular.hbgd",
        include_bytes!("../data/Mono-Regular.hbgd"),
    ),
    ("Mono-Bold.hbgd", include_bytes!("../data/Mono-Bold.hbgd")),
    (
        "NotoSymbols.hbgd",
        include_bytes!("../data/NotoSymbols.hbgd"),
    ),
    ("Symbol.hbgd", include_bytes!("../data/Symbol.hbgd")),
    (
        "ZapfDingbats.hbgd",
        include_bytes!("../data/ZapfDingbats.hbgd"),
    ),
    ("Wingdings.hbgd", include_bytes!("../data/Wingdings.hbgd")),
];

fn sources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../sources")
}

/// Runs a function on all the files, in parallel.
fn for_all_files(f: impl Fn(&str, &[u8]) + Sync) {
    std::thread::scope(|scope| {
        for (name, file) in FILES {
            let f = &f;
            scope.spawn(move || f(name, file));
        }
    });
}

/// The files have been built with the default settings: decoding them and
/// encoding them again must give the same bytes, whatever the number of
/// threads of the search.
#[test]
fn encodes_the_files_again() {
    for_all_files(|name, file| {
        let payload = decode_model_payload(&file[6..]).unwrap();
        for threads in [1, 4] {
            let options = EncoderOptions {
                threads,
                ..EncoderOptions::default()
            };
            let coded = encode_model_payload(&payload, options).unwrap();
            assert!(
                coded == file[6..],
                "{name}, {threads} threads: {} bytes instead of {}",
                coded.len(),
                file.len() - 6
            );
        }
    });
}

/// Round-trip with fewer candidates, then check truncation and one corrupted
/// sample.
#[test]
fn codes_the_payloads_exactly() {
    let payload = decode_model_payload(&FILES[1].1[6..]).unwrap();
    let options = EncoderOptions {
        candidates: 256,
        ..EncoderOptions::default()
    };
    let coded = encode_model_payload(&payload, options).unwrap();
    assert!(coded.len() < payload.len() * 2 / 3);
    assert!(decode_model_payload(&coded).unwrap() == payload);
    assert!(decode_model_payload(&coded[..coded.len() / 2]).is_err());
    let mut corrupted = coded.clone();
    for i in (100..corrupted.len()).step_by(97) {
        corrupted[i] ^= 0x5a;
    }
    assert!(decode_model_payload(&corrupted).ok() != Some(payload));
}

/// Disabling candidate cost evaluation still round-trips and produces the
/// same bytes with one and four threads.
#[test]
fn codes_without_exact_costs() {
    let payload = decode_model_payload(&FILES[0].1[6..]).unwrap();
    let coded = [1, 4].map(|threads| {
        let options = EncoderOptions {
            exact_costs: 0,
            threads,
            ..EncoderOptions::default()
        };
        encode_model_payload(&payload, options).unwrap()
    });
    assert!(coded[0] == coded[1]);
    assert!(decode_model_payload(&coded[0]).unwrap() == payload);
}

/// The uncompressed files (`--codec none`) give the same glyph data.
#[test]
fn reads_uncompressed_files() {
    for_all_files(|name, file| {
        let mut uncompressed = b"HBGD\x01\x00".to_vec();
        uncompressed.extend(decode_model_payload(&file[6..]).unwrap());
        let a = GlyphData::new(file).unwrap();
        let b = GlyphData::new(&uncompressed).unwrap();
        assert_eq!(a.num_glyphs(), b.num_glyphs(), "{name}");
        assert!(a.codepoints().eq(b.codepoints()), "{name}");
        for cp in a.codepoints() {
            assert_eq!(a.glyph_id(cp), b.glyph_id(cp), "{name}: U+{cp:04X}");
        }
        for gid in 0..a.num_glyphs() {
            let (oa, ob) = (a.outline(gid), b.outline(gid));
            assert!(
                oa == ob && a.advance(gid) == b.advance(gid),
                "{name}: glyph {gid}"
            );
        }
        assert!(GlyphData::new(&uncompressed[..uncompressed.len() - 10]).is_err());
    });
}

/// The normalization tables are the ones of the codepoints of the files.
#[test]
fn generates_the_unicode_tables() {
    let mut available = HashSet::new();
    for (_, file) in FILES {
        available.extend(GlyphData::new(file).unwrap().codepoints());
    }
    let tables = unicode_tables(&available);
    assert!(tables.bytes == include_bytes!("../data/unicode.bin"));
}

#[test]
#[ignore = "needs the fonts of sources/ (tools/fetch_sources.sh)"]
fn builds_the_files() {
    let builds = data_builds(&sources(), 1);
    std::thread::scope(|scope| {
        for (name, options) in &builds {
            scope.spawn(move || {
                let (data, _) = build_glyph_data(options).unwrap();
                let expected = FILES.iter().find(|(n, _)| n == name).unwrap().1;
                assert!(data == expected, "{name}");
            });
        }
    });
}

/// The outlines of the files built with a grid are within its tolerance (the
/// build checks it).
#[test]
#[ignore = "needs the fonts of sources/ (tools/fetch_sources.sh)"]
fn builds_with_a_grid() {
    let options = BuildOptions {
        grid: 2,
        ..BuildOptions::new(sources().join("Arimo-Regular.ttf"))
    };
    let (data, _) = build_glyph_data(&options).unwrap();
    assert!(data.len() < FILES[0].1.len());
}

/// The three codecs code the same payload (the library doesn't read the
/// deflate one, which needs a decompressor).
#[test]
#[ignore = "needs the fonts of sources/ (tools/fetch_sources.sh)"]
fn supports_the_three_codecs() {
    let build = |codec| {
        let mut options = BuildOptions::new(sources().join("Arimo-Regular.ttf"));
        options.font.ranges = Some("20-17F".to_string());
        options.codec = codec;
        build_glyph_data(&options).unwrap().0
    };
    let (none, deflate, model) = (
        build(Codec::None),
        build(Codec::Deflate),
        build(Codec::Model),
    );
    assert!(model.len() < deflate.len());
    assert_eq!(
        GlyphData::new(&deflate).err(),
        Some(Error::UnsupportedCompression)
    );
    let mut inflated = Vec::new();
    flate2::read::ZlibDecoder::new(&deflate[6..])
        .read_to_end(&mut inflated)
        .unwrap();
    assert!(inflated == none[6..]);
    assert!(decode_model_payload(&model[6..]).unwrap() == none[6..]);
    assert!(GlyphData::new(&none).unwrap().codepoints().count() > 300);
}
