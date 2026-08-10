# Vendored index conformance fixtures

Golden vectors **vendored verbatim** from `ocx-sh/index` (`bot/tests/golden/`) —
never hand-edit. They are this crate's cross-language conformance corpus: the
same bytes drive the Python bot's tests and this crate's, so two implementations
of one rule cannot drift silently. A fixture generated from ocx's own code would
certify whatever ocx does, which is the failure mode the corpus exists to prevent.

## Layout

- `root/*.json` — `PackageRoot` vectors in the pretty-printed root form (2-space
  indent, insertion-order fields, one trailing newline), asserted **byte-exact**:
  `index_wire_conformance.rs::root_fixtures_round_trip_byte_exact` re-serializes
  each vector through `oci::index::serialize_root` and compares bytes.
- `catalog/normal.json`, `config/normal.json` — `c/index.json` and `config.json`
  vectors, one case each (C-025/S-017). All 8 `render/<case>` cases upstream emit
  byte-identical `config.json` and structurally identical `c/index.json` (same
  shape, differing only in the one packages key/digest), so a single case proves
  the shared `PythonJson` formatter for both documents; more would repeat the same
  assertion. No upstream `render/<case>` renders a multi-package or an empty
  catalog, so those shapes are pinned only by `wire_writer.rs`'s own unit tests,
  not by cross-language parity. Asserted byte-exact by `index_wire_conformance.rs`'s
  `catalog_fixtures_round_trip_byte_exact` (`oci::index::serialize_catalog`) and
  `config_fixtures_round_trip_byte_exact` (`oci::index::serialize_config`).
- `tag_verdicts.json` — `{tag, reserved, why}` rows for the reserved-tag rule
  (`adr_oci_index_only_dispatch.md` D7). `tag_verdicts.rs` drives every row
  through both `Tag::is_reserved` and `Tag::is_reserved_str`. Reservation spans
  `oci::Algorithm::ALL` = {sha256, sha384, sha512}, deliberately wider than
  D7:319's sha256-only text — reservation and addressability are different
  questions, and only reservation widens. `why` is documentation, never asserted.
- `cpython/*` — **not** vendored from `ocx-sh/index`: generated locally from
  CPython's `json` module, which is itself the §14 byte authority the bot repo
  implements. They exist because the vendored corpus is a sample of real index
  documents, so it certifies only the bytes the index happens to ship today — a
  wrong escape boundary (DEL emitted raw) once survived it because no fixture
  contains a `0x7F` byte. `codepoint_escapes.txt` asks CPython scalar by scalar;
  `layout.json` / `top_level_array.json` cover the structural shapes the sample
  lacks (empty containers, array root, u64/i64 extremes, escape-bearing keys).
  Regenerate with `python3 cpython/generate.py`, which documents the selection;
  never hand-edit. Asserted by `index_wire_conformance.rs`'s
  `codepoint_escapes_match_the_cpython_truth_table` and
  `cpython_structural_vectors_round_trip_byte_exact`.
- `dispatch/sha256/<hex>.json` — real OCI image indices, one per file, each named
  by the sha256 of its own bytes (the CAS convention of `p/<ns>/<pkg>/o/<algo>/`,
  D1). `dispatch/expected_platforms.json` records, per vector, the exact
  `(platform, digest)` candidate list a correct selection derives.
  `dispatch_conformance.rs` asserts both.

`dispatch/` vectors are **decode** parity, not byte parity: what a tag points at
is a registry's own image index, stored byte-for-byte as served and never
re-rendered (`adr_oci_index_only_dispatch.md` R4), so nothing re-serializes them.
The CAS filename guards the *fixtures* — it fails when a vector is hand-edited
or regenerated rather than re-captured. It does not guard ocx: that ocx keeps
the served bytes is enforced by `LocalIndex::stage_dispatch_bytes` recomputing
the digest before it writes, and pinned by that module's unit tests.

Upstream's `dispatch/README.md` records where every byte in that directory came
from: which registry, which index digest, and the one field deliberately altered.
It is not vendored — read it at the pinned commit in `ocx-sh/index`.

## Provenance & re-sync

- `SOURCE_COMMIT` pins the `ocx-sh/index` commit these bytes came from.
- `test/scripts/sync_index_conformance.sh` re-vendors: a bare re-run verifies
  against the pin, `--ref <ref>` vendors and re-pins, `--check` compares the
  vendored tree against `ocx-sh/index@main` without writing to it. It prints the
  resulting working-tree changes and **never commits** — review and commit
  yourself.
- Deliberately **not** wired into `task verify`: a live check at test time would
  reintroduce the network dependency this vendored copy exists to remove
  (offline-first). Drift is bounded instead by `task test:index-conformance-drift`
  on the weekly `verify-deep.yml` schedule — a 7-day window, not per-PR network
  cost for a signal that changes monthly.
