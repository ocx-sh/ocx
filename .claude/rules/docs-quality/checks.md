---
title: The checks
summary: One row per shipped script, what it checks, how to run it, what its fixtures prove, and the three configs that are not scripts
---

# The checks

`checks/` holds eight Python scripts and three configs. Every script needs
Python 3.11 at least, imports nothing outside the standard library, and reads
source markdown rather than built output. Every rule ID a script prints is
defined in a depth file the index routes you to.

Contents: [The shared contract](#the-shared-contract) ·
[One row per script](#one-row-per-script) ·
[Extra modes](#extra-modes) ·
[Configs, not scripts](#configs-not-scripts) ·
[Run every self-test](#run-every-self-test) ·
[What the fixtures do not prove](#what-the-fixtures-do-not-prove)

## The shared contract

Every script takes the same command line and returns the same exit codes.

```text
<script> [--root DIR] [PATH ...] [--format text|json] [--self-test]
```

`--root DIR` walks a tree. `PATH ...` checks named files. `--format json` emits
findings as an array. `--self-test` runs the script over its own fixtures.

| Exit | Meaning |
|---|---|
| 0 | clean, or the script decided it does not apply |
| 1 | at least one finding |
| 2 | a usage error, or an input path that does not exist |

Findings print one per line, as `path:line: DOC-XXX-nn: message`. A finding is
always an exit 1. Which rule IDs block a merge is the gate's decision, stated in
each rule's Severity column, never inside a script.

Two scripts bend the shape and say so on their row. `strip_prose.py` prints
stripped prose on stdout and sends findings to stderr, so a rule row can pipe it
into `grep`. `nav_depth.py` prints `not applicable` and exits 0 when the tree
carries no generator config, which is DOC-NAV-01.

## One row per script

| Script | Rules it checks | Run it | Fixtures |
|---|---|---|---|
| `strip_prose.py` | DOC-PLAIN-04 | `python3 checks/strip_prose.py --root .` | `fixtures/strip_prose/pass-clean.md`, `fail-unclosed-fence.md` |
| `prose.py` | DOC-PLAIN-01, 02, 03, 05, 08, 10, 11, 12, 13 | `python3 checks/prose.py --root .` | `fixtures/prose/pass-clean.md`, `fail-punctuation.md`, `fail-paragraph.md`, `fail-dense.md` |
| `doc_declaration.py` | DOC-TYPE-01, 02, 28, 29, 30, DOC-DISC-13 | `python3 checks/doc_declaration.py --root .` | `fixtures/doc_declaration/`, 9 files, 3 pass and 6 fail |
| `page_type.py` | DOC-TYPE-03, 04, 07, 08, 09, 22, 32, 33, 37, 41, DOC-DISC-16, 17 | `python3 checks/page_type.py --root .` | `fixtures/page_type/`, 9 files, 2 pass and 7 fail |
| `landing_check.py` | DOC-TYPE-10, 11, 12, 13, 14, 15 | `python3 checks/landing_check.py --root .` | `fixtures/landing_check/pass-landing.md`, `fail-landing.md`, `fail-cta.md` |
| `nav_depth.py` | DOC-NAV-01, 02, 03, 04, 05, 06 | `python3 checks/nav_depth.py --root .` | `fixtures/nav_depth/`, two page fixtures and three `mkdocs.yml` trees |
| `doc_examples.py` | DOC-EX-01, 02, 05, 06, 07, 20, 21 | `python3 checks/doc_examples.py --root . --changed-only` | `fixtures/doc_examples/`, 7 pages plus `harness-pass/` and `harness-fail/` |
| `links_raw.py` | DOC-OBS-02, and DOC-NAV-07's authoring half | `python3 checks/links_raw.py --root docs docs` | `fixtures/links_raw/pass-links.md`, two fail pages, `reference/cli.md`, `reference/api.md` |

`strip_prose.py` is the hot dependency. The other three text scripts import it
by a path relative to `__file__`, so the whole directory moves as one unit. Its
public API is `strip(text)` line-preserving, `iter_sentences(prose, start_line)`,
`iter_paragraphs(prose)`, `declaration(text)`, `word_count`, `inline`, `collect`,
`emit`, `self_test` and `run_cli`.

The DOC-AGENT family names no script here. Its verifications are inline `diff`,
`awk`, `grep` and `curl` commands plus two reading heuristics.

## Extra modes

**`doc_declaration.py --seed [--root DIR]`** proposes a type per page for a
retrofit and prints `page<TAB>type<TAB>confidence<TAB>source`. It reads
`mkdocs.yml` nav line by line, so `!ENV` and `!!python` tags cannot break it. It
also reads `docs/SUMMARY.md` or `docs/src/SUMMARY.md`, skipping mdBook's own
mandatory first heading, and the three named `.vitepress/config.*` locations.
Confidence is high when a nav group label maps, medium when only the page label
maps, none otherwise. VitePress config paths are named rather than globbed,
because a recursive glob picked up vendored copies and inflated one repository
from 6 rows to 44. A seed is a migration proposal a human reviews, never a
runtime source of truth.

**`doc_examples.py`** has four modes beyond `--self-test`.

```bash
python3 checks/doc_examples.py --root .                  # fences, tokens, markers, set-diff
python3 checks/doc_examples.py --root . --tests test/    # DOC-EX-02 key diff, both ways
python3 checks/doc_examples.py --harness test/doc/       # run each bound example file
python3 checks/doc_examples.py --root . --changed-only   # the merge-gate setting
```

The harness dispatches by suffix through the `RUNNERS` table: `.sh` and `.bash`
to `bash`, `.py` to `python3`, and `.ts`, `.mts`, `.js` and `.mjs` to `node`.
Each example file's header carries `# doc: <slug>` and an optional
`# expect_exit: N`. Project configuration sits in one block at the top of the
file: `RUNNABLE_TIERS`, `TIER_WORDS`, `NORUN_OPEN`, `NORUN_CLOSE` and `RUNNERS`.

**`links_raw.py --root`** is the site source root, and it is the resolution base
for every root-relative link. Three resolutions run before a link is called
dead: an explicit `{#kebab-id}` anchor with `attr_list` spacing tolerated, a
root-relative path with `.md`, `.mdx` and `/index.md` tried for an extensionless
target, and a skip for any target page whose anchors are generated at build
time. Skipped pages print on stderr, or in the `skipped` array under
`--format json`. That listing is the reviewable exemption list DOC-OBS-02 and
DOC-OBS-18 both read, so never silence it. The generated-anchor markers are one
commented config block at the top: `Auto-generated`, `:::`, `{{#include`, `<<<`
and `--8<--`. Heading slugs follow the Python-Markdown toc slug, which keeps
underscores and collapses runs of hyphens and spaces to one hyphen.

**`nav_depth.py`** splits its work by argument. `--root` drives the nav arm and
`PATH` arguments drive the per-page arm. Generator detection runs in order:
`mkdocs.yml` or `mkdocs.yaml`, then the three named `.vitepress/config.*` paths,
then `book.toml` with `SUMMARY.md`. `mkdocs.yml` is read line by line inside the
`nav:` block only. VitePress depth is a bracket-balanced slice of the `sidebar`
block alone, which is what stops the top nav bar inflating the top-level count.
An mdBook `# Part Title` divider counts as grouping and `SUMMARY.md`'s own first
heading is skipped, because counting it marked a 20-item flat nav as grouped.

Two commands, run against the planted-violation trees:

```bash
$ python3 nav_depth.py --root fixtures/nav_depth/fail-nav-depth4
fixtures/nav_depth/fail-nav-depth4/mkdocs.yml:1: DOC-NAV-02: mkdocs nav is 4 levels deep, over the 3-level cap
fixtures/nav_depth/fail-nav-depth4/mkdocs.yml:1: DOC-NAV-04: nav reaches 4 levels with no breadcrumb, so a reader at level 3 has no trail back up

$ python3 nav_depth.py --root fixtures/nav_depth/fail-nav-flat9
fixtures/nav_depth/fail-nav-flat9/mkdocs.yml:1: DOC-NAV-03: 9 top-level nav entries with no group, at or over the 8-page grouping floor
```

**`prose.py` arms**, in the order the rule rows cite them: sentence length,
paragraph length, banned punctuation, chatbot artifacts and AI badges,
time-relative words, marketing superlatives, tell density per 1,000 words,
Flesch reading ease, and heading hygiene. The heading arm runs only when
`markdownlint` is not on PATH, so DOC-PLAIN-13 is never unchecked where the
optional linter is absent. Banned marks are written as escapes in the source, so
the file holds none of them itself. Per-type carve-outs read the declaration:
the readability floor skips `reference`, `troubleshooting` and `changelog`, the
time-word arm skips `changelog`, and both the readability and the tell-density
arms skip any page under 300 prose words.

**`page_type.py`** skips a page with no `doc_type` declaration and reports the
skipped count on stderr. It never guesses a type from a path, and
`doc_declaration.py` owns the missing-declaration finding.
**`landing_check.py`** runs DOC-TYPE-10 to DOC-TYPE-13 on `doc_type: landing`
pages only, and DOC-TYPE-14 and DOC-TYPE-15 on every page.

## Configs, not scripts

| File | What it is | How to use it |
|---|---|---|
| `checks/markdownlint.jsonc` | The shipped markdownlint config, tuning MD001, MD003, MD025 with `front_matter_title` set to the empty string, MD036, MD040, MD041, MD045 and MD054 | `npx markdownlint-cli2 --config checks/markdownlint.jsonc 'docs/**/*.md'` |
| `checks/lychee.toml` | An example built-output link checker config for DOC-OBS-01, with both measured traps handled | `lychee --include-fragments --config checks/lychee.toml 'site/**/*.html'` |
| `checks/vale.ini` | The optional tier-1 layer, packages pinned by org and repo, with the severity split noted | `vale --config checks/vale.ini docs/` |

MD041 is off wherever the declaration comment is line 1. MD040 carries
`allowed_languages`, which is DOC-EX-05's fence tier list. MD054 takes per-style
booleans and has no `style` key, and the invalid shape throws a schema error at
run time. Only the inline and autolink styles are true. Vale is never a rule's only verification, and it was absent from every
repository measured, so the tier-1 path is untested end to end.

## Run every self-test

`--self-test` runs a script over its own fixtures. It exits 1 unless every
`fail-*` fixture is rejected and every `pass-*` fixture is accepted. Run all
eight before trusting any number in this rule set.

```bash
cd rules/docs-quality/checks
for s in strip_prose prose doc_declaration page_type landing_check nav_depth doc_examples links_raw
do python3 "$s.py" --self-test || echo "FAILED: $s"
done
```

All eight exit 0 today. The four text scripts print a per-fixture line and a
`self-test passed` summary. The other four print `self-test: ok, 0 fixture
mismatches`. Three runs carry more than a summary line.

`strip_prose.py` adds a residue test after its summary.

```text
residue-test ok: fixture strips clean and keeps its line count
```

That test asserts the stripped fixture holds no fence, link target, table pipe,
admonition marker, include directive, HTML block or reference definition, and
that the line count is preserved. Without it the self-test would prove only that
the fence detector works, not that the stripper strips.

`doc_examples.py` runs both harness fixtures, so the pass and the deliberate
failure are both exercised.

```text
2/2 bound examples passed
.../fixtures/doc_examples/harness-fail/publish.sh:1: DOC-EX-07: example bound to page 'publish-a-package' exited 3, expected 0.
0/1 bound examples passed
```

`prose.py` exercises the two arms that are otherwise dead on real corpora,
through the 330-word `fail-dense.md` fixture.

```text
DOC-PLAIN-10: 6.6 vocabulary tells per 1,000 words over a default of 3.0 (uncalibrated), a human should read this page
DOC-PLAIN-05: Flesch reading ease -168.5 below the floor of 50.0 (wave-2 fleet median 49.0)
```

## What the fixtures do not prove

- **DOC-TYPE-02 has no path-conflict fixture.** It holds by construction
  instead: `read_declaration()` and `classify()` take text and take nothing
  else. The meta-check is `grep -nE 'dirname|basename' checks/doc_declaration.py`
  returning nothing. A fixture declaring `how-to` under a `reference/` path
  would prove it mechanically, and nobody has written one.
- **No fixture covers Starlight or Sphinx.** The `{/*`, `..` and `%` carriers
  rest on primary documentation with no built site behind them.
- **The `--harness` mode asserts an exit code and nothing else.** No output diff
  and no type check happen without a step you add.
- **DOC-PLAIN-22 and DOC-PLAIN-23 ship with no fixture pair at all.** Both are
  greps over a pull request body and a file list, not over page content.
- **No fixture is a false-positive corpus.** A fixture proves a check can go red
  and can go green. It does not report how often the check is wrong on real
  pages, which is what each rule row's measured count is for.

Two habits belong to any check you add here. Write the fail fixture first, and
watch it go red before the rule ships. Report the false-positive rate on real
pages before raising a rule above SHOULD.
