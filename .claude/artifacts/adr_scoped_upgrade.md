# ADR: Scoped `ocx upgrade` (per-tool / per-group)

## Metadata

**Status:** Accepted
**Date:** 2026-07-09
**Deciders:** Michael Herwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, thin CLI over `ocx_lib`)
**Domain Tags:** api, cli
**Supersedes:** the "no subset upgrade" stance in `design_spec_partial_mutator_pin_preservation.md` (§2 table `upgrade` row, §4.5, §8.1)
**Superseded By:** N/A

## Context

`ocx upgrade` re-resolves **every** advisory tag in `ocx.toml` and rewrites the whole `ocx.lock`.
There is no way to advance *one* tool. The concrete user need: **change one tool's version** — e.g.
advance `ripgrep` to where its declared tag points today — while every other pin stays frozen.

This whole-file-only shape was not incidental. It was a deliberate, Codex-reviewed decision in the
**approved** spec `design_spec_partial_mutator_pin_preservation.md` (§2, §4.5, §8.1), which *deleted*
the earlier `--group`/positional surface from `lock` and `upgrade`. `upgrade.rs` even carried two
tests (`rejects_group_flag`, `rejects_positional_args`) locking that in. The stated rationale was two
guarantees:

- **Codex-H1 — anti-laundering.** A mutator must never move a pin the user did not ask to move. A
  silent full re-resolve reachable from `add`/`remove` would "launder" a stale lock under a fresh
  declaration hash.
- **Codex-R2 — anti-drift.** An untouched binding must never be live-re-resolved as a side effect of
  an unrelated operation; its pinned content is carried forward exactly.

This ADR **reverses** the "no subset upgrade" decision — but in a way that keeps both guarantees
intact.

## Decision Drivers

- Single-tool version advancement is a first-class package-manager need (cf. `cargo update -p <crate>`,
  `npm update <pkg>`, `poetry update <pkg>`).
- The reversal must not reintroduce the laundering/drift path Codex-H1/R2 closed.
- Smallest safe diff: reuse existing machinery, add no new resolution engine.
- The signed `handshake_toolchain_cli.md:77` already lists `ocx [--global] upgrade [-g grp]`, so `-g`
  partially *restores* the handshake surface rather than inventing a new one.

## Industry Context & Research

**Trending approaches:** every mainstream lockfile-based manager separates a whole-file upgrade from a
targeted one — `cargo update` vs `cargo update -p <crate>`; `npm update` vs `npm update <pkg>`;
`poetry update` vs `poetry update <pkg>`; `pip-compile --upgrade` vs `--upgrade-package`. The targeted
form advances only the named dependency and leaves the rest of the lock pinned. OCX's whole-file-only
`upgrade` was the outlier.

**Key insight:** the spec's blanket ban was **broader than its own rationale required.** The dangerous
path it deleted was an *unnamed, silent* full live re-resolve reachable from mutators
(`resolve_lock(&[])` inside `add`/`remove`) — pins moving while the user was not looking. Scoped
`upgrade` is the *opposite* operation: an explicit, user-named advance that reuses the very
pin-preserving primitive `add`/`remove` already depend on.

## Considered Options

### Option 1: Keep whole-file-only; edit `ocx.toml` to pin a version

**Description:** To move one tool, the user pins an explicit version in `ocx.toml`, then runs `ocx lock`.

| Pros | Cons |
|------|------|
| Zero code change | Does not solve "advance the *moving* tag by one step" — the user must know and hand-write the new version |
| No reversal of a reviewed decision | Turns a version *advance* into a declaration *edit*; a moving tag (`:latest`, `:3`) cannot be nudged without freezing it |

### Option 2: New standalone `ocx bump <name>` command

**Description:** Add a separate verb for targeted advancement.

| Pros | Cons |
|------|------|
| Leaves `upgrade` untouched | Splits a single concept (advance advisory tags) across two verbs; violates "extend existing mechanisms, do not duplicate" |
| — | New command surface, new docs, new taxonomy row for no semantic gain over a scoped flag |

### Option 3: Scope `ocx upgrade` by name / group, routed through `resolve_lock_touched`

**Description:** Reintroduce `-g/--group` and positional binding names on `upgrade` only. Bare
`ocx upgrade` keeps whole-file behavior; a scoped invocation re-resolves only the explicitly named
`(group, name)` pairs and carries every other pin forward verbatim, via the same `resolve_lock_touched`
primitive `add`/`remove` use.

| Pros | Cons |
|------|------|
| Matches every mainstream manager's targeted-upgrade UX | Reverses a Codex-reviewed decision (mitigated: guarantees preserved, see below) |
| Reuses `resolve_lock_touched` — no new resolution engine, CLI-wiring-only | Adds `-g`/positional parsing + a selection helper to `upgrade.rs` |
| Partially restores the signed handshake's `[-g grp]` | Two `upgrade` modes to document |

## Decision Outcome

**Chosen Option:** Option 3 — scope `ocx upgrade` by name / group.

**Contract:**

| Invocation | Behavior |
|---|---|
| `ocx upgrade` | Whole-file bump — **unchanged** (`resolve_lock(&[])`). |
| `ocx upgrade NAME...` | Advance only the named bindings' declared tags; freeze the rest. |
| `ocx upgrade -g GROUP...` | Advance every binding in the named group(s); freeze the rest. |
| `ocx upgrade -g GROUP NAME...` | Advance the named bindings **within** those groups (mirrors `ocx run`). |

- **Advances the declared tag's current resolution; does NOT change the declaration.** A moving tag
  (`:latest`, `:3`) moves to its current tip. To pin a *new* explicit version, edit `ocx.toml` — that
  is a declaration change, not an upgrade.
- A **name matches by config binding key across every group it appears in** (each resolves against its
  own declared tag); `-g` narrows the match to those groups. A name present in several groups advances
  in all of them — deliberate, no ambiguity error (unlike `ocx run`'s compose path, which must pick one).
- **Scoped mode requires a predecessor `ocx.lock`** → exit 78 if absent (`resolve_lock_touched` needs a
  `previous` lock to carry untouched pins forward).
- **Freshness:** `ocx.toml` must be current with the lock → exit 65 (`StaleLockOnPartial`) if hand-edited
  since locking (message names `ocx lock`).
- **Unknown group or name** → exit 64 (`UsageError`), mirroring `ocx run`.
- `--check`, `--pull`/`--no-pull`, `--platform`, `--global` all compose with scoping unchanged.

**Rationale — why Codex-H1 and Codex-R2 still hold.** Scoped upgrade reuses `resolve_lock_touched`
(`crates/ocx_lib/src/project/resolve.rs`), the same primitive `add`/`remove` use, and the guarantees
follow from three properties of that path:

1. **Only the explicitly named `(group, name)` pairs re-resolve against the live index**
   (`resolve.rs` step 3). Nothing unnamed moves → **no laundering (Codex-H1 holds).** The dangerous
   deleted path was an *unnamed, silent* `resolve_lock(&[])` from a mutator; scoped upgrade is the
   named, user-invoked inverse.
2. **Every untouched entry is carried forward verbatim** — V2 byte-identical, V1 exact-transcribe
   (`merge_carry_forward`), never a live-tag re-resolve → **no drift (Codex-R2 holds).**
3. **The freshness gate refuses if `ocx.toml` drifted from the lock** (exit 65) *before any resolve*,
   so a scoped upgrade can never launder a stale lock under a fresh hash.

The deleted `resolve_lock(&[])`-from-mutator path **stays deleted**. Bare `ocx upgrade` (no args) keeps
its existing whole-file behavior. The reversal is scoped precisely to a user-named, pin-preserving
operation — the exact shape the two guarantees permit.

### Consequences

**Positive:**
- Single-tool / single-group advancement without touching any other pin.
- No new resolution engine; CLI-wiring-only change over a verified primitive.
- Brings `upgrade` in line with `cargo`/`npm`/`poetry`/`pip-tools` targeted-upgrade UX.
- Restores part of the signed handshake surface (`[-g grp]`).

**Negative:**
- Two `upgrade` modes to document and test.
- Reverses a previously reviewed decision — audit trail must make the guarantee-preservation argument
  explicit (this ADR).

**Risks:**
- *Risk:* a future refactor makes the scoped path fall back to "resolve everything" on an empty touched
  set, reintroducing laundering. *Mitigation:* `resolve_lock_touched` is contractually explicit-touched
  (never falls back to whole-file on empty), and the freshness/coherence gates fail closed.

## Technical Details

### API Contract

```
fn select_touched(config: &ProjectConfig, groups: &[String], names: &[String])
    -> Result<Vec<(String, String)>, CommandError>   // UsageError (64) on unknown group/name

resolve_lock_touched(candidate, pre_mutation, previous, index, &touched, opts) -> ProjectLock
// candidate == pre_mutation == config for a lock-only op (upgrade stages the identity closure)
```

Scoped dispatch in `crates/ocx_cli/src/command/upgrade.rs`:
- no `groups` and no `names` → existing whole-file path (`resolve_lock(&[])`), including the `--check`
  missing-predecessor gate — **left untouched**;
- scoped → require `guard.previous_lock()` (else 78); `select_touched` builds the pairs; call
  `resolve_lock_touched`; `--check` compares candidate to predecessor via `lock_content_matches`.

Library layer is **unchanged** — `resolve_lock_touched`, `expand_all_keyword`, `DEFAULT_GROUP`,
`ALL_GROUP` are already public.

## Validation

- [x] Rust unit tests: `select_touched` (name→pairs, group→pairs, name+group intersection, unknown
      name/group → 64) + parse tests for `-g`/positional.
- [x] pytest acceptance: single-tool bumps only named, group scopes to group, untouched-pin byte
      preservation, scoped-no-lock → 78, scoped-stale-toml → 65, unknown-name → 64; whole-file
      `test_upgrade_bumps_every_tag` and offline-policy-blocked retained.
- [ ] CLI help gates (`app::tests::cli_help_text_*`) — ASCII + no internal references.
- [ ] Recommended: `/codex-adversary` plan-artifact pass on this ADR, since it reverses a
      Codex-reviewed design.

## Links

- [design spec (superseded stance)](./design_spec_partial_mutator_pin_preservation.md)
- [handshake — CLI model authority](./handshake_toolchain_cli.md)
- `crates/ocx_lib/src/project/resolve.rs` — `resolve_lock_touched`, `merge_carry_forward`
- `crates/ocx_cli/src/command/upgrade.rs` — scoped dispatch + `select_touched`

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-09 | Michael Herwig | Initial draft — reverse "no subset upgrade"; scope `upgrade` by name/group via `resolve_lock_touched`, guarantees preserved |
