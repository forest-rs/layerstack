# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Checks USDZ archive layout with Python's zipfile, independently of Rust.

Usage: check_usdz_layout.py [--expect-invalid] [--arkit] FILE...

Checks every member is stored (uncompressed), unencrypted, CRC-32 correct
(re-read via ZipFile.testzip and recomputed from the raw bytes), starts its
data at a multiple of 64 bytes, and has a USDZ member extension; that the
first member is a USD layer; that there is no archive comment; and that the
End of Central Directory record ends the file.

With --arkit, also checks the ARKit / AR Quick Look profile: the first
member is a .usdc file holding a crate layer (PXR-USDC) and is the only USD
layer in the package.

Exit status 0 when every file meets the expectation (valid by default, or
invalid with --expect-invalid), 1 otherwise.
"""
import struct
import sys
import zipfile
import zlib

MEMBER_EXTENSIONS = {"usda", "usdc", "usd", "png", "jpg", "jpeg", "exr", "avif", "m4a", "mp3", "wav"}
LAYER_EXTENSIONS = {"usda", "usdc", "usd"}


def problems(path, arkit=False):
    found = []
    data = open(path, "rb").read()
    with zipfile.ZipFile(path) as archive:
        bad = archive.testzip()
        if bad is not None:
            found.append(f"testzip: bad CRC in {bad}")
        if archive.comment:
            found.append("archive has a comment")
        infos = archive.infolist()
        for i, info in enumerate(infos):
            off = info.header_offset
            name_len, extra_len = struct.unpack_from("<HH", data, off + 26)
            data_off = off + 30 + name_len + extra_len
            payload = data[data_off:data_off + info.file_size]
            ext = info.filename.rsplit(".", 1)[-1] if "." in info.filename else ""
            if info.compress_type != zipfile.ZIP_STORED:
                found.append(f"{info.filename}: compressed")
            if info.flag_bits & 0x41:
                found.append(f"{info.filename}: encrypted")
            if info.flag_bits & 0x08:
                found.append(f"{info.filename}: data descriptor")
            if zlib.crc32(payload) & 0xFFFFFFFF != info.CRC:
                found.append(f"{info.filename}: CRC mismatch")
            if data_off % 64:
                found.append(f"{info.filename}: data offset {data_off} not 64-byte aligned")
            if ext not in MEMBER_EXTENSIONS:
                found.append(f"{info.filename}: unsupported member type")
            if i == 0 and ext not in LAYER_EXTENSIONS:
                found.append(f"{info.filename}: first member is not a USD layer")
            if arkit and i == 0 and (ext != "usdc" or payload[:8] != b"PXR-USDC"):
                found.append(f"{info.filename}: ARKit root layer is not USDC")
            if arkit and i > 0 and ext in LAYER_EXTENSIONS | {"usdz"}:
                found.append(f"{info.filename}: second USD layer in an ARKit package")
            print(f"  {info.filename}: data@{data_off} size={info.file_size} crc={info.CRC:08x}")
    if data[-22:-18] != b"PK\x05\x06":
        found.append("EOCD is not the last 22 bytes")
    return found


def main(argv):
    expect_invalid = "--expect-invalid" in argv
    arkit = "--arkit" in argv
    ok = True
    for path in (a for a in argv if not a.startswith("--")):
        print(path)
        found = problems(path, arkit)
        for p in found:
            print(f"  problem: {p}")
        met = bool(found) == expect_invalid
        print(f"  -> {'as expected' if met else 'UNEXPECTED'} ({'invalid' if found else 'valid'})")
        ok &= met
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
