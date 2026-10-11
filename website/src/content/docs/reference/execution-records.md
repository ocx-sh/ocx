---
title: "Execution Records"
---
<!-- doc_type: reference -->

The record OCX writes immediately before it starts a binary, and the `[records]` sink that receives it. How recording is switched on, who may set it and what the fields are for is in [Execution records][execution-records-explanation].

## Record shape {#execution-records-format}

A record describes what OCX was about to run and not what happened, so it has no exit code and no duration. It is written on the code path that resolves the environment and finds the target executable, immediately before the child process starts.

```json
{
  "schemaVersion": "1",
  "kind": "sh.ocx.execution-record",
  "recordedAt": "2026-07-26T14:03:12.006Z",
  "ocx": { "version": "0.5.0", "binary": "/opt/ocx/bin/ocx" },
  "frame": { "command": "package exec", "identity": "complete" },
  "process": {
    "pid": 48124,
    "user": { "id": "1000", "name": "ci" },
    "arch": "amd64",
    "executable": "/home/ci/.ocx/packages/internal.corp.example/sha256/aa/11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899/content/bin/solver",
    "working_directory": "/scratch/job-88213"
  },
  "host": { "name": "batch-node-17" },
  "os": { "type": "linux" },
  "executable": {
    "sh.ocx.provenance": "ocx-package",
    "sh.ocx.kind": "binary",
    "sh.ocx.package": "pkg:oci/solver@sha256:aa11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899?arch=amd64&repository_url=internal.corp.example%2Fsolver"
  },
  "scope": {
    "tier": "package",
    "cleanEnv": true,
    "requested": ["internal.corp.example/solver:2024.3"]
  },
  "resolution": {
    "offline": false,
    "remote": false,
    "frozen": false,
    "requestedPlatform": "linux/amd64+libc.glibc",
    "registries": ["internal.corp.example"],
    "mirrors": {},
    "autoInstalled": ["internal.corp.example/solver"]
  },
  "packages": [
    {
      "name": "solver",
      "uri": "pkg:oci/solver@sha256:aa11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899?arch=amd64&repository_url=internal.corp.example%2Fsolver&tag=2024.3",
      "digest": { "sha256": "aa11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899" },
      "annotations": {
        "sh.ocx.role": "root",
        "sh.ocx.platform": "linux/amd64+libc.glibc",
        "sh.ocx.visibility": "public",
        "sh.ocx.resolved-from": "tag"
      }
    }
  ]
}
```

| Key | Holds |
|---|---|
| `schemaVersion`, `kind`, `recordedAt` | The schema version, the constant `sh.ocx.execution-record` and the UTC time of the write |
| `ocx` | `version` and `binary` of the writing ocx |
| `frame` | `command` that launched the frame, `identity` (`complete` or `degraded`) and, on a degraded frame, `identityNote` |
| `process` | `pid`, `user`, `parent`, `arch`, `executable` and `working_directory` of the process that runs the binary |
| `host`, `os` | `host.name` and `os.type` |
| `executable` | `sh.ocx.provenance`, `sh.ocx.kind` and `sh.ocx.package` of the resolved executable |
| `scope` | `tier`, `cleanEnv`, and the requested identifiers or the project's `projectRoot`, `lock` and `groups` |
| `resolution` | How the closure was resolved: see [Resolution](#execution-records-format-resolution) |
| `packages` | One descriptor per package that composed the child's environment |

- No completion event: on Unix OCX replaces its own process image with the child ([`execvp(2)`][execvp-manpage]), so nothing runs afterward that could write one

### Process {#execution-records-format-process}

`process` describes the process that runs the binary, and three of its fields are best-effort. A field that cannot be determined is omitted and never guessed.

- `process.user.id`: from the kernel, cannot be forged by the caller's environment
- `process.user.name`: read from `$USER`/`$LOGNAME`, can be forged. Omitted with `user` when the host has no passwd entry
- `process.parent`: the pid of the process that launched ocx, when it is identifiable. Omitted for a bare shell invocation
- `process.arch`: the architecture of the running ocx binary, in [OTel][otel-host]'s `host.arch` vocabulary
- `process.executable`: the resolved executable, as a flat string
- `process.args`: absent by design, command-line arguments are never recorded

### Executable {#execution-records-format-executable}

`executable` describes the resolved executable, and `sh.ocx.*` is the namespace for everything OCX-specific. `sh.ocx.provenance` and `sh.ocx.kind` are present whenever the executable lives inside the OCX store, root or dependency alike.

- `sh.ocx.provenance`: `ocx-package` for an executable inside the store, a shim target included. `external` when the executable was not supplied by ocx
- `sh.ocx.kind`: `binary` for the leaf executable, `launcher` when the resolved path is an entrypoint launcher, `shim` when nothing has been downloaded yet
- `sh.ocx.package`: the purl of a root package. Omitted when the executable comes from one of that root's dependencies, whose identity is recorded in full in `packages[]` with `sh.ocx.role: "dependency"`

### Resolution {#execution-records-format-resolution}

`resolution` records how the package closure was resolved. Optional fields follow one rule: state a fact, never assert its absence.

```json
{
  "resolution": {
    "mirrors": {
      "docker.io": { "registry": "https://mirror.corp.example" },
      "ghcr.io": { "index": "https://index-mirror.corp.example" }
    }
  }
}
```

- `offline`, `remote`: whether `--offline` and `--remote` were in force
- `requestedPlatform`: what OCX asked resolution to find, in its own `os/arch[+libc.*]` grammar. The request and not the outcome
- `registries`: the content registries the frame's resolved packages carry, not the index endpoint a version was looked up through. A frame that fetched nothing names a host only when this machine's index copy holds its routing pointer
- `mirrors`: per-host map keyed by the upstream host a [`[mirrors]`][config-mirrors] entry rewrites. `registry` covers OCI blob and manifest traffic, `index` covers index-tree lookups, and a host declaring both carries both keys
- `mirrors` shape: only rewritten hosts appear. `{}` when `[mirrors]` is not configured. Omitted at the launcher frame. Any `user:password@` userinfo is stripped from both endpoints
- `insecureRegistries`: the hosts of `registries` declared plain-HTTP for this resolution, through an [`insecure = true`][config-registries-insecure] entry or [`OCX_INSECURE_REGISTRIES`][env-ocx-insecure-registries]. Absent when none were
- `frozen`: mirrors [`--frozen`][arg-frozen], which freezes package-tier resolution only. `true` means the package closure could not have added a new pin during this run. The patch tier is frozen separately by [`ocx patch freeze`][cmd-patch-freeze] and `OCX_PATCH_SNAPSHOT`
- `noVerify`: `true` when [`OCX_NO_VERIFY`][env-ocx-no-verify] disabled the install-time signature gate for this invocation. Env-tier only, a posture and never an outcome. `false` says nothing disabled the gate. Whether a package triggered it is an install-time fact this record does not carry. The per-command `--no-verify` flag is never recorded
- `managedConfig`: the source of the managed-config tier in force and its applied digest when known. Absent when no managed tier applies
- `autoInstalled`: the packages this invocation materialized on the spot. Depends on cache state, so the same command reports it only the first time
- `patchSnapshot`: see [Patch companions](#execution-records-patch)

### Scope {#execution-records-format-scope}

`scope.tier` is `package` for an OCI-tier command, `project` for a project-tier `ocx exec` and `launcher` for a launcher re-entry, which carries no other scope field. A project-tier record also carries `lock` and the `ocx.toml` binding and group each package was selected under.

```json
{
  "scope": {
    "tier": "project",
    "cleanEnv": false,
    "projectRoot": "/scratch/job-88213",
    "lock": {
      "path": "/scratch/job-88213/ocx.lock",
      "declarationDigest": { "sha256": "9c1f0b3a77d2e4518ab6c0f92d3e7a41b8c5d6e0f1a2b3c4d5e6f708192a3b4c" }
    },
    "groups": ["default"]
  }
}
```

- `lock.declarationDigest`: hashes the `ocx.toml` declarations the lock was generated from and not the lock file's contents. It answers "was this run driven by the `ocx.toml` I expect" and never "what actually ran"
- `sh.ocx.binding`: the `ocx.toml` binding the package was selected under, on package annotations only
- `sh.ocx.group`: the `[group.<name>]` that supplied it, `default` for the top-level `[tools]` table

### Packages {#execution-records-format-packages}

`packages[]` is field-for-field an [in-toto][in-toto-attestation] `ResourceDescriptor` (`name`, `uri`, `digest`, `annotations`), so it drops into an [SLSA][slsa] `resolvedDependencies` list without reshaping.

```json
{
  "name": "cmake",
  "uri": "pkg:oci/cmake@sha256:3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0?arch=amd64&repository_url=index.ocx.sh%2Focx%2Fcmake",
  "digest": { "sha256": "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0" },
  "annotations": {
    "sh.ocx.role": "root",
    "sh.ocx.binding": "cmake",
    "sh.ocx.group": "default",
    "sh.ocx.platform": "linux/amd64+libc.glibc",
    "sh.ocx.visibility": "public",
    "sh.ocx.entrypoints": ["cmake", "ctest", "cpack"]
  }
}
```

- `uri`: a [purl][purl-spec] of type `pkg:oci` whose version is the sha256 digest. A `tag` qualifier appears only when one was resolved
- `digest`: a bare lowercase-hex map keyed by algorithm, never a `sha256:`-prefixed string. The manifest digest is the join key to [`ocx package sbom`][cmd-package-sbom] and [`ocx package verify --attestation`][cmd-package-verify-attestations]
- `sh.ocx.role`: `root`, `dependency` or `companion`
- `sh.ocx.platform`: roots only, the platform the manifest leaf actually resolved to. A flat single-image package selects `any` under any request. A dependency's descriptor and `uri` carry no platform and no arch qualifier
- `sh.ocx.visibility`: the surface the package was composed on
- `sh.ocx.resolved-from`: `tag` when the digest was reached through a tag rather than named directly. Absent for a digest-pinned identifier, a lock-resolved package and a `launcher shim` frame
- `sh.ocx.binaries`, `sh.ocx.entrypoints`: arrays of strings, one per binary or entry point admitted onto `PATH`, present only when the package declares at least one. These are the same claim [`ocx package env`'s arrays][cmd-package-env] expose
- `sh.ocx.composition`: `deferred` on a package that composed but never materialized
- `sh.ocx.identity`: `synthetic` on a degraded frame, which also carries no `uri`

### Where field names come from {#execution-records-format-sources}

Every borrowed field name is pinned by this page and not by the standard it came from. An upstream rename therefore does not change what OCX emits.

| Record part | Source | Notes |
|---|---|---|
| `packages[]` shape (`name`, `uri`, `digest`, `annotations`) | [in-toto][in-toto-attestation] `ResourceDescriptor` | [Sigstore][sigstore], [Tekton Chains][tekton-chains] and [GUAC][guac] all consume in-toto shapes |
| `packages[].uri` | [purl][purl-spec], `pkg:oci` type | Version is the sha256 digest; `tag` is advisory, matching OCX's digest-is-identity model |
| `process.pid`, `process.working_directory`, `host.name` | [ECS][ecs-process] and [OTel][otel-host] agree | Both standards name these fields the same way |
| `os.type` | [OTel][otel-host] | OTel's top-level attribute, member spelling `darwin`/`linux`/`windows`/…; [ECS][ecs-os] nests the field under `host.os.type` and expects `macos`, not `darwin` |
| `process.parent.pid`, `process.user.name`, `process.executable` | [ECS][ecs-process] | ECS types `process.executable` as a flat string; OTel's object form would collide with the `sh.ocx.*` fields nested under it |
| `process.arch` | [OTel][otel-host] | An OCX extension inside ECS's `process` block, which has no process-level architecture field, carrying OTel's `host.arch` value vocabulary (`amd64`, `arm32`, `arm64`, …) |
| `schemaVersion`, `recordedAt`, `frame`, `scope`, `resolution` | OCX | No existing standard describes "a CLI invoked a binary with these packages" |
| `sh.ocx.*` annotations (`role`, `binding`, `group`, `platform`, `visibility`, `kind`, `provenance`, `resolved-from`, `binaries`, `entrypoints`, `composition`, `identity`) | OCX | Everything OCX-specific lives in this namespace so the standard-shaped fields stay standard-shaped |


## Frames {#execution-records-frames}

An entrypoint launcher re-enters OCX, so one invocation through a launcher writes two records, one per frame. Both carry the same package content digest, which is the join key.

| | Outer frame (`exec` / `package exec`) | Inner frame (`launcher exec`) |
|---|---|---|
| `frame.command` | `exec` or `package exec` | `launcher exec` |
| `frame.identity` | `complete` | `complete` when the parent forwarded the package name, otherwise `degraded` |
| `executable["sh.ocx.kind"]` | `launcher` — the resolved path is the launcher shim | `binary` — the actual leaf executable |
| `packages[0].digest.sha256` | same content digest as the inner frame | same content digest as the outer frame |

- Sink: [`ocx launcher exec`][cmd-launcher-exec] takes no `--records-*` flags and inherits the sink through the forwarded `OCX_RECORDS_DIR` and `OCX_RECORDS_NAME`

See [Two records per launcher invocation][execution-records-explanation-frames] for how the frames join and count. [Launcher frames in records][records-launcher-frames] covers degraded frames and why previews write no record.

### Shim frame {#execution-records-shim-frame}

A package composed with `lazy-mode = always` puts a generated shim on `PATH`, and the hidden [`ocx launcher shim`][in-depth-lazy-loading-materialize] frame is the only place its content is downloaded. The first invocation of a [deferred package][in-depth-lazy-loading] therefore writes two records.

| | Outer frame (`exec` / `package exec`) | Shim frame (`launcher shim`) |
|---|---|---|
| `frame.identity` | `complete` | `complete`, always, unlike `launcher exec` |
| `executable["sh.ocx.kind"]` | `shim` — nothing has downloaded yet | `binary` — the now-materialized leaf executable |
| `packages[].annotations["sh.ocx.composition"]` | `deferred`, on the package that composed but never materialized | absent — the package is real by the time this frame writes |
| `resolution.autoInstalled` | absent for this package — the outer frame only composed it | present — this frame performed the pull |


### Patch companions {#execution-records-patch}

A [patch companion][patches-how] appears in `packages[]` as an ordinary entry with `sh.ocx.role: "companion"`, beside `"root"` and `"dependency"`. `resolution.patchSnapshot` names the patch snapshot pinned for the invocation when one is in force.

```json
{
  "resolution": {
    "patchSnapshot": { "sha256": "e2f4a6b8c0d1e3f5a7b9c1d3e5f7092c4d6e8f0a2b4c6d8e0f2a4c6e8f0a2c4e" }
  }
}
```

- `sh.ocx.visibility`: the surface the companion was composed on. `private` under `--self` when it matched only roots. `interface` when it matched only dependencies or the view is a consumer's. `public` under `--self` when it matched both a root and a dependency
- Dependencies: a companion's own dependencies are recorded with `sh.ocx.role: "dependency"`, and a package the roots or an earlier entry already reached keeps its first position and role
- `patchSnapshot`: absent, never null or empty, when no snapshot is in force. Set by [`ocx patch freeze`][cmd-patch-freeze] or [`OCX_PATCH_SNAPSHOT`][env-ocx-patch-snapshot]

## The sink {#execution-records-sink}

The sink is always a directory. OCX writes one self-named file per record into it with a create-exclusive temp file and a no-clobber rename.

- Collision: a naming collision retries under a fresh random filename and never overwrites an existing record
- Creation: OCX does not create the directory. `dir` must already exist and be writable. A missing directory fails like a permissions problem, with an I/O error naming the path
- Durability: publication is atomic but not synced to disk, so a power loss can lose the most recent records
- Network sinks: a collector on another client can briefly observe a short, still-being-written file. It should retry before treating a JSON parse failure as corruption
- Mode: each file is owner-only (`0600` on Unix), so a collector under a different account cannot read it without explicit access
- Debris: a crash between publish and cleanup can strand a hidden `.tmp*` file. A collector that lists the directory rather than globbing `*.json` must skip every `.tmp*` entry
- Authenticity: a record is not signed and the sink is not authenticated. On a shared sink a collector should reject a record whose owning uid disagrees with `process.user.id` and treat `process.user.name` as display only
- Symlink pin: the configured sink is resolved to its real location once per invocation and re-checked before every write. A sink that no longer resolves to itself refuses the write with the configured failure posture. The posture is a warning by default or exit `78` under `required = true`, classified as a configuration fault
- Limits of the pin: it compares a canonical path and not directory identity. It is not shared across the outer and inner frame of an entrypoint invocation. It is an integrity control and not a security boundary
- Paths: records contain absolute paths, including the invoking user's home directory (`process.executable` and the package `content/` and `entrypoints/` paths), so size sink permissions and retention accordingly

See [What the sink guarantees][execution-records-explanation-sink].

## Filename grammar {#execution-records-filename}

The default filename is `<utc-basic-ms>-<pid>-<8 hex random>.json`, which sorts lexicographically in chronological order. A `name` template over a closed placeholder set overrides it.

| Placeholder | Expands to |
|---|---|
| `{time}` | `20260726T140311482Z` — same basic UTC form as the default |
| `{pid}` | the process ID of the process that runs the binary |
| `{rand}` | 8 hex characters |
| `{host}` | hostname, reduced to a filename-safe form |

- `{host}` reduction: keeps only `A-Z a-z 0-9 . _ -` and replaces everything else with `_`. A hostname that reduces to `.` or `..` expands to nothing, exactly as an undeterminable hostname does
- No `{command}` placeholder: the invoked binary is already in the record as `process.executable`
- Errors: an unknown placeholder is a configuration error at resolve time. So is a template that renders to anything other than a single plain filename, such as one carrying a path separator

## Configuring `[records]` {#execution-records-configuration}

Recording is off by default: absent a sink directory at every layer, OCX writes nothing and adds no I/O to the exec path.

```toml
[records]
dir      = "/var/log/ocx/records"
name     = "{time}-{host}-{pid}.json"
required = true
```

| Key | Flag | Variable | Notes |
|---|---|---|---|
| `dir` | `--records-dir` | [`OCX_RECORDS_DIR`][env-ocx-records-dir] | The sink directory |
| `name` | `--records-name` | [`OCX_RECORDS_NAME`][env-ocx-records-name] | The filename template; no effect unless a sink is active |
| `required` | none | none | Config-file-only at every tier |

- Precedence: [`[records]`][config-records] config, then the variables, then the flags on [`ocx exec`][cmd-exec] or [`ocx package exec`][cmd-package-exec]. The highest layer wins per field
- Empty variable: reads as unset, so a profile-exported value can be disabled for one invocation
- Forwarding: both variables are resolution-affecting and forwarded to every subprocess
- SYSTEM lock: a SYSTEM-scope `[records]` declaration locks the whole block, `dir`, `name` and `required` together, and the variables and flags have no effect once it is locked
- Lock survival: the lock survives [`OCX_NO_CONFIG=1`][env-ocx-no-config], and an unreadable `/etc/ocx/config.toml` aborts the invocation with exit `78`. An absent system file is silent
- Relative `dir` in a config file: anchored at the declaring file's directory, like [`[trust.sigstore] trusted_root`][config-trust-sigstore]. `--records-dir` and `OCX_RECORDS_DIR` resolve against the process's working directory
- Project tier: a project `ocx.toml` cannot set `[records]`. The section is stripped
- Managed tier: an operator-published [`[managed]`][config-managed] configuration can set `required = true` in its `[records]` payload
- Placeholder error: an unrecognized placeholder in `name` is a configuration error (exit `78`) at resolve time
- `required` without `dir`: a configuration error (exit `78`), except a SYSTEM-scope `[records]` block with neither `dir` nor `required`, which locks recording off for the host

| `required` | Default | Behavior when a record cannot be written |
|---|---|---|
| `true` | when a SYSTEM-scope (`/etc/ocx/config.toml`) `[records]` policy is present | The invocation aborts — exit `74` for an unwritable sink, exit `78` when the sink resolves through a symlink. |
| `false` | when no SYSTEM-scope policy is present | A warning prints to stderr; the child still runs. |

- Exit codes: `required` decides only whether OCX aborts or warns, never which code an abort uses
- Windows: the sink is probed before the child is spawned and the record written after, so `required = true` can abort after the child has begun work. OCX stops and reaps that child

See [Failure posture in detail][execution-records-explanation-required] and [`[records]`][config-records] in the Configuration reference.

## Schema {#execution-records-schema}

The published schema lives at `https://ocx.sh/schemas/execution-record/v1.json` and moves in lockstep with the in-band `schemaVersion` string inside every record. `schemaVersion` changes only for a backward-incompatible break, and new fields are additive.

- Consumers: a consumer written against `schemaVersion: "1"` must tolerate unknown keys and not reject them
- Breaking change: bumps the schema version and `schemaVersion` together, never one without the other

<!-- external -->
[purl-spec]: https://github.com/package-url/purl-spec
[in-toto-attestation]: https://github.com/in-toto/attestation
[slsa]: https://slsa.dev/spec/v1.0/provenance
[sigstore]: https://www.sigstore.dev/
[tekton-chains]: https://tekton.dev/docs/chains/
[guac]: https://github.com/guacsec/guac
[ecs-process]: https://www.elastic.co/docs/reference/ecs/ecs-process
[ecs-os]: https://www.elastic.co/docs/reference/ecs/ecs-os
[otel-host]: https://opentelemetry.io/docs/specs/semconv/registry/attributes/host/
[execvp-manpage]: https://man7.org/linux/man-pages/man3/execvp.3.html

<!-- commands -->
[cmd-exec]: ./command-line/commands.md#exec
[cmd-package-exec]: ./command-line/package-consuming.md#package-exec
[cmd-package-env]: ./command-line/package-consuming.md#package-env
[cmd-launcher-exec]: ../explanation/launcher-forwarding.md#launcher-embedding
[cmd-patch-freeze]: ./command-line/patch.md#patch-freeze
[cmd-package-sbom]: ./command-line/package-consuming.md#package-sbom
[cmd-package-verify-attestations]: ../publishing/attest-and-sbom.mdx#attestations-verification
[arg-frozen]: ./command-line/index.md#arg-frozen

<!-- configuration -->
[config-records]: ./configuration.md#keys-records
[config-mirrors]: ./configuration.md#keys-mirrors
[config-managed]: ./configuration.md#keys-managed
[config-trust-sigstore]: ./configuration.md#keys-trust-sigstore
[config-registries-insecure]: ./configuration.md#keys-registries-insecure

<!-- environment -->
[env-ocx-records-dir]: ./environment.md#ocx-records-dir
[env-ocx-records-name]: ./environment.md#ocx-records-name
[env-ocx-no-config]: ./environment.md#ocx-no-config
[env-ocx-no-verify]: ./environment.md#ocx-no-verify
[env-ocx-insecure-registries]: ./environment.md#ocx-insecure-registries
[env-ocx-patch-snapshot]: ./environment.md#ocx-patch-snapshot

<!-- internal -->
[execution-records-explanation]: ../explanation/execution-records.md
[execution-records-explanation-frames]: ../explanation/execution-records.md#frames
[records-launcher-frames]: ../explanation/records-launcher-frames.md
[execution-records-explanation-sink]: ../explanation/execution-records.md#sink-guarantees
[execution-records-explanation-required]: ../explanation/execution-records.md#required-details
[patches-how]: ../explanation/patches.mdx#patches-how
[in-depth-lazy-loading]: ../explanation/deferred-packages.mdx
[in-depth-lazy-loading-materialize]: ../explanation/deferred-packages.mdx#deferred-tools-materialize
