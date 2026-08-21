# Review R1 — test coverage: `ocx package copy`

- **Scope:** `git diff main...HEAD` on branch `evelynn`, baseline `main` (0ed4a446)
- **Focus:** quality, test-coverage emphasis (Specify-phase adequacy)
- **Verdict:** needs work
- **Evidence run:** `cargo nextest run -p ocx_lib --lib -E 'test(/oci::copy::|publisher::copy::|native_transport::tests::(file_backed|upload_)|test_transport::tests::/)'` -> 23 tests run, 23 passed, 4546 skipped.

## Tooling caveat (affects how these findings were produced)

`git diff` and `diff` are both intercepted in this environment and returned
false results: `git diff main...HEAD | grep ...` yielded a stat summary
instead of a patch (so every detector reported zero), and `diff <(git show
main:crates/ocx_lib/src/oci/client.rs) crates/ocx_lib/src/oci/client.rs`
printed "Files are identical" for a file `git diff --numstat` reports as
`4 4`. All findings below were rebuilt from a `python3 difflib` corpus over
`git show main:<path>` vs the worktree, plus direct file reads. Reviewers
using the shell diff tools here will get false negatives.

## Diff integrity — clean

Detectors run against the reconstructed added/removed corpus:

| Detector | Result |
|---|---|
| new `#[allow(` | none |
| removed `assert` | none |
| new `#[ignore]` | none |
| new `todo!` / `unimplemented!` | none — the diff **removes** two `unimplemented!()` stubs from `test_transport.rs` and replaces them with real implementations |
| new `unsafe` | none |
| new `.unwrap()` | 2, both inside `#[cfg(test)] mod tests` in `oci/copy.rs` |
| gate files (Taskfile / workflows / deny.toml / clippy.toml / rust-toolchain) | untouched |
| snapshots | none |
| `Cargo.lock` | untouched |

Two changes that look like gate edits and are not:

- `test/tests/test_state_registry_characterization.py` 12 -> 13 canonical setup
  names, adding `"promotion"`. Justified pin bump: the diff adds a
  `setup:promotion` provider in `test/recordings/setups.py`, and the assertion
  is still set **equality**, so any other drift still fails. Not a weakening.
- `.claude/tests/test_ai_config.py` gains `"package_copy": "package copy"` —
  additive, tightens the docs-coverage map.
- The `T-arch-A1` `canonical_reference` allow-list in `oci/client.rs` was **not**
  extended; the new modules route through `Client::read_reference` /
  `transport_write_reference`, which is the intended seam.

## Findings

### [Block] test/conftest.py:110-134 — the whole `test_package_copy.py` suite can vanish green

`target_registry` skips the session when `registry_is_reachable(addr)` is false.
Two independent paths make that reachable without anything failing:

1. `src/helpers.py::start_registry` short-circuits on `if
   registry_is_reachable(registry): return` — the **primary** registry. A
   developer or runner with a warm registry from before this branch never
   issues `docker compose up -d`, so the newly declared `target-registry`
   service is never created, and every test in the new file skips.
2. `pytest_sessionstart` gives `mirror_registry` an explicit 5 s retry loop with
   the comment "Compose was already run for the primary registry; ports may not
   be bound yet." The new `target-registry` service has the identical exposure
   and got **no** warm-up and **no** retry — a single probe, then skip.

`subsystem-tests.md` names this shape directly: "A whole file skipping itself
away is indistinguishable from a pass." The suite this gates includes
`test_a_signature_survives_the_promotion`, which the file's own module docstring
calls load-bearing. The same repo already contains the correct precedent:
`tests/fixtures/sigstore_stack.py::sigstore_stack` raises rather than skipping,
with the comment "A skip here is indistinguishable from a pass, and these are
the tests that carry the supply-chain contract."

**Remediation:** in `pytest_sessionstart`, probe `TARGET_REGISTRY` and issue
`docker compose up -d` + the same bounded retry the mirror gets; then make the
`target_registry` fixture **fail** (not skip) when it is still unreachable and
`OCX_TESTS_NO_REGISTRY != "1"`, matching `sigstore_stack`.

### [High] crates/ocx_cli/src/command/package_copy.rs + website/src/docs/reference/command-line.md — a documented exit code neither matches the code nor has a test

The reference table for `package copy` says:

| Condition | Exit code |
|---|---|
| No platform in the source matches `--platform` | 64 |

The implementation returns
`ClientError::InvalidManifest(format!("{source} offers no platform matching the
request"))` (`crates/ocx_lib/src/publisher/copy.rs:334-336`), which reaches
`CopyError::Other(crate::Error::OciClient(..))`. `crate::Error::classify`
delegates (`crates/ocx_lib/src/error.rs:336`) and `ClientError::InvalidManifest`
is in the `ExitCode::DataError` arm (`oci/client/error.rs:262-273`) — **65**, not
64. No test at any level exercises this path.

**Remediation:** add the test first (a two-platform source, `--platform` naming a
third), then align one side. Which side is a contract decision — see Deferred.

### [High] crates/ocx_lib/src/publisher/copy.rs:250-258 — `--canonical-tag` is default-on and untested everywhere

`CopyRequest.canonical_tag` is `true` by default at the CLI
(`options::CanonicalTag`), and phase 2 writes a `sha256.<hex>` tag per platform
via `push_canonical_tag`, reporting them in `CopyOutcome.canonical_tags` and in
the JSON envelope. Every unit test sets `canonical_tag: false`
(`publisher/copy.rs:528`), and no acceptance test asserts the tag exists at the
target or that `canonical_tags` is non-empty. A default-on write to the target's
tag namespace has zero assertions behind it.

**Remediation:** one acceptance assertion that
`fetch_manifest_from_registry(target_registry, repo, f"sha256.{hex}")` resolves
after a plain copy, plus a `--no-canonical-tag` run asserting it does not — the
paired form the referrers test already uses.

### [High] crates/ocx_lib/src/publisher/copy.rs — the two-phase write order has no test

`plan_package_copy.md` "Test Contracts / Rust unit" specifies: "Phase ordering: a
scripted failure on the first index merge leaves every leaf, referrer and
canonical tag written and no rolling tag moved." No such test exists. The stub
already has the mechanism (`StubTransportInner::push_results`, consumed FIFO), so
this is writable today.

This is the safety property the module doc-comment sells ("An interruption during
the first phase therefore leaves the target's tags exactly as they were",
`publisher/copy.rs:168-169`) and it is asserted nowhere.

**Remediation:** seed `push_results` so the first `merge_platform_into_index`
PUT fails, then assert on the `calls` log that `push_blob:*` and
`push_manifest_raw` ran and that no rolling-tag PUT did.

### [High] crates/ocx_lib/src/oci/copy.rs:300-320 — `verify_spooled_blob` is untested

The re-hash of the spooled blob before upload is the CWE-345 guard that
attributes wrong bytes to the *source* registry rather than to us; the doc
comment says so explicitly. `plan_package_copy.md` lists it as a unit contract
("A spooled blob whose re-hash disagrees with its descriptor -> typed error
before any upload"). No test drives it. `StubTransport::pull_blob_to_file`
writes whatever is in `blobs`, so seeding a digest key whose value does not hash
to it is a two-line fixture.

**Remediation:** seed `blobs[<digest of A>] = <bytes of B>`, assert
`ClientError::DigestMismatch` and that `calls` contains no `push_blob:`.

### [Warn] crates/ocx_lib/src/oci/copy.rs:508-529 — the unit byte-identity test cannot fail against a re-serializing copy, and its docstring claims it can

The docstring reads: "Compared as bytes — comparing parsed values would let a
re-serialisation through, which is the exact defect." The fixture is
`serde_json::to_vec(&leaf_manifest())` (`seed`, line 474), i.e. exactly what a
re-serialization would emit. `ImageManifest` is
`oci_client::manifest::OciImageManifest` — a plain
`#[derive(Deserialize, Serialize)] #[serde(rename_all = "camelCase")]` struct
with no `flatten` catch-all — so a deserialize/serialize round-trip of that
fixture is byte-stable, and an implementation that pushed
`serde_json::to_vec(&manifest)` instead of `leaf_bytes` would pass this test.

The author reached the same conclusion one level up: the acceptance docstring at
`test/tests/test_package_copy.py:97-108` says verbatim that "a
parse-then-reserialize round-trip of one of those is byte-stable — so it passes
against a copy that rebuilds the manifest, and cannot tell the two apart", which
is why `test_a_non_canonical_manifest_is_copied_byte_for_byte` exists. That
acceptance test **is** discriminating (pretty-printed body, `layers` before
`config`, digest taken over those exact bytes) and is good.

So this is a docstring-accuracy defect plus a gap in unit-level discrimination,
not a hole in the feature's coverage.

**Remediation:** seed the unit fixture from hand-written non-canonical bytes with
the digest computed over them (same trick as the acceptance test), or soften the
docstring to say the discriminating case lives in the acceptance suite.

### [Warn] test/tests/test_package_copy.py:205 — a tolerance band where the fixture fixes the answer

```python
assert dispositions[host] in {"added", "replaced"}
```

The setup publishes the target's tag for `other` only, so the target index
carries no entry for `host` and `lookup()` returns `None` -> `Disposition::Added`
is the only correct value. `subsystem-tests.md` "Unfalsifiable Greens" lists the
tolerance band as a shape that "cannot tell 'still a stub' from 'the binary
rejected my input'".

**Remediation:** `assert dispositions[host] == "added"`.

### [Warn] test/tests/test_package_copy.py:59-64 — `_target_has_tag` fails to `False`, and two negative assertions have no positive control

```python
def _target_has_tag(target_registry, repo, tag) -> bool:
    try:
        fetch_manifest_from_registry(target_registry, repo, tag)
    except Exception:
        return False
    return True
```

A connection error, a wrong argument order, a bad repo name and an auth failure
all render as "tag absent". Two call sites assert only the negative and have no
in-test positive:

- `test_dry_run_reports_the_plan_and_writes_nothing:257`
- `test_a_digest_source_without_a_platform_is_a_usage_error:356`

(The description tests at :450/:456 and :476/:478 do assert both polarities and
are fine.) `quality-python.md` classifies bare `except Exception:` as Block-tier
for exactly this reason; the repo has precedent (`test_cascade.py:234`) and one
site that annotates it (`test_doc_scripts_one_tree.py:93`, `# noqa: BLE001`),
and there is no ruff gate configured, so the lint would not have caught it.

**Remediation:** narrow the except to the HTTP/registry error the helper can
actually raise, and pair each `assert not _target_has_tag(...)` with a positive
assertion on a tag the same test knows is present (e.g. the source tag on the
source registry, or the target tag after the non-dry-run copy).

### [Warn] test/tests/test_package_copy.py — no error-path test asserts the stream

Every error test asserts `result.returncode == N` and passes `result.stderr`
only as the assertion *message*. `testing.md` TEST-10 requires asserting "`.code(n)`
and the stream contents **separately** — asserting on combined output cannot tell
a code change from a wording change." A user is expected to act on these
messages (they name `--platform`, `--identifier`, "copy the tag instead").

**Remediation:** add `assert "--platform" in result.stderr` (and the analogous
needle) to the four 64 tests and the 79 test.

### [Warn] test/tests/test_package_copy.py — three contracted behaviours have no test

- **`--offline` -> 81.** `plan_package_copy.md` item 9 lists it. Verified
  reachable: `Context::remote_client` returns `Err(Error::OfflineMode)` when
  `options.offline` (`app/context.rs:591`, construction at :228-230), and
  `Error::OfflineMode` classifies to `ExitCode::PolicyBlocked`
  (`error.rs:317`). No test, and the row is also missing from the reference
  doc's exit-code table.
- **Exit 80 (auth).** Documented in `command-line.md` ("Authentication to either
  registry fails | 80"). No test.
- **`--annotation KEY=VALUE`.** Documented option; every unit test passes an
  empty `BTreeMap` and no acceptance test passes the flag. The annotation
  reaches `merge_platform_into_index` and lands on the target index — untested
  end to end.

### [Warn] crates/ocx_lib/src/oci/copy.rs, publisher/copy.rs — three plan unit contracts absent

From `plan_package_copy.md` "Test Contracts / Rust unit":

- "Duplicate self-heal: an index seeded with two `linux/amd64` entries merges to
  one." — absent.
- "Aliased digest: two platforms sharing one digest both survive; one canonical
  tag written." — absent (and see the canonical-tag finding above).
- "same digest -> `unchanged` **and zero pushes recorded**" — only the *blob*
  half is covered (`a_second_copy_uploads_nothing`, `blobs.uploaded == 0`). The
  implementation deliberately re-runs `copy_leaf` on `Unchanged`
  (`publisher/copy.rs:201-207`), so the leaf manifest **is** re-pushed. Nothing
  pins either reading. See Deferred.

"Cascade blockers read from target-side versions" is listed as a unit contract
but is covered at the acceptance level
(`test_cascade_is_computed_against_the_target_not_the_source`) — level drift, not
a gap.

### [Suggest] crates/ocx_lib/src/oci/copy.rs:698-752 — the recursive-referrer test counts, it does not identify

`referrers_are_copied_recursively_and_only_when_asked` asserts
`copied.referrers == 2` and two `push_referrer_manifest` calls, then discards the
depth-2 manifest's digest (`let _ = signature_digest;`, line 734). Two pushes of
the *same* referrer twice would satisfy it. The fixture-integrity assertion at
:721-726 (the seeded descriptor names the seeded manifest) is a good pattern and
should be extended to the assertion side: check
`pushed_manifest(&data, &target, &sbom_digest)` and
`pushed_manifest(&data, &target, &signature_digest)` are both `Some`.

Same class: `let _ = leaves;` at `publisher/copy.rs:573`.

### [Suggest] test/tests/test_package_copy.py:222 — idempotency `uploaded == 0` has no in-test positive control

`first` asserts only the disposition set; nothing asserts `uploaded > 0` on the
first pass, so the second pass's `uploaded == 0` is unpaired within the test.
The unit test `a_second_copy_uploads_nothing` does pair it
(`second.blobs.present == 2`), so the risk is contained.

### [Suggest] test/recordings/setups.py, test/docker-compose.yml — the `promotion` setup and `prod-registry` (5004) are name-pinned only

The characterization test asserts the key `"promotion"` exists; nothing executes
the function. `prod-registry` on 5004 is consumed only by the asciicast
recording, never by pytest, so a broken third registry fails nothing in
`task test`. Acceptable for a recording fixture; noted so it is a decision
rather than an accident.

## What is well covered

- **Diff integrity is clean**, including the removal of two `unimplemented!()`
  stubs from the stub transport.
- **The 84 negative control is exactly right.**
  `test_referrers_against_a_registry_without_the_api_exits_84` runs both halves —
  `--referrers` (default) -> 84 and `--no-referrers` against the *same*
  `legacy_registry` -> 0 — which is what proves the 84 comes from the capability
  probe and not from the target being a different host. This is the pattern the
  rest of the file's negatives should copy.
- **`test_a_non_canonical_manifest_is_copied_byte_for_byte`** is the one test
  that genuinely discriminates byte-copy from re-serialize, and it is
  constructed correctly (foreign key order, `indent=2`, digest over those bytes).
- **`file_backed_body_streams_the_same_bytes_as_memory_and_replays`**
  (`native_transport.rs`) drains the file-backed body twice and asserts equality
  both times — a source consumed by its first read fails the third assertion.
  Multi-frame by construction (`UPLOAD_FRAME_SIZE * 2 + 7`).
- **`StubTransportInner::blob_locations` as `Option<HashMap<..>>`** with the
  documented reason ("an empty map would make a copy test that forgot to set it
  up pass for the wrong reason") is the right call, and `blob_location_key`
  keys on registry **and** repository so a same-repo cross-registry promotion is
  not reported as already-present.
- **The stub's own tests** pin that a pushed referrer is listable back and that
  the `artifact_type` filter both selects and rejects — the fixture is tested
  before it is trusted.
- **No self-referential tests** (TEST-13): no test uses the implementation as its
  own oracle.
- **Determinism** (TEST-05..09): no `env::set_var` anywhere (env reaches
  subprocesses via `env_overlay`, the sanctioned form), no wall-clock
  assertions, no hash-order assertions (`index_platforms` returns a set compared
  to a set), no fixed temp paths, no real sockets in the Rust suite.
- **No structural source-text guards** were added, so the dead-gate failure mode
  (`quality-rust.md` "Structural guards") does not apply to this diff.
