# Research: Bazel 9 / rules_rust / gazelle_rust — verification of the 2026-09-20 UNVERIFIED claims

Date: 2026-09-21 · Lane: technology/tooling (verification) · Model: sonnet · For: `.claude/artifacts/adr_bazel_build_adoption.md`
Expires: 2026-12-21

## 1. rules_rust #3807 / #3732 / #3286 / #2753

**#3807 — "no native story for first-party path-dep workspace members"**
VERIFIED — CLOSED (completed), 2026-02-06. https://github.com/bazelbuild/rules_rust/issues/3807
Decisive quote (maintainer @dzbarsky, 2026-01-26): *"You may be interested in https://github.com/dzbarsky/rules_rs/ which provides an alternate implementation of crate_universe including support for generating build files for first party crates."* The issue was not fixed in rules_rust itself — it was closed by pointing the reporter at a competing ruleset (`rules_rs`). rules_rust's own answer for a 100+-crate first-party workspace remains "hand-write BUILD.bazel, or use Gazelle" (gazelle_rust — see #4 below fills this gap for us).

**#3732 — alias for a `[patch]`ed crate points at the vendored location**
VERIFIED, but scope-corrected — OPEN. https://github.com/bazelbuild/rules_rust/issues/3732
Maintainer @illicitonion (2025-12-15): *"I don't believe we've ever (intentionally) added path dependency support to `crates_vendor`, so I'm not surprised it doesn't work... We **do** have path dependency support for non-`crates_vendor`."* **This bug is scoped to `crates_vendor` only.** ocx-sion would use `crate.from_cargo` (bzlmod `crate_universe`), a different code path the maintainer says already has path-dependency support. The prior lane's "against" framing overstated the blast radius — #3732 does not block `from_cargo`.

**#3286 — unable to specify Rust version in crate_universe bzlmod**
VERIFIED — CLOSED (completed) 2025-09-16 via [PR #3293](https://github.com/bazelbuild/rules_rust/pull/3293). https://github.com/bazelbuild/rules_rust/issues/3286. Fixed before the 0.74.0 window; not a live blocker.

**#2753 — `rust-toolchain.toml` ignored**
VERIFIED — still OPEN. https://github.com/bazelbuild/rules_rust/issues/2753. A fix exists as an unmerged draft: [PR #3792](https://github.com/bazelbuild/rules_rust/pull/3792) "feat(bzlmod): Add rust_toolchain_file attribute for rust.toolchain()", opened 2025-12-20, **still open/unmerged** as of 2026-04-01 (last activity). Workaround remains: a second, parallel `rust.toolchain(versions=["1.95.0"])` pin in `MODULE.bazel`, independent of `rust-toolchain.toml`.

## 2. rules_rust release / default Rust version / 1.95.0 acceptance

VERIFIED, with a correction to the prior lane. https://github.com/bazelbuild/rules_rust/releases/tag/0.74.0
Newest release: **0.74.0**, published **2026-08-28**. `.bazelci/presubmit.yml` pins `minimum_bazel_version: "7.4.1"` — confirms the CI floor claim.
**Correction:** `DEFAULT_RUST_VERSION` at tag `0.74.0` (`rust/private/common.bzl`) is **`"1.98.0"`, not 1.94.0** as the prior lane stated — the 0.74.0 release notes list "Added Rust 1.98.0" ([PR #4244](https://github.com/bazelbuild/rules_rust/pull/4244)) as the change that bumped the default.
`rust.toolchain(versions=["1.95.0"])` today: no static `known_shas.bzl` allow-list exists anywhere under `rust/` at the 0.74.0 tag (rules_rust fetches Rust-distribution checksums dynamically at repo-rule time rather than from a pinned table), and `rust/private/nightly_versions.bzl` at 0.74.0 already tracks the 1.95.0 release window (`"2026-01-22": "1.95.0"`), three stable releases before the 1.98.0 default. **VERIFIED (by absence of a version allow-list + presence in the version-tracking table): 1.95.0 is accepted.**

## 3. bazel#29114 — CARGO_BAZEL_REPIN crash under Bazel 9.0.1

VERIFIED, with an important nuance. https://github.com/bazelbuild/bazel/issues/29114 — **CLOSED (completed)**, closed 2026-09-15.
The crash (`IllegalStateException: Conflicting values recorded for input FILE:@@//cargo-bazel-lock.json`) is a repo-rule bug: it re-reads a file it already watched after the file's own content changed mid-fetch (repin rewrites `cargo-bazel-lock.json` while rules_rust is still reading it).
Two independent fixes exist, on two different sides:
- **Bazel side**: [PR #31091](https://github.com/bazelbuild/bazel/pull/31091) "Lock in the first observed value of a recorded input" — shipped only in **9.3.0rc1** (2026-09-15) per the bazel-io bot comment. **9.3.0 has not reached GA as of 2026-09-21** (see §5 — newest stable is 9.2.0, 2026-07-13). Pinning "the newest 9.x" today does **not** include this fix.
- **rules_rust side**: [PR #3932](https://github.com/bazelbuild/rules_rust/pull/3932) "Move module_ctx.watch to after lockfile repin", merged 2026-04-07 — lands in every rules_rust release since (0.71.2 onward, including the 0.74.0 we'd pin). **This mitigation is already present in 0.74.0**, independent of which Bazel 9.x GA is used.
Net: on Bazel 9.2.0 (current GA) + rules_rust 0.74.0, the repin crash is already avoided by the rules_rust-side fix; the Bazel-side hardening is extra safety arriving in 9.3.0 (not yet GA).

## 4. gazelle_rust (Calsign)

VERIFIED — exists, actively maintained, and materially more capable than the 2026-09-20 lane assumed. https://github.com/Calsign/gazelle_rust
- Last repo update: **2026-09-18** (3 days before this verification), 52 stars, 20 forks, 2 open issues (of 16 total).
- **On BCR**: yes — `modules/gazelle_rust/0.1.0` exists in `bazelbuild/bazel-central-registry` (https://github.com/bazelbuild/bazel-central-registry/tree/main/modules/gazelle_rust). Issue [#38](https://github.com/Calsign/gazelle_rust/issues/38) "Add to Bazel-Central-Registry" is CLOSED (completed), 2026-05-19.
- **Its own `.bazelversion` = `8.7.0`, `MODULE.bazel` pins `rules_rust` `0.71.0`.** It does **not** declare Bazel 9 / rules_rust 0.74 support in its own CI config — Bazel 9 compatibility is user-reported-and-patched, not a declared/tested support matrix.
- **Bazel 9 compat exists but is reactive**: issue [#29](https://github.com/Calsign/gazelle_rust/issues/29) "`sh_test` no longer shipped with Bazel 9.x" — CLOSED (completed) 2026-01-28, exactly the `--incompatible_autoload_externally` breakage this ADR must also handle in its own `BUILD.bazel` files (see §5).
- **First-party Cargo-workspace path deps: already solved**, contrary to the prior lane's framing. Issue [#5](https://github.com/Calsign/gazelle_rust/issues/5) "Support for a workspace that uses `Cargo.toml` files as the source of truth" (a `foo` crate path-depending on a sibling `bar` crate, exactly this repo's shape) — CLOSED (completed) 2026-01-03.
- One related open limitation: issue [#15](https://github.com/Calsign/gazelle_rust/issues/15) — when using the `# gazelle:rust_cargo_lockfile` directive, transitive (not just direct) dependencies leak into the generated `deps`, which can misattribute or omit deps. Refinement gap, not a blocker.
- **No open issue found about `[patch.crates-io]` or submodule crates** (searched all 16 issues). This is a genuine gap in gazelle_rust's own issue tracker, not evidence it works — it has simply never been asked.
Net: gazelle_rust is viable as the spike target (WP-1), but the spike must explicitly test (a) generation against `rules_rust` 0.74 / Bazel 9, since gazelle_rust's own CI does not, and (b) the three `[patch]`ed submodule crates specifically, since no evidence either way exists. Hand-written `BUILD.bazel` remains the documented fallback if the spike reds.

## 5. Bazel 9.x release line + the four 9.0.0 behaviour flips

VERIFIED. Full 9.x GA list (https://github.com/bazelbuild/bazel/releases), newest first:
| Version | Date |
|---|---|
| 9.2.0 | 2026-07-13 |
| 9.1.1 | 2026-06-03 |
| 9.1.0 | 2026-04-20 |
| 9.0.2 | 2026-04-09 |
| 9.0.1 | 2026-03-10 |
| 9.0.0 | 2026-01-20 |
**9.3.0 is only at rc2 (2026-09-17) as of 2026-09-21 — not GA. Newest GA is 9.2.0.**

All four flips confirmed from the primary source, the [9.0.0 release notes](https://github.com/bazelbuild/bazel/releases/tag/9.0.0):
1. **WORKSPACE gone**: *"Bzlmod is now always enabled, and all `WORKSPACE` logic has been removed from Bazel (#26131)... The `--enable_bzlmod` and `--enable_workspace` flags are now no-ops."*
2. **`--incompatible_autoload_externally` empty by default**: *"The flag `--incompatible_autoload_externally` now defaults to the empty string (#23043), meaning that all language-specific rules will now need to be loaded from their respective modules."* — corroborated independently by gazelle_rust issue #29 (§4) hitting exactly this for `sh_test`.
3. **`--incompatible_strict_action_env` defaults on**: listed under the release notes' *"Migration-Ready Incompatible Flags / The following flags are flipped in 9.0"* section, alongside `--incompatible_repo_env_ignores_action_env` etc. — the `incompatible_*` naming convention means the flag flips to the new (strict) behaviour by default.
4. **`--repo_contents_cache` defaults on**: *"Added a new flag `--repo_contents_cache` (defaults to the `contents` directory under the `--repository_cache`) where Bazel stores fetched contents of repos that can be safely cached across workspaces."*

Exact load paths for `sh_test`/`sh_binary` on Bazel 9 (confirmed against `bazel-contrib/rules_shell`, the module `rules_rust` itself depends on at `0.6.1`+):
- `load("@rules_shell//shell:sh_test.bzl", "sh_test")`
- `load("@rules_shell//shell:sh_binary.bzl", "sh_binary")`

## 6. `.bazelversion` without bazelisk

VERIFIED, with a load-bearing nuance the prior lane's blanket claim missed. Primary source: `bazelbuild/bazelisk/README.md`.
Bazelisk's own docs distinguish two non-bazelisk cases:
- *"Bazel installers typically provide Bazel's shell wrapper script as the `bazel` on the PATH. When installed this way, Bazel checks the `.bazelversion` file itself..."* — an OS-package install (apt/homebrew) ships a **wrapper script** (`scripts/packages/bazel.sh`) that DOES read `.bazelversion`.
- *"Note that if users directly downloaded a Bazel binary and put it in their PATH, rather than running an installer, then `tools/bazel` and `.bazelversion` are not checked."* — a **raw prebuilt binary** on PATH ignores it entirely.
Since `ocx.sh/bazelbuild/bazel:9` materializes the raw upstream release artifact (OCX's model is "OCI registries as storage for pre-built binaries", not repackaging an OS installer's wrapper script), the **raw-binary case applies**: the prior claim "a plain `bazel` binary ignores `.bazelversion`" holds for this specific delivery mechanism. No Bazel startup flag validates the running version against `.bazelversion` — the closest native mechanism named anywhere is the third-party `bazel_skylib` `versions.check()` Starlark helper, invoked from `WORKSPACE` (now removed in Bazel 9, so this workaround is itself stale). Confirms: pin authority must be `ocx.lock`; `.bazelversion` needs an explicit drift check against it, as the discussion already concluded.

## 7. `ocx.sh/bazelbuild/bazel` and `ocx.sh/asciinema/agg` in the live index

VERIFIED via direct fetch of `https://index.ocx.sh/c/index.json` (reachable; no 403 encountered from this fetcher — the discussion's UA-block note applies to `Python-urllib/*` specifically).
- **`bazelbuild/bazel`: EXISTS.** The catalog carries `"bazelbuild/bazel": "sha256:dcd7df88fa5e2eb844c9ccf1cdd5c4b0f77c53993dcda5118d30e73a636aae75"` — a root pointer into an OCI image index. Resolving that digest to enumerate specific versions/platforms requires pulling the OCI index from the backing registry (out of scope for a docs/issue-tracker verification pass — needs the `ocx` binary or an authenticated OCI client, not a plain HTTP fetch of a static JSON tree).
- **`asciinema/agg`: DOES NOT EXIST.** Confirmed absent from the same catalog (alphabetically, it would sort near the start, between `actionlint` and the rest; it is not there). Matches the prior lane's finding — `agg` still needs a mirror package created (`../mirror-*` pattern) before it can be an `ocx.toml` entry.

## 8. `--build_event_json_file` and `--execution_log_compact_file` on Bazel 9

VERIFIED, with a correction and two omissions filled in. Primary sources: `src/main/protobuf/spawn.proto` and `src/main/java/com/google/devtools/build/lib/exec/ExecutionOptions.java` at `refs/tags/9.2.0`, and `src/main/java/com/google/devtools/build/lib/buildeventstream/proto/build_event_stream.proto`.
- **Flag name**: the flag has **graduated from experimental**. At the 9.2.0 tag: `@Option(name = "execution_log_compact_file", oldName = "experimental_execution_log_compact_file", ...)` — `--execution_log_compact_file` is the current, non-experimental name; the old spelling still works as a backward-compat alias. (The `.proto` file's own header comment is stale and still says "generated by `--experimental_execution_log_compact_file`" — a doc-drift artifact, not a stability signal.)
- **Per-action cache status field**: `ExecLogEntry.Spawn.runner` (string) + `ExecLogEntry.Spawn.cache_hit` (bool), in `src/main/protobuf/spawn.proto`. Documented semantics verbatim: *"If the spawn did not hit a disk or remote cache, this will be the name of the runner, e.g. \"remote\", \"linux-sandbox\" or \"worker\". If the spawn hit a disk or remote cache, this will be \"disk cache hit\" or \"remote cache hit\", respectively... spawns whose owning action hits the persistent action cache are never reported at all."*
- **Per-target wall time — correction**: `TargetComplete` (in `build_event_stream.proto`) carries **no timing field at all** (only `success`, `output_group`, `tag`, `test_timeout`). Wall time only exists on `TestResult.test_attempt_duration` (`google.protobuf.Duration`) — **and only for test targets**. A non-test target (e.g. a `rust_library` crate with no test) has no BEP-level wall-clock field; that would have to come from the execution log's `SpawnMetrics.total_time`/`execution_wall_time` (`spawn.proto`) aggregated per target via `target_label`, not from `TargetComplete`. The ADR's "per-target wall time from `TargetComplete`/`TestResult`" plan needs this split.
- **Schema stability**: no explicit "unstable" disclaimer found on either flag's help text or the proto comments; the schema is versioned in-tree as a first-class `.proto` (not a scraped/undocumented format).
- **Existing parser**: Bazel ships its own, `//src/tools/execlog` (`src/tools/execlog/README.md`: *"This tool is used to inspect and parse the Bazel execution logs. Currently supported formats are `binary`, `json`, and `compact`."*) — a ready-made reference implementation for the BEP/execlog → OTLP script.

## 9. `rules_js` / `bun.lock`

VERIFIED — unchanged from the prior lane, re-confirmed live.
- `npm_translate_lock` still accepts only pnpm/npm/yarn locks; no `bun_lock` parameter exists.
- [rules_js#1258](https://github.com/aspect-build/rules_js/issues/1258) "Support for other runtimes like Bun" — **still OPEN**, still labeled `need: funding`, 11 reactions, last touched 2025-08-23 (over a year stale as of 2026-09-21 — no momentum).
- **No maintained Bun ruleset exists on BCR** — a full listing of `bazelbuild/bazel-central-registry/modules` shows `aspect_rules_js` (pnpm-based, as expected) but no `rules_bun` or equivalent anywhere in the catalog. The coarse hand-written Starlark rule (`bun install` + `bunx vitepress build`, `requires-network`) remains the only viable path; no new evidence changes this.

## 10. bazel#5373 (PTY in linux-sandbox) + `no-sandbox` caching

VERIFIED. https://github.com/bazelbuild/bazel/issues/5373 — **CLOSED, state_reason: `not_planned`**, labeled `P4`, `stale`, closed 2023-06-07 by a stale-bot. No maintainer engagement disputing the "not planned" resolution; this is exactly the `pty.openpty()` / `OSError: out of pty devices` failure `pexpect.spawn` will hit.
`no-sandbox` does not disable caching — exact quote from Bazel's Common Definitions reference (`docs/.../reference/be/common-definitions.mdx`, "tags" attribute): *"`no-sandbox` keyword results in the action or test never being sandboxed; it can still be cached or run remotely - use `no-cache` or `no-remote` to prevent either or both of those."* This is version-stable language (checked against the 7.6.1 doc snapshot; the sandboxing/tags semantics have not changed).

## 11. `bazel mod deps --lockfile_mode=error` + bazel#28717

VERIFIED. `--lockfile_mode` is a real, current flag: `@Option(name = "lockfile_mode", converter = LockfileMode.Converter.class, defaultValue = "update", documentationCategory = OptionDocumentationCategory.BZLMOD, ...)` in `src/main/java/com/google/devtools/build/lib/bazel/repository/RepositoryOptions.java`. `bazel mod deps --lockfile_mode=error` is the correct Bazel 9 spelling (also used verbatim in rules_rust's own `.bazelci/presubmit.yml` `hello_world` example: `test_flags: - "--lockfile_mode=error"`).
[bazel#28717](https://github.com/bazelbuild/bazel/issues/28717) "Facts API behavior with `--lockfile_mode=error` is incorrect when rolling back MODULE.bazel changes" (false reds on branch switch) — **CLOSED (completed)**, closed 2026-02-24 via [PR #28718](https://github.com/bazelbuild/bazel/pull/28718) "Only use workspace facts for validation with `--lockfile_mode=error`". Fixed before any 9.x GA in scope (closed before 9.0.1, 2026-03-10) — this failure mode should not reproduce on the Bazel 9.x line this ADR would pin.

## Decisions this changes

- **rules_rust default is 1.98.0, not 1.94.0** — the "second, parallel Rust pin" the ADR plans for `rust.toolchain(versions=["1.95.0"])` is further from HEAD than previously stated (three releases back, not one); re-state the version drift in the ADR's toolchain section.
- **#3732 does not block `crate.from_cargo`** — it is a `crates_vendor`-only bug per the maintainer. Drop it as a blocker for this repo's chosen mode; keep it only as a note in case a future vendoring mode is considered.
- **The repin crash (bazel#29114) is already mitigated on the rules_rust side (0.74.0, since PR #3932)** — do not gate the pilot on waiting for Bazel 9.3.0 GA (which only has the Bazel-side belt-and-suspenders fix, still at rc2 today). Pin 9.2.0 (current GA), not "newest 9.x" as a moving target.
- **gazelle_rust already solves first-party Cargo-workspace path deps (issue #5, closed)** and is on BCR (0.1.0) — upgrade WP-1's framing from "spike to see if it's viable at all" to "spike to confirm it works against *our* pin pair (Bazel 9.2.0 / rules_rust 0.74.0) and the 3 patched submodule crates specifically," since gazelle_rust's own CI still runs Bazel 8.7.0 / rules_rust 0.71.0 and has never been asked about `[patch.crates-io]`.
- **Per-target wall time cannot come from `TargetComplete`** — it has no timing field. The BEP→OTLP script needs `TestResult.test_attempt_duration` for test targets and a separate `SpawnMetrics`-based aggregation (execution log, keyed by `target_label`) for non-test build targets. Update the observability design in the discussion/ADR.
- **`asciinema/agg` confirmed absent from the live index** (not just "UNVERIFIED") — the mirror-package step for `agg` is a hard prerequisite, not a contingency.
- **No maintained Bun ruleset exists anywhere (BCR-checked, not just rules_js-checked)** — the coarse hand-written Starlark rule is not a fallback, it is the only option; state it as a decision, not an open question.
- **`--execution_log_compact_file` (no `experimental_` prefix) is confirmed current at Bazel 9.2.0 GA** — use this spelling in the ADR/plan; the `experimental_` form is a legacy alias only.
- **Pin `.bazelversion` expectations to the raw-binary-ignores-it behaviour**, now with a citation, since `ocx.sh/bazelbuild/bazel:9` is a raw-binary delivery, not an OS-installer wrapper script.
