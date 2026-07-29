# Bugfix: `ocx add` / `ocx remove` discard `ocx.toml` content

## Status

- **State**: implemented, verified
- **Issue**: [ocx-sh/ocx#256](https://github.com/ocx-sh/ocx/issues/256)
- **Branch**: `fix/toml-preserve-user-content`

## Reproduce

```sh
ocx init                      # writes a #:schema directive + [tools]
ocx add ocx.sh/shellcheck:0.11
```

Before the fix the file came back as:

```toml
[tools]
shellcheck = "ocx.sh/shellcheck:0.11"

[group]

[package]
```

— schema directive gone, comments gone, bindings sorted alphabetically, two empty
tables added. Reproduced by `test/tests/test_project_toml_preservation.py`, all ten
cases red against the pre-fix binary.

## Root cause

`MutationGuard::commit` re-serialized the parsed `ProjectConfig` over the whole file
(`toml::to_string_pretty`, `project/mutation.rs`; the same helper was duplicated in
`project/mutate.rs`). serde models neither comments nor source order, so everything the
typed struct does not describe was dropped on every mutation. The empty `[group]` /
`[package]` headers are the same cause from the other side: both fields serialize
unconditionally, having no `skip_serializing_if`.

Second, smaller defect in the same area: `init_project` wrote a commented-out
`registry = "ocx.sh"` hint, but `ocx.toml` has no `registry` key and
`RawProjectConfig` is `deny_unknown_fields` — uncommenting it made every subsequent
command exit 78 (verified).

## Fix

`crates/ocx_lib/src/project/document.rs` — `render_preserving(original, candidate, path)`
applies the mutation to a `toml_edit::DocumentMut` built from the on-disk text: stale
keys removed, new keys appended, an unchanged key not even re-inserted so its decor
survives. `[group]` super-tables are created implicit, so only `[group.<n>.tools]`
headers appear. The rendered text is re-parsed and compared against the candidate;
a mismatch fails the command (`ManifestEditDiverged`) rather than falling back to the
lossy rewrite — the `[env]` / `[package]` surfaces are deliberately unsynced because no
mutator touches them, and this check is what catches it if one ever starts.

`MutationGuard` now carries a `ManifestSnapshot { config, text }` so the commit path has
the document to edit; `mutate::{add,remove}_binding` route through the same helper. Both
copies of the whole-file serializer are gone.

## Regression coverage

- `crates/ocx_lib/src/project/document.rs` unit tests — comment/order/spacing
  preservation, no empty tables, group creation shape, env/package sections verbatim,
  add-then-remove byte round trip, both failure paths.
- `test/tests/test_project_toml_preservation.py` — the same contract end-to-end through
  the binary, plus `--group`, `--global`, and the `ocx init` → `ocx add` path.
- `test/tests/test_project_init.py::test_init_advertises_no_key_the_parser_rejects`.

Known limitation, unchanged from before: `toml_edit` normalises CRLF to LF, so a mutated
CRLF file comes back LF. Pinned by a test so the day it changes is visible.
