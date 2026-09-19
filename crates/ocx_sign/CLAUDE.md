# CLAUDE.md — ocx_sign

Agent-specific guidance with no other home. `README.md` says what this crate
owns; this file states only what a change here must not break.

## No `ClassifyExitCode` impl belongs in this crate

C-054 and E3 state it and the code holds it: nothing here implements
`ClassifyExitCode` or `ClassifyErrorKind`, and nothing here should. The
exit-code taxonomy lives in `crates/ocx_cli/src/exit/` — `exit/ocx_sign.rs`
carries this crate's arms, and the two rungs for it are
`downcast_arm!(cause, SignError)` and `downcast_arm!(cause, VerifyError)`.

The doc comments in `sign/error.rs` and `verify/error.rs` name those traits and
are right to: the three-layer shape (E2) is `SignError { identifier, kind }` →
`SignErrorKind`, where the wrapper is the armed type and the Kind is a pure
discriminant. The *impls* for both live in the binary. Writing one here would
compile — the trait is `pub(crate)` to `ocx_cli`, so in practice it would not —
and it would be the first crack in the rule that one error has one
classification site.

## Several types here are unarmed on purpose, each with a recorded reason

`UNARMED_AT_THE_BOUNDARY` in
`crates/ocx_test_support/tests/workspace_structure.rs` holds a row per type this
crate hands the CLI that no `downcast_arm!` registers —
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
`ClassifyErrorKind::exit_code`, the same impl the armed wrapper delegates to, so
the code is identical by construction rather than by coincidence.

## The transparency-log split is a contract, not a category

`TransparencyLogUnavailable` (86) and the trust/policy refusals (85, 78, 64) are
distinguished so a CI script can retry the first and never the second. A change
that folds a log-availability failure into a policy refusal, or the reverse,
changes what every caller's `case $?` does. `exit/ocx_sign.rs`'s tests pin each
family; a diff that reds one of them is a published-interface change.
