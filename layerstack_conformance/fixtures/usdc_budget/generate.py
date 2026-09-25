# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Generates crate files that stress the USDC reader's decode budget.

Each file is small but references one long value many times, the way
OpenUSD deduplicates values, so a reader that copied the value for each
reference would materialize far more than the file holds.
`layerstack_conformance/tests/usdc_robustness.rs` reads them and requires
what the read allocates to stay within what it charges to its
`DecodeBudget`.

- `shared_array_edit_literal.usdc`: `string[]` and `token[]` array edits
  whose instructions all reuse one 64 KiB literal: 64 appends, prepends,
  inserts and writes each, plus a `MinSize` and a `SetSize` fill. OpenUSD
  stores the literal once in the edit's literal table.

Pinned oracle: `usd-core` 26.8 from PyPI (OpenUSD v26.08), as for the
`usdc_versions` fixtures:

    python3 -m venv venv && venv/bin/pip install usd-core==26.8
    venv/bin/python layerstack_conformance/fixtures/usdc_budget/generate.py
"""

import os
import sys

from pxr import Sdf, Usd, Vt

PINNED_USD_VERSION = (0, 26, 8)
HERE = os.path.dirname(os.path.abspath(__file__))

LITERAL = "x" * (64 * 1024)
REPEATS = 64


def shared_literal_edit(builder_type):
    builder = builder_type()
    for i in range(REPEATS):
        builder.Append(LITERAL)
        builder.Prepend(LITERAL)
        builder.Insert(LITERAL, i)
        builder.Write(LITERAL, i)
    builder.MinSize(4 * REPEATS + 1, LITERAL)
    builder.SetSize(4 * REPEATS + 2, LITERAL)
    return builder.FinalizeAndReset()


def main():
    if Usd.GetVersion() != PINNED_USD_VERSION:
        sys.exit(f"OpenUSD {Usd.GetVersion()} is not the pinned {PINNED_USD_VERSION}")
    path = os.path.join(HERE, "shared_array_edit_literal.usdc")
    if os.path.exists(path):
        os.remove(path)
    layer = Sdf.Layer.CreateNew(path)
    prim = Sdf.CreatePrimInLayer(layer, "/Root")
    prim.specifier = Sdf.SpecifierDef
    for name, type_name, builder in [
        ("strings", Sdf.ValueTypeNames.StringArray, Vt.StringArrayEditBuilder),
        ("tokens", Sdf.ValueTypeNames.TokenArray, Vt.TokenArrayEditBuilder),
    ]:
        attr = Sdf.AttributeSpec(prim, name, type_name)
        attr.default = shared_literal_edit(builder)
    layer.Save()


if __name__ == "__main__":
    main()
