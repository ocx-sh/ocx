---
title: "Command line"
---
<!-- doc_type: reference -->

Options before the command name apply to every command. Each command group has its own page.

## `--color` {#arg-color}

When to use ANSI colors in output.

Default `auto`: color only on a terminal. An explicit `--color` outranks `NO_COLOR`, `CLICOLOR` and `CLICOLOR_FORCE`.

- Value: `auto|always|never`

## `--config` {#arg-config}

Path to the ocx configuration file.

Can also be set via the `OCX_CONFIG` environment variable. The file must exist, or the command exits 79.

To disable config discovery entirely, set `OCX_NO_CONFIG=1`.

The file layers on top of the discovered system, user and `$OCX_HOME` files and of the managed tier; it does not replace them.

- Short: `-c`
- Value: `FILE`
- Environment: [`OCX_CONFIG`](../environment.md#ocx-config)

## `--format` {#arg-format}

Output format for stdout reports: `plain` (default) or `json`.

Applies to every command; there is no per-command `--format`. The `--shell[=NAME]` output of `env` / `package env` is unaffected.

Each report's fields are in the reports schema, <https://ocx.sh/schemas/reports/v1.json>.

- Value: `json|plain`

## `--frozen` {#arg-frozen}

Freeze tag resolution to the local index; never fetch an unknown tag.

A tag in the local index resolves from cache. A digest-pinned reference (or a tag `ocx.lock` pins) still fetches its content.

An unpinned tag missing from the index errors. Populate the index first with `ocx index update` (refused under this flag, exit 81).

Scoped to packages: patch companions and managed configuration still resolve live. Unlike `--offline` and Cargo's `--frozen`, the network stays reachable for known content.

Conflicts with `--remote`. Env var: `OCX_FROZEN`.

- Environment: [`OCX_FROZEN`](../environment.md#ocx-frozen)

## `--global` {#global-flag}

Select the global toolchain (`$OCX_HOME/ocx.toml`) as the project file in effect, instead of discovering a project by CWD walk.

Mutually exclusive with `--project`. Resolution-affecting: forwarded to child ocx processes via `OCX_GLOBAL`.

Equivalent env var: `OCX_GLOBAL`. The global toolchain never composes into project resolution; `ocx exec` and `ocx package exec` stay hermetic and never read it.

Before the subcommand `-g` means `--global`; after it, `-g` means `--group`. Nothing selects the global toolchain implicitly.

- Short: `-g`
- Environment: [`OCX_GLOBAL`](../environment.md#ocx-global)

## `--index` {#arg-index}

Overrides the local index home directory.

The local index is a collection of per-source subtrees, one per configured registry or index, each holding tag roots plus their manifest data. Resolution can work fully offline once it is populated.

This flag redirects the whole collection, replacing the default at `$OCX_HOME/index`.

Digest-addressed lookups still consult the local index even under `--remote`; only tag listings and tag->manifest lookups route to the remote registry there. Can also be set via the `OCX_INDEX` environment variable.

- Value: `PATH`
- Environment: [`OCX_INDEX`](../environment.md#ocx-index)

## `--jobs` {#arg-jobs}

Maximum number of root packages to pull concurrently.

`0` means "use all logical cores" (GNU Parallel convention). Omitting the flag falls back to the `OCX_JOBS` environment variable; when neither is set, pulls run unbounded.

Negative values are rejected at parse time.

Caps only the outer dispatch - transitive dependency and layer extraction stay unbounded.

- Value: `N`
- Environment: [`OCX_JOBS`](../environment.md#ocx-jobs)

## `--json` {#arg-json}

Shorthand for `--format json`.

Last one wins if combined with `--format`.

## `--log-level` {#arg-log-level}

The log level to use.

Default `info`. Logs go to stderr, never stdout.

At `info` and below a line reads `error: message` or `warning: message`; `debug` and `trace` add a timestamp and level. Can also be set via the `OCX_LOG_LEVEL` environment variable.

- Short: `-l`
- Value: `trace|debug|info|warn|error|off`
- Environment: [`OCX_LOG_LEVEL`](../environment.md#ocx-log-level)

## `--offline` {#arg-offline}

Disable all network access.

Tag->digest resolution must be satisfied by the local index or a digest-pinned identifier; unpinned tags missing from the local index exit 81. Run `ocx index update` online first, or switch to a digest-pinned identifier.

Useful for hermetic CI runs and air-gapped environments.

Equivalent env var: `OCX_OFFLINE`.

- Environment: [`OCX_OFFLINE`](../environment.md#ocx-offline)

## `--project` {#arg-project}

Project directory, or the project file itself (project-tier toolchain config).

A directory resolves to the `ocx.toml` inside it, so `--project .` and `--project /path/to/repo` both work; a file may have any name (like Cargo `--manifest-path`). Equivalent env var: `OCX_PROJECT`; `OCX_NO_PROJECT=1` disables project discovery.

A path that does not exist exits 79, a directory with no `ocx.toml` exits 64, and an unreadable path exits 74.

- Value: `PATH`
- Environment: [`OCX_PROJECT`](../environment.md#ocx-project)

## `--quiet` {#arg-quiet}

Suppress the stdout report and progress bars (errors and warnings remain).

When set, the CLI's structured report (table or JSON) is not printed and no transfer progress is rendered on stderr. Equivalent env var: `OCX_QUIET`.

- Short: `-q`
- Environment: [`OCX_QUIET`](../environment.md#ocx-quiet)

## `--remote` {#arg-remote}

Route mutable lookups (tag list, catalog, tag->manifest) to the remote registry instead of the local index.

Pure queries (`index list`, `index catalog`, `package description pull`) do **not** persist the result to the local index - use `ocx index update` to refresh the local index explicitly. Implies network access.

Combined with `--offline` the result is "pinned-only mode": no source contact. Any tag-addressed resolution that cannot be satisfied locally errors instead of silently falling back.

Equivalent env var: `OCX_REMOTE`.

- Short: `-r`
- Environment: [`OCX_REMOTE`](../environment.md#ocx-remote)

## Command pages {#command-pages}

Every command is documented on the page of its group.

| Page | Commands |
| --- | --- |
| [Top-level commands](./commands.md) | `ocx about`, `ocx add`, `ocx clean`, `ocx env`, `ocx exec`, `ocx init`, `ocx inspect`, `ocx lock`, `ocx login`, `ocx logout`, `ocx pull`, `ocx remove`, `ocx status`, `ocx update`, `ocx upgrade`, `ocx version` |
| [ocx config](./config.md) | `ocx config`, `ocx config push`, `ocx config setup`, `ocx config test`, `ocx config update` |
| [ocx direnv](./direnv.md) | `ocx direnv`, `ocx direnv export`, `ocx direnv init` |
| [ocx index](./index-commands.md) | `ocx index`, `ocx index catalog`, `ocx index list`, `ocx index regenerate`, `ocx index sync`, `ocx index update` |
| [ocx package: publishing](./package-publishing.md) | `ocx package`, `ocx package announce`, `ocx package attest`, `ocx package cascade`, `ocx package cascade check`, `ocx package cascade repair`, `ocx package claim`, `ocx package copy`, `ocx package create`, `ocx package description`, `ocx package description pull`, `ocx package description push`, `ocx package push`, `ocx package sign`, `ocx package test` |
| [ocx package: consuming](./package-consuming.md) | `ocx package deps`, `ocx package deselect`, `ocx package env`, `ocx package exec`, `ocx package inspect`, `ocx package install`, `ocx package prune`, `ocx package pull`, `ocx package receipt`, `ocx package sbom`, `ocx package select`, `ocx package uninstall`, `ocx package verify`, `ocx package which` |
| [ocx patch](./patch.md) | `ocx patch`, `ocx patch freeze`, `ocx patch publish`, `ocx patch sync`, `ocx patch test`, `ocx patch why` |
| [ocx self](./self.md) | `ocx self`, `ocx self activate`, `ocx self setup`, `ocx self update` |
| [ocx shell](./shell.md) | `ocx shell`, `ocx shell allow`, `ocx shell completion`, `ocx shell revoke`, `ocx shell state` |

## Anchors on other pages {#moved-anchors}

These anchors resolve on the page listed.

| Anchor | Page |
| --- | --- |
| `about` | [commands.md#about](./commands.md#about) |
| `add` | [commands.md#add](./commands.md#add) |
| `clean` | [commands.md#clean](./commands.md#clean) |
| `config` | [config.md#config](./config.md#config) |
| `config-push` | [config.md#config-push](./config.md#config-push) |
| `config-setup` | [config.md#config-setup](./config.md#config-setup) |
| `config-test` | [config.md#config-test](./config.md#config-test) |
| `config-update` | [config.md#config-update](./config.md#config-update) |
| `deps` | [package-consuming.md#deps](./package-consuming.md#deps) |
| `direnv` | [direnv.md#direnv](./direnv.md#direnv) |
| `direnv-export` | [direnv.md#direnv-export](./direnv.md#direnv-export) |
| `direnv-init` | [direnv.md#direnv-init](./direnv.md#direnv-init) |
| `env-root` | [commands.md#env-root](./commands.md#env-root) |
| `exec` | [commands.md#exec](./commands.md#exec) |
| `exit-codes` | [../exit-codes.md](../exit-codes.md) |
| `index` | [index-commands.md#index](./index-commands.md#index) |
| `index-catalog` | [index-commands.md#index-catalog](./index-commands.md#index-catalog) |
| `index-list` | [index-commands.md#index-list](./index-commands.md#index-list) |
| `index-regenerate` | [index-commands.md#index-regenerate](./index-commands.md#index-regenerate) |
| `index-sync` | [index-commands.md#index-sync](./index-commands.md#index-sync) |
| `index-update` | [index-commands.md#index-update](./index-commands.md#index-update) |
| `init` | [commands.md#init](./commands.md#init) |
| `inspect` | [commands.md#inspect](./commands.md#inspect) |
| `lock` | [commands.md#lock](./commands.md#lock) |
| `login` | [commands.md#login](./commands.md#login) |
| `logout` | [commands.md#logout](./commands.md#logout) |
| `package` | [package-publishing.md#package](./package-publishing.md#package) |
| `package-announce` | [package-publishing.md#package-announce](./package-publishing.md#package-announce) |
| `package-attest` | [package-publishing.md#package-attest](./package-publishing.md#package-attest) |
| `package-cascade` | [package-publishing.md#package-cascade](./package-publishing.md#package-cascade) |
| `package-cascade-check` | [package-publishing.md#package-cascade-check](./package-publishing.md#package-cascade-check) |
| `package-cascade-repair` | [package-publishing.md#package-cascade-repair](./package-publishing.md#package-cascade-repair) |
| `package-claim` | [package-publishing.md#package-claim](./package-publishing.md#package-claim) |
| `package-copy` | [package-publishing.md#package-copy](./package-publishing.md#package-copy) |
| `package-create` | [package-publishing.md#package-create](./package-publishing.md#package-create) |
| `package-description` | [package-publishing.md#package-description](./package-publishing.md#package-description) |
| `package-description-pull` | [package-publishing.md#package-description-pull](./package-publishing.md#package-description-pull) |
| `package-description-push` | [package-publishing.md#package-description-push](./package-publishing.md#package-description-push) |
| `package-deselect` | [package-consuming.md#package-deselect](./package-consuming.md#package-deselect) |
| `package-env` | [package-consuming.md#package-env](./package-consuming.md#package-env) |
| `package-exec` | [package-consuming.md#package-exec](./package-consuming.md#package-exec) |
| `package-inspect` | [package-consuming.md#package-inspect](./package-consuming.md#package-inspect) |
| `package-install` | [package-consuming.md#package-install](./package-consuming.md#package-install) |
| `package-prune` | [package-consuming.md#package-prune](./package-consuming.md#package-prune) |
| `package-pull` | [package-consuming.md#package-pull](./package-consuming.md#package-pull) |
| `package-push` | [package-publishing.md#package-push](./package-publishing.md#package-push) |
| `package-receipt` | [package-consuming.md#package-receipt](./package-consuming.md#package-receipt) |
| `package-sbom` | [package-consuming.md#package-sbom](./package-consuming.md#package-sbom) |
| `package-select` | [package-consuming.md#package-select](./package-consuming.md#package-select) |
| `package-sign` | [package-publishing.md#package-sign](./package-publishing.md#package-sign) |
| `package-test` | [package-publishing.md#package-test](./package-publishing.md#package-test) |
| `package-uninstall` | [package-consuming.md#package-uninstall](./package-consuming.md#package-uninstall) |
| `package-verify` | [package-consuming.md#package-verify](./package-consuming.md#package-verify) |
| `patch` | [patch.md#patch](./patch.md#patch) |
| `patch-freeze` | [patch.md#patch-freeze](./patch.md#patch-freeze) |
| `patch-publish` | [patch.md#patch-publish](./patch.md#patch-publish) |
| `patch-sync` | [patch.md#patch-sync](./patch.md#patch-sync) |
| `patch-test` | [patch.md#patch-test](./patch.md#patch-test) |
| `patch-why` | [patch.md#patch-why](./patch.md#patch-why) |
| `pull` | [commands.md#pull](./commands.md#pull) |
| `remove` | [commands.md#remove](./commands.md#remove) |
| `self` | [self.md#self](./self.md#self) |
| `self-activate` | [self.md#self-activate](./self.md#self-activate) |
| `self-setup` | [self.md#self-setup](./self.md#self-setup) |
| `self-update` | [self.md#self-update](./self.md#self-update) |
| `shell` | [shell.md#shell](./shell.md#shell) |
| `shell-allow` | [shell.md#shell-allow](./shell.md#shell-allow) |
| `shell-completion` | [shell.md#shell-completion](./shell.md#shell-completion) |
| `shell-revoke` | [shell.md#shell-revoke](./shell.md#shell-revoke) |
| `shell-state` | [shell.md#shell-state](./shell.md#shell-state) |
| `status` | [commands.md#status](./commands.md#status) |
| `update` | [commands.md#update](./commands.md#update) |
| `upgrade` | [commands.md#upgrade](./commands.md#upgrade) |
| `version` | [commands.md#version](./commands.md#version) |
| `which` | [package-consuming.md#which](./package-consuming.md#which) |
