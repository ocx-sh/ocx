---
title: "Usage telemetry"
description: "The records, variables and decision tables behind OCX usage telemetry export."
---
<!-- doc_type: reference -->

What an armed OCX command sends to an OTLP collector, which variables and settings arm it, and which inputs stop it. How to switch it on and where it fits next to execution records is in [Send usage telemetry][guide-telemetry].

## Fields {#fields}

One armed command sends one OTLP/HTTP protobuf logs request with a resource and its records. Presence is **always**, **best-effort** (omitted when undeterminable, never a placeholder) or the named condition.

### Resource {#fields-resource}

| Key | Type | Presence | Origin | Value |
|---|---|---|---|---|
| `service.name` | string | always | OTel Stable | `ocx`, unless an operator override applies |
| `service.version` | string | always | OTel Stable | the version `ocx version` prints |
| `os.type` | string | best-effort | OTel (Development), execution records | `linux`, `darwin`, `windows` |
| `process.arch` | string | best-effort | execution records, OTel `host.arch` vocabulary | architecture of the running `ocx` binary, OCI spelling (`amd64`, `arm64`) |
| `host.name` | string | best-effort | OTel (Development) and ECS, execution records | the host name |
| `process.pid` | int | always | OTel RC and ECS | this `ocx` process |
| `process.parent.pid` | int | best-effort; omitted on Windows | ECS, execution records | the parent process |
| `sh.ocx.installation.id` | string | when the id file is readable or creatable | OCX | lowercase hyphenated UUID v4, kept in `$OCX_HOME/state/telemetry/installation-id`. Delete the file to [reset it][guide-telemetry-installation-id] |
| `sh.ocx.interactive` | bool | always | OCX | stderr is a terminal |
| `sh.ocx.ci.provider` | string | in CI only | OCX | `github` (`GITHUB_ACTIONS` truthy), else `gitlab` (`GITLAB_CI` truthy), else `other` (`CI` truthy) |
| `cicd.pipeline.run.id` | string | GitHub, GitLab; non-empty source | OTel RC | `GITHUB_RUN_ID` or `CI_PIPELINE_ID` |
| `cicd.pipeline.task.name` | string | GitHub, GitLab; non-empty source | OTel RC | `GITHUB_JOB` or `CI_JOB_NAME` |
| `cicd.pipeline.task.run.id` | string | GitLab; non-empty source | OTel RC | `CI_JOB_ID` |
| *(operator keys)* | string | when set | `OTEL_RESOURCE_ATTRIBUTES`, `resource_attributes` | any |

The instrumentation scope is named `ocx` and carries the same value as `service.version`.

### Command record {#fields-command}

Event name `sh.ocx.command`. Exactly one per request.

| Key | Type | Presence | Origin | Value |
|---|---|---|---|---|
| `sh.ocx.command.name` | string | always | OCX | the command path joined by spaces, such as `package install` or `direnv init` |
| `sh.ocx.command.duration` | double, seconds | always | OCX | from entry into the command to the outcome being known |
| `process.exit.code` | int | always | OTel RC | the process exit code |
| `error.type` | string | when the command returned an error | OTel Stable | that error's `error.detail` slug, `_OTHER` when none classified |

### Package record {#fields-package}

Event name `sh.ocx.package`. One per package the command installed, pulled or removed, with a logical identity.

| Key | Type | Presence | Origin | Value |
|---|---|---|---|---|
| `sh.ocx.package` | string | always | execution records | the `pkg:oci` purl, as plain text |
| `sh.ocx.package.action` | string | always | OCX | `installed`, `pulled`, `removed` |
| `sh.ocx.role` | string | always | execution records | `root`, `dependency` (`removed` is always `root`) |
| `sh.ocx.package.store_hit` | bool | `installed`, `pulled` | OCX | true when the package was already in the store |
| `sh.ocx.package.download.size` | int, bytes | when `store_hit` is false | OCX | layer bytes this package fetched, 0 when none |
| `sh.ocx.package.resolve.duration` | double, s | `installed`, `pulled` | OCX | time spent resolving the package |
| `sh.ocx.package.download.duration` | double, s | when this package fetched at least one layer | OCX | layer phase wall time: fetch, decompress and unpack as one stream |
| `sh.ocx.package.extract.duration` | double, s | when `store_hit` is false | OCX | assembly into the store |

Every record has severity INFO, its event name set, no body and one shared timestamp.

## Names that differ from OTel {#otel-names}

Two keys follow the execution record spelling instead of the OTel one, and one OTel key is left out on purpose.

| Key | Difference |
|---|---|
| `process.parent.pid` | OTel names it `process.parent_pid` |
| `process.arch` | OTel names the same fact `host.arch`, and the value vocabulary is the same |
| `service.instance.id` | absent: `sh.ocx.installation.id` identifies an installation, not an instance |

## Never collected {#never-collected}

No record and no resource carries `telemetry.sdk.*`, argv, the working directory, a user name or id, environment variables, file paths or registry credentials.

## Arming {#arming}

The first matching row decides. Rows 2 to 5 and 6 to 7 beat every config file and every system lock.

| # | Condition | Result |
|---|---|---|
| 1 | The command is in the never-send set, a plugin, or runs under the in-process test seam | never, nothing is read |
| 2 | `DO_NOT_TRACK` truthy | off |
| 3 | `OTEL_SDK_DISABLED` truthy | off |
| 4 | `OTEL_LOGS_EXPORTER` is `none`, any case | off |
| 5 | `--offline` or `OCX_OFFLINE` | off |
| 6 | `OCX_TELEMETRY` falsy | off |
| 7 | `OCX_TELEMETRY` unrecognized or not UTF-8 | off, one warning naming the variable |
| 8 | The merged `[telemetry]` section is unusable | off. One warning naming the origin and the key when the run asked for export (`OCX_TELEMETRY` truthy, merged `enabled = true`, or the invalid key is `enabled`), else silent |
| 9 | The merged section is system-locked with `enabled = false` | off |
| 10 | `OCX_TELEMETRY` truthy | armed |
| 11 | `OCX_TELEMETRY` unset or empty and merged `enabled = true` | armed |
| 12 | otherwise | off |

Truthy is `1`, `y`, `yes`, `on` or `true`, falsy is `0`, `n`, `no`, `off` or `false`, both ASCII case-insensitive.

An unrecognized `DO_NOT_TRACK` value is not an opt-out. An unrecognized `OTEL_SDK_DISABLED` value is not an opt-out either, and an armed run prints one warning.

## Destination {#destination}

An armed run picks one destination, and endpoint, headers, timeout and protocol never mix between rows. The default timeout is 500 ms and the only protocol is `http/protobuf`.

| # | Condition | Endpoint | Headers sent |
|---|---|---|---|
| D1 | merged `[telemetry].endpoint` set | the endpoint path plus `/v1/logs` | merged `[telemetry].headers`, then `OCX_TELEMETRY_HEADERS` per name |
| D2 | else `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` set | used as given | `OTEL_EXPORTER_OTLP_LOGS_HEADERS` |
| D3 | else `OTEL_EXPORTER_OTLP_ENDPOINT` set | the endpoint path plus `/v1/logs` | `OTEL_EXPORTER_OTLP_LOGS_HEADERS` if set, else `OTEL_EXPORTER_OTLP_HEADERS` |
| D4 | else | none: nothing sent and no installation id written | none |

A link-local destination is refused, whether it is written as an address or reached through a name that resolves to one. The cloud metadata addresses `fd00:ec2::254` and `100.100.100.200` are refused the same way.

Behind an HTTP proxy the proxy resolves the endpoint name, so only a literal address is refused, never a resolved one.

The refused header-name list is intentionally minimal. It blocks framing, hop-by-hop and transport-set names such as `host` and `user-agent`.

D1 reads no `OTEL_EXPORTER_OTLP_*` variable, while D2 and D3 take timeout and protocol from them. Under D2 and D3 neither `[telemetry].headers` nor `OCX_TELEMETRY_HEADERS` is sent, and one warning names both when either is set.

`OTEL_EXPORTER_OTLP_HEADERS` is read only under D3 with no `OTEL_EXPORTER_OTLP_LOGS_HEADERS` set.

Under D2, and under D3 with `OTEL_EXPORTER_OTLP_LOGS_HEADERS` set, it is ignored and one warning names `OTEL_EXPORTER_OTLP_LOGS_HEADERS` as the place to move the headers.

Under D1 and D4 it is ignored without a warning. Each warning ends with the step that clears it.

## Resource precedence {#resource-precedence}

Per key, a later row replaces an earlier one, so an operator value overrides a detected one.

| # | Source |
|---|---|
| R1 | the derived keys in [Resource](#fields-resource) |
| R2 | `OTEL_RESOURCE_ATTRIBUTES`, `k=v` pairs, percent-decoded, malformed pairs skipped |
| R3 | `OTEL_SERVICE_NAME`, when non-empty, as `service.name` |
| R4 | merged `[telemetry].resource_attributes` |
| R5 | merged `[telemetry].service_name`, as `service.name` |

## Variables {#variables}

Each variable is declared in the [environment reference][environment-reference], and the settings in the [`[telemetry]`][config-telemetry] section carry the same inputs.

### Honoured {#variables-honoured}

| Variable | Role |
|---|---|
| [`OCX_TELEMETRY`][env-ocx-telemetry] | The switch. Arms on a truthy value, disables on a falsy or unrecognized one |
| [`OCX_TELEMETRY_HEADERS`][env-ocx-telemetry-headers] | `name=value` pairs for the OCX destination, secret, removed from every child process |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | The destination when no `[telemetry].endpoint` is set |
| `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_LOGS_HEADERS` | Headers for those destinations |
| `OTEL_EXPORTER_OTLP_TIMEOUT`, `OTEL_EXPORTER_OTLP_LOGS_TIMEOUT` | Timeout in whole milliseconds from 1 to 10000. Any other value falls back to 500 ms with one warning on an armed run |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL` | Only `http/protobuf` is accepted |
| `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES` | Resource overrides, see [Resource precedence](#resource-precedence) |
| `GITHUB_ACTIONS`, `GITHUB_RUN_ID`, `GITHUB_JOB`, `GITLAB_CI`, `CI_PIPELINE_ID`, `CI_JOB_ID`, `CI_JOB_NAME`, `CI` | Read to fill the CI keys in [Resource](#fields-resource) |

None of the `OTEL_EXPORTER_OTLP_*` variables arms export on its own.

The export runs under one deadline, the timeout.

The resource probe (host name, environment, terminal), the installation id and the proxy host lookup run inside it.

The export adds at most the timeout to a command, whatever stalls.

### Opt-out {#variables-opt-out}

| Variable | Effect |
|---|---|
| [`DO_NOT_TRACK`][dnt] | Off when truthy |
| `OTEL_SDK_DISABLED` | Off when truthy |
| `OTEL_LOGS_EXPORTER` | Off when `none`. Any other value is ignored with a debug line |
| [`OCX_OFFLINE`][env-ocx-offline] and `--offline` | Off |
| `OCX_TELEMETRY` | Off when falsy or unrecognized |

### Unsupported {#variables-unsupported}

OCX never compresses the request, and TLS client authentication is not available.

| Variable | Treatment |
|---|---|
| `OTEL_EXPORTER_OTLP_CERTIFICATE`, `OTEL_EXPORTER_OTLP_LOGS_CERTIFICATE` | Not honoured. Extra trust roots come from [`OCX_EXTRA_CA_CERTS`][env-ocx-extra-ca-certs] |
| `OTEL_EXPORTER_OTLP_CLIENT_KEY`, `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE` and the `_LOGS_` forms | Not honoured |
| `OTEL_EXPORTER_OTLP_COMPRESSION`, `OTEL_EXPORTER_OTLP_LOGS_COMPRESSION` | Not honoured, the request is never compressed |

## Commands that send nothing {#never-send}

These commands never read the telemetry variables, never touch `state/telemetry/` and never open a connection.

| Commands | Reason |
|---|---|
| `exec`, `package exec`, `launcher exec`, `launcher shim` | A launch is covered by [execution records][execution-records-reference] |
| `shell`, `shell state`, `direnv export` | Interactive or per-prompt |
| `self activate`, `shell completion`, `version` | Shell startup and static paths |
| `env --shell`, `package env --shell` | A login shell runs them at every start |
| `status`, `config test` | Their contracts promise no network |

Help, `--version`, usage errors and `ocx-<name>` plugins send nothing either. Every other command sends, `direnv init` and `package test` included.

See [Send usage telemetry][guide-telemetry] for the settings that arm export.

<!-- external -->
[dnt]: https://consoledonottrack.com/

<!-- environment -->
[environment-reference]: ./environment.md
[env-ocx-telemetry]: ./environment.md#ocx-telemetry
[env-ocx-telemetry-headers]: ./environment.md#ocx-telemetry-headers
[env-ocx-offline]: ./environment.md#ocx-offline
[env-ocx-extra-ca-certs]: ./environment.md#ocx-extra-ca-certs

<!-- configuration -->
[config-telemetry]: ./configuration.md#keys-telemetry

<!-- internal -->
[guide-telemetry]: ../guides/telemetry.mdx
[guide-telemetry-installation-id]: ../guides/telemetry.mdx#telemetry-installation-id
[execution-records-reference]: ./execution-records.md
