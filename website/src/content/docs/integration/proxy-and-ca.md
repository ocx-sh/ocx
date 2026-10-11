---
title: "Proxy and Corporate CA"
description: "Make OCX trust an internal certificate authority and reach registries through a proxy."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows how to make OCX trust an internal certificate authority. That lets it reach a registry, an [ocx-index][in-depth-indices-public] or a forge behind a TLS-intercepting proxy, without installing the CA into the operating system.

A corporate CONNECT proxy or TLS-intercepting gateway presents a certificate signed by an internal CA. OCX already merges the [host's own trust store on top of the compiled-in Mozilla roots][env-external-ca-certificates]. Installing the CA system-wide is not always allowed on a CI image or a locked-down workstation. The `extra_ca_certs` and `extra_ca_certs_pem` keys let OCX carry that one root itself.

## Trust a corporate CA with one variable {#ca-quick}

Export [`OCX_EXTRA_CA_CERTS`](#ca-env) with the path to your corporate root in PEM form, then run the command that failed:

```sh
export OCX_EXTRA_CA_CERTS=/etc/pki/corp-root.pem
ocx package install internal/tool:1.2
```

Behind a TLS-intercepting proxy the install succeeds. Without the variable, it fails with an `UnknownIssuer` TLS error and exit `69`. The variable only adds a root, so public registries keep working.

## The trust OCX starts from {#ca-baseline}

OCX verifies registry TLS against a compiled-in copy of the [Mozilla CA root store][mozilla-ca]. This way HTTPS works even on a host with no certificates installed: a scratch or [distroless][distroless] container, a stripped CI image, an air-gapped runner. That is what makes the single static binary self-contained. It never depends on the system trust store being present.

On top of those roots, OCX also loads the host's own trust store and merges the two sets, never replacing the public roots. A certificate the operating system already trusts is honored alongside the bundled ones, a privately installed corporate root included. A public registry and an internal one behind a TLS-intercepting proxy both work in the same invocation, with no verification disabled.

On Linux, two well-known variables change where the host store is read from. They are not OCX configuration and carry no `OCX_` prefix. `SSL_CERT_FILE` replaces only the default bundle file, and `SSL_CERT_DIR` adds directories. The distribution's default certificate directories still load, and the compiled-in roots are unaffected.

| Variable | Meaning |
|----------|---------|
| `SSL_CERT_FILE` | Path to a single file containing one or more certificates in PEM format |
| `SSL_CERT_DIR` | One or more directories, `:`-separated, of PEM certificate files |

As an alternative, point `SSL_CERT_FILE` at a bundle that includes your corporate root. The file takes the place of the default bundle file, and the distribution's default certificate directories still add their roots:

```sh
SSL_CERT_FILE=/etc/pki/corp-root.pem ocx package install internal/tool:1.2
```

:::caution[PEM text, not DER]
Both variables expect PEM armor (`-----BEGIN CERTIFICATE-----`) regardless of file extension, so a `.crt` file loads only if its contents are PEM. A raw DER certificate is ignored. Convert one with `openssl x509 -inform der -in corp.crt -out corp.pem`.
:::

Windows and macOS ignore both variables. The keys in the next section work the same way on every platform and only ever add a root, never replace one. Set both mechanisms at once and all three sets apply together. These are the compiled-in roots, the host store wherever `SSL_CERT_FILE` and `SSL_CERT_DIR` point it, and the extra root.

To skip HTTPS entirely for a development registry that has no certificate, list it in [`OCX_INSECURE_REGISTRIES`][env-ocx-insecure-registries] instead. That is scoped to the hosts you name and never weakens verification for public registries.

## Behind a proxy {#proxy}

On a network where outbound traffic goes through a corporate HTTP proxy, OCX reads the forward-proxy variables [curl][curl-proxy-env], [git][git-http-proxy] and [reqwest][reqwest-proxy] already read. There is no `OCX_PROXY` variable. A proxy configured once, the same way, works for all of them.

| Variable | Applies to |
|----------|------------|
| `HTTPS_PROXY` | `https://` destinations |
| `HTTP_PROXY` | `http://` destinations |
| `ALL_PROXY` | Either scheme, when the scheme-specific variable above is unset |
| `NO_PROXY` | Destinations dialed directly, bypassing every variable above |

Each is checked upper-case first, then lower-case (`https_proxy`, `http_proxy`, `all_proxy`, `no_proxy`). The whole set is read once at process start. An `https://` destination consults `HTTPS_PROXY`, then falls back to `ALL_PROXY`. It never uses `HTTP_PROXY`, whose scheme it does not share. `NO_PROXY` accepts an exact host, a leading-dot domain suffix, a CIDR block, or `*` to exclude every destination.

On Windows and macOS, OCX also reads the operating system's own proxy configuration. That is the Internet Settings in the registry on Windows and the System Configuration framework on macOS. A proxy set through Control Panel or System Settings applies with no environment variable at all.

This matters for more than routing. On many such networks OCX cannot resolve an external hostname itself, only the proxy can. So the SSRF guard that runs before every index-indirected registry dial (see [`trusted_hosts`][config-registries-trusted-hosts]) adapts to a proxied destination. OCX never resolves the host itself. The proxy does, over a `CONNECT` tunnel for `https://` or an absolute-form request line for `http://`.

The guard skips the DNS pre-flight, because there is nothing local to resolve. It still refuses a forbidden IP literal (loopback, private or link-local) written into an index root's pointer. It admits the proxy's own hostname at the connection-time resolver hook. A destination `NO_PROXY` excludes is dialed directly with the full resolve-then-validate guard unchanged.

## Name the root {#ca-keys}

Set one of the two keys at the root of a `config.toml`, outside every section, beside `toolchain_dir`:

```toml
extra_ca_certs = "corp-ca.pem"    # path, relative to THIS config file
```

| Field | Type | Description |
|-------|------|-------------|
| `extra_ca_certs` | string | A path to a file holding one or more `CERTIFICATE`-tagged PEM blocks, read from disk each time a command resolves TLS trust. A relative path resolves against the directory of the `config.toml` that declared it, the same rule as `trusted_root` in [`[trust.sigstore]`][signing-trusted-root-fields]. Mutually exclusive with `extra_ca_certs_pem` |
| `extra_ca_certs_pem` | string | The certificate PEM text inlined verbatim. This is the form a fleet receives, see [Roll it out to a fleet](#ca-fleet). Mutually exclusive with `extra_ca_certs` |

Setting both in the same file is a configuration error, exit `78`. Neither key is an `ocx.toml` key. That file rejects every key it does not recognize, so writing either one there is a parse error (exit 78). A cloned repository can never add a trust root of its own. The default is unset: no root beyond the platform trust store and the compiled-in Mozilla set.

Every root named this way is appended to the platform store and the bundled Mozilla set, never replacing either. That is a stronger guarantee than [`SSL_CERT_FILE` and `SSL_CERT_DIR`][env-external-ca-certificates] give. The two variables only move where the Linux host store is read from, and Windows and macOS ignore them.

The compiled-in Mozilla roots stay put either way. These two keys never touch host-store discovery at all. A public registry and an internal one behind an intercepting proxy both work in the same invocation, without one bundle having to name every root.

Two dials are outside the keys' reach:

- An announce over the GitLab `git` write transport runs a spawned `git`, whose TLS trust is libcurl's. Point `GIT_SSL_CAINFO` (or `GIT_SSL_CAPATH`) at the bundle for that push. OCX passes both through to that `git` process, next to `SSL_CERT_FILE` and `SSL_CERT_DIR`. `OCX_EXTRA_CA_CERTS` reaches OCX's REST client only and is never exported to `git`, where it would replace the whole trust store.
- The public-good Sigstore trust root's own TUF fetch (rung 6 of the [`[trust.sigstore]` ladder][signing-trust-root]) dials through the shared Sigstore HTTP client. Fulcio and Rekor use that same client (`ocx#470`), so these roots reach it too. Pin a [`trusted_root`][signing-trusted-root-fields] instead only for a fully offline host, where even that client's TUF fetch has nowhere to go.

### The environment variable {#ca-env}

For a one-off run, export [`OCX_EXTRA_CA_CERTS`][env-ocx-extra-ca-certs] instead of editing a file. It adds trust only, for the registry, [ocx-index][in-depth-indices-public], forge and Sigstore clients. It never changes how a failure is reported. With the wrong CA, or none, a command against an intercepted host fails with an `UnknownIssuer` TLS error and exit `69`.

One variable carries both forms. A value containing `-----BEGIN` is read as PEM text. Leading comment lines or a `subject=` label before it are accepted, the shape a Fedora or RHEL bundle already has. Anything else is a path. This is the same content sniff the `install.sh`, `install.ps1`, `install.fish`, `install.nu` and `install.elv` scripts at [setup.ocx.sh][setup-home] use.

```sh
export OCX_EXTRA_CA_CERTS=/etc/pki/corp-root.pem
# or, inline:
export OCX_EXTRA_CA_CERTS="-----BEGIN CERTIFICATE-----
MIIB...
-----END CERTIFICATE-----"
```

An empty value is treated as unset. A short, single-line value with no `-----BEGIN` is a path. When that path does not exist, it is echoed in the not-found message like any other misnamed path. Only a value over 256 bytes or spanning lines is withheld, so do not put a secret in this variable.

On Windows, an inline PEM value passed through a `.bat` file or a `cmd /c` wrapper hits cmd.exe's own 8,191-character ceiling. That ceiling is well below, and separate from, the 32,767-character Win32 per-variable limit. It applies to command lines, to `%VAR%` expansions and to inherited variables. A `set` line that long is refused. An expansion is cut at the ceiling, and an inherited variable over it is dropped.

What reaches ocx is then a truncated bundle (exit `78`) or no variable at all (`UnknownIssuer`, exit `69`). The path form sidesteps the limit.

The variable is not part of the set OCX forwards. So a CA supplied only through it does not reach a child launched under `ocx exec --clean`. That command rebuilds the environment from the composed package variables and OCX's own resolved `OCX_*` configuration. The config-key form survives `--clean`, because the child re-reads `config.toml` from disk itself.

[`ocx self setup`][cmd-self-setup] reads the variable in a dedicated phase before it bootstraps anything else. That phase runs before any network access. A later bootstrap or managed-config fetch can then trust the corporate CA.

It persists inline PEM text into `extra_ca_certs_pem` in `$OCX_HOME/config.toml`. A path value is read at that moment and not stored as a path. The installer's own path is typically a temporary file. The stored PEM is a snapshot, so re-run `ocx self setup` with the new file after a CA rotation. See `ocx self setup` for the phase, the JSON report shape and its exit codes.

It writes `extra_ca_certs_pem` and clears a hand-written `extra_ca_certs` path in the same tier, never both. So a file can never carry both spellings. That write, like every other `ocx` edit of `$OCX_HOME/config.toml`, runs under one cross-process lock. A concurrent `ocx self setup --hook` therefore cannot publish over it.

## Fix a refused value {#ca-refusals}

OCX validates the roots at startup for every command, `--offline` included. A refused value never produces a client with fewer roots than configured.

| Condition | Exit |
|---|---|
| The path is unreadable, is not a regular file, or its content exceeds 32 KiB | 74 |
| Inline text (`extra_ca_certs_pem`, or an `OCX_EXTRA_CA_CERTS` value sniffed as text) exceeds 32 KiB | 78 |
| A PEM block's tag is not `CERTIFICATE`, including a `TRUSTED CERTIFICATE` or `X509 CERTIFICATE` block, the tag OpenSSL's `-trustout` option produces | 65 (file) / 78 (inline) |
| No certificate block is found at all | 65 (file) / 78 (inline) |
| A block that does not parse as an X.509 certificate, or that the platform's own TLS stack rejects | 65 (file) / 78 (inline) |
| A `-----BEGIN` line with no matching `-----END` (a bundle cut off mid-copy), so paste the rest | 65 (file) / 78 (inline) |
| Both `extra_ca_certs` and `extra_ca_certs_pem` are set in the same file | 78 |

Every message names the source (the key, the path, or `OCX_EXTRA_CA_CERTS`) and, where one applies, a byte count or a block index. It never prints the certificate's own content, so a value that accidentally holds something else is never echoed into a log. A config-sourced refusal also names the tier whose file set the key: `extra_ca_certs=<path> (user config.toml)`, `extra_ca_certs_pem (system config.toml)`, `($OCX_HOME/config.toml)`, `(managed config)` or `(--config / OCX_CONFIG)`. It never guesses from the value, so a host with three `config.toml` files knows which one to open.

A `TRUSTED CERTIFICATE` or `X509 CERTIFICATE` block strips down to the plain form accepted here with `openssl x509 -in trusted.pem -out plain.pem`.

### What validation does not check {#ca-limits}

Validation proves the bytes parse and that the platform's verifier accepts them as anchors. It tests neither `CA:TRUE` nor expiry, so a pasted leaf or an expired root installs as an anchor on every platform, the same as `NODE_EXTRA_CA_CERTS` and `SSL_CERT_FILE` behave. What the handshake then does differs by platform:

- On Linux, webpki keeps only an anchor's subject and public key, so an expired root still verifies chains.
- On Windows and macOS the OS chain engine evaluates the anchor's own validity, so an expired root fails every dial with `Expired`.
- On Windows the platform verifier consults extra roots only after the platform certificate chain fails. The same revocation policy applies as for any other root in that chain.
- On Windows the exclusive root store built for these roots anchors only a self-signed certificate. It is built without the `CERT_CHAIN_EXCLUSIVE_ENABLE_CA_FLAG` that would let an intermediate CA in the store anchor a chain on its own. A bundle holding only an intermediate certificate (no root) anchors nothing on Windows, even though the identical bundle works on Linux and macOS. Publish the root, not just the intermediate.

## Know where it wins {#ca-precedence}

Unlike `toolchain_dir`, the environment variable here is not the weakest rung. [`OCX_EXTRA_CA_CERTS`][env-ocx-extra-ca-certs], when set and non-empty, wins over every `config.toml` tier, unless the pair is [system-locked](#ca-system-lock), which outranks the variable too.

Below the variable, the two keys follow the [ordinary precedence table][config-precedence] like any other field: env, then `--config` or `OCX_CONFIG`, then `[managed]`, then `$OCX_HOME`, then user, then system. There is no union special case. Contrast `insecure`, which does union.

The two keys are coupled across tiers the same way [`[trust.sigstore]`'s two spellings are][signing-trusted-root-fields]. If a higher tier sets either key, both are taken from that tier. A tier switching from `extra_ca_certs` to `extra_ca_certs_pem` drops the lower tier's path rather than leaving both set. So the cross-tier merge itself can never produce the both-keys ambiguity.

## Lock it system-wide {#ca-system-lock}

Declared at the system scope (`/etc/ocx/config.toml`), the pair becomes non-overridable. The moment the system file sets either `extra_ca_certs` or `extra_ca_certs_pem`, both are taken from it, and no lower tier nor `OCX_EXTRA_CA_CERTS` can add or replace a root. The lock is per pair, not per field, the same shape as the [`[trust.sigstore]` lock][signing-trusted-root-fields]: a lower tier cannot supply `extra_ca_certs_pem` alongside a system `extra_ca_certs`.

A lower-tier attempt is dropped with a warning that names the tier, the locking file and the remedy, never the value:

```text
ignoring extra_ca_certs / extra_ca_certs_pem from <tier>: the pair is locked by /etc/ocx/config.toml; edit the system tier or ask its owner
```

`OCX_EXTRA_CA_CERTS` gets its own warning for the same reason:

```text
ignoring OCX_EXTRA_CA_CERTS: extra_ca_certs / extra_ca_certs_pem are locked by /etc/ocx/config.toml; edit the system tier or ask its owner
```

`--config` and `OCX_CONFIG` are tiers like any other here and are dropped the same way. [`ocx self setup`][cmd-self-setup] honours the lock too: with the pair locked it persists nothing from `OCX_EXTRA_CA_CERTS` and reports `system_locked` instead of `persisted`.

A system file that sets neither key locks nothing. The lock is a consequence of the system tier declaring a root, not a standing policy an empty system file can express. The lock survives [`OCX_NO_CONFIG`][env-no-config]. That flag prunes every discovered tier except a system-scope lock. So a hermetic CI job cannot use the flag to drop out of a corporate CA the system tier pinned.

## Install ocx behind the CA {#ca-install}

A machine with no ocx yet cannot read `OCX_EXTRA_CA_CERTS`. The installer has its own variable, `OCX_INSTALL_CA_BUNDLE`, which trusts the CA for the installer's own downloads. Export `OCX_EXTRA_CA_CERTS` beside it, so the registry is trusted when [`ocx self setup`][cmd-self-setup] first fetches anything, a managed-config seed included.

```sh
export OCX_INSTALL_CA_BUNDLE=/etc/pki/corp-root.pem OCX_EXTRA_CA_CERTS=/etc/pki/corp-root.pem
curl --cacert "$OCX_INSTALL_CA_BUNDLE" -fsSL https://setup.ocx.sh/sh | sh
```

The PowerShell installer has no equivalent option. [Install ocx][install-ocx] describes the workaround.

## Roll it out to a fleet {#ca-fleet}

Both files below are `config.toml`. Only their lifecycle differs, the same split `toolchain_dir` uses. The payload is what the operator publishes with [`ocx config push`][cmd-config-push]:

```toml
extra_ca_certs = "corp-ca.pem"

[mirrors]
"ghcr.io" = "https://artifactory.corp/ghcr-remote"
```

The seed on each host, in `$OCX_HOME/config.toml`, points at it through the [`[managed]`][config-managed] tier:

```toml
[managed]
source   = "internal.company.com/ocx-config:user"
required = true
refresh  = "notify"
interval = "1d"
```

The operator's `corp-ca.pem` path means nothing on a consumer's disk, so [publishing](#ca-publish) rewrites the payload's `extra_ca_certs` into `extra_ca_certs_pem`. On the consuming side, a path-form `extra_ca_certs` arriving from the `[managed]` tier is ignored with a warning, the same rule as a `trusted_root` path.

Unlike `trusted_root_json`, `extra_ca_certs_pem` from a managed payload is honored for registry, index and forge traffic without requiring a digest-pinned `[managed] source`. A certificate authority on its own grants no signature-verification bypass, unlike a Sigstore trust root. The `[trust.sigstore]` client is the one exception: [`ocx package verify`][cmd-package-verify], [`ocx package sign`][cmd-package-sign] and `ocx package push --sbom` accept managed-tier roots only behind a digest-pinned source, and local tiers and the variable always.

One client is deliberately excluded from all of this. The client that fetches the `[managed]` snapshot itself trusts only the local tiers (system, user, `$OCX_HOME`, `OCX_CONFIG` or `--config`) and `OCX_EXTRA_CA_CERTS`. It trusts no CA from the managed payload it is about to fetch. The refresh channel can never be secured by material it delivers.

### A CA the host cannot load {#ca-fleet-unloadable}

[`ocx config push`][cmd-config-push] proves the bundle on the publisher's platform, and every consumer re-runs the same check with its own TLS verifier before it persists a snapshot. A published `extra_ca_certs_pem` this host cannot load is refused there: [`ocx config update`][cmd-config-update] exits `78` naming the block, and the background refresh warns. The previous snapshot stays in force, so every command keeps running and a corrected payload is one more push away.

A snapshot that already carries an unloadable bundle still refuses every command of that host. It may have been adopted by an ocx release without this check, or written by hand. Recover in either of two ways:

- Run `OCX_EXTRA_CA_CERTS=<valid bundle> ocx config update`. The variable outranks every tier, including the managed one, for that invocation.
- Delete the snapshot under `$OCX_HOME/state/managed-config/`, so the tier contributes nothing until the next `ocx config update` replaces it.

:::warning
An unpinned managed CA is not equivalent to `insecure = true`. Without an on-path attacker, an unpinned `extra_ca_certs_pem` from a managed publisher grants nothing. TLS root material never enters OCI content or Sigstore signature verification. With an on-path attacker (a corporate CONNECT proxy included), that root can impersonate every TLS endpoint OCX dials. The exception is the `[trust.sigstore]` client, which is gated behind a digest-pinned source.

For registry and index traffic this is strictly weaker than the already-accepted unpinned `insecure = true` posture. For Sigstore it would otherwise reach the OIDC identity token that `fulcio_url` is gated to protect. That is why that client is held to the stricter rule. The hard guarantee is a digest-pinned `[managed] source`, or a [system-scope lock](#ca-system-lock) pinning the pair from `/etc/ocx/config.toml`. The lock excludes the unpinned managed tier from ever contributing a root at all.

That gate has a usability cost. A fleet running an unpinned `[managed] source` alongside `[[trust.policy]]` auto-verify gets `UnknownIssuer` (exit `69`) behind an intercepting proxy. This hits every install, verify, sign or ambient-OIDC call. The Sigstore client never sees the managed CA. Pin `[managed] source` by digest, or supply the CA through a local config tier or `OCX_EXTRA_CA_CERTS` instead.
:::

## Publish the payload {#ca-publish}

[`ocx config push`][cmd-config-push] reads a path-form `extra_ca_certs` from the payload at publish time. It validates the file the same way a loaded config would (the [refusals table](#ca-refusals) above). It rewrites it into `extra_ca_certs_pem` before pushing. Comments, key order and every other field survive. An `extra_ca_certs_pem` written into the payload directly gets the same validation without the rewrite.

[`ocx config test`][cmd-config-test] refuses the both-keys ambiguity locally (78). The path is read, and either form validated, only at push time. The push exit codes are:

| Condition | Exit |
|---|---|
| A missing path | 79 |
| Permission denied | 77 |
| Another I/O failure | 74 |
| Path content that fails validation, or that is not UTF-8 and so cannot be stored as a TOML string | 65 |
| An inline `extra_ca_certs_pem` that fails validation | 78 |
| Both keys present in the payload (`AmbiguousExtraCaCerts`, beside `AmbiguousTrustRoot`) | 78 |

After inlining, the payload's total size is re-checked against the 64 KiB managed-payload cap. A value that fits as a path but not once inlined is refused rather than silently truncated.

<!-- external -->
[env-external-ca-certificates]: #ca-baseline
[mozilla-ca]: https://wiki.mozilla.org/CA/Included_Certificates
[distroless]: https://github.com/GoogleContainerTools/distroless
[curl-proxy-env]: https://curl.se/docs/manpage.html
[git-http-proxy]: https://git-scm.com/docs/git-config#Documentation/git-config.txt-httpproxy
[reqwest-proxy]: https://docs.rs/reqwest/latest/reqwest/struct.Proxy.html
[setup-home]: https://setup.ocx.sh

<!-- commands -->
[cmd-self-setup]: ../reference/command-line/self.md#self-setup
[cmd-config-push]: ../reference/command-line/config.md#config-push
[cmd-config-update]: ../reference/command-line/config.md#config-update
[cmd-config-test]: ../reference/command-line/config.md#config-test
[cmd-package-verify]: ../reference/command-line/package-consuming.md#package-verify
[cmd-package-sign]: ../reference/command-line/package-publishing.md#package-sign

<!-- environment -->
[env-ocx-extra-ca-certs]: ../reference/environment.md#ocx-extra-ca-certs
[env-ocx-insecure-registries]: ../reference/environment.md#ocx-insecure-registries
[env-no-config]: ../reference/environment.md#ocx-no-config

<!-- internal -->
[config-registries-trusted-hosts]: ../reference/configuration.md#keys-registries-trusted-hosts
[in-depth-indices-public]: ../explanation/public-index.md#public-index
[config-precedence]: ../explanation/configuration.mdx#precedence
[config-managed]: ../explanation/configuration.mdx#managed
[signing-trust-root]: ../explanation/signing.mdx#trust-root
[signing-trusted-root-fields]: ../guides/self-hosted-sigstore.md#trusted-root-fields

<!-- guides -->
[install-ocx]: ../getting-started/install-ocx.mdx
