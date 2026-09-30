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
void emit(double value) { std::cout << value; }
void emit(VtDoubleArray const &value) {
    std::cout << '[';
    for (size_t i=0; i<value.size(); ++i) { if (i) std::cout << ','; emit(value[i]); }
    std::cout << ']';
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

void emit(VtValue const &value) {
    if (value.IsHolding<VtFloatArray>()) emit(value.UncheckedGet<VtFloatArray>());
    else if (value.IsHolding<VtDoubleArray>()) emit(value.UncheckedGet<VtDoubleArray>());
    else if (value.IsHolding<VtVec3fArray>()) emit(value.UncheckedGet<VtVec3fArray>());
    else if (value.IsHolding<VtArray<SdfTimeCode>>()) emit(value.UncheckedGet<VtArray<SdfTimeCode>>());
    else if (value.IsHolding<float>()) emit(value.UncheckedGet<float>());
    else if (value.IsHolding<double>()) emit(value.UncheckedGet<double>());
    else std::cout << "null";
}
char const *kind(VtValue const &value) {
    if (value.IsHolding<VtFloatArray>()) return "float[]";
    if (value.IsHolding<VtDoubleArray>()) return "double[]";
    if (value.IsHolding<VtVec3fArray>()) return "float3[]";
    if (value.IsHolding<VtArray<SdfTimeCode>>()) return "timecode[]";
    if (value.IsHolding<float>()) return "float";
    if (value.IsHolding<double>()) return "double";
    return "unsupported";
}
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
    if (std::string(path).rfind("/Sparse", 0)==0) {
        VtValue raw;
        std::cout << ",\"rawDefault\":";
        if (attr.Get(&raw, UsdTimeCode::Default())) {
            emit(raw); std::cout << ",\"rawDefaultKind\":\"" << kind(raw) << "\"";
        } else std::cout << "null";
        std::cout << ",\"rawNumeric\":";
        if (attr.Get(&raw, UsdTimeCode(9))) {
            emit(raw); std::cout << ",\"rawNumericKind\":\"" << kind(raw) << "\"";
        } else std::cout << "null";
    }
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
    stage->GetRootLayer()->SetField(SdfPath("/SparseMisdeclaredEdit.value"), TfToken("typeName"), VtValue(TfToken("double[]")));
    weakLayer->SetField(SdfPath("/SparseMisdeclaredWeakEdit.value"), TfToken("typeName"), VtValue(TfToken("float[]")));
    weakLayer->SetField(SdfPath("/SparseWrongEditUpper.value"), TfToken("timeSamples"),
        VtValue(SdfTimeSampleMap{{0, weakLayer->GetField(SdfPath("/SparseWrongEditUpper.value"), TfToken("default"))}, {10, weakLayer->GetField(SdfPath("/SparseMixedEdits.value"), TfToken("default"))}}));
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
    record<VtArray<SdfTimeCode>>(stage,"/SparseTimeResize.value",first);
    record<VtArray<SdfTimeCode>>(stage,"/SparseTimeLiteral.value",first);
    for (char const *path : {"/SparseCompatibleBase.value", "/SparseSampledBase.value",
                            "/SparseScalarBase.value", "/SparseWrongDense.value",
                            "/SparseWrongLower.value", "/SparseWrongUpper.value",
                            "/SparseFillDeclaration.value", "/SparseMixedEdits.value",
                            "/SparseMisdeclaredEdit.value", "/SparseMisdeclaredWeakEdit.value",
                            "/SparseDeclaration.value", "/SparseConnection.value", "/SparseContribution.value",
                            "/SparseWrongEditUpper.value", "/SparseEmptySamples.value",
                            "/SparseEmptyDeclaration.value"})
        record<VtFloatArray>(stage,path,first);
    record<GfVec3f>(stage,"/Vector.value",first);
    record<GfVec3f>(stage,"/EmptySamplesXform.xformOp:translate",first);
    record<VtVec3fArray>(stage,"/Array.value",first);
    record<VtVec3fArray>(stage,"/SparseResizeKind.value",first);
    record<VtVec3fArray>(stage,"/SparseDeleteKind.value",first);
    record<GfMatrix4d>(stage,"/Matrix.value",first);
    record<SdfTimeCode>(stage,"/Time.value",first);
    record<SdfAssetPath>(stage,"/Asset.value",first);
    record<SdfPathExpression>(stage,"/Expression.value",first);
    std::cout << "\n}\n";
}
