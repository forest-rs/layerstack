# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Generates USDA text written by OpenUSD, each with the USDC of the same layer.

Each fixture is authored through `Sdf` and exported twice, as `NAME.usda` and
`NAME.usdc`. The text is what OpenUSD's own writer produces, and the crate
file carries the same layer in a form whose reader is checked independently
(`usdc_versions`, `usdc_binary`), so
`layerstack_conformance/tests/usda_openusd_text.rs` requires the USDA reader
to read each `.usda` as the USDC reader reads its `.usdc`.

- `strings`: string, token and dictionary-key values holding every
  character OpenUSD's writer escapes (`Sdf_FileIOUtility::Quote`): both
  quotes, backslash, control characters written as `\\n`, `\\t`, `\\r` and
  `\\xNN`, DEL, and multi-line text written triple-quoted.

Pinned oracle: `usd-core` 26.8 from PyPI (OpenUSD v26.08), as for the
`usdc_versions` fixtures:

    python3 -m venv venv && venv/bin/pip install usd-core==26.8
    venv/bin/python layerstack_conformance/fixtures/openusd_usda_text/generate.py
"""

import os
import sys

from pxr import Sdf, Usd

PINNED_USD_VERSION = (0, 26, 8)
HERE = os.path.dirname(os.path.abspath(__file__))

# Every ASCII control character but NUL (which OpenUSD's writer truncates
# at), then the characters `Quote` escapes or passes through.
TRICKY = "".join(chr(c) for c in range(1, 32)) + "\x7f say \"hi\" it's \\ é 日"


def attribute(prim, name, type_name, value):
    attr = Sdf.AttributeSpec(prim, name, type_name)
    attr.default = value
    return attr


def author_strings(layer):
    layer.documentation = "Layer doc with 'single' and \"double\" quotes\nand a second line"
    layer.comment = "only 'single' quotes"
    prim = Sdf.PrimSpec(layer, "Strings", Sdf.SpecifierDef)
    prim.documentation = "a\\b\tc"
    attribute(prim, "tricky", Sdf.ValueTypeNames.String, TRICKY)
    attribute(prim, "doubleOnly", Sdf.ValueTypeNames.String, 'say "hi"')
    attribute(prim, "bothQuotes", Sdf.ValueTypeNames.String, "it's \"x\"")
    attribute(prim, "multiline", Sdf.ValueTypeNames.String, "one\ntwo \"\"\" three\n")
    attribute(prim, "token", Sdf.ValueTypeNames.Token, "tab\there")
    attribute(prim, "array", Sdf.ValueTypeNames.StringArray, [TRICKY, "", "plain"])
    prim.SetInfo("customData", {TRICKY: TRICKY, "multi\nline": "v\nw"})


FIXTURES = {
    "strings": author_strings,
}


def main():
    if Usd.GetVersion() != PINNED_USD_VERSION:
        sys.exit("expected OpenUSD %s, found %s" % (PINNED_USD_VERSION, Usd.GetVersion()))
    for name, author in FIXTURES.items():
        layer = Sdf.Layer.CreateAnonymous(".usda")
        author(layer)
        for ext in ("usda", "usdc"):
            path = os.path.join(HERE, "%s.%s" % (name, ext))
            if not layer.Export(path):
                sys.exit("failed to write " + path)


if __name__ == "__main__":
    main()
