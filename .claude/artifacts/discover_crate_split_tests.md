# Discovery: acceptance + unit test surface (crate-split smoke-tier planning)

Scope: `/home/mherwig/dev/ocx-evelynn`, worktree branch `evelynn`, HEAD `d8750fd7`, 2026-09-16.
Context read: `adr_crate_split_workspace.md` §§ Verification tiers, Phase 2, Validation;
`subsystem-tests.md` in full.

**Headline framing finding.** The ADR's smoke tier (`task test:smoke`, a `smoke` pytest
marker, `test_smoke_coverage.py`) is **entirely unimplemented** on this tree — verified by
grep: no `smoke` string anywhere in `test/taskfile.yml` or `test/pyproject.toml`, and no
`test_smoke_coverage.py` file exists. `test/pyproject.toml:23-32` registers exactly two
markers today, `requires_tty` and `divergence`. Everything below is the "before" snapshot
the ADR plans against, not evidence the plan has landed.

**Acceptance-count discrepancy, stated plainly.** The ADR's § Validation and § Quantified
Impact cite "2,412 today" / "558 s (the full 2,412-test suite)". Both the local collection
run and the last green `verify-basic.yml` CI run on `main` (2026-09-15, run 35032266887)
show **3,618 collected / 3,358 passed + 255 skipped + 5 xfailed = 3,618** — see § 1 and § 4.
The ADR's baseline is stale by roughly 1,200 tests as of this HEAD; anyone gating "count
does not drop" on the ADR's literal number will false-positive-fail. Report this to whoever
owns the ADR text; this file does not correct it.

---

## 1. Acceptance suite shape

**File count.** `test/tests/*.py` (excluding `__pycache__`): **175 files**; **168** of them
contain at least one collected test (the remainder are fixture/helper modules that live in
`tests/` — `fake_forge.py`, `fake_gitlab.py`, `git_http_fixture.py`, etc.).

**Total collected.**
```
$ cd test && uv run pytest --collect-only -q | tail -1
3618 tests collected in 0.63s
```
(TMPDIR set to `~/.cache/hexdisc-tmp`; no docker/registry needed for pure collection.)

**Per-file counts, top 30** (from the collect-only listing, `sed -E 's/::.*$//' | sort | uniq -c`):

| Count | File |
|---|---|
| 301 | test_shell_reconcile.py |
| 216 | test_shell_reconcile_edge_cases.py |
| 122 | test_state_providers.py |
| 98 | test_doc_scripts_parser.py |
| 80 | test_bench_smoke.py |
| 76 | test_transport_git.py |
| 73 | test_doc_scripts.py |
| 71 | test_toolchain_render.py |
| 67 | test_doc_command_reference.py |
| 63 | test_patches.py |
| 62 | test_self_setup.py |
| 61 | test_dependencies.py |
| 52 | test_toolchain_env.py |
| 49 | test_package_claim.py |
| 48 | test_announce.py |
| 46 | test_project_env.py |
| 43 | test_verify.py |
| 43 | test_package_test_script.py |
| 41 | test_git_http_fixture.py |
| 40 | test_exec_modes.py |
| 40 | test_announce_e2e_evidence.py |
| 38 | test_execution_records.py |
| 37 | test_sign.py |
| 37 | test_project_pull.py |
| 36 | test_managed_config.py |
| 36 | test_doc_scripts_publish.py |
| 35 | test_self_activate.py |
| 34 | test_toolchain_cli.py |
| 34 | test_toolchain_activate.py |
| 34 | test_index_ocx_sh.py |

**Marker usage.** `requires_tty`: 9 sites, all in `test_update_check_throttle.py`.
`divergence`: 1 site, `test_cosign_matrix_cosign_signs.py`. `xfail`: 23 sites across ~11
files. `pytest.mark.skip(if)`/`pytest.skip(`: 210 sites. No `smoke` marker exists (grep
returns 0; the marker isn't registered).

**`sign|attest|verify|cosign` name/file match** (regex over the collected test-ID list,
case-insensitive): **258 collected tests**, spanning **24 files**:
`test_attest.py`, `test_auto_verify.py`, `test_cosign_interop.py`,
`test_cosign_matrix_{attest,cosign_signs,extras,ocx_signs}.py`, `test_doc_scripts.py`,
`test_execution_record{s,_standards}.py`, `test_exit_codes.py`, `test_golden_fixtures.py`,
`test_offline_verify.py`, `test_package_copy.py`, `test_push.py`,
`test_referrers_capability.py`, `test_sbom.py`, `test_sign{,_platforms}.py`,
`test_sigstore_stack_smoke.py`, `test_state_providers.py`, `test_transport_git.py`,
`test_trust_policy_signers.py`, `test_verify.py`. This is a **superset** of files that
actually touch Sigstore (some — `test_state_providers.py`, `test_transport_git.py`,
`test_doc_scripts.py`, `test_exit_codes.py` — match only because a case name or file
contains the substring "verify"/"sign" incidentally; see the fixture-chain count below for
the tighter signal).

**`sigstore_stack` fixture chain.** Defined `test/tests/fixtures/sigstore_stack.py:96`
(`@pytest.fixture(scope="session")`). One transitive dependent:
`identity_token` (`sigstore_stack.py:114`, also session-scoped,
`fn identity_token(sigstore_stack, tmp_path_factory)`). No other fixture wraps it — the
`cosign_matrix.py` helpers (`ocx_verify_args`, `_stage_trust`, `cosign_sign`, …) take
`stack: SigstoreStack` as a **plain function argument** the test passes in, not a nested
pytest fixture, so the graph is flat: `sigstore_stack` → `identity_token` → (tests).

AST-based count of `def test_*` whose parameter list names `sigstore_stack` and/or
`identity_token` (`ast`-walk over `tests/test_*.py`, since fixture requests can span
multi-line signatures a plain grep misses):

```
tests/test_attest.py: sigstore_stack=23 identity_token=20 either=24
tests/test_auto_verify.py: sigstore_stack=14 identity_token=10 either=14
tests/test_cosign_interop.py: sigstore_stack=5 identity_token=5 either=5
tests/test_cosign_matrix_attest.py: sigstore_stack=7 identity_token=4 either=7
tests/test_cosign_matrix_cosign_signs.py: sigstore_stack=9 identity_token=9 either=9
tests/test_cosign_matrix_extras.py: sigstore_stack=7 identity_token=7 either=7
tests/test_cosign_matrix_ocx_signs.py: sigstore_stack=8 identity_token=8 either=8
tests/test_offline_verify.py: sigstore_stack=6 identity_token=6 either=6
tests/test_package_copy.py: sigstore_stack=1 identity_token=1 either=1
tests/test_referrers_capability.py: sigstore_stack=2 identity_token=2 either=2
tests/test_sbom.py: sigstore_stack=32 identity_token=16 either=32
tests/test_sign.py: sigstore_stack=27 identity_token=25 either=27
tests/test_sign_platforms.py: sigstore_stack=5 identity_token=5 either=5
tests/test_sigstore_stack_smoke.py: sigstore_stack=4 identity_token=4 either=4
tests/test_trust_policy.py: sigstore_stack=14 identity_token=14 either=14
tests/test_trust_policy_signers.py: sigstore_stack=4 identity_token=4 either=4
tests/test_trust_root_distribution.py: sigstore_stack=6 identity_token=6 either=6
tests/test_verify.py: sigstore_stack=24 identity_token=17 either=24
TOTAL: {'sigstore_stack': 198, 'identity_token': 163, 'either': 199}
```

**199 tests** transitively pull up the real Sigstore stack (7 docker-compose services —
see § 3). `test_offline_verify.py` and `test_trust_root_distribution.py` request the
fixture but exercise the **offline** trust-root cache path, not a live signing round trip —
still counted here because the fixture line still starts the stack.

---

## 2. Top-level CLI verbs

Source: `crates/ocx_cli/src/command.rs` `pub enum Command` (`:96-215`).
`pub mod` count in that file: **75** (ADR text says "73 command modules" — off by 2 against
this HEAD, minor drift, not investigated further).

**22 visible top-level verbs:**
`env`, `add`, `clean`, `config` (group), `direnv`, `index` (group), `about`, `init`, `lock`,
`login`, `logout`, `update`, `package` (group), `patch` (group), `pull`,
`remove` (alias `rm`), `exec` (alias `x`), `shell` (group), `self` (group), `status`,
`inspect`, `version`.

**Hidden / internal / deprecated** (excluded from the 22 above):
- `launcher` (group) — `#[command(hide = true)]`, `command/launcher.rs:29`; internal,
  used only by generated entry-point launchers.
- `run` — `Command::DeprecatedRun`, `#[command(name = "run", hide = true)]`,
  `command.rs:198`; warns once, removed in 0.7 (`deprecated.rs::RENAMED`).
- `package describe` / `package info` — hidden deprecated spellings of
  `package description push` / `package description pull` (`package.rs:59,62`).
- `package announce --package` — hidden deprecated **flag** (not a command), on
  `package_announce.rs:86`.

**Second-level subcommands:**

| Group | Subcommands |
|---|---|
| `package` | `announce`, `attest`, `cascade` (→ `check`, `repair`), `claim`, `copy`, `create`, `description` (→ `push`, `pull`), `describe` [hidden dep.], `info` [hidden dep.], `deps`, `env`, `inspect`, `install`, `pull`, `push`, `receipt`, `sbom`, `select`, `deselect`, `sign`, `test`, `verify`, `exec` (alias `x`), `uninstall`, `which` |
| `index` | `catalog`, `list` (alias `ls`), `update`, `sync`, `regenerate` |
| `config` | `setup`, `update`, `test`, `push` |
| `self` | `activate`, `setup`, `update` |
| `shell` | `allow`, `completion`, `revoke`, `state` |

---

## 3. Smoke candidates

Every candidate below was read (not just grepped) to confirm no `sign`/`attest`/`verify`/
`cosign` code path and no `sigstore_stack`/`identity_token` fixture. Registry (`zot`,
`localhost:5000`) is session-scoped, already up for the whole run — its startup cost is
amortized across the suite, not per-test.

| Verb | Candidate | What it does |
|---|---|---|
| `about` | `test/tests/test_color.py::test_default_piped_suppresses_ansi` (`:27`) | `ocx.run("about")` — no registry interaction at all. Fastest possible. |
| `version` | `test/tests/test_color.py::test_color_never_suppresses_ansi` (`:10`) | `ocx.run("--color","never","version")` — no registry. |
| `init` | `test/tests/test_project_init.py::test_init_creates_minimal_ocx_toml` (`:48`) | `ocx init` in an empty dir, asserts `[tools]` written. No registry. |
| `direnv` | `test/tests/test_direnv.py::test_direnv_init_creates_envrc` (`:63`) | `ocx direnv init`, asserts `.envrc` content. No registry. |
| `logout` | `test/tests/test_login.py::test_logout_not_logged_in_exits_0_noop` (`:913`) | `ocx logout ghcr.io` on fresh state, exit 0 no-op. No registry, no credential helper. |
| `login` | `test/tests/test_login.py::test_login_password_stdin_ci_stores_credential` (`:235`) | Non-interactive `--password-stdin` flow against a fake cred-helper sidecar. Needs a tiny helper-script fixture, no real registry auth. |
| `env` | `test/tests/test_toolchain_env.py::test_env_in_project_default_plain` (`:136`) | One published package + `ocx env`, asserts plain-table default. One package push. |
| `add` | `test/tests/test_project_add.py::test_add_appends_to_tools_table` (`:74`) | `make_package` + `ocx add <pkg>`. One package push. |
| `remove` | `test/tests/test_project_remove.py::test_remove_drops_binding_and_uninstalls` (`:104`) | Setup with one tool, `ocx remove`, asserts candidate symlink gone. One package push. |
| `lock` | `test/tests/test_lock.py::test_lock_two_tools_produces_valid_lock_file` (`:177`) | Two published tools + `ocx lock`, asserts V3 shape. Two package pushes — earliest test in the file; no cheaper one-tool variant exists today. |
| `pull` | `test/tests/test_project_pull.py::test_pull_no_args_pulls_all_groups` (`:181`) | Two published tools, `ocx lock && ocx pull`. Two package pushes. |
| `update` | `test/tests/test_update.py::test_update_bumps_every_tag` (`:165`) | Earliest test in the file; two cascade-tagged packages, `ocx update`. |
| `status` | `test/tests/test_status.py::test_status_without_lock_reports_declared_only` (`:52`) | One package, `add --no-pull`, delete lock, `ocx status`. One push, no resolution. |
| `inspect` | `test/tests/test_inspect.py::test_inspect_default_lists_locked_candidates_without_resolving` (`:60`) | Default mode is a pure lock projection — no `--resolve`, no network. One push. |
| `exec` | `test/tests/test_project_run.py::test_run_golden_path` (`:144`) | One published tool, `ocx lock`, then `ocx exec -- hello` (the helper is named `_run_run` but its literal arg is `"exec"`). One push. |
| `package` | `test/tests/test_install.py::test_install_creates_candidate_symlink` (`:18`) | The project's own CLAUDE.md-designated canonical single acceptance test. `ocx package install <pkg>`, asserts candidate symlink. |
| `index` | `test/tests/test_index.py::test_index_list_shows_tag` (`:210`) | `published_package` fixture + `ocx index update` + `ocx index list`. One push (via fixture). |
| `clean` | `test/tests/test_clean.py::test_clean_removes_unreferenced_objects` (`:7`) | install → uninstall → `ocx clean`, asserts content GC'd. One push. |
| `config` | `test/tests/test_config_setup.py::test_adopt_writes_fence_and_snapshot` (`:105`) | `ocx config setup` against a published `config.toml` payload. One push. |
| `self` | `test/tests/test_self_setup.py::test_setup_writes_shims_and_fence` (`:148`) | Fresh `ocx self setup`, asserts shims + RC fence written. No registry beyond the seeded local candidate. |
| `shell` | `test/tests/test_toolchain_cli.py::test_shell_state_json_always_names_the_resolved_toolchain_home` (`:397`) | `ocx --format json shell state`, with and without a project. No registry. |
| `patch` | **NONE — needs a new smoke test.** Every existing `patch`-touching test lives in `test_patches.py`, which is pinned `pytest.mark.xdist_group("patch_global_slot")` (`:52`) because the tests share one registry-wide global descriptor slot and must run **serially** — the opposite of what a `-n auto --dist loadgroup` smoke budget wants, and its cheapest "test" is actually a module-teardown fixture (`patch publish` with an empty descriptor), not a real assertion. |

**Fixtures a smoke run needs.** `registry` fixture only (session-scoped `zot` on
`localhost:5000`, docker-compose service `registry` — see `test/docker-compose.yml:41`).
None of the 21 candidates above touches `mirror_registry` (`:87`), `target_registry`
(`:106`, undocumented in `subsystem-tests.md`'s fixture table — drift worth fixing
separately), `legacy_registry` (`:120`), or the 7-container `sigstore` compose profile
(`dex`, `sigstore-ct`, `fulcio`, `sigstore-mysql`, `trillian-log-{server,signer}`, `rekor`
— all `profiles:`-gated, matching the ADR's "seven containers" figure). The `registry`
fixture is session-scoped (`conftest.py:118`) — its container start cost is paid once for
the whole pytest session regardless of which subset runs, so it does not count against a
smoke-only budget beyond the one-time session cost already paid today.

---

## 4. Timing evidence

Last green `verify-basic.yml` run on `main`: `databaseId=35032266887`, 2026-09-15.

**Job list** (`gh run view 35032266887 --json jobs`): `Smoke (Linux)`, `Workflow Lint`,
`Conventional Commits` (skipped), `Unit Test Results`, `Smoke (Windows)`,
`Acceptance Tests`, `Smoke (macOS)`, `Acceptance Test Results`.

**"Smoke (Linux)" is NOT the ADR's proposed pytest smoke tier** — it is the existing
fmt+clippy+nextest job:
```
task: [rust:format:check] cargo fmt --check
task: [rust:clippy:check] cargo clippy --workspace --locked --all-targets -- -D warnings
task: [rust:test:unit] cargo nextest run --workspace --release --locked --profile ci
     Summary [ 127.743s] 8049 tests run: 8049 passed, 8 skipped
```
Job wall time 22:42:15 → 22:54:50 (~12m35s total, dominated by clippy+build, not the
127.7s nextest run itself).

**"Acceptance Tests" job** (pytest, Linux, `databaseId=104596794383`), wall time
22:56:28 → 23:04:18 (~7m50s including container pulls):
```
===== 3358 passed, 255 skipped, 5 xfailed, 9 warnings in 402.36s (0:06:42) =====
```
3358+255+5 = **3618**, exactly matching this session's local `--collect-only` count (§ 1) —
good cross-validation that nothing platform-specific is being skipped out from under the
count. No per-test `--durations` output is emitted in this job's log (no `-vv`/`--durations`
flag on the CI invocation), so **per-test slowest-N timing is not available** from this run;
only the aggregate 402.36s is.

No `-m smoke` step exists in either job (confirms § framing finding).

---

## 5. Unit tests

**Per top-level module of `crates/ocx_lib/src`.** The directory has **25 subdirectories +
16 standalone `.rs` files = 41 top-level module units** (not "35" — recounted directly;
`shims/` is a binary-blob directory, no `.rs` files, 0 tests, see § 6 for what it holds).
Counted via a script summing `#[test]` and `#[tokio::test` occurrences under each module
(directory recursively + its sibling top-level `.rs` file merged as one unit):

```
TOTAL MODULES: 41
TOTAL #[test]: 4460   TOTAL #[tokio::test]: 2380
```

| Module | `#[test]` | `#[tokio::test]` | files |
|---|---|---|---|
| oci | 794 | 799 | 87 |
| package | 691 | 135 | 47 |
| package_manager | 322 | 475 | 44 |
| config | 318 | 129 | 10 |
| project | 291 | 106 | 18 |
| shell | 288 | 0 | 9 |
| setup | 193 | 62 | 12 |
| file_structure | 171 | 99 | 16 |
| env.rs | 152 | 2 | 1 |
| forge | 146 | 121 | 14 |
| cli | 144 | 5 | 20 |
| record | 132 | 6 | 9 |
| utility | 95 | 161 | 25 |
| trust.rs | 83 | 0 | 1 |
| ci | 66 | 0 | 6 |
| script | 64 | 0 | 12 |
| patch | 50 | 6 | 6 |
| announce | 49 | 36 | 4 |
| managed_config | 44 | 43 | 6 |
| publisher | 42 | 39 | 4 |
| auth | 41 | 6 | 6 |
| tls.rs | 36 | 1 | 1 |
| claim | 34 | 25 | 5 |
| lazy.rs | 32 | 0 | 1 |
| archive | 27 | 32 | 6 |
| activate.rs | 23 | 0 | 1 |
| symlink.rs | 22 | 0 | 1 |
| activation.rs | 18 | 43 | 1 |
| sbom | 17 | 0 | 2 |
| reference_manager.rs | 15 | 15 | 1 |
| shim.rs | 15 | 0 | 1 |
| hardlink.rs | 12 | 0 | 1 |
| launch | 12 | 12 | 2 |
| ladder.rs | 7 | 0 | 1 |
| compression | 6 | 5 | 2 |
| error.rs | 5 | 0 | 1 |
| media_type.rs | 3 | 0 | 1 |
| codesign.rs | 0 | 17 | 1 |
| lib.rs, log.rs, shims | 0 | 0 | 1/1/0 |

`crates/ocx_cli`: **1078 `#[test]`, 58 `#[tokio::test]`**.
`crates/ocx_schema`: **30 `#[test]`, 0 `#[tokio::test]`**.
`crates/ocx_shim`: **109 `#[test]`, 0 `#[tokio::test]`**.
Sum across all four crates: 8115 inline test attributes vs. nextest's reported 8049 —
a ~66-test gap not investigated (candidates: parametrized-macro expansion counted
differently by nextest vs. a literal-attribute grep, or a crate/profile excluded from the
`--workspace --release` run).

**`crates/ocx_lib/tests/*.rs` integration tests:**

| File | Tests | Asserts |
|---|---|---|
| `dispatch_conformance.rs` | 4 | Cross-repo decode parity for OCI-image-index dispatch objects vs. vendored `ocx-sh/index` fixtures (`adr_oci_index_only_dispatch.md` D1) |
| `index_wire_conformance.rs` | 9 | Byte-parity of `ocx_lib::oci::index::serialize_root` against `ocx-sh/index`'s Python serializer output, vendored verbatim |
| `live_index_wire.rs` | 3 | Conformance against bytes `index.ocx.sh` actually serves live — the "did the catalog envelope drift" gate |
| `tag_verdicts.rs` | 2 | Cross-repo drift gate for the reserved-tag rule (`adr_oci_index_only_dispatch.md` D7) against `Tag::is_reserved` |

All four pin a `SOURCE_COMMIT` (documented in `tests/fixtures/index_wire/README.md:63`,
maintained by `test/scripts/sync_index_conformance.sh`). `task test:index-conformance-drift`
is defined at `test/taskfile.yml:515` — runs `./scripts/sync_index_conformance.sh --check`;
explicitly **not** wired into `task verify` (network-bound), runs only on
`verify-deep.yml`'s weekly schedule.

---

## 6. `crate::test` and `__testing`

**`crate::test::` sites:** **363 occurrences in 40 files** (script: regex count of the
literal `crate::test::` string per file). This differs from the file-map artifact's claimed
"367 in 44 files" — re-verified with a fresh script rather than trusting the prior figure;
the gap is small and not chased further (likely a few multi-item `use` braces counted
per-item there vs. per-line here). Per-module breakdown:

```
config 106   ci 44   package_manager 37   env.rs 35   activate.rs 20   config.rs 18
setup.rs 18  forge 12   oci 12   ci.rs 9   lazy.rs 8   project 7   activation.rs 5
claim.rs 5   claim 5   record 5   announce 4   managed_config 4   codesign.rs 2
trust.rs 2   file_structure.rs 1   setup 1   auth 1   utility 1   shell 1
TOTAL 363
```

**`crates/ocx_lib/test/{mod,data,env,manifest_source,fifo}.rs` exports:**

| File | Public items |
|---|---|
| `mod.rs` | `pub mod data; pub mod env; #[cfg(unix)] pub mod fifo; pub mod manifest_source;` |
| `data.rs` | `data_dir()`, `archive_dir()`, `archive_xz()` — paths into the `test/data/` fixture tree (a real directory, not Rust) |
| `env.rs` | `struct EnvLock` (`.set`, `.remove`, `.isolate_project_home`), `fn lock() -> EnvLock` — the `env::var` override hook's backing store |
| `manifest_source.rs` | `struct ConcurrencyProbe` (`.peak()`), `struct FakeManifestSource` (`.with`, `.with_concurrency_probe`, `.with_blob`, `.with_digest`) |
| `fifo.rs` (unix only) | `mkfifo(path)`, `release_blocked_reader(path)` |

**`env::var`'s `#[cfg(test)]` override site:** `crates/ocx_lib/src/env.rs:1821-1823`
(`pub fn var` — under `#[cfg(test)]` it first consults
`crate::test::env::get_override(key.as_ref())` before falling through to
`std::env::var`). Note: the ADR cites `env.rs:1297` — that line does not match this content
on this HEAD; the function has moved. Cite the symbol (`pub fn var`), not the line, per this
repo's own reviewing convention.

**`feature = "__testing"` sites:** 52 occurrences across **17 files**:
`forge/{gitlab,github,git_workspace}.rs`, `oci/index.rs`, `oci/index/ocx_index.rs`,
`oci/identifier.rs`, `oci/host_capabilities.rs`, `config.rs` (doc-comment mention only, not
an attribute), `package_manager/tasks/render_toolchain.rs`, `shell/hook.rs`,
`record/environment.rs`, `auth/store.rs`, `config/edit.rs`, `config/loader.rs`,
`file_structure/shim_bin_store.rs`, `package/metadata/template.rs`,
`package/metadata/template/render.rs`.

Of those, **8 files carry the `#[cfg(not(any(test, feature = "__testing")))]` production
sibling arm**: `forge/gitlab.rs`, `forge/github.rs`, `forge/git_workspace.rs`,
`oci/index/ocx_index.rs`, `package_manager/tasks/render_toolchain.rs`,
`record/environment.rs`, `auth/store.rs`, `file_structure/shim_bin_store.rs`. The other 9
files' `__testing`-gated code has no not()-arm — either it's additive (a test-only helper
function with no production counterpart) or the production behavior lives in an `else`
branch of ordinary control flow rather than a second `cfg` arm; not further classified here.

**`crates/ocx_cli/Cargo.toml` `[features]`, verbatim:**
```toml
[features]
# Forwards `ocx_lib`'s internal-only test escape hatch. NEVER enable in
# release builds. See `ocx_lib`'s manifest for the full contract.
__testing = ["ocx_lib/__testing"]
```

**`test/bin/ocx` build command** (`test/taskfile.yml:74-89`, task `build`, internal):
```sh
cargo build --release -p ocx -p ocx_shim --features ocx/__testing --locked
mkdir -p {{.BIN_DIR}}
cp target/release/ocx{{exeExt}} {{.OCX_BINARY}}
cp target/release/ocx-shim{{exeExt}} {{.OCX_SHIM_BINARY}}
```

---

## 7. Test-support helpers in `test/src/`

| Module | Purpose |
|---|---|
| `runner.py` | `OcxRunner` (the binary wrapper) + `PackageInfo`, `current_platform()`, `registry_dir()` |
| `helpers.py` | `make_package()` and other package-authoring/publishing helpers (no module docstring) |
| `assertions.py` | Cross-platform path assertions (`assert_symlink_exists`, junction-aware) |
| `registry.py` | OCI registry client (wraps `oras-py`) for manifest/platform inspection |
| `connect_proxy.py` | Stdlib TLS-terminating `CONNECT` proxy fixture (extra-CA-roots tests) |
| `forward_proxy.py` | Stdlib absolute-form HTTP forward proxy fixture (SSRF-guard-under-proxy tests) |
| `doc_binding.py` | Static verify-path checks for walkthrough doc bindings |
| `doc_scripts.py` | Header parser, discovery, drift-gate executor for `test/doc_scripts/*.sh` |
| `shell_eval.py` | Safe subprocess evaluation of shell export-line output |
| `shell_matrix.py` | Shared stdlib-only helpers for the all-shell per-prompt reconciler matrix |
| `state_providers.py` | Unified state-provider registry (replaces two legacy registries) |
| `static_index.py` | Local static-file HTTP fixture encoding `index.ocx.sh` wire shapes |
| `terminal.py` | Runs a script on a pty, captures what reached the controlling terminal |
| `tls_index.py` | TLS-wrapped `StaticIndexServer` fixture (extra-CA-roots tests) |
| `toolchain_fixtures.py` | Shared construction/observation helpers for the rendered toolchain tree |
| `scenarios/*.py` | `Scenario` base class + registered pre-publish-state subclasses (basic, diamond_deps, multi_entrypoints, multi_layer, three/two_level_deps) |
| `announce_e2e/evidence.py` | Track-D announce end-to-end evidence collection |

**Binary acquisition:** `ocx_binary` session fixture (`conftest.py:213`) resolves
`$OCX_COMMAND` if set, else `test/bin/ocx` (`.exe` appended on win32) — the file the `build`
task above produces. Hard failure (not a skip) if absent, unless
`OCX_TESTS_NO_REGISTRY=1` is also set (the Windows-shim-only CI leg).

**`conftest.py` session fixtures:** `registry` (`:118`, zot `localhost:5000`),
`mirror_registry` (`:128`, registry:2 `localhost:5001`), `target_registry` (`:154`,
second zot `localhost:5003` — **not documented in `subsystem-tests.md`'s fixture table**,
a doc-drift worth a follow-up), `legacy_registry` (`:181`, registry:2 `localhost:5001`,
referrers-negative), `ocx_binary` (`:213`).

---

## 8. Command → test-file mapping

Two invocation idioms coexist in this suite (relevant to any future "grep the glob table
from the crate map" tooling): `ocx.run/json/plain("verb", ...)` via the `OcxRunner` fixture
directly, **and** a per-file local `_run(ocx, cwd, *args)` wrapper that shells out to
`subprocess.run([str(ocx.binary), *args], cwd=cwd, ...)` because `OcxRunner.run` has no
`cwd=` parameter and many CWD-walk tests need one. **80 of 175 test files** reference
`ocx.binary` directly (the second idiom). Mapping below merges both idioms via three
AST-based passes (direct receiver calls; `[str(x.binary), *args]` literal lists; and calls
into a file-local wrapper function whose body contains that literal-list shape).

| Verb | Files | Verb | Files |
|---|---|---|---|
| `package` | 95 | `pull` | 13 |
| `lock` | 32 | `exec` | 11 |
| `index` | 32 | `about` | 4 |
| `add` | 17 | `direnv` | 5 |
| `launcher` | 9 | `shell` | 5 |
| `clean` | 7 | `update` | 5 |
| `remove` | 7 | `patch` | 5 |
| `env` | 7 | `self` | 3 |
| `config` | 6 | `version` | 3 |
| `init` | 6 | `status` | 2 |
| | | `login` | 1 |
| | | `logout` | 1 |
| | | `inspect` | 1 |

Coverage caveat: this counts a file once it invokes the verb **anywhere**, not necessarily
in a happy-path/smoke-eligible way — it is the raw material for a crate-split glob table,
not itself a smoke selection (that's § 3).

Notably thin: `inspect` (1 file), `login`/`logout` (1 file, shared), `status` (2 files) —
these top-level verbs have the least acceptance depth today, independent of anything the
crate split changes.
