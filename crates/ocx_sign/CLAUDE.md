# CLAUDE.md — ocx_sign

Agent-specific guidance with no other home. `README.md` says what this crate
owns; this file states only what a change here must not break.

## Classification is declared on the types, never hand-implemented

`SignErrorKind`, `VerifyErrorKind` and the two wrappers derive `ocx_exit::Classify`:
every variant carries one `#[exit(...)]` with its exit code, `error.detail` slug and
summary, and a guard or computed answer goes through `with = fn, rows(...)`. Nothing here
writes a `ClassifyExitCode` or `ClassifyErrorKind` impl by hand. `exit/ocx_sign.rs` in
`ocx_cli` carries only the family's contract tests; `SignError` and `VerifyError` are the two entries
of its `families!` list in `exit.rs`.

The three-layer shape stays: `SignError { identifier, kind }` delegates to `SignErrorKind`
(`#[exit(delegate = kind)]`), and `Internal` defers (`chain, fallback(...)`) so the walker
reaches the wrapped cause, since a `Some(Failure)` would exit a wrapped registry 401/503 as 1.

## Several types here are unarmed on purpose, each with a recorded reason

`UNARMED_AT_THE_BOUNDARY` in
`crates/ocx_test_support/tests/workspace_structure.rs` holds a row per type this
crate hands the CLI that no ladder rung registers —
`attest/dsse.rs::VerifyErrorKind`, `attest/predicate.rs::serde_json::Error`,
`attest/statement.rs::{SignErrorKind, VerifyErrorKind}`,
`sbom/cyclonedx.rs::SbomError` and their siblings. Each is reached by the CLI
only inside an armed wrapper, or is rendered with `Display` into a refusal
reason and never classified at all.

**Arming one moves an exit code** (DEC-23), which is a behaviour decision and
not a refactor: a rung on a Kind would register a second, *closer*
classification for a value whose wrapper already has one. If you think a type
needs its own code, that is an ADR question.

The one hand-classification is `command/package_sign_common.rs::leg_exit_code`,
for the case the error path cannot express — a `--signature-format both` run
where one leg failed and another did not, so the run is `Ok`. It calls
`ClassifyExitCode::classify`, the same impl the armed wrapper delegates to, so
the code is identical by construction rather than by coincidence.

## The transparency-log split is a contract, not a category

A transient transparency-log failure (`transparency_log_unavailable`, exit 75 `TempFail`, the one retry code) and the trust/policy refusals (82, 81, 78, 64) are
distinguished so a CI script can retry the first and never the second. A change
that folds a log-availability failure into a policy refusal, or the reverse,
changes what every caller's `case $?` does. `exit/ocx_sign.rs`'s tests pin each
family; a diff that reds one of them is a published-interface change.
