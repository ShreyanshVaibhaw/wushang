"""Writes testdata/test_zlib_subs.mkv: 6 s of silent PCM audio plus an English text subtitle track
whose packets are zlib-compressed (Matroska ContentCompression, algorithm 0), the way mkvmerge
stores subtitles by default. FFmpeg's own Matroska muxer never compresses tracks, so this writes
the few elements needed by hand.

    python scripts/make-zlib-mkv.py
"""

import struct
import zlib
from pathlib import Path

RATE, SECONDS, CHUNK_MS = 8000, 6, 250
CUES = [(1000, 2000, "First zlib cue"), (3500, 2000, "Second zlib cue")]


def size(n):
    """An 8-byte EBML size: wasteful but simple, and every reader accepts it."""
    return bytes([0x01]) + n.to_bytes(7, "big")


def el(id_, payload):
    return id_.to_bytes((id_.bit_length() + 7) // 8, "big") + size(len(payload)) + payload


def uint(id_, v):
    return el(id_, v.to_bytes(max(1, (v.bit_length() + 7) // 8), "big"))


def text(id_, s):
    return el(id_, s.encode())


def double(id_, v):
    return el(id_, struct.pack(">d", v))


def block(track, rel_ms, flags, payload):
    return bytes([0x80 | track]) + struct.pack(">hB", rel_ms, flags) + payload


header = el(0x1A45DFA3, uint(0x4286, 1) + uint(0x42F7, 1) + uint(0x42F2, 4) + uint(0x42F3, 8)
            + text(0x4282, "matroska") + uint(0x4287, 4) + uint(0x4285, 2))
info = el(0x1549A966, uint(0x2AD7B1, 1_000_000) + text(0x4D80, "make-zlib-mkv")
          + text(0x5741, "make-zlib-mkv") + double(0x4489, SECONDS * 1000.0))
audio = el(0xAE, uint(0xD7, 1) + uint(0x73C5, 1) + uint(0x83, 2) + text(0x86, "A_PCM/INT/LIT")
           + text(0x22B59C, "eng")
           + el(0xE1, double(0xB5, float(RATE)) + uint(0x9F, 1) + uint(0x6264, 16)))
# ContentEncodings > ContentEncoding > ContentCompression with ContentCompAlgo 0 (zlib).
compression = el(0x6D80, el(0x6240, uint(0x5031, 0) + uint(0x5032, 1) + uint(0x5033, 0)
                             + el(0x5034, uint(0x4254, 0))))
subs = el(0xAE, uint(0xD7, 2) + uint(0x73C5, 2) + uint(0x83, 0x11) + text(0x86, "S_TEXT/UTF8")
          + text(0x22B59C, "eng") + compression)
tracks = el(0x1654AE6B, audio + subs)

clusters = b""
silence = bytes(RATE * CHUNK_MS // 1000 * 2)
for second in range(SECONDS):
    start = second * 1000
    body = uint(0xE7, start)
    for rel in range(0, 1000, CHUNK_MS):
        body += el(0xA3, block(1, rel, 0x80, silence))
    for at, duration, line in CUES:
        if start <= at < start + 1000:
            packet = zlib.compress(line.encode())
            body += el(0xA0, el(0xA1, block(2, at - start, 0x00, packet)) + uint(0x9B, duration))
    clusters += el(0x1F43B675, body)

segment = el(0x18538067, info + tracks + clusters)
out = Path(__file__).resolve().parent.parent / "testdata" / "test_zlib_subs.mkv"
out.write_bytes(header + segment)
print(f"wrote {out} ({out.stat().st_size} bytes)")
