# Research: Telemetry codebase recon — Grimoire implementation and OCX config layering

## Metadata

**Date:** 2026-11-18
**Domain:** observability
**Triggered by:** hex-discuss otel-telemetry
**Expires:** 2027-05-18

Read-only recon. Grimoire read at worktree `/home/mherwig/.herdr/worktrees/grimoire/duo` (HEAD `c16330cd`); every path named in the brief existed there, so the main checkout was not used. OCX read at `/home/mherwig/.herdr/worktrees/ocx/duo` (HEAD `8a265e26`). Notation: `grimoire:path:line` and `ocx:path:line`, paths relative to each repo root. No recommendation is made. Nothing was built or run; every behaviour below is read from code, docs or test source, not observed.

---

## Part A — Grimoire

### 1. What is emitted

- Signal: logs only, as OTel events (log records with an event name). No traces, no metrics. `grimoire:.agents/adr/adr_otel_usage_telemetry.md:84-90` (decision), `grimoire:Cargo.toml:131-134` (only the `logs` feature of `opentelemetry`/`opentelemetry_sdk`; `http-proto`+`logs` on the exporter), `grimoire:src/telemetry/export.rs:6-10`.
- One OTLP/HTTP POST to `/v1/logs` per CLI invocation holding all events. `grimoire:src/telemetry/export.rs:55-71` (one `LogExporter::export` of one `LogBatch`), `grimoire:docs/src/content/docs/telemetry.md:19-24`.
- Event names: `grim.command` (one per invocation) and `grim.artifact.changed` (one per changed artifact). `grimoire:src/telemetry/payload.rs:31-32`.
- `grim.command` attributes: `grim.command.name`, `grim.command.subcommand` (when present), `grim.command.duration` (double, seconds), `process.exit.code`, `error.type` (only on non-success), `grim.ci`, `grim.scope` (`global`/`project`, when a command resolved one), `grim.clients`, `grim.clients.detected` (install-like commands). `grimoire:src/telemetry/payload.rs:34-42,188-214`.
- `grim.artifact.changed` attributes: `grim.artifact.ref` (`registry/repository[:tag]`, absent for a path source), `grim.artifact.kind`, `grim.artifact.action` (`installed|updated|removed|added`). `grimoire:src/telemetry/payload.rs:44-46,174-185`, `grimoire:src/telemetry/record.rs:21-36,56-90`. Only artifacts a run actually wrote produce an event; unchanged/skipped/refused/failed rows do not. `grimoire:src/telemetry/record.rs:92-95`.
- Resource attributes: `service.name` (`grim`), `service.version`, `os.type`, `host.arch`, `grim.installation.id`, and with the repository switch `vcs.repository.url.full`, `vcs.owner.name`, `vcs.repository.name`, `vcs.provider.name`; plus operator overrides. `grimoire:src/telemetry/payload.rs:21-29,119-149`. Built from `Resource::builder_empty()` (no detectors, no `telemetry.sdk.*`, no host name). `grimoire:src/telemetry/export.rs:74-83`.
- Each record: severity INFO, timestamp = observed timestamp = one `SystemTime::now()` taken at build time. `grimoire:src/telemetry/export.rs:93-98`, `grimoire:src/telemetry/payload.rs:218`.
- Life-cycle points: `Telemetry::from_env` inside `Context::new` (gate); `start(&command)` right after the context is built; per-command `record_*` calls (scope, clients, artifacts); `finish(&outcome)` as the last statement of `app::run`, after the command's output was rendered. `grimoire:src/context.rs:196`, `grimoire:src/app.rs:45-46`, `grimoire:src/app.rs:212`, `grimoire:src/telemetry.rs:166-240`.
- Per-command coverage: bare `grim`, `--help`, `--version`, usage errors send nothing (return before `Context::new` or before parsing a command). `mcp` and `tui` are `LONG_LIVED`: only the closing `grim.command`, no scope/clients/artifact events. `grimoire:src/app.rs:27-46`, `grimoire:src/telemetry.rs:49-51,252-260`, `grimoire:docs/src/content/docs/telemetry.md:33-39`.
- `grim.command.name` is an exhaustive match over the clap `Command` tree (a new command is a compile error until named); aliases report the canonical name. `grimoire:src/telemetry/command_name.rs:11-18`.
- A debug line `telemetry payload <json>` carries the same `Payload` value that is converted to the wire form (so audit line and wire cannot disagree). `grimoire:src/telemetry.rs:360`, `grimoire:src/telemetry/payload.rs:4-9,234-255`.

### 2. Default state and consent

- Default: off. No telemetry code path allocates an OTel type unless armed; an unarmed run costs env reads only (ADR table: 4 env reads, 5 after the amendment, plus one capped read per `GRIM_CONFIG` entry when no env opt-out applies). `grimoire:.agents/adr/adr_otel_usage_telemetry.md:165,215-217`, `grimoire:src/telemetry.rs:8-10`.
- Turns it on: `GRIM_TELEMETRY` truthy (`1|true|yes|on`, any case), or a `GRIM_CONFIG` file with `[telemetry] enabled = true`. `grimoire:src/env.rs:20-21,82-109,200-203`, `grimoire:src/telemetry.rs:138`. Setting `OTEL_*` alone never arms anything. `grimoire:docs/src/content/docs/telemetry.md:51-53`, test `grimoire:test/tests/test_telemetry_export.py:65-84`.
- No endpoint, no request: an armed run with neither `OTEL_EXPORTER_OTLP_[LOGS_]ENDPOINT` nor a file `endpoint` sends nothing (debug line only). Arming still writes the installation-id file. `grimoire:src/telemetry/config.rs:103-109`, `grimoire:src/telemetry.rs:362-368`, `grimoire:docs/src/content/docs/telemetry.md:9-11,86-88`.
- Turns it off, in this order (first match wins; no config file is opened for 1 and 2): (1) `DO_NOT_TRACK` truthy, `OTEL_SDK_DISABLED` truthy, `GRIM_OFFLINE`/`--offline`; (2) `GRIM_TELEMETRY` set to a non-blank, non-truthy value (`0`, `false`, anything else, or non-Unicode); (3) any unusable `GRIM_CONFIG` layer disarms for the run, even with `GRIM_TELEMETRY=1`; (4) armed when `GRIM_TELEMETRY` truthy or merged `enabled = true`; `GRIM_TELEMETRY=1` beats a file's `enabled = false`. `grimoire:src/telemetry.rs:123-148`, `grimoire:src/env.rs:93-109`, `grimoire:.agents/adr/adr_otel_usage_telemetry.md:228-240`.
- What the user is told: nothing at run time when armed and healthy (export is silent at the default log level). Failures are visible only at `GRIM_LOG=debug`. Warnings on stderr at default level exist only for config-file problems (unusable layer, ignored keys, grim-owned headers with no file endpoint). `grimoire:docs/src/content/docs/telemetry.md:29-31`, `grimoire:src/telemetry.rs:117,135,137,141-143,187-189`. The consent surface is documentation (`telemetry.md`, `configuration.md:1437-1440`, `stability.md`) and the debug audit line; no first-run prompt or banner was found.
- Docs state there is no default endpoint and no maintainer-run collector. `grimoire:docs/src/content/docs/telemetry.md:13-15`.

### 3. Identity and redaction

- Installation id: random UUID v4 at `$GRIM_HOME/telemetry/installation-id`, created on the first armed run via atomic write, validated as lowercase hyphenated UUID, 128-byte cap, regular-file only, never created for a relative `GRIM_HOME`; `None` (id omitted) on any failure. `grimoire:src/telemetry/identity.rs:9-14,24-44,46-79`. Test: absent when unarmed, stable across armed runs. `grimoire:test/tests/test_telemetry_export.py:287-309`.
- No user name, host name, cwd, argv, branch, commit or author is collected; `os.type`/`host.arch` come from `std::env::consts`, not resource detectors. `grimoire:.agents/discussions/otel-usage-telemetry.md:93-95,243-246`, `grimoire:src/telemetry/payload.rs:127-128`, test `grimoire:test/tests/test_telemetry_contract.py:208` (`test_never_collected`).
- Repository identity (opt-in `GRIM_TELEMETRY_REPOSITORY`, project scope only): derived by spawning `git -C <dir> rev-parse --show-toplevel` and `git config --local --get remote.origin.url`; skipped when the toplevel is the home directory; each `git` call has a 1 s timeout, `kill_on_drop`, and nine `GIT_*` variables removed from its environment. `grimoire:src/telemetry/vcs.rs:13-18,26-37,46-61,66-84`, gate at `grimoire:src/telemetry.rs:350-355`.
- `redact::repository_url`: refuses scp-style local paths; strips userinfo (ends at last `@`), query, fragment, trailing `/`; wraps `normalize_remote_url` (shared with release annotations, so left unedited), lowercases, and returns `None` if an `@` survives. `grimoire:src/telemetry/redact.rs:10-52`. `redact::url` renders scheme/host/port/path only for debug lines and returns `<unparsable url>` instead of echoing bad input. `grimoire:src/telemetry/redact.rs:57-69`.
- `vcs.owner.name` is every path segment but the last; `vcs.provider.name` is host-substring match on github/gitlab/bitbucket/gitea (+ codeberg.org). `grimoire:src/telemetry/vcs.rs:95-115`.
- `scope.rs` does no redaction: it records the scope only when the call repeats the invocation's own `--global` and carries no workspace seed (first such call wins), so an internal side resolve cannot flip `grim.scope` or the `vcs.*` gate. `grimoire:src/telemetry/scope.rs:10-27`.
- Artifact refs are built from `Identifier` accessors (`registry_repository()` plus a declared tag), so a digest or other raw string never reaches the wire. `grimoire:src/telemetry/record.rs:55-70`.
- Hashing: none. The identifiers sent are plain text (installation UUID, redacted repo URL, artifact refs). No hash function was found in `src/telemetry/`.
- Endpoint safety: URL with userinfo is refused; header values are `secrecy::SecretString`, `Debug` prints a count; `TelemetryLayer`'s `Debug` redacts the endpoint query. `grimoire:src/telemetry/config.rs:20-52,136-150`, `grimoire:src/telemetry/layer.rs:30-47`.

### 4. Configuration surface (complete table)

Standard OTel and cross-tool variables (names from `grimoire:src/telemetry/config.rs:71-82`, `grimoire:src/env.rs:20-31`):

| Variable | Behaviour in Grimoire | Ref |
|---|---|---|
| `OTEL_SDK_DISABLED` | Truthy disarms, wins over everything. Enforced by grim because the crate ignores it | `src/env.rs:116-118`; ADR cond. 2 |
| `DO_NOT_TRACK` | Truthy disarms, wins over everything | `src/env.rs:121-123` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Base URL; `/v1/logs` appended, path kept. http/https, host required, userinfo refused. Used only when no file sets `endpoint` | `src/telemetry/config.rs:102-109,135-150` |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | Full URL used verbatim; wins over the base endpoint | `config.rs:103-104` |
| `OTEL_EXPORTER_OTLP_HEADERS` | `k=v,k=v`, percent-decoded. Deviation from spec: sent only to the base endpoint, never to a `_LOGS_ENDPOINT` | `config.rs:116-126` |
| `OTEL_EXPORTER_OTLP_LOGS_HEADERS` | Same grammar; replaces the generic ones; sent to either endpoint | `config.rs:119-125` |
| `OTEL_EXPORTER_OTLP_TIMEOUT`, `..._LOGS_TIMEOUT` | Integer ms > 0, logs variable wins; anything else gives default 500 ms (spec default is 10 s) | `config.rs:84,152-159` |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `..._LOGS_PROTOCOL` | Only `http/protobuf` (case-insensitive) or unset; any other value skips the export with no request | `config.rs:111-114` |
| `OTEL_SERVICE_NAME` | Replaces `service.name` | `config.rs:170-172` |
| `OTEL_RESOURCE_ATTRIBUTES` | `k=v,k=v`, percent-decoded (the SDK's own parser does not decode), added to resource, overrides derived keys incl. `vcs.*` | `config.rs:165-169`, ADR cond. 6 |
| `OTEL_EXPORTER_OTLP_COMPRESSION` (+`_LOGS_`) | Ignored: body always uncompressed (crate gzip feature off) | `telemetry.md:123-124` |
| `OTEL_EXPORTER_OTLP_CERTIFICATE`, `..._CLIENT_*` (+`_LOGS_`) | Ignored; TLS roots come from grim's client | `telemetry.md:125-126` |

Grimoire-specific variables:

| Variable | Behaviour | Ref |
|---|---|---|
| `GRIM_TELEMETRY` | Tri-state: unset/blank defers to files; truthy arms; any other value is explicit off (also disarms a file) | `src/env.rs:82-109` |
| `GRIM_TELEMETRY_REPOSITORY` | Truthy adds `vcs.*`; env-only; applies when armed by either source | `src/env.rs:111-114`, `telemetry.rs:192` |
| `GRIM_CONFIG` | Path list of TOML files, see Q5 | `src/env.rs:68-71` |
| `GRIM_TELEMETRY_OTLP_HEADERS` | `k=v,k=v` percent-decoded headers; merged over file headers per name; sent only with a file `endpoint` | `config.rs:70-71,193-210` |
| `GRIM_OFFLINE`, `--offline` | Disarm like every other network call | `telemetry.md:69` |
| `GRIM_LOG` | `GRIM_LOG=debug` (or `grim::telemetry=debug`) shows the payload line and skip/failure reasons; target `grim::telemetry` | `src/telemetry.rs:53-55`, `main.rs:426-436` |
| Proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`) | Honoured by the reqwest client | `telemetry.md:99-101` |

Config-file keys (`[telemetry]` of a `GRIM_CONFIG` file; unknown keys inside it are skipped and named in one warning, wrong type or value on a known key makes the layer unusable): `enabled` (bool), `endpoint` (string, same validation as base env endpoint, `/v1/logs` appended), `headers` (table of strings, no percent-decoding, reserved names `host|content-length|content-type|transfer-encoding` refused), `timeout` (integer ms > 0), `protocol` (only `http/protobuf`), `service_name` (string; empty ignored), `resource_attributes` (table of strings). `grimoire:src/telemetry/layer.rs:93-160`, `grimoire:docs/src/content/docs/telemetry.md:171-185`. Both `GRIM_TELEMETRY_REPOSITORY` and `GRIM_TELEMETRY_OTLP_HEADERS` have no file key.

Precedence:
- Arming, highest first: env opt-outs, explicit non-truthy `GRIM_TELEMETRY`, broken layer, then `GRIM_TELEMETRY` truthy or merged `enabled`. `grimoire:src/telemetry.rs:123-148`.
- Destination is one unit chosen by whether any layer sets `endpoint`. With a file endpoint, grim's own selection reads no `OTEL_EXPORTER_OTLP_*` variable (endpoint, headers, protocol, timeout); headers = merged file headers, then `GRIM_TELEMETRY_OTLP_HEADERS`; timeout = file value or 500 ms. With no file endpoint, the `OTEL_*` rules apply and neither grim-owned header source is sent (one warning when such headers are set). `grimoire:src/telemetry/config.rs:187-210`, `grimoire:src/telemetry.rs:141-143`.
- Resource, lowest first: derived keys, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_SERVICE_NAME`, file `resource_attributes`, file `service_name`. Applies to both destinations. `grimoire:src/telemetry/config.rs:161-185`, `grimoire:src/telemetry/payload.rs:117-149`.
- ADR rung table (OTEL generic < GRIM_CONFIG files < reserved user-file rung (not in v1) < grim-namespaced env and cross-tool opt-outs < flags). `grimoire:.agents/adr/adr_grim_config_layers.md:116-124`.
- Deliberately ignored or reinterpreted: `OTEL_SDK_DISABLED` re-implemented by grim; generic headers scoped to the base endpoint only; 500 ms default timeout; protocol other than http/protobuf skips rather than falling back; compression and certificate variables ignored; no default `localhost:4318` endpoint. Not verified: whether the pinned 0.33.1 exporter builder reads any further `OTEL_*` variable itself; the ADR records only that it reads the headers (and `GrimHttpClient` discards what it sets). `grimoire:.agents/adr/adr_otel_usage_telemetry.md:119-126`, `grimoire:src/telemetry/export.rs:136-138,181-194`.

### 5. Config layering (`GRIM_CONFIG`)

- Value is a path list split by `std::env::split_paths` (`:` on Unix, `;` on Windows); blank value is unset; empty entries skipped. `grimoire:src/config/env_layers.rs:61-75`, `grimoire:src/env.rs:68-71`. Yes, one variable names several files.
- Per entry: must be absolute (relative refused, no `~`/`$VAR` expansion), must be a regular file (checked via `metadata` before open, so a FIFO cannot hang), size-capped by `config::read_capped` (docs: 8 MiB, same cap as `grimoire.toml`), must parse as TOML. Read order = list order; first failing entry aborts, later entries unread. `grimoire:src/config/env_layers.rs:77-110`, `grimoire:docs/src/content/docs/telemetry.md:158-163`.
- Merge is done by each consumer, not the loader. Rule: a later file's scalar replaces the earlier value; `headers` merge per name ignoring case; `resource_attributes` merge per exact key; later wins in both. Nothing appends as a list. `grimoire:src/config/env_layers.rs:1-6`, `grimoire:src/telemetry/layer.rs:70-91,93-160`, `grimoire:src/telemetry/config.rs:33-40`, `grimoire:docs/src/content/docs/telemetry.md:183-185`.
- Only the `[telemetry]` table is read in v1; any other top-level key is ignored with one warning naming file and keys. Artifact declarations, consent records and registry credentials never come from these files. `grimoire:src/telemetry/layer.rs:76-90`, `grimoire:.agents/adr/adr_grim_config_layers.md:90-96`.
- Read lazily, only past the env opt-outs and an explicit off, inside `Context::new`; `grim config` neither reads nor writes these files; grim reads no system or user config file for telemetry. `grimoire:src/telemetry.rs:119-124,169-186`, `grimoire:docs/src/content/docs/telemetry.md:168-169`.
- A project `grimoire.toml` is not a rung for any key a `GRIM_CONFIG` file may carry, and "never will be" for a telemetry destination (a committed file would redirect on clone). The auto-detected user-file rung 3 is reserved, not built. `grimoire:.agents/adr/adr_grim_config_layers.md:126-128,116-124`.
- Failure mode (D-1): fail closed for telemetry, never for the command: one stderr warning naming path (and line/column for parse errors, never file content), telemetry off for the run. A missing listed file warns on every command by design (PD-15). `grimoire:.agents/adr/adr_grim_config_layers.md:131-150`, `grimoire:.agents/plans/plan_grim_config_telemetry.md:73`.
- Delivery example in docs: an ocx patch package ships the file and sets `GRIM_CONFIG` as a public constant with `${installPath}`. `grimoire:docs/src/content/docs/telemetry.md:269-285`.

### 6. Export mechanics

- Crates (exact pins `=0.33.1`, default features off): `opentelemetry` (`logs`), `opentelemetry_sdk` (`logs`), `opentelemetry-otlp` (`http-proto`, `logs`), `opentelemetry-http` (trait only); plus `uuid` v4. `grimoire:Cargo.toml:126-136`. No `tonic`/`h2` dependency from this; `reqwest` 0.13.5 shared with the app. ADR measured +24 crates, +415,984 B on `--profile dist` (+1.6 %), +655,056 B on release (+2.1 %). `grimoire:.agents/adr/adr_otel_usage_telemetry.md:213-216`.
- Protocol: OTLP/HTTP with protobuf body (`Protocol::HttpBinary`); no JSON, no gRPC. Content-type forced to `application/x-protobuf`. `grimoire:src/telemetry/export.rs:55-60,192-194`.
- Async, one request. Records are minted through an `SdkLoggerProvider` whose only processor collects them in memory (no worker, no flush); the exporter's `export` is called once under `tokio::time::timeout`. The crate's own processors and transport are never used. `grimoire:src/telemetry/export.rs:85-134`, `grimoire:src/telemetry.rs:369`.
- Own transport: `GrimHttpClient` wraps a `reqwest::Client` built through `crate::tls::merge_embedded_roots`, connect and total timeout = export timeout, redirects off, `RetryPolicy::disabled()`. It clears every header the crate set (including the crate's merge of `OTEL_EXPORTER_OTLP_[LOGS_]HEADERS`), then sets `user-agent: grim/<version>`, grim's selected headers (marked sensitive) and the content type. Response body is dropped unread. `grimoire:src/telemetry/export.rs:136-219`.
- Flush before exit: there is no provider flush. `finish` is awaited at the end of `app::run`, before `main` returns; `main` then calls `runtime.shutdown_timeout(1 s)`, which abandons a blocking thread still running. `grimoire:src/app.rs:212`, `grimoire:src/main.rs:205-206,239-245`.
- Isolation: everything after the armed check runs in its own `tokio::spawn`ed task with a thread-local "quiet" panic hook wrapper, so a panic ends as a debug line, never in the exit path. `grimoire:src/telemetry.rs:290-345`.
- Collector unreachable: connect/DNS failure, non-2xx, or timeout become an `ExportError` that is logged at debug only; exit code and stdout are unchanged. Documented wall-time cost: up to the export timeout (default 500 ms), plus up to 2 x 1 s for the bounded `git` calls with the repository switch; ADR probe numbers: refused ~18 ms, hung 520 ms. `grimoire:src/telemetry.rs:347-380`, `grimoire:.agents/adr/adr_otel_usage_telemetry.md:217-218`, `grimoire:docs/src/content/docs/telemetry.md:128-139`.
- Residual: docs say a hostname whose resolver hangs can exceed the timeout because the runtime waits on the blocking resolver thread (`telemetry.md:130-133`), yet `main.rs:239-245` bounds runtime shutdown at 1 s; the two statements are not reconciled in the sources read. Ctrl-C or kill loses the export. `grimoire:docs/src/content/docs/telemetry.md:138-139`.

### 7. How it is tested

- Fixture: `grimoire:test/src/otlp_fixture.py` is an in-process HTTP server that records every request (method, path, headers, raw body) and decodes `POST /v1/logs` into `ExportLogsServiceRequest` (via `opentelemetry-proto`, `grimoire:test/pyproject.toml:28`). Modes `ok`, `status=<code>`, `hang`, `delay=<ms>`, `redirect`. Debug-line helpers parse `GRIM_LOG=debug` stderr (`telemetry_payload`). Served by the `otlp_collector` fixture in `grimoire:test/tests/conftest.py:87-96`. The fixture has its own tests: `grimoire:test/tests/test_otlp_fixture.py:67-198`.
- Unarmed sends nothing: `test_unarmed_sends_nothing_with_ambient_otel` sets endpoint, headers, protocol, resource attributes and service name, asserts zero requests, no `telemetry` dir, no payload debug line; then adds `GRIM_TELEMETRY=1` as positive control and expects exactly one POST. `grimoire:test/tests/test_telemetry_export.py:65-84`. Also `test_no_endpoint_sends_nothing` (`:174`), `test_disarm_switches_win` (`:97`, four disarm routes), `test_opted_out_run_never_opens_grim_config` (`grimoire:test/tests/test_telemetry_config_file.py:253`), `test_installation_id_absent_when_unarmed` (`export.py:287`).
- Header and transport invariants: `test_signal_endpoint_never_gets_generic_headers` (`:147`), `test_generic_headers_cannot_replace_the_content_type` (`:137`), `test_redirect_is_not_followed` (`:189`), `test_no_retry_on_503` (`:225`), `test_unsupported_protocol_sends_nothing` (`:201`), `test_export_failure_never_changes_outcome` (`:360`, refused/503/hang), `test_debug_line_never_carries_header_values` (`:322`), `test_one_post_per_invocation` (`:397`), `test_debug_line_matches_wire` (`:311`).
- Contract tests: `test_wire_keys_match_documented_field_table` (`grimoire:test/tests/test_telemetry_contract.py:276`), `test_never_collected` (`:208`), `test_multi_record_single_post` (`:178`), `test_mcp_session_sends_one_record` (`:140`), `test_stability_page_lists_telemetry_as_unstable` (`:322`).
- Events: `grimoire:test/tests/test_telemetry_events.py` (status/failure/global scope, clients, install/add/update/remove, bundle, dev install, path source). Repository: `grimoire:test/tests/test_telemetry_repository.py` (credentialed origin, no git spawn without opt-in, global scope, home repo, no git on PATH, git env and config injection, slow git killed).
- `GRIM_CONFIG`: 15 tests in `grimoire:test/tests/test_telemetry_config_file.py` (arms alone, file endpoint gets only grim headers, later file wins, broken file disarms, relative refused, userinfo refused, secrets never printed, FIFO never blocks, non-Unicode `GRIM_TELEMETRY` is explicit off). Docs binding: `grimoire:test/tests/test_telemetry_config_docs.py`.
- Rust unit tests: gate truth table over 8 opt-out rows x switch (`grimoire:src/telemetry.rs:420-427`), endpoint/header scoping/grammar tables (`grimoire:src/telemetry/config.rs:337-420`), layer merge errors never echo values (`grimoire:src/telemetry/layer.rs:195-210`), `GRIM_CONFIG` reader tests (`grimoire:src/config/env_layers.rs:120-200`).
- Interop: one run against `otel/opentelemetry-collector-contrib` (0.163.2) with the docs recipe, recorded in the plan, not a standing test. Windows: `grim-wintest` run, 52 passed, 3 skipped. `grimoire:.agents/plans/plan_otel_usage_telemetry.md` § Verification record items 5-6.

### 8. Rejected decisions, follow-ups, known gaps

Rejected or out of scope (ADR and discussion):
- Hand-written OTLP/HTTP+JSON over existing `reqwest`+`serde_json` (0 crates, ~+47 KB; http/json only, Dynatrace is protobuf-only). Rejected by the owner in favour of the crate, against the recommendation to hand-write. `grimoire:.agents/adr/adr_otel_usage_telemetry.md:55-62,84-85`, `grimoire:.agents/discussions/otel-usage-telemetry.md:195-201`.
- The crate's built-in processors and `reqwest-client` transport: `SimpleLogProcessor` deadlocks on a current-thread runtime and sends one POST per record; `BatchLogProcessor` panics with an async client; default retries and 10 s timeout stall exit 5-15 s. ADR Option 3.
- `tracing-opentelemetry` / `opentelemetry-appender-tracing` bridge (forwards 353 `tracing` sites, shape follows log lines). ADR Option 4.
- Traces, metrics (events only; metrics are collector-derived via a documented recipe; connectors alpha), gRPC (+1.95 MB), default or maintainer-hosted collector, any `grimoire.toml` form of the switches, crash reporting, per-tool-call export from hooks and mcp, user/host identity. `grimoire:.agents/discussions/otel-usage-telemetry.md:30-37`.
- Resource detectors (`opentelemetry-resource-detectors` collects unredacted argv, spawns `hostname`, reads machine-id). `grimoire:.agents/discussions/otel-usage-telemetry.md:243-246`.
- Config layering: Option 2 (grim-prefixed telemetry variables only) survives only as `GRIM_TELEMETRY_OTLP_HEADERS`; Option 3 (fixed-path managed layer such as `/etc/grimoire/...`) rejected (needs root/MDM, sits above the user). Per-entry header merge kept against an architect finding that asked for endpoint-layer ownership. `${VAR}` substitution, `headers_file` and helper-command keys rejected. Skipping a missing listed file rejected. `grimoire:.agents/adr/adr_grim_config_layers.md:58-76`, `grimoire:.agents/adr/adr_otel_usage_telemetry.md:269-277`, `grimoire:.agents/plans/plan_grim_config_telemetry.md:70-73,234`.
- Refs-in-payload: owner kept artifact refs against a 2-1 council split (dissent: defer refs, additive later but removal is breaking). `grimoire:.agents/discussions/otel-usage-telemetry.md:138-153,176-182`.

Open or accepted gaps:
- Threat T3 (repository-supplied environment can arm export and name an endpoint, including by naming a `GRIM_CONFIG` file, and receive ambient or grim-owned headers): owner-accepted, no gate beyond the switch; an egress ruling in `arch-threat-model.md` is "owed and deferred to the owner". `grimoire:.agents/adr/adr_otel_usage_telemetry.md:148-163`, `grimoire:.agents/plans/plan_otel_usage_telemetry.md:397`.
- ADR Validation checklist boxes are unchecked in the file (condition tests, binary size record, license/audit, security review) although the plan's verification record reports them done. `grimoire:.agents/adr/adr_otel_usage_telemetry.md:197-202` vs plan § Verification record.
- ADR condition 7 describes a `Hook(Run)` early return in `app::run`; the code has no such branch and the docs say hooks run through a separate `grim-hook` launcher with no telemetry code. I found no acceptance test in the telemetry test files asserting "hook run never exports". `grimoire:src/app.rs:27-46,191-205`, `grimoire:docs/src/content/docs/telemetry.md:37`.
- A FIFO named as `grimoire.toml` hangs `read_capped`; reported as a follow-up candidate, not fixed. `grimoire:.agents/plans/plan_grim_config_telemetry.md:236`.
- Windows proof of the `;` separator is owed to a CI deep run. `grimoire:.agents/plans/plan_grim_config_telemetry.md` PD-22.
- Ctrl-C/panic skip the flush: accepted. No build git SHA in `service.version` (OBS-15 unmet). `grimoire:.agents/discussions/otel-usage-telemetry.md:254-257`.
- Reserved rung 3 (auto-detected user config) and non-telemetry tables in `GRIM_CONFIG` (`[options.experimental]` first) are future work; admitting a table is an ADR amendment. `grimoire:.agents/adr/adr_grim_config_layers.md:90-96,116-124`.

---

## Part B — OCX

### 9. The config tier chain today

Tiers, lowest to highest (fold order). Doc table `ocx:website/src/content/docs/explanation/configuration.mdx:107-117`; enum `ocx:crates/ocx_config/src/lib.rs:43-58` (`System`, `User`, `Home`, `Managed`, `Explicit`).

1. Compiled defaults: only `[registries."ocx.sh"] index`. `ocx:crates/ocx_config/src/loader.rs:~160-175` (`builtin_defaults`), not gated by `OCX_NO_CONFIG`.
2. System: `/etc/ocx/config.toml` (Windows `%ProgramFiles%\ocx\config.toml`). `ocx:crates/ocx_config/src/loader.rs:1166-1178`.
3. User: `dirs::config_dir()/ocx/config.toml`. `loader.rs:1182-1189`.
4. OCX home: `$OCX_HOME/config.toml`, default `~/.ocx/config.toml`. `loader.rs:~1192-1195`.
5. Managed snapshot payload (identity-gated, zero network). `loader.rs:237-335`.
6. `OCX_CONFIG` file. 7. `--config` file. `loader.rs:108-114,145` (merged on top of the managed fold).
8. Env variables, 9. CLI flags (outside the file chain; five "ladder" variables invert row 8). `configuration.mdx:118-121`.
- Project tier: an `ocx.toml` contribution folds in with `[shell]`, `[records]` and `[update]` stripped (warning). `ocx:crates/ocx_config/src/loader.rs:1052-1078`.

Discovery: `tier_candidates` pushes system, user, home in that order; `existing_candidates` keeps those that exist; a missing file is silent, a symlinked or unreadable user/home candidate is skipped with a warning, a symlinked or unreadable system file is an error (`Error::SystemConfig`, exit 78). `loader.rs:435-495`. Files over 64 KiB (`MAX_CONFIG_SIZE`) are refused. `loader.rs:14,~770-785`.

Merge semantics per key, `Config::merge(other)`: "set scalars win, tables merge key-by-key". `ocx:crates/ocx_config/src/lib.rs:288-348`:
- `[registry]`, `[patches]`, `[managed]`, `[update]`, `[shell]`, `[records]`: per-section field-wise merge, `Some` of the higher tier wins (`merge` on each type, e.g. `records.rs:57-72`).
- `[registries]`, `[mirrors]`: map entry-wise, then field-wise per entry. `lib.rs:297-306`.
- `[trust]`: policies array-append across tiers; `[trust.sigstore]` replaces. `lib.rs:131-143,321-326`.
- `toolchain_dir`: higher non-empty wins. `lib.rs:345-353`. `extra_ca_certs` + `_pem`: replaced as a pair, unless system-locked. `lib.rs:355-362`.
- System locks: only the system file is passed through `apply_system_locks` (before it merges); locked `[registry]`, `[registries.*]`, `[mirrors.*]` (per role), `[patches]`, `[managed]` (when `required`), `[trust.policy]` entries, `[records]`, and the extra-CA pair are marked `system_locked`, after which lower-precedence merges into that section are ignored. `loader.rs:~798-806,834-869`; `records.rs:57-72`.
- Unknown sections and keys are ignored (no `deny_unknown_fields` anywhere in the tree). `lib.rs:73-80`.

The four controls, quoted:
- `OCX_CONFIG`: `ocx:crates/ocx_env/src/vars.rs:56-63`: "Path to an explicit configuration file. Mirrors `--config`. Layers on top of the discovered chain (system, user, `$OCX_HOME/config.toml`) at the highest file-tier precedence. With `--config` both load and the flag wins on conflicting scalars. A missing file is an error naming the path; empty is unset." Declared `Path`, not `PathList`. The loader pushes it as one path: `explicit_paths.push(PathBuf::from(env_path))` (`loader.rs:109-111`); `--config` adds exactly one more (`loader.rs:112-114`; flag is `Option<PathBuf>`, `ocx:crates/ocx_cli/src/app/context_options.rs:12-20`). So at most two explicit files (env first, flag later), neither can name several; a colon-separated value would be treated as one path. Missing file is `Error::FileNotFound` (exit 79). `loader.rs:~735-745`.
- `--config`: one `FILE`; must exist (exit 79); "layers on top of the discovered system, user and `$OCX_HOME` files and of the managed tier; it does not replace them". `context_options.rs:11-19`.
- "Skip discovered config" variable, `OCX_NO_CONFIG`: `vars.rs:64-73`: "Boolean — skip the discovered config-tier chain; explicit `--config` / `OCX_CONFIG` paths still load and a system-tier lock survives. Forwarded to child OCX. Also drops the `[managed]` snapshot and the `OCX_MANAGED_CONFIG` read. A system-tier policy in `/etc/ocx/config.toml` still loads, filtered to its locked sections ... Env-only: there is no `--no-config` flag." Mechanism: `discovered = existing_candidates([system_path])` then `retain_system_locked_sections`, which keeps only `system_locked` pieces of `registry`, `registries`, `mirrors`, `patches`, `records`, `trust.policy`, `trust.sigstore`, the extra-CA pair, and drops `managed`, `shell`, `update`, `toolchain_dir`. `loader.rs:100-122,1105-1145`.
- `OCX_MANAGED_CONFIG`: see Q10.

### 10. The managed tier

- `[managed]` (`ManagedConfig`): `source` (OCI reference of the managed-config artifact), `required` (default true, fail closed with exit 78 until synced), `refresh` (`apply|notify|manual`, default `notify`), `interval` (default `1d`), plus loader-set `system_locked`. `ocx:crates/ocx_config/src/managed.rs:14-57`, constants `managed.rs:~189-195`.
- `OCX_MANAGED_CONFIG`: `ocx:crates/ocx_env/src/vars.rs:186-194`: "OCI reference overriding `[managed].source` for this invocation only; empty is unset, `OCX_NO_CONFIG` suppresses it. Forwarded to child OCX. Never written back to the seed ... Resolving the tier never fetches." Read at `loader.rs:196-218`; a child can also receive it through the parent's exec-state policy. A bare override activates the tier with no seed (`managed.rs:339-340`). If `[managed]` is system-locked, an override that does not match the locked source is ignored with a warning (`managed.rs:342-357`).
- Where the config comes from: an OCI artifact (published with `ocx config push`) whose layer is a plain `config.toml`; fetched by `ocx config update`, `ocx config setup`, `ocx self setup --managed-config`, or a background refresh (interactive, outside CI, online, past the throttle, unpaused); persisted as `$OCX_HOME/state/managed-config/snapshot.json` (metadata: source, tag, digest, fetched_at) plus sibling `config.toml`. Every ordinary command reads the snapshot with zero network. Payload cap 64 KiB. `ocx:crates/ocx_config/src/managed_config.rs:1-25`, `ocx:crates/ocx_config/src/managed_config/paths.rs:34-52`, `ocx:crates/ocx_config/src/managed.rs:~60-92`, `ocx:website/src/content/docs/guides/manage-config-centrally.mdx:29-69`.
- Fold rules (`fold_managed_tier`, `loader.rs:237-335`): snapshot applies only if its `registry/repository` matches the effective source (a digest-pinned source also needs the digest to match; cross-repository snapshots never match); `[update]` is stripped; a `[managed]` section in the payload is stripped (one-hop: a payload cannot redirect the tier that fetched it); path-form `extra_ca_certs` and `[trust.sigstore] trusted_root` are dropped; with an unpinned source, `trusted_root_json`, `fulcio_url`, `rekor_url` and `[shell.consent]` are dropped; an unparsable payload is a warning (or exit 78 when `required`). Every other section of `Config` a payload sets is merged above the home tier.
- Who may set it: `[managed]` seed in system/user/home/explicit files; `OCX_MANAGED_CONFIG` env; `ocx self setup --managed-config`. A system-tier `[managed]` with `required = true` is `system_locked`: lower tiers and env cannot redirect it, and `--managed-config` may not clear or redirect it (`SystemLockedOverride`). `managed.rs:~195-232,309-331`, `loader.rs:857-859`.
- User can and cannot override: the user, `$OCX_HOME`, `OCX_CONFIG` and `--config` files all sit at or around the managed tier; the managed payload folds above user and home, and `OCX_CONFIG`/`--config` fold on top of it ("The explicit overlay merges on top of the managed fold, never under it", `loader.rs:~118-119,145`). So a user can override a managed key with `OCX_CONFIG`, `--config`, env variables or flags, and with `OCX_NO_CONFIG=1` drop the tier entirely, except for sections the system file locked (those survive and clamp every lower tier, the managed payload included, `loader.rs:832-835`). The managed fetch client is built from the local-only view so a payload cannot reroute its own fetch. `loader.rs:38-39`.
- Not carried by the payload: `[update]` (personal), `[shell]` consent when unpinned, and sigstore locations when unpinned. Everything else, including any new section, is in scope for a payload (no allow-list was found).

### 11. Where a new section is declared and validated (worked pattern `[records]`)

1. Field on `Config`: `ocx:crates/ocx_config/src/lib.rs:155-163` (`pub records: Option<records::RecordsOptions>`), module declared `lib.rs:19`.
2. Section type with `Deserialize` + `schemars::JsonSchema`, no `deny_unknown_fields` (explicit comment: older binary must not be bricked): `ocx:crates/ocx_config/src/records.rs:9-39`. `#[serde(skip)] system_locked`, `#[schemars(skip)]`.
3. Env tier: `RecordsOptions::from_env`/`from_values` (empty is unset); `required` is deliberately config-file-only. `records.rs:42-62`; CLI wiring `ocx:crates/ocx_cli/src/app/context.rs:1316`.
4. Merge: field-wise, `other` wins unless `system_locked`. `records.rs:64-79`; wired in `Config::merge` at `lib.rs:339-344`.
5. Lock: `lock_as_system` (`records.rs:81-84`), applied in `apply_system_locks` `loader.rs:866-868`; and kept under `OCX_NO_CONFIG` in `retain_system_locked_sections` `loader.rs:1121,1130`. The destructure there is exhaustive on purpose ("a section added to `Config` cannot reach here without a decision", `loader.rs:1112-1113`), so a new section forces a decision.
6. Tier restrictions: `fold_project_tier` strips `[records]` from a project `ocx.toml` `loader.rs:1052-1078`; `fold_managed_tier` has per-section guards (`[update]`, `[trust.sigstore]`, `[shell.consent]`) `loader.rs:~290-330`.
7. Relative paths: `anchor_relative_paths` resolves `records.dir` against the declaring file's directory `loader.rs:1081-1092`.
8. Validation style: lenient. Unknown keys ignored; an invalid value usually warns and falls back (e.g. `refresh` uses `deserialize_lenient`, `managed.rs:~58-62`); hard errors are reserved for ambiguous or unsafe states (`guard_extra_ca_certs_ambiguity`, `loader.rs:~812-826`).
9. Reference docs: `website/src/content/docs/reference/configuration.md:192-206` is generated "from the published config schema" (`reference/configuration.md:6`); `schemars::JsonSchema` on the section type feeds it.
10. Other worked sections: `[mirrors]` has a hand-rolled deserializer so a malformed entry names its host (`lib.rs:104-106`); `[update]` is the example of a personal-only section stripped from managed payload and project files (`lib.rs:128-134`, `loader.rs:~257-272,1070-1076`).

### 12. Environment registry rules (`ocx_env`)

- Declaration: one entry in the `env_vars!` macro per variable in `ocx:crates/ocx_env/src/vars.rs` (module doc: "Every environment variable OCX reads, declared once"), `pub NAME [= "literal"]: <EnvValue>, <Visibility>, [secret], [on_invalid = Error], [propagation = X], [reader = Dependency("crate")]`; doc comment is the summary and feeds docs. Value kinds: `Bool, TriBool, String, Integer, Path, PathList, PathOrPem, HostList, Choice, Json`. `ocx:crates/ocx_env/src/lib.rs:25-62,454-535`. Name patterns with one `{SLOT}` placeholder are allowed (`OCX_AUTH_{REGISTRY}_TOKEN`). `lib.rs:28-29`.
- Reads: only through the registry; `std::env::{var,var_os,vars,vars_os,set_var,remove_var}` are `disallowed-methods` in the root `clippy.toml` (tests included; a crate-local `clippy.toml` would silently drop the ban). Tests set variables through `ocx_env::overrides`. `ocx:clippy.toml:6-12`, `ocx:.claude/rules/subsystem-interface-contract.md` § Environment reads. The ban does not see reads inside dependencies: those are declared with `reader = Dependency("...")` when the product depends on them.
- Prefix rule: a non-`OCX_` variable can be registered and read. Four visibilities: `Public` (`OCX_*`, user config, in the SDK manifest and compat gate), `Foreign` (owned by another tool or platform, "documented, never renamed"), `Plumbing` (`__OCX_*`), `Testing` (`__OCX_TESTING_*`, absent from release builds). `ocx:crates/ocx_env/src/lib.rs:66-77`. Existing `Foreign` examples: `CI`, `NO_COLOR`, `GITHUB_ACTIONS`, `HTTP_PROXY` (with `reader = Dependency("reqwest")`), `RUST_LOG`, `DOCKER_CONFIG`, `SSL_CERT_FILE`. `ocx:crates/ocx_env/src/vars.rs:539-706`. I did not find the test that enforces "Public must start with `OCX_`"; IC-13/IC-14 state the naming rules (`.claude/rules/subsystem-interface-contract.md:40-41`, "registry test"). `is_reserved_ocx_key` (`OCX_*`/`__OCX_*`, case-insensitive) is the gate that stops config-file and env-file surfaces from setting `OCX_*`. `lib.rs:445-452`.
- No `OTEL_*`, `DO_NOT_TRACK` or `*TELEMETRY*` declaration exists in `ocx_env` (grep over `crates/ocx_env/src`). Note: `Foreign` is listed as "documented", and `website/.../reference/environment.md` "Every variable OCX reads, sets or lists as external, public ones first".
- Secrets: `secret` makes the declaration answer `None` from `get()`; the value is read only through `SecretVar::get() -> Sensitive` (no `Display`, `Debug` prints `<redacted>`). `lib.rs:131-140,201-233`.
- Propagation to children is a per-declaration enum: `UserInput` (inherited untouched, never written by ocx), `SecretInput` (not recorded in state), `SetupWritten` (files `ocx self setup` generates may export it), `State` (`__OCX_ENV_STATE`), `BinaryPin`, `HookLocal` (removed at every projected spawn), `Credential` (secret, removed at every spawn, never recorded). `lib.rs:82-99`. Example doc lines: `OCX_MANAGED_CONFIG` and `OCX_NO_CONFIG` "Forwarded to child OCX" (declared `UserInput`), and the spawn contract says inherited `OCX_*` pass through unchanged. `ocx:.claude/artifacts/adr_ocx_interface_contract.md:350-360`.
- Invalid values: `OnInvalid::Default` falls back; `OnInvalid::Error` refuses with `InvalidEnv` (exit 78, names the key only) for hardening switches. `lib.rs:47-57,106-122`. Retired spellings live in `RETIRED` with `Status::Window`/`Removed`. `ocx:crates/ocx_env/src/retired.rs:97-111`.

### 13. Existing logging and tracing; OTel crates; HTTP and TLS stack

- Two facades coexist. `log` is used widely in library crates (139 files match `log::{info,debug,warn}`, e.g. the config loader uses `log::warn!`/`log::debug!`); `tracing` is used in a minority of crates (`ocx_sign`, `ocx_util` archive, `ocx_package_manager` pull, `ocx_announce`). Workspace deps: `log 0.4` (std), `tracing 0.1.44`, `tracing-subscriber 0.3.23` with `env-filter, std, fmt, json, registry, tracing-log`. `ocx:Cargo.toml:129-131`. Lockfile: `log 0.4.34`, `tracing-subscriber 0.3.23`. `ocx:Cargo.lock:3636,7283`.
- Subscriber installation: `ocx:crates/ocx_cli/src/tracing_init/log_settings.rs:66-103` (`LogSettings::init` / `init_with_progress`: `tracing_subscriber::registry().with(fmt_layer).try_init()`), called from `ocx:crates/ocx_cli/src/app.rs:209` and `ocx:crates/ocx_cli/src/app/context.rs:146`; the stderr writer is the console's bar-suspending `ProgressLogWriter` (`ocx:crates/ocx_cli/src/tracing_init.rs:16-25`). `EventFormat` is custom (`tracing_init/event_format.rs`). Level flag `--log-level`/`OCX_LOG_LEVEL`, console-only `OCX_LOG_CONSOLE`, fallback `RUST_LOG` (declared Foreign, reader `tracing-subscriber`). `ocx:crates/ocx_env/src/vars.rs:324-326,556-557`. In-process test seam installs its own call-scoped subscriber and does not bridge `log::` records. `ocx:crates/ocx_cli/src/app/seam.rs:164-183`.
- Spans: `info_span!`/`#[instrument]`/`tracing::span` matches exist in the tree (the grep returned hits; I did not enumerate which crates, so the extent of span usage is not characterised).
- OpenTelemetry in `Cargo.lock`: none. No package named `opentelemetry*` or `tracing-opentelemetry`; no `tonic` (0 matches). `prost 0.14.4` is present, pulled by `prost-build`/`prost-reflect`/`prost-types`/`sigstore_protobuf_specs` (the Sigstore stack), not by anything OTel. `ocx:Cargo.lock:5213-5222`, dependents found by scanning `"prost"` in dependency lists.
- HTTP/TLS stack an exporter could reuse: `reqwest 0.13.5` (`default-features = false`, features `json, rustls, charset, http2, system-proxy`), `hyper 1.12.0`, `h2 0.4.20`, `rustls`, `hyper-util` with `client-proxy`; `oci-client` is a path dependency on a fork (`external/rust-oci-client`). `ocx:Cargo.toml:24,142,154`, `ocx:Cargo.lock:2780,2995,5600,5770`. TLS roots: `ocx_util::tls::seed_embedded_roots(builder)` seeds bundled `webpki-root-certs` into every hand-rolled `reqwest::ClientBuilder` ("Without seeded roots reqwest reads the OS trust store, which panics on a host with an empty one"), and `ExtraRoots` appends operator CA roots (`OCX_EXTRA_CA_CERTS` / `extra_ca_certs`). `ocx:crates/ocx_util/src/tls/embedded_roots.rs:1-18`, `ocx:crates/ocx_util/src/tls.rs:1-40`. Clients are built in `ocx_oci`, `ocx_index`, `ocx_announce` (forge http), `ocx_package_manager` tasks and `ocx_cli` context. Process spawning is clippy-banned outside `ocx_package_manager::launch` (relevant to any `git` lookup). `ocx:clippy.toml:4-7`.
- Mirror routing: `[mirrors]` rewrites OCI read traffic; the managed fetch is routed only through local-tier mirrors. `ocx:crates/ocx_config/src/lib.rs:94-103`. No telemetry-specific egress seam exists.

### 14. Existing CI telemetry (commit `44c950ab`)

- Commit `44c950ab` ("feat(telemetry): export Bazel build events to otel.ocx.sh as OTLP traces", 2026-09-22) touches `scripts/bep_to_otlp.py` (1797 lines), `taskfiles/telemetry.taskfile.yml`, and three fixtures under `test/fixtures/bep/`. `git show --stat 44c950ab`.
- It is not product code: Python stdlib (`json` + `urllib`) script and a Taskfile, OTLP/HTTP+JSON synchronous POST appended `/v1/traces`, redirect refused, field allowlist with a planted-credential self-test, reader floor; failure is loud but never fatal. `ocx:.claude/rules/subsystem-ci.md:188-198`, commit body.
- Its sibling, the JUnit half, pushes test reports with `junit2otlp` 0.1.2 (Go, gRPC) via composite action `.github/actions/test-telemetry` and the `telemetry:push` task, service `ocx-tests`. Auth is nginx basic auth in org secret `OTEL_OTLP_AUTH` passed as `OTEL_EXPORTER_OTLP_HEADERS`; locally off unless `~/.config/ocx-telemetry/env` (mode 600) exists. `ocx:.claude/rules/subsystem-ci.md:152-186`.
- No `opentelemetry*` crate appears in any OCX `Cargo.toml`/`Cargo.lock` (Q13); the Grimoire archaeology note records the same finding for ocx at `e70bac609`. `grimoire:.agents/research/research_otel_repo_archaeology.md:89`.

### 15. Product principles and research findings that bear on telemetry (quoted)

`ocx:.claude/rules/product-context.md:57-65` (Product Principles):
- "1. **Backend-first** — JSON output, composable commands, clean exit codes, made for machine consumption"
- "2. **Offline-first** — local index never auto-update; populated index + cached object = zero network"
- "7. **Private-first** — registry auth first-class, internal tools easy as public ones"
- "5. **Zero infrastructure cost** — BYO OCI registry; no Homebrew tap, no Artifactory license"
- Differentiator 9: "First-class corporate / air-gapped mirror support ... replace semantics (no egress fallback)"; differentiator 11: "Centrally managed corporate config as ordinary OCX package". `product-context.md:79,81`.
- Non-target and target: "Non-target: End users typing interactive commands — OCX = backend tool, not user-facing PM" (`:55`).
- The file contains no explicit "never phone home" principle; the phrase appears in a PRD user story: "I want the activation mechanism to work with stock `direnv` and never phone home". `ocx:.claude/artifacts/prd_project_toolchain.md:65`. And as a cost line: "no new dependency, no new service, no telemetry". `ocx:.claude/artifacts/adr_shell_env_overhaul.md:516`.

`ocx:.claude/artifacts/research_deprecation_telemetry_patterns.md` (dated 2026-04-29, 104 lines; written for one question, tracking `--mode=full` drift):
- `:15`: "**Declining:** Phone-home opt-in telemetry for tools targeting CI/private-registry use. Incompatible with offline + private-first constraints."
- `:44-46`: "OpenTelemetry Opt-In — Incompatible With OCX's Three Constraints ... None satisfies all three OCX constraints simultaneously: backend-first (clean stdout), offline-first (no network call), private-first (no shared telemetry endpoint across orgs on private registries)."
- `:16`: "Hard rule from real incidents: Never put deprecation messages on stdout — breaks JSON consumers (gh CLI #5674, sf CLI #1896)."
- `:58-60`: stable-prefix stderr line, "CI log retention is the aggregation layer; platform teams grep. No phone-home, no opt-in, no privacy issue."
- `:84-87`: comparison table row "Phone-home opt-in | No (network call) | No | Sometimes | No"; `:81` "Option C (registry-side aggregation): Structurally incompatible. Private-first users do not share a common telemetry channel."; `:83` verdict chose structured stderr.
- Scope caveat: the finding addressed flag-drift tracking, predates OCX's CI telemetry (Sep 2026) and has no operator-hosted-collector case; it reasons about a collector shared across orgs, not one each operator runs.

### 16. Interface-contract obligations of a new env variable or config key

Quoted from `ocx:.claude/rules/subsystem-interface-contract.md` and `ocx:AGENTS.md`:
- Counterparts table: "An env declaration in `ocx_env` | regenerate `reference/environment.md` (`task website:reference:generate`) | `task website:reference:check`, `crates/ocx_schema/tests/docs.rs`".
- "A flag, argument or command | regenerate `reference/command-line/**` ... " (applies if a flag is added); "A deliberate exception | `crates/ocx_schema/contract/exemptions.toml` (cites a decision record) or a `waivers/<area>.toml` entry".
- IC-13: "Env name = `OCX_` + SCREAMING(long flag that departs from the default); positive `OCX_X`, negative `OCX_NO_X`". IC-14: "Internal variables are `__OCX_TESTING_*` (tests) or `__OCX_*` (plumbing), never `OCX_TEST_*`". IC-20: "Defaults are literals; env applies in `ContextOptions`, not in clap". IC-06: "Keys and enum values are snake_case on every surface (JSON, flag choices, env values)".
- Visibility doc: `Public` is "documented, in the SDK manifest and the compatibility gate" (`ocx:crates/ocx_env/src/lib.rs:68-69`); the SDK manifest is `ocx_sdkgen`'s "public environment manifest" (`ocx:crates/ocx_sdkgen/src/rust.rs:815`). The `Foreign` visibility is documented but, per its doc comment, owned by another tool.
- Break rules: "Does it rename or remove a flag, env name, slug or field? It is a break: the retired name goes into the registry, and an in-flight spelling into `deprecated.rs`." (`:Semantic-break checklist`.)
- `AGENTS.md` (stability tiers): interfaces are "every wire/persisted format — command and flag grammar, exit codes, package metadata, OCI manifests, `ocx.lock`, the index format, `ocx.toml`, the `--format json` reports and error document, the `OCX_*` environment variables"; "Even interfaces break pre-1.0. A break is announced in the changelog and nowhere else"; the changelog entry is the commit subject (never `CHANGELOG.md`).
- Ledger scope: "a break to a gated machine document — a `--format json` report root, the error document, or a command in `cli.json` — also needs an entry in the contract ledger (`ledger.toml` ...) and a bumped version". `crates/ocx_schema/contract/ledger.toml` exists. The AGENTS.md sentence names report roots, the error document and `cli.json` commands; it does not name env variables or config keys as ledger-gated. Whether a new `Public` env variable alone is baseline-gated through the SDK manifest was not verified.
- `config.toml` keys: the `Config` doc says unknown keys degrade silently and "a change that cannot degrade ... ships as a managed payload under a new OCI tag" (`ocx:crates/ocx_config/src/lib.rs:77-80`); `config.toml` is not listed among the interface formats in `AGENTS.md`, but `reference/configuration.md` is generated from the published schema (`reference/configuration.md:6`). Whether a `config.toml` schema change is compat-gated was not verified.
- Env reads: any new variable must be read through `ocx_env`; a dependency that reads its own variables (an OTel exporter reading `OTEL_*`) is outside the clippy ban and "Re-audit env reads inside dependencies on each bump, and declare any the product depends on". (`subsystem-interface-contract.md` § Environment reads.)

---

## Cross-checks and contradictions

negative:
- OCX has no `opentelemetry*`, `tracing-opentelemetry` or `tonic` package in `Cargo.lock`; no `OTEL_*`, `DO_NOT_TRACK` or `TELEMETRY` declaration in `ocx_env`. (Confirms the brief.)
- OCX `OCX_CONFIG` is declared `Path`, not `PathList`, and the loader never splits it; no OCX variable names more than one config file (`--config` is a single `Option<PathBuf>`). The assumption "a variable can name several files" holds in Grimoire (`GRIM_CONFIG`) and not in OCX.
- The OCX product-context file has no explicit no-telemetry or no-phone-home principle; the nearest are backend-first, offline-first, private-first, and the "never phone home" line lives in a PRD user story and an ADR cost line. The only explicit rejection of OTel opt-in is a research artifact written for a different question (flag-drift), dated 2026-04-29, before OCX's own CI telemetry existed.
- OCX does not have a "per-key append" config merge anywhere except `[trust.policy]`; everything else replaces or merges field-wise. Grimoire's layers also never append (only per-entry map merge).
- Grimoire has no first-run consent prompt or banner, no hashing of identifiers, no dry-run mode (the debug audit line is the audit path), and no traces or metrics.
- No acceptance test found asserting that the hook launcher never exports; the ADR's "Hook(Run) early return" (condition 7) no longer matches the code, where the launcher is a separate program.
- Grimoire docs claim a hung DNS resolver can exceed the export timeout; `main.rs` caps runtime shutdown at 1 s. Not reconciled in what I read.
- The Grimoire ADR's Validation boxes are unchecked in the file though the plan's verification record reports the checks done.
- I did not find where OCX enforces `Public => OCX_ prefix` (only the IC-13/IC-14 statements and `is_reserved_ocx_key`); and I did not verify whether a new Public env variable or a new `config.toml` section is baseline-gated beyond the regenerate-reference-docs counterpart.
- The Grimoire archaeology note (`research_otel_repo_archaeology.md:49-50`) says ocx has "a hand-written OTLP/HTTP+JSON exporter" (Python, CI only); I confirmed it is `scripts/bep_to_otlp.py` and not product code.

leads:
- Check how the Grimoire `GrimHttpClient` header-strip and the exporter's own `OTEL_*` env reads would interact with OCX's clippy ban on dependency env reads; the pinned crate's full env surface (beyond headers) was not audited here.
- Read `adr_managed_config_tier.md` and `adr_ocx_interface_contract.md` (baseline scope) to settle whether a new `config.toml` section or `Public` env variable is compat-gated; the AGENTS.md sentence only names JSON reports, error document and `cli.json`.
- Enumerate `info_span!`/`#[instrument]` use in OCX crates; span coverage decides whether a tracing-based bridge has anything to export.
- Read `crates/ocx_config/src/loader.rs` tests around `OCX_NO_CONFIG` plus `managed` (lines ~3848-4440) to confirm what an operator-injected section survives for a hermetic CI child; this governs whether telemetry settings delivered through a managed payload reach `OCX_NO_CONFIG=1` runs (the `retain_system_locked_sections` list says only locked sections do).
- Verify how a child `ocx` launched by `ocx exec` inherits config policy (`exec_state::received::policy`, `loader.rs:~178-218`) before assuming telemetry settings or opt-outs propagate to nested ocx processes.
- Check the OCX CLI exit/shutdown path (runtime shutdown bound, where the process exits) before sizing any end-of-command export; the Grimoire equivalent is `main.rs:205-206,239-245` and a 500 ms default timeout.
- Look at `ocx-mirror` and `ocx-sdk-python` (lockstep consumers listed in AGENTS.md) for env handling that an added `OTEL_*`/telemetry variable would touch; they were out of scope here.
- Obtain the Grimoire PR/issue state for the egress ruling in `arch-threat-model.md` ("owed ... deferred to the owner"), since it is the one unresolved policy decision of the Grimoire design.
