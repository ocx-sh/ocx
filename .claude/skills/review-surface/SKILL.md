---
name: review-surface
description: Use when a diff or PR is too big to review by eye and the question is "what should I actually look at" — sorts each changed line into wire / CLI / API / logic / test / doc tiers, then opens a clickable local page with the real hunks. A reading order, never a verdict.
user-invocable: true
disable-model-invocation: false
argument-hint: "[PR number | --base <ref> | --diff-file <path>]"
triggers:
  - "review surface"
  - "what should I review"
  - "triage this PR"
  - "review focus"
  - "what matters in this diff"
---

# Review Surface

A large diff hides a small change. On ocx#227 — 9,241 lines across 84 files — only **384 lines
(4.2%)** were production logic, and **2,816 of the 4,069 added Rust lines lived inside
`#[cfg(test)]` regions of production files**. File-level views cannot show that, which is why
GitHub's page reads as an undifferentiated wall.

This skill produces a **local, self-contained HTML page** with the diff sorted by what moves a
contract, opened in the owner's own browser. Nothing is uploaded.

## Run it

```sh
task claude:review-surface -- 227          # a PR (fetches via the GitHub API)
task claude:review-surface -- --base main  # current branch vs main (the default)
task claude:review-surface -- --diff-file /path/to.diff
task claude:review-surface -- 227 --no-open
```

The script declares its one dependency (pygments, for syntax highlighting) with
PEP 723 inline metadata, so `uv run --script` resolves it into a cached
ephemeral environment — no venv, nothing added to a pyproject. `uvx` is
`uv tool run`, for PyPI tools rather than local scripts, and refuses this path.

Writes `out/review-<slug>.html` (`out/` is gitignored) and opens it. Report the tier totals the
script prints, name the top one or two files worth reading first, and give the path — do not
paste the diff into the conversation.

## Tiers

Assigned **per line, first match wins**. A file appears under the most severe tier it carries.

| Tier | Rule | Why it ranks here |
|---|---|---|
| **T0 WIRE** | `#[serde(`, `deny_unknown_fields`, or a wire-file path (`oci/index/wire*.rs`, `oci/manifest.rs`, `project/{config,lock}.rs`, `package/metadata*`, `ocx_schema/`, `fixtures/index_wire/`) | Breaks *other programs*, including already-published packages. Nothing else in a diff can do that. |
| **T1 CLI & EXIT** | anything under `crates/ocx_cli/src/`, `cli/exit_code.rs`, `cli/classify.rs`, `*/error.rs`, `try_downcast!` | What a calling script types, parses and branches on. Flags, `--format json` shapes and exit codes are one contract. |
| **T2 API** | `pub fn/struct/enum/trait/type/const`, or `impl … for …` | New types and changed signatures. |
| **T3 LOGIC** | any other non-comment line under `crates/*/src/` | No contract signal — **read anyway**. |
| **T4 DOC** | `///`, `//!`, `website/`, `*.md` | In this repo rustdoc is design record; skim, don't skip. |
| **T5 TEST** | inside `#[cfg(test)]`, `crates/*/tests/`, `test/`, `fixtures/` | Counted, not ranked. |
| **T6 SCAFFOLD** | `.claude/`, `.agents/`, `taskfiles/`, `.github/` | Counted, not ranked. |

Only hunks carrying a production line are expandable. A production file's *test* hunks are
filtered out — including them would reproduce the exact problem the page exists to solve.

## The limitation, which must be stated when reporting

**Tiers rank by declaration shape, not blast radius, and the two come apart exactly where it
hurts.** On ocx#227 the largest single production file — `oci/index/local_index.rs`, 47 lines,
12% of all production logic in the PR — carries no serde attribute and no clap derive, so it
lands in **T3, at the bottom**, while a two-line serde tweak sorts above it.

Three things exist solely to contain that, and none may be removed:

1. **Biggest movers** prints the top production files by line count *above every tier*.
2. **T3 is enumerated by file and never collapsed to a count.** If anyone ever makes T3
   collapsible, the tool has become dangerous and should be deleted.
3. The page's contract is *"here is a reading order"* — never *"here is what you can skip"*.

Say this out loud when handing over a page. A reviewer who reads T0–T2 and merges is worse off
than with a flat diff, because the flat diff at least made no claim about what mattered.

## Gotchas

- **Run from the repo root.** The `#[cfg(test)]` region map reads each changed file by relative
  path; from elsewhere every read fails, zero ranges are produced, and all test lines silently
  reclassify as production — inflating the count roughly sixfold while looking plausible. The
  script refuses to start outside the root for exactly this reason.
- **`git diff` returns empty in this environment** (the RTK proxy swallows it), which reads as
  "no changes" rather than as an error. The PR path goes through the GitHub API with `curl`; the
  script hard-fails on an empty diff rather than rendering an empty page.
- **The headline churn is adds + deletes including blanks**, matching the forge's own number. The
  tier totals count only classified added lines. They differ on purpose — the gap between "what
  GitHub counts" and "what is worth reading" is the whole point.

## Related

- `/code-check`, `/security-auditor` — actual review, after this decides where to look.
- `/swarm-review` tiers on total `lines_changed`, so a diff like #227 classifies `max` on 9,241
  lines and spends an adversarial panel on 384 lines of content. Worth feeding it the production
  count instead.
- Design record: `.claude/artifacts/design_spec_review_surface.md`.
