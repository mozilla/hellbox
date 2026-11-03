// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Generates TrueType fonts with only the requested glyphs, from compressed
//! glyph data embedded in the library, to replace the fonts which aren't
//! embedded in PDF files: the standard fonts of PDF (see `StandardFont`), and
//! the other fonts of each family (see `Family`).
//!
//! The default `embedded-data` feature embeds `data/`. Without it, call
//! `set_data` for the files listed by `standard_font_data_files` or
//! `font_data_files` before generating a font.
//!
//! Each glyph is horizontally scaled to the width given by the PDF, its stems
//! getting the scale of the font (see `font_builder`). The glyphs
//! come from the embedded style of the family closest to the requested one
//! (regular or bold, and italic for the serif family), the rest of the style
//! being synthesized (emboldening or thinning, and slanting the outlines).
//! With the `encoder` feature, the crate also builds the glyph data files from
//! TrueType fonts (see `encoder`).
//!
//! ```
//! use hellbox::{generate_font, Family, Options, Request};
//!
//! # #[cfg(not(feature = "embedded-data"))]
//! # for name in hellbox::data_file_names() {
//! #     let path = format!("{}/data/{name}", env!("CARGO_MANIFEST_DIR"));
//! #     hellbox::set_data(name, std::fs::read(path).unwrap()).unwrap();
//! # }
//! let glyphs = [
//!     Request { unicode: vec!['A' as u32], width: Some(667.0), char_code: None },
//!     Request { unicode: "ffi".chars().map(|c| c as u32).collect(), width: None, char_code: Some(0x86) },
//! ];
//! let options = Options { bold: true, ..Options::default() };
//! let font = generate_font(Family::Sans, &glyphs, &options);
//! assert!(font.missing.is_empty());
//! assert_eq!(&font.data[..4], &[0, 1, 0, 0]);
//! ```

#[cfg(feature = "encoder")]
pub mod encoder;
mod font_builder;
mod glyph_data;
mod model;
mod outline;
#[cfg(feature = "encoder")]
mod payload;
mod range_decoder;
mod style;
mod unicode;
#[cfg(target_arch = "wasm32")]
mod wasm;

use std::fmt;
use std::sync::OnceLock;

use font_builder::clamp;
pub use font_builder::{Kerning, Options, Request};
pub use glyph_data::{GlyphData, Names, Outline};
use style::{Target, BOLD_THRESHOLD, BOLD_WEIGHT, DEFAULT_ITALIC_ANGLE, DEFAULT_WEIGHT};

/// Error while reading glyph data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidData,
    UnsupportedCompression,
    /// Not the name of a data file (see `data_file_names`).
    UnknownDataFile,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::InvalidData => write!(f, "invalid glyph data"),
            Error::UnsupportedCompression => write!(f, "unsupported glyph data compression"),
            Error::UnknownDataFile => write!(f, "unknown data file"),
        }
    }
}

impl std::error::Error for Error {}

/// The family of the generated font. Callers can select it from the PDF
/// `FontDescriptor` flags (`FixedPitch`, `Serif`). Glyph data comes from Arimo
/// (sans serif), Tinos (serif), and Liberation Mono (monospace): regular and
/// bold, plus italic and bold italic for the serif family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

/// Embedded or runtime-provided bytes of a file in `data/`.
struct DataFile {
    name: &'static str,
    #[cfg(feature = "embedded-data")]
    embedded: &'static [u8],
    #[cfg(not(feature = "embedded-data"))]
    provided: OnceLock<&'static [u8]>,
}

impl DataFile {
    #[cfg(feature = "embedded-data")]
    fn bytes(&self) -> Option<&'static [u8]> {
        Some(self.embedded)
    }

    #[cfg(not(feature = "embedded-data"))]
    fn bytes(&self) -> Option<&'static [u8]> {
        self.provided.get().copied()
    }

    fn needed(&self) -> &'static [u8] {
        self.bytes()
            .unwrap_or_else(|| panic!("The data file {} isn't provided.", self.name))
    }
}

/// A data file with cached glyph data.
struct GlyphDataFile {
    file: DataFile,
    decoded: OnceLock<GlyphData>,
}

impl GlyphDataFile {
    fn get(&'static self) -> &'static GlyphData {
        self.decoded.get_or_init(|| {
            GlyphData::lazy(self.file.needed()).expect("Invalid embedded glyph data")
        })
    }
}

macro_rules! data_file {
    ($name:literal) => {
        DataFile {
            name: $name,
            #[cfg(feature = "embedded-data")]
            embedded: include_bytes!(concat!("../data/", $name)),
            #[cfg(not(feature = "embedded-data"))]
            provided: OnceLock::new(),
        }
    };
}

macro_rules! glyph_data_file {
    ($name:literal) => {
        GlyphDataFile {
            file: data_file!($name),
            decoded: OnceLock::new(),
        }
    };
}

// The glyph data of the styles of the families (see `style_glyph_data`).
static FAMILIES: [GlyphDataFile; 8] = [
    glyph_data_file!("Sans-Regular.hbgd"),
    glyph_data_file!("Sans-Bold.hbgd"),
    glyph_data_file!("Serif-Regular.hbgd"),
    glyph_data_file!("Serif-Bold.hbgd"),
    glyph_data_file!("Serif-Italic.hbgd"),
    glyph_data_file!("Serif-BoldItalic.hbgd"),
    glyph_data_file!("Mono-Regular.hbgd"),
    glyph_data_file!("Mono-Bold.hbgd"),
];
static SYMBOLS: GlyphDataFile = glyph_data_file!("NotoSymbols.hbgd");
static SYMBOL: GlyphDataFile = glyph_data_file!("Symbol.hbgd");
static ZAPF_DINGBATS: GlyphDataFile = glyph_data_file!("ZapfDingbats.hbgd");
static WINGDINGS: GlyphDataFile = glyph_data_file!("Wingdings.hbgd");
static UNICODE: DataFile = data_file!("unicode.bin");
static UNICODE_TABLES: OnceLock<unicode::UnicodeTables> = OnceLock::new();

fn glyph_data_files() -> impl Iterator<Item = &'static GlyphDataFile> {
    FAMILIES
        .iter()
        .chain([&SYMBOLS, &SYMBOL, &ZAPF_DINGBATS, &WINGDINGS])
}

fn data_files() -> impl Iterator<Item = &'static DataFile> {
    glyph_data_files().map(|data| &data.file).chain([&UNICODE])
}

/// Names of all glyph data files and the Unicode tables in `data/`.
pub fn data_file_names() -> impl Iterator<Item = &'static str> {
    data_files().map(|file| file.name)
}

/// Whether a named file is embedded or has been provided with `set_data`.
pub fn has_data(name: &str) -> bool {
    data_files().any(|file| file.name == name && file.bytes().is_some())
}

/// Registers a file from `data_file_names` for the process lifetime.
/// Call before generating fonts that need it (see `standard_font_data_files`
/// and `font_data_files`). Repeated calls for a registered file are ignored.
#[cfg(not(feature = "embedded-data"))]
pub fn set_data(name: &str, bytes: Vec<u8>) -> Result<(), Error> {
    if name == UNICODE.name {
        if UNICODE.provided.get().is_none() {
            let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
            let tables = unicode::UnicodeTables::new(bytes)?;
            let _ = UNICODE_TABLES.set(tables);
            let _ = UNICODE.provided.set(bytes);
        }
        return Ok(());
    }
    let data = glyph_data_files()
        .find(|data| data.file.name == name)
        .ok_or(Error::UnknownDataFile)?;
    if data.file.provided.get().is_none() {
        let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let _ = data.decoded.set(GlyphData::lazy(bytes)?);
        let _ = data.file.provided.set(bytes);
    }
    Ok(())
}

// The italic angle of the oblique standard fonts, and the one of Tinos Italic.
const OBLIQUE_ANGLE: f64 = -12.0;
const SERIF_ITALIC_ANGLE: f64 = -16.33;

/// The 14 standard fonts of PDF, which can be used without being embedded,
/// and Wingdings, which often isn't.
///
/// Courier, Helvetica and Times use the monospace, sans-serif and serif
/// families (see `Family`). Courier and Helvetica oblique styles are slanted
/// at runtime. Symbol uses Tinos and Noto glyphs; ZapfDingbats uses Noto symbols.
/// Wingdings has the glyphs of Wingdings, Wingdings 2 and Wingdings 3, mostly
/// from Noto Sans Symbols 2, at their Unicode codepoints: the caller maps the
/// codes of these fonts to Unicode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StandardFont {
    Courier,
    CourierBold,
    CourierOblique,
    CourierBoldOblique,
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    HelveticaBoldOblique,
    TimesRoman,
    TimesBold,
    TimesItalic,
    TimesBoldItalic,
    Symbol,
    ZapfDingbats,
    Wingdings,
}

impl StandardFont {
    pub const ALL: [StandardFont; 15] = [
        StandardFont::Courier,
        StandardFont::CourierBold,
        StandardFont::CourierOblique,
        StandardFont::CourierBoldOblique,
        StandardFont::Helvetica,
        StandardFont::HelveticaBold,
        StandardFont::HelveticaOblique,
        StandardFont::HelveticaBoldOblique,
        StandardFont::TimesRoman,
        StandardFont::TimesBold,
        StandardFont::TimesItalic,
        StandardFont::TimesBoldItalic,
        StandardFont::Symbol,
        StandardFont::ZapfDingbats,
        StandardFont::Wingdings,
    ];

    /// Its PostScript name, e.g. "Helvetica-BoldOblique".
    pub fn name(self) -> &'static str {
        match self {
            StandardFont::Courier => "Courier",
            StandardFont::CourierBold => "Courier-Bold",
            StandardFont::CourierOblique => "Courier-Oblique",
            StandardFont::CourierBoldOblique => "Courier-BoldOblique",
            StandardFont::Helvetica => "Helvetica",
            StandardFont::HelveticaBold => "Helvetica-Bold",
            StandardFont::HelveticaOblique => "Helvetica-Oblique",
            StandardFont::HelveticaBoldOblique => "Helvetica-BoldOblique",
            StandardFont::TimesRoman => "Times-Roman",
            StandardFont::TimesBold => "Times-Bold",
            StandardFont::TimesItalic => "Times-Italic",
            StandardFont::TimesBoldItalic => "Times-BoldItalic",
            StandardFont::Symbol => "Symbol",
            StandardFont::ZapfDingbats => "ZapfDingbats",
            StandardFont::Wingdings => "Wingdings",
        }
    }

    /// The standard font with this PostScript name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|font| font.name() == name)
    }

    /// Its family (none for Symbol, ZapfDingbats and Wingdings), and its style.
    fn style(self) -> (Option<Family>, Target) {
        let style = |family, bold, italic_angle| {
            let weight = if bold { BOLD_WEIGHT } else { DEFAULT_WEIGHT };
            (
                Some(family),
                Target {
                    weight,
                    italic_angle,
                },
            )
        };
        let (mono, sans, serif) = (Family::Mono, Family::Sans, Family::Serif);
        match self {
            StandardFont::Courier => style(mono, false, 0.0),
            StandardFont::CourierBold => style(mono, true, 0.0),
            StandardFont::CourierOblique => style(mono, false, OBLIQUE_ANGLE),
            StandardFont::CourierBoldOblique => style(mono, true, OBLIQUE_ANGLE),
            StandardFont::Helvetica => style(sans, false, 0.0),
            StandardFont::HelveticaBold => style(sans, true, 0.0),
            StandardFont::HelveticaOblique => style(sans, false, OBLIQUE_ANGLE),
            StandardFont::HelveticaBoldOblique => style(sans, true, OBLIQUE_ANGLE),
            StandardFont::TimesRoman => style(serif, false, 0.0),
            StandardFont::TimesBold => style(serif, true, 0.0),
            StandardFont::TimesItalic => style(serif, false, SERIF_ITALIC_ANGLE),
            StandardFont::TimesBoldItalic => style(serif, true, SERIF_ITALIC_ANGLE),
            StandardFont::Symbol | StandardFont::ZapfDingbats | StandardFont::Wingdings => (
                None,
                Target {
                    weight: DEFAULT_WEIGHT,
                    italic_angle: 0.0,
                },
            ),
        }
    }
}

/// The glyph data of the regular style of a family.
pub fn glyph_data(family: Family) -> &'static GlyphData {
    style_glyph_data(family, false, false)
}

/// The glyph data of a style of a family: the sans-serif and monospace
/// families have no italic style (it's slanted from the upright one).
pub fn style_glyph_data(family: Family, bold: bool, italic: bool) -> &'static GlyphData {
    style_file(family, bold, italic).get()
}

fn style_file(family: Family, bold: bool, italic: bool) -> &'static GlyphDataFile {
    let index = match family {
        Family::Sans => usize::from(bold),
        Family::Serif => 2 + usize::from(bold) + 2 * usize::from(italic),
        Family::Mono => 6 + usize::from(bold),
    };
    &FAMILIES[index]
}

/// The glyph data with the symbols (arrows, math operators, dingbats...), used
/// as a fallback for all the fonts.
pub fn symbols_glyph_data() -> &'static GlyphData {
    SYMBOLS.get()
}

/// The glyph data of a standard font: the one of a style of its family, or
/// the one of Symbol, ZapfDingbats or Wingdings.
pub fn standard_glyph_data(font: StandardFont) -> &'static GlyphData {
    standard_file(font).0.get()
}

/// The glyph data file of a standard font, and its italic angle if it's
/// italic (see `base_file`).
fn standard_file(font: StandardFont) -> (&'static GlyphDataFile, Option<f64>) {
    match font.style() {
        (Some(family), target) => base_file(family, &target),
        _ if font == StandardFont::Symbol => (&SYMBOL, None),
        _ if font == StandardFont::ZapfDingbats => (&ZAPF_DINGBATS, None),
        _ => (&WINGDINGS, None),
    }
}

/// The style of a family closest to a style: its glyph data file, and its
/// italic angle if it's italic.
fn base_file(family: Family, target: &Target) -> (&'static GlyphDataFile, Option<f64>) {
    let bold = target.weight >= BOLD_THRESHOLD;
    let italic = family == Family::Serif && target.italic_angle < 0.0;
    (
        style_file(family, bold, italic),
        italic.then_some(SERIF_ITALIC_ANGLE),
    )
}

fn unicode_tables() -> &'static unicode::UnicodeTables {
    UNICODE_TABLES.get_or_init(|| {
        unicode::UnicodeTables::new(UNICODE.needed()).expect("Invalid embedded Unicode tables")
    })
}

/// Unique glyph data file names, followed by the Unicode tables.
fn data_files_of(files: impl IntoIterator<Item = &'static GlyphDataFile>) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for name in files
        .into_iter()
        .map(|data| data.file.name)
        .chain([UNICODE.name])
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The data files needed to generate a standard font, or to find its missing
/// glyphs (see `set_data`).
pub fn standard_font_data_files(font: StandardFont) -> Vec<&'static str> {
    data_files_of(std::iter::once(standard_file(font).0).chain(standard_fallbacks(font)))
}

/// The data files needed to generate a font of a family with some options,
/// or to find its missing glyphs (see `set_data`).
pub fn font_data_files(family: Family, options: &Options) -> Vec<&'static str> {
    // Resolve the style without loading data to measure stem weight.
    let options_without_stem = Options {
        stem_v: None,
        ..options.clone()
    };
    let target = target_of(family, &options_without_stem);
    // missing_glyphs uses the regular style.
    let mut files = vec![
        base_file(family, &target).0,
        style_file(family, false, false),
        &SYMBOLS,
    ];
    if options.stem_v.is_some_and(|stem_v| stem_v > 0.0) {
        let italic = family == Family::Serif && target.italic_angle < 0.0;
        files.extend([
            style_file(family, false, italic),
            style_file(family, true, italic),
        ]);
    }
    data_files_of(files)
}

/// A generated font.
pub struct GeneratedFont {
    /// The TrueType font. Retained request i has glyph id i + 1.
    pub data: Vec<u8>,
    /// Indices of unresolved requests. Also includes requests
    /// exceeding point/contour limits or after the first 65534.
    /// `debug_boxes` can draw boxes for missing requests with a nonzero advance.
    pub missing: Vec<usize>,
}

/// Adds fallback sources when canonical resolution from the primary source
/// is incomplete. Glyph outlines remain lazily decoded.
fn with_fallbacks(
    primary: &'static GlyphData,
    fallbacks: impl IntoIterator<Item = &'static GlyphDataFile>,
    texts: &[&[u32]],
) -> Vec<&'static GlyphData> {
    let mut sources = vec![primary];
    // Only with the canonical forms: the fallbacks may have the codepoints of
    // the compatibility forms (e.g. "①" for "1").
    let resolver = unicode::Resolver::canonical(&sources, unicode_tables());
    let complete = texts
        .iter()
        .all(|text| text.is_empty() || !resolver.resolve(text).is_empty());
    if !complete {
        sources.extend(fallbacks.into_iter().map(GlyphDataFile::get));
    }
    sources
}

/// The fallbacks of a standard font: the symbols, and the glyphs of a family
/// for Symbol, ZapfDingbats and Wingdings.
fn standard_fallbacks(font: StandardFont) -> impl Iterator<Item = &'static GlyphDataFile> {
    let family = match font {
        StandardFont::Symbol => Some(Family::Serif),
        StandardFont::ZapfDingbats | StandardFont::Wingdings => Some(Family::Sans),
        _ => None,
    };
    family
        .into_iter()
        .map(|family| style_file(family, false, false))
        .chain([&SYMBOLS])
}

fn texts_of(glyphs: &[Request]) -> Vec<&[u32]> {
    glyphs.iter().map(|glyph| &glyph.unicode[..]).collect()
}

/// The style requested by the options.
fn target_of(family: Family, options: &Options) -> Target {
    let mut weight = match options.weight {
        Some(weight) if weight > 0.0 => weight,
        _ if options.bold => BOLD_WEIGHT,
        _ => DEFAULT_WEIGHT,
    };
    // The weight of the expected stems, with the stems of the regular style.
    if let Some(stem_v) = options.stem_v.filter(|&stem_v| stem_v > 0.0) {
        let regular = glyph_data(family);
        let stem = (stem_v * regular.units_per_em as f64) / options.width_units();
        weight = style::weight_of_stem(regular, stem).unwrap_or(weight);
    }
    let mut italic_angle = (options.italic_angle)
        .filter(|angle| angle.is_finite())
        .unwrap_or(0.0);
    if italic_angle == 0.0 && options.italic {
        italic_angle = DEFAULT_ITALIC_ANGLE;
    }
    Target {
        weight: clamp(weight, 1.0, 1000.0),
        italic_angle,
    }
}

/// Generates a font of a family from the requested glyphs, taken from
/// the style of the family closest to the requested one (see `Family`), or
/// else from the symbols, the rest of the style being synthesized.
pub fn generate_font(family: Family, glyphs: &[Request], options: &Options) -> GeneratedFont {
    let target = target_of(family, options);
    let (base, base_italic_angle) = base_file(family, &target);
    let base = base.get();
    let sources = with_fallbacks(base, [&SYMBOLS], &texts_of(glyphs));
    let units_per_em = base.units_per_em as f64;
    let cap_height = (options.cap_height)
        .filter(|&cap_height| cap_height > 0.0)
        .map(|cap_height| (cap_height * units_per_em) / options.width_units());
    let mut style = style::font_style(&sources, base_italic_angle, &target, cap_height, true);
    // The expected stems are the ones of the font, whatever its scale.
    style.scaled_stems = !options.stem_v.is_some_and(|stem_v| stem_v > 0.0);
    let (data, missing) =
        font_builder::build_font(&sources, &style, unicode_tables(), glyphs, options);
    GeneratedFont { data, missing }
}

/// Generates a standard font from the requested glyphs: they're taken
/// from its glyph data (see `StandardFont`), or else from the symbols. Its
/// style is the one of the standard font: the style options (`bold`,
/// `weight`, `stem_v`, `italic`, `italic_angle` and `cap_height`) are ignored.
pub fn generate_standard_font(
    font: StandardFont,
    glyphs: &[Request],
    options: &Options,
) -> GeneratedFont {
    let (family, target) = font.style();
    let (base, base_italic_angle) = standard_file(font);
    let sources = with_fallbacks(base.get(), standard_fallbacks(font), &texts_of(glyphs));
    let style = style::font_style(&sources, base_italic_angle, &target, None, family.is_some());
    let (data, missing) =
        font_builder::build_font(&sources, &style, unicode_tables(), glyphs, options);
    GeneratedFont { data, missing }
}

/// The indices of the texts which can't be rendered with some glyph data and
/// its fallbacks.
fn missing_texts(
    primary: &'static GlyphData,
    fallbacks: impl IntoIterator<Item = &'static GlyphDataFile>,
    texts: &[&[u32]],
) -> Vec<usize> {
    let sources = with_fallbacks(primary, fallbacks, texts);
    let resolver = unicode::Resolver::new(&sources, unicode_tables());
    texts
        .iter()
        .enumerate()
        .filter(|(_, text)| resolver.resolve(text).is_empty())
        .map(|(i, _)| i)
        .collect()
}

/// Indices of the texts which can't be rendered with a standard font (and the
/// symbols).
pub fn missing_standard_glyphs(font: StandardFont, texts: &[&[u32]]) -> Vec<usize> {
    missing_texts(standard_glyph_data(font), standard_fallbacks(font), texts)
}

/// Indices of the texts which can't be rendered with a family (and the
/// symbols).
pub fn missing_glyphs(family: Family, texts: &[&[u32]]) -> Vec<usize> {
    missing_texts(glyph_data(family), [&SYMBOLS], texts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_data_without_panicking() {
        let bytes = include_bytes!("../data/NotoSymbols.hbgd");
        assert!(GlyphData::new(&bytes[..bytes.len() / 2]).is_err());
        assert!(GlyphData::new(&bytes[..10]).is_err());
        assert!(GlyphData::new(b"HBGD").is_err());
        let mut other = bytes.to_vec();
        other[5] = 1; // Deflate.
        assert_eq!(
            GlyphData::new(&other).err(),
            Some(Error::UnsupportedCompression)
        );
        // These mutations must not panic.
        for step in [97, 1009, 4099] {
            let mut corrupted = bytes.to_vec();
            for i in (100..corrupted.len()).step_by(step) {
                corrupted[i] ^= 0x5a;
            }
            let _ = GlyphData::new(&corrupted);
        }
    }

    #[cfg(feature = "embedded-data")]
    #[test]
    fn finds_the_missing_glyphs() {
        let a: &[u32] = &['A' as u32];
        let sum: &[u32] = &[0x2211];
        let pua: &[u32] = &[0xe000];
        let fi: &[u32] = &['f' as u32, 'i' as u32];
        assert_eq!(
            missing_glyphs(Family::Serif, &[a, sum, pua, fi, &[]]),
            vec![2, 4]
        );
    }
}
