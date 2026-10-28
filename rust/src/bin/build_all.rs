// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Builds the embedded data (`data/`) from the fonts of `sources/` (see
//! `tools/fetch_sources.sh`): the glyph data files, their license and the
//! normalization tables of their codepoints (`unicode.bin`).
//!
//! Usage: `build_all [--out <dir>] [--threads <n>] [grid]`: the files are
//! built in parallel, so their searches have one thread by default.

use hellbox::encoder::{build_glyph_data, data_builds, unicode_tables};
use hellbox::GlyphData;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn run() -> Result<(), String> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = crate_dir.join("data");
    let mut grid = 1;
    let mut threads = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--out" {
            out = PathBuf::from(args.next().ok_or("Missing directory for --out.")?);
        } else if arg == "--threads" {
            let n = args.next().ok_or("Missing number for --threads.")?;
            threads = Some(
                n.parse::<usize>()
                    .map_err(|_| format!("Invalid number: {n}"))?,
            );
        } else {
            grid = arg.parse().map_err(|_| format!("Invalid grid: {arg}"))?;
        }
    }
    let sources = crate_dir.join("../sources");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut builds = data_builds(&sources, grid);
    for (_, options) in &mut builds {
        options.encoder.threads = threads.unwrap_or(1).max(1);
    }
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = builds
            .iter()
            .map(|(_, options)| scope.spawn(|| build_glyph_data(options)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut total = 0;
    let mut available = HashSet::new();
    // The copyright notices of the files of the families (they have a
    // reference font) and of the other ones.
    let (mut family_notices, mut other_notices) = (HashSet::new(), Vec::new());
    for ((name, options), result) in builds.iter().zip(results) {
        let (data, stats) = result.map_err(|e| format!("{name}: {e}"))?;
        std::fs::write(out.join(name), &data).map_err(|e| e.to_string())?;
        total += data.len();
        let glyph_data = GlyphData::new(&data).map_err(|e| format!("{name}: {e}"))?;
        available.extend(glyph_data.codepoints());
        for notice in glyph_data.names.copyright.lines() {
            if options.reference.is_some() {
                family_notices.insert(notice.to_string());
            } else if !other_notices.iter().any(|other| other == notice) {
                other_notices.push(notice.to_string());
            }
        }
        println!(
            "{name:<20} {:>7} bytes, {} codepoints, {} glyphs, {} points",
            data.len(),
            stats.codepoints,
            stats.glyphs,
            stats.points
        );
    }
    println!("{:<20} {total:>7} bytes", "Total");
    let licenses = [
        ("LICENSE_ARIMO", "LICENSE_ARIMO"),
        ("LICENSE_TINOS", "LICENSE_TINOS"),
        ("LICENSE_LIBERATION", "LICENSE_LIBERATION"),
    ];
    for (source, license) in licenses {
        std::fs::copy(sources.join(source), out.join(license)).map_err(|e| e.to_string())?;
    }
    // The OFL of the Noto fonts, with the notices of all of them (those of the
    // other files than the families'): OFL.txt only has the one of a project.
    let ofl = std::fs::read_to_string(sources.join("OFL.txt")).map_err(|e| e.to_string())?;
    let (_, terms) = ofl
        .split_once("\n\n")
        .ok_or("OFL.txt: no copyright notice")?;
    let noto_notices: Vec<&str> = (other_notices.iter())
        .filter(|notice| !family_notices.contains(*notice))
        .map(String::as_str)
        .collect();
    let license = format!("{}\n\n{terms}", noto_notices.join("\n"));
    std::fs::write(out.join("LICENSE_NOTO"), license).map_err(|e| e.to_string())?;
    let tables = unicode_tables(&available);
    std::fs::write(out.join("unicode.bin"), &tables.bytes).map_err(|e| e.to_string())?;
    println!(
        "unicode.bin: {} bytes, {} singletons, {} decompositions, {} ligatures",
        tables.bytes.len(),
        tables.singletons,
        tables.decompositions,
        tables.ligatures
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
