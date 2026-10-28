// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Builds a glyph data file (see FORMAT.md) from TrueType fonts (see
//! `encoder::build_glyph_data` for the details).

use hellbox::encoder::{build_glyph_data, BuildOptions, Codec, FontSpec, Stems};
use std::process::ExitCode;

const USAGE: &str = "\
Usage: build_glyph_data --font <font.ttf> -o <output> [options]
  --ranges <ranges>             Codepoints of the font (e.g. 0-24F,370-3FF).
  --fallback <font.ttf[:ranges]> Another font (can be repeated).
  --reference <font.ttf>        The other weight of the family (bold for a regular
                                font): the stems give the emboldening strength.
  --grid <n>                    The coordinates are rounded to multiples of n.
  --serif, --fixed-pitch        Flags of the family.
  --family <name>
  --exclude-covered-by <font.ttf> Skip the codepoints covered by all of them.
  --stems latin|cjk             Glyphs measured to get the stems.
  --codec model|deflate|none
  --candidates <n>              Contours examined by the model encoder.
  --exact-costs <n>             Number of them whose exact cost is computed.
  --threads <n>                 Threads of the search (8 at most by default).
  --no-check                    Don't check that the outlines round-trip.";

fn run(args: Vec<String>) -> Result<(), String> {
    let mut options = BuildOptions::new("");
    let mut font = None;
    let mut output = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if arg.starts_with("--") => {
                (name.to_string(), Some(value.to_string()))
            }
            _ => (arg, None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("Missing value for {name}."))
        };
        let number = |v: String| {
            v.parse::<usize>()
                .map_err(|_| format!("Invalid number: {v}"))
        };
        match name.as_str() {
            "--font" => font = Some(value()?),
            "-o" | "--output" => output = Some(value()?),
            "--ranges" => options.font.ranges = Some(value()?),
            "--fallback" => options.fallback.push(FontSpec::parse(&value()?)),
            "--reference" => options.reference = Some(value()?.into()),
            "--grid" => {
                options.grid = value()?
                    .parse::<u8>()
                    .ok()
                    .filter(|&g| g >= 1)
                    .ok_or("--grid must be in [1, 255].")?
            }
            "--family" => options.family = Some(value()?),
            "--serif" => options.serif = true,
            "--fixed-pitch" => options.fixed_pitch = Some(true),
            "--exclude-covered-by" => options.exclude_covered_by.push(value()?.into()),
            "--stems" => {
                options.stems = match value()?.as_str() {
                    "latin" => Stems::Latin,
                    "cjk" => Stems::Cjk,
                    kind => return Err(format!("Unknown stems kind: {kind}")),
                }
            }
            "--codec" => {
                options.codec = match value()?.as_str() {
                    "model" => Codec::Model,
                    "deflate" => Codec::Deflate,
                    "none" => Codec::None,
                    other => return Err(format!("Unknown codec: {other}")),
                }
            }
            "--candidates" => options.encoder.candidates = number(value()?)?,
            "--exact-costs" => options.encoder.exact_costs = number(value()?)?,
            "--threads" => options.encoder.threads = number(value()?)?.max(1),
            "--no-check" => options.check = false,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("Unknown option: {other}\n{USAGE}")),
        }
    }
    let (Some(font), Some(output)) = (font, output) else {
        return Err(USAGE.to_string());
    };
    options.font.path = font.into();

    let (data, stats) = build_glyph_data(&options)?;
    let output = std::path::Path::new(&output);
    if let Some(dir) = output.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(output, data).map_err(|e| format!("{}: {e}", output.display()))?;
    println!("{}: {stats:#?}", output.display());
    Ok(())
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
