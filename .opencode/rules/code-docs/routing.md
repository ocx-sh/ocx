---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.code-docs-depth-on-demand"
title: Routing, Clause by Clause
summary: The RTE family, owning which store holds each clause a comment carries, when a test may carry a guard, and where essays, provenance, subsystem invariants and references to other code go
---

# Routing, Clause by Clause

Owns where each clause of a comment lives: what stays in source, what moves to a
test's doc comment, a path-scoped rule or a decision record, and what is deleted.
Also owns when a test carries a guard well enough for the comment to shrink, and
how a comment names other code. The shape of a guard that stays, and the proof
that a lint or a type owns it, are `GRD`. Pointer grammar and process-ID bans are
`LNK`. Length caps are `LEN`. Text that renders into help, schemas or tool lists
is `SRF`. Shortening existing comments in bulk is the code-docs-cleanup skill.

Contents: [Dates and Defaults](#dates-and-defaults) · [Where It Goes](#where-it-goes) ·
[Worked Split](#worked-split) · [Routing a Block, Keeping It Tracked](#routing-a-block-keeping-it-tracked) ·
[Tests as Guard Carriers](#tests-as-guard-carriers) · [Naming Other Code](#naming-other-code) ·
[Records, Rules and Provenance](#records-rules-and-provenance) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

## Dates and Defaults

- Measured 2026-09-27. Tool behaviour (Claude Code rule loading, cargo-mutants,
  Stryker, mutmut 3.8.0) is dated to that day.
- `tests` stands for the repository's test roots and `origin/main` for its base
  branch; name every test root, such as `crates` in a workspace. Every grep is a
  locator: empty output is the pass unless the row says otherwise, and a
  pathspec that matches no file prints nothing without passing anything.
- **Defaults an adopter may override:** the `timeout 1800` bound on a proof run
  and the Claude Code hook in RTE-08. A client with no path-scoped instructions
  skips RTE-07 and RTE-08.

## Where It Goes

Every destination is in the tracked tree (RTE-02). Test each clause top to bottom
and stop at the first row that fits.

| Clause | Destination | Routing test | Rule |
|---|---|---|---|
| Guard on a specific line | Plain comment at that line, constraint plus consequence; a doc comment only for a caller-visible precondition. When a test owns it, one line naming each test the planted edit turned red | Would a plausible edit here break something no type, lint or test forbids? | GRD-01, GRD-04, RTE-03 |
| Guard whose breaking edit lands in a data file that cannot hold comments (JSON, a data fixture) | The type, field or schema that constrains the data, as constraint plus consequence; else the doc comment of the test that asserts it | Does the edit that breaks it land in a file with no comment syntax? | GRD-01, RTE-04 |
| Guard on a universal negative or an absence | The doc comment of the test that asserts it | Is there no single line that implements the property? | RTE-04 |
| Guard a lint, type or checker owns | One line naming the lint or type, or nothing when the construct states itself | Does that owner fail on the planted breaking edit? | GRD-08, GRD-09, GRD-12 |
| Why-constraint | One sentence at the site | Local reason, with no single named breaking edit? | RTE-01 |
| Subsystem-wide invariant | The path-scoped rule covering the subsystem; per-file copies deleted | Is the sentence true verbatim at every file the rule's glob matches? | RTE-07 |
| Contract | Doc comment on the item | Does a caller outside this file need it to use the item correctly? | GRD-04 |
| Maintainer rationale inside rendered text (help, schema, tool description) | Moved verbatim to a plain comment at the line, in the same edit | Would a user of this flag or field need the sentence? | SRF-01, SRF-02 |
| A module's reason to exist | The module doc, once | Does an item doc repeat the module's reason, pointer or ID? | SRF-05 |
| Essay argument: alternatives weighed, a trade-off, the design story | Decision record, plus one file-qualified pointer in code | Does it argue for a decision someone could re-open, and does Henderson's filter keep it? | RTE-10 |
| Record paraphrase | Deleted; the pointer stays | Does it restate a tracked record? | RTE-09 |
| Pointer to a record | `path.md#anchor` of a tracked file | Does it name a document instead of stating the reason? | LNK-02, LNK-03 |
| Bare plan or process ID | Deleted, or the constraint restated in the present tense | Does the ID fail to name a file? | LNK-01, LNK-05 |
| Reference to other code | The symbol name | Is the target a function, test or type? | RTE-05 |
| External reference: an RFC or spec section, an issue URL | Kept while it resolves | Does it name a published document outside the repo? | LNK-12 checks liveness |
| Provenance that justifies a live value | At most 2 lines beside the value, carrying a count or a named artifact | Would removing it let someone "fix" the number back? | RTE-11 |
| Provenance that justifies nothing live | Deleted from code; the finalized commit body at most. If it still constrains an edit, it is a guard: rewrite it in the present tense | Does it bear on any plausible future edit? | RTE-02, RTE-11 |
| Deferred shortcut | A marker naming its ceiling and upgrade path, such as a `ponytail:` line | Does it name a deliberate, deferred shortcut? | GRD-06 keeps it |
| Narration, tautology, banner, phase marker | Deleted, unless a clause names what the step guards; then it is a guard | Does the next line or the name already say it? | the cleanup skill's CLN-01 |

## Worked Split

A 12-line comment above a web-audit CI config, routed clause by clause:

```js
// Lighthouse CI gate.
// We assert per-category floors instead of extending the `lighthouse:no-pwa`
// preset. The preset was tried first (see WP-2): it fails the build on
// `color-contrast`, `link-name` and `link-in-text-block` for third-party
// embeds we cannot fix, and 40 hand-pinned overrides cost more to keep current.
// Floors: accessibility 0.97, best-practices 0.93, seo 0.97, performance 0.85.
// Each floor sits 0.03 under the score measured over 10 runs, so run-to-run
// variance never fails the build. A planted a11y regression dropped 0.92 to 0.77.
// Audits the built `dist/`, never the dev server: its unminified bundles score
// about 0.2 lower on performance, and a gate tuned there passes real regressions.
// Now set up the assertions.
```

| # | Clause | Row | Goes to |
|---|---|---|---|
| 1 | "Lighthouse CI gate." | Narration | Deleted: the file name says it |
| 2 | Floors over the preset, the three failing audits, the override cost | Essay argument | Decision record; Henderson's filter keeps it, because the gate governs every page and can be re-opened. The record keeps the three audit names, which make the choice falsifiable |
| 3 | "(see WP-2)" | Bare process ID | Deleted |
| 4 | The four floor values | Narration | Deleted: the `assertions` object holds them, and a second copy drifts |
| 5 | "0.03 under the 10-run score, so variance never fails the build" | Provenance for a live value | Stays, one line beside the floors |
| 6 | "A planted regression dropped 0.92 to 0.77" | Essay argument: the evidence behind clause 2 | Record, with clause 2: it is the gate's red proof |
| 7 | "Audits the built `dist/` … passes real regressions" | Guard on a specific line | Stays at `staticDistDir`, as fact plus consequence |
| 8 | "Now set up the assertions." | Narration | Deleted |

```js
module.exports = { ci: {
  // Dev-server bundles are unminified and score ~0.2 lower on performance; a gate tuned there passes real regressions.
  collect: { staticDistDir: "dist" },
  // Category floors, not the preset: docs/adr/0007-audit-gate.md#category-floors
  // Each floor sits 0.03 under the 10-run measured score, so run-to-run variance never fails the build.
  assert: { assertions: { "categories:accessibility": ["error", { minScore: 0.97 }] /* … */ } },
} };
```

The record receives clauses 2 and 6, appended if it already exists. Cutting the
block by length alone loses clause 7, the only one a cold editor needs.

## Routing a Block, Keeping It Tracked

```bash
python3 checks/comment_census.py --root . --list-blocks --min-block 4 --kind any      # 1
git diff -U0 "$(git merge-base origin/main HEAD)" -- . ':(exclude)*.md' | grep -e '^-[[:space:]]*//' -e '^-[[:space:]]*#' -e '^-[[:space:]]*\*' | grep -i -e 'instead of' -e 'rather than' -e 'must not' -e 'never' -e 'otherwise'   # 2
python3 checks/cleanup_check.py --base origin/main                                     # 3 comment-only diffs
git grep -n -E -e '//.*\bcommit [0-9a-f]{7,40}\b' -e '#.*\bcommit [0-9a-f]{7,40}\b' -e '\*.*\bcommit [0-9a-f]{7,40}\b' -- '*.rs' '*.py' '*.ts' '*.go' '*.java' '*.kt'   # 4
git ls-files -z -- '*.rs' '*.py' '*.ts' '*.go' '*.java' '*.kt' | xargs -r -0 rg -n -P -e '(?://|#).*\b(?=[0-9a-f]{0,39}[0-9])(?=[0-9a-f]{0,39}[a-f])[0-9a-f]{7,40}\b'   # 4b, triage
python3 checks/linkage_check.py pointers --root .                                      # 5
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| RTE-01 | Route every comment block longer than 3 lines clause by clause. Guard, why-constraint and contract clauses stay at the site, every other clause goes to its row of the table, and a clause that fits no row stays. | Routing a whole block by its length or its first sentence deletes the guards inside it. An 18-line block that read as an essay held the only statement of a UI regression ("blinked out on the second paint"), and 3 of 10 long blocks in a dry run bundled guards the same way. Human plain-comment blocks run 3 lines or fewer at the 90th percentile. | Command 1 lists candidates. Command 2 prints every removed comment line that named an alternative or a consequence: find its surviving sentence among the added lines, or restore it. On a comment-only diff, command 3 exits 0. Reading test: every clause that named a breaking edit and its consequence before the edit is still there after, or its owner is proven (RTE-03, GRD-07). | MUST |
| RTE-02 | Keep every reason a future edit depends on in the tracked tree. Never make a commit body, pull-request text, branch, backup ref or gitignored file its only store, and never point a comment at a commit SHA, branch, backup ref or gitignored path. | A cold agent reads the tree, and history rewrites replace the wording it needs. A finalize step that recomposes messages from the diff turned one 25-commit branch into 10 and dropped its review-round wording, squash merges discard series bodies, and backup refs get deleted. Comments citing plans in a gitignored state directory point at an empty directory. The finalized commit body stays a useful second copy. In the reason-recovery eval, 3 of 113 sessions whose guard comment was deleted opened git history at all. | Command 4 prints nothing. Command 4b lists bare hex runs for reading: a digest or a UUID part is no finding, a commit SHA is. Command 5 reports an untracked or gitignored record as `no tracked file`. A comment naming a branch or a backup ref is the same finding, read by eye. | MUST |

## Tests as Guard Carriers

The planted-edit proof, GRD-07 applied to tests. Apply one breaking edit the
guard names in a scratch copy, run the suite to find every test that goes red,
named by the guard or not, then run each of those alone:

```bash
git worktree add --detach ../guard-proof HEAD && cd ../guard-proof     # 6 apply ONE breaking edit here
NAME=a_file_declared_ttl_cannot_outlive_the_built_in_ceiling
timeout 1800 cargo test -p example_crate "$NAME"                        # 7 Rust: "test result: FAILED"
timeout 1800 uv run pytest tests -k "$NAME"                             #   Python (add the test extra): "FAILED"
timeout 1800 npx vitest run test/example.test.ts -t "$NAME"             #   TypeScript: "FAIL"
cd - && git worktree remove --force ../guard-proof
git grep -n -e "$NAME" -- .                                             # 8 in the real tree: at least 2 hits
git grep -n -e 'id="case' -e 'id="test' -- tests                        # 9
git grep -n -e 'id="' -e 'it("' -e 'test("' -- tests                    # 10
```

Read the run through these traps:

- **Apply the edit the guard names, not a substitute.** A test can fail on a
  neighbouring edit and never see the one the guard argues against.
- **A failed build is not a failed test.** An edit that orphans an import or a binding
  fails `-D warnings` first. Delete the leftover and re-run; count a red only on the
  test's own failure line.
- **A hang is red only when the guard's consequence is the hang.** Exit 124 from
  `timeout` otherwise means unproven. Without a per-test timeout, CI hangs instead of
  naming the test.
- **Run each named test alone.** At several guards only one test of a named pair
  catches the edit; name that one.
- **A test compiled out on this host proves nothing.** Run a platform-gated guard's
  proof on its platform. A test that stays green leaves the comment whole.

The cleanup skill never edits tests, so moving a reason onto a test's doc comment
(RTE-04) or renaming a test (RTE-12) is its own change.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| RTE-03 | Shrink a guard on a specific line because of a test only after applying the guard's breaking edit in a scratch copy and seeing that test fail. The line that stays names every test that failed and no test that stayed green, and one local line stays even when the test owns the guard. | A test named for the constraint is not proof. Across 22 guards whose edit was applied, the named test stayed green at 3, and at 4 more only one of two named tests failed. Reading the test bodies called two of those 4 fully covered. A shared noun proves less: a test whose doc says "symlink" never plants the symlinked parent directory its guard defends against. The local line carries the reason: across 24 test-owned guards, deleting the comment and keeping only the test cut reason recovery by 10 points and full recovery by 20. | Commands 6 and 7, read through the traps above. Command 8 hits the test and the local line. | MUST |
| RTE-04 | Put the reason for a universal negative or an emergent absence (nothing reserved, never refused, never swept in) on the doc comment of the test that asserts it, as a fact plus its consequence. | No line implements such a property, so production code would grow an unbounded "and not X" list, or a cleanup deletes the test doc as chatter. The test is the only checkable form of the guarantee, and 7 of 15 sampled guards in one agent-written codebase already sit there. | Reading test: for a guard with no implementing line, a test exists whose doc comment states the constraint and names the plausible breaking edit, for example "fails the moment someone helpfully reserves the name". | SHOULD |
| RTE-06 | Use a mutation run only as supporting evidence for a guard shrink, never as its proof. `unviable`, `CompileError`, `NoCoverage` or no mutant on the guarded span proves nothing, and a surviving mutant beside the guard is a test gap to report. | Mutation never decided a guard the planted edit did not also decide. cargo-mutants generated no mutant for a bare `.min()` or `.saturating_add()` call and called a guard collapse unviable under `-D warnings`, yet the hand-applied edits turned all three tests red. What it adds is a gap beside a guard, such as a surviving `<` to `<=`. | Reading test on the report for the guarded line, against the planted-edit result. Tool notes below. | CONSIDER |
| RTE-12 | Name a test that carries a guard after the constraint: in its name or, when one function covers several scenarios, in each parametrize id or `it()` string. A guard-to-test pairing check reads those ids and strings too. | In a parametrized test the constraint noun often appears only in the id. `id="tar-slip-parent"` sits on `test_archive_refuses_anything_but_one_regular_member`, and deleting the traversal check turns exactly that id red. | Command 9 prints nothing. Command 10 lists the strings a pairing scan reads beside `def` and `fn` names. | CONSIDER |

Mutation tool notes, 2026-09-27:

- **TypeScript:** install Stryker into the project (`npm install --no-save
  @stryker-mutator/core @stryker-mutator/vitest-runner`); `npx --yes` fails with
  `ERR_MODULE_NOT_FOUND: typescript`. Set `mutate` to the one source file and point
  `vitest.configFile` at a config whose `include` names only the target test, since
  the dry run needs every configured test green.
- **Rust:** run cargo-mutants with `-f` on the one file and `--in-place` in a clean
  worktree. It has no operator for a bare method call, and 34 to 65% of its mutants
  per file were unviable.
- **Python:** mutmut 3.8.0 fails inside its sandbox on `src/`-layout packages. Use the
  planted edit.

## Naming Other Code

```bash
NAME=a_file_declared_ttl_cannot_outlive_the_built_in_ceiling
git grep -n -e "$NAME" -- .                                                                  # 11
git grep -n -E -e '//.*[A-Za-z_]\.rs:[0-9]' -e '#.*[A-Za-z_]\.py:[0-9]' -e '//.*[A-Za-z_]\.ts:[0-9]' -- '*.rs' '*.py' '*.ts'   # 12
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| RTE-05 | When a comment refers to other code (the test that proves a guard, a sibling function with the opposite posture), name the symbol, never a plan ID or a `file:line`. | Plan IDs collide: 0 of 30 sampled resolved to one document, and one sat in 21 files. 45% of sampled path citations in records died after one package split. A symbol search still finds a renamed function. | Command 11 hits the test and every comment that relies on it. Each line command 12 prints is a finding. Plan-ID families are LNK-01. | SHOULD |

## Records, Rules and Provenance

```bash
PHRASE=example; git grep -l -F -e "$PHRASE" -- '*.rs' '*.py' '*.ts' '*.go' '*.java' '*.kt'                                  # 13
git grep -l -e '^paths:' -- .claude/rules                                                                                     # 14
git grep -n -E -e '"[A-Za-z0-9_-]+\.md"' -- .claude/hooks                                                                     # 15
git grep -n -e 'adr_[a-z0-9_]*\.md' -e '/adr/' -e '/decisions/' -- '*.rs' '*.py' '*.ts' '*.go' '*.java' '*.kt'               # 16
git grep -n -i -e 'we considered' -e 'alternative' -e 'we chose' -e 'trade-off' -e 'tradeoff' -- '*.rs' '*.py' '*.ts' '*.go' '*.java' '*.kt'   # 17
git grep -n -i -e 'used to' -e 'previously' -e 'historically' -- '*.rs' '*.py' '*.ts' '*.go' '*.java' '*.kt'                 # 18
```

The RTE-08 reminder, as Claude Code behaves on 2026-09-27: path-scoped rules load
when the agent reads a matching file, not on every tool use, and after compaction
only once a matching file is read again. The reminder is a PostToolUse hook on
Edit and Write that reads every rule's `paths:` list, matches the touched file, and
returns the matching rule names in `hookSpecificOutput.additionalContext` with
`hookEventName` set to `PostToolUse`. Match with a glob engine that honours `**`,
such as Python 3.13's `PurePath.full_match`: `fnmatch` needs a `/` before `*.rs` in
`**/*.rs` and misses files at the root.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| RTE-07 | Move an invariant that is true verbatim at every file a path-scoped rule's glob matches into that rule, and delete the per-file copies. | Every per-file copy of a subsystem rule is one more copy that drifts, and agents consult instruction files for 60.5% of their documentation lookups against 10.6% for classical docs ([arXiv 2608.20195](https://arxiv.org/abs/2608.20195)). | Reading test: is the sentence true at every matched file? Command 13: a key phrase in 2 or more files under one rule's glob is a finding. | SHOULD |
| RTE-08 | Build the post-edit reminder that names the rules covering a touched path from the rules' own `paths:` frontmatter, never from a hand-written table. | A Write-only edit or a compaction skips a path-scoped rule. Both hand-written reminder tables measured had drifted: one omitted 6 subsystem rules, the other omitted one and narrowed its globs. | Command 14 lists the path-scoped rules. Each line command 15 prints is a rule named by hand. Planted proof: add a rule with a `paths:` glob in a scratch copy, pipe a PostToolUse payload naming a matching file into the hook, and see the new rule named without editing the hook. | SHOULD |
| RTE-09 | Once a comment carries a file-qualified pointer to a record, delete the clauses that restate that record, and keep only the clauses the record does not hold. | Agents add the pointer and keep the essay: 2 of 10 spot checks, including a 22-line module doc that cites three decisions of one record and restates them. The clauses the record lacks stay, because essays hold local constraints no record states. | Command 16 lists comments that name a record. Reading test: a block that names a record and restates 2 or more of its sentences is a finding. On a comment-only diff, command 3 fails a pointer that coexists with the paraphrase it replaced. | SHOULD |
| RTE-10 | Move an argument that weighs alternatives into a decision record unless Henderson's filter says skip ("limited in scope and time and risk and cost, or already covered elsewhere"), and keep in the record the specifics that make the choice falsifiable. | An argument in code gets re-litigated by every reader or silently lost in a cut. A 51-line config comment weighing a preset against category floors sat in a repo with one record, and its rejected audit names are what make the choice checkable later. How few records a repo holds is not a skip condition. | Reading test, Henderson's filter per essay ([joelparkerhenderson/architecture-decision-record](https://github.com/joelparkerhenderson/architecture-decision-record)). Command 17 lists candidates. | SHOULD |
| RTE-11 | Keep provenance only when it justifies a value still in the code, as at most 2 lines carrying a count or a named artifact. Rewrite history that still constrains an edit as a present-tense guard, and delete the rest. | Provenance blocks carried guard content in 1 of 43 sampled, and the kept exception is quantified ("12 of 18 were ordinary technical nouns"). A history-phrase regex has 37% false positives and hits live guards another 37% of the time, so phrasing never licenses a delete. | Reading test: does the sentence carry a number, a named test or a tracked artifact? Command 18 locates candidates for reading and is never a delete list. | SHOULD |

## What Agents Get Wrong Here

Ranked by how often each was measured to bite.

1. **Writing the plan's own ID as the pointer.** The agent reuses the vocabulary in
   its context; one codebase carries such IDs on 1,872 production comment lines
   (RTE-02, RTE-05, LNK-01).
2. **Crediting a named test without running the breaking edit.** Wrong or incomplete
   at 7 of 22 guards, every one sharing nouns with its test (RTE-03).
3. **Routing a long block as one unit.** Length marks it an essay and the guard
   clause goes with the cut: 3 of 10 long blocks (RTE-01).
4. **Adding the pointer and keeping the paraphrase:** 2 of 10 spot checks (RTE-09).
5. **Misreading a proof run that never reached a test result:** a build failure on an
   orphaned import read as red, or a hang, at 3 of 22 guards (RTE-03's traps).
6. **Reading mutation output as safety:** a green run below a guard, or a high
   `unviable` count, taken as protection (RTE-06).
7. **Deleting history by its phrasing.** "Used to" and "previously" open live guards
   as often as dead ones (RTE-11).
8. **Editing without the governing rule loaded,** after a Write-only edit or a
   compaction (RTE-08).
9. **Pointing at a commit SHA** instead of stating the reason (RTE-02).
