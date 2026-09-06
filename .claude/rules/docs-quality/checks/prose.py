#!/usr/bin/env python3
"""Plain-English checks for documentation prose.

Covers DOC-PLAIN-01 (banned punctuation), DOC-PLAIN-02 (25-word sentence cap),
DOC-PLAIN-03 (5-sentence paragraph cap), DOC-PLAIN-05 (Flesch reading ease),
DOC-PLAIN-08 (chatbot artifacts), DOC-PLAIN-10 (tell density per 1,000 words),
DOC-PLAIN-11 (time-relative words), DOC-PLAIN-12 (marketing superlatives) and
DOC-PLAIN-13 (heading hygiene, only where markdownlint is absent).

Every text rule reads strip_prose.strip() output, which is DOC-PLAIN-04.
Flesch reading ease is computed here, not imported:
    206.835 - 1.015 * (words / sentences) - 84.6 * (syllables / words)
with syllables counted as vowel-group runs per word and a trailing silent-e
correction.

Per-type carve-outs read the doc_type and doc_tier declaration comment in the
page's first 12 lines. Every finding exits 1. Which rule ids block a merge is
the gate's decision, stated in the rule table's Severity column, not here.

Usage: prose.py [--root DIR] [PATH ...] [--format text|json] [--self-test]
Exit 0 clean, 1 findings, 2 usage or missing input.
"""

from __future__ import annotations

import os
import re
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import strip_prose as sp
from strip_prose import Finding

# --- thresholds and wordlists, each with the source that fixes it ------------

# GOV.UK clear-language guidance states both numbers.
SENTENCE_WORD_CAP = 25
PARAGRAPH_SENTENCE_CAP = 5

# DOC-PLAIN-05. Floor calibrated on the wave-2 corpus median of 49.0 across
# 249 pages, not on an aspirational 60. Scored types only, because reference
# and troubleshooting prose is identifier-dense by nature.
FLESCH_FLOOR = 50.0
FLESCH_TYPES = {"landing", "tutorial", "how-to", "explanation"}
# DOC-DISC-09's stub floor. Below it the sentence-length term dominates a tiny
# denominator: one wave-2 page scored 8.47 tell density on 118 words.
MIN_SCORED_WORDS = 300

# DOC-PLAIN-10. Uncalibrated default: no source states a validated threshold.
TELL_DENSITY_PER_1000 = 3.0

# DOC-PLAIN-01. Written as escapes so this file holds no banned mark itself.
BANNED_MARKS = {
    "\u2014": "em dash",
    "\u2013": "en dash",
    ";": "semicolon",
    "\u201c": "curly quote",
    "\u201d": "curly quote",
    "\u2018": "curly quote",
    "\u2019": "curly quote",
}
BANNED_RE = re.compile("[" + "".join(re.escape(c) for c in BANNED_MARKS) + "]")

# DOC-PLAIN-08. Chatbot leftovers plus the AI-authorship badge clause.
CHATBOT_RE = re.compile(
    r"I hope this helps|as an AI|as of my last update|knowledge cutoff|"
    r"let me know if|feel free to ask|contentReference|oaicite|\[cite: [0-9]|"
    r"AI-generated|AI-assisted|"
    r"written with (?:the )?(?:help|assistance) of (?:AI|Claude|ChatGPT|Copilot|Gemini)|"
    r"assisted by (?:AI|Claude|ChatGPT|Copilot|Gemini)",
    re.IGNORECASE,
)

# DOC-PLAIN-11. The list Google's developer documentation style guide publishes.
TIME_RE = re.compile(
    r"\b(as of this writing|currently|does not yet|eventually|in the future|"
    r"latest|newer|newest|now|older|presently|at present|soon)\b",
    re.IGNORECASE,
)
# Wave-2 sampling found about half of 398 fleet hits describe a resolved
# runtime value rather than a documentation claim. Those phrases are exempt.
RUNTIME_PHRASE_RE = re.compile(
    r"\b(?:currently|latest|newer|older|newest)\s+"
    r"(?:installed|resolved|cached|running|available|digest|tag|version|release)\b|"
    r"\bthe (?:latest|newest) (?:digest|tag|version|release|commit)\b",
    re.IGNORECASE,
)

# DOC-PLAIN-12. Asserted wordlist, no published guide states one.
MARKETING_RE = re.compile(
    r"\b(powerful|seamless(?:ly)?|revolutionary|game.chang\w*|supercharge\w*|"
    r"unlock\w*|empower\w*|cutting.edge|robust|effortless\w*)\b",
    re.IGNORECASE,
)
# Internal decision records weigh trade-offs. Five of eight wave-2 hits sat
# there and every one was a false positive.
MARKETING_EXEMPT_RE = re.compile(r"docs/(?:research|decisions)/")

# DOC-PLAIN-10. Single-word tells from Wikipedia's "Signs of AI writing"
# essay. "underscore" and "unlock" are deliberately absent: wave-2 read all
# 18 fleet hits and 12 of them were those two words used as ordinary
# technical nouns. "paradigm" is out for the same reason, 3 hits, 3 false.
# "realm" is out on the same evidence: it is the OCI auth field name, and it
# alone produced every hit on one 2,253-word fleet page.
TELLS = (
    "delve",
    "delves",
    "delving",
    "tapestry",
    "testament",
    "boast",
    "boasts",
    "boasting",
    "myriad",
    "plethora",
    "furthermore",
    "moreover",
    "paramount",
    "pivotal",
    "invaluable",
    "intricate",
)
TELL_RE = re.compile(r"\b(?:" + "|".join(TELLS) + r")\b", re.IGNORECASE)

VOWEL_GROUPS = re.compile(r"[aeiouy]+")
BOLD_ONLY = re.compile(r"^\s*(?:\*\*|__)(?P<t>[^*_].*?)(?:\*\*|__)\s*$")


def flesch(prose: str) -> float | None:
    """Flesch reading ease. Returns None when there is nothing to score."""
    words = sp.WORD.findall(prose)
    sentences = list(sp.iter_sentences(prose))
    if not words or not sentences:
        return None
    syllables = sum(_syllables(w) for w in words)
    return round(
        206.835 - 1.015 * (len(words) / len(sentences)) - 84.6 * (syllables / len(words)), 1
    )


def _syllables(word: str) -> int:
    word = word.lower()
    n = len(VOWEL_GROUPS.findall(word))
    if word.endswith("e") and n > 1 and not word.endswith("le"):
        n -= 1
    return max(n, 1)


def _markdownlint_present() -> bool:
    return bool(shutil.which("markdownlint-cli2") or shutil.which("markdownlint"))


def check(path: str, text: str, root: str) -> list[Finding]:
    decl = sp.declaration(text)
    doc_type = decl.get("doc_type")
    prose = sp.strip(text)
    lines = prose.split("\n")
    out: list[Finding] = []

    for line_no, line in enumerate(lines, start=1):
        for m in BANNED_RE.finditer(line):
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-PLAIN-01",
                    f"{BANNED_MARKS[m.group(0)]} in prose, rewrite the sentence",
                )
            )
        for m in CHATBOT_RE.finditer(line):
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-PLAIN-08",
                    f"chatbot artifact or authorship badge: {m.group(0)!r}",
                )
            )
        if doc_type != "changelog":
            for m in TIME_RE.finditer(line):
                if not RUNTIME_PHRASE_RE.search(line[max(0, m.start() - 20) : m.end() + 20]):
                    out.append(
                        Finding(
                            path,
                            line_no,
                            "DOC-PLAIN-11",
                            f"time-relative word {m.group(0)!r} goes stale, "
                            "name the version or the release",
                        )
                    )
        if not MARKETING_EXEMPT_RE.search(path.replace(os.sep, "/")):
            for m in MARKETING_RE.finditer(line):
                out.append(
                    Finding(
                        path,
                        line_no,
                        "DOC-PLAIN-12",
                        f"marketing superlative {m.group(0)!r}, state the fact the reader came for",
                    )
                )

    for line_no, sentence in sp.iter_sentences(prose):
        words = len(sp.WORD.findall(sentence))
        if words > SENTENCE_WORD_CAP:
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-PLAIN-02",
                    f"sentence of {words} words, cap is {SENTENCE_WORD_CAP} "
                    "(GOV.UK clear-language guidance)",
                )
            )

    for line_no, block in sp.iter_paragraphs(prose):
        count = len(list(sp.iter_sentences(block)))
        if count > PARAGRAPH_SENTENCE_CAP:
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-PLAIN-03",
                    f"paragraph of {count} sentences, cap is "
                    f"{PARAGRAPH_SENTENCE_CAP} (GOV.UK clear-language guidance)",
                )
            )

    total_words = sp.word_count(prose)
    if total_words >= MIN_SCORED_WORDS:
        hits = TELL_RE.findall(prose)
        density = 1000 * len(hits) / total_words
        if density > TELL_DENSITY_PER_1000:
            out.append(
                Finding(
                    path,
                    1,
                    "DOC-PLAIN-10",
                    f"{density:.1f} vocabulary tells per 1,000 words over a "
                    f"default of {TELL_DENSITY_PER_1000} (uncalibrated), "
                    "a human should read this page",
                )
            )
        if doc_type in FLESCH_TYPES:
            score = flesch(prose)
            if score is not None and score < FLESCH_FLOOR:
                out.append(
                    Finding(
                        path,
                        1,
                        "DOC-PLAIN-05",
                        f"Flesch reading ease {score} below the floor of "
                        f"{FLESCH_FLOOR} (wave-2 fleet median 49.0)",
                    )
                )

    if not _markdownlint_present():
        out += _headings(path, text)
    return out


def _headings(path: str, text: str) -> list[Finding]:
    """DOC-PLAIN-13 without markdownlint: MD001, MD025 and MD036 by hand.

    markdownlint.jsonc owns these three where the tool is installed. This
    fallback exists so the rule is never unchecked, per DOC-PLAIN-19.
    """
    out: list[Finding] = []
    fence: str | None = None
    previous = 0
    h1_count = 0
    for line_no, line in enumerate(text.split("\n"), start=1):
        m = sp.FENCE.match(line)
        if fence is not None:
            if m and m.group(2)[0] == fence[0]:
                fence = None
            continue
        if m:
            fence = m.group(2)
            continue
        head = sp.HEADING.match(line)
        if head:
            level = len(head.group(1))
            if previous and level > previous + 1:
                out.append(
                    Finding(
                        path,
                        line_no,
                        "DOC-PLAIN-13",
                        f"heading jumps from level {previous} to {level}, "
                        "skipped levels break the outline",
                    )
                )
            if level == 1:
                h1_count += 1
                if h1_count == 2:
                    out.append(
                        Finding(
                            path,
                            line_no,
                            "DOC-PLAIN-13",
                            "second top-level heading, a page has one",
                        )
                    )
            previous = level
            continue
        bold = BOLD_ONLY.match(line)
        if bold and not bold.group("t").rstrip().endswith((".", ":", "!", "?", ",")):
            out.append(
                Finding(
                    path,
                    line_no,
                    "DOC-PLAIN-13",
                    "bold line standing in for a heading, use a real heading",
                )
            )
    return out


def _nav_line_test() -> int:
    """Extra self-test proof for the DOC-PLAIN-02 Contents-line exemption.

    The fixture lives with strip_prose's own fixtures (it proves strip(),
    not a prose-only rule) so this reads it there directly, the same way
    strip_prose's own residue test reads a fixture outside its stem.
    """
    path = sp.HERE / "fixtures" / "strip_prose" / "pass-contents-line.md"
    hits = check(str(path), sp.read_text(str(path)), str(path.parent))
    ok = not hits
    print(f"{'ok  ' if ok else 'FAIL'} {path.name}: {len(hits)} findings, expected 0")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(sp.run_cli(__doc__ or "", check, "prose", extra_self_test=_nav_line_test))
