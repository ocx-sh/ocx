# Classification: port-down candidates in the acceptance suite

C-024 (ADR `adr_test_speed_tiers.md` C-RUBRIC) — WP-10, read-only. Amended after L1 review (`.tmp/hex-tiers/wp10-l1.md`, PASS with 5 actionable + 1 deferred/ruled) — this version applies all 5 fixes plus the orchestrator's ruling on finding 6.

## Method

**Headline count: pytest-collected cases**, not the AST function count the first pass used — C-RUBRIC's `>= 300` is denominated in the unit the ADR's `3,884` was itself counted in (collected test items, parametrize fully expanded), and the function-count column under-stated the suite by 504 cases (2941 vs 3445 collected). Function count is kept as a secondary column.

```
cd test && OCX_TESTS_NO_REGISTRY=1 uv run pytest --collect-only -q tests/
```

Printed: `3445 tests collected in 0.66s` across all 172 `test_*.py` modules (per-module counts parsed from the `tests/<module>.py::<case>` lines). `uv run` directly (not `ocx exec -- uv run`, which failed — see Divergences) because collection with `OCX_TESTS_NO_REGISTRY=1` needs no registry or binary; this is read-only and does not execute any test body.

Function-count enumeration (first pass, kept for cross-reference): `modules: 172 test functions: 2735` (unchanged by this fix round — the AST function-count method doesn't distinguish flat from mixed modules; the 24+5=29 functions in the two mixed modules are still part of that 2735, just re-split by class below instead of carrying one class each). 172 modules is 9 fewer than the ADR's "181 modules" / "3,884 cases" estimate (written before Stage 5 landed on this base, `ed23bd1a`).

## Classification granularity

**Ruling (exec-orch, after L1 finding 6):** C-024 says every *case* is classified. A **homogeneous** module — every case shares the same class — may stay module-level (the class applies to each of its cases; this is still the ADR's own Stage-6 framing: "Sonnet classifies all 181 modules"). A **mixed** module — cases split across keep-e2e and port-capable, or across two different port targets — must be split per case.

Two modules were mixed and are split below: `test_config.py` (24 cases: 18 port-capable targeting `ocx_config`, 6 port-capable targeting `ocx_cli` — command-dispatch config-coupling, discounted from the below-`ocx_cli` threshold) and `test_metadata_forward_compat.py` (5 cases: 3 keep-e2e R2, 2 port-capable targeting `ocx_package::metadata::env::modifier`).

**Sweep for other mixed modules** (the L1 review's "find them all"): a per-case AST sweep over every keep-e2e module (excluding the shell/pty/platform/doc-script families, which are keep-e2e regardless of any single case's fixtures) flagged every test function whose fixture list contains none of `registry, mirror_registry, legacy_registry, target_registry, published_package, published_two_versions, unique_repo, forward_proxy, identity_token, sigstore_stack` as a *candidate* mixed case. That flagged **53 modules / 583 candidate cases** — but the heuristic has a large false-positive rate it cannot self-correct: the `ocx` fixture (an `OcxRunner` already wired to the live registry) is not itself in the exclusion list, so a case that calls `ocx.run("add", "real-package:tag")` and genuinely needs the registry is flagged purely because it doesn't ALSO take `unique_repo` as a separate parameter. Confirmed false positives on manual spot-check: `test_project_add.py` (27 flagged, all genuinely registry-backed per its own docstring), `test_login.py` (27 flagged, registry auth), `test_transport_git.py` (55 flagged, forge/git network). The two modules actually split above were found this way and then verified by reading every flagged case's body/docstring — the other 51 modules' 500-ish remaining candidates were **not** individually verified; this is out of scope for this pass and is listed in full at the end of this artifact as a reproducible input for whoever executes C-025/WP-13 (each port there re-derives its own input synthetically anyway, which is exactly where a real per-case split naturally happens).

## Registry-boundary rule (fix 4 — applied uniformly)

**The registry-boundary rule** (applied uniformly, fix 4): classify by what the ASSERTION verifies, not by what the CURRENT test's fixture setup happens to need. A case stays keep-e2e only when the property being verified itself requires crossing a real boundary — actual bytes from a live registry/forge/Sigstore service, actual shell/pty behaviour, actual process-boundary semantics (signal, execvp, env_clear, an exit code as a real parent process observes it), actual on-disk symlink/permission layout from the full command pipeline, or a platform-specific OS behaviour a mock cannot reproduce. A case is port-capable when the asserted outcome is fully determined by a pure Rust computation reachable via an existing library API — parsing, resolution/precedence, validation, error classification, serialization — even if today's Python test happens to reach that computation by pushing/installing a real package first, because a port can hand the equivalent already-resolved input to the library call directly. This is why `test_project_toml_preservation.py` and 18 of `test_config.py`'s 24 cases move to port-capable despite registry-touching setup: the mutation-preservation and config-resolution PROPERTIES don't need the registry, only today's fixture-building path does.

## Totals

| Class | Modules (flat) | Cases (collected) | Cases (fn, secondary) |
|---|---|---|---|
| keep-e2e (flat modules) | 155 | 3226 | 2532 |
| port-capable (flat modules, all targets) | 15 | 190 | 174 |
| mixed modules (2, split above) | 2 | 29 | 29 |
| **Total classified** | **172** | **3445** | **2735** |

Rolled up across flat + mixed: **keep-e2e 3229 cases**, **port-capable 216 cases** (195 below `ocx_cli`, 21 discounted as `ocx_cli`-targeted).

## >= 300 below-`ocx_cli` threshold (ADR C-RUBRIC pilot GO criterion)

**Still NOT MET.** Port-capable cases below `ocx_cli`: **195** (collected) — up from the first pass's 151 (function count) / 161 (param-expanded) after correcting to collected cases and applying all 5 fixes, but the L1 review's own error-bar analysis ("about 150-235 collected... below ~276 (8% of 3445)") already predicted every point in range stays under the threshold, and 195 confirms it. `test_package_receipt.py` (4) and `test_status.py` (11) were retargeted to `ocx_cli` itself (both APIs are defined in `crates/ocx_cli`, not a lower crate) and no longer count; `test_config.py`'s 6 command-dispatch cases are `ocx_cli`-targeted too and also don't count. **C-025/WP-13's GO/NO-GO is NO-GO on the case-count criterion** — unchanged from the first pass; the other three ADR criteria still need the pilot's own numbers.

## Totals by target crate (port-capable, below `ocx_cli` only)

| Target crate | Cases (collected) | Modules |
|---|---|---|
| `ocx_package` | 57 | `test_archive_containment.py`, `test_package_create_bin_scan.py`, `test_package_create_extract.py`, `test_schema.py`, `test_metadata_forward_compat.py (2 of 5 cases)` |
| `ocx_package_manager` | 43 | `test_config_test.py`, `test_execution_record_standards.py` |
| `ocx_project` | 43 | `test_project_config.py`, `test_project_config_home_walk.py`, `test_project_init.py`, `test_project_toml_preservation.py` |
| `ocx_config` | 18 | `test_config.py (18 of 24 cases)` |
| `ocx_setup` | 13 | `test_config_setup.py` |
| `ocx_oci` | 12 | `test_platform_pairs.py` |
| `ocx_console` | 9 | `test_color.py` |

`ocx_cli` (discounted, not below `ocx_cli`): `test_package_receipt.py` (4) + `test_status.py` (11) + `test_config.py` dispatch cases (6) = 21 cases, not in the table above.

## Pilot proposal (5 modules, C-RUBRIC selection) — amended

| Module | Target | Cases | Seam needed | Rationale |
|---|---|---|---|---|
| `test_config_setup.py` | ocx_setup::apply_managed_config | 13 | no | Docstring names the exact shared lib function both `ocx config setup` and `ocx self setup --managed-config` call. No registry, no shell, no binary bootstrap. |
| `test_status.py` | ocx_cli::api::data::status::StatusReport (CLI-bound, discounted from below-ocx_cli) | 11 | no | Kept in the pilot per the orchestrator (retarget + discount, not remove). Caveat: also the sole module reaching `ocx status` (R7) — the port must be executed and mutation-proven before the original is deleted, which is how the deletion guard retires the R7 concern rather than classification alone. |
| `test_execution_record_standards.py` | ocx_package_manager::record::execution_record | 16 | no | Replaces `test_package_receipt.py` in the pilot (fix 2 — receipt is `ocx_cli`-bound, not below it). Wire-shape standards compliance on the packages[] block against a real crate API, no registry. |
| `test_platform_pairs.py` | ocx_oci::platform::SUPPORTED_PAIRS / Platform parsing | 12 | no | Pure OS x Arch legality + wasm platform parsing, no I/O at all. Cheapest mutation-proof target in the pool. |
| `test_config_test.py` | ocx_package_manager::managed_config::preview_managed_config | 27 | YES | Retargeted (fix 3) from the original `ocx_config` guess — `preview_managed_config` is the actual API `ocx config test` calls, in `ocx_package_manager`, not `ocx_config`. It reads process env through `ocx_config::env::mirrors()/insecure_registries()` and `ocx_config::patch::patches_from_env()` while composing the 'what would this machine look like' preview, so C-SEAM's hermetic-env invariant (1) is confirmed needed, not merely possible. |

Combined pilot size: 79 collected cases across 5 target crates+APIs (`ocx_setup`, `ocx_cli`, `ocx_package_manager` x2, `ocx_oci`). `test_state_providers.py` excluded per rubric item 8.

## Fixes applied from L1 review

1. Headline count switched from AST function count to `pytest --collect-only` collected cases (the ADR's own counting unit); verdict unchanged.
2. `test_package_receipt.py` and `test_status.py` retargeted to `ocx_cli` (both APIs are defined there, not below it) and discounted from the threshold; `test_package_receipt.py` replaced in the pilot by `test_execution_record_standards.py`.
3. `test_config_test.py` retargeted to `ocx_package_manager::managed_config::preview_managed_config` (was incorrectly `ocx_config`); seam need corrected from "maybe" to "yes" (confirmed env reads).
4. Registry-boundary rule stated once and re-applied: `test_project_toml_preservation.py` (14) and `test_schema.py` (9) move from keep-e2e to port-capable; `test_config.py` (24) and `test_metadata_forward_compat.py` (5) — both mixed — are split per case.
5. `test_catalog_readme.py` reclassified R2 -> R8 (pure Python CATALOG.md lint, no describe, no registry). R7 (sole verb+flag reacher) checked for `test_status.py` per the review's own finding (confirmed the only module reaching `ocx status`); a full R7 audit of the remaining 12 port-capable modules was attempted via cross-module grep but the signal was too noisy on common words ("status", "init", "receipt" as JSON field names, not command argv) to trust without per-hit verification, so it is not claimed here beyond the one confirmed case.

## Mixed-module case split: `test_config.py`

24 cases -> 18 port-capable (`ocx_config`) + 6 port-capable (`ocx_cli`, discounted from the below-ocx_cli threshold — none are keep-e2e). Per the registry-boundary rule: every case here runs against the `ocx` fixture (registry-configured), but none of the 24 assertions are actually about registry I/O — they're about which config tier/env var wins, or which CLI surfaces are config-exempt. `ocx install <bare-name>:0` (used in several) triggers a real resolution attempt to name the resolved registry in its failure message, but the property under test is the resolver's PRECEDENCE decision, which `ocx_config`'s resolution API computes directly from layered config+env inputs without needing that install attempt.

| Case | Target |
|---|---|
| `test_config_default_registry_takes_effect` | `ocx_config` (registry/config-tier resolution) |
| `test_env_var_overrides_config_file` | `ocx_config` (registry/config-tier resolution) |
| `test_no_config_kills_file_loading` | `ocx_config` (registry/config-tier resolution) |
| `test_invalid_config_produces_clear_error_with_path` | `ocx_config` (registry/config-tier resolution) |
| `test_explicit_config_flag_loads_file` | `ocx_config` (registry/config-tier resolution) |
| `test_ocx_config_file_env_loads_file` | `ocx_config` (registry/config-tier resolution) |
| `test_no_config_with_explicit_flag_loads_only_explicit` | `ocx_config` (registry/config-tier resolution) |
| `test_registry_default_is_a_literal_prefix` | `ocx_config` (registry/config-tier resolution) |
| `test_empty_ocx_config_file_is_escape_hatch` | `ocx_config` (registry/config-tier resolution) |
| `test_explicit_config_nonexistent_file_errors` | `ocx_config` (registry/config-tier resolution) |
| `test_ocx_config_file_nonexistent_errors` | `ocx_config` (registry/config-tier resolution) |
| `test_unknown_top_level_section_ignored` | `ocx_config` (registry/config-tier resolution) |
| `test_file_too_large_errors_with_helpful_message` | `ocx_config` (registry/config-tier resolution) |
| `test_explicit_config_overrides_env_var_config_file` | `ocx_config` (registry/config-tier resolution) |
| `test_layered_merge_home_tier_and_explicit_config` | `ocx_config` (registry/config-tier resolution) |
| `test_exit_code_on_config_not_found` | `ocx_config` (registry/config-tier resolution) |
| `test_exit_code_on_config_parse_error` | `ocx_config` (registry/config-tier resolution) |
| `test_no_config_env_var_suppresses_discovery` | `ocx_config` (registry/config-tier resolution) |
| `test_version_survives_invalid_ambient_config` | `ocx_cli` (command-dispatch config-coupling matrix — discounted) |
| `test_completion_survives_invalid_ambient_config` | `ocx_cli` (command-dispatch config-coupling matrix — discounted) |
| `test_bare_ocx_survives_malformed_ambient_config` | `ocx_cli` (command-dispatch config-coupling matrix — discounted) |
| `test_help_survives_invalid_ambient_config` | `ocx_cli` (command-dispatch config-coupling matrix — discounted) |
| `test_about_still_requires_valid_config_when_ambient_broken` | `ocx_cli` (command-dispatch config-coupling matrix — discounted) |
| `test_cli_help_mentions_config_env_vars` | `ocx_cli` (command-dispatch config-coupling matrix — discounted) |

## Mixed-module case split: `test_metadata_forward_compat.py`

5 cases -> 3 keep-e2e (R2) + 2 port-capable (`ocx_package::metadata::env::modifier`, the shared `ModifierKind::FromStr` grammar). The 2 port-capable cases' own docstrings say why: `test_env_flag_unknown_modifier_type_exits_usage_error`'s docstring states outright "The flag is parsed before package resolution, so no published package is needed to exercise it" — the `unique_repo` fixture it takes is only used to build a syntactically valid identifier string, never resolved. The 3 keep-e2e cases need a real registry-published package whose metadata (or installed-dependency closure) carries the unknown modifier, which the grammar-only cases don't.

| Case | Class | Target |
|---|---|---|
| `test_install_unknown_env_modifier_type_exits_data_error` | keep-e2e | R2 |
| `test_exec_unknown_env_modifier_type_exits_data_error` | keep-e2e | R2 |
| `test_deps_skips_installed_dependency_with_unknown_env_modifier_type` | keep-e2e | R2 |
| `test_project_toml_unknown_env_modifier_type_exits_config_error` | port-capable | `ocx_package::metadata::env::modifier (ModifierKind::FromStr)` |
| `test_env_flag_unknown_modifier_type_exits_usage_error` | port-capable | `ocx_package::metadata::env::modifier (ModifierKind::FromStr)` |

## Unverified mixed-module candidates (sweep output, not individually triaged)

Reproduction: AST sweep flags every keep-e2e test function (outside the shell/pty/platform/doc-script families) whose fixture args contain none of `registry, mirror_registry, legacy_registry, target_registry, published_package, published_two_versions, unique_repo, forward_proxy, identity_token, sigstore_stack`. Known high false-positive rate (the `ocx` fixture itself is registry-configured and not excluded) — do not treat a listing here as a verified port-capable case; it is a starting point for whoever ports these modules to check each flagged case's actual property-vs-setup shape.

| Module | Flagged candidate cases |
|---|---|
| `test_attest.py` | 1 |
| `test_catalog_readme.py` | 1 |
| `test_ci_export.py` | 3 |
| `test_clean.py` | 3 |
| `test_clean_project_backlinks.py` | 10 |
| `test_cross_repo_dedup.py` | 1 |
| `test_env.py` | 1 |
| `test_env_list.py` | 3 |
| `test_exit_codes.py` | 3 |
| `test_extra_ca_certs.py` | 9 |
| `test_fake_forge_deletions.py` | 4 |
| `test_fake_forge_mergeability.py` | 3 |
| `test_frozen.py` | 1 |
| `test_git_http_fixture.py` | 41 |
| `test_index.py` | 5 |
| `test_index_determinism.py` | 1 |
| `test_inspect.py` | 1 |
| `test_install_libc.py` | 4 |
| `test_lock.py` | 27 |
| `test_login.py` | 27 |
| `test_managed_config.py` | 1 |
| `test_offline.py` | 2 |
| `test_offline_portability.py` | 1 |
| `test_package_claim.py` | 43 |
| `test_package_create_libc_lint.py` | 14 |
| `test_package_info.py` | 5 |
| `test_package_inspect.py` | 4 |
| `test_pinned_offline.py` | 1 |
| `test_plugin_dispatch.py` | 14 |
| `test_project_add.py` | 27 |
| `test_project_add_render.py` | 1 |
| `test_project_env.py` | 38 |
| `test_project_groups.py` | 7 |
| `test_project_hooks.py` | 2 |
| `test_project_lifecycle.py` | 2 |
| `test_project_pull.py` | 34 |
| `test_project_remove.py` | 7 |
| `test_project_toml_preservation.py` | 14 |
| `test_select.py` | 3 |
| `test_sign.py` | 5 |
| `test_tag_fallback.py` | 1 |
| `test_toolchain_cli.py` | 28 |
| `test_toolchain_env.py` | 44 |
| `test_toolchain_offline_after_pull.py` | 7 |
| `test_transport_git.py` | 55 |
| `test_trust_policy_signers.py` | 4 |
| `test_update.py` | 21 |
| `test_update_check_throttle.py` | 8 |
| `test_update_report.py` | 6 |
| `test_verify.py` | 6 |
| `test_warm_resolve_no_network.py` | 4 |

## Per-module classification (flat modules — homogeneous, class applies to every case)

| Module | Cases (collected/fn) | Class | Rubric / Target | Rationale |
|---|---|---|---|---|
| `test_announce.py` | 48/48 | keep-e2e | R2 | fake-forge harness = forge/network boundary; SSRF/token/PR state machine |
| `test_announce_e2e_evidence.py` | 40/29 | keep-e2e | R8 | docstring: pure logic, no registry/network/binary — tests announce_e2e.evidence python module, not ocx (R8-style, no Rust target) |
| `test_announce_gitlab.py` | 25/25 | keep-e2e | R2 | GitLabForge over fake-forge HTTP surface |
| `test_announce_orphans.py` | 3/3 | keep-e2e | R2 | index object removal via announce, registry-backed |
| `test_announce_push_file.py` | 3/3 | keep-e2e | R2 | push --tags-file -> announce --tags-file, registry-backed |
| `test_announce_refresh_and_name.py` | 7/7 | keep-e2e | R2 | announce refusals against committed index root, registry-backed |
| `test_archive_containment.py` | 9/9 | **port-capable** | ocx_package::create (archive extract) or ocx_util::archive | local `--extract` path-traversal containment guard, no registry |
| `test_assembly.py` | 6/6 | keep-e2e | R5 | hardlink-based assembly / symlink chain preserved under real OCX_HOME after install |
| `test_attest.py` | 27/27 | keep-e2e | R2 | real Sigstore stack (sigstore compose profile) + registry |
| `test_auto_verify.py` | 15/15 | keep-e2e | R2 | policy-gated auto-verify against real signed packages in registry |
| `test_bench_smoke.py` | 80/80 | keep-e2e | R8 | docstring: purely python, no network/subprocess/registry — validates bench harness scenario matrix, not ocx (R8-style) |
| `test_cascade.py` | 19/19 | keep-e2e | R2 | real pushes exercising platform-aware cascade |
| `test_catalog_readme.py` | 1/1 | keep-e2e | R8 | lints packaging/ocx/CATALOG.md in pure Python (repo-relative link check) — no `ocx package describe`, no registry, no ocx binary at all. R2 was wrong; this is python-tooling-not-ocx like R8's other members. |
| `test_ci_export.py` | 13/13 | keep-e2e | R2 | --ci export reads a composed env that needs an installed toolchain (registry) |
| `test_clean.py` | 7/7 | keep-e2e | R2 | GC over a registry-populated store |
| `test_clean_project_backlinks.py` | 10/10 | keep-e2e | R2 | backlink guard over registry-populated packages |
| `test_color.py` | 9/9 | **port-capable** | ocx_console (styling/theme render) | ANSI/--color output formatting, no registry, callable directly against a mock writer |
| `test_completion_ascii.py` | 17/2 | keep-e2e | R1 | drives real shell completion output paths (self activate --completion, shell completion) |
| `test_config_push.py` | 15/15 | keep-e2e | R2 | managed-config v2 publishes as an ordinary OCI package via real push |
| `test_config_setup.py` | 13/13 | **port-capable** | ocx_setup::apply_managed_config | docstring names the exact shared lib fn; config-only tier adoption, no binary bootstrap, no registry |
| `test_config_test.py` | 27/27 | **port-capable** | ocx_package_manager::managed_config::preview_managed_config | validates + previews the merge (writes/publishes/adopts nothing) via the shared preview_managed_config the `ocx config push` validator also calls — NOT ocx_config (corrected from the original ruling). Reads process env through ocx_config::env::mirrors()/insecure_registries() and ocx_config::patch::patches_from_env() while composing the 'what would this machine look like' preview, so C-SEAM's hermetic-env invariant (1) is needed, not optional. |
| `test_cosign_interop.py` | 5/5 | keep-e2e | R2 | real cosign 3.x binary + local Fulcio/Rekor |
| `test_cosign_matrix_attest.py` | 7/7 | keep-e2e | R2 | image-level DSSE attestation against real registry + cosign |
| `test_cosign_matrix_cosign_signs.py` | 12/9 | keep-e2e | R2 | cosign writes to real registry, ocx verifies |
| `test_cosign_matrix_extras.py` | 8/8 | keep-e2e | R2 | two-shape registry states / cosign interop |
| `test_cosign_matrix_ocx_signs.py` | 8/8 | keep-e2e | R2 | ocx signs to real registry, cosign verifies |
| `test_cross_platform_materialize.py` | 9/9 | keep-e2e | R2 | --platform fetches a foreign platform leaf from the registry |
| `test_cross_repo_dedup.py` | 1/1 | keep-e2e | R2 | cross-repo content-addressed dedup against real registry pushes |
| `test_dependencies.py` | 61/61 | keep-e2e | R2 | dependency resolution against registry-published packages |
| `test_deps_interpolation.py` | 10/10 | keep-e2e | R2 | env interpolation over registry-installed deps |
| `test_describe.py` | 9/9 | keep-e2e | R2 | pushes description data to __ocx.desc tag |
| `test_direnv.py` | 9/9 | keep-e2e | R1 | evaluates emitted shell lines in a real non-interactive bash |
| `test_doc_scripts.py` | 73/2 | keep-e2e | R6 | doc-script / recording fidelity: executes real .sh scripts through the Scenario harness |
| `test_doc_scripts_cast.py` | 14/14 | keep-e2e | R6 | cast-layer / recording fidelity spec |
| `test_doc_scripts_executor.py` | 15/15 | keep-e2e | R6 | drift-gate executor drives the real ocx binary against real doc scripts |
| `test_doc_scripts_one_tree.py` | 10/8 | keep-e2e | R6 | structural guard for the doc-script/recordings one-tree convergence (no ocx target to port to) |
| `test_doc_scripts_publish.py` | 8/8 | keep-e2e | R6 | publish-task / recording-fidelity contract (task + website glue, not ocx logic) |
| `test_entrypoints.py` | 24/21 | keep-e2e | R5 | generated launcher symlinks under a real materialized OCX_HOME |
| `test_entrypoints_crossplat.py` | 5/5 | keep-e2e | R6 | platform-conditional launcher smoke (skipif Windows/MSYS2) |
| `test_env.py` | 34/34 | keep-e2e | R2 | ocx package env auto-installs missing packages from the registry |
| `test_env_list.py` | 11/11 | keep-e2e | R2 | list-typed metadata through the real push product path |
| `test_exec.py` | 2/2 | keep-e2e | R4 | ocx exec — C-SEAM excludes exec verbs from the in-process seam |
| `test_exec_clean_home.py` | 2/2 | keep-e2e | R4 | --clean strips HOME; re-entrant launcher exec (process/env boundary) |
| `test_exec_forwarding.py` | 17/17 | keep-e2e | R4 | OCX_* config forwarding across a real subprocess spawn |
| `test_exec_modes.py` | 40/14 | keep-e2e | R4 | exec-mode visibility crossed against real subprocess env composition |
| `test_exec_rm.py` | 4/4 | keep-e2e | R4 | package exec --rm — child exit status + process-scoped cleanup |
| `test_execution_record_standards.py` | 16/16 | **port-capable** | ocx_package_manager (execution record: in-toto ResourceDescriptor / SLSA shape) | wire-shape standards compliance on the packages[] block; pure data-shape validation |
| `test_execution_records.py` | 38/38 | keep-e2e | R2 | exec-time resolution record over registry-resolved packages |
| `test_exit_codes.py` | 27/5 | keep-e2e | R3 | exit code AS THE PARENT SEES IT via minimal real subprocess failures (sysexits contract) |
| `test_extra_ca_certs.py` | 25/23 | keep-e2e | R2 | HTTPS index/registry against a minted corp CA (network + TLS trust) |
| `test_fake_forge_deletions.py` | 4/4 | keep-e2e | R2 | pins the fake-forge harness's own deletion semantics (forge-protocol fixture) |
| `test_fake_forge_mergeability.py` | 3/3 | keep-e2e | R2 | pins the fake-forge harness's own mergeability computation (forge-protocol fixture) |
| `test_frozen.py` | 14/14 | keep-e2e | R2 | --frozen tag-resolution refusal against local index vs live registry |
| `test_git_http_fixture.py` | 41/41 | keep-e2e | R2 | executable spec for the git-over-HTTP fixture + git shim (forge/network fixture) |
| `test_global_toolchain.py` | 16/16 | keep-e2e | R2 | --global tier pulls tools from the registry |
| `test_golden_fixtures.py` | 2/2 | keep-e2e | R8 | python fixture self-check (SBOM fixture consistency), no ocx binary — python test tooling |
| `test_index.py` | 19/19 | keep-e2e | R2 | local index collection populated via registry updates |
| `test_index_determinism.py` | 4/4 | keep-e2e | R2 | index pin determinism across registry tag moves |
| `test_index_ocx_sh.py` | 34/33 | keep-e2e | R2 | index.ocx.sh two-hop resolve against an HTTP fixture (network) |
| `test_index_selfcontained.py` | 15/15 | keep-e2e | R2 | self-containment/verifiability of an index shipped/synced from a registry-backed source |
| `test_index_servable_snapshot.py` | 29/29 | keep-e2e | R2 | index sync/snapshot against a real source |
| `test_inspect.py` | 9/9 | keep-e2e | R2 | toolchain-tier inspect over registry-resolved bindings |
| `test_inspect_no_index_growth.py` | 1/1 | keep-e2e | R2 | read-only package inspect resolves content-addressed through the registry |
| `test_install.py` | 9/9 | keep-e2e | R2 | registry-backed install |
| `test_install_libc.py` | 10/10 | keep-e2e | R2 | libc-aware install resolution against registry-published platform variants |
| `test_launcher_exec.py` | 7/7 | keep-e2e | R4 | hidden `ocx launcher exec` internal subcommand — process re-entry ABI |
| `test_layer_layout.py` | 4/4 | keep-e2e | R2 | per-layer strip/prefix against pushed/pulled layered packages |
| `test_lazy_direnv.py` | 4/4 | keep-e2e | R1 | evaluates emitted direnv lines in a real bash, invokes the shimmed name |
| `test_lazy_loading.py` | 27/26 | keep-e2e | R2 | lazy tool materialization via registry-backed shims |
| `test_lock.py` | 27/27 | keep-e2e | R2 | ocx lock resolves every tool's tag to a digest against the fixture registry |
| `test_logging.py` | 15/2 | keep-e2e | R4 | global tracing subscriber wiring (C-SEAM invariant 4 forbids global installs) |
| `test_login.py` | 29/29 | keep-e2e | R2 | ocx login/logout against registry auth |
| `test_managed_config.py` | 36/36 | keep-e2e | R2 | managed-config tier adoption incl. registry-backed publish/fetch scenarios |
| `test_multi_layer.py` | 16/16 | keep-e2e | R2 | multi-layer push/pull lifecycle against real registry |
| `test_oci_registry_mirror.py` | 12/12 | keep-e2e | R2 | client-declared registry mirror routed at the real network seam |
| `test_offline.py` | 6/6 | keep-e2e | R2 | offline resolution semantics against a registry-warmed cache |
| `test_offline_portability.py` | 7/7 | keep-e2e | R2 | copies blobs/layers/index out of a registry-warmed OCX_HOME |
| `test_offline_verify.py` | 6/6 | keep-e2e | R2 | offline verify scopes to Sigstore trust services (network-adjacent) |
| `test_package_cascade.py` | 15/15 | keep-e2e | R2 | real pushes + hand-corrupted alias via direct HTTP PUT |
| `test_package_claim.py` | 54/45 | keep-e2e | R2 | REST claim against a forge API + argv-boundary git gate |
| `test_package_copy.py` | 29/29 | keep-e2e | R2 | promotes a published package between registries |
| `test_package_create_bin_scan.py` | 19/19 | **port-capable** | ocx_package::create (interface-binaries auto-scan) | local directory scan writing the create-time sidecar, no registry |
| `test_package_create_extract.py` | 18/12 | **port-capable** | ocx_package::create / ocx_util::archive (--extract) | local archive unpack into a temp content root, no registry |
| `test_package_create_libc_lint.py` | 14/14 | keep-e2e | R7 | proves the CLI actually invokes the already-unit-tested libc_lint check — wiring proof, not logic (ambiguous, kept conservatively) |
| `test_package_create_pinning.py` | 13/13 | keep-e2e | R2 | resolves tag-only deps to a platform manifest digest against the registry |
| `test_package_info.py` | 12/12 | keep-e2e | R2 | reads description metadata pushed to __ocx.desc |
| `test_package_inspect.py` | 15/15 | keep-e2e | R2 | registry-resolved metadata/layers/resolution chain |
| `test_package_inspect_closure.py` | 12/12 | keep-e2e | R2 | metadata-only dependency closure walker over registry-resolved deps |
| `test_package_lifecycle.py` | 1/1 | keep-e2e | R2 | full lifecycle: create, push, index update, install, find |
| `test_package_pull.py` | 4/4 | keep-e2e | R2 | ocx package pull against registry |
| `test_package_push.py` | 12/12 | keep-e2e | R2 | push --format json report against a live registry |
| `test_package_push_annotations.py` | 12/12 | keep-e2e | R2 | OCI annotation on the pushed index, asserted against a live registry |
| `test_package_push_gate.py` | 23/23 | keep-e2e | R2 | push dependency gate refusal against registry-resolvable pins |
| `test_package_push_mount.py` | 1/1 | keep-e2e | R2 | cross-repository blob mount against the live registry:2 fixture |
| `test_package_receipt.py` | 4/4 | **port-capable** | ocx_cli (build_receipt::read, conventions::infer_receipt_file, api::data::package_receipt) — CLI-bound, NOT below ocx_cli | prints the <stem>-receipt.json sidecar create wrote; local file read, but the reading logic (build_receipt, infer_receipt_file, package_receipt data model) all lives in ocx_cli itself, not a lower crate. Discounted from the below-ocx_cli threshold. |
| `test_package_test.py` | 25/25 | keep-e2e | R4 | materializes + executes a command in the composed env (process boundary) |
| `test_package_test_script.py` | 43/43 | keep-e2e | R4 | embedded Starlark runner drives the real ocx binary + materialized package |
| `test_patch_smoke.py` | 1/1 | keep-e2e | R2 | smoke happy path against the patch registry (UUID-scoped, still registry-backed) |
| `test_patches.py` | 63/63 | keep-e2e | R2 | companion package discovery/install against registry |
| `test_path_idempotency.py` | 18/4 | keep-e2e | R1 | drives real ocx package env --shell output through an actual interpreter |
| `test_pinned_offline.py` | 1/1 | keep-e2e | R2 | ocx lock + pull then OCX_OFFLINE=1 exec against pinned digests |
| `test_platform_pairs.py` | 12/4 | **port-capable** | ocx_oci / ocx_package (Platform / SUPPORTED_PAIRS) | OS x Arch pair legality + wasm platform parsing, pure validation logic |
| `test_plugin_dispatch.py` | 14/14 | keep-e2e | R7 | only case reaching the plugin-dispatch verb surface end-to-end via the compiled binary |
| `test_progress_channel.py` | 6/5 | keep-e2e | R1 | assertions on bytes a controlling terminal received (pty-only observer) |
| `test_project_add.py` | 27/27 | keep-e2e | R2 | ocx add resolves bindings against the registry |
| `test_project_add_render.py` | 1/1 | keep-e2e | R2 | no-op batch render, setup requires a registry-resolved add |
| `test_project_concurrency.py` | 5/5 | keep-e2e | R4 | concurrent real subprocess invocations racing the mutation lock |
| `test_project_config.py` | 13/13 | **port-capable** | ocx_project (config path discovery) | Phase 1: --project/OCX_PROJECT path discovery only, no registry |
| `test_project_config_home_walk.py` | 1/1 | **port-capable** | ocx_project (CWD walk) | pure local walk logic: $OCX_HOME/ocx.toml must not be adopted as a project |
| `test_project_crash_recovery.py` | 4/4 | keep-e2e | R4 | kills a real child process mid-mutation to prove crash recovery (process boundary) |
| `test_project_env.py` | 47/42 | keep-e2e | R2 | composed [env] over registry-installed deps |
| `test_project_groups.py` | 7/7 | keep-e2e | R2 | per-group binding uniqueness via ocx add/remove against the registry |
| `test_project_hooks.py` | 2/2 | keep-e2e | R2 | ocx run env composition over a registry-resolved toolchain |
| `test_project_init.py` | 15/14 | **port-capable** | ocx_project (init scaffold) | creates ocx.toml locally, no registry |
| `test_project_lifecycle.py` | 2/2 | keep-e2e | R2 | init -> add -> update -> pull -> remove roundtrip against the registry |
| `test_project_pull.py` | 37/34 | keep-e2e | R2 | ocx pull project-config path against the registry |
| `test_project_remove.py` | 7/7 | keep-e2e | R2 | ocx remove — setup requires a registry-resolved add |
| `test_project_run.py` | 26/26 | keep-e2e | R4 | ocx run executes a real child process over the composed env |
| `test_project_toml_preservation.py` | 14/14 | **port-capable** | ocx_project::mutate / ocx_project::document (comment/order-preserving TOML mutation) | the property under test — a mutation touches only the key it mutates, comments and #:schema directive survive — is pure ocx_project TOML-editing logic (toml_edit-based), independent of the registry digest add/remove currently uses to construct the input; a port supplies a synthetic already-resolved binding directly to the mutate/document API. (Reclassified from keep-e2e R2 per the registry-boundary rule below — setup needing a registry does not make the property under test registry-dependent.) |
| `test_proxy_registry.py` | 4/4 | keep-e2e | R2 | SSRF guard honouring the HTTP proxy route, real forward-proxy + registry |
| `test_pull_progress.py` | 2/2 | keep-e2e | R1 | progress-bar rendering, pty-only observer |
| `test_purge.py` | 11/11 | keep-e2e | R2 | dependency-aware scoped GC over a registry-populated store |
| `test_push.py` | 15/12 | keep-e2e | R2 | push --sign / --sbom / --rekor-upload against a live registry + Sigstore |
| `test_push_pull_three_layers.py` | 3/3 | keep-e2e | R2 | three-layer push/pull round trip against a live registry |
| `test_referrers_capability.py` | 2/2 | keep-e2e | R2 | referrers-API capability cache against the registry |
| `test_referrers_fallback.py` | 3/3 | keep-e2e | R8 | pins the registry fixture's own referrers-fallback tag behaviour (harness validation, not ocx code) |
| `test_referrers_smoke.py` | 2/2 | keep-e2e | R8 | infrastructure smoke test for the referrers-capable harness itself |
| `test_registry_startup_retry.py` | 3/3 | keep-e2e | R8 | unit test for conftest._wait_for_reachable — pure Python retry-loop, not ocx |
| `test_resolution_chain_refs.py` | 21/21 | keep-e2e | R2 | full OCI resolution chain captured against registry pulls |
| `test_run_global_isolation.py` | 5/5 | keep-e2e | R4 | --global run composes env for one child process only (process isolation boundary) |
| `test_sbom.py` | 33/33 | keep-e2e | R2 | reads SBOM referrers off a live registry |
| `test_scenarios_smoke.py` | 18/1 | keep-e2e | R1 | discovers and executes real .sh scenario scripts |
| `test_schema.py` | 9/9 | **port-capable** | ocx_package::metadata::bundle (Bundle.entrypoints serde round-trip) | JSON Schema + TOML/JSON round-trip for the entrypoints field is a serde-level property of ocx_package::metadata::bundle::Bundle, no registry or binary needed to construct or assert it. (Reclassified: the original R6 'doc-script/recording fidelity' citation was wrong — this is neither a doc-script test nor platform-gated.) |
| `test_select.py` | 6/6 | keep-e2e | R2 | install variant selection against the registry |
| `test_self_activate.py` | 35/30 | keep-e2e | R1 | drives real shell activation |
| `test_self_setup.py` | 62/54 | keep-e2e | R4 | self setup bootstrap installs from ocx.sh/ocx/cli — process/global bootstrap |
| `test_self_update.py` | 13/13 | keep-e2e | R4 | end-to-end self-update install path (process replaces itself) |
| `test_session_path.py` | 10/10 | keep-e2e | R6 | writes real OS session-PATH state (HKCU Environment, environment.d, LaunchAgent) |
| `test_shell_activation.py` | 17/10 | keep-e2e | R1 | drives real login shells (bash/dash/fish/nu/elvish/pwsh) |
| `test_shell_hook.py` | 22/22 | keep-e2e | R1 | global toolchain activation via a real shell + direnv export |
| `test_shell_reconcile.py` | 301/91 | keep-e2e | R1 | per-prompt reconciler driven through the real shell zoo |
| `test_shell_reconcile_edge_cases.py` | 211/134 | keep-e2e | R1 | named edge-case corpus, each row a real pytest-hostshell/pytest-shellzoo run |
| `test_sign.py` | 37/37 | keep-e2e | R2 | real Rust sign pipeline against real Sigstore + registry |
| `test_sign_platforms.py` | 16/5 | keep-e2e | R2 | signs a published multi-manifest package against the registry |
| `test_sigstore_stack_smoke.py` | 4/4 | keep-e2e | R2 | single gate proving real Sigstore services are reachable |
| `test_state_providers.py` | 122/31 | keep-e2e | R8 | ADR-named exemplar — tests Python doc-command state-provider tooling, not ocx |
| `test_state_registry_characterization.py` | 14/14 | keep-e2e | R8 | characterizes legacy Python SETUPS/SCENARIOS registries, not ocx |
| `test_status.py` | 11/10 | **port-capable** | ocx_cli (api::data::status::StatusReport) — CLI-bound, NOT below ocx_cli | docstring: no resolution, no registry, no platform selection — but StatusReport itself is defined and computed in ocx_cli::api::data::status, not a lower crate. Discounted from the below-ocx_cli threshold. Caveat: also the ONLY module reaching `ocx status` (R7) — a port must be executed and mutation-proven (C-025 deletion guard) before the original is removed, which is how R7's coverage concern is actually retired here, not by classification alone. |
| `test_tag_fallback.py` | 8/8 | keep-e2e | R2 | transparent tag fallback against the ChainedIndex/registry |
| `test_tag_reserved.py` | 7/7 | keep-e2e | R2 | reserved-tag refusal proven at real push time against the registry |
| `test_taplo_project_toolchain.py` | 3/3 | keep-e2e | R1 | drives the real external `taplo check` CLI |
| `test_toolchain_activate.py` | 34/22 | keep-e2e | R1 | drives a real bash process, reads PATH back out of it |
| `test_toolchain_cli.py` | 34/28 | keep-e2e | R2 | toolchain-home CLI wiring over registry-pulled tools |
| `test_toolchain_env.py` | 52/47 | keep-e2e | R2 | composed toolchain env over registry-pulled tools |
| `test_toolchain_offline_after_pull.py` | 8/8 | keep-e2e | R2 | offline-after-pull correctness against a registry-warmed lock |
| `test_toolchain_render.py` | 71/54 | keep-e2e | R1 | rendered toolchain tree, group-emitter healing via real shell/PATH checks |
| `test_trampoline_exec.py` | 19/19 | keep-e2e | R4 | rendered <home>/bin/<name> actually run as a process |
| `test_transport_git.py` | 76/55 | keep-e2e | R2 | --transport git write path against a real git-over-HTTP fixture + forge |
| `test_trust_policy.py` | 15/15 | keep-e2e | R2 | [[trust.policy]] resolution against real signed packages in the registry |
| `test_trust_policy_signers.py` | 8/8 | keep-e2e | R2 | signers array against real Sigstore signatures |
| `test_trust_root_distribution.py` | 6/6 | keep-e2e | R2 | trust-root ladder against real Sigstore trust services |
| `test_uninstall.py` | 3/3 | keep-e2e | R2 | uninstall a package that was registry-installed |
| `test_update.py` | 21/21 | keep-e2e | R2 | ocx update re-resolves against the live registry |
| `test_update_check_throttle.py` | 8/8 | keep-e2e | R2 | throttle window gates a real registry query |
| `test_update_report.py` | 6/6 | keep-e2e | R2 | diff report between predecessor lock and registry-resolved candidate |
| `test_variants.py` | 5/5 | keep-e2e | R2 | variant install/select/discovery against the registry |
| `test_verify.py` | 43/43 | keep-e2e | R2 | package verify against the real Sigstore trust-root + registry |
| `test_warm_resolve_no_network.py` | 4/4 | keep-e2e | R2 | asserts zero index-source HTTP calls — network-observability is the property under test |
| `test_which.py` | 6/6 | keep-e2e | R2 | resolves a registry-pulled tool's location |
| `test_windows_shim.py` | 14/12 | keep-e2e | R6 | genuinely Win32-dependent shim behaviour (CreateProcessW, Ctrl+C forwarding) |
