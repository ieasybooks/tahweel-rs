# /// script
# requires-python = ">=3.13"
# dependencies = ["rapidfuzz==3.14.6"]
# ///
"""Replay the evaluation's text cleanup and per-page CER/WER scoring.

The clean, normalize, score and selftest functions are copied unchanged from
notes/defaults/evaluate.py. Reference text is the prepared ground truth;
hypothesis text is the exact UTF-8 Drive export. Rates are fractions.
RapidFuzz 3.14.6 is the evaluation's pinned scorer (MIT).
"""
import argparse
import json
from pathlib import Path
import re
import unicodedata

from rapidfuzz.distance import Levenshtein

HEADER = "\ufeff________________\r\n\r\n"


def clean(text):
    if text.startswith(HEADER):
        text = text[len(HEADER):]
    return text.removeprefix("\ufeff").replace("\r\n", "\n").strip()


def normalize(text):
    text = unicodedata.normalize("NFKC", text)
    text = re.sub(r"[\u0610-\u061a\u064b-\u065f\u0670\u06d6-\u06ed\u0640]", "", text)
    text = text.translate(str.maketrans("أإآٱؤئىة", "ااااوييه"))
    text = "".join(str(unicodedata.digit(c)) if c.isdecimal() else c for c in text)
    return " ".join(text.split())


def score(reference, hypothesis):
    def errors(ref, hyp):
        chars = Levenshtein.distance(ref, hyp) / max(1, len(ref))
        words = Levenshtein.distance(ref.split(), hyp.split()) / max(1, len(ref.split()))
        return chars, words
    cer, wer = errors(reference, hypothesis)
    ncer, nwer = errors(normalize(reference), normalize(hypothesis))
    return dict(cer=cer, wer=wer, normalized_cer=ncer, normalized_wer=nwer)


def selftest():
    assert clean(HEADER + "ألف\r\nباء\r\n") == "ألف\nباء"
    assert clean("________________\r\nhello") == "________________\nhello"
    assert normalize("إِنَّ الـفَتَى ١٢۳ ة ؤ ئ") == "ان الفتي 123 ه و ي"
    assert score("اب", "ات")["cer"] == 0.5
    assert score("أول ثان", "أول ثالث")["wer"] == 0.5
    assert score("", "")["cer"] == 0
    assert score("بَ", "ب")["normalized_cer"] == 0
    print("Cleanup/normalization/CER/WER self-tests passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reference", type=Path, nargs="?")
    parser.add_argument("export", type=Path, nargs="?")
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args()
    if args.selftest:
        selftest()
    else:
        if args.reference is None or args.export is None:
            parser.error("provide a reference and a Drive export, or --selftest")
        reference = args.reference.read_bytes().decode("utf-8")
        hypothesis = clean(args.export.read_bytes().decode("utf-8"))
        result = score(reference, hypothesis)
        # Empty references have no meaningful error percentage.
        if not reference:
            result.update(cer=None, wer=None)
        if not normalize(reference):
            result.update(normalized_cer=None, normalized_wer=None)
        print(json.dumps(dict(reference_chars=len(reference), output_chars=len(hypothesis), **result), indent=2))
