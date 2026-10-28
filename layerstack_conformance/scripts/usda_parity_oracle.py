# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's text format reads and writes for small USDA
layers, each written to show one rule of its parser or writer.

Usage: usda_parity_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/usda_parity` by default,
`oracle.json`, which `tests/conformance/usda_parity.rs` replays against
layerstack's USDA reader and writer.

For each case of `CASES`, its source is written to a file and opened with
`Sdf.Layer.OpenAsAnonymous`. The oracle records whether OpenUSD reads it
(`reads`), and then `ExportToString` of the layer (`text`) and, for each
property path the case queries, the composed targets of a relationship
(`UsdRelationship::GetTargets`) or connections of an attribute
(`UsdAttribute::GetConnections`) on a stage opened on the layer
(`targets`). For a file OpenUSD refuses it records the first line of the
error (`error`) with the file's location removed.

The cases:

- `read_*`: syntax OpenUSD reads that layerstack must read too: a
  relative target in a variant, which anchors at the prim the variant
  belongs to (`SdfPath::GetPrimPath` drops a path's trailing variant
  selections, `TextParserAction<PathRef>`,
  `pxr/usd/sdf/textFileFormatParser.cpp`); `varying rel`;
  `symmetryFunction` as an identifier, a C++ namespaced one or nothing;
  `;` between dictionary entries; `displayUnit` as a unit name; list-op
  statements in property metadata; the legacy type names (`Vec3f`,
  `NormalFloat`, ...), and the four that keep their names (`Transform`,
  `PointIndex`, `EdgeIndex`, `FaceIndex`, `_AddLegacyTypesToRegistry`,
  `pxr/usd/sdf/schema.cpp`); `yes` and `no` as bools; comments in a spline;
  an authored but empty `relocates`, which OpenUSD keeps
  (`SdfLayer::HasRelocates`); identifiers in metadata no schema registers,
  which OpenUSD keeps as their text (`SdfUnregisteredValue`) and writes
  unquoted.
  Some are refused, as OpenUSD refuses them: a relative target of a prim
  inside the variant, which anchors at `/P{v=one}Child` and so names a
  variant selection (`<x>`, `<../Other>`), a quoted
  `symmetryFunction` and an unknown unit.
- `write_*`: layers whose text OpenUSD writes back its own way: a
  `defaultPrim` the layer does not hold; `prefixSubstitutions` and
  `suffixSubstitutions` (`_WriteDictionary`, `stringValuesOnly`); a
  connection to a prim; a variant selection that is an expression; the
  header version each feature requests (`RequestWriteVersionUpgrade`,
  `pxr/usd/sdf/fileIO_Common.cpp`: 1.1 for a tangent algorithm, 1.2 for
  an array edit of strings, tokens or asset paths but not of numbers, 1.3
  for a loop boundary time, 1.4 for a duration value, nothing for a
  `timecode` spline or a `duration` declaration); `__AnyType__` prims
  untyped; knot custom data on one line; the curve type only with a curve
  segment; offsets within 1e-6 of the identity dropped
  (`SdfLayerOffset::operator==`); empty comments, an empty layer `doc` and
  a false `hasOwnedSubLayers` dropped; `, ` between relocates; `@@@`
  around an asset path holding `@`, with `\\@@@` inside.
- `strict_*`: text OpenUSD refuses, which a strict import refuses too:
  `None` or `[]` in a list-edit statement; duplicate items in a target,
  connection, arc or `variantSets` statement (`_HasDuplicates`,
  `pxr/usd/sdf/textParserHelpers.h`), though not in a list-op metadata
  statement (`read_duplicate_metadata_list_items`); an attribute whose
  statements disagree on its type or variability; an inherit path
  climbing above the root; a tuple of the wrong dimensions; a line break
  in a quoted string; `curve(` with no space; a list-op keyword on a
  value and metadata after `.connect` or `.timeSamples`; two metadata
  entries, or dictionary entries, on one line without `;`; a line break
  after `=`, before a list's `,`, after an attribute's type or before a
  prim's name; relocates without `,` between them; a prim body
  statement not ended by a line break (or, after a property or
  `reorder` statement, a `;`) before the prim's closing `}` or another
  statement, and a `;` that ends no statement on its line (`PrimItem`,
  `LayerSpec`, `StatementSequenceOf`, `pxr/usd/sdf/textFileFormatParser.h`),
  while `read_statements_on_one_line` reads the forms that do end one; a
  spline with both inner loops and a looping extrapolation with a loop
  boundary time, in either order (`TsSpline::_SetInnerLoopParams`,
  `SetPreExtrapolation`, `pxr/base/ts/spline.cpp`), where
  `read_spline_bounded_loop_and_default_inner_loops` reads each alone.
- `statements_*`: several list-op statements for one field, on a
  relationship, a connection, references, inherits, `variantSets` and
  metadata (`apiSchemas`). OpenUSD's parser sets each statement's list
  with `SdfListOp::SetItems` (`_SetListOpItemsWithError`,
  `pxr/usd/sdf/textParserHelpers.h`): a later statement of one kind
  replaces the earlier one's items, an explicit statement drops the edits
  and an edit statement drops the explicit list
  (`SdfListOp::_SetExplicit`, `pxr/usd/sdf/listOp.cpp`).

`--check` rewrites `oracle.json` into a temporary directory and fails
unless it matches the checked-in fixture.
"""
import argparse
import difflib
import filecmp
import json
import os
import re
import sys
import tempfile

from pxr import Sdf, Tf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "usda_parity"))

# name -> (source, property paths whose composed targets are recorded)
CASES = {
    "statements_prepend_after_explicit_rel": ('''#usda 1.0

def "P"
{
    rel s = </A>
    prepend rel s = </B>
}
''', ["/P.s"]),
    "statements_repeated_prepend_rel": ('''#usda 1.0

def "P"
{
    prepend rel t = [</A>, </B>]
    prepend rel t = </C>
    append rel t = </D>
}
''', ["/P.t"]),
    "statements_explicit_after_prepend_rel": ('''#usda 1.0

def "P"
{
    prepend rel u = </A>
    rel u = []
}
''', ["/P.u"]),
    "statements_repeated_delete_rel": ('''#usda 1.0

def "P"
{
    delete rel r = </A>
    delete rel r = </B>
    append rel r = [</A>, </B>, </C>]
}
''', ["/P.r"]),
    "statements_explicit_after_add_connect": ('''#usda 1.0

def "P"
{
    add float c.connect = </X.y>
    float c.connect = None
}
''', ["/P.c"]),
    "statements_prepend_after_explicit_connect": ('''#usda 1.0

def "P"
{
    float c.connect = [</X.a>, </X.b>]
    prepend float c.connect = </X.c>
    append float c.connect = </X.d>
    append float c.connect = </X.e>
}
''', ["/P.c"]),
    "statements_explicit_after_edits_refs": ('''#usda 1.0

def "P" (
    delete references = @a.usda@
    add references = @b.usda@
    prepend references = @c.usda@
    references = @d.usda@
)
{
}
''', []),
    "statements_edits_after_explicit_refs": ('''#usda 1.0

def "P" (
    references = @d.usda@
    prepend references = @c.usda@
    prepend references = @e.usda@
    append references = @f.usda@
)
{
}
''', []),
    "statements_inherits_in_variant": ('''#usda 1.0

def "P" (
    variants = {
        string v = "one"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "one" (
            inherits = </A>
            prepend inherits = </B>
            prepend inherits = </C>
        ) {
        }
    }
}

class "A"
{
}

class "B"
{
}

class "C"
{
}
''', []),
    "statements_variant_sets": ('''#usda 1.0

def "P" (
    variantSets = ["a", "b"]
    append variantSets = "c"
)
{
}
''', []),
    "statements_explicit_after_prepend_meta": ('''#usda 1.0

def "P" (
    prepend apiSchemas = ["A"]
    apiSchemas = []
)
{
}
''', []),
    "statements_prepend_after_explicit_meta": ('''#usda 1.0

def "P" (
    apiSchemas = ["A"]
    prepend apiSchemas = ["B"]
)
{
}
''', []),
    "statements_repeated_append_meta": ('''#usda 1.0

def "P" (
    append apiSchemas = ["A", "B"]
    prepend apiSchemas = ["C"]
    append apiSchemas = ["D"]
)
{
}
''', []),
    "read_variant_relative_targets": ('''#usda 1.0

def "P" (
    variants = {
        string v = "one"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "one" (
            prepend inherits = <Base>
        ) {
            rel r = <Child>
            rel up = <../Other>
            float c.connect = <Child.x>

            def "Child"
            {
                float x
            }
        }
    }
}

def "Other"
{
}
''', ["/P.r", "/P.up", "/P.c"]),
    "read_variant_child_relative_target": ('''#usda 1.0

def "P" (
    variants = {
        string v = "one"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "one" {
            def "Child"
            {
                float x
                rel side = <x>
            }
        }
    }
}
''', []),
    "read_variant_climbing_target": ('''#usda 1.0

def "P" (
    variants = {
        string v = "one"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "one" {
            def "Child"
            {
                rel side = <../Other>
            }
        }
    }
}
''', []),
    "read_varying_rel": ('''#usda 1.0

def "P"
{
    varying rel a = </A>
    custom varying rel b
    delete varying rel c = </A>
    prepend varying rel c = [</B>, </C>]
    reorder varying rel c = [</C>, </B>]
    custom varying rel d = </A> (
        doc = "varying"
    )
    rel e = </A>
}
''', ["/P.a", "/P.c", "/P.d", "/P.e"]),
    "read_symmetry_function": ('''#usda 1.0

def "P" (
    symmetryFunction = mirror
)
{
    double a = 1 (
        symmetryFunction = Ns::Inner::flip
    )
    rel r (
        symmetryFunction = 
    )
    custom double b = 0 (symmetryFunction = )
    rel s (permission = private; symmetryFunction = rotate)
}
''', []),
    "read_symmetry_function_string": ('''#usda 1.0

def "P" (
    symmetryFunction = "mirror"
)
{
}
''', []),
    "read_dictionary_semicolons": ('''#usda 1.0

def "P" (
    customData = { string a = "yes"; int b = 4; dictionary c = { string d = "x"; double e = 1 } }
)
{
    double s.spline = {
        0: 6; post linear; {},
        1: 6; {},
    }
}
''', []),
    "read_display_unit": ('''#usda 1.0

def "P"
{
    double a = 1 (displayUnit = mm)
    double b = 2 (
        displayUnit = deg
        doc = "angle"
    )
}
''', []),
    "read_display_unit_unknown": ('''#usda 1.0

def "P"
{
    double a = 1 (displayUnit = furlong)
}
''', []),
    "read_property_metadata_list_ops": ('''#usda 1.0

def "P"
{
    double a (
        delete testIntOps = [1, 2]
        prepend testIntOps = [3]
        append testTokenOps = ["x", "y"]
    )
    rel r (
        add testIntOps = [4]
        reorder testIntOps = [4, 3]
    )
}
''', []),
    "read_legacy_type_names": ('''#usda 1.0

def "P"
{
    custom Vec3f a = (1, 2, 3)
    custom Vec2d[] b = [(1, 2), (3, 4)]
    custom PointFloat[] c = [(0, 0, 0)]
    custom NormalFloat d = (0, 1, 0)
    custom VectorFloat e = (1, 0, 0)
    custom Color f = (1, 0.5, 0)
    custom Quatf g = (1, 0, 0, 0)
    custom Matrix2d h = ( (1, 0), (0, 1) )
    custom Frame i = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) )
    custom Vec4h j.timeSamples = {
        1: (1, 2, 3, 4),
    }
    custom double k (
        customData = {
            Vec3d offset = (1, 2, 3)
        }
    )
}
''', []),
    "read_bool_identifiers": ('''#usda 1.0

def "P"
{
    bool a = yes
    bool b = no
    bool[] c = [yes, no, true, 0]
    bool[] d = edit [
        write yes to [0]
        minsize 3 fill no
    ]
}
''', []),
    "read_spline_comments": ('''#usda 1.0

def "P"
{
    double a.spline = {
        bezier,
        # a comment line
        7: 5.5; post curve/* inline */(1.1, -1.4),
        // another comment
        9: 6,
    }
}
''', []),
    "write_default_prim_in_sublayer": ('''#usda 1.0
(
    defaultPrim = "World"
    subLayers = [
        @./geometry.usda@
    ]
)
''', []),
    "write_substitution_maps": ('''#usda 1.0

def "P" (
    prefixSubstitutions = {
        "$Left": "Right",
        "Left": "Right"
    }
    suffixSubstitutions = {
        "$NUM": "1",
    }
)
{
}
''', []),
    "write_prim_connection": ('''#usda 1.0

def "P"
{
    string a.connect = </Q>
    float b.connect = [</Q>, </Q.x>]
}
''', []),
    "write_expression_selection": ('''#usda 1.0
(
    expressionVariables = {
        string V = "one"
    }
)

def "P" (
    variants = {
        string v = "`${V}`"
    }
    prepend variantSets = "v"
)
{
    variantSet "v" = {
        "one" {
        }
    }
}
''', []),
    "write_version_timecode_spline": ('''#usda 1.0

def "P"
{
    timecode t.spline = {
        bezier,
        0: 10; post curve (0.3, 2),
        2: 20,
    }
}
''', []),
    "write_version_duration_declaration": ('''#usda 1.0

def "P"
{
    duration d
    duration e.spline = {
        0: 1,
    }
}
''', []),
    "write_version_duration_value": ('''#usda 1.0

def "P"
{
    duration d = 2.5
}
''', []),
    "write_version_numeric_array_edit": ('''#usda 1.0

def "P"
{
    int[] a = edit [resize 2; insert 4 at [0]; insert 5 at [1]; append 6]
    double[] b = edit [prepend 1]
}
''', []),
    "write_version_string_array_edit": ('''#usda 1.0

def "P"
{
    token[] a = edit [append "x"]
    asset[] b = edit [prepend @@@a@b@@@; append @c@]
}
''', []),
    "write_version_loop_boundary": ('''#usda 1.0

def "P"
{
    double a.spline = {
        bezier,
        post: loop repeat(5),
        0: 0,
        5: 1,
    }
}
''', []),
    "write_any_type": ('''#usda 1.0

def __AnyType__ "A"
{
}

over __AnyType__ "B"
{
}
''', []),
    "write_knot_custom_data": ('''#usda 1.0

def "P"
{
    double a.spline = {
        bezier,
        0: 6; post curve (1, 2); { string a = "yes"; int b = 4; dictionary c = { string d = "x" } },
        1: 6; post held; { double e = 1 },
        2: 7; post curve (1, 1),
    }
}
''', []),
    "write_curve_type_last_knot": ('''#usda 1.0

def "P"
{
    double a.spline = {
        bezier,
        0: 6; post held,
        1: 7; post curve (1, 1),
    }
}
''', []),
    "write_near_identity_offsets": ('''#usda 1.0
(
    subLayers = [
        @./s.usda@ (offset = 0.0000001),
        @./t.usda@ (offset = 2; scale = 1.0000001),
        @./u.usda@ (scale = 1.0000001)
    ]
)

def "A" (
    prepend references = @./r.usda@ (offset = 0.0000001)
    prepend payload = @./y.usda@ (scale = 1.0000001)
)
{
}

def "B" (
    references = @./r.usda@ (
        offset = 0.0000001
        customData = {
            int k = 1
        }
    )
)
{
}
''', []),
    "write_empty_comments": ('''#usda 1.0
(
    ""
    doc = ""
    hasOwnedSubLayers = false
    upAxis = "Z"
)

def "A" (
    ""
)
{
    float f (
        ""
    )
    rel r (
        ""
    )
    float g (
        ""
        doc = ""
    )
}
''', []),
    "write_layer_header_fields_kept": ('''#usda 1.0
(
    doc = "layer doc"
    hasOwnedSubLayers = true
)
''', []),
    "write_relocates": ('''#usda 1.0
(
    relocates = {
        </A/B>: </A/C>,
        </A/D>: </A/E>
    }
)

def "A"
{
}
''', []),
    "write_asset_paths_with_at": ('''#usda 1.0
(
    subLayers = [
        @@@./a@b.usda@@@
    ]
)

def "P" (
    prepend references = @@@./x@y.usda@@@
)
{
    asset a = @@@cfg@x.ocio@@@
    asset b = @@@a\\@@@b@@@
    asset[] c = [@@@a@b@@@, @plain@]
}
''', []),
    "strict_list_edit_none_refs": ('''#usda 1.0
def "A" (
    prepend references = None
)
{
}
''', []),
    "strict_list_edit_empty_refs": ('''#usda 1.0
def "A" (
    prepend references = []
)
{
}
''', []),
    "strict_list_edit_none_payload": ('''#usda 1.0
def "A" (
    append payload = None
)
{
}
''', []),
    "strict_list_edit_none_inherits": ('''#usda 1.0
def "A" (
    add inherits = None
)
{
}
''', []),
    "strict_list_edit_empty_variant_sets": ('''#usda 1.0
def "A" (
    prepend variantSets = []
)
{
}
''', []),
    "strict_list_edit_none_rel": ('''#usda 1.0
def "A"
{
    prepend rel r = None
}
''', []),
    "strict_list_edit_empty_rel": ('''#usda 1.0
def "A"
{
    delete rel r = []
}
''', []),
    "strict_list_edit_none_connect": ('''#usda 1.0
def "A"
{
    delete float c.connect = None
}
''', []),
    "strict_list_edit_empty_metadata": ('''#usda 1.0
def "A" (
    prepend apiSchemas = []
)
{
}
''', []),
    "strict_duplicate_targets": ('''#usda 1.0
def "A"
{
    rel r = [</B>, </C>, </B>]
}
''', []),
    "strict_duplicate_deleted_targets": ('''#usda 1.0
def "A"
{
    delete rel r = [</B>, </B>]
}
''', []),
    "strict_duplicate_connections": ('''#usda 1.0
def "A"
{
    float c.connect = [</X.a>, </X.a>]
}
''', []),
    "strict_duplicate_references": ('''#usda 1.0
def "A" (
    prepend references = [@a.usda@, @a.usda@]
)
{
}
''', []),
    "strict_duplicate_inherits": ('''#usda 1.0
def "A" (
    prepend inherits = [</B>, </B>]
)
{
}
''', []),
    "strict_duplicate_variant_sets": ('''#usda 1.0
def "A" (
    prepend variantSets = ["v", "v"]
)
{
}
''', []),
    "read_duplicate_metadata_list_items": ('''#usda 1.0
def "A" (
    prepend apiSchemas = ["X", "X"]
)
{
}
''', []),
    "read_references_differing_offsets": ('''#usda 1.0
def "A" (
    references = [@a.usda@, @a.usda@ (offset = 1)]
)
{
}
''', []),
    "strict_type_change": ('''#usda 1.0
def "A"
{
    float a = 1
    double a.timeSamples = {
        1: 2,
    }
}
''', []),
    "strict_role_change": ('''#usda 1.0
def "A"
{
    point3d p = (1, 0, 0)
    normal3d p.timeSamples = {
        1: (0, 1, 0),
    }
}
''', []),
    "strict_variability_change": ('''#usda 1.0
def "A"
{
    uniform float a = 1
    float a.connect = </B.c>
}
''', []),
    "read_variability_restated": ('''#usda 1.0
def "A"
{
    uniform float a = 1
    uniform float a.connect = </B.c>
    varying rel r = </B>
    prepend rel r = </C>
    custom float b = 1
    float b.timeSamples = {
        1: 2,
    }
}
''', []),
    "strict_inherit_above_root": ('''#usda 1.0
def "A" (
    inherits = <../../B>
)
{
}
''', []),
    "strict_matrix_row_length": ('''#usda 1.0
def "A"
{
    matrix2d m = ( (1, 0, 0), (0, 1) )
}
''', []),
    "strict_matrix_row_count": ('''#usda 1.0
def "A"
{
    matrix2d m = ( (1, 0), (0, 1), (0, 0) )
}
''', []),
    "strict_vector_dimension": ('''#usda 1.0
def "A"
{
    float3 v = (1, 2)
}
''', []),
    "strict_vector_array_dimension": ('''#usda 1.0
def "A"
{
    float2[] v = [(1, 2), (3, 4, 5)]
}
''', []),
    "strict_newline_in_string": ('''#usda 1.0
def "A"
{
    string s = "a
b"
}
''', []),
    "strict_spline_curve_spacing": ('''#usda 1.0
def "A"
{
    double a.spline = {
        bezier,
        0: 1; post curve(1, 2),
        1: 2,
    }
}
''', []),
    "strict_list_op_on_value": ('''#usda 1.0
def "A"
{
    add float a = 1
}
''', []),
    "strict_metadata_after_connect": ('''#usda 1.0
def "A"
{
    float a.connect = </Q.b> (doc = "x")
}
''', []),
    "strict_metadata_after_samples": ('''#usda 1.0
def "A"
{
    float a.timeSamples = {
        1: 2,
    } (doc = "x")
}
''', []),
    "strict_layer_metadata_one_line": ('''#usda 1.0
(
    startFrame = 1 endFrame = 2
)
''', []),
    "strict_attribute_metadata_one_line": ('''#usda 1.0
def "A"
{
    float a (doc = "x" hidden = true)
}
''', []),
    "strict_dictionary_one_line": ('''#usda 1.0
def "A" (
    customData = { int a = 1 int b = 2 }
)
{
}
''', []),
    "read_legacy_kept_type_names": ('''#usda 1.0
def "P"
{
    custom Transform a = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (2, 3, 4, 1) )
    custom PointIndex[] b = [0, 1, 2]
    custom EdgeIndex c = 3
    custom FaceIndex[] d.timeSamples = {
        1: [4, 5],
    }
}
''', []),
    "read_empty_relocates": ('''#usda 1.0
(
    relocates = {
    }
)
''', []),
    "read_unregistered_identifiers": ('''#usda 1.0
(
    layerKey = foo
)

def "P" (
    access = foo
    names = [foo, bar]
    mixed = [foo, 1]
    quoted = "foo"
)
{
    double a (
        access = foo
    )
    rel r (
        access = bar
    )
}
''', []),
    "read_unregistered_list_ops": ('''#usda 1.0
def "A" (
    prepend mixed = [1, 2.50, "x", y]
    append mixed = [3]
    prepend mixed = [4]
    delete mixed = ["a"]
    add pairs = [(1, 2), (3, 4)]
    prepend repeated = [1, 1]
    kept = [1, 2]
    prepend kept = [3]
    prepend replaced = [3]
    replaced = [1, 2]
)
{
    double a (
        reorder ints = [3, -1]
        append ints = [18446744073709551615, -2.5]
    )
}

def "B" (
    prepend nothing = None
)
{
}
''', []),
    "read_metadata_semicolons": ('''#usda 1.0
(startFrame = 1; endFrame = 2)

def "A"
{
    float a (doc = "x"; hidden = true)
}
''', []),
    "strict_newline_after_equals": ('''#usda 1.0
def "A"
{
    float a =
        1
}
''', []),
    "strict_newline_after_metadata_equals": ('''#usda 1.0
def "A" (
    kind =
    "x"
)
{
}
''', []),
    "strict_newline_before_comma": ('''#usda 1.0
def "A"
{
    float[] a = [1
    , 2]
}
''', []),
    "read_newline_after_comma": ('''#usda 1.0
def "A"
{
    float[] a = [1,
    2
    ]
    float2 b = (1,
 2)
}
''', []),
    "strict_newline_after_type": ('''#usda 1.0
def "A"
{
    float
    a = 1
}
''', []),
    "strict_newline_before_prim_name": ('''#usda 1.0
def Scope
"A"
{
}
''', []),
    "strict_relocates_without_comma": ('''#usda 1.0
(
    relocates = {
        </a>: </b>
        </c>: </d>
    }
)
''', []),
    "strict_spline_bounded_loop_then_inner_loops": ('''#usda 1.0
def "A"
{
    double a.spline = {
        pre: loop repeat(0),
        loop: (0, 16, 0, 2, 5),
        0: 1; post held,
        16: 2; post held,
    }
}
''', []),
    "strict_spline_inner_loops_then_bounded_loop": ('''#usda 1.0
def "A"
{
    double a.spline = {
        loop: (0, 16, 1, 0, 0),
        post: loop oscillate(16),
        0: 1; post held,
        16: 2; post held,
    }
}
''', []),
    "read_spline_bounded_loop_and_default_inner_loops": ('''#usda 1.0
def "A"
{
    double a.spline = {
        pre: loop repeat(0),
        loop: (0, 0, 0, 0, 0),
        0: 1; post held,
        16: 2; post held,
    }
    double b.spline = {
        post: loop reset,
        loop: (0, 16, 0, 2, 5),
        0: 1; post held,
        16: 2; post held,
    }
}
''', []),
    "strict_property_before_brace": ('''#usda 1.0
def "A" { double a = 1 }
''', []),
    "strict_relationship_before_brace": ('''#usda 1.0
def "A"
{
    rel r = </A> /* a comment */ }
''', []),
    "strict_reorder_before_brace": ('''#usda 1.0
def "A"
{
    reorder nameChildren = ["B"] }
''', []),
    "strict_prim_after_prim_on_line": ('''#usda 1.0
def "A"
{
} def "B"
{
}
''', []),
    "strict_child_after_child_on_line": ('''#usda 1.0
def "A"
{
    def "B"
    {
    } def "C"
    {
    }
}
''', []),
    "strict_child_before_brace": ('''#usda 1.0
def "A" { def "B" {} }
''', []),
    "strict_variant_set_before_brace": ('''#usda 1.0
def "A" (
    variantSets = "v"
)
{
    variantSet "v" = { "x" {} } }
''', []),
    "strict_semicolon_after_prim": ('''#usda 1.0
def "A"
{
};
''', []),
    "strict_semicolon_after_child": ('''#usda 1.0
def "A"
{
    def "B"
    {
    };
}
''', []),
    "strict_semicolon_alone": ('''#usda 1.0
def "A"
{
    double a = 1
    ;
}
''', []),
    "strict_semicolon_twice": ('''#usda 1.0
def "A"
{
    double a = 1;;
}
''', []),
    "strict_metadata_semicolon_alone": ('''#usda 1.0
(
    doc = "x";
    ;
)
''', []),
    "strict_dictionary_semicolon_alone": ('''#usda 1.0
def "A" (
    customData = { ; }
)
{
}
''', []),
    "strict_root_prims_then_prim_on_line": ('''#usda 1.0
reorder rootPrims = ["A"] def "A"
{
}
''', []),
    "read_statements_on_one_line": ('''#usda 1.0
reorder rootPrims = ["A"]; def "A" { double a = 1; rel r = </A>; }
def "B"
{
    reorder nameChildren = ["C"]; def "C" { double c = 1; } # a comment
    variantSet "v" = { "x" { double x = 1; } "y" {} }
}
def "D" (
    customData = { int a = 1; }
)
{
}
def "E" {}''', []),
}


def error_line(error):
    """The first line of OpenUSD's message, without the file's location or
    the parser rule it names: that is a demangled template, which each
    compiler spells differently, so the oracle would differ by platform."""
    lines = [line.strip() for line in str(error).splitlines() if line.strip()]
    text = lines[0] if lines else str(error)
    text = re.sub(r"'[^']*/case\.usda:\d+:\d+: ", "'", text)
    text = re.sub(r"parse error matching .* at '", "parse error at '", text)
    return re.sub(r"Error in '[^']*' at line \d+ in file [^ ]* : ", "", text)


def record(directory, source, queries):
    path = os.path.join(directory, "case.usda")
    with open(path, "w") as f:
        f.write(source)
    try:
        layer = Sdf.Layer.OpenAsAnonymous(path)
    except Tf.ErrorException as e:
        return {"reads": False, "error": error_line(e)}
    if not layer:
        return {"reads": False, "error": "no layer"}
    out = {"reads": True, "text": layer.ExportToString()}
    if queries:
        stage = Usd.Stage.Open(layer)
        targets = {}
        for query in queries:
            prop = stage.GetPropertyAtPath(query)
            if isinstance(prop, Usd.Relationship):
                items = prop.GetTargets()
            else:
                items = prop.GetConnections()
            targets[query] = [str(p) for p in items]
        out["targets"] = targets
    return out


def write(out):
    os.makedirs(out, exist_ok=True)
    _, minor, patch = Usd.GetVersion()
    cases = {}
    with tempfile.TemporaryDirectory() as scratch:
        for name, (source, queries) in CASES.items():
            case = {"source": source}
            case.update(record(scratch, source, queries))
            cases[name] = case
    oracle = {"openusd_version": f"{minor}.{patch}", "cases": cases}
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        fresh = os.path.join(tmp, "oracle.json")
        committed = os.path.join(FIXTURES, "oracle.json")
        if not filecmp.cmp(fresh, committed, shallow=False):
            with open(committed) as a, open(fresh) as b:
                diff = difflib.unified_diff(a.readlines(), b.readlines(),
                                            "committed", "regenerated", n=2)
                sys.stdout.writelines(list(diff)[:200])
            raise SystemExit(f"oracle.json differs from {FIXTURES}")


if __name__ == "__main__":
    main()
