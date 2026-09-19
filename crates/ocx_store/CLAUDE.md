# CLAUDE.md — ocx_store

Agent-specific guidance with no other home. `README.md` says what this crate
owns; this file states only what a change here must not break.

## No `ClassifyExitCode` impl belongs in this crate

C-054 states it and the code holds it. The exit-code taxonomy lives in
`ocx_cli` (`exit/ocx_store.rs` carries this crate's whole family), and three
error types here reach a user without a `downcast_arm!` rung of their own, each
recorded with its reason in `UNARMED_AT_THE_BOUNDARY` in
`crates/ocx_test_support/tests/workspace_structure.rs`:

- `assemble::AssembleError`, `cas_path::DigestFileError` and
  `package_store::PackageDirError` — the three narrow unions WP-27's E1 minted
  so this tier could stop naming the crate-wide `Error` (`ocx_lib`'s, until
  WP-37 dissolved it). Each is rebuilt into the exact variant it replaced by an
  `impl From<..>` in `crates/ocx_package_manager/src/error.rs`, so the value
  that reaches the CLI is the armed `ocx_package_manager::Error` and never the
  union.

Arming any of them adds a *second, closer* classification for a value that
already has one — which moves an exit code. That is a behaviour decision and an
ADR question, not an arm added in passing.

## Narrow the error, do not widen the signature

`ocx_util::error::FileError` is the whole failure surface of ten of this
crate's modules, and `From<FileError> for ocx_package_manager::Error` yields exactly
`InternalFile(path, cause)`. A new fallible function returns `FileError`, or a
union of it with the *one* other shape that function can actually raise — never
a wide enum "so callers can match". `FileError` deliberately carries no
`source()` (ocx#286); do not add one.

The one union that earns its keep is `AssembleError::SymlinkWalk`: the
destination symlink-safety check bypasses `io::Error::other` so an
ancestor-symlink refusal classifies as `UsageError` (64) and not the flat
`IoError` (74). Collapsing it back to a bare `FileError` silently re-forces 74.

## The two `__` features are not interchangeable

`__testing` is the ordinary seam feature, forwarded by `ocx`'s own `__testing`
and therefore **present in the acceptance binary**. It gates
`shim_bin_store`'s `__OCX_TESTING_SHIM_LOST_PUBLISH_RACE`.

`__test_scaffolding` is **not** forwarded by anything that ships. It exists so
`ocx_lib`'s own unit tests can reach a seam a bare `#[cfg(test)]` cannot cross a
crate boundary to see — today `BlobStore::write_call_count`, the instrument
`pull_local`'s singleflight-coalescing test reads. Putting a product seam behind
it, or forwarding it from `ocx`, makes a unit-test instrument part of the
binary.

## `shim.rs` embeds committed blobs by relative path

`include_bytes!("shims/ocx-shim-<arch>.exe")` resolves against `shim.rs`'s own
directory, and two tests scan `env!("CARGO_MANIFEST_DIR")/src/shims`. Moving
either the module or the blob directory breaks both at once, and
`.github/workflows/build-windows-shims.yml` names the same two paths in its
`paths:` filter and in five shell steps. `SHIM_SHA256` is a corruption canary,
not a provenance control — refresh it only in the dedicated blob-refresh PR.
