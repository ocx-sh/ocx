# Research: OpenTelemetry in operator-configured CLI tools — prior art

## Metadata

**Date:** 2026-11-18
**Domain:** observability
**Triggered by:** hex-discuss otel-telemetry
**Expires:** 2027-05-18

Method: web search plus page fetches through a summarising fetcher. Claims marked **[unverified]** could not be confirmed from a primary source. Sources older than about 18 months (before 2025-05) are marked **[re-verify]**. No recommendation and no ranking is given, by design.

## Direct answer

No recommendation. Evidence that constrains the option space:

- **Standard `OTEL_*` variables are a stable, well-specified vocabulary.** The core set (`OTEL_SDK_DISABLED`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, exporter selection incl. `none`, samplers, batch processor, the whole OTLP exporter family) is marked Stable. Declarative file config (`OTEL_CONFIG_FILE`) exists but, when set, makes the SDK ignore every other `OTEL_*` variable except substitution references inside the file.
- **The Rust SDK honours only part of that vocabulary without application code.** `opentelemetry-otlp` 0.33.1 reads endpoint, protocol, headers, timeout, compression, insecure (plus per-signal variants). Its HTTP and tonic exporter source does not read `OTEL_EXPORTER_OTLP_CERTIFICATE`, `_CLIENT_KEY` or `_CLIENT_CERTIFICATE`. `OTEL_SDK_DISABLED` and `OTEL_TRACES_EXPORTER` are not documented as honoured by `opentelemetry_sdk`. A tool that wants these must implement them itself.
- **Traces in the Rust crates are still beta;** logs and metrics are stable.
- **Comparable CLIs split three ways:** (a) vendor telemetry to the vendor, default on, opt-out by `DO_NOT_TRACK` or a tool variable (GitHub CLI, .NET, Homebrew, Turborepo, Dagger); (b) operator-directed OTLP, off until configured (Docker CLI, Pulumi, Terraform, Claude Code); (c) none documented (uv's only metadata is a user-agent, npm logs registry request headers).
- **Exit-path harm is a documented, recurring failure mode:** retries and unbounded flush delay or hang CLIs (Claude Code incidents). Docker caps metric flush at 50 ms.
- **Env-var trace-context carrier is a Release Candidate**, not Stable, with a stabilisation window opening no earlier than 2026-11-02.
- **Default-on vendor CLI telemetry has repeatedly drawn backlash** (Go 2023, GitHub CLI 2026-04).

## Trends

| Class | Items |
|---|---|
| Trending | OTLP export from CLIs gated by an explicit operator-set endpoint or variable (Docker, Pulumi, Terraform, Claude Code); env-var trace-context propagation (Buildkite, Jenkins, Concourse, Terraform, Claude Code); `DO_NOT_TRACK` as a cross-tool opt-out; agent-detection fields in CLI telemetry (.NET, `gh`) |
| Established | `OTEL_*` SDK and OTLP exporter variables; OTLP/HTTP protobuf as spec default; batch processors with timeouts; `TRACEPARENT` from CI agents |
| Emerging | Declarative config via `OTEL_CONFIG_FILE`; env-var carrier spec reaching Stable; Rust OTLP retries on by default (0.33); managed-settings enforcement of OTEL destinations (Claude Code) |
| Declining | `OTEL_EXPORTER_ZIPKIN_*`, `jaeger`/`ottrace` propagators (deprecated), `OTEL_EXPERIMENTAL_CONFIG_FILE` (deprecated), Prometheus exporter in Rust (deprecated at 0.29), `grpcio` Rust exporter (removed), default-on-without-notice CLI telemetry (Go reversed to opt-in) |

## Axis 1 — OpenTelemetry environment-variable specification

Source: [SDK environment variables](https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/), [OTLP exporter](https://opentelemetry.io/docs/specs/otel/protocol/exporter/). Page header: Stable except where marked.

| Variable(s) | Status | Default / note |
|---|---|---|
| `OTEL_SDK_DISABLED` | Stable | false; no-op SDK for all signals; does not affect propagators |
| `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES` | Stable | service name overrides same key in attributes |
| `OTEL_LOG_LEVEL`, `OTEL_PROPAGATORS` | Stable | info; `tracecontext,baggage` (`jaeger`, `ottrace` deprecated) |
| `OTEL_TRACES_SAMPLER`, `_ARG` | Stable | `parentbased_always_on` |
| `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER`, `OTEL_LOGS_EXPORTER` | Stable | default `otlp`; values `otlp`, `console`, `none`; comma list is a MAY; `otlp/stdout` is Development |
| `OTEL_BSP_*` (schedule 5000 ms, export timeout 30000, queue 2048, batch 512), `OTEL_BLRP_*` (schedule 1000 ms) | Stable | batch span / log record processors |
| `OTEL_METRIC_EXPORT_INTERVAL` (60000), `_TIMEOUT` (30000) | Stable | metric reader |
| `OTEL_*_LIMIT` family | Stable | attribute value/count limits |
| `OTEL_EXPORTER_OTLP_{ENDPOINT,PROTOCOL,HEADERS,TIMEOUT,COMPRESSION,INSECURE,CERTIFICATE,CLIENT_KEY,CLIENT_CERTIFICATE}` + `_TRACES_/_METRICS_/_LOGS_` variants | Stable (exporter doc) | protocol default `http/protobuf`; timeout 10 s covering retries; compression only `gzip`/`none`; per-signal takes precedence |
| `OTEL_EXPORTER_ZIPKIN_*` | Deprecated | |
| `OTEL_EXPORTER_PROMETHEUS_HOST/PORT` | Development | |
| `OTEL_EXPERIMENTAL_CONFIG_FILE` | Deprecated | use `OTEL_CONFIG_FILE` |
| `OTEL_CONFIG_FILE` | no marker on the page | see below |

Facts that matter for design:
- Parsing: empty value = unset; booleans true only for case-insensitive "true"; invalid values warn and are ignored (same page).
- Endpoint path rule: only the generic endpoint gets `/v1/traces` etc. appended; a per-signal endpoint is used verbatim ([exporter spec](https://opentelemetry.io/docs/specs/otel/protocol/exporter/)).
- Headers format `k=v,k=v`; there is no spec'd secret-source indirection ([exporter spec](https://opentelemetry.io/docs/specs/otel/protocol/exporter/)).
- Client private key supports only unencrypted keys; a password variable has been proposed ([spec issue 4580](https://github.com/open-telemetry/opentelemetry-specification/issues/4580), surfaced via search snippet, **[unverified]** beyond the snippet).
- Declarative config: when `OTEL_CONFIG_FILE` is set the file is parsed and **all other environment variables are ignored**, except those referenced for substitution inside the file; combining sources requires implementation-specific customisation ([same page](https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/)). The configuration SDK page is Stable except ConfigProvider and some programmatic options which are Development ([config SDK](https://opentelemetry.io/docs/specs/otel/configuration/sdk/)). The data-model page's own status was not retrieved **[unverified]**. Support is per-language ([config SDK](https://opentelemetry.io/docs/specs/otel/configuration/sdk/)); no Rust support was found (the Rust status pages say nothing, see axis 2).
- Release-candidate or experimental items are not mixed with the Stable ones: env-var carrier (axis 5), `otlp/stdout` exporter value.

## Axis 2 — Rust implementation options

Versions (fetched 2026-11-18): `opentelemetry` 0.33.1 (2026-10-08), `opentelemetry_sdk` 0.33.1, `opentelemetry-otlp` 0.33.1, `tracing-opentelemetry` 0.34.0 (2026-09-23, targets `opentelemetry ^0.33`). Sources: [releases](https://github.com/open-telemetry/opentelemetry-rust/releases), [otlp docs](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/), [sdk docs](https://docs.rs/opentelemetry_sdk/latest/opentelemetry_sdk/), [tracing-opentelemetry](https://docs.rs/crate/tracing-opentelemetry/latest).

Stability:
- Logs and metrics API/SDK stable; OTLP exporters for logs and metrics declared stable in 0.33.1; **tracing and the traces OTLP exporter still beta** ([otlp docs](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/), [Rust status](https://opentelemetry.io/docs/languages/rust/)). Each minor release since 0.27 carried breaking changes ([otlp changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-otlp/CHANGELOG.md)). 0.33.0 alone removed transport-first builders, shrank `ExporterBuildError` to two variants, and made config traits sealed.
- Logs bridge: 0.29.0 made the Logs SDK and the tracing appender stable ([releases](https://github.com/open-telemetry/opentelemetry-rust/releases)).

Env variables honoured without application code:
- `opentelemetry-otlp`: endpoint, protocol (per-signal since 0.32), headers, timeout, compression, insecure, plus metrics temporality ([docs](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/), [changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-otlp/CHANGELOG.md)). Since 0.33: protocol/compression case-insensitive, empty = unset, invalid endpoint errors rather than falling back, schemeless endpoints default to `https://`.
- Precedence: builder settings, then per-signal env, then generic env, then defaults ([docs](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/)). Since 0.29, code-based config is final and env no longer overrides it ([changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-otlp/CHANGELOG.md)). So an operator override and an application default need explicit layering by the tool.
- Headers: per-signal var is used instead of the generic one, not merged ([HTTP exporter source](https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/opentelemetry-otlp/src/exporter/http/mod.rs)). Claude Code, by contrast, merges per-signal headers with generic ([monitoring docs](https://code.claude.com/docs/en/monitoring-usage)).
- **Certificate / client key / client certificate variables: not read** by the HTTP exporter or the tonic exporter source; TLS comes only from `with_tls_config()` (tonic) or from a user-supplied HTTP client ([HTTP source](https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/opentelemetry-otlp/src/exporter/http/mod.rs), [tonic source](https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/opentelemetry-otlp/src/exporter/tonic/mod.rs)). Based on those files at `main` read through a summariser; a crate-wide grep was not run.
- `opentelemetry_sdk`: `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_TRACES_SAMPLER(_ARG)`, span limits, `OTEL_BSP_*`, `OTEL_BLRP_*`, `OTEL_METRIC_EXPORT_INTERVAL` ([sdk docs](https://docs.rs/opentelemetry_sdk/latest/opentelemetry_sdk/)). `OTEL_SDK_DISABLED` and `OTEL_TRACES_EXPORTER` are not on the page; a third-party crate checks `OTEL_SDK_DISABLED` itself ([search snippet](https://docs.rs/crate/crabka-telemetry/0.3.2/source/src/lib.rs), **[unverified]** for the official SDK). No autoconfigure layer is described. Sampler gaps in some versions: `parentbased_jaeger_remote` falls back with a warning ([search snippet](https://docs.rs/crate/opentelemetry_sdk/latest/source/src/trace/config.rs), **[unverified]** for 0.33).
- `OTEL_BSP_EXPORT_TIMEOUT` / `OTEL_BLRP_EXPORT_TIMEOUT` were removed from processors in 0.28; the timeout now lives on the exporter (`OTEL_EXPORTER_OTLP_TIMEOUT`) ([sdk changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-sdk/CHANGELOG.md)). 0.33 made the BSP/BLRP/PeriodicReader variable names public constants.
- Proxies: HTTP exporter source sets no proxy; behaviour depends on the reqwest client's defaults **[unverified]**.

Feature flags and weight ([otlp docs](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/)):
- Default features: `http-proto`, `reqwest-blocking-client` (plus signals). `grpc-tonic` left the defaults in 0.28. Others: `http-json`, `gzip-http`, `zstd-http`, `reqwest-client`, `hyper-client`, `tls-ring`, `tls-aws-lc`, `tls-provider-agnostic`, `tls-roots`, `tls-webpki-roots`, `reqwest-rustls`.
- gRPC path pulls tonic, prost, http and tokio ([forked Cargo.toml snapshot](https://redirect.github.com/djc/opentelemetry-rust/blob/4e45893d06d76f6c60c54668aaaf7cfba87df157/opentelemetry-otlp/Cargo.toml), **[re-verify]**, older snapshot).
- **No published binary-size or compile-time measurements were found.** Search returned only feature lists; the advice offered was to measure with `cargo bloat` / `cargo build --timings`.
- 0.33.0 made the SDK's only required dependency the API crate, enabling a minimal build with `default-features = false` ([sdk changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-sdk/CHANGELOG.md)).
- 0.33.1 and vNext: zstd bumped for RUSTSEC-2026-0391 ([otlp changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-otlp/CHANGELOG.md)); an exporter dependency set is a supply-chain surface a tool inherits.

Blocking vs async:
- Since 0.28 the batch span processor, batch log processor and periodic reader run on their own OS thread, no async runtime required. Limits: process must be able to spawn threads, `hyper` and `reqwest` async clients unsupported on that path, no concurrent exports ([sdk changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-sdk/CHANGELOG.md)). The otlp docs say the default processors should use the blocking reqwest client, and gRPC/tonic needs Tokio ([otlp docs](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/)).
- `rt-tokio` and `rt-tokio-current-thread` exist for the experimental async-runtime processors ([sdk docs](https://docs.rs/opentelemetry_sdk/latest/opentelemetry_sdk/)).
- Simple processor over tonic: panics outside a Tokio runtime, and inside one deadlocked through reentrancy from exporter-generated `tracing` spans ([issue 2188](https://github.com/open-telemetry/opentelemetry-rust/issues/2188), Oct-Dec 2024, **[re-verify]**). 0.30 added context-based suppression on the batch threads; 0.32.0 added then 0.32.1 reverted suppression in `SimpleSpanProcessor` after a `RefCell` borrow panic ([sdk changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-sdk/CHANGELOG.md)).

Hand-writing OTLP instead of the SDK:
- For: OTLP/HTTP JSON is a small schema; SDK-free crates exist (`zerodds-observability-otlp`, `rlg-otlp`, `ro11y`) but all are small third-party crates ([search results](https://docs.rs/zerodds-observability-otlp), **[unverified]** quality and maintenance).
- Against: the spec's OTLP/HTTP behaviours (retry classes, `Retry-After`, path rules, 64 MiB body cap, partial-success handling) are what the SDK now implements by default ([otlp changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-otlp/CHANGELOG.md)). The project's own rule treats hand-rolled wire-format emitters as Block-tier risk (`quality-core.md` § "Don't Own Non-Domain Code"; local file, not web).
- No source found comparing the two in a CLI context.

## Axis 3 — Short-lived process behaviour

- Simple vs batch: the Rust guidance and #2188 point to batch processors for networked exporters ([issue 2188](https://github.com/open-telemetry/opentelemetry-rust/issues/2188)). The `SimpleSpanProcessor` exports synchronously per span, so per-span latency lands on the instrumented path **[inference from processor design; not stated in a fetched source]**. Terraform uses `SimpleSpanProcessor` ([telemetry.go](https://github.com/hashicorp/terraform/blob/main/telemetry.go)).
- Defaults a short-lived process must override: batch schedule delay 5000 ms (spans) / 1000 ms (logs), metric export interval 60000 ms ([spec](https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/)). A process shorter than those never exports unless it flushes at exit.
- Flush and shutdown: `shutdown_with_timeout` across signals since 0.30; the default `SpanExporter::shutdown()` timeout was fixed from 5 ns to 5 s in 0.32.0; 0.33.0 fixed a race that could drop spans around `force_flush()` and `shutdown()`; vNext makes batch processors report final-export failures from shutdown ([sdk changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-sdk/CHANGELOG.md)). Dropping the provider handle does not flush; only explicit shutdown ships the last batch ([search snippet](https://docs.rs/crate/ruststream/0.6.1/source/examples/otel_export.rs), **[unverified]**, third-party example).
- Timeouts: spec exporter timeout is 10 s total including retries ([exporter spec](https://opentelemetry.io/docs/specs/otel/protocol/exporter/)); Rust 0.33 adds default retries (3, exponential backoff with jitter, HTTP 429/502/503/504) ([otlp changelog](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-otlp/CHANGELOG.md)). Worst-case exit delay on an unreachable or slow collector is therefore bounded by exporter timeout (and, for a collector that accepts but never answers, by the full timeout) ([guide, vendor blog](https://oneuptime.com/blog/post/2026-02-06-troubleshoot-collector-not-exporting-data/view), **[unverified]** against Rust specifically).
- Connection: tonic channel uses `connect_lazy()`, so connection is deferred to first export ([tonic source](https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/opentelemetry-otlp/src/exporter/tonic/mod.rs)).
- Reported incidents: Claude Code CLI unusable under OTEL ECONNRESET with no circuit breaker ([issue mirror 37079](https://claudeissues.com/issue/37079-bug-otel-telemetry-econnreset-propagates-to-main-event-loop-making-cli-unusable), [36294](https://claudeissues.com/issue/36294-otel-telemetry-econnreset-makes-claude-code-cli-unusable)); subagent processes hung at exit retrying exports against a 429 endpoint ([30378](https://claudeissues.com/issue/30378-subagent-processes-hang-on-exit-due-to-otel-telemetry-export-retries-429-rate-li)). These are third-party mirrors of the tracker, dates not retrieved. Earlier Rust report of a shutdown hang under Tokio multi-thread schedulers: [issue 536](https://redirect.github.com/open-telemetry/opentelemetry-rust/issues/536), old, **[re-verify]**.
- Bounded-exit practice in the wild: Docker CLI sets metric `ExportTimeout` to 50 ms so telemetry cannot hold up exit ([telemetry.go](https://raw.githubusercontent.com/docker/cli/master/cli/command/telemetry.go)). Homebrew sends from a detached background process with a short timeout, failures silent ([Homebrew analytics](https://docs.brew.sh/Analytics)). otel-cli's own README advises a local buffering collector because it connects on every run, and exposes `--timeout` and `--fail` ([otel-cli](https://github.com/equinix-labs/otel-cli)). Terraform's file has no shutdown/flush logic in `telemetry.go` (may live elsewhere) ([telemetry.go](https://github.com/hashicorp/terraform/blob/main/telemetry.go)).
- No Rust issue found documenting a CLI slowdown from the Rust SDK specifically **[negative]**.

## Axis 4 — How comparable CLIs expose telemetry

| Tool | Default | Consent | Standard `OTEL_*` honoured | Tool-specific variables | Admin enforcement | Source |
|---|---|---|---|---|---|---|
| Claude Code | off | explicit `CLAUDE_CODE_ENABLE_TELEMETRY=1` | `OTEL_{METRICS,LOGS,TRACES}_EXPORTER`, `OTEL_EXPORTER_OTLP_*` incl. CERTIFICATE/CLIENT_KEY/CLIENT_CERTIFICATE, `OTEL_RESOURCE_ATTRIBUTES`, intervals, temporality | `OTEL_LOG_USER_PROMPTS`, `_TOOL_DETAILS`, `_TOOL_CONTENT`, `_RAW_API_BODIES`, `OTEL_METRICS_INCLUDE_*`, `otelHeadersHelper`, `CLAUDE_CODE_PROPAGATE_TRACEPARENT` | Yes: managed settings `env` block; conflicting developer values (per-signal endpoints, protocols, credentials) are stripped at startup; exporter selectors are not stripped unless also set; repo-level settings ignore OTEL exporter vars (a repo may still set a selector to `none`); OTEL vars not passed to subprocesses | [monitoring](https://code.claude.com/docs/en/monitoring-usage) |
| Docker CLI | off | setting an endpoint | no standard names | `DOCKER_CLI_OTEL_EXPORTER_OTLP_ENDPOINT`; `OTEL_RESOURCE_ATTRIBUTES` read but `docker.cli.*` keys stripped | not documented | [docs](https://docs.docker.com/engine/cli/otel/), [source](https://raw.githubusercontent.com/docker/cli/master/cli/command/telemetry.go) |
| buildx / BuildKit | off | env | `TRACEPARENT`/`TRACESTATE` read; `OTEL_TRACE_PARENT` fallback may remain (code and comment disagree) | none confirmed | none | [BuildKit traceenv search result](https://sources.debian.org/src/docker.io/26.1.5%2Bdfsg1-9/buildkit/util/tracing/env/traceenv.go), **[unverified]** |
| Bazel | off (BEP is flag-opt-in; default not stated on page) | flags | no OTLP; BEP via `--bes_backend`, `--build_event_json_file` | n/a | not covered on page (rc-file layering not verified) | [BEP](https://bazel.build/remote/bep) |
| Dagger | CLI telemetry to vendor on | opt-out | engine traces are OTLP; env names **[unverified]** | `DO_NOT_TRACK=1` | not documented | [0.9 archive, re-verify](https://archive.docs.dagger.io/0.9/294022/telemetry); current page returned 404 |
| Nx | prompt on first interactive run; CI never prompts, runs only if `analytics: true` | prompt + `nx.json` | none | none documented | workspace file | [Nx](https://nx.dev/docs/reference/telemetry) |
| Turborepo | on (inferred; default not stated) | opt-out | none | `TURBO_TELEMETRY_DISABLED=1`, `DO_NOT_TRACK=1`, `turbo telemetry disable` | not documented | [Turborepo](https://turborepo.dev/docs/telemetry) |
| Go toolchain | `local` (collect locally, never upload) | `go telemetry on` for upload | none (own counter system) | `go telemetry on/off/local`; `GOTELEMETRY` is read-only | none documented | [go.dev](https://go.dev/doc/telemetry) |
| .NET CLI | on for Microsoft distributions | first-run notice | none | `DOTNET_CLI_TELEMETRY_OPTOUT=1`; CI and LLM-agent detection fields | not documented | [MS Learn](https://learn.microsoft.com/en-us/dotnet/core/tools/telemetry) |
| Homebrew | on after notice | notice before first event | none | `HOMEBREW_NO_ANALYTICS=1`, `brew analytics off` | not documented | [Homebrew](https://docs.brew.sh/Analytics) |
| Terraform | tracing off; Checkpoint (version/security check) on | env | `OTEL_TRACES_EXPORTER=otlp` then `autoexport` reads the standard OTLP variables; `TRACEPARENT`/`TRACESTATE`/`BAGGAGE` read | `CHECKPOINT_DISABLE`; `disable_checkpoint` in CLI config; code comment: experimental, "not a committed external interface" | CLI config file | [telemetry.go](https://github.com/hashicorp/terraform/blob/main/telemetry.go), [checkpoint](https://developer.hashicorp.com/terraform/cli/commands) |
| Pulumi | off | `--otel-traces file://` or `grpc://` flag (v3.226+) | none documented | flag; `PULUMI_OTEL_EXPORTER_OTLP_ENDPOINT` is set by Pulumi for child processes | not documented | [Pulumi tracing](https://www.pulumi.com/docs/iac/operations/debugging/performance-tracing/) |
| GitHub CLI | on since v2.91.0 (2026-04) | none at first; opt-out | none | `GH_TELEMETRY` (falsy values; `log` prints payload to stderr), `DO_NOT_TRACK`, `gh config set telemetry`; none when target is GHES | not documented | [GitHub docs](https://docs.github.com/en/github-cli/github-cli/github-cli-telemetry), [changelog](https://github.blog/changelog/2026-04-22-github-cli-opt-out-usage-telemetry) |
| uv | none found; sends "linehaul" metadata in the user-agent to indexes, no opt-out per maintainers | n/a | none | none | none | [uv #8474](https://github.com/astral-sh/uv/issues/8474); no privacy page found (404 on policy URL) **[unverified]** that no other telemetry exists |
| cargo | none found | n/a | n/a | n/a | n/a | issue search shows only chrome-trace logging PR [#13399](https://github.com/rust-lang/cargo/issues?q=telemetry); no primary statement **[unverified]** |
| mise | none found | n/a | n/a | n/a | n/a | [FAQ](https://mise.jdx.dev/faq.html) is silent; **[unverified]** |
| pnpm | none found | n/a | n/a | n/a | n/a | [FAQ](https://pnpm.io/faq) is silent; user-agent carries versions ([Socket glossary](https://socket.dev/glossary/telemetry.md)); **[unverified]** |
| npm | no telemetry endpoint; registry request logging | none, no opt-out | n/a | n/a | n/a | [npm privacy](https://docs.npmjs.com/policies/privacy): session id, `npm-in-ci`, command in referrer with paths redacted |

Observations drawn from the table:
- Operator-directed OTLP tools use one of three naming schemes: standard `OTEL_*` (Terraform, Claude Code), a tool-prefixed endpoint variable (Docker), or a flag (Pulumi). Claude Code is the only one found that documents operator enforcement and conflict-stripping.
- Docker's `OTEL_RESOURCE_ATTRIBUTES` handling strips keys with its own prefix, so an operator cannot spoof tool attributes ([source](https://raw.githubusercontent.com/docker/cli/master/cli/command/telemetry.go)).
- `DO_NOT_TRACK` is honoured by gh, Turborepo, Dagger; not by .NET, Homebrew. Dagger and others are listed on a community registry ([telemetry.how](https://www.github.com/yeskunall/telemetry.how), not reviewed).
- GitHub CLI's `log` mode (print payload to stderr instead of sending) and Claude Code's content-gating variables are the two documented transparency devices.
- Search-only claims: Dagger's current docs path 404'd, so Dagger's OTEL variables and Cloud-vs-DO_NOT_TRACK interaction are unconfirmed.

## Axis 5 — Trace-context propagation between processes

- Status: environment-variable carrier spec is **Release Candidate** ([env carriers](https://opentelemetry.io/docs/specs/otel/context/env-carriers/)). Stabilisation: not before 2026-11-02 and not until 14 days with no new related issue; feedback on [#5040](https://opentelemetry.io/blog/2026/environment-variable-context-propagation/) ([blog](https://opentelemetry.io/blog/2026/environment-variable-context-propagation/)). The earlier proposal issue was closed 2025-04-02 after consensus to use `TRACEPARENT`/`TRACESTATE` unadorned ([spec issue 740](https://github.com/open-telemetry/opentelemetry-specification/issues/740)).
- Rules: format-agnostic carrier; keys normalised (uppercase, non-alphanumerics to `_`, prefix `_` if leading digit); implementations MUST NOT spawn child processes as part of propagation; guidance is to copy the environment per child, inject just before spawn, extract at startup, avoid mutating the parent's global environment ([env carriers](https://opentelemetry.io/docs/specs/otel/context/env-carriers/), [blog](https://opentelemetry.io/blog/2026/environment-variable-context-propagation/)). The spec page itself does not name `TRACEPARENT`; it follows from the W3C propagator and normalisation.
- Security: not for sensitive information; env visible to the process and possibly others; review baggage before crossing a trust boundary; receivers treat fields as untrusted ([same two sources](https://opentelemetry.io/blog/2026/environment-variable-context-propagation/)). Windows case-insensitivity is an open concern.
- Who sets it: Buildkite agent (`TRACEPARENT`, `TRACESTATE`) ([docs](https://buildkite.com/docs/agent/v3/tracing)); Jenkins OpenTelemetry plugin for shell/batch/PowerShell steps ([blog](https://opentelemetry.io/blog/2026/environment-variable-context-propagation/)); Concourse injects `TRACEPARENT` into task runs ([docs](https://concourse-ci.org/docs/operation/tracing/), search-snippet, **[unverified]** details); otel-cli, Thoth, Run-with-Telemetry wrap commands ([blog](https://opentelemetry.io/blog/2026/environment-variable-context-propagation/)); Claude Code sets it on Bash/PowerShell subprocesses and reads inbound values in `claude -p` and SDK sessions but not interactive ones ([monitoring](https://code.claude.com/docs/en/monitoring-usage)). GitHub Actions: no native support found; third-party wrappers only ([traci snippet](https://beta.pkg.go.dev/github.com/nextrevision/traci)).
- Who reads it: Terraform (`TRACEPARENT`, `TRACESTATE`, `BAGGAGE`, only when export is enabled) ([telemetry.go](https://github.com/hashicorp/terraform/blob/main/telemetry.go)); BuildKit/buildx ([source](https://sources.debian.org/src/docker.io/26.1.5%2Bdfsg1-9/buildkit/util/tracing/env/traceenv.go)); otel-cli with `--tp-ignore-env` to disable ([otel-cli](https://github.com/equinix-labs/otel-cli)).
- Older variants: `OTEL_TRACE_PARENT` / `OTEL_TRACE_STATE` (BuildKit fallback, opentelemetry-swift) predate the consensus ([issue 740](https://github.com/open-telemetry/opentelemetry-specification/issues/740)).
- Jenkins had a defect where `TRACEPARENT` was not updated per step inside a stage ([community RFC](https://community.jenkins.io/t/rfc-architecture-design-scalable-opentelemetry-for-jenkins/36060/2), whether fixed in a release **[unverified]**).
- What a spawner is expected to do: per spec guidance above; applications pass the SDK-provided context to their own spawning mechanism. Claude Code additionally withholds all `OTEL_*` exporter variables from subprocesses ([monitoring](https://code.claude.com/docs/en/monitoring-usage)), whereas the spec text is silent on exporter variables, so a child tool may inherit the parent's exporter configuration (and secrets in `OTEL_EXPORTER_OTLP_HEADERS`) unless the parent strips them **[inference]**.

## Axis 6 — Privacy and enterprise constraints

Recorded attributes in practice:
- Docker: one metric, `command.time`, attributes command name, exit code, and three TTY flags ([Docker](https://docs.docker.com/engine/cli/otel/)).
- gh: device ID, OS, arch, version, command and flags, TTY/CI/Actions flags, repo visibility for some commands ([GitHub docs](https://docs.github.com/en/github-cli/github-cli/github-cli-telemetry)).
- Homebrew: package names, install options, OS, no user identifier or IP field ([Homebrew](https://docs.brew.sh/Analytics)).
- Nx and Turborepo: explicitly do not collect env vars, paths, file contents ([Nx](https://nx.dev/docs/reference/telemetry), [Turborepo](https://turborepo.dev/docs/telemetry)).
- .NET: hashes MSBuild properties, template and tool names, MAC address and working directory; collects crash stack traces of CLI code; leaks SDK source paths of custom builds unhashed; since SDK 11.0.200 can send a normalised Microsoft-employee alias ([MS Learn](https://learn.microsoft.com/en-us/dotnet/core/tools/telemetry)).
- Claude Code: prompts, tool parameters, tool content, raw API bodies off by default and individually opt-in; `user.id` is a random anonymous id; content truncation limit 61440 UTF-16 units ([monitoring](https://code.claude.com/docs/en/monitoring-usage)).

Redaction practices:
- OTel semconv: `url.full`, `url.path`, `url.query` sensitive content SHOULD be scrubbed; URLs MUST NOT carry credentials; a default sensitive-query-key list exists (partial list seen via mirror) ([semconv URL](https://opentelemetry.io/docs/specs/semconv/url/url)). A semconv PR proposed `process.command.args` / `.command_line` not be collected by default without sanitisation; merge status **[unverified]** ([PR 961 snippet](https://github-redirect.dependabot.com/open-telemetry/semantic-conventions/pull/961)). Discussion there argued instrumentation should not bear redaction and collectors should (comment, not spec).
- Hashing of values (.NET), allow-listed arguments (.NET), env-key stripping (Docker), per-attribute opt-in (Claude Code) are the four patterns seen.
- Collector-side redaction processors are vendor-documented only ([Dynatrace](https://docs.dynatrace.com/docs/ingest-from/opentelemetry/collector/use-cases/redact), [Splunk](https://help.splunk.com/ja-jp/splunk-observability-cloud/manage-data/manage-sensitive-data)).

Secrets and credentials:
- Exporter headers carry tokens (`Authorization=Bearer ...` in the Claude Code example). Claude Code offers a headers-helper script run every 29 min by default for short-lived tokens, but only for HTTP protocols, and a failing helper makes exports fail silently ([monitoring](https://code.claude.com/docs/en/monitoring-usage)). Rust SDK has no equivalent in the sources read.
- Claude Code issue mirror: docs omitted that OTEL client-certificate variables in project settings need trust confirmation ([issue 66389](https://claudeissues.com/issue/66389-docs-otel-client-certificate-env-vars-docs-omit-trust-confirmation-requirement-f), mirror, not read in full **[unverified]**). Repository-controlled files cannot choose a destination in Claude Code ([monitoring](https://code.claude.com/docs/en/monitoring-usage)); this addresses a checked-out repo redirecting telemetry.
- Go OTLP log exporter ignored env TLS certs and fell back to system roots, bypassing mTLS/pinning; fixed in 0.21.0 ([advisory](https://advisories.gitlab.com/golang/go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploggrpc/CVE-2026-81871/), via search snippet). Same failure shape is possible in Rust because `CERTIFICATE` variables are not read there (axis 2) — operators who set them would get silent system-root trust **[inference]**.
- Go: client cert/key files from env are not reloaded for short-lived certs ([go #4683](https://github-redirect.dependabot.com/open-telemetry/opentelemetry-go/issues/4683), snippet), a lesser concern for short-lived processes.

Proxies, custom CAs, air-gapped:
- Claude Code uses `NODE_EXTRA_CA_CERTS` for HTTP OTLP trust and `OTEL_EXPORTER_OTLP_CERTIFICATE` for gRPC ([monitoring](https://code.claude.com/docs/en/monitoring-usage)). Rust: see axis 2; custom roots require code or a user-supplied HTTP client.
- Terraform Checkpoint: `CHECKPOINT_DISABLE` plus CLI config key; Pulumi `PULUMI_SKIP_UPDATE_CHECK` for no-internet use ([Terraform](https://developer.hashicorp.com/terraform/cli/commands), [Pulumi install snippet](https://www.pulumi.com/docs/iac/cli/environment-variables/)). Pulumi report that env-block injection from an extension still produced outbound requests ([issue snippet](https://github-users.yhajoa.workers.dev/pulumi/pulumi-vscode-tools/issues/30), **[unverified]**).
- File sink as air-gap pattern: Pulumi `file://` writes OTLP JSON locally ([Pulumi](https://www.pulumi.com/docs/iac/operations/debugging/performance-tracing/)); `otlp/stdout` exporter value is spec Development ([spec](https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/)).

Backlash and regulatory:
- Go: opt-out proposal (discussion [#58409](https://redirect.github.com/golang/go/discussions/58409), about 506 comments) revised to opt-in after pushback incl. GDPR concerns; Go 1.23 shipped with upload opt-in ([DevClass 2024-08](https://www.devclass.com/development/2024/08/14/go-123-released-with-telemetry-uploaded-to-google-but-opt-in-after-developer-feedback/1622400), [Register 2023](https://www.theregister.com/2023/05/17/googles_go_data_collection/), **[re-verify]**: 2023/2024 sources; the reversal date differs across sources).
- GitHub CLI: default-on from v2.91.0 without a standalone announcement; criticism that the data fields were not fully listed and that AI-agent measurement was the stated motive; the article does not mention GDPR ([Register 2026-04-22](https://www.theregister.com/2026/04/22/github_opts_all_cli_users/)). GitHub kept it default-on with opt-out and a `log` preview mode.
- No regulatory enforcement action against a CLI's telemetry was found **[negative]**; only commentary invoking GDPR.
- npm does not honour the Do Not Track header and offers no CLI opt-out for registry request logging ([npm privacy](https://docs.npmjs.com/policies/privacy)).

## Sources

- https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/ — all SDK env variables, statuses, `OTEL_CONFIG_FILE` interaction
- https://opentelemetry.io/docs/specs/otel/protocol/exporter/ — OTLP exporter variables, path rules, defaults
- https://opentelemetry.io/docs/specs/otel/configuration/sdk/ — configuration SDK status
- https://opentelemetry.io/docs/specs/otel/configuration/ — config overview (no status detail)
- https://opentelemetry.io/docs/specs/otel/context/env-carriers/ — env-var carrier spec (RC)
- https://opentelemetry.io/blog/2026/environment-variable-context-propagation/ — stabilisation window, implementations, concerns
- https://github.com/open-telemetry/opentelemetry-specification/issues/740 — proposal history for env propagation
- https://github.com/open-telemetry/opentelemetry-specification/issues/4580 — client key password proposal (search snippet only)
- https://opentelemetry.io/docs/specs/semconv/url/url — URL redaction rules
- https://opentelemetry.io/docs/languages/rust/ — Rust signal status
- https://opentelemetry.io/docs/languages/rust/exporters/ — Rust exporter example (no shutdown guidance)
- https://github.com/open-telemetry/opentelemetry-rust/releases — release history and stability steps
- https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-otlp/CHANGELOG.md — OTLP crate changes through 0.33.1
- https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-sdk/CHANGELOG.md — SDK threads, shutdown, processor changes
- https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/ — features, env variables honoured
- https://docs.rs/opentelemetry_sdk/latest/opentelemetry_sdk/ — SDK env variables honoured
- https://docs.rs/crate/tracing-opentelemetry/latest — version 0.34.0
- https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/opentelemetry-otlp/src/exporter/http/mod.rs — HTTP exporter env reads, client selection, retries
- https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/opentelemetry-otlp/src/exporter/tonic/mod.rs — tonic TLS configuration
- https://github.com/open-telemetry/opentelemetry-rust/issues/2188 — simple processor with tonic, reentrancy deadlock
- https://redirect.github.com/open-telemetry/opentelemetry-rust/issues/536 — old shutdown hang report
- https://code.claude.com/docs/en/monitoring-usage — Claude Code OTEL, managed settings, TRACEPARENT, privacy defaults
- https://claudeissues.com/issue/37079-bug-otel-telemetry-econnreset-propagates-to-main-event-loop-making-cli-unusable — CLI unusable on unreachable endpoint (mirror)
- https://claudeissues.com/issue/36294-otel-telemetry-econnreset-makes-claude-code-cli-unusable — same (mirror)
- https://claudeissues.com/issue/30378-subagent-processes-hang-on-exit-due-to-otel-telemetry-export-retries-429-rate-li — hang at exit (mirror)
- https://claudeissues.com/issue/66389-docs-otel-client-certificate-env-vars-docs-omit-trust-confirmation-requirement-f — docs gap on trust (mirror, snippet)
- https://docs.docker.com/engine/cli/otel/ — Docker CLI OTEL
- https://raw.githubusercontent.com/docker/cli/master/cli/command/telemetry.go — Docker CLI provider, 50 ms flush
- https://sources.debian.org/src/docker.io/26.1.5%2Bdfsg1-9/buildkit/util/tracing/env/traceenv.go — BuildKit env trace context (search result)
- https://bazel.build/remote/bep — Bazel BEP flags
- https://archive.docs.dagger.io/0.9/294022/telemetry — Dagger CLI telemetry (old)
- https://nx.dev/docs/reference/telemetry — Nx telemetry
- https://turborepo.dev/docs/telemetry — Turborepo telemetry
- https://go.dev/doc/telemetry — Go telemetry modes
- https://golang.google.cn/issue/58409 and https://redirect.github.com/golang/go/discussions/58409 — Go proposal threads
- https://www.devclass.com/development/2024/08/14/go-123-released-with-telemetry-uploaded-to-google-but-opt-in-after-developer-feedback/1622400 — Go opt-in outcome
- https://www.theregister.com/2023/05/17/googles_go_data_collection/ — Go backlash coverage
- https://learn.microsoft.com/en-us/dotnet/core/tools/telemetry — .NET CLI telemetry
- https://docs.brew.sh/Analytics — Homebrew analytics
- https://developer.hashicorp.com/terraform/cli/commands — Terraform Checkpoint
- https://github.com/hashicorp/terraform/blob/main/telemetry.go — Terraform OTLP tracing
- https://www.pulumi.com/docs/iac/operations/debugging/performance-tracing/ — Pulumi `--otel-traces`
- https://www.pulumi.com/docs/iac/cli/environment-variables/ — Pulumi env variables (search snippet)
- https://docs.github.com/en/github-cli/github-cli/github-cli-telemetry — gh telemetry
- https://github.blog/changelog/2026-04-22-github-cli-opt-out-usage-telemetry — gh default-on announcement
- https://www.theregister.com/2026/04/22/github_opts_all_cli_users/ — gh backlash
- https://github.com/astral-sh/uv/issues/8474 — uv linehaul metadata, no opt-out
- https://github.com/rust-lang/cargo/issues?q=telemetry — cargo issue search (no telemetry issue)
- https://mise.jdx.dev/faq.html — mise FAQ (silent)
- https://pnpm.io/faq — pnpm FAQ (silent)
- https://docs.npmjs.com/policies/privacy — npm request logging
- https://socket.dev/glossary/telemetry.md — npm/pnpm user-agent header observation
- https://github.com/equinix-labs/otel-cli — otel-cli env propagation and OTLP variables
- https://buildkite.com/docs/agent/v3/tracing — Buildkite `TRACEPARENT`
- https://concourse-ci.org/docs/operation/tracing/ — Concourse `TRACEPARENT` (snippet)
- https://community.jenkins.io/t/rfc-architecture-design-scalable-opentelemetry-for-jenkins/36060/2 — Jenkins TRACEPARENT defect (snippet)
- https://beta.pkg.go.dev/github.com/nextrevision/traci — GitHub Actions wrapper (snippet)
- https://www.github.com/yeskunall/telemetry.how — community opt-out variable list (not reviewed)
- https://oneuptime.com/blog/post/2026-02-06-troubleshoot-collector-not-exporting-data/view — vendor blog on exporter timeouts at exit
- https://advisories.gitlab.com/golang/go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploggrpc/CVE-2026-81871/ — Go OTLP exporter ignoring env TLS (snippet)
- https://docs.rs/zerodds-observability-otlp — SDK-free OTLP/HTTP JSON crate (snippet)

negative:
- No primary source for telemetry in uv (other than user-agent linehaul), cargo, mise, pnpm. "None" is an absence of documentation, not a statement by the projects.
- The Rust OTLP exporters (HTTP and tonic) do not read `OTEL_EXPORTER_OTLP_CERTIFICATE`, `_CLIENT_KEY`, `_CLIENT_CERTIFICATE`, contrary to the spec's Stable list and to the assumption that "OTLP exporter env vars" are fully covered.
- `OTEL_SDK_DISABLED` and `OTEL_TRACES_EXPORTER` are not documented as honoured by `opentelemetry_sdk`; `OTEL_CONFIG_FILE` has no Rust support found.
- No binary-size or compile-time measurements for the OTLP crates were found; the "tonic is heavy" assumption has no data in the sources reviewed.
- No Rust-SDK-specific incident of a CLI hanging on an unreachable collector was found; all CLI hang reports found are Node (Claude Code mirrors).
- No regulatory enforcement action against CLI telemetry was found, only GDPR commentary.
- Docker's operator variable is not a standard `OTEL_*` name, which contradicts the assumption that OTLP-capable CLIs reuse the standard names.
- Terraform's trace export is explicitly marked not a committed interface, so "Terraform supports OTEL" is weaker than it sounds.
- Env-var carrier spec is RC, not Stable, and the spec text does not itself name `TRACEPARENT`.

leads:
- Read the `opentelemetry-otlp` source and tests for any TLS/proxy environment handling (grep `CERTIFICATE`, `proxy`) — the summariser read only two files.
- Read the declarative-config data model page and opentelemetry-rust issue tracker for `OTEL_CONFIG_FILE` support — no Rust status found.
- Measure `cargo build --timings` and `cargo bloat` for `opentelemetry-otlp` http-proto + blocking reqwest vs grpc-tonic in a minimal binary — no published numbers.
- Confirm `OTEL_SDK_DISABLED` and `OTEL_TRACES_EXPORTER` handling in `opentelemetry_sdk` 0.33 source — docs page silent.
- Test the batch-processor shutdown behaviour against a blackholed endpoint and a slow endpoint in a Rust CLI — only vendor-blog evidence on exit delay.
- Check Docker CLI source for the exact `DOCKER_CLI_OTEL_*` variable names, protocol, and `TRACEPARENT` handling — fetched file did not contain them.
- Re-fetch Dagger's current telemetry and observability docs (404 on the guessed URL) — need current OTEL variable names and DO_NOT_TRACK scope.
- Check Bazel's rc-file layering (system rc) for admin enforcement and whether any OTLP BEP bridge exists — page silent.
- Check the OTel Rust compliance matrix and the env-carrier compliance matrix for Rust implementation status — blog says it is listed there.
- Verify the semconv status of `process.command.args` sanitisation guidance (PR 961 follow-up) — relevant to recording argv of a package manager.
- Read the Claude Code docs remainder (offset 100000) for any enforcement details not read — fetch covered only the first 100000 of 176669 characters.
