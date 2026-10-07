# Review: ADR + system design, OCX machine-interface contract (focus: spec)

Reviewer: Opus, hex-architect xhigh panel, spec seat. Repo `/home/mherwig/dev/ocx` at `34337a6a2`.
Under review: `adr_ocx_interface_contract.md` (ADR), `system_design_ocx_interface_contract.md` (SD).

Summary: **Needs Work**. Block 4, Warn 16, Suggest 11. The decision (Option D) holds; the matrix arithmetic checks out
(A 65, B 64, C 70, D 82, recomputed). The defects are in contracts that cannot go red, or that go red on a correct tree,
and in one unplanned lockstep break.

Evidence method: read both artifacts, the discover file, the dossier; checked claims against `scripts/crate_map.toml`,
`crates/ocx_schema/{Cargo.toml,src/reports.rs}`, the reports golden (parsed with Python), `crates/ocx_cli/src/{app.rs,error_envelope.rs}`,
`crates/ocx_exit/Cargo.toml`, `taskfiles/satellite.taskfile.yml`, `website/schema.taskfile.yml`, `.claude/rules/workflow-release.md`,
`.claude/tests/test_ai_config.py`, `test/lint/conftest.py`, the sibling `ocx-mirror` sources, and a `--help` walk of `test/bin/ocx`.

---

## Block

### B1. `schema_version` injected into root `$def`s that are also nested types (schema stops matching the wire)
- **Anchor:** SD § 3.2 "Report roots" (`report_roots!` writes `schema_version` const "as the first required property of the root `$def`"); ADR § Root convention item 2; lint L08.
- **Defect:** four root `$def`s are also referenced from inside other `$defs` in the reports golden: `SignatureReport` (2 refs, inside `SweepReport<…>` tag rows), `AttestationReport` (1), `PackageInspect` (1, `InspectReport.packages`), `PackageDescription` (1). `Versioned<T>` adds `schema_version` only at the top level. The schema would mark `schema_version` as required inside every nested occurrence, but the wire would not carry it there. A strict generated decoder (the Rust SDK) then fails on every `InspectReport` and every sweep. V8 cannot see this: it checks the golden plus one smoke root (`version`).
- **Fix:** do not change the type's `$def`. Each `reports` map entry becomes its own root schema: `{type: object, required: [schema_version, …], properties: {schema_version: {const: N}, …T's properties}}`, emitted as a distinct `$def` (for example `<Name>Root`), or inline in the `reports` map. L08 then applies to the `reports` map entries, not to `$defs`. Add a lint rule (L14): "no root wrapper `$def` is referenced from another `$def`". Add V8 red: nest a root type in another report, and the wrapper stays top-only while the nested `$def` has no `schema_version`.

### B2. The baseline-freshness lint (V7) is red on the release commit itself
- **Anchor:** SD § 3.8 "Baseline" (`test/lint/test_contract_baseline.py`: each file equals `git show $(cat RELEASE):…golden/<file>`); ADR § Verification V7; ADR § Dossier reconciliation row "Baseline via git show".
- **Defect:** `task release:prepare` ends with `task verify NOCACHE=1`, and `git tag vX.Y.Z` runs after it (`workflow-release.md` § Release Ceremony). If `RELEASE` names the new tag, the T0 lint runs `git show` against a tag that does not exist yet, so it reds inside `release:prepare`. If `RELEASE` names the previous tag, the copied goldens are the current tree, not that tag, so it also reds. Neither green state is reachable on the commit the release is cut from. The likely patch, "skip when the tag is missing", makes the lint vacuous.
- **Fix:** define the lint as two arms. (a) Tag `RELEASE` exists → each baseline file is byte-equal to `git show <RELEASE>:…/golden/<file>`. (b) Tag absent → `HEAD`'s subject is `release: <RELEASE>` and each baseline file is byte-equal to the current golden. Any other state is red. V7 then needs a red and a green for both arms.

### B3. The ocx-mirror gate (V11, Consumers row) cannot go red on a JSON break
- **Anchor:** ADR § Verification V11; ADR § Consumers, `ocx-mirror` phase-1 cell ("`task satellite:verify` is the gate").
- **Defect:** `satellite:verify` rsyncs the tree into the mirror, runs `cargo build --workspace`, and runs the operations-tier closure check. Nothing more (`taskfiles/satellite.taskfile.yml` summary, steps 1-5). The mirror parses `ocx --format json` at runtime through tolerant `#[serde(default)]` structs, so a reshaped root still compiles. `satellite:verify` stays green, and V11's red is unreachable. The header also records that the job runs with `continue-on-error: true` in `verify-deep.yml` until the mirror re-points. One more gap: the mirror runs a pinned, released `ocx` binary (`OCX_BINARY_PIN`, or `ocx` on `PATH`), not its submodule HEAD. So the claim that "lockstep through the submodule" covers the subprocess surface does not hold, during phase 1 or after the phase-2 SDK swap.
- **Fix:** give V11 a runtime arm. Either `satellite:verify` runs the mirror's `ocx_cli` pipeline tests against the `ocx` binary built from this tree, or the mirror adds a contract test that decodes the ocx goldens or recorded outputs with its parse structs. Then name, separately, which `ocx` binary the mirror's SDK must handshake with (released pin vs submodule) and how a pin bump is gated.

### B4. Phase 0 breaks ocx-mirror's compile, and the phase table does not plan it
- **Anchor:** SD § 3.4 "Migration" (`ocx_config::env::keys` deleted; public `var`, `flag`, `string(&str)` removed); ADR § Consumers (lists only phase 1 and 2 for `ocx-mirror`).
- **Defect:** the mirror calls the removed API at 20 or more sites, found by grep over `ocx-mirror/crates`: `ocx_util::env::var` (`ocx_mirror_http/src/auth.rs:142-145,183`, `ocx_mirror_spec/src/source.rs:123`, `annotations.rs:82,85`, `ocx_mirror_pipeline/src/ocx_cli/announce.rs:44`), `ocx_util::env::flag` (`ocx_cli/create.rs:58`), `ocx_config::env::keys::*` including `CREDENTIAL_KEYS` (`ocx_mirror_spec/src/validate.rs:605`, `sign_config/tests.rs:555`, `ocx_mirror_http/src/lib.rs:100`). Several of those are the mirror's own variables (`GITHUB_ACTIONS`, `GITLAB_CI`, `NETRC`, `URL_REWRITE_ENV`, auth names computed at runtime). OCX's registry must not declare them, and the SD gives a satellite no way to declare its own: `env_vars!` export, a public `EnvVar` constructor, and the scope of the `dynamic()` ratchet are all unspecified. CLAUDE.md makes the same-series mirror upgrade an obligation for ecosystem crates.
- **Fix:** add a phase-0 row to § Consumers covering the mirror edits and the satellite verify run. In SD § 3.4, state the satellite API (`env_vars!` is `#[macro_export]`, and `EnvVar` can be built in another crate) and where `CREDENTIAL_KEYS` lives after `keys` is deleted. Keep `dynamic()` public, and confine its ratchet to `//crates/...`.

---

## Warn

### W1. Differ B05 reds on every legitimate version bump
- **Anchor:** SD § 3.8 table (B05 "enum / `const` value removed", B08), and step (4) of the gate algorithm.
- **Defect:** bumping a root's `schema_version` from `{const: N}` to `{const: N+1}` removes a `const` value. That is B05, at `/…/properties/schema_version`, so every correct bump needs a second, spurious ledger entry. Likewise, the `$id` change on `errors`/`cli` is a value change at the document level, and the SD does not say the differ ignores it.
- **Fix:** state that the differ excludes the `schema_version` property of root wrappers and the document `$id` from B05/B04; step (4) owns them. Add a V6 green: "ledger entry + bump → exactly one finding".

### W2. L04 rejects the design's own `errors` document
- **Anchor:** SD § 3.7 L04 ("`oneOf` arms are objects with a required `type` const", applied to reports and errors); SD § 3.3 (`ExitCode` `$def` = "`oneOf` of `{const, title, description}`"); SD § 3.11 (the IR loader re-checks L01-L04).
- **Defect:** the `ExitCode` and `ErrorDetail` `$defs` are const-only `oneOf`, and so are the 21 string-const `oneOf` enums schemars already emits in the reports golden (discover § Claim diff). L04 fires on all of them, and the generator's IR loader refuses `errors.json`.
- **Fix:** split L04. A `oneOf` whose arms are all `{const, …}` scalars is an enum (allowed; L06 checks the values). Any other `oneOf` must have object arms with a required `type` const.

### W3. The recommended permanent `-g` waiver cannot coexist with "waivers empty"
- **Anchor:** ADR § Open Questions 1 (recommended: "a lint waiver with that reason, permanent"); ADR § Phases (phase-1 exit and phase-2 entry: "Waiver file empty"); ADR § Waiving today's violations ("the file can only shrink"); V3 green.
- **Defect:** if the owner takes the recommendation, the phase-2 go/no-go cannot pass. The ADR also has no exemption channel for borrowed vocabulary or for false positives of name-based rules (see W14) once the waiver file must be empty.
- **Fix:** add a second, permanent file, `contract/exemptions.toml`: entries `{rule, pointer, reason, adr}`. Allowed only with a cited decision record, and a stale entry reds. Waivers stay a ratchet that must reach zero.

### W4. The schema differ "fails closed" only on what the lint forbids
- **Anchor:** SD § 3.8, first paragraph ("Any construct the lint forbids … yields `unmodelled`"); ADR § The six tensions, tension 4.
- **Defect:** the subset is defined only negatively (L01-L13). Keywords the lint allows but no B-rule models pass the differ unexamined: `additionalProperties` with a schema (the 4 map roots stay maps under a named field), `patternProperties`, `minItems`, `minimum`, `allOf`, `$ref` siblings. A value-type change inside a map is therefore invisible. This contradicts the ADR's risk mitigation "fail closed on any construct outside the subset".
- **Fix:** define the subset positively, as a keyword allowlist in `ocx_schema::lint`, shared by the lint and the differ. Any keyword outside it is `unmodelled` in both. Add B-rules for map value type (`additionalProperties`) and array item type, with one mutation-corpus case each.

### W5. The grammar differ rules leave input-direction breaks out
- **Anchor:** SD § 3.8 G01-G11; SD § 3.6 `ArgSpec`/`CommandSpec`.
- **Defect:** (a) No rule covers a *new required* flag or positional on an existing command. That is breaking for input, but the additive list says "flag … added" passes. (b) Hidden transitions are undefined: does non-hidden → hidden count as G01/G02, and is removing a hidden arg a break? (c) G02 says "(outside a window)", but `ArgSpec` carries no deprecation data (only `CommandSpec.deprecated`, fed from `RENAMED`, which holds command pairs only), so the differ cannot compute "window". (d) Closing a window (removing the hidden spelling in the removal release) has no stated classification.
- **Fix:** add G12 "new required arg on an existing command". Add `deprecated: Option<Deprecation>` to `ArgSpec`, fed from `deprecated.rs`. State that "non-hidden → hidden with `deprecated` set" is additive, "hidden + deprecated removed in its named removal release" is additive, and every other removal is G01/G02.

### W6. `cli.json` defaults are read from the environment at walk time
- **Anchor:** SD § 3.6 (`ArgSpec.default`); G09 "default changed".
- **Defect:** root options compute defaults from the environment: `default_value_t = ocx_util::env::flag(...)` (`context_options.rs:42,56,65,75,94`, per discover § 3). The walker records whatever the generating process's environment holds. A developer with `OCX_OFFLINE=1` produces a different golden, and G09 reports a false break.
- **Fix:** run the walk inside `ocx_util::env::overrides` `EnvLock::hermetic`, or move env defaults out of clap. Record env-derived defaults as the static default plus `env`. Add a V5 case: setting `OCX_OFFLINE=1` in the walker's env leaves `cli.json` byte-identical.

### W7. Env aliases conflict with CLAUDE.md's batched-window rule, and the sweep has no data path
- **Anchor:** ADR § Flag renames, last paragraph ("an `aliases` field in the registry, swept by `test_deprecated_spellings.py`"); SD § 3.4 `EnvVar.aliases`; SD § 3.6 `EnvSpec` (no `aliases`).
- **Defect:** CLAUDE.md requires every old spelling in flight to live in one `deprecated.rs` and be listed in its `RENAMED`. Registry aliases in `ocx_util` violate that, and the ADR's CLAUDE.md amendment does not cover it. `test_deprecated_spellings.py` parses `RENAMED` out of the Rust source by regex, and `EnvSpec` exports no aliases, so the stated sweep cannot see them.
- **Fix:** either extend the ADR's CLAUDE.md amendment ("env-name spellings in flight live in the `ocx_util` registry's `aliases`, exported through `cli.json`") and have `test_deprecated_spellings.py` read `EnvSpec.aliases` from the `cli.json` golden, or list env renames in `RENAMED` as well. Add `aliases` to `EnvSpec` either way.

### W8. Missing crate-map edges, and `ocx_exit` cannot derive the schema the SD assumes
- **Anchor:** SD § 3.3 (`#[derive(Serialize, JsonSchema)] ErrorDocument { exit_code: ocx_exit::ExitCode, error.kind: ErrorCategory }`; "`ocx_exit` … stays dependency-free"); SD § 3.4 ("checked in `ocx_schema`, which sees both"); SD § 3.1 ("No crate-map edge changes").
- **Defect:** in `scripts/crate_map.toml`, `ocx_schema = ["ocx", "ocx_oci", "ocx_config", "ocx_package", "ocx_project", "ocx_package_manager"]` has no `ocx_exit` and no `ocx_util`, yet the SD reads `ExitCode::ALL` and the env `ALL` there. `ocx`'s `lib.rs` re-exports neither crate. `ExitCode` derives no `Serialize` today, and `ocx_exit/Cargo.toml` pins serde as its one dependency (C-050), so a `JsonSchema` derive cannot reach `ExitCode`/`ErrorCategory`. Orphan rules forbid implementing it in `ocx_cli` or `ocx_schema`. "Dependency-free" is also already false (serde).
- **Fix:** add `ocx_exit` and `ocx_util` to the `ocx_schema` row (and to the Rust mirror table checked by `crate_map_toml_matches_rust_table`). Use `#[schemars(with = "u8")]` / `with = "String"` on the `ErrorDocument` fields and build the `ExitCode`/`ErrorCategory` `$defs` from `ALL` in `ocx_schema`. Correct the "dependency-free" wording to "no new dependency".

### W9. Root convention rule 1 ("exactly one JSON document per invocation") is false today, and no phase makes it true
- **Anchor:** ADR § Root convention item 1; SD § 5 IC-01; SD § 3.12 `Error::Ocx(ErrorDocument)`.
- **Defect:** `app.rs` returns through `Context::try_init(...).await?` before the envelope branch, so config, env and project errors during context setup print no error document under `--format json`. clap usage errors (exit 64) print none either, and `--quiet` suppresses reports (zero documents). The SDK maps all three to `Decode`, not to a typed `Ocx` error.
- **Fix:** add a phase-1 item: emit the error document for context-init and usage errors whenever `--format json` was parsed (or define the fallback when it was not), and state the `--quiet` exception in rule 1. Add a V row: bad `OCX_*` config under `--format json` → one error document on stdout with `exit_code` 78/64.

### W10. Acceptance tests, website docs and schema publishing are missing from the phase plan
- **Anchor:** ADR § Phases (phase 1 scope); SD § 4 File map.
- **Defect:** (a) 105 modules under `test/tests/` parse JSON output (9 index `["data"]`), and phase 1 reshapes 19 roots and 91 fields; the suite floors (`test/SUITE_FLOOR`) and `bazel:test:accept` move with it. (b) `command-line.md` has 24 hand-written "JSON output" sections with examples (for example, `clean` documented as a bare array with `dry_run`, which L12/L13 retire), and 17 doc pages mention `--format json`; tested doc scripts can carry golden output. (c) `website/schema.taskfile.yml` hard-codes exactly 7 files, including `reports/v1.json`. Phase 0 adds `errors`/`cli` (→ 9), phase 1 moves reports to v2, and only the current version is published (precedent: only `project-lock/v3.json`), so `https://ocx.sh/schemas/reports/v1.json` goes 404. The ADR does not state that consequence. The `//crates/ocx_schema:schemas` genrule outputs and `main.rs` kinds also change.
- **Fix:** add phase-1 rows for the acceptance-test migration and the `command-line.md` JSON sections (no migration prose). Add both `schema.taskfile.yml` edits, the genrule and `main.rs` to the file map. Decide, and state in the ADR, whether retired `$id` versions stay published.

### W11. No test checks that runtime output conforms to the schema
- **Anchor:** ADR § Verification V8 (golden + one smoke test of `ocx --format json version`); ADR driver C1.
- **Defect:** every gate in the ADR runs on the generated schema. Nothing checks that the 56 roots' actual stdout validates against it, and B1 shows the two can diverge silently. C1 ("loud at runtime in a consumer") rests on schema == wire.
- **Fix:** add V14. An acceptance-suite hook validates every `--format json` stdout the suite captures against its root schema in the reports golden, with a floor on the number of distinct roots validated. Precedent: `jsonschema` is already used in `test/tests/test_execution_records.py`. Red: drop a field from the wire only, via `skip_serializing`.

### W12. The clippy residue plan ignores test code
- **Anchor:** SD § 3.5 Residue (seam body, `ocx_shim` (2), `ocx_cli/build.rs`); ADR § Phases (phase-0 exit "`disallowed_methods` baseline 0"); V0.
- **Defect:** test code calls the banned methods: `std::env::set_var`/`remove_var` in unit tests (`ocx_store/src/file_structure/shim_bin_store.rs:566-595`, `ocx_package_manager/src/tasks/render_toolchain.rs:7899,7923`), `var_os("CARGO")` in `ocx_test_support/tests/workspace_structure.rs:116,678`, plus the test-only reads discover § 4 lists. The SD neither lists them nor says whether test targets are in scope for the Bazel aspect run (`bazel build //crates/... --output_groups=clippy_output`). The residue ratchet count and the "baseline 0" exit are therefore unspecified.
- **Fix:** state the test policy: either route test-side overrides through `overrides::EnvLock` and `#[expect]` the rest, counted in the residue ratchet, or exclude test targets and say so. Add a V0 case seeded inside a `#[cfg(test)]` module, so the actual scope is proven red or green.

### W13. `ocx_sdkgen` is tiered internal but ships a consumer-facing CLI, and its build and release wiring is unlisted
- **Anchor:** SD § 2 container table and § 3.11 ("internal"; "Release pipeline publishes `ocx.sh/ocx/sdkgen`"; `include_str!` of goldens, Bazel `compile_data`).
- **Defect:** SDK repos pin and invoke `ocx-sdkgen --lang … --out …`, and its output is diffed in their CI. By CLAUDE.md's test ("someone's script can observe it"), that CLI and its output are an interface, not internal. The file map also omits `crates/ocx_sdkgen/BUILD.bazel`, the cross-package `exports_files`/filegroup for the goldens in `crates/ocx_schema/BUILD.bazel`, the `crate_map.toml` row plus its Rust table, the release-pipeline publish step, and the CLAUDE.md architecture table ("21 workspace members").
- **Fix:** tier the `ocx-sdkgen` CLI and its output layout as interface (the crate internals stay internal), and add the listed files to SD § 4 with their phase.

### W14. L13 is undefined, and L10 collides with existing relative paths
- **Anchor:** SD § 3.7 L10, L13; SD § 5 IC-03, IC-08.
- **Defect:** L13 says "root collections live under `items`", but `InspectReport {platform, packages: [...]}` is a root object holding a collection, and the rule does not say whether that is a violation. L10 maps `*_path`/`*_dir` to `AbsolutePath`, but `ocx_util::fs::path::RelativePath` exists and no vocabulary entry covers relative paths. With waivers forced to zero (W3), every relative path must be renamed or mistyped.
- **Fix:** define L13 as "a root whose payload is a list is `{schema_version, items}`; a root object may hold named plural collections". Add `RelativePath` to the vocabulary, with a suffix rule such as `*_rel_path` or a `$ref` check instead of a name rule.

### W15. Contracts in the SD have no red/green proof in the V table
- **Anchor:** ADR § Verification (V0-V13); SD § 3.3, 3.4, 3.8, 3.11.
- **Defect:** these have no V row: the errors-registry completeness test (§ 3.3); the env registry naming test (§ 3.4), whose Plumbing regex `^_{1,2}OCX_` also matches `__OCX_TESTING_*`, so a test seam declared Plumbing passes the prefix check; the `EnvVar.flag` ↔ `cli.json` join; the `ocx_shim` literal test; generator determinism; the IR loader refusal. V6 names 6 cases, while the Risks table promises a mutation corpus with "one case per rule" (B01-B08, R01-R03, G01-G11, which is 22).
- **Fix:** add V rows for each. Make V6 mirror V3: `compat_corpus/` fixtures must produce exactly the full set of differ codes (set equality). Tighten the Plumbing regex to `^_{1,2}OCX_(?!TESTING_)`.

### W16. The V12 floor names no unit and can go red on a clean tree
- **Anchor:** ADR § Verification V12, SD § 3.9 ("floor ≥ 70 commands").
- **Defect:** a `--help` walk of `test/bin/ocx` gives 73 visible commands including groups, but 64 leaf commands. Counted as leaves, the floor is red on a correct tree. The ADR's own figure is "~70". The SD also does not say how global flags are checked: they are documented once under "General Options", not in each command's Options block.
- **Fix:** define the unit (non-hidden `CommandSpec` nodes, groups included) and set the floor from a measured count minus a margin, recorded in the test. State that `global: true` args are checked against the General Options section only.

---

## Suggest

- **S1. ADR § Trade-off matrix.** C scores 4 on C3 although § Option C says "Phases 0 and 1 of the chosen option *are* C", which is the same break wave as D (3); recomputed, C = 66. A scores 5 on C5 while its own con says `git show` cannot run in the sandbox. The rationale says D is "best" on C4, but it ties B (4). The outcome is unchanged; fix the numbers or the wording.
- **S2. ADR § Context, Reports row.** "42 objects, 7 bare arrays, 4 maps, 2 untagged unions" sums to 55, against "56 published roots" in the same cell. Say it counts `Printable` impls.
- **S3. ADR § Option D.** "collections under `items` (… OCX's existing spelling)" is false: the reports golden has 0 properties named `items` and 5 named `entries`. Either pick `entries` or drop the claim.
- **S4. SD § 5 IC-05** ("no payload field named `type`"). The reports golden has 9 properties named `type`. Add them to ADR § Quantified impact.
- **S5. V1.** "no longer compiles" is a one-time observation. Make it a `compile_fail` doctest on `ocx_util::env` (Bazel runs doctests in `bazel:test:unit`), so the red stays reachable.
- **S6. SD § 3.8 ledger `pointer`.** `$def` names are "not contract", so state which namespace a pointer uses: the baseline document for removals and changes, the current one for additions.
- **S7. SD § 3.8 gate step (4).** A root added after the baseline has no baseline value. Require `schema_version == 1` for it.
- **S8. ADR § Consumers, `ocx-sdk-python`.** Its probe only debug-logs a newer `ocx` (`_client.py:855`, per discover). Make the frozen 0.6.x release refuse `> 0.6.x` (a `MAX_SUPPORTED` check), so it fails loudly instead of misparsing.
- **S9. ADR § Flag renames.** If phase 1 ships in a 0.6.x release, its renames must join the open 0.6 → 0.7 window (one window per release pair). Name the target release.
- **S10. SD § 3.10.** `test_path_overlaps_declared_or_absent` matches identical pattern strings only. The overlaps it will flag are `crates/ocx_cli/src/command/**` (with `subsystem-cli-api.md` + `subsystem-cli-commands.md`) and `crates/ocx_schema/**` (with `subsystem-metadata-schema.md`). The `website/**` group row is not needed.
- **S11. ADR § Contract scope** says `cli.json` carries the "public env manifest". SD § 3.6 puts Public, Foreign and Plumbing in `env`. Make the ADR row match the SD.

---

## Policy check (CLAUDE.md)

- Never edit `CHANGELOG.md`: complied with. Breaks reach users through commit subjects only (ADR § Communicating breaks), and the ledger is truncated, not published.
- Stability tiers: see W13 (`ocx_sdkgen` CLI) and W7 (env aliases outside `deprecated.rs`). The ADR's amendment text otherwise matches the Interfaces paragraph.
- Bazel gates / `task verify`: the new Rust tests sit in `ocx_schema` (cached), and the T0 lints read committed files plus `git`, which `test/lint/conftest.py` admits. B2 is the one gate that reds inside `release:prepare`'s `task verify NOCACHE=1`. W13 lists the BUILD wiring the file map omits.

## Finding classification

Actionable (architect can fix without the owner): B1, B2, B3, B4, W1, W2, W4, W5, W6, W8, W9, W10, W11, W12, W13, W14, W15, W16, S1-S11.
Deferred (reason: human judgment needed on…):
- W3: whether `-g` stays a permanent exemption (ADR open question 1), which decides whether an exemptions file is needed at all.
- W7: whether env spellings in flight may live outside `deprecated.rs` (a CLAUDE.md text change).
- W10(c): whether retired schema `$id` versions stay published on ocx.sh.
