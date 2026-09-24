# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Renders an exported package with usdrecord and checks the colors seen.

Usage: render_check.py PACKAGE OUT_PNG HUE[,HUE...]

Writes a wrapper layer next to OUT_PNG that sublayers PACKAGE and adds a
camera looking at the origin from the (+X, -Y, +Z) octant, so the +Z, +X
and -Y faces of a unit cube are in view (usdrecord's default camera frames
the stage bounds poorly). Renders it with `usdrecord` (Storm), decodes the
PNG with the standard library, classifies every opaque pixel by hue, and
requires each named HUE to cover at least MIN_SHARE of them. Hues:
red, blue, orange, teal.

Exit status 0 when every hue is present, 1 otherwise; the pixel shares are
printed either way.
"""
import math
import os
import struct
import subprocess
import sys
import zlib

MIN_SHARE = 0.02


def camera_rows(eye, target=(0.0, 0.0, 0.0), up=(0.0, 0.0, 1.0)):
    """USD row-vector camera transform: the camera looks down its -Z with +Y up."""
    def sub(a, b):
        return [a[i] - b[i] for i in range(3)]

    def norm(v):
        n = math.sqrt(sum(c * c for c in v))
        return [c / n for c in v]

    def cross(a, b):
        return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]

    forward = norm(sub(target, eye))
    right = norm(cross(forward, up))
    cam_up = cross(right, forward)
    back = [-c for c in forward]
    return [right + [0.0], cam_up + [0.0], back + [0.0], list(eye) + [1.0]]


def wrapper(package):
    rows = camera_rows((2.6, -3.2, 2.4))
    matrix = ", ".join("(" + ", ".join(f"{v:.6f}" for v in row) + ")" for row in rows)
    return f"""#usda 1.0
(
    metersPerUnit = 1
    subLayers = [@{package}@]
    upAxis = "Z"
)

def Camera "RenderCam"
{{
    float focalLength = 35
    matrix4d xformOp:transform = ( {matrix} )
    uniform token[] xformOpOrder = ["xformOp:transform"]
}}
"""


def decode_png(path):
    """Returns (width, height, channels, rows of bytes) for an 8-bit PNG file."""
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, header = 8, b"", None
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
    width, height, depth, color_type, _, _, interlace = header
    assert depth == 8 and interlace == 0, "only 8-bit non-interlaced PNG files"
    channels = {0: 1, 2: 3, 4: 2, 6: 4}[color_type]
    raw = zlib.decompress(idat)
    stride = width * channels
    rows, prev = [], bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        kind, line = raw[start], bytearray(raw[start + 1:start + 1 + stride])
        for i in range(stride):
            a = line[i - channels] if i >= channels else 0
            b = prev[i]
            c = prev[i - channels] if i >= channels else 0
            if kind == 1:
                line[i] = (line[i] + a) & 0xFF
            elif kind == 2:
                line[i] = (line[i] + b) & 0xFF
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
        rows.append(line)
        prev = line
    return width, height, channels, rows


def hue(r, g, b):
    if r > 60 and r > 2.5 * g and r > 2.5 * b:
        return "red"
    if b > 60 and b > 2.0 * r and b > 1.5 * g:
        return "blue"
    if r > 60 and r > 1.3 * g and g > 1.5 * b:
        return "orange"
    if g > 40 and b > 40 and g > 2.0 * r and b > 2.0 * r:
        return "teal"
    return None


def main():
    package, out_png, wanted = sys.argv[1], sys.argv[2], sys.argv[3].split(",")
    out_dir = os.path.dirname(os.path.abspath(out_png))
    layer = os.path.join(out_dir, os.path.splitext(os.path.basename(out_png))[0] + ".usda")
    with open(layer, "w") as f:
        f.write(wrapper(os.path.relpath(os.path.abspath(package), out_dir)))
    subprocess.run(
        ["usdrecord", "--camera", "/RenderCam", "--imageWidth", "480", layer, out_png],
        check=True,
    )
    width, height, channels, rows = decode_png(out_png)
    counts, opaque = {}, 0
    for line in rows:
        for x in range(width):
            px = line[x * channels:(x + 1) * channels]
            if channels == 4 and px[3] < 128:
                continue
            opaque += 1
            h = hue(px[0], px[1], px[2])
            if h:
                counts[h] = counts.get(h, 0) + 1
    shares = {h: counts.get(h, 0) / max(opaque, 1) for h in ("red", "blue", "orange", "teal")}
    print(f"{out_png}: {width}x{height}, {opaque} opaque pixels; " +
          ", ".join(f"{h} {s:.1%}" for h, s in shares.items()))
    missing = [h for h in wanted if shares[h] < MIN_SHARE]
    if missing:
        print(f"missing hues: {', '.join(missing)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
