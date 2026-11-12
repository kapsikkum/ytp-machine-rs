#!/usr/bin/env python3
"""Extra parity fixtures for crates/ytp-music, beyond tests/golden/.

Runs the Python app (READ-ONLY, <python app>) on fixed inputs
and writes small JSON files (plus one MIDI file) into
crates/ytp-music/tests/fixtures/:

    rng.json      numpy default_rng(seed).standard_normal() streams
    drums.json    drums.shape() of a synthetic word, per group
    pitch.json    Voice marks + render(perfect/tape/raw, with hold) of a glide
    music.json    music.parse() of a generated MIDI file (song.mid)
    master.json   master.balance() / gain_for / ride / limit on synthetic parts
    numpy.json    pairwise-sum / mean / interp / linspace edge cases

Usage:  python crates/ytp-music/tools/gen_fixtures.py
"""
import io
import json
import os
import sys

PY_APP = r"<python app>"
sys.path.insert(0, PY_APP)
os.environ.setdefault("MRS_DATA_DIR", os.path.join(os.environ.get("TEMP", "."), "ytp-fixture-data"))

import numpy as np  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(os.path.dirname(HERE), "tests", "fixtures")
os.makedirs(OUT, exist_ok=True)
SR = 44100


def dump(name, obj):
    with open(os.path.join(OUT, name), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(obj, fh, indent=1, sort_keys=True)
        fh.write("\n")


def f(x):
    return float(x)


def stats(a):
    a = np.asarray(a, dtype=np.float64)
    pts = np.linspace(0, len(a) - 1, 64).astype(int) if len(a) else []
    return {"len": int(len(a)), "peak": f(np.abs(a).max()) if len(a) else 0.0,
            "rms": f(np.sqrt(np.mean(a * a))) if len(a) else 0.0, "dc": f(a.mean()) if len(a) else 0.0,
            "idx": [int(i) for i in pts], "val": [f(a[i]) for i in pts]}


# -- rng ----------------------------------------------------------------------
def gen_rng():
    out = {}
    for seed in (0, 1, 7, 11, 13, 5, 6, 20260913, 4294967295, 5000000000):
        rng = np.random.default_rng(seed)
        z = rng.standard_normal(400000)
        bits = z.view(np.uint64)
        h = 1469598103934665603
        for b in bits[:: 7].tolist():
            h = ((h ^ b) * 1099511628211) & 0xFFFFFFFFFFFFFFFF
        out[str(seed)] = {"first": [f(v) for v in z[:32]], "n": len(z), "fnv_every7": str(h),
                          "sum": f(z.sum()), "max": f(z.max()), "min": f(z.min())}
    dump("rng.json", out)


# -- drums --------------------------------------------------------------------
def word():
    n = int(0.5 * SR)
    t = np.arange(n) / SR
    r1 = np.random.default_rng(5).standard_normal(n)
    r2 = np.random.default_rng(6).standard_normal(n)
    burst = 0.6 * np.exp(-t / 0.01) * r1
    vowel = 0.4 * np.sin(2 * np.pi * 140.0 * t) * (t > 0.05) * np.exp(-(t - 0.05).clip(0) / 0.2)
    hiss = 0.3 * ((t > 0.25) & (t < 0.4)) * r2
    x = np.concatenate([np.zeros(int(0.03 * SR)), burst + vowel + hiss]).astype(np.float32)
    return x


def gen_drums():
    from app.ytpmv import drums
    x = word()
    out = {"input": {"len": len(x)}, "onset": int(drums.onset(x))}
    hiss = drums.find_hiss(x)
    out["hiss"] = list(hiss) if hiss else None
    share, level = drums.hiss_profile(x)
    out["profile"] = {"share": stats(share), "level": stats(level)}
    for g in drums.GROUPS:
        y, start = drums.shape(x, g)
        out[g] = {"start": int(start), **stats(y)}
    dump("drums.json", out)


# -- pitch --------------------------------------------------------------------
def glide():
    n = int(0.4 * SR)
    t = np.arange(n) / SR
    f0 = 140.0 + 40.0 * t / 0.4 + 3.0 * np.sin(2 * np.pi * 5 * t)
    ph = 2 * np.pi * np.cumsum(f0) / SR
    x = sum(a * np.sin(k * ph) for k, a in ((1, 1.0), (2, 0.5), (3, 0.3), (4, 0.15)))
    env = np.minimum(1.0, t / 0.03) * np.minimum(1.0, (0.4 - t) / 0.05)
    x = 0.4 * x * env / np.abs(x).max()
    return np.concatenate([np.zeros(int(0.02 * SR)), x]).astype(np.float32)


def gen_pitch():
    from app.ytpmv import pitch as p
    x = glide()
    v = p.Voice(x)
    out = {"input": {"len": len(x)},
           "info": {"f0": v.info.f0, "note": v.info.note, "voiced_ratio": v.info.voiced_ratio,
                    "stability": v.info.stability},
           "marks": {"n": int(len(v.marks)), "first": [int(m) for m in v.marks[:24]],
                     "periods": [f(q) for q in v.periods[:24]], "voiced_count": int(v.mvoiced.sum())},
           "v0": v.v0, "v1": v.v1, "peak": list(v.peak()) if v.peak() else None}
    tr = p.track_f0(x)
    out["track"] = {"f0": stats(tr.f0), "rms": stats(tr.rms), "voiced": int(tr.voiced.sum()),
                    "frames": int(len(tr.f0))}
    cases = [("perfect_hold", 330.0, 0.9, "perfect", 0.0, True),
             ("perfect_short", 262.0, 0.2, "perfect", 0.0, True),
             ("perfect_nostretch", 196.0, 0.9, "perfect", 0.0, False),
             ("tape", 220.0, 0.5, "tape", 0.0, True),
             ("raw", None, 0.6, "raw", 5.0, True)]
    out["renders"] = {}
    for name, hz, dur, mode, semis, stretch in cases:
        y, w = v.render(hz, dur, mode, semis, stretch)
        out["renders"][name] = {"hz": hz, "dur": dur, "mode": mode, "semis": semis, "stretch": stretch,
                                "warp": {"v0": w.v0, "v1": w.v1, "s": w.s, "rate": w.rate, "hold": w.hold, "P": w.P},
                                **stats(y)}
    dump("pitch.json", out)


# -- music --------------------------------------------------------------------
def gen_music():
    import mido
    from app.ytpmv import music
    mid = mido.MidiFile(type=1, ticks_per_beat=480)
    t0 = mido.MidiTrack()
    t0.append(mido.MetaMessage("set_tempo", tempo=500000, time=0))
    t0.append(mido.MetaMessage("time_signature", numerator=3, denominator=4, time=0))
    t0.append(mido.MetaMessage("track_name", name="Conductor", time=0))
    t0.append(mido.MetaMessage("set_tempo", tempo=400000, time=1920))
    mid.tracks.append(t0)

    def melody(program, notes, channel, name, pan=None):
        tr = mido.MidiTrack()
        tr.append(mido.MetaMessage("track_name", name=name, time=0))
        tr.append(mido.Message("program_change", program=program, channel=channel, time=0))
        if pan is not None:
            tr.append(mido.Message("control_change", control=10, value=pan, channel=channel, time=0))
        for (pitch, start, dur) in notes:
            pass
        events = []
        for (pitch, start, dur) in notes:
            events.append((start, 1, pitch))
            events.append((start + dur, 0, pitch))
        events.sort()
        last = 0
        for tick, on, pitch in events:
            tr.append(mido.Message("note_on" if on else "note_off", note=pitch, velocity=90 if on else 0,
                                   channel=channel, time=tick - last))
            last = tick
        mid.tracks.append(tr)

    # lead: a tune in D minor-ish, single notes
    tune = [(74, 0, 240), (77, 240, 240), (81, 480, 480), (79, 960, 240), (77, 1200, 240), (74, 1440, 480),
            (72, 1920, 240), (74, 2160, 240), (77, 2400, 480), (76, 2880, 480), (74, 3360, 480)]
    melody(80, tune, 0, "", pan=20)
    # bass: GM bass program on low D/A
    bass = [(38, 0, 480), (45, 480, 480), (38, 960, 480), (45, 1440, 480), (36, 1920, 480), (43, 2400, 480),
            (38, 2880, 960)]
    melody(33, bass, 1, "Bass", pan=64)
    # chords: three-note stacks
    chords = []
    for i, root in enumerate((50, 53, 57, 55)):
        for iv in (0, 3, 7):
            chords.append((root + iv, i * 960, 900))
    melody(48, chords, 2, "Pad")
    # an unreleased stuck note
    tr = mido.MidiTrack()
    tr.append(mido.Message("program_change", program=0, channel=3, time=0))
    tr.append(mido.Message("note_on", note=60, velocity=70, channel=3, time=0))
    mid.tracks.append(tr)
    # drums on channel 9
    dr = mido.MidiTrack()
    dr.append(mido.MetaMessage("track_name", name="Kit", time=0))
    ev = []
    for beat in range(8):
        ev.append((beat * 480, 36 if beat % 2 == 0 else 38))
        ev.append((beat * 480 + 240, 42))
    ev.append((3780, 49))
    ev.sort()
    last = 0
    for tick, n in ev:
        dr.append(mido.Message("note_on", note=n, velocity=100, channel=9, time=tick - last))
        dr.append(mido.Message("note_off", note=n, velocity=0, channel=9, time=60))
        last = tick + 60
    mid.tracks.append(dr)

    buf = io.BytesIO()
    mid.save(file=buf)
    data = buf.getvalue()
    with open(os.path.join(OUT, "song.mid"), "wb") as fh:
        fh.write(data)

    song = music.parse(data, "fixture")
    parts = []
    for p in song.parts:
        s = p.summary()
        s["notes_head"] = [[n.start, n.dur, n.pitch, n.velocity] for n in p.notes[:6]]
        s["program"] = p.program
        s["track"] = p.track
        s["channel"] = p.channel
        parts.append(s)
    dump("music.json", {"duration": song.duration, "bpm": song.bpm, "time_signature": list(song.time_signature),
                        "key": song.key, "key_confidence": song.key_confidence,
                        "tempo_changes": song.tempo_changes, "description": song.describe(), "parts": parts})


# -- master -------------------------------------------------------------------
def gen_master():
    from app.ytpmv import master
    out = {}
    rng = np.random.default_rng(99)
    n = 6 * SR
    t = np.arange(n) / SR
    mono = (0.2 * np.sin(2 * np.pi * 440 * t) * (np.sin(2 * np.pi * 0.7 * t) > -0.3)).astype(np.float64)
    out["block_power"] = stats(master.block_power(mono))
    out["loudness"] = master.loudness(mono)
    stereo = np.stack([mono, 0.5 * mono + 0.02 * rng.standard_normal(n)], axis=1)
    out["block_power_stereo"] = stats(master.block_power(stereo))
    out["loudness_stereo"] = master.loudness(stereo)
    out["k_weight"] = stats(master.k_weight(mono[:SR]))

    parts = []
    for i, (role, amp) in enumerate((("lead", 0.2), ("bass", 0.3), ("chords", 0.05), ("drums:kick", 0.4),
                                      ("rhythm", 0.1), ("rhythm", 0.12))):
        env = (np.sin(2 * np.pi * (0.3 + 0.11 * i) * t) > -0.2).astype(float)
        x = amp * env * np.sin(2 * np.pi * (110 * (i + 1)) * t)
        parts.append({"id": f"p{i}", "role": role, "power": master.block_power(x), "trim_db": 1.5 if i == 2 else 0.0})
    bal = master.balance(parts)
    out["balance"] = {k: {"lufs": f(v[0]), "gain": f(v[1]), "ride": stats(v[2])} for k, v in bal.items()}
    out["balance_inputs"] = [{"id": p["id"], "role": p["role"], "trim_db": p["trim_db"], "power": stats(p["power"])}
                             for p in parts]
    out["gain_for"] = [[m, r, f(master.gain_for(m, r, 0.0, -20.0, s))]
                       for m, r, s in ((-30.0, "lead", 1.0), (-14.0, "bass", 0.5), (-22.0, "drums:hats", 1.0),
                                       (float("-inf"), "lead", 1.0))
                       ] if False else []
    # limiter on a hot mix
    hot = (1.6 * np.sin(2 * np.pi * 200 * t[: 2 * SR])).astype(np.float64)
    hot[SR:SR + 200] *= 3.0
    lim = hot.copy()
    master.limit(lim)
    out["limit"] = {"in": stats(hot), "out": stats(lim)}
    env = master.envelope(np.array([1.0, 0.5, 0.25]), 100, 40000)
    out["envelope"] = stats(env)
    dump("master.json", out)


# -- numpy semantics ----------------------------------------------------------
def gen_numpy():
    rng = np.random.default_rng(3)
    out = {"sums": []}
    for n in (1, 7, 8, 9, 100, 127, 128, 129, 130, 1000, 4097, 22050):
        a32 = (rng.standard_normal(n) * 0.37).astype(np.float32)
        a64 = a32.astype(np.float64) * 1.000001
        out["sums"].append({"n": n, "f32_sum": float(a32.sum()), "f32_mean": float(a32.mean()),
                            "f64_sum": float(a64.sum()), "f64_mean": float(a64.mean()),
                            "f64_std": float(a64.std())})
    out["linspace"] = [{"a": a, "b": b, "n": n, "v": [float(v) for v in np.linspace(a, b, n)]}
                       for a, b, n in ((0.0, 1.0, 7), (1.0, 0.0, 5), (0.0, 1.0, 1), (-1.0, 1.0, 10))]
    out["hanning"] = [float(v) for v in np.hanning(9)]
    xp = np.array([0.0, 1.0, 2.5, 4.0, 10.0])
    fp = np.array([1.0, 3.0, 2.0, 8.0, -1.0])
    xs = np.array([-1.0, 0.0, 0.5, 1.0, 2.0, 2.5, 3.0, 9.99, 10.0, 12.0])
    out["interp"] = {"xp": xp.tolist(), "fp": fp.tolist(), "x": xs.tolist(), "y": np.interp(xs, xp, fp).tolist()}
    y = (rng.standard_normal(11)).astype(np.float32)
    pos = np.array([0.0, 0.25, 3.5, 9.99, 10.0, 11.0, -2.0])
    out["interp_arange"] = {"y": [float(v) for v in y], "pos": pos.tolist(),
                            "out": np.interp(pos, np.arange(len(y)), y).tolist()}
    out["arange"] = [{"stop": s, "step": st, "n": int(len(np.arange(0.0, s, st)))}
                     for s, st in ((9.0, 0.7), (100.0, 1.3), (10.0, 2.0), (21.0, 0.8409))]
    out["rfft"] = {"x": [float(v) for v in a64[:16]] if False else [],}
    dump("numpy.json", out)


def gen_fft32():
    rng = np.random.default_rng(17)
    out = []
    for n in (2, 4, 8, 16, 32, 64, 128, 256, 1024, 4096, 8192):
        for n_in in (n, max(1, n - 3), max(1, n // 3)):
            x = (rng.standard_normal(n_in) * 0.5).astype(np.float32)
            spec = np.fft.rfft(x, n)
            assert spec.dtype == np.complex64
            re = (spec.real.astype(np.float32) + np.float32(0)).view(np.uint32)
            im = (spec.imag.astype(np.float32) + np.float32(0)).view(np.uint32)
            h = 1469598103934665603
            for b in list(re.tolist()) + list(im.tolist()):
                h = ((h ^ b) * 1099511628211) & 0xFFFFFFFFFFFFFFFF
            out.append({"n": n, "n_in": n_in, "seed_pos": len(out), "hash": str(h),
                        "re": [int(v) for v in re[:12]], "im": [int(v) for v in im[:12]]})
    dump("fft32.json", out)


if __name__ == "__main__":
    wanted = sys.argv[1:] or ["rng", "drums", "pitch", "music", "master", "numpy", "fft32"]
    for name in wanted:
        print("[" + name + "]")
        globals()["gen_" + name]()
    print("wrote", OUT)
