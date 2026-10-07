# Validation review: ADR + system design, OCX machine-interface contract (round-1 fixes)

Reviewer: Opus, spec seat, hex-architect Review-Fix Loop single re-validation pass. Repo `/home/mherwig/dev/ocx` at `34337a6a2`.
Under review: `adr_ocx_interface_contract.md` (ADR), `system_design_ocx_interface_contract.md` (SD), against the five round-1 reviews.

Summary: **Needs Work** (one remaining Block, partial fix of SP-B3/CX-6; text-only patch).
Focus: spec-compliance (plan-artifact scope).
Blocks: 20 in round 1 (spec 4, quality 2, security 3, Codex 11; SOTA labels none Block). **18 resolved, 2 partial** (SP-B3 and CX-6 are one defect).
New/remaining: Block 1, Warn 9, Suggest 10. All actionable; none deferred.

Evidence method: read both artifacts and all five reviews in full; checked arithmetic of the trade-off matrix (62/68/75/77/81 and 60/67/78/75/80, both reproduce);
read `scripts/crate_map.toml`, `adr_crate_split_workspace.md` (crate map, § "`env` splits", § Stability tiers), `.claude/rules/arch-principles.md` crate table,
`taskfiles/release.taskfile.yml` (`prepare`), `taskfiles/satellite.taskfile.yml`, `crates/ocx_util/src/env.rs`, `crates/ocx_test_support/src/lib.rs`;
grepped env-seam use per crate; listed local tags; read ocx-mirror's root `Cargo.toml`, `crates/crate_map.toml`, `MODULE.bazel`,
`crates/ocx_mirror_pipeline/src/ocx_cli/sign.rs` (`SweepEnvelope`), `ocx_cli/sign/tests.rs` (stub-binary tests) and `test/conftest.py` (real-binary fixtures).

---

## 1. Block status (every round-1 Block)

| Id | Finding | Status | Where verified |
|---|---|---|---|
| SP-B1 | `schema_version` in nested root `$def`s | Resolved | SD 3.2 `<Name>Root` wrappers, payload `$def` untouched; L14; ADR Root convention 3; V8 red (nest a root) reachable |
| SP-B2 | Baseline lint red on the release commit | Resolved | ADR § Baseline rotation; SD 3.9. Walked against `release.taskfile.yml`: `prepare` runs with HEAD = previous commit (T = RELEASE, green); release commit HEAD = T (green, old baseline); next non-rotation commit reds "rotation pending"; rotation commit green. Residual: W2, W3 below |
| SP-B3 | ocx-mirror gate cannot red on a JSON break | **Partial** | See B1 below |
| SP-B4 | Phase 0 breaks mirror compile | Resolved | ADR Phases row 0 (mirror env migration, exit `satellite:verify` + `satellite:contract`); Consumers row; SD 3.4 Satellite API (`#[macro_export]`, `dynamic()` ratchet scoped to `//crates/...`). Residual: W6 |
| Q-B1 | Differ is a denylist | Resolved | SD 3.7 keyword allowlist; L17; U01 in either document; keyword reader floor (SD 3.8 item 2, SD 3.9); V3/V6 `allOf`/`minItems` reds. Residual: W4 |
| Q-B2 | Mirror phase-0 lockstep row | Resolved | as SP-B4 |
| SE-B1 | Testing is a naming rule, read ungated | Resolved | SD 3.4 `env_vars!` cfg expansion, excluded from `DECLARED`; V13 `cargo build --release -p ocx --locked` red; SD § 6 row 1 names the cfg gate. Red reachable: the static does not exist without `ocx_env/__testing` |
| SE-B2 | Argv option injection (CWE-88) | Resolved | ADR SDK contract; SD 3.15 Argv; `ArgSpec.allow_hyphen_values/last/value_terminator`; V18 `--env=X=1` refused |
| SE-B3 | Renamed env vars fail open | Resolved | ADR Env contract (window warn, exit 78 after, polarity never aliased, hardening `on_invalid = Error`); SD 3.4 `Retired`; V14 red. Residual: W7 (V14 has no green) |
| CX-1 | `NeverNull` erases L03 evidence | Resolved | SD 3.2 strips `null` only from non-`required` properties under `for_serialize`; V9 red (remove `skip_serializing_if` → required + nullable → L03) reachable; runtime hook for custom `Serialize` |
| CX-2 | Opaque publisher payloads vs never-null | Resolved | `OpaqueJson`, `x-ocx-opaque`, leaf exemption; V10. Residual: W7 (V10 has no red) |
| CX-3 | Grammar drops clap semantics | Resolved | SD 3.6 `ArgSpec`/`GroupSpec`; G04, G07, G10, G11; V5 probe parses valid/invalid argv per relation |
| CX-4 | No command→response mapping | Resolved | `OutputMode` + `CONTRACT` table; C09; V11 "observed root not in declared modes → red" |
| CX-5 | SDK cannot represent report-then-fail | Resolved | `Outcome<R>`; V18 partial sign and mixed sweep |
| CX-6 | V11 red not exercised by named gate | **Partial** | same defect as SP-B3; see B1 |
| CX-7 | Phase-0 mirror env API deletion | Resolved | as SP-B4 |
| CX-8 | L04 rejects the errors document | Resolved | `x-ocx-enum` scalar form; L04 rewritten; SD 3.3 `ExitCode`/`ErrorCategory`/`ErrorDetail` as scalar enums |
| CX-9 | Baseline rotation vs release sequence | Resolved | as SP-B2 |
| CX-10 | Root removal/addition unreachable green | Resolved | SD 3.9 three algorithms; step 4 checks surviving only; V6 greens for acknowledged deletion and new root at 1 |
| CX-11 | `cli` mismatch only warns | Resolved | per-command `CommandSpec.version`; `contract` table in `version`; refusal before spawn; V18 "only a command's `cli` version changed → zero spawns" |

Warn sample (verified resolved): Q-W4, Q-W5 (D01), Q-W6 (option b), Q-W7 (Reversibility rewritten), Q-W8 (new crate; see § 2), SE-W5 (`^__OCX_(?!TESTING_)`),
SE-W8 (pattern + `rev-parse --end-of-options`; see W2), SE-W9 (fail-closed predicates), SP-W3 (`exemptions.toml`), SP-W12 (`#[cfg(test)]` seed in V0), SP-W16 (floor = measured − 3), SO-G1, SO-G2, SO-G6, SO-G8.
Partially resolved Warn: SP-W13 (tier fixed; `crates/ocx_sdkgen/BUILD.bazel`, `crates/ocx_env/BUILD.bazel`, golden `exports_files` and the `ocx.sh/ocx/sdkgen` release step still absent from SD § 4; see S6).

Dispositions table: every finding id of the five reviews maps to a row (spec W1–W16, S1–S11; quality W1–W11, S1–S8; security B/W/S/D; SOTA G1–G8 + notes; Codex 1–13). Count 86+3+2+3 = 94 matches the stated total with SP-W10(c) as a sub-part.

---

## 2. The `ocx_env` design change

| Question | Answer |
|---|---|
| Consistent with `scripts/crate_map.toml`? | **No, edge set wrong** (W5). SD § 4 lists `ocx_env = []`, `ocx_util += ocx_env`, `ocx_schema += ocx_exit, ocx_env`. `ocx_util` reads no env var itself (only `crate::env::PATH_SEPARATOR` and `std::env::current_dir`), so its edge is dead weight; the ~15 crates that call the seam today (`ocx_console`, `ocx_oci`, `ocx_trust`, `ocx_sign`, `ocx_config`, `ocx_store`, `ocx_index`, `ocx_package`, `ocx_shell`, `ocx_project`, `ocx_package_manager`, `ocx_announce`, `ocx_script`, `ocx_setup`, `ocx`) each need a direct `ocx_env` edge, or `deps_direction` reds. A re-export from `ocx_util` instead is the compat shim CLAUDE.md bans |
| Consistent with `adr_crate_split_workspace.md` / tiers? | Tier choice is right: ecosystem = "a crate a lockstep submodule consumer links", and mirror links it directly. But the ADR amends only "crate map (`ocx_env`, `ocx_sdkgen`, `ocx_schema` edges)"; it does not name the superseded § "`env` splits" ruling (vocabulary/accessor → `ocx_config`), the § Stability tiers ecosystem list, or the `ocx_config` charter row (W5) |
| Consistent with the CLAUDE.md crate table? | Architecture table row is planned (§ CLAUDE.md amendment). Missing: the **Stability tiers** ecosystem-crate enumeration (`ocx_util`, `ocx_console`, … `ocx_python`) and `.claude/rules/arch-principles.md`'s crate table with edges (W5) |
| Who edits `crate_map.toml`? | Stated: Required artifacts row "`scripts/crate_map.toml` rows and the Rust mirror table — 0 / 2 — builder" |
| ocx-mirror consequences counted? | **Partly** (W6). Counted: env API migration, own declarations, `CREDENTIAL_KEYS`, both satellite gates. Not counted: mirror root `Cargo.toml` `[workspace.dependencies]` needs an `ocx_env` row (each row is "a surface this binary names directly"); `crates/crate_map.toml` `[ocx] allowed` needs `ocx_env` or mirror's `tests/workspace_structure.rs` reds; mirror's Bazel `@crates//:ocx_env` deps and lock repin; SD 3.4's "Mirror bans `std::env` with its own `clippy.toml`" is stated as fact but appears in no ADR row, and mirror's own tests call `std::env::set_var` (`ocx_cli/sign/tests.rs:620,653`) |
| Rebuild fan-out stated honestly? | **Understated** (W8). NFR § Build: "rebuild its dependents (most crates)". `EnvVar.doc` is a `&'static str` compiled into the binary, so an env doc-comment edit changes the bytes of `//crates/ocx_cli:ocx`, which per CLAUDE.md re-runs every one of the 171 cached acceptance targets, plus `rust:build`, clippy and unit tests across the workspace |

---

## 3. Findings (remaining or new)

### Block

**B1. `satellite:contract` still cannot red on the sweep-envelope break (SP-B3/CX-6 partial).**
- Anchor: ADR § Consumers (ocx-mirror phase 1: "`satellite:contract` red without it"); ADR V20; SD § 3.12.
- Defect: the gate now runs "mirror's integration tests for its 6 spawn sites" against the candidate binary, but nothing makes mirror's decoders fail on a shape change. Mirror's `SweepEnvelope { #[serde(default)] data }` (`ocx-mirror/crates/ocx_mirror_pipeline/src/ocx_cli/sign.rs:370-374`) documents that a mis-shaped sweep document "silently yielded an empty `tags` and narrated nothing, for every sweep this pipeline has ever run": its suite did not catch that. Phase 1 unwraps exactly that envelope. Mirror's Rust tests drive stub binaries on `OCX_BINARY_PIN` (`sign/tests.rs:596-668`), so they never see the candidate either. Only the Python acceptance suite (`test/conftest.py` `ocx_binary`/`real_ocx_binary`) runs a real `ocx`. The design also does not name that suite or its override variable (`OCX_COMMAND`/`OCX_TEST_BINARY`). So V20's red is not established for the root the ADR's own review cites, and phase 1's N5 enforcement for the subprocess surface rests on it.
- Remediation: (a) ADR § Consumers phase 0 for ocx-mirror adds "strict decode at the 6 sites: no `#[serde(default)]` on any field mirror acts on, one assertion per site in mirror's acceptance suite on a parsed field". (b) SD § 3.12 names the suite (`ocx-mirror/test`, `OCX_COMMAND`/`OCX_TEST_BINARY` pointing at the candidate) and floors it on the 6 sites exercised. (c) V20 red runs per parsed root, with the sweep unwrap as the named case.

### Warn

**W1. Released versions ship ungated between phase 1 and the bootstrap rotation, and indefinitely under no-go.**
- Anchor: ADR § Versioning ("emission … in the last phase-1 release"); § Baseline rotation "Bootstrap" ("first release after phase 1 is cut with no gate"); § Phases row 2 ("No-go = stop at C").
- Defect: published `schema_version` integers have no rule from the first emitting release until the gate exists. The ADR does not say whether "the first release after phase 1" is that same release. Phase 2 opens with a go/no-go, so the window spans every release until the spikes finish. Under no-go, versions stay emitted with no bump rule forever: the drift Q-W6 closed for phase 1 reopens after it.
- Remediation: name the bootstrap tag as the release that first emits `schema_version`, and rotate to it right away. For the interval and for no-go, adopt Q-W6 option (c): any golden change to a root bumps that root (a byte check, no differ).

**W2. "Newest tag" is not restricted to release tags.**
- Anchor: SD § 3.9 T0 lint, third bullet ("Let `T` = the newest tag that is an ancestor of `HEAD`").
- Defect: the pattern applies to `RELEASE` only. This repo carries non-release tags (`0.3.1`, `backup/*`, `pre-rebase-lazy`, `finalize-backup-*`, `recovered/*`, listed at `34337a6a2`). A backup tag on an ancestor newer than the last release reds "rotation pending" on a clean tree. SE-W8's fix text said "newest `v*` tag".
- Remediation: T = newest ancestor tag matching `^v\d+\.\d+\.\d+$` (`git describe --tags --abbrev=0 --match 'v[0-9]*'` plus the regex). Add a V7 green: a non-release tag newer than `RELEASE` leaves the lint green.

**W3. A `doc` ledger entry forces an `errors` bump.**
- Anchor: SD § 3.9 gate step (4) ("For `errors`, … = baseline + 1 when any entry exists"); ADR § Versioning table (description change: `doc` = no bump, "same" for `errors`).
- Defect: the two texts contradict. Read literally, a `doc`-only edit to the errors document has no green state.
- Remediation: in step (4), change "any entry" to "any breaking or `semantic` entry". Add a V6 green: an `errors` description change with a `doc` entry and no bump.

**W4. Sub-fields of `x-ocx-enum` entries are outside the allowlist's reach.**
- Anchor: SD § 3.7 keyword allowlist; § 3.9 (R02/R03 cover `category` and `exit_code` only).
- Defect: the allowlist closes keywords but not the members of an `x-ocx-enum` entry (`value`, `name`, `description`, `category`, `exit_code`). A `name` change renames the generated SDK variant, which breaks SDK source, and no rule sees it. An unknown member passes silently. This is Q-B1's denylist shape one level down.
- Remediation: give each entry member a classification (`value` removed → B05; `name` changed → breaking, new code; `description` → D01; `category` → R02; `exit_code` → R03). An unlisted member is U01. Add one corpus case each.

**W5. Crate-map edges and amendment targets for `ocx_env` are wrong or incomplete.**
- Anchor: SD § 4 row `crates/ocx_env/`; ADR Metadata "Amends"; ADR § CLAUDE.md amendment.
- Defect and remediation: see § 2, rows 1–3. List the direct `ocx_env` edge for every crate that reads env, drop `ocx_util += ocx_env` unless a use is named, and keep `PATH_SEPARATOR` in `ocx_util`. Add to the amendment list: `adr_crate_split_workspace.md` § "`env` splits" (superseded), its § Stability tiers ecosystem list and `ocx_config` charter; CLAUDE.md § Stability tiers ecosystem enumeration; the `arch-principles.md` crate table.

**W6. Mirror-side consequences of a new ecosystem crate are not counted.**
- Anchor: ADR § Consumers ocx-mirror phase 0; SD § 3.4 Satellite API.
- Defect: see § 2, row 5.
- Remediation: the phase-0 mirror row lists the `ocx_env` workspace-dependency row, the `crates/crate_map.toml` `[ocx] allowed` entry, the Bazel deps and lock repin. Either list mirror's `clippy.toml` ban with its residue (`set_var` in tests), or mark it optional for mirror.

**W7. Two V rows lack a side.**
- Anchor: ADR § Verification V10 (red "—"), V14 (green "—"); the section's own rule "Every gate has a reachable red and green".
- Remediation: V10 red: let a transform or lint descend into an `x-ocx-opaque` leaf (for example `NeverNull` without the leaf guard), so a payload `null` is stripped or L03 fires. V14 green: replacement set and retired name unset → no warning, exit 0; retired name in window with replacement unset → old value honoured.

**W8. The rebuild fan-out of `ocx_env` is understated.**
- Anchor: ADR § NFR coverage, Build row.
- Remediation: state that any `ocx_env` edit, doc text included, changes the `ocx` binary and re-runs all acceptance targets. Either accept that in the row, or keep `doc` out of the binary: `doc` is read only by `ocx_schema`, for example through a separate docs table or a target that the binary does not link.

**W9. The runtime conformance hook cannot catch an unregistered enum value emitted by `ocx`.**
- Anchor: SD § 3.10; § 3.7 scalar enum ("Open by construction: a validator accepts any string/integer").
- Defect: open schemas are right for consumers, but the producer-side hook inherits that openness. If `ocx` emits a status value missing from `x-ocx-enum`, it passes. Every generated SDK then maps the value to `Unknown`, and `is_success()` returns false.
- Remediation: the hook validates `ocx`'s own output closed. Every emitted scalar-enum value must appear in its `x-ocx-enum`, and every union `type` must match a known arm. V11 red: emit an unlisted status.

### Suggest

- **S1. SD § 3.5 residue names a seam that does not exist.** The override table is `ocx_util::env::overrides` (to `ocx_env::overrides`, SD 3.4), not "`ocx_test_support`'s override seam". Fix both sentences in § 3.5.
- **S2. "std only" for `ocx_env`.** The moved `overrides` module uses `tempfile::TempDir` under `__testing`. Say "std only outside `__testing`".
- **S3. The argv pre-scan for `--format json` must stop at `--`.** Otherwise `ocx exec -- tool --format json` turns a usage error into a JSON document the user never asked for (SD § 3.3 "One document per invocation").
- **S4. Open question 3's recommendation adds `RENAMED_ENV`.** `test_deprecated_spellings.py` reads `RENAMED`, so the recommendation should also make the sweep read `RENAMED_ENV`.
- **S5. The differ needs the current release version** to judge "hidden-deprecated item removed in its named `removal` release". Name its source: `CARGO_PKG_VERSION` of `ocx_sdkgen`, which is the same workspace version.
- **S6. SD § 4 file map** lacks `crates/ocx_env/BUILD.bazel`, `crates/ocx_sdkgen/BUILD.bazel`, the `exports_files`/filegroup for goldens in `crates/ocx_schema/BUILD.bazel`, the root `Cargo.toml` workspace rows, the `ocx_cli` `__testing` forward of `ocx_env/__testing` (policed by `testing_feature_forward_list_matches_grep`), and the `ocx.sh/ocx/sdkgen` publish step.
- **S7. `env_vars!` in a satellite.** The expansion's `feature = "__testing"` resolves in the caller crate, so mirror needs a declared `__testing` feature, or `unexpected_cfgs` fires under `-D warnings`. Say so in the Satellite API paragraph.
- **S8. Naming drift.** The ADR says Testing vars are "absent from `ALL`"; the SD uses `DECLARED` and `all()`. Pick one.
- **S9. The unknown-variant arm's `not.enum` list** changes with every variant added or removed. The differ should derive and skip that arm, or a variant addition reads as an enum change inside `not` and a removal is reported twice.
- **S10. The hermetic walk.** With C07 forcing literal defaults, the clap build no longer reads env, so running the build-time walker under `overrides::hermetic()` only drags `ocx_env/__testing` into `ocx_schema`'s normal dependencies. Keep hermetic for the C07 test, not for the genrule.

---

## 4. Open questions

3 open questions, each with a `Recommended:` line (ADR § Open Questions 1–3). Compliant.

## 5. Regression check

- ADR ↔ SD: consistent except W3 (`errors` bump on `doc`), S1 (seam location) and S8 (`ALL`/`DECLARED`).
- Rotation sequence against `release.taskfile.yml` `prepare`: all four states have reachable greens, and the rotation-pending red is reachable. W2 is the one false red.
- Differ root algorithms: surviving, added and removed each have a reachable green and red. Shared-`$def` findings attribute to every reaching root, which matches per-root versions.
- No previously sound contract was broken by the fix pass. W1 is a gap the Q-W6 fix leaves at the phase boundary, not a regression.

Verdict: not ready for handoff until B1 is patched (ADR § Consumers phase-0 mirror row, SD § 3.12, V20; three sentences). The Warns can be fixed in the same pass or carried as plan-entry items. No further review round is needed for B1 beyond confirming the text.
