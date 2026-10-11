---
title: "Configuration"
---
<!-- doc_type: reference -->

Keys of `config.toml`, from the published config schema. Tiers, precedence and the kill switch are in [Configuration](../explanation/configuration.mdx).

## Config file locations {#config-file-locations}

OCX reads each file that exists, lowest precedence first. A file over 64 KiB is refused with exit 78.

| Tier | Path |
| --- | --- |
| System | `/etc/ocx/config.toml` |
| User on Linux | `$XDG_CONFIG_HOME/ocx/config.toml`, else `~/.config/ocx/config.toml` |
| User on macOS | `~/Library/Application Support/ocx/config.toml` |
| User on Windows | `%APPDATA%\ocx\config.toml` |
| OCX home | `$OCX_HOME/config.toml`, default `~/.ocx/config.toml` |
| Explicit | [`OCX_CONFIG`](./environment.md#ocx-config) and [`--config`](./command-line/index.md#arg-config) |

## `extra_ca_certs` {#keys-extra_ca_certs}

Path to a PEM file of extra CA certificate(s), trusted for OCI registry, index and forge
traffic in addition to the platform trust store.

A relative path resolves against the directory of the `config.toml` that declared it.

Overridden by the `OCX_EXTRA_CA_CERTS` environment variable, which outranks every
`config.toml` tier unless the pair is declared at system scope.

- Type: `string`
- Default: unset
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `extra_ca_certs_pem` {#keys-extra-ca-certs-pem}

The extra CA certificate bundle inlined verbatim (PEM text).

This is the form a fleet receives; `ocx config push` inlines a path-form `extra_ca_certs` here.

- Type: `string`
- Default: unset
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `managed` {#keys-managed}

Corporate managed-configuration tier (`[managed]`).

Points at an operator-controlled OCI artifact that supplies a plain
`config.toml` payload, such as mirrors, a patches pointer and a default
registry. The payload is synced into local state and merged above the
user config every invocation.

Absent → no managed tier configured (opt-in, seeded by
`ocx self setup --managed-config`).

- Type: `table`
- See: [Manage configuration centrally](../guides/manage-config-centrally.mdx)

## `managed.interval` {#keys-managed-interval}

Background refresh throttle interval, `\d+[smhd]?` (bare = seconds).
Defaults to `"1d"`.

- Type: `string`
- Default: `"1d"`
- See: [Manage configuration centrally](../guides/manage-config-centrally.mdx)

## `managed.refresh` {#keys-managed-refresh}

Background refresh posture. Defaults to `notify`.

`apply` swaps in a drifted snapshot, `notify` advises `ocx config update`, `manual`
refreshes only on `ocx config update`. CI never runs the background check.

An unknown value is ignored with a warning, so the default applies.

- Type: `string`
- Values:
  - `apply`: A change is applied without asking, by the background check only, which runs only on an interactive terminal, outside CI, and online.
  - `notify`: A change prints a stderr notice naming the command that applies it; the check probes the registry but applies nothing.
  - `manual`: The background check is skipped entirely; only an explicit command refreshes.
- Default: `"notify"`
- See: [Manage configuration centrally](../guides/manage-config-centrally.mdx)

## `managed.required` {#keys-managed-required}

Fail posture when the snapshot is required but absent.

- `true` (default) — fail closed: a config error (exit 78) until `ocx config update`
  (or `self setup --managed-config`) syncs one.
- `false` — the tier contributes nothing until synced; a throttle-gated
  stderr hint only.

- Type: `boolean`
- Default: `true`
- See: [Manage configuration centrally](../guides/manage-config-centrally.mdx)

## `managed.source` {#keys-managed-source}

OCI reference for the managed-config artifact, e.g.
`"internal.company.com/ocx-config:user"`.

Absent → no managed tier configured. The `OCX_MANAGED_CONFIG` environment variable
overrides it for one invocation.

- Type: `string`
- See: [Manage configuration centrally](../guides/manage-config-centrally.mdx)

## `mirrors.<name>` {#keys-mirrors}

Per-traffic-host mirrors (`[mirrors."<host>"]`).

Maps a canonical upstream traffic host (e.g. `"ghcr.io"`, `"index.ocx.sh"`) to replacement
endpoint(s), so read traffic goes to a corporate mirror instead of a blocked origin. A bare
string rewrites both traffic roles for that host; a `{registry?, index?}` table splits per
role — `registry` rewrites OCI distribution traffic only, `index` index-tree traffic only.

Replace semantics: no origin fallback. The canonical identifier and content-addressed digest
stay unchanged, so an `ocx.lock` produced behind the mirror remains valid with direct egress
and vice versa.

- Type: `table`
- Overridden by: [`OCX_MIRRORS`](./environment.md#ocx-mirrors) (per host and role; a role it omits still comes from `[mirrors]`)
- See: [Corporate mirror](../guides/corporate-mirror.mdx)

## `mirrors.<name>.index` {#keys-mirrors-index}

Endpoint for index-tree traffic (`/config.json`, `/c`, `/p`) to this
host, if an index role was declared for it.

- Type: `string`
- See: [Corporate mirror](../guides/corporate-mirror.mdx)

## `mirrors.<name>.registry` {#keys-mirrors-registry}

Endpoint for OCI distribution traffic (`/v2`) to this host, if a
registry role was declared for it (a bare string, `{registry: ...}`,
or `{registry: ..., index: ...}`).

- Type: `string`
- See: [Corporate mirror](../guides/corporate-mirror.mdx)

## `patches` {#keys-patches}

Site-tier patch configuration (`[patches]`).

Points at an operator-controlled patch registry that provides companion
packages, such as CA bundles, proxy env vars and license-server endpoints.
They are layered onto unmodified upstream packages at compose time.

Absent → no patch tier configured (opt-in, not required).

- Type: `table`
- See: [Patch packages](../guides/patch-packages.mdx)

## `patches.path` {#keys-patches-path}

Path-template for per-package patch repositories. Default: `"{registry}/{repository}"`.

Placeholders: `{registry}` — the base identifier's registry host, slugified (characters
outside `[a-zA-Z0-9._-]` become `_`); `{repository}` — the base identifier's repository path.

- Type: `string`
- Default: `"{registry}/{repository}"`
- See: [Patch packages](../guides/patch-packages.mdx)

## `patches.registry` {#keys-patches-registry}

OCI registry hosting patch descriptors, e.g.
`"internal.company.com/ocx-patches"`.

Absent → no patch tier configured.

- Type: `string`
- Overridden by: [`OCX_PATCHES`](./environment.md#ocx-patches)
- See: [Patch packages](../guides/patch-packages.mdx)

## `patches.required` {#keys-patches-required}

Fail posture when a matched companion is unavailable.

- `true` (default) — fail closed: abort launch if a required companion
  cannot be resolved.
- `false` — fail open: skip with a warning (suitable for non-security
  companions like a license-server URL or metrics endpoint).

- Type: `boolean`
- Default: `true`
- See: [Patch packages](../guides/patch-packages.mdx)

## `records` {#keys-records}

Execution-record sink (`[records]`).

Designates where OCX writes one JSON resolution record per tool launch —
the resolved package closure with digests, plus the resolved executable.
Declared at SYSTEM scope it clamps: no lower tier can redirect or
disable it.

Absent → no records written (opt-in).

- Type: `table`
- See: [Execution records](../explanation/execution-records.md)

## `records.dir` {#keys-records-dir}

Directory records are written to. Absent → recording is off.

Overridden by the `--records-dir` flag and the `OCX_RECORDS_DIR` environment variable.

- Type: `string`
- See: [Execution records](../explanation/execution-records.md)

## `records.name` {#keys-records-name}

Name template for each record file, e.g. `{time}-{host}-{pid}-{rand}.json`.

Must contain a component that varies per record, or two frames sharing a sink resolve to one path. Defaults to `{time}-{pid}-{rand}.json`.

Overridden by the `--records-name` flag and the `OCX_RECORDS_NAME` environment variable.

- Type: `string`
- Default: `"{time}-{pid}-{rand}.json"`
- See: [Execution records](../explanation/execution-records.md)

## `records.required` {#keys-records-required}

Fail posture when a record cannot be written.

`true` aborts the launch; `false` warns and proceeds. **Config-file only at every tier** —
never settable from the environment or a flag.

Defaults `false` unlocked, `true` when SYSTEM-locked.

- Type: `boolean`
- See: [Execution records](../explanation/execution-records.md)

## `registries.<name>` {#keys-registries}

Named per-registry configuration tables (`[registries.<name>]`).

Every key is an identifier prefix, always — `[registry] default`
never dereferences through this table. The key is the package name's
domain, not the host serving the bytes: an entry keyed on the host
matches no package, so its `trusted_hosts` never applies.

- Type: `table`

## `registries.<name>.index` {#keys-registries-index}

Base URL of the `index.ocx.sh`-protocol index this namespace resolves through, e.g.
`"https://index.ocx.sh"`, or `file:///absolute/path` for a local index read with no server.

**Field presence is the kind marker.** An entry that carries `index` resolves via the
ocx-index protocol; one without it, or no entry at all, resolves as plain OCI, with no
fallback. A `file://` base is checked at startup: a relative path or a remote authority is refused.

`index = ""` resolves the namespace as plain OCI. A `[mirrors]` registry entry for the
namespace in a config file you control suppresses the compiled-in index; the managed tier
and `OCX_MIRRORS` cannot.

- Type: `string`
- See: [Public index](../explanation/public-index.md), [Serve an index](../guides/serve-an-index.md)

## `registries.<name>.insecure` {#keys-registries-insecure}

Contact this registry over plain HTTP instead of HTTPS.

Unioned with `OCX_INSECURE_REGISTRIES`, except that a SYSTEM-scope entry stating
`insecure = false` locks that host shut against the env list too.

- Type: `boolean`
- Default: `false`
- See: [Corporate mirror](../guides/corporate-mirror.mdx)

## `registries.<name>.trusted_hosts` {#keys-registries-trusted-hosts}

SSRF escape hatch for this namespace's physical registry hosts. Each
entry is an exact hostname or a CIDR block whose resolved addresses skip
the default-on private/loopback/link-local/metadata refusal.

- Type: `array`
- See: [Corporate mirror](../guides/corporate-mirror.mdx)

## `registry` {#keys-registry}

Global registry-subsystem settings (`[registry]` section).

- Type: `table`
- See: [Configure ocx](../guides/configure-ocx.mdx)

## `registry.default` {#keys-registry-default}

Default registry for bare identifiers (e.g. `cmake:3.28` expands to
`<default>/cmake:3.28`).

Overridden by the `OCX_DEFAULT_REGISTRY` environment variable. Always a
literal prefix (e.g. `"ghcr.io"`, `"ocx.sh"`) — never resolved through
the `[registries.<name>]` table.

- Type: `string`
- Default: `"ocx.sh"`
- See: [Configure ocx](../guides/configure-ocx.mdx)

## `shell` {#keys-shell}

Shell-integration settings (`[shell]`) — the per-prompt hook and
completions toggles, plus the activation consent whitelist.

**Never read from a project's `ocx.toml`**, only from `config.toml`
tiers: consent read from a repository's own file would let a clone
consent to itself. Consumed by `ocx self activate`, `ocx self setup`
and `ocx shell state`.

- Type: `table`
- See: [Shell integration](../explanation/shell-integration.mdx)

## `shell.completions` {#keys-shell-completions}

Whether shell completions load, with the same precedence as `hook`:
`--completion` / `--no-completion` and a truthy `OCX_NO_COMPLETION` win.

- Type: `boolean`
- Default: unset (follows session interactivity)
- See: [Shell integration](../explanation/shell-integration.mdx)

## `shell.consent` {#keys-shell-consent}

The activation consent whitelist (`[shell.consent]`).

- Type: `table`
- See: [Shell consent](../explanation/shell-consent.mdx)

## `shell.consent.namespaces` {#keys-shell-consent-namespaces}

Source namespaces that activate a project whose whole lock is inside them.

One pattern, or one `{include, exclude}` table where `exclude` beats `include`, so
"everything under `ocx.sh/acme/*` except the one compromised namespace" is spellable.

- Type: `string or table`
- Joined by: [`OCX_CONSENT_NAMESPACES`](./environment.md#ocx-consent-namespaces) (the two are unioned across tiers)
- See: [Shell consent](../explanation/shell-consent.mdx)

## `shell.consent.namespaces.exclude` {#keys-shell-consent-namespaces-exclude}

Patterns carved back out. Beats `include` wherever both match.

- Type: `array`
- See: [Shell consent](../explanation/shell-consent.mdx)

## `shell.consent.namespaces.include` {#keys-shell-consent-namespaces-include}

Patterns the policy covers. Required in the table form; an empty list drops the `[shell.consent]` table with a warning.

- Type: `array`
- See: [Shell consent](../explanation/shell-consent.mdx)

## `shell.consent.paths` {#keys-shell-consent-paths}

Directories that activate unconditionally, modelled on git's `safe.directory`.

An entry is one exact directory, or with a trailing `/*` that directory and everything
beneath it, matched component-wise. A bare `*` grants nothing.

Entries are compared **literally**, after separator and trailing-slash normalization,
against the canonical project directory; an entry naming a symlinked checkout never matches.
A **leading** `~` expands textually against the home directory.

A `paths` grant is drift-blind and writes no stamp, so revoking it is immediate.

- Type: `array`
- Default: `[]`
- Joined by: [`OCX_CONSENT_PATHS`](./environment.md#ocx-consent-paths) (the two are unioned across tiers)
- See: [Shell consent](../explanation/shell-consent.mdx)

## `shell.hook` {#keys-shell-hook}

Whether the per-prompt shell hook runs.

`--hook` / `--no-hook` and a truthy `OCX_NO_HOOK` take precedence; absent means it
follows session interactivity. A higher tier's value wins in either direction, the managed
tier over a user's own file included.

- Type: `boolean`
- Default: unset (follows session interactivity)
- See: [Shell integration](../explanation/shell-integration.mdx)

## `shell.modify_path` {#keys-shell-modify-path}

Whether `ocx self setup` may write PATH surfaces — shell profiles and
the session-PATH stores alike. Absent means true.

- Type: `boolean`
- Default: `true`
- Overridden by: [`OCX_NO_MODIFY_PATH`](./environment.md#ocx-no-modify-path) (outranks this key; `--no-modify-path` outranks both and writes `false`)
- See: [Shell integration](../explanation/shell-integration.mdx)

## `shell.profiles` {#keys-shell-profiles}

Which shell profile files receive the managed ocx block.

Absent means auto-detect fresh on every run; an empty list means write
no profile blocks at all.

- Type: `array`
- Default: unset (detected on every run)
- See: [Shell integration](../explanation/shell-integration.mdx)

## `toolchain_dir` {#keys-toolchain_dir}

Root directory holding every project's toolchain tree, each at `<root>/<project-key>/toolchain/`.

The global toolchain home ignores this key, and the directory need not exist yet.

- Type: `string`
- Default: unset (each project renders at `<project>/.ocx/toolchain`)
- Environment fallback: [`OCX_TOOLCHAIN_DIR`](./environment.md#ocx-toolchain_dir) (read only when no config file sets the key)
- See: [Project toolchain](../guides/project-toolchain.mdx)

## `trust` {#keys-trust}

Identity-pinned verification policies (`[[trust.policy]]`).

Unlike every other section, trust policies **array-append** across the `config.toml` tiers
rather than replace — the operator trust set is the union of system, user and `$OCX_HOME`.
At verify time this operator set takes precedence over the project `ocx.toml`.

Consumed by `ocx package verify`.

- Type: `table`
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy` {#keys-trust-policy}

The declared policies. Empty when `[trust]` is present but lists none.

- Type: `array`
- Default: `[]`
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.builder` {#keys-trust-policy-builder}

Expected SLSA provenance builder identity, matched against the
provenance predicate during attestation verify.

Ignored in signature mode, without an error.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.scope` {#keys-trust-policy-scope}

Which packages this policy applies to: one prefix pattern, or an include/exclude set of them.

A pattern matches on `/`-separated path-segment boundaries, and an absent scope is a catch-all.

```toml
scope = "ghcr.io/acme/*"
scope = { include = ["ghcr.io/acme/*"], exclude = ["ghcr.io/acme/experimental/*"] }
```

- Type: `string or table`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.scope.exclude` {#keys-trust-policy-scope-exclude}

Patterns carved back out. Beats `include` wherever both match.

- Type: `array`
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.scope.include` {#keys-trust-policy-scope-include}

Patterns the policy covers. Empty is a catch-all.

- Type: `array`
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers` {#keys-trust-policy-signers}

The signers this policy accepts: an **ANY-of** set, each entry tagged
`kind = "keyless"` or `kind = "key"`.

```toml
signers = [
  { kind = "keyless", identity = "release@acme.example", oidc_issuer = "https://accounts.google.com" },
  { kind = "key",     key = "etc/acme-release.pub" },
]
```

**Every added entry widens what verifies**, and an empty set is a configuration error, not a
catch-all.

- Type: `array`
- Default: `[]`
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.identity` {#keys-trust-policy-signers-identity}

Exact expected certificate SAN (byte-equal).

Mutually exclusive with `identity_regexp`.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.identity_regexp` {#keys-trust-policy-signers-identity-regexp}

Regex the certificate SAN must match in full (anchored `\A…\z`).

Mutually exclusive with `identity`.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.key` {#keys-trust-policy-signers-key}

A key reference (`[scheme://]<rest>`), the same spelling `--key` takes on the command line.

Mutually exclusive with `key_pem`, and refused in a managed-config payload.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.key_pem` {#keys-trust-policy-signers-key-pem}

Verbatim SPKI PEM.

Mutually exclusive with `key`.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.kind` {#keys-trust-policy-signers-kind}

Selects the variant of this entry; the other fields depend on the value.

- Type: `string`
- Values:
  - `keyless`: `{ kind = "keyless", identity | identity_regexp, oidc_issuer }`.
  - `key`: `{ kind = "key", key | key_pem }`.
  - `unknown`: A `kind` this build does not know — an entry written by a newer ocx.
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.oidc_issuer` {#keys-trust-policy-signers-oidc-issuer}

Exact expected OIDC issuer URL (byte-equal).

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.sigstore` {#keys-trust-sigstore}

Sigstore trust material for a self-hosted stack (`[trust.sigstore]`).

Read from the operator `config.toml` tiers only. The project `ocx.toml`
also accepts a `[trust]` table, but its `sigstore` sub-table is never
consulted.

- Type: `table`
- Default: unset
- See: [Self-hosted Sigstore](../guides/self-hosted-sigstore.md)

## `trust.sigstore.fulcio_url` {#keys-trust-sigstore-fulcio-url}

Default Fulcio base URL for `ocx package sign` when `--fulcio-url` is
omitted.

- Type: `string`
- Default: unset
- See: [Self-hosted Sigstore](../guides/self-hosted-sigstore.md)

## `trust.sigstore.rekor_upload` {#keys-trust-sigstore-rekor-upload}

Fleet-wide default for uploading a key-mode signature to the transparency log, when neither
`--rekor-upload` nor `--no-rekor-upload` is given.

Absent means off; keyless signing always uploads and ignores this field.

- Type: `boolean`
- Default: unset
- See: [Self-hosted Sigstore](../guides/self-hosted-sigstore.md)

## `trust.sigstore.rekor_url` {#keys-trust-sigstore-rekor-url}

Default Rekor base URL for `ocx package sign` / `verify` when
`--rekor-url` is omitted.

- Type: `string`
- Default: unset
- See: [Self-hosted Sigstore](../guides/self-hosted-sigstore.md)

## `trust.sigstore.trusted_root` {#keys-trust-sigstore-trusted-root}

Path to a Sigstore trusted-root JSON, or a directory holding
`trusted_root.json`.

A relative path resolves against the directory of the `config.toml` that declared it. It is rewritten to absolute at load time, so the same value names the same file whatever the working directory.

Mutually exclusive with `trusted_root_json`.

- Type: `string`
- Default: unset
- See: [Self-hosted Sigstore](../guides/self-hosted-sigstore.md)

## `trust.sigstore.trusted_root_json` {#keys-trust-sigstore-trusted-root-json}

The trusted-root document inlined verbatim.

This is the form a fleet receives: `ocx config push` reads a path-form
`trusted_root` at publish time and inlines it here. Mutually exclusive
with `trusted_root`.

- Type: `string`
- Default: unset
- See: [Self-hosted Sigstore](../guides/self-hosted-sigstore.md)

## `update` {#keys-update}

Background update checks (`[update]`): the posture for ocx itself and for the toolchain,
plus the interval between checks.

A personal setting, read from `config.toml` only: a managed payload's `[update]` is
ignored and an `ocx.toml` refuses it. An unknown value never fails a command.

- Type: `table`
- See: [Update ocx](../guides/update-ocx.mdx)

## `update.interval` {#keys-update-interval}

Minimum time between two checks, `\d+[smhd]?` (bare digits = seconds); `"0"` checks on every command. Defaults to `"1d"`.

Overridden by the `OCX_UPDATE_CHECK_INTERVAL` environment variable.

- Type: `string`
- Default: `"1d"`
- See: [Update ocx](../guides/update-ocx.mdx)

## `update.self` {#keys-update-self}

What ocx does when a newer ocx is published: `apply` installs it after the command
finishes, `notify` (default) prints a notice, `manual` never checks.
Overridden by the `OCX_SELF_UPDATE` environment variable.

- Type: `string`
- Values:
  - `apply`: A change is applied without asking, by the background check only, which runs only on an interactive terminal, outside CI, and online.
  - `notify`: A change prints a stderr notice naming the command that applies it; the check probes the registry but applies nothing.
  - `manual`: The background check is skipped entirely; only an explicit command refreshes.
- Default: `"notify"`
- See: [Update ocx](../guides/update-ocx.mdx)

## `update.toolchain` {#keys-update-toolchain}

What ocx does when a locked tool's tag has moved: `notify` (default) prints a notice, `manual` never checks. `apply` is treated as `notify`: only `ocx update` moves a pin.

Overridden by the `OCX_TOOLCHAIN_UPDATE` environment variable.

- Type: `string`
- Values:
  - `apply`: Read as `notify`: only `ocx update` moves a pin.
  - `notify`: A change prints a stderr notice naming the command that applies it; the check probes the registry but applies nothing.
  - `manual`: The background check is skipped entirely; only an explicit command refreshes.
- Default: `"notify"`
- See: [Update ocx](../guides/update-ocx.mdx)
