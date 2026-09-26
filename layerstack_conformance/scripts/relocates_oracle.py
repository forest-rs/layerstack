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
included; for every attribute its resolved default; for every
relationship its targets; and for every composition error its kind and
the composed prim whose prim index reports it.
"""
import json
import os
import sys

from pxr import Pcp, Sdf, Usd

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
        </Gravel/Chip>: </Bin/Tray/Flake>,
        </Flyer/Streamer>: </Ribbon>,
        </Flyer/Tail>: </Knot>,
        </Chain_2/Tail>: </Chain_2/Tail_2>,
        </Chain/Tail>: </Chain/Tail_1>
    }
)

# Classes implied through relocation sources ("spooky" inherits):
# `puppet.usda` relocates prims its reference to `strings.usda` brings. The
# relocated prims keep the classes of their sources' namespace: the class
# `/_class_Puppet` that `/Puppet` inherits, implied into this layer stack
# as `/_class_Puppet`, and the class `/Strings/SymHand` that both hands
# inherit, implied as `/Marionette/Strings/SymHand`. That class references
# `glove.usda`, whose variant set it selects here; `RHand` selects its own.
def "Marionette" (
    references = @./puppet.usda@</Puppet>
)
{
    over "Strings"
    {
        over "SymHand" (
            variants = {
                string fit = "tight"
            }
        )
        {
            int reach = 0

            over "Tip"
            {
                int bend = 0
            }
        }

        over "RHand" (
            variants = {
                string fit = "loose"
            }
        )
        {
        }
    }
}

class "_class_Puppet"
{
    over "Strings"
    {
        over "Thumb"
        {
            int reach = 5
        }

        over "LHand"
        {
            over "Tip"
            {
                int bend = 5
            }
        }
    }

    over "Controls"
    {
        over "LTip"
        {
            int tilt = 5
        }
    }
}

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

# `kite.usda` relocates `/Kite/Tail`, which its reference to `frame.usda`
# brings, to `/Kite/Streamer`. This layer relocates both: `/Ribbon`
# composes the relocated tail, and `/Knot` relocates a prohibited child,
# so it composes nothing and is reported. `kite.usda`'s opinion at its
# source is reported on each prim whose arcs reach the relocated tail:
# `/Ribbon`, and `/Bowline`, which references a child of it.
def "Flyer" (
    references = @./kite.usda@</Kite>
)
{
}

def "Bowline" (
    references = </Ribbon/Bow>
)
{
}

# `/Chain` references `/Chain_1`, which references `/Chain_2`, and both
# `/Chain/Tail` and `/Chain_2/Tail` are relocated here. `/Chain/Tail`
# reaches `/Chain_2/Tail`, a relocation source, so `/Chain/Tail_1` composes
# nothing, not even `/Chain_1/Tail`, and `/Chain/Tail_2` composes the tail.
def "Chain" (
    references = </Chain_1>
)
{
}

def "Chain_1" (
    references = </Chain_2>
)
{
    def "Tail"
    {
        int length = 5
    }
}

def "Chain_2" (
    references = @./frame.usda@</Frame>
)
{
}
''',
    "kite": '''#usda 1.0
(
    relocates = {
        </Kite/Tail>: </Kite/Streamer>
    }
)

def "Kite" (
    references = @./frame.usda@</Frame>
)
{
    over "Streamer"
    {
        int width = 2
    }

    over "Tail"
    {
        int length = 9
    }
}
''',
    "frame": '''#usda 1.0

def "Frame"
{
    def "Tail"
    {
        int length = 1

        def "Bow"
        {
            int loops = 1
        }
    }
}
''',
    "arm": '''#usda 1.0

def "Arm"
{
    # Target paths map through the relocates of the layer stacks above
    # them; a relocate to nothing does not change them.
    rel wrist = </Arm/Rig/Controls/Wrist>
    rel wristAngle = </Arm/Rig/Controls/Wrist.angle>
    rel debug = </Arm/Rig/Debug>

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
    rel gear = </Parts/Gear>

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
    "puppet": '''#usda 1.0
(
    relocates = {
        </Puppet/Strings/Thumb>: </Puppet/Controls/Thumb>,
        </Puppet/Strings/LHand/Tip>: </Puppet/Controls/LTip>,
        </Puppet/Strings/RHand/Tip>: </Puppet/Controls/RTip>
    }
)

def "Puppet" (
    inherits = </_class_Puppet>
)
{
    def "Strings" (
        references = @./strings.usda@</Strings>
    )
    {
    }

    def "Controls"
    {
    }
}

class "_class_Puppet"
{
    over "Strings"
    {
        over "Thumb"
        {
            int slack = 1
        }

        over "LHand"
        {
            over "Tip"
            {
                int slack = 1
            }
        }
    }

    over "Controls"
    {
        over "LTip"
        {
            int grip = 1
        }
    }
}
''',
    "strings": '''#usda 1.0

def "Strings"
{
    class "SymHand" (
        references = @./glove.usda@</Glove>
    )
    {
        int reach = 3
    }

    def "Thumb" (
        inherits = </Strings/SymHand>
    )
    {
    }

    def "LHand" (
        inherits = </Strings/SymHand>
    )
    {
    }

    def "RHand" (
        inherits = </Strings/SymHand>
    )
    {
    }
}
''',
    "glove": '''#usda 1.0

def "Glove" (
    variantSets = "fit"
    variants = {
        string fit = "loose"
    }
)
{
    variantSet "fit" = {
        "loose" {
            int slack = 2

            def "Tip"
            {
                int slack = 2
            }
        }
        "tight" {
            int slack = 0

            def "Tip"
            {
                int slack = 0
            }
        }
    }
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


def error_kind(error):
    """The `PcpErrorType` name of a composition error
    (`ErrorArcToProhibitedChild` is `ArcToProhibitedChild`)."""
    return type(error).__name__.removeprefix("Error")


def composition_errors(root):
    """Every composition error, as its kind and the composed prim whose
    prim index reports it (`/` for the root layer stack's).

    A prim index reports the errors of the prim indexes its arcs compute
    from scratch too, whose own root site `rootSite` names."""
    cache = Pcp.Cache(Pcp.LayerStackIdentifier(root), usd=True)
    _, errs = cache.ComputeLayerStack(cache.GetLayerStackIdentifier())
    errors = {(error_kind(error), "/") for error in errs}

    def walk(path):
        index, errs = cache.ComputePrimIndex(path)
        errors.update((error_kind(error), str(path)) for error in errs)
        names, _prohibited = index.ComputePrimChildNames()
        for name in names:
            walk(path.AppendChild(name))

    walk(Sdf.Path.absoluteRootPath)
    return sorted(errors)


def compose(directory):
    root = Sdf.Layer.FindOrOpen(os.path.join(directory, "root.usda"))
    stage = Usd.Stage.Open(root)
    prims = []
    values = {}
    targets = {}
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
        for rel in prim.GetRelationships():
            targets[str(rel.GetPath())] = [str(t) for t in rel.GetTargets()]
    errors = [{"kind": kind, "prim": prim} for kind, prim in composition_errors(root)]
    return {"prims": prims, "values": values, "targets": targets, "errors": errors}


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
