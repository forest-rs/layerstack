// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// UsesFloat* is not exposed in the Python wheel: call both C++ overloads.
#include <cstdlib>
#include <iostream>
#include "pxr/usd/usd/stage.h"
#include "pxr/usd/usd/primRange.h"
#include "pxr/usd/usdVol/particleField3DGaussianSplat.h"
#include "pxr/base/js/json.h"
#include "pxr/base/js/value.h"
PXR_NAMESPACE_USING_DIRECTIVE
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    auto stage = UsdStage::Open(argv[1]);
    if (!stage) return 3;
    JsArray rows;
    for (const auto &prim : stage->Traverse()) {
        if (!prim.IsA<UsdVolParticleField3DGaussianSplat>()) continue;
        UsdVolParticleField3DGaussianSplat splat(prim);
        JsArray channels;
        auto record = [&](const char *channel, bool tokenFloat, const TfToken &token,
                         bool attrFloat, const UsdAttribute &attr) {
            if (tokenFloat != attrFloat || attr.GetName() != token) std::abort();
            channels.emplace_back(JsObject{{"channel", JsValue(std::string(channel))},
                {"uses_float", JsValue(tokenFloat)}, {"name", JsValue(token.GetString())},
                {"property", JsValue(attr.GetPath().GetString())}});
        };
        TfToken token;
        UsdAttribute attr;
#define RECORD(Name, channel) { \
        bool tf = splat.UsesFloat##Name(&token); \
        bool af = splat.UsesFloat##Name(&attr); \
        record(channel, tf, token, af, attr); }
        RECORD(Positions, "positions")
        RECORD(Orientations, "orientations")
        RECORD(Scales, "scales")
        RECORD(Opacities, "opacities")
        RECORD(RadianceCoefficients, "radiance_coefficients")
#undef RECORD
        rows.emplace_back(JsObject{{"path", JsValue(prim.GetPath().GetString())},
            {"channels", JsValue(channels)}});
    }
    std::cout << JsWriteToString(JsValue(JsObject{
        {"version", JsValue(std::to_string(PXR_MINOR_VERSION) + "." + std::to_string(PXR_PATCH_VERSION))},
        {"rows", JsValue(rows)}})) << '\n';
}
