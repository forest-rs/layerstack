// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Call OpenUSD's C++ render computation (not exposed by the Python wheel).
#include <iostream>
#include "pxr/usd/usd/stage.h"
#include "pxr/usd/usdRender/settings.h"
#include "pxr/usd/usdRender/spec.h"
#include "pxr/usd/usdShade/materialBindingAPI.h"
#include "pxr/usd/usdShade/material.h"
#include "pxr/base/js/json.h"
#include "pxr/base/js/value.h"
#include "pxr/base/vt/array.h"
#include "pxr/base/tf/stringUtils.h"

PXR_NAMESPACE_USING_DIRECTIVE

template<typename T> JsValue vector2(const T &v) {
    return JsValue(JsArray{JsValue(double(v[0])), JsValue(double(v[1]))});
}
JsValue vector2(const GfVec2i &v) {
    return JsValue(JsArray{JsValue(v[0]), JsValue(v[1])});
}
JsValue dictionary(const VtDictionary &d) {
    JsObject out;
    for (const auto &[name, value] : d) {
        if (value.IsHolding<float>()) out[name] = JsValue(double(value.UncheckedGet<float>()));
        else if (value.IsHolding<int>()) out[name] = JsValue(value.UncheckedGet<int>());
        else if (value.IsHolding<bool>()) out[name] = JsValue(value.UncheckedGet<bool>());
        else if (value.IsHolding<std::string>()) out[name] = JsValue(value.UncheckedGet<std::string>());
        else if (value.IsHolding<SdfPathVector>()) {
            JsArray paths;
            for (const auto &p : value.UncheckedGet<SdfPathVector>()) paths.emplace_back(p.GetString());
            out[name] = JsValue(paths);
        } else out[name] = JsValue(TfStringify(value));
    }
    return JsValue(out);
}
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    const auto stage = UsdStage::Open(argv[1]);
    if (!stage) return 3;
    JsArray cases;
    for (const auto &namespaces : {TfTokenVector{}, TfTokenVector{TfToken("ri")}, TfTokenVector{TfToken("ri:integrator")}}) {
        auto spec = UsdRenderComputeSpec(UsdRenderSettings(stage->GetPrimAtPath(SdfPath("/Settings"))), namespaces);
        JsArray products, vars, included, material, ns, bindings;
        for (const auto &p : spec.products) {
            JsArray indices;
            for (auto i : p.renderVarIndices) indices.emplace_back(int(i));
            const auto mn = p.dataWindowNDC.GetMin(), mx = p.dataWindowNDC.GetMax();
            products.emplace_back(JsObject{
                {"path", JsValue(p.renderProductPath.GetString())}, {"type", JsValue(p.type.GetString())},
                {"name", JsValue(p.name.GetString())}, {"camera", JsValue(p.cameraPath.GetString())},
                {"resolution", vector2(p.resolution)}, {"pixel_aspect", JsValue(double(p.pixelAspectRatio))},
                {"policy", JsValue(p.aspectRatioConformPolicy.GetString())}, {"aperture", vector2(p.apertureSize)},
                {"window", JsValue(JsArray{JsValue(double(mn[0])),JsValue(double(mn[1])),JsValue(double(mx[0])),JsValue(double(mx[1]))})},
                {"disable_motion", JsValue(p.disableMotionBlur)}, {"disable_dof", JsValue(p.disableDepthOfField)},
                {"indices", JsValue(indices)}, {"settings", dictionary(p.namespacedSettings)}});
        }
        for (const auto &v : spec.renderVars) vars.emplace_back(JsObject{
            {"path", JsValue(v.renderVarPath.GetString())}, {"data_type", JsValue(v.dataType.GetString())},
            {"source_name", JsValue(v.sourceName)}, {"source_type", JsValue(v.sourceType.GetString())},
            {"settings", dictionary(v.namespacedSettings)}});
        for (const auto &p : spec.includedPurposes) included.emplace_back(p.GetString());
        for (const auto &p : spec.materialBindingPurposes) material.emplace_back(p.GetString());
        for (const auto &n : namespaces) ns.emplace_back(n.GetString());
        for (const auto &name : {"/BindingCycle", "/BindingEmptyCycle", "/BindingMissing"}) {
            UsdRelationship relationship;
            auto material = UsdShadeMaterialBindingAPI(stage->GetPrimAtPath(SdfPath(name))).ComputeBoundMaterial(TfToken(), &relationship);
            bindings.emplace_back(JsObject{{"path", JsValue(std::string(name))},
                {"material", material ? JsValue(material.GetPath().GetString()) : JsValue()}});
        }
        cases.emplace_back(JsObject{{"namespaces",JsValue(ns)}, {"products",JsValue(products)},
            {"vars",JsValue(vars)}, {"included",JsValue(included)}, {"material",JsValue(material)},
            {"settings",dictionary(spec.namespacedSettings)}, {"bindings", JsValue(bindings)}});
    }
    std::cout << JsWriteToString(JsValue(cases)) << '\n';
}
