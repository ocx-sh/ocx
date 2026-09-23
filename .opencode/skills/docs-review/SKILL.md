---
name: docs-review
description: Grade documentation pages against the docs-quality rule set on demand, in any directory and any markup format (Markdown, MDX, reStructuredText, MyST, AsciiDoc). Use when someone asks to review, check, grade, lint or audit a doc page, a README, a changelog, a docs directory or the docs changed on a branch, asks whether a page meets docs-quality, or wants the docs gate run by hand. Not for deciding which pages to write, which is docs-plan, and not for wiring the checks into CI, which is docs-instrument.
license: Apache-2.0
metadata:
  summary: Runs the docs-quality gate by hand over named pages or a branch diff, in any directory and any markup format
  keywords: docs,documentation,review,docs-review,check,lint,audit,grade,readme,changelog,markdown,mdx,restructuredtext,rst,myst,asciidoc,adoc,sphinx,antora,doc_type,docs-quality
---

# docs-review

The `docs-quality` rule loads when an agent edits a page. This skill is the
explicit trigger: grade pages nobody is editing, pages in unusual places, and
pages in a format the scripts only partly read.

## Stop condition

Stop when every target page has a verdict: each finding as
`path:line: DOC-XXX-nn: message`, or `clean`. Mark any finding no script
produced `unverified: reading heuristic`. Report; never fix unless asked.

## 1. Pick the targets

- Named paths or directories: use them as given, whatever their location.
- Nothing named: the branch diff,
  `git diff --name-only --diff-filter=d "$(git merge-base HEAD origin/HEAD)" -- '*.md' '*.mdx' '*.rst' '*.adoc' '*.asciidoc'`.
- Drop agent config (`CLAUDE.md`, `AGENTS.md`, `SKILL.md`, `.claude/`,
  `.agents/`), vendored copies and build output. The rule excludes them too.

## 2. Load the rule

Read the installed `docs-quality` rule index, then only the depth files its
routing table names for the target page types. Find it with
`find . -path '*docs-quality*' -name 'docs-quality.md' -not -path '*/node_modules/*'`.
Its `checks/` directory sits beside it. No hit means the rule is not
installed: say `grim add ghcr.io/ocx-sh/lore/docs-quality` and stop.

## 3. Run the checks by format

`CHECKS` below is that `checks/` directory. Every script takes explicit paths,
so pass the targets rather than `--root`.

| Format | Scripted | Read by hand |
|---|---|---|
| `.md`, `.mdx` | the whole gate, in the rule's order | rows the scripts mark heuristic |
| `.rst`, MyST, `.adoc` | `doc_declaration.py` | everything else, or convert first |

Markdown and MDX:

```bash
python3 "$CHECKS/doc_declaration.py" PAGE...
python3 "$CHECKS/prose.py"           PAGE...
python3 "$CHECKS/page_type.py"       PAGE...
python3 "$CHECKS/landing_check.py"   PAGE...
```

Add `doc_examples.py`, `nav_depth.py` and `links_raw.py` when the review covers
a whole docs tree rather than single pages. Their inputs are a tree.

reStructuredText and AsciiDoc: run `doc_declaration.py` on the source. The
prose and page-type scripts parse Markdown only. When `pandoc` is on the PATH,
convert each page into a scratch directory and run them on the copy:

```bash
pandoc -f rst -t gfm PAGE.rst -o "$SCRATCH/PAGE.md"   # asciidoc: asciidoctor -b docbook | pandoc -f docbook
```

Line numbers from a converted copy point at the copy. Map each finding back to
the source line by its quoted text before reporting it. No converter means the
non-negotiables are read by hand and every such finding is marked unverified.

## 4. Report

One line per finding, grouped by page, ordered by the rule's non-negotiable
number. End with a count per page. Undeclared pages are listed first, because
every type-scoped check skipped them.
