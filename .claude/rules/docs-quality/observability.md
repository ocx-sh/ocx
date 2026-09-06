---
title: Documentation observability
summary: Link and drift gates, the signal manifest, error identifiers that resolve to a docs anchor, and the reader signals you defer
---

# Documentation observability

Grade documentation on whether it is still true and still findable, not on how
it reads. This file owns the DOC-OBS family: link gates, drift gates, the signal
manifest, and error identifiers that resolve to a docs anchor.

Contents: [Stand these up in order](#stand-these-up-in-order) ·
[The rules](#the-rules) · [The blocking split](#the-blocking-split-pinned) ·
[Fork detection](#fork-detection) ·
[Error identifiers by ecosystem](#error-identifiers-by-ecosystem) ·
[Link checking by generator](#link-checking-by-generator) ·
[Worked pairs](#worked-pairs) · [Pinned decisions](#pinned-decisions) ·
[Not studied](#not-studied)

## Stand these up in order

Cheapest first. Each step is useful on its own, so a project that stops after
step two still gained something.

1. **Free, today.** A docs issue template that applies a `docs` label
   (DOC-OBS-11), and the one-file signal manifest that records what exists
   (DOC-OBS-10). Neither needs a build, a site or a vendor.
2. **One afternoon, by hand.** Run the quickstart with a stopwatch and write the
   number down with its date (DOC-OBS-07). No repo in the 22-repo study corpus
   records this anywhere (22 of 22, `wave2-calibration-a.md` §3).
3. **The gates.** Built-output link checking (DOC-OBS-01) and a configured raw
   pass or none at all (DOC-OBS-02). The trigger matrix (DOC-OBS-03) and the
   blocking split (DOC-OBS-04 and DOC-OBS-05).
4. **Reader signals, last, with the bias stated.** A feedback widget waits for a
   real traffic denominator (DOC-OBS-16), and its sink discloses its own filter
   (DOC-OBS-17). A signal your stack cannot produce is deferred with its
   precondition named (DOC-OBS-12).

Error identifiers (DOC-OBS-18 and DOC-OBS-19) enter whenever the project emits
or consumes a stable error identifier, at any step.

**Rollout.** Every rule here enforces on changed files from the first commit and
warns whole-tree until the backfill lands. Then the whole-tree gate turns red
and stays red. Rows that carry standing violations in most repos say so.

## The rules

| ID | Rule | Rationale | Verification | Severity |
| --- | --- | --- | --- | --- |
| DOC-OBS-01 | Fail the docs build on a broken internal link or heading anchor, checked against built output rather than the raw markdown tree. | A raw-tree scan misreads explicit heading anchors and root-relative links, so it reports rot that does not exist. | `grep -rn -e 'mkdocs build --strict' -e mdbook-linkcheck -e include-fragments <ci-config-dir>` finds one of them, or run `lychee --include-fragments --config checks/lychee.toml <build-dir>` after the build. Prove it once by breaking one anchor and one root-relative link and confirming a non-zero exit. Measured 89% dead falling to 2.9% once both traps are handled (89% to 2.9%, `docs-shape.md` §5). Measured 7 of 9 sites already satisfying this through the generator (7 of 9, `wave2-calibration-a.md` §3). Changed files first. | MUST |
| DOC-OBS-02 | Give any pre-build markdown link pass a source root and an exclusion for every page whose anchors are generated at build time. Run no raw pass rather than an unconfigured one. | Without both, the pass either floods the log with false positives or silently checks nothing. | `checks/links_raw.py --root <site-src> <docs-dir>` exits 0 on a clean tree and lists every page it skipped. Each skip names a page whose anchors are generated, not a directory chosen by hand. Measured 65 phantom dead links traced to one four-line generated stub (65, `docs-shape.md` §5). Measured 7 of 9 sites running a raw pass with no source root (7 of 9, `wave2-calibration-a.md` §3). Changed files first. | MUST |
| DOC-OBS-03 | Keep a project-local table mapping each source-file glob to the doc file and section a change to it invalidates. | Without an explicit map, code-to-doc drift is found only when someone happens to reread the page. | `test -f docs/.meta/trigger-matrix.md` and the file holds at least 3 non-header rows (3 rows, one per trigger class, asserted floor). `grep -n -e crates/ -e services/ -e packages/ docs/.meta/trigger-matrix.md` must return zero hits from a template, because a copied source path is not portable. Measured 0 of 22 repos carrying the file (0 of 22, `wave2-calibration-a.md` §3). | SHOULD |
| DOC-OBS-04 | Merge a change whose general documentation is behind, and open a tracked issue for the gap in the same action. | A uniform block stalls a project with no writer capacity, and then gets bypassed. | unverified: reading heuristic. Look for the docs policy stating the non-blocking posture for default-class pages, and for a real issue reference beside every deferred drift finding. An empty issue reference fails. Normative source: [GitLab states that documentation reviews must not be blockers](https://docs.gitlab.com/development/documentation/workflow/), with a required post-merge follow-up issue. | SHOULD, pinned |
| DOC-OBS-05 | Fail the merge when a page declaring `doc_type: runbook` contains a step that no longer resolves. | A wrong runbook step costs incident minutes, and nothing else pages anyone when it rots. | `checks/doc_declaration.py --format json <docs-dir>` names the runbook set. Classify on the declaration comment only, never on a path (DOC-TYPE-02). Then unverified: reading heuristic. Look for a changed page in that set carrying an unresolved drift finding. Cost model: 3 stale steps in 30 at 8 to 15 minutes per wrong step ([ekline.io](https://ekline.io/blog/why-your-incident-runbook-lies-to-you-at-3-a-m-and-how-to-tell-before-the-page-fires)). The wave-1 path and frontmatter carriers matched 0 of 248 pages (0 of 248, `wave2-calibration-a.md` §3). Retrofit the declaration first, or this rule never fires. | SHOULD, pinned |
| DOC-OBS-06 | Write every runbook step against a runnable command, a live URL or a query, never a screenshot or a remembered value. | A step tied to nothing live decays without any signal that it decayed. | `checks/doc_declaration.py --format json <docs-dir>` names the runbook set. Then unverified: reading heuristic. Look for a step heading whose block holds no fenced block and no `https?://` match. Exempt a non-routable example address (RFC 1918, RFC 5737) from any liveness assertion. A scheduled job that runs each command and each URL is the stronger form and is not built here. | SHOULD |
| DOC-OBS-07 | Measure by hand how long the quickstart takes to reach a working result, and record the number with its measurement date. | An unrecorded onboarding time cannot regress visibly, so a broken step hides. | `docs/.meta/tthw.md` holds an integer and an ISO date. The gate fails when a page declaring `doc_type: landing` or `doc_type: tutorial` is in the changed paths and that file is not. Bands for reading the number: under 30 minutes rates 5 of 5 (30 minutes, Ably's band 5 via [Nordic APIs](https://nordicapis.com/why-time-to-first-call-is-a-vital-api-metric/)). Measured 22 of 22 repos and 39 of 39 candidate pages with no record (22 of 22, `wave2-calibration-a.md` §3). | SHOULD |
| DOC-OBS-08 | Never publish a docs metric you did not measure, and state its channel, its denominator and its date beside it. | A plausible invented percentage reads as evidence and cannot be corrected by a later reader. | `checks/strip_prose.py <docs-dir>` piped into `grep -nE -e '[0-9]+%' -e 'most users' -e 'most readers' -e 'most developers' -e 'nearly all users' -e 'nearly all readers' -e 'nearly all developers' -e 'the majority of'`. Every surviving hit needs a denominator, a channel and a date in the same paragraph, or it is deleted. The strip is load-bearing. On the raw tree the pattern measured 5 of 7 hits as false positives, from benchmark tables and changelog entries (5 of 7, `wave2-severity-ledger.md` §4). The stripped rate has not been re-measured. Changed files first. | SHOULD |
| DOC-OBS-09 | State in every documentation change what was removed, or state explicitly that nothing was removed. | Unreviewed growth reads as improvement while it buries the pages that already worked. | The PR template carries `Added:` and `Removed:` keys, and CI greps the PR body and fails when either key is missing or empty. The literal value `none` passes. The cost of unchecked volume: a 7.5% quality gain paired with a 7.2% stability drop at a 25% AI-adoption increase (DORA 2024, via [Swimm](https://swimm.io/blog/heres-what-the-2024-dora-report-has-to-say-about-code-documentation)). Measured 22 of 22 repos failing, 21 with no PR template (22 of 22, `wave2-calibration-a.md` §3). | SHOULD |
| DOC-OBS-10 | Record every observability signal in one file with three fields: status, review trigger, and bias disclosure. | A signal that exists but has no recorded review is indistinguishable from one nobody ever read. | unverified: reading heuristic. Look in `docs/.meta/observability.md` for a signal with status `instrumented` and no `last_reviewed` date. Look also for a review trigger that is a bare cadence word instead of a count or a release boundary (DOC-NAV-15). | SHOULD, pinned |
| DOC-OBS-11 | Ship an issue template that pre-applies a `docs` label, and name the trigger on which that label is triaged. | Without a labelled landing place, a deferred docs fix silently becomes no fix. | An issue template under `.github/ISSUE_TEMPLATE/` or the forge equivalent has `docs` in its `labels:` list. The trigger line names an existing release or iteration boundary, or a count of open docs issues. A bare cadence word fails (DOC-NAV-15). Measured 22 of 22 repos failing, 19 with no template directory (22 of 22, `wave2-calibration-a.md` §3). Surveyed teams tracking nothing at all: 39% (39%, [State of Docs 2025](https://www.stateofdocs.com/2025/documentation-metrics-and-measurement)). | SHOULD |
| DOC-OBS-12 | Defer any signal the current stack cannot produce, and record in the manifest the exact precondition that would unblock it. | A requirement no repo can satisfy trains readers to ignore the whole rule set. | unverified: reading heuristic. Look for a deferred manifest entry naming a checkable precondition, then check the precondition rather than the missing log. Worked example: agent-versus-human traffic share, whose precondition is a stated consumer question plus a host that exposes request logs. Zero-result search is not an example here, because its sink is priced and DOC-NAV-10 owns it. | SHOULD |
| DOC-OBS-13 | Never fail a docs build because a page's last-updated date is old. | No validated review interval exists, so a clock gate fails on a number nobody can defend. | `grep -rni -e days_since -e stale_after -e max_age <ci-config-dir>` returns zero hits. Measured 0 hits across 22 repos (0 of 22, `wave2-calibration-a.md` §3). The only numeric staleness model found anywhere is runbook-specific, which is why DOC-OBS-05 exists and this rule does not. | SHOULD |
| DOC-OBS-14 | Detect duplicated documentation by hashing normalized paragraphs across files, and never ban repetition sentence by sentence. | Two independently edited files claiming the same subject drift apart, while a restated default value on two pages helps the reader. | `checks/strip_prose.py --format json <docs-dir>` then the paragraph-hash pipeline in [Fork detection](#fork-detection). Flag any file pair sharing 3 or more identical paragraphs of 40 or more words. Threshold is an invented default (3 paragraphs of 40 words, calibrated at 3 hits and 0 false positives over 248 pages, `wave2-calibration-a.md` §3). Changed files first. | SHOULD |
| DOC-OBS-16 | Ship a per-page feedback widget only after the repo already reports a real, nonzero traffic number for 30 consecutive days. | A helpfulness percentage with no traffic denominator is the unmeasured metric DOC-OBS-08 already forbids. | The manifest names a custom-event-capable page-analytics signal reporting nonzero for 30 days (30 days, asserted default). Plausible, Umami and Fathom carry named events with properties. GoatCounter and Cloudflare Web Analytics do not, so they do not qualify. A repo with no generator and no site runs `gh api repos/<owner>/<repo>/traffic/views` instead, and that call must return 200. | CONSIDER |
| DOC-OBS-17 | Name a feedback signal's sink and that sink's own selection bias in the same manifest entry, beside any percentage it produces. | A sink that asks the reader to authenticate filters the count a second time, on top of ordinary survivorship bias. | The manifest entry for a `feedback` signal names its sink mechanism: giscus, a serverless `createDiscussion` call, or a vendor custom event. When the sink requires reader authentication the entry says so beside the percentage. [giscus.app](https://giscus.app/) states that visitors must authorize the app through the GitHub OAuth flow, and GitHub's `createDiscussion` mutation needs no reader account. | CONSIDER |
| DOC-OBS-18 | Resolve every stable error identifier the project's own code emits to a docs anchor a checker can confirm exists. | An identifier with no page sends the reader to a search engine instead of a jump. | Grep the error-defining source for its identifier pattern, such as an exit-code enum or a set of named error classes. Diff that list against the anchor set `checks/links_raw.py --format json <docs-dir>` already produces. Both differences must be empty. Reuse that resolver, do not build a second checker. Skip this rule when the project's errors carry only free-text messages. | SHOULD |
| DOC-OBS-19 | When error-handling code receives a docs link inside a dependency's own error payload, surface that link separately. | That link is the one part of the payload built to answer the reader's next question, and an opaque body dump hides it. | unverified: reading heuristic. Look in the function that consumes an external API's error body for a link-shaped field such as `documentation_url` or `help_url`. Check that it is read out and shown on its own line, not passed through inside the whole body. | SHOULD |

One habit sits behind every row and is not itself a rule here. Never name a
check that does not exist, and resolve on disk every path a verification names.
A stated gate that runs nothing reads as coverage and hides the gap it claims to
close.

## The blocking split, pinned

Two rows disagree on purpose, and the split is by blast radius rather than by
vote.

- **Default class does not block.** DOC-OBS-04 merges the change and opens a
  tracked issue. A uniform block on a large surface stalls and then gets
  bypassed.
- **Runbook class blocks.** DOC-OBS-05 fails the merge, because a wrong step at
  3am costs incident minutes and nothing else notices the rot.

The adopter picks this once, in the repo's docs policy, and every later
reference reads that one statement. Overriding the default class is a one-line
edit in one place. Overriding the runbook class is not recommended, because the
runbook class is the only one with a measured staleness cost.

DOC-OBS-05 fires only on pages that declare themselves. A repo with no
`<!-- doc_type: runbook -->` comment anywhere ships this rule permanently inert.
Retrofit the declaration onto the real runbook pages at rollout, or do not claim
the coverage.

## Fork detection

Two files that independently say the same thing drift apart. Two pages that
restate the same default value do not. So hash paragraphs across files, and
never ban a repeated sentence.

Run `checks/strip_prose.py --format json <docs-dir>` for the prose, then:

1. Lowercase each paragraph and collapse its whitespace.
2. Drop any paragraph under 40 words.
3. Hash each surviving paragraph and record which file it came from.
4. Count, per file pair, how many hashes both files share.
5. Report a pair at 3 or more shared hashes.

Thresholds are invented defaults calibrated once at 3 hits and 0 false positives
over 248 pages (3 paragraphs of 40 words, `wave2-calibration-a.md` §3). Raise
them if your corpus carries a large shared boilerplate block by design.

## Error identifiers by ecosystem

DOC-OBS-18 applies only where the project already emits a stable identifier. The
shape differs per ecosystem, and the identifier in the terminal must be the
identifier in the anchor.

| Ecosystem | Identifier in the terminal | Where it resolves |
| --- | --- | --- |
| Rust | `error[E0308]`, plus `rustc --explain E0308` | One static page per code in the [error index](https://doc.rust-lang.org/error_codes/error-index.html) |
| Node | `ERR_INVALID_ARG_TYPE` on `error.code`, which is stable while `error.message` is not | An anchor of the same name on [errors.html](https://nodejs.org/api/errors.html) |
| Python, via mypy | A bracketed code such as `[import-untyped]`, printed by default | Two fixed [error code pages](https://mypy.readthedocs.io/en/stable/error_code_list.html) |
| Go, via staticcheck | Check codes such as `SA1019` | Anchors on one [checks page](https://staticcheck.dev/docs/checks). The `go/analysis` `Analyzer.URL` field exists and `go vet` does not print it |
| Deno | The typed class name, such as `Deno.errors.NotFound` | A page per class, so the class name and the page title are one fact |
| A CLI with exit codes | The numeric exit code and its enum name | A table row in the reference page, bound to the enum by one test |

The last row is the cheapest instance and the one most often left unwired. A
documented exit-code table and the enum that produces it agree today by accident
until one test diffs them.

## Link checking by generator

DOC-OBS-01 accepts the generator's own strict build. Do not hard-code one tool.
Only the MkDocs row was measured in this program, on 9 real CI configs. The rest
name the generator's own documented mechanism and were not run here.

| Generator | Mechanism | Build output |
| --- | --- | --- |
| MkDocs Material | `mkdocs build --strict` fails on a broken internal link or anchor. Measured on 7 of 9 sites (7 of 9, `wave2-calibration-a.md` §3) | `site/` |
| VitePress | `vitepress build` fails on a dead internal link by default | `.vitepress/dist/` |
| mdBook | The `mdbook-linkcheck` backend, enabled in `book.toml` | `book/` |
| Docusaurus | `onBrokenLinks` throws by default, and `onBrokenAnchors` warns. Raise the anchor setting or the anchor half is not gated | `build/` |
| Starlight | An Astro build plus a links-validator integration | `dist/` |
| Sphinx | `sphinx-build -b linkcheck` for external links, and `-n -W` to make an unresolved internal reference fail | `_build/` |

A raw-markdown pass is a different job with different traps, and DOC-OBS-02 owns
it. Run `checks/links_raw.py` or run no raw pass at all.

## Worked pairs

### A raw link pass that checks nothing

Wrong, and it reports clean forever:

```yaml
- run: lychee 'docs/**/*.md'
```

Right, with the source root and the generated-anchor pages named:

```yaml
- run: checks/links_raw.py --root docs/ docs/
```

The first command resolves no root-relative link and no explicit `{#id}`
anchor, so it either floods or silently passes. The second prints every page it
skipped, which makes the exemption list reviewable instead of invisible.

### Burying a link the dependency handed you

Wrong, and the link survives in the bytes while vanishing from the signal:

```rust
format!("HTTP status {status}: {body}")
```

Right, with the field read out:

```rust
format!("HTTP status {status}: {message}\nSee: {documentation_url}")
```

### A PR body that admits nothing was removed

Wrong, because volume becomes free:

```markdown
## Changes
Added a new page on retries.
```

Right, and the literal `none` passes:

```markdown
Added: docs/how-to/retries.md
Removed: none
```

## Pinned decisions

A pinned row rests on a project decision rather than on measurement. Each one is
a default the adopter may override once, in one place, by editing that row.

- **DOC-OBS-04 and DOC-OBS-05, the blocking split.** Default class does not
  block, runbook class does.
- **DOC-OBS-10, the manifest shape.** One file, three fields per signal. No
  source ships this shape, so it is this program's own decision.

## Not studied

Named holes, not silence.

- **No validated freshness interval exists.** No source surveyed supports a
  review-every-N-days number. The one numeric staleness model found is
  runbook-specific. This is why DOC-OBS-13 forbids a clock gate.
- **DOC-OBS-08's tightened pattern was never re-measured.** The 5-of-7
  false-positive rate predates the strip. Re-run it over the same corpus before
  raising this rule to MUST.
- **DOC-OBS-06's scheduled harness is unbuilt and unpriced.** The shipped form is
  a per-step read, not a job that runs each command. MUST returns when the job
  ships and its cost is known.
- **The scope gate is undecided, and it bites here.** If the file list is built
  from a directory holding a generator config, a committed `docs/` tree with no
  generator is never read. That is exactly where runbook pages tend to live, so
  DOC-OBS-05 and DOC-OBS-06 would ship inert. Scope the declaration check to the
  committed docs tree when a repo has no generator config.
- **Vendor figures behind DOC-OBS-16 are second-hand for one vendor.** Umami's
  own pricing page did not render to a fetch, so its hosted-tier numbers come
  from third parties. The self-host cost and license are confirmed directly.
- **The GA4 exclusion rests on stated mechanisms, not a live session.** No
  DebugView run was made. A ten-minute check would close it.
- **A hosted search vendor's free-tier analytics dashboard is unconfirmed.**
  Verify your own dashboard after acceptance rather than assuming the metric set.
- **Time to first result stays hand-measured.** Whether an existing tested-example
  harness can emit it as a byproduct was not tested. If it can, DOC-OBS-07
  becomes measured instead of recorded.
- **Versioned docs, translations, and print or offline output.** No signal in this
  family was studied against any of them.
