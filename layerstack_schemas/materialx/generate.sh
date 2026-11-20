#!/bin/bash
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT

set -euo pipefail

if [[ $# -gt 1 ]] || [[ $# -eq 1 && $1 != --check ]]; then
    echo "Usage: bash $0 [--check]" >&2
    exit 2
fi

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
output=layerstack_schemas/src/materialx/standard.rs
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

cargo run --locked -p layerstack_schemagen -- \
    --materialx layerstack_schemas/materialx/standard.mtlx \
    --node ND_image_color3 --node ND_open_pbr_surface_surfaceshader \
    --out "$tmp/generated.rs"

{
    cat <<'HEADER'
// Copyright Contributors to the MaterialX Project
// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0

HEADER
    cat "$tmp/generated.rs"
} > "$tmp/standard.rs"

if [[ "${1:-}" == --check ]]; then
    diff -u "$output" "$tmp/standard.rs"
else
    cp "$tmp/standard.rs" "$output"
fi
