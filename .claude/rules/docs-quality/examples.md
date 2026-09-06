---
title: Tested examples, recordings and interactive elements
summary: The DOC-EX family. The gate is the test, the recording is an optional view on a passing test, and the interactive layer is a per-generator support contract.
---

# Tested examples, recordings and interactive elements

Every runnable example in the docs is backed by a test in the same required
gate as the unit tests. A terminal recording is an optional view on a test that
already passes, never a substitute for one.

Contents: [The gate](#the-gate) - [Fence marking](#fence-marking) -
[The recording layer](#the-recording-layer) -
[The interactive layer](#the-interactive-layer) -
[Mechanism by language](#mechanism-by-language) -
[Fence tags, wrong and right](#fence-tags-wrong-and-right) -
[Binding, wrong and right](#binding-wrong-and-right) -
[The recording contract](#the-recording-contract) -
[Generator divergence](#generator-divergence) -
[Pinned decisions](#pinned-decisions) - [Not studied](#not-studied)

All checks below read source markdown. `checks/doc_examples.py` carries the
DOC-EX modes. `--changed-only` limits a run to the diff, which is the rollout
setting for every row that says so.

## The gate

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-EX-01 | Back every runnable documented example with one automated test in the same required gate as the unit tests. | A documented command drifts away from the tool it demonstrates and nothing reports it. Set-diff has no false positives by construction, because it only reads fences the author marked runnable. | `python3 checks/doc_examples.py --root . --changed-only` for the merge gate, whole-tree as a warning until the backfill lands. It set-diffs runnable-tagged fences against fences carrying a DOC-EX-02 binding key. | MUST |
| DOC-EX-02 | Bind a page to an out-of-page test with a declared key in the test header, never with a mirrored file path. | A mirrored path makes every move of the test tree break every page that cites it. Measured at 66 live `# doc:` bindings with 0 orphans on the one public worked example. | `python3 checks/doc_examples.py --root . --bindings` lists declared keys and cited keys and diffs both directions. Both lists must come back empty. | MUST |
| DOC-EX-03 | Test an example with the language's own doctest runner unless it needs a real external system. | A bespoke harness gets built for work an installed tool already does. For TypeScript an agent reaches for a Markdown parser without knowing Twoslash and `deno test --doc` exist. | `grep -rni -e sybil -e 'cargo test --doc' -e 'mdbook test' -e twoslash -e deno <dependency manifests> <generator config> <ci config>` before adding any doc-example module. A new harness with no external system in scope is the finding. | SHOULD |
| DOC-EX-04 | Start a doc-example harness as one file that globs the example tree and runs each file as a subprocess. | A small project copies the scale of a large worked example and never starts. The floor is 55 lines with no dependency (measured, the shipped harness mode). | `python3 checks/doc_examples.py --harness checks/fixtures/doc_examples/` runs the bound example files. `--self-test` runs the passing and the deliberately broken fixture, and must exit 0 then 1. | SHOULD |
| DOC-EX-06 | Wrap a snippet that must not run in a paired marker that states why. | A bare skip flag reads later as an oversight rather than a decision. Measured at 1 hit in 249 pages across nine real doc sites, 2026-09. | `python3 checks/doc_examples.py --root .` requires a `: <reason>` suffix on the open marker and a matching close marker before the next open marker or end of file. | MUST |
| DOC-EX-07 | Make a failing example test name the doc page, not only the test file and line. | Without the page name every CI failure costs a manual reverse map back to the reader's view. | Force one bound example to fail and read the CI output for the declared binding key and the human title. A one-off probe cannot carry a MUST. | SHOULD |
| DOC-EX-08 | Do not claim a shown example is identical to what ran when the harness substitutes any value. | The claim is falsified the first time a reader checks a parallel-safe test run. Measured at 709 fleet-wide hits with 662 off-target, a 93.4 percent false-positive rate at page-type scope (calibration, 2026-09). | `grep -rni -e exactly -e identical -e verbatim -e byte-for-byte <the harness mechanism's own doc tree>`. Default scope where no such tree exists: the pages that carry a DOC-EX-02 binding key, because only those can make the claim. Each hit needs a stated canonicalization step. | SHOULD |
| DOC-EX-09 | Confirm a doc-collection glob reaches a page nested two levels deep before trusting it. | A tool whose docs promise `fnmatch` may call `pathlib.Path.match`, whose `**` matches one level only (Python `pathlib.PurePath.match`). | Add one fixture page two directories below the collector root, run the collector, and confirm the page is collected. | SHOULD |
| DOC-EX-10 | Ship a scan for documented commands with no backing test as a lead list, never as a merge gate. | The scan flagged 20 mentions on one real page and 11 were legitimate annotated history, roughly 55 percent false positives (measured, 2026-09). | `grep -rni -e unbacked -e orphan-command <ci config>`. Any hit must sit in a non-blocking job. Run the scan against the DOC-EX-06 markers first to strip known exemptions. | SHOULD |
| DOC-EX-23 | Never let one tool both rewrite a page's fenced output and serve as that example's only correctness check. | A rewrite-in-place runner captures whatever a flaky or newly broken command printed as the new expected output. | For any fence carrying a rewrite-in-place directive, `grep` for a second independent check. It is either a committed last-known-good output diffed in CI, or a bind-and-assert test on the same command. | CONSIDER |

## Fence marking

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-EX-05 | Tag every code fence you add or change with a language from the project's declared tier list. | An untagged fence is invisible to every drift check, so it neither passes nor fails. Measured at 151 of 1,281 fences untagged, 11.8 percent, across nine real doc sites, 2026-09. | `markdownlint --config checks/markdownlint.jsonc` with MD040 `allowed_languages` set to the tier list. Run on changed lines only until the backfill lands. | SHOULD |
| DOC-EX-20 | Write a fence tier suffix as one whitespace-free token joined to the language by a hyphen. | A space in a fence info string is unparsed by `pymdownx.superfences`, and the damage spreads past that fence into later page content. Measured on real builds of MkDocs Material 9.7.7, mdBook 0.5.3 and VitePress 2.0.0-alpha.16. | `python3 checks/doc_examples.py --root . --changed-only` rejects any fence info string containing whitespace. Fixture pair: one hyphen-tagged fence and one space-tagged fence built under `pymdownx.superfences`. | MUST |
| DOC-EX-21 | Use a tool-native space-separated fence attribute only on a site that will never render under MkDocs Material. | Banning `ts twoslash` and `ts ignore` outright costs real functionality on the two generators that parse them correctly. 7 of 9 real sites run MkDocs Material (measured, 2026-09). | `python3 checks/doc_examples.py --root . --changed-only` fails a whitespace info string whenever `mkdocs.yml` is present in the docs tree, and reports it otherwise. | SHOULD |

## The recording layer

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-EX-11 | Keep the recorder out of the gate so the example suite passes or fails without it. | A recorder wired into the gate lets a rendering step fail a correctness check. | Disable the recording step and re-run the required gate. The result must not change. | MUST |
| DOC-EX-12 | Produce every terminal recording by running a real command, never by typing a transcript. Delete an unused non-executing mockup mode rather than leave it available. | A fabricated transcript is the exact artifact the tested-example system exists to prevent. An unused way to fabricate one is a standing invitation. Measured at 0 uses of the mockup component across 36 live embeds, 2026-09. | `grep -rn` the component library for any mode that builds player input from static text and timestamp pairs. Any use on a page with no backing test fails. A mode with zero uses beside a live mode is deleted. | MUST pinned |
| DOC-EX-13 | Commit a recording only when no build step regenerates it. | Committing a regenerated file invites drift between the file and the test that made it. Measured at 0 tracked casts on a regenerating repo and 1 on a repo with no pipeline, 2026-09. | `git ls-files <recordings dir>` read against the build task graph. Tracked and regenerated at once is the contradiction to catch. | MUST pinned |
| DOC-EX-14 | State the cast version you write and the player version you pin, and check the player parses it. | Asciicast is three formats, and v3 changed the header schema and the timestamp base (asciicast v3 spec). | `head -c 40 <a generated cast>` for `"version": N`, then `grep` the pinned player's parser for a matching `parseAsciicastVN` branch. | SHOULD |
| DOC-EX-15 | Default a recording-bound player to no autoplay. | Auto-starting motion triggers WCAG 2.2.2 Pause Stop Hide, Level A. Not creating it is cheaper than answering it. | `grep` the component's `autoPlay` default. It must evaluate false whenever a recording source is set. Then `grep` pages for explicit overrides. | MUST |
| DOC-EX-16 | Leave the embedded player's own accessible controls enabled. | A custom skin is far likelier to drop the pause button's label than the upstream library is (WCAG 2.2.2, Level A). | `grep` the player init for a `controls:` option. Absent, `"auto"` or `true` passes. `controls: false` or a CSS-hidden bar with no keyboard replacement fails. | MUST |
| DOC-EX-17 | Check `prefers-reduced-motion` before starting playback. | A viewer who asked the operating system for less motion still gets a full replay without it. Measured at 0 hits across both recording sites, 2026-09. WCAG 2.3.3 places this at AAA, which caps the severity. | `grep -rn "matchMedia('(prefers-reduced-motion" <player init path>`. Absence is the finding. | SHOULD |
| DOC-EX-18 | Add a declarative recorder such as VHS only when no page-bound acceptance-script tree exists. | Two script formats mean two discovery paths and two classes of sanitization. Measured at 0 `.tape` files fleet-wide, 2026-09. | `find . -iname '*.tape'`. A hit beside a page-bound acceptance-script tree fails. | SHOULD |

## The interactive layer

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| DOC-EX-24 | Set `content.code.copy` and `content.code.annotate` in `theme.features` on a MkDocs Material site. | The copy button is off by default on that generator and costs one config line. Measured at 7 of 7 real MkDocs Material sites already setting it, 2026-09. | `grep -A5 'features:' mkdocs.yml \| grep 'content.code.copy'`. A site with a `mkdocs.yml` and no match fails. | SHOULD |
| DOC-EX-25 | Do not add a component or script to give VitePress or mdBook a copy button. | Both ship one, so a hand-rolled button is dead code plus a second surface to keep accessible. Sources: mdbook-core `config.rs` defaults `copyable` to true, and VitePress `preWrapper.ts` injects the button unconditionally. | For mdBook, confirm `clipboard*.min.js` appears in a fresh `mdbook build` and that `book.toml` does not set `copyable = false`. For VitePress, confirm no custom copy component exists. | SHOULD |
| DOC-EX-26 | Present parallel install or usage paths as tabs only when the reader's own context decides which path they need. | Tabbing a step that has one right answer manufactures a decision the reader does not have to make. | unverified: reading heuristic. Look for a tabbed block whose one-sentence answer to "what varies between these tabs" names nothing about the reader. | SHOULD |
| DOC-EX-27 | Never make a live sandbox the reader's only way to see a documented example. | Full WebContainers support needs SharedArrayBuffer under cross-origin isolation, which is Chromium-only at full strength (webcontainers.io browser support). | For any page embedding a live sandbox, `grep` the page source for a fenced block carrying the same code. It must not sit only inside the sandbox's initial-file payload. | MUST |
| DOC-EX-29 | State mdBook's `runnable` and `editable` playground keys explicitly whenever the book contains a Rust example. | An unstated default reads later as nobody deciding, and a book wanting no Run button silently keeps one. | `grep -A3 '\[output.html.playground\]' book.toml`. Its absence on a book with fenced Rust blocks is the finding. | SHOULD |
| DOC-EX-30 | Set `#![doc(html_playground_url = "https://play.rust-lang.org/")]` on a published crate whose doc comments carry runnable examples. | The rustdoc book states that without the attribute there are no Run buttons. Measured at 0 of 3 real crates setting it, 2026-09. | `grep -rn html_playground_url <crate root>`. Its absence beside doctested public examples is the finding. | SHOULD |
| DOC-EX-32 | Do not add Twoslash, a live sandbox or a try-it console to a project with no TypeScript reference and no OpenAPI surface. | Naming a mechanism for a surface that does not exist yet is speculative scaffolding. Measured at 0 of 23 real docs surfaces carrying either, 2026-09. | `find . -name 'openapi*.yaml' -o -name 'openapi*.json'` and a search for `.d.ts`-backed reference pages. Zero hits on both means no action is required. | CONSIDER |
| DOC-EX-33 | Reserve a tooltip or an abbreviation for a term that would otherwise force a definitional clause into the sentence. Never hide content the reader must read to follow the page. | A load-bearing definition hidden behind hover is invisible to a plain-text reader and to anyone not using a mouse. No project anywhere measures whether a tooltip is opened, so there is no engagement threshold to cite. | unverified: reading heuristic. Look for a sentence that becomes false or unreadable once the tooltip's definition text is removed. | CONSIDER |
| DOC-EX-34 | Make a hover or focus tooltip keyboard reachable, pointer hoverable, and dismissible with Escape. | A trigger passed through as a bare span loses focus behaviour silently. That is a WCAG 2.1 SC 1.4.13 failure at Level AA, not a matter of taste. | `grep -rni -e tabindex -e keydown -e escape -e aria-describedby <the tooltip component source>`. No keyboard path is the finding. Then tab to a trigger with no mouse, move a pointer onto the popup, and press Escape. | MUST |

DOC-EX-33 and DOC-EX-34 were misfiled in the research corpus under two DOC-TYPE
numbers already held by the declaration-carrier rules. They are renumbered here
and belong to this family, because both govern an interactive element on a page.

## Mechanism by language

Match the mechanism to what the example exercises, not to the project's main
language. Status is as of September 2026.

| Surface | Runner | Status | What it does not do |
|---|---|---|---|
| Shell, or anything needing a real registry, network or PTY | A page-bound acceptance-script tree, one script per binding key | No mainstream doctest tool models these side effects | Nothing generic. You write and own the harness. |
| Python | Sybil 10.1.0, over `docs/**/*.md` plus docstring doctests | Current | Its `should_parse` calls `pathlib.Path.match`, so `**` matches one level. See DOC-EX-09. |
| Rust, in doc comments | `cargo test --doc` | Current | Nothing outside the crate's own doc comments. |
| Rust, inside an mdBook | `mdbook test` | Current | Rust only by design, so it cannot be a polyglot repo's only mechanism. |
| TypeScript, a type-level claim | Twoslash 0.3.9, with `@shikijs/vitepress-twoslash` 4.4.3 for VitePress | Current, published 2026-06-22 and 2026-08-10 | Never executes the sample and never captures output. |
| TypeScript, a sample that must execute | `deno test --doc`, or `deno check --doc-only` for types alone, Deno 2.9.6 | Current, released 2026-08-27 | Runs under Deno, so Node-specific code is a real gap. |
| TypeScript, no doctest tool wanted | `node <file>.ts`, type-stripping only, stable since Node 23.6 | Current | Strips types, never checks them. |
| Go | `go test` Example functions, transcluded into the page with `embedmd` | Current, `embedmd` pushed 2026-04-11 | No native Markdown-fence runner exists. An authored `go` fence is untested by construction. |
| Anything else | Subprocess per example file, dispatched by suffix | Shipped as the harness mode of `checks/doc_examples.py` | Asserts an exit code only. No type-checking and no output diff without an added step. |

Five JavaScript doctest tools are dead and must not be recommended:
`jsdoctest` (2017), `markdown-doctest` (2020), `tsdoc-testify` (2019),
`@power-doctest/markdown` (2025-07) and `bashup/mdsh` (2022). Vitest in-source
testing tests source files, not fenced Markdown, and Bun ships a Markdown
parser with no doctest feature.

A rewrite-in-place runner and a bind-and-assert runner are different contracts.
Rewriting regenerates the page from a live command. Asserting leaves the page
alone and fails a separate test. DOC-EX-23 exists because one tool doing both
accepts a broken command's new output as the new expectation.

## Fence tags, wrong and right

A tier suffix is one hyphen-joined token. The reason is not highlighting, it is
parsing.

````text
```ts twoslash
const x: number = 1
```
````

Under `pymdownx.superfences` that line is not an opening fence at all. The
backticks print as text, and the next paragraph and the next real fence get
swallowed into one wrongly classed block. Write the token instead.

````text
```ts-twoslash
const x: number = 1
```
````

The hyphenated form degrades safely on all three generators. See
[Generator divergence](#generator-divergence) for what each one does with it.

## Binding, wrong and right

Bind by a declared key in the test header, never by mirroring the doc tree into
the test tree.

```text
test/doc_scripts/user-guide/install.sh   # path mirrors docs/user-guide/install.md
```

Moving either tree breaks the pairing with no error. Declare the key instead.

```text
# doc: user-guide-install
# title: Install the CLI
# expect_exit: 0
```

The page cites `user-guide-install`. Either tree can move freely. The set-diff
in DOC-EX-01 reads the same key, so one convention feeds both checks.

## The recording contract

A recording is a view on a test that already passes. Six clauses, all of them
rules above.

- The cast comes from a real run. A typed transcript is the artifact the gate
  exists to prevent (DOC-EX-12).
- The recorder never sits in the required gate (DOC-EX-11).
- Autoplay is off whenever a recording source is set (DOC-EX-15).
- The player's own controls stay enabled and labelled (DOC-EX-16).
- Playback checks `prefers-reduced-motion` first (DOC-EX-17).
- The cast version written and the player version pinned are both stated, and
  the player is confirmed to parse that version (DOC-EX-14).

Commit policy is a branching rule, not a preference. Commit the cast only when
no build step regenerates it (DOC-EX-13). A repo whose site build regenerates
every cast tracks none. A repo with no pipeline tracks the one it re-records by
hand. Both are the same rule read against a different repo shape.

The one public worked example is ocx. It runs 66 acceptance-tested scripts
bound to pages by a `# doc:` key. 35 of those scripts opt into a cast with a
per-script flag. The cast pipeline runs only in the site build, never in the
merge gate. So 31 gated examples ship with no recording at all, which is the
evidence that the cast is not load-bearing for correctness.

## Generator divergence

The mechanics genuinely differ, so a rule that reads as universal is often not.

**Fence tier tokens.** MkDocs Material 9.7.7 falls back to Pygments' `text`
lexer for an unknown hyphenated token, silently and cleanly. mdBook 0.5.3 falls
back to highlight.js `no-highlight` and logs it. VitePress falls back to Shiki
`txt` with a build warning. A fence with no tag at all is worse on mdBook than
an unknown hyphenated one, because highlight.js then guesses a language.

**The space form.** Only MkDocs Material breaks. mdBook pulls the first token
out with its own regex, and VitePress splits the info string per CommonMark.
That is why DOC-EX-21 gates on `mkdocs.yml` being present rather than banning
the form outright.

**Copy buttons.** Opt-in on MkDocs Material through `content.code.copy`.
Shipped by default on VitePress and on mdBook. DOC-EX-24 and DOC-EX-25 are the
same obligation pointing in opposite directions.

**Tabs.** Native on MkDocs Material through `pymdownx.tabbed` and on VitePress
through `::: code-group`. Absent on mdBook, where parallel paths are two
headings or two pages.

**Tooltips and glossaries.** MkDocs Material has the `abbr` extension, with
`pymdownx.snippets` and `auto_append` for a shared glossary file. VitePress has
no native equivalent, so a tooltip there is a hand-built component, and
DOC-EX-34 applies to that component. Of the three, only mdBook has neither, so
a glossary there is a plain page and a normal link.

**Run buttons.** mdBook reads `[output.html.playground]` in `book.toml`
(DOC-EX-29). Rustdoc reads `html_playground_url` on the crate root
(DOC-EX-30). Neither adds one on its own.

## Pinned decisions

A pinned row is a default you may override once, in one place, and the row says
so. Two rows in this family are pinned.

- **DOC-EX-12**, a recording is a real run. Override only by deleting the rule,
  not by adding an exception, because an exception is the fabrication path.
- **DOC-EX-13**, commit a cast only when nothing regenerates it. This replaces
  a single pinned default with a branching rule, because the answer is readable
  from the repo shape.

## Not studied

Named holes, not silence.

- The recording pipeline's wall-clock cost. The timing was measured at a
  22-script baseline. The count is confirmed at 35, so the 59 percent growth is
  current, but the timing was never re-run.
- Tooltip engagement. No project measures whether a tooltip is ever opened, so
  DOC-EX-33 states a shape and cites no threshold.
- The term count at which inline tooltips stop working and a glossary page
  starts. No evidence anywhere.
- OpenAPI try-it console vendors and the Sandpack staleness signal. Both rest
  on vendor status that ages inside a rule file, so they stay in the research
  corpus. Re-read the vendor's own pricing page or README before adopting one.
- Go transclusion. Real and current, and held in the corpus for the same
  reason: no rule here should carry a release date that goes stale.
- Accessibility outside the terminal player and the tooltip. Alt text,
  contrast, keyboard order and table semantics are governed nowhere in this
  family.
- Sphinx and reStructuredText fence behaviour. The `..` and `%` carriers rest
  on primary documentation with no built fixture.
