# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records `UsdShadeConnectionSourceInfo(stage, sourcePath)`, path by path.

Usage: connection_source_info_oracle.py [--check]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.

Opens `fixtures/connection_source_info/scene.usda` and, for each path in
`PATHS` (text, parsed with `Sdf.Path`), builds the source info as
`UsdShadeConnectionSourceInfo`'s stage constructor does
(`pxr/usd/usdShade/connectableAPI.cpp`): nothing for a path that is not a
property path (an empty path where the text does not parse); otherwise
the name split from its `inputs:` or `outputs:` namespace
(`UsdShadeUtils::GetBaseNameAndType`), the prim at the path's prim path,
variant selections stripped (`SdfPath::GetPrimPath`), and the type name
of the attribute at the path when there is one. It records whether the
info is valid, its name, type, type name and source prim, in
`fixtures/connection_source_info/oracle.json`, which
`tests/conformance/connection_source_info.rs` replays against
`layerstack_schemas::ConnectionSourceInfo::new`. `--check` writes nothing
and fails on a difference.
"""
import argparse
import json
import os

from pxr import Sdf, Usd, UsdShade

FIXTURE = os.path.normpath(os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "fixtures", "connection_source_info"))

PATHS = [
    "/Mat/Source.outputs:out",
    "/Mat/Source.outputs:defined",
    "/Mat/Source.inputs:in",
    "/Mat/Source.inputs:a:b",
    "/Mat/Source.outputs:rel",
    "/Mat/Source.out",
    "/Mat/Source.inputs",
    "/Mat/Source.inputsx:in",
    "/Mat/Source",
    "/Mat/Missing.outputs:out",
    "/Mat/Over.outputs:out",
    "/Mat/Inactive.outputs:out",
    "/Mat/Inactive/Kid.outputs:out",
    "/Mat.outputs:surface",
    "/Mat.outputs:missing",
    "/Mat/Source.outputs:",
    "/Mat/Source.outputs:out[/X]",
    "/Mat/Source.outputs:out[/X].inputs:y",
    "/Mat/Source{v=a}.outputs:out",
    "/Mat/Lamp{shade=warm}.outputs:glow",
    "/Mat/Lamp.outputs:glow",
    "/Mat{v=}Source.outputs:defined",
    "/Robot.outputs:out",
    "/Robot/Eye.outputs:out",
    "/RobotPrototype/Eye.outputs:out",
    "Source.outputs:out",
    "../Source.outputs:out",
    ".outputs:out",
    "/Mat/Lamp{shade=warm}{other=}.outputs:glow",
    "/Mat/Lamp/{shade=warm}.outputs:glow",
    "/Mat/Source.outputs:out.connect",
    "/",
    "",
    "not a path",
]


def record(stage, text):
    path = Sdf.Path(text)
    info = UsdShade.ConnectionSourceInfo(stage, path)
    source = info.source.GetPrim()
    return {
        "path": text,
        "parsed": str(path),
        "valid": bool(info.IsValid()),
        "name": str(info.sourceName),
        "type": str(info.sourceType).split(".")[-1],
        "type_name": str(info.typeName),
        "source": str(source.GetPath()) if source else "",
    }


def oracle_text():
    stage = Usd.Stage.Open(os.path.join(FIXTURE, "scene.usda"))
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd_version": f"{minor}.{patch}",
        "sources": [record(stage, text) for text in PATHS],
    }
    return json.dumps(oracle, indent=1, sort_keys=True) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    text = oracle_text()
    out = os.path.join(FIXTURE, "oracle.json")
    if args.check:
        with open(out) as f:
            if f.read() != text:
                raise SystemExit(f"{out} differs from OpenUSD's")
        return
    with open(out, "w") as f:
        f.write(text)


if __name__ == "__main__":
    main()
