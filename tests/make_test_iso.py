#!/usr/bin/env python3
"""Build a minimal synthetic SACD ISO to validate `sacd-tool list`.

Produces a Scarlet Book disc with Master TOC, Master Text and a stereo
Area TOC (3 tracks, plain DSD) with track text, LSN offsets, durations
and ISRC. Intentionally small (no real audio) so `list` can be exercised
end-to-end without a commercial disc.
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

LSN = 2048
MASTER_TOC_LSN = 510
MASTER_TEXT_LSN = 511
AREA_TOC_LSN = 544
AREA_TOC_SECTORS = 6  # header + TTxt + TRL1 + TRL2 + IGL(2 sectors)


def be16(v: int) -> bytes:
    return struct.pack(">H", v)


def be32(v: int) -> bytes:
    return struct.pack(">I", v)


def u8(v: int) -> bytes:
    return struct.pack(">B", v)


def build_master_toc() -> bytes:
    b = bytearray()
    b += b"SACDMTOC"
    b += u8(1) + u8(0x20)          # version major.minor
    b += b"\x00" * 6               # reserved01
    b += be16(1) + be16(1)         # album_set_size, sequence
    b += b"\x00" * 4               # reserved02
    b += b"CATALOG0001".ljust(16, b" ")[:16]
    b += b"\x00" * (4 * 4)         # album_genre[4]
    b += b"\x00" * 8               # reserved03
    b += be32(AREA_TOC_LSN)        # area_1_toc_1_start (stereo)
    b += be32(0) + be32(0) + be32(0)  # other toc starts
    b += u8(0)                     # disc_flags
    b += b"\x00" * 3               # reserved04
    b += be16(AREA_TOC_SECTORS)    # area_1_toc_size
    b += be16(0)                   # area_2_toc_size
    b += b"DISCCAT0002".ljust(16, b" ")[:16]
    b += b"\x00" * (4 * 4)         # disc_genre[4]
    b += be16(1997) + u8(6) + u8(15)  # disc_date
    b += b"\x00" * 4               # reserved05
    b += u8(1)                     # text_area_count
    b += b"\x00" * 7               # reserved06
    b += (b"EN" + b"\x00" * 6) * 8  # locales[8]
    b += b"\x00" * (LSN - len(b))
    assert len(b) == LSN
    return bytes(b)


def build_master_text() -> bytes:
    b = bytearray()
    b += b"SACDText"
    b += b"\x00" * 8               # reserved (to offset 16)
    n_slots = 12
    table_end = 16 + n_slots * 2
    strings = bytearray()

    def put(s: str) -> int:
        if not s:
            return 0
        while (table_end + len(strings)) % 4 != 0:
            strings.append(0)
        off = table_end + len(strings)
        strings.extend(s.encode("ascii") + b"\x00")
        return off

    album_title = put("Synthetic SACD Album")
    album_artist = put("The Test Ensemble")
    album_publisher = put("Clean Room Records")
    album_copyright = put("(c) 1997 CR")
    disc_title = put("Synthetic Disc")
    disc_artist = put("The Test Ensemble")
    disc_publisher = put("Clean Room Records")
    disc_copyright = put("(c) 1997 CR")
    slots = [album_title, album_artist, album_publisher, album_copyright,
             0, 0, 0, 0, disc_title, disc_artist, disc_publisher, disc_copyright]
    for v in slots:
        b += be16(v)
    b += strings
    b += b"\x00" * (LSN - len(b))
    assert len(b) == LSN
    return bytes(b)


def build_area_toc() -> bytes:
    track_count = 3
    channels = 2
    frame_format = 2  # Dsd3In14 (plain DSD)

    # Sector 0: Area TOC header
    h = bytearray()
    h += b"TWOCHTOC"
    h += u8(1) + u8(0x20)
    h += be16(AREA_TOC_SECTORS)
    h += b"\x00" * 4
    h += be32(0)
    h += u8(2)                     # sample_frequency (DSD64)
    h += u8(frame_format)          # area_flags
    h += b"\x00" * 10
    h += u8(channels)
    h += u8(0)
    h += u8(channels)
    h += u8(0)
    h += b"\x00" * 12
    h += u8(0)
    h += b"\x00" * 15
    h += u8(5) + u8(30) + u8(0)    # total_playtime
    h += u8(0) + u8(0)
    h += u8(track_count)
    h += b"\x00" * 2
    h += be32(1000) + be32(1100)
    h += u8(1)
    h += b"\x00" * 7
    h += (b"EN" + b"\x00" * 6) * 8
    h += b"\x00" * 8
    h += be16(LSN)                 # track_text_offset
    h += be16(2 * LSN)
    h += be16(3 * LSN)
    h += b"\x00" * 10
    h += be16(0) + be16(0) + be16(0) + be16(0)
    h += b"\x00" * (LSN - len(h))
    assert len(h) == LSN

    # Sector 1: SACDTTxt
    t = bytearray()
    t += b"SACDTTxt"
    t += be16(0)
    entries = [
        (0, "First Movement"), (1, "A. Composer"),
        (0, "Second Movement"), (1, "A. Composer"),
        (0, "Third Movement"), (1, "A. Composer"),
    ]
    for ttype, text in entries:
        t += u8(ttype) + u8(0x20)
        t += text.encode("ascii") + b"\x00"
        while len(t) % 4 != 0:
            t += b"\x00"
    t += b"\x00" * (LSN - len(t))
    assert len(t) == LSN

    # Sector 2: SACDTRL1
    o = bytearray()
    o += b"SACDTRL1"
    o += b"\x00" * 8
    starts = [1000, 1034, 1068]
    lengths = [30, 30, 32]
    for i in range(255):
        o += be32(starts[i] if i < track_count else 0)
    for i in range(255):
        o += be32(lengths[i] if i < track_count else 0)
    o += b"\x00" * (LSN - len(o))
    assert len(o) == LSN

    # Sector 3: SACDTRL2
    d = bytearray()
    d += b"SACDTRL2"
    d += b"\x00" * 8
    starts_m = [(0, 0, 0), (2, 0, 0), (4, 0, 0)]
    durs_m = [(2, 0, 0), (2, 0, 0), (1, 30, 0)]
    for i in range(255):
        m, s, f = starts_m[i] if i < track_count else (0, 0, 0)
        d += u8(m) + u8(s) + u8(f)
    for i in range(255):
        m, s, f = durs_m[i] if i < track_count else (0, 0, 0)
        d += u8(m) + u8(s) + u8(f)
    d += b"\x00" * (LSN - len(d))
    assert len(d) == LSN

    # Sector 4: SACD_IGL
    g = bytearray()
    g += b"SACD_IGL"
    g += b"\x00" * 8
    isrcs = ["GBCRX0000001", "GBCRX0000002", "GBCRX0000003"]
    for i in range(255):
        code = isrcs[i] if i < track_count else ""
        g += code.encode("ascii").ljust(12, b"\x00")[:12]
    for _ in range(255):
        g += b"\x00" * 4
    g += b"\x00" * (LSN - len(g))
    assert len(g) == LSN

    return bytes(h) + bytes(t) + bytes(o) + bytes(d) + bytes(g)


def main() -> int:
    total_sectors = AREA_TOC_LSN + AREA_TOC_SECTORS + 16
    iso = bytearray(total_sectors * LSN)
    iso[MASTER_TOC_LSN * LSN: MASTER_TOC_LSN * LSN + LSN] = build_master_toc()
    iso[MASTER_TEXT_LSN * LSN: MASTER_TEXT_LSN * LSN + LSN] = build_master_text()
    area = build_area_toc()
    base = AREA_TOC_LSN * LSN
    iso[base: base + len(area)] = area
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).with_name("test.iso")
    out.write_bytes(iso)
    print(f"Wrote {out} ({out.stat().st_size} bytes, {total_sectors} sectors)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
