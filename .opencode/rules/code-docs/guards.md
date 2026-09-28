---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.code-docs-depth-on-demand"
title: Guards
summary: The GRD family, owning what a comment that protects a line must say, where it sits, when a test, lint or type may own it instead, and what a rewrite must carry through
---

# Guards

A guard is a comment whose removal lets a plausible edit break something no
type, lint or test catches. It is the one comment a cold agent cannot rebuild
from the code, and the one agents write worst: six lines where a human writes
two, in the doc register, several guards bundled into one essay, or trimmed to
the mechanism with the consequence gone. This file owns what a guard says,
where it sits, when an owner may replace its prose, and what a rewrite keeps.
Where a non-guard clause goes is `RTE`, block caps and the ratchet are `LEN`,
and the pointer grammar is `LNK`.

Contents: [Dates and Defaults](#dates-and-defaults) · [The Guard Shape](#the-guard-shape) ·
[One Site, the Right Register](#one-site-the-right-register) ·
[The Carve-Out and the Rewrite Diff](#the-carve-out-and-the-rewrite-diff) ·
[Owners, Proven by a Planted Edit](#owners-proven-by-a-planted-edit) ·
[Exhaustive Matches and RAII Bindings](#exhaustive-matches-and-raii-bindings) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

## Dates and Defaults

Measured 2026-09-27 against rustc and clippy 1.93.1, pyright 1.1.414 and
TypeScript 7.0.2. The lint and type behaviour in the exhaustiveness and RAII
rows was watched on planted edits at those versions. `checks/` is this rule's
own directory; run each script from the repository root. Every grep here is
triage: read each hit, and treat none as a finding for matching alone.

Defaults an adopter may override once, with the reason recorded: the 3-line
guard signal (GRD-02), the recogniser's rule set and pinned counts (GRD-06), and
the Rust RAII lint policy (GRD-10). Every other row is portable as written.

## The Guard Shape

A guard is the constraint plus the concrete consequence of breaking it, stated
as a fact, one sentence per breaking edit, in the register its reader opens.
It names the tempting edit only when the constraint does not already imply it.

```rust
// wrong: a doc essay with a plan ID and an order, and no consequence
/// Acquires the render lock (WP-12). The lock is load-bearing and held until
/// this returns. NEVER rename this binding. A per-file lock was rejected.
fn render(&self) -> Result<()> {
    let _render_lock = self.lock.acquire()?;

// right: the caller's contract in the doc, the hazard at its own line
/// Renders the toolchain, blocking while another process renders it.
fn render(&self) -> Result<()> {
    // Renamed to `_`, the lock drops at once and two renders interleave.
    let _render_lock = self.lock.acquire()?;
```

```sh
# GRD-02: guard-shaped blocks over 3 lines. The recogniser misses over half of all guards, so read the second list too.
python3 checks/guard_recogniser.py --root . | awk '$4 ~ /^x([4-9]|[1-9][0-9]+)$/'
python3 checks/comment_census.py --root . --list-blocks --min-block 4
# GRD-05: guards phrased as orders. A hit is a finding only when its sentence names no consequence.
grep -rnE --include='*.rs' --include='*.py' --include='*.ts' --include='*.tsx' --include='*.js' --include='*.go' --include='*.java' --include='*.kt' --exclude-dir=target --exclude-dir=node_modules --exclude-dir=.venv --exclude-dir=vendor -e '^\s*[/#*]+ *Never ' -e '^\s*[/#*]+ *Do not ' -e "^\s*[/#*]+ *Don't " -e '^\s*[/#*]+ *Keep ' -e '^\s*[/#*]+ *Make sure ' -e '[/#*] .*NEVER ' .
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| GRD-01 | Write every guard as the constraint plus the concrete consequence of breaking it, and let no edit remove either half. This covers every comment another rule mandates: an `unsafe` block's `SAFETY:` comment, a discarded result's rationale, a lint suppression's reason, and any rule that requires "a comment naming X". | Without the consequence a cold reader cannot tell a guard from narration, so it deletes the guard or invents a reason. All 39 human-written guards sampled from pre-2022 open-source repos name a concrete failure. In a blind reason-recovery eval over 25 guards from five real codebases (sonnet reader, opus judge), deleting the guard text cut reason recovery by 31 points on guards no test owns, and a bare record pointer in its place by 12; the constraint-plus-consequence rewrite lost nothing. | Consequence test, per guard the diff touches: point to the clause that names what breaks (a hang, a leak, a torn write, a wrong exit code, an escaped path). "Important", "careful" or "load-bearing" alone is no consequence. After a guard is shortened, a fresh read-only session asked the unled question "Why is it written this way? Could it be written differently without changing anything that matters?" must name the consequence; the `code-docs-cleanup` skill runs that re-check. The recogniser is never this check (GRD-06). | MUST |
| GRD-02 | Keep a guard to the constraint, the consequence and, only when the constraint does not imply it, the tempting edit. For a deliberate divergence, the tempting edit is the sibling the code must not be unified with, named as a symbol. Route history, weighed alternatives, restated context and bare plan IDs out of the comment. | Agent-written guards run a median 6 lines, and a third run past 10. Human guards are 2 lines or fewer 85% of the time and never past 9. Rewriting 20 agent guards to this shape cut 394 lines to 114 and lost no breaking edit. A named sibling lets a cold reader diff two functions instead of trusting prose. In the same eval, rewriting those 25 guards to this shape cut 443 lines to 209 with no loss in reason recovery and 8 points more full recoveries. | A guard over 3 lines signals a bundle (GRD-03) or an essay (RTE-01). The first two commands above list the candidates. | MUST |
| GRD-05 | Phrase a guard as a fact and its consequence: "X holds, or Y happens". Never phrase it as an order to the reader ("NEVER …", "Keep …", "do not assume …"). | An order carries no consequence, so the next agent cannot re-judge it when the code changes. Imperative text in a comment is also the shape an indirect prompt injection takes, and the published mitigation strips it ([arXiv 2603.21642](https://arxiv.org/abs/2603.21642)). | The phrasing grep above. Rewrite a hit only when its sentence carries no consequence: "Never called: rules stop at the kind gate" is a fact and passes. | SHOULD |

## One Site, the Right Register

```sh
# GRD-03: verbatim comment lines repeated across the tree, most frequent first
grep -rh --include='*.rs' --include='*.py' --include='*.ts' --include='*.tsx' --include='*.go' --include='*.java' --include='*.kt' --exclude-dir=target --exclude-dir=node_modules --exclude-dir=.venv --exclude-dir=vendor -e '^\s*// [A-Z]' -e '^\s*/// [A-Z]' -e '^\s*# [A-Z]' . | sed -e 's/^\s*//' | awk 'length($0) > 50' | sort | uniq -cd | sort -rn
# GRD-04: hazard wording added to a doc register on this branch (Python docstrings need the reading question)
git diff -U0 "$(git merge-base origin/main HEAD)" -- '*.rs' '*.ts' '*.tsx' '*.js' '*.java' '*.kt' | grep -e '^+\s*///' -e '^+\s*//!' -e '^+\s*\*' | grep -i -e 'must not' -e 'load-bearing' -e 'rather than' -e 'instead of' -e 'otherwise'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| GRD-03 | Split a block that guards more than one breaking edit into one sentence per edit, each at that edit's own line. State each guard once: at the type or at the binding, never both. A constraint repeated across implementations moves to one owner (a shared default, a table test) or to one comment. | A bundle hides its second and third guard from a reader and from the length cap (`LEN-05` applies the same split to an over-cap block). Copies drift: for example, one codebase repeats the same "never called: skipped at the gate" line at 23 sites. Consolidating is safe: 5 of 5 consolidations found in 200 real commits kept the fact at its remaining home. | The repeat triage above, then read each hit. A consolidation moves text across hunks and files, so any check of it pairs removed and added blocks across the whole commit (`difflib.SequenceMatcher` ratio, with `git diff --color-moved=blocks --diff-algorithm=histogram` as a pre-filter), never per hunk. | SHOULD |
| GRD-04 | Put a guard in the doc register only when a caller who never opens the body needs it: a precondition, an error condition, a sentinel value. Put an implementation hazard in a plain comment on the line it guards. | Doc text renders to hover, generated API docs and, through derive-based CLI and schema generators, user-visible output. Agent-written guards sit in the doc register 53% of the time, human ones 18%. This generalises the split between an `unsafe fn`'s `# Safety` section and a block's `// SAFETY:` comment. The eval's rewrite applied this split with no recovery loss; no arm isolated it. | Ask of each doc block the diff adds or edits: "does someone who only calls this need it?" A pure implementation hazard in the doc register is a finding. The diff triage above lists candidates. | SHOULD |

## The Carve-Out and the Rewrite Diff

```sh
python3 checks/guard_recogniser.py --self-test          # pins this code's own counts on its fixture
python3 checks/guard_recogniser.py --root .             # the carve-out: every flagged prod block
python3 checks/cleanup_check.py --base origin/main      # exit 1: a cleanup removed a flagged or labelled block with no replacement
# GRD-11: file-qualified pointers the branch removed and never re-added; any output is a finding
MB=$(git merge-base origin/main HEAD); comm -23 <(git diff -U0 "$MB" -- . ':(exclude)*.md' | grep -e '^-' | grep -v -e '^---' | grep -oE -e '[A-Za-z0-9_./-]+\.md' | sort -u) <(git diff -U0 "$MB" -- . ':(exclude)*.md' | grep -e '^+' | grep -v -e '^+++' | grep -oE -e '[A-Za-z0-9_./-]+\.md' | sort -u)
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| GRD-06 | Never auto-cut a block `guard_recogniser.py` flags, a `SAFETY:` label, the repo's deferred-shortcut marker (for example `ponytail:`), or a comment another rule mandates. A recogniser miss means unclassified, never safe to cut. The recogniser never passes or fails an edit or a marker; the most it does with a human or agent diff is put a recogniser-loss item on a review list (`LEN-06`). | Its recall is about 0.43 in every measurement, so as a gate it would approve cutting 57% of guards. As a post-edit check it failed 6 of 20 loss-free rewrites, and all 12 of its flags over 200 real commits were false. A false positive only over-protects; the misses are the risk. | `--self-test` exits 0 only while the committed code matches its pinned precision and recall. `cleanup_check.py` exits 1 when a cleanup's own diff removes a flagged or labelled block whole with no replacement. The length ratchet never exits non-zero for recogniser loss. | MUST |
| GRD-11 | A guard rewrite may drop a bare plan, decision or issue ID. It never drops a file-qualified pointer unless the same target reappears in the diff. | A bare ID resolves nowhere (none of 30 sampled resolved uniquely), while every file-qualified pointer sampled resolved. A rewrite that drops both together loses the one form that works. | The `comm` line above, run on the rewrite's branch. It compares pointers one by one, not whole files, and writes no temporary file. | MUST |

## Owners, Proven by a Planted Edit

An owner is a lint, a compiler or type error, or a named test that fails on the
guard's breaking edit. Prove it on a scratch copy: apply the exact edit the
guard names, run only the owner, and watch it go red.

| Owner | Run only it, with the breaking edit applied |
|---|---|
| Rust test | `CRATE=example_crate; NAME=example_test; timeout 1800 cargo test -p "$CRATE" "$NAME"` |
| Rust lint, type | `cargo clippy --all-targets`, `cargo check` |
| TypeScript test, type | `NAME=example_test; timeout 1800 npx vitest run -t "$NAME"`, `npx tsc --noEmit` |
| Python test, type | `NAME=example_test; timeout 1800 python3 -m pytest -k "$NAME"`, `npx pyright` |
| Any other language | The project's runner with its test-name filter, under the same `timeout` |

Four traps turn a proof into a false credit:

- **A substituted edit.** Apply the edit the guard names. A test that fails when
  a short-circuit is removed proves nothing about the other edit the guard argues against.
- **A leftover.** An unused import or binding the edit orphans fails the build
  under `-D warnings`. That is not the owner: delete the leftover and run again.
- **A pair.** Run each named test on its own and credit only the ones that fail.
- **A hang.** Exit 124 from `timeout` counts as red only when the guard's
  consequence is the hang itself. A mutation run supports a planted edit and never replaces it.

```ts
// wrong: the second test stays green when `undefined` collapses to `false`
// Covered by "unset flag defers to env" and "false flag disables verify".
// right: only the test that went red on the planted edit
// "unset flag defers to env" fails if undefined is read as false.
const verify = flag ?? envDefault();
```

```sh
# GRD-08: owned constructs with prose above them; a comment over one line there is a finding
grep -rn -B5 --include='*.ts' --include='*.tsx' --include='*.py' --include='*.rs' --exclude-dir=node_modules --exclude-dir=target --exclude-dir=.venv -e ': never = ' -e 'assert_never(' -e 'deny(clippy::wildcard_enum_match_arm' .
NAME=example_test; grep -rn -e "$NAME" .      # a test pointer: the definition plus every line citing it
# GRD-12: tri-state candidates collapsed into a boolean
grep -rn --include='*.rs' --include='*.py' --include='*.ts' --exclude-dir=target --exclude-dir=node_modules --exclude-dir=.venv -e 'Option.bool.' -e 'unwrap_or(false)' -e 'Optional\[bool\]' -e 'bool | None' -e 'boolean | undefined' .
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| GRD-07 | Before shortening a guard below GRD-01, list every breaking edit it names. Credit an owner for an edit only after that owner fails on it in a scratch copy. A check that only demands a comment exists owns nothing, and neither does a test that merely shares the guard's nouns. Every edit without a proven owner keeps its prose. | Credit by name was wrong or incomplete at 7 of 22 guarded sites that named a test: 3 named tests stayed green on the literal edit, and at 4 sites only one of two named tests failed. A compile-time assert passes when someone raises its own constant, and a scoped wildcard deny misses a `matches!` rewrite. | The planted-edit run above, per credited owner, through all four traps. For each uncredited edit, its prose is still present after the diff. | MUST |
| GRD-08 | When proven owners catch every breaking edit a guard names, cut the comment to one line naming them: the lint, the type, or every test that failed on the planted edit, never one that stayed green. Cut it to nothing when the owning construct states itself (`const exhaustive: never = x`, `assert_never(x)`, `#[deny(…)]`, a type whose constructor is the only path to a value). Use no prefix. | Once the owner fails CI on the edit, prose that re-derives the mechanism is a copy, and copies drift. A line naming a test that stays green sends a cold reader to a test that never sees the edit. In the eval, one line naming the owning test kept reason recovery at 13 test-owned guards, while the test alone, with the comment deleted, lost 10 points across 24. | The owned-construct grep above: more than one comment line directly above the construct is a finding. For a test pointer, the GRD-07 run showed that exact test red, and the name grep returns its definition. | SHOULD |
| GRD-12 | Before filing a breaking edit as comment-only or test-owned, ask whether a type would make it fail to compile or type-check: a tri-state enum in place of an optional or collapsed boolean, a validated newtype whose one constructor runs the check, a trait method with no default, a sealed policy parameter in place of two near-identical functions, a typestate or builder that cannot skip a step, or one value computed once and threaded through. Land the type in its own diff, never inside a comment cut, and credit it under GRD-07 only after the planted edit fails the type checker. Once it owns the edit, the comment follows GRD-08. | A compile error holds on every build; a test holds only when it runs and builds the scenario, and 3 of 22 named tests did not build theirs. About 45 of 100 sampled guards had a type shape. A type cannot own ordering inside one function body, a platform-API gap, durability that needs fault injection, a deliberate non-optimization, or a judgment call. | The tri-state grep above lists candidates; read each for a collapse that loses a state. Then the planted-edit run against the landed type: `cargo check`, `npx tsc --noEmit` or `npx pyright` must fail. | SHOULD |

## Exhaustive Matches and RAII Bindings

| Language | The checker that owns "this match stays exhaustive" | Planted proof on a scratch copy |
|---|---|---|
| Rust | `#[deny(clippy::wildcard_enum_match_arm, clippy::match_wildcard_for_single_variants)]` on the enclosing fn or impl. The first lint misses a `_` that covers exactly one remaining variant; the second catches it. Neither sees a `matches!` rewrite or delegation to another classifier, so those edits keep their prose | Add a `_ =>` arm; `cargo clippy` fails |
| TypeScript | `const exhaustive: never = x;` in `default:`, then a throw. typescript-eslint's `switch-exhaustiveness-check` is optional and needs typed linting first | Delete a case; `npx tsc --noEmit` fails |
| Python | `assert_never(x)` in the final `case _:`. At a public boundary, put it under `if TYPE_CHECKING:` and keep the runtime `raise` the contract names, because `assert_never` raises `AssertionError` at runtime | Delete a case; `npx pyright` fails. A bare `case _: raise` passes pyright |
| Any other | The compiler's or linter's exhaustiveness check, credited only after the planted proof fails it (GRD-07) | Add a default branch or delete a case |

```python
# wrong: pyright accepts this with a Kind missing
case _:
    raise TypeError(f"unknown kind: {kind!r}")
# right: pyright fails on a missing Kind, and callers still get TypeError
case _:
    if TYPE_CHECKING:
        assert_never(kind)
    raise TypeError(f"unknown kind: {kind!r}")
```

```sh
# GRD-09: Rust files claiming an exhaustive match without the deny; each output file is a finding
comm -23 <(grep -rli --include='*.rs' --exclude-dir=target -e 'no wildcard' -e 'rather than wildcard' -e 'not wildcarded' -e 'exhaustive match' -e 'must stay exhaustive' . | sort) <(grep -rl --include='*.rs' --exclude-dir=target -e 'wildcard_enum_match_arm' . | sort)
grep -rn -A2 --include='*.py' --exclude-dir=.venv -e 'case _:' .       # read each claimed-exhaustive match for assert_never(
# GRD-10: whether the lint is on, then every Drop type; read each guard type's declaration for #[must_use]
grep -rn --include='Cargo.toml' --exclude-dir=target -e 'let_underscore_drop' .
grep -rhoE --include='*.rs' --exclude-dir=target -e 'impl(<[^>]*>)? Drop for [A-Za-z0-9_]+' . | awk '{print $NF}' | sort -u
NAME=ExampleGuard; grep -rn -B3 --include='*.rs' --exclude-dir=target -e "struct $NAME" .
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| GRD-09 | Where a comment says a match must stay exhaustive, let the checker in the table above own it, and shrink the comment by GRD-08. In Rust, scope the deny to the fn or impl, never workspace-wide, and never write it as `#[expect]`, which silences the lint it names. | The compilers already reject a missed variant; the gap is a later wildcard or default arm. Workspace-wide, the wildcard lint flagged 41 to 232 sites per workspace, mostly test `_ => panic!` arms and legitimate wildcards on foreign `#[non_exhaustive]` enums. In Python, `case _: raise` satisfies pyright's coverage check. | The claim-without-deny `comm` line and the Python `case _:` read above. Then the planted proof per language: the checker must fail. | SHOULD |
| GRD-10 | Mark every Drop-guard type `#[must_use]`, which catches a bare `acquire();` statement. Give each named guard binding one line naming the rename hazard: renamed to `_`, it releases at once. Enable rustc's `let_underscore_drop` in `[workspace.lints.rust]` only after triaging the workspace's existing `let _ =` sites; it is the lint that catches the `_` rename, which `#[must_use]` does not. Run clippy's `let_underscore_must_use` and `let_underscore_untyped` once as an audit, never as standing lints. | Renaming `_lock` or `_slot` to `_` is routine cleanup that drops the guard immediately. clippy's `let_underscore_lock` sees only std lock guards, never a domain wrapper. `let_underscore_drop` fires on every `let _ =` of a type that needs drop, every discarded `io::Result` included: 39 to 163 sites across three workspaces measured, and under `warnings = "deny"` every hit must clear before the enable lands. The two clippy lints read 70% noise in a 30-hit sample. | Planted proof: rename a guard binding to `_` on a scratch copy; `cargo clippy` must fail. To size the triage, on a scratch copy set `warnings = "deny"` to `"warn"` and run `cargo clippy --all-targets -- -W let_underscore_drop`. Never add `-A warnings`: it silences the `-W` in either order, and the run exits 0 with zero hits. | SHOULD |

## What Agents Get Wrong Here

1. Writing the guard long and bundling several into one block: a third of
   agent guards run past 10 lines, against none of 39 human ones (GRD-02, GRD-03).
2. Putting an implementation hazard in the doc register, where it renders to
   hover, API docs and schemas (GRD-04).
3. Leaving a bare plan ID where the reason belongs, then dropping the
   file-qualified pointer with it in a rewrite (GRD-02, GRD-11).
4. Crediting a test by its name, a presence-only check, or a compile error from
   an orphaned import as the owner (GRD-07, GRD-08).
5. Pasting the same guard at every implementation (GRD-03).
6. Trusting a regex or a marker to protect guards: the recogniser misses more
   than half, and a `GUARD:`-style marker has no structural oracle (GRD-06).
7. Reaching for the wrong tool: `#[must_use]` for the `_` rename, `#[expect]`
   as a lint pointer, `wildcard_enum_match_arm` alone, sizing a lint under
   `-A warnings` (GRD-09, GRD-10).
8. Dropping the consequence clause while compressing (GRD-01), or writing the
   guard as an order (GRD-05).
9. Defending with prose or a test what a type could make fail to compile (GRD-12).
