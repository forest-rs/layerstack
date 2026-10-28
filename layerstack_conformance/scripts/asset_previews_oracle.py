# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""`UsdMediaAssetPreviewsAPI` default thumbnails and
`GetAssetDefaultPreviews` on scene.usda.

Reads need the API applied and an asset-valued `defaultImage`. Setting
writes the edit target's `previews:thumbnails:default` dictionary whole, so
a weaker sibling key still composes in; clearing removes the local entry
(and the emptied `assetInfo`). The edits are authored in a root layer over
scene.usda. `GetAssetDefaultPreviews` opens a stage masked to
`<defaultPrim>/__No_Such_Prim__`."""
import json
import sys
from pathlib import Path
from pxr import Sdf, Usd, UsdMedia

here = Path(__file__).resolve().parents[1] / "fixtures" / "asset_previews"
scene_path = str(here / "scene.usda")


def plain(value):
    if isinstance(value, dict):
        return {k: plain(v) for k, v in value.items()}
    if isinstance(value, Sdf.AssetPath):
        return {"asset": value.path}
    return value


def thumbnail(api):
    thumbs = api.GetDefaultThumbnails()
    return thumbs.defaultImage.path if thumbs else None


stage = Usd.Stage.Open(scene_path)
reads = [{"prim": p, "default_image": thumbnail(UsdMedia.AssetPreviewsAPI(stage.GetPrimAtPath(p)))}
         for p in ["/Asset", "/Unapplied", "/StringImage"]]

weak = Sdf.Layer.FindOrOpen(scene_path)
root = Sdf.Layer.CreateAnonymous()
root.subLayerPaths = [weak.identifier]
edit_stage = Usd.Stage.Open(root)
edit_stage.SetEditTarget(root)
edits = []
for call, prim_path, image in [
    ("set", "/Asset", "./strong_thumb.png"),
    ("clear", "/Asset", None),
    ("set", "/Unapplied", "./set.png"),
    ("clear", "/Unapplied", None),
]:
    prim = edit_stage.GetPrimAtPath(prim_path)
    api = UsdMedia.AssetPreviewsAPI(prim)
    if call == "set":
        api.SetDefaultThumbnails(UsdMedia.AssetPreviewsAPI.Thumbnails(Sdf.AssetPath(image)))
    else:
        api.ClearDefaultThumbnails()
    spec = root.GetPrimAtPath(prim_path)
    edits.append({
        "call": call,
        "prim": prim_path,
        "image": image,
        "local_asset_info": plain(spec.GetInfo("assetInfo")) if spec and spec.HasInfo("assetInfo") else None,
        "composed_default": plain(prim.GetAssetInfoByKey("previews:thumbnails:default")),
        "default_image": thumbnail(api),
    })

previews = UsdMedia.AssetPreviewsAPI.GetAssetDefaultPreviews(scene_path)
result = {
    "version": ".".join(map(str, Usd.GetVersion()[1:])),
    "reads": reads,
    "edits": edits,
    "asset_default_previews": {
        "prim": str(previews.GetPrim().GetPath()),
        "default_image": thumbnail(previews),
        "populated": [str(p.GetPath()) for p in previews.GetPrim().GetStage().Traverse()],
    },
}
text = json.dumps(result, indent=2) + "\n"
dest = here / "oracle.json"
if "--check" in sys.argv:
    assert dest.read_text() == text, "asset previews oracle differs"
else:
    dest.write_text(text)
