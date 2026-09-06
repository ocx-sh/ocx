---
title: Page types and the declaration
summary: The nine declared page types, the comment carrier that survives every generator, and the tier and tutorial rules that hang off them
---

# Page types and the declaration

Every page states its type in a comment line, and each type is a contract a
check can read. This file carries that contract, plus the tier model and the
tutorial and first-steps rules that depend on it.

Contents: [The declaration](#the-declaration) ·
[The nine types](#the-nine-types-one-line-each) ·
[Declaration and scope](#rules-declaration-and-scope) ·
[Page shape](#rules-page-shape) ·
[Landing pages](#rules-landing-pages) ·
[README, CHANGELOG, CONTRIBUTING](#rules-readme-changelog-and-contributing) ·
[Tier and first steps](#rules-tier-and-first-steps) ·
[Worked pairs](#worked-pairs) ·
[Generator divergence](#generator-divergence) ·
[Pinned decisions](#pinned-decisions) · [Not studied](#not-studied)

**Rollout for every rule here.** Enforce at error on changed files from the
first commit. Warn whole tree until the backfill lands, then turn the whole
tree red. The declaration rows need this: 181 of 181 pages in the 248-page
calibration corpus carry no declaration today. A whole-tree error on day one
would block every merge.

## The declaration

Two keys, one comment line each, inside the first 12 lines, and never above an
existing front matter block. Markdown:

```markdown
---
title: Install the CLI
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

# Install the CLI
```

One opener per markup family, all four accepted by the same check:

```text
<!-- doc_type: how-to -->        markdown
{/* doc_type: reference */}      MDX, and any .md file a Docusaurus site parses as MDX
.. doc_type: reference           reStructuredText
% doc_type: explanation          MyST
```

`doc_type` is one of `tutorial`, `how-to`, `reference`, `explanation`,
`troubleshooting`, `runbook`, `landing`, `readme`, `changelog`. `doc_tier` is
one of `first-steps`, `everyday`, `integration`, and it is required only on
`tutorial`, `how-to` and `landing`. Both enums are pinned.

Three placements that break a build or a search index, all measured:

```text
the keys inside front matter        mdBook indexes a fabricated <h2> with its own anchor
a comment above the front matter    the front matter renders as visible body text
an HTML comment in an .mdx file     MDX 3.1.1 fails the build with a parser error
```

## The nine types, one line each

| Type | Opens with | Contains | Never |
|---|---|---|---|
| landing | a value claim, a definition, a command, or a title with a caveat | a runnable command or a link menu before word 150, and at most 2 button calls to action | a walkthrough, a reference table, or a second positioning paragraph |
| tutorial | what the reader will have built at the end | ordered steps, each ending in a result the reader can see | branching choices, package-manager alternatives, or a prose "or, with" |
| how-to | a goal or scope sentence before the first `##` | numbered reader actions, or one heading per independent choice | a learning frame, an unbounded concept preamble over 150 words |
| reference | a description sentence for the item | a syntax block and a parameter table per entry, in the code's own order | first person, narrative openers, problem framing, invented entries |
| explanation | a sentence naming what the page explains | mechanism narrated in third person, and the judgements the other types may not carry | a numbered sequence of dependent reader actions |
| troubleshooting | an entry title prefixed `Error:` or `Warning:` | a cause paragraph opening "This issue occurs when", then a fix | generic numbered steps, or entries above four on a page that is not one |
| runbook | the operational trigger for running it | an ordered procedure with a verifiable end state | a symptom catalogue, which is a troubleshooting page |
| readme | a plain-language description of the project | one zero-to-using action, a docs-site link when a site exists, a license | a sponsor table, a funding appeal or a feature list before the description |
| changelog | a version heading, or `Unreleased` when the generator emits one | Keep a Changelog category spellings, and a migration path on every breaking entry | a re-rendered commit log, or a build gated on `Unreleased` being present |

## Rules: declaration and scope

One script reads the declaration. Every other rule in this file scopes itself
from what that script returns.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-TYPE-01 | Declare a page's type in a `doc_type` comment line inside the file's first 12 lines, using the comment opener of its markup family. | An undeclared page is skipped by every type-scoped check, so the gate stays green while nothing is checked. | `checks/doc_declaration.py --root .` Nine values, four openers. Changed files first: 181 of 181 corpus pages fail today (248-page calibration corpus). | MUST, pinned |
| DOC-TYPE-02 | Read a page's type from its declaration comment only, never from its directory or file name. | A path classifier reads one repo cleanly and misclassifies 18 of 23 pages in the next one (measured, two repos of the same fleet). | `checks/doc_declaration.py --self-test` The fixture `pass-path-conflict.md` sits under a `reference/` path and declares `doc_type: how-to`, and the script must accept it. | MUST |
| DOC-TYPE-28 | Never write the declaration as YAML front matter. | mdBook 0.5.3 renders the block as a horizontal rule plus a real `<h2>`, and that fake heading enters the search index with its own anchor. | `checks/doc_declaration.py --root .` fails a front matter `doc_type:` or `doc_tier:` key (measured on a built fixture, 9 index hits across 9 pages). | MUST |
| DOC-TYPE-29 | Never place the declaration comment above an existing front matter block. | Front matter must start on line 1, so a comment above it turns the whole block into visible content on all three measured generators. | `checks/doc_declaration.py --root .` fails a file whose line 1 is the declaration and whose line 2 is `---` (measured on all three generators). | MUST |
| DOC-TYPE-30 | Use `{/* doc_type: V */}` in an `.mdx` file, and set a Docusaurus site's `markdown.format` to `detect` before putting an HTML comment in any `.md` file. | `@mdx-js/mdx` 3.1.1 raises "Unexpected character `!`" and fails the build, and Docusaurus 3 parses plain `.md` as MDX by default. | `checks/doc_declaration.py --root .` plus `grep -E -e "format: *['\"]detect['\"]" -e "format: *['\"]md['\"]" docusaurus.config.*` when that config file is present. | MUST |
| DOC-TYPE-31 | Run this family only over published documentation, never over an agent's working directory or build output. | Without a scope gate the family fires on research notes and build output. A naive directory walk loaded 420 generated report files in one measured repository. | `checks/doc_declaration.py --root . \| grep -E '^(\.agents\|\.claude\|node_modules\|target\|site\|book\|dist)/'` returns nothing. The file list is `git ls-files` markdown under a documentation directory, plus repo-root `README.md` and `CHANGELOG.md`. | MUST, pinned |
| DOC-DISC-13 | Declare tier and type as two separate keys, allow only the three `doc_tier` values, and require a tier only on `tutorial`, `how-to` and `landing`. | One enum carrying both axes files reference as a tier, and requiring a tier everywhere forces a meaningless value onto reference and changelog pages. | `checks/doc_declaration.py --root .` reads the tier arm for those three types only. Changed files first: 248 of 248 corpus pages fail today. | MUST, pinned |

## Rules: page shape

`checks/page_type.py` reads the declaration, then applies the contract for that
type. It calls `checks/strip_prose.py` first, so include directives and link
targets never count as prose words.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-TYPE-03 | Never let a page declare one type and carry another type's content. | A learning opener that drops into task conditionals is the shape a model produces for the word "guide", and a reader trusts the declared type. | `checks/page_type.py <paths>` On `how-to` and `tutorial`, a learning opener plus a conditional-instruction hit fails. On `landing`, an ordered list over 2 items or a table over 3 rows fails (argued thresholds). Measured 0 hits over 248 pages for the conflation arm, 12 of 22 landing pages for the landing arm. | MUST |
| DOC-TYPE-04 | Write reference prose as description only, with no first person, no narrative opener and no problem framing. | Narrative reference prose is the type mix a reader cannot detect until a fact turns out to be wrong. | `checks/page_type.py <paths>` on `doc_type: reference`, outside fences and tables. Measured 1 hit over 53 reference pages, 0 of 1 false positive. | MUST |
| DOC-TYPE-05 | Put opinions, comparisons and recommendations only in explanation content. | Judgement inside a how-to or a reference page reads to the reader as a fact about the product. | `checks/page_type.py <paths>` on non-explanation pages. The pattern has never been run against real prose, so its false-positive rate is unknown. | CONSIDER |
| DOC-TYPE-06 | Keep a culture-bound analogy inside explanation content or a skippable callout, cite it, and state the same idea plainly without the analogy. | An analogy carrying the only explanation locks out every reader who does not know the compared tool. | `checks/page_type.py <paths>` requires an analogy verb next to the tool name, not a bare mention. The plain-sentence half is unverified: reading heuristic. Look for the same idea stated once without the comparison. Measured 249 hits over 248 pages under the bare-mention pattern, 12 of 12 sampled false positives, all literal install commands. | CONSIDER |
| DOC-TYPE-07 | Allow at most one concept paragraph of 150 words before a how-to or reference page's own content. | An unbounded concept opener turns a task page into an explanation the reader has to skip. | `checks/page_type.py <paths>` counts stripped prose between the H1 and the first `##`, and fails over 150. The number is measured: 13 real preambles run 0 to 216 words, median 78, and a 100-word cap failed 4 pages carrying real motivation. Measured 26 hits over the how-to and reference subset of 248 pages. | CONSIDER |
| DOC-TYPE-08 | Open every troubleshooting entry title with `Error:` or `Warning:` and its cause paragraph with "This issue occurs when". | A generic numbered-steps block buries the message the reader pasted into search. | `checks/page_type.py <paths>` counts every entry heading, then asserts each carries the prefix. A title over 70 characters (asserted, no source states it) must end in an ellipsis and carry no link. Measured 3 of 3 real troubleshooting pages non-compliant, false-positive rate not yet reported. | SHOULD |
| DOC-TYPE-09 | Put troubleshooting entries last on a page and move them to their own page at five or more. | Troubleshooting content in the middle of a page displaces the task the reader came for. | `checks/page_type.py <paths>` fails over four tagged entries (5 from the GitLab troubleshooting topic type) and asserts the first tagged heading follows every other `##`. Measured 0 hits over 248 pages, because no page carries a tagged entry yet. | SHOULD |
| DOC-TYPE-17 | Give every reference entry a description sentence, a syntax block and a parameter table. | Sibling entries drift apart across one authoring pass, and the thinnest entry is the one a reader hits first. | `checks/page_type.py <paths>` runs per entry heading, not per file, and asserts a syntax fence plus a multi-row table. Measured 649 hits over 658 entries on 53 pages, 98.6 percent. 10 of 10 sampled hits carry the content in ordinary prose, so this cannot gate. | CONSIDER |
| DOC-TYPE-18 | Derive a reference page's item set and its order from the code's own enumeration, proven by a test that reads the page. | An invented or dropped entry is the most damaging reference defect and the cheapest one to catch mechanically. | `checks/doc_examples.py --root .` fails a `doc_type: reference` page with no test bound by a `# doc: <slug>` key. That test parses the page, derives the real list from `--help` or an enum, and asserts set equality. | MUST |
| DOC-TYPE-19 | Split a reference page into one page per item once it passes roughly fifteen items. | An agent appends each new command to the file it already has, which is how one measured page reached 30 commands and 34,298 words. | `checks/page_type.py <paths>` counts top-level entry headings, warns past 15 and fails past 20 (argued, calibrated on that one 30-command page). Measured 6 hits over 53 reference pages. | CONSIDER |
| DOC-TYPE-20 | Surround every generated-reference directive with hand-written framing prose. | A bare directive is a stub, and a generator leaks internal detail that only a human pass removes. | `checks/page_type.py <paths>` word-counts stripped prose outside the directive. Fails at 0 words (measured: the real failure case is literally zero). Warns under 100 words (argued, calibrated on one 109-word page against one zero-word page). Measured 7 hits over 248 pages. | MUST |
| DOC-TYPE-22 | State a how-to page's goal or scope in prose before its first `##` heading. | A reader with no framing cannot tell whether the page solves their problem without reading every step. | `checks/page_type.py <paths>` fails a `doc_type: how-to` page with zero stripped prose words between the H1 and the first `##`. Measured 1 violation over 13 real how-to pages. | MUST |
| DOC-TYPE-23 | State a hard prerequisite before the first step, and use a heading for it only at three or more. | A step that silently assumes prior setup strands the reader who skipped it. | unverified: reading heuristic. Look for the setup a step assumes, stated above the first step rather than inside it. Measured 0 of 13 pages carry the heading and 1 of 13 states one inline. | CONSIDER |
| DOC-TYPE-24 | Phrase a fixed-order procedure as numbered reader actions, one heading per independent choice, and one fenced command for a single action. | Forcing every page into one flat numbered list breaks the pages that correctly describe several independent choices, measured at 8 of 13. | unverified: reading heuristic. Look for third-person system narration where the page numbers steps the reader is meant to perform. Measured 5 of 13 pages carry a real sequence and 4 phrase it as reader action. | SHOULD |
| DOC-TYPE-25 | Close a how-to page with a heading linking to related or next reading. | A page that solves one task and stops leaves the reader with no path to the next one. | `checks/page_type.py <paths>` requires a `see also`, `next steps`, `what's next` or `related` heading on `doc_type: how-to`. Measured 9 of 13 real pages already comply. | SHOULD |
| DOC-TYPE-26 | Open an explanation page with a sentence stating what it explains and, where it applies, why it matters. | A reader who does not know what question the page answers cannot judge whether to keep reading. | `checks/page_type.py <paths>` warns when no such sentence appears before the first `##`, because the check catches one phrasing of a broader requirement. Measured 12 of 14 real explanation pages comply. | SHOULD |
| DOC-TYPE-27 | Never write a numbered sequence of dependent reader actions as explanation content. | Order-dependent instructions belong in a how-to the reader can follow on their own. | unverified: reading heuristic. Look for numbered steps the reader must perform in order, and leave mechanism narration and order-free tip lists alone. A blanket ban on numbered lists is wrong on 3 of 5 measured instances. Measured 2 of 14 pages violate. | SHOULD |

## Rules: landing pages

`checks/landing_check.py` walks the markdown body. It parses no front matter,
because a front-matter parse resolved on only one of nine measured sites.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-TYPE-10 | Hold a landing page's lead-in positioning prose to one sentence and never stack two of them. | A multi-paragraph marketing hero is the default a model writes and the outlier among real landing pages. | `checks/landing_check.py <paths>` fails on more than one sentence terminator before the first heading or fence (measured across 5 fetched exemplars). It warns over 30 words (argued from those same 5 pages). Measured 21 hits over 22 landing pages. | SHOULD |
| DOC-TYPE-11 | Give every landing page a runnable command or a link menu before word 150. | A page that opens with a caveat and never resolves to an action leaves the reader nothing to do. | `checks/landing_check.py <paths>` takes the first of a fence, a link menu, or a block-level `<a href>` outside a list. It fails past 150 words (the same 150 DOC-NAV-06 uses). Measured on 9 real landing pages plus 2 controls: 3 fail, 0 report "cannot verify". | MUST |
| DOC-TYPE-12 | Cap a landing page at two button-style calls to action. | An ungrouped stack of seven calls to action gives the reader no hierarchy to read. | `checks/landing_check.py <paths>` counts button entries and raw `<a href>` buttons, and fails over 2 (re-measured exemplars: 2, 1 and about 3, against one real page at 7). It warns separately when ungrouped task links pass roughly 8 with no labelled group (argued). | SHOULD |
| DOC-TYPE-13 | State who the docs are for, through task-phrased link labels or one sentence naming the reader. | A grid labelled with product nouns tells a reader nothing about whether they are in the right place. | unverified: reading heuristic. Look for whether each grid label names a task or a product noun. `checks/landing_check.py <paths>` asserts only the true-zero case, that a grid or a reader-naming sentence exists at all. Measured 22 of 22 landing pages reach that true-zero case. | SHOULD |
| DOC-TYPE-14 | Never publish placeholder text. | Scaffold copy reaches published sites, and three placeholder tiles shipped verbatim in one measured repo. | `checks/landing_check.py --root .` fails any page matching `lorem ipsum`, `placeholder text`, `TODO: write` or `coming soon`. Runs over every type, not only landing. Measured 0 hits over 186 pages today. | MUST |
| DOC-TYPE-15 | Never state an adoption, popularity or trust claim without a link to its source. | No site in the calibration corpus carries social proof, so any such claim a model writes is invented. | `checks/landing_check.py --root .` requires an adjacent link on every hit, and the count-noun form of `trusted by`. Measured 3 hits over 248 pages under the loose pattern, 3 of 3 false positives from security-trust prose, and 0 under the tightened one. | MUST |

## Rules: README, CHANGELOG and CONTRIBUTING

A README is not a landing page. It renders on a forge with no hero slot, and a
path classifier that filed READMEs as landing pages handed 6 repositories the
wrong contract. A CONTRIBUTING file declares `doc_type: how-to`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-TYPE-32 | Put the project's plain-language description before any content that is not descriptive metadata. | A reader who cannot find out what the project is has no reason to read the rest. One fetched README opens with two sponsor tables. | `checks/page_type.py <paths>` strips a leading centered image, a run of badge-only lines and a table of contents, then requires prose before the first table. Measured 15 of 15 READMEs over 100 lines already comply. | MUST |
| DOC-TYPE-33 | Name or link the one action that takes a new reader from zero to using the project. | A README that never shows the install path sends the reader to the site to look for it. | `checks/page_type.py <paths>` accepts a fence under an install, quickstart, usage or getting-started heading, or a link whose text matches the same pattern. Measured 15 of 15 fleet and 7 of 7 external READMEs comply. | SHOULD |
| DOC-TYPE-34 | Link the project's documentation site from the README when a site exists. | A second copy of the docs forks into two answers the first time one side is edited alone. | `checks/page_type.py --root .` requires a link to the generator's deploy target when the tree carries a generator config. Measured 9 of 9 generator-having repos comply. | SHOULD |
| DOC-TYPE-35 | State or link a license in the README. | A reader who cannot find the license cannot use the project at work. | `checks/page_type.py --root .` accepts a `## License` heading, or a root `LICENSE` file plus a mention of it. Measured 14 of 15 READMEs comply. | SHOULD |
| DOC-TYPE-36 | State who the project is for, separately from what it does. | A reader can know exactly what a tool does and still not know whether they are the intended user. | unverified: reading heuristic. Look for a reader role, a prerequisite or a reader's problem in the first two paragraphs. Never scored against real READMEs, so no rate exists. | CONSIDER |
| DOC-TYPE-37 | Spell every changelog category heading exactly as Keep a Changelog states it, and never require all six. | Training data carries many changelog dialects, and a rule demanding all six would fail every changelog measured. | `checks/page_type.py <paths>` matches every `###` inside a version section against `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security` (Keep a Changelog 1.1.0). Measured 11 of 11 changelogs comply, and 0 of 11 ever emit the other three. | SHOULD |
| DOC-TYPE-38 | Never let a version section be a re-rendered commit log. | Keep a Changelog states this as an explicit don't, and its founding principle is that changelogs are for humans. | unverified: reading heuristic. Look for a version section where every line is a Conventional Commits subject copied verbatim. | SHOULD |
| DOC-TYPE-39 | State a migration path in every entry marked breaking. | A breaking entry with no migration sends the reader to the diff to work it out. | `checks/page_type.py <paths>` requires migration prose, an explicit "no migration needed", or a markdown link within 3 lines of a breaking marker, and fails a bare filename. Resolve any link with `checks/links_raw.py`. Measured 23 of 23 breaking entries carry inline prose, 0 carry a link, 2 carry a bare filename. | SHOULD |
| DOC-TYPE-40 | Never gate a build on the presence of an `## [Unreleased]` heading. | A generator correctly omits it between a release and the next merged commit, so the gate reddens a compliant repo. | `grep -rn "Unreleased" checks/` returns no failing assertion. Measured 10 of 11 changelogs carry no such heading right now and every one is compliant. | MUST |
| DOC-TYPE-41 | Credit Keep a Changelog when the file uses its template sentence. | Borrowing a template sentence and dropping its citation is the same defect as an uncited analogy. | `checks/page_type.py <paths>` fails a file matching "All notable changes to this project" with no `keepachangelog.com` link. Measured 9 of 11 files cite it, 2 do not. | SHOULD |
| DOC-TYPE-42 | On a CONTRIBUTING file, state prerequisites, setup, how to run the tests, the commit convention and a before-you-submit checklist, in that order. | Three independently authored files converge on exactly this order, and a contributor reads it once, top to bottom. | unverified: reading heuristic. Look for which of the five sections are missing, and accept a thin project that skips one. Read on 3 of 20 files. | CONSIDER |
| DOC-TYPE-43 | Never open a CONTRIBUTING file with marketing or onboarding-sell content. | Its reader has already decided to contribute, so re-selling the project wastes the one thing they came for. | `checks/page_type.py <paths>` requires the first element after the H1 to be prose or a setup step, never an image or a badge line. Read on the same 3 of 20 files. | CONSIDER |

## Rules: tier and first steps

The tier answers "how far into the product is this reader", and the type
answers "what shape is this page". They are two keys because no measured site
can produce all three tier values from its navigation. `DOC-DISC-23`, the product-shape key that selects the thresholds below, is
declared once per repository and defined in the last table of this file.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-DISC-14 | Give every task row in the plan a tier, and decide first-steps membership by dependency order rather than by rank. | Without a tier the ranked list never becomes navigation, and the most painful task outranks the entry task. | unverified: reading heuristic. Look for a tier on every row and an empty dependency list on every `first-steps` row. The `docs-plan` skill runs this as a schema check over its own artifact. | SHOULD |
| DOC-DISC-15 | End a first-steps page at one verified observable result, and justify a page that runs past its shape's budget. | A fixed step budget is wrong for a single binary and a one-command budget is wrong for a multi-system setup. | `checks/page_type.py <paths>` branches on the declared product shape. For `cli` and `hosted-service` it counts ordered items and command fences to the first success marker. Above 9 actions it requires an external-system list (9 from the longest measured quickstart). For `library` it flags above 4 fences (measured ceiling is 2 across 9 library quickstarts). The success marker is DOC-EX-02's `# doc:` key. Measured 1 hit over 248 pages, 0 of 1 false positive. | SHOULD |
| DOC-DISC-16 | Keep a first-steps page under about 100 words before its first command, and move any callout past it. | Padding before the first command is the agent's default and it costs the reader the fastest path to a result. | `checks/page_type.py <paths>` counts stripped prose from the nearest example-introducing heading, not from the H1. It treats `<<<`, `--8<--` and `{{#include` as command blocks. The 100 is unsourced and shared with DOC-TYPE-07, which owns the number. Measured 4 hits over 248 pages, 1 of them a detector gap that read 2019 words against a confirmed 185. | SHOULD |
| DOC-DISC-17 | Keep branching choices out of a page declaring `doc_type: tutorial`, including a branch written as prose. | A page cannot promise a single safe path and offer package-manager alternatives at the same time. | `checks/page_type.py <paths>` runs on `doc_type: tutorial` only. It fails a tab or code-group directive, and an "or, with", "alternatively" or "if you prefer" between two fences of one language. The same syntax passes on a how-to. Measured 0 scoped hits, because no page declares a type yet. | MUST |
| DOC-DISC-18 | Make every step of a first-steps or tutorial page produce a result the reader can see. | A step whose only effect is "no error" breaks the confidence chain the whole tier exists to build. | unverified: reading heuristic. Look for a printed value, a new file or a rendered page inside each step. A `>>>` line, a `#>` comment or an inline `// prints` comment satisfies it with no extra prose. Returns to MUST as a set comparison once the success marker is wired per step. | SHOULD |
| DOC-DISC-19 | Type a page as a tutorial only when the reader must assemble two or more interacting concepts. | An agent labels any onboarding page a tutorial, because that is the most frequent word for it. | unverified: reading heuristic. Look for two interacting concepts rather than one linear setup. Compare against a corpus base rate of 0 tutorials in 248 pages. | CONSIDER |
| DOC-DISC-20 | Have a reader who did not write the page walk a tutorial before it ships. | A passing script proves the commands run, not that an unfamiliar reader can follow the prose between them. | unverified: reading heuristic. Look for a named walkthrough reviewer on the change that added the tutorial. For an agent fleet that reviewer is a subagent with no repository context, reporting where it stalled. | SHOULD |
| DOC-DISC-21 | Put the next tier in a different top-level navigation group from the first-steps entry point. | A navigation break is what stops tier one absorbing everyday and integration content over time. | `checks/nav_depth.py --root .` confirms the first-steps pages and the everyday hub sit in different top-level groups, and skips a tree with no generator config. Measured on 9 of 22 repositories with a nav config, 1 real violation. | SHOULD |
| DOC-DISC-22 | State the production scope of a quickstart whose own commands are dev-only, with named before-you-ship items. | Shipping insecure defaults with no caveat teaches a bad habit as if it were best practice. | `checks/page_type.py <paths>` applies where the product shape is `hosted-service` or a multi-tenant `cli`, and requires a production sentence when a fence matches the dev-only trigger list. Measured 0 hits over 55 scoped pages, so the trigger list is narrow and the zero is not proof of compliance. | SHOULD |
| DOC-DISC-23 | Declare one product shape per repository, from `cli`, `library`, `hosted-service` or `framework`. Read it before applying any first-steps threshold. | The first-steps thresholds were calibrated on CLI and hosted-service examples. They misfire on a library. | Grep the docs config (a `mkdocs.yml` extra, a `pyproject.toml` tool table, or `docs.toml`) for one of the four values. Absence fails. The shape then selects the DOC-DISC-15 branch and the DOC-DISC-22 scope. | SHOULD, pinned |
| DOC-DISC-24 | Publish a per-entry parity table with a closed support-tier enum when a library wraps a CLI or a service. | Prose about what a library wraps is not falsifiable, so a drifted or missing wrapper is invisible. | `checks/page_type.py <paths>` finds a reference table whose header carries a tier legend, then fails any row whose tier value is outside that closed enum. DOC-TYPE-18 owns the code-surface version of this contract. | SHOULD |
| DOC-DISC-25 | State a first example's precondition before the call, whenever the example needs a binary, a key or a running server. | A wrapper library cannot reach a zero-setup result, so a "just works" quickstart fails for every real reader. | unverified: reading heuristic. Look for a precondition sentence above the first fence when that example touches a wrapped binary, an account key or a separate process. | SHOULD |

## Worked pairs

**Type mixing, DOC-TYPE-03 and DOC-DISC-17.** Wrong, a tutorial that branches:

```markdown
<!-- doc_type: tutorial -->
We are going to build a small index. If you prefer npm, run `npm i`, or with
Homebrew run `brew install`.
```

Right, one path in the tutorial and the choices in a how-to:

```markdown
<!-- doc_type: tutorial -->
By the end you have a working index. Run `brew install acme`.
Package-manager alternatives live in [Install](install.md).
```

**Troubleshooting entry, DOC-TYPE-08.** Wrong, generic steps:

```markdown
## Fixing index problems
1. Check your config.
2. Try again.
```

Right, the message the reader searched for, then cause, then fix:

```markdown
## Error: index not found
This issue occurs when the index was built under a different cache key.
Rebuild it with `acme index --rebuild`.
```

## Generator divergence

- **MkDocs Material.** `<!--` works. The include directive is `--8<--`, and
  every word count and first-command scan must treat it as a command block.
- **VitePress.** Front matter is normal here, so the declaration goes after it.
  The include directive is `<<<`. No landing check parses the front matter,
  because that parse resolved on one of nine measured sites.
- **mdBook.** Front matter is unsupported and renders as a horizontal rule plus
  a fake `<h2>` that enters the search index (0.5.3, measured). The include
  directive is `{{#include`. `SUMMARY.md` is excluded from the landing check,
  which reads the first chapter instead.
- **Docusaurus and MDX.** Only `{/* */}` works. An HTML comment is a hard build
  error under MDX 3.1.1, and Docusaurus 3 applies MDX parsing to plain `.md`
  unless `markdown.format` is `detect`.
- **Starlight.** Treat every page as MDX and use `{/* */}`. No fixture was ever
  built for this carrier.
- **Sphinx.** `..` for reStructuredText and `%` for MyST, both taken from the
  primary documentation. No fixture was ever built for either.

## Pinned decisions

Each is a default the adopter may override once, in one place.

1. **The nine `doc_type` values.** Every value exists because a rule already
   written cannot be expressed without it. Adding a tenth means adding the rule
   that needs it. Override by editing the enum in `checks/doc_declaration.py`.
2. **The three `doc_tier` values, required only on tutorial, how-to and landing.**
   Requiring a tier on every page would add roughly 150 more lines to a
   248-page migration. It would also force a tier onto reference and
   changelog pages.
3. **The scope of this family, DOC-TYPE-31.** Tracked markdown under a
   documentation directory, plus repo-root `README.md` and `CHANGELOG.md`, minus
   agent, build, cache and vendor directories. This deliberately differs from
   the navigation family, which gates on a generator config file. A page-type
   contract is about content, and the two measured runbook pages live in a
   repository with no generator config. Override by editing that one exclude
   list.

## Not studied

- **No page has ever declared a type in production.** Every scoped check here is
  inert until the backfill lands. The migration is 325 to 358 added lines across
  248 files.
- **Zero real tutorials exist in the calibration corpus.** The tutorial contract
  has never run against a real tutorial page.
- **Two real runbook pages exist**, and both sit in a repository with no
  generator config, so no check has run over them yet.
- **DOC-TYPE-05 was never calibrated.** Its opinion pattern has not been run
  against real prose, which is why it ships at CONSIDER.
- **The Starlight and Sphinx carriers rest on primary documentation**, not on a
  built site.
- **CONTRIBUTING was read on 3 of 20 files**, and no README was ever scored
  against DOC-TYPE-36's audience sentence.
- **Page-level accessibility is not governed here.** No rule in this family
  covers alt text, contrast, keyboard order or table semantics. The interactive
  half lives with the example rules.
- **Versioned documentation and translated documentation are out of scope.**
  Nothing here says what a declaration means on a versioned copy of a page.
