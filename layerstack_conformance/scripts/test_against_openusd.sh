#!/bin/bash
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT

# Runs every conformance test that compares with OpenUSD, given a Python
# with `pxr` in `LAYERSTACK_USD_PYTHON`:
#
#   LAYERSTACK_USD_PYTHON=python bash layerstack_conformance/scripts/test_against_openusd.sh
#
# Those tests are every test file that reads `LAYERSTACK_USD_PYTHON`, found
# by that name so that a new one runs too, with layer saves and prim field
# edits. A file in `tests/conformance/` is a module of the `conformance`
# binary and runs by its test names; one in `tests/` is its own binary. Every
# test runs even when one fails, so a failure shows all that differ.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
: "${LAYERSTACK_USD_PYTHON:?set LAYERSTACK_USD_PYTHON to a Python with pxr}"

# Each test file by name, wherever it is, once.
names=$(
    {
        grep -l LAYERSTACK_USD_PYTHON tests/*.rs tests/conformance/*.rs 2>/dev/null |
            xargs -n1 basename | sed 's/\.rs$//'
        echo layer_save
        echo prim_field_edits
    } | sort -u
)
binaries=()
modules=()
for name in $names; do
    if [ -f "tests/conformance/$name.rs" ]; then
        modules+=("$name")
    else
        binaries+=(--test "$name")
    fi
done

status=0
if [ ${#binaries[@]} -gt 0 ]; then
    cargo test --locked --no-fail-fast -p layerstack_conformance "${binaries[@]}" || status=1
fi
if [ ${#modules[@]} -gt 0 ]; then
    # Filters match substrings (`flatten::` would also run
    # `layer_stack_flatten::`), so select whole names and run them `--exact`.
    pattern="^($(IFS='|'; echo "${modules[*]}"))::"
    tests=$(cargo test --locked -p layerstack_conformance --test conformance -- --list --format terse |
        sed -n 's/: test$//p' | grep -E "$pattern") || {
        echo "no tests found in: ${modules[*]}" >&2
        exit 1
    }
    # shellcheck disable=SC2086 # one argument per test name
    cargo test --locked -p layerstack_conformance --test conformance -- --exact $tests || status=1
fi
exit $status
