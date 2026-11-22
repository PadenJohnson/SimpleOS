#!/usr/bin/env python3
"""Build Solon's English spelling dictionary (assets/spell/en.dict) from SCOWL.

    curl -L -o scowl.tar.gz https://downloads.sourceforge.net/wordlist/scowl-2020.12.07.tar.gz
    tar xzf scowl.tar.gz
    python3 tools/spell/mkdict.py scowl-2020.12.07

Takes SCOWL's lists up to size 60 (what the usual en_US / en_GB spelling dictionaries use):
words, capitalised words, names, abbreviations and contractions, in the common ("english"),
American and British (-ise) spellings. Possessives ("abbey's") are left out: Solon accepts
"X's" whenever X is a word.

Output: zlib-compressed UTF-8, sorted, one word per line, each followed by one tag character
'A' + level * 3 + region, where level 0..6 is SCOWL size 10, 20, 35, 40, 50, 55, 60 (how common
the word is) and region is 0 everywhere, 1 American only, 2 British only.
"""
import os, sys, zlib

LEVELS = [10, 20, 35, 40, 50, 55, 60]
TYPES = ["words", "upper", "proper-names", "abbreviations", "contractions"]
REGIONS = [("english", 0), ("american", 1), ("british", 2)]

src = os.path.join(sys.argv[1], "final")
words = {}  # word -> (level, set of regions)
for li, level in enumerate(LEVELS):
    for cat, region in REGIONS:
        for t in TYPES:
            path = os.path.join(src, f"{cat}-{t}.{level}")
            if not os.path.exists(path):
                continue
            for w in open(path, encoding="latin-1").read().split():
                lv, regs = words.get(w, (li, set()))
                words[w] = (min(lv, li), regs | {region})

def region_of(regs):
    if 0 in regs or regs == {1, 2}:
        return 0
    return 1 if 1 in regs else 2

out = []
for w in sorted(words):
    if w.endswith("'s") and w[:-2] in words:
        continue
    lv, regs = words[w]
    out.append(w + chr(ord("A") + lv * 3 + region_of(regs)))
data = ("\n".join(out) + "\n").encode("utf-8")
here = os.path.dirname(os.path.abspath(__file__))
dest = os.path.join(here, "..", "..", "assets", "spell", "en.dict")
open(dest, "wb").write(zlib.compress(data, 9))
print(f"{len(out)} words, {len(data)} bytes, {os.path.getsize(dest)} compressed -> assets/spell/en.dict")
