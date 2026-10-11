---
title: "Environment"
---
<!-- doc_type: reference -->

Every variable OCX reads, sets or lists as external, public ones first, linking the root flag a variable stands in for; test seams are excluded. [ocx-mirror](https://github.com/ocx-sh/ocx-mirror/blob/main/docs/reference/environment.md) documents its own variables.

## Truthy values {#truthy-values}

A boolean variable reads these spellings in any case.

| Reads as | Spellings |
| --- | --- |
| On | `1`, `y`, `yes`, `on`, `true` |
| Off | `0`, `n`, `no`, `off`, `false` |

- Strict: [`OCX_OFFLINE`](#ocx-offline), [`OCX_FROZEN`](#ocx-frozen), [`OCX_NO_CONSENT`](#ocx-no-consent) and [`OCX_NO_VERIFY`](#ocx-no-verify) exit 78 naming the variable on any other value.
- Lenient: every other boolean variable logs a warning and uses its default.

## `OCX_ALLOW_YANKED` {#ocx-allow-yanked}

Boolean — allow a tag resolving to a yanked index entry. Forwarded to child ocx.

Without it a tag resolve against a yanked entry of the public index exits 65. This holds online and against a local copy with no network.

`ocx index update` does not bypass it. A digest-pinned reference never needs it, and a project resolved from `ocx.lock` is unaffected by a yank.

- Visibility: public
- Value: `bool`
- See: [Public index](../explanation/public-index.md)

## `OCX_ANNOUNCE_GIT_TOKEN` {#ocx-announce-git-token}

The push-only forge credential (the secret half of `git push`'s Basic pair).

Used only under `--transport git`, for every `git` call to the index remote; REST reads keep
using `OCX_ANNOUNCE_TOKEN`. Unset or empty reuses the resolved API credential.

Sent as the Basic password via git's config environment, never in a URL, `argv` or
`.git/config`; the user half is `OCX_ANNOUNCE_GIT_USERNAME`. Stripped from plugin-dispatched
children and from any environment OCX writes, unlike the API half.

- Visibility: public
- Value: `string`
- See: [Announce from CI](../publishing/announce-from-ci.md)

## `OCX_ANNOUNCE_GIT_USERNAME` {#ocx-announce-git-username}

The user half of the git write transport's Basic pair, default `gitlab-ci-token`; a value containing `:`, and an empty one, fall back to the default.

Not a credential, so plugins inherit it from the ambient environment.

- Visibility: public
- Value: `string`
- See: [Announce from CI](../publishing/announce-from-ci.md)

## `OCX_ANNOUNCE_TOKEN` {#ocx-announce-token}

The API half of the forge credential pair (a forge access token).

Read by `ocx package announce` and `ocx package claim` to open or update a pull or merge
request; `--output` needs none. GitHub needs `repo` scope, GitLab `api`.

Under `--transport git` it authenticates only the REST calls and `OCX_ANNOUNCE_GIT_TOKEN`
carries the push. In a GitLab job an unset token falls through to `CI_JOB_TOKEN`, which cannot
write through the API.

- Visibility: public
- Value: `string`
- See: [Announce from CI](../publishing/announce-from-ci.md)

## `OCX_AUTH_<REGISTRY>_TOKEN` {#ocx-auth-registry-token}

Registry token for one registry; `<REGISTRY>` is the registry slug.

The bearer token, or the password for `basic`; ignored unless the type is `bearer` or `basic`.

- Visibility: public
- Value: `string`
- See: [Authenticate](../guides/authenticate.mdx)

## `OCX_AUTH_<REGISTRY>_TYPE` {#ocx-auth-registry-type}

Authentication type for one registry.

`<REGISTRY>` is the registry name with every non-alphanumeric character replaced by `_`, case
kept, so `docker.io` reads `OCX_AUTH_docker_io_TYPE`.

- Visibility: public
- Value: `choice: anonymous, basic, token, bearer`
- See: [Authenticate](../guides/authenticate.mdx)

## `OCX_AUTH_<REGISTRY>_USER` {#ocx-auth-registry-user}

User name for one registry; `<REGISTRY>` is the registry slug.

Read for the `basic` type only; ignored for any other type.

- Visibility: public
- Value: `string`
- See: [Authenticate](../guides/authenticate.mdx)

## `OCX_BINARY_PIN` {#ocx-binary-pin}

Absolute path to the running `ocx`, set on every spawn so a child ocx runs the same binary, not whatever `$PATH` resolves.

Set by ocx on every subprocess it spawns, so the inner `ocx launcher exec` of a generated
launcher runs the binary that installed the package. Unset, launchers fall back to
`$PATH`-resolved `ocx`; set it by hand only to pin a specific binary.

The Windows `.exe` shim treats a defined but empty value as defined, the Unix launcher's
`${OCX_BINARY_PIN:-ocx}` as unset.

- Visibility: public
- Value: `path`
- See: [Launcher forwarding](../explanation/launcher-forwarding.md)

## `OCX_CEILING_PATH` {#ocx-ceiling-path}

Path at which the CWD walk for a project `ocx.toml` stops; relative values join onto the working directory. An explicit `--project` or `OCX_PROJECT` is not bounded by it.

The walk probes the ceiling directory and never ascends above it, so a project file there is
found and one in a parent is not. It already stops at the first `.git` entry; this bounds it
where no repository marks the edge, such as a CI workspace under a shared parent.

- Visibility: public
- Value: `path`
- See: [Project toolchain](../guides/project-toolchain.mdx)

## `OCX_CONFIG` {#ocx-config}

Path to an explicit configuration file. Mirrors `--config`.

Layers on top of the discovered chain (system, user, `$OCX_HOME/config.toml`) at the highest
file-tier precedence. With `--config` both load and the flag wins on conflicting scalars.

A missing file is an error naming the path; empty is unset. Combine with `OCX_NO_CONFIG` for
a hermetic load.

- Visibility: public
- Value: `path`
- Flag: [`--config`](./command-line/index.md#arg-config)

## `OCX_CONSENT_NAMESPACES` {#ocx-consent-namespaces}

Comma-separated OCI namespaces; a project whose whole lock names sources inside them activates without a directory grant or a consent stamp.

Each token is `<registry>/<first-path-segment>`, with or without a trailing `/*`; there is
no whole-registry token, and one malformed token discards the whole variable with a warning.
Additive with `[shell.consent] namespaces`.

The match is against the store's record of the coordinate each digest was pulled under on
this machine. It is not against the lock text, and it never covers a project's own `[env]`
table.

- Visibility: public
- Value: `string`
- See: [Shell consent](../explanation/shell-consent.mdx)

## `OCX_CONSENT_PATHS` {#ocx-consent-paths}

Project directories (separated like `PATH`) the per-prompt shell hook may activate in without a consent stamp, subtrees included.

An entry is an exact canonicalized directory, or with a trailing `/*` that directory and
everything beneath it.

- Visibility: public
- Value: `path_list`
- See: [Shell consent](../explanation/shell-consent.mdx)

## `OCX_DEFAULT_REGISTRY` {#ocx-default-registry}

The registry a bare identifier resolves under; outranks `[registry] default`, else `ocx.sh`. Empty is unset.

Forwarded so every frame of a launch chain agrees.

Environment-only: no CLI flag, so the forwarded value is the one this invocation read. An
empty value clears an inherited setting rather than travelling onward.

- Visibility: public
- Value: `string`
- See: [Configure ocx](../guides/configure-ocx.mdx)

## `OCX_ENV` {#ocx-env}

JSON envelope of the resolved project `[env]` entries plus `ocx exec --env` overrides, or a launcher re-entry silently reverts them.

Decode fails closed on the whole envelope (reserved key, unknown `kind`): a forged entry
can set a value, so the leniency of `OCX_PATCHES` must not be copied.

Keys starting `OCX_` or `__OCX_` are refused in `ocx.toml` `[env]` (exit 78) and in `--env` (exit 64).

- Visibility: public
- Value: `json`
- See: [Environments](../explanation/environments.mdx)

## `OCX_EXTRA_CA_CERTS` {#ocx-extra-ca-certs}

Extra CA roots — a path or inline PEM (contains `-----BEGIN`); empty is unset.

Adds trust for registry, index, forge and Sigstore clients on top of the host store and
compiled-in roots. Wins over every `config.toml` tier unless the `extra_ca_certs` pair is
system-locked, when it is ignored with a warning.

A failure still reports `UnknownIssuer` (exit 69). On Windows an inline PEM can hit
cmd.exe's 8,191-character limit; use a path.

- Visibility: public
- Value: `path_or_pem`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `OCX_FROZEN` {#ocx-frozen}

Boolean — a tag-only reference missing from the local index errors instead of being fetched; digest-pinned content still fetches. Mirrors `--frozen`.

A tag in the local index resolves from cache; a digest or `ocx.lock` pin still fetches. An
unpinned tag missing from it exits 81, as does a frozen `ocx index update`.

Unlike `OCX_OFFLINE` it is not a network ban; patch companions and the managed-config tier
are unaffected. The flag takes precedence; combining with `OCX_REMOTE` is a usage error
(exit 64), with `OCX_OFFLINE` offline wins.

- Visibility: public
- Value: `bool`
- Flag: [`--frozen`](./command-line/index.md#arg-frozen)

## `OCX_GLOBAL` {#ocx-global}

Boolean — select the global toolchain (`$OCX_HOME/ocx.toml`). Mirrors `--global`.

Toolchain-tier commands (`add`, `remove`, `lock`, `update`, `pull`, `exec`, `env`) target
`$OCX_HOME/ocx.toml` instead of a discovered project, with no implicit home-tier fallback
otherwise. Setting it together with `OCX_PROJECT` is a usage error (exit 64).

The global toolchain never composes into project-tier resolution, and `ocx exec` never sees
global packages.

- Visibility: public
- Value: `bool`
- Flag: [`--global`](./command-line/index.md#global-flag)

## `OCX_HOME` {#ocx-home}

The OCX data root — `$OCX_HOME`, else `~/.ocx`.

Forwarded set-always as the resolved root, or an `ocx exec --clean` child (no `HOME`)
resolves another root and misses the package the parent just materialized.

The project walk skips an `ocx.toml` sitting exactly here. That file is the global
toolchain manifest, reached only through `--global` or `OCX_GLOBAL`, never by upward discovery.

- Visibility: public
- Value: `path`
- See: [Storage](../explanation/storage.mdx)

## `OCX_IDENTITY_TOKEN` {#ocx-identity-token}

OIDC bearer token for keyless signing, below `--identity-token-file`/`-stdin`.

Used by `ocx package sign` and `ocx package attest` only when neither
`--identity-token-file` nor `--identity-token-stdin` is given; ambient CI detection and
browser OAuth rank below it. Never forwarded or logged, and stripped from plugin-dispatched
children and from any environment OCX writes.

A short-lived JWT (typically under 10 minutes): fetch a fresh one just before signing.

- Visibility: public
- Value: `string`
- See: [Sign a release](../publishing/sign-a-release.md)

## `OCX_INDEX` {#ocx-index}

Path to the local index directory. Mirrors `--index`.

Swaps the whole local index collection (default `$OCX_HOME/index/`) for a shipped one, never a partial overlay. It is meant for a bundled copy in a GitHub Action, Bazel rule or devcontainer.

`--index` takes precedence.

Under `--remote` or `OCX_REMOTE`, tag- and catalog-addressed lookups bypass it, while
digest-addressed lookups still consult it first.

- Visibility: public
- Value: `path`
- Flag: [`--index`](./command-line/index.md#arg-index)

## `OCX_INSECURE_REGISTRIES` {#ocx-insecure-registries}

Comma-separated `host[:port]` authorities that may be dialled over plain HTTP.

Forwarded, or a child with a narrower set cannot reach a registry the parent just pulled from.

Union with `[registries.<name>] insecure`; only a system-scope `insecure = false` subtracts.
Names match exactly as `host[:port]`.

Also gates plain `http://` `[mirrors]` targets. Credentials travel in the clear; an HTTPS
registry naming an `http` token realm, or redirected to `http://`, is still refused.

- Visibility: public
- Value: `host_list`
- See: [Corporate mirror](../guides/corporate-mirror.mdx)

## `OCX_JOBS` {#ocx-jobs}

Maximum number of root packages pulled in parallel.

Applies to every command that fans out through the package manager (`install`, `pull`,
`package pull`, `exec`, `env`). `0` uses all logical cores, unlike Cargo's `--jobs 0` which
errors.

Negative or non-numeric values are ignored with a warning; unset is unbounded. `--jobs`
takes precedence.

- Visibility: public
- Value: `integer`
- Flag: [`--jobs`](./command-line/index.md#arg-jobs)

## `OCX_KEY_PASSWORD` {#ocx-key-password}

Password for an encrypted signing key; never a flag, since `argv` is visible host-wide.

Pair it with `--key <path>` or `OCX_SIGNING_KEY`; it opens a key and never names one. An unset variable reads as the empty password.

That is the shape of a pair made by pressing enter twice in `cosign generate-key-pair`, so that case is supported rather than an error.

Stripped from plugin-dispatched children and from any environment OCX writes. OCX does not
generate key pairs.

- Visibility: public
- Value: `string`
- See: [Sign a release](../publishing/sign-a-release.md)

## `OCX_LAUNCH_IDENTITIES` {#ocx-launch-identities}

JSON map from a content digest to the `registry/repository[:tag]` names one composition resolved it under; read by `ocx launcher exec` to match targeted patch rules.

Written by ocx only while a `[patches]` tier is in effect; reserved, so `[env]` and
`--env` cannot set it. Never forwarded explicitly, or a launcher nested under another
launcher loses the outer composition's names.

- Visibility: public
- Value: `json`
- See: [Execution records](../explanation/execution-records.md)

## `OCX_LAZY_MODE` {#ocx-lazy-mode}

Lazy materialisation mode (`never` / `always`), the weakest tier of the `lazy-mode` ladder.

Not forwarded: it changes when content materializes, never which digest resolves.

`never` (default) composes eagerly, `always` composes a shim that defers the download to
first invocation. Case-insensitive, unlike the `--lazy-mode` flag and the `ocx.toml` key.

An unrecognized value is ignored with a warning. Overridden by the `--lazy-mode` flag,
`[package."<id>"]`, `[group.<name>]` and the toolchain-level `lazy-mode` key.

- Visibility: public
- Value: `choice: never, always`
- See: [Deferred packages](../explanation/deferred-packages.mdx)

## `OCX_LAZY_REPORT` {#ocx-lazy-report}

Lazy materialisation report (`silent` / `progress`); not forwarded, like `OCX_LAZY_MODE`.

`silent` (default) or `progress`, case-insensitive. `progress` opens a channel on the controlling terminal only when the first-invocation download actually happens.

It falls back to `silent` where no terminal is reachable (Docker build, CI, `setsid`).

Read inside `ocx launcher shim`, never by the composing command. Overridden by
`--lazy-report`, `[package."<id>"]` and the toolchain-level key; there is no group tier.

- Visibility: public
- Value: `choice: silent, progress`
- See: [Deferred packages](../explanation/deferred-packages.mdx)

## `OCX_LOG_CONSOLE` {#ocx-log-console}

Log level for console messages only; outranks `OCX_LOG_LEVEL` there.

- Visibility: public
- Value: `string`
- See: [`--log-level`](./command-line/index.md#arg-log-level)

## `OCX_LOG_LEVEL` {#ocx-log-level}

Log level, as `--log-level` accepts it; the flag wins. Defaults to `info`.

- Visibility: public
- Value: `string`
- Flag: [`--log-level`](./command-line/index.md#arg-log-level)

## `OCX_MANAGED_CONFIG` {#ocx-managed-config}

OCI reference overriding `[managed].source` for this invocation only; empty is unset, `OCX_NO_CONFIG` suppresses it. Forwarded to child ocx.

Never written back to the seed; only `ocx config setup` and `ocx self setup
--managed-config` write it. Resolving the tier never fetches: pair the variable with an
explicit `ocx config update` in CI.

Forwarded to children. Suppressed entirely by `OCX_NO_CONFIG`.

- Visibility: public
- Value: `string`
- See: [Manage configuration centrally](../guides/manage-config-centrally.mdx)

## `OCX_MIRRORS` {#ocx-mirrors}

JSON object mapping an upstream host to a mirror string or `{"registry"?, "index"?}`, the same union `[mirrors."<host>"]` accepts. Forwarded to child ocx.

A string value redirects both the `registry` and `index` roles; the object form splits them.
A role value is `scheme://host[/repo-key-prefix]`.

Replaces the `[mirrors]` entry per host and per role; unlike a config-file `registry` entry
it cannot drop a namespace off the compiled-in index. A malformed value (bad JSON, wrong
shape, `http://` for a host not plaintext-eligible) aborts at startup.

- Visibility: public
- Value: `json`
- See: [Corporate mirror](../guides/corporate-mirror.mdx)

## `OCX_NO_CODESIGN` {#ocx-no-codesign}

Boolean — skip ad-hoc code signing of macOS binaries after installation.

- Visibility: public
- Value: `bool`
- See: [Platforms](../troubleshooting/platforms.mdx)

## `OCX_NO_COMPLETION` {#ocx-no-completion}

Boolean — `ocx self activate` skips the shell-completion block.

`PATH` prepend and the global env eval still run. Use it when a framework such as oh-my-zsh
or a Nix completion store manages completions.

`ocx shell completion` without `--if-enabled` generates the script regardless; with it, this
variable is the first rung of the policy that decides whether anything prints.

- Visibility: public
- Value: `bool`
- See: [Shell integration](../explanation/shell-integration.mdx)

## `OCX_NO_CONFIG` {#ocx-no-config}

Boolean — skip the discovered config-tier chain; explicit `--config` / `OCX_CONFIG` paths still load and a system-scope lock survives. Forwarded to child ocx.

Also drops the `[managed]` snapshot and the `OCX_MANAGED_CONFIG` read. A system-scope policy
in `/etc/ocx/config.toml` still loads, filtered to its locked sections, and an unreadable
system file still aborts (exit 78).

Env-only: there is no `--no-config` flag. Hermetic pattern: `OCX_NO_CONFIG=1 ocx --config
/ci/ocx.toml ...`.

- Visibility: public
- Value: `bool`
- See: [Configuration](../explanation/configuration.mdx)

## `OCX_NO_CONFIG_REFRESH` {#ocx-no-config-refresh}

Boolean — kill switch for the managed-config background refresh; `ocx config update` still works.

Forwarded, or a child's refresh replaces the managed tier with config the parent never saw.

Silences both the `apply` and `notify` postures of the background tick. `ocx config update`
still runs; `ocx self setup` and `ocx config setup` still re-sync an adopted seed regardless
(use `--offline` to skip that).

The tick is also off when `CI` is truthy, `OCX_OFFLINE` is set or stderr is not a terminal.
Distinct from `OCX_NO_UPDATE_CHECK`, which covers the ocx self-update notice.

- Visibility: public
- Value: `bool`
- See: [Managed configuration](../explanation/managed-configuration.md)

## `OCX_NO_CONSENT` {#ocx-no-consent}

Boolean — commands that stamp shell-activation consent as a side effect write none; `--consent` / `--no-consent` outrank it and `ocx shell allow` ignores it.

Forwarded, else a script run by `ocx exec` that calls `ocx pull` stamps after all.

Covers `add`, `remove`, `lock`, `update`, `upgrade`, `pull`, `exec` and `init`. `ocx exec
--no-consent` sets it on the child too; `--consent` does not clear it.

Set it in a pipeline or image, not an interactive profile: exported globally, growing a lock
past the stamped sources leaves the project inert.

- Visibility: public
- Value: `bool`
- See: [Shell consent](../explanation/shell-consent.mdx)

## `OCX_NO_HOOK` {#ocx-no-hook}

Boolean — disables the per-prompt shell reconciler; `ocx self activate` still runs at shell start.

`ocx self activate` still prepends `PATH`, loads completions and evals the global env, but
registers no per-prompt hook. Read once at shell start, so exporting it mid-session takes
effect at the next shell.

The only variable that makes a shell wholly inert: `OCX_NO_CONFIG` leaves explicit
`--config` files and the consent grants active. Boolean, not tri-state; `--hook` is the
positive channel.

- Visibility: public
- Value: `bool`
- See: [Shell integration](../explanation/shell-integration.mdx)

## `OCX_NO_MODIFY_PATH` {#ocx-no-modify-path}

Boolean — `ocx self setup` modifies no shell profile. Mirrors `--no-modify-path`.

Suppresses both PATH surfaces on every platform, the managed profile blocks and the
session-level registration; each location still shows in the summary as `skipped_opt_out`.
Second rung of the ladder: `--no-modify-path`, this variable, `[shell] modify_path`, then
allowed.

The opt-out persists as `modify_path = false`; there is no positive flag. Only truthy values
enable it.

- Visibility: public
- Value: `bool`
- Flag: [`--no-modify-path`](./command-line/self.md#self-setup)

## `OCX_NO_PROJECT` {#ocx-no-project}

Boolean — skip the CWD walk and `OCX_PROJECT`; explicit `--project` paths still load. Forwarded only when this invocation resolved no project.

Skips the CWD walk and ignores `OCX_PROJECT`; explicit `--project` still loads. Env-only: no
`--no-project` flag.

Forwarded so children also skip discovery, except when an explicit `--project` is present,
in which case the path is forwarded instead. Hermetic pattern: `OCX_NO_PROJECT=1 ocx
--project /ci/ocx.toml exec -- ...`.

- Visibility: public
- Value: `bool`
- See: [Project toolchain](../guides/project-toolchain.mdx)

## `OCX_NO_UPDATE_CHECK` {#ocx-no-update-check}

Boolean — kill switch for the background self-update and toolchain drift checks; forwarded to child ocx.

The checks are also off when `CI` is truthy, `OCX_OFFLINE` is set, stderr is not a terminal, or the command is `version`, `about` or `shell completion`. The drift notice additionally skips `update`, `lock`, `add`, `remove` and `init`.

This variable also suppresses the throttle state write.

Distinct from `OCX_NO_CONFIG_REFRESH`, which silences only the managed-config tick.

- Visibility: public
- Value: `bool`
- See: [Update ocx](../guides/update-ocx.mdx)

## `OCX_NO_VERIFY` {#ocx-no-verify}

Boolean — skip the policy-gated auto-verify on install/pull; `--no-verify` wins. Forwarded to child ocx.

Skips the policy-gated auto-verify on `install`, `pull` and every command that auto-installs on demand (`exec`, `env`, `package exec`, `package env`, patch discovery). Only `install` and `pull` carry the `--verify` / `--no-verify` flag, which outranks this variable.

A skipped policy-covered package prints one `warning:` line per invocation.

With no `[[trust.policy]]` covering a package, verification does not run regardless.

- Visibility: public
- Value: `bool`
- See: [Verify installs](../guides/verify-installs.mdx)

## `OCX_OFFLINE` {#ocx-offline}

Boolean — disables network access when truthy. Mirrors `--offline`.

Tag-to-digest resolution must come from the local index or a digest pin; an unpinned tag
missing locally errors immediately. `--offline` takes precedence.

With `OCX_REMOTE` it is pinned-only mode. For `ocx package verify` it scopes to the Sigstore trust services (Rekor key, TUF), not the registry.

It needs a pinned Rekor key from a trust-root rung or the cache. Without one it exits 81 and never skips verification.

- Visibility: public
- Value: `bool`
- Flag: [`--offline`](./command-line/index.md#arg-offline)

## `OCX_PATCHES` {#ocx-patches}

JSON object encoding the resolved `[patches]` config; forwarded only when configured.

- Visibility: public
- Value: `json`
- See: [Patch packages](../guides/patch-packages.mdx)

## `OCX_PATCH_SNAPSHOT` {#ocx-patch-snapshot}

Path to the patch snapshot file `ocx patch freeze` writes, whose pinned digests win over live tag lookups. Forwarded to child ocx.

While set, every command but `ocx patch sync` (refused, exit 78) and `ocx patch freeze` uses
only the snapshot's pinned digests and records no pin. A companion the snapshot omits is
absent, and a required one fails with exit 79, as does a missing pinned blob under
`--offline`.

A format version this ocx cannot read exits 65. Independent of `--frozen`; `ocx clean` roots
the snapshot's companions only while this is set.

- Visibility: public
- Value: `path`
- See: [Patch packages](../guides/patch-packages.mdx)

## `OCX_PROJECT` {#ocx-project}

Path to an explicit project `ocx.toml`. Mirrors `--project`.

A file loads directly and a directory resolves to `<dir>/ocx.toml`. Precedence: `--project`,
this variable, the CWD walk; `OCX_NO_PROJECT` prunes the last two.

A path that does not exist exits 79, a directory with no `ocx.toml` exits 64, an unreadable
one exits 74. Empty is unset.

Explicit paths follow symlinks; the CWD walk rejects a symlinked `ocx.toml`.

- Visibility: public
- Value: `path`
- Flag: [`--project`](./command-line/index.md#arg-project)

## `OCX_QUIET` {#ocx-quiet}

Boolean — suppresses the stdout report and the transfer progress bars.

- Visibility: public
- Value: `bool`
- Flag: [`--quiet`](./command-line/index.md#arg-quiet)

## `OCX_RECORDS_DIR` {#ocx-records-dir}

Directory execution records are written to; absent or empty turns recording off. Forwarded so every frame of a launch chain records into the outermost sink.

Equivalent to `--records-dir` on `ocx exec` and `ocx package exec` and to the `[records]
dir` key. Unset at every tier means no record and no added I/O.

Forwarded, so a launcher re-entry (`ocx launcher exec`, which takes no flags) writes to the
same sink. The flag overrides it; a system-scope `[records]` declaration locks the section
and this variable then has no effect.

- Visibility: public
- Value: `path`
- See: [Execution records](../explanation/execution-records.md)

## `OCX_RECORDS_NAME` {#ocx-records-name}

Filename template for each execution record; forwarded alongside `OCX_RECORDS_DIR`.

No `OCX_RECORDS_REQUIRED` exists: fail-closed recording is config-file-only operator policy.

Equivalent to `--records-name` and the `[records] name` key; the flag overrides it. The
placeholders are `{time}`, `{pid}`, `{rand}` and `{host}`; an unrecognized one is a
configuration error (exit 78), never a literal.

Needs a sink from `OCX_RECORDS_DIR`, `--records-dir` or `[records] dir`. Locked with the
rest of `[records]` under a system-scope policy.

- Visibility: public
- Value: `string`
- See: [Execution records](../explanation/execution-records.md)

## `OCX_REMOTE` {#ocx-remote}

Boolean — uses the remote index by default when truthy. Mirrors `--remote`.

Routes tag list, catalog and tag resolution to the registry. Pure-query commands (`ocx index
list`, `ocx index catalog`, `ocx package description pull`) persist nothing; run `ocx index
update` to refresh the snapshot.

Digest-addressed reads still consult the local index first. `--remote` takes precedence.

- Visibility: public
- Value: `bool`
- Flag: [`--remote`](./command-line/index.md#arg-remote)

## `OCX_SELF_UPDATE` {#ocx-self-update}

`RefreshPolicy` wire value for ocx's own update check; beats `[update] self`. Not forwarded.

`notify` (default) prints a notice naming `ocx self update`, `apply` installs the newer ocx
after the command finishes without changing its exit code, `manual` never checks. Lowercase
only; an unrecognized value is ignored at debug level and falls through.

Beats the config file. `apply` needs a terminal on stderr, `CI` unset, online, and
`OCX_NO_UPDATE_CHECK` unset, and does nothing for an ocx that `ocx self setup` did not
install.

- Visibility: public
- Value: `choice: apply, notify, manual`
- See: [Update ocx](../guides/update-ocx.mdx)

## `OCX_SIGNING_KEY` {#ocx-signing-key}

The signing key PEM itself, for `--key env://OCX_SIGNING_KEY`; any other `env://` name works but is not scrubbed from plugins.

Holds the PEM, never a path: a cosign `ENCRYPTED SIGSTORE PRIVATE KEY` envelope when
signing, an SPKI public key when a `[[trust.policy]]` signer names it. For runners with no
writable disk.

Unset or empty exits 74 naming the variable; over 64 KiB exits 65. Stripped from plugin-dispatched
children and from any environment OCX writes; a key held in any other `env://` name is inherited by plugins and launchers.

- Visibility: public
- Value: `string`
- See: [Sign a release](../publishing/sign-a-release.md)

## `OCX_SIGSTORE_TRUSTED_ROOT` {#ocx-sigstore-trusted-root}

Path (bare or `file://`) to a Sigstore trusted-root JSON document, or a directory holding `trusted_root.json`, that `ocx package verify` loads its trust material from.

The `--sigstore-trusted-root` flag takes precedence. The root must carry the Fulcio CAs, the CT log keys and the
pinned Rekor key together; CA anchors alone are refused with exit 78.

Bypasses the TUF fetch, so keeping it current is the operator's job.

With `OCX_OFFLINE` it is the fully offline path. Ladder after the flag: this variable,
`[trust.sigstore]`, `$OCX_HOME/sigstore/trusted-root.json`, the cache, then TUF.

- Visibility: public
- Value: `path`
- See: [Self-hosted Sigstore](../guides/self-hosted-sigstore.md)

## `OCX_TOOLCHAIN_ACTIVATE` {#ocx-toolchain-activate}

Toolchain activation mode (`env` / `bin` / `none`), the weakest tier of the `activate` ladder. Not forwarded, like `OCX_LAZY_MODE`.

`env` composes the toolchain environment every prompt (default), `bin` puts the home's
`active/bin` on `PATH` only, `none` withdraws what ocx owns. Case-insensitive, not trimmed.

An unrecognized value warns and falls through to the next tier; the `ocx.toml` key is a
parse error (exit 78). Sits below the `ocx.toml` `activate` key.

- Visibility: public
- Value: `choice: env, bin, none`
- See: [Toolchain activation](../explanation/toolchain-activation.md)

## `OCX_TOOLCHAIN_DIR` {#ocx-toolchain_dir}

Root under which project toolchain homes render — the env tier of `toolchain_dir`.

Weakest tier: `config.toml` wins. A refused root exits 78.

- Visibility: public
- Value: `path`
- See: [Project toolchain](../guides/project-toolchain.mdx)

## `OCX_TOOLCHAIN_PINNED` {#ocx-toolchain-pinned}

Boolean — composed paths pin to digest roots; the weakest tier of the `pinned` ladder.

Unset and an explicit `false` are different answers. Below `--pinned` / `--no-pinned` and
the `ocx.toml` `pinned` key.

True names the digest roots `ocx.lock` pins, so a later `ocx update` does not reach that
environment; false follows the rendered `links/<group>/<entry>` links, the default.

A root entry whose link is absent or stale composes on its digest path; dependency paths
always do. An unparseable value warns and falls through.

- Visibility: public
- Value: `tri_bool`
- See: [Toolchain activation](../explanation/toolchain-activation.md)

## `OCX_TOOLCHAIN_UPDATE` {#ocx-toolchain-update}

`RefreshPolicy` wire value for the toolchain drift check; beats `[update] toolchain`. Not forwarded.

`notify` (default) prints one stderr line per toolchain file with drift, naming `ocx
update`; `apply` is read as `notify`, since only `ocx update` moves a pin; `manual` never
checks. Lowercase only; other values are ignored at debug level.

The check compares the lock's platform digests with the registry without writing the local index. It probes a project only when it is consented.

It stays silent on a missing or stale lock, a registry error or a 5-second timeout.

- Visibility: public
- Value: `choice: apply, notify, manual`
- See: [Update ocx](../guides/update-ocx.mdx)

## `OCX_UPDATE_CHECK_INTERVAL` {#ocx-update-check-interval}

Throttle interval of the background update checks, `\d+[smhd]?` (bare digits = seconds).

Beats `[update] interval`. Not forwarded: a personal preference, like `OCX_LAZY_MODE`.

Unset means `1d`; `0` checks on every eligible invocation; units are `s`, `m`, `h`, `d`. Surrounding whitespace is trimmed.

An invalid value is logged at debug level and falls through to `[update] interval`.

The check waits for a registry round trip and never fails the command. It is also suppressed
when stderr is not a terminal or `CI` is set, whatever the interval; `ocx self update`
always bypasses it.

- Visibility: public
- Value: `string`
- See: [Update ocx](../guides/update-ocx.mdx)

## Internal {#internal}

Variables OCX writes and reads for its own plumbing. Do not set them by hand unless an entry says so.

## `__OCX_ENV_PWD` {#ocx-env-pwd}

Elvish only: the recording shell's pid and `$pwd`, so the hook re-runs on a directory change.

Holds the pid, `$pwd`, the direnv/mise yield sentinels and the two consent-grant variables,
space-joined; never set it by hand.

- Visibility: plumbing
- Value: `string`

## `__OCX_ENV_STATE` {#ocx-env-state}

The per-prompt reconciler's private ledger of what it last applied to the shell.

The ledger that tells a value ocx wrote from one you typed. Crossing into the interactive shell makes it a user-facing contract despite the private name.

`unset __OCX_ENV_STATE` (`unset-env` in elvish) repairs a stale or corrupted shell, at the cost of the reconciler's memory of what it may restore.

Reserved: a package cannot declare it and `--env` cannot set it.

- Visibility: plumbing
- Value: `string`

## External {#external}

Variables set by the system or by other tools, which OCX reads.

## `ACTIONS_ID_TOKEN_REQUEST_TOKEN` {#external-actions-id-token-request-token}

Bearer token for `ACTIONS_ID_TOKEN_REQUEST_URL`.

- Visibility: foreign
- Value: `string`

## `ACTIONS_ID_TOKEN_REQUEST_URL` {#external-actions-id-token-request-url}

URL the runner serves OIDC tokens from.

- Visibility: foreign
- Value: `string`

## `ALL_PROXY` {#external-all-proxy}

Proxy URL for every scheme.

- Visibility: foreign
- Value: `string`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `CI` {#external-ci}

Set by most CI providers. When truthy, the background update check and the managed-config refresh are skipped; see `OCX_NO_UPDATE_CHECK` and `OCX_NO_CONFIG_REFRESH`.

- Visibility: foreign
- Value: `bool`
- See: [Unattended CI](../integration/unattended-ci.md)

## `CIRCLE_OIDC_TOKEN_V2` {#external-circle-oidc-token-v2}

CircleCI OIDC token.

- Visibility: foreign
- Value: `string`

## `CI_COMMIT_SHA` {#external-ci-commit-sha}

Commit the GitLab pipeline runs on.

- Visibility: foreign
- Value: `string`

## `CI_JOB_TOKEN` {#external-ci-job-token}

GitLab CI job token.

- Visibility: foreign
- Value: `string`

## `CI_JOB_URL` {#external-ci-job-url}

URL of the GitLab CI job.

- Visibility: foreign
- Value: `string`

## `CI_PIPELINE_CREATED_AT` {#external-ci-pipeline-created-at}

Creation time of the GitLab pipeline.

- Visibility: foreign
- Value: `string`

## `CI_PROJECT_PATH` {#external-ci-project-path}

`group/project` path of the GitLab project.

- Visibility: foreign
- Value: `string`

## `CI_PROJECT_URL` {#external-ci-project-url}

URL of the GitLab project.

- Visibility: foreign
- Value: `string`

## `CLICOLOR` {#external-clicolor}

`0` disables colour output.

- Visibility: foreign
- Value: `string`

## `CLICOLOR_FORCE` {#external-clicolor-force}

Non-`0` forces colour output.

- Visibility: foreign
- Value: `string`

## `DIRENV_DIR` {#external-direnv-dir}

Set by direnv inside an allowed directory.

- Visibility: foreign
- Value: `string`

## `DOCKER_CONFIG` {#external-docker-config}

Docker credential-store directory, read by the credential helper.

- Visibility: foreign
- Value: `path`
- See: [Authenticate](../guides/authenticate.mdx)

## `GITHUB_ACTIONS` {#external-github-actions}

`true` inside GitHub Actions.

- Visibility: foreign
- Value: `bool`

## `GITHUB_ACTOR` {#external-github-actor}

Login of the user that triggered the workflow.

- Visibility: foreign
- Value: `string`

## `GITHUB_ACTOR_ID` {#external-github-actor-id}

Numeric id of the user that triggered the workflow.

- Visibility: foreign
- Value: `string`

## `GITHUB_ENV` {#external-github-env}

File GitHub Actions reads exported variables from.

- Visibility: foreign
- Value: `path`

## `GITHUB_PATH` {#external-github-path}

File GitHub Actions reads `PATH` additions from.

- Visibility: foreign
- Value: `path`

## `GITHUB_REPOSITORY` {#external-github-repository}

`owner/repo` of the workflow.

- Visibility: foreign
- Value: `string`

## `GITHUB_RUN_ID` {#external-github-run-id}

Workflow run id.

- Visibility: foreign
- Value: `string`

## `GITHUB_SERVER_URL` {#external-github-server-url}

GitHub server URL.

- Visibility: foreign
- Value: `string`

## `GITHUB_SHA` {#external-github-sha}

Commit the workflow runs on.

- Visibility: foreign
- Value: `string`

## `GITLAB_CI` {#external-gitlab-ci}

`true` inside GitLab CI.

- Visibility: foreign
- Value: `bool`

## `GITLAB_USER_ID` {#external-gitlab-user-id}

Numeric id of the GitLab user that started the job.

- Visibility: foreign
- Value: `string`

## `GITLAB_USER_LOGIN` {#external-gitlab-user-login}

Login of the GitLab user that started the job.

- Visibility: foreign
- Value: `string`

## `GIT_SSL_CAINFO` {#external-git-ssl-cainfo}

CA bundle git uses.

- Visibility: foreign
- Value: `path`

## `GIT_SSL_CAPATH` {#external-git-ssl-capath}

CA directory git uses.

- Visibility: foreign
- Value: `path`

## `HOME` {#external-home}

The user's home directory.

- Visibility: foreign
- Value: `path`

## `HOMEDRIVE` {#external-homedrive}

Drive of the Windows home directory. Passed only to the git process `ocx package announce` runs; never used to derive paths.

- Visibility: foreign
- Value: `string`

## `HOMEPATH` {#external-homepath}

Path of the Windows home directory below `HOMEDRIVE`. Passed only to the git process `ocx package announce` runs; never used to derive paths.

- Visibility: foreign
- Value: `string`

## `HTTPS_PROXY` {#external-https-proxy}

HTTPS proxy URL.

- Visibility: foreign
- Value: `string`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `HTTP_PROXY` {#external-http-proxy}

HTTP proxy URL.

- Visibility: foreign
- Value: `string`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `MISE_SHELL` {#external-mise-shell}

Set by mise's shell activation.

- Visibility: foreign
- Value: `string`

## `NO_COLOR` {#external-no-color}

Disables colour output when set to any non-empty value.

- Visibility: foreign
- Value: `string`

## `NO_PROXY` {#external-no-proxy}

Hosts reached without a proxy.

- Visibility: foreign
- Value: `string`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `PATH` {#external-path}

Executable search path.

- Visibility: foreign
- Value: `path_list`

## `RUST_LOG` {#external-rust-log}

Log filter of the tracing subscriber.

Fallback filter, read only when neither `OCX_LOG_CONSOLE` nor `OCX_LOG_LEVEL` is set.

- Visibility: foreign
- Value: `string`

## `SIGSTORE_ID_TOKEN` {#external-sigstore-id-token}

OIDC token for keyless signing, from any provider.

- Visibility: foreign
- Value: `string`

## `SOURCE_DATE_EPOCH` {#external-source-date-epoch}

Fixed timestamp for reproducible builds, in seconds since the epoch.

- Visibility: foreign
- Value: `integer`

## `SSL_CERT_DIR` {#external-ssl-cert-dir}

CA directory OpenSSL-style clients use.

- Visibility: foreign
- Value: `path`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `SSL_CERT_FILE` {#external-ssl-cert-file}

CA bundle OpenSSL-style clients use.

- Visibility: foreign
- Value: `path`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `TEMP` {#external-temp}

Temporary directory on Windows.

- Visibility: foreign
- Value: `path`

## `TMP` {#external-tmp}

Temporary directory on Windows, alternate spelling.

- Visibility: foreign
- Value: `path`

## `TMPDIR` {#external-tmpdir}

Temporary directory.

- Visibility: foreign
- Value: `path`

## `USERPROFILE` {#external-userprofile}

The user's home directory on Windows.

- Visibility: foreign
- Value: `path`

## `XDG_CONFIG_HOME` {#external-xdg-config-home}

XDG configuration directory.

- Visibility: foreign
- Value: `path`

## `XDG_DATA_HOME` {#external-xdg-data-home}

XDG data directory.

`ocx self setup` writes the nushell autoload file under `$XDG_DATA_HOME/nushell/vendor/autoload/`,
else under `~/.local/share/nushell/vendor/autoload/`.

- Visibility: foreign
- Value: `path`

## `__MISE_ORIG_PATH` {#external-mise-orig-path}

`PATH` before mise's activation changed it.

- Visibility: foreign
- Value: `path_list`

## `all_proxy` {#all-proxy-external-all-proxy}

Proxy URL for every scheme, lower-case spelling.

- Visibility: foreign
- Value: `string`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `http_proxy` {#http-proxy-external-http-proxy}

HTTP proxy URL, lower-case spelling.

- Visibility: foreign
- Value: `string`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `https_proxy` {#https-proxy-external-https-proxy}

HTTPS proxy URL, lower-case spelling.

- Visibility: foreign
- Value: `string`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## `no_proxy` {#no-proxy-external-no-proxy}

Hosts reached without a proxy, lower-case spelling.

- Visibility: foreign
- Value: `string`
- See: [Proxy and CA](../integration/proxy-and-ca.md)

## Shell and terminal {#external-shell-terminal}

The login shell, the terminal type and the directory zsh reads its startup files from.

## `SHELL` {#external-shell}

The user's login shell.

- Visibility: foreign
- Value: `path`

## `TERM` {#external-term}

The terminal type.

`TERM=dumb` turns colour off. `NO_COLOR` and `CLICOLOR_FORCE` rank above it.

- Visibility: foreign
- Value: `string`

## `ZDOTDIR` {#external-zdotdir}

Directory zsh reads its startup files from.

`ocx self setup` writes the zsh activation blocks into `$ZDOTDIR` instead of `$HOME`.

- Visibility: foreign
- Value: `path`

## User name {#external-user-name}

The account name, which execution records store as a readable label.

## `LOGNAME` {#external-logname}

POSIX login name.

- Visibility: foreign
- Value: `string`

## `USER` {#external-user}

POSIX user name.

- Visibility: foreign
- Value: `string`

## `USERNAME` {#external-username}

Windows user name.

- Visibility: foreign
- Value: `string`

## Windows system directories {#external-windows-directories}

Directories Windows sets, which OCX reads to refuse a system location for `toolchain_dir`.

## `APPDATA` {#external-appdata}

Windows roaming application data directory.

- Visibility: foreign
- Value: `path`

## `ProgramData` {#external-programdata}

Windows program data directory.

- Visibility: foreign
- Value: `path`

## `ProgramFiles` {#external-programfiles}

Windows program files directory.

- Visibility: foreign
- Value: `path`

## `ProgramFiles(x86)` {#external-programfiles-x86}

Windows 32-bit program files directory.

- Visibility: foreign
- Value: `path`

## `SYSTEMROOT` {#external-systemroot}

Windows system root, upper-case spelling.

- Visibility: foreign
- Value: `path`

## `SystemRoot` {#systemroot-external-systemroot}

Windows system root.

- Visibility: foreign
- Value: `path`
