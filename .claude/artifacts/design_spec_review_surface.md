# Design Spec: `surface` — deterministic diff triage for review focus

<!--
Produced by an 8-agent design workflow (4 survey / 3 competing designs / 1 judge).
Origin: ocx PR #227 measured 9241 lines of churn carrying 384 lines (4.2%) of
production logic. The owner's complaint: "the interface shows me all changes
equally but not those I should focus on."
Status: PROPOSED — not built. v0 is a weekend of work.
-->

## Measured basis (ocx PR #227)

| Area | Churn | % |
|---|---:|---:|
| Rust production files | 5230 | 56.6% |
| Acceptance tests (pytest) | 1725 | 18.7% |
| `.claude` config/plans/artifacts | 1574 | 17.0% |
| Rust test/fixture files | 331 | 3.6% |
| Config / CI | 264 | 2.9% |
| Docs + website | 117 | 1.3% |
| **TOTAL** | **9241** | |

Of 4069 **added Rust** lines: test code 2816 (69.2%), doc comments 530 (13.0%),
**production logic 384 (9.4%)**, blank 210, inline comments 129.

**True reviewable core = 384 lines = 4.2% of the PR**, across 29 files, largest
single file 47 lines (`oci/index/local_index.rs`).

Method caveat worth carrying into the implementation: an early run of the
classifier reported 580, and another 2459, because `#[cfg(test)]` region
detection reads the working-tree file by relative path and a wrong cwd silently
yields no ranges — reclassifying every test line as production. The final script
refuses to run unless it is at the repo root. **Any tool built on this heuristic
inherits that failure mode and must assert its preconditions loudly.**

---

## 1. VERDICT

**Proposal 1 (`surface`) wins the spine.** All three proposals converged on the same classifier â path allowlist + derive-attribute scan + `#[cfg(test)]` brace tracking â so the only real difference is what wraps it. Proposal 1 wraps it in nothing: one stdlib Python script, one task, one pytest, plain text out, zero tokens, zero build, ~1 day, and every tier assignment is a pure function of `(diff, HEAD)` that a fixture test pins. Proposal 2 spends an Opus turn per invocation to produce narrative that can hallucinate consequences the anchors don't catch, and Proposal 3 spends half a day on an HTML renderer plus a staleness problem to solve a sorting task that `less` already handles â both are the same classifier with a bill attached. Where they beat the winner is in three specific, cheap ideas worth grafting: LENS's **UNPARSED escape hatch** and **TEST-ONLY / NO-CALLERS badges**, and review-focus's **production-ratio bail-out** and **coverage cross-ref**.

## 2. SCORECARD

| Proposal | Solves real problem | Build cost (5=cheap) | Per-use cost (5=cheap) | Robust when wrong | Fits repo conventions | **Total** |
|---|---|---|---|---|---|---|
| **1 â `surface` (static analyzer)** | 4 | 5 | 5 | 4 | 5 | **23** |
| 2 â `/review-focus` (AI skill) | 5 | 3 | 2 | 3 | 4 | **17** |
| 3 â `lens` (HTML dashboard) | 4 | 2 | 3 | 3 | 2 | **14** |

Notes on the scores that matter: P2 loses per-use because an Opus turn per diff read is a recurring tax on a task the user will run several times a day; it loses robustness because "this breaks older clients" is exactly the sentence a model fabricates and the `file:line` anchor rule only validates *locations*, not *consequences*. P3 loses fits-repo because `out/*.html` opened in a browser is the wrong surface for a terminal + agent workflow, and its output cannot be piped into `swarm-review` without also emitting the JSON â at which point the JSON was the product and the HTML was decoration.

## 3. THE RECOMMENDED DESIGN

### `surface` â deterministic diff triage

One Python script. Reads a diff, emits a ranked reading order. No LLM in the hot path, no nightly toolchain, no new dependency, no new crate.

> PR #227 is 9241 lines. Surface says 384 of them run in production, 137 of those move a contract, and the other 8857 are tests, rustdoc and `.claude/` scaffold you already said you don't care about.

#### 3.1 Taxonomy

Four review tiers, three ledger tiers, one escape hatch. Fewer tiers than any of the three proposals on purpose â **EXIT/ERROR is merged into CLI**, because flags, JSON output and exit codes are one contract from a calling script's point of view, which is exactly the user's framing ("input/output interfaces, CLI changes"). Rows carry a `[flag]`/`[json]`/`[exit]` marker so the distinction survives the merge.

Assignment is **per changed line, first match wins, T! â T6**. Lines are counted in their own tier; a file appears in every tier where it has lines. "Not test" means: not inside a `#[cfg(test)]` region and not under `crates/*/tests/**`, `test/**`, `**/fixtures/**`.

| Tier | Name | Assignment rule |
|---|---|---|
| **T!** | **UNPARSED** | The region scanner failed on this file (brace depth negative, or EOF at depth > 0). Whole file listed here, at the top, never silently demoted. *(grafted from LENS L-1 â the single best idea in Proposal 3.)* |
| **T0** | **WIRE** | Not test, and: (a) line matches `#\[serde\(`, `deny_unknown_fields`, `#\[non_exhaustive\]`; or (b) the enclosing item's derive block contains `Serialize`/`Deserialize` and the line is a field, variant or the item header; or (c) file in the wire allowlist â `oci/index/{wire,wire_writer,ocx_index,oci_index}.rs`, `oci/manifest.rs`, `project/{config,lock}.rs`, `package/metadata*.rs`, `package/resolved_package.rs`, `config.rs`, `config/{registry,patch,managed}.rs`, `crates/ocx_schema/**`, `crates/ocx_lib/tests/fixtures/index_wire/**`; or (d) line changes `LOCK_VERSION`/`SUPPORTED_LOCK_VERSION`/`schemas/[a-z-]+/v\d`. |
| **T1** | **CLI & EXIT** | Not test, and: (a) file under `crates/ocx_cli/src/**` and line matches `#\[(arg\|command\|clap)\(` or the enclosing item derives `Parser`/`Subcommand`/`Args` â `[flag]`; or (b) file under `crates/ocx_cli/src/api/data/*.rs` and line is a field of a `Serialize` struct or inside an `impl Serialize` â `[json]`; or (c) file is `cli/exit_code.rs`, or line matches `try_downcast!\(`, or line is inside an `impl ClassifyExitCode`, or line adds a variant to an item whose derive block contains `Error`/`thiserror` â `[exit]`. |
| **T2** | **API** | Not test, and line matches `^\s*pub(\([^)]*\))?\s+(fn\|struct\|enum\|trait\|type\|const\|union)\b` or `^\s*impl\b.*\bfor\b`; **or** the line lies in the signature span of such an item (header through the opening `{`/`;`). Bodies are T3. |
| **T3** | **LOGIC** | Any other non-blank, non-comment line under `crates/*/src/**`, not test. |
| **T4** | **DOC** | `///`/`//!` lines **not attached to an item that also changed in T0âT2** (attached rustdoc is pulled into that item's row as a `doc +N` marker, never merged into the tier's code count); `website/**`; `*.md` outside `.claude/`. |
| **T5** | **TEST** | Test regions, `crates/*/tests/**`, `test/**`, `**/fixtures/**`. |
| **T6** | **SCAFFOLD** | `.claude/**`, `.agents/**`, `taskfiles/**`, `.github/**`. |

Two cross-cutting **flags**, raised on a row, not tiers:

- **`ARCH`** â `deny_unknown_fields` *added* to a fleet-read config (root `Config`, `config/managed.rs`, `oci/index/wire.rs`). Direct violation of the rule written in `arch-principles.md` and in the doc comment at `config.rs:22-23`. One regex plus a three-path allowlist; the highest value-per-line in the tool.
- **`UNGATED`** â a T0 row in a file with no byte-fixture net. Only `p/<ns>/<pkg>.json` roots are covered by `tests/index_wire_conformance.rs`; `ocx.lock`, `ocx.toml`, `metadata.json` have none.

#### 3.2 Detection

Three mechanisms, total ~250 lines:

1. **Diff** â `git diff -U0 -M --no-color <base>...HEAD`. `-U0` gives every `Â±` line an exact number; `-M` folds renames. Base defaults to `git merge-base main HEAD`; `--base` overrides. Baseline discovery via `gh pr view` is **not** re-implemented â `swarm-review/classify.md` already owns that, and surface takes `--base` from it.
2. **Region map** â one pass over each changed file's HEAD content producing, per line: `(in_cfg_test, enclosing_item_kind, enclosing_item_name, derive_block, is_doc)`. Brace-depth scanner with `//`-comment and `"â¦"`/`r#"â¦"#` stripping. This is the load-bearing piece and the thing nothing in the repo does today: `classify.md` and `duplo:diff` are both whole-file granularity, and the user's entire signal (2816 test lines living **inside** production files) is only visible below file granularity. Depth desync â the file goes to **T!**, visibly, never to T3.
3. **Derive block** â the contiguous `#[...]` run above an item header. This is what makes T0 and T1 precise rather than path-guessy: `Serialize` in the block turns a plain field into a wire change; `Parser` turns it into a CLI change. Survey 2 confirms the CLI is 100% clap-derive with zero builder calls, so this key is *complete* for the CLI surface, not merely good.

**One-line accuracy upgrade, do it first:** append `*.rs diff=rust` to `.gitattributes` (it currently has LFS and merge drivers, no diff drivers). Git's builtin rust userdiff then names the enclosing item in every `@@` header; surface cross-checks it against the region map and routes disagreements to a `low-confidence` footer instead of guessing. It also makes every human `git diff` in this repo more readable, for free.

**Not used, and not in v2 either:** `cargo-public-api` (nightly, one rustdoc build per lib crate per side, and its output has no line numbers, so the reviewer reconciles a second artifact by hand â for a workspace whose only consumer is `ocx_cli` and whose gate is the compiler); `cargo-semver-checks` (documented open blind spot on parameter/return/field *type* changes, zero serde awareness, and it treats a `#[non_exhaustive]` wire enum identically to a `#[non_exhaustive]` error enum â precisely the discrimination this repo needs and precisely the one it cannot make); raw rustdoc JSON (nightly-only, versioned schema you'd own the churn for).

#### 3.3 Mockup â ocx PR #227

Totals are the measured ground truth (384 production lines / 29 files, 4069 added Rust). The T0âT3 split is what the classifier would produce.

```
SURFACE   main...HEAD   9241 lines / 84 files      base: 86020d1a
          production: 384 lines / 29 files (4.2%)   contract-moving (T0+T1): 112

BIGGEST MOVERS  (top production files, regardless of tier -- read these too)
  47 oci/index/local_index.rs    36 oci/client.rs        30 oci/manifest.rs
  29 announce/pipeline.rs        28 oci/index/wire_writer.rs   27 package/tag.rs

T! UNPARSED ....... none

T0 WIRE ........... 75 lines / 5 files          published bytes, unfixable after push
  oci/manifest.rs                    +30 -2  doc +9   validate_image_index    UNGATED
  oci/index/wire_writer.rs           +28 -4  doc +6   serialize_root          fixture-covered
       (tests/index_wire_conformance.rs, 3 fixtures touched in this PR)
  oci/index/wire.rs                   +8 -0  doc +12  struct RootTag +1 field, #[serde(default)]
  project/lock.rs                     +5 -0           deny_unknown_fields, lock_version pinned [3]
  package/metadata.rs                 +4 -0           enum Metadata +1 variant   UNGATED
  ! fixture net covers p/<ns>/<pkg>.json roots only. lock/toml/metadata have none.

T1 CLI & EXIT ..... 37 lines / 5 files          what callers type, parse, and branch on
  api/data/announce.rs        [json]  +12 -0   AnnounceReport +3 fields
       no envelope in this repo: this struct IS the --format json contract
  announce/error.rs           [exit]  +11 -0   2 variants, both #[error(transparent)]
       transparent erases the source hop -> classify() must delegate by hand (classify.rs:826)
  command/announce.rs         [flag]   +8 -0   #[arg(long = "dry-run")]
       ok  walked by app.rs::cli_help_text_is_ascii / _has_no_internal_references
       !!  no entry in website/src/docs/reference/command-line.md, and nothing in CI checks
  cli/exit_code.rs            [exit]   +4 -0   +1 variant, pinned-value test present ok
  cli/classify.rs             [exit]   +2 -0   try_downcast!(AnnounceError) registered ok

T2 API ............ 45 lines / 9 files          new types + changed signatures
  NEW  pub struct TagAnnotations        package/tag.rs:14        +14
  NEW  pub struct AnnouncePipeline      announce/pipeline.rs:22   +6
  CHG  pub async fn put_manifest(&self, r: &Reference, m: &Manifest)
                      -> Result<Digest>
    -> pub async fn put_manifest(&self, r: &Reference, m: &Manifest,
                      ann: Option<&TagAnnotations>) -> Result<Digest>
                                        oci/client.rs:412        +10
  (+4 files, 15 lines -- --all)

T3 LOGIC .......... 227 lines / 19 files        no contract signal; read anyway
  oci/index/local_index.rs   47 | oci/client.rs        26 | announce/pipeline.rs  23
  package/tag.rs             13 | oci/index/ocx_index.rs 13 | announce/forge.rs   11
  (+13 files, 94 lines -- --all)

NEW SYMBOL USAGE  (resolved against HEAD, not the diff)
  TagAnnotations            package/tag.rs:14
    11 refs / 5 files   7 production, 4 test
    cross-crate  ocx_cli  api/data/announce.rs:41, command/package_push.rs:71
    same-crate   ocx_lib  oci/client.rs:412,428  oci/index/wire_writer.rs:112
    -> reaches T0 wire AND T1 json. Widest blast radius in this PR. Read tag.rs first.
  AnnouncePipeline          announce/pipeline.rs:22
    3 refs / 2 files    1 production, 2 test
    same-crate           command/announce.rs:61
    -> narrow.

LEDGER  (counted, not ranked)
  T4 DOC        647   530 rustdoc / 117 website+md    (attached rustdoc shown above, not here)
  T5 TEST      4872   2816 rust inline / 1725 pytest / 331 rust fixture
  T6 SCAFFOLD  1574   .claude/
  blank+comment 339

  coverage cross-ref: T0 ok(+3 fixtures)  T1 ok  T2 ok  T3 MISSING in 3 files
    local_index.rs, announce/pipeline.rs, oci/client.rs changed production code
    with no test delta in this PR.
  this report skipped the ledger. it did not verify it.
    see it: git diff main...HEAD -- test/ .claude/
```

Three presentation rules that are not negotiable:

- **T3 is always enumerated with per-file counts, never collapsed.** Only T4âT6 â the three the user explicitly named as low-priority â may be reduced to a number.
- **BIGGEST MOVERS sits above every tier** *(grafted from LENS)*, so the highest-churn production file is on screen even when it tiers low.
- **The last two lines state what was not looked at, and how to look** *(grafted from review-focus)*.

#### 3.4 Bail-out

If production lines are **> 40% of the diff**, or **< 150 absolute**, surface prints `production ratio 62% -- just read the diff` and exits 0. *(Grafted from review-focus Â§7b.)* The tool earns its keep only on lopsided diffs; on balanced ones it is an ordered file list plus ceremony.

#### 3.5 Where it lives

```
.claude/scripts/surface.py            ~350 lines, python3 stdlib only
.claude/tests/test_surface.py         ~120 lines, tier-assignment fixtures
.claude/tests/fixtures/surface/       ~15 tiny diffs, one per rule
.claude/taskfile.yml                  + task `surface`
.gitattributes                        + `*.rs diff=rust`
```

Every one of these is an existing pattern: `sync-canonical-blocks.py` is the precedent for a stdlib script invoked from `.claude/taskfile.yml`; `.claude/tests/` already carries `pyproject.toml` + `uv.lock` + pytest; and `task claude:verify` â which runs them â is already a phase-1 dep of the top-level `task verify`. **Zero new taskfile, zero new harness, zero new dependency, zero new crate.**

```sh
task claude:surface                       # merge-base main HEAD
task claude:surface -- --base v0.9.0 --all
task claude:surface -- --json             # for swarm-review / codex-adversary
```

#### 3.6 "New struct + its usages"

For each `NEW pub struct|enum|trait|type` in T2: `git grep -nw '<Name>' -- 'crates/**/*.rs' 'test/**'`, drop the declaring line, bucket every hit through the **same region map** the tool already built. Buckets and their meaning:

- **cross-crate** â an `ocx_lib` type surfacing in `ocx_cli` is a new interface, not an implementation detail. Strongest architectural signal the tool computes.
- **same-crate / same-module** â internal wiring. Same-module only â print `consider pub(crate)`.
- **reaches T0/T1** â a new type whose refs land in a wire or CLI file is a contract change wearing a type's clothes. This is what promoted `TagAnnotations` to the top of the usage block above.
- **`TEST-ONLY`** â every non-defining ref is in T5. *(Grafted from LENS Â§8.)* On a PR where 69% of added Rust is test code, this is the single highest-value badge: production scaffolding that landed without its wiring commit.
- **`NO CALLERS`** â new `pub` item referenced nowhere outside its file. Dead on arrival.

`grep -w` on a CamelCase Rust identifier over-reports on collisions (`Tag`, `Client`, `Manifest` are live risks here) and never under-reports; every hit prints its source line, so a false positive is visible in the same glance that reports it. Over 40 hits it prints `ambiguous -- not resolved` rather than fabricating a call graph, and the agent in the session escalates to `mcp__serena__find_referencing_symbols` for that one name. Exact resolution already exists in the toolbox; surface must not re-own it.

```python
# ponytail: grep -w call graph. Exact for distinctive type names, over-reports on
# collisions, never under-reports. Escalate to serena LSP for ambiguous names;
# do not build an index in here.
```

## 4. PHASED PLAN

**v0 â one weekend, deterministic, useful alone.**
`.gitattributes` one-liner; `surface.py` with diff parse, region map, T!/T0âT6 tiers, ARCH + UNGATED flags, BIGGEST MOVERS strip, ledger with the "did not verify" line, bail-out; the `claude:surface` task; `test_surface.py` with one fixture per rule â including *"field added to a `#[derive(Serialize)]` struct in `project/lock.rs` is T0"*, *"the same line inside `#[cfg(test)] mod tests` is T5"*, *"`deny_unknown_fields` added to root `Config` raises ARCH"*, and *"unterminated raw string lands the file in T!"*. Ship without the usage block if the weekend runs out. **v0 alone is the whole answer to the user's stated problem** â run it, read top-down. Everything after is amplification.

**v1 â half a day, after v0 has been run on three real PRs.**
New-symbol usage block with cross-crate / TEST-ONLY / NO-CALLERS badges. `--json`. Then three one-line consumer edits: `swarm-review/classify.md` drives its tier table on `production_lines` instead of `lines_changed` (today PR #227 classifies `max` on 9241 lines and spends an adversarial panel on 384 lines of real content) and adds a T0-hit structural marker; `codex-adversary` takes the T0âT2 file:line set as its scope â `terra` on 112 focused lines beats `sol` on an undifferentiated dump; `.claude/rules.md` gains a "Reviewing a large diff" row in *By concern* (its structural tests fail otherwise). One producer, three consumers, no second pipeline â per `feedback_extend_dont_duplicate.md`.

**v2 â only on evidence, one item at a time.**
Trigger for each: *a real miss, observed.* If T2 demonstrably misses a signature change â `cargo public-api diff <base>..HEAD -p ocx_lib` behind `--exact-api` as a row *annotator*, never the tiering source. If a flag ships undocumented again â the `command-line.md` â clap-tree sync gate, which is its own piece of work and a missing *test*, not a missing feature of this tool. If someone asks for CI â `task claude:surface -- --json` uploaded as a PR artifact, report-only. Not planned: a skill wrapper, an LLM narrative, HTML.

## 5. THE HONEST RISK

**The tool ranks by declaration shape, not by blast radius, and the two come apart exactly where it hurts most.** PR #227's largest single production file is `oci/index/local_index.rs` at 47 lines â 12% of all production logic in the PR. It has no serde attribute, no clap derive, probably no changed `pub` signature. It lands in **T3, at the bottom**, while a 5-line no-op in `project/lock.rs` sits at the top of T0. If those 47 lines corrupt the local index cache, surface actively steered the reviewer away from the bug, and did it with the authority of a tool that just printed a confident-looking taxonomy. A reviewer who reads T0âT2, sees "112 contract-moving lines", and merges is *worse off than with an undifferentiated diff*, because the flat diff at least made no claim about what mattered.

This is not a regex bug. It is the ceiling of any syntactic classifier, and no amount of tuning raises it.

**Mitigation, and it is presentation-only because there is no detection answer:** the **BIGGEST MOVERS strip prints the top-6 production files by line count above every tier, unconditionally**, so the highest-churn internal file is always the second thing on screen; **T3 is always enumerated by file with counts and is never collapsible**; the **coverage cross-ref names T3 files that changed production code with no test delta**, which is precisely how `local_index.rs` gets flagged in the mockup above; and the tool's contract, written in `SKILL`-adjacent prose and in the script's header, is *"here is a reading order"*, never *"here is what you can skip"*. **The single line in this design that must never move: if anyone ever makes T3 collapsible or replaces it with a count, the tool has become dangerous and should be deleted.** Add that as a comment above the renderer.

## 6. WHAT NOT TO BUILD

- **The HTML dashboard, and the localStorage "unreviewed" counter with it** (P3). It is a renderer for data that reads fine as text, it is stale the moment you commit, it needs a browser in a workflow that lives in a terminal and an agent session, and its machine-readable half (the JSON) was the actual product. The counter is worse than decoration â it is a completeness theatre that decrements on *expanding a section*, which measures scrolling, not reviewing.
- **The LLM narrative stage as the product** (P2). The pre-filter is 100% of the value and 0% of the cost; the narrative is where "this breaks older clients" gets fabricated, and the `file:line` anchor rule validates locations, not consequences. The agent reading `--json` in-session already produces the narrative for free, in context, with the diff open. Do not pay an Opus turn for it.
- **`cargo-public-api`, `cargo-semver-checks`, raw rustdoc JSON â in any phase.** Nightly-pinned, one rustdoc build per lib crate per side, and semver-checks is blind to exactly this repo's questions (parameter/field type changes, serde representation, wire-enum vs error-enum). Revisit the day `ocx_lib` has an external consumer; today the compiler is that gate.
- **A `--fail-on-arch` CI gate, or any CI gate.** A gate teaches "green means reviewed". Print the `ARCH` flag; let a human act on it.
- **An `xtask` or Rust dev-tools crate.** No `[[bin]]` precedent exists beyond `ocx_cli` and `ocx_shim`; inventing one spends an innovation token on what a 350-line stdlib script does, next to two scripts already doing exactly this.
- **`serde-reflection` wire snapshots.** Right problem, wrong shape â a library needing a golden test on both sides of a diff. If wire drift ships a break, that's an ADR and a fixture, not a feature of a triage tool.
- **Health scores, grades, trend history, severity weighting.** A tier and a line count. Anything else is a number people optimize instead of reading the diff.
- **Baseline-resolution logic (`gh pr view`, PR-number handling).** `swarm-review/classify.md` and `codex-adversary` already each own a copy. A third is how you get drift. Take `--base` and nothing else.