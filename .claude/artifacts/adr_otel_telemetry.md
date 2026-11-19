# ADR: OpenTelemetry usage export (OBS-12 exemption)

## Metadata

**Status:** Accepted 2026-11-18 under goal-loop authority (.agents/goals/otel-telemetry.md); the owner may revisit
**Date:** 2026-11-18
**Deciders:** @michael-herwig (closed positions in `.agents/discussions/otel-telemetry.md` § Decisions), architect (hex-plan xhigh, branch `hex/otel-telemetry`) for everything the discussion left open
**GitHub Issue:** none
**Related:** source discussion `.agents/discussions/otel-telemetry.md`; goal `.agents/goals/otel-telemetry.md`; companion design record [`system_design_otel_telemetry.md`](./system_design_otel_telemetry.md) (contracts `C-001…`, scenarios `S-001…`)
**Tech Strategy Alignment:**
- [x] Rust 2024, Tokio, the `reqwest` 0.13 client already in the graph; no second TLS stack
- [x] Deviation from the vendored rule OBS-12 justified below (§ OBS-12 exemption)
**Domain Tags:** integration, security, api, devops
**Supersedes:** none. **Superseded By:** none.
**Scope:** Large · **One-Way-Door: HIGH** — the switch and header variable names, the `[telemetry]` keys, the `OCX_CONFIG` list grammar, and the record's event and attribute names become contracts the moment an operator's backend queries them.

---

## Context

The owner asked for telemetry in OCX: an operator who runs `ocx` in an environment they own (a CI image, a devcontainer, a developer fleet) wants `ocx` usage in a collector they own. Grimoire shipped the same feature (`/home/mherwig/dev/grimoire-rs/grimoire/.agents/adr/adr_otel_usage_telemetry.md`, amended 2026-11-13 by `adr_grim_config_layers.md`) and is the bootstrap. OCX already delivers settings to a fleet through the managed config tier (`adr_managed_config_tier.md`) and already records tool launches as files (`adr_exec_resolution_record.md`).

What exists today, read from code (full map: [`discover_otel_telemetry.md`](./discover_otel_telemetry.md)):

| Fact | Evidence |
|---|---|
| No `opentelemetry*`, `tonic` or telemetry variable anywhere in the product | `Cargo.lock` (0 matches); `crates/ocx_env/src/vars.rs` |
| `main` is `app::finish(app::run().await)`; `finish` is synchronous and structurally pinned | `crates/ocx_cli/src/main.rs:8-13,25-46`; `crates/ocx_cli/src/app/boundary.rs:13-22,50-89` |
| Help and `--version` exit inside clap; usage errors and five Context-free arms return before `Context::try_init` | `crates/ocx_cli/src/app.rs:94-147` |
| Exit code and `error.detail` slug come from one function | `crates/ocx_cli/src/exit.rs:174` (`classify_decision`) |
| Unix `ocx exec` replaces the process; Windows exits inside the library | `crates/ocx_package_manager/src/launch/child_process.rs:53-55,83` |
| `OCX_CONFIG` names one file; the received exec state carries one `config_path` | `crates/ocx_config/src/loader.rs:93-114`; `crates/ocx_config/src/exec_state/policy.rs:54`; `crates/ocx_cli/src/app/context.rs:197-216` |
| `retain_system_locked_sections` destructures `Config` exhaustively; `[records]` survives when locked | `crates/ocx_config/src/loader.rs:1110-1149` |
| `ocx.toml` is `deny_unknown_fields` and refuses `[shell]` and `[update]` by name | `crates/ocx_project/src/config.rs:127-129,514-527` |
| Background checks share one gate that skips in CI | `crates/ocx_cli/src/app/background_check.rs:36-54`; `.claude/rules/subsystem-cli.md` § Background Checks |
| Every `reqwest` client is guarded or listed in a shrink-only baseline | `crates/ocx_oci/CLAUDE.md`; `crates/ocx_test_support/tests/fixtures/ssrf_unguarded_baseline.txt` |
| Execution records name the process `process.pid`, `process.parent.pid`, `host.name` (via `sysinfo`), `os.type`, `process.arch`, and packages as `pkg:oci` purls | `crates/ocx_package_manager/src/record/environment.rs:14-39,89-98`; `record/purl.rs:30` |
| The exec-state decoder refuses more than 8 recorded tier paths | `crates/ocx_config/src/exec_state/types.rs:28`; `exec_state/decode.rs:173`; written by `command/self_group/activate.rs:270` |

The vendored rule **OBS-12** (`.claude/rules/rust-quality/docs-and-tracing.md:133`) forbids an `opentelemetry*` dependency in a short-lived CLI unless the justification is written down. This ADR is that record.

---

## Decision Drivers

| ID | Driver | Weight |
|---|---|---|
| DR1 | **Never change a command's outcome.** Exit code and stdout identical with or without a collector; added wall time bounded by a timeout | gate |
| DR2 | **Off unless an operator arms it.** No default destination, no export on `OTEL_*` alone (consistent with the earlier phone-home rejection, `research_deprecation_telemetry_patterns.md:15`) | gate |
| DR3 | **Backend reach**: protobuf OTLP/HTTP reaches every backend without a collector hop | x2 |
| DR4 | **Do not own a wire encoder** (`quality-core.md` § Don't Own Non-Domain Code; a hand-rolled emitter is Block-tier) | x3 |
| DR5 | **Exit latency by construction**, not by tuning a library's defaults | x3 |
| DR6 | **Credential hygiene**: a token reaches only the destination it belongs to | x2 |
| DR7 | **Binary size and crate count** | x1 |
| DR8 | **Grimoire parity**: port, do not reinvent (owner, 2026-11-18) | x2 |
| DR9 | **Upgrade churn**: the `opentelemetry*` crates break on every minor | x1 |

---

## Industry Context & Research

**Research artifacts:** [`research_telemetry_collector_and_sdk.md`](./research_telemetry_collector_and_sdk.md), [`research_telemetry_codebase_recon.md`](./research_telemetry_codebase_recon.md), [`research_telemetry_prior_art.md`](./research_telemetry_prior_art.md), [`research_telemetry_repo_archaeology.md`](./research_telemetry_repo_archaeology.md), [`research_telemetry_admin_enforced_config.md`](./research_telemetry_admin_enforced_config.md), [`research_telemetry_council_precedence.md`](./research_telemetry_council_precedence.md) (its "Orchestrator synthesis" is superseded on the token, see D-7).

**Trending approach:** operator-directed OTLP from CLIs, gated by an explicit tool switch rather than by the presence of `OTEL_*` (Docker, Pulumi, Terraform, Claude Code). Exit-path stalls from SDK retries and flushes are the recurring failure (Claude Code incidents; Docker caps its flush at 50 ms).

**Key insights:**
1. `opentelemetry-otlp` 0.33.1 is usable from a one-shot CLI only if the tool drives the exporter itself: the crate's processors deadlock or stall, its retries are on by default, and it merges `OTEL_EXPORTER_OTLP_[LOGS_]HEADERS` into every request unconditionally (Grimoire measured a bearer-token leak).
2. The Rust SDK does not read the certificate variables and ignores `OTEL_SDK_DISABLED`; a tool that honours them must do so itself.
3. Logs and the logs OTLP exporter are stable in 0.33.1; traces are beta.

---

## Considered Options

### Option A — `opentelemetry-otlp` 0.33.1 (`http-proto`), ocx drives the exporter (chosen; Grimoire parity)

A collect-only logger provider mints the records; ocx calls `LogExporter::export` once with one batch, under `tokio::time::timeout`, through its own `HttpClient` over a `reqwest::Client` with ocx's TLS roots.

| Pros | Cons |
|---|---|
| Protobuf to every backend; maintained encoder | ~+24 crates and ~+0.5 MB (Grimoire measured +416 KB dist, +655 KB release) |
| Every measured crate hazard becomes a pinned, tested condition | Logs API is a bridge the crate documents as not for direct calls; a minor bump may break the glue |
| Same `reqwest` 0.13 as ocx; no second TLS stack | Needs counter-measures for env headers, retries, processors and `OTEL_SDK_DISABLED` |

### Option B — hand-written OTLP/HTTP+JSON over `reqwest` + `serde_json`

| Pros | Cons |
|---|---|
| ~0 new crates; OBS-12 untouched | JSON only: protobuf-only backends need a collector hop |
| Full control of bytes and headers | Owns a wire encoder (partial success, path rules, body caps) — Block-tier under `quality-core.md` |

### Option C — the crate's own processors and `reqwest-client` transport

| Pros | Cons |
|---|---|
| Least glue | Measured: simple processor deadlocks on a current-thread runtime and sends one POST per record; batch processor panics with an async client; default retries plus a 10 s timeout stall exit 5–15 s; env headers merged |

### Option D — `tracing-opentelemetry` / logs-appender bridge

| Pros | Cons |
|---|---|
| Reuses the `tracing` subscriber | Record shape follows log lines, not a frozen field table; same processor problems as C; OCX logs mostly through `log`, not `tracing` |

### Option E — no OTLP: extend execution records to every command, operators ship files

| Pros | Cons |
|---|---|
| Zero network, zero crates | Fails the owner's requirement (OTLP export to a collector); needs a shipper on every host |

### Weighted comparison (1 worst, 5 best)

| Driver (weight) | A | B | C | D | E |
|---|---|---|---|---|---|
| DR1 / DR2 gates | pass | pass | **fail** (stalls) | **fail** (stalls) | **fail** (no OTLP) |
| DR3 reach (x2) | 5 | 2 | 5 | 5 | 1 |
| DR4 no owned encoder (x3) | 4 | 1 | 5 | 4 | 5 |
| DR5 latency by construction (x3) | 5 | 5 | 1 | 1 | 5 |
| DR6 credentials (x2) | 4 | 5 | 2 | 2 | 5 |
| DR7 size (x1) | 3 | 5 | 3 | 2 | 5 |
| DR8 parity (x2) | 5 | 1 | 2 | 1 | 1 |
| DR9 churn (x1) | 2 | 4 | 2 | 1 | 5 |
| **Total (max 70)** | **60** | 43 | 41 | 34 | 54 |

E scores high on everything it does and fails the requirement. B is the honest runner-up and loses on DR4 and DR8.

---

## Decision Outcome

**Chosen: Option A**, the owner's position inherited from Grimoire (discussion § Decisions, "Working position … the maintained `opentelemetry-otlp` crate over a hand-written encoder").

**Rationale.** A is the only option that passes both gates and owns no encoder. OBS-12's concern — provider setup and flush-on-shutdown for a process that lives seconds — is answered by construction: no provider runs a worker, nothing flushes at shutdown, and the whole export is one request bounded by a 500 ms default deadline that runs only when an operator armed it and named a destination. An unarmed run builds no OTel type and no client.

**Explicitly not in the design:** traces, spans, the metrics signal, `TRACEPARENT` read or write, any default or vendor destination, a crate shared with Grimoire, drop-in directories and `include`, a first-run prompt, a live check against `otel.ocx.sh`, the existing CI telemetry (Bazel BEP and JUnit export from workflows, not product code), and any new variable written into a child process.

---

## OBS-12 exemption — the conditions it is granted on

OBS-12 lives in a vendored rule file, which is **not edited**. Every `Cargo.toml` line that names an `opentelemetry*` crate carries the one-line comment `# OBS-12 exemption and its conditions: .claude/artifacts/adr_otel_telemetry.md`. The exemption holds only while every condition holds. Test names are binding on the plan; contracts are in the design record.

| # | Condition | Pinned by |
|---|---|---|
| 1 | **Exact pins.** `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-http` at `=0.33.1` in root `[workspace.dependencies]`, all `default-features = false`. Features exactly: `["logs"]`, `["logs"]`, `["http-proto", "logs"]`, `[]`. Forbidden: `reqwest-client`, `reqwest-blocking-client`, `hyper-client`, `grpc-tonic`, `gzip-http`, `zstd-http`, `http-json`, `internal-logs`, any `tls-*`, any `rt-*`, any `experimental*`. `Cargo.lock` holds no `tonic`. A bump is a deliberate edit with a changelog row here | `ocx_telemetry` unit test `opentelemetry_pins_match_the_exemption` (parses the manifests and the lock; C-028) |
| 2 | **One crate.** Only `crates/ocx_telemetry/Cargo.toml` depends on an `opentelemetry*` crate | same test, scanning every `crates/*/Cargo.toml` |
| 3 | **Gate before construction.** The command class, the opt-outs, the switch and the `[telemetry]` section are decided before any OTel type, exporter or HTTP client exists; no destination means nothing is built and nothing is written | `ocx_telemetry::gate` unit test `gate_truth_table`; `off_never_reaches_the_exporter` (export double counting calls); acceptance S-001, S-003, S-006 |
| 4 | **No crate processor, no crate transport.** A collect-only processor; exactly one `LogExporter::export` of one `LogBatch` per run under `tokio::time::timeout`; the exporter receives ocx's own `HttpClient` | acceptance `test_one_post_carries_command_and_package_records` (S-023); structural unit test `no_crate_processor_or_transport_is_named` (non-zero needle on the collect-only processor, zero on `BatchLogProcessor`, `SimpleLogProcessor`, `with_batch_exporter`, `with_simple_exporter`) |
| 5 | **Headers are ocx's.** The client drops every header the crate set and inserts only `user-agent: ocx/<version>`, the headers ocx selected, and `content-type: application/x-protobuf` last | acceptance `test_ambient_otel_headers_never_reach_an_ocx_destination`, `test_logs_endpoint_never_gets_generic_headers`, `test_content_type_cannot_be_overridden` |
| 6 | **No retries, bounded time, no redirects.** `RetryPolicy::disabled()`; connect and total timeout equal the export timeout (default 500 ms, at most 10 000 ms); `redirect(Policy::none())` | acceptance `test_no_retry_on_503`, `test_dead_collector_never_changes_outcome` (refused, status, hang, delay), `test_redirect_is_not_followed` |
| 7 | **No detectors.** `Resource::builder_empty()`; ocx parses `OTEL_RESOURCE_ATTRIBUTES` itself (percent-decoded); no `telemetry.sdk.*`, argv, cwd, user name or environment dump | unit `golden_payload` (C-016); acceptance `test_never_collected` |
| 8 | **Silent commands never construct telemetry.** The exec family, the per-prompt commands, the Context-free arms and every clap exit construct nothing (D-12) | unit `every_leaf_has_a_telemetry_class`; acceptance `test_never_send_commands_send_nothing` (S-006) |
| 9 | **Guarded destination.** The client seeds `ocx_oci::GuardedResolver` with no trusted host (a resolver miss on the endpoint name fails closed); the SSRF baseline gains no line | `workspace_structure::ssrf_guard_ratchet` green with `ssrf_unguarded_baseline.txt` unchanged on the branch |
| 10 | **Size recorded.** Release-profile binary size of `ocx` before (merge base) and after, linux x86_64, recorded in § Quantified Impact before the branch lands | the recorded row (C-038) |

**Closing clause.** A crate processor, a background worker, the traces or metrics signal, `TRACEPARENT`, a default destination, or a second crate depending on `opentelemetry*` is outside this exemption and needs a new ADR.

---

## Decisions

### D-1 Crate placement and tiers

- New crate **`ocx_telemetry`** (package name `ocx_telemetry`), **internal tier**, owns: the arming gate, destination selection, the resource and record builders, the installation id, the HTTP client and the one export. It reads every variable through `ocx_env`.
- `scripts/crate_map.toml`: `ocx_telemetry = ["ocx_config", "ocx_env", "ocx_oci", "ocx_util"]`; the `ocx` row gains `"ocx_telemetry"`. No other row changes. Mirrored in `crates/ocx_test_support/tests/workspace_structure.rs`.
- `ocx_config` (ecosystem) owns the `[telemetry]` section type `ocx_config::telemetry::TelemetryConfig` and its merge and lock. It gains the `secrecy` dependency (D-9).
- `ocx_package_manager` gains **no** telemetry dependency; it exposes plain activity data (D-17). `ocx_store` gains one accessor, `StateStore::telemetry_installation_id_file()` → `state/telemetry/installation-id`. The CLI passes that path in.
- `ocx_exit` is not a dependency: no telemetry error ever crosses into the exit-code chain.

### D-2 The switch: `OCX_TELEMETRY`

`OCX_TELEMETRY: TriBool, Public, propagation = UserInput`. Positive name per IC-13 (no flag exists, so none is mirrored). Grammar:

| Value | Meaning |
|---|---|
| unset, or empty | defers to config |
| `1`, `y`, `yes`, `on`, `true` (ASCII case-insensitive) | on |
| `0`, `n`, `no`, `off`, `false` | explicit off |
| anything else, non-UTF-8 included | explicit off, plus one warning per run naming the variable, never the value |

A typo never arms export and never defers to a file that could arm it (Grimoire parity). `OnInvalid::Error` is not used: the switch never fails a command.

### D-3 The secret header variable: `OCX_TELEMETRY_HEADERS`

`OCX_TELEMETRY_HEADERS: String, Public, secret, propagation = Credential` (amended by R-13: removed at every spawn, as `subsystem-cli.md` § Credential exemption requires of a bearer-credential variable). Name mirrors the config key `[telemetry] headers` (as `OCX_RECORDS_DIR` mirrors `[records] dir`). Grammar: the OTel exporter header grammar `name=value[,name=value…]`, names trimmed, values percent-decoded. Name and value rules as the config key (D-4). It merges over the merged `[telemetry].headers` per name, case-insensitive, except that a name a **system-locked** section sets wins. It is sent **only** to an OCX-config destination (D-10). A malformed value is D-11. Read only through `SecretVar` → `Sensitive`. Consequence: a nested `ocx` under `ocx exec` armed only by environment exports without this token; a token in a config file (any tier, or an `OCX_CONFIG` entry, both inherited) keeps working, and the docs recommend the file form for tokens.

### D-4 The `[telemetry]` section

Grimoire's keys, unchanged in name:

| Key | Type | Rule |
|---|---|---|
| `enabled` | bool | `true` arms (D-10) |
| `endpoint` | string | absolute `http`/`https` URL with a host and no userinfo; ocx appends `/v1/logs` to its path (one `/` between) |
| `headers` | table of strings | name is an HTTP token and not `host`, `content-length`, `content-type`, `transfer-encoding` (any case); value has no CR, LF or NUL; values are secrets (D-9) |
| `timeout` | integer, milliseconds, `1..=10000` | default 500 |
| `protocol` | string | only `http/protobuf` (ASCII case-insensitive); absent means `http/protobuf` |
| `service_name` | string, non-empty | replaces `service.name` |
| `resource_attributes` | table of strings, keys non-empty | added to the resource per key |

Unknown keys inside the section are ignored, as everywhere in `Config` (`arch-principles.md` § fleet forward-compat); the published JSON schema catches typos. A known key with a wrong type or value makes the section **unusable** for that layer (D-11), without failing the file's parse: the section deserializes leniently, the precedent being `refresh::deserialize_lenient` (`crates/ocx_config/src/refresh.rs:52`). No `${VAR}` substitution, no `headers_file`, no helper command (Grimoire's reasons hold: a pointed-at file must not pick which secret goes where).

### D-5 Merge per tier

Fold order is the existing one: compiled defaults, system, user, `$OCX_HOME`, managed payload, each `OCX_CONFIG` entry in list order, `--config`. In `TelemetryConfig::merge`: a set scalar in a later layer replaces the earlier one; **a layer that sets `endpoint` drops every header inherited from earlier layers** (R-1: endpoint and headers come from one source, so a token is never sent to a destination its author did not name; headers-only layers after it, and `OCX_TELEMETRY_HEADERS`, still add); otherwise `headers` merge per name, ASCII case-insensitive, later wins; `resource_attributes` merge per exact key, later wins; an unusable later layer marks the result unusable and records its origin; a system-locked accumulator ignores every later layer whole (the `RecordsOptions::merge` shape, `crates/ocx_config/src/records.rs:66-81`).

### D-6 Lockability and `OCX_NO_CONFIG` (OQ-1, F10)

- `apply_system_locks` locks a system-tier `[telemetry]` unconditionally and per block, like `[records]` (`loader.rs:866-868`): every key clamps together.
- `retain_system_locked_sections` keeps `telemetry` when `system_locked`, drops it otherwise (`loader.rs:1130` shape). The exhaustive destructure gains the field.
- `fold_managed_tier` changes nothing for `[telemetry]` (D-7).
- What a lock does **not** clamp: the opt-outs and an explicit-off `OCX_TELEMETRY` (D-10 ranks 2–3). What it does clamp: an on `OCX_TELEMETRY` cannot override a locked `enabled = false`; `OCX_TELEMETRY_HEADERS` cannot replace a header name the locked section sets.

### D-7 Managed tier: no carve-out

The managed payload may carry the whole `[telemetry]` section, `headers` and token included (owner, 2026-11-18). `fold_managed_tier` (`loader.rs:237-335`) gains no strip and no guard for it. A system lock clamps it as it clamps every section. The docs warn that anyone who can pull the managed-config artifact can read a token in it (OQ-13).

### D-8 Project tier (F7)

- `ocx.toml` refuses `[telemetry]` by name: `RawProjectConfig` declares `telemetry: Option<toml::Table>` only to refuse it, raising a new `ocx_project::Error::TelemetrySectionInProject(PathBuf)` — exit 78 `ConfigError`, slug `telemetry_section_in_project`, message `<path>: [telemetry] belongs in config.toml, not in ocx.toml`. Same shape as `UpdateSectionInProject` (`crates/ocx_project/src/error.rs:42-50`).
- `ConfigLoader::fold_project_tier` (`loader.rs:1057-1077`) also strips `telemetry` with a warning, so the property does not rest on `deny_unknown_fields` alone (the C-033 precedent for `[shell]`).
- A repository file never arms, disarms or redirects export (Grimoire's rung rule; the competitive-lane defect class).

### D-9 Secret typing (F5)

Header values in `TelemetryConfig` are `secrecy::SecretString` (or a newtype over it whose `Debug` prints `<redacted>`), so the derived `Debug` of `Config` never prints a token. The JSON schema describes them as strings (`#[schemars(with = …)]`). Warnings name a file path, a key or a variable, never a value; a TOML parse error that would quote a source line is reported by line and column only. The debug payload line lists header **names**, never values (D-24). `ocx_config` gains `secrecy` (workspace dependency `secrecy = "0.10.3"`, already used by `ocx_cli` and `ocx_oci`).

### D-10 Precedence: one ladder

Evaluated in this order; the first matching row decides.

| Rank | Input | Effect |
|---|---|---|
| 1 | Command in the never-send set (D-12), or the in-process test seam | nothing evaluated, nothing built |
| 2 | Opt-out: `DO_NOT_TRACK` truthy, `OTEL_SDK_DISABLED` truthy, `OTEL_LOGS_EXPORTER` = `none` (ASCII case-insensitive), or offline (`--offline`, `OCX_OFFLINE`, received narrowing, credential-helper mode — `Context::is_offline()`) | off; beats every file and every lock |
| 3 | `OCX_TELEMETRY` explicit off (falsy or unrecognized) | off; beats every file and every lock |
| 4 | merged `[telemetry]` unusable | off, one warning (D-11) |
| 5 | system-locked `[telemetry] enabled = false` | off |
| 6 | `OCX_TELEMETRY` on | armed |
| 7 | merged `[telemetry] enabled = true` | armed |
| 8 | otherwise | off (default) |

Truthy for `DO_NOT_TRACK` and `OTEL_SDK_DISABLED` is ocx's boolean set (`1`, `y`, `yes`, `on`, `true`), a superset of the OTel spec's `true`, erring toward the opt-out.

**Destination** — one unit, chosen only when armed; endpoint, headers, timeout and protocol never mix families:

| Rank | Source | Endpoint | Headers | Timeout | Protocol |
|---|---|---|---|---|---|
| 9 | merged `[telemetry].endpoint` set | it, `+ /v1/logs` | merged `[telemetry].headers`, then `OCX_TELEMETRY_HEADERS` per name | `[telemetry].timeout` or 500 ms | `[telemetry].protocol` |
| 10 | `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | verbatim | `OTEL_EXPORTER_OTLP_LOGS_HEADERS` only | `…_LOGS_TIMEOUT` ▸ `…_TIMEOUT` ▸ 500 ms | `…_LOGS_PROTOCOL` ▸ `…_PROTOCOL` ▸ `http/protobuf` |
| 11 | `OTEL_EXPORTER_OTLP_ENDPOINT` | it, `+ /v1/logs` | `OTEL_EXPORTER_OTLP_LOGS_HEADERS` if set, else `OTEL_EXPORTER_OTLP_HEADERS` | same as 10 | same as 10 |
| 12 | none | — | — | — | nothing sent, nothing written, one debug line |

Under rank 9 ocx reads no `OTEL_EXPORTER_OTLP_*` variable. Under ranks 10–11 neither `[telemetry].headers` nor `OCX_TELEMETRY_HEADERS` is sent; if either is set, one warning names both sources, never a value. An OTEL timeout outside `1..=10000` falls back to 500 ms with a debug line.

**Resource attributes**, per key, lowest first, for either destination: ocx's derived keys (D-15), `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_SERVICE_NAME`, `[telemetry].resource_attributes`, `[telemetry].service_name`. An operator's attribute therefore overrides a detected one, CI ids and pids included.

### D-11 Unusable input: fail closed for telemetry, never for the command

| Input | When unusable | Effect |
|---|---|---|
| `[telemetry]` in any contributing layer | a known key with a wrong type or value | off for the run; one warning `telemetry is off: [telemetry] in <origin> has an invalid <key>` (`<origin>` a path or `managed config <source>`) — even with `OCX_TELEMETRY=1`, because the broken layer may have named the destination |
| `OCX_TELEMETRY` | not a boolean | explicit off, one warning (D-2) |
| `OCX_TELEMETRY_HEADERS` | bad grammar or a refused name | off, one warning naming the variable |
| selected OTEL endpoint | not `http`/`https`, no host, userinfo | nothing sent, one warning naming the variable |
| selected OTEL headers | bad grammar or a refused name | nothing sent, one warning naming the variable |
| selected protocol | anything but `http/protobuf` | nothing sent, one warning naming the source |
| OTEL timeout | not an integer in range | 500 ms, debug line |
| `OTEL_RESOURCE_ATTRIBUTES` | a malformed pair | that pair skipped, debug line |

Warnings go to stderr through the existing UI/log path, never stdout, under the R-19 rule: after an opt-out or an explicit off nothing is read and nothing warns; the switch warns whenever the gate reaches it; the section warns only when the run asked for export (amendment 2026-11-19); destination inputs warn only on an armed run. The command's exit code and stdout are unchanged in every row.

### D-12 Commands that send nothing (F2)

One exhaustive `match` over `command::leaf::Leaf` with no wildcard (precedent `should_check_toolchain_drift`, `app.rs:351-381`), so a new leaf does not compile until classed. `Command::leaf() == None` (an `ocx-<name>` plugin) is never-send.

| Never send | Why |
|---|---|
| `Exec`, `Run` (deprecated `ocx run`), `PackageExec`, `LauncherExec`, `LauncherShim` | the exec family: Unix replaces the process, Windows exits inside the library; launches belong to execution records |
| `Shell` (bare `ocx shell`) | opens an interactive session; its "end" is the session's end; launches belong to execution records |
| `ShellState`, `DirenvExport` | per-prompt (owner) |
| `SelfActivate`, `ShellCompletion`, `Version` | Context-free arms on the shell-startup and static paths; no config is loaded |
| `Env`, `PackageEnv` **with `--shell`** (R-23) | the eval-safe channel a login shell runs at every start (`self activate` writes `ocx --global env --shell=…` into the activation stream); a dropped collector must not delay every shell in a fleet. Without `--shell` both send |
| `Status`, `ConfigTest` (R-18) | their documented contracts promise no network (`ocx status`) and no network and no state writes (`ocx config test`) |

Also never: clap help, `--version`, every usage error (all return before parsing completes), bare `ocx`, and `External` plugins. **Every other leaf sends** — 58 of today's 71, `direnv init`, `about`, `package test` and `patch test` included. The class is a function of the parsed command (leaf plus the `--shell` flag of the two env leaves). The unit test `every_leaf_has_a_telemetry_class` pins every row of `Leaf::ALL` and both `--shell` rows.

### D-13 Export point and time bound (F1)

- `main.rs` and `app::finish` are untouched (their structural tests stand). The export is awaited once, at the tail of `App::run_from`, after `command.execute`, after `update_check::apply_pending`, and after the JSON error document is printed — on success and on failure. A structural test pins exactly one finish call site in `app.rs`.
- The record's `process.exit.code` and `error.type` come from the same functions `finish` uses: an `Err` goes through `exit::classify_decision` (`exit.rs:174`), so code and slug equal the process's; an `Ok(std::process::ExitCode)` exposes no integer on stable, so the wiring recovers it by equality over `0..=255` (`ExitCode` is `PartialEq`), unit-tested for every value.
- `Context::try_init` fills a telemetry slot right after the extra-CA roots resolve (`context.rs:250-288`). A failure before that point (bad `OCX_*` value, unreadable config, CA error) sends nothing — telemetry cannot know whether it is armed without the config. A failure after it (managed `required` gate, project exclusivity, any command error) sends a record carrying that failure.
- The whole finish — installation id, record build, export — runs in a spawned task awaited under one `tokio::time::timeout(destination timeout)`; the reqwest client carries the same connect and total timeouts. Added wall time is at most the timeout plus scheduling ε: arm time reads no proxy settings, and the proxy read, every name lookup and the client build run on detached threads (the build loads the platform CA roots on Linux only; on macOS and Windows the platform verifier checks the chain during the handshake, inside the request deadline) under that one deadline. A panic or `JoinError` becomes a debug line. No process-global panic hook is installed.
- Duration is measured from `run_from` entry to the moment the outcome is known.

### D-14 Not a background check (F4)

Telemetry never calls `background_check::skip_reason`: CI is the primary use case and a non-terminal stderr is normal there. It keeps that section's other contract — it never fails a command and never writes stdout. It is skipped under the in-process seam (`in_seam()`), as every network side effect is. `subsystem-cli.md` gains the text in § Amends.

### D-15 Record schema

OTel logs signal; one `ResourceLogs`, one scope (`ocx`, version = ocx version), the event name in the LogRecord **`EventName` field** (the `event.name` attribute is deprecated), severity INFO, every record stamped with one wall-clock time taken at build. Event names: **`sh.ocx.command`** (exactly one) and **`sh.ocx.package`** (zero or more). OCX-specific names use the **`sh.ocx.*`** namespace execution records already use (`website/src/content/docs/reference/execution-records.md:92`), not `ocx.*`: one namespace across both data sets, reverse-DNS of a domain ocx owns.

| Where | Attribute | Origin | Presence |
|---|---|---|---|
| resource | `service.name` (`ocx`), `service.version` | OTel Stable | always |
| resource | `os.type` | OTel; same as execution records | best-effort |
| resource | `process.arch` | execution-record name; OTel `host.arch` vocabulary | best-effort |
| resource | `host.name` | OTel / ECS; same as execution records | best-effort |
| resource | `process.pid` | OTel RC / ECS | always |
| resource | `process.parent.pid` | ECS, as execution records (OTel spells `process.parent_pid`) | best-effort; omitted on Windows, as execution records |
| resource | `sh.ocx.installation.id` | OCX | when the id file is readable or creatable |
| resource | `sh.ocx.interactive` | OCX | always (stderr is a terminal) |
| resource | `sh.ocx.ci.provider` | OCX; values as `--ci`: `github`, `gitlab`, `other` | in CI only |
| resource | `cicd.pipeline.run.id` | OTel RC | GitHub Actions, GitLab CI |
| resource | `cicd.pipeline.task.name` | OTel RC | GitHub Actions, GitLab CI |
| resource | `cicd.pipeline.task.run.id` | OTel RC | GitLab CI only (GitHub has no job id variable) |
| `sh.ocx.command` | `sh.ocx.command.name`, `sh.ocx.command.duration` (s) | OCX | always |
| `sh.ocx.command` | `process.exit.code` | OTel RC | always |
| `sh.ocx.command` | `error.type` | OTel Stable | when the command returned an error: its `error.detail` slug, `_OTHER` when no slug classified |
| `sh.ocx.package` | `sh.ocx.package` (plain `pkg:oci` purl), `sh.ocx.package.action`, `sh.ocx.role` | OCX; the purl and role as execution records | always |
| `sh.ocx.package` | `sh.ocx.package.store_hit`, `.download.size`, `.resolve.duration`, `.download.duration`, `.extract.duration` | OCX | per D-17 |

Two names deliberately diverge from the brief and from OTel: **`process.arch`, not `host.arch`** — the value is the running binary's architecture, the exact fact execution records call `process.arch` (`adr_exec_resolution_record.md` § "process.arch is the running process"); a `host.arch` key would claim the machine's architecture, which ocx does not probe. **`process.parent.pid`, not `process.parent_pid`** — the goal requires execution-record names so the two data sets join. `error.type` carries the slug; there is no second `sh.ocx.error.detail` key. The full table with types and sources is the design record's § 4, which the user docs mirror and a test binds.

### D-16 Installation id

`$OCX_HOME/state/telemetry/installation-id`: lowercase hyphenated UUID v4 (`uuid` crate, `v4` feature, already in `Cargo.lock` at 1.28.0). Read with a 128-byte cap, regular files only. Created only on an armed run **with a destination**, only when the file is absent (`NotFound`), through `ocx_util::fs::write_private_file_atomic` (0600) on a blocking thread. An unreadable, oversized, non-regular or malformed file is left untouched and the attribute omitted; a read-only home omits it. Divergence from Grimoire (which writes the id on any armed run): no destination, no state — the "no endpoint, no export" rule extended to local writes.

### D-17 Package performance data (F8, OQ-9)

- **Collector, not result types.** `ocx_package_manager` gains an `ActivityLog` (shared, cheap to clone) attached with `PackageManager::with_activity_log` only when telemetry is armed with a destination; a manager without one records nothing. Clones share it, so `pull_or_prepare` (`tasks/pull_or_prepare.rs:25` discards infos), dependency setup and `apply_pending` all record. The CLI drains it after `apply_pending`. Reason for diverging from the source's "existing result types": `InstallInfo` is ecosystem-tier and cloned into the singleflight (`pull.rs:281`), and per-dependency records need per-call collection (discovery § 2).
- **Append points.** Every `setup_impl` that holds the package singleflight as **leader** (`pull.rs:250`) appends one entry when it completes; a waiter (`pull.rs:237`) appends nothing. `uninstall_all` appends one `removed` entry per removed candidate. Entries carry plain types: pinned identifier, platform, action, role, store hit, layer bytes, three optional durations.
- **Action.** `pulled` for every setup; `install_all` with `candidate = true` upgrades its roots to `installed` once the candidate symlink exists; `removed` for an uninstall. `ocx clean` records nothing (GC removes objects without identity). Deferred tools (`prepare_lazy`) record nothing until materialized.
- **Role.** `root` when any request in the run named the package as a root, else `dependency` (execution-record vocabulary). One record per `registry/repository@digest` per run. Amendment 2026-11-19: a patch companion's pull records `companion`, the execution-record value; a digest holding several roles reports the strongest, `root` over `dependency` over `companion`.
- **Store hit.** True when `setup_owned_impl` found the package already installed (`pull.rs:338-353`, or after the temp-dir lock, `:366-384`); then download and extract fields are absent.
- **Bytes.** `download.size` = the sum of the descriptor sizes of the layers this package's setup fetched as layer-singleflight leader and that were not already in the layer store. A successful `pull_layer` reads exactly the descriptor size (digest-verified; a short read fails `ShortBlobRead`), so no `ocx_oci` signature changes. Manifests and config blobs are excluded. Present whenever `store_hit` is false (0 when every layer was local or fetched by another package).
- **Durations** (seconds, wall clock): `resolve` around `PackageManager::resolve` at `pull.rs:227`; `download` around this package's layer phase (fetch, decompress and unpack are one stream in `Client::pull_layer`), present when this package fetched at least one layer; `extract` around assembly — `assemble_from_layers_with_layouts` through `move_temp_to_object_store` (`pull.rs:461-513`) — never around dependency setup, which runs concurrently (`pull.rs:423-432`). Per-package durations overlap and are inclusive.
- **Uninstall identity.** `UninstallResult` gains the pinned identifier of the removed candidate: the registry, repository and tag the user named plus the digest of the package the candidate pointed at, so the purl renders.
- The CLI renders each purl with `ocx_package_manager::record::purl::package_url` (`record/purl.rs:30`), the function execution records use; an entry with no logical identity produces no record.

### D-18 The collector client (F3)

- Built only when a destination exists, in `ocx_telemetry`: `reqwest::Client::builder()` with ocx's TLS roots (`ocx_util::tls::seed_embedded_roots` plus the resolved extra roots — `OCX_EXTRA_CA_CERTS`, `extra_ca_certs[_pem]`), connect and total timeout = export timeout, `redirect(Policy::none())`, `user-agent: ocx/<version>`, and a `GuardedResolver` (`crates/ocx_oci/src/ssrf.rs:214-221`) seeded with `proxy_rules()` and **no trusted host** (amended 2026-11-19: a resolver miss on the endpoint name fails closed). reqwest's own proxy read is switched off with `no_proxy()`; the one `proxy_rules()` read decides the route, and a proxied route hands reqwest that proxy and its credentials.
- Reason: the destination is operator configuration, the trust class of `[registries.*].trusted_hosts`; collectors usually live on loopback or a private address, which the guard refuses unless trusted. **What the guard buys here is ratchet compliance and a backstop, not an SSRF property** (R-6): the endpoint's own addresses are pinned or literal and never reach it, hyper skips the resolver for IP literals, a proxied dial resolves only the proxy, and redirects are off, so no second host is ever dialled; the destination's safety rests on it being operator-trusted (risk T3, accepted). One real floor is added: an endpoint whose host is an IP literal in `169.254.0.0/16` or `fe80::/10` (link-local), or is exactly `fd00:ec2::254` (AWS IPv6) or `100.100.100.200` (Alibaba), is unusable (D-11). The unique-local and CGNAT ranges around those two stay allowed. A redirect is a failed export.
- Amendment 2026-11-19: the floor also applies to a hostname endpoint. Each address the name resolves to is judged by the same link-local predicate before it is pinned; refused addresses are dropped, and an empty remainder is a `blocked` failure (a class of its own, so the debug line differs from a dead collector's `connect`). Loopback and private ranges stay allowed. The predicate unwraps these embedded-IPv4 forms: mapped, translated `::ffff:0:0:0/96`, compatible, NAT64 `64:ff9b::/96`, and local-use NAT64 `64:ff9b:1::/48` under its `/48`, `/56`, `/64` and `/96` layouts. The proxy host's pinned answer is not judged: `ssrf.rs` exempts the configured proxy from the range floor by design. Behind an HTTP proxy the proxy resolves the endpoint name, so only a literal address is refused, never a resolved one.
- Name resolution (R-26): the endpoint host is resolved on a detached thread bounded by the remaining export deadline and handed to reqwest with `resolve_to_addrs`, so a stuck `getaddrinfo` never holds the tokio blocking pool at runtime shutdown; an IP literal needs no lookup; a proxy host is resolved the same way (`pin_proxy`) and pinned on the resolver, so no lookup runs on the blocking pool.
- Plaintext (R-7): when headers go to an `http://` endpoint whose host is not loopback, one stderr warning names the header source, never a value. A loopback spelling is judged by the client once the route and the answer are known, so arming reads no proxy settings.
- Proxies: `HTTP(S)_PROXY`/`NO_PROXY` are read once, through the guard's proxy rules. The client calls `.no_proxy()` and, on a proxied route, adds the proxy and its credentials from that one read, so reqwest never reads the OS proxy settings itself. Certificates come only from ocx's roots; `OTEL_EXPORTER_OTLP_CERTIFICATE` and the client-key variables are unsupported and documented as such. An `http://` endpoint is honoured as written; the docs warn that headers then travel in clear text.

### D-19 Export mechanics

`LogExporter::builder().with_http()` with the full logs URL, `Protocol::HttpBinary`, the timeout, `RetryPolicy::disabled()` and ocx's `HttpClient`; `Resource::builder_empty()`; a logger provider whose only processor collects records in memory; one `export(LogBatch)`; the response body dropped unread. Any failure (connect, DNS, TLS, non-2xx, 3xx, timeout, encode, panic) is one debug line, never a warning, never an exit-code change. The body is never compressed; `OTEL_EXPORTER_OTLP_[LOGS_]COMPRESSION` is ignored and documented. The builder's own env reads (endpoint, timeout, protocol, headers, compression) must not change the request: code settings are final for the first three, the client discards headers, and compression is pinned by acceptance test (the builder's behaviour with an ambient `gzip` value and the feature off was not verified; § Risks).

### D-20 `OCX_CONFIG` becomes a path list (F6, OQ-5)

- **Declaration.** `OCX_CONFIG: PathList, Public, propagation = UserInput` (was `Path`). Doc text in § Amends.
- **Semantics.** Split with `std::env::split_paths` (`:` Unix, `;` Windows) on the UTF-8 value, as read today (`loader.rs:93`; a non-UTF-8 value stays ignored with a debug line, unchanged). An empty value is unset. Empty entries are skipped; no trimming. Entries load left to right into the explicit overlay, a later file winning on conflicting scalars; `--config` loads after the whole list. Every entry must exist (`Error::FileNotFound`, exit 79, naming that entry) and be a regular file (existing check). Relative entries resolve against the working directory, as a single path does today — a single path behaves exactly as before. Under `OCX_NO_CONFIG` every entry still loads. Each entry joins `config_tier_paths` (absolutized, `loader.rs:151-163`).
- **Cap.** At most **32** entries (owner decision 2026-11-19, was 4); more is a new `ocx_config::error::Error::ConfigListTooLong { count, limit }`, exit 78, slug `config_list_too_long`, naming `OCX_CONFIG` and the limit. Reason: the per-prompt hook records every config tier path in `__OCX_ENV_STATE` (`activate.rs:270`) and the decoder refuses more than `MAX_RECORDED_TIERS` (`decode.rs:173`, `MAX_RECORDED_TIERS`); 3 discovered candidates + 32 entries + `--config` = 36, and `MAX_RECORDED_TIERS` is derived from the cap as that sum. Without the cap a long list would make every prompt's state unreadable.
- **Received policy.** `Policy.config_path` keeps its name and wire type and carries the **raw** `OCX_CONFIG` value (it already holds "the string the replaced variable would hold", `adr_exec_env_model.md` C2); it is a payload field, not the frozen header, so no major bump. `validate_policy` (`decode.rs:289-301`) checks each split entry for absoluteness instead of the whole value. The CLI passes a received list to the loader in the `OCX_CONFIG` slot (new `ConfigInputs` field), keeping today's rule that it joins only when neither `--config` nor an ambient `OCX_CONFIG` is set (`context.rs:197-216`). A vanished received entry is skipped and reported per entry under `ocx shell state`. A same-major older reader handed a list fails closed (exit 79 on the joined string), never silently.
- **Report break.** `ShellStateReport`/`VerboseShellState` `policy.config_path` (`crates/ocx_cli/src/api/data/shell_state.rs:1195-1198`, schema `AbsolutePath`) becomes `policy.config_paths: [AbsolutePath]` (IC-03 plural). A gated reports break: a ledger entry and a version bump on both roots.
- **The one break** for users is a path containing the separator; `--config` stays one path (`golden/cli.json` keeps `"env": "OCX_CONFIG"` on it; no parity test compares their kinds, `crates/ocx_schema/src/cli.rs:1433-1446`).

### D-21 Nested `ocx` (OQ-10)

Each invocation exports its own record when armed in its own environment and config. Nothing new is written into any child: `OCX_TELEMETRY` (UserInput) is inherited untouched like every other ambient variable, `OCX_TELEMETRY_HEADERS` (Credential, R-13) is removed at every spawn, and no `Policy` field is added. Records join on `process.pid` / `process.parent.pid` (Unix: `ocx exec -- ocx pull` keeps one pid, so the exec record's `process.pid` equals the inner command's; a tool's child `ocx` names that tool's pid as parent) and on the CI run id. Under `ocx exec --clean` the child's environment is empty, so ambient switches and opt-outs do not cross; a config file that arms still applies (Risks).

### D-22 Test targets (F9)

- **Capture server** ported from `grimoire:test/src/otlp_fixture.py` to `test/src/otlp_fixture.py` (modes `ok`, `status=<code>`, `hang`, `delay=<ms>`, `redirect`; decodes `ExportLogsServiceRequest` with `opentelemetry-proto==1.45.2`, added to `test/pyproject.toml` `[dependency-groups] dev`), with self-tests. Backs every behaviour test.
- **Real collector**: core `otelcol` v0.165.0 (all six platforms, Apache-2.0, `file` exporter), pinned in `ocx.toml` `[group.test.tools]` with `ocx.lock` leaves for every platform a CI leg installs. Resolved like `ocx-previous` (`test/src/previous_release.py:37-80`): `OCX_TEST_OTELCOL`, else `ocx --project <root>/ocx.toml env -g test`, else **fail** naming the tool, the pin and the variable. Only a job that installs no `test` group may declare `OCX_TEST_NO_OTELCOL=1`, whose skip reason names the variable. Under Bazel it is reached the `ocx-previous` way (`module_data` with the project toolchain files), not through `@tools`.
- The package is not on `ocx.sh` today (research § Recommendations). Publishing it is the owner's step; until then that criterion reads not met.

### D-23 Documentation

Generated: `reference/environment.md` (all new variables, `OCX_CONFIG`'s new text) and `reference/configuration.md` (`[telemetry]` from the schema). Hand-written: a reference page `reference/telemetry.md` holding the field tables the binding test reads, and a how-to `guides/telemetry.mdx` telling one story — what is sent, turning it on and off, the managed-config recipe, the CI run id, tool launches through execution records and the collectors that ship those files (OTel Collector `filelog`, Vector, Fluent Bit, Fluentd, Filebeat, Alloy, Splunk UF; `adr_exec_resolution_record.md:614`), and the token warning. Cross-links from `reference/execution-records.md` and `guides/manage-config-centrally.mdx`.

### D-24 Audit path (OQ-12)

At `--log-level debug`, target `ocx_telemetry`: one line with the arming decision and its reason (`off (DO_NOT_TRACK)`, `armed by OCX_TELEMETRY`, …), one with the destination (URL without query, source family, header names), and one `telemetry payload <json>` line built from the same value the wire form is built from, so the two cannot disagree. No header value ever appears. No new command.

### D-25 Interface-contract treatment (OQ-11)

| Change | Gate | Obligation |
|---|---|---|
| New Public `OCX_TELEMETRY`, `OCX_TELEMETRY_HEADERS`; new Foreign `DO_NOT_TRACK`, `OTEL_*`, CI ids | additive (`crates/ocx_sdkgen/src/compat.rs:28`) | regenerate golden `cli.json`, `reference/environment.md` |
| `OCX_CONFIG` `path` → `path_list` | **G14** (`compat.rs:56,994`) | `[[break]]` in `crates/ocx_schema/contract/ledger.toml`, `document = "cli"`, `rule = "G14"`, pointer `/env/<index>` as the gate reports it; version bump where the gate demands |
| `ShellStateReport`/`VerboseShellState` `config_path` → `config_paths` | reports break | `[[break]]` entries naming both roots; both versions bumped |
| `[telemetry]` section | golden-only | regenerate golden `config.json` |
| Slugs `telemetry_section_in_project`, `config_list_too_long`, `config_path_contains_separator` | additive enum values | errors golden, `error.detail` docs |

---

## Design forks resolved

| Fork | Decision |
|---|---|
| F1 export point | D-13 |
| F2 silent commands | D-12 |
| F3 client vs SSRF ratchet | D-18 |
| F4 background-check gate | D-14 |
| F5 secret typing | D-9 |
| F6 `OCX_CONFIG` list, received policy | D-20 |
| F7 `ocx.toml` | D-8 |
| F8 per-package records | D-17 |
| F9 collector under Bazel | D-22 |
| F10 lock, `retain_system_locked_sections`, managed fold | D-6, D-7 |

---

## Open questions answered

Numbered in the source's order (`.agents/discussions/otel-telemetry.md` § Open questions).

- **OQ-1** Is `[telemetry]` lockable, and does a locked section survive `OCX_NO_CONFIG`? → evidence: `[records]` is the operator-owned sink precedent (`loader.rs:866-868,1130`); the council split three ways; the owner made opt-outs and an off switch beat config. → **Decision: lockable per block and surviving `OCX_NO_CONFIG`; opt-outs and an explicit-off switch beat the lock; an on switch does not beat a locked `enabled = false` (D-6, D-10).**
- **OQ-2** Which OCX variables beyond the switch and the header variable override config; is there an endpoint variable? → evidence: the `OCX_CONFIG` list and the standard `OTEL_EXPORTER_OTLP_*` cover an environment with no file. → **Decision: none; no OCX endpoint variable (as recommended).**
- **OQ-3** Does an on switch beat `enabled = false` in a file? → evidence: env beats files throughout OCX, below a system lock. → **Decision: yes, below a system lock (D-10 ranks 5–7).**
- **OQ-4** Which standard `OTEL_*` variables does OCX honour? → evidence: the 0.33.1 exporter reads endpoint, headers, timeout, compression and protocol itself but ocx sets them in code and clears headers; no certificate variables; `OTEL_SDK_DISABLED` ignored by the SDK; `OTEL_LOGS_EXPORTER` is a Stable selector with `none`. → **Decision: Grimoire's subset — `OTEL_EXPORTER_OTLP_[LOGS_]ENDPOINT`, `_HEADERS`, `_TIMEOUT`, `_PROTOCOL`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_SDK_DISABLED` (checked by ocx) — plus `OTEL_LOGS_EXPORTER=none` as an opt-out (addition: the owner asked for adherence to the standard variables and `none` is the standard off for this signal; it only ever stops export). Documented as unsupported: certificate and client-key variables (TLS from `OCX_EXTRA_CA_CERTS`/`extra_ca_certs`), compression, `_INSECURE`, `OTEL_CONFIG_FILE`, other `OTEL_LOGS_EXPORTER` values.**
- **OQ-5** Exact `OCX_CONFIG` list semantics? → evidence: `OCX_CONSENT_PATHS` and `GRIM_CONFIG` precedents; the hook's tier cap (`decode.rs:173`). → **Decision: as recommended — `split_paths`, every entry must exist, later wins, `--config` above the list, empty value unset — plus empty entries skipped, relative entries kept for single-path compatibility, and a 4-entry cap (D-20; raised to 32 on 2026-11-19).**
- **OQ-6** Does the record carry `host.name`? → evidence: execution records populate it best-effort through `sysinfo::System::host_name` (`record/environment.rs:14-18`). → **Decision: yes, best-effort, the same probe (the CLI calls `record::environment::host()`), omitted when undeterminable.**
- **OQ-7** Which CI systems, under which names? → evidence: `cicd.pipeline.run.id`, `.task.name`, `.task.run.id` are Release Candidate; GitHub has no numeric job id variable; GitLab has `CI_PIPELINE_ID`, `CI_JOB_ID`, `CI_JOB_NAME`. → **Decision: GitHub Actions (`GITHUB_RUN_ID`, `GITHUB_JOB`) and GitLab CI (`CI_PIPELINE_ID`, `CI_JOB_ID`, `CI_JOB_NAME`) under the RC names; `sh.ocx.ci.provider` = `github`/`gitlab`/`other` (`CI` truthy alone). RC status recorded; a rename upstream is a changelog break.**
- **OQ-8** Which collector distribution? → evidence: core `otelcol` v0.165.0 is the smallest single binary on all six platforms with a `file` exporter; `otelcol-otlp` has no file exporter; `otelcol-k8s` has no darwin build. → **Decision: core `otelcol` 0.165.0, read back through the `file` exporter (`format: json`) by polling, never `debug` output. Suggested package `ocx.sh/open-telemetry/otelcol:0.165.0`; publishing is the owner's step.**
- **OQ-9** How do phase times and byte counts reach the record? → evidence: dependencies need per-call collection; `InstallInfo` is ecosystem-tier and cloned into the singleflight; `pull_or_prepare` discards infos. → **Decision: diverges from "existing result types" — a shared `ActivityLog` in `ocx_package_manager`, drained by the CLI; bytes from descriptor sizes; still no telemetry dependency in the package manager (D-17).**
- **OQ-10** What does a nested `ocx` do? → evidence: `adr_exec_env_model.md:76`, `adr_exec_resolution_record.md:210` forbid new propagation. → **Decision: as recommended — each invocation exports its own record; nothing new reaches a child (D-21).**
- **OQ-11** Are a new public variable and a new section compat-gated? → evidence: discovery § 3 (`compat.rs:28,966-997`). → **Decision: additions are not ledger-gated (goldens only); the `OCX_CONFIG` kind change is G14 with a ledger entry; the `shell state` report field is a second, newly found break (D-25).**
- **OQ-12** Does an operator need an introspection command? → evidence: `ocx config test` validates a payload but reports no provenance; Grimoire's audit path is the debug line. → **Decision: no new command and no change to `ocx config test`; the debug lines are the audit path (D-24).**
- **OQ-13** Should the docs warn about a token in a broadly readable managed config? → **Decision: yes, one sentence in the telemetry guide and the same sentence in the managed-config guide: anyone who can pull the artifact can read the token.**

---

## Amends and respects

| Earlier decision | Status | New text or reason |
|---|---|---|
| OBS-12, `.claude/rules/rust-quality/docs-and-tracing.md:133` | **exempted** | Vendored file not edited. Conditions in § OBS-12 exemption; pointer comment on each `Cargo.toml` line |
| `adr_exec_env_model.md` D2 (:76) — no public `OCX_*` written into a child but `OCX_BINARY_PIN` | respected | Nothing written; `OCX_TELEMETRY` is UserInput, `OCX_TELEMETRY_HEADERS` is Credential (scrubbed at spawn, R-13); no `Policy` field added |
| `adr_exec_env_model.md` C2 `Policy.config_path` (:353) | **amended** | Row becomes `pub config_path: Option<PathBuf>, // relaxing OCX_CONFIG: the raw value, a path list the reader splits; every entry absolute`. Payload field, not the frozen header grammar (§ C3), so no major bump; the decoder validates per entry |
| `adr_exec_resolution_record.md` (:210) no new environment propagation; (:600) `traceparent` rejected; (:214, :542-555) field names | respected | No propagation, no `TRACEPARENT`; `process.pid`, `process.parent.pid`, `host.name`, `os.type`, `process.arch`, `sh.ocx.package`, `sh.ocx.role`, `pkg:oci` purls reused verbatim |
| `.claude/rules/subsystem-cli.md:382` § Background Checks | **amended** | Append after "**One gate.**": "**Usage telemetry is not a background check.** The OTLP export of `adr_otel_telemetry.md` runs once, after the command, only when an operator armed it, and it exists to report CI runs, so it never calls `skip_reason`: neither `CI` nor a non-terminal stderr stops it. It keeps the rest of this section's contract: it never fails a command, never writes to stdout, and adds at most its timeout to the run." |
| `background_check::skip_reason` gate | respected by exclusion | D-14 |
| Hermetic `OCX_NO_CONFIG` (`vars.rs:64-73`; `adr_managed_config_tier.md` A) | **amended** | `OCX_NO_CONFIG` doc becomes: "Boolean — skip the discovered config-tier chain; every file `--config` and `OCX_CONFIG` name still loads and a system-tier lock survives. Forwarded to child OCX. / Also drops the `[managed]` snapshot and the `OCX_MANAGED_CONFIG` read. A system-tier policy in `/etc/ocx/config.toml` still loads, filtered to its locked sections — `[records]` and `[telemetry]` among them — and an unreadable system file still aborts (exit 78). / Env-only: there is no `--no-config` flag. Hermetic pattern: `OCX_NO_CONFIG=1 ocx --config /ci/ocx.toml ...`." |
| `OCX_CONFIG` declaration (`vars.rs:56-63`) | **amended** | "Paths to explicit configuration files, separated like `PATH` (`:` on Unix, `;` on Windows). Mirrors `--config`, which takes one path. / Each file layers on the discovered chain and the managed tier in list order, a later file winning on conflicting scalars; `--config` loads after the whole list. At most 32 files. / A missing file is an error naming it; an empty value is unset and empty entries are skipped. A path containing the separator cannot be listed. With `OCX_NO_CONFIG` the listed files still load." |
| `adr_managed_config_tier.md` — operator registry is the trust root; payload overrides everything outside `[managed]` not system-locked | respected | No telemetry carve-out (owner); a system lock clamps `[telemetry]` like every section |
| `adr_ocx_interface_contract.md` — additive vs gated; ledger | respected (applied) | D-25 |
| SSRF ratchet (`crates/ocx_oci/CLAUDE.md`, shrink-only baseline) | respected | Guarded construction, no baseline line (D-18) |
| `research_deprecation_telemetry_patterns.md` — phone-home rejected | respected (consistent) | No default endpoint; no endpoint, no export, no state; private-first: each operator owns the collector |
| `plan_configuration_system.md:159-161` / `research_configuration_patterns.md:73-91` — `include` and drop-ins deferred; "CI uses `OCX_CONFIG` to a single explicit file" | **amended** (the single-file assumption only) | `OCX_CONFIG` is a list (owner); `include` and drop-ins stay deferred |
| `adr_shell_env_overhaul.md:516` — "no telemetry" on the hook path | respected | `self activate`, `shell state` and `direnv export` never construct telemetry |
| `adr_crate_split_workspace.md` tiers; `ocx-mirror` lockstep | respected | Internal crate; mirror uses `Config::default()` and `exec_state::Inherited` only, both unchanged in type |
| `arch-principles.md` State glossary and ADR index | **amended** | State examples gain "`state/telemetry/installation-id` for the usage-telemetry installation id (`adr_otel_telemetry.md`; not TTL-bound, never GC-walked)"; ADR index gains this ADR's row |
| `AGENTS.md` architecture table ("24 workspace members") | **amended** | 25 members; row "`ocx_telemetry` [internal] — usage telemetry: the arming gate, destination selection and one OTLP/HTTP export per command"; `product-context.md` § Technical Overview crate count +1 |

---

## Constitution check (`arch-principles.md`, `product-context.md`)

Facade, command pattern, three-layer errors, exhaustive internal matches, private-by-default, no `deny_unknown_fields` in `Config`, `ocx_env` as the one read seam, test seams under `__testing` — all followed. Deviations:

| Violation | Why needed | Simpler alternative rejected because |
|---|---|---|
| OBS-12: an `opentelemetry*` dependency in a short-lived CLI | Owner's choice of the maintained encoder (Grimoire parity); protobuf reach | Hand-written JSON (Option B) owns a wire encoder, Block-tier under `quality-core.md`, and loses protobuf-only backends |
| Product principle 2 (offline-first: cached work = zero network): an armed run makes one request even when everything is cached | The operator explicitly asked for usage records | A file sink only (Option E) does not deliver OTLP; mitigated: off by default, offline disarms, the request is bounded and never changes the outcome |
| Duplication: a third CI-detection site | `ocx_telemetry` may not depend on `ocx_shell` or `ocx_package_manager` | Extracting a shared helper for a one-line read adds a crate edge for no behaviour; the field names are pinned by a parity test instead (C-027). `host.name` is not a second probe: the CLI reads it through `ocx_package_manager::record::environment::host()`, the execution-record probe, and hands it to `ocx_telemetry` |

---

## Divergences from Grimoire

Configuration rides the OCX tier chain (not a telemetry-only `GRIM_CONFIG`); `host.name`, pid, parent pid and CI ids are recorded; `process.arch` instead of `host.arch`; `sh.ocx.*` namespace; `OTEL_LOGS_EXPORTER=none` honoured; the installation id is written only when a destination exists; header variable `OCX_TELEMETRY_HEADERS`; timeout capped at 10 s; no panic-hook wrapper; relative `OCX_CONFIG` entries allowed (single-path compatibility); unknown `[telemetry]` keys ignored silently (OCX convention) rather than warned.

---

## Quantified Impact

| Metric | Before | After | Notes |
|---|---|---|---|
| New crates in `Cargo.lock` | 762 entries (merge base `8a265e26`) | 776 entries: +14 (`ocx_telemetry`, five `opentelemetry*`, `rand` 0.9 family, `getrandom` 0.3, `httpdate`, wasm shims) | Grimoire: +24; no `tonic` (condition 1); counted as `name =` lines in `Cargo.lock` |
| `ocx` release binary, linux x86_64 | 70,553,008 B (54,337,200 B stripped), merge base `8a265e26` | 72,631,712 B (55,879,008 B stripped) at `5856b88c`: +2,078,704 B (+2.9 %), stripped +1,541,808 B (+2.8 %) | condition 10; `cargo build --release -p ocx --locked`, `strip` for the second figure; Grimoire: +655 KB release (+2.1 %), +416 KB dist (+1.6 %) |
| Unarmed run | 0 | 4 env reads + one look at the already-loaded `[telemetry]` | no OTel type, no client, no file |
| Armed, collector up / refused | — | Grimoire probe: ~20 ms / ~18 ms | to be re-measured |
| Armed, collector hangs | — | ≤ timeout (default 500 ms) + ε | Grimoire probe 520 ms |

---

## Consequences

**Positive**
- Operators get command usage, failures (exit code and slug), and per-package performance in their own backend, with one variable or one config section, delivered fleet-wide through the managed tier.
- Command records and execution records join on purl, pid and CI run id with no new propagation.
- The crate's measured hazards become pinned, tested invariants.

**Negative**
- ~+24 crates and ~0.5 MB for a feature most users never arm.
- Each `opentelemetry*` minor bump is a deliberate edit with likely glue breakage.
- `OCX_CONFIG` breaks for a path containing the separator, gains a 4-entry cap, and the `shell state` report renames one field.

**Risks and mitigations**
- **Header-strip regression leaks a token** → condition 5 acceptance tests.
- **Exit stall** → conditions 4–6; a DNS lookup stuck in `getaddrinfo` is mitigated by R-26 (endpoint resolved on a detached thread under the deadline); residual closed by the amendment of 2026-11-19 (review-fix pass): the proxy host lookup runs inside the export deadline.
- **Builder env reads**: resolved by § Amendment 2026-11-19 — 0.33.1 skips a compression value whose feature is off, so the request stays uncompressed; the acceptance test pins it.
- **Older `ocx` binaries and a list-valued `OCX_CONFIG`** (R-16): an older binary in the same environment reads the list as one path and exits 79 on every command → the guide's recipe says to set a list only once every `ocx` there is at or above this release.
- **Repository-supplied environment** (`.envrc`, devcontainer, CI `variables:`) can set `OCX_TELEMETRY` and an `OTEL_*` endpoint, or name an `OCX_CONFIG` file, and so collect usage and receive configured headers. Same class Grimoire's owner accepted (T3): such an environment already owns the process; `ocx.toml` itself can never do it (D-8).
- **Token in a managed payload** is readable by anyone who can pull the artifact, and lands in `state/managed-config/` → owner-accepted trust model; documented (OQ-13).
- **`--clean` drops ambient opt-outs** for the child (D-21) → documented; set the opt-out in config.
- **Older same-major `ocx` receiving a list** in the exec state fails closed with exit 79 → acceptable, never silent.
- **Collector package absent from `ocx.sh`** → the real-collector criterion reads not met; owner action.
- **`otelcol` internal metrics port 8888** may collide between concurrent targets → verify `service.telemetry.metrics.level: none` with two concurrent collectors, else a per-test metrics port (research § A).

---

## Migration and rollout

1. **`OCX_CONFIG` list** — its own commit, subject `feat(config)!: OCX_CONFIG accepts a list of config files` (the `!` is the changelog's break notice: a path containing the separator, the 32-entry cap, `shell state`'s `config_paths`). Same commit: G14 and reports `[[break]]` entries in `ledger.toml`, version bumps, regenerated golden `cli.json` and report schemas, `reference/environment.md`. No migration prose in user docs (`AGENTS.md` § Stability tiers).
2. **Telemetry** — additive commits (`feat(telemetry): …`); goldens `cli.json` and `config.json`, generated reference pages, the two new doc pages.
3. **Lockstep satellite** — `ocx_config` and `ocx_env` are ecosystem crates `ocx-mirror` links; `task satellite:verify` must pass in the same series. No mirror source change is expected (`Config::default()`, `exec_state::Inherited::from_process_env`, `Policy` field types unchanged).
4. **AI config and docs** — `subsystem-cli.md`, `arch-principles.md`, `AGENTS.md`, `product-context.md` per § Amends, as `chore:` commits.
5. **Owner step** — publish the collector package to `ocx.sh`, then `ocx add -g test` and `ocx lock` pin it.

---

## Out of scope

Traces and spans; the metrics signal; reading or writing `TRACEPARENT`; any default or vendor-chosen destination; a crate shared with Grimoire; drop-in directories and `include`; a first-run prompt; a live check against `otel.ocx.sh`; the existing CI telemetry (Bazel BEP and JUnit export); per-tool-call export from launches (execution records cover those); an introspection command.

---

## Validation

- [ ] Every OBS-12 condition pinned by its named test (C-028, C-011, C-019, C-022, C-018)
- [ ] Binary size before/after recorded in § Quantified Impact (condition 10)
- [ ] `task rust:license:deps`, `task rust:license:notice`, `cargo deny check` green on the new graph; `cargo tree -i tonic` empty
- [ ] `ssrf_guard_ratchet` green with the baseline file unchanged
- [ ] Ledger entries for G14 and the `shell state` report break; compat gate green
- [ ] All 15 goal criteria mapped in the design record § 11, each with a passing contract or scenario
- [ ] `task verify:scoped --force` and `task satellite:verify` green
- [ ] Security review of the header, URL, TLS and token paths in the hex-execute review panel

## Amendment 2026-11-19 — plan-artifact review round 1

Binding on the plan. Findings from the review panel (spec, architect, security, researcher) and the cross-model pass, triaged by the hex-plan orchestrator under goal-loop authority; the IDs are the plan's `R-*` rows (`plan_otel_telemetry.md` § Review fixes), where each carries its evidence. Inline edits above: D-3 (R-13), D-5 (R-1), D-12 (R-18, R-23), D-18 (R-6, R-7, R-26), D-21 (R-13).

| ID | Change |
|---|---|
| R-1 | A layer that sets `endpoint` drops headers inherited from earlier layers (D-5): the owner's "same source, never mixed" rule read per layer, not only per family |
| R-2 | `ocx_config::Error::Parse` and the managed `InvalidToml` render path, line, column and message, never the offending source line (toml's `Display` quotes it), so a token on a broken line never reaches stderr or the JSON error document (D-9) |
| R-3 | Parsed header values stay `SecretString` until the request is built; each is inserted with `HeaderValue::set_sensitive(true)`; the `HttpClient` and `Destination` have hand-written `Debug` that prints names only (D-9, D-18) |
| R-4 | The `HttpClient` never reads the response body: it returns the status with an empty body, so a hostile collector cannot grow memory; `partial_success` is therefore not inspected (D-19) |
| R-5 | An export failure's debug line names its class only, never the `Display` of `reqwest::Error` or `ExporterBuildError` (both embed the URL and its query) (C-020) |
| R-6 | The `GuardedResolver` is ratchet compliance and a backstop; one floor: IP-literal endpoints in `169.254.0.0/16` and `fe80::/10` are unusable (D-18) |
| R-7 | Headers to an `http://` non-loopback endpoint: one stderr warning naming the source; not refused (operators run plaintext collectors inside their own network) |
| R-8 | Refused header names add the hop-by-hop set `connection`, `keep-alive`, `proxy-connection`, `te`, `trailer`, `upgrade`; values must pass `http::HeaderValue::from_bytes` at parse time, so an unusable header fails closed with D-11's warning (D-4, C-002) |
| R-9 | Installation id: read refuses a symlink (`symlink_metadata`); create publishes with `ocx_util::fs::persist_temp_file_noclobber` (create only if absent) (D-16) |
| R-10 | A `--config` path containing the path-list separator is refused when ocx composes a child's received policy: exit 78, slug `config_path_contains_separator` (D-20). Without it the child would re-split the path and load files the parent never named |
| R-11 | Relative `OCX_CONFIG` entries stay allowed (single-path compatibility is a goal criterion; the environment that sets them already owns the process, T3); the docs recommend absolute paths |
| R-12 | D-8's claim narrowed: `ocx.toml` never arms, disarms or redirects export. A package's declared environment reaches only the processes it launches, which already run that package's code, so reserving `OTEL_*` or `DO_NOT_TRACK` in package env would protect nothing |
| R-13 | `OCX_TELEMETRY_HEADERS` is `Credential`, not `SecretInput` (D-3, D-21) |
| R-14 | A `removed` package record carries the platform when the store records it for that package, else its purl omits `arch`; the S-015 uninstall assertion compares purls without the `arch` qualifier (D-17) |
| R-15 | `ActivityLog` keeps each `_all` entry point's root set apart from the per-setup entries; the drain takes `role` from the union of root sets (a root that lost the singleflight race to its own dependency path is still `root`) and keeps the performance fields of the entry that fetched (non-store-hit) when several operations in one run touched the same digest (D-17) |
| R-16 | Risk added: an older `ocx` reads a multi-entry `OCX_CONFIG` as one path and exits 79 on every command; the guide's recipe says to set a list only once every `ocx` in that environment is at or above this release (D-20) |
| R-17 | Dependencies settled before the lock freezes: `ocx_config` += `secrecy`, `http`, `url`; `ocx_telemetry` += `url`, `percent-encoding`, `bytes`, `http` (all already workspace dependencies) (D-1) |
| R-18 | `Status` and `ConfigTest` are never-send (D-12) |
| R-19 | Warning rule (replaces D-11's "only on an armed run"): after an opt-out (design § 3.1 rows 2–5) or an explicit off (row 6) nothing is read and nothing warns; an invalid switch (row 7) warns whenever the gate reaches it; an unusable section (row 8) warns only when the run asked for export (amendment 2026-11-19); destination inputs warn only on an armed run |
| R-20 | The 4-entry cap also applies to a received list in `validate_policy`; `--config`'s help text and `PolicyReport`'s doc are amended to match the list (D-20) |
| R-21 | The telemetry slot is an out-parameter owned by `run_from`, so a `try_init` failure after the slot is filled still exports; the duration stops before `apply_pending`; on timeout the export task's handle is aborted, not dropped (D-13) |
| R-22 | The pin guard also asserts every `opentelemetry*` package in `Cargo.lock` is `0.33.1` (the transitive `opentelemetry-proto` included); the constitution table gains the OBS-20 row (condition 1) |
| R-23 | `env` and `package env` with `--shell` are never-send (D-12) |
| R-24 | The real-collector module stays off the branch until M-1; its pin, module and suite counts land together, gated on their own run |
| R-25 | No package-record cap: the log holds one entry per distinct package the run already materialized; about 0.5 KB per record; the exporter's 64 MiB body cap is the backstop and exceeding it is an export failure (debug line). Pinned by a 10 000-record unit test |
| R-26 | Endpoint name resolution on a detached thread under the export deadline (D-18) |
| R-27 | Event names are `&'static str` constants (`LogRecord::set_event_name` takes `&'static str`); the proto `event_name` field is Development status; the golden test and the acceptance decode assert `event_name` on the wire |
| R-28 | `DO_NOT_TRACK` and `OTEL_SDK_DISABLED` truthiness is ocx's boolean superset, a deliberate deviation from the spec's `true`-only rule, erring toward off; an `OTEL_LOGS_EXPORTER` value other than `none` is ignored with a debug line |
| R-29 | Never retrying deviates from the OTLP spec's SHOULD for 429/502/503/504; `Retry-After` is ignored because the deadline is shorter than any useful wait (D-19) |
| R-30 | The telemetry reference lists the names that differ from OTel (`process.parent.pid`, `process.arch`) and states that `service.instance.id` is deliberately absent (the installation id is per installation, not per instance) |

**Considered and not taken.** Option F — encode `opentelemetry-proto` messages with prost and POST them without the exporter — owns no encoder and would remove conditions 4–6, but departs from the owner's maintained-exporter position and from Grimoire parity; the hazards it would remove are pinned by those conditions. A package-record cap (cross-model reviewer) — R-25. Parsing `partial_success` (researcher) — R-4. Emitting `process.parent_pid` beside `process.parent.pid` — one fact, one key; the goal names the execution-record key.

**Constitution row added** (§ Constitution check): *OBS-20 vocabulary* — `process.parent.pid` and `process.arch` instead of OTel's `process.parent_pid` and `host.arch`; needed so command records join execution records on the same keys; using the OTel names was rejected because the goal names the execution-record keys and `host.arch` would claim a machine fact ocx does not probe.

**Risk resolved.** The ambient-compression risk (§ Risks; design § 12): in `opentelemetry-otlp` 0.33.1 an `OTEL_EXPORTER_OTLP_[LOGS_]COMPRESSION` value whose feature is compiled out is skipped by `resolve_compression_from_env` (`src/exporter/mod.rs:258-285`: the convert error goes to `warn_ignored_enum_env_var` and the loop continues), so the request stays uncompressed and export proceeds; the acceptance test pins it.

---

## Links

- [`system_design_otel_telemetry.md`](./system_design_otel_telemetry.md) — contracts, scenarios, field tables
- [`discover_otel_telemetry.md`](./discover_otel_telemetry.md) — code map and forks
- [`adr_exec_resolution_record.md`](./adr_exec_resolution_record.md), [`adr_exec_env_model.md`](./adr_exec_env_model.md), [`adr_managed_config_tier.md`](./adr_managed_config_tier.md), [`adr_ocx_interface_contract.md`](./adr_ocx_interface_contract.md)
- Grimoire (read-only): `.agents/adr/adr_otel_usage_telemetry.md`, `.agents/adr/adr_grim_config_layers.md`

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-11-18 | architect (hex-plan xhigh) | Initial record: OBS-12 exemption and its ten conditions, D-1…D-25, forks F1–F10, OQ-1…OQ-13, amends/respects |
| 2026-11-19 | hex-plan orchestrator | Amendment: plan-artifact review round 1, R-1…R-30; inline edits to D-3, D-5, D-12, D-18, D-21 |
| 2026-11-19 | hex-execute review-fix pass | Amendment: (a) an unusable `[telemetry]` section warns only when the run asked for export (`OCX_TELEMETRY` truthy, merged `enabled = true`, or the invalid key is `enabled`), else off silently; narrows R-19 for the section row, the invalid switch still always warns. (b) The resource probe and the proxy host lookup run inside the export deadline; closes the proxy-lookup residual named in D-18 and risk "Exit stall" |
| 2026-11-19 | owner decision | Amendment: the `OCX_CONFIG` list cap goes from 4 to 32 (D-20); `MAX_RECORDED_TIERS` becomes 3 + 32 + 1 = 36 and is derived from the cap. The 4/8 arithmetic above is superseded. No state version bump: a same-major older reader refuses a state with more than 8 tiers and falls back, reachable only by a list that was an error before |
