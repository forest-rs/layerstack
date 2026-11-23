// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Read-only queries return named records so human and JSON reports use the same evidence.

use super::{is_help, json_string, stage::QueryError};
use std::{
    ffi::OsString,
    io::{self, Write},
};

pub(super) struct Arguments {
    pub positional: Vec<String>,
    pub options: Vec<(String, String)>,
}

pub(super) struct Record {
    pub kind: &'static str,
    pub fields: Vec<(&'static str, String)>,
}

pub(super) fn run(
    args: impl Iterator<Item = OsString>,
    out: &mut impl Write,
    help: &str,
    value_flags: &[&str],
    query: impl FnOnce(Arguments) -> Result<Vec<Record>, QueryError>,
) -> Result<(), String> {
    let args: Vec<_> = args.collect();
    let flags = || args.iter().take_while(|arg| *arg != "--");
    if flags().any(|arg| is_help(arg)) {
        return out
            .write_all(help.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| e.to_string());
    }
    let json = flags().any(|arg| arg == "--json");
    let result = parse(args, value_flags).and_then(query);
    let write = match &result {
        Ok(records) => report(out, records, json),
        Err(QueryError(code, message)) if json => (|| {
            out.write_all(b"{\"error\":{\"code\":")?;
            json_string(out, Some(code))?;
            out.write_all(b",\"message\":")?;
            json_string(out, Some(message))?;
            writeln!(out, "}}}}")
        })(),
        Err(_) => Ok(()),
    };
    write
        .and_then(|()| out.flush())
        .map_err(|e| e.to_string())?;
    result.map(|_| ()).map_err(|e| e.1)
}

fn parse(args: Vec<OsString>, value_flags: &[&str]) -> Result<Arguments, QueryError> {
    let mut parsed = Arguments {
        positional: vec![],
        options: vec![],
    };
    let mut args = args.into_iter();
    let mut options = true;
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|_| QueryError("usage", "expected UTF-8 arguments".into()))?;
        if options && arg == "--" {
            options = false;
        } else if options && arg == "--json" {
            continue;
        } else if options && value_flags.contains(&arg.as_str()) {
            let value = args
                .next()
                .and_then(|v| v.into_string().ok())
                .ok_or_else(|| QueryError("usage", format!("{arg} expects a value")))?;
            parsed.options.push((arg, value));
        } else if options && arg.starts_with('-') {
            return Err(QueryError("usage", format!("unknown option {arg}")));
        } else {
            parsed.positional.push(arg);
        }
    }
    Ok(parsed)
}

fn report(out: &mut impl Write, records: &[Record], json: bool) -> io::Result<()> {
    if json {
        out.write_all(b"{\"records\":[")?;
    }
    for (i, r) in records.iter().enumerate() {
        if json {
            if i > 0 {
                out.write_all(b",")?;
            }
            write!(out, "{{\"kind\":\"{}\"", r.kind)?;
            for (name, value) in &r.fields {
                write!(out, ",\"{name}\":")?;
                json_string(out, Some(value))?;
            }
            out.write_all(b"}")?;
        } else {
            write!(out, "{}", r.kind)?;
            for (name, value) in &r.fields {
                write!(out, "  {name}={value}")?;
            }
            writeln!(out)?;
        }
    }
    if json {
        writeln!(out, "]}}")?;
    }
    Ok(())
}
