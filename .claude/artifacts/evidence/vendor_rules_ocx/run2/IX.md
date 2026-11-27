# IX — the index lookup recorder (run 2)

Branch `hex/bazel-rbe-rules-ocx--IX`, base `333a5264`. One commit: `feat(vendor): record the index paths a vendor run reads`.
Scope of every result below: the named unit tests and per-crate lanes; no acceptance module ran (V-L55's location is `test_vendor.py`, owned by VA).

## Step 1 — recording (C-118 lookups half)

All five `read_index_document` call sites in `crates/ocx_index/src/store.rs` go through `IndexStore::read_document`, which records
the path relative to the store root, `/`-separated, found = document present. A failed read and a target outside the root record nothing.

| Test | Red | Green |
|---|---|---|
| `ocx_index` `store::wire_grammar_tests::contract_recorder_records_a_found_and_an_absent_path` | CW2: `store.rs:2790`, `left: []`; mutation (recording removed from `read_document`): `store.rs:2800` | pass |
| `ocx_index` `chained_index::chain_refs_tests::read_only_view_keeps_the_lookup_recorder` (new) | mutation (recording removed from `read_document`): `chained_index.rs:1594` | pass |
| `ocx_index` `store::wire_grammar_tests::recorder_survives_clone` | green since CW2 | pass |

`cargo test -p ocx_index --locked`: 538 lib + 4 + 10 + 3 integration, all pass. clippy `-D warnings` and fmt clean.

## Step 2 — attachment

`crates/ocx_cli/src/app/context.rs` attaches `LookupRecorder::new()` only when `ContextOptions.index` (the `--index` flag) is set;
`OCX_INDEX` and the default home attach none. `PackageManager::index_lookups()` maps the recorder into `IndexLookup`s; `None` without one.

| Test | Red | Green |
|---|---|---|
| `ocx_package_manager` `tests::index_lookups_returns_recorded_paths_only_with_a_recorder` (new) | `lib.rs:596:9` `not implemented: index_lookups: filled by the index lookup step` | pass |

`cargo test -p ocx_package_manager --lib`: 1630 passed, 1 ignored. `cargo clippy -p ocx -p ocx_package_manager --all-targets --locked -- -D warnings` and fmt clean.

## Not run

- `task verify:scoped --force`: `scripts/scoped_gate.py --plan` returns `escalate` on the branch diff (routes a Bazel lane, rc-file rule, DP-39); lanes run by hand as above.
- `cargo test -p ocx`; no CLI-level unit covers "no recorder without `--index`" — V-L55 (`test_vendor.py`, VA.2) and its mut row (PF) cover it.
