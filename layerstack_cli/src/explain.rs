// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{
    is_help, json_string,
    stage::{self, Document, QueryError},
};
use layerstack::{
    Contribution, IgnoreCause, InMemoryStore, OpinionRole, SpecPath, TargetPath,
    TargetsExplanation, Time, Value, ValueSource,
};
use std::{
    ffi::OsString,
    io::{self, Write},
};

const HELP: &str = "\
Usage: layerstack explain [--json] [--time FRAME] STAGE /Prim.property

Show the composed value, connections or relationship targets and their authored
opinions, strongest first. Uses active variants and authored declarations.
Schema fallback values require a registry and are outside this command's scope.
Arrays and dictionaries are summarized by size.

  layerstack explain scene.usda /World/Model.material:binding
  layerstack explain --time 12 scene.usdc /World/Model.visibility

Exit status: 0 explained, 2 invalid query, missing property or incomplete stage.
";

pub(super) fn run(
    args: impl Iterator<Item = OsString>,
    out: &mut impl Write,
) -> Result<(), String> {
    let args: Vec<_> = args.collect();
    let options = || args.iter().take_while(|arg| *arg != "--");
    if options().any(|arg| is_help(arg)) {
        return out
            .write_all(HELP.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| e.to_string());
    }
    let json = options().any(|arg| arg == "--json");
    let result = parse(args.into_iter()).and_then(|(stage, path, time)| {
        let sections = query(&stage, &path, time)?;
        Ok((stage, path, time, sections))
    });
    let written = match &result {
        Ok((stage, path, time, sections)) if json => write_json(out, stage, path, *time, sections),
        Ok((_, path, time, sections)) => write_text(out, path, *time, sections),
        Err(QueryError(code, message)) if json => (|| {
            out.write_all(b"{\"error\":{\"code\":")?;
            json_string(out, Some(code))?;
            out.write_all(b",\"message\":")?;
            json_string(out, Some(message))?;
            writeln!(out, "}}}}")
        })(),
        Err(_) => Ok(()),
    };
    written
        .and_then(|()| out.flush())
        .map_err(|e| e.to_string())?;
    result.map(|_| ()).map_err(|e| e.1)
}

fn parse(
    mut args: impl Iterator<Item = OsString>,
) -> Result<(String, String, Option<f64>), QueryError> {
    let mut time = None;
    let mut positional = Vec::new();
    let mut options = true;
    while let Some(arg) = args.next() {
        if options && arg == "--" {
            options = false;
        } else if options && arg == "--json" {
            continue;
        } else if options && arg == "--time" {
            let frame = args
                .next()
                .and_then(|v| v.to_str()?.parse::<f64>().ok())
                .filter(|v| v.is_finite())
                .ok_or_else(|| {
                    QueryError("invalid_time", "--time expects a finite stage time".into())
                })?;
            time = Some(frame);
        } else if options && arg.as_encoded_bytes().starts_with(b"-") {
            return Err(QueryError(
                "usage",
                format!("unknown option {arg:?}. See 'explain --help'"),
            ));
        } else {
            positional.push(arg.into_string().map_err(|_| {
                QueryError(
                    "usage",
                    "expected a UTF-8 stage identifier and property path".into(),
                )
            })?);
        }
    }
    let [stage, path] = positional.try_into().map_err(|_: Vec<String>| {
        QueryError(
            "usage",
            "expected STAGE and /Prim.property. See 'explain --help'".into(),
        )
    })?;
    Ok((stage, path, time))
}

struct Section {
    field: &'static str,
    source: &'static str,
    values: Vec<String>,
    opinions: Vec<Row>,
}

struct Row {
    layer: String,
    spec: String,
    role: &'static str,
    detail: Vec<String>,
    edits: Vec<(&'static str, Vec<String>)>,
}

fn site(
    doc: &Document,
    layer: layerstack::LayerId,
    spec: &SpecPath,
    role: &'static str,
    detail: Vec<String>,
) -> Row {
    Row {
        layer: doc.identifier(layer).unwrap_or("(anonymous)").into(),
        spec: spec.display(doc.store().paths(), doc.store().tokens()),
        role,
        detail,
        edits: Vec::new(),
    }
}

fn query(root: &str, path: &str, frame: Option<f64>) -> Result<Vec<Section>, QueryError> {
    let mut doc = stage::open(root)?;
    let incomplete = |msg: String| QueryError("incomplete_stage", format!("{root}: {msg}"));
    let prop = doc.store().try_property_path(path).map_err(|e| {
        QueryError(
            "invalid_path",
            format!("{path}: {e}. Use a composed path such as /World/Model.visibility"),
        )
    })?;
    if let Some(rel) = doc.view().relationship(prop) {
        if frame.is_some() {
            return Err(QueryError(
                "invalid_time",
                format!("{path}: relationship targets have no time samples"),
            ));
        }
        return Ok(vec![targets(&doc, "targets", rel.explain_targets())]);
    }
    if doc.view().attribute(prop).is_none() {
        return Err(QueryError(
            "missing_property",
            format!("{root}: no composed authored property at {path}"),
        ));
    }
    let time = frame.map_or(Time::Default, Time::at);
    if let Some(frame) = frame {
        let prepared = doc.prepare_clip_query(prop, time, usize::MAX, usize::MAX);
        if let Some(failure) = prepared.failures.first() {
            return Err(incomplete(format!(
                "{}: {}",
                failure.request.identifier, failure.error
            )));
        }
        if let Some(error) = prepared.plan.error {
            return Err(incomplete(format!("{path}: {error}")));
        }
        if let Some(request) = prepared.plan.demands.iter().map(|d| &d.request).find(|r| {
            matches!(
                r.status,
                layerstack::value_clips::ClipAssetStatus::Unavailable(_)
            ) && r.role != layerstack::value_clips::ClipAssetRole::TemplateCandidate
        }) {
            return Err(incomplete(format!(
                "{path}: unavailable clip source {}",
                request.identifier
            )));
        }
        if !prepared.plan.is_complete() {
            return Err(incomplete(format!(
                "could not resolve clip sources for {path} at {frame}"
            )));
        }
    }
    let attr = doc
        .view()
        .attribute(prop)
        .ok_or_else(|| incomplete(format!("lost {path} during clip loading")))?;
    if let Some(e) = attr.clip_evaluation_error(time) {
        return Err(incomplete(format!("{path}: {e}")));
    }
    let mut section = Section {
        field: "value",
        source: "none",
        values: vec![],
        opinions: vec![],
    };
    if let Some(why) = attr.explain(time) {
        section.source = match why.source {
            ValueSource::None => "none",
            ValueSource::Default => "default",
            ValueSource::Fallback => "fallback",
            ValueSource::Spline => "spline",
            ValueSource::ValueClips => "valueClips",
            ValueSource::TimeSamples { .. } => "timeSamples",
        };
        if let Some(value) = why.value {
            section.values.push(
                preview(&value, doc.store()).map_err(|e| incomplete(format!("{path}: {e}")))?,
            );
        }
        for opinion in why.opinions {
            let detail = opinion
                .samples
                .iter()
                .map(|s| format!("sample {} ({})", s.time, role(&s.role)))
                .collect();
            section.opinions.push(site(
                &doc,
                opinion.layer(),
                &opinion.spec_path(),
                role(&opinion.role),
                detail,
            ));
        }
        if let Some(clip) = why.clip {
            let mut samples = vec![&clip.lower];
            if clip.lower != clip.upper {
                samples.push(&clip.upper);
            }
            for sample in samples {
                if let Some(layer) = sample.layer {
                    section.opinions.push(site(
                        &doc,
                        layer,
                        &sample.spec_path,
                        "clip sample",
                        vec![format!(
                            "{} at clip time {} (stage {})",
                            clip.set_name, sample.clip_time, sample.stage_time
                        )],
                    ));
                }
            }
        }
    }
    let mut sections = vec![section];
    let connections = targets(&doc, "connections", attr.explain_connections());
    if !connections.opinions.is_empty() {
        sections.push(connections);
    }
    Ok(sections)
}

fn role(role: &OpinionRole) -> &'static str {
    match role {
        OpinionRole::Contributed(Contribution::ArrayEdit) => "array edit",
        OpinionRole::Contributed(Contribution::Dictionary(_)) => "merged dictionary",
        OpinionRole::Contributed(Contribution::ListEdit) => "list edit",
        OpinionRole::Contributed(_) => "contributed",
        OpinionRole::Block => "blocked",
        OpinionRole::Ignored(IgnoreCause::Shadowed) => "shadowed",
        OpinionRole::Ignored(IgnoreCause::CutOffByBlock) => "cut off by block",
        OpinionRole::Ignored(IgnoreCause::Incompatible) => "incompatible",
        OpinionRole::Ignored(_) => "ignored",
    }
}

fn preview(value: &Value, store: &InMemoryStore) -> Result<String, String> {
    Ok(match value {
        Value::Token(id) => store.tokens().resolve(*id).into(),
        Value::String(text) => format!("{text:?}"),
        Value::Array(items) => format!("[{} elements]", items.len()),
        Value::TypedArray(items) => format!("[{} elements]", items.len()),
        Value::Dictionary(items) => format!("{{{} entries}}", items.len()),
        Value::Opaque { type_name, bytes } => format!(
            "{} ({} bytes)",
            store.tokens().resolve(*type_name),
            bytes.len()
        ),
        Value::Deferred(value) => {
            return preview(value.try_materialize().map_err(|e| e.to_string())?, store);
        }
        value => value.to_string(),
    })
}

fn targets(doc: &Document, field: &'static str, why: Option<TargetsExplanation>) -> Section {
    let mut section = Section {
        field,
        source: "listOp",
        values: vec![],
        opinions: vec![],
    };
    let Some(why) = why else {
        return section;
    };
    let display = |p: &TargetPath| p.display(doc.store().paths(), doc.store().tokens());
    section.values = why.targets.iter().map(display).collect();
    for op in why.opinions {
        let list = &op.authored;
        let mut detail = Vec::new();
        let mut edits = Vec::new();
        for (name, paths) in [
            ("explicit", list.explicit.as_deref()),
            ("delete", Some(list.delete.as_slice())),
            ("add", Some(&list.add)),
            ("prepend", Some(&list.prepend)),
            ("append", Some(&list.append)),
            ("reorder", Some(&list.reorder)),
        ] {
            if let Some(paths) = paths.filter(|paths| !paths.is_empty() || name == "explicit") {
                edits.push((name, paths.iter().map(display).collect()));
            }
        }
        for (from, to) in op.kept {
            if from != to {
                detail.push(format!("{} maps to {}", display(&from), display(&to)));
            }
        }
        for drop in op.dropped {
            detail.push(format!(
                "dropped {} ({})",
                display(&drop.authored),
                drop.reason
            ));
        }
        let role = if op.contributes {
            "contributed"
        } else {
            "shadowed"
        };
        let mut row = site(doc, op.layer, &op.spec, role, detail);
        row.edits = edits;
        section.opinions.push(row);
    }
    section
}

fn write_text(
    out: &mut impl Write,
    path: &str,
    frame: Option<f64>,
    sections: &[Section],
) -> io::Result<()> {
    write!(out, "{path}")?;
    if let Some(frame) = frame {
        write!(out, " at {frame}")?;
    }
    writeln!(out)?;
    for s in sections {
        let value = if s.field == "value" {
            s.values
                .first()
                .map_or("(no value)", String::as_str)
                .to_owned()
        } else {
            format!("[{}]", s.values.join(", "))
        };
        writeln!(out, "  {} ({}): {value}", s.field, s.source)?;
        for row in &s.opinions {
            writeln!(out, "    {}  {}  {}", row.role, row.layer, row.spec)?;
            for (edit, paths) in &row.edits {
                writeln!(out, "      {edit} [{}]", paths.join(", "))?;
            }
            for detail in &row.detail {
                writeln!(out, "      {detail}")?;
            }
        }
    }
    Ok(())
}

fn strings(out: &mut impl Write, values: &[String]) -> io::Result<()> {
    out.write_all(b"[")?;
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.write_all(b",")?;
        }
        json_string(out, Some(value))?;
    }
    out.write_all(b"]")
}

fn write_json(
    out: &mut impl Write,
    root: &str,
    path: &str,
    frame: Option<f64>,
    sections: &[Section],
) -> io::Result<()> {
    out.write_all(b"{\"stage\":")?;
    json_string(out, Some(root))?;
    out.write_all(b",\"property\":")?;
    json_string(out, Some(path))?;
    write!(
        out,
        ",\"time\":{},\"fields\":{{",
        frame.map_or_else(|| "null".into(), |v| v.to_string())
    )?;
    for (i, s) in sections.iter().enumerate() {
        if i > 0 {
            out.write_all(b",")?;
        }
        write!(out, "\"{}\":{{\"source\":\"{}\",", s.field, s.source)?;
        if s.field == "value" {
            out.write_all(b"\"preview\":")?;
            json_string(out, s.values.first().map(String::as_str))?;
        } else {
            out.write_all(b"\"paths\":")?;
            strings(out, &s.values)?;
        }
        out.write_all(b",\"opinions\":[")?;
        for (j, row) in s.opinions.iter().enumerate() {
            if j > 0 {
                out.write_all(b",")?;
            }
            out.write_all(b"{\"layer\":")?;
            json_string(out, Some(&row.layer))?;
            out.write_all(b",\"spec\":")?;
            json_string(out, Some(&row.spec))?;
            write!(out, ",\"role\":\"{}\",\"detail\":", row.role)?;
            strings(out, &row.detail)?;
            if !row.edits.is_empty() {
                out.write_all(b",\"edits\":{")?;
                for (i, (operation, paths)) in row.edits.iter().enumerate() {
                    if i != 0 {
                        out.write_all(b",")?;
                    }
                    json_string(out, Some(operation))?;
                    out.write_all(b":")?;
                    strings(out, paths)?;
                }
                out.write_all(b"}")?;
            }
            out.write_all(b"}")?;
        }
        out.write_all(b"]}")?;
    }
    writeln!(out, "}}}}")
}
