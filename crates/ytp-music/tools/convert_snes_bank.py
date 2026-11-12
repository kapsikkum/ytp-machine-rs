#!/usr/bin/env python3
"""One-off: convert the Python app's snes_bank.npz into data/snes_bank.bin.

    python tools/convert_snes_bank.py <path to snes_bank.npz> [out.bin]

Binary layout (little endian), read by src/snes.rs:

    magic    b"SNB1"
    u32      entry count
    u32      pcm length (i16 samples)
    u32      source-json length, then that many UTF-8 bytes (the bank's "_source")
    entries  key(u8 len + bytes) name(u8 len + bytes) f64 rate
             i32 loop_start i32 loop_end (both -1 for no loop)
             i32 root i32 cents f64 scale f64 attenuation_db f64 env[6]
             u32 offset u32 length
    pcm      i16 * pcm length
"""
import json
import struct
import sys

import numpy as np


def main() -> None:
    src = sys.argv[1]
    dst = sys.argv[2] if len(sys.argv) > 2 else "data/snes_bank.bin"
    with np.load(src) as z:
        pcm = z["pcm"].astype("<i2")
        index = json.loads(bytes(z["index"]).decode())
    source = json.dumps(index.get("_source", {}), sort_keys=True).encode()
    entries = [(k, e) for k, e in index.items() if not k.startswith("_")]
    out = bytearray(b"SNB1")
    out += struct.pack("<III", len(entries), len(pcm), len(source)) + source
    for key, e in entries:
        for s in (key, e["name"]):
            b = s.encode()
            out += struct.pack("<B", len(b)) + b
        loop = e["loop"] or (-1, -1)
        out += struct.pack("<d", float(e["rate"]))
        out += struct.pack("<ii", int(loop[0]), int(loop[1]))
        out += struct.pack("<ii", int(e["root"]), int(e["cents"]))
        out += struct.pack("<dd", float(e["scale"]), float(e["attenuation_db"]))
        out += struct.pack("<6d", *[float(v) for v in e["env"]])
        out += struct.pack("<II", int(e["offset"]), int(e["length"]))
    out += pcm.tobytes()
    with open(dst, "wb") as fh:
        fh.write(out)
    print(f"{len(entries)} entries, {len(pcm)} samples, {len(out)} bytes -> {dst}")


if __name__ == "__main__":
    main()
