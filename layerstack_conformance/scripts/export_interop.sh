#!/bin/bash
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Optional, independent compatibility gate for exporter output.
#
# Writes the fixtures from `layerstack_conformance::export_fixtures`
# (identifiers, value types, metadata, mesh scenes, materials, packages) as
# USDA, USDC and generic and ARKit-profile USDZ, and checks them with tools
# that share no code with Layerstack:
#
#   - usdcat                  every fixture must open, and prints the same
#                             text for a document's USDC as for its USDA;
#   - usdchecker              default validators, then `--arkit` (which adds
#                             the USDZ package validators such as byte
#                             alignment and member file types);
#   - python3 zipfile         archive layout (check_usdz_layout.py), plus the
#                             ARKit profile (a single USDC root layer);
#   - usdcat | grep           texture color spaces as OpenUSD reads them:
#                             sRGB for base color, raw for data and normals;
#   - usdrecord (optional)    renders the ARKit and generic cube packages
#                             through a camera in a wrapper layer (the images
#                             must be identical), and the two-material and
#                             textured cubes in both profiles, whose pixels
#                             must show each material's hues
#                             (render_check.py).
#
# The `export_interop` test then compares each USDC with OpenUSD's own USDC
# for the same USDA (`usdcat -o`), decoded structurally.
#
# Negative controls (a package with a missing asset, a normal map read as
# sRGB, a Python-written archive with unaligned data, and a USDA-root
# package checked against the ARKit profile) must be rejected, proving the selected validators detect
# those problems. Tool versions and the selected validator rules are
# recorded in the report.
#
# Not part of CI and not a build dependency. Usage:
#
#   layerstack_conformance/scripts/export_interop.sh [OUT_DIR]
#
# Exit status is non-zero if any expectation is not met or a tool is missing.

set -u
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="${1:-$root/target/export-interop}"
report="$out/report.txt"
mkdir -p "$out"
: > "$report"

log() { echo "$*" | tee -a "$report"; }

for t in usdcat usdchecker python3 cargo; do
    if ! command -v "$t" > /dev/null; then
        log "missing tool: $t"
        exit 2
    fi
done

log "== tools"
log "usdcat:     $(command -v usdcat) ($(usdcat --version 2>&1 | head -1))"
log "usdchecker: $(command -v usdchecker) ($(usdchecker --version 2>&1 | head -1))"
if command -v usdrecord > /dev/null; then
    log "usdrecord:  $(command -v usdrecord) ($(usdrecord --version 2>&1 | head -1))"
else
    log "usdrecord:  not found (render check skipped)"
fi
log "python3:    $(python3 --version 2>&1)"
log "host:       $(uname -srm)"
log "commit:     $(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)"

log "== fixtures"
listing="$(cd "$root" && cargo run -q -p layerstack_conformance --example write_export_fixtures -- "$out")" || {
    log "fixture generation failed"
    exit 1
}
log "== validator selection"
usdchecker --dumpRules "$out/package.usdz" > "$out/rules-default.txt" 2>&1 || true
usdchecker --arkit --dumpRules "$out/package.usdz" > "$out/rules-arkit.txt" 2>&1 || true
log "default rules: $(grep -c '^\[' "$out/rules-default.txt") (see rules-default.txt)"
log "--arkit rules: $(grep -c '^\[' "$out/rules-arkit.txt") (see rules-arkit.txt)"

# Python-written control: stored entries without alignment padding.
python3 - "$out/control_misaligned.usdz" <<'EOF'
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w", zipfile.ZIP_STORED) as z:
    z.writestr("root.usda", '#usda 1.0\n(\n    defaultPrim = "Root"\n    metersPerUnit = 1\n    upAxis = "Z"\n)\n\ndef Xform "Root"\n{\n}\n')
    z.writestr("textures/a.png", b"\x89PNG\r\n\x1a\n")
EOF
listing="$listing
invalid:ByteMisalignment $out/control_misaligned.usdz"

failures=0
fail() { log "FAIL: $*"; failures=$((failures + 1)); }

run() { # name, command...; logs the outcome, returns the command's status
    local name="$1"; shift
    local output status
    output="$("$@" 2>&1)"; status=$?
    printf '%s\n' "$output" > "$out/$name.log"
    return $status
}

layout="$root/layerstack_conformance/scripts/check_usdz_layout.py"
while read -r expect path; do
    [ -n "$path" ] || continue
    base="$(basename "$path")"
    log "-- $base ($expect)"
    case "$expect" in valid|valid-arkit)
        run "$base.usdcat" usdcat "$path" && log "   usdcat: ok" || fail "usdcat rejected $base (see $base.usdcat.log)"
        run "$base.usdchecker" usdchecker "$path" && log "   usdchecker: ok" || fail "usdchecker rejected $base"
        run "$base.usdchecker-arkit" usdchecker --arkit "$path" && log "   usdchecker --arkit: ok" || fail "usdchecker --arkit rejected $base"
        case "$path" in *.usdz)
            run "$base.zipfile" python3 "$layout" "$path" \
                && log "   zipfile layout: ok" || fail "zipfile layout check rejected $base";;
        esac
        if [ "$expect" = valid-arkit ]; then
            run "$base.zipfile-arkit" python3 "$layout" --arkit "$path" \
                && log "   zipfile ARKit profile (single USDC root): ok" || fail "ARKit profile check rejected $base"
        fi
        ;;
    *)
        validator="${expect#invalid:}"
        run "$base.usdchecker-arkit" usdchecker --arkit "$path"
        if grep -q "$validator" "$out/$base.usdchecker-arkit.log"; then
            log "   usdchecker --arkit reports $validator: ok (control rejected)"
        else
            fail "usdchecker --arkit did not report $validator for control $base"
        fi
        if [ "$validator" = ByteMisalignment ]; then
            run "$base.zipfile" python3 "$layout" --expect-invalid "$path" \
                && log "   zipfile layout: rejected as expected" || fail "zipfile layout check accepted control $base"
        fi
        ;;
    esac
done <<< "$listing"

log "== ARKit profile control"
run "control_usda_root.zipfile-arkit" python3 "$layout" --arkit --expect-invalid "$out/cube.usdz" \
    && log "   USDA-root cube.usdz rejected by the ARKit profile check: ok" \
    || fail "ARKit profile check accepted the USDA-root cube.usdz"

log "== USDC vs USDA through usdcat"
for usdc in "$out"/*.usdc; do
    stem="${usdc%.usdc}"
    base="$(basename "$stem")"
    [ -f "$stem.usda" ] || continue
    usdcat "$usdc" > "$stem.usdc.txt" 2>&1
    usdcat "$stem.usda" > "$stem.usda.txt" 2>&1
    if cmp -s "$stem.usdc.txt" "$stem.usda.txt"; then
        log "   $base: same text"
    else
        fail "$base: usdcat prints different text for the USDC and the USDA"
    fi
done

log "== USDC vs OpenUSD's USDC (structural)"
if (cd "$root" && cargo test -q -p layerstack_conformance --test export_interop) > "$out/export_interop_test.log" 2>&1; then
    log "   export_interop test: ok"
else
    fail "export_interop test failed (see export_interop_test.log)"
fi

log "== color spaces (material_textured.usdz, material_textured_arkit.usdz)"
for package in material_textured material_textured_arkit; do
    usdcat "$out/$package.usdz" > "$out/$package.usdcat.usda" 2>&1
    spaces="$(grep -o 'inputs:file = @[^@]*@\|sourceColorSpace = "[A-Za-z]*"' "$out/$package.usdcat.usda" | paste -d' ' - - | sed 's/inputs:file = //')"
    printf '%s\n' "$spaces" | sed "s/^/   $package: /" | tee -a "$report"
    for expected in '@textures/albedo.png@ sourceColorSpace = "sRGB"' \
                    '@textures/orm.png@ sourceColorSpace = "raw"' \
                    '@textures/ridges_normal.png@ sourceColorSpace = "raw"'; do
        printf '%s\n' "$spaces" | grep -qF "$expected" || fail "$package: expected $expected"
    done
done

log "== render"
if command -v usdrecord > /dev/null; then
    render() { # fixture, hues
        local name="render_${1%.usdz}"
        if run "$name" python3 "$root/layerstack_conformance/scripts/render_check.py" "$out/$1" "$out/$name.png" "$2"; then
            log "   $1: $(tail -1 "$out/$name.log")"
        else
            fail "render of $1 lacks $2 (see $name.log, $name.png)"
        fi
    }
    render material_partition.usdz red,blue
    render material_partition_arkit.usdz red,blue
    render material_textured.usdz orange,teal
    render material_textured_arkit.usdz orange,teal
    for package in cube_arkit cube; do
        cat > "$out/render_$package.usda" <<EOF
#usda 1.0
(
    subLayers = [
        @$package.usdz@
    ]
    metersPerUnit = 1
    upAxis = "Z"
)

def Camera "Cam"
{
    float focalLength = 35
    matrix4d xformOp:transform = ( (0.70710678, 0.70710678, 0, 0), (-0.30151134, 0.30151134, 0.90453403, 0), (0.63960215, -0.63960215, 0.42640143, 0), (3, -3, 2.5, 1) )
    uniform token[] xformOpOrder = ["xformOp:transform"]
}
EOF
        if run "render_$package" usdrecord --camera /Cam --imageWidth 640 \
            "$out/render_$package.usda" "$out/render_$package.png" && [ -s "$out/render_$package.png" ]; then
            log "   $package: rendered render_$package.png"
        else
            fail "usdrecord could not render $package.usdz (see render_$package.log)"
        fi
    done
    if cmp -s "$out/render_cube_arkit.png" "$out/render_cube.png"; then
        log "   ARKit (USDC root) and generic (USDA root) renders are identical"
    else
        fail "the ARKit and generic cube renders differ"
    fi
else
    log "   skipped: usdrecord is not on PATH"
fi

log "== result: $failures failure(s); logs in $out"
[ "$failures" -eq 0 ]
