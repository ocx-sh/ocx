# Vendored index wire-format conformance fixtures

Byte-exact golden vectors for the `ocx-sh/index` CONTRACTS §14 serializer,
**vendored verbatim** from that repo's `bot/tests/golden/serializer/` — never
hand-edit. They are this crate's cross-language conformance corpus: the Rust
`serializer_parity` test (Track A — not built by the plan that vendored these)
parses and re-serializes each vector and asserts byte-identity against the
committed bytes, proving the Rust wire writer matches the Python bot's output
exactly.

## Layout

- `root/*.json` — `PackageRoot` vectors in the pretty-printed root form
  (2-space indent, insertion-order fields, one trailing newline).
- `observation/sha256/<hex>.json` — `ObservationObject` vectors in the minified
  CAS form (alphabetized keys, `ensure_ascii`, no trailing newline); `<hex>` is
  the sha256 of the file's own bytes, matching the real CAS filename convention.

## Provenance & re-sync

- `SOURCE_COMMIT` pins the `ocx-sh/index` commit these bytes came from.
- Re-sync with `test/scripts/sync_index_conformance.sh` — fast path when a local
  `ocx-sh/index` checkout is available, otherwise a GitHub fetch pinned to
  `SOURCE_COMMIT`. It prints a diff and **never commits**; review and commit the
  result yourself.
- Deliberately **not** wired into `task verify`: an automatic live check would
  reintroduce the network dependency this vendored copy exists to remove
  (offline-first). Staleness is caught by re-running the sync script, not at
  test time — an explicit, accepted trade-off.
