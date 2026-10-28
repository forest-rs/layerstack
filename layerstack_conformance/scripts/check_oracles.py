#!/usr/bin/env python3
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Checks every committed OpenUSD oracle against the OpenUSD it runs on.

Usage: check_oracles.py [--list] [NAME ...]

Runs `<script> --check` for every `*_oracle.py` beside this file that
takes `--check` and no required argument (each regenerates its oracle
into a temporary directory and fails when it differs from the committed
one), several at once, and reports each result. Scripts that need a
required argument, such as a compiled probe passed as `--binary`, are
run on their own. NAME limits the run to the scripts whose names
contain it. A new oracle script is checked as soon as it exists, so
adding one never needs a CI change.

Every script runs with the same interpreter and environment as this one:
CI sets `LAYERSTACK_OPENUSD_SOURCE` for the oracles that read OpenUSD's
sources. Exits non-zero, after printing each failure's output, when any
check fails.
"""

import argparse
import concurrent.futures
import os
import re
import subprocess
import sys
import time

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(SCRIPTS))


def checkable():
    """The oracle scripts that take `--check` and no required argument, in
    name order."""
    found = []
    for name in sorted(os.listdir(SCRIPTS)):
        if not name.endswith("_oracle.py"):
            continue
        with open(os.path.join(SCRIPTS, name)) as f:
            text = f.read()
        if re.search(r"""["']--check["']""", text) and "required=True" not in text:
            found.append(name)
    return found


def run(name):
    start = time.monotonic()
    result = subprocess.run(
        [sys.executable, os.path.join(SCRIPTS, name), "--check"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return name, result.returncode, result.stdout + result.stderr, time.monotonic() - start


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--list", action="store_true", help="list the scripts and exit")
    parser.add_argument("names", nargs="*")
    args = parser.parse_args()
    scripts = [s for s in checkable() if not args.names or any(n in s for n in args.names)]
    if args.list:
        print("\n".join(scripts))
        return
    if not scripts:
        raise SystemExit("no oracle script matches")
    with concurrent.futures.ThreadPoolExecutor(max_workers=os.cpu_count() or 4) as pool:
        results = list(pool.map(run, scripts))
    failed = [r for r in results if r[1] != 0]
    for name, code, output, seconds in results:
        print(f"{'ok  ' if code == 0 else 'FAIL'} {name} ({seconds:.1f} s)")
    for name, _, output, _ in failed:
        print(f"\n== {name}\n{output.rstrip()}")
    print(f"\n{len(results) - len(failed)} of {len(results)} oracles match")
    if failed:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
