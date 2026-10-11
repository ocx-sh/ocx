---
title: "ocx patch"
---
<!-- doc_type: reference -->

## `ocx patch` {#patch}

Manage patch overlays for a project.

```text
ocx patch <COMMAND>
```

## `ocx patch freeze` {#patch-freeze}

Freeze companion package digests to a snapshot for reproducible builds.

Resolves every companion and descriptor digest in the active patch overlay and writes a `patches.snapshot.json` file beside `ocx.lock`. Once frozen, setting `OCX_PATCH_SNAPSHOT` to that path makes all env composition prefer the pinned digests over live tag lookups.

Works offline: only the local object store is consulted.

Adopting a snapshot is a deliberate opt-in, independent of `--frozen` (which scopes to the package tier). Companions are pinned per `repository:tag`, so a descriptor naming one repository at two tags freezes both versions independently.

Under `--global` the file is written beside `$OCX_HOME/ocx.lock`; the flag goes before the subcommand.

```text
ocx patch freeze
```

## `ocx patch publish` {#patch-publish}

Publish a patch descriptor to the patch registry.

Reads a descriptor JSON file and validates it. Then it pushes the descriptor to the configured patch registry.

The target is either the reserved global repository (`--global`) or the package-specific sub-path for a given base identifier.

The descriptor only references companion packages by identifier. Publish the companion packages separately with `ocx package push`.

Requires network access; fails in offline mode.

`--registry HOST/PATH` overrides the configured `[patches]` tier for this command, which bootstraps a new patch registry without a config block. The descriptor is validated before any network work, so a malformed file fails with its path named.

```text
ocx patch publish [OPTIONS] --descriptor <DESCRIPTOR> [BASE-ID]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--descriptor` | `DESCRIPTOR` | Path to the patch descriptor JSON file to publish |
| `--global` |  | Publish the descriptor as the global descriptor so it applies to every base. Stored at the reserved `global` repository in the patch registry. Mutually exclusive with a base identifier |
| `--registry` | `HOST/PATH` | Patch registry to publish to, as `HOST/PATH` (e.g. registry.corp.example/ocx-patches) |

## `ocx patch sync` {#patch-sync}

Refresh patch descriptors and companion packages from the registry.

Re-fetches every patch descriptor for all installed packages and the global descriptor. Installs any newly-referenced companion packages.

Requires network access.

This command also picks up patches for packages installed before patch configuration was added. All states are re-checked regardless of what was previously recorded.

The scope is every installed package, the tools locked in every known project's `ocx.lock`, and the global descriptor. It is to the patch tier what `ocx index update` is to the index, not what `ocx index sync` is.

Other commands re-check descriptors they have already seen (`install`, `pull`, `lock`, `add`, `update`, `package test`) or fetch only unseen ones (`exec`, `env`). Under a `required` tier a descriptor that was present and is now gone fails them with exit 79 until this command records it as absent.

Under `OCX_PATCH_SNAPSHOT` none of them re-checks anything.

Without `--platform` it resolves every concrete ship platform (linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64, windows/arm64), not only the host. The synced set therefore covers whatever a teammate might run offline.

Pass one `--platform` to narrow it.

```text
ocx patch sync [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |

## `ocx patch test` {#patch-test}

Compose a patch descriptor onto a base locally, without publishing.

Reads a descriptor JSON file and composes its matched companions onto the given base identifier in a scratch store. It then runs a test script, runs a trailing command in the composed environment, or prints the composed environment.

Composes the surface a consumer of the base sees, or the base's own with `--self`.

Lets a maintainer verify a descriptor before publishing it.

Required companion packages must be resolvable (installed locally or pulled from the registry); an unresolvable required companion fails the command. An optional companion that cannot be resolved is warned about and skipped, matching production fail-open.

Nothing is published and `$OCX_HOME` is not modified. With `--script`, a Starlark test script runs against the composed environment.

```text
ocx patch test [OPTIONS] --descriptor <DESCRIPTOR> <BASE-ID> [-- <COMMAND>...]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--companion-archive` | `PATH` | Path to a local archive for a companion package, read without a registry round-trip; needs a `<archive-stem>-metadata.json` sidecar whose identifier names a companion the descriptor lists. Repeatable |
| `--descriptor` | `DESCRIPTOR` | Path to the patch descriptor JSON file to compose |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--platform`, `-p` | `PLATFORM` | Target platform for composing the environment. Defaults to the host platform |
| `--registry` | `HOST/PATH` | Patch registry to compose against, as `HOST/PATH` (e.g. registry.corp.example/ocx-patches) |
| `--script` | `SCRIPT` | Path to a Starlark test script to run in the composed environment. Mutually exclusive with a trailing command |
| `--self` |  | Use the base's own surface, the one its launchers see, instead of its consumers' |

## `ocx patch why` {#patch-why}

Show which companion contributes each patched env var to a base, and by which descriptor rule.

Resolves the base identifier directly and lists every env var the configured patch registry overlays onto it. Each row names the descriptor rule glob that matched and the companion identifier that produced the var.

Traces the surface a consumer of the base sees, or the base's own with `--self`.

A base with no applicable patch reports an empty result, not an error; a companion that applies but adds no var is named instead.

It never consults `ocx.toml`: the base is resolved against the configured `[patches]` registry. Output follows the root `--format` flag, a `Variable | Rule | Companion` table in plain form and an `items` array of `variable`, `rule`, `companion` objects in JSON.

```text
ocx patch why [OPTIONS] <BASE-ID>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--self` |  | Use the base's own surface, the one its launchers see, instead of its consumers' |
