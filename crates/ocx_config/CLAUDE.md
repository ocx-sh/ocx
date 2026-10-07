# CLAUDE.md — ocx_config

Agent-specific guidance with no other home. `README.md` says what this crate
owns; this file states only what a change here must not break.

## Classification is declared on the types, never hand-implemented

The error types here derive `ocx_exit::Classify`: every variant carries one `#[exit(...)]`
with its exit code, `error.detail` slug and summary, a guard or computed answer goes through
`with = fn, rows(...)` (`TlsError` picks by origin: a file the operator named is data, inline
text is the configuration), and a wrapper delegates (`error::Error::Toolchain`,
`ManagedConfigUpdateError`). Nothing here writes a `ClassifyExitCode` or `ClassifyErrorKind`
impl by hand. `ocx_cli`'s `exit/ocx_config.rs` holds the family's contract tests, its rungs
being entries of the `families!` list in `exit.rs`, and three error types here reach a user
without a rung of their own, each for a stated reason recorded in `UNARMED_AT_THE_BOUNDARY`
in `crates/ocx_test_support/tests/workspace_structure.rs`:

- `ToolchainRootError` — reaches the CLI only inside `error::Error::Toolchain`,
  whose classifier already delegates to this type's own. A second, closer
  classifier on a value that already has one **moves an exit code**.
- `ConsentPatternError` — the `[shell.consent]` deserializer hands every
  refusal to `serde::de::Error::custom`, which erases the type into a
  `toml::de::Error` before it leaves the loader.
- `deserialize_mirrors_table`'s `D::Error` — the deserializer's own associated
  type, not a nameable one. There is nothing to register.

Arming any of them is a behaviour decision and an ADR question, not an arm
added in passing.

## The two `__` features are not interchangeable

`__testing` is the ordinary seam feature, forwarded by `ocx`'s own `__testing`
and therefore **present in the acceptance binary**. It gates
`loader::SYSTEM_CONFIG_OVERRIDE`, `edit`'s lock-hold delay and
`managed_config::test_support`.

`__test_scaffolding` is not forwarded, and must never be. It gates
`ToolchainRoot::from_validated`, which bypasses C-017 to C-019 wholesale — an
unvalidated `toolchain_dir` root, which the real funnel cannot produce — and
`sandbox_or_skip`. Only a **dev**-dependency turns it on; a dev edge is not
transitive and never enters `cargo build -p ocx`. Inside the monolith the same
guarantee came free from `#[cfg(test)]`; across a crate boundary `#[cfg(test)]`
carries nothing, which is why it is a feature now.

`sandbox_or_skip` is one function rather than the `anchor_sandbox` /
`sandbox_or_skip` pair it was in the monolith: under a feature gate — which,
unlike `#[cfg(test)]`, the armed-error scan reads as production — the inner
half's `Result<_, String>` was an error type crossing a boundary no
`families!` rung could ever cover. Do not split it back apart.

## `managed_config` is one module because a crate has only one

`ocx_lib` had two before the split: `config::managed_config` (the on-disk layout) and
`managed_config` (fetch, persist, pause). Both are this crate, and both cannot
be called `managed_config`, so they are one module with `paths` kept as the
layout half. The data model they serve is `crate::managed`, which is a third
thing again — `ManagedConfig` / `ResolvedManagedConfig` / `resolve_managed_config`.
Publishing a managed config (`ocx config push`) is
`ocx_package_manager::managed_config` and must stay unreachable from here.

## Every failure the loader raises is `error::Error`

`ConfigLoader::{load, load_with_local_view, load_and_merge}` return this crate's
own `Result`, and that is a measured fact rather than a convention: narrowing
them from the then-crate-wide `ocx_lib::Error` produced compile errors in test
helpers only, and seven `.into()` widenings in `loader.rs` became
`clippy::useless_conversion` to the same type. A `?` that needs a different
error here is a new variant on `error::Error`, not a widened return type.
