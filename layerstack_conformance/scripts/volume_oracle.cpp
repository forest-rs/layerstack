// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Exercise the actual UsdVol C++ helpers, including methods without Python bindings.
#include <iostream>
#include "pxr/usd/usd/stage.h"
#include "pxr/usd/usdVol/volume.h"
#include "pxr/base/js/json.h"
#include "pxr/base/js/value.h"
PXR_NAMESPACE_USING_DIRECTIVE
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    auto stage = UsdStage::Open(argv[1]);
    if (!stage) return 3;
    UsdVolVolume volume(stage->GetPrimAtPath(SdfPath("/Volume")));
    JsObject fields;
    for (const auto &[name, path] : volume.GetFieldPaths())
        fields[name.GetString()] = JsValue(path.GetString());
    JsArray queries;
    for (const auto &name : {"density", "field:density", "forward", "duplicate", "multiple",
            "empty", "missing", "property", "cycle", "cycleWithExit", "emptyForward",
            "a:density", "z:density", "attribute", "absent"})
        queries.emplace_back(JsObject{{"name", JsValue(std::string(name))},
            {"exists", JsValue(volume.HasFieldRelationship(TfToken(name)))},
            {"path", JsValue(volume.GetFieldPath(TfToken(name)).GetString())}});
    JsArray edits;
    auto record = [&](const std::string &name, bool result) {
        auto rel = volume.GetPrim().GetRelationship(TfToken("field:" + name));
        SdfPathVector paths;
        if (rel) rel.GetTargets(&paths);
        JsArray targets;
        for (const auto &p : paths) targets.emplace_back(p.GetString());
        edits.emplace_back(JsObject{{"name", JsValue(name)}, {"result", JsValue(result)},
            {"exists", JsValue(bool(rel))}, {"custom", JsValue(rel && rel.IsCustom())},
            {"targets", JsValue(targets)}});
    };
    record("new", volume.CreateFieldRelationship(TfToken("new"), SdfPath("/Field")));
    record("new", volume.CreateFieldRelationship(TfToken("field:new"), SdfPath("/Other")));
    record("new", volume.BlockFieldRelationship(TfToken("new")));
    record("absent", volume.BlockFieldRelationship(TfToken("absent")));
    volume.GetPrim().CreateRelationship(TfToken("field:noncustom"), false);
    record("noncustom", volume.CreateFieldRelationship(TfToken("noncustom"), SdfPath("/Field")));
    record("forwarded", volume.CreateFieldRelationship(TfToken("forwarded"), SdfPath("/Links.chain")));
    record("root", volume.CreateFieldRelationship(TfToken("root"), SdfPath("/")));
    std::cout << JsWriteToString(JsValue(JsObject{{"version", JsValue(std::to_string(PXR_MINOR_VERSION) + "." + std::to_string(PXR_PATCH_VERSION))},
        {"fields", JsValue(fields)}, {"queries", JsValue(queries)}, {"edits", JsValue(edits)}})) << '\n';
}
