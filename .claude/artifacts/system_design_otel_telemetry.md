# System Design: OpenTelemetry usage export

## Metadata

**Status:** Draft — companion to [`adr_otel_telemetry.md`](./adr_otel_telemetry.md) (Accepted 2026-11-18 under goal-loop authority)
**Author:** architect (hex-plan xhigh, branch `hex/otel-telemetry`)
**Date:** 2026-11-18
**Related ADRs:** `adr_otel_telemetry.md` (the decisions; this file holds no trade-offs), `adr_exec_resolution_record.md`, `adr_exec_env_model.md`, `adr_managed_config_tier.md`, `adr_ocx_interface_contract.md`
**Goal:** `.agents/goals/otel-telemetry.md` (15 source-derived criteria, mapped in § 11)

## Executive Summary

An operator arms OCX with `OCX_TELEMETRY` or `[telemetry] enabled = true` and names a collector through `[telemetry]` (any config tier, the managed payload included, or files listed in `OCX_CONFIG`) or the standard `OTEL_EXPORTER_OTLP_*` variables. Every armed command — except the exec family, the per-prompt commands and the static paths — then sends exactly one OTLP/HTTP protobuf request at its end, holding one command record and one record per package it installed, pulled or removed. A dead collector never changes the exit code or stdout and costs at most the timeout. Contracts below are written so a test can be written from each without reading code; every "Test:" line names where it lands.

---

## 1. Context

| Actor | Role |
|---|---|
| Operator | Owns the collector; arms and points OCX through config, the managed tier, `OCX_CONFIG` files or environment |
| CI job / developer | Runs `ocx`; may opt out (`DO_NOT_TRACK`, `OTEL_SDK_DISABLED`, `OTEL_LOGS_EXPORTER=none`, `--offline`, `OCX_TELEMETRY=0`) |
| Collector | Any OTLP/HTTP logs receiver the operator runs (`/v1/logs`) |
| Execution-record shipper | Ships the launch records ocx already writes (`filelog`, Vector, Fluent Bit, …); joins with command records on purl, pid and CI run id |

```
ocx <command> ──(armed, destination set)──► one POST <endpoint>/v1/logs ──► operator's collector
ocx exec ... ───(never)──────────────────► execution record file ──► shipper ──► same backend
```

---

## 2. Component view

| Crate (tier) | Module | New / changed | Responsibility |
|---|---|---|---|
| `ocx_env` (ecosystem) | `src/vars.rs` | changed | Declares `OCX_TELEMETRY`, `OCX_TELEMETRY_HEADERS`, the Foreign `OTEL_*`, `DO_NOT_TRACK` and CI-id variables; `OCX_CONFIG` becomes `PathList`; doc text of `OCX_CONFIG` and `OCX_NO_CONFIG` |
| `ocx_config` (ecosystem) | `src/telemetry.rs` | new | `TelemetryConfig`: lenient parse, validation, merge, lock, secret headers |
| | `src/lib.rs` | changed | `Config.telemetry` field and merge arm |
| | `src/loader.rs` | changed | `OCX_CONFIG` split and cap; received list input; `apply_system_locks`, `retain_system_locked_sections`, `fold_project_tier`; origin stamping |
| | `src/error.rs` | changed | `ConfigListTooLong` |
| | `src/exec_state/decode.rs` | changed | per-entry absoluteness for `policy.config_path` |
| `ocx_project` (internal) | `src/config.rs`, `src/error.rs` | changed | refuse `[telemetry]` in `ocx.toml` by name |
| `ocx_store` (internal) | `src/file_structure/state_store.rs` | changed | `telemetry_installation_id_file()` |
| `ocx_package_manager` (internal) | `src/activity.rs` | new | `ActivityLog`, `PackageActivity`; no telemetry dependency |
| | `src/tasks/pull.rs`, `install.rs`, `uninstall.rs` | changed | append points, timing, bytes; `UninstallResult.pinned` |
| `ocx_telemetry` (internal) | `src/lib.rs`, `gate.rs`, `destination.rs`, `resource.rs`, `ci.rs`, `identity.rs`, `payload.rs`, `client.rs`, `export.rs` | new crate | everything between "the config is loaded" and "the request is sent" |
| `ocx` (`ocx_cli`, interface) | `src/app.rs` | changed | the one finish site; timing start |
| | `src/app/telemetry.rs` | new | never-send table, slot, exit-code recovery, activity → record mapping |
| | `src/app/context.rs` | changed | fill the slot after extra roots; attach the activity log; pass received `OCX_CONFIG` list |
| | `src/api/data/shell_state.rs` | changed | `policy.config_paths`; per-entry `ReceivedConfigMissing` |
| `ocx_schema` | goldens | changed | `cli.json`, `config.json`, reports |
| tests | `test/src/otlp_fixture.py`, `test/src/otelcol.py`, `test/tests/test_telemetry_*.py`, `test/tests/test_otlp_fixture.py`, `test/tests/test_config.py` | new / changed | capture server, collector harness, behaviour tests |
| docs | `website/.../reference/telemetry.md`, `guides/telemetry.mdx` | new | field tables, the one story |

Allowed edges: `ocx_telemetry → {ocx_config, ocx_env, ocx_oci, ocx_util}`; `ocx → ocx_telemetry`. Nothing else gains an `ocx_*` edge.

---

## 3. The arming and destination algorithm

### 3.1 Arming (first matching row wins)

| # | Condition | Result | Warning |
|---|---|---|---|
| 1 | leaf in the never-send set (§ C-022), plugin, Context-free arm, or `in_seam()` | **never** — gate not consulted | — |
| 2 | `DO_NOT_TRACK` truthy | off: `DoNotTrack` | — |
| 3 | `OTEL_SDK_DISABLED` truthy | off: `SdkDisabled` | — |
| 4 | `OTEL_LOGS_EXPORTER` = `none` (case-insensitive) | off: `LogsExporterNone` | — |
| 5 | `Context::is_offline()` | off: `Offline` | — |
| 6 | `OCX_TELEMETRY` falsy | off: `Switch` | — |
| 7 | `OCX_TELEMETRY` unrecognized or non-UTF-8 | off: `Switch` | yes, names the variable |
| 8 | merged `[telemetry]` unusable | off: `Unusable` | one warning naming origin and key when the run asked for export (`OCX_TELEMETRY` truthy, merged `enabled = true`, or the invalid key is `enabled`), else none |
| 9 | merged section system-locked with `enabled = false` | off: `Locked` | — |
| 10 | `OCX_TELEMETRY` truthy | **armed** | — |
| 11 | `OCX_TELEMETRY` unset/empty and merged `enabled = true` | **armed** | — |
| 12 | otherwise | off: `NotEnabled` | — |

"Truthy" for rows 2, 3, 6, 10: `1`, `y`, `yes`, `on`, `true`; "falsy": `0`, `n`, `no`, `off`, `false`; both ASCII case-insensitive. An unrecognized `DO_NOT_TRACK` / `OTEL_SDK_DISABLED` value is not an opt-out.

### 3.2 Destination (armed only)

| # | Condition | Endpoint | Headers sent | Timeout | Protocol |
|---|---|---|---|---|---|
| D1 | merged `[telemetry].endpoint` set | `<endpoint path>/v1/logs` | merged `[telemetry].headers`, then `OCX_TELEMETRY_HEADERS` per name (locked names win) | `[telemetry].timeout` ▸ 500 ms | `[telemetry].protocol` ▸ `http/protobuf` |
| D2 | else `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` set | verbatim | `OTEL_EXPORTER_OTLP_LOGS_HEADERS` | `OTEL_EXPORTER_OTLP_LOGS_TIMEOUT` ▸ `OTEL_EXPORTER_OTLP_TIMEOUT` ▸ 500 ms | `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL` ▸ `OTEL_EXPORTER_OTLP_PROTOCOL` ▸ `http/protobuf` |
| D3 | else `OTEL_EXPORTER_OTLP_ENDPOINT` set | `<endpoint path>/v1/logs` | `OTEL_EXPORTER_OTLP_LOGS_HEADERS` if set, else `OTEL_EXPORTER_OTLP_HEADERS` | as D2 | as D2 |
| D4 | else | none: nothing sent, no id written, one debug line | — | — | — |

Under D1 no `OTEL_EXPORTER_OTLP_*` variable is read. Under D2/D3, if `[telemetry].headers` or `OCX_TELEMETRY_HEADERS` is set, one warning names both sources and neither is sent. Any selected input that is unusable (ADR D-11) makes the run send nothing with one warning, except a bad OTEL timeout (→ 500 ms, debug line).

### 3.3 Resource attributes (per key, later row wins)

| # | Source |
|---|---|
| R1 | derived keys (§ 4.1) |
| R2 | `OTEL_RESOURCE_ATTRIBUTES` (`k=v,k=v`, percent-decoded; malformed pairs skipped) |
| R3 | `OTEL_SERVICE_NAME` (non-empty) → `service.name` |
| R4 | merged `[telemetry].resource_attributes` |
| R5 | merged `[telemetry].service_name` → `service.name` |

### 3.4 Config fold for `[telemetry]`

| Tier | Contributes | Under `OCX_NO_CONFIG` |
|---|---|---|
| compiled defaults | nothing | — |
| system `config.toml` | yes; locks the section when present | kept only when locked |
| user, `$OCX_HOME` | yes | dropped |
| managed payload | yes, whole section incl. `headers` | dropped (tier skipped) |
| each `OCX_CONFIG` entry, in order | yes | kept |
| `--config` | yes | kept |
| `ocx.toml` | never: refused by name; stripped in `fold_project_tier` | never |

---

## 4. Record field tables

This is the table `reference/telemetry.md` mirrors and the binding test reads. Presence: **always**, **best-effort** (omitted when undeterminable, never a placeholder), or the named condition.

### 4.1 Resource {#fields-resource}

| Key | Type | Presence | Origin | Value |
|---|---|---|---|---|
| `service.name` | string | always | OTel Stable | `ocx`, unless R3/R5 |
| `service.version` | string | always | OTel Stable | the version `ocx version` prints |
| `os.type` | string | best-effort | OTel (Development); execution records | `linux`, `darwin`, `windows` |
| `process.arch` | string | best-effort | execution records; OTel `host.arch` vocabulary | architecture of the running `ocx` binary, OCI spelling (`amd64`, `arm64`) |
| `host.name` | string | best-effort | OTel (Development) / ECS; execution records | `sysinfo::System::host_name()` |
| `process.pid` | int | always | OTel RC / ECS | this `ocx` process |
| `process.parent.pid` | int | best-effort; omitted on Windows | ECS; execution records | the parent process |
| `sh.ocx.installation.id` | string | when the id file is readable or creatable | OCX | lowercase hyphenated UUID v4 |
| `sh.ocx.interactive` | bool | always | OCX | stderr is a terminal |
| `sh.ocx.ci.provider` | string | in CI only | OCX | `github` (`GITHUB_ACTIONS` truthy), else `gitlab` (`GITLAB_CI` truthy), else `other` (`CI` truthy) |
| `cicd.pipeline.run.id` | string | GitHub, GitLab; non-empty source | OTel RC | `GITHUB_RUN_ID` / `CI_PIPELINE_ID` |
| `cicd.pipeline.task.name` | string | GitHub, GitLab; non-empty source | OTel RC | `GITHUB_JOB` / `CI_JOB_NAME` |
| `cicd.pipeline.task.run.id` | string | GitLab; non-empty source | OTel RC | `CI_JOB_ID` |
| *(operator keys)* | string | when set | R2, R4 | any |

Scope: name `ocx`, version = `service.version`. Never present anywhere: `telemetry.sdk.*`, argv, cwd, user name or id, environment variables, file paths, registry credentials.

### 4.2 Command record — event `sh.ocx.command` {#fields-command}

| Key | Type | Presence | Origin | Value |
|---|---|---|---|---|
| `sh.ocx.command.name` | string | always | OCX | `Leaf::path()` joined by spaces, e.g. `package install`, `direnv init` |
| `sh.ocx.command.duration` | double, seconds | always | OCX | `run_from` entry to outcome known |
| `process.exit.code` | int | always | OTel RC | the process exit code |
| `error.type` | string | when the command returned an error | OTel Stable | that error's `error.detail` slug; `_OTHER` when no slug classified |

### 4.3 Package record — event `sh.ocx.package` {#fields-package}

| Key | Type | Presence | Origin | Value |
|---|---|---|---|---|
| `sh.ocx.package` | string | always | execution records | `pkg:oci` purl, plain text, as `record::purl::package_url` renders it |
| `sh.ocx.package.action` | string | always | OCX | `installed`, `pulled`, `removed` |
| `sh.ocx.role` | string | always | execution records | `root`, `dependency` (`removed` is always `root`) |
| `sh.ocx.package.store_hit` | bool | `installed`, `pulled` | OCX | true when the package was already in the store |
| `sh.ocx.package.download.size` | int, bytes | when `store_hit` is false | OCX | layer bytes this package fetched; 0 when none |
| `sh.ocx.package.resolve.duration` | double, s | `installed`, `pulled` | OCX | time in `PackageManager::resolve` |
| `sh.ocx.package.download.duration` | double, s | when this package fetched ≥ 1 layer | OCX | layer phase wall time (fetch, decompress, unpack — one stream) |
| `sh.ocx.package.extract.duration` | double, s | when `store_hit` is false | OCX | assembly into the store |

All records: severity INFO; `EventName` field set; no body; one shared timestamp taken at build.

---

## 5. Sequence of one armed command

```
App::run_from
 ├─ t0 = Instant::now()
 ├─ clap parse ── help/version/usage ──► return (nothing)
 ├─ leaf = command.leaf(); class = telemetry::class(leaf) ── Never ──► no slot (exec family, per-prompt, static)
 ├─ Context::try_init(…, slot)
 │    ├─ config loaded (OCX_CONFIG list, locks, managed fold)        ── error ──► no slot filled ─► return (nothing)
 │    ├─ extra CA roots resolved                                      ── error ──► return (nothing)
 │    ├─ slot ← ocx_telemetry::decide(section, offline)   // env reads only; no OTel type
 │    │          Armed{destination: Some} ⇒ manager.with_activity_log(log)
 │    └─ later try_init steps                                         ── error ──► outcome = Err (slot filled)
 ├─ background checks (unchanged)
 ├─ command.execute(context) ── PackageManager appends PackageActivity to log
 ├─ update_check::apply_pending
 ├─ outcome = Ok(code) | Err(fail(err))      // JSON error document already on stdout
 ├─ slot.finish(outcome, t0, log.drain())    // the ONE finish site
 │    └─ spawn + timeout(T):
 │         id = read-or-create installation id
 │         payload = build(resource, command record, package records)
 │         debug "telemetry payload <json>"
 │         client = reqwest + GuardedResolver{trusted: []}, no_proxy() + the proxy from the one read + roots
 │         exporter.export(one LogBatch)  ── any failure ──► debug line
 └─ return outcome ──► app::finish ──► process exit (code unchanged)
```

---

## 6. Component contracts

Each contract lists its location, the contract, and the test that pins it. "Unit" means a Rust unit test in the named crate; "accept" means `test/tests/`.

### C-001 `OCX_TELEMETRY` declaration
- Location: `crates/ocx_env/src/vars.rs`.
- Declared `OCX_TELEMETRY: TriBool, Public, propagation = UserInput`; appears in golden `cli.json` env manifest and `reference/environment.md`.
- `ocx_telemetry` reads it through one function returning `Unset | On | Off | Invalid`: empty and unset → `Unset`; truthy set → `On`; falsy set → `Off`; any other value, non-UTF-8 included → `Invalid`.
- Test: unit `switch_grammar` (table over `"", "1", "TRUE", "Yes", "on", "0", "False", "no", "off", "2", "enable", non-UTF-8`); golden `cli_document_matches_golden`.

### C-002 `OCX_TELEMETRY_HEADERS` declaration
- `OCX_TELEMETRY_HEADERS: String, Public, secret, propagation = Credential` (R-13: removed at every spawn); `get()`/`get_raw()` return `None`; read via `SecretVar`.
- Parser: `name=value` pairs separated by `,`; names trimmed; values percent-decoded; a pair without `=`, an empty name, a non-token name, a value `http::HeaderValue::from_bytes` refuses after decoding, or a refused name (`host`, `content-length`, `content-type`, `transfer-encoding`, and the hop-by-hop `connection`, `keep-alive`, `proxy-connection`, `te`, `trailer`, `upgrade`, any case; R-8) → whole variable invalid.
- Test: unit `header_variable_grammar` (valid, percent-decoded, each refusal); unit `secret_variable_is_never_readable_through_get`.

### C-003 Foreign declarations
- Declared `Foreign`, `propagation = UserInput` unless noted: `DO_NOT_TRACK` (Bool), `OTEL_SDK_DISABLED` (Bool), `OTEL_LOGS_EXPORTER` (String), `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`, `OTEL_EXPORTER_OTLP_TIMEOUT`, `OTEL_EXPORTER_OTLP_LOGS_TIMEOUT` (Integer), `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS` and `OTEL_EXPORTER_OTLP_LOGS_HEADERS` (`secret`, `SecretInput`), `OTEL_EXPORTER_OTLP_COMPRESSION` and `OTEL_EXPORTER_OTLP_LOGS_COMPRESSION` (`reader = Dependency("opentelemetry-otlp")`, doc: "not honoured; the request is never compressed"), `GITHUB_JOB`, `CI_PIPELINE_ID`, `CI_JOB_ID`, `CI_JOB_NAME` (String). `GITHUB_RUN_ID`, `GITHUB_ACTIONS`, `GITLAB_CI`, `CI` already exist (`vars.rs:543,562,574,588`).
- Each doc line states how OCX treats the variable (honoured, honoured as opt-out, or not honoured).
- Test: golden `cli.json`; `task website:reference:check`; registry test (Public ⇒ `OCX_` prefix) stays green.

### C-004 `OCX_CONFIG` is a path list
- Location: `vars.rs` (kind `PathList`, ADR § Amends doc text), `loader.rs`.
- Given `OCX_CONFIG="a<sep>b"`, files `a` and `b` load in that order into the explicit overlay; a scalar both set takes `b`'s value; `--config c` loads after `b`, its value winning over both.
- A single path loads exactly as before: same tier, same precedence, same `FileNotFound` (exit 79, slug `config_file_not_found`) when missing, same refusal of a non-regular file, relative path resolved against the working directory.
- An empty value is unset (debug line, as today); empty entries (`a<sep><sep>b`, leading or trailing separator) are skipped; entries are not trimmed.
- More than 32 non-empty entries → `ConfigListTooLong` (exit 78, slug `config_list_too_long`), message naming `OCX_CONFIG` and the limit 32; no file is read.
- A `--config` path containing the path-list separator is refused when ocx composes a child's received policy: exit 78, slug `config_path_contains_separator` (R-10); test `config_flag_with_separator_is_not_forwarded`.
- Under `OCX_NO_CONFIG=1` every entry still loads.
- Every entry, absolutized, appears in `LoadedConfig.config_tier_paths` after the discovered candidates, then `--config`.
- Test: unit `ocx_config_list_later_entry_wins`, `ocx_config_empty_entries_are_skipped`, `ocx_config_list_over_cap_is_refused`, `ocx_config_list_loads_under_no_config`, `ocx_config_tier_paths_list_every_entry`; accept S-012.

### C-005 Received policy and the `shell state` report
- `Policy.config_path` (name and wire type unchanged) carries the raw `OCX_CONFIG` value. `validate_policy` refuses the payload with field `policy.config_path` when any split entry is relative or there are more than 32 entries (R-20); a single absolute path decodes as today. `--config`'s help text and `PolicyReport`'s doc are amended to the list.
- When neither `--config` nor an ambient `OCX_CONFIG` is set, the received value is split and loaded in the `OCX_CONFIG` slot (a new `ConfigInputs` field), not the `--config` slot; with `tolerate_vanished_received_config`, each missing entry is skipped and reported, the rest load.
- `PolicyReport.config_path` is replaced by `config_paths: Vec<AbsolutePath>` (omitted when empty); `ReceivedConfigMissing { path }` is emitted once per vanished entry.
- Ledger: `[[break]]` entries for the reports document naming `ShellStateReport` and `VerboseShellState`; both versions bumped.
- Test: unit `decode/tests.rs` — a list with one relative entry names `policy.config_path`; a list of absolute entries decodes; unit `received_list_loads_in_the_ocx_config_slot`; accept extension of `test_env_received_policy.py` (list round-trips through `ocx exec --clean -- ocx shell state --format json`).

### C-006 `[telemetry]` section type
- Location: `crates/ocx_config/src/telemetry.rs`; `Config.telemetry: Option<TelemetryConfig>` with a doc comment rendered into `config.json` and `reference/configuration.md`.
- Keys and rules exactly as ADR D-4. A known key with a wrong type or value yields a `TelemetryConfig` marked unusable with the offending key name, and the file's parse succeeds; every other section of that file still applies.
- Unknown keys inside `[telemetry]` are ignored silently.
- A TOML syntax error in any config file, and the managed `InvalidToml`, render path, line, column and message only, never the offending source line (R-2); test `parse_error_never_quotes_the_source_line` with a token on the broken line.
- `headers` values are secrets: `format!("{:?}", config)` never contains a header value; `system_locked` and the origin are `#[serde(skip)]`/`#[schemars(skip)]` and never read from disk.
- The JSON schema shows the seven keys with types (`headers` and `resource_attributes` as string maps, `timeout` integer with `minimum 1`, `maximum 10000`).
- Test: unit `telemetry_full_section_parses`, `telemetry_unknown_keys_are_ignored`, `telemetry_bad_value_marks_the_section_unusable` (one row per key and rule), `telemetry_debug_never_prints_a_header_value`, `telemetry_cannot_set_system_locked`; golden `config_schema_matches_golden`.

### C-007 Merge
- Later scalar replaces; `None` never clobbers; a layer that sets `endpoint` drops every header inherited from earlier layers (R-1); otherwise `headers` merge per name ignoring ASCII case, later wins, and the later spelling is kept; `resource_attributes` merge per exact key; an unusable later layer makes the result unusable and keeps its origin; a system-locked accumulator ignores later layers whole.
- Test: unit matrix modelled on `records.rs` tests (`merge_higher_tier_wins_field_by_field`, `…_none_never_clobbers`, `headers_merge_case_insensitive`, `a_later_endpoint_drops_earlier_headers`, `unusable_layer_poisons_the_merge`, `merge_system_locked_ignores_every_field`, `merge_does_not_inherit_a_lock`).

### C-008 Lock and `OCX_NO_CONFIG`
- `apply_system_locks` locks a system-tier `[telemetry]` unconditionally; `retain_system_locked_sections` keeps it only when locked (the exhaustive destructure names `telemetry`).
- A locked `[telemetry]` clamps user, home, managed and explicit layers.
- Test: unit `apply_system_locks_covers_every_lockable_section` and `lib.rs` `every_lockable_section_respects_system_lock` gain the row; new `retain_keeps_a_locked_telemetry_section`, `retain_drops_an_unlocked_telemetry_section`; `a_locked_telemetry_section_clamps_the_managed_payload`.

### C-009 Managed fold
- A payload `[telemetry]` (all keys, `headers` included) folds above `$OCX_HOME` and below the explicit overlay with no strip and no warning; a system lock clamps it.
- Test: unit `managed_payload_carries_telemetry_with_headers`; accept S-010.

### C-010 Project tier
- `ocx.toml` with a `[telemetry]` table → `TelemetrySectionInProject` (exit 78, slug `telemetry_section_in_project`), message `<path>: [telemetry] belongs in config.toml, not in ocx.toml`.
- `fold_project_tier` strips `telemetry` with one warning.
- Test: unit `telemetry_section_in_ocx_toml_is_refused_by_name`, `c033_style_project_fold_cannot_contribute_telemetry`; accept S-013.

### C-011 Gate
- `ocx_telemetry::decide(section, offline) -> Decision` implements § 3.1 rows 2–12 exactly; reads only `ocx_env`; constructs no OTel type, no client, no file handle.
- `Decision::Off { reason, warnings }` carries its reason and the unusable-switch or unusable-section warnings the gate reached; `Decision::Armed { destination, armed_by, warnings }` carries the destination (§ 3.2, possibly none), what armed the run and the unusable-destination-input warnings. Resource inputs are not part of the decision: the exporter holds a resource builder and runs it at export time (C-019).
- Test: unit `gate_truth_table` — switch {unset, on, off, invalid} × opt-outs {none, each of rows 2–5} × section {absent, enabled true, enabled false, locked false, locked true, locked unset, unusable}, asserting decision and reason; unit `off_never_reaches_the_exporter` (an export double records zero calls for every off row and for `Armed` without destination).

### C-012 Destination selection
- Implements § 3.2. Endpoint validation: scheme `http`/`https`, host present, no userinfo; `/v1/logs` appended with exactly one `/` (`https://c.example:4318` → `…/v1/logs`; `…/otlp/` → `…/otlp/v1/logs`); a logs endpoint is used verbatim.
- The selected destination records its family (`ocx_config`, `otel_logs`, `otel`) for the debug line.
- Test: unit `destination_table` (each row; header scoping; the unused-OCX-headers warning; the ignored-generic-headers warning (`OTEL_EXPORTER_OTLP_HEADERS` set where D2, or D3 with logs headers, names the destination); each warning ends with a remedy; protocol refusal; timeout fallback and cap); accept S-008.

### C-013 Resource builder
- Builds § 4.1 from `Resource::builder_empty()`; applies § 3.3 in order; the `OTEL_RESOURCE_ATTRIBUTES` parser percent-decodes keys and values and skips malformed pairs.
- Test: unit `resource_precedence` (R1…R5 each overriding the previous), `resource_attributes_are_percent_decoded`; accept S-017.

### C-014 Command record
- Exactly one `sh.ocx.command` per export, fields § 4.2. `process.exit.code` equals the process's exit code; `error.type` present iff the command's result was `Err`, equal to the `error.detail` of the JSON error document for the same run.
- Test: accept S-018; unit `command_record_fields`.

### C-015 Package record
- One `sh.ocx.package` per `ActivityLog` entry with a logical identity, fields § 4.3; presence rules exactly as the table.
- Test: unit `package_record_presence` (store hit vs fetched vs removed); accept S-015.

### C-016 Golden payload
- From fixed inputs (version, pid 4242, ppid 4241, host `ci-host`, CI GitHub ids, one command failing with a known slug, two package entries), the debug-line JSON equals `crates/ocx_telemetry/tests/golden/payload.json` byte-for-byte; the wire form is built from the same value.
- Test: unit `golden_payload`; accept `test_debug_line_matches_wire`.

### C-017 Installation id
- Path `$OCX_HOME/state/telemetry/installation-id` (`StateStore::telemetry_installation_id_file`).
- Absent and armed with a destination → created 0600 (Unix), content a lowercase hyphenated UUID v4 with no trailing data beyond one optional newline; same value on every later run.
- Present and valid → reused. Present but unreadable, larger than 128 bytes, a symlink (checked with `symlink_metadata`), not a regular file, or not a lowercase v4 UUID → attribute omitted, file untouched. Creation uses `persist_temp_file_noclobber` (R-9). Parent not creatable or home read-only → attribute omitted, command unaffected.
- Not created when unarmed or armed without destination.
- Test: unit `identity_*` table; accept S-021.

### C-018 HTTP client
- Built only for a destination. TLS: embedded roots plus resolved extra roots. Connect and total timeout = destination timeout. `redirect(Policy::none())`. `user-agent: ocx/<version>`. `dns_resolver(GuardedResolver::new([], rules))` (no trusted host) and `no_proxy()` in the constructing function, so the one `proxy_rules()` read is the only proxy decision — ratchet compliance and a backstop, not an SSRF property (R-6).
- The endpoint host is resolved on a detached thread bounded by the remaining deadline and passed with `resolve_to_addrs`; an IP literal needs no lookup (R-26). An IP-literal endpoint in `169.254.0.0/16` or `fe80::/10`, or exactly `fd00:ec2::254` or `100.100.100.200`, is unusable (R-6); a resolved address in those sets is dropped, and none left is a `blocked` failure.
- Header values stay `SecretString` until the request is built and are inserted with `set_sensitive(true)`; the client and `Destination` print names only in `Debug` (R-3). Headers sent to an `http://` non-loopback endpoint produce one warning naming the source (R-7).
- `crates/ocx_test_support/tests/fixtures/ssrf_unguarded_baseline.txt` is byte-identical to `main`'s.
- Test: `workspace_structure::ssrf_guard_ratchet`; unit `link_local_endpoint_is_unusable`, `client_and_destination_debug_never_print_a_header_value`, `stalled_resolver_never_outlives_the_deadline` (injected lookup that never returns: `finish` returns within the timeout plus ε and a runtime built in the test drops promptly); accept `test_redirect_is_not_followed`, `test_collector_behind_extra_ca` (an HTTPS capture server whose CA is trusted only through `OCX_EXTRA_CA_CERTS` receives the request).

### C-019 Export
- One provider whose only processor collects in memory; exactly one `export(LogBatch)` per run; `RetryPolicy::disabled()`; the client clears every crate-set header and sends exactly `user-agent`, the selected headers, `content-type: application/x-protobuf` (plus transport headers reqwest adds: `host`, `content-length`, `accept`); body uncompressed; the resource probe (host name, environment, terminal) and the installation id run inside the spawned export task, under the one deadline, with the probe on a detached thread so a stalled probe cannot outlive the deadline; the proxy host lookup runs under that deadline as well; the `HttpClient` returns the status with an empty body and never reads the response (R-4). Event names are `&'static str` constants set through `set_event_name` (R-27).
- Any failure → one debug line naming its class (`connect`, `blocked`, `status <n>`, `timeout`, `redirect`, `encode`, `panic`) and never the `Display` of `reqwest::Error` or `ExporterBuildError`, which embed the URL (R-5); never a warning; exit code and stdout unchanged.
- Test: unit `no_crate_processor_or_transport_is_named`, `response_body_is_never_read` (fixture mode `stream-body` streams until the timeout; the client returns at once with an empty body), `ten_thousand_package_records_encode_as_one_request` (R-25); accept `test_one_post_carries_command_and_package_records`, `test_ambient_otel_headers_never_reach_an_ocx_destination`, `test_logs_endpoint_never_gets_generic_headers`, `test_content_type_cannot_be_overridden`, `test_no_retry_on_503`, `test_ambient_compression_leaves_the_request_uncompressed`.

### C-020 Debug audit lines
- At `--log-level debug`, target `ocx_telemetry`: `telemetry: off (<reason>)` or `telemetry: armed by <OCX_TELEMETRY|[telemetry] enabled>`; `telemetry: destination <url-without-query> (<family>), headers [<names>]` or `telemetry: no destination`; `telemetry payload <json>`.
- No header value, token or query string ever appears on stderr at any level, in any warning, or in any `--format json` output.
- Test: accept `test_debug_line_matches_wire`, `test_secrets_never_printed` (S-020).

### C-021 CLI wiring
- One finish call site in `crates/ocx_cli/src/app.rs`, after `apply_pending` and after the error document; `main.rs` and `boundary.rs` unchanged and their structural tests green.
- The slot is an out-parameter owned by `run_from`, filled in `Context::try_init` right after extra CA roots resolve; a `try_init` error before that point sends nothing, after it sends a record with that error's code and slug (R-21). The duration stops before `apply_pending`; on timeout the export task's handle is aborted.
- `Ok(ExitCode)` → integer by equality over `0..=255`; `Err` → `classify_decision`.
- The whole finish runs in a spawned task awaited under `tokio::time::timeout(destination timeout)`.
- Test: unit `telemetry_finishes_at_one_site` (structural, non-zero needle), `exit_code_value_round_trips_every_u8`; accept S-011, S-018.

### C-022 Never-send command set
- `telemetry::class(command) -> Class` is an exhaustive match over `Leaf` without a wildcard, plus the `--shell` flag of the two env leaves. `Never`: `Exec`, `Run`, `PackageExec`, `LauncherExec`, `LauncherShim`, `Shell`, `ShellState`, `DirenvExport`, `SelfActivate`, `ShellCompletion`, `Version`, `Status`, `ConfigTest` (R-18), `Env` and `PackageEnv` with `--shell` (R-23), and `None` (plugin). Every other leaf: `Send`.
- A `Never` command never reads the telemetry variables, never touches `state/telemetry/`, and never opens a connection.
- Test: unit `every_leaf_has_a_telemetry_class` (iterates `Leaf::ALL`, compares against the literal never set, plus both `--shell` rows; a new leaf fails to compile); accept S-006.

### C-023 Seam and CI behaviour
- Under `in_seam()` the slot stays empty. `skip_reason` is never called by telemetry: `CI=true` and a non-terminal stderr do not stop export.
- Test: accept `test_export_runs_in_ci` (`CI=true`, `GITHUB_ACTIONS=true`, stderr piped → one request); unit seam test asserting no slot under the seam.

### C-024 Package-manager activity log
- Location: `crates/ocx_package_manager/src/activity.rs`. `ActivityLog` is `Clone` and shared by every manager clone; `PackageManager::with_activity_log(log)`; a manager without one appends nothing.
- Entry: pinned identifier, platform, action (`Installed | Pulled | Removed`), role (`Root | Dependency`), store hit, download size, optional resolve/download/extract durations.
- Appended by every `setup_impl` leader on completion (success only), never by a waiter; `install_all(candidate = true)` marks its roots `Installed`; `uninstall_all` appends `Removed` per removed candidate with `UninstallResult.pinned` (requested registry/repository/tag + the candidate target's digest).
- Each `_all` entry point records its root set apart from the setup entries; the drain takes `role` from the union of root sets, so a root that lost the singleflight race to its own dependency path is still `root`, and when several operations touched one digest it keeps the performance fields of the entry that fetched (R-15). Drained once; one entry per `registry/repository@digest`, `installed` over `pulled`.
- A `removed` entry carries the platform when the store records it for that package, else none (its purl then omits `arch`, R-14).
- `crates/ocx_package_manager/Cargo.toml` has no `ocx_telemetry` or `opentelemetry*` edge (`deps_direction`).
- Test: unit `activity_log_records_leaders_only`, `a_root_that_waited_is_still_root`, `the_fetching_entry_keeps_its_performance_fields`, `install_marks_roots_installed`, `dependency_entries_carry_role_dependency`, `uninstall_records_the_pinned_identity`, `no_log_attached_records_nothing`.

### C-025 Timing and byte attribution
- Rules exactly as ADR D-17: resolve around `resolve` (`pull.rs:227`); download around the package's own layer phase; extract around assembly through `move_temp_to_object_store`; bytes = descriptor sizes of layers this package fetched as layer leader and that were absent; store hit omits download and extract.
- Test: unit with a fake transport — two packages sharing one layer: only the leader's record counts its size; second run of the same install: `store_hit` true, no download/extract; dependency setup time not inside the root's `extract`.

### C-026 CI detection
- § 4.1 rows for `sh.ocx.ci.provider` and `cicd.*`; empty values omitted; no CI variable → none of these keys.
- Test: unit `ci_detection_table` (GitHub, GitLab, `CI` only, none, empty ids); accept S-016.

### C-027 Probes and name parity with execution records
- `process.pid` = `std::process::id()`; `process.parent.pid` = Unix parent id, omitted on Windows; `host.name` via `ocx_package_manager::record::environment::host()`, called by the CLI (`resource_inputs`) and handed to `ocx_telemetry`; `os.type`/`process.arch` via `OperatingSystem::current`/`Architecture::current`, each omitted on failure.
- The attribute names equal the execution record's key paths for the same facts.
- Test: unit in `ocx_cli` `telemetry_names_match_execution_record_keys` (serializes an `ExecutionRecord` fixture and asserts each telemetry key path exists in it).

### C-028 Pin guard
- As ADR condition 1–2; plus every `opentelemetry*` line in any `Cargo.toml` carries the pointer comment, and every `opentelemetry*` package in `Cargo.lock` is `0.33.1` (R-22).
- Test: unit `opentelemetry_pins_match_the_exemption` (red on a changed version, a dropped `default-features = false`, an added feature, a second dependent crate, a `tonic` package in `Cargo.lock`).

### C-029 Crate placement
- `crates/ocx_telemetry/{Cargo.toml, BUILD.bazel, src/**}`; `scripts/crate_map.toml` rows per ADR D-1; mirrored tables in `workspace_structure.rs`; root `Cargo.toml` path and workspace deps (`opentelemetry*`, `uuid` with `v4`); `TEST_TARGET_MAP.toml` and gate scripts naming crates; `Cargo.lock`, `MODULE.bazel.lock`, `LICENSE-THIRD-PARTY.md` refreshed.
- Test: `deps_direction`, `crate_map_toml_matches_rust_table`, `bazel:mod:check`, `bazel:build:drift`, `task rust:license:deps`.

### C-030 Interface-contract artifacts
- Golden `cli.json` (new env rows, `OCX_CONFIG` `path_list`), golden `config.json` (`[telemetry]`), reports schema (`config_paths`), errors (three new slugs: `telemetry_section_in_project`, `config_list_too_long`, `config_path_contains_separator`) regenerated; `ledger.toml` gains the G14 and the reports entries; affected versions bumped.
- Test: `cli_document_matches_golden`, `config_schema_matches_golden`, the compat gate, `contract_lint`.

### C-031 Documentation
- `reference/environment.md` and `reference/configuration.md` regenerated.
- `reference/telemetry.md` (`doc_type: reference`): § 4 tables under `{#fields-resource}`, `{#fields-command}`, `{#fields-package}` inside `{#fields}`; the honoured, opt-out and unsupported variable lists; the § 3 ladder.
- `guides/telemetry.mdx` (`doc_type: how-to`): turn on (variable, config, managed payload), turn off (each opt-out), the managed-config recipe, the `OCX_CONFIG` list recipe for a CI image, CI run id and joining with execution records, the shippers that pick record files up (OTel Collector `filelog`, Vector, Fluent Bit, Fluentd, Filebeat, Alloy, Splunk UF), the token warning, the `ocx env --shell` login-shell note, the `--clean` note.
- `guides/manage-config-centrally.mdx` gains the token sentence; `reference/execution-records.md` links the guide.
- Test: `task website:reference:check`; docs-quality checks; C-032.

### C-032 Doc binding
- The keys in `reference/telemetry.md`'s three field tables equal, in both directions, the keys observed on the wire across a run set that exercises every presence condition (a failing command, a CI run, a fetched and a cached package, a removal); every key marked "always" appears on every record of its kind.
- Test: accept `test_wire_keys_match_documented_field_table` (port of Grimoire's).

### C-033 Capture-server fixture
- `test/src/otlp_fixture.py`: modes `ok`, `status=<code>`, `hang`, `delay=<ms>`, `redirect`, and `stream-body` (answers 200 and streams a body until closed, R-4); binds `127.0.0.1:0`; records method, path, headers, body; decodes `/v1/logs` into `ExportLogsServiceRequest`; helpers to flatten attributes and to name a closed port. `opentelemetry-proto==1.45.2` in `test/pyproject.toml`; `uv.lock` updated.
- Test: `test/tests/test_otlp_fixture.py` self-tests (each mode red and green; shutdown leaves no thread).

### C-034 Real collector
- `ocx.toml` `[group.test.tools]` pins the collector (suggested `otelcol = "ocx.sh/open-telemetry/otelcol:0.165.0"`), `ocx.lock` has every CI platform leaf.
- `test/src/otelcol.py` resolves `OCX_TEST_OTELCOL`, else `ocx --project <root>/ocx.toml env -g test`; neither → `pytest.fail` naming `otelcol`, the `[group.test.tools]` pin and `OCX_TEST_OTELCOL`. Only `OCX_TEST_NO_OTELCOL=1` skips, with a reason naming that variable.
- Config: OTLP HTTP receiver on a per-test port, `file` exporter `format: json` to a fresh per-test path with `flush_interval: 100ms` (no `append`, `rotation` or `group_by`), internal metrics off. The reader polls with a deadline and parses each line with `google.protobuf.json_format.Parse(line, ExportLogsServiceRequest(), ignore_unknown_fields=True)`, never by hand-picked keys.
- Test: accept `test_real_collector_receives_the_command_record` (S-014); lint `test_otelcol_resolver.py` (red: missing tool fails, never skips).

### C-035 `ocx.toml` has no effect
- Test: accept S-013.

### C-036 Rule and AI-config text
- `subsystem-cli.md`, `arch-principles.md`, `AGENTS.md`, `product-context.md` carry the ADR § Amends text; OBS-12's file is unchanged.
- Test: `task claude:tests`; review.

### C-037 Satellite
- `task satellite:verify` builds `ocx-mirror` against the branch unchanged.
- Test: the task.

### C-038 Binary size
- Release `ocx` size on the merge base and the branch, linux x86_64, written into the ADR's Quantified Impact before landing.
- Test: the recorded row.

---

## 7. UX scenarios

Format: action → expected outcome → error cases. "Capture" is the C-033 server; ε is 1.5 s.

- **S-001 Unconfigured sends nothing, shown red by arming.** Ambient `OTEL_EXPORTER_OTLP_ENDPOINT=<capture>`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_SERVICE_NAME`; no switch; run `ocx package install <pkg>` → capture records 0 requests; no `state/telemetry/`; no payload debug line. Same test, add `OCX_TELEMETRY=1` → exactly 1 request. → If the control sees 0, the test fails (server never listened).
- **S-002 Switch on, no endpoint.** `OCX_TELEMETRY=1`, no OTEL endpoint, no `[telemetry].endpoint` → 0 requests, no `state/telemetry/`, debug line `telemetry: no destination`. Control: add the endpoint → 1 request.
- **S-003 Config arms alone.** User `config.toml` with `[telemetry] enabled = true, endpoint = <capture>` → 1 request; `OCX_TELEMETRY` unset.
- **S-004 One request per armed command.** Armed; run three different send-set commands → exactly 3 POSTs to `/v1/logs`, each holding exactly one `sh.ocx.command` whose `sh.ocx.command.name` matches the command.
- **S-005 OTEL alone never arms.** S-001's first half; also with `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` → 0.
- **S-006 Silent commands, every platform.** Armed with a capture endpoint; run `ocx exec -- <tool>`, `ocx package exec <pkg> -- <bin>`, `ocx package exec --rm …`, `ocx shell state`, `ocx direnv export`, `ocx --help`, `ocx package --help`, `ocx version`, a usage error (`ocx package install --no-such-flag`), `ocx self activate --shell=bash`, `ocx shell completion --shell bash`, `ocx env --shell=bash`, `ocx package env --shell=bash <pkg>`, `ocx status`, `ocx config test <file>` → 0 requests and no `state/telemetry/` (R-18, R-23). Control: `ocx package install <pkg>` → 1. → Runs in the whole suite; Windows and macOS through the deep tier.
- **S-007 Each opt-out wins.** Armed by both `OCX_TELEMETRY=1` and `[telemetry] enabled = true` with an endpoint; separately set `DO_NOT_TRACK=1`, `OTEL_SDK_DISABLED=true`, `OTEL_LOGS_EXPORTER=none`, `--offline`, `OCX_OFFLINE=1`, `OCX_TELEMETRY=0`, `OCX_TELEMETRY=maybe` → 0 requests each; the last also prints one warning naming `OCX_TELEMETRY` without its value. Unit: the same rows beat a system-locked `enabled = true`.
- **S-008 Destination precedence.** Two captures A and B. (f) User `config.toml` with `[telemetry] headers = { authorization = "Bearer u" }` and an `OCX_CONFIG` entry with `[telemetry] enabled = true, endpoint = A` → A's request carries no `authorization` (R-1). (a) `[telemetry] endpoint = A` + `OTEL_EXPORTER_OTLP_ENDPOINT = B` → A gets 1, B gets 0. (b) No `[telemetry].endpoint`, `OTEL_EXPORTER_OTLP_ENDPOINT = B` → B gets 1. (c) (a) plus ambient `OTEL_EXPORTER_OTLP_HEADERS=x-otel=1` → A's request has no `x-otel`. (d) (b) plus `OCX_TELEMETRY_HEADERS=x-ocx=1` → B's request has no `x-ocx`, one warning naming both sources. (e) `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT = B` with generic headers only → B's request carries none of them.
- **S-009 Switch versus file.** `[telemetry] enabled = false, endpoint = A` + `OCX_TELEMETRY=1` → 1 request. `enabled = true` + `OCX_TELEMETRY=0` → 0. Unit: system-locked `enabled = false` + `OCX_TELEMETRY=1` → 0.
- **S-010 Managed payload arms.** Seed a matching managed snapshot whose payload carries `[telemetry] enabled = true, endpoint = <capture>, headers = { authorization = "Bearer t" }`; no other config → 1 request carrying `authorization`. → `OCX_NO_CONFIG=1` → 0 (tier skipped).
- **S-011 Dead collector.** For each of: closed port, `status=503`, `hang`, `delay=3000` with timeout 300 ms, `redirect` to a second capture: exit code and stdout byte-identical to the unarmed run of the same command; wall time ≤ unarmed + timeout + ε; `503` → exactly 1 request (no retry); `redirect` → second capture gets 0. Repeat with a failing command (exit 79): same code, same JSON error document.
- **S-012 `OCX_CONFIG` list.** (a) Two files setting `[registry] default` differently, `OCX_CONFIG=f1<sep>f2` → `f2`'s value applies. (b) Every existing single-path test in `test_config.py` passes unchanged. (c) `OCX_NO_CONFIG=1 OCX_CONFIG=f1<sep>f2` → both load (user config ignored). (d) `f1<sep>missing` → exit 79 naming `missing`. (e) `<sep>f1<sep><sep>` → loads `f1`. (f) five entries → exit 78, slug `config_list_too_long`. (g) `--config f3` beats `f2`. (h) A telemetry-only file appended to a user's existing single `OCX_CONFIG` arms export without displacing it.
- **S-013 `ocx.toml` cannot configure telemetry.** Project with `[telemetry] enabled = true, endpoint = B`; armed by env to A → `ocx pull` exits 78, slug `telemetry_section_in_project`; A gets 1 request whose record carries 78 and that slug; B gets 0. Without the env switch and with no other config → 0 requests anywhere.
- **S-014 Real collector round trip.** Start the pinned `otelcol`; armed `ocx package install <pkg>` with `[telemetry].endpoint` at its receiver → within 30 s the file exporter's output decodes to one request with one `sh.ocx.command` (name `package install`, `process.exit.code` 0) and one `sh.ocx.package` (`action = installed`). → Missing binary: fails naming the tool.
- **S-015 Package records.** Fresh home: `ocx package install <pkg-with-dep>` → root record `installed`, `role = root`, `store_hit = false`, `download.size > 0`, three durations; dependency record `pulled`, `role = dependency`. Re-run → `store_hit = true`, no `download.*`, no `extract.duration`. `ocx package pull <other>` → `pulled`. `ocx package uninstall <pkg>` → one `removed` record whose purl equals the install's once the `arch` qualifier is removed from both (R-14). Project `ocx pull` → one `pulled` record per lock entry. `ocx env` with everything cached → no package records.
- **S-016 Process and CI fields.** POSIX: `process.pid` equals the spawned ocx's pid, `process.parent.pid` equals the test process's pid; Windows: `process.parent.pid` absent. With `GITHUB_ACTIONS=true GITHUB_RUN_ID=11 GITHUB_JOB=build` → `sh.ocx.ci.provider = github`, `cicd.pipeline.run.id = 11`, `cicd.pipeline.task.name = build`, no `task.run.id`. With `GITLAB_CI=true CI_PIPELINE_ID=21 CI_JOB_ID=22 CI_JOB_NAME=test` → `gitlab`, 21, `test`, 22. With `CI=true` only → `other`, no ids. The child environment of an armed `ocx exec` equals the unarmed one (no new variable).
- **S-017 Operator override.** S-016's GitHub env plus `OTEL_RESOURCE_ATTRIBUTES=cicd.pipeline.run.id=override,team=a%2Cb` → `override`, `team = a,b`. Add `[telemetry] resource_attributes = { team = "c" }`, `service_name = "ocx-ci"`, `OTEL_SERVICE_NAME=x` → `team = c`, `service.name = ocx-ci`.
- **S-018 Failure record.** Armed `ocx --format json package install <missing-tag>` → exit 79; record `process.exit.code = 79`, `error.type` equals the error document's `error.detail`. A command returning a non-zero `Ok` (`ocx update --check` with drift, exit 65) → code 65, no `error.type`.
- **S-019 Unusable section.** `[telemetry] enabled = true, endpoint = <capture>, timeout = "fast"` → command exit 0, stdout unchanged, one warning naming the file path and `timeout` (not `"fast"`), 0 requests; same with `OCX_TELEMETRY=1`. A broken `[telemetry]` in an unlocked user file under `OCX_NO_CONFIG=1` → no warning.
- **S-020 Secrets never printed.** File header `authorization = "Bearer s3cr3t"`, `OCX_TELEMETRY_HEADERS=x-token=t0k3n`, `OTEL_EXPORTER_OTLP_HEADERS=x-otel=o1`; run with `--log-level trace` and `--format json`, against ok, 503 and a malformed-config variant → none of `s3cr3t`, `t0k3n`, `o1` on stdout or stderr; the debug line lists the header names. Also (R-2, R-5): an endpoint carrying `?api-key=q5ecr3t` against a closed port, a 503 and a URL `http::Uri` refuses; and a config file whose broken line holds `Bearer s3cr3t`, run with `--format json` → neither secret on stdout (error document included) or stderr.
- **S-021 Installation id.** First armed run with destination → file exists, mode 0600 (POSIX), lowercase v4 UUID, equal to the wire `sh.ocx.installation.id`; second run → same value. Unarmed, or armed without destination → no file. File replaced by garbage → attribute absent, file content unchanged. `$OCX_HOME/state` read-only → attribute absent, exit 0.
- **S-022 Nested `ocx`.** Armed; `ocx exec -- ocx package pull <pkg>` → exactly 1 request, from the inner command; on POSIX its `process.pid` equals the execution record's `process.pid` when `[records]` is on. `ocx exec --clean -- ocx package pull <pkg>` with env-only arming → 0 requests (documented).
- **S-023 One request, many records.** Armed `ocx package install <a> <b>` → 1 POST holding 1 command record and ≥ 2 package records.
- **S-024 Lock survival (unit).** System file (test seam) with `[telemetry] enabled = true, endpoint = E`; `OCX_NO_CONFIG=1` → merged section present and locked; user file `endpoint = F` → ignored; unlocked system file absent → under `OCX_NO_CONFIG` the section is `None`.
- **S-025 Doc binding.** C-032.
- **S-026 Pin guard red.** Editing any pin, feature, or adding a `tonic` dependency turns `opentelemetry_pins_match_the_exemption` red (shown once in the PR body, then restored).

---

## 8. Error taxonomy

| Class | Surfaces as | Exit code |
|---|---|---|
| Telemetry off (any § 3.1 row) | debug line | unchanged |
| No destination | debug line | unchanged |
| Export failure (connect, DNS, TLS, status, redirect, timeout, encode, panic) | debug line | unchanged |
| Unusable telemetry input (ADR D-11) | one stderr warning, never a value | unchanged |
| `OCX_CONFIG` entry missing | existing `config_file_not_found` | 79 |
| `OCX_CONFIG` entry not a regular file | existing `Error::Io` | as today |
| `OCX_CONFIG` over 32 entries | new `config_list_too_long` | 78 |
| `[telemetry]` in `ocx.toml` | new `telemetry_section_in_project` | 78 |
| Received `policy.config_path` with a relative entry | exec-state payload unreadable (existing warning) | as today |

No telemetry type implements `Classify`; nothing from `ocx_telemetry` reaches `classify_error`.

---

## 9. Edge cases

| Case | Behaviour |
|---|---|
| `OCX_CONFIG` entry contains the separator | splits; the halves are missing files → exit 79 (the one break) |
| Same file listed twice | loads twice; idempotent |
| `--config` path containing the separator, forwarded to a child | refused by the parent, exit 78 `config_path_contains_separator` (R-10) |
| Older same-major `ocx` receives a list | treats it as one path → exit 79; never loads a wrong file |
| Endpoint IPv6 literal (`http://[::1]:4318`) | hyper dials the literal without a lookup; nothing to resolve (R-6) |
| Endpoint behind `HTTPS_PROXY` | goes through the proxy per `NO_PROXY`; guard's proxy rules apply |
| `http://` endpoint with headers | sent in clear; documented warning |
| Collector returns 2xx with partial success | treated as success; body unread |
| Collector accepts TCP and never answers | bounded by the timeout |
| DNS resolver blocks | the endpoint lookup runs on a detached thread under the deadline (R-26); the proxy host is resolved the same way and pinned (`pin_proxy`), and the CA-root load in the client build runs detached under the same deadline |
| `self update` background apply installs during the run | its package activity joins this command's records |
| `ocx self setup --handoff` child | a nested `ocx` with its own record |
| Deferred tool (`lazy-mode always`) | no package record until `launcher shim` materializes it, which sends nothing |
| Package with no logical identity (`file-url-mode/`) | no package record |
| `ocx clean` | no package records |
| Login shell evaluating `ocx --global env --shell` | one `env` record per shell start when armed |
| `ocx exec --clean` child | ambient switch and opt-outs absent; config-file arming still applies |
| Unknown key in `[telemetry]` | ignored silently |
| `[telemetry]` in a payload for an older `ocx` | ignored as an unknown section by that binary |
| Ctrl-C or kill before finish | nothing sent |
| Panic in the export task | `JoinError` → debug line; the default panic message reaches stderr |
| Read-only `$OCX_HOME` | id omitted, export proceeds |
| Two concurrent first runs creating the id | atomic rename; one value wins; both runs may report different ids once |

---

## 10. Rollout and migration

1. Commit series A — `feat(config)!: OCX_CONFIG accepts a list of config files`: C-004, C-005, the G14 and reports ledger entries, goldens, regenerated `environment.md`. Changelog = the subject; no migration prose in user docs.
2. Commit series B — `feat(telemetry): …` in dependency order: `ocx_env` declarations (C-001…C-003), `ocx_config` section (C-006…C-010), `ocx_store` accessor, `ocx_package_manager` activity (C-024, C-025), `ocx_telemetry` crate (C-011…C-020, C-026…C-029), CLI wiring (C-021…C-023), goldens and docs (C-030…C-032), test infrastructure (C-033, C-034), scenarios.
3. `chore:` commits for C-036.
4. `task verify:scoped --force` per step; `task satellite:verify` before finalize (C-037); size recorded (C-038).
5. Owner step: publish the collector package; pin it with `ocx add -g test` and `ocx lock`. Until then criterion 12 reads not met and the run reports it as an owner action.

---

## 11. Goal criterion → contracts and scenarios

| # | Criterion | Contracts | Scenarios |
|---|---|---|---|
| 1 | ADR accepted: OBS-12 exemption and amendments | `adr_otel_telemetry.md`; C-028, C-036 | S-026 |
| 2 | `[telemetry]` loads from every config tier | C-006, C-007, C-008, C-009, C-010 | S-003, S-010, S-024 |
| 3 | `OCX_CONFIG` is a path list | C-004, C-005 | S-012 |
| 4 | Switch and header variables declared and documented | C-001, C-002, C-003, C-031 | — |
| 5 | Unconfigured `ocx` sends nothing | C-011, C-012 | S-001, S-002, S-005 |
| 6 | One request per armed command; exec and prompt hooks silent | C-019, C-021, C-022, C-023 | S-004, S-006, S-023 |
| 7 | Command record answers usage and failures | C-014, C-016, C-032 | S-018, S-025 |
| 8 | Package records carry purl, action and performance | C-015, C-024, C-025 | S-015 |
| 9 | Records carry pid, parent pid and CI run id | C-013, C-026, C-027 | S-016, S-017, S-022 |
| 10 | Opt-out and destination precedence under test | C-011, C-012 | S-007, S-008, S-009, S-010 |
| 11 | Dead collector never changes exit code or stdout | C-018, C-019, C-021 | S-011 |
| 12 | Standing test against a real collector | C-034 | S-014 |
| 13 | `ocx.toml` cannot configure telemetry | C-010, C-035 | S-013 |
| 14 | Docs tell one story with execution records | C-031, C-032 | S-025 |
| 15 | Telemetry crate in crate map; scoped and satellite verify pass | C-028, C-029, C-037, C-038 | — |

---

## 12. Risks

| Risk | Probability | Impact | Mitigation |
|---|---|---|---|
| 0.33.1 builder rejects an ambient compression value | resolved | — | the builder skips a value whose feature is off (`src/exporter/mod.rs:258-285`); `test_ambient_compression_leaves_the_request_uncompressed` pins it |
| `otelcol` file-exporter JSON casing differs from protobuf-JSON | Medium | parser rework | inspect one capture file first (research § A); parse plain JSON if needed |
| Two concurrent collectors collide on port 8888 | Medium | flaky real-collector test | verify `metrics.level: none` with two concurrent instances; else per-test metrics port |
| `@tools` cannot expose a test-group-only binary under Bazel | Medium | test cannot find the tool | the `ocx-previous` route (`module_data` + `ocx --project … env -g test`) |
| Exit-time DNS stall | Low | run exceeds timeout | endpoint, proxy-host and proxy-settings reads detached under the deadline (R-26), `stalled_resolver_never_outlives_the_deadline`, `the_proxy_rules_read_in_new_is_bounded_by_the_deadline` |
| Collector package not published | High until the owner acts | criterion 12 not met | owner action in the DONE block |

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-11-18 | architect (hex-plan xhigh) | Initial design: C-001…C-038, S-001…S-026, decision tables, field tables, goal map |
| 2026-11-19 | hex-plan orchestrator | Review round 1: R-1…R-30 folded into C-002, C-004…C-007, C-017…C-019, C-021, C-022, C-024, C-028, S-006, S-008, S-015, S-020, § 9, § 12 (ADR § Amendment 2026-11-19) |
