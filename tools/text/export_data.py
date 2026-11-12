#!/usr/bin/env python3
"""Export the data tables of the Python app that ytp-text embeds.

Read-only against the Python tree: it imports app.phonemes and reads nltk's
bundled CMU dictionary. Writes:

  crates/ytp-text/data/cmudict.txt     word<TAB>PH PH PH  (first pronunciation, stress removed)
  crates/ytp-text/data/tables.json     overrides / spelled-out / said-as-word / letter names / recipes
  crates/ytp-text/tests/data/num2words.json   cardinal+ordinal fixtures for the num2words port

Usage: python tools/text/export_data.py
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
PY_APP = r"<python app>"
sys.path.insert(0, PY_APP)
os.environ.setdefault("MRS_DATA_DIR", os.path.join(os.environ.get("TEMP", "."), "ytp-export-data"))
os.makedirs(os.environ["MRS_DATA_DIR"], exist_ok=True)

from app import phonemes as ph  # noqa: E402

out_data = os.path.join(REPO, "crates", "ytp-text", "data")
out_tests = os.path.join(REPO, "crates", "ytp-text", "tests", "data")
os.makedirs(out_data, exist_ok=True)
os.makedirs(out_tests, exist_ok=True)

d = ph._dict()
with open(os.path.join(out_data, "cmudict.txt"), "w", encoding="utf-8", newline="\n") as fh:
    for word in sorted(d):
        fh.write(f"{word}\t{' '.join(d[word][0])}\n")

tables = {
    "overrides": ph._OVERRIDES,
    "spelled_out": sorted(ph._SPELLED_OUT),
    "said_as_word": ph._SAID_AS_WORD,
    "letter_names": ph._LETTER_NAMES,
    "recipes": {k: [[g, p] for g, p in v] for k, v in ph._RECIPES.items()},
}
with open(os.path.join(out_data, "tables.json"), "w", encoding="utf-8", newline="\n") as fh:
    json.dump(tables, fh, indent=1, sort_keys=True, ensure_ascii=False)
    fh.write("\n")

from num2words import num2words  # noqa: E402

nums = sorted(set(list(range(0, 1201)) + [
    1500, 1999, 2000, 2001, 2010, 2020, 2024, 2100, 9999, 10000, 10001, 12345, 99999, 100000,
    100001, 101000, 110000, 999999, 1000000, 1000001, 1100000, 2500000, 12345678, 100000000,
    1000000000, 1234567890, 10**12, 10**15 + 7, 10**18, 123456789012345678,
]))
with open(os.path.join(out_tests, "num2words.json"), "w", encoding="utf-8", newline="\n") as fh:
    json.dump([[n, num2words(n), num2words(n, to="ordinal")] for n in nums], fh)
    fh.write("\n")

# Markup the golden sentences do not cover: effects, notes, spelling, noises,
# held letters decided by a corpus (given as clip counts).
from app import generate as g  # noqa: E402

EXTRA = [
    "hello{pause=0.5,rate=0.8,pitch=2,vol=6,emph=strong} world",
    "hello{shake,shake,rainbow,boom,vocode,clip=12,v=james-channel} x",
    "usb{spell} rocks. a5{spell}!",
    "sing^+2 it^A3 now^c#4. hmm^-3",
    "*click* and *spew*! ~backwards~. ~done~?!",
    "soooo goooood noooo stoppp yesss hmmmm",
    "wait{pause=9,rate=0.1,pitch=-40,vol=abc} ok{emph=weird}",
    "3rd 21st 2020s 1990s 10s 7x7 $1,250.50 99% 4.5km",
    "l/100km mph km/h and/or a-b c/d",
    "Ünïcode wörds, naïve café 5€ £3",
]
CLIPS = {"good": 3, "goood": 1, "so": 2, "no": 4, "yes": 5, "stop": 2, "hmm": 1}
g._ensure_cache = lambda: None
g._clips_by_word_cache = {w: [{}] * n for w, n in CLIPS.items()}
cases = []
for text in EXTRA:
    cases.append({
        "text": text,
        "full": [
            {"word": t["word"], "ends": bool(t["ends"]), "noise": bool(t["noise"]),
             "reverse": bool(t["reverse"]),
             "stretch": [list(s) for s in (t.get("stretch") or [])],
             "shown": t.get("shown"),
             "note": (list(t["note"]) if t.get("note") else None),
             "fx": t.get("fx") or {}}
            for t in g.tokenize_full(text)
        ],
    })
with open(os.path.join(out_tests, "markup.json"), "w", encoding="utf-8", newline="\n") as fh:
    json.dump({"clips": CLIPS, "cases": cases}, fh, indent=1, sort_keys=True, ensure_ascii=False)
    fh.write("\n")

print("words:", len(d), "numbers:", len(nums))
