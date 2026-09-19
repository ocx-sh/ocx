# CLAUDE.md — ocx_shell

Agent-specific guidance with no other home. `README.md` says what this crate
owns; this file states only what a change here must not break.

## Never `use ocx_project`, and never `use ocx_package_manager`

`project::consent` reads `shell::coexistence` and `shell::reconcile`, and
`package_manager::activation` sequences a reconcile. Both therefore depend on
this crate, so an import in the other direction is a cycle — and since WP-32 it
is a **Cargo error**, not a convention.

That is a change in kind worth knowing about, because the rule used to be
enforced by two source-text guards that walked `src/shell/**` looking for the
string `crate::project`: `no_project_dependency_under_shell` in
`package_manager/activation.rs` and `shell_does_not_import_activation` in
`ocx_lib/tests/boundaries.rs`. Both were deleted at WP-32 rather than
re-pointed, because a guard whose red state is unreachable reads as coverage
while providing none (DEC-47). The second was shown red first — `walked 0
file(s) across 2 subtree(s)` — rather than assumed dead.

So: if you find yourself wanting application-layer sequencing here, it belongs
in `ocx_package_manager::activation`, exactly as it did before. The compiler now
tells you so directly. Six doc references pointing the other way survive as
plain text rather than intra-doc links, for the same reason — the crate they
name is not a dependency and cannot become one.

## The CI export path returns `ci::error::Error`, not a crate-wide error

Its nine signatures were `crate::Result<()>` while `shell/` and `ci/` lived in
`ocx_lib`. WP-32's E1 narrowed them to what the sites can actually produce:
every `?` under `ci/` converts a `ci::error::Error` (`MissingEnv`, `File`,
`Write`) and nothing else is constructed there.

**This was checked against the exit-code table, not assumed.** `ocx_lib`'s
`Error::Ci` arm read `Self::Ci(e) => e.classify()` — it delegated to the very
impl the DEC-24 bridge `ocx_cli/src/exit/ocx_shell.rs` provides for `CiError` —
so narrowing moved no exit code. That enum is gone since WP-37 and the
delegation is now the ladder's rung on `CiError`, which is the same answer. Do the same check before narrowing anything
else here: DEC-23 is the hazard that an extraction silently relocates a code
while every test still passes.

`shell::error::Error` is the other half of the same E1. Its one reach, the
`TryInto<clap_complete::Shell>` associated type, is consumed by exactly one
caller — `ocx_cli/src/command/shell_completion.rs` — which formats it into an
`anyhow::bail!`. The wrapper never reached the classifier, which is why
`Self::Shell(_) => None` was not a constraint on the narrowing.

## The `__testing` feature exists for one gate, and it is not the shell zoo

`shell::hook` exposes a registration-delay seam under
`#[cfg(any(test, feature = "__testing"))]`. The C-044 latency gate in
`shell-activation.yml` injects that delay so its `--expect-fail` step has a
reachable red state; the shell-zoo artifact is release-clean and physically
lacks the code path. The two artifacts are not interchangeable in either
direction — pointing the latency gate at the zoo binary leaves it unable to
prove anything. That workflow's `paths:` filter names `crates/ocx_shell/src/`
since WP-32, re-pointed only after its red was observed.
