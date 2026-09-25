# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD composes for relocated prims.

Usage: relocates_oracle.py [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/relocates` by default:

- `*.usda`: the layers below, as authored;
- `oracle.json`: what OpenUSD composes from `root.usda`, which
  `tests/relocates.rs` replays against layerstack.

A layer's `relocates` metadata moves a prim of its layer stack to a new
path: the relocated prim composes at the target from the target's own
opinions and the ancestral opinions of the source, and the source path no
longer exists in that layer stack or in any namespace it is mapped into.
Opinions a layer stack authors at a source are ignored, and an arc to a
source contributes nothing; both are composition errors (AOUSD Core
§10.3.2.6; `_EvalNodeRelocations` in `pxr/usd/pcp/primIndex.cpp`).

For every composed prim the vectors record its prim stack, repeats
included; for every attribute its resolved default; and for every
composition error its kind and the prim it was found on.
"""
import json
import os
import sys

from pxr import Usd

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(os.path.join(HERE, "..", "fixtures", "relocates"))

LAYERS = {
    "root": '''#usda 1.0
(
    relocates = {
        </Garden/Bed>: </Garden/Plot>,
        </Robot/Rig/Controls/Wrist>: </Robot/Anim/Wrist>,
        </Robot/Rig/Debug>: <>,
        </Boulder/Chip>: </Nowhere/Flake>,
        </Rubble/Chip>: </Heap/Flake>,
        </Gravel/Chip>: </Bin/Tray/Flake>
    }
)

# A local relocate: `/Garden/Bed` comes from the class `/_class_Garden`
# that `/Garden` inherits, and composes at `/Garden/Plot`.
def "Garden" (
    inherits = </_class_Garden>
)
{
    over "Plot"
    {
        int depth = 0
    }
}

class "_class_Garden"
{
    def "Bed"
    {
        int depth = 1
        int rows = 1
    }
}

# The rig case: `/Robot/Rig/Controls/Wrist` comes from the reference to
# `arm.usda`, and composes at `/Robot/Anim/Wrist`. The wrist inherits a
# class of the arm, implied into this layer stack as `/Robot/_class_Joint`.
# `/Robot/Rig/Debug` is relocated to nothing: it no longer exists.
def "Robot" (
    references = @./arm.usda@</Arm>
)
{
    def "Anim"
    {
        over "Wrist"
        {
            int angle = 0
        }
    }

    # Opinions at the relocation sources are ignored.
    over "Rig"
    {
        over "Controls"
        {
            over "Wrist"
            {
                int angle = 9
                int twist = 9
            }
        }

        over "Debug"
        {
            int level = 9
        }
    }

    over "_class_Joint"
    {
        int twist = 0
    }
}

# A reference to a relocation source contributes nothing.
def "StaleWrist" (
    references = </Robot/Rig/Controls/Wrist>
)
{
}

# A reference to the relocation target composes the relocated prim.
def "SpareWrist" (
    references = </Robot/Anim/Wrist>
)
{
}

# Relocates authored in the layer stack an unselected variant branch
# references do not apply: `/Pebble/Chip` stays where the selected branch
# puts it, and `/Cobble`, which selects the other branch, has it at
# `/Cobble/Flake`.
def "Pebble" (
    variantSets = "cut"
    variants = {
        string cut = "plain"
    }
)
{
    variantSet "cut" = {
        "moved" (
            references = @./quarry.usda@</Stone>
        )
        {
        }
        "plain" (
            references = @./stone.usda@</Stone>
        )
        {
        }
    }
}

def "Cobble" (
    variantSets = "cut"
    variants = {
        string cut = "moved"
    }
)
{
    variantSet "cut" = {
        "moved" (
            references = @./quarry.usda@</Stone>
        )
        {
        }
        "plain" (
            references = @./stone.usda@</Stone>
        )
        {
        }
    }
}

# A relocated prim composes at its target only beneath a parent that
# exists without it: `/Nowhere` does not, `/Heap` has a spec here, and
# `/Bin/Tray` comes from the reference to `bin.usda`.
def "Boulder" (
    references = @./stone.usda@</Stone>
)
{
}

def "Rubble" (
    references = @./stone.usda@</Stone>
)
{
}

def "Heap"
{
}

def "Gravel" (
    references = @./stone.usda@</Stone>
)
{
}

def "Bin" (
    references = @./bin.usda@</Bin>
)
{
}

# Relocates of a referenced layer stack apply through the reference.
def "Crate" (
    references = @./kit.usda@</Kit>
)
{
    over "Gear"
    {
        int teeth = 0
    }
}
''',
    "arm": '''#usda 1.0

def "Arm"
{
    def "Rig"
    {
        def "Controls"
        {
            def "Wrist" (
                inherits = </Arm/_class_Joint>
            )
            {
                int angle = 1
            }

            def "Elbow"
            {
                int angle = 1
            }
        }

        def "Debug"
        {
            int level = 1
        }
    }

    class "_class_Joint"
    {
        int angle = 2
        int twist = 2
        int limit = 2
    }
}
''',
    "kit": '''#usda 1.0
(
    relocates = {
        </Kit/Parts/Gear>: </Kit/Gear>
    }
)

def "Kit"
{
    def "Parts" (
        references = @./parts.usda@</Parts>
    )
    {
    }

    over "Gear"
    {
        int teeth = 1
        int size = 1
    }
}
''',
    "parts": '''#usda 1.0

def "Parts"
{
    def "Gear"
    {
        int teeth = 2
        int size = 2
        int pitch = 2
    }

    def "Spring"
    {
        int coils = 2
    }
}
''',
    "stone": '''#usda 1.0

def "Stone"
{
    def "Chip"
    {
        int grain = 7
    }
}
''',
    "quarry": '''#usda 1.0
(
    relocates = {
        </Stone/Chip>: </Stone/Flake>
    }
)

def "Stone" (
    references = @./stone.usda@</Stone>
)
{
}
''',
    "bin": '''#usda 1.0

def "Bin"
{
    def "Tray"
    {
        int slots = 3
    }
}
''',
}


def layer_name(identifier):
    """The layer's file name without directory."""
    return os.path.basename(identifier)


def write_layers(directory):
    os.makedirs(directory, exist_ok=True)
    for name, text in LAYERS.items():
        with open(os.path.join(directory, f"{name}.usda"), "w") as f:
            f.write(text)


def error_record(error):
    """The kind of a composition error (`ErrorArcToProhibitedChild` is
    `ArcToProhibitedChild`) and the prim it was found on."""
    kind = type(error).__name__.removeprefix("Error")
    return (kind, str(error.rootSite.path))


def compose(directory):
    stage = Usd.Stage.Open(os.path.join(directory, "root.usda"))
    prims = []
    values = {}
    for prim in stage.TraverseAll():
        prims.append({
            "path": str(prim.GetPath()),
            "prim_stack": [[layer_name(spec.layer.identifier), str(spec.path)]
                           for spec in prim.GetPrimStack()],
        })
        for attr in prim.GetAttributes():
            value = attr.Get()
            if value is not None:
                values[str(attr.GetPath())] = value
    # OpenUSD may report one error once per prim index that reaches it.
    errors = [{"kind": kind, "prim": prim} for kind, prim in
              sorted({error_record(error) for error in stage.GetCompositionErrors()})]
    return {"prims": prims, "values": values, "errors": errors}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    write_layers(out_dir)
    version = ".".join(str(v) for v in Usd.GetVersion())
    doc = {
        "generator": "layerstack_conformance/scripts/relocates_oracle.py",
        "openusd_version": version,
        "root": "root.usda",
        **compose(out_dir),
    }
    out_path = os.path.join(out_dir, "oracle.json")
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print(f"wrote {len(doc['prims'])} prims from OpenUSD {version} to {out_path}")


if __name__ == "__main__":
    main()
