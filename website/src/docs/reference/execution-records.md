---
layout: doc
outline: deep
---

# Execution Records

A team running a batch cluster needs to prove, after the fact, that a job's output came from an *approved* package version. [`ocx.lock`][cmd-lock] pins tag-to-digest resolution for the project tier, but that pin is checked at `pull` time — and between a pull and the exec that follows it, config can change, a floating tag can move, or [`ocx package exec`][cmd-package-exec] can auto-install a package no prior pull ever saw. On the OCI tier there is no lock at all. The gap is real and widest in exactly this configuration: no `ocx.toml`, no lock, a bare `package exec` against a tag.

Today, closing that gap means a wrapper script that logs a resolved digest next to whatever a job produces — and the wrapper is the actual control point: it has to be trusted by every caller, which is the status quo teams are trying to leave.

`[records]` closes it a different way. When configured, OCX writes one JSON file to an operator-designated directory immediately before it starts a tool, naming every package digest that composed the child's environment plus the resolved executable that is about to run. No caller can opt out of a sink the operator has locked at system scope — not through configuration, and not through the [maintainer-preview exemption][execution-records-frames], which a fail-closed policy refuses rather than grants. No wrapper script has to know the mechanism exists.

## A policy check that works today {#execution-records-consumer}

The realistic first consumer of a record is not an attestation pipeline — it is a policy check over raw JSON. [Conftest][conftest], built on [Open Policy Agent][opa]'s [Rego][rego] language, asks nothing of OCX: no signing key, no registry, no recognized predicate type. It reads the file as it is written.

```rego
package ocx.records

# Fail the invocation record if any package is not on the approved list.
approved := {
  "sha256:1f4b7c9a2e6d0f83b5c8e1a4d7f0b3c6e9a2d5f8b1c4e7a0d3f6b9c2e5a8d1f4",
  "sha256:8c1e4a7d0f3b6c9e2a5d8f1b4c7e0a3d6f9b2c5e8a1d4f7b0c3e6a9d2f5b8c1e",
}

deny contains msg if {
  package := input.packages[_]
  digest  := sprintf("sha256:%s", [package.digest.sha256])
  not approved[digest]
  msg := sprintf("unapproved package %s (%s)", [package.name, digest])
}

# The thing that ran was not supplied by ocx at all.
deny contains msg if {
  input.executable["sh.ocx.provenance"] == "external"
  msg := sprintf("external executable: %s", [input.process.executable])
}
```

```sh
conftest test --policy approved.rego /var/log/ocx/records/*.json
```

That is enough to gate a CI job on approved digests with no signing infrastructure. The rest of this page is the record's field reference and the configuration that turns it on.

## Record shape {#execution-records-format}

A record describes what OCX was **about to run**, not what happened: it is written immediately before the child process starts, on the same code path that resolves the environment and finds the target executable. There is no matching completion event — no exit code, no duration. On Unix, OCX replaces its own process image with the child ([`execvp(2)`][execvp-manpage]), so nothing runs afterward that could write one; a caller correlates the child's own `$?` separately.

```json
{
  "schemaVersion": "1",
  "kind": "sh.ocx.execution-record",
  "recordedAt": "2026-07-26T14:03:12.006Z",
  "ocx": { "version": "0.5.0", "binary": "/opt/ocx/bin/ocx" },
  "frame": { "command": "package-exec", "identity": "complete" },
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
    "sh.ocx.package": "pkg:oci/solver@sha256:aa11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899?repository_url=internal.corp.example"
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
      "uri": "pkg:oci/solver@sha256:aa11bb22cc33dd44ee55ff6677889900aabbccddeeff00112233445566778899?arch=amd64&repository_url=internal.corp.example&tag=2024.3",
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

`process.user` carries two fields with different trust: `id` comes from the kernel and cannot be forged by the caller's environment, while `name` is read from `$USER`/`$LOGNAME` and can be — key on `id` for anything that must hold up under scrutiny, keep `name` for readability. `process.parent`, when the process that launched ocx is itself identifiable, names its pid: an `ocx run` invoked from a Makefile records `make`'s pid there, so a consumer reconstructs `make → ocx run → tool` call trees from records alone, with no minted correlation ID. All three of `process.user`, `process.parent`, and `process.arch` are best-effort — a scratch container with no passwd entry omits `user` entirely, and a bare shell invocation with no identifiable launcher omits `parent`.

::: warning Command-line arguments are never recorded
`process` carries no `args` field. A command line routinely carries access tokens and passwords typed as flag values, and a record's sink is exactly the kind of destination that turns one leaked argv into many — operator-collected, and often fleet-aggregated into a central store with a wider audience than the invoking host. `process.executable` still records which binary ran; only its arguments are excluded. A config-gated opt-in for operators who accept that exposure may follow later; it is not available today.
:::

`executable["sh.ocx.package"]` names only a **root** package: building the purl needs a repository, and only a root carries one. When the resolved executable instead comes from one of that root's dependencies, the key is **omitted** rather than pointing at the dependency or guessing an identity — the dependency's own identity is still recorded in full inside `packages[]` (`sh.ocx.role: "dependency"`), just not mirrored onto the `executable` block. `sh.ocx.provenance` and `sh.ocx.kind` carry no such restriction; they are present whenever the resolved executable lives inside the OCX store, root or dependency alike. A [Rego][rego] policy reading `input.executable["sh.ocx.package"]` should treat it as optional.

This is a `package exec` of a tag OCX had not previously seen — the reporter's actual configuration. `resolution.autoInstalled` and `packages[0].annotations["sh.ocx.resolved-from"]` together make that auditable: this invocation resolved a *floating* tag and materialized the package on the spot, a state no pull-time record could have captured.

The two markers answer **different questions**, and reading them as one signal is the easiest mistake to make here:

| Marker | Scope | Present when |
|---|---|---|
| `resolution.autoInstalled` | the **invocation** | this run materialized the package on the spot. Depends on cache state, so the same command run twice reports it only the first time |
| `sh.ocx.resolved-from: "tag"` | the **identity** | the digest was reached through a tag rather than named directly. A property of what was requested, so it holds whether or not this run did the pulling |

A package installed ten minutes ago from a moving tag is still tag-derived and still drift-exposed, so it still carries `sh.ocx.resolved-from`. Tying that marker to cache state would make the drift signal blink in and out between two identical invocations — precisely the nondeterminism an audit record must not have. A policy that wants "was anything pulled just now" reads `autoInstalled`; one that wants "could this identity move under me" reads `sh.ocx.resolved-from`.

Had the identifier been digest-pinned instead, **both** would be absent and the purl would carry no `tag` qualifier — a tag appears only when one was actually resolved, never synthesized. A project-tier `ocx run` is the same case for a different reason: `ocx.lock` stores no tags at all, so a lock-resolved package never carries the marker.

`packages[]` is field-for-field an [in-toto][in-toto-attestation] `ResourceDescriptor` (`name`/`uri`/`digest`/`annotations`), so it drops into an [SLSA][slsa] `resolvedDependencies` list without reshaping if an attestation flow is ever built on top. `digest` is always a bare lowercase-hex map keyed by algorithm — never a `sha256:`-prefixed string, never a transport prefix.

`resolution.registries` lists the **content** registries the frame's resolved packages carry — not the index endpoint a version choice was looked up through. A package resolved via an index host is routinely fetched from a different registry that index points at; `registries` names the latter, since that is what an auditor needs to reach the same bytes again. `resolution.mirrors` is a per-host map, keyed by the upstream host a [`[mirrors]`][config-mirrors] entry rewrites, each value naming which traffic role that entry handles:

```json
"mirrors": {
  "docker.io": { "registry": "https://mirror.corp.example" },
  "ghcr.io": { "index": "https://index-mirror.corp.example" }
}
```

`registry` covers OCI blob/manifest traffic, `index` covers index-tree lookups, and a host declaring both roles carries both keys. Only rewritten hosts appear; the map is `{}` when [`[mirrors]`][config-mirrors] is not configured, and the key is omitted entirely at the launcher frame, which composes no environment of its own.

A project-tier `ocx run` record carries two annotations no OCI-tier `package exec` record can: `sh.ocx.binding` names the `ocx.toml` binding the package was selected under, and `sh.ocx.group` names which `[group.<name>]` supplied it (`default` for the top-level `[tools]` table). Its `scope` block also differs — `tier` is `"project"`, and `lock` names the `ocx.lock` the closure was resolved through:

```json
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
```

`lock.declarationDigest` hashes the `ocx.toml` **declarations** the lock was generated from, not the lock file's own contents — two runs whose declarations agree share this value even when they resolved different closures. It answers "was this run driven by the `ocx.toml` I expect", never "what actually ran"; that second question is `packages[]`, package by package, with its own digests.

`sh.ocx.binaries` and `sh.ocx.entrypoints` are JSON arrays of strings, one entry per binary or entry point the package admitted onto `PATH`, present only when the package declares at least one:

```json
{
  "name": "cmake",
  "uri": "pkg:oci/cmake@sha256:3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0?repository_url=index.ocx.sh%2Focx&arch=amd64",
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

This is the same claim [`ocx package env`'s `binaries`/`entrypoints` arrays][cmd-package-env] expose, derived once and shared between the two features. `sh.ocx.platform` appears on **root** packages only — the platform a dependency resolved to is not in hand at this frame, so its descriptor and its purl both omit any platform reference rather than guessing one from the frame's own requested platform.

### Where field names come from {#execution-records-format-sources}

Every borrowed field name is pinned by this page, not by the standard it came from — an upstream rename does not change what OCX emits.

| Record part | Source | Notes |
|---|---|---|
| `packages[]` shape (`name`, `uri`, `digest`, `annotations`) | [in-toto][in-toto-attestation] `ResourceDescriptor` | [Sigstore][sigstore], [Tekton Chains][tekton-chains], and [GUAC][guac] all consume in-toto shapes |
| `packages[].uri` | [purl][purl-spec], `pkg:oci` type | Version is the sha256 digest; `tag` is documented as advisory, matching OCX's digest-is-identity model |
| `process.pid`, `process.working_directory`, `os.type`, `host.name` | [ECS][ecs-process] and [OTel][otel-host] agree | Both standards name these fields the same way |
| `process.parent.pid`, `process.user.name`, `process.executable` | [ECS][ecs-process] | [ECS][ecs-process] types `process.executable` as a flat string; [OTel][otel-host]'s object form would collide with the `sh.ocx.*` fields nested under it |
| `process.arch` | [OTel][otel-host] | Reuses [OTel][otel-host]'s `host.arch` **closed enum** as the value vocabulary, but on `process` rather than `host` — see below for why |
| `schemaVersion`, `recordedAt`, `frame`, `scope`, `resolution` | OCX | No existing standard describes "a CLI invoked a tool with these packages" |
| `sh.ocx.*` annotations (`role`, `binding`, `group`, `platform`, `visibility`, `kind`, `provenance`, `resolved-from`, `binaries`, `entrypoints`, `identity`) | OCX | Everything OCX-specific lives in this namespace so the standard-shaped fields stay standard-shaped |

`process.arch` and `resolution.requestedPlatform` are not the same fact recorded twice. `process.arch` is the architecture of the **running ocx binary** — exactly true even under emulation, where an amd64 ocx executing via Rosetta or QEMU on an arm64 host still reports `amd64`, because that genuinely is the process's own architecture. It answers "what process is this", not "what is the host machine's native architecture" — recovering the latter reliably needs a per-OS native probe this module deliberately does not attempt, since a wrong answer here would be worse than an absent key. `resolution.requestedPlatform` answers a third question: what OCX **asked** resolution to find, in its own `os/arch[+libc.*]` grammar — the request, not the outcome. What a given package's manifest leaf actually resolved to is that package's own `sh.ocx.platform` annotation, and the two differ legitimately: a flat single-image package selects `any` under any request. All three fields can disagree, and the disagreement is itself the audit signal (Rosetta, `binfmt` emulation, an explicit cross-platform pull) — not noise to reconcile.

## What `pkg:oci` buys {#execution-records-purl}

Stated plainly so nothing is promised that is not delivered:

| | |
|---|---|
| **Does** | A stable, standardized identity string — the same shape [Trivy][trivy] emits — that joins across tools and forward-compatible if records ever feed an SBOM or attestation pipeline. |
| **Does not** | Vulnerability lookup. [OSV][osv] has no OCI ecosystem and treats `purl` as informational rather than a query key; [Grype][grype] decomposes an image into its constituent packages and matches those, so a whole-artifact `pkg:oci` purl gets zero CVE matches. |

If CVE visibility into a packaged tool is ever needed, that requires the tool's *upstream ecosystem* purl carried alongside — a separate feature, not a side effect of this one.

## Two records per launcher invocation {#execution-records-frames}

An entrypoint launcher re-enters OCX (`ocx run cmake -- cmake …` resolves `cmake` against the composed `PATH`, hits the package's generated launcher, and that launcher calls the hidden [`ocx launcher exec`][cmd-launcher-exec] subcommand). Each of the two frames records — deliberately, not as a defect — because they see different halves of the truth:

| | Outer frame (`run` / `package exec`) | Inner frame (`launcher exec`) |
|---|---|---|
| `frame.command` | `run` or `package-exec` | `launcher-exec` |
| `frame.identity` | `complete` | `degraded`, always |
| `executable["sh.ocx.kind"]` | `launcher` — the resolved path is the launcher shim | `binary` — the actual leaf executable |
| `packages[0].digest.sha256` | same content digest as the inner frame | same content digest as the outer frame |

The join key is the package's content digest, identical in both records — no minted correlation ID, no environment propagation between the two frames. **A consumer that counts records to count invocations will over-count by 2× for every entrypoint invocation.** Filter on `frame.command` (or on `executable["sh.ocx.kind"] == "binary"`) to count actual tool runs.

A package with no declared entry points resolves straight to its real binary on the composed `PATH`, so it produces exactly one record with `sh.ocx.kind: "binary"` — the split only happens when a launcher is in the chain.

The inner frame's `frame.identity` is `degraded` **unconditionally** — including the ordinary case where an outer `run` or `package-exec` frame pairs with it a few milliseconds earlier. This is not a fallback for an unparented launcher; it is structural. The launcher frame's identifier is a synthetic content digest minted at the launcher, because package directories are content-shared and never persist their own registry or repository. Having an outer frame does not change that: the inner frame still cannot name what it ran, only which digest it ran. A consumer whose policy assumes "degraded means no ocx parent" will fire on every ordinary entrypoint invocation, not only the unusual direct one.

Every degraded frame carries `frame.identityNote`, a sentence stating the limitation in-band, so a consumer reading one record in isolation — without this page open — still learns why the name is missing. The package descriptor carries the matching signal: `sh.ocx.identity: "synthetic"` in its `annotations`, and no `uri` at all, since a purl cannot be built without a repository:

```json
{
  "frame": {
    "command": "launcher-exec",
    "identity": "degraded",
    "identityNote": "package directories are content-shared and carry no registry/repository, so logical identity is not recoverable in this frame and no purl can be emitted"
  },
  "packages": [
    {
      "name": "file-url-mode/3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0",
      "digest": { "sha256": "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0" },
      "annotations": { "sh.ocx.role": "root", "sh.ocx.identity": "synthetic" }
    }
  ]
}
```

A user who runs a generated launcher straight from `PATH`, with no ocx parent at all, produces exactly this shape too — only the content digest identifies what ran, never a name.

[`ocx launcher exec`][cmd-launcher-exec] takes no `--records-*` flags of its own. It inherits the active sink purely through the forwarded `OCX_RECORDS_DIR` / `OCX_RECORDS_NAME` environment variables set by the parent invocation, the same mechanism that forwards every other resolution-affecting setting into a launcher re-entry.

[`ocx package test`][cmd-package-test] and [`ocx patch test`][cmd-patch-test] write **no** record, entrypoint or not. They are maintainer previews over locally materialised, unpublished artifacts, and a record from one would describe something that was never published. The exclusion covers the launcher re-entry too: a launcher built into a package under one of those commands' scratch roots carries it, so no forwarded sink and no sink the child re-reads from its own config chain makes that frame record.

**The exemption stops at `required = true`.** Under a fail-closed posture the two are a contradiction — the policy says this invocation must be recorded, the exemption says it will not be — and it is resolved in the operator's favour: no exemption is granted and the launch is refused with exit `74`, naming the `[records]` policy that refused it. The reason is that the exemption is claimed from a **path**: [`ocx launcher exec`][cmd-launcher-exec] grants it when the pkg-root it was handed sits under one of those scratch roots, and both roots are inside the invoking user's own `$OCX_HOME`. Anyone who can place a directory there can claim it, and no capability handshake fixes that when the parent and the caller run as the same user. So on a host with a fail-closed policy, run a preview command with the policy relaxed, or accept that previews do not run there.

## The sink {#execution-records-sink}

The sink is always a **directory**, never a single file: OCX creates one self-named file per record with a create-exclusive temp file, then publishes it with a **no-clobber** rename — on a naming collision, the record is retried under a fresh random filename rather than silently overwriting an existing record. That is the only sink shape that stays correct across every filesystem the sink might live on, including a shared network mount where two containers can independently draw the same PID in the same millisecond.

**OCX does not create the sink directory.** `dir` must already exist and be writable before the first invocation reaches it — a missing directory fails exactly like a permissions problem (an I/O error naming the path), not with a distinct "directory absent" message. Create the sink once, out of band (a provisioning script, a container image layer, a `mkdir -p` in the job that sets `OCX_RECORDS_DIR`), before pointing OCX at it.

Publication is atomic but **not synced to disk**: OCX does not call `fsync` on the record before publishing it. What the no-clobber publish guarantees is that a completed record's bytes are never interleaved with another writer's — not that those bytes have survived a power loss. A host that loses power between the write and the platter catching up can lose the most recent records; on a network sink, a collector on another client can briefly observe a short, still-being-written file, and should retry rather than treat a JSON parse failure as corruption. Skipping `fsync` is deliberate: it is the only step in the write path that costs real, measured time (tens of milliseconds, versus microseconds without it) on a path that runs once per tool invocation.

Each published file is created **owner-only** (mode `0600` on Unix) — readable and writable by the user OCX ran as, nobody else. A collector process running as a different account (a service user, a log-shipping daemon) cannot read the records unless it runs as the same user or is granted access explicitly; plan the collector's identity around this rather than discovering it when the collector's read comes back empty-handed.

A crash between the publish and the cleanup of its own temp handle can strand a hidden temp file in the sink permanently. **A collector that lists the sink directory rather than globbing `*.json` must skip any `.tmp*` entry** — it is write-in-progress or crash debris, never a completed record.

**The sink is pinned to where it really is, once.** The first time a process resolves `[records]`, the configured sink is resolved to its real location and that result is kept for the rest of the invocation. A sink that already reaches through a symlink at that moment is fine — it is simply pinned to its target — which is what makes an ordinary path work on macOS, where `/var/log/…` is reached through `/var` → `/private/var`. Before every write, the pinned path is re-checked: if it no longer resolves to itself, the write is refused rather than allowed to relocate the audit trail somewhere the operator never designated, silently, while the source keeps exiting 0 and the collector sees an empty directory. The refusal takes the configured failure posture — a warning by default, or a hard stop (exit `78`) under `required = true` — classified as a configuration fault rather than an I/O fault, since the sink arrives from `[records]` config, `OCX_RECORDS_DIR`, or a SYSTEM-scope lock, never from a CLI argument.

**What that check does and does not cover.** It compares a canonical *path*, not the directory's identity, and it runs before the write rather than holding the directory open across it. So it catches a symlink inserted into the pinned path and still present when the check runs — the accidental case it exists for. It does not catch a sink replaced by a *different real directory* at the same path, which re-resolves to itself and is accepted; nor a symlink inserted in the window between the check and the file being created. It also does not reach across the two processes an entrypoint invocation spans (the outer frame and the `ocx launcher exec` re-entry): each re-resolves and re-pins independently, so a symlink planted before the inner process starts is pinned as *its* designated sink. Closing any of these would mean holding an open directory handle from designation through every write, and — for the cross-process case — a way for the launcher to tell "a sink forwarded by my parent" from "a sink an operator configured directly", which `[records]` deliberately carries no channel for. **Treat this as an integrity control, not a security boundary**: it defends against a script or a log rotator redirecting the trail by accident, not against an adversary. Anyone able to plant that symlink can equally just not run `ocx`.

::: warning Records contain absolute paths, including the user's home directory
`process.executable` and the package `content/`/`entrypoints/` paths inside each `packages[]` entry are absolute filesystem paths under the invoking user's home directory (or `$OCX_HOME`, if set elsewhere). Size sink permissions and retention accordingly — this matters most once records leave the machine they were written on, for a central SIEM or long-retention archive with a wider audience than the operator who configured the sink.
:::

## Filename grammar {#execution-records-filename}

The default filename is `<utc-basic-ms>-<pid>-<8 hex random>.json` — an ISO-8601-basic UTC millisecond timestamp (sorts lexicographically in chronological order), the owning process ID, and an 8-character random suffix that breaks a same-pid-same-millisecond collision across hosts or containers.

The pattern is overridable via a template over a closed placeholder set:

| Placeholder | Expands to |
|---|---|
| `{time}` | `20260726T140311482Z` — same basic UTC form as the default |
| `{pid}` | the process ID of the process that runs the tool |
| `{rand}` | 8 hex characters |
| `{host}` | hostname, reduced to a filename-safe form |

A placeholder is in the set only if it is cheap to make safe as a filename. `{time}`, `{pid}` and `{rand}` are OCX-generated and safe by construction. `{host}` is read from the environment, where a hostname may legitimately contain a `/`, so its expansion keeps only `A-Z a-z 0-9 . _ -` and replaces everything else with `_`; a hostname that reduces to `.` or `..` expands to nothing, exactly as an undeterminable hostname does. There is no `{command}` placeholder, deliberately — the invoked binary is already in the record (`process.executable`) one `jq` away, and sanitizing a user-controlled argv for separators, spaces, unicode and length would be real surface for no real gain — and would need [the argument list `process` deliberately omits][execution-records-format].

An **unknown placeholder is a configuration error at resolve time**, not a silently-unexpanded literal — the failure mode of a stray `{jobid}` is a directory of identically-named files, which is worse discovered mid-audit than at startup. A template that renders to anything other than a single plain filename (one carrying a path separator, say) is refused the same way, so a record can never land outside the sink.

## Configuring `[records]` {#execution-records-configuration}

Recording is off by default: absent a sink directory at every layer, OCX writes nothing and adds no I/O to the exec path.

```toml
[records]
dir      = "/var/log/ocx/records"
name     = "{time}-{host}-{pid}.json"
required = true
```

```sh
ocx run --records-dir /var/log/ocx/records --records-name '{time}-{pid}.json' -- cmake --build build
OCX_RECORDS_DIR=/var/log/ocx/records OCX_RECORDS_NAME='{time}-{host}.json' ocx package exec cmake:3.28 -- cmake --version
```

`dir` and `name` resolve through the usual four-layer fold — [`[records]`][config-records] config → [`OCX_RECORDS_DIR`][env-ocx-records-dir] / [`OCX_RECORDS_NAME`][env-ocx-records-name] → `--records-dir` / `--records-name` on [`ocx run`][cmd-run] or [`ocx package exec`][cmd-package-exec] — with the highest layer winning per field.

`required` is different: it is **config-file-only at every tier**. There is no `OCX_RECORDS_REQUIRED` and no `--records-required` — recording posture is an operator decision, not a per-invocation one, and a "strengthen-only" flag would exist only to be typed and do nothing. An operator-published [`[managed]`][config-managed] configuration can set `required = true` in its own `[records]` payload the same as any other config tier — that is the intended use of the managed tier, not a special case, and it is not required to also carry a SYSTEM-scope lock.

| `required` | Default | Behavior when a record cannot be written |
|---|---|---|
| `true` | when a SYSTEM-scope (`/etc/ocx/config.toml`) `[records]` policy is present | The invocation aborts — exit `74` for an unwritable sink, exit `78` when the sink resolves through a symlink. |
| `false` | when no SYSTEM-scope policy is present | A warning prints to stderr; the child still runs. |

Both exit codes share the same posture — `required` decides only whether OCX aborts or warns, never which code an abort uses. The split exists because the two failures are different kinds of fault: an unwritable sink (missing directory, full disk, denied permission) is an I/O fault the operator fixes by clearing the underlying condition; a symlinked sink is a configuration fault the operator fixes by editing `[records] dir` or the file behind `--records-dir`/`OCX_RECORDS_DIR`, since a caller never typed the sink as a command-line argument in the first place.

**Writing `required = true` without a `dir` is a configuration error (exit `78`), not recording turned off.** A block carrying only that line is the plainest way to say "recording is mandatory here", and resolving it to a policy with nowhere to write would give every invocation on the host the exact opposite — silently, exit `0`, no warning. It is refused when the configuration is read, before any work. The one shape that is *not* an error is a SYSTEM-scope `[records]` block with neither `dir` nor `required`: that is an operator locking recording **off** for the host, and it keeps working.

::: warning `required = true` aborts before the child starts on Unix only
On Unix the record is written before OCX replaces its own process image with the tool, so a failure to write means the tool never ran. On Windows there is no such moment: the pid the record names does not exist until the child is spawned, so OCX probes the sink beforehand and writes the record after. A sink that is writable at probe time and fails at write time — a mount that goes away, a disk that fills, a permission change in between — aborts after the child has already begun work. OCX stops and reaps that child, but it ran. Treat `required = true` on Windows as "refused up front if the sink was unwritable, recorded otherwise", not as proof that no unrecorded tool ever started.
:::

A SYSTEM-scope `[records]` declaration locks the **whole block** — `dir`, `name`, and `required` together, not field by field. A collector downstream depends on the sink location *and* the filename pattern together, so a partial override would break collection exactly as surely as redirecting `dir` alone. See [`[records]`][config-records] in the Configuration reference for the full field list and the lock's interaction with the other config tiers.

A SYSTEM-scope lock survives [`OCX_NO_CONFIG=1`][env-ocx-no-config]: that variable prunes ambient system/user/`$OCX_HOME` configuration and the managed-config tier, but it does not let a caller step around a policy the operator declared at system scope. See [`OCX_NO_CONFIG`][env-ocx-no-config] for the full interaction.

It also survives a `/etc/ocx/config.toml` that cannot be read. Where an unreadable user-tier config is skipped with a warning, an unreadable **system** one aborts the invocation (exit `78`) — including the case that motivated it, a `/etc/ocx/config.toml` symlinked at a fleet-managed file. Skipping it would drop the locked policy along with the file, on every invocation, with nothing but a warning to say so. Absence is still absence: no system file at all is the ordinary case and stays silent.

## Schema {#execution-records-schema}

The published schema lives at `https://ocx.sh/schemas/execution-record/v1.json` and moves in lockstep with the in-band `schemaVersion` string inside every record — the first schema-breaking change bumps both together, never one without the other. `schemaVersion` itself only changes for a backward-incompatible break; new fields are additive and never bump it, so a consumer written against `schemaVersion: "1"` must tolerate unknown keys rather than reject them.

<!-- external -->
[conftest]: https://www.conftest.dev/
[opa]: https://www.openpolicyagent.org/
[rego]: https://www.openpolicyagent.org/docs/policy-language
[purl-spec]: https://github.com/package-url/purl-spec
[in-toto-attestation]: https://github.com/in-toto/attestation
[slsa]: https://slsa.dev/spec/v1.0/provenance
[sigstore]: https://www.sigstore.dev/
[tekton-chains]: https://tekton.dev/docs/chains/
[guac]: https://github.com/guacsec/guac
[ecs-process]: https://www.elastic.co/docs/reference/ecs/ecs-process
[otel-host]: https://opentelemetry.io/docs/specs/semconv/registry/attributes/host/
[trivy]: https://trivy.dev/
[osv]: https://osv.dev/
[grype]: https://github.com/anchore/grype
[execvp-manpage]: https://man7.org/linux/man-pages/man3/execvp.3.html

<!-- commands -->
[cmd-run]: ./command-line.md#run
[cmd-package-exec]: ./command-line.md#package-exec
[cmd-package-env]: ./command-line.md#package-env
[cmd-launcher-exec]: ./command-line.md#launcher-exec
[cmd-package-test]: ./command-line.md#package-test
[cmd-patch-test]: ./command-line.md#patch-test
[cmd-lock]: ./command-line.md#lock

<!-- configuration -->
[config-records]: ./configuration.md#keys-records
[config-mirrors]: ./configuration.md#keys-mirrors
[config-managed]: ./configuration.md#keys-managed

<!-- environment -->
[env-ocx-records-dir]: ./environment.md#ocx-records-dir
[env-ocx-records-name]: ./environment.md#ocx-records-name
[env-ocx-no-config]: ./environment.md#ocx-no-config

<!-- internal -->
[execution-records-format]: #execution-records-format
