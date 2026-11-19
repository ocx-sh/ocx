# Research: OTel collector distribution, Rust SDK pin and semantic conventions

## Metadata

**Date:** 2026-11-18
**Domain:** observability
**Triggered by:** hex-plan otel-telemetry (research axis: technology)
**Expires:** 2027-05-18

Method: GitHub raw files and release pages, crates.io API, PyPI JSON, the opentelemetry.io semantic-convention registry. The orchestrator re-checked the core `otelcol` assets with HEAD requests and the repository licence through the GitHub API (marked **checked**). "Not verified" marks anything not confirmed.

## A. Collector for the standing acceptance test

Latest release v0.165.0, published 2026-11-07 ([releases API](https://api.github.com/repos/open-telemetry/opentelemetry-collector-releases/releases/latest)); contrib components in the manifests are v0.165.1.

| Distribution | Exporters | Platforms | Archive size |
|---|---|---|---|
| `otelcol-otlp` | `otlp`, `otlphttp` only — no `file`, no `debug` ([manifest](https://raw.githubusercontent.com/open-telemetry/opentelemetry-collector-releases/main/distributions/otelcol-otlp/manifest.yaml)) | linux, darwin, windows (amd64, arm64 and more) | linux/amd64 13.2 MB (**checked**) |
| `otelcol` (core) | `debug`, `file` (contrib 0.165.1), `otlphttp`, `otlp`, `nop`, kafka, prometheus, prometheusremotewrite, zipkin ([manifest](https://raw.githubusercontent.com/open-telemetry/opentelemetry-collector-releases/main/distributions/otelcol/manifest.yaml), `fileexporter` at line 24, **checked**) | all six OCX development platforms | linux/amd64 47.7 MB, linux/arm64 42.3 MB, darwin/amd64 49.1 MB, darwin/arm64 45.1 MB, windows/amd64 48.9 MB, windows/arm64 42.6 MB (**checked**, HTTP 200 + content-length on v0.165.0) |
| `otelcol-contrib` | superset | same | 87.6–109 MB |
| `otelcol-k8s` | `debug`, `file`, `otlphttp`, `otlp`, … | linux and windows only, **no darwin** | 43–49 MB |
| OCB custom build | any | built by the user | not a prebuilt binary |

- Asset names: `{{.Binary}}_{{.Version}}_{{.Os}}_{{.Arch}}.tar.gz`, e.g. `otelcol_0.165.0_linux_amd64.tar.gz`; every archive is `.tar.gz`, Windows included. Release URL pattern: `https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v0.165.0/<asset>`.
- Licence: Apache-2.0 (repository licence, **checked** via the GitHub API).
- `debug` exporter output is not parseable by contract — its README says the format of every verbosity level may change without a breaking change ([README](https://raw.githubusercontent.com/open-telemetry/opentelemetry-collector/main/exporter/debugexporter/README.md)). Use it for humans only.
- `file` exporter: alpha for logs; `format: json` (default) writes newline-delimited OTLP JSON, one line per batch; `flush_interval` default 1 s ([README](https://raw.githubusercontent.com/open-telemetry/opentelemetry-collector-contrib/main/exporter/fileexporter/README.md)). Not verified: that each line is an `ExportLogsServiceRequest` in protobuf-JSON mapping; inspect one file before writing the parser.
- **Recommendation: core `otelcol` v0.165.0.** It is the smallest distribution that ships as one prebuilt binary on all six platforms and can write what it received to a file. `otelcol-otlp` is smaller but can only forward over `otlphttp` to a second process; `otelcol-contrib` is 2× larger; `otelcol-k8s` has no darwin build.

Minimal config (per-test port and capture path; the test polls the file because flushing is asynchronous):

```yaml
receivers:
  otlp:
    protocols:
      http:
        endpoint: 127.0.0.1:${env:OTLP_PORT}
exporters:
  file:
    path: ${env:CAPTURE_PATH}
    format: json
    flush_interval: 100ms
service:
  telemetry:
    metrics:
      level: none
    logs:
      level: error
  pipelines:
    logs:
      receivers: [otlp]
      exporters: [file]
```

- The internal metrics endpoint defaults to port 8888; since v0.123.0 `service.telemetry.metrics.address` is ignored. `level: none` should stop it binding ([internal telemetry](https://opentelemetry.io/docs/collector/internal-telemetry/)); not verified — confirm by running two collectors at once, else use a `readers: - pull: exporter: prometheus: {host, port}` block on a per-test port.
- The OTLP receiver defaults to 4318: always set the endpoint.

## B. Rust crates

| Crate | Latest | Released | MSRV | Licence |
|---|---|---|---|---|
| `opentelemetry` | 0.33.1 | 2026-10-09 | not verified | Apache-2.0 |
| `opentelemetry_sdk` | 0.33.1 | 2026-10-09 | 1.75.0 | Apache-2.0 |
| `opentelemetry-otlp` | 0.33.1 | 2026-10-09 | 1.75.0 | Apache-2.0 |
| `opentelemetry-http` | 0.33.1 | 2026-10-09 | 1.75.0 | Apache-2.0 |
| `opentelemetry-proto` | 0.33.1 | 2026-10-09 | 1.75.0 | Apache-2.0 |

Sources: crates.io API ([otlp](https://crates.io/api/v1/crates/opentelemetry-otlp), [sdk](https://crates.io/api/v1/crates/opentelemetry_sdk), [http](https://crates.io/api/v1/crates/opentelemetry-http), [proto](https://crates.io/api/v1/crates/opentelemetry-proto)).

- 0.33.1 is current; `vNext` in the otlp changelog holds only a `zstd` bump (RUSTSEC-2026-0391, `zstd-http` feature only) ([changelog](https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/opentelemetry-otlp/CHANGELOG.md)). Cadence: 0.32.0 2026-05-08, 0.33.0 2026-09-18; next minor expected early 2027, likely breaking.
- Already in the 0.33 line and relevant: logs and metrics exporters declared stable (0.33.1); OTLP/HTTP retries **on by default** (up to 4 attempts on 429/502/503/504) — disable with `.with_retry_policy(RetryPolicy::disabled())`; invalid endpoint env vars are a build error; 64 MiB body cap; `LogExporter::builder().with_http()` replaces `HttpExporterBuilder`; `WithExportConfig` / `WithHttpConfig` are sealed.
- The HTTP builder reads by itself ([http/mod.rs @ 0.33.1](https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/opentelemetry-otlp-0.33.1/opentelemetry-otlp/src/exporter/http/mod.rs)): `OTEL_EXPORTER_OTLP_{,LOGS_}ENDPOINT`, `_HEADERS`, `_TIMEOUT`, `_COMPRESSION`, `_PROTOCOL`. No certificate or client-certificate variables — TLS must come from the application's own HTTP client. Grimoire measured the crate merging `OTEL_EXPORTER_OTLP_[LOGS_]HEADERS` onto every request (`src/exporter/http/mod.rs:293-298`), which is why its client clears crate-set headers.
- `OTEL_SDK_DISABLED` is not honoured by the SDK (absent from the SDK env list, [sdk lib.rs](https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/main/opentelemetry-sdk/src/lib.rs); open request and PR #3088). The application must check it.
- `http-proto` does not pull `tonic` (`tonic` is behind `grpc-tonic`; `gen-tonic-messages` is messages only) ([Cargo.toml](https://raw.githubusercontent.com/open-telemetry/opentelemetry-rust/opentelemetry-otlp-0.33.1/opentelemetry-otlp/Cargo.toml)). Not verified transitively — run `cargo tree -i tonic` after adding. `default` includes `reqwest-blocking-client`: keep `default-features = false`.
- Not checked here: the exact 0.33.1 signatures of `LogExporter::export`, `HttpClient`, `Resource::builder_empty`. Grimoire compiles against 0.33.1 with them (`grimoire:src/telemetry/export.rs`).
- Context7 (`/open-telemetry/opentelemetry-rust`, 2026-11-18) confirms for the 0.33 line: a custom `HttpClient` is injected with `.with_http_client()`; retries are on by default (up to 3 retries, 4 attempts) and `.with_retry_policy(RetryPolicy::disabled())` opts out, with `opentelemetry_otlp::RetryPolicy` the public path (the `retry` modules are crate-private); the default HTTP client feature is `reqwest-blocking-client`; logs API and SDK are stable with no breaking changes in 0.33 ([release notes](https://github.com/open-telemetry/opentelemetry-rust/blob/main/docs/release_0.33.md), [OTLP design](https://github.com/open-telemetry/opentelemetry-rust/blob/main/docs/design/otlp.md)).
- **Pin: `=0.33.1` for `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-http`**, `default-features = false`, matching Grimoire.

## C. Semantic conventions

From the [attribute registry](https://opentelemetry.io/docs/specs/semconv/registry/attributes/) (registry version not shown on the pages).

| Attribute | Stability | Note |
|---|---|---|
| `cicd.pipeline.name`, `cicd.pipeline.run.id`, `cicd.pipeline.task.name`, `cicd.pipeline.task.run.id`, `cicd.pipeline.run.url.full` | Release Candidate | [cicd](https://opentelemetry.io/docs/specs/semconv/registry/attributes/cicd/); no `cicd.system.name` attribute exists |
| `process.pid`, `process.parent_pid`, `process.exit.code` | Release Candidate | [process](https://opentelemetry.io/docs/specs/semconv/registry/attributes/process/) |
| `host.name`, `host.arch` | Development | [host](https://opentelemetry.io/docs/specs/semconv/registry/attributes/host/); `host.arch` well-known values `amd64`, `arm64`, … |
| `os.type` | Development | [os](https://opentelemetry.io/docs/specs/semconv/registry/attributes/os/); `linux`, `darwin`, `windows` |
| `service.name`, `service.version`, `service.instance.id` | Stable | [service](https://opentelemetry.io/docs/specs/semconv/registry/attributes/service/); instance id a UUID v1/v4 per process, v5 allowed for stability |
| `error.type` | Stable | [error](https://opentelemetry.io/docs/specs/semconv/registry/attributes/error/); low-cardinality class, not set on success |
| `event.name` attribute | Deprecated | [event](https://opentelemetry.io/docs/specs/semconv/registry/attributes/event/); set the LogRecord `EventName` field instead |

- Event names must identify the event structure, carry no dynamic values, and be domain-qualified ([events](https://opentelemetry.io/docs/specs/semconv/general/events/)).
- CI mapping (the tool's choice; the spec defines no mapping):

| Attribute | GitHub Actions | GitLab CI |
|---|---|---|
| `cicd.pipeline.run.id` | `GITHUB_RUN_ID` (attempt `GITHUB_RUN_ATTEMPT` has no `cicd.*` home — not verified) | `CI_PIPELINE_ID` |
| `cicd.pipeline.name` | `GITHUB_WORKFLOW` | none obvious |
| `cicd.pipeline.task.name` | `GITHUB_JOB` (job key) | `CI_JOB_NAME` |
| `cicd.pipeline.task.run.id` | no numeric job-id variable (from knowledge, not fetched) | `CI_JOB_ID` |

## D. Python `opentelemetry-proto`

[PyPI](https://pypi.org/pypi/opentelemetry-proto/json): 1.45.2, requires `protobuf>=5.0,<8.0` (compatible with the locked `protobuf 7.36.1`) and Python `>=3.10`, classifiers through 3.14, Apache-2.0. Generated code only; decodes `ExportLogsServiceRequest` from protobuf bytes, and from the file exporter's JSON through `google.protobuf.json_format` (casing of the collector's JSON not verified).

## Recommendations

- Collector package: core `otelcol` v0.165.0, all six platforms, Apache-2.0. Not on `ocx.sh` today (`ocx --remote index catalog` on 2026-11-18 lists 150 repositories, none matching `otel|collector|telemetry`).
- Read back through the `file` exporter (`format: json`), polling; never parse `debug` output.
- Parallel safety: per-test receiver port and capture path; `service.telemetry.metrics.level: none`, verified by two concurrent collectors.
- Rust: `=0.33.1` on the four crates, `default-features = false`, `RetryPolicy::disabled()`, own HTTP client and TLS roots, own `OTEL_SDK_DISABLED` check; `cargo tree -i tonic` after adding.
- Attribute names live in one constants table: Stable (`service.*`, `error.type`), Release Candidate (`cicd.*`, `process.*`), Development (`host.*`, `os.*`).
- Event name in the LogRecord `EventName` field.
- Python test dependency `opentelemetry-proto==1.45.2`.
