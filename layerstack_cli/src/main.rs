// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Command line tools for USD layers and composed stages.

mod check;
mod deps;
mod explain;
mod inspect;
mod material;
mod query;
mod stage;

use layerstack_io::diff::{
    Change, DiffSide, DiffStats, LayerDiff, diff_document_stats, diff_documents, documents_differ,
    read_authored_document,
};
use layerstack_usda::writer::Document;
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

  check    Report missing assets, composition errors and invalid mesh topology
  deps     List authored asset uses and the layers that contain them
  diff     Compare two authored layers or composed stages
  explain  Trace a composed property to its source layers
  inspect  Browse prims and their authored properties
  material Trace the winning material binding and its surface shader

Run 'layerstack COMMAND --help' for arguments and examples.
";

const DIFF_HELP: &str = "\
Usage: layerstack diff [--composed | --jobs N] [--quiet | --stat] [--json] BEFORE AFTER

Compare the authored opinions in two USDA or USDC layers.
Includes all variant branches. Only the two given files are read.
Formatting and comments outside authored fields are ignored.
Authored comparison accepts loose USDA and USDC files.

--jobs 1 limits authored comparisons to one reader to reduce peak memory.
The default, --jobs 2, can read large authored layers concurrently.
--quiet validates both inputs and returns the exit status without a report.
--stat shows counts of added, removed and changed records and distinct paths.
--composed opens both stages and compares their losslessly flattened contents.
Reads dependencies, selects active variants and reports expanded instance paths.
Use 'layerstack explain STAGE /Prim.property' to trace a changed property.

Exit status: 0 equal, 1 different, 2 error or incomplete comparison.
";

fn main() -> ExitCode {
    match run(env::args_os().skip(1)) {
        Ok(status) => ExitCode::from(status),
        Err(message) => {
            let _ = writeln!(io::stderr().lock(), "layerstack: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(mut args: impl Iterator<Item = OsString>) -> Result<u8, String> {
    let output_error = |error: io::Error| format!("writing output: {error}");
    let mut output = io::BufWriter::new(io::stdout().lock());
    let command = args.next();
    if command.as_deref().is_some_and(is_help) {
        output.write_all(HELP.as_bytes()).map_err(output_error)?;
        output.flush().map_err(output_error)?;
        return Ok(0);
    }
    if command.as_deref() == Some("explain".as_ref()) {
        return explain::run(args, &mut output).map(|()| 0);
    }
    if command.as_deref() == Some("inspect".as_ref()) {
        return query::run(
            args,
            &mut output,
            inspect::HELP,
            &["--limit"],
            inspect::query,
        )
        .map(|()| 0);
    }
    if command.as_deref() == Some("check".as_ref()) {
        return query::run_with_status(
            args,
            &mut output,
            check::HELP,
            &[],
            check::query,
            check::status,
        );
    }
    if command.as_deref() == Some("deps".as_ref()) {
        return query::run(args, &mut output, deps::HELP, &[], deps::query).map(|()| 0);
    }
    if command.as_deref() == Some("material".as_ref()) {
        return query::run(
            args,
            &mut output,
            material::HELP,
            &["--purpose", "--context"],
            material::query,
        )
        .map(|()| 0);
    }
    if command.as_deref() != Some("diff".as_ref()) {
        return Err(String::from("expected a command. See 'layerstack --help'"));
    }
    let mut json = false;
    let mut composed = false;
    let mut stat = false;
    let mut quiet = false;
    let mut jobs = None;
    let mut options = true;
    let mut paths = Vec::with_capacity(2);
    while let Some(arg) = args.next() {
        if options && is_help(&arg) {
            output
                .write_all(DIFF_HELP.as_bytes())
                .map_err(output_error)?;
            output.flush().map_err(output_error)?;
            return Ok(0);
        }
        if options && arg == "--" {
            options = false;
        } else if options && arg == "--jobs" {
            jobs = Some(
                match args.next().as_deref().and_then(|value| value.to_str()) {
                    Some("1") => 1,
                    Some("2") => 2,
                    _ => return Err(String::from("--jobs expects 1 or 2")),
                },
            );
        } else if options && arg == "--quiet" {
            quiet = true;
        } else if options && arg == "--stat" {
            stat = true;
        } else if options && arg == "--composed" {
            composed = true;
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
    if composed && jobs.is_some() {
        return Err(String::from("--jobs applies only to authored comparisons"));
    }
    if quiet && (stat || json) {
        return Err(String::from(
            "--quiet cannot be combined with --stat or --json",
        ));
    }
    let compare = |before: &Document, after: &Document| {
        if quiet {
            Report::Quiet(documents_differ(before, after))
        } else if stat {
            Report::Stat(diff_document_stats(before, after))
        } else {
            Report::Full(diff_documents(before, after))
        }
    };
    let report = if composed {
        match stage::diff(before, after, compare) {
            Ok(diff) => diff,
            Err((side, error)) => {
                let path = match side {
                    DiffSide::Before => before,
                    DiffSide::After => after,
                };
                if json {
                    write_stage_error(&mut output, side, path, &error).map_err(output_error)?;
                    output.flush().map_err(output_error)?;
                }
                return Err(format!("{side} stage {}: {}", path.display(), error.1));
            }
        }
    } else {
        authored_diff(before, after, jobs.unwrap_or(2), compare)?
    };
    let different = match report {
        Report::Quiet(different) => different,
        Report::Stat(stats) => {
            write_stat(&mut output, &stats, composed, json).map_err(output_error)?;
            stats.paths != 0
        }
        Report::Full(diff) => {
            if json {
                write_json(&mut output, &diff, composed).map_err(output_error)?;
            } else {
                write_text(&mut output, &diff, composed).map_err(output_error)?;
            }
            !diff.changes.is_empty()
        }
    };
    output.flush().map_err(output_error)?;
    Ok(u8::from(different))
}

enum Report {
    Full(LayerDiff),
    Stat(DiffStats),
    Quiet(bool),
}

fn authored_diff<R>(
    before: &Path,
    after: &Path,
    jobs: u8,
    compare: impl FnOnce(&Document, &Document) -> R,
) -> Result<R, String> {
    let read = |path: &Path| {
        fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))
    };
    let first = read(before)?;
    let second = read(after)?;
    let left = || {
        read_authored_document(&first)
            .map_err(|e| format!("{}: before layer: {e}", before.display()))
    };
    let right = || {
        read_authored_document(&second)
            .map_err(|e| format!("{}: after layer: {e}", after.display()))
    };
    if first == second {
        let doc = left()?;
        return Ok(compare(&doc, &doc));
    }
    // Small layers cost less to decode than starting a second reader.
    const PARALLEL_MIN_BYTES: usize = 256 * 1024;
    if jobs == 2
        && first.len().min(second.len()) >= PARALLEL_MIN_BYTES
        && !cfg!(target_family = "wasm")
        && !std::thread::available_parallelism().is_ok_and(|cpus| cpus.get() == 1)
    {
        return std::thread::scope(|scope| {
            if let Ok(worker) = std::thread::Builder::new().spawn_scoped(scope, right) {
                let baseline = left();
                let revision = worker
                    .join()
                    .unwrap_or_else(|e| std::panic::resume_unwind(e));
                return Ok(compare(&baseline?, &revision?));
            }
            Ok(compare(&left()?, &right()?))
        });
    }
    Ok(compare(&left()?, &right()?))
}

fn is_help(arg: &std::ffi::OsStr) -> bool {
    arg == "--help" || arg == "-h"
}

fn write_stat(
    out: &mut impl Write,
    stats: &DiffStats,
    composed: bool,
    json: bool,
) -> io::Result<()> {
    let DiffStats {
        added,
        removed,
        changed,
        paths,
    } = *stats;
    let total = added + removed + changed;
    let mode = if composed { "composed" } else { "authored" };
    if json {
        writeln!(
            out,
            "{{\"mode\":\"{mode}\",\"equal\":{},\"stat\":{{\"records\":{total},\"paths\":{paths},\"added\":{added},\"removed\":{removed},\"changed\":{changed}}}}}",
            total == 0
        )
    } else {
        writeln!(
            out,
            "{total} {mode} diff records across {paths} paths ({added} added, {removed} removed, {changed} changed)"
        )
    }
}

fn write_text(out: &mut impl Write, diff: &LayerDiff, composed: bool) -> io::Result<()> {
    if diff.changes.is_empty() {
        let mode = if composed { "composed" } else { "authored" };
        return writeln!(out, "No {mode} differences.");
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

fn write_json(out: &mut impl Write, diff: &LayerDiff, composed: bool) -> io::Result<()> {
    out.write_all(b"{")?;
    if composed {
        out.write_all(b"\"mode\":\"composed\",")?;
    }
    write!(out, "\"equal\":{},\"changes\":[", diff.changes.is_empty())?;
    for (index, change) in diff.changes.iter().enumerate() {
        if index != 0 {
            out.write_all(b",")?;
        }
        write_change(out, change)?;
    }
    writeln!(out, "]}}")
}

fn write_stage_error(
    out: &mut impl Write,
    side: DiffSide,
    path: &Path,
    error: &stage::QueryError,
) -> io::Result<()> {
    write!(
        out,
        "{{\"mode\":\"composed\",\"error\":{{\"side\":\"{side}\",\"stage\":"
    )?;
    json_string(out, path.to_str())?;
    out.write_all(b",\"code\":")?;
    json_string(out, Some(error.0))?;
    out.write_all(b",\"message\":")?;
    json_string(out, Some(&error.1))?;
    writeln!(out, "}}}}")
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
