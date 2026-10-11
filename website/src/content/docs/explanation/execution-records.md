---
title: "Execution Records"
description: "How OCX writes an audit record before it starts a binary, who controls the sink, and what a record does and does not promise."
---
<!-- doc_type: explanation -->

This page explains how OCX records which package digests composed a job's environment. It covers how recording is switched on, which setting wins, and what a record promises. The record's fields, the sink's write behaviour and the schema live in the [Execution Records reference][execution-records-ref].

An operator may need to show, after the fact, which package digests composed a job's environment. That takes evidence written by the tool that composed it. A wrapper script that every caller has to trust is weaker evidence. An execution record is the stronger kind. It is one JSON file, written just before OCX starts a binary, that names every package digest in the child's environment.

Two sibling pages go deeper. [Records and SBOMs][records-sbom] explains the `pkg:oci` identity and how a record chains to an SBOM or attestation. [Launcher frames in records][records-frames] explains the extra records that entrypoint launchers, deferred packages and patch companions produce.

## Switching recording on {#execution-records-enable}

Recording is off by default. Without a sink directory at any layer, OCX writes nothing and adds no I/O to the exec path. A sink directory is the only thing that turns recording on. It can come from four places, and the highest wins per field:

- The `--records-dir` and `--records-name` flags on [`ocx exec`][cmd-exec] and [`ocx package exec`][cmd-package-exec].
- The [`OCX_RECORDS_DIR`][env-ocx-records-dir] and [`OCX_RECORDS_NAME`][env-ocx-records-name] environment variables.
- The `dir` and `name` keys of the [`[records]`][config-records] config section.
- Nothing, which means no record.

The environment variables exist for CI and container setups where the command line is not under your control. Both affect resolution, and OCX forwards both to every subprocess it spawns. A generated launcher's re-entry takes no flags of its own. It therefore writes to the same sink as the invocation that started it.

```sh
export OCX_RECORDS_DIR=/var/log/ocx/records
export OCX_RECORDS_NAME='{time}-{host}-{pid}.json'
ocx exec -- cmake --build build
```

The flags do the same for one invocation, and they outrank the variables:

```sh
ocx exec --records-dir /var/log/ocx/records --records-name '{time}-{pid}.json' -- cmake --build build
```

After the command, the sink holds one new file named by the template, such as `20260101T120000482Z-build01-4242.json` (see the [filename grammar][execution-records-filename]). Read it with `jq . /var/log/ocx/records/*.json`. To check a record, validate it with any JSON Schema validator against the published [execution-record schema][execution-records-schema]. [Scripts and JSON](../integration/scripts-and-json.md#scripts-schemas) shows how.

Setting either variable to the empty string reads as unset. That is the escape hatch for a value exported from a shell profile. `OCX_RECORDS_DIR= ocx exec -- cmake --version` disables recording for that one invocation without unsetting the variable.

The sink directory must already exist and be writable, because OCX does not create it. [The sink][execution-records-sink] describes why.

## Naming the files {#execution-records-naming}

The default filename is `<utc-basic-ms>-<pid>-<8 hex random>.json`. A template over the closed placeholder set `{time}`, `{pid}`, `{rand}` and `{host}` overrides it. See [Filename grammar][execution-records-filename].

An unrecognized placeholder is a configuration error (exit 78) at resolve time. OCX never leaves it as a silently unexpanded literal. `name` has no effect unless a sink is also active.

## Who can set it {#execution-records-policy}

Recording is an operator decision, so the config tiers are not equal.

A SYSTEM-scope `[records]` declaration locks the whole section: `dir`, `name` and `required` together. A collector depends on the sink location and the filename pattern as a pair. Once locked, the flags and variables have no effect, and the lock survives [`OCX_NO_CONFIG`][env-ocx-no-config].

A repository's own project-tier `ocx.toml` cannot set `[records]` at all. OCX strips the section there, so a cloned repository can never redirect the audit trail of whoever builds it.

A relative `dir` in a config file is anchored at the declaring file's directory. `--records-dir` and `OCX_RECORDS_DIR` stay relative to the process's working directory, like every other flag and variable path in OCX.

## When a record cannot be written {#execution-records-required}

`required` decides whether a failed write aborts the invocation or only warns. It is config-file-only at every tier. There is no `OCX_RECORDS_REQUIRED` and no `--records-required`, because a per-invocation switch would let the caller weaken a posture the operator chose.

| `required` | Default | When a record cannot be written |
|---|---|---|
| `true` | when a SYSTEM-scope `[records]` policy is present | The invocation aborts: exit 74 for an unwritable sink, exit 78 when the sink resolves through a symlink. |
| `false` | when no SYSTEM-scope policy is present | A warning prints to stderr and the child still runs. |

Writing `required = true` without a `dir` is a configuration error (exit 78), not recording turned off. The Windows ordering caveat and the exit-code split are under [Failure posture in detail](#required-details). The field-level facts are under [Configuring `[records]`][execution-records-configuration] in the reference.

## The gap it closes {#gap}

[`ocx.lock`][cmd-lock] pins tag-to-digest resolution for the project tier, but OCX checks the pin at `pull` time. Between a pull and the exec that follows it, config can change and a floating tag can move. [`ocx package exec`][cmd-package-exec] can also auto-install a package no prior pull ever saw.

On the OCI tier there is no lock at all. The gap is widest in exactly that configuration: no `ocx.toml`, no lock, a bare `package exec` against a tag.

A wrapper script can log a resolved digest next to whatever a job produces. That makes the wrapper the actual control point, and every caller has to trust it. A record moves the control into OCX itself. No wrapper script has to know the mechanism exists.

No caller can opt out of a sink the operator has locked at system scope. That holds through configuration and through the maintainer-preview exemption, which a fail-closed policy refuses and does not grant.

## A policy check that works today {#consumer}

The realistic first consumer of a record is a policy check over raw JSON, not an attestation pipeline. [Conftest][conftest] is built on [Open Policy Agent][opa]'s [Rego][rego] language. It asks nothing of OCX: no signing key, no registry, no recognized predicate type. It reads the file as written.

Install Conftest from its site first. The `approved` set below is a placeholder: you supply your own approved digests, for example from a lock file or a release manifest.

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
conftest test --policy approved.rego --namespace ocx.records /var/log/ocx/records/*.json
```

That is enough to gate a CI job on approved digests with no signing infrastructure. The field `executable["sh.ocx.package"]` is omitted when the executable comes from a dependency. A policy reading it should treat it as optional. The [record shape][execution-records-format] lists when each field is present.

To wire a check like this into a script or a pipeline, see [Scripts and JSON][integration-scripts].

## What the record promises {#record-semantics}

A record describes what OCX was about to run, not what happened. OCX writes it on the same code path that resolves the environment and finds the target executable, immediately before the child starts. On Unix, OCX then replaces its own process image with the child ([`execvp(2)`][execvp-manpage]). Nothing runs afterward that could write a completion event. A caller correlates the child's own `$?` separately.

Several fields are deliberate omissions or best-effort facts. Each has a reason.

- **Arguments are never recorded.** A command line routinely carries access tokens and passwords typed as flag values. A sink turns one leaked argv into many. Operators collect it, and often aggregate it fleet-wide into a central store with a wider audience than the invoking host. `process.executable` still records which binary ran.

  A config-gated opt-in for operators who accept that exposure may follow. It is not available.
- **`process.user` carries two fields with different trust.** `id` comes from the kernel and the caller's environment cannot forge it. `name` is read from `$USER` or `$LOGNAME` and can be forged. Key on `id` for anything that must hold up under scrutiny, and keep `name` for readability.
- **`process.parent` names the launching process's pid** when it is identifiable. An `ocx exec` invoked from a Makefile lets a consumer reconstruct `make → ocx exec → binary` call trees from records alone, with no minted correlation ID.
- **Mirror credentials are stripped.** A mirror URL with an embedded `user:password@` credential is a mainstream Artifactory and Nexus remote-repository idiom. The record keeps the bare endpoint it rewrites to. That is what an auditor needs: the host the traffic went to.

  The credential is not published into every record on the fleet. It still works, because stripping changes what is recorded and not what is sent.
- **`process.arch`, `resolution.requestedPlatform` and a package's `sh.ocx.platform` are three facts.** `process.arch` is the architecture of the running ocx binary, exactly true even under emulation. Recovering the host's native architecture would need a per-OS probe that this module deliberately does not attempt. A wrong answer would be worse than an absent key.

  `requestedPlatform` is what OCX asked resolution to find, in its own `os/arch[+libc.*]` grammar. The package's `sh.ocx.platform` is what its manifest leaf actually resolved to. A flat single-image package selects `any` under any request. The three can disagree legitimately, and the disagreement is itself the audit signal.
- **`registries` names content registries.** An index host often points at a different registry that serves the bytes. `registries` names the latter, since an auditor needs it to reach the same bytes again.
- **`frozen` covers one tier.** `--frozen` freezes package-tier resolution only. A patch companion in force during the same invocation still resolves live and pins independently under `$OCX_HOME/state/patch-companions/`. Freezing the patch tier is a separate act ([`ocx patch freeze`][cmd-patch-freeze] plus [`OCX_PATCH_SNAPSHOT`][env-ocx-patch-snapshot]).
- **`noVerify` is a posture and not an outcome.** Verification happens at install and not at exec. A warm store serves a cache hit without consulting the verifier. The record therefore never claims a package was verified. It says only whether [`OCX_NO_VERIFY`][env-ocx-no-verify] switched off the gate that would have covered it for this run.

### Reading the resolution markers {#resolution-markers}

`resolution.autoInstalled` and `sh.ocx.resolved-from: "tag"` answer different questions. Reading them as one signal is the easiest mistake here.

The first is about the invocation. This run materialized the package on the spot, which depends on cache state, so the same command run twice reports it only the first time. The second is about the identity. The digest was reached through a tag rather than named directly. That is a property of what was requested, and it holds whether or not this run did the pulling.

`resolved-from` says a tag was present in the resolved request. It does not say the identity remains free to move.

[The local index is never updated automatically][local-index]. Once the index commits a tag's digest, a later invocation reads that pin and does not re-walk the registry. Only the first resolution of a tag the index has not seen walks the registry. A patch companion's tag is kept for the same reason and resolved by digest.

The marker is deliberately stable. A package installed ten minutes ago from a tag still carries it. Tying it to cache state would make it blink in and out between two identical invocations, which an audit record must not do.

A policy that wants "was anything pulled in this run" reads `autoInstalled`. A policy that wants "was this identity reached by name rather than digest" reads `sh.ocx.resolved-from`. Neither answers "could this identity still move". That depends on whether the index has committed a pin, and the record does not carry it.

A digest-pinned identifier carries neither marker, and its purl has no `tag` qualifier, because a tag appears only when one was actually resolved. A project-tier `ocx exec` is the same case for a different reason. OCX rejoins the declared tag from `ocx.toml` onto the resolved identifier only so tag-scoped [patch rules][patches-how] can match it. It is never persisted in `ocx.lock` or the record.

A `launcher shim` frame never carries the markers either. The shim pulls by its baked digest, and the tag baked into a shim directory shared across projects is whichever tag installed it first. That is no evidence of how this invocation reached the digest.

`lock.declarationDigest` in a project-tier record hashes the `ocx.toml` declarations the lock was generated from. It does not hash the lock file's own contents. Two runs whose declarations agree share it even when they resolved different closures. It answers "was this run driven by the `ocx.toml` I expect". It never answers "what actually ran", which is `packages[]`, package by package, with its own digests.

## Two records per launcher invocation {#frames}

An entrypoint launcher re-enters OCX. `ocx exec cmake -- cmake …` resolves `cmake` against the composed `PATH` and hits the package's generated launcher. That launcher calls the hidden [`ocx launcher exec`][cmd-launcher-exec] subcommand.

Each of the two frames records, deliberately and not as a defect, because they see different halves of the truth. The outer frame sees the launcher shim as the resolved path. The inner frame sees the actual leaf executable.

The join key is the package's content digest, identical in both records, with no minted correlation ID and no environment propagation between the two frames. A consumer that counts records to count invocations counts every entrypoint invocation twice. It filters on `frame.command` or on `executable["sh.ocx.kind"] == "binary"` to count actual binary runs. A package with no declared entry points resolves straight to its real binary and produces exactly one record.

[Launcher frames in records][records-frames] covers how the inner frame learns its package name and what a degraded frame looks like. It also covers why previews write no record, and the deferred-package and patch-companion frames.

## What the sink guarantees {#sink-guarantees}

The sink is a directory and not a single file. Only a directory stays correct across every filesystem it might live on. That includes a shared network mount, where two containers can draw the same PID in the same millisecond. OCX creates each record as a create-exclusive temp file and publishes it with a no-clobber rename. A collision retries under a fresh random filename.

Publication is atomic but not synced to disk. The no-clobber publish guarantees that a completed record's bytes are never interleaved with another writer's. It does not guarantee those bytes survive a power loss.

Skipping `fsync` is deliberate. It is the only step in the write path that costs real, measured time: tens of milliseconds, against microseconds without it. It would run once per binary invocation.

A collector on a network sink should retry a short file and not treat a JSON parse failure as corruption. It can briefly observe a file that is still being written.

A record is not signed, and the sink is not authenticated. Any principal who can write to the directory can create a file that parses as a record. It can claim any package digest, executable and user name. On a single-writer sink that costs nothing. On a shared sink the residual integrity signals are the file's own mode and ownership.

OCX creates every record `0600` under the invoking user, and `process.user.id` comes from the kernel and not the environment.

OCX pins the sink to where it really is, once, the first time a process resolves `[records]`. A sink that already reaches through a symlink at that moment is pinned to its target. That makes an ordinary path work on macOS, where `/var/log/…` is reached through `/var` → `/private/var`.

Before every write, OCX re-checks the pinned path. If it no longer resolves to itself, OCX refuses the write. Otherwise the audit trail could relocate somewhere the operator never designated, silently, while the source keeps exiting 0 and the collector sees an empty directory. The refusal is a configuration fault and not an I/O fault. The sink arrives from `[records]` config, `OCX_RECORDS_DIR` or a SYSTEM-scope lock, never from a CLI argument.

The check compares a canonical path and not the directory's identity. It runs before the write rather than holding the directory open across it. It catches a symlink inserted into the pinned path and still present when the check runs, which is the accidental case it exists for.

It does not catch a sink replaced by a different real directory at the same path. It does not catch a symlink inserted between the check and the file being created. It does not reach across the two processes an entrypoint invocation spans either. Each process re-resolves and re-pins independently, so a symlink planted before the inner process starts is pinned as its designated sink.

Closing any of these would mean holding an open directory handle from designation through every write. The cross-process case would also need a way for the launcher to tell a forwarded sink from one an operator configured directly. `[records]` deliberately carries no channel for that.

:::caution[An integrity control, not a security boundary]
The pin defends against a script or a log rotator redirecting the trail by accident, not against an adversary. Anyone able to plant that symlink can equally just not run `ocx`.
:::

:::caution[Records contain absolute paths, including the user's home directory]
`process.executable` and the package `content/` and `entrypoints/` paths inside each `packages[]` entry are absolute filesystem paths. They sit under the invoking user's home directory, or under `$OCX_HOME` if set elsewhere. Size sink permissions and retention accordingly. That matters most once records leave their machine for a central SIEM or a long-retention archive. Those have a wider audience than the operator who configured the sink.
:::

## Failure posture in detail {#required-details}

`required` decides only whether OCX aborts or warns. It never decides which code an abort uses. The two failures are different kinds of fault:

- An unwritable sink (missing directory, full disk, denied permission) is an I/O fault. The operator fixes it by clearing the underlying condition.
- A symlinked sink is a configuration fault. The operator fixes it by editing `[records] dir`, or the file behind `--records-dir` and `OCX_RECORDS_DIR`.

Writing `required = true` without a `dir` is a configuration error. A block carrying only that line is the plainest way to say "recording is mandatory here". Resolving it to a policy with nowhere to write would give every invocation on the host the exact opposite. That would happen silently, with exit 0 and no warning. OCX refuses it when it reads the configuration, before any work.

The one shape that is not an error is a SYSTEM-scope `[records]` block with neither `dir` nor `required`. That is an operator locking recording off for the host.

An operator-published [`[managed]`][config-managed] configuration can set `required = true` in its own `[records]` payload like any other config tier. That is the intended use of the managed tier, and it does not also need a SYSTEM-scope lock.

A SYSTEM-scope lock survives [`OCX_NO_CONFIG=1`][env-ocx-no-config]. It also survives a `/etc/ocx/config.toml` that cannot be read. OCX skips an unreadable user-tier config with a warning. An unreadable system one aborts the invocation with exit 78, including when `/etc/ocx/config.toml` is a symlink at a fleet-managed file.

Skipping it would drop the locked policy along with the file, on every invocation, with nothing but a warning to say so. Absence is still absence. No system file at all is the ordinary case and stays silent.

:::caution[`required = true` aborts before the child starts on Unix only]
On Unix, OCX writes the record before it replaces its own process image with the binary. A failure to write therefore means the binary never ran. On Windows the pid the record names does not exist until the child is spawned. OCX therefore probes the sink beforehand and writes the record after.

A sink that is writable at probe time and fails at write time aborts after the child has already begun work. Examples are a mount that goes away, a disk that fills or a permission change in between. OCX stops and reaps that child, but it ran. Treat `required = true` on Windows as "refused up front if the sink was unwritable, recorded otherwise". It is not proof that no unrecorded binary ever started.
:::

## Filename placeholders {#placeholders}

A placeholder is in the closed set only if it is cheap to make safe as a filename. `{time}`, `{pid}` and `{rand}` are OCX-generated and safe by construction. `{host}` is read from the environment, where a hostname may legitimately contain a `/`. Its expansion keeps only `A-Z a-z 0-9 . _ -` and replaces everything else with `_`.

There is no `{command}` placeholder, deliberately. The invoked binary is already in the record as `process.executable`, one `jq` away. Sanitizing a user-controlled argv for separators, spaces, unicode and length would be real surface for no real gain. It would also need the argument list that `process` deliberately omits.

An unknown placeholder is a configuration error at resolve time and not a silently unexpanded literal. The failure mode of a stray `{jobid}` is a directory of identically named files. That is worse to discover mid-audit than at startup.

## Schema versioning {#schema-versioning}

The published schema moves in lockstep with the in-band `schemaVersion` string inside every record. The first schema-breaking change bumps both together, and never one without the other. New fields are additive and never bump `schemaVersion`. A consumer written against `"1"` must therefore tolerate unknown keys and not reject them.

<!-- reference -->
[execution-records-ref]: ../reference/execution-records.md
[execution-records-schema]: ../reference/execution-records.md#execution-records-schema
[execution-records-sink]: ../reference/execution-records.md#execution-records-sink
[execution-records-filename]: ../reference/execution-records.md#execution-records-filename
[execution-records-configuration]: ../reference/execution-records.md#execution-records-configuration
[execution-records-format]: ../reference/execution-records.md#execution-records-format
[config-records]: ../reference/configuration.md#keys-records
[config-managed]: ../reference/configuration.md#keys-managed
[env-ocx-records-dir]: ../reference/environment.md#ocx-records-dir
[env-ocx-records-name]: ../reference/environment.md#ocx-records-name
[env-ocx-no-config]: ../reference/environment.md#ocx-no-config
[env-ocx-patch-snapshot]: ../reference/environment.md#ocx-patch-snapshot
[env-ocx-no-verify]: ../reference/environment.md#ocx-no-verify

<!-- commands -->
[cmd-exec]: ../reference/command-line/commands.md#exec
[cmd-package-exec]: ../reference/command-line/package-consuming.md#package-exec
[cmd-lock]: ../reference/command-line/commands.md#lock
[cmd-launcher-exec]: ./launcher-forwarding.md#launcher-embedding
[cmd-patch-freeze]: ../reference/command-line/patch.md#patch-freeze

<!-- external -->
[conftest]: https://www.conftest.dev/
[opa]: https://www.openpolicyagent.org/
[rego]: https://www.openpolicyagent.org/docs/policy-language
[execvp-manpage]: https://man7.org/linux/man-pages/man3/execvp.3.html

<!-- internal -->
[records-sbom]: records-and-sbom.md
[records-frames]: records-launcher-frames.md
[local-index]: local-index.md#local
[patches-how]: patches.mdx#patches-how
[integration-scripts]: ../integration/scripts-and-json.md
