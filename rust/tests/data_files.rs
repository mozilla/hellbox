// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Generate fonts using only their listed runtime data files.
//! Each case needs a fresh process because registered data persists.

#![cfg(not(feature = "embedded-data"))]

use hellbox::{
    data_file_names, font_data_files, generate_font, generate_standard_font, has_data,
    missing_glyphs, missing_standard_glyphs, set_data, standard_font_data_files,
    standard_glyph_data, Error, Family, Options, Request, StandardFont,
};
use std::process::Command;

const CASE: &str = "HELLBOX_DATA_CASE";

fn provide(names: &[&str]) {
    for name in names {
        let path = format!("{}/data/{name}", env!("CARGO_MANIFEST_DIR"));
        set_data(name, std::fs::read(path).unwrap()).unwrap();
    }
}

/// Primary, fallback and missing glyphs, including a sequence.
const TEXTS: [&str; 7] = ["A", "ffi", "é", "α", "∑", "★", "一"];

fn family_case(case: &str) -> Option<(Family, Options)> {
    let (family, style) = case.split_once(':')?;
    let family = match family {
        "sans" => Family::Sans,
        "serif" => Family::Serif,
        "mono" => Family::Mono,
        _ => return None,
    };
    let options = match style {
        "regular" => Options::default(),
        "bold" => Options {
            bold: true,
            ..Options::default()
        },
        "italic" => Options {
            italic: true,
            ..Options::default()
        },
        "black" => Options {
            weight: Some(900.0),
            ..Options::default()
        },
        "thick-italic" => Options {
            stem_v: Some(200.0),
            italic: true,
            ..Options::default()
        },
        _ => return None,
    };
    Some((family, options))
}

const STYLES: [&str; 5] = ["regular", "bold", "italic", "black", "thick-italic"];

/// Child process: exercise generation and missing-glyph lookup with only
/// the listed files.
#[test]
fn runs_a_case() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    let requests: Vec<Request> = TEXTS
        .iter()
        .map(|text| Request {
            unicode: text.chars().map(u32::from).collect(),
            ..Request::default()
        })
        .collect();
    let texts: Vec<&[u32]> = requests
        .iter()
        .map(|request| request.unicode.as_slice())
        .collect();
    if let Some(font) = StandardFont::from_name(&case) {
        provide(&standard_font_data_files(font));
        let generated = generate_standard_font(font, &requests, &Options::default());
        assert!(!generated.data.is_empty());
        missing_standard_glyphs(font, &texts);
        assert!(standard_glyph_data(font).codepoints().count() > 0);
    } else {
        let (family, options) = family_case(&case).unwrap();
        provide(&font_data_files(family, &options));
        let generated = generate_font(family, &requests, &options);
        assert!(!generated.data.is_empty());
        missing_glyphs(family, &texts);
    }
}

#[test]
fn provides_the_files_of_each_font() {
    let mut cases: Vec<String> = StandardFont::ALL
        .into_iter()
        .map(|font| font.name().to_string())
        .collect();
    for family in ["sans", "serif", "mono"] {
        cases.extend(STYLES.map(|style| format!("{family}:{style}")));
    }
    for case in cases {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runs_a_case", "--test-threads=1", "--quiet"])
            .env(CASE, &case)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn lists_the_files_of_the_fonts() {
    // The style, the fallbacks and the Unicode tables.
    assert_eq!(
        standard_font_data_files(StandardFont::HelveticaBold),
        ["Sans-Bold.hbgd", "NotoSymbols.hbgd", "unicode.bin"]
    );
    assert_eq!(
        standard_font_data_files(StandardFont::Symbol),
        [
            "Symbol.hbgd",
            "Serif-Regular.hbgd",
            "NotoSymbols.hbgd",
            "unicode.bin"
        ]
    );
    // The regular style for the missing glyphs, and both weights with an
    // expected stem.
    let italic = Options {
        italic: true,
        ..Options::default()
    };
    assert_eq!(
        font_data_files(Family::Serif, &italic),
        [
            "Serif-Italic.hbgd",
            "Serif-Regular.hbgd",
            "NotoSymbols.hbgd",
            "unicode.bin"
        ]
    );
    let stem = Options {
        stem_v: Some(200.0),
        ..italic
    };
    assert_eq!(
        font_data_files(Family::Serif, &stem),
        [
            "Serif-Italic.hbgd",
            "Serif-Regular.hbgd",
            "NotoSymbols.hbgd",
            "Serif-BoldItalic.hbgd",
            "unicode.bin"
        ]
    );
    let names: Vec<&str> = data_file_names().collect();
    assert_eq!(names.len(), 13);
    for font in StandardFont::ALL {
        assert!(standard_font_data_files(font)
            .iter()
            .all(|name| names.contains(name)));
    }
}

#[test]
fn checks_the_provided_files() {
    assert_eq!(
        set_data("Arial.hbgd", Vec::new()),
        Err(Error::UnknownDataFile)
    );
    assert_eq!(
        set_data("Mono-Bold.hbgd", b"HBGD".to_vec()),
        Err(Error::InvalidData)
    );
    assert!(!has_data("Mono-Bold.hbgd"));
    provide(&["Mono-Bold.hbgd"]);
    assert!(has_data("Mono-Bold.hbgd"));
    // Provided again: the first one is kept.
    assert_eq!(set_data("Mono-Bold.hbgd", Vec::new()), Ok(()));
}
