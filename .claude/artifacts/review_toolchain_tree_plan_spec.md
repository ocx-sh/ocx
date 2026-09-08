# Spec-compliance review — `plan_toolchain_tree_layout.md`

**Focus:** spec-compliance · **Phase:** post-stub · **Scope:** plan artifact
**Date:** 2026-09-07 · **Reviewed against:** `adr_toolchain_activation.md` §
*Addendum — 2026-09-07* (C-071…C-084, validation items 39-50, R11-R13),
`research_toolchain_tree_doc_surface.md`, `research_toolchain_tree_corrupt_states.md`

**Verdict: not ready for `/hex-execute`** — 7 Block findings; the wave graph and
WP-4's file list are unexecutable as written, and 6 of 14 contracts plus S-003
have no cited test step.

This file carries the pieces that truncated in transit. The seven Blocks were
delivered and accepted separately and are not restated here.

## Two team-lead corrections, folded in

- **`## Implementation Steps` is being added.** The Warn stands as written and is
  answered by that edit: `Active phase: 1 — Stubs (grammar)` currently names a
  phase the document does not define.
- **`website/src/_scripts/**` is generated, not drift.** Agreed — its own
  `.gitignore` names `website:scripts:publish` as the producer and is the only
  tracked file of the 75. The research's byte-diff observed the publish
  transform, and its own caveat ("the regeneration path was not identified in
  this pass") is now closed. **WP-6 does not edit those files; it re-runs the
  task.** Add the step; drop the file glob I proposed.

---

## 1. Coverage — all 14 contracts and all 3 scenarios

| ID | WP | Test step | Gap |
|---|---|---|---|
| C-071 | WP-1 | ADR item **39** | item uncited by the plan |
| C-072 | WP-1 | ADR item **39** (renders-at half only) | uncited; **no item covers `links_group()`'s `HOSTILE_COMPONENTS` refusal through *both* accessors — mint item 51, §4** |
| C-073 | WP-1 | ADR item **40** | item uncited |
| C-074 | WP-2 | ADR item **41**; §C rows 17, 18 | item uncited; **no item covers "a depth-1 dir named after a *locked* group is pruned" — mint item 52, §4** |
| C-075 | WP-2 | ADR item **49** (partial); §C row 6 | §C 6 is option-B-only and **not** tagged `[active-only]` |
| C-076 | WP-2 | ADR item **41**; §C rows 17, 18 | item uncited |
| C-077 | WP-3 | **none** | **verify-only — see §5**; no item to mint |
| C-078 | WP-1 (accessors) / WP-3 (exclusion set) | ADR item **45** | item uncited |
| C-079 | WP-2 | ADR items **42, 48**; §C rows 21, 22, 29 | — |
| C-080 | WP-1 (predicate) / WP-2 + WP-3 (callers) | ADR items **43, 47(a)**; §C rows 23, 30 | — |
| C-081 | WP-2 | ADR items **44, 49**; §C rows 24, 26, 27, 28, 31 | — |
| C-082 | WP-0 | ADR items **47(b), 49**; §C rows 1, 2, 3 | — |
| C-083 | WP-2 | ADR item **50**; §C rows 15, 31 | — |
| C-084 | WP-3 | ADR item **46** | red unreachable on Linux — plan already flags this correctly |
| S-001 | WP-5 | WP-5's lock-swap test | sits outside §C, which the plan calls "the matrix of record" |
| S-002 | **no WP cell** — prose only (`plan:150`) | §C rows 1, 2, 3 | add to WP-0's Scope cell |
| S-003 | **no WP cell** | ADR item **40** | add to WP-1's Scope cell |

Every contract has a WP. No WP claims a contract the ADR does not define.
**8/14 contracts have a cited test step.** The root cause is single: the plan
adopts corrupt-states §C as the test matrix of record, but §C tests the
*corrupt-state* space (A1-A21); Block 1's contracts are tested by ADR validation
items 39-50, which no WP cites. Adding items 39-50 to § Testing strategy with a
per-WP mapping closes six of the seven gaps at once.

## 2. Wave assignment — option B (buildable)

WP-7 dissolves. The option is chosen by **which contracts ride**, not by which
wave exists — which is what makes one re-cut serve both answers.

| WP | Wave | Contracts | Files |
|---|---|---|---|
| WP-0 | 0 | C-082 | `crates/ocx_lib/src/package_manager/tasks/render_toolchain.rs` (`publish_link_within:1540`), `test/tests/test_toolchain_render.py` |
| WP-1 | 1 | C-071, C-072, C-073, **C-078 accessors**, **C-080 predicate** | `crates/ocx_lib/src/file_structure/toolchain_store.rs`, `crates/ocx_lib/src/file_structure.rs`, `crates/ocx_lib/src/project/{config,error,mutate}.rs`, `crates/ocx_lib/src/cli/classify.rs` |
| WP-2 | 2 | C-074, C-075, C-076, **C-079, C-081, C-083** + 8 RT rows + 7 active-only RT rows | `render_toolchain.rs`, `crates/ocx_lib/src/file_structure/state_store.rs` |
| WP-3 | 2 | C-077, **C-080 gate**, **C-084** | `activation.rs`, `setup.rs`, `setup/session_path*.rs`, `package_manager/composer.rs`, `env.rs`, `activate.rs`, `record/execution_record.rs`, `package_manager/mutate.rs`, `package_manager/launcher/body.rs`, `oci/client/builder.rs`, `crates/ocx_cli/src/{app.rs,command.rs,command/**,options/pinned.rs,api/data/shell_state.rs}`, `crates/ocx_shim/src/{core,main}.rs` |
| WP-5 | 3 | S-001 + 9 TR rows + 6 active-only TR rows | `test/src/toolchain_fixtures.py`, `test/tests/test_toolchain_render.py` |
| WP-4 | 3, **after WP-5** | — | `test/src/shell_matrix.py`, `test/tests/test_toolchain_cli.py`, `test_trampoline_exec.py`, `test_toolchain_offline_after_pull.py`, `test_session_path.py` |
| WP-6 | 3 | — | `website/src/docs/**`, `test/doc_scripts/*.sh`, `website/src/public/casts/**`; **step:** re-run `website:scripts:publish`, then `task schema` |

Three things this fixes, in order of severity:

- **The B ordering.** ADR `:1232-1233` puts `shell_bin`/`active`/`DEFAULT_SHELL`
  in wave 1 and the `active` publish in wave 2. The plan's WP-7 at wave 4 put them
  *after* the renderer, the tests and the docs that must already speak them.
- **`active_is_valid` lives in `toolchain_store.rs`** (WP-1), so WP-2's heal and
  WP-3's gate call one function. That is C-080's "one validity predicate,
  read-only, shared by the gate and the heal" read literally.
- **WP-1 compiles.** Its current file list omits `cli/classify.rs:1359` (constructs
  `ToolchainPathError::Reserved`), `project/config.rs:1034` (raises
  `ReservedToolchainName`) and `project/mutate.rs:916-946` (asserts it).

## 3. Wave assignment — option C (differs from the plan's cut)

Take the table in §2 and **strike the bold contracts** (C-078, C-079, C-080,
C-081, C-083, C-084) and the active-only rows. Waves, files and dependencies are
otherwise unchanged. Two edits then remain, both inside the test matrix:

- §C row 5 (`a_symlinked_tree_own_directory_is_refused_before_any_write`) is
  written against `home/"shells"` — **re-anchor it to `links`**.
- §C rows 6 and 7 (`an_unknown_shell_directory_is_left_alone`,
  `test_an_emptied_shells_directory_is_repopulated_by_the_next_pull`) are
  `shells/`-only — **strike them**.

This is why the plan's `plan:83` ("Every other work package is identical under B
and C") does not hold: rows 5-7 are option-dependent and untagged, and under B,
WP-2's closed set and WP-3's PATH consumers change too.

### Row assignment (both options)

- **RT** = `render_toolchain.rs` `mod tests` → **WP-2**: rows 1, 3, 4, 5, 6, 12,
  14 (Windows), 15; active-only 21, 22, 26, 27, 28, 31, 33 (Windows).
- **TR** = `test/tests/test_toolchain_render.py` → **WP-5**: rows 2, 7, 9, 10, 11,
  13, 16, 17, 18; active-only 23, 24, 25, 29, 30, 34 (Windows).
- **TS** = `toolchain_store.rs` `mod tests` → **WP-1**: row 8, verify-only (the
  existing `ensure_gitignore_*` cases), so the 18th row is not silently dropped.

**The `toolchain_fixtures.py` collision:** WP-5 owns that file outright — it
already extends `two_branch_checkout:511` and `snapshot_tree:238-262` — and WP-4
depends on WP-5 rather than running beside it. Splitting the fixture into a third
package for one file is not worth the overhead.

**Counts, corrected:** §C has **32 live rows including** the active-only ones, not
32 plus a further 15. After striking row 32 the active-only set is **13**, and the
live not-CI-verifiable rows are **14, 33, 34 = 3**, not 4 (row 35 is struck). Row
8 has no mutation, so "each with a named mutation" is false for it.

## 4. Two validation items to mint

Paste into § *Validation*, continuing the ADR's numbering.

> 51. **Both accessors refuse the hostile set identically.** For every name in
>     `HOSTILE_COMPONENTS`, and for the `Empty`, `ControlCharacter`, `Separator`,
>     `PathPrefix`, `TrailingDotOrSpace` and `Relative` cases, assert
>     `ToolchainHome::links_group(name)` and `ToolchainHome::entry(name, "x")`
>     return the *same* `ToolchainPathError` variant; and that an accepted pair
>     resolves exactly three components below the root with `links` first.
>     **Red:** implement `links_group` as `root().join("links").join(group)`
>     without `validate_component` — every hostile name resolves and the
>     paired-variant assert reds on the first row. Second, independent red: drop
>     the `links` literal from `entry()` and watch the three-component assert
>     read two.
>
> 52. **A depth-1 directory named after a locked group is pruned.** Seed an empty
>     `<root>/<g>/` for a group `<g>` that *is* in the lock, beside the correctly
>     rendered `<root>/links/<g>/`; render; assert `<root>/<g>` is gone and
>     `<root>/links/<g>` is intact with its entry links.
>     **Red:** keep `locked_groups` in the depth-1 scan
>     (`render_toolchain.rs:1455`) — `<g>` is allowlisted as a group name, so the
>     stale directory survives every render and the removal assert reds.

Item 51 covers C-072's second testable clause; item 52 covers the half of C-074
that item 41 (which tests only the *legacy* tree) does not reach. Both reds are
the exact mutations the contracts name, so neither is a green that cannot go red.

## 5. C-077 — verify-only, do not mint

**Call: verify-only.** C-077 re-anchors *clause text* to accessors, and the ADR
states at `:1104` that the ruling text in `rulings_toolchain_activation.md` is
deliberately left byte-identical. There is nothing observable to assert that the
existing suite does not already assert: the only behavioural half is the session
and login-stream `PATH` order (C-059/C-060, RUL-62/RUL-87, A-15/A-16), which
`test/tests/test_session_path.py` and `test_self_activate.py` already pin. A new
item would restate them and pass for the same reason they pass — a second green
that cannot independently go red.

**But verify-only is not unverified.** WP-3's Scope cell must name the shipped
order assertions it relies on, and the merge gate for WP-3 is that those tests
pass **unmodified** across the accessor swap. If any of them needs editing, the
swap changed behaviour and C-077 was not a re-anchoring — that is the signal to
stop and mint an item after all.
