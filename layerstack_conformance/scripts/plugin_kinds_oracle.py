# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records what OpenUSD's kind registry makes of plugin-declared kinds; run
with usd-core 26.11.

Usage: plugin_kinds_oracle.py [--check]

Registers three plugins whose `plugInfo.json` declares `Kinds`:
`fixtures/custom_kinds` (kinds alone, with a forward base, a base nobody
declares, an empty base and a cycle), `fixtures/custom_kinds/rejected`
(declarations OpenUSD refuses: an invalid name, a built-in kind, a
declaration and a `baseKind` of the wrong type, and a kind the first plugin
declares too, with the same base, so either registration order gives the
same hierarchy) and the `lab` schema library of
`fixtures/schema_compiler`. It records the errors registration raises, the
kinds registered and their base kinds, and for every pair of those kinds
(and two unregistered names) whether `KindRegistry::IsA` holds, or that it
would not return, which is not asked: a walk of base kinds that comes back
on itself without meeting the kind asked for. It writes
`fixtures/custom_kinds/oracle.json`, which `tests/conformance/plugin_kinds.rs` replays
against the kinds layerstack registers from the same files; with `--check`
it writes nothing and fails on a difference.
"""
import argparse
import json
from pathlib import Path

from pxr import Kind, Plug, Tf, Usd

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"
PLUGINS = [
    FIXTURES / "custom_kinds",
    FIXTURES / "custom_kinds" / "rejected",
    FIXTURES / "schema_compiler" / "lab",
]
UNREGISTERED = ["stack_missing", "unknown"]


def walk(kind, target):
    """`IsA(kind, target)` from base kinds alone: True, False, or None when
    OpenUSD's walk would never return."""
    seen = set()
    while True:
        if kind == target:
            return True
        if not Kind.Registry.HasKind(kind):
            return False
        if kind in seen:
            return None
        seen.add(kind)
        kind = str(Kind.Registry.GetBaseKind(kind))
        if not kind:
            return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    registered = Plug.Registry().RegisterPlugins([str(p) for p in PLUGINS])
    names = sorted(p.name for p in registered)
    if names != ["LayerstackKindFixture", "LayerstackRejectedKinds", "labInstruments"]:
        raise SystemExit(f"registered {names}")
    errors = []
    try:
        Kind.Registry.GetAllKinds()
    except Tf.ErrorException as error:
        for line in str(error).strip().splitlines():
            message = line.split(" : ", 1)[-1].strip()
            if message.startswith("'") and message.endswith("'"):
                message = message[1:-1]
            errors.append(message)
    kinds = sorted(str(k) for k in Kind.Registry.GetAllKinds())
    queried = kinds + UNREGISTERED
    is_a = {}
    hangs = {}
    for kind in queried:
        answers = {target: walk(kind, target) for target in queried}
        for target, answer in answers.items():
            # Asked of OpenUSD wherever it returns.
            if answer is not None and bool(Kind.Registry.IsA(kind, target)) != answer:
                raise SystemExit(f"IsA({kind}, {target}) is not {answer}")
        is_a[kind] = [t for t, answer in answers.items() if answer]
        if any(answer is None for answer in answers.values()):
            hangs[kind] = [t for t, answer in answers.items() if answer is None]
    text = json.dumps({
        "openusd_version": ".".join(str(v) for v in Usd.GetVersion()),
        "errors": sorted(errors),
        "kinds": kinds,
        "base_kinds": {k: str(Kind.Registry.GetBaseKind(k)) for k in kinds},
        "is_a": is_a,
        "hangs": hangs,
    }, indent=1, sort_keys=True) + "\n"
    snapshot = FIXTURES / "custom_kinds" / "oracle.json"
    if args.check:
        if snapshot.read_text() != text:
            raise SystemExit(f"OpenUSD's kind registry differs from {snapshot}")
    else:
        snapshot.write_text(text)
        print(f"wrote {snapshot}: {len(kinds)} kinds")


if __name__ == "__main__":
    main()
