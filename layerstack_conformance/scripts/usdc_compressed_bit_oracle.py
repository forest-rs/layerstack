# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Records how OpenUSD reads a crate file whose arrays of element types it
never compresses carry the compressed bit.

Usage: usdc_compressed_bit_oracle.py [--check] [OUT_DIR]

Needs OpenUSD through Python `pxr`, for example `pip install usd-core==26.11`.
Writes, under `layerstack_conformance/fixtures/usdc_compressed_bit` by
default, `arrays.usda`, `compressed_bit.usdc` and `oracle.json`, which
`tests/conformance/usdc_compressed_bit.rs` replays with `read_usdc`.

OpenUSD honours a value's compressed bit only for arrays of `int`,
`uint`, `int64` and `uint64` and of `half`, `float` and `double`, and reads
every other element type uncompressed whatever the bit says
(`_ReadPossiblyCompressedArray`, `pxr/usd/sdf/crateFile.cpp:2178-2290`).
No OpenUSD writer sets the bit on those, but another writer may.

The oracle makes such a file call by call: `SdfLayer::Export` of
`arrays.usda` to a crate file, then a copy (`compressed_bit.usdc`) whose
FIELDS section sets bit 61, the compressed bit, on the value of every
array of `bool`, `uchar`, `string`, `token`, `asset`, `timecode` and
`pathExpression` (`PATCHED`), each of at least 16 elements
(`MinCompressedArraySize`), so a reader that honoured the bit would
decompress it. The section is decoded (`TfFastCompression`, LZ4 blocks),
patched, stored again as one block of literals and appended with a new
table of contents, so nothing else in the file moves. Beside them, an
`int[]` and a `float[]` stay compressed as written. It records:

- `patched`: the property paths whose value got the bit;
- `read`: `Sdf.Layer.OpenAsAnonymous` of the patched file, then
  `ExportToString`, which reads every value;
- `same_as_export`: whether that text is the text of the exported file
  read the same way, that is whether OpenUSD ignored the bit.

`--check` rewrites everything into a temporary directory and fails unless
it matches the checked-in fixture: the text files byte for byte, and the
crate file by what OpenUSD reads from it, since its bytes vary from
platform to platform (B019).
"""
import argparse
import filecmp
import json
import os
import struct
import tempfile

from pxr import Sdf, Usd

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "fixtures", "usdc_compressed_bit"))

COUNT = 18
SOURCE = """#usda 1.0

def "Arrays"
{{
    asset[] files = [{assets}]
    bool[] flags = [{bools}]
    uchar[] bytes = [{uchars}]
    float[] widths = [{floats}]
    int[] counts = [{ints}]
    pathExpression[] patterns = [{expressions}]
    string[] names = [{strings}]
    token[] tags = [{tokens}]
    timecode[] frames = [{timecodes}]
}}
""".format(
    assets=", ".join("@./rock{}.png@".format(i) for i in range(COUNT)),
    bools=", ".join("1" if i % 3 else "0" for i in range(COUNT)),
    uchars=", ".join(str(i * 7 % 256) for i in range(COUNT)),
    floats=", ".join("{}.5".format(i) for i in range(COUNT)),
    ints=", ".join(str(i * i - 40) for i in range(COUNT)),
    expressions=", ".join('"/Lamp{}"'.format(i) for i in range(COUNT)),
    strings=", ".join('"robot {}"'.format(i) for i in range(COUNT)),
    tokens=", ".join('"tag{}"'.format(i) for i in range(COUNT)),
    timecodes=", ".join("{}.25".format(i) for i in range(COUNT)),
)
# Crate value types (`crateDataTypes.h`) whose arrays get the bit: bool,
# uchar, string, token, asset, timecode, pathExpression.
PATCHED = {1: "bool", 2: "uchar", 10: "string", 11: "token", 12: "asset",
           56: "timecode", 57: "pathExpression"}
COMPRESSED_BIT = 1 << 61


def lz4_decompress(src, size):
    """An LZ4 block decoded to `size` bytes."""
    out = bytearray()
    i = 0
    while i < len(src):
        token = src[i]
        i += 1
        literals = token >> 4
        if literals == 15:
            while True:
                b = src[i]
                i += 1
                literals += b
                if b != 255:
                    break
        out += src[i:i + literals]
        i += literals
        if i >= len(src):
            break
        offset = src[i] | (src[i + 1] << 8)
        i += 2
        length = token & 15
        if length == 15:
            while True:
                b = src[i]
                i += 1
                length += b
                if b != 255:
                    break
        for _ in range(length + 4):
            out.append(out[-offset])
    if len(out) != size:
        raise SystemExit(f"LZ4 block decoded to {len(out)} bytes, not {size}")
    return bytes(out)


def lz4_literals(raw):
    """`raw` as one LZ4 block of literals only."""
    if len(raw) < 15:
        head = bytes([len(raw) << 4])
    else:
        rest = len(raw) - 15
        head = bytearray([0xF0])
        while rest >= 255:
            head.append(255)
            rest -= 255
        head.append(rest)
    return bytes(head) + raw


def set_compressed_bit(data):
    """A copy of crate file `data` whose array values of `PATCHED` types
    carry the compressed bit, and the indices of the field values it set
    it on."""
    data = bytearray(data)
    toc = struct.unpack_from("<Q", data, 16)[0]
    sections = []
    for i in range(struct.unpack_from("<Q", data, toc)[0]):
        at = toc + 8 + i * 32
        name = data[at:at + 16].rstrip(b"\0").decode()
        start, size = struct.unpack_from("<qq", data, at + 16)
        sections.append([name, start, size])
    fields = next(s for s in sections if s[0] == "FIELDS")
    at = fields[1]
    count = struct.unpack_from("<Q", data, at)[0]
    at += 8
    tokens_size = struct.unpack_from("<Q", data, at)[0]
    tokens = bytes(data[at:at + 8 + tokens_size])
    at += 8 + tokens_size
    reps_size = struct.unpack_from("<Q", data, at)[0]
    at += 8
    compressed = bytes(data[at:at + reps_size])
    if compressed[0] != 0:
        raise SystemExit("expected one TfFastCompression chunk")
    reps = list(struct.unpack("<%dQ" % count, lz4_decompress(compressed[1:], count * 8)))
    patched = []
    for i, rep in enumerate(reps):
        is_array, inlined = rep >> 63 & 1, rep >> 62 & 1
        if is_array and not inlined and rep & ((1 << 48) - 1) and (rep >> 48) & 0xFF in PATCHED:
            reps[i] = rep | COMPRESSED_BIT
            patched.append(i)
    stored = b"\0" + lz4_literals(struct.pack("<%dQ" % count, *reps))
    section = struct.pack("<Q", count) + tokens + struct.pack("<Q", len(stored)) + stored
    while len(data) % 8:
        data.append(0)
    fields[1], fields[2] = len(data), len(section)
    data += section
    while len(data) % 8:
        data.append(0)
    new_toc = len(data)
    data += struct.pack("<Q", len(sections))
    for name, start, size in sections:
        data += name.encode().ljust(16, b"\0") + struct.pack("<qq", start, size)
    struct.pack_into("<Q", data, 16, new_toc)
    return bytes(data), patched


def write(out):
    out = os.path.realpath(out)
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, "arrays.usda"), "w") as f:
        f.write(SOURCE)
    with tempfile.TemporaryDirectory() as tmp:
        exported = os.path.join(tmp, "arrays.usdc")
        if not Sdf.Layer.FindOrOpen(os.path.join(out, "arrays.usda")).Export(exported):
            raise SystemExit("export failed")
        with open(exported, "rb") as f:
            data, patched = set_compressed_bit(f.read())
        export_text = Sdf.Layer.OpenAsAnonymous(exported).ExportToString()
    with open(os.path.join(out, "compressed_bit.usdc"), "wb") as f:
        f.write(data)
    if len(patched) != len(PATCHED):
        raise SystemExit(f"patched {len(patched)} values, expected {len(PATCHED)}")
    read = Sdf.Layer.OpenAsAnonymous(os.path.join(out, "compressed_bit.usdc"))
    text = read.ExportToString()
    patched_paths = []
    for prim in read.rootPrims:
        for attribute in prim.attributes:
            if str(attribute.typeName) in {name + "[]" for name in PATCHED.values()}:
                patched_paths.append(str(attribute.path))
    _, minor, patch = Usd.GetVersion()
    oracle = {
        "openusd": f"{minor}.{patch}",
        "patched": sorted(patched_paths),
        "read": text,
        "same_as_export": text == export_text,
    }
    with open(os.path.join(out, "oracle.json"), "w") as f:
        json.dump(oracle, f, indent=1, sort_keys=True)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("out", nargs="?", default=FIXTURES)
    args = parser.parse_args()
    if not args.check:
        write(args.out)
        return
    with tempfile.TemporaryDirectory() as tmp:
        write(tmp)
        for name in ["arrays.usda", "oracle.json"]:
            if not filecmp.cmp(os.path.join(tmp, name), os.path.join(FIXTURES, name),
                               shallow=False):
                raise SystemExit(f"{name} differs from {FIXTURES}")
        crate = "compressed_bit.usdc"
        fresh = Sdf.Layer.OpenAsAnonymous(os.path.join(tmp, crate)).ExportToString()
        kept = Sdf.Layer.OpenAsAnonymous(os.path.join(FIXTURES, crate)).ExportToString()
        if fresh != kept:
            raise SystemExit(f"{crate} reads differently from {FIXTURES}")


if __name__ == "__main__":
    main()
