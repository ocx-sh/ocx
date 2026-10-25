---
title: "Launcher frames in records"
description: "Why an entrypoint launcher writes two execution records, how the inner frame learns its package name, and how previews, deferred packages and patch companions appear."
---
<!-- doc_type: explanation -->

This page explains the extra frames that appear in execution records when an entrypoint launcher, a deferred package or a patch companion is involved. It extends [Two records per launcher invocation][records-frames-summary] in [Execution Records][records].

## Forwarded names and degraded frames {#frames-identity}

The inner frame can name its package only when the parent forwarded the name in [`__OCX_LAUNCH_IDENTITIES`][env-ocx-launch-identities]. A launcher finds its package by directory. Every name that resolves to the same digest shares that directory, so only the parent knows which name it used.

OCX sets the variable, never you, and only while a [`[patches]`][config-patches] tier is in effect. The value is a JSON object mapping each content digest a composition resolved to the `registry/repository[:tag]` names it was resolved under. `no_patches` marks a package the project opted out.

These commands write the variable for the process they spawn:

- [`ocx exec`][cmd-exec], `ocx package exec`, `ocx package test`,
- a lazy tool's shim.

These commands export it, keeping an inherited value's entries for packages they did not compose:

- `ocx env`, `ocx package env`,
- `ocx direnv export`,
- the per-prompt shell hook, which also restores the replaced value when you leave a project.

A malformed value is a hard startup error for the launcher (exit 78) while a patch tier is in effect. Like every `OCX_*` key, it cannot be set from `ocx.toml` or through `--env`. With a forwarded name the frame is `complete`. Its package descriptor carries the real name and a purl without a `tag` qualifier, as a shim frame does.

Without a forwarded name, the identifier is a synthetic content digest minted at the launcher. Package directories are content-shared and never persist their own registry or repository. The frame is then `degraded`, even when an outer `exec` or `package exec` frame pairs with it a few milliseconds earlier.

On a host without a patch tier, a consumer whose policy assumes "degraded means no ocx parent" fires on every ordinary entrypoint invocation.

Every degraded frame states the limitation in-band, so a consumer reading one record in isolation still learns why the name is missing. The frame carries `frame.identityNote`. The package descriptor carries `sh.ocx.identity: "synthetic"` and no `uri` at all, since a purl cannot be built without a repository.

```json
{
  "frame": {
    "command": "launcher exec",
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

A launcher run from a `PATH` that no ocx command composed produces this shape too. Only the content digest identifies what ran.

A launcher run from a `PATH` that [`ocx env`][cmd-env-root], [`ocx direnv export`][cmd-direnv-export] or the shell hook composed names its package. Those exports carry the same variable while a patch tier is in effect.

## Previews write no record {#frames-previews}

[`ocx package test`][cmd-package-test] and [`ocx patch test`][cmd-patch-test] write no record, entrypoint or not. They are maintainer previews over locally materialised, unpublished artifacts, and a record from one would describe something that was never published.

The exclusion covers the launcher re-entry too. A launcher built into a package under one of those commands' scratch roots carries it. No forwarded sink makes that frame record, and neither does a sink the child re-reads from its own config chain.

The exemption stops at `required = true`. Under a fail-closed posture the two contradict each other. The policy says this invocation must be recorded, and the exemption says it will not be. OCX resolves the contradiction in the operator's favour. It grants no exemption and refuses the launch with exit 74, naming the `[records]` policy that refused it.

The reason is that the exemption is claimed from a path. [`ocx launcher exec`][cmd-launcher-exec] grants it when the pkg-root it was handed sits under one of those scratch roots. Both roots are inside the invoking user's own `$OCX_HOME`. Anyone who can place a directory there can claim the exemption. No capability handshake fixes that when the parent and the caller run as the same user.

On a host with a fail-closed policy, run a preview command with the policy relaxed, or accept that previews do not run there.

## The launcher shim frame {#shim-frame}

[Deferred packages][deferred-packages] add a fourth launching frame beside `exec`, `package exec` and `launcher exec`. It is the hidden [`ocx launcher shim`][deferred-materialize] subcommand. A package composed with `lazy-mode = always` puts a generated shim on `PATH` instead of an eager install. Nothing about it exists on disk until that shim runs for the first time.

The shim frame is therefore the only place a deferred package's content is downloaded. Its first invocation produces two records, for a different reason than the entrypoint split.

The join key is the same content digest used everywhere else. Unlike an `ocx launcher exec` frame with no forwarded name, a shim's generated launcher carries [the pinned identifier it was built for][deferred-materialize] as an argument. `ocx launcher shim` reads that identifier directly, so it needs no parent to know its own logical name.

A later invocation of the same binary resolves straight to its real `entrypoints/`, which outranks the shim on `PATH`. It produces exactly one record, the same shape as any package that was never deferred.

## Patch companions in the record {#patch}

A [patch companion][patches-how] composes as part of the packages it patches rather than replacing them. The record treats it as one more package and not as a side channel. Any companion admitted for the invocation appears in `packages[]` as an ordinary entry. Scanning `packages[]` in order gives the sequence of roots, package dependencies, companions and the companions' dependencies.

A companion that contributes only [integrations][env-composition-integrations] is recorded too, because OCX loaded it into the environment like any other package.

The patch tier's freeze is independent of the package tier's. `resolution.patchSnapshot` can be present while `resolution.frozen` is `false`, and the reverse. Each fact is carried by its own field.

<!-- internal -->
[records]: execution-records.md
[records-frames-summary]: execution-records.md#frames
[deferred-packages]: deferred-packages.mdx
[deferred-materialize]: deferred-packages.mdx#deferred-tools-materialize
[patches-how]: patches.mdx#patches-how

<!-- reference -->
[config-patches]: ../reference/configuration.md#keys-patches
[env-ocx-launch-identities]: ../reference/environment.md#ocx-launch-identities
[env-composition-integrations]: ../reference/env-composition.md#integrations-companions

<!-- commands -->
[cmd-exec]: ../reference/command-line/commands.md#exec
[cmd-env-root]: ../reference/command-line/commands.md#env-root
[cmd-direnv-export]: ../reference/command-line/direnv.md#direnv-export
[cmd-launcher-exec]: ./launcher-forwarding.md#launcher-embedding
[cmd-package-test]: ../reference/command-line/package-publishing.md#package-test
[cmd-patch-test]: ../reference/command-line/patch.md#patch-test
