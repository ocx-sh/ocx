---
paths:
  - "README.md"
  - "CHANGELOG.md"
  - "CONTRIBUTING.md"
  - "docs/**"
  - "doc/**"
  - "website/**"
  - "site/**"
  - "mkdocs.yml"
  - "book.toml"
  - ".vitepress/config.*"
  - "docusaurus.config.*"
summary: The documentation quality index. The page declaration every check reads, the 18 rules that block any docs edit, the runnable gate, and where the per-topic depth lives
keywords: documentation,docs,technical-writing,diataxis,plain-english,readability,tested-examples,doctest,code-fences,asciinema,navigation,information-architecture,link-checking,anchors,changelog,readme,runbook,markdownlint,vale,mkdocs,vitepress,mdbook,docusaurus,starlight,sphinx,llms-txt,agent-readable
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# Documentation quality

This is a merge gate for documentation, expressed as counted limits and
runnable scripts. It is not a voice, a template, or an opinion about how a page
should read. Every rule in this set either names a command you can run, or says
`unverified: reading heuristic` and states what a reviewer looks for.

Contents: [Declare the page](#declare-the-page) ·
[Non-negotiables](#non-negotiables) · [The gate](#the-gate) ·
[Where the depth is](#where-the-depth-is) · [Severity](#severity) ·
[Not studied](#not-studied) · [Siblings](#siblings)

## Declare the page

Two comment lines carry the contract. Every check in this set scopes itself
from them. A page with no declaration is skipped, so the gate reports green
while nothing is checked.

```markdown
---
title: Install the CLI
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

# Install the CLI
```

`doc_type` is one of `tutorial`, `how-to`, `reference`, `explanation`,
`troubleshooting`, `runbook`, `landing`, `readme`, `changelog`. `doc_tier` is
one of `first-steps`, `everyday`, `integration`, and only `tutorial`, `how-to`
and `landing` require one. Both enums are pinned defaults.

Placement is inside the file's first 12 lines and never above an existing front
matter block. One comment opener per markup family: `<!--` for markdown, `{/*`
for MDX, `..` for reStructuredText, `%` for MyST. Never YAML front matter,
because mdBook 0.5.3 renders that block as a fake `<h2>` and puts the fake
heading into the search index with its own anchor.

```bash
python3 checks/doc_declaration.py --root .     # add --seed to propose a retrofit
```

## Non-negotiables

Each row blocks a merge. The depth files carry 43 MUST rows in total. These 18
are the ones that apply to any documentation edit. The rest stay in their depth
file, conditional on a page type, a generator config or a mechanism. Every ID
resolves to the file the routing table names.

| # | Rule | ID |
|---|---|---|
| 1 | Declare a page's type and tier in a comment line inside the first 12 lines. | DOC-TYPE-01, DOC-DISC-13 |
| 2 | Never write the declaration as front matter, never place it above front matter, and use `{/* */}` in MDX. | DOC-TYPE-28, DOC-TYPE-29, DOC-TYPE-30 |
| 3 | Read a page's type from its declaration comment, never from its directory or file name. | DOC-TYPE-02 |
| 4 | Run this set over published documentation only, never over an agent's working directory or build output. | DOC-TYPE-31, DOC-PLAIN-23 |
| 5 | Never let a page declare one type and carry another type's content, and keep every branch off a tutorial. | DOC-TYPE-03, DOC-DISC-17 |
| 6 | State a how-to page's goal before its first `##`, and write reference prose as description with no first person. | DOC-TYPE-22, DOC-TYPE-04 |
| 7 | Never publish placeholder text, and never state an adoption claim without a link to its source. | DOC-TYPE-14, DOC-TYPE-15 |
| 8 | Split any prose sentence longer than 25 words. | DOC-PLAIN-02 |
| 9 | Use real headings, one top-level heading per page, and no skipped levels. | DOC-PLAIN-13 |
| 10 | Remove every chatbot artifact and AI-authorship badge from a published page. | DOC-PLAIN-08 |
| 11 | Never word a finding, a rule or a commit message as a claim about who or what wrote a page. | DOC-PLAIN-09 |
| 12 | State whether AI assistance drafted a change in the pull request body, and never name a tool as an author. | DOC-PLAIN-22 |
| 13 | Compute every word count and readability number on stripped prose, never on raw file text. | DOC-PLAIN-04 |
| 14 | Back every runnable example with a test in the required gate, bound by a declared key rather than a mirrored path. | DOC-EX-01, DOC-EX-02 |
| 15 | Write a fence tier suffix as one hyphen-joined token, and give a no-run snippet a paired marker stating why. | DOC-EX-20, DOC-EX-06 |
| 16 | Produce a recording by running a real command, keep the recorder out of the gate, and commit a cast only when nothing regenerates it. | DOC-EX-11, DOC-EX-12, DOC-EX-13 |
| 17 | Default a player to no autoplay, leave its own controls on, make a tooltip Escape-dismissible, and never let a sandbox be the only view. | DOC-EX-15, DOC-EX-16, DOC-EX-34, DOC-EX-27 |
| 18 | Give every cross-linked heading an explicit `{#kebab-id}` anchor, and fail the build on a dead internal link in built output. | DOC-NAV-07, DOC-OBS-01 |

## The gate

`checks/` is this rule's own directory of runnable files. Every script needs
Python 3.11 at least, imports nothing outside the standard library, and reads
source markdown only. Each exits 0 clean, 1 findings, 2 usage error. Run them in
this order, because each later one reads the declaration the first one checks.

```bash
python3 checks/doc_declaration.py --root .              # types and tiers
python3 checks/prose.py           --root .              # sentences, punctuation, readability
python3 checks/page_type.py       --root .              # per-type page contracts
python3 checks/landing_check.py   --root .              # hero shape, placeholders, claims
python3 checks/doc_examples.py    --root . --changed-only
python3 checks/nav_depth.py       --root .              # exits 0 with no generator config
python3 checks/links_raw.py       --root docs docs      # anchors, root-relative targets
npx markdownlint-cli2 --config checks/markdownlint.jsonc 'docs/**/*.md'
```

**Rollout, for every rule in this set.** Enforce at error on changed files from
the first commit. Warn whole tree until the backfill lands, then turn the whole
tree red and keep it red. Every calibrated page in the 248-page corpus fails the
declaration rows today, so a whole-tree error on day one blocks every merge.

**Two tiers.** Tier 0 is the list above plus `grep`, and it is mandatory. Every
rule in this set resolves at tier 0 alone. Tier 1 is Vale with
`checks/vale.ini`, optional, and no rule may rest on it for its only
verification.

**Dogfood.** This set is held to the rules it prescribes, by the same script.

```bash
python3 rules/docs-quality/checks/prose.py rules/docs-quality.md rules/docs-quality/*.md
```

It reports nothing on the index, the depth files, both skills and their
companions. Never point that run at `checks/fixtures/`, whose pages are planted
violations by design.

## Where the depth is

Read the file for the work you are about to do. One level deep, and these files
never link each other.

| Doing | Read |
|---|---|
| Deciding which page type this is, what it must contain, and how to declare it | [docs-quality/page-types.md](docs-quality/page-types.md) |
| Writing or reviewing prose: sentence and paragraph limits, punctuation, readability, AI tells, headings, link style | [docs-quality/plain-english.md](docs-quality/plain-english.md) |
| Adding a code fence, wiring a doc-example test, recording a terminal cast, or adding tabs, a tooltip or a sandbox | [docs-quality/examples.md](docs-quality/examples.md) |
| Changing the sidebar, splitting a long page, adding an anchor, or fixing a query that finds nothing | [docs-quality/navigation.md](docs-quality/navigation.md) |
| Standing up docs signals: link and drift gates, the runbook blocking split, the signal manifest, error identifiers | [docs-quality/observability.md](docs-quality/observability.md) |
| Deciding what the site owes an agent reader: the Markdown twin, `llms.txt`, agent-directed callouts | [docs-quality/machine-readers.md](docs-quality/machine-readers.md) |
| Running, extending or debugging a check, or reading what its fixtures prove | [docs-quality/checks.md](docs-quality/checks.md) |

## Severity

MUST = block, fix before it lands. SHOULD = warn, fix or state why not in the
commit body. CONSIDER = suggest, never blocks and never re-raised after a
decline. Structural and drift checks fail red. Readability and tell counts
report as warnings, because a red prose gate gets switched off.

A row marked **unverified: reading heuristic** has no runnable check. It names
what a reviewer looks for instead, and it never blocks.

A row marked **pinned** rests on a project decision rather than a measurement.
Each is a default the adopter overrides once, in one place, and the row says
where. Thirteen rows are pinned.

| Pinned decision | Rows |
|---|---|
| The `doc_type` and `doc_tier` enums | DOC-TYPE-01, DOC-DISC-13 |
| The scope list, tracked docs plus root `README.md` and `CHANGELOG.md` | DOC-TYPE-31 |
| The punctuation ban, and per-change AI disclosure | DOC-PLAIN-01, DOC-PLAIN-22 |
| A recording is a real run, and when to commit a cast | DOC-EX-12, DOC-EX-13 |
| Nav grouping floor, page-length trigger, search beacon name and sink | DOC-NAV-03, DOC-NAV-06, DOC-NAV-10 |
| The blocking split, and the signal manifest shape | DOC-OBS-04, DOC-OBS-05, DOC-OBS-10 |

## Not studied

Named holes, so a reader can tell a gap from a clearance.

- **No page in the calibration corpus declares a type.** Every type-scoped check
  is inert until the backfill lands, and zero real tutorials exist to test the
  tutorial contract against.
- **Page-level accessibility is governed nowhere here.** Alt text, contrast,
  keyboard order, table semantics, the sidebar and the search widget are out of
  scope. Only the terminal player and the tooltip are covered, in `examples.md`.
- **Vale was never exercised**, and the sentence-case heuristic, the tell-density
  threshold and the marketing wordlist have no measured false-positive rate.
- **No validated freshness interval exists**, which is why no rule gates on a
  page's age, and the Markdown twin has no measured implementation anywhere.
- **Versioned docs, translations, and print or offline output** were not studied
  by any family. Neither was reStructuredText, whose carriers rest on primary
  documentation with no built fixture.

## Siblings

- **`docs-plan`** is the discovery skill. Run it before writing pages. It
  produces tiered use cases, a typed page inventory, an IA plan, and the product
  shape that selects the first-steps thresholds.
- **`docs-instrument`** is the gate skill. Run it to retrofit declarations and
  wire the checks above into CI. It picks a link checker per generator and
  stands up a tested-example harness per language. It also lands a new lint
  without reddening every open pull request.

Nothing in this set co-loads with `css-theming`. A docs site's own stylesheet is
that rule's subject, and this one stops at what reaches the page.
