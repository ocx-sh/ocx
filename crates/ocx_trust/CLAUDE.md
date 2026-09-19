# CLAUDE.md — ocx_trust

Agent-specific guidance with no other home. `README.md` says what this crate
owns; this file states only what a change here must not break.

## No `ClassifyExitCode` impl belongs in this crate

C-054 states it and the code holds it: nothing here implements
`ClassifyExitCode`, and nothing here should. The exit-code taxonomy lives in
`ocx_cli`, and every error this crate raises reaches a user only after a caller
has consumed it **by value** into that caller's own kind:

- `KeyEnvError` → `TrustPolicyError::KeyMalformed`, or `KeyBackendError::{Io,
  MalformedKey}` in `ocx_sign`'s key backend.
- `KeyRefError` → `SignErrorKind` / `VerifyErrorKind` via their `From` impls,
  which is what routes an unimplemented backend to 85 and everything else to 64.
- `TrustPolicyError` → `VerifyErrorKind::TrustPolicyInvalid`, whose armed
  classifier matches on the inner variant.

So none of the three has a `downcast_arm!` rung, and all three are recorded in
`UNARMED_AT_THE_BOUNDARY` in `crates/ocx_test_support/tests/workspace_structure.rs`
with that reasoning. **Arming any of them moves an exit code**, which is a
behaviour decision and not a refactor. If you think one needs a code, that is an
ADR question, not an arm added in passing.

## `KeyEnvError` and `KeyRefError` carry no `#[non_exhaustive]`, on purpose

Both are matched exhaustively from `ocx_sign`, and since WP-25 that match crosses
a crate boundary — where the attribute stops being inert and forces a `_` arm.
A variant added later would then reach that arm silently instead of failing the
build, and each variant here answers with its own exit code. The crate is
`publish = false`, so there is no downstream to break. Do not "fix" their
absence; the compile error a new variant causes is the point.

## The operator tier is authoritative, and the gate is the tier, not specificity

`resolve_tiered` consults the operator set first and, **if any operator policy
matches the target, never looks at the project `ocx.toml` at all** — a project
can neither override nor weaken an operator pin, whether by writing a more
specific scope or by enrolling a second signer at the same one. That ordering is
the security property; the most-specific-wins and ANY-of rules operate *within* a
tier, beneath it. `operator_tier_is_authoritative_over_project` pins the gate;
`system_locked_pin_refuses_a_more_specific_unlocked_entry`,
`equal_specificity_entry_cannot_join_the_locked_any_of_set` and
`a_system_locked_object_scope_still_governs_its_targets_alone` pin the lock's own
defence over the pooled operator array; `system_locked_cannot_be_set_from_toml`
pins that no file can set the flag; and
`a_locked_operator_pin_does_not_open_the_project_tier` pins the composite the
others leave open — the gate must not consult the lock. A change that reds any of
them is a change to who may sign.

## The `ScopeSpec` schema is hand-written, and must stay that way

A derive reads the Rust type, so it cannot see what the hand-rolled
`Deserialize` actually accepts: the derive emits `anyOf` where the shared union
helper emits `oneOf`, and it cannot express "one of `include`/`exclude` is
required", which is the whole point of the refusal. An editor bound to a derived
schema would show no error for `scope = {}` while ocx exits 78 on the same file.
Two golden schemas (`config`, `project`) pin the output.
