---
title: "Managed configuration in depth"
description: "Where the managed tier sits in the configuration chain, why refresh never blocks, how locks and publishing work."
---
<!-- doc_type: explanation -->

This page explains how the `[managed]` configuration tier behaves once a host has adopted it. It covers the tier's place in the chain, the refresh and offline rules, system locks and what a fleet publisher may put in a payload. The [Configuration page][config-page] introduces the tier and the chain around it.

## Where it sits in the chain {#managed-position}

The seed pointer is part of the ordinary `$OCX_HOME/config.toml` file, the [OCX home tier][tier-ocx-home]. The package it resolves to is a **synthetic fifth tier**. It is a locally cached snapshot, folded in after the three static-file tiers and below [`OCX_CONFIG`][env-config] and [`--config`][arg-config]. The [full precedence table][config-precedence] shows where it sits among every other source.

The snapshot lives at `$OCX_HOME/state/managed-config/snapshot.json`. Only [`ocx config update`][cmd-config-update], [`ocx config setup`][cmd-config-setup], `ocx self setup --managed-config` and the background tick under `refresh = "apply"` write it. It records:

- the source it was fetched from,
- the tag it tracked at that moment,
- the package's top-level manifest digest, which is the tier's drift identity,
- the fetch timestamp,
- the payload text.

A pause file beside it (`pause.json`, written by `ocx config update --pause`) short-circuits the background tick and nothing else. An expired or corrupt pause file reads as absent.

Before OCX merges the snapshot, it checks the embedded provenance. The `source` the snapshot was fetched from must match the effective `source`: an [`OCX_MANAGED_CONFIG`][env-ocx-managed-config] override, or else the seed. The registry and repository must match. When the seed pins a digest, that exact digest must match too.

Tags float within the repository, so a host pinned to an older version with `ocx config update <version>` still passes. A cross-repository mismatch is treated as if no snapshot exists, even when `required = false`.

This closes a real failure mode. A CI job may reuse a cached `$OCX_HOME` from a *different* pipeline, pointed at a different managed-config source. It must never silently inherit that pipeline's mirrors, patch registry or default registry.

The client that fetches the package is built only from **local-only** config tiers. The payload's own `[mirrors]` never contributes to the route used to fetch it. This adds defense in depth to the [one-hop `[managed]` strip](#managed-one-hop), which already keeps a fetched payload from redirecting the tier that fetched it. A compromised payload cannot also redirect the *transport* used for its next refresh.

:::caution[Trust boundary]
The identity gate defends against the wrong content being merged, most often a stale `$OCX_HOME` reused across CI pipelines. It does not defend against an attacker who already has write access to `$OCX_HOME`. The `config.toml`, the snapshot (`state/managed-config/snapshot.json`) and the pause file are ordinary local state. They sit at the same trust level as any other file such an attacker could edit directly.

The tier's digest pins bind what was fetched from the registry at sync time. They are not a tamper-evidence check against what sits on disk afterward.
:::

## Why refresh never blocks a command {#managed-refresh}

Some corporate policy-sync tools contact a central server on every invocation. That suits an always-connected device. It breaks a build tool that must run identically on a laptop on a plane and on a CI runner behind a firewall. OCX never resolves config that way. Every ordinary command reads the snapshot already on disk, with zero network access.

Only three things touch the network:

- an explicit sync with [`ocx config update`][cmd-config-update],
- an [`ocx config setup`][cmd-config-setup] or `ocx self setup --managed-config` run, which reconciles an already-adopted seed every time and not only the first,
- an optional background tick.

The tick never blocks the command it rides along with. It is a throttled, best-effort probe, governed by [`interval`][config-managed-interval]. It does one of three things:

- applies a new snapshot silently (`refresh = "apply"`),
- prints a one-line stderr advisory (`refresh = "notify"`, the default),
- does nothing (`refresh = "manual"`).

A registry that is down, or a network that is unreachable, degrades the tick to a no-op. It never becomes a command failure.

The tick skips itself before contacting the registry in the same three situations that silence OCX's own [update check][env-ocx-no-update-check]:

- inside CI (`CI` set),
- under [`--offline`][arg-offline] or [`OCX_OFFLINE`][env-offline],
- whenever stderr is not a terminal.

Two more conditions gate it: an active [pause][cmd-config-update], and an [`interval`][config-managed-interval] throttle window that has not yet elapsed. [`OCX_NO_CONFIG_REFRESH`][env-ocx-no-config-refresh] is the explicit kill switch on top of all five.

One consequence matters. `refresh = "apply"` never auto-converges a CI runner or another headless host, however the tier is configured. Those hosts converge only through an explicit [`ocx config update`][cmd-config-update].

None of that gates the setup-time re-sync. `ocx config setup` and `ocx self setup --managed-config` reconcile an already-adopted seed inside CI, non-interactively, and regardless of [`OCX_NO_CONFIG_REFRESH`][env-ocx-no-config-refresh]. Only three things skip it:

- [`--offline`][arg-offline] or [`OCX_OFFLINE`][env-offline],
- a digest-pinned seed, which is content-addressed and has nothing to converge toward,
- an in-force [pause][cmd-config-update].

That re-sync is also best-effort rather than tick-like. A fetch failure keeps the existing snapshot and exits 0 instead of degrading to a silent no-op probe.

## One file, many ocx versions {#managed-forward-compat}

A fleet is never on one ocx version. That is the reason this tier exists. The payload an operator publishes is read by whatever binaries the hosts happen to run, including binaries released before the payload was written.

OCX answers by [ignoring what it does not recognize][config-unknown-keys]. It skips an unknown section, and an unknown key inside a known section, and everything else in the file still applies.

The alternative is rejecting the file. That is the failure mode worth naming, because it looks safe. A single new key would take the *entire* payload out of service on every host that had not upgraded. A fleet-wide mirror map and patch registry would vanish at once. Degrading to "the parts I understand" keeps a rollout independent of upgrade order, which makes central publishing safe at all.

The trade is that a typo silently does nothing instead of failing loudly. Three tools catch typos at different points:

- Write payloads against the [config schema][config-schema], so your editor catches them where they are cheap.
- Run [`ocx config test`][cmd-config-test] before publishing to catch the rest.
- Run [`ocx config update --check`][cmd-config-update] afterward to see what a host resolved.

The same tolerance covers a value this binary does not know. A later ocx may add a new [`refresh`][config-managed-refresh] posture. A host running an earlier ocx ignores it with one warning and applies the default. It does not fail the file, so the rest of the section still takes effect.

Tolerance handles *added* keys. A key whose meaning or value shape changes needs the other lever: a new tag family, with fleets moving over as they upgrade. See [rolling out an incompatible change][user-guide-managed-config-incompatible].

## What a payload cannot set {#managed-update}

One section is deliberately outside the managed tier's reach. [`[update]`][config-update] decides whether this host installs a new ocx in the background, and that is a personal choice. A payload that carries `[update]` has the section dropped before the merge. An `ocx.toml` that carries it fails to parse.

The reason is the direction of trust. A fleet publisher or a cloned repository can supply registries, mirrors and patches, because those name where content comes from. Neither should be able to switch on binary replacement on a host that did not ask for it. The checks never run in CI at all, so a fleet has no need to push the setting.

[`[update]`][config-update] never fails a command either. An invalid value is ignored and the next source applies. That holds for the file and for [`OCX_SELF_UPDATE`][env-ocx-self-update], [`OCX_TOOLCHAIN_UPDATE`][env-ocx-toolchain-update] and [`OCX_UPDATE_CHECK_INTERVAL`][env-ocx-update-check-interval]. A setting about convenience must not be able to take a host out of service.

## Offline and `required` {#managed-offline}

[`required`][config-managed-required] decides whether a tier that contributes nothing is fatal. OCX makes that decision against **local disk state**, not network reachability.

- `required = true` (the default) fails closed identically online and offline. It reports `SnapshotRequired` with exit 78 until [`ocx config update`][cmd-config-update] syncs a snapshot.
- `required = false` lets the command proceed with the tier contributing nothing, whether or not the registry is reachable at that moment.

The gate reads what actually reached the merged config, not whether a file is present. A snapshot that matches `source` but whose payload is not a usable config applies nothing. `required = true` fails closed on it too (`SnapshotUnusable`, also exit 78).

An identity check alone would report such a tier satisfied while none of its settings were in force. That would fail open in the one place an operator asked to fail closed. Unknown keys and sections are not "unusable". They fold normally, as the section above describes.

A machine that adopted the tier once and then goes fully offline resolves configuration identically to when it was last online. Examples are a laptop on a plane and an air-gapped runner reusing a warm `$OCX_HOME`. The snapshot is the tier's entire runtime state.

## The seed fields {#managed-fields}

The `[managed]` table in `$OCX_HOME/config.toml` carries four keys. `ocx config setup` or `ocx self setup --managed-config` normally writes them, rather than a human. Both re-serialize the same four fields with their resolved values. Bootstrapping performs a synchronous fetch before it writes the seed, so a network failure leaves no partial state.

- **`source`** is the OCI reference of the managed-config package, `<registry>/<repository>[:<tag>][@<digest>]`. Omitting it, or the whole table, leaves the tier inactive. A present but empty `source = ""` is a hard error, the same guard as an empty `[patches] registry` or `[mirrors]` value. [`OCX_MANAGED_CONFIG`][env-ocx-managed-config] overrides it for one invocation and is never written back.
- **`source` resolution.** A registry-less `source` resolves against the **built-in** default registry (`ocx.sh`), never a configured `[registry] default`. The tier's trust root therefore cannot be redirected by the config it is about to replace. Use a fully qualified reference in corporate seeds.
- **`source` pinned by digest.** The pin binds the tier to that exact content. The `required` gate then accepts only a snapshot carrying that digest, so a drifted registry fails closed until the pin is updated.
- **`required`** (default `true`) is the fail posture from [Offline and `required`](#managed-offline). Under `false` the tier contributes nothing until synced, and a throttle-gated stderr hint replaces the failure.
- **`refresh`** (`apply`, `notify` or `manual`, default `notify`) is the background posture from [Why refresh never blocks a command](#managed-refresh). An unknown value is ignored with one warning and `notify` applies, in the local seed and in a payload alike. The `[update]` keys read the same three values with the same leniency.
- **`interval`** (`\d+[smhd]?`, default `1d`) is the minimum spacing between background probes. `"0"` or `"0s"` disables the throttle, so every eligible invocation probes. `ocx config update` always bypasses it. Bare digits are seconds. The `[update]` table has its own separate `interval` with the same grammar.

## One hop only {#managed-one-hop}

OCX strips a `[managed]` section inside the fetched payload before the merge, with a warning. The tier that fetched a payload can never be redirected or loosened by that same payload. Every other section in the payload merges normally. A payload's `[update]` is dropped too (see [What a payload cannot set](#managed-update)).

## System locks {#managed-locks}

A section declared at the system scope (`/etc/ocx/config.toml`) is locked. No lower tier, no managed payload and, where the section has one, no environment variable can change it. The lock applies before OCX folds in the managed content, the same as for any lower tier. Each section locks in the shape that fits it:

| Section | What the system declaration locks |
|---|---|
| `[registry]` | `default`, unconditionally. There is no `required` field to gate it on. |
| `[registries.<name>]` | Each entry, per entry: `index`, `trusted_hosts` and `insecure`. An explicit `insecure = false` also subtracts the host from [`OCX_INSECURE_REGISTRIES`][env-insecure-registries]. A system tier silent about `insecure` locks nothing. |
| `[mirrors]` | Per host and per role. A plain string locks both roles. An object with only `index` locks the index role and leaves the registry role to a lower tier. |
| `[patches]` | The **system-required** posture. With `required = true` (or no `required` line, which defaults to `true`), no higher tier can redirect, suppress or fail-open the tier, including `OCX_PATCHES` and a per-package `no-patches`. An explicit `required = false` is not locked. |
| `[records]` | The whole block, `dir`, `name` and `required` together, because a collector depends on the sink location and the filename pattern as a pair. See [execution records](../reference/execution-records.md#execution-records-configuration). |
| `[managed]` | A declaration with `required = true` (the default) is itself non-overridable by a lower tier. |
| `[[trust.policy]]` | Each entry **governs alone** for the scopes it matches. Policies otherwise pool across tiers, see [tier precedence](./signing.mdx#identity-matching-tiers). |
| `[trust.sigstore]` | The whole sub-table, per table and not per field. A scalar trust root cannot pool, because two Fulcio CAs are an ambiguity, not a merge. |
| `extra_ca_certs`, `extra_ca_certs_pem` | The pair, once the system file sets either one. See [proxy and CA](../integration/proxy-and-ca.md#ca-system-lock). |

## Publishing to a fleet {#managed-publish}

The payload is an ordinary `config.toml`, and most keys travel as written, `toolchain_dir` included. One publish moves every host's project trees onto a chosen volume. A refused value in a payload is still exit 78, reported as `config.toml` `toolchain_dir`.

Three kinds of value need care. A value that names something on the operator's machine, or that could widen trust, must not arrive over the channel it governs.

```toml
# Payload published with ocx config push
toolchain_dir = "~/.cache/ocx/toolchains"

[mirrors]
"ghcr.io" = "https://artifactory.corp/ghcr-remote"
```

```toml
# $OCX_HOME/config.toml, the seed on each host
[managed]
source   = "internal.company.com/ocx-config:user"
required = true
refresh  = "notify"
interval = "1d"
```

- **Paths are inlined at publish time.** [`ocx config push`][cmd-config-push] reads a path-form `trusted_root`, checks that it parses as a Sigstore trusted root, and publishes it as `trusted_root_json`. It reads a path-form `extra_ca_certs` and publishes it as `extra_ca_certs_pem`. Comments, key order and every other field survive. OCX re-checks the payload against the 64 KiB cap after inlining, and refuses a value that fits as a path but not once inlined.
- **A `kind = "key"` signer by path is refused.** `push` exits 78 and names `key_pem` as the fix. A KMS reference is not a path and travels with the payload. A backend OCX has not implemented is refused as exit 82 (`unsupported_key_backend`). Local tiers leave `key` unrestricted.
- **The consuming side holds back anything that could bootstrap trust.** What a managed payload may contribute depends on whether `source` is pinned by digest:

| Payload key | Unpinned `source` | Digest-pinned `source` |
|---|---|---|
| `trusted_root` as a path | Ignored with a warning: a remote payload cannot name a path on this machine | Ignored with a warning |
| `trusted_root_json` | Ignored with a warning: the trust root would arrive over the channel it exists to verify | Honored |
| `fulcio_url`, `rekor_url` | Ignored with a warning. `fulcio_url` names where the OIDC identity token is sent, and `ocx package push --sbom` has no flag to oppose a config value | Honored |
| `extra_ca_certs` as a path | Ignored with a warning | Ignored with a warning |
| `extra_ca_certs_pem` | Honored for registry, index and forge traffic, because a CA alone grants no signature bypass. The `[trust.sigstore]` client still requires a pinned source | Honored |
| `[shell.consent]` | Table stripped with a warning. An `exclude` carve-out inside it survives, since a withdrawal can only narrow | Honored |
| `[update]` | Dropped before the merge | Dropped |

The client that fetches the snapshot trusts only the local tiers: system, user, `$OCX_HOME`, `OCX_CONFIG` and `--config`. It also trusts [`OCX_EXTRA_CA_CERTS`][env-ocx-extra-ca-certs], and never a CA carried by the payload it is about to fetch. The refresh channel cannot be secured by material it delivers.

:::tip[Learn more]
[Managed-configuration walkthrough][user-guide-managed-config]: corporate onboarding, CI recipe, publisher recipe, staged rollout.
[`[managed]` reference][config-managed]: every field, type, default and error condition.
[`ocx config test` reference][cmd-config-test]: local validation and merge preview before publishing.
[`ocx config update` reference][cmd-config-update]: exit codes, JSON shape and `--check`.
:::


[config-page]: configuration.mdx#managed
[tier-ocx-home]: configuration.mdx#tier-ocx-home
[config-precedence]: configuration.mdx#precedence
[config-unknown-keys]: configuration.mdx#merge-rules
[config-schema]: https://ocx.sh/schemas/config/v1.json
[config-managed]: ../reference/configuration.md#keys-managed
[config-managed-required]: ../reference/configuration.md#keys-managed-required
[config-managed-refresh]: ../reference/configuration.md#keys-managed-refresh
[config-managed-interval]: ../reference/configuration.md#keys-managed-interval
[config-update]: ../reference/configuration.md#keys-update
[env-insecure-registries]: ../reference/environment.md#ocx-insecure-registries
[env-ocx-extra-ca-certs]: ../reference/environment.md#ocx-extra-ca-certs
[env-config]: ../reference/environment.md#ocx-config
[env-offline]: ../reference/environment.md#ocx-offline
[env-ocx-managed-config]: ../reference/environment.md#ocx-managed-config
[env-ocx-no-config-refresh]: ../reference/environment.md#ocx-no-config-refresh
[env-ocx-no-update-check]: ../reference/environment.md#ocx-no-update-check
[env-ocx-self-update]: ../reference/environment.md#ocx-self-update
[env-ocx-toolchain-update]: ../reference/environment.md#ocx-toolchain-update
[env-ocx-update-check-interval]: ../reference/environment.md#ocx-update-check-interval
[arg-config]: ../reference/command-line/index.md#arg-config
[arg-offline]: ../reference/command-line/index.md#arg-offline
[cmd-config-setup]: ../reference/command-line/config.md#config-setup
[cmd-config-test]: ../reference/command-line/config.md#config-test
[cmd-config-update]: ../reference/command-line/config.md#config-update
[cmd-config-push]: ../reference/command-line/config.md#config-push
[user-guide-managed-config]: ../guides/manage-config-centrally.mdx#managed-config
[user-guide-managed-config-incompatible]: ../guides/manage-config-centrally.mdx#managed-config-incompatible
