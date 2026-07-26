# Vendored spec data

Subsets of upstream ECS / OpenTelemetry semantic-conventions schema files,
vendored for `test/tests/test_execution_record_standards.py` to validate the
execution record's `process.*`/`host.*`/`os.*` fields against the standards'
own field definitions — never a hand-rolled restatement of them.

Every file here is a **subset**, not a full copy of the upstream file
(`ecs_flat.yml` alone is too large to fetch through the available tooling).
Fetched via `WebFetch` on 2026-09-04 by the session lead (this agent's tool
set has no network access), vendored verbatim by this agent.

**The sha256 below is of the vendored subset file in this repo, not of the
upstream file** — vendoring a subset means the two never match, and this
column is a change-detection tripwire for the vendored copy, not a spec
integrity proof against upstream.

| File | Upstream | Tag | sha256 (vendored subset) |
|---|---|---|---|
| `ecs/os.subset.yml` | [elastic/ecs `schemas/os.yml`](https://raw.githubusercontent.com/elastic/ecs/v9.1.0/schemas/os.yml) | `v9.1.0` | `2a2cba24fa71c875d83fd3d11152e909b49a73da7280a3d6da2c52aed700f98a` |
| `ecs/process.subset.yml` | [elastic/ecs `schemas/process.yml`](https://raw.githubusercontent.com/elastic/ecs/v9.1.0/schemas/process.yml) | `v9.1.0` | `1bcaab07feb54b43a0faa29c35988315411bc0637a3db696ffce7a771465ef4d` |
| `ecs/user.subset.yml` | [elastic/ecs `schemas/user.yml`](https://raw.githubusercontent.com/elastic/ecs/v9.1.0/schemas/user.yml) | `v9.1.0` | `ecf8c910a54a52357a2d1720ae0860376a1d5b52aee9dc713b0b89fb25b8b2d8` |
| `ecs/host.subset.yml` | [elastic/ecs `schemas/host.yml`](https://raw.githubusercontent.com/elastic/ecs/v9.1.0/schemas/host.yml) | `v9.1.0` | `dfc46630c3754eb9e8c8a674a4033b0680763cb4dc07b5f83aa51c88cacb2eb3` |
| `otel/host.subset.yml` | [open-telemetry/semantic-conventions `model/host/registry.yaml`](https://raw.githubusercontent.com/open-telemetry/semantic-conventions/v1.36.0/model/host/registry.yaml) (`host.arch` entry only) | `v1.36.0` | `669ea1a338a044aaa2749e6476f927713c9971bac3d19eb2cf32a8bdb08138bb` |
| `otel/os.subset.yml` | [open-telemetry/semantic-conventions `model/os/registry.yaml`](https://raw.githubusercontent.com/open-telemetry/semantic-conventions/v1.36.0/model/os/registry.yaml) (`os.type` entry only) | `v1.36.0` | `a8ce314a995b82385f7d626c35538bb707c20bb9f6ade0100ff1dec4fe86effa` |

## `os.type`: OTel, not ECS

ECS's `schemas/os.yml` documents `os.type` as reused under `host.os.type`
(`reusable.top_level: false`) — never top-level — and its own metadata flags
a values conflict against OTel's `os.type`
(`otel: [{relation: conflict, ...}]`, see `ecs/os.subset.yml`). The record
emits a top-level `os.type` using OTel/GOOS spelling (`darwin`, not ECS's
`macos`), so it is checked against `otel/os.subset.yml`'s enum instead —
never ECS.

## Regenerating the sha256 column

```sh
cd test/specs && sha256sum ecs/*.yml otel/*.yml
```
