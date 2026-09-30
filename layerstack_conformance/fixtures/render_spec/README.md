# Render-spec comparison

`oracle.json` records OpenUSD **26.8** (`PXR_VERSION=2608`) through the public
C++ `UsdRenderComputeSpec` and `UsdShadeMaterialBindingAPI::ComputeBoundMaterial`
APIs. These are actual computations, not a Python translation of their algorithms.
The render-spec computation is not exposed by the usd-core Python wheel.

The scene checks settings-to-product inheritance, authored blocks and sampled-only
product overrides, incompatible default values, the five aperture/pixel-aspect
conform policies, invalid dimensions and cameras, ordered shared render vars,
relationship forwarding and cycles, output providers through a node graph,
and three namespace filters. Separate bindings verify that the shared forwarding
helper retains a valid material branch next to a cycle, terminates an empty cycle,
and rejects a missing material.

Build `../../scripts/render_spec_oracle.cpp` against a matching OpenUSD SDK.
For example, from the workspace root, with paths to the SDK/source tree,
the configured `pxr/pxr.h` include directory, and the USD shared library:

```sh
c++ -std=c++17 \
  -I"$OPENUSD_INCLUDE" -I"$OPENUSD_SOURCE" \
  $(python3-config --includes) \
  layerstack_conformance/scripts/render_spec_oracle.cpp \
  "$OPENUSD_LIBRARY" $(python3-config --embed --ldflags) \
  -o .local/render-spec-oracle
python layerstack_conformance/scripts/render_spec_oracle.py \
  --binary .local/render-spec-oracle --check
```

The compiler must also find the SDK's TBB headers and the runtime must find its
shared libraries. When using wheel libraries directly, set `LD_LIBRARY_PATH`
(on Linux) or `DYLD_LIBRARY_PATH` (on macOS) to their directory. Omit `--check`
to regenerate the JSON after deliberately updating the fixture or reference version.

The golden result matches the C++ implementation, including its current omission
of the deprecated `instantaneousShutter` field: `disableMotionBlur` controls the
computed result. The fixture authors both fields to preserve that distinction.
The API resolves configuration; it does not render images, execute shaders or
translate settings to a renderer. Unrecognized conform-policy tokens are retained
without adjusting the aperture. Cyclic forwarding branches are skipped while
valid branches remain usable. Products with invalid cameras are omitted; invalid
render vars are omitted and reported by Layerstack's typed diagnostics.
