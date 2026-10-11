---
title: "ocx config"
---
<!-- doc_type: reference -->

## `ocx config` {#config}

Manage the corporate managed-configuration tier.

```text
ocx config <COMMAND>
```

## `ocx config push` {#config-push}

Publish a config file as a managed-config package.

Validates the payload (must parse as an ocx config, must not contain a `[managed]` section, at most 64 KiB) and stages it as `config.toml`. It then pushes the payload as an ordinary package, so fleets adopt it via `ocx self setup --managed-config` and `ocx config update`.

The payload can never redirect the tier that fetches it, hence no `[managed]` section. It is bundled as a tar+gzip layer and pushed with the machinery of `ocx package push`.

With `--cascade`, pushing `user-1.4.2` also advances `user-1.4`, `user-1` and `user`, so fleets track a floating tag while hosts can still pin any version.

```text
ocx config push [OPTIONS] --identifier <IDENTIFIER> <CONFIG>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--cascade` |  | Update rolling variant tags derived from the version tag |
| `--identifier`, `-i` | `IDENTIFIER` | Identifier under which the config is published (e.g. `corp/ocx-config:user-1.4.2`) |
| `--platform`, `-p` | `PLATFORM` | Platform entry written into the package index. Defaults to `any` |

## `ocx config setup` {#config-setup}

Adopt (or clear) the corporate managed-config tier.

The configuration-only counterpart to `ocx self setup --managed-config`: resolves the source (flag, then `OCX_MANAGED_CONFIG`, then the existing seed), synchronously fetches and persists a snapshot, then writes the `[managed]` seed fence. Installs no binary and touches no shell profile - built for automation and CI environments.

The source resolves with the same precedence as `ocx self setup --managed-config`, and the adoption sequence is identical. The snapshot is fetched and persisted first, and the seed fence is written only on success.

A fetch failure during first adoption, or while self-healing a wiped or mismatched snapshot, leaves no partial state and fails.

A bare re-run against an adopted seed reconciles it every time: a newer digest replaces the snapshot (`refreshed`), unchanged content confirms it (`already_adopted`). That re-sync is best-effort: a fetch failure warns on stderr, keeps the snapshot and exits 0 (`refresh_unavailable`).

A bare run with nothing configured at any level is a usage error (exit 64), unlike `ocx self setup`, where an unresolved source is a no-op.

The `managed_config` status is `adopted`, `already_adopted`, `refreshed`, `refresh_unavailable`, `cleared`, `dirty`, `would_adopt` or `would_refresh`.

Details: <https://ocx.sh/docs/reference/command-line/config/#config-setup>

```text
ocx config setup [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--dry-run` |  | Report the intended actions without writing anything |
| `--force` |  | Overwrite a `[managed]` fence that carries user edits (the dirty state) |
| `--managed-config` | `REF` | Adopt this managed-config source; an empty string clears the seed and deletes the snapshot |

## `ocx config test` {#config-test}

Check a config file and preview the configuration it would produce.

Runs the same checks `ocx config push` runs before publishing (parses as an ocx config, carries no `[managed]` section, at most 64 KiB). It reports the configuration this machine would resolve once the payload is adopted: default registry, registries, mirrors and the `[patches]` tier.

It also reports the machine's own `[managed]` posture. Keys this ocx does not recognize are listed as warnings and do not fail the run.

They are equally typos and settings a newer ocx understands.

Keys inside a `[mirrors]` entry are not checked.

Validates locally; nothing is published, adopted, or written. Exits 78 when the file is not a publishable payload.

The preview reproduces the adoption fold order: this machine's discovered tiers first, then the candidate, then any explicit `--config` / `OCX_CONFIG` overlay. The machine's current managed snapshot is never used, because the candidate stands in for it.

An overlay wins over the candidate only for the keys it sets.

The merged result also passes the gates every invocation applies. An invalid `[mirrors."<host>"]` entry fails the command.

That means an unparseable URL, or plain HTTP for a host the candidate does not declare insecure. An empty `[patches] registry` fails the command too.

`--format json` always carries `candidate`, `valid`, `registries`, `mirrors`, `plain_http` and `unknown_keys`, each `[]` when empty. `registry_default`, `patches` and `managed` appear only when their tier is configured.

```text
ocx config test <CONFIG>
```

## `ocx config update` {#config-update}

Refresh the managed-config snapshot from the registry.

Syncs the configured source (or an explicit VERSION - tag, digest, or `tag@digest` - for pins and rollbacks), always bypassing the background-refresh throttle. `--pause <duration>` holds the background tick for up to 7 days; `--resume` clears the pause and syncs; `--check` reports status (drift, pause, pin) without fetching or swapping.

Without a VERSION, `--pause` freezes the on-disk state with no fetch; with one it syncs first and records the pause only once the sync persists. An update without `--pause` clears an active pause.

The snapshot identity is the repository, not the tag, so a pinned snapshot still satisfies the `required` gate of a seed tracking a floating tag. A pause affects only the background tick, never the `required` gate or an explicit update.

`--check` never modifies the pause file, and probes only the registry's top-level manifest digest. Offline or on any probe failure it degrades to a local-state report (`check_unavailable`) rather than claiming the tier is current.

Without `--check` the outcome is `not_configured` (no source resolved), `already_current` (digest matches), `updated` (new snapshot persisted) or `check_unavailable` (`--pause` without a VERSION fetched nothing).

`--check` reports `checked` when the probe found drift. A status report also lists `kill_switches`, the active `OCX_NO_CONFIG_REFRESH` or `OCX_NO_CONFIG` names.

Details: <https://ocx.sh/docs/reference/command-line/config/#config-update>

```text
ocx config update [OPTIONS] [VERSION]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--check` |  | Report the managed-config tier's status without fetching or swapping |
| `--pause` | `DURATION` | Pause the background refresh for a duration (e.g. `4h`, `3d`; max `7d`); an update without `--pause` clears it |
| `--resume` |  | Clear an active pause and refresh immediately |
