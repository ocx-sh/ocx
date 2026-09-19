# ocx_project

The project tier, extracted from `ocx_lib` at WP-33.

## The subtree flattened; `ocx_project::project` does not exist

`ocx_lib::project::**` became this crate's root rather than a `project`
module, so a caller writes `ocx_project::config`, not
`ocx_project::project::config`. `ocx_oci` set that precedent when it flattened to
`auth/ client/ digest/ …`; `ocx_shell` kept its subtree only because it holds two
of them. `activate`, `ladder` and `lazy` came along as siblings — the per-prompt
path crosses all four, and splitting them would have put a crate boundary
through one sequence.

The practical consequence when reading old code or an old artifact:
`ocx_lib::project::X` is now `ocx_project::X`, one segment shorter.

## Never name a crate that depends on this one

`ocx_package_manager` depends on this crate and is its heaviest caller, so an
edge back would be a cycle — as it was for `ocx_lib` before WP-37 deleted it.
Cargo enforces this, but the failure arrives as a resolution error rather than
a review comment, so it is worth knowing before you write the line. The rule
covers doc links and doctests too: a doc link into a non-dependency resolves to
nothing, and a doctest `use` of one needs a dev-dependency, which is the same
cycle. `lock.rs`'s example is re-pointed at `ocx_project::lock::lock_path_for`
for that reason.

## The error type is the tier's own, and its exit codes are pinned

`Error` carries `Project`, and — since WP-33 — `OciClient`, `OciIndex`, `Config`
and `InternalFile`, because the tier raises those itself and could no longer
borrow the crate-wide `ocx_lib::Error` for them (E1, DEC-27; that enum was
deleted outright at WP-37).

**Each new variant reproduces exactly what its predecessor in that enum
classified to** (DEC-23): the first three delegate to the wrapped error's own `classify`,
and `InternalFile` is `IoError` (74). Delegation is not a style choice —
`ClientError` and the index error distinguish transient from terminal, and a
flat code would collapse 69 into 74. The arms live in
`crates/ocx_cli/src/exit/ocx_project.rs`; a new variant needs one there in the
same commit, or the binary stops compiling, which is the intended tripwire.
