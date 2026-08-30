# WP-5 execution report — sweep double-fetch (#373)

Branch `hex/issue-sweep--wp5`, base `52eecc15`. Contracts C-040, C-041, C-042; scenario S-011.

## Contracts

- **C-040 — DONE.** `sign_tags` now hands the resolution it already paid for to
  `sign_one` → `SignContext.resolved` → `resolve_platform_target`, which fetches only when
  handed `None`. `resolves_to_index` is renamed `resolve_swept_index` and returns
  `Result<Option<(Digest, Manifest)>>` — `Some` is the index, `None` is the bare manifest
  the sweep skips, `Err` is unchanged. Measured 3 tags: **6 → 3** index resolutions.
- **C-041 — DONE.** `attest_tags` / `attest_one` / `AttestContext` carry the same field
  through the same shared `resolve_platform_target`. Measured 3 tags: **6 → 3**.
- **C-042 — DONE.** Both non-pre-resolving callers pass `None`, and the `None` arm is the
  old body verbatim (fetch, then `TargetNotFound` on `Ok(None)`); only ownership changed,
  from a moved pair to a borrowed one. Proven, not asserted — see the acceptance runs
  below, plus the 8 `platform_narrowing_tests` that drive every branch of the function
  with `None`.

## Measured resolutions

| Sweep | tags | before | after |
|---|---|---|---|
| `sign_tags` | 3 | 6 | 3 |
| `attest_tags` | 3 | 6 | 3 |

Recorded — not merely counted — by an `IndexImpl` double (`sweep_test_support::CountingIndex`,
`tasks/sign.rs`) that appends the reference it was asked about on every `fetch_manifest`, and
asserted against the exact ordered list of tag references the sweep owes. The prior art is
`test_source` in `oci/index.rs`, which instruments an `IndexImpl` the same way to count
`trusted_hosts` questions. A bare tally was the first shape; it proves *how many* resolutions
happened and nothing about what each was for, so a sweep resolving the wrong tag N times would
have read as fixed.

The double answers with an **image index**, never a bare manifest: a sweep skips a bare manifest
without entering the pipeline, so a fixture of that shape would record one resolution per tag
whether or not the answer is threaded. Two positive controls keep the green honest — each tag's
outcome must be `Failed` (not `SkippedBareManifest`), and the stub registry must record one
`pull_manifest_raw` per tag, which happens only *after* the resolution being recorded.

A third test, `a_supplied_resolution_is_acted_on_and_the_index_is_never_asked`
(`oci/sign/pipeline.rs`, `platform_narrowing_tests`), pins what the counting tests structurally
cannot see: *which answer* the rule then applied. It drives `resolve_platform_target` against
`EmptyIndex` — which resolves to nothing — while supplying a resolution, through both arms of the
narrowing rule, so an implementation that took the supplied digest but re-fetched the children
still reds.

## Red / green proofs

The mutation is the fix applied in reverse: `resolve_platform_target`'s `Some` arm forced to fall
through to the fetch (`match resolved.filter(|_| false)`, tagged `// __MUTANT__`). Presence of the
token was greped before each run and its absence after the restore, so a no-op edit could not have
passed for a real one.

> The first attempt at that mutation — `match None::<&(Digest, Manifest)>` — left `resolved`
> unused and the crate's `-D warnings` turned it into a **build** failure, which is a red that
> proves nothing about the tests. Rewritten to keep the parameter live.

| Step | Command | Exit | Result |
|---|---|---|---|
| Red | `cargo test -p ocx_lib --lib -- resolves_each_tag_exactly_once a_supplied_resolution` | **101** | 0 passed, **3 failed** |
| Restore + green | same command | **0** | **3 passed** |

Red output, verbatim (both sweeps; the duplicate per tag is visible in the assertion):

```
oci::sign::pipeline::platform_narrowing_tests::a_supplied_resolution_is_acted_on_and_the_index_is_never_asked ... FAILED
package_manager::tasks::sign::tests::a_tag_sweep_resolves_each_tag_exactly_once ... FAILED
package_manager::tasks::attest::tests::an_attest_tag_sweep_resolves_each_tag_exactly_once ... FAILED
  left: ["…:1.0.0", "…:1.0.0", "…:1.0.1", "…:1.0.1", "…:1.1.0", "…:1.1.0"]
 right: ["…:1.0.0", "…:1.0.1", "…:1.1.0"]
test result: FAILED. 0 passed; 3 failed
```

An earlier red, taken against the threaded-but-not-yet-honoured stub before the fix existed at
all, reported the same 2N for both sweeps (`left: 6 / right: 3`, exit 101). The two sweeps are
independent tests: threading one and not the other leaves the other red.

## Gates

Re-run after rebasing onto `goat` at `dd6a7b6b` (WP-6, WP-1, WP-4, WP-2, WP-3), which replayed
with no conflict. WP-4's `FileKeyBackend` → `PemKeyBackend` rename touches `build_signer` only,
which this package does not; WP-2's bounded `--tags-file` read lands at `package_push.rs:602`,
37 lines from this package's `, None` at `:565`.

| Gate | Command | Exit |
|---|---|---|
| Rust | `task rust:verify --force` | **0** — 6457 tests run, 6457 passed, 8 skipped; 9 stages (fmt, clippy `-D warnings`, license, deny, notice, build, windows-cfg, unit) |
| Counting proof, re-measured on the rebased tree | `cargo test -p ocx_lib --lib -- resolves_each_tag_exactly_once a_supplied_resolution` | **0** green / **101** mutated / **0** restored |
| Acceptance — sign, attest, push | `pytest tests/test_sign.py tests/test_attest.py tests/test_push.py` | **0** — 77 passed, 1 xfailed |

`test/bin/ocx` rebuilt from the rebased worktree with `--features ocx/__testing` before the
acceptance run. This package touches no file under `test/**`, so WP-6's ruff gate has nothing
of its to lint.

## Scope

**Two files outside the declared scope were edited, one line each.** `attest_one`'s signature
change forces its other two callers to compile:

- `crates/ocx_cli/src/command/package_attest.rs:218` — `, None`
- `crates/ocx_cli/src/command/package_push.rs:565` — `, None`

Neither file appears in any work package's *Expected files* column in the plan, so no sibling
collides here. Refusing would have left the crate not compiling, and the alternative — a
second `attest_one_resolved` entry point — is the compat shim `CLAUDE.md` forbids outright.

`tasks/sign.rs`'s `build_signer` (WP-4's site) is untouched; the two diffs are separated by
15 lines of unchanged text.

## Deliberate omissions

- **No new acceptance test for S-011.** The plan's collisions table assigns WP-5 an
  acceptance test for it, but the scenario's substance — how many manifest fetches a sweep
  performs — is not CLI-observable: a 2N run and an N run emit byte-identical reports and
  the same exit code. The only acceptance test writable here is the smoke test the handover
  explicitly ruled out. The nine sweep and single-reference acceptance tests already in
  `test_sign.py` / `test_attest.py` cover every observable half, and were run green above.
- **`Option<&(Digest, Manifest)>` rather than a named struct.** That pair is
  `Index::fetch_manifest`'s own return type; minting a `ResolvedManifest` for one field
  would add a type whose only job is to re-say what the fetch already says.

## Review

One Opus adversarial pass over the diff: **pass, no Block, no Warn, two Suggest.** It traced
both sweeps end to end and confirmed no lost `Index::fetch_manifest` side effect — the sweep's
call is the local miss that persists the dispatch object and commits the root tag, so the
second call was already a local-first pure read.

- **Taken.** The counting tests were identifier-blind, and the `Some` arm had no unit-level
  assertion on its *output*. Both are closed above.
- **Declined.** A `debug_assert_eq!` that the supplied pair belongs to the identifier being
  signed. The pair carries no identifier, so the guard needs one threaded alongside it — which
  is the named wrapper type this work package deliberately did not mint, added to guard a call
  site that does not exist and cannot exist without someone writing new code against a doc
  comment that warns them off. Recorded here rather than silently dropped.

## Deferred

Nothing. No finding was left unaddressed.
