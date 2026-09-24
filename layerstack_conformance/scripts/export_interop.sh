#!/bin/bash
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Optional, independent compatibility gate for exporter output.
#
# Writes the fixtures from `layerstack_conformance::export_fixtures`
# (identifiers, value types, metadata, primvars, packages, materials) and
# checks them with tools that share no code with Layerstack:
#
#   - usdcat                  every fixture must open;
#   - usdchecker              default validators, then `--arkit` (which adds
#                             the USDZ package validators such as byte
#                             alignment and member file types);
#   - python3 zipfile         archive layout (check_usdz_layout.py);
#   - usdcat | grep           texture color spaces as OpenUSD reads them:
#                             sRGB for base color, raw for data and normals;
#   - usdrecord               renders of the two-material cube and the
#                             textured cube through a camera wrapper layer,
#                             whose pixels must show each material's hues
#                             (render_check.py). Skipped without usdrecord.
#
# Negative controls (a package with a missing asset, a normal map read as
# sRGB, and a Python-written archive with unaligned data) must be rejected,
# proving the selected validators detect those problems. Tool versions and
# the selected validator rules are recorded in the report.
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

while read -r expect path; do
    [ -n "$path" ] || continue
    base="$(basename "$path")"
    log "-- $base ($expect)"
    if [ "$expect" = valid ]; then
        run "$base.usdcat" usdcat "$path" && log "   usdcat: ok" || fail "usdcat rejected $base (see $base.usdcat.log)"
        run "$base.usdchecker" usdchecker "$path" && log "   usdchecker: ok" || fail "usdchecker rejected $base"
        run "$base.usdchecker-arkit" usdchecker --arkit "$path" && log "   usdchecker --arkit: ok" || fail "usdchecker --arkit rejected $base"
        case "$path" in *.usdz)
            run "$base.zipfile" python3 "$root/layerstack_conformance/scripts/check_usdz_layout.py" "$path" \
                && log "   zipfile layout: ok" || fail "zipfile layout check rejected $base";;
        esac
    else
        validator="${expect#invalid:}"
        run "$base.usdchecker-arkit" usdchecker --arkit "$path"
        if grep -q "$validator" "$out/$base.usdchecker-arkit.log"; then
            log "   usdchecker --arkit reports $validator: ok (control rejected)"
        else
            fail "usdchecker --arkit did not report $validator for control $base"
        fi
        if [ "$validator" = ByteMisalignment ]; then
            run "$base.zipfile" python3 "$root/layerstack_conformance/scripts/check_usdz_layout.py" --expect-invalid "$path" \
                && log "   zipfile layout: rejected as expected" || fail "zipfile layout check accepted control $base"
        fi
    fi
done <<< "$listing"

log "== color spaces (material_textured.usdz)"
usdcat "$out/material_textured.usdz" > "$out/material_textured.usdcat.usda" 2>&1
spaces="$(grep -o 'inputs:file = @[^@]*@\|sourceColorSpace = "[A-Za-z]*"' "$out/material_textured.usdcat.usda" | paste -d' ' - - | sed 's/inputs:file = //')"
printf '%s\n' "$spaces" | sed 's/^/   /' | tee -a "$report"
for expected in '@textures/albedo.png@ sourceColorSpace = "sRGB"' \
                '@textures/orm.png@ sourceColorSpace = "raw"' \
                '@textures/ridges_normal.png@ sourceColorSpace = "raw"'; do
    printf '%s\n' "$spaces" | grep -qF "$expected" || fail "expected $expected"
done

log "== renders"
if command -v usdrecord > /dev/null; then
    log "usdrecord:  $(command -v usdrecord)"
    render() { # fixture, hues
        local name="render_${1%.usdz}"
        if run "$name" python3 "$root/layerstack_conformance/scripts/render_check.py" "$out/$1" "$out/$name.png" "$2"; then
            log "   $1: $(tail -1 "$out/$name.log")"
        else
            fail "render of $1 lacks $2 (see $name.log, $name.png)"
        fi
    }
    render material_partition.usdz red,blue
    render material_textured.usdz orange,teal
else
    log "   skipped: usdrecord is not on PATH"
fi

log "== result: $failures failure(s); logs in $out"
[ "$failures" -eq 0 ]
