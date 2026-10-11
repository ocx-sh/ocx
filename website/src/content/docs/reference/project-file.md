---
title: "Project file"
---
<!-- doc_type: reference -->

Keys of `ocx.toml`, from the published project schema. An unknown key is a parse error (exit 78), so `config.toml`-only keys such as `[shell]`, `[update]` and `toolchain_dir` are refused here.

## `activate` {#project-config-activate}

Toolchain-tier `activate` mode: how a project's rendered toolchain reaches a shell —
per-prompt environment composition (`env`, the default), a `PATH` entry for
`<home>/toolchain/active/bin` (`bin`), or neither (`none`). Absent falls through to
`OCX_TOOLCHAIN_ACTIVATE`, then to `env`.

An unrecognized value is a parse error (exit 78). An edit never invalidates `ocx.lock`.

- Type: `string`
- Values:
  - `env`: Compose the toolchain environment on every prompt.
  - `bin`: Put `<home>/toolchain/active/bin` on `PATH` and compose nothing else, so a tool is resolved by its launcher trampoline at invocation time.
  - `none`: Neither.
- Default: `"env"`
- See: [Toolchain activation](../explanation/toolchain-activation.md)

## `env.<name>` {#project-config-env}

Environment variables for the default group; `[env]` in TOML.

A key is a POSIX name and must not start with `OCX_` or `__OCX_`; either is a parse error (exit 78).

Stands to `[group.<name>.env]` as `[tools]` does to
`[group.<name>.tools]`. It does not change which packages resolve, so an
edit never forces a re-lock.

- Type: `string or table`
- See: [Environments](../explanation/environments.mdx)

## `env.<name>.separator` {#project-env-separator}

For `list` only: the string this contribution is joined to the variable's existing value with. Omit it to inherit whatever separator another contributor to the same key declared, or a single space if none did.

It must be non-empty and free of `=`, newline and carriage return. A separator on a non-list entry, an unusable one, or a list value that starts or ends with it is a parse error (exit 78).

- Type: `string`
- See: [Environments](../explanation/environments.mdx)

## `env.<name>.type` {#project-env-type}

`constant` replaces the variable's value; `path` prepends to it; `list` appends to it.

- Type: `string`
- Values:
  - `path`: Prepended to any existing value of the variable.
  - `constant`: Replaces any existing value of the variable.
  - `list`: Appended to any existing value of the variable, joined by its separator.
- See: [Environments](../explanation/environments.mdx)

## `env.<name>.value` {#project-env-value}

The literal value. For `path`, a relative value resolves against the project root; an absolute one passes through.

A `path` value containing the platform path separator is a data error (exit 65): declare one directory per entry.

- Type: `string`
- See: [Environments](../explanation/environments.mdx)

## `group.<name>` {#project-config-groups}

Named additive groups; `[group.<name>]` in TOML. A name matches `^[A-Za-z0-9][A-Za-z0-9._-]*$` and is at most 64 bytes.

`default` and `all` are reserved, ignoring case: declaring `[group.default]` or `[group.all]` is a parse error.

Under `[group.<name>]` only `tools`, `env` and `lazy-mode` are accepted. Anything else,
including a binding written directly there, is a parse error (exit 78).

- Type: `table`
- See: [Project groups](../explanation/project-groups.md)

## `group.<name>.env.<name>` {#project-group-env}

Environment variables applied when this group is selected.

A key is a POSIX name and must not start with `OCX_` or `__OCX_`; either is a parse error (exit 78).

- Type: `string or table`
- See: [Project groups](../explanation/project-groups.md)

## `group.<name>.env.<name>.separator` {#project-group-env-separator}

For `list` only: the string this contribution is joined to the variable's existing value with. Omit it to inherit whatever separator another contributor to the same key declared, or a single space if none did.

It must be non-empty and free of `=`, newline and carriage return. A separator on a non-list entry, an unusable one, or a list value that starts or ends with it is a parse error (exit 78).

- Type: `string`
- See: [Project groups](../explanation/project-groups.md)

## `group.<name>.env.<name>.type` {#project-group-env-type}

`constant` replaces the variable's value; `path` prepends to it; `list` appends to it.

- Type: `string`
- Values:
  - `path`: Prepended to any existing value of the variable.
  - `constant`: Replaces any existing value of the variable.
  - `list`: Appended to any existing value of the variable, joined by its separator.
- See: [Project groups](../explanation/project-groups.md)

## `group.<name>.env.<name>.value` {#project-group-env-value}

The literal value. For `path`, a relative value resolves against the project root; an absolute one passes through.

A `path` value containing the platform path separator is a data error (exit 65): declare one directory per entry.

- Type: `string`
- See: [Project groups](../explanation/project-groups.md)

## `group.<name>.lazy-mode` {#project-group-lazy-mode}

Group-tier `lazy-mode` override: outranks the toolchain tier and is outranked by
`[package."<id>"]` and the CLI flag. Absent inherits from the toolchain tier; it does not
mean `never`.

An edit never invalidates `ocx.lock`.

- Type: `string`
- Values:
  - `never`: Compose eagerly: content materializes before the tool reaches `PATH`.
  - `always`: Compose a shim: content materializes on the first invocation of one of the tool's declared names.
- See: [Project groups](../explanation/project-groups.md)

## `group.<name>.tools.<name>` {#project-group-tools}

Tool bindings for this group. Values are fully-qualified package
references, validated exactly as `[tools]` is.

A binding name matches `^[A-Za-z0-9][A-Za-z0-9._-]*$` and is at most 64 bytes.

- Type: `string`
- See: [Project groups](../explanation/project-groups.md)

## `lazy-mode` {#project-config-toolchain-lazy}

Toolchain-tier `lazy-mode`, the least specific config tier: outranked by `[group.<g>]`,
`[package."<id>"]` and the CLI flag. Absent falls through to `OCX_LAZY_MODE`; it does not
mean `never`.

An edit never invalidates `ocx.lock`.

- Type: `string`
- Values:
  - `never`: Compose eagerly: content materializes before the tool reaches `PATH`.
  - `always`: Compose a shim: content materializes on the first invocation of one of the tool's declared names.
- See: [Deferred packages](../explanation/deferred-packages.mdx)

## `lazy-report` {#project-lazy-report}

Toolchain-tier `lazy-report`. Absent falls through to `OCX_LAZY_REPORT`, then to `silent`.

An edit never invalidates `ocx.lock`.

- Type: `string`
- Values:
  - `silent`: No progress channel is opened for a first-invocation materialization.
  - `progress`: Render progress for a first-invocation materialization, best-effort.
- See: [Deferred packages](../explanation/deferred-packages.mdx)

## `package.<name>` {#project-config-package}

Per-package resolve-time settings; `[package."<id>"]` in TOML, keyed by
the canonical author string `registry/repo[:tag]`.

Resolve-time policy, not a tool binding: a `no-patches` edit never
invalidates `ocx.lock`.

- Type: `table`
- See: [Patches](../explanation/patches.mdx)

## `package.<name>.lazy-mode` {#project-package-lazy-mode}

Package-tier `lazy-mode` override, the most specific config tier: only the CLI flag outranks
it. Absent inherits from the group and toolchain tiers; it does not mean `never`.

An edit never invalidates `ocx.lock`.

- Type: `string`
- Values:
  - `never`: Compose eagerly: content materializes before the tool reaches `PATH`.
  - `always`: Compose a shim: content materializes on the first invocation of one of the tool's declared names.
- See: [Patches](../explanation/patches.mdx)

## `package.<name>.lazy-report` {#project-package-lazy-report}

Package-tier `lazy-report` override. Absent inherits from the toolchain tier.

An edit never invalidates `ocx.lock`.

- Type: `string`
- Values:
  - `silent`: No progress channel is opened for a first-invocation materialization.
  - `progress`: Render progress for a first-invocation materialization, best-effort.
- See: [Patches](../explanation/patches.mdx)

## `package.<name>.no-patches` {#project-package-no-patches}

Decline the site-tier companion overlay for this base.

A system-required patch still applies (enforcement beats opt-out).

- Type: `boolean`
- Default: `false`
- See: [Patches](../explanation/patches.mdx)

## `pinned` {#project-config-pinned}

Toolchain-tier `pinned`: whether composed paths follow the rendered `links/<group>/<entry>`
links (`false`, the default) or pin to digest roots (`true`). Absent falls through to
`OCX_TOOLCHAIN_PINNED`.

An edit never invalidates `ocx.lock`.

- Type: `boolean`
- Default: `false`
- See: [Toolchain activation](../explanation/toolchain-activation.md)

## `tools.<name>` {#project-tools}

Tools in the reserved `default` group (the top-level `[tools]` table in
`ocx.toml`). Values are fully-qualified package references.

A binding name matches `^[A-Za-z0-9][A-Za-z0-9._-]*$` and is at most 64 bytes.

- Type: `string`
- See: [Project toolchain](../guides/project-toolchain.mdx)

## `trust` {#project-trust}

Identity-pinned verification policies; `[[trust.policy]]` in TOML.

Consumed by `ocx package verify`. A project policy applies only to targets that no `config.toml` policy matches; it never pools with them.

Resolve-time policy, not a tool binding: a policy edit never invalidates `ocx.lock`.

- Type: `table`
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy` {#project-trust-policy}

The declared policies. Empty when `[trust]` is present but lists none.

- Type: `array`
- Default: `[]`
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.builder` {#project-trust-policy-builder}

Expected SLSA provenance builder identity, matched against the
provenance predicate during attestation verify.

Ignored in signature mode, without an error.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.scope` {#project-trust-policy-scope}

Which packages this policy applies to: one prefix pattern, or an include/exclude set of them.

A pattern matches on `/`-separated path-segment boundaries, and an absent scope is a catch-all.

```toml
scope = "ghcr.io/acme/*"
scope = { include = ["ghcr.io/acme/*"], exclude = ["ghcr.io/acme/experimental/*"] }
```

- Type: `string or table`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.scope.exclude` {#project-trust-policy-scope-exclude}

Patterns carved back out. Beats `include` wherever both match.

- Type: `array`
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.scope.include` {#project-trust-policy-scope-include}

Patterns the policy covers. Empty is a catch-all.

- Type: `array`
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers` {#project-trust-policy-signers}

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

## `trust.policy.signers.identity` {#project-trust-policy-signers-identity}

Exact expected certificate SAN (byte-equal).

Mutually exclusive with `identity_regexp`.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.identity_regexp` {#project-trust-policy-signers-identity-regexp}

Regex the certificate SAN must match in full (anchored `\A…\z`).

Mutually exclusive with `identity`.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.key` {#project-trust-policy-signers-key}

A key reference (`[scheme://]<rest>`), the same spelling `--key` takes on the command line.

Mutually exclusive with `key_pem`, and refused in a managed-config payload.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.key_pem` {#project-trust-policy-signers-key-pem}

Verbatim SPKI PEM.

Mutually exclusive with `key`.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.kind` {#project-trust-policy-signers-kind}

Selects the variant of this entry; the other fields depend on the value.

- Type: `string`
- Values:
  - `keyless`: `{ kind = "keyless", identity | identity_regexp, oidc_issuer }`.
  - `key`: `{ kind = "key", key | key_pem }`.
  - `unknown`: A `kind` this build does not know — an entry written by a newer ocx.
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.policy.signers.oidc_issuer` {#project-trust-policy-signers-oidc-issuer}

Exact expected OIDC issuer URL (byte-equal).

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.sigstore` {#project-trust-sigstore}

Sigstore trust material for a self-hosted stack (`[trust.sigstore]`).

Read from the operator `config.toml` tiers only. The project `ocx.toml`
also accepts a `[trust]` table, but its `sigstore` sub-table is never
consulted.

- Type: `table`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.sigstore.fulcio_url` {#project-trust-sigstore-fulcio-url}

Default Fulcio base URL for `ocx package sign` when `--fulcio-url` is
omitted.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.sigstore.rekor_upload` {#project-trust-sigstore-rekor-upload}

Fleet-wide default for uploading a key-mode signature to the transparency log, when neither
`--rekor-upload` nor `--no-rekor-upload` is given.

Absent means off; keyless signing always uploads and ignores this field.

- Type: `boolean`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.sigstore.rekor_url` {#project-trust-sigstore-rekor-url}

Default Rekor base URL for `ocx package sign` / `verify` when
`--rekor-url` is omitted.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.sigstore.trusted_root` {#project-trust-sigstore-trusted-root}

Path to a Sigstore trusted-root JSON, or a directory holding
`trusted_root.json`.

A relative path resolves against the directory of the `config.toml` that declared it. It is rewritten to absolute at load time, so the same value names the same file whatever the working directory.

Mutually exclusive with `trusted_root_json`.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)

## `trust.sigstore.trusted_root_json` {#project-trust-sigstore-trusted-root-json}

The trusted-root document inlined verbatim.

This is the form a fleet receives: `ocx config push` reads a path-form
`trusted_root` at publish time and inlines it here. Mutually exclusive
with `trusted_root`.

- Type: `string`
- Default: unset
- See: [Pin a signer identity](../guides/pin-signer-identity.mdx)
