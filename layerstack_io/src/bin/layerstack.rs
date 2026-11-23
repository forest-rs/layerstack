// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Command line tools for USD layers and composed stages.

#[path = "layerstack/explain.rs"]
mod explain;

use layerstack_io::diff::{Change, LayerDiff, diff_layers};
use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

const HELP: &str = "\
Usage: layerstack COMMAND [OPTIONS]

  diff     Compare the authored opinions in two layers
  explain  Trace a composed property to its source layers

Run 'layerstack COMMAND --help' for arguments and examples.
";

const DIFF_HELP: &str = "\
Usage: layerstack diff [--json] BEFORE AFTER

Compare the authored opinions in two USDA or USDC layers.
Includes all variant branches. Only the two given files are read.
Formatting and comments outside authored fields are ignored.
USDZ and content the layer readers cannot represent are unsupported.

Exit status: 0 equal, 1 different, 2 error or incomplete comparison.
";

fn main() -> ExitCode {
    match run(env::args_os().skip(1)) {
        Ok(different) => ExitCode::from(u8::from(different)),
        Err(message) => {
            let _ = writeln!(io::stderr().lock(), "layerstack: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(mut args: impl Iterator<Item = OsString>) -> Result<bool, String> {
    let output_error = |error: io::Error| format!("writing output: {error}");
    let mut output = io::BufWriter::new(io::stdout().lock());
    let command = args.next();
    if command.as_deref().is_some_and(is_help) {
        output.write_all(HELP.as_bytes()).map_err(output_error)?;
        output.flush().map_err(output_error)?;
        return Ok(false);
    }
    if command.as_deref() == Some("explain".as_ref()) {
        return explain::run(args, &mut output).map(|()| false);
    }
    if command.as_deref() != Some("diff".as_ref()) {
        return Err(String::from(
            "expected 'diff' or 'explain'. See 'layerstack --help'",
        ));
    }
    let mut json = false;
    let mut options = true;
    let mut paths = Vec::with_capacity(2);
    for arg in args {
        if options && is_help(&arg) {
            output
                .write_all(DIFF_HELP.as_bytes())
                .map_err(output_error)?;
            output.flush().map_err(output_error)?;
            return Ok(false);
        }
        if options && arg == "--" {
            options = false;
        } else if options && arg == "--json" {
            json = true;
        } else if options && arg.as_encoded_bytes().starts_with(b"-") {
            return Err(format!("unknown option {arg:?}; see 'diff --help'"));
        } else {
            paths.push(PathBuf::from(arg));
        }
    }
    let [before, after] = paths.as_slice() else {
        return Err(String::from("expected BEFORE and AFTER; see 'diff --help'"));
    };
    let read = |path: &Path| {
        fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))
    };
    let first = read(before)?;
    let second = read(after)?;
    let diff = diff_layers(&first, &second).map_err(|error| {
        let path = match error.side {
            layerstack_io::diff::DiffSide::Before => before,
            layerstack_io::diff::DiffSide::After => after,
        };
        format!("{}: {error}", path.display())
    })?;
    if json {
        write_json(&mut output, &diff).map_err(output_error)?;
    } else {
        write_text(&mut output, &diff).map_err(output_error)?;
    }
    output.flush().map_err(output_error)?;
    Ok(!diff.changes.is_empty())
}

fn is_help(arg: &std::ffi::OsStr) -> bool {
    arg == "--help" || arg == "-h"
}

fn write_text(out: &mut impl Write, diff: &LayerDiff) -> io::Result<()> {
    if diff.changes.is_empty() {
        return writeln!(out, "No authored differences.");
    }
    for change in &diff.changes {
        write!(out, "{} {}", change.kind.as_str(), change.path)?;
        if let Some(field) = &change.field {
            write!(out, " {field}")?;
        }
        if let Some(time) = change.time {
            write!(out, " at {time}")?;
        }
        match &change.array {
            Some(array) if array.before_len == array.after_len => {
                write!(
                    out,
                    ": {} of {} elements changed",
                    array.changed_count, array.before_len
                )?;
            }
            Some(array) => {
                write!(
                    out,
                    ": {} → {} elements, {} differing positions",
                    array.before_len, array.after_len, array.changed_count
                )?;
            }
            None => match (&change.before, &change.after) {
                (Some(a), Some(b)) => write!(out, ": {a} → {b}")?,
                (Some(value), None) | (None, Some(value)) => write!(out, ": {value}")?,
                (None, None) => (),
            },
        }
        if let Some(array) = &change.array {
            write!(out, " at indices {:?}", array.indices)?;
            if array.changed_count > array.indices.len() {
                write!(out, " (first {})", array.indices.len())?;
            }
        }
        writeln!(out)?;
    }
    Ok(())
}

fn write_json(out: &mut impl Write, diff: &LayerDiff) -> io::Result<()> {
    write!(out, "{{\"equal\":{},\"changes\":[", diff.changes.is_empty())?;
    for (index, change) in diff.changes.iter().enumerate() {
        if index != 0 {
            out.write_all(b",")?;
        }
        write_change(out, change)?;
    }
    writeln!(out, "]}}")
}

fn write_change(out: &mut impl Write, change: &Change) -> io::Result<()> {
    out.write_all(b"{\"path\":")?;
    json_string(out, Some(&change.path))?;
    out.write_all(b",\"kind\":")?;
    json_string(out, Some(change.kind.as_str()))?;
    out.write_all(b",\"field\":")?;
    json_string(out, change.field.as_deref())?;
    if let Some(time) = change.time {
        write!(out, ",\"time\":{time}")?;
    }
    out.write_all(b",\"before\":")?;
    json_string(out, change.before.as_deref())?;
    out.write_all(b",\"after\":")?;
    json_string(out, change.after.as_deref())?;
    if let Some(array) = &change.array {
        write!(
            out,
            ",\"array\":{{\"before_len\":{},\"after_len\":{},\"changed_count\":{},\"indices\":[",
            array.before_len, array.after_len, array.changed_count
        )?;
        for (offset, index) in array.indices.iter().enumerate() {
            if offset != 0 {
                out.write_all(b",")?;
            }
            write!(out, "{index}")?;
        }
        out.write_all(b"]}")?;
    }
    out.write_all(b"}")
}

fn json_string(out: &mut impl Write, value: Option<&str>) -> io::Result<()> {
    let Some(value) = value else {
        return out.write_all(b"null");
    };
    out.write_all(b"\"")?;
    for c in value.chars() {
        match c {
            '"' => out.write_all(b"\\\"")?,
            '\\' => out.write_all(b"\\\\")?,
            '\0'..='\u{1f}' => write!(out, "\\u{:04x}", u32::from(c))?,
            _ => write!(out, "{c}")?,
        }
    }
    out.write_all(b"\"")
}
