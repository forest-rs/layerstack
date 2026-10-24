# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's authoring calls do when aimed at the pseudo-root.

Usage: pseudo_root_edits_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.8`.
Writes, under `layerstack_conformance/fixtures/pseudo_root_edits` by default:

- `scene.usda`, the layer every case edits;
- `oracle.json`, which `tests/conformance/pseudo_root_edits.rs` replays against
  layerstack.

Each case is one `Transaction` edit addressed to `/`, or to the property
`/.height`, and the matching OpenUSD call at two levels, each made on a
fresh copy of `scene.usda`:

- `usd`: the `UsdPrim` of the stage's pseudo-root (`UsdStage::GetPseudoRoot`)
  and the `UsdAttribute` / `UsdRelationship` it hands out for `height`;
- `sdf`: the layer's pseudo-root spec (`SdfLayer::GetPseudoRoot`), or
  `Sdf.CreatePrimInLayer` / `Sdf.CopySpec` on `/`.

A level without a matching call records `null`. Otherwise it records
whether OpenUSD accepted the call (`false` when it raised a coding error
or returned false or an invalid object), whether the layer changed, and if
it did, whether OpenUSD's own text reader reads the layer it left
(`reads_back`), the prim spec fields the pseudo-root spec type does not
register that it left on the pseudo-root (`prim_fields`), the value it
left in the case's layer field (`field`), and the layer's root prim order. A `usd` record also lists the stage's root
prims, in order.

OpenUSD rejects most of these calls: a field not registered on the
pseudo-root spec type (`_CanEditInfoOnSpec`, `UsdStage::_SetMetadata`), a
prim field (`SdfPrimSpec::_ValidateEdit`), a property (`SdfAttributeSpec::New`,
or a property path that does not parse). It accepts metadata registered on
layers, writing it as the layer's metadata, and the root prim order. A few
calls it accepts write fields the pseudo-root spec type does not register
(references, payloads, `variantSetNames`, `properties`, or a copied
prim's `specifier` and `typeName`), some leaving a layer its own reader
rejects, and a few clears of specs that cannot exist succeed and change
nothing. `SdfAttributeSpec::New` rejects an attribute on the pseudo-root
only while no property path of that name exists yet in the process; the
scene's `/Garden.height` makes it accept `height`.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixtures.
"""
import argparse
import filecmp
import json
import os
import tempfile

from pxr import Sdf, Tf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "pseudo_root_edits"))

SCENE = """#usda 1.0
(
    defaultPrim = "Garden"
    doc = "A garden of rocks and ferns"
)

reorder rootPrims = ["Rock", "Garden"]

def Xform "Garden" (
    variants = {
        string season = "summer"
    }
    prepend variantSets = "season"
)
{
    double height = 1
    variantSet "season" = {
        "autumn" {
        }
        "summer" {
        }
    }
}

def Xform "Rock"
{
}
"""

ROOT = Sdf.Path.absoluteRootPath

# Fields of prim specs that the pseudo-root spec type does not register.
PRIM_FIELDS = [
    "active", "apiSchemas", "customData", "hidden", "inheritPaths", "instanceable", "kind",
    "payload", "properties", "propertyOrder", "references", "specializes", "specifier",
    "typeName", "variantSelection", "variantSetChildren", "variantSetNames",
]


def ref_op():
    return Sdf.ReferenceListOp.Create(prependedItems=[Sdf.Reference("", "/Rock")])


def height(prim):
    return prim.GetAttribute("height")


def rel(prim):
    return prim.GetRelationship("height")


def erase_variant_set(spec):
    if "season" in spec.variantSets:
        del spec.variantSets["season"]
    return True


# (name, the layerstack edit, the `UsdPrim` call on the pseudo-root, the
# call on the layer's pseudo-root spec). Each call takes the stage or layer
# and the object it edits, and returns whether OpenUSD accepted it.
CASES = [
    # Prim specs.
    ("create_prim", {"op": "create_prim"},
     lambda s, p: s.DefinePrim("/").IsValid(),
     lambda l, r: bool(Sdf.CreatePrimInLayer(l, ROOT))),
    ("remove_spec", {"op": "remove_spec"},
     lambda s, p: s.RemovePrim(ROOT),
     None),
    ("copy_spec_to_pseudo_root", {"op": "copy_spec", "from": "/Rock", "to": "/"},
     None,
     lambda l, r: Sdf.CopySpec(l, "/Rock", l, ROOT)),
    ("copy_spec_from_pseudo_root", {"op": "copy_spec", "from": "/", "to": "/Copy"},
     None,
     lambda l, r: Sdf.CopySpec(l, ROOT, l, "/Copy")),
    # Metadata.
    ("set_metadata_documentation",
     {"op": "set_metadata", "key": "documentation", "string": "Moss on every rock"},
     lambda s, p: p.SetMetadata("documentation", "Moss on every rock"),
     lambda l, r: r.SetInfo("documentation", "Moss on every rock") or True),
    ("set_metadata_start_time_code",
     {"op": "set_metadata", "key": "startTimeCode", "double": 12.0},
     lambda s, p: p.SetMetadata("startTimeCode", 12.0),
     lambda l, r: r.SetInfo("startTimeCode", 12.0) or True),
    ("set_metadata_kind", {"op": "set_metadata", "key": "kind", "token": "component"},
     lambda s, p: p.SetMetadata("kind", "component"),
     lambda l, r: r.SetInfo("kind", "component") or True),
    ("set_metadata_hidden", {"op": "set_metadata", "key": "hidden", "bool": True},
     lambda s, p: p.SetMetadata("hidden", True),
     lambda l, r: r.SetInfo("hidden", True) or True),
    ("clear_metadata_documentation", {"op": "clear_metadata", "key": "documentation"},
     lambda s, p: p.ClearMetadata("documentation"),
     lambda l, r: r.ClearInfo("documentation") or True),
    ("clear_metadata_kind", {"op": "clear_metadata", "key": "kind"},
     lambda s, p: p.ClearMetadata("kind"),
     lambda l, r: r.ClearInfo("kind") or True),
    # Variant selections and variant sets.
    ("set_variant_selection", {"op": "set_variant_selection", "variant": "autumn"},
     lambda s, p: p.GetVariantSet("season").SetVariantSelection("autumn"),
     lambda l, r: r.variantSelections.__setitem__("season", "autumn") or True),
    ("clear_variant_selection", {"op": "set_variant_selection", "variant": None},
     lambda s, p: p.GetVariantSet("season").ClearVariantSelection(),
     lambda l, r: r.variantSelections.__delitem__("season") or True),
    ("add_variant_set", {"op": "add_variant_set"},
     lambda s, p: p.GetVariantSets().AddVariantSet("season").IsValid(),
     lambda l, r: bool(Sdf.VariantSetSpec(r, "season"))),
    ("remove_variant_set", {"op": "remove_variant_set"},
     None,
     lambda l, r: erase_variant_set(r)),
    ("set_variant_set_names", {"op": "set_variant_set_names"},
     lambda s, p: p.SetMetadata(
         "variantSetNames", Sdf.StringListOp.Create(prependedItems=["season"])),
     lambda l, r: r.variantSetNameList.Prepend("season") or True),
    # Applied schemas.
    ("add_applied_schema", {"op": "add_applied_schema"},
     lambda s, p: p.AddAppliedSchema("CollectionAPI:plants"),
     lambda l, r: r.SetInfo(
         "apiSchemas", Sdf.TokenListOp.Create(prependedItems=["CollectionAPI:plants"])) or True),
    ("remove_applied_schema", {"op": "remove_applied_schema"},
     lambda s, p: p.RemoveAppliedSchema("CollectionAPI:plants"),
     lambda l, r: r.SetInfo(
         "apiSchemas", Sdf.TokenListOp.Create(deletedItems=["CollectionAPI:plants"])) or True),
    # Prim fields.
    ("set_specifier", {"op": "set_specifier"},
     lambda s, p: p.SetSpecifier(Sdf.SpecifierOver),
     lambda l, r: setattr(r, "specifier", Sdf.SpecifierOver) or True),
    ("set_type_name", {"op": "set_type_name"},
     lambda s, p: p.SetTypeName("Scope"),
     lambda l, r: setattr(r, "typeName", "Scope") or True),
    ("clear_type_name", {"op": "clear_type_name"},
     lambda s, p: p.ClearTypeName(),
     lambda l, r: r.ClearInfo("typeName") or True),
    ("set_active", {"op": "set_active"},
     lambda s, p: p.SetActive(False),
     lambda l, r: setattr(r, "active", False) or True),
    ("clear_active", {"op": "clear_active"},
     lambda s, p: p.ClearActive(),
     lambda l, r: r.ClearActive() or True),
    ("set_instanceable", {"op": "set_instanceable"},
     lambda s, p: p.SetInstanceable(True),
     lambda l, r: setattr(r, "instanceable", True) or True),
    ("set_property_order", {"op": "set_property_order"},
     lambda s, p: p.SetPropertyOrder(["height"]),
     lambda l, r: setattr(r, "propertyOrder", ["height"]) or True),
    ("set_prim_order", {"op": "set_prim_order", "order": ["Garden", "Rock"]},
     lambda s, p: p.SetChildrenReorder(["Garden", "Rock"]) or True,
     lambda l, r: setattr(r, "nameChildrenOrder", ["Garden", "Rock"]) or True),
    ("clear_prim_order", {"op": "clear_prim_order"},
     lambda s, p: p.ClearChildrenReorder() or True,
     lambda l, r: r.ClearInfo("primOrder") or True),
    # Composition arcs.
    ("add_reference", {"op": "add_reference"},
     lambda s, p: p.GetReferences().AddInternalReference("/Rock"),
     lambda l, r: r.referenceList.Prepend(Sdf.Reference("", "/Rock")) or True),
    ("set_references", {"op": "set_references"},
     lambda s, p: p.SetMetadata("references", ref_op()),
     lambda l, r: r.SetInfo("references", ref_op()) or True),
    ("add_payload", {"op": "add_payload"},
     lambda s, p: p.GetPayloads().AddInternalPayload("/Rock"),
     lambda l, r: r.payloadList.Prepend(Sdf.Payload("", "/Rock")) or True),
    ("add_inherit", {"op": "add_inherit"},
     lambda s, p: p.GetInherits().AddInherit("/Rock"),
     lambda l, r: r.inheritPathList.Prepend("/Rock") or True),
    ("add_specialize", {"op": "add_specialize"},
     lambda s, p: p.GetSpecializes().AddSpecialize("/Rock"),
     lambda l, r: r.specializesList.Prepend("/Rock") or True),
    # Properties of the pseudo-root.
    ("create_attribute", {"op": "create_attribute"},
     lambda s, p: p.CreateAttribute("height", Sdf.ValueTypeNames.Double).IsValid(),
     lambda l, r: bool(Sdf.AttributeSpec(r, "height", Sdf.ValueTypeNames.Double))),
    ("create_relationship", {"op": "create_relationship"},
     lambda s, p: p.CreateRelationship("height").IsValid(),
     lambda l, r: bool(Sdf.RelationshipSpec(r, "height"))),
    ("remove_property", {"op": "remove_spec", "property": True},
     lambda s, p: p.RemoveProperty("height"),
     None),
    ("set_default", {"op": "set_default"},
     lambda s, p: height(p).Set(2.0),
     None),
    ("set_time_sample", {"op": "set_time_sample"},
     lambda s, p: height(p).Set(2.0, 12.0),
     None),
    ("block_attribute", {"op": "block_attribute"},
     lambda s, p: height(p).Block() or True,
     None),
    ("clear_default", {"op": "clear_default"},
     lambda s, p: height(p).ClearDefault(),
     None),
    ("remove_time_sample", {"op": "remove_time_sample"},
     lambda s, p: height(p).ClearAtTime(12.0),
     None),
    ("set_property_metadata", {"op": "set_property_metadata"},
     lambda s, p: height(p).SetMetadata("documentation", "How tall"),
     None),
    ("clear_property_metadata", {"op": "clear_property_metadata"},
     lambda s, p: height(p).ClearMetadata("documentation"),
     None),
    ("set_targets", {"op": "set_targets"},
     lambda s, p: rel(p).SetTargets(["/Rock"]),
     None),
    ("clear_targets", {"op": "clear_targets"},
     lambda s, p: rel(p).ClearTargets(True),
     None),
]


def attempt(call, *args):
    try:
        return call(*args) is not False
    except (Tf.ErrorException, ValueError):
        return False


def record(layer, before, key, accepted, stage=None):
    text = layer.ExportToString()
    out = {"accepted": accepted, "changed": text != before}
    if out["changed"]:
        reread = Sdf.Layer.CreateAnonymous(".usda")
        try:
            out["reads_back"] = bool(reread.ImportFromString(text))
        except Tf.ErrorException:
            out["reads_back"] = False
        out["root_prim_order"] = list(layer.rootPrimOrder)
        out["prim_fields"] = [k for k in PRIM_FIELDS if layer.pseudoRoot.HasInfo(k)]
        if key is not None:
            value = layer.pseudoRoot.GetInfo(key) if layer.pseudoRoot.HasInfo(key) else None
            out["field"] = value
    if stage is not None:
        out["root_prims"] = [str(p.GetName()) for p in stage.GetPseudoRoot().GetChildren()]
    return out


def fresh(out):
    layer = Sdf.Layer.CreateAnonymous(".usda")
    with open(os.path.join(out, "scene.usda")) as f:
        layer.ImportFromString(f.read())
    return layer


def cases(out):
    recorded = []
    for name, edit, usd_call, sdf_call in CASES:
        key = edit.get("key")
        case = {"name": name, "edit": edit, "usd": None, "sdf": None}
        if usd_call is not None:
            layer = fresh(out)
            stage = Usd.Stage.Open(layer)
            before = layer.ExportToString()
            accepted = attempt(usd_call, stage, stage.GetPseudoRoot())
            case["usd"] = record(layer, before, key, accepted, stage)
        if sdf_call is not None:
            layer = fresh(out)
            before = layer.ExportToString()
            accepted = attempt(sdf_call, layer, layer.pseudoRoot)
            case["sdf"] = record(layer, before, key, accepted)
        recorded.append(case)
    return recorded


def write(out):
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, "scene.usda"), "w") as f:
        f.write(SCENE)
    _, minor, patch = Usd.GetVersion()
    oracle = {"openusd_version": f"{minor}.{patch}", "cases": cases(out)}
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
        for name in ("scene.usda", "oracle.json"):
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")


if __name__ == "__main__":
    main()
