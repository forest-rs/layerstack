# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Prim and property metadata by dictionary key path, call by call:
`SetMetadataByDictKey` / `SetCustomDataByKey` / `SetAssetInfoByKey` and
`ClearMetadataByDictKey` / `ClearCustomDataByKey`, authored in a root layer
over weak.usda. After each call: whether C++ accepts it, the root layer's
`customData` and `assetInfo` fields of the object, and its composed
`customData`. Clearing the last entry removes the field; a weaker layer's
dictionary keeps composing under a by-key edit."""
import json
import sys
from pathlib import Path
from pxr import Sdf, Usd

here = Path(__file__).resolve().parents[1] / "fixtures" / "metadata_by_key"
weak = Sdf.Layer.FindOrOpen(str(here / "weak.usda"))
root = Sdf.Layer.CreateAnonymous()
root.subLayerPaths = [weak.identifier]
stage = Usd.Stage.Open(root)
stage.SetEditTarget(root)


def plain(value):
    if isinstance(value, dict):
        return {k: plain(v) for k, v in value.items()}
    if isinstance(value, Sdf.AssetPath):
        return {"asset": value.path}
    return value


def obj(path):
    return stage.GetObjectAtPath(path)


def local(path, field):
    spec = root.GetObjectAtPath(path)
    if not spec or not spec.HasInfo(field):
        return None
    return plain(spec.GetInfo(field))


calls = [
    ("set", "/Q", "customData", "a:b:c", 1),
    ("set", "/Q", "customData", "a:d", "x"),
    ("set", "/Q", "assetInfo", "identifier", Sdf.AssetPath("x.usda")),
    ("set", "/Q.y", "customData", "k:j", "v"),
    ("clear", "/Q", "customData", "a:b:c", None),
    ("clear", "/Q", "customData", "missing", None),
    ("clear", "/Q", "customData", "a:d", None),
    ("clear", "/Q.y", "customData", "k:j", None),
    ("set", "/W", "customData", "shared:y", 2),
    ("set", "/W", "customData", "w", 3),
    ("clear", "/W", "customData", "w", None),
    # C++ accepts both of these and authors junk: a dictionary in the
    # string `documentation` field (written as `doc = ""`), and an entry
    # named "" under `a`. Layerstack refuses them without authoring.
    ("set", "/Q", "documentation", "a", "x"),
    ("set", "/Q", "customData", "a::b", "x"),
]
records = []
for call, path, field, key_path, value in calls:
    try:
        if call == "set":
            accepted = obj(path).SetMetadataByDictKey(field, key_path, value)
        else:
            accepted = obj(path).ClearMetadataByDictKey(field, key_path)
        accepted = bool(accepted)
    except Exception:
        accepted = False
    records.append({
        "call": call,
        "path": path,
        "field": field,
        "key_path": key_path,
        "value": plain(value),
        "accepted": accepted,
        "local_documentation": local(path, "documentation"),
        "local_custom_data": local(path, "customData"),
        "local_asset_info": local(path, "assetInfo"),
        "composed_custom_data": plain(obj(path).GetCustomData()),
        "composed_entry": plain(obj(path).GetMetadataByDictKey(field, key_path))
        if field != "documentation" else None,
    })
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "calls": records}
text = json.dumps(result, indent=2) + "\n"
dest = here / "oracle.json"
if "--check" in sys.argv:
    assert dest.read_text() == text, "metadata by key oracle differs"
else:
    dest.write_text(text)
