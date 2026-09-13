# Plan: CI-integration branch — review fixes (#449–#453)

## Status

- **Plan:** plan_ci_integration_review_fixes
- **State:** review
- **Tier:** xhigh
- **Active phase:** done — all WPs merged; `L2` + two Codex passes on the redux closed
- **Step:** All eight packages, the WP1 redux, its post-loop sweep, and the three owner items merged; `task verify --force` green at the final tree (`5a457966`: unit 7887/7887, acceptance 3373 passed, reds environmental and re-run green); both MSVC targets type-check including tests. Nothing open. Next: `/hex-review` on the whole branch.
- **Feature branch:** `sion` @ `77412e06` (75 commits on top of `731133d9`; history rewritten once for the `5558e561` reword — now `de851b19` — backup at `backup/sion-pre-reword-20260913`; **not pushed**)
- **Last update:** 2026-09-13
- **Next:** `/hex-review .claude/artifacts/plan_ci_integration_review_fixes.md`

---

## Overview

`/hex-review` (xhigh, 8 seats + Codex) on the branch implementing
[#449](https://github.com/ocx-sh/ocx/issues/449),
[#450](https://github.com/ocx-sh/ocx/issues/450),
[#451](https://github.com/ocx-sh/ocx/issues/451),
[#452](https://github.com/ocx-sh/ocx/issues/452),
[#453](https://github.com/ocx-sh/ocx/issues/453) returned **Request Changes**: two
Block findings (both reproduced against the real binary), ~10 High, ~20 Warn. Both Blocks
are pre-existing defects the branch newly exposes to publisher- and registry-supplied input.

The owner settled every open decision on 2026-09-12. This plan is the executable list.
One `/hex-execute` pass over everything, then one re-review.

## Decisions (owner, 2026-09-12 — not open for re-litigation)

| # | Decision |
|---|---|
| D1 | Traversal fix **and** the tar mode cap land **on this branch**, one commit. |
| D2 | **No advisory, no patch release.** Fix ships with the next minor as a normal `fix:` line. |
| D3 | bzip2: **add a pure-Rust decoder** (no `libbz2` — `linux_self_contained.rs:45` bans it). |
| D4 | `--default-variant <NAME>` → **`--default`** (bool). Pushed tag's own variant becomes the default. Tag without a variant + `--default` = exit 64. No-op warning disappears. |
| D5 | **Amend `adr_variants.md` in place**; add it to `arch-principles.md`'s ADR index. |
| D6 | **Delete `PushOutcome::with_aliases`**; fold `aliases` into `new()`. ocx-mirror takes the one-line break. |
| D7 | zip **refuses** a traversal entry (typed escape error, exit 65) exactly like tar. |
| D8 | Mask **`0o7000 \| 0o022`** in both tar and zip extraction. |
| D9 | `--extract` scratch under **`$OCX_HOME/temp`**, not `tempfile::tempdir()`. |
| D10 | Parse `CI_PIPELINE_CREATED_AT` as RFC 3339, warn + fall back like the epoch branch; **`SOURCE_DATE_EPOCH` keeps winning**. |
| D11 | `/hex-execute` takes **everything below in one pass**. |

## Block

- **B1 — `crates/ocx_lib/src/archive/tar.rs:185-233` — arbitrary file write outside the extraction root.**
  `escapes_root(&stripped)` at `:191` checks the *lexically normalized* path; `output.join(&stripped)` at `:195` keeps the raw `..`; `entry.unpack` at `:233` is tar-rs's non-validating variant (`unpack_in` is the validating one). A symlink entry earlier in the archive (`:207-211`, `validate_target` is lexical too) makes the kernel resolve the chain physically. Reproduced via `--extract` and via local materialization (`ocx package test` → `pull_layer` → `extract_tar_from_reader` → the same `extract_from_archive`): `$OCX_HOME/PWNED-ON-INSTALL`, exit 0.
  **Fix:** join the normalized path (`utility::fs::path::join_under_root`), then re-verify the resolved parent with `dunce::canonicalize(parent).starts_with(canonicalize(output))` before every write — the check `resolve_hard_link_source` (`tar.rs:265-278`) already performs for hard links, applied to the regular-file and symlink branches. Same commit: D8 mode mask (`tar.rs:155` `set_preserve_permissions(true)` applies `0o7777`; `zip.rs:292` masks only `0o022`) and D7 (`zip.rs:236-238` `enclosed_name() == None` → `Error::EntryEscape`, not `continue`).
  **Tests (must red before the fix):** the two-entry symlink+traversal archive through `--extract` AND through `pull_layer`; a `0o4755` entry lands `0o755`; zip `../evil` → exit 65.

- **B2 — `crates/ocx_cli/src/command/package_create.rs:90` + `command-line.md:3542` — bzip2 claimed, not implemented.**
  `CompressionAlgorithm` has no bzip2 variant; `.tar.bz2` falls through to raw `File::open` → `tar error: failed to read entire block`. **Fix (D3):** pure-Rust bzip2 decoder, `CompressionAlgorithm::Bzip2` + `from_file` arm for `bz2`/`tbz2`, `cargo deny check licenses`, acceptance test through `--extract` (helpers.py already builds `w:bz2`).

## High

- H1 `.claude/artifacts/adr_variants.md:308-318,411,465-470` — amend per D5: mechanism superseded by `merge_platform_into_index`; `--default` is the channel the mirror's `default: true` flows through; mirror passes it. Add to `arch-principles.md:84-125` index.
- H2 `crates/ocx_cli/src/command/package_push.rs` + `crates/ocx_lib/src/{publisher.rs,package/cascade.rs}` — rename per D4 (`--default`, `Option<&str>` → `bool` through `push`/`push_cascade`/`write_default_variant_aliases`/`managed_config/publish.rs`); delete the no-op warning at `package_push.rs:433-445`; refuse `--default` on a variant-less tag (exit 64). Update help, `command-line.md`, `building-pushing.md`, `subsystem-cli-commands.md`, tests.
- H3 `crates/ocx_lib/src/publisher.rs:214-231` — `--default` without `--cascade` is untested; deleting the block ships green. Add a `#[tokio::test]` mirroring `cascade.rs::default_variant_aliases_the_bare_track_with_one_manifest_upload`.
- H4 `test/tests/test_package_cascade.py` — add the tombstone: `cascade repair --announce-tags <path> <repo>` exits 64, writes nothing (plan requirement dropped in WP-E; re-adding a clap alias reds nothing today).
- H5 `test/tests/test_package_test_script.py:1417-1439` — add `assert "premidpost<&>" in text` so the illegal-XML test observes that the fixture emitted the bytes.
- H6 `crates/ocx_cli/src/command/package_test.rs:115` — `--junit` alone renders an unsatisfiable usage error demanding both `--script` and `<COMMAND>`. Drop `requires = "script"`, keep `conflicts_with = "command"`, refuse in `execute` with a `UsageError` naming both flags (the `validate_bin_scan` shape).
- H7 `package_test.rs:102`, `testing.md`, `command-line.md` `--junit` row — "written on every exit path" is false (missing sidecar / missing layer / no platform exit 74 with no file). Reword: "written whenever the scripted run is reached, including a red run and an unreadable `--script` path; a failure resolving the package writes none."
- H8 `crates/ocx_cli/src/command/package_create.rs:293-322` — `--strip-components 1 <directory>` fails with `tar error: Is a directory (os error 21): Is a directory (os error 21)`. Refuse a directory up front (`UsageError`, exit 64, naming the flag and path); map an unrecognised suffix to `UnsupportedFormat` instead of falling through to tar.
- H9 `crates/ocx_cli/src/api/junit.rs:161-172` — `description()` appends the full stdout/stderr capture again on top of `<system-out>`/`<system-err>` (~40 MiB of `String` at the cap). Drop the append; the body carries `detail` alone. If a self-contained body is wanted, bound it (last N KiB).
- H10 `website/src/docs/authoring/testing.md:329-331` + `junit.rs:19-21` — the GitLab merge claim is false: GitLab keys a suite on the job name (`Ci::Build#test_suite_name`), never `<testsuite name>`; the example glob runs inside one matrix leg and matches one file. Rewrite: each matrix leg is its own GitLab suite; the naming scheme pays off for GitHub reporters that glob downloaded artifacts; drop the intra-leg glob.
- H11 `crates/ocx_lib/src/ci/annotations.rs:84` — D10: `DateTime::parse_from_rfc3339`, re-emit via `bundle_created`, warn + fall back on garbage.
- H12 `website/src/docs/reference/environment.md:963-994` — seven new runner-variable reads undocumented (`GITHUB_SERVER_URL`, `GITHUB_REPOSITORY`, `GITHUB_SHA`, `CI_PROJECT_URL`, `CI_COMMIT_SHA`, `CI_PIPELINE_CREATED_AT`, `SOURCE_DATE_EPOCH`); `:965` scoping sentence and the `GITHUB_ACTIONS`/`GITLAB_CI` subsections (`:980`, `:994`) must name `package push --ci-annotations`.
- H13 `website/src/docs/reference/command-line.md:3802-3810` — `assertion.location` missing from the reference JSON envelope sample; `:3810`'s "stable v1 contract" sentence must acknowledge the additive field.
- H14 `command-line.md` `push` section — no `--format json` key list; add one covering `annotations_written`, `aliases_written` and the existing set.
- H15 `command-line.md:3274` — exit-74 row says "reading `--tags-file`"; repair only writes it. Also confirm the symlink-refusal clause applies to `tokio::fs::write` at all.
- H16 `command-line.md` `--ci-annotations` row + `building-pushing.md:130` — `.version` strips the variant prefix and a non-version tag writes no key; both facts are in `--help`, neither in docs.
- H17 `website/src/docs/authoring/bundle-anatomy.md:84-94`, `migration.md:88-100` — `--extract` has no narrative home; `bundle-anatomy.md#strip-components` documents only the metadata `strip_components` (two mechanisms, build-time vs install-time, no guidance); `migration.md#github-releases` is literally the use case.
- H18 `test/tests/test_package_create_extract.py` — containment tests at the new trust boundary: tar `../evil` → 65; zip `../evil` → 65 (D7). Extend `build_archive` with a traversal entry name.

## Warn

- W1 `crates/ocx_lib/src/publisher.rs:123-133` — delete `with_aliases` (D6); fold into `new()`; update the three test-fixture callers in `api/data/push.rs`.
- W2 `crates/ocx_lib/src/archive/error.rs:46,69` vs `package_create.rs:309-321` — `EmptyExtraction` is a lib variant only the CLI constructs, so `pull_local.rs:508` never enforces it. Push the refusal into `extract_with_options` (an `ExtractOptions` field or an entry count), or move the variant to the CLI.
- W3 `crates/ocx_cli/src/api/junit.rs:6-11` — module doc borrows the `export_ci` exception, which licenses a different thing. Reword: placement is deliberate because `quick-junit` would otherwise land on `ocx_lib` (the crate slated to split); add a `junit` row to `subsystem-cli-api.md`'s exception list.
- W4 `package_test.rs:344-362` — the `--script -` I/O-fault JUnit arm is untested (`report_argv_fault(…, ScriptStatus::Io, "io", …)` at `:347`); add the case: rc 74, file exists, `error/@type == "io"`.
- W5 `test/tests/test_exit_codes.py:540-560` — add `_package_test_junit` → 74 against `_unwritable(tmp_path, "junit.xml")`.
- W6 `crates/ocx_lib/src/ci/annotations.rs:55-59` — GHES trailing-slash trim and partial-variable (`GITHUB_SERVER_URL` set, `GITHUB_REPOSITORY` unset) both untested.
- W7 `package_push.rs:396-399` + `test_package_push_annotations.py` — a push to `:nightly` under `--ci-annotations=gitlab` writes no `.version`, `SOURCE` still present (untested); also a receipt-driven push with `-i` omitted asserting `.version` equals the receipt's tag (the whole reason #450 earns the flag).
- W8 `publisher.rs:227-230,314-318` — multi-platform alias dedup (`if !aliases_written.contains`) untested; extend the two-platform fan-out test at `publisher.rs:529` with `--default`.
- W9 `package_test.rs:102` — `--junit`'s first help paragraph is ~330 chars, so it is the short help. First line "Write a JUnit XML report for the scripted run to PATH", blank line, rest.
- W10 `crates/ocx_lib/src/archive/error.rs:38-46` — `EmptyExtraction` renders `with --strip-components 0` when the operator never typed it. Carry `Option<usize>`; strip clause only when non-zero.
- W11 `package_create.rs:396-404` — `infer_filename` uses `file_prefix()`: `hello-1.2.3.tar.gz` → `hello-1.tar.xz`. Under `--extract`, strip the recognised archive suffix as a unit.
- W12 `crates/ocx_cli/src/api/data/package_cascade_repair.rs:33` — per-entry `announce_tags` beside top-level `tags_file` is two vocabularies in one document; rename the entry field (`tags`) and the pinned key-set assertion.
- W13 `crates/ocx_cli/src/conventions.rs:352-358` — `--ci-annotations github` (space) sends `github` to `LAYERS` and reports "could not autodetect"; append "; the value must be attached with `=`" (shared with `env --ci`).
- W14 `building-pushing.md:133` + `command-line.md` — "unset or blank writes no annotation" is false for `created` (always stamped). Scope the sentence to `source`/`revision`/`version`.
- W15 `testing.md:304-306` — "attached on every run" → "on every run that produced captured output" (`junit.rs:74-77` gates on `report.run.is_some()`).
- W16 `testing.md:286-333` — zero external hyperlinks (JUnit XML, GitLab, GitHub reporters); add link defs at `:357-380`.
- W17 `command-line.md:3239` — garbled clause "the [`--tags-file`] `announce` reads takes"; → "the same comma/newline format `announce --tags-file` reads".
- W18 `command-line.md:3749`, `testing.md:299-303` — `--script -` emits no `file`/`line`; both passages promise them unconditionally.
- W19 `junit.rs:64-66` — GitLab renders `file` as a link; there is no re-run affordance. Drop the clause.
- W20 `tar.rs:182-241`, `zip.rs:234-307` via `package_create.rs:300` — no entry-count or output-size cap under `--extract`. Reuse the layer-pull `Read::take(cap_with_probe)` pattern from `oci/client.rs`; new `archive::Error` variant → 65.
- W21 `oci/referrer/manifest.rs:141-152` vs `ci/annotations.rs:91-96` — `SOURCE_DATE_EPOCH=""` warns on the sign path and is silent on the annotate path under a doc comment claiming one parser decides. Shared `pinned_instant() -> Option<DateTime<Utc>>`.
- W22 `package_create.rs:295` — D9: scratch under `$OCX_HOME/temp` (a `create/` subdir beside the digest-keyed `TempStore`).
- W23 `test/pyproject.toml:8` — `.tar.zst` rows `importorskip("compression.zstd")` and skip on 3.13; raise `requires-python` to `>=3.14` or `pytest.fail`.
- W24 `test_package_create_extract.py` — `--extract` on an unsupported suffix → 65 untested (reclassify `UnsupportedFormat` and nothing reds).

## Suggest (apply where one line, else skip)

`junit.rs:23-28` note that `XmlString` also strips ANSI · `--junit` help states its own exit codes (64/74) · dry-run + `--tags-file` prints no hint naming the file · `zip.rs:240-243` partially-emptied strip silently drops entries (test the observed contract) · `package_push.rs:481-528` inline-signing fold could be `fn signature_rows` (mechanical; skip).

## Deferred — decided or left alone

- **`--junit` follows symlinks while `--output` refuses them, on the same command.** `api/junit.rs:107-114`
  uses `create_dir_all` + `tokio::fs::write`; `package_test.rs:216` walks ancestors via
  `refuse_if_symlink_in_path`. Defensible: `--output` guards a hardlink-assembled package tree that must share
  a filesystem with `$OCX_HOME/layers`, while `--junit` writes one XML file. Deliberate, and the branch's docs
  describe the resulting three-tier policy accurately — `--tags-file`/`--key`/`--readme`/`--descriptor`/`--junit`
  follow symlinks, `--identity-token-file`/`--predicate` refuse via `O_NOFOLLOW`, `--output` refuses via the
  ancestor walk. Left as-is; recorded so the asymmetry is a decision rather than an accident.
- **`ocx-mirror`'s submodule bump needs sequencing** (owner's call). `publisher.rs:107`/`:170`/`:255`
  (`PushOutcome::new`, `push`, `push_cascade`) each gained a parameter and the mirror links `ocx_lib`.
  `adr_variants.md:473` records the break with its migration direction — the mirror passes
  `ocx package push --default` and does not implement the aliasing itself. The parameter cannot silently
  mis-bind (a `Vec<String>` between `Vec<(Platform, Digest)>` and `LayerCounts` fails to type-check in the
  wrong slot), and `ocx_lib` carries no stability per CLAUDE.md.
- **Nothing verifies an in-repo anchor referenced from Rust help text.** Pre-existing and branch-wide: ~10
  help strings link `https://ocx.sh/docs/<section>/<page>#<anchor>` and `lychee --offline` sees none of them.
  Worth an issue, not a merge gate.

- **One root-owned leftover needs `sudo` to clear**: `.agents/worktrees/wp5-junit/test/zot-config.json` is a
  *directory* created as a bind-mount by the zot test-registry container, so the orphaned worktree directory
  cannot be removed as the invoking user. The git worktree itself is deregistered and the branch deleted; the
  path is gitignored, so it affects nothing on the branch. Clear with
  `sudo rm -rf .agents/worktrees/wp5-junit`.

- **`--help`'s deep link to `testing.md#scripted-tests-junit-per-platform` is unguarded.** `lychee` runs
  `--offline`, so renaming that anchor silently breaks the link inside shipped `--help` text. Whoever edits
  `website/src/docs/authoring/testing.md` needs to know the anchor is load-bearing outside the website.

- **An unknown `--ci`/`--ci-annotations` token is still absorbed** (`--ci-annotations jenkins` →
  `'jenkins-metadata.json'`). Closing it needs a different heuristic — refuse a bare flag whose next
  positional carries no `/` and no extension — which buys a much larger false-positive surface. Not worth
  the trade.
- **Nothing pins the full `undetectable_ci_provider` string**; the "byte-unchanged" claim is reading-derived
  (the old `\` continuation stripped the next line's leading whitespace). A future edit to the shared
  `ci_value_needs_equals` clause could silently reword the autodetect-failure message.

- **`cargo deny check bans` is enforced by nothing** — `deny.toml`'s `[bans]` section contradicts its own
  comment, and `taskfiles/rust.taskfile.yml:200` runs `check licenses` only. Wiring it in may red on
  pre-existing entries, so it is a follow-up, not this branch's work.
- **`EXTRACTION_CAP_MULTIPLIER = 100`** (WP1's constant) now also governs bzip2, whose real-world ratios
  exceed 100x far more often than gzip's: a `.tar.bz2` over 2.56 MiB compressed that expands past 100x is
  refused. WP2 widened the blast radius without touching the constant. Revisit if a user reports it.

- Alias-write read-modify-write without CAS (`oci/client.rs:674-753`) — pre-existing on every multi-platform push; `cascade repair` detects the race, push aliasing does not. **Leave; note.**
- Bare-track blocker manifests re-fetched once per platform (`cascade.rs:424-427`) — amplifies a decided pattern. **Leave.**
- Alias index writes sequential, most-specific-first — matches `push_manifest_and_merge_tags`. **Leave.**
- `package_push.rs::execute` at 280 lines — linear pipeline whose ordering is the contract. **Leave.**
- Whether any ocx-mirror pipeline reads `announce_tags_path` — check when the mirror bumps its submodule.
- `test_package_push_mount::test_package_push_mount_cross_repository_reuse` — red on `main` too; not this branch's.

## Parallelization

Derived by `/hex-execute` on 2026-09-12 — the review plan carried no table. File-disjoint by
construction; the three shared doc files and the two shared rule files belong to WP6 alone.
WP4 has no dependency and is held to wave 2 only for the build-slot RAM cap (three concurrent
cargo builds). Findings are referenced by their IDs above.

| WP | Scope | Expected Files | Size | Wave | Depends on | Review | Verify | Status |
|---|---|---|---|---|---|---|---|---|
| WP1 `archive-containment` | B1 (D1/D7/D8), W20, W2, W10, W11, W22 (D9), H8, H18, W24, Suggest `zip.rs:240-243` | `crates/ocx_lib/src/archive/{tar.rs,zip.rs,error.rs}`, `crates/ocx_lib/src/archive.rs`, `crates/ocx_lib/src/utility/fs/path.rs`, `crates/ocx_cli/src/command/package_create.rs`, `crates/ocx_lib/src/package_manager/pull_local.rs`, `test/tests/test_package_create_extract.py`, `test/src/helpers.py`, new `test/tests/test_archive_containment.py` | L | 1 | — | risk | scoped | merged |
| WP2 `bzip2` | B2 (D3), W23 | `crates/ocx_lib/src/compression.rs`, `crates/ocx_lib/Cargo.toml`, `Cargo.toml`, `Cargo.lock`, `deny.toml`, `test/tests/test_package_create_extract.py`, `test/pyproject.toml`, `test/uv.lock` | M | 2 | WP1 | — | scoped | merged |
| WP3 `push-default-annotations` | H2 (D4), H3, W1 (D6), W8, H11 (D10), W6, W7, W13, W21 | `crates/ocx_cli/src/command/package_push.rs`, `crates/ocx_lib/src/publisher.rs`, `crates/ocx_lib/src/package/cascade.rs`, `crates/ocx_lib/src/managed_config/publish.rs`, `crates/ocx_cli/src/api/data/push.rs`, `crates/ocx_lib/src/ci/annotations.rs`, `crates/ocx_cli/src/conventions.rs`, `crates/ocx_lib/src/oci/referrer/manifest.rs`, `test/tests/test_cascade.py`, `test/tests/test_package_push_annotations.py` | L | 1 | — | — | scoped | merged |
| WP4 `cascade-report` | W12, H4, Suggest dry-run hint | `crates/ocx_cli/src/api/data/package_cascade_repair.rs`, `crates/ocx_cli/src/command/package_cascade_repair.rs`, `test/tests/test_package_cascade.py` | S | 2 | — | — | scoped | merged |
| WP5 `junit` | H5, H6, H7 (code), H9, H10 (code), W3 (code), W4, W5, W9, W19, Suggest ANSI note + help exit codes | `crates/ocx_cli/src/command/package_test.rs`, `crates/ocx_cli/src/api/junit.rs`, `test/tests/test_package_test_script.py`, `test/tests/test_exit_codes.py` | M | 1 | — | — | scoped | merged |
| WP6 `docs` | H1 (D5), H2/H7/H10/W3 doc halves, H12–H17, W14–W18, B2 doc check | `website/src/docs/reference/{command-line.md,environment.md}`, `website/src/docs/authoring/{testing.md,building-pushing.md,bundle-anatomy.md,migration.md}`, `website/src/docs/in-depth/versioning.md`, `.claude/artifacts/adr_variants.md`, `.claude/rules/{arch-principles.md,subsystem-cli-commands.md,subsystem-cli-api.md,subsystem-cli.md}` | L | 3 | WP1, WP2, WP3, WP4, WP5 | risk | scoped | merged |
| WP7 `ci-flag-guard` | A space-separated `--ci-annotations` value is absorbed as a layer path (found by WP6's `L1`) | `crates/ocx_cli/src/command/package_push.rs`, `crates/ocx_cli/src/conventions.rs`, `crates/ocx_cli/src/command/toolchain_env.rs`, `test/tests/test_package_push_annotations.py` | S | 4 | WP3 | risk | scoped | merged |
| WP8 `aggregate-fixes` | `--junit` exit-code contract (docs + error precedence), the undocumented `ExtractionCapExceeded` and `--extract`-on-a-directory refusals, the unrecorded guard convention, and the `--build-timestamp` guard — all found by the run-level `L2` | `crates/ocx_cli/src/command/{package_test.rs,script_runner.rs,package_push.rs}`, `crates/ocx_cli/src/conventions.rs`, `website/src/docs/reference/command-line.md`, `.claude/rules/{subsystem-cli.md}`, `.claude/rules.md`, `test/tests/{test_package_test_script.py,test_exit_codes.py,test_package_push_annotations.py}` | M | 5 | WP1..WP7 | risk | scoped | merged |

- Verify-default: scoped
- Feature branch: `sion` — frozen base `425961ce`.

## Schedule log

- WP8 `aggregate-fixes` → `sion` @ `49b0b441` — trigger `merge`, gate `scoped` (clippy clean + 7867 Rust tests
  passed). Six items: the `--junit` exit-code contract (docs corrected, exit 74 kept per the
  operator-supplied-path convention), the masking fix at **all three** `report_argv_fault` call sites, the two
  undocumented `--extract` refusals, the unrecorded guard convention, the `--build-timestamp` guard, and two
  traversal rows now owned by the WP1 redux. Acceptance rows run on the merged tip: 81 passed / 1 xfailed over
  `test_package_push_annotations.py test_package_test_script.py test_exit_codes.py`, with
  `test_a_spaced_build_timestamp_value_is_a_usage_error`,
  `test_an_attached_build_timestamp_beside_a_same_named_layer_is_accepted` (the control proving the guard does
  **not** refuse `--build-timestamp=date none`) and `test_junit_write_failure_does_not_displace_an_unreadable_script`
  confirmed by name.
- WP7 follow-up → `sion` @ `f1b82f50` — trigger `merge`, gate `scoped` (clippy clean + 7862 Rust tests
  passed; prose-only, no behaviour change). **Orchestrator error**: WP7's worktree was removed while it was
  still mid-pass on the `--junit` help nits, destroying four uncommitted edits. It redid them on
  `hex/ci-help-followup--wp7` (`324c7157`) rather than amending a commit already merged to `sion` — the right
  call. Lesson for the run: do not merge and remove a worktree while its builder has work dispatched.
- WP7 `ci-flag-guard` → `sion` @ `7a6f9c9d` — trigger `merge`, gate `scoped` (clippy clean + 23 passed over
  `tests/test_ci_export.py tests/test_package_push_annotations.py`, both spaced-value cases confirmed by
  name). `L1` returned 9 findings, no blocker; fixed in `3efe4289`. **The env row was proven to discriminate
  by the orchestrator after the merge**: mutating the call site to `let _ = self.refuse_spaced_ci();` (keeping
  the method live) reds it `assert 79 == 64` with `gitlab` absorbed as a package name — `failed to find
  package: localhost:5000/gitlab` — and restoring returns it green. `env.rs` restored byte-identically
  (`cmp`), no residue.
- WP6 `docs` → `sion` @ `51db215a` — trigger `merge`, gate `scoped` (structural tests 51 passed / 3 skipped,
  lychee 2797 links / 0 errors, VitePress build clean with its strict dead-link check). `L1` (raised from
  `L0` by the `risk` cell) verified every claim against source and found **three actionable falsehoods**,
  including the H10 GitLab claim returning in softer form in the `--junit` row — it asserted the merge while
  linking to its own refutation. Fixed across `d3335f43`, `bcb1740b`, `f8cedd81`.
- WP2 `bzip2` → `sion` @ `120f25a5` — trigger `merge`, gate `scoped` (clippy clean + 22 passed over
  `tests/test_package_create_extract.py tests/test_archive_containment.py`; the three bzip2 cases
  confirmed by name). `L1` round 1 returned 4 actionable / 0 Block; fixed in `e183181d`.
- WP1 `archive-containment` → `sion` @ `f9f4a1f4` — trigger `merge`, gate `scoped` (clippy clean + 19 passed
  over `tests/test_archive_containment.py tests/test_package_create_extract.py`, all cases listed by name).
  `L1` took **3 rounds** (authorised: round 1 Fail with a reachable out-of-root write; round 2 fixed it but
  regressed the tar directory mode cap on install paths; round 3 moved the predicate to the longest existing
  prefix and keyed the mode cap on on-disk type). Final commits `a33cf762`, `dc2be475`, `40215c91`,
  `551767e9`, `a0aff54a`. Reviewer verdict on the merged state: no reachable out-of-root write.
- WP3 `push-default-annotations` → `sion` @ `2eb93c34` — trigger `merge`, gate `scoped` (clippy clean +
  28 passed over `tests/test_cascade.py tests/test_package_push_annotations.py`; the 7 `--default`
  and CI-version cases confirmed by name). `L1` round 1 returned 3 actionable + 3 trivia; fixed in
  `761696ac`/`69e6b832`/`a6c714dd` (finding 1 took the real-counter option).
- WP4 `cascade-report` → `sion` @ `97994b00` — trigger `merge`, gate `scoped` (clippy clean + 15 passed over
  `tests/test_package_cascade.py`; `j14`/`j8`/`j12` confirmed by name). `L1` round 1 returned Pass with one
  trivia doc contradiction, fixed in `7d8f1fc3`.
- WP5 `junit` → `sion` @ `5abef896` — trigger `merge`, gate `scoped` (clippy clean + 68 passed / 1 xfailed over
  `tests/test_package_test_script.py tests/test_exit_codes.py`; the 11 `-k junit` cases confirmed by name).
  `L1` round 1 returned 4 actionable + 2 trivia, all fixed in `e794677b`/`2b1fe33a`.

### File-set extensions (justified, per the merge-time re-validation rule)

- **WP4** additionally edits `crates/ocx_cli/src/conventions.rs:911` — one field name in a
  `#[cfg(test)]` `RepairEntry` helper, required to compile after the `announce_tags` → `tags`
  rename. The file is WP3's; the two edits sit in different regions (WP3 works around `:759-770`).
  WP4 merged first (it was ready while WP3 was still in its fix pass), so WP3 carries any
  textual reconciliation; the hunks do not overlap.
- **WP3** additionally edits `crates/ocx_lib/src/oci/client/test_transport.rs` — `tag_manifest_writes`
  and `digest_manifest_writes` counters, authorised in its `L1` fix pass so the "never writes twice"
  assertions can witness a duplicate write (the stub's `HashMap<String, …>` made an identical
  re-write invisible). Merged; existing helper semantics unchanged.

- **WP2** additionally edits `crates/ocx_lib/src/compression/error.rs` (the new `DecodeOnly` variant),
  `crates/ocx_lib/src/oci/client.rs` (an exhaustiveness arm refusing a bzip2 layer — unreachable today
  because `from_media_type` has no bzip2 arm, and commented as such), `test/src/helpers.py`,
  `about.toml` and `LICENSE-THIRD-PARTY.md` (license bookkeeping for the new crate). All compile-required
  or generated; none belongs to another WP.

- **WP6** additionally edits one row of `.claude/rules/product-tech-strategy.md` (the acceptance suite's
  Python floor, 3.13 → 3.14), because `test/pyproject.toml` cites that file as the floor's single source
  of truth and WP2 raised the floor.

### D3 had no red-able guard — WP2 `L1`'s sharpest finding

`crates/ocx_cli/tests/linux_self_contained.rs:45` bans a **dynamic** `libbz2` NEEDED entry, but
`bzip2-sys` vendors C libbz2 and static-links it via `cc`, emitting no NEEDED entry — so it passes that
test identically. `ldd` output and "0 dynamic `BZ2_*` imports" therefore never established "no libbz2";
the lockfile is the decisive evidence, not the binary.

The graph carries a live footgun: **`zip` 8.6, already a dependency, has a feature named `bzip2-rs`
that maps to `bzip2/bzip2-sys`** (`zip-8.6.0/Cargo.toml:78-82`) — the C path, under the name a reader
would assume means the Rust one. Enabling it silently unifies `ocx_lib`'s `bzip2` onto the C backend on a
green `linux_self_contained`. Closed with a lockfile test asserting `bzip2-sys` absent (`compression.rs:464`), with
`libbz2-rs-sys` present as a positive control so a moved or emptied lockfile cannot pass it — proven red
by enabling `zip/bzip2-rs` and regenerating the lock. WP2 then **measured** the claim with the C backend
live: `linux_self_contained` PASSED, `ldd | grep bz2` found nothing, and the ELF carried statically linked
C libbz2. It also goes one step past the review: `BZ2_bz*` symbol names do not discriminate either, since
`libbz2-rs-sys` exports the same C ABI — of the original D3 evidence only the `libbz2_rs_sys`-prefixed
symbol count carried information. A `deny.toml [bans]` entry was explicitly **rejected as the guard**: nothing runs
`cargo deny check bans` (`taskfiles/rust.taskfile.yml:200` runs `check licenses` only), so it would be an
unchecked green.

### WP2 dependency decision (recorded — the rejected option is the interesting half)

`bzip2` 0.6.1 (MIT OR Apache-2.0), default backend, adding exactly two crates: itself and
`libbz2-rs-sys` 0.2.5 (licence `bzip2-1.0.6`, zlib-style; allowlisted in `deny.toml`). Neither has a
build script; the backend is pinned `default-features = false, features = ["rust-allocator"]`, so the
C path (`bzip2-sys`) stays unselected. D3 verified directly: `linux_self_contained` passes, `ldd` lists
only `libgcc_s`/`libm`/`libc`, dynamic `BZ2_*` imports are 0 of 229, and 57 `libbz2_rs_sys` symbols are
statically linked.

**`bzip2-rs` 0.1.2 was rejected despite needing no licence change and adding one crate**: its
`DecoderReader` never resets on a stream footer, so a pbzip2/lbzip2 **multi-stream** tarball decodes to
its first stream and reports a clean EOF — silent truncation, on exactly the real-world-tarball class
WP1 had just fixed. Last released 2022. WP2 uses `MultiBzDecoder`, inside the `take(cap+1)` reader.

**Encode is an explicit typed refusal**, not a half-feature: no OCI layer media type spells bzip2, so a
bzip2 bundle could never be pushed. `write_file` refuses `Bzip2` with `DecodeOnly` → exit 65, **before**
the output file is opened and truncated. No `from_media_type` arm and no `MEDIA_TYPE_TAR_BZ2` — this adds
no wire format.

**Correction to B2 as filed**: the symptom is no longer `tar error: failed to read entire block`. WP1's
suffix gate now catches `.tar.bz2` first (`unsupported archive format`, exit 65) while `--help` still
advertised bzip2 — the same defect, one layer earlier.

### Docs handoffs for WP6 (collected from the implementing WPs)

- **WP3** flag rename: `--default-variant <NAME>` (took a value) → `--default` (boolean). The pushed
  tag's OWN variant becomes the default track; the old no-op warning is gone. Exit-64 refusal text,
  verbatim: `--default requires a variant-prefixed tag, but <tag> carries no variant`. Sweep
  `command-line.md`, `building-pushing.md`, `versioning.md`, `adr_variants.md`,
  `subsystem-cli-commands.md`.
- **WP4** `cascade repair` report: per-entry key `announce_tags` → `tags`. Stale in
  `command-line.md:3355` (the `"announce_tags": ["3.28"]` JSON example) and `:3363`
  (`entries[].announce_tags` prose). `.github/workflows/oci-publish.yml:299` has a same-named shell
  variable that already passes `--tags-file` — no action.
- **WP5** `.claude/rules/subsystem-cli-commands.md`'s `package test` row still states the false
  rationale "clap waives a `requires` that conflicts with a present arg". The `requires` was dropped
  and the premise is false (verified against locked `clap_builder 4.6.2`); the mechanism is
  `required_unless_present_any = ["script", "junit"]`.
- **WP5** corrected `--junit` help, to mirror in docs: short line
  `Write a JUnit XML report for the scripted run to PATH.`; then "Requires `--script`; cannot be
  combined with a trailing command. Parent directories are created; an existing file is truncated,
  never merged"; then "Written whenever the scripted run is reached, including a red run and an
  unreadable `--script` path; a failure resolving the package writes none."

### WP1 `L1` round 1 — Fail; the guard did not hold

The reviewer built the branch and **overwrote a file four levels above the extraction root** via
`--extract` on a zip. 4 Block, 4 Warn, 1 trivia. One root cause plus three local gaps:

- **B1a** `zip.rs:301-302` — `verify_parent_contained` canonicalizes only the parent; `File::create`
  then opens the final component *through* an existing symlink.
- **B1b** `symlink.rs:48-49` — `validate_target` budgets a link's `..` hops from **lexical** depth
  (`parent.strip_prefix(root)`), so a chain of `.`-target symlinks inflates lexical past physical
  depth. Every hop reads as "in root". Defeats tar too, on the **registry-supplied** path
  (`client.rs:1231` → `extract_tar_from_reader`), yielding attacker-chosen `mkdir -p` and an
  escaping symlink planted in `layers/<digest>/content/` where no bundler re-validates.
- **B1c** regression: a `./` member (what `tar -czf x.tgz .` emits first) fails every extraction.
- **B1d** regression: `pax_global_header` (every `git archive` / GitHub tarball) exits 74 — the
  headline use case for `--extract`.
- **Warn**: zip's dir branch unchecked; zip symlink bodies `read_to_string`'d before the cap check
  (2 GiB allocation, CWE-400); `ocx clean` removes the new `temp/create` scratch as an unlocked
  orphan mid-run; two acceptance tests green on the pre-fix code (both refusals also exit 65).

Remediation shape is the reviewer's: lexical `join_under_root`, `continue` when the join yields the
root itself, `verify_parent_contained` in **every** branch returning the canonical parent, symlink
targets validated against **physical** parent depth, writes via `create_new` + `remove_file` on
`EEXIST`, and tar's `chmod` gated on `unpack` having created a file. No standalone refusal for a
self-resolving target — the physical predicate gives each hop a `..` budget of 0.

Round 3 closed two "unreachable today, landmine tomorrow" holes the reviewer found rather than filing
them: `validate_target` now canonicalizes the **longest existing prefix** of the link's parent and
re-appends the absent tail lexically (the previous fallback would have escaped on
`root/a` → `.` with `root/a/b/c` absent), and the tar mode cap keys on `symlink_metadata(dst).is_dir()`
instead of the header typeflag (tar-rs treats a non-ustar name ending in `/` as a directory whatever the
typeflag says, so the typeflag inference skipped the mask on that shape). `guard.rs`'s break at the first
absent component is now load-bearing for the predicate's precondition and carries a comment saying so.

Confirmed contained and not to be churned: hard links, absolute/Windows forms, directory-over-symlink,
the `0o7000|0o022` mask, every exit code, tar's cap axis, and W2 option B.

### WP1 `L1` round 2 — verification passed, one regression and a root-cause move

Verified against the rebuilt `40215c91`: all nine findings **closed**, both exploits refused (the
zip refusal now precedes any bundler line — the extractor refuses, not the re-validation), the
two real-world regressions extract at exit 0, and the `clean` race closed for a mechanical reason
(`LockedFile::open_exclusive` creates `temp/create.lock` *before* the dir, flipping `has_lock_file`
and routing the entry through `try_acquire` instead of unconditional orphan removal).
**Verdict: no reachable out-of-root write.**

An extra round was authorised — beyond the tier's one — because the fix introduced a regression on
an install path rather than leaving reviewable residue:

- **R1** `tar.rs:275` — the D8 mode cap stopped covering tar **directory** entries: tar-rs's dir
  branch returns `Unpacked::__Nonexhaustive` after applying the raw header mode itself, so
  `if let Unpacked::File(_)` cannot fire. A `0o2777` directory keeps setgid and world-write on
  `pull_local.rs:512` and `client.rs:1231`, which write installed trees verbatim;
  `HeaderMode::Deterministic` (`tar.rs:26`) hides it on the bundle path.
- **Root cause vs placement** — `validate_target` is unchanged, so the lexical/physical weakness
  still lives in the shared function. `utility/fs/path.rs:459` is safe only by accident (symlink-aware
  `read_dir`); `script/guard.rs:143` carries the same weakness latently (its walk continues past a
  validated symlink, so `current` diverges from the physical location). Predicate moved inside
  `validate_target`; the extraction `dst` becomes belt-and-braces rather than load-bearing.
- **R2**, noted and left: `create_dir_all` still runs on the lexical parent before the containment
  check, unreachable from archive content now that planting is fixed.

### WP7 — added mid-run by WP6's `L1`, not in the original list

`--ci-annotations` is declared `num_args = 0..=1, require_equals = true` (`package_push.rs:146-147`), so a
space-separated value is never taken as the flag's value — it falls through to the `layers` positional
(`:283`). Measured against the real binary inside a simulated runner, `--ci-annotations gitlab` and
`--ci-annotations=gitlab` parse **identically** and `gitlab` is silently absorbed as a layer path; the
value is refused only *outside* CI, and then because autodetection failed. So on a real runner the space
form annotates with the autodetected flavour and adds a bogus layer argument. W13's fix improved the
message on the autodetect-failure path only and never reached this.

Guarded rather than documented: a bare `--ci-annotations` plus a layer argument that parses as a known
`CiFlavor` name is refused with exit 64 naming the `=` requirement, before any upload or auth. The
heuristic is deliberate and commented — a layer path literally named `gitlab` is implausible, and
`./gitlab` escapes it. `require_equals`/`num_args` are untouched: that grammar is shipped on `--ci` too.

### Four phantom symlink refusals — pre-existing, found by WP6's `L1`, fixed here

The "or the symlink refusal" clause had been copy-pasted onto exit-code rows for **four** readers that
never refuse a symlink — each is a bare `tokio::fs::read`/`write` with no `O_NOFOLLOW`, no cap and no
regular-file check: `package description push --readme` (`package_description_push.rs:89`),
`patch publish --descriptor` (`patch_publish.rs:58`), `patch test --descriptor` (`patch_test.rs:160`) and
`package sbom --output` (`package_sbom.rs:686`). The last was wrong twice — `--output` is a *write* sink
documented as "An I/O error **reading** `--output`". The three rows this branch's own findings covered
(`announce`/`sign`/`attest` `--tags-file`) turned out to differ per reader once traced:
`sign --identity-token-file` refuses a symlink via `O_NOFOLLOW`/`ELOOP` but reports `OidcPreCheckFailed`
(exit **77**), while `attest --predicate` refuses the same way and does land at 74.

**The repo's link gate cannot catch external rot**: `task claude:lint:links` runs `lychee --offline`, so a
green result says nothing about external URLs. The three new links were verified by hand.

### WP7 — the call site nothing covered, and a mutation that killed the build instead

Deleting `self.refuse_spaced_ci()?;` from `Env::execute` does **not** leave the suite green: it reds the
**build** (`method \`refuse_spaced_ci\` is never used`, `-D dead-code`). That guard defends the method's
*existence*, not the refusal — so the honest mutation keeps the method live (`let _ = self.refuse_spaced_ci();`),
and under it **7862 Rust tests pass**. All four guard tests call the method directly; nothing calls `execute`.
The acceptance row is therefore the only real cover, which is why it had to be run rather than authored.

Also settled by that review: a mis-cased provider is unreachable on the **env** tier — `GitLab` reds in
`parse()` as `IdentifierError { kind: UppercaseRepository }`, since an OCI repository name must be lowercase,
so the guard never sees it; a test pins that parse-level refusal instead, with a comment so nobody "fixes"
the unreachable branch. `--shell` (`env.rs:77-84`) carries the identical absorption shape and is
**deliberately left unguarded**: `gitlab` is an implausible layer name, but `bash` is an entirely plausible
package name, so `ocx package env --shell bash` is a legitimate invocation the same heuristic would refuse.
Same shape, opposite trade; recorded in the `ponytail:` comment.

### The run-level `L2` — no semantic conflict, six seam findings

The aggregate review found the seven packages coherent with each other, and every finding at a seam rather
than inside a package. Two are worth recording beyond their fix:

- **`--junit`'s documented exit-code contract was falsified by the branch's own test.** `package_test.rs:116`
  and `command-line.md:3752` say "the exit code [is] unchanged", while `script_runner.rs:96,108` propagate a
  junit write failure with `?` and `test_exit_codes.py:342` **pins exit 74** for it. A passing script with an
  unwritable `--junit` path prints `"status":"passed"` and exits 74. The exit code is correct — the repo's
  `test_an_operator_supplied_path_never_exits_internal` sweep is the convention and `--junit` is a row in it —
  so the documentation was the defect. Separately, `package_test.rs:541` let a junit write failure **mask** the
  usage error naming an unreadable `--script` path, which is the worse diagnosis; the original fault now wins.
- **`--build-timestamp` had the same silent-absorption shape as `--ci-annotations`, on the same command, with
  a worse outcome.** Declared `num_args = 0..=1, require_equals = true, default_missing_value = "datetime"`
  beside the same `layers` positional, so `--build-timestamp none` leaves the flag bare, resolves it to
  **`Datetime`** — the opposite of what was typed — and drops `none` into `layers`. With a file named `none`,
  `date` or `datetime` present the push *proceeds*, publishing `pkg:1.0+20260912…` where the operator asked
  for `pkg:1.0`: a **wrong published artifact**, against `--ci-annotations`' worst case of a missing
  annotation. Pre-existing, but the branch's new guard comment enumerated two flags and should have covered
  three.

The reusable rule the review extracted, now stated in the carve-out comment: **the heuristic is safe when the
positional's value space does not plausibly collide with the flag's value vocabulary.** `gitlab`, `datetime`,
`date` and `none` are implausible layer paths; `bash` is a plausible *package* name — and this repo's own CI
proves it, passing shell packages to `package env` at `.github/workflows/shell-activation-deep.yml:91`. That
is why `--shell` stays unguarded.

**Not a finding, by the reviewer's ruling**: shipped `--help` carrying a deep link into a website anchor that
`lychee --offline` cannot verify. ~10 existing help strings already do exactly that
(`options/platform.rs:25`, `options/pinned.rs:38`, `options/bin_scan.rs:27`, `options/content_path.rs:11`);
nothing verifies any in-repo anchor referenced from Rust help text. Pre-existing and branch-wide — worth an
issue, not a merge gate.

**Seam verdicts (run-level `L2`, all re-derived from source rather than from the review notes):**

1. **WP1 × WP2 clean** — bzip2 decodes *inside* the cap: `compression::read_file` returns `MultiBzDecoder`
   as a plain `Box<dyn Read>`, which `archive.rs:187-196` wraps in `reader.take(cap+1)` before
   `tar::extract_returning_reader`, so it inherits containment **and** the mode cap (both live in
   `extract_from_archive`, not in a caller). `MultiBzDecoder` carries no bound of its own, so nothing
   double-refuses; a refusal needs **both** >256 MiB decompressed and >100×, and bzip2 on tool binaries runs
   ~5-8×. The registry half holds: `oci/client.rs:1231`/`:1253`/`:1271` → `extract_tar_from_reader` → the
   same guarded loop.
2. **Path handling clean** — three coherent tiers, accurately documented at every site.
3. **WP3 × WP4 × WP7 coherent** — one `=`-requirement phrasing (`conventions.rs:398`) feeds both refusals, so
   one spelling of one rule across a three-way `conventions.rs` merge; no refusal is unreachable
   (`refuse_spaced_ci_annotations()` before `resolve_ci_annotations_arg()`, `package_push.rs:295`/`:311` — the
   only order where the space-form message beats autodetect; `--default`'s check after identifier resolution
   and before auth, costing no round trip).
4. **WP3 × WP6 clean** — eleven `PushReport` fields in declaration order matching the doc, exactly the last
   five `skip_serializing_if`; "the first six keys alone" is literally true.
5. **WP4 × WP6 clean** — `announce_tags` survives only as a shell variable that already passes `--tags-file`.
6. **WP1 × WP6 — all four corrected symlink rows verified right**, re-derived from the readers:
   `bounded_read.rs:71` is a plain `File::open` refusing only non-regular/over-cap;
   `package_sign_common.rs:155-169` maps `ELOOP` → `OidcPreCheckFailed` → **77**; `:707-720` maps `ELOOP` →
   `file_error` → **74**; `--readme`/`--descriptor` use `tokio::fs::read`, so the refusal was indeed phantom.
   The gaps ran the other way — two user-reachable refusals with no table row (findings 2 and 3).
7. **Sysexits clean across all seven packages** — 64 for the six usage refusals, 65 for `EntryEscape`,
   `SymlinkEscape`, `ExtractionCapExceeded`, `DecodeOnly`, empty extraction and unsupported format, 74 for
   tags-file and junit writes. No refusal contradicts a sibling for the same fault class.
8. **Test suite clean** — `helpers.py` changed additively only (`symlinks=` on `build_archive`, `push_env=` on
   `make_package`), so no package's test rests on a signature another package changed underneath it.

**The one uncrossed seam, and a gap against this plan's own requirement.** The traversal guard has **no
acceptance test through the registry/install path** — `test_archive_containment.py` drives `--extract`
exclusively. B1 required both routes ("through `--extract` AND through `pull_layer`") and only the first
shipped. It matters more than its low-risk rating suggests: the install route is the one that originally wrote
`$OCX_HOME/PWNED-ON-INSTALL`, i.e. from **registry-supplied** bytes, where `--extract` needs the operator to
have chosen a hostile archive. Same `extract_from_archive`, and the Rust unit tests do cover it via
`Archive::extract`, so this pins the reachable path rather than doubting the guard. Assigned to WP8.

bzip2 through the registry is **not** a gap: `oci/client.rs:1288`'s arm is genuinely unreachable because
`from_media_type` has no bzip2 mapping, verified by grep finding no bzip2 reference outside `compression.rs`,
`client.rs` and `package_create.rs`.

**Stability-tier audit (`L2`, checked against the diff rather than assumed):** `CHANGELOG.md` is absent from
all 66 changed files; every interface break carries `!` — `ca00c319 feat(push)!`, `395c6da1 feat(cascade)!`,
`8bac82a4 fix(cascade)!`, `6a986c5a feat(cascade)!`; and **no compat shim was added for any internal rename** —
`--announce-tags` is gone with a tombstone acceptance test (`test_package_cascade.py:482`) rather than a clap
alias, `graph::announce_tags` was deleted outright rather than re-exported, and `PushOutcome::with_aliases`
left no deprecated stand-in.

## Block reopened — the cross-model gate broke the containment guard (2026-09-12)

Codex, run after seven packages had merged and `task verify --force` was green, **reproduced an escape on the
built binary**. The write primitive is closed; **`mkdir` and symlink creation are not.**

`symlink.rs:62-90` canonicalizes only the **link's parent** and then folds the **target** lexically, so an
intermediate component *of the target* can be a symlink an earlier entry planted — the lexical fold then
disagrees with the kernel. `tar.rs:212-218` and `zip.rs:280-285` compound it by running `create_dir_all`
**before** `verify_parent_contained`, so the directory exists by the time the refusal fires.

```
symlink  a   -> .          # root/a == root
symlink  e1  -> a/..       # folds lexically to the root -> accepted; physically 1 level UP
symlink  e2  -> e1/..      # accepted; 2 up ... repeat to reach /
regular  e20/tmp/PWNED/x   # create_dir_all() mkdirs /tmp/PWNED, THEN the check refuses
```

Observed: `mkdir` outside the root to arbitrary depth (`/tmp/OCX_ROOT_ESCAPE_PROOF`), `$OCX_HOME/ocx.lock`
created **as a directory** (persistent DoS of the user's install), and `ROOTED -> e12/etc/passwd` extracted at
**exit 0**, landing in the published bundle with `readlink -f` giving `/etc/passwd`. Both backends. Reachable
from **registry bytes** via `pull.rs:1010` → `pull_layer` → `extract_tar_from_reader`.

**Two of WP1's own tests assert the falsified premise** — `tar.rs:679` and `zip.rs:608` plant the chain in the
link's *parent*, which the longest-existing-prefix fix does close; none plants it in the *target*, and
`zip.rs:652`'s comment claims exactly what the recipe disproves. One root cause, **three call sites**:
`symlink.rs`, `assemble.rs:968`, `script/guard.rs:153`.

Direction: reuse `refuse_if_symlink_in_path` (`utility/fs/symlink_walk.rs:79`, already used by
`package test --output`) rather than patching the lexical fold — an archive has no legitimate need to write
through a symlink it planted, and an ancestor-symlink refusal closes mkdir, symlink and write together.
Nothing may be created before containment is established.

**Second finding: GNU sparse entries bypass the extraction cap.** `archive.rs:192`'s `take(cap + 1)` counts
**tar-stream** bytes, but `tar-0.4.46/src/entry.rs:671-675` materializes a hole with `f.seek(to)` +
`f.set_len(size)`, consuming nothing from the stream. Source-verified, not reproduced.

**The rest of the cross-model findings.** Landing in the WP1 redux:

- **The mode cap never reaches created directories** (`tar.rs:212-218`, `zip.rs:280-296`). Implicit parents and
  zip directory entries are made with `create_dir_all` — mode `0o777 & ~umask`, never chmodded. Under
  `umask 000`/`002`, routine in containers and CI, an archive of just `a/b/file` leaves `a` and `b` at
  `0777`/`0775`, retaining exactly the group/other write D8's `0o022` cap promises to strip. The
  *entry*-driven directory path is capped correctly; only the implicit one is not — **and a test run under the
  default `umask 022` cannot tell the two states apart**, which is why it survived three review rounds.
- **`ocx clean` can unlink the `temp/create.lock` a live `create --extract` holds** (`acquire_result.rs:27-35`),
  putting the scratch directory straight back into the orphan class WP1's W5 fix removed it from.
- **Windows: a relative archive symlink resolves against the process CWD** (`symlink.rs:446-449` joins onto
  `current_dir()` instead of `link_path.parent()`), so `link -> .` becomes a junction to `$CWD`. Pre-existing,
  but this branch promotes `validate_target` to the trust boundary, so the validator can be correct while the
  creator lands the link elsewhere. Not verifiable on this host.

Deferred: zero-length entries exhausting inodes inside the byte cap (`zip.rs:246-250`, `--extract` only — an
entry-count cap is an owner trade); the duplicated cap constants (drift only loosens a ceiling).

**What the adversary could not break — recorded so it is not churned.** Each was expected to work and did not:
a planted final-component symlink (`create_new` + `remove_file` never follows); a directory entry onto a
planted in-root symlink (tar-rs refuses `EEXIST`, and its tolerance does not accept a symlink-to-dir);
replacing a directory with a symlink (`EEXIST`); hard links (`resolve_hard_link_source` canonicalizes, so a
ladder link resolves outside and is refused, and `a -> .` hits `EPERM` on a directory); `strip_components`
variants (`join_under_root` folds lexically first, so a residual leading `..` is refused — keeping raw `..`
off the filesystem is what makes the entry-name path solid); escaping via a directory the ladder itself
created (**the physical parent check is load-bearing**); and re-breaking the parent-side chain the
longest-existing-prefix fix closed.

### The gate's own false green — worth more than the findings

**Codex's first run returned "approve / no material findings" because its turn was killed by a cybersecurity
content filter at step 4 (exit 1).** The rerun, with defensive framing, produced everything above. A clean
cross-model verdict was therefore indistinguishable from a review that never ran — the exact failure class
this branch spent the session hunting, one level up, in the gate rather than the code. Any future
cross-model pass must confirm the review actually completed before its verdict counts.

**Still unverified by anything executed**: `extract_tar_from_reader`, the actual registry-stream route. WP8's
local-route evidence shares a function with `--extract`, and the row that reaches the stream path was authored
but never run, so its push precondition is untested. Owned by the WP1 redux — not to be assumed covered.

### WP1 redux — orchestrator verification (my own run, not the builder's)

Builder committed `fff5856b` on `hex/wp1-archive-target-guard`. I verified it myself
rather than taking the report:

- `test_target_component_symlink_traversal_is_refused` **passes** on the committed tree.
- **Mutation A** — `validate_target` reverted to the lexical fold: the row **reds**, on the
  message assertion (`'escapes the root directory'` absent; stderr carried
  `archive entry 'e1/PWNED/x' escapes the extraction root`), exit still 65. The
  pre-create ancestor guard caught the second entry instead. Two independent guards.
- **Mutation B** — A plus the tar ancestor guard disabled: `assert 0 == 65`, bundle created,
  and the escape landed on disk at `/home/mherwig/.ocx/temp/create/PWNED` (removed). Both
  guards are load-bearing; the property is real.
- **Corrected.** I first recorded that the row's `tmp_path` sweep could not red. That was
  wrong: under the acceptance harness `$OCX_HOME` lives *inside* `tmp_path`, so the sweep does
  reach the escape. My contrary reading came from a manual repro run without `OCX_HOME` set,
  which hit the real `~/.ocx`. Measured on the redux tip with both guards disabled, the ladder
  row's escape lands at `tmp_path/ocx-home/PWNED` — the sweep discriminates.

### Reopened: the registry-stream route exits 1, not 65

`test_a_registry_served_hostile_layer_is_refused_on_install` — WP8 authored it, nothing ran
it, and it is **red**: `assert 1 == 65`. The guard fires and nothing is written, so the
security property holds; the **exit-code contract does not**. The operator gets 1 where the
documented data error is 65.

Narrowed, so nobody re-derives it: the classification chain is **correct**. A throwaway probe
built the exact chain
(`crate::Error::Dependency(SetupFailed(singleflight::Error::Failed(SharedError(PackageErrorKind::Internal(crate::Error::Archive(SymlinkEscape))))))`)
and `classify_error` returned `DataError` — every link works (`DependencyError::SetupFailed =>
None`, `SingleflightError::Failed => None`, `SharedError::source`, the `try_downcast!` ladder).
The break is in what the install command hands the classifier, or whether it calls
`classify_error` on that path at all. The sibling local-layer row asserts 65 and is green, so
the difference is the install/singleflight route, not the archive error.

**Lesson, again and more expensively than last time.** I mutated files inside a worktree whose
builder still had work dispatched, and restored them from a snapshot taken eight minutes
earlier — the same failure shape as the WP7 loss, in a worktree I did not own. Verification
that needs a mutation belongs in a throwaway copy of the tree, never in the builder's.

### WP1 redux tip `96cb4421` — measured, one row still vacuous

Builder rebased onto WP8 and added findings 3/4/6: `create_dir_all_capped` (directory mode cap
for implicitly created dirs, red/green under umask 000), `lock_matches_path` (the temp-lock
flock+unlink-by-path race, `(dev,ino)` re-verify after acquire), and the Windows `create_link`
relative-target join. Finding 3 is **unverified on Windows** — no runtime coverage on this host,
recorded as such rather than claimed.

Serial run on `96cb4421`: 7 passed, 1 failed (the install row, below). Both guards disabled:

| Row | Reds? | Escape on disk |
|---|---|---|
| `test_target_component_ladder_traversal_is_refused` | yes, `assert 0 == 65` | `tmp_path/ocx-home/PWNED` — sweep is real |
| `test_mkdir_only_traversal_through_planted_symlink_is_refused` | yes, `assert 0 == 65` | **nothing under `tmp_path`** — sweep is decoration |

The mkdir-only row cannot produce an escape: `a -> .` is an in-root hop, so `root/a/PWNED` **is**
`root/PWNED`, and the `TempDir` drop deletes the scratch tree when the create succeeds. Its
docstring claimed the filesystem assertion is what pins the mkdir escape; that claim was false,
and the sweep was removed rather than kept as a can't-fire belt-and-braces.

**The row itself is kept, and the two rows are not redundant — measured, not argued.** Mutating
*only* the ancestor guard (`refuse_if_symlink_in_path_sync` in both extractors, target guard
intact): the mkdir row **reds** (`assert 0 == 65`, bundle produced) while the ladder row stays
**green**. Mutating only the target predicate reverses it. So each row defends a different
guard, and `a -> ..` would *not* make the mkdir row "more real" — the target guard refuses that
shape first as `SYMLINK_ESCAPE`, collapsing it into a ladder duplicate and losing the
ancestor-guard coverage entirely. Its discriminators are the exit code, the
`ENTRY_ESCAPE`-present / `SYMLINK_ESCAPE`-absent pair, and `not out.exists()`.

The install exit-code red (1, not 65) is unchanged by the rebase and still owned.

### Root cause of the install exit-1: `ClientError::Internal` is a verdict where it should delegate

`crates/ocx_lib/src/oci/client/error.rs:375` — `Self::Internal(_) => ExitCode::Failure`.
`pull_layer` wraps the extraction failure at `client.rs:1376`
(`ClientError::internal(archive_err)`), and that terminal arm **short-circuits the chain
walker** before it reaches `archive::Error::SymlinkEscape` → `DataError`. That is why the
chain classified correctly in an isolated probe and the real install still exited 1: the
probe had no `ClientError` node in it.

Known trap, fixed at one producer and never at the source. `tasks/common.rs:158` deliberately
avoids the wrapper — *"wrapping it in `ClientError::internal` would classify as the terminal
`Failure` (1) and end the chain walk"* — with a regression test at `tasks/pull.rs:1246`
pinning that one path. The layer-extraction producer was never swept. The doctrine is written
two lines up in the same enum, on the `Mirrored` variant: *"Adds provenance, never a verdict."*

Fix is one line at the source, so every producer inherits it (`Self::Internal(_) => return
None`). Verified in my own worktree at `96cb4421`:

| Gate | Before | After |
|---|---|---|
| `test_archive_containment.py` | 7 passed / 1 failed (`assert 1 == 65`) | **8 passed** |
| `cargo nextest --workspace --locked` | 7880 passed / 8 skipped | 7880 passed / 8 skipped |
| `test_exit_codes.py` + `test_install.py` | — | 35 passed / 1 xfailed |

No test anywhere pinned `Internal → 1`; checked before changing it. Handed to the builder to
commit on its branch with a regression test beside `pull.rs:1246`'s, since without one the next
producer re-introduces it.

### WP1 redux — builder died with the exit-code fix uncommitted; orchestrator took over

The builder's "background gate `bh1046v89`" died silently while its status said running:
no `cargo`/`rustc`/`nextest` process, `target/` untouched for 40 minutes, and every
teammate gone from the agent list. The tell was the same one recorded for Codex jobs — a
status field that says running when the worker is dead. Its three uncommitted files were
intact in `.agents/worktrees/wp1-target`; I reviewed them, ran the gates myself, and am
committing on its branch as the sole remaining writer.

- `client/error.rs` — `Self::Internal(_) => return None` with the `Mirrored` doctrine
  comment, plus `internal_delegates_its_exit_code_to_the_wrapped_source` asserting both a
  classifiable inner (`DataError`) and an opaque inner (`Failure`), so a hardcoded constant
  fails one arm or the other.
- `package_manager/error.rs` — `install_batch_with_a_client_wrapped_traversal_refusal_classifies_as_data_error`,
  the end-to-end `InstallFailed` batch through `anyhow` the way `main.rs` classifies it.
- `test_archive_containment.py` — mkdir-only row: vacuous sweep removed, docstring corrected
  to name its real discriminators, `SYMLINK_ESCAPE not in stderr` added.

Gates on that tree, run by me: `cargo fmt --check` clean · `clippy --workspace --all-targets
--all-features -D warnings` clean · `nextest --workspace --locked` **7882 passed / 8 skipped**
(both new tests green by name) · containment suite serial **8 passed**, including the install
row that was red. `task verify --force` on the tree in progress, logged to
`.tmp/wp1-verify.log` (the session scratchpad was wiped mid-run once already).

Two lessons for the memory upkeep step: (1) `nohup … &` combined with the harness's own
background mode tracks the wrapper shell, not the job — the "completed exit 0" I acted on was
the shell; (2) a long gate's log belongs under `<repo>/.tmp/`, never the session scratchpad.

### Schedule — WP1 redux merged as `67c4c98a`

Branch `hex/wp1-archive-target-guard` (`5558e561` target-side predicate + ancestor guard +
GNU-sparse refusal · `96cb4421` implicit-dir mode cap + temp-lock `(dev,ino)` re-verify +
Windows relative junction target · `28c92194` `ClientError::Internal` delegates, install exits
65) merged `--no-ff` onto `49b0b441`. Merge tree `a0ef06ca` **is** the tree `task verify` ran
on (identical tree hash), so the verify evidence carries: 23 gates, unit **7882 passed / 8
skipped**, acceptance **3372 passed / 156 skipped / 5 xfailed**, `claude:tests` 51/3.

Eight acceptance reds on the first verify run, all environmental and re-run to green: seven
`test_schema.py` rows because the builder's worktree never ran `task schema` (gitignored
generated files absent — the documented fresh-worktree prerequisite); one
`test_package_push_mount_cross_repository_reuse` (`mounted 0, uploaded 1` — the local
registry's known cross-repo mount limitation, red on main, untouched by this branch's file
set). After `task schema`: `test_schema.py` 9 passed; the mount row is the sole FAILED line,
which is the documented clean-branch signature.

File-set extension beyond WP1's original scope, all justified by the reopened findings:
`oci/client/error.rs` + `package_manager/error.rs` (exit-code delegation and its two tests),
`utility/fs/locked_file.rs` + `file_structure/temp_store.rs` (the `clean`-vs-held-lock race),
`utility/fs/symlink_walk.rs` (the sync ancestor guard).

Worktrees `wp1-target` (builder's, builder dead) and `lead-verify` (mine) removed; branch
deleted. `wp5-junit` remains — root-owned zot bind-mount, owner's `sudo rm -rf`.

Finding 3 (Windows `create_link` relative junction target) is **unverified on Windows** — unit-
tested logic only; no runtime coverage on this host. Recorded, not claimed.

### Fresh `L2` on the redux diff — one High, confirmed and fixed (`47de833d`, not yet merged)

**High — the ladder in reverse entry order.** `e1 -> a/..` arrives while `a` is absent: the
target's missing tail folds lexically to the root and the per-entry predicate accepts it. Then
`a -> .` lands and `e1` physically resolves to `root/..`. Nothing is written through it (the
ancestor guard holds), but the extracted tree carries an escaping link. The reviewer reproduced
it on the real `67c4c98a` binary: forward order → refusal after `a`; reverse order →
`Extracted e1`, `Extracted a`, `Extracted README.md`, `3 entries total`.

Every route but one re-packs and the re-pack's sweep caught it by accident — measured: with
the fix disabled, `--extract` and the `package test` local layer still refuse (via
`add_dir_all`'s sweep and assembly). **The registry stream route does not**: with the fix
disabled, `install` finalized the layer into `layers/localhost_5000/sha256/d8/…/content/e1`
carrying the escaping link, and assembly refused only afterwards — the next install would
find the layer "present on disk" and skip the fetch. The first acceptance row I wrote was on
the local-layer route and stayed green under mutation; moved to the install route it reds
on the filesystem assertion (finalized layer exists), which is the defect itself.

Fix: both extractors call `archive::sweep_symlinks(&canonical_root)` after the entry loop —
`validate_symlinks_in_dir`, whose own doc already called it "the sweep after archive
extraction" and which nothing called there. A refused link is unlinked before the error
returns, repeating until none remain. Rust tests both backends red (`must be refused:
Ok(())`) → green; `nextest` **7884 passed**; containment suite **9/9** serial.

Also from that `L2`, applied: `SymlinkWalkError::Io` (EACCES on stat) now maps to `Io` (74),
not `EntryEscape` (65); three stale comments reworded (`common.rs` "terminal Failure" claim,
`tar.rs`/`zip.rs` "canonical parent"); `command-line.md` `--extract` refusal list gains GNU
sparse; the mkdir-only row's docstring no longer claims an accepted in-root link can never come
to escape. **Deferred to the owner**: `5558e561`'s subject omits the GNU-sparse refusal it
ships (the subject is the changelog line) — rewriting a merged series is the owner's call.

Codex's second pass reached the same reverse-order break independently (3-link recipe `e1 ->
a/..`, `ROOTED -> e1/SECRET`, `a -> .`, reproduced with a faithful port reading `TOPSECRET`
through the tree) and one thing the `L2` missed: the **async** `LockedFile::try_exclusive`
never got the `(dev,ino)` re-verify. Both on `hex/wp1-post-sweep` at `17dfd2d0`
(`47de833d` sweep · `17dfd2d0` async lock routes through `try_exclusive_blocking` + the
3-link recipe pinned on both backends).

One red on the first verify was **my own new test**, not the sweep: it demanded `ROOTED` be
gone, but which link the sweep meets first is `read_dir` order — meeting `e1` first removes
it, after which `ROOTED`'s target is absent and it is re-judged as an in-root *dangling*
link. Both end states satisfy the property the sweep promises — nothing under the root
resolves outside it — and that is what the test asserts now (6× dev, 8× release stable).

`task verify --force` on `17dfd2d0`: unit **7886 / 7886**, acceptance **3380 passed / 156
skipped / 5 xfailed**, sole FAILED line the known push-mount phantom. Merge pending the
second Codex pass, scoped to the two fix commits with a brief to break the sweep itself.

### Schedule — WP1 sweep merged as `df8d33bb`; run complete

`hex/wp1-post-sweep` (`47de833d` post-loop sweep + Io→74 + docs · `17dfd2d0` async lock inode
re-verify + 3-link recipe · `b22a3f59` sweep judges by resolution, junction-safe) merged
`--no-ff` onto `67c4c98a`. Merge tree `4d0bcc34` is the verified tree. `task verify --force`:
unit **7886 / 7886**, acceptance **3378 passed / 156 skipped / 5 xfailed**; three FAILED lines —
the push-mount phantom, and two sigstore rows that read `Rekor transparency log unavailable`
(exit 83) during the parallel run and pass on serial re-run (29/29 with the containment suite).

Codex pass #2 on the sweep: Unix holds (8-scenario oracle, zero surviving escapes — reverse
ladders, sibling re-acceptance, ELOOP, 5-hop, nested, root-self, absent-then-hop, pre-sweep
error paths, lock bound semantics); one High, Windows-only: the sweep re-ran the entry-time
predicate, which refuses absolute targets, and every Windows junction is absolute. Fixed in
`b22a3f59` by making the sweep the physical post-condition (canonicalize, refuse only what
lands outside the canonical root, remove via `symlink::remove`, detect via `symlink::is_link`).
Corrected Codex's impact claim in the record: symlinks in layers are already unsupported on
Windows at assembly (`WindowsSymlinksUnsupported`), so nothing installable regressed — but the
failure would have moved, changed code, and left a junction behind. **Windows half is by trace
only**, as with Finding 3; no runtime coverage on this host. Not sent for a third Codex pass:
the change narrows what the sweep refuses, and every Unix oracle scenario still reds under
mutation.

Worktree `wp1-sweep` and branch removed. `wp5-junit` remains — owner's `sudo rm -rf`.

**Owner decisions:** all three taken 2026-09-13 — see the next entry.

### Owner decisions (2026-09-13) — all three taken into the branch

1. **Zero-length-entry inode exhaustion — fixed** (`38068b22`). The zip cap counted bytes
   written; an empty entry writes none but costs an inode. Every zip entry is now charged the
   tar-header floor (512 bytes) against the same byte budget, so both backends share one
   ceiling and one refusal — no second constant, no new error variant. Test: 64 empties refused
   under a 63-header budget, extracted under 64; red with the charge zeroed (first mutation
   killed the build via dead-code, so the consumer arm was mutated instead). `--extract` docs
   name the floor.

2. **`5558e561`'s subject rewritten** to name the GNU-sparse refusal. No `rebase -i` here, so
   the series was rebuilt by hand off `49b0b441`: amend → cherry-pick → recreate both `--no-ff`
   merges with their original messages → cherry-pick the chore. **Tree equality proven at every
   node** against the originals (`a0ef06ca`, `4d0bcc34`, `2a75ae86`); backup ref
   `backup/sion-pre-reword-20260913` at `b9def151`. Only the one subject line differs.

3. **Windows symlink coverage before landing** — and it paid for itself twice over. Type-checking
   the redux for both MSVC targets through `task rust:check:windows` (cargo-xwin in Docker; the
   host gate cannot see `cfg(windows)` and `check:windows-cfg` is scoped to `ocx_shim`) found
   **two build breaks the merged redux would have shipped to every Windows CI leg**:
   `create_dir_all_capped` left `root` unused off Unix, and `handle_matches_path` was
   `cfg(test)` with only unix-gated callers — both hard errors under `-D warnings`. The builder's
   "windows-cfg checks passed" was true of a gate that never compiled this crate. Fixed in
   `4bdd3380`, both targets clean including tests.

   The row itself: `cfg(windows)` tests on both backends — an in-root directory symlink extracts
   as a junction, resolves to the in-root directory, is readable through, and survives the
   post-loop sweep (the case the second Codex pass caught); and the reverse-order ladder never
   leaves a junction resolving outside the root, asserted as the invariant because whether NTFS
   resolves `..` inside a substitute name is unobservable here. These run on the Windows unit
   legs (`verify-basic`, `verify-deep`).

   **What cannot be green on Windows yet, stated plainly:** an end-to-end `package create
   --extract` row with a symlink. The re-pack (`add_dir_all` → `validate_symlinks_in_dir`)
   still judges a junction by its spelled, necessarily absolute target and refuses it. That is
   pre-existing and orthogonal to the sweep, and fixing it is a *format* decision — a bundle
   built on Windows would carry an absolute link target unless the re-pack rewrites the junction
   back to a relative symlink entry. Layer symlinks are already unsupported at assembly on
   Windows (`WindowsSymlinksUnsupported`). Left as a follow-up for the owner to scope.

### Two independent guards, one property — WP8's measurement

WP8 neutered `symlink::validate_target`'s containment verdict and its two-entry traversal archive **still
exited 65 and still wrote nothing**: the per-entry guard catches the second entry once the link is let
through. Two independent guards defending one property, indistinguishable by exit code — so a test that
asserts only "exit 65" passes with either one deleted. That is exactly why WP8's rows would have stayed green
through the defect the cross-model gate found: its shape is a *single* symlink with a long `../` chain, not
the ladder, and never exercises `mkdir`. The redux's tests must assert **that nothing was created outside the
root**, not the exit code.

Two corrections WP8 made to my briefs, both accepted: `package test` with a **local** layer reaches
`extract_with_options` — the same function `--extract` uses — not `pull_layer`, so the registry route needs a
real publish first; and the `--shell` CI evidence is `shell-activation-deep.yml:92`, whose invocations would
survive a guard twice over (two-segment names do not parse as a `Shell`, and `--shell=bash` is attached). What
that line proves is that shell packages are already passed positionally, not that CI would break — the
carve-out rests on the plausible-package-name argument alone.

Also from WP8, on the guard it generalised: for `Option<Option<V>>` (`--ci`, `--ci-annotations`) `Some(None)`
*is* the bare flag, but for `Option<V>` + `default_missing_value` (`--build-timestamp`) bare and `=datetime`
are indistinguishable downstream, so the precondition is `matches!(Some(Datetime))`. `is_some()` would refuse
`--build-timestamp=date none`, where a layer named `none` is exactly what the publisher meant.

## Verification

Per the plan this branch was built from: `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo nextest run --workspace --locked`, `task test:parallel --force -- <touched test files>` from the repo root, then `task verify --force`. Every new test must be shown red before its fix and green after; B1's regression tests are the ones the branch cannot land without.

Never edit `CHANGELOG.md`. The traversal fix's commit subject is its release note: write it as a `fix(archive)!:`-free `fix(archive):` sentence a user reads — the interface did not change, the containment did.

### WP5 note — H6's premise was false

H6 prescribed "drop `requires`, keep `conflicts_with`, refuse in `execute`" on the premise that
clap waives the `<COMMAND>` requirement when `--junit` is present. It does not: with `requires`
dropped, `--junit` alone still failed `MissingRequiredArgument: <COMMAND>...` and `execute` was
never reached. WP5 additionally changed `command` from `required_unless_present = "script"` to
`required_unless_present_any = ["script", "junit"]`. All four combinations verified on the built
binary. Carried to WP6: `.claude/rules/subsystem-cli-commands.md`'s `package test` row still
states the (false) clap-waiver rationale.
