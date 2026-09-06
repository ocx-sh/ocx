---
title: Plain English for documentation
summary: The DOC-PLAIN rules, each with the check that runs it, the measured hit count behind it, and the page types it exempts
---

# Plain English for documentation

Plain English ships here as counted, greppable limits, not as a voice.
Every row below names a command, a threshold source, and what it measured.

Contents: [How the checks run](#how-the-checks-run) ·
[Sentence and paragraph shape](#sentence-and-paragraph-shape) ·
[Readability](#readability) ·
[Tells and honest labelling](#tells-and-honest-labelling) ·
[Structure](#structure) · [Links](#links) ·
[Tooling and scope](#tooling-and-scope) ·
[Shown, not told](#shown-not-told) ·
[Generator divergence](#generator-divergence) ·
[Pinned decisions](#pinned-decisions) · [Not studied](#not-studied)

## How the checks run

Every prose rule reads stripped prose, never raw file text.
`checks/strip_prose.py` removes front matter, declaration comments, fenced
code, code spans, link targets, images, tables, HTML, admonition markers and
include directives (`<<<`, `--8<--`, `{{#include`). It preserves line numbers.
`checks/prose.py` imports it and carries most of the arms below.

Page type comes from the `doc_type` comment in a page's first 12 lines, read by
`checks/doc_declaration.py`. Never from front matter. Never from a path.

The gate has two tiers. Tier 0 is mandatory and needs no new binary.

| Tier | Tools | Status |
|---|---|---|
| 0 | `checks/prose.py`, `checks/strip_prose.py`, grep, markdownlint with `checks/markdownlint.jsonc` | Mandatory. Every rule below resolves at this tier alone |
| 1 | Vale with `checks/vale.ini`, packages pinned by org and repo | Optional. Adds severity per rule and a heading-case check. No rule may rest on it for its only verification |

Rollout, where a row says so: the check runs at error on the lines a change
adds or edits. It warns whole-tree until the backfill lands. A rule launches
at error whole-tree only when the tree is already at zero violations.

## Sentence and paragraph shape

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-PLAIN-01 | Write documentation prose without em dashes, en dashes, semicolons or curly quotes. | Machine translation and terminal rendering both mangle these marks, which corrupts a command the reader copies. | `python3 checks/prose.py --root . PATH` reports no DOC-PLAIN-01 finding, equivalent to `python3 checks/strip_prose.py PATH \| grep -nP '[\x{2013}\x{2014};\x{201C}\x{201D}\x{2018}\x{2019}]'` returning nothing. House style for translation and terminal rendering, never a claim about authorship. Measured 8,462 hits over 229 of 249 pages (wave-2 calibration), so it runs on changed lines first. | SHOULD, pinned |
| DOC-PLAIN-02 | Split any prose sentence longer than 25 words. | A stacked-clause sentence makes the reader hold three ideas at once before any of them resolves. | `python3 checks/prose.py --root . PATH`. Threshold 25 words (GOV.UK clear-language guidance). The counter reads `strip_prose.py` output, so a link target is not counted as prose. Measured 4,674 long sentences over 211 of 249 pages, 2.6 percent of them false before the link-target strip (wave-2 calibration). Error on changed lines, warning whole-tree until the backfill lands. | MUST |
| DOC-PLAIN-03 | Keep every paragraph to 5 sentences or fewer. | A long block hides its own topic sentence, so a scanner has no entry point. | `python3 checks/prose.py --root . PATH`. Threshold 5 sentences (GOV.UK clear-language guidance). The counter discards bare `N.` list markers first. Measured 153 long paragraphs over 69 of 249 pages, 10.5 percent of them false before that fix (wave-2 calibration). | SHOULD |

## Readability

Flesch reading ease is `206.835 - 1.015*(words/sentences) - 84.6*(syllables/words)`,
computed on `strip_prose.py` output.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-PLAIN-04 | Compute every readability score on stripped prose, never on raw file text. | A code fence, a table row or a link target corrupts the sentence split, so the number means nothing. | `python3 checks/prose.py --self-test` passes, including the fixture that scores 59.6 raw and 48.8 stripped, a 10.8-point gap (wave-2 fixture). | MUST |
| DOC-PLAIN-05 | Keep a narrative page at Flesch reading ease 50 or above. | A page below the median of what the team already publishes is harder than everything around it. | `python3 checks/prose.py --root . PATH` reports the score against floor 50.0 (the corpus median, measured 51.6 over 186 pages and 49.0 over 249). Skips any page under 300 prose words, this family's own floor, because a tiny denominator dominates the sentence-length term (wave-2 calibration). Reports as a warning, never a red gate. | SHOULD |
| DOC-PLAIN-07 | Wrap every identifier in a code span when it appears in running prose. | A bare identifier shifts the score by accident of digit placement, and it renders as ordinary words. | `python3 checks/strip_prose.py PATH \| grep -nE '(^\|[[:space:]])(--[a-z]\|[A-Za-z_]+\(\)\|[A-Za-z_]+::)'`. Match only shapes ordinary English cannot produce: a leading `--`, a trailing `()`, a `::`, a `/`, or a term on a project-maintained identifier list. The broad wave-1 pattern is banned: it measured 1,621 hits over seven research files (wave-2 ledger) and 9,618 hits over 184 of 186 pages (wave-1 count), topped by `how-to` and ISO dates. Report the tightened pattern's count before wiring it. Under 50 is acceptable, over 200 is not. | CONSIDER |

The readability floor exempts pages declaring `doc_type: reference`,
`troubleshooting` or `changelog`. Every syllable formula breaks on identifiers,
and those three types are identifier-dense by design. The exemption is a
carve-out, not a laxer floor, because no source states a second number.

## Tells and honest labelling

The tell wordlist descends from Wikipedia's "Signs of AI writing" taxonomy,
which is the only published ancestor any of these lists has.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-PLAIN-08 | Remove every chatbot artifact and every AI-authorship badge from a published page. | A leftover "I hope this helps" proves an unedited paste, and a page badge is a second unmaintained place for an authorship fact to rot. | `python3 checks/prose.py --root . PATH` (artifact and badge arm), covering `I hope this helps`, `as an AI`, `knowledge cutoff`, `oaicite`, `AI-generated`, `AI-assisted` and the assisted-by phrasings. Measured 0 hits over 249 pages (wave-2 calibration), so this one launches at error whole-tree today. | MUST |
| DOC-PLAIN-09 | Never word a finding, a rule or a commit message as a claim about who or what wrote a page. | Single-instance authorship detection is falsified, so the claim is false and it gives a reader a reason to switch the whole gate off. | `grep -rniE -e 'AI-written' -e 'AI-generated' -e 'sign of AI' -e 'detect.{0,12}AI' -e 'written by (an )?AI' <your check messages and rule files>` returns nothing. Findings read as a count, for example "4 tells per 1,000 words, a human should read this". Measured single-instance accuracy of 57 and 64 percent (Wikipedia, Signs of AI writing), and a human author at 10.13 em dashes per 1,000 words inside a model's own range (Freeburg 2026). | MUST |
| DOC-PLAIN-10 | Gate a vocabulary tell on its density per 1,000 words, never on one occurrence. | One "delve" is ordinary prose, so a per-instance fail flags human writing. | `python3 checks/prose.py --root . PATH` reports hits per 1,000 words against a default of 3, labelled uncalibrated in the config because no source states a validated threshold. The wordlist excludes `underscore` and `unlock`, which name ordinary technical objects. Skips any page under 300 prose words, this family's own floor. Measured 18 raw hits over 14 of 249 pages, 17 of them false, and the single page over threshold failed on a false positive (wave-2 calibration). | CONSIDER |
| DOC-PLAIN-11 | Remove time-relative words from documentation prose. | Words like "currently" and "latest" are accurate on the day of writing and wrong one release later. | `python3 checks/prose.py --root . PATH` (time-relative arm, Google's timeless-documentation list). Pages declaring `doc_type: changelog` are skipped through `checks/doc_declaration.py`. A noun phrase naming a value the reader's environment computes is exempt, for example "the latest digest": unverified: reading heuristic. Look for whether the sentence claims product status or describes a resolved runtime value. Measured 398 hits over 104 of 249 pages, about half runtime-state phrases in a 10-item sample. Changed lines first. | SHOULD |
| DOC-PLAIN-12 | Remove marketing superlatives from documentation prose. | A superlative makes a claim the reader cannot check and delays the fact they came for. | `python3 checks/prose.py --root . PATH` (superlative arm) over `powerful`, `seamless`, `revolutionary`, `game-chang`, `supercharge`, `unlock`, `empower`, `cutting-edge`, `robust`, `effortless`. The list has no published ancestor, which is why it stays advisory. Files under a research or decisions path are excluded before reporting. Measured 8 hits over 6 of 249 pages, all 8 false (wave-2 calibration). | CONSIDER |

## Structure

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-PLAIN-13 | Use real headings, one top-level heading per page, and no skipped levels. | A bold line standing in for a heading is invisible to the outline, to search, and to a screen reader. | `npx markdownlint-cli2 --config checks/markdownlint.jsonc PATH` (MD001, MD025 with `front_matter_title` set to the empty string, MD036). Where markdownlint is absent, `python3 checks/prose.py --root . PATH` carries the same three arms. Measured on markdownlint-cli2 v0.23.2: MD001 0 hits, MD025 8 hits all false without the override, MD036 324 hits over 204 pages at about 12 percent false. Changed files first. | MUST |
| DOC-PLAIN-14 | Write every heading in sentence case. | Title Case reads as a marketing header rather than a section label, and it breaks a reader scanning for the one section they need. | unverified: reading heuristic. Look for a heading with two or more capitalised words that are neither proper nouns nor identifiers. Vale `Google.Headings` covers it at tier 1 where Vale is installed. Vale was absent from every repo measured, so the tier-1 path is untested. Never cite MD003, which does not read casing. | SHOULD |

## Links

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-PLAIN-15 | Write every link inline and never as a reference-style definition. | Reference style splits a link from its text, so an edit updates one half and rots the other. | `npx markdownlint-cli2 --config checks/markdownlint.jsonc PATH` (MD054, per-style booleans, `inline` and `autolink` true). MD054 has no `style` key, and the invalid shape throws a schema error at run time. Measured 3,595 hits with a 0 percent false-positive rate, because the check is pure syntax. Changed files first. | SHOULD |
| DOC-PLAIN-16 | Link or explain a term once, at its first meaningful mention on the page. | Linking every occurrence blows any link budget and needs an entity list nobody maintains. | `grep -oE '\]\([^)]+\)' PATH \| sort \| uniq -d` returns nothing, and `grep -cE '\]\([^)]+\)' PATH` stays at 15 or below (GitLab style guide states the 15-link cap). Pages declaring `doc_type: reference` are skipped through `checks/doc_declaration.py`, because a command table repeats a cross-reference by design. Measured 25 of 249 pages over the cap and 48 repeating a link, both concentrated in reference tables. | SHOULD |

## Tooling and scope

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-PLAIN-20 | Give every checkable construct exactly one owning tool and disable the other tool's equivalent. | Two linters can demand opposite fixes on one construct, so the contributor who satisfies the first breaks the second. | unverified: reading heuristic. Look for one construct active in two tool configs at once. Link style is the known collision: markdownlint MD054 owns it in `checks/markdownlint.jsonc`, and the equivalent Vale rule stays off in `checks/vale.ini`. | SHOULD |
| DOC-PLAIN-22 | State whether AI assistance drafted a documentation change, in the pull request body, and never name a tool as an author. | An unlabelled AI draft reads as human-reviewed prose to the next person who trusts it. | CI greps the PR body for the literal key `AI assistance: yes\|no\|partial` and fails when it is missing or empty. A second grep over the commit trailers for `Co-Authored-By:.*([Cc]laude\|[Gg][Pp][Tt]\|[Cc]opilot\|[Gg]emini)\|assisted-by\|co-developed` returns zero hits. Disclosure lives on the pull request, never on the page, which is DOC-PLAIN-08's badge ban. | MUST, pinned |
| DOC-PLAIN-23 | Run this family only over published documentation, never over an agent's own notes. | Every rule here fires on research corpora and agent config, where the findings are noise nobody priced. | The file list is `git ls-files '*.md'` under a directory holding a generator config, plus repo-root `README.md` and `CHANGELOG.md`. Assert it excludes `.agents`, `.claude`, `.serena`, `.worktrees`, `node_modules`, `dist`, `target` and build output. Measured: a naive `find` loads 420 report files and 257 stale worktree files. | MUST |

A repository with a committed docs tree and no generator config names that
directory once in the check config. Without that line, its pages are out of
scope and no rule in this family runs on them.

## Shown, not told

DOC-PLAIN-09, how a finding is worded.

```text
wrong: docs/install.md: this page reads as AI-generated (3 tells)
right: docs/install.md: 3 tells per 1,000 words, a human should read this
```

DOC-PLAIN-11, a staleness claim against a resolved runtime value.

```text
wrong: The latest release adds the --json flag.
right: The --json flag is available from 2.4.0. Run `tool --version` to check.
ok:    Pull the latest digest the registry resolves for that tag.
```

DOC-PLAIN-15, link syntax.

```text
wrong: See the [style guide][sg].

       [sg]: https://example.com/style
right: See the [style guide](https://example.com/style).
```

## Generator divergence

| Generator | What differs |
|---|---|
| mdBook | Renders YAML front matter as a visible heading and indexes it, so the type declaration is a comment and MD025 needs `front_matter_title` set to the empty string |
| Docusaurus and Starlight | MDX parses an HTML comment as a hard build error, so the declaration opener is `{/* */}` and the site sets `markdown.format: detect` |
| MkDocs Material | Include directives use `--8<--`, and a space-separated fence info string is unparsed and swallows the next fence, so `strip_prose.py` must strip includes before counting |
| VitePress | Include directives use `<<<`. Front matter parsing is site-specific, so no check here reads it |
| Sphinx and MyST | Comment openers are `..` and `%`. Both rest on the generators' own documentation and no built fixture |

## Pinned decisions

A pinned row is a default the adopter overrides once, in one place.

- DOC-PLAIN-01. The punctuation ban is house style, justified by machine
  translation and terminal rendering. It is never an AI detector, and
  DOC-PLAIN-09 forbids wording it as one. Override by editing the row.
- DOC-PLAIN-22. Disclosure is per pull request, with the tool never named as an
  author. Two other real policies exist. A site-wide banner and an outright ban
  both assume a review team an adopting repository does not have.

## Not studied

- Vale was never exercised. The binary was absent from every repository and
  from the measurement environment, so tier 1 is untested end to end.
- The sentence-case heuristic has no measured false-positive rate. A crude run
  flagged 72 of 783 headings and nobody verified how many were real.
- The tell-density threshold of 3 is uncalibrated. The whole corpus produced 18
  raw hits, which is no population to fit a threshold to.
- The marketing wordlist has no published ancestor and produced zero true
  positives on the corpus measured.
- No ratchet schedule exists for the readability floor. A floor at 50 fails
  about half the measured corpus and rewards nothing once the median moves.
- DOC-PLAIN-10 and DOC-PLAIN-12 each rediscovered the same false positives from
  words that double as technical vocabulary. One shared exclusion list would
  prevent the third rediscovery and nobody has written it.
- DOC-PLAIN-22 and DOC-PLAIN-23 ship with no measured hit count. Write the
  fixture pair each check must reject and accept before trusting either number.
- This family carries no accessibility rule. Alt text, contrast, keyboard order
  and table semantics are out of scope here and stated as an exclusion.
