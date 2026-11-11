#!/usr/bin/env python3
"""Capture golden parity fixtures from the Python app.

The Rust port in this repo is checked against the behaviour of the Python
implementation in ``<python app>``.  This script runs the
Python code on fixed inputs and writes the results to ``tests/golden/`` so the
port can be compared byte-for-byte, number-for-number, sample-for-sample.

The Python tree is READ-ONLY: nothing here writes to it, and the app is used
through its public entry points plus the same monkey-patching the old test
suite uses (``tests/test_generate_options.py`` stubs the corpus so the
arithmetic can be tested without media).

Usage::

    python tools/golden/capture.py              # everything
    python tools/golden/capture.py tokens pitch # just those sections

Determinism: every section seeds ``random`` and numpy's generator from a
constant before use, loads no corpus and touches no network, so two runs
produce identical bytes.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import sys
import tempfile
import wave

# ── Paths ─────────────────────────────────────────────────────────────────────
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))            # ytp-machine-rs
OUT = os.path.join(REPO, "tests", "golden")
AUDIO = os.path.join(OUT, "audio")
PY_APP = r"<python app>"

# An empty data directory, so nothing consults (or writes to) a real corpus.
# The tokeniser still uses the bundled CMU dictionary through nltk, which is
# what ``tests/test_tokenize.py`` relies on too.
NO_CORPUS = os.path.join(tempfile.mkdtemp(prefix="ytp-golden-"), "data")
os.makedirs(NO_CORPUS, exist_ok=True)
os.environ["MRS_DATA_DIR"] = NO_CORPUS

sys.path.insert(0, PY_APP)

import numpy as np  # noqa: E402

SEED = 20260913
random.seed(SEED)
np.random.seed(SEED)

FIXTURES: list[str] = []


# ── JSON helpers ──────────────────────────────────────────────────────────────
def _clean(obj, nd: int = 9):
    """JSON-safe copy of *obj*, with every float rounded to *nd* digits.

    Deterministic serialisation is part of the fixture: rounding keeps a last
    bit of floating-point noise from making a fixture depend on the CPU.
    """
    if isinstance(obj, dict):
        return {str(k): _clean(v, nd) for k, v in sorted(obj.items(), key=lambda kv: str(kv[0]))}
    if isinstance(obj, (list, tuple)):
        return [_clean(v, nd) for v in obj]
    if isinstance(obj, (bool, str)) or obj is None:
        return obj
    if isinstance(obj, (np.bool_,)):
        return bool(obj)
    if isinstance(obj, (np.integer, int)):
        return int(obj)
    if isinstance(obj, (np.floating, float)):
        f = float(obj)
        if f != f or f in (float("inf"), float("-inf")):
            return None
        return round(f, nd)
    return obj


def _write_json(name: str, payload) -> str:
    path = os.path.join(OUT, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        json.dump(_clean(payload), fh, indent=2, sort_keys=True, ensure_ascii=False)
        fh.write("\n")
    FIXTURES.append(os.path.relpath(path, OUT))
    return path


def _write_wav(name: str, samples, channels: int = 1) -> str:
    """16-bit PCM WAV, peak-clipped, no dithering: byte-stable across runs."""
    path = os.path.join(AUDIO, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    a = np.asarray(samples, dtype=np.float64)
    pcm = np.clip(np.round(a * 32767.0), -32768, 32767).astype("<i2")
    with wave.open(path, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(int(SR))
        w.writeframes(pcm.tobytes())
    size = os.path.getsize(path)
    if size > 200 * 1024:
        raise SystemExit(f"{name} is {size / 1024:.0f} KB, over the 200 KB budget")
    FIXTURES.append(os.path.relpath(path, OUT))
    return path


def _stats(x) -> dict:
    a = np.asarray(x, dtype=np.float64)
    if a.size == 0:
        return {"samples": 0, "peak": 0.0, "rms": 0.0, "dc": 0.0}
    return {
        "samples": int(a.size),
        "peak": float(np.abs(a).max()),
        "rms": float(np.sqrt(np.mean(a * a))),
        "dc": float(a.mean()),
    }


def _sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ── 1. Tokeniser / normalisation ──────────────────────────────────────────────
# Twenty sentences covering what the tokeniser is opinionated about: numbers,
# units, decades, rates, decimals, thousands separators, contractions,
# apostrophes, hyphens, markup, held letters and the reverse marker.
SENTENCES = [
    "Hello, world!",
    "It's Michael Rosen's birthday.",
    "I'm 30 years old.",
    "The 2.0 litre engine costs $50.",
    "He ran 50km in 1st place.",
    "The 1980s were different from the 90s.",
    "It drinks 3.5l per 100km.",
    "Don't touch that - it's hot!",
    'She said "no way".',
    "y'know, it's a four-cylinder turbo-charged car.",
    "MP3s and 4x4s from the 80s.",
    "The 22nd of May, 1,500 people came.",
    "100kg at 80kmh is fine.",
    "km/h and l/100km are rates.",
    "v8 mk2 330i i30.",
    "Wait... what? Really?!",
    "sooo gooood, loooong time.",
    "say ~hello~ now, then ~i30~.",
    "Mr. Smith's dog died. Then what?",
    "Numbers: 0 1 2 42 1000 3.14 50% 5s.",
]

# The token rules, as cases rather than sentences -- the same list the old
# tests use, since each one was once wrong.
TOKEN_CASES = [
    "rosen's", "dad's", "dads'", "don't", "they're", "i've", "y'know",
    "i30", "v8", "mk2", "330i", "4x4",
    "km", "50km", "100kg", "50mm", "2l",
    "80s", "90s", "1980s", "5s",
    "km/h", "80kmh", "l/100km", "and/or",
    "2.0", "3.5l", "1,500", "$50", "50%",
    "four-cylinder", "turbo-charged", "1st", "22nd",
    "hello", "", "...",
]


def capture_tokens() -> str:
    from app import generate as g

    sentences = []
    for text in SENTENCES:
        marked = g.tokenize_marked(text)
        full = g.tokenize_full(text)
        sentences.append({
            "text": text,
            "marked": [list(row) for row in marked],
            "full": [
                {
                    "word": t["word"],
                    "ends": bool(t["ends"]),
                    "noise": bool(t["noise"]),
                    "reverse": bool(t["reverse"]),
                    "stretch": [list(s) for s in (t.get("stretch") or [])],
                    "shown": t.get("shown"),
                    "note": t.get("note"),
                    "fx": t.get("fx") or {},
                }
                for t in full
            ],
        })

    cases = [{"token": tok, "expanded": g._expand_token(tok)} for tok in TOKEN_CASES]

    return _write_json("tokens.json", {
        "python_module": "app.generate",
        "seed": SEED,
        "sentences": sentences,
        "cases": cases,
    })


# ── 2. Phonemes ───────────────────────────────────────────────────────────────
WORDS = [
    "hello", "world", "michael", "rosen", "machine", "rust", "port", "golden",
    "parity", "fixture", "the", "a", "and", "because", "through", "thought",
    "thirty", "hundred", "thousand", "kilometres", "kilograms", "millimetres",
    "litres", "dollars", "percent", "engine", "cylinder", "turbo", "charged",
    "birthday", "different", "people", "numbers", "measure", "sentence",
    "pronunciation", "dictionary", "synthetic", "corpus", "splice", "phoneme",
    "voice", "sample", "pitch", "audio", "rhythm", "kitten", "squirrel",
    "strength", "twelfths", "acknowledge", "worcestershire", "quay", "zebra",
    "tomato", "schedule", "aluminium", "colour", "favourite",
]

NONSENSE = ["blorptastic", "zibble", "frobnify", "quuxly", "snazzle"]


def capture_phonemes() -> str:
    from app import phonemes as ph

    words = []
    for w in WORDS:
        phone_list = ph.word_to_phonemes(w)
        words.append({
            "word": w,
            "phonemes": list(phone_list) if phone_list else None,
            "phones_of": list(ph.phones_of(w) or []) if phone_list else None,
        })

    guesses = []
    for w in NONSENSE:
        try:
            guesses.append({"word": w, "guess": list(ph.guess_phonemes(w) or [])})
        except Exception as exc:                     # pragma: no cover - diagnostic
            guesses.append({"word": w, "error": f"{type(exc).__name__}: {exc}"})

    return _write_json("phonemes.json", {
        "python_module": "app.phonemes",
        "dictionary": "nltk cmudict (bundled), American pronunciations",
        "words": words,
        "nonsense": guesses,
    })


# ── 3. Splice / selection plans ───────────────────────────────────────────────
# A tiny synthetic corpus: one source file, words laid out in a line, each with
# its own stored span. Built the way tests/test_generate_options.py builds its
# pretend corpus -- no media, no database.
CORPUS_WORDS = [
    "so", "long", "good", "too", "soon", "no", "way", "stop", "yes",
    "cool", "hmm", "god", "hello", "world", "the", "and", "i", "thirty",
]
SOURCE = r"D:\synthetic-corpus\rosen-take-01.mp4"

PLAN_SENTENCES = [
    "so long, good.",
    "too soon. no way!",
    "stop. yes, cool hmm.",
    "hello world, the 30 and i.",
    "say ~no~ now",       # not in the corpus vocabulary? "now" added below
]


def _synth_corpus() -> dict:
    settings = {"splice_mode": "strict", "max_units": 8}
    cbw: dict[str, list[dict]] = {}
    words = CORPUS_WORDS + ["now", "say"]
    for i, w in enumerate(words):
        clips = []
        # Two takes per word, at different offsets in the same source, so the
        # selector has a real choice to make.
        for take in range(2):
            start = 1.0 + 2.0 * i + 5.0 * take
            dur = round(0.28 + 0.011 * i + 0.03 * take, 6)
            clips.append({
                "word": w,
                "id": 1000 + 10 * i + take,
                "source_file": SOURCE,
                "source_id": 1,
                "start_time": start,
                "end_time": start + dur,
                "prev_end": start - 0.05,
                "next_start": start + dur + (0.06 if take == 0 else 0.03),
                "quality": 0.9,
                "tone": None,
            })
        cbw[w] = clips
    return {"clips_by_word": cbw, "settings": settings, "slug": "synthetic"}


def capture_splice() -> str:
    import app.generate as g

    corpus = _synth_corpus()
    cbw = corpus["clips_by_word"]

    # The corpus's own tables would be read from a database; supply them.
    g._ensure_cache = lambda: None
    g._clips_by_word_cache = cbw
    g._use_voice = lambda slug: {"settings": corpus["settings"], "slug": corpus["slug"]}
    g._pick_idle = lambda duration: None      # no silent footage in a fake corpus
    g._penalty_for = lambda word: None
    g._sound_ends = lambda source_file, start, stored_end: stored_end
    g._ordered_by_source = {1: []}
    g._word_positions = {}

    options = {"phrases": False}              # runs need a real source ordering
    plans = []
    for text in PLAN_SENTENCES:
        random.seed(SEED)                     # selection uses random.choices
        segments, report = g.resolve_text(text, None, options)
        timeline = g.timeline(segments, g.generation_options(options))
        plans.append({
            "text": text,
            "units": [
                {
                    "word": s.get("word"),
                    "source_file": s.get("source_file"),
                    "source_id": s.get("source_id"),
                    "clip_id": s.get("id"),
                    "start_time": s.get("start_time"),
                    "end_time": s.get("end_time"),
                    "span": (float(s["end_time"]) - float(s["start_time"]))
                            if s.get("start_time") is not None and s.get("end_time") is not None else None,
                    "prev_end": s.get("prev_end"),
                    "next_start": s.get("next_start"),
                    "pause_after": s.get("pause_after"),
                    "reverse": bool(s.get("reverse")),
                    "token": s.get("_tok"),
                }
                for s in segments
            ],
            "report": report,
            "timeline": timeline,
        })

    return _write_json("splice.json", {
        "python_module": "app.generate.resolve_text + app.generate.timeline",
        "seed": SEED,
        "source_file": SOURCE,
        "corpus": {
            "settings": corpus["settings"],
            "words": [
                {"word": w, "clips": cbw[w]} for w in sorted(cbw)
            ],
        },
        "options": g.generation_options(options),
        "plans": plans,
    })


# ── 4. Chip synths, drums and master ─────────────────────────────────────────
from app.ytpmv.pitch import SR, midi_to_hz  # noqa: E402

NOTE_SEQ = [48, 55, 60, 64]                 # C3 G3 C4 E4
NOTE_SECONDS = 0.18
INPUT_PEAK = 0.7


def _render_notes(tone_name: str, notes, seconds: float, *, program=None,
                  key=None, level: float = 1.0) -> np.ndarray:
    """Play a fixed note sequence through the app's own render path."""
    from app.ytpmv import tone as tone_mod

    n = int(round(seconds * SR))
    chunks = []
    for midi in notes:
        y = np.full(n, INPUT_PEAK, dtype=np.float32)
        chunks.append(tone_mod.apply(y, tone_name, hz=midi_to_hz(midi),
                                     level=level, program=program, key=key))
    return np.concatenate(chunks)


def _audio_entry(name: str, tone_name: str, notes, seconds: float, x,
                 extra: dict) -> dict:
    path = _write_wav(name + ".wav", x)
    st = _stats(x)
    entry = {
        "name": name,
        "tone": tone_name,
        "sample_rate": int(SR),
        "notes": list(notes) if notes is not None else None,
        "note_seconds": seconds,
        "input_peak": INPUT_PEAK,
        "samples": st["samples"],
        "duration": st["samples"] / float(SR),
        "peak": st["peak"],
        "rms": st["rms"],
        "dc": st["dc"],
        "sha256": _sha256(path),
        "wav": os.path.relpath(path, OUT),
    }
    entry.update(extra)
    return entry


CHIPS = [
    # (name, tone, notes, seconds, extra)
    ("nes", "pulse", NOTE_SEQ, NOTE_SECONDS, {}),
    ("gb", "gb-pulse", NOTE_SEQ, NOTE_SECONDS, {}),
    ("sid", "sid-lead", NOTE_SEQ, NOTE_SECONDS, {"model": "6581"}),
    ("opl2", "opl2", NOTE_SEQ, NOTE_SECONDS, {"program": 0, "opl3": False}),
    ("opl3", "opl3", NOTE_SEQ, NOTE_SECONDS, {"program": 0, "opl3": True}),
    ("ym2612", "lead", NOTE_SEQ, NOTE_SECONDS, {"patch": "lead"}),
    ("sms", "psg", NOTE_SEQ, NOTE_SECONDS, {}),
    # The SNES leaves its echo ringing for seconds after the note, so it gets a
    # shorter sequence and a documented cut-off to stay inside the budget.
    ("snes", "snes", [48, 60], 0.05, {"program": 0, "truncate_seconds": 1.0}),
]

DRUM_PATTERN = [
    ("md-kick", 36, 0.15),
    ("nes-snare", 38, 0.12),
    ("sms-hat", 42, 0.08),
    ("gb-kick", 36, 0.12),
    ("sid-cymbal", 49, 0.15),
    ("md-snare", 38, 0.12),
]


def capture_audio() -> str:
    from app.ytpmv import master

    entries = []
    for name, tone_name, notes, seconds, extra in CHIPS:
        x = _render_notes(tone_name, notes, seconds, program=extra.get("program"))
        if extra.get("truncate_seconds"):
            x = x[: int(round(extra["truncate_seconds"] * SR))]
            extra = dict(extra, captured_seconds=len(x) / float(SR))
        entries.append(_audio_entry(name, tone_name, notes, seconds, x, extra))

    # Drums: each voice takes a flat buffer and synthesises its own hit.
    from app.ytpmv import tone as tone_mod
    chunks = []
    for tone_name, key, seconds in DRUM_PATTERN:
        n = int(round(seconds * SR))
        y = np.full(n, 0.8, dtype=np.float32)
        chunks.append(tone_mod.apply(y, tone_name, hz=None, key=key))
    drums_x = np.concatenate(chunks)
    entries.append(_audio_entry("drums", "pattern", [k for (_t, k, _s) in DRUM_PATTERN],
                                 None, drums_x,
                                 {"pattern": [{"tone": t, "key": k, "seconds": s}
                                              for t, k, s in DRUM_PATTERN]}))

    # Master: two detuned sines with a spike, run through the real mastering
    # chain (balance-free finish: EQ, limiter, loudness target).
    t = np.arange(int(0.5 * SR)) / float(SR)
    mix = np.stack([
        0.30 * np.sin(2 * np.pi * 220.0 * t),
        0.30 * np.sin(2 * np.pi * 330.0 * t),
    ], axis=1)
    mix[2000:2050] = 0.99
    pre = _stats(mix)
    master.finish(mix)
    path = _write_wav("master.wav", mix, channels=2)
    st = _stats(mix)
    entries.append({
        "name": "master",
        "tone": "master.finish",
        "sample_rate": int(SR),
        "channels": 2,
        "samples": st["samples"],
        "frames": st["samples"] // 2,
        "duration": (st["samples"] // 2) / float(SR),
        "peak_before": pre["peak"],
        "rms_before": pre["rms"],
        "peak": st["peak"],
        "rms": st["rms"],
        "dc": st["dc"],
        "loudness_lufs": float(master.loudness(mix)),
        "target_lufs": float(master.TARGET_LUFS),
        "ceiling": float(master.CEILING),
        "sha256": _sha256(path),
        "wav": os.path.relpath(path, OUT),
    })

    return _write_json("audio.json", {
        "python_module": "app.ytpmv.tone.apply / master.finish",
        "sample_rate": int(SR),
        "note_sequence": NOTE_SEQ,
        "note_seconds": NOTE_SECONDS,
        "input_peak": INPUT_PEAK,
        "entries": entries,
    })


# ── 5. Pitch ─────────────────────────────────────────────────────────────────
def _detect_summary(info) -> dict:
    return {
        "f0": info.f0,
        "note": info.note,
        "stability": info.stability,
        "voiced_ratio": info.voiced_ratio,
    }


def capture_pitch() -> str:
    from app.ytpmv import pitch as p

    sr = p.SR
    t = np.arange(int(0.5 * sr)) / float(sr)
    sine = (0.4 * np.sin(2 * np.pi * 220.0 * t)).astype(np.float32)

    info = p.detect_pitch(sine)
    track = p.track_f0(sine)
    voiced = np.asarray(track.voiced, dtype=bool)
    f0 = np.asarray(track.f0, dtype=np.float64)

    noise = (np.random.default_rng(SEED).standard_normal(sr // 2) * 0.2).astype(np.float32)
    noise_info = p.detect_pitch(noise)

    # The resynthesis path, on the same fixed input: a perfect shift and a tape
    # shift onto A3.
    voice = p.Voice(sine)
    target = p.midi_to_hz(57)
    perfect, perfect_marks = voice.render(target, 0.3, "perfect")
    tape, tape_marks = voice.render(target, 0.3, "tape")

    return _write_json("pitch.json", {
        "python_module": "app.ytpmv.pitch",
        "sample_rate": int(sr),
        "input": {
            "kind": "sine",
            "hz": 220.0,
            "amplitude": 0.4,
            "seconds": 0.5,
            "samples": int(sine.size),
        },
        "detect": _detect_summary(info),
        "track": {
            "frames": int(f0.size),
            "voiced_frames": int(voiced.sum()),
            "median_f0_voiced": float(np.median(f0[voiced])) if voiced.any() else None,
            "min_f0_voiced": float(f0[voiced].min()) if voiced.any() else None,
            "max_f0_voiced": float(f0[voiced].max()) if voiced.any() else None,
        },
        "midi": {
            "midi_to_hz_57": float(p.midi_to_hz(57)),
            "hz_to_midi_220": float(p.hz_to_midi(220.0)),
            "note_name_57": p.note_name(57),
        },
        "noise": {
            "seed": SEED,
            "detect": _detect_summary(noise_info),
        },
        "render": {
            "target_midi": 57,
            "target_hz": float(target),
            "seconds": 0.3,
            "perfect": {
                "stats": _stats(perfect),
                "detect": _detect_summary(p.detect_pitch(perfect)),
                "hold": perfect_marks.hold,
                "rate": perfect_marks.rate,
            },
            "tape": {
                "stats": _stats(tape),
                "detect": _detect_summary(p.detect_pitch(tape)),
                "hold": tape_marks.hold,
                "rate": tape_marks.rate,
            },
        },
    })


# ── Manifest + main ──────────────────────────────────────────────────────────
SECTIONS = {
    "tokens": capture_tokens,
    "phonemes": capture_phonemes,
    "splice": capture_splice,
    "audio": capture_audio,
    "pitch": capture_pitch,
}


def _write_manifest() -> str:
    path = os.path.join(OUT, "manifest.json")
    rows = []
    for rel in sorted(set(FIXTURES)):
        full = os.path.join(OUT, rel)
        rows.append({"path": rel, "bytes": os.path.getsize(full), "sha256": _sha256(full)})
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        json.dump({"fixtures": rows, "seed": SEED}, fh, indent=2, sort_keys=True)
        fh.write("\n")
    return path


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("sections", nargs="*",
                    help="sections to capture: " + ", ".join(sorted(SECTIONS)))
    args = ap.parse_args(argv)
    unknown = [s for s in args.sections if s not in SECTIONS]
    if unknown:
        ap.error(f"unknown section(s): {', '.join(unknown)}")
    wanted = args.sections or list(SECTIONS)

    os.makedirs(OUT, exist_ok=True)
    os.makedirs(AUDIO, exist_ok=True)

    for name in wanted:
        print(f"[{name}] ...", flush=True)
        SECTIONS[name]()
        print(f"[{name}] ok", flush=True)

    if set(wanted) == set(SECTIONS):
        _write_manifest()
    print(f"wrote {len(set(FIXTURES))} files to {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
