# Plan — the six `test_push_report_*` acceptance tests have never run

## Status
- State:   done
- Tier:    medium
- Updated: 2026-08-29
- Next:    /hex-review high (branch diff)

## Root cause

**`test_push_report_json_schema_has_required_fields` and its five siblings have
never executed a single assertion, because each invokes `ocx package push` with
two independent usage errors and then converts the resulting exit 64 into a
`pytest.skip`. Both defects were introduced with the tests themselves in
`dcdcd2b3`; the JSON path they claim is "not yet implemented" has worked the
whole time.**

Reproduced in this worktree against the live `registry:2` fixture:

```
$ ocx --format json package push --cascade --format json -i localhost:5000/foo/bar:1.0.0
error: unexpected argument '--format' found
Usage: ocx package push --cascade [LAYERS]...
rc=64

$ ocx --format json package push --cascade -i localhost:5000/foo/bar:1.0.0
{"schema_version":1,"command":"package push","exit_code":64,
 "error":{"kind":"usage_error","message":"--platform is required: …"}}
rc=64
```

- **Defect 1 — `--format` in the wrong position.** `OcxRunner.run`
  (`test/src/runner.py:112-115`) already prepends the root-level `--format
  json`. The six tests pass `"--format", "json"` a *second* time, after the
  `push` subcommand. `--format` is not `global = true`: it lives on
  `options::Format` (`crates/ocx_cli/src/options/format.rs:17-30`), flattened
  into `ContextOptions` (`crates/ocx_cli/src/app/context_options.rs:86-87`),
  which is flattened only into the root `Cli` (`crates/ocx_cli/src/app.rs:104-110`).
  Its own doc comment says so: *"Applies to every command; there is no
  per-command `--format`."* clap rejects the second occurrence → exit 64.
- **Defect 2 — nothing to push.** Even with the stray flag removed, the
  invocation names no layer positional and no `--metadata`, so
  `ocx package push` has no bundle and no build receipt to read a platform
  from → exit 64 again.
- **The amplifier.** `if result.returncode != 0: pytest.skip(...)` is
  unconditional cover: it cannot tell "the feature is unimplemented" (its
  claim) from "my command line is malformed" (the truth). The skip message
  names a cause it never observed — the exact shape `quality-core.md`
  § "Unchecked Green" calls Block-tier, and a green indistinguishable from
  never having run.

**Why it matters now.** Wave 1 added `PushReport.platform_digests`
(`crates/ocx_cli/src/api/data/push.rs`), the signing input for the whole
cosign-parity flow. Only the three `S-00x` tests below the helper cover it;
the six blind tests are the ones that would have caught a regression in the
report contract as a whole.

## Census — one bug or a pattern?

`grep -rn --include='*.py' -B4 'pytest.skip' test/ | grep -e returncode` →
**8 return-code-gated skips**:

| Site | Verdict |
|---|---|
| `test/tests/test_package_push.py:57,87,114,140,167,194` | **DEFECTIVE ×6** — gates on the exit code of the command under test |
| `test/tests/test_schema_generation.py:81` | GENUINE — `cargo build -p ocx_schema` probe (no cargo in env) |
| `test/tests/test_project_env.py:1628` | GENUINE — same `ocx_schema` build probe |

Every remaining `pytest.skip` in `test/` probes a real capability
(`shutil.which`, `platform.machine`, `os.geteuid`, `sys.platform == "win32"`,
container absence). **One bug, six instances, confined to one file** — not a
codebase pattern.

## Component contracts

- **C-001/C-002 (revised)** — no new helper and no new parameter. `_push_json`
  becomes **create-once**: it computes the bundle path first and skips the
  layer/metadata/`package create` block when that file already exists. Calling
  it twice with identical arguments therefore pushes the *same* bundle. Two
  lines instead of an extraction plus a signature change across three existing
  callers. Every existing caller is untouched.
  - Why create-once at all: `ocx package create` refuses an existing `-o`
    without `--force` (`crates/ocx_cli/src/command/package_create.rs:111`), so
    a second unguarded call aborts before it reaches the push.
  - **Corrected during review.** This plan first claimed bundles are not
    byte-reproducible because `archive/tar.rs:117` copies the source mtime.
    That is false: `tar.rs:26` sets `tar::HeaderMode::Deterministic`, which
    zeroes uid/gid/mtime, and `test_headers_have_zero_ownership_and_constant_mtime`
    in the same file pins it. The claim survived because the falsifying line
    sits 91 lines *above* the line cited as evidence, and the search that
    looked for mtime normalization was scoped to
    `crates/ocx_lib/src/package/` — a directory that excludes the very file
    the claim was about. A directory-scoped search that omits the cited file
    cannot falsify a claim about that file.
- **C-003** — the six tests take their report from `_push_json`, never from a
  hand-rolled `ocx.run(...)`, and each asserts on `platform_digests`.

## User-experience scenarios (revised after spec review)

The spec reviewer rejected the first draft: under
`#[serde(skip_serializing_if = "BTreeMap::is_empty")]` the mutation makes
`platform_digests` **absent**, not empty — so a `.get(k, {})` default or a
loop over its values greens vacuously. Three of the six original scenarios
would have stayed green under the very mutation the plan prescribed. Every
scenario below therefore **indexes `report["platform_digests"]` directly** and
compares against a value fetched from the registry, never against another
report.

| ID | Test | Subject | Reds under the mutation because |
|---|---|---|---|
| S-101 | `..._json_schema_has_required_fields` | closed key set `_REPORT_KEYS` + anchored `platform_digests` | the key set no longer matches |
| S-102 | `..._cascade_tags_written_is_array` | `cascade_tags_written` equals the rolling tags the registry actually holds + anchored `platform_digests` | `KeyError` |
| S-103 | `..._platform_digest_is_no_tag_index` (renamed) | reported platform digest is the index digest of **no** tag this push wrote; `status == "pushed"` | `KeyError` |
| S-104 | `..._non_cascade_has_empty_cascade_tags` | `cascade_tags_written == []` **and** `keep_tags_written` non-empty **and** `platform_digests` populated, in one push | `KeyError` |
| S-105 | `..._repush_reports_the_same_platform_digest` (renamed) | same bundle pushed twice; both sides anchored to the registry, not to each other | `KeyError` |
| S-106 | `..._cascade_with_no_keep_tag_still_reports_platform_digests` (renamed) | `--cascade` **and** `--no-keep-tag` together: rolling tags written, keep tags empty, digests full | `KeyError` |

Findings taken from the review, all Block:

- **S-103 asserted only `status`** — could not red, and violated "each asserts
  on `platform_digests`". Re-subjected to the tag-index exclusion, which
  `S-001` covers only for the primary tag, never the rolling ones.
- **S-105 compared two reports to each other** — symmetric, so no uniform
  producer defect can red it. Both sides now anchored to
  `fetch_platform_manifest_digest`.
- **S-106 iterated a possibly-empty map** and regex-checked a value that
  `oci::Digest` already parse-validates: vacuous *and* tautological.
  Re-subjected to the `--cascade` + `--no-keep-tag` combination, which nothing
  covered.
- **S-101 membership → closed key set**: a key silently *added* to a document
  `ocx-mirror` parses is as much a contract break as one removed.
- **S-102 leak claim was unfalsifiable** in a one-platform fixture; replaced
  with a comparison against the registry's own tag list.

Three renames are deliberate. `test_push_report_status_snake_case` and
`test_push_report_skipped_existing_status_on_repush` both named a
`skipped_existing` status the product does not have and never had —
`status` is the constant `"pushed"`
(`crates/ocx_cli/src/api/data/push.rs`), so those names promised coverage that
could not exist. `test_push_report_manifest_digest_sha256_format` is renamed
for the re-subject above.

## Executable phases

**Stub** — make `_push_json` create-once and move it above the first test so
every test in the module can call it (C-001/C-002). `S-001`/`S-002`/`S-003`
call it unchanged and must stay green.

**Specify / Implement** — rewrite the six test bodies against S-101..S-106.
Delete every `if result.returncode != 0: pytest.skip(...)`. Rename
`test_push_report_skipped_existing_status_on_repush` → it asserts a status the
product does not have; the truthful subject is S-105. Drop the now-dead
`subprocess` import and the `published_package` usage (used nowhere else in
this file). Rewrite the stale module docstring and the
"§3.2 Unit-level … (no registry required)" section header — both document the
contract that justified the skips, and both are now false.

**Verify** — red/green per test, then `task verify --force`.

**Review** — `/hex-review high` scoped to this branch diff.

## Red/green proof obligation

A green here is only evidence if red was reachable, and the restore must be
proven to have landed in the file *and* in what pytest executed
(`__pycache__` staleness). Mutation target: the **consumer** side —
`PushReport::from_outcome` in `crates/ocx_cli/src/api/data/push.rs` — populating
`platform_digests` from an empty map. Mutating the producer risks tripping
`-D dead-code` and killing the build instead of the test. Every one of the six
must red on that mutation; a test that stays green under it is not asserting on
`platform_digests` and must be strengthened.

## Parallelization

| WP | Scope | Files | Size | Wave | Depends on | Review | Status |
|---|---|---|---|---|---|---|---|
| WP-1 | C-001..C-003, S-101..S-106, docstring/header repair | `test/tests/test_package_push.py` | S | 1 | — | panel | pending |

Single work package: one file, ~90 lines, and the helper extraction and the six
rewrites are the same edit — splitting them would need a contract-first stub
between two agents touching the same file for less work than the handover
costs. Shippable after wave 1. Critical path: WP-1.

## Open questions

None. Resolved during discovery:

- *Reuse `make_package` instead of `_push_json`?* No — `make_package`
  (`test/src/helpers.py:392`) pushes through `ocx.plain` and discards the
  report. `_push_json` is the only create-push-return-report helper in the
  tree; extending it is rung 2 of the ladder.
- *Keep `published_package`?* No — the fixture pushes through `make_package`
  and hands back no report, which is what pushed the original author into
  hand-rolling the broken invocation. It stays in `conftest.py` for other
  modules.
- *Is `status` worth asserting at all if it is a constant?* Yes — it is the
  field `ocx-mirror pipeline push` keys its go/no-go off
  (`crates/ocx_cli/src/api/data/push.rs` doc), so its exact spelling is a wire
  contract.

## Constitution check

`arch-principles.md` — no production change planned beyond what the tests may
expose. Diff is test-only unless the rewritten assertions surface a real
`PushReport` defect, in which case that fix is the root cause and lands here
too.
