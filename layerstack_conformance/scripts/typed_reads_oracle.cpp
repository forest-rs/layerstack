// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Typed UsdAttribute::Get<T> is not Python's VtValue-returning Get().
// Recorded with OpenUSD 26.08. Build with an installed OpenUSD:
// c++ -std=c++17 typed_reads_oracle.cpp -I "$USD_PREFIX/include" \
//     -L "$USD_PREFIX/lib" -lusd_ms -o typed_reads_oracle
// Run with fixtures/typed_reads/scene.usda; stdout is oracle.json.
// A Python-enabled USD build may also require its Python library at link time.
#include "pxr/usd/usd/stage.h"
#include "pxr/usd/usd/attribute.h"
#include "pxr/usd/sdf/assetPath.h"
#include "pxr/usd/sdf/pathExpression.h"
#include "pxr/usd/sdf/timeCode.h"
#include "pxr/usd/sdf/layer.h"
#include "pxr/base/vt/dictionary.h"
#include "pxr/base/gf/vec3f.h"
#include "pxr/base/gf/matrix4d.h"
#include "pxr/base/vt/array.h"
#include <iostream>
#include <iomanip>
PXR_NAMESPACE_USING_DIRECTIVE

void emit(float value) { std::cout << value; }
void emit(GfVec3f const &value) {
    std::cout << '[' << value[0] << ',' << value[1] << ',' << value[2] << ']';
}
void emit(VtFloatArray const &value) {
    std::cout << '[';
    for (size_t i=0; i<value.size(); ++i) {
        if (i) std::cout << ',';
        emit(value[i]);
    }
    std::cout << ']';
}
void emit(VtArray<SdfTimeCode> const &value) {
    std::cout << '[';
    for (size_t i=0; i<value.size(); ++i) {
        if (i) std::cout << ',';
        std::cout << value[i].GetValue();
    }
    std::cout << ']';
}
void emit(VtVec3fArray const &value) {
    std::cout << '[';
    for (size_t i=0; i<value.size(); ++i) {
        if (i) std::cout << ',';
        emit(value[i]);
    }
    std::cout << ']';
}
void emit(GfMatrix4d const &value) {
    std::cout << '[';
    for (int row=0; row<4; ++row) {
        if (row) std::cout << ',';
        std::cout << '[';
        for (int col=0; col<4; ++col) {
            if (col) std::cout << ',';
            std::cout << value[row][col];
        }
        std::cout << ']';
    }
    std::cout << ']';
}
void emit(SdfTimeCode const &value) { std::cout << value.GetValue(); }
void emit(SdfAssetPath const &value) { std::cout << '"' << value.GetAssetPath() << '"'; }
void emit(SdfPathExpression const &value) { std::cout << '"' << value.GetText() << '"'; }

template<class T>
void record(UsdStageRefPtr const &stage, char const *path, bool &first) {
    if (!first) std::cout << ',';
    first=false;
    auto attr=stage->GetAttributeAtPath(SdfPath(path));
    std::cout << "\n  \"" << path << "\": {\"default\":";
    T result;
    if (attr.Get(&result, UsdTimeCode::Default())) emit(result);
    else std::cout << "null";
    std::cout << ",\"numeric\":";
    if (attr.Get(&result, UsdTimeCode(9))) emit(result);
    else std::cout << "null";
    std::cout << '}';
}
int main(int argc, char **argv) {
    if (argc!=2) return 2;
    auto stage=UsdStage::Open(argv[1]);
    if (!stage) return 1;
    // These fields deliberately disagree with their declared attribute type.
    // SetField permits that authored state, which exercises actual-value
    // conversion rather than a guess from the declaration.
    stage->GetRootLayer()->SetField(SdfPath("/WrongDictionary.value"),
        TfToken("default"), VtValue(VtDictionary{{"key", VtValue(1)}}));
    std::string weak=stage->GetRootLayer()->GetSubLayerPaths()[0];
    SdfLayer::FindOrOpenRelativeToLayer(stage->GetRootLayer(), weak)->SetField(
        SdfPath("/MisdeclaredTime.value"), TfToken("default"), VtValue(SdfTimeCode(2)));
    auto weakLayer=SdfLayer::FindOrOpenRelativeToLayer(stage->GetRootLayer(), weak);
    weakLayer->SetField(SdfPath("/SparseWrongLower.value"), TfToken("timeSamples"),
        VtValue(SdfTimeSampleMap{{0, VtValue(VtDoubleArray{2})}, {10, VtValue(VtFloatArray{6})}}));
    weakLayer->SetField(SdfPath("/SparseWrongUpper.value"), TfToken("timeSamples"),
        VtValue(SdfTimeSampleMap{{0, VtValue(VtFloatArray{2})}, {10, VtValue(VtDoubleArray{6})}}));
    weakLayer->SetField(SdfPath("/MisdeclaredNumeric.value"), TfToken("default"), VtValue(SdfTimeCode(2)));
    weakLayer->SetField(SdfPath("/MisdeclaredTimeArray.value"), TfToken("default"), VtValue(VtArray<SdfTimeCode>{SdfTimeCode(2)}));
    weakLayer->SetField(SdfPath("/MisdeclaredTimeArray.value"), TfToken("timeSamples"),
        VtValue(SdfTimeSampleMap{{0, VtValue(VtArray<SdfTimeCode>{SdfTimeCode(2)})}, {10, VtValue(VtArray<SdfTimeCode>{SdfTimeCode(4)})}}));
    bool first=true;
    std::cout << std::showpoint << std::setprecision(17);
    std::cout << '{';
    for (char const *path : {"/Skip.inputs:intensity", "/Fallback.inputs:intensity",
                            "/Compatible.inputs:intensity", "/Block.inputs:intensity",
                            "/BelowBlock.inputs:intensity", "/Samples.inputs:intensity", "/NoDefault.inputs:intensity"})
        record<float>(stage,path,first);
    record<float>(stage,"/NoCompatible.value",first);
    record<float>(stage,"/WrongExpression.value",first);
    record<float>(stage,"/WrongDictionary.value",first);
    record<float>(stage,"/WrongEdit.value",first);
    record<SdfPathExpression>(stage,"/ExpressionBelow.value",first);
    record<SdfTimeCode>(stage,"/MisdeclaredTime.value",first);
    record<SdfTimeCode>(stage,"/MisdeclaredNumeric.value",first);
    record<VtArray<SdfTimeCode>>(stage,"/MisdeclaredTimeArray.value",first);
    for (char const *path : {"/SparseCompatibleBase.value", "/SparseSampledBase.value",
                            "/SparseScalarBase.value", "/SparseWrongDense.value",
                            "/SparseWrongLower.value", "/SparseWrongUpper.value"})
        record<VtFloatArray>(stage,path,first);
    record<GfVec3f>(stage,"/Vector.value",first);
    record<VtVec3fArray>(stage,"/Array.value",first);
    record<GfMatrix4d>(stage,"/Matrix.value",first);
    record<SdfTimeCode>(stage,"/Time.value",first);
    record<SdfAssetPath>(stage,"/Asset.value",first);
    record<SdfPathExpression>(stage,"/Expression.value",first);
    std::cout << "\n}\n";
}
