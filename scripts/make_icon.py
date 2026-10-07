#!/usr/bin/env python3
"""Draws the app icon (a blue rounded square with dial-pad dots) without any external libraries.

Writes to assets/: icon.png (1024) for the window and macOS, icon.ico for Windows and
tray.png (64) for the system tray.
Run: python3 scripts/make_icon.py
"""
import math
import struct
import zlib
from pathlib import Path

TOP = (0x5B, 0x9B, 0xFF)
BOTTOM = (0x35, 0x68, 0xDB)
WHITE = (255, 255, 255)


def clamp01(x):
    return 0.0 if x < 0 else 1.0 if x > 1 else x


def rounded_rect_sd(px, py, cx, cy, half_w, half_h, radius):
    qx = abs(px - cx) - (half_w - radius)
    qy = abs(py - cy) - (half_h - radius)
    outside = math.hypot(max(qx, 0.0), max(qy, 0.0))
    inside = min(max(qx, qy), 0.0)
    return outside + inside - radius


def render(size):
    """Returns RGBA bytes. All sizes are defined for a 1024 canvas and scaled."""
    k = size / 1024.0
    body = 412 * k  # half of the square's side (824 / 2)
    radius = 185 * k
    c = size / 2.0
    pitch = 130 * k
    dot_r = 42 * k
    dots = [
        (c + (col - 1) * pitch, c + (row - 1.5) * pitch)
        for row in range(4)
        for col in range(3)
    ]
    rows = []
    for y in range(size):
        py = y + 0.5
        t = clamp01((py - (c - body)) / (2 * body))
        base = tuple(TOP[i] + (BOTTOM[i] - TOP[i]) * t for i in range(3))
        row = bytearray([0])  # PNG filter: none
        for x in range(size):
            px = x + 0.5
            shape = clamp01(0.5 - rounded_rect_sd(px, py, c, c, body, body, radius))
            if shape == 0.0:
                row += b"\x00\x00\x00\x00"
                continue
            dot = 0.0
            for dx, dy in dots:
                if abs(px - dx) < dot_r + 1 and abs(py - dy) < dot_r + 1:
                    dot = max(dot, clamp01(0.5 - (math.hypot(px - dx, py - dy) - dot_r)))
            r, g, b = (int(round(base[i] + (WHITE[i] - base[i]) * dot)) for i in range(3))
            row += bytes((r, g, b, int(round(shape * 255))))
        rows.append(bytes(row))
    return b"".join(rows)


def png_bytes(size):
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(render(size), 9))
        + chunk(b"IEND", b"")
    )


def ico_bytes(sizes):
    images = [(s, png_bytes(s)) for s in sizes]
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for size, data in images:
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return out + b"".join(data for _, data in images)


def main():
    assets = Path(__file__).resolve().parent.parent / "assets"
    assets.mkdir(exist_ok=True)
    (assets / "icon.png").write_bytes(png_bytes(1024))
    (assets / "icon.ico").write_bytes(ico_bytes([256, 64, 48, 32, 16]))
    (assets / "tray.png").write_bytes(png_bytes(64))
    print("assets/icon.png, assets/icon.ico and assets/tray.png are ready")


if __name__ == "__main__":
    main()
