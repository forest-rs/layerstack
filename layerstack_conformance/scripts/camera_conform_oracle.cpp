// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Call OpenUSD's `CameraUtilConformWindow` on a `GfCamera` (`cameraUtil` is
// an imaging library, not in the Python wheel). Reads cases from stdin, one
// per line: projection (0 perspective, 1 orthographic), the 16 numbers of
// the camera's transform row by row, horizontal and vertical aperture and
// their offsets, focal length, near, far, the policy (0 MatchVertically,
// 1 MatchHorizontally, 2 Fit, 3 Crop, 4 DontConform) and the target aspect.
// Writes one JSON object per case: the conformed apertures, the frustum's
// window and its view and projection matrices.
#include <cstdio>
#include <iostream>
#include "pxr/base/gf/camera.h"
#include "pxr/base/gf/frustum.h"
#include "pxr/base/gf/matrix4d.h"
#include "pxr/base/gf/range1f.h"
#include "pxr/imaging/cameraUtil/conformWindow.h"

PXR_NAMESPACE_USING_DIRECTIVE

static void matrix(const GfMatrix4d &m) {
    std::printf("[");
    for (int row = 0; row < 4; ++row) {
        std::printf("%s[%.17g, %.17g, %.17g, %.17g]", row ? ", " : "", m[row][0], m[row][1],
                    m[row][2], m[row][3]);
    }
    std::printf("]");
}

int main() {
    int projection;
    while (std::cin >> projection) {
        double t[16];
        for (double &v : t) std::cin >> v;
        double h, v, ho, vo, focal, near, far, aspect;
        int policy;
        std::cin >> h >> v >> ho >> vo >> focal >> near >> far >> policy >> aspect;
        GfMatrix4d transform(t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7], t[8], t[9], t[10],
                             t[11], t[12], t[13], t[14], t[15]);
        GfCamera camera(transform,
                        projection ? GfCamera::Orthographic : GfCamera::Perspective,
                        float(h), float(v), float(ho), float(vo), float(focal),
                        GfRange1f(float(near), float(far)));
        CameraUtilConformWindow(&camera, CameraUtilConformWindowPolicy(policy), aspect);
        const GfFrustum frustum = camera.GetFrustum();
        const GfRange2d &window = frustum.GetWindow();
        std::printf("{\"aperture\": [%.9g, %.9g], \"window\": [%.17g, %.17g, %.17g, %.17g], ",
                    camera.GetHorizontalAperture(), camera.GetVerticalAperture(),
                    window.GetMin()[0], window.GetMin()[1], window.GetMax()[0],
                    window.GetMax()[1]);
        std::printf("\"view\": ");
        matrix(frustum.ComputeViewMatrix());
        std::printf(", \"projection\": ");
        matrix(frustum.ComputeProjectionMatrix());
        std::printf("}\n");
    }
    return 0;
}
