---
title: "Top-level commands"
---
<!-- doc_type: reference -->

## `ocx about` {#about}

Print ocx version, registry, platform, shell, and home directory.

Also lists the detected libc family on Linux. When build provenance was baked in at compile time, two more rows appear: `Commit` (short SHA and clean or dirty status) and `Channel` (for example `dev`).

Both are absent on local builds and on stable releases.

The `Libc` row is absent on non-Linux hosts and on hosts with no readable dynamic loader. Non-FHS layouts such as Gentoo Prefix and Homebrew on Linux are detected from a system binary's loader path.

NixOS is detected only while nix-ld is active.

The `Platform` row is the bare `os/arch` of the host.

`--format json` emits `platforms`, `features`, `libc`, `shell` and `home`, plus the `commit`, `build` and `ci` blocks when baked in. `platforms[0]` is the host as an OCI platform object with its `os.features`.

`features` lists those `os.features`, and `libc` is its libc subset. A package runs on the host when its offered features are a subset of `features`.

In a terminal the table renders beside an isometric logo; piped output without `--color always` falls back to plain key-value lines.

```text
ocx about
```

## `ocx add` {#add}

Add one or more package bindings to ocx.toml.

Walks up from the working directory to the nearest `ocx.toml` and fails with exit 64 when there is none; it never creates one (run `ocx init` first). Resolves only the new bindings, carries every existing `ocx.lock` entry forward unchanged, and installs the new packages.

Re-renders the project toolchain into `<project>/.ocx/toolchain/` - or under the `toolchain_dir` root, when one is configured. That directory carries its own `.gitignore`, so `git status` stays clean.

Several identifiers may be given at once. They are staged together and committed atomically: if one is invalid, or its name is bound to a different identifier, nothing is written.

`--group` applies to every identifier in the batch, and `--platform` only picks which platform's leaf to fetch - the lock stays host-agnostic.

Each binding is named after the repository basename, so two packages with the same basename collide. Prefix an identifier with `NAME=` to choose the name yourself: `ocx add glab=ocx.sh/gitlab/cli`.

A bare identifier defaults to `:latest`, and the `ocx.toml` entry is written explicitly. A group name and a `NAME` match `^[A-Za-z0-9][A-Za-z0-9._-]*$` and are at most 64 bytes.

Adding a binding the target group already declares with the same identifier leaves `ocx.toml` byte-identical, including when one batch repeats the identifier. Such a binding is re-locked only when `ocx.lock` has no entry for it; an existing pin is never re-resolved, so run `ocx update <name>` to move one.

The packages are downloaded either way, so re-running after a failed download finishes the install.

Each already-declared binding prints `Unchanged: <name> is already added` on stderr. Fails with exit 64 when the binding name is already bound to a different identifier.

Run `ocx remove <name>` and add it again, or `ocx update <name>` to move the pin. The same name in a different group is legal, so a project can carry different versions of one package in different groups.

Fails with exit 65 when `ocx.toml` drifted from `ocx.lock` before this add (run `ocx lock` to reconcile), or exit 78 when a carried entry can no longer be migrated (run `ocx update`).

```text
ocx add [OPTIONS] <[NAME=]IDENTIFIER>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--group`, `-g` | `GROUP` | Named group to add the bindings to. Defaults to the implicit `[tools]` table when omitted |
| `--no-pull` |  | Skip materialization; resolve against local state only. Materialization is deferred to `ocx pull` or the first `ocx exec` / direnv hit |
| `--no-purge` |  | Leave unlinked packages on disk for `ocx clean`. The default |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--pull` |  | Materialize resolved packages into the object store, installing on a local miss |
| `--purge` |  | Also delete each package this command unlinks, when nothing else references it |

## `ocx clean` {#clean}

Remove unreferenced objects from the local object store.

An object is unreferenced when no candidate or current symlink points to it and no installed package depends on it. A registered project's `ocx.lock` also pins objects, patch companions included.

Projects register in the `$OCX_HOME/projects/` ledger when `ocx lock` or `ocx add` writes a lockfile.

Dependencies of a removed package become unreferenced and are collected in the same pass.

With `--format json`, each item carries `kind`, `dry_run`, `path` and `held_by`. A dry run adds a `Held By` column naming the projects that pin an object it keeps.

The rendered project toolchain under `<home>/toolchain/` is derived state that `ocx pull` rewrites from `ocx.lock`; `clean` never walks or collects from it. Every run also removes the consent stamp of any project whose directory no longer exists, and `--dry-run` lists those stamps without removing them.

Do not run `clean` while other ocx commands are running: a concurrent install may reference an object that `clean` is about to remove.

```text
ocx clean [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--dry-run` |  | Show what would be removed without actually removing anything |
| `--force` |  | Ignore the project registry and collect all unreferenced packages, including those held by other projects' `ocx.lock` files |

## `ocx env` {#env-root}

Compose and print the toolchain environment.

Reads the in-scope `ocx.toml` + `ocx.lock`, or the global toolchain under `--global`. Defaults to a plain table; `ocx --format json env` emits JSON.

`--shell[=NAME]` is the only eval-safe form.

Under `--global` an unusable toolchain (none configured, or a corrupt or stale lock) exits 0 with an empty environment.

With `--format json` the document also carries `binaries`, `entrypoints` and `integrations` arrays beside `items`, plus an `advisories` array for any deferred package in the composition. A package missing from the object store is installed as part of composition.

`--no-pull` skips that and never contacts the registry: an unmaterialized package is warned about on stderr and omitted, and the exit code stays 0. One a `[[trust.policy]]` covers is signature-verified first.

Opt out with `OCX_NO_VERIFY`.

`--shell` needs the equals form (`--shell=bash`) so an unquoted positional token cannot inject shell.

```text
ocx env [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--ci` | `github (github-actions)\|gitlab (gitlab-ci)` | Write the composed environment into a CI system's persistence channel. Bare `--ci` autodetects the provider and exits 64 when none is found |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--export-file` | `PATH` | Write the GitLab export to this file instead of stdout |
| `--group`, `-g` | `GROUPS` | Restrict the composition to the named group(s) |
| `--lazy-mode` | `never\|always` | Control when a package's content downloads: now, or on first use |
| `--no-pinned` |  | Compose through the toolchain links instead of digest paths |
| `--no-pull` |  | Skip materialization; resolve against local state only. Materialization is deferred to `ocx pull` or the first `ocx exec` / direnv hit |
| `--pinned` |  | Compose digest paths instead of the toolchain links |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--pull` |  | Materialize resolved packages into the object store, installing on a local miss |
| `--shell` | `ash\|ksh\|dash (sh)\|bash\|elvish\|fish\|batch (cmd)\|powershell (pwsh)\|zsh\|nushell (nu)` | Target shell for eval-safe export lines. A bare `--shell` autodetects it from `$SHELL` or the parent process and exits 64 when undetectable |
| `--show-patches` |  | Annotate each entry with its origin package or companion identifier |

## `ocx exec` {#exec}

Run a command with the composed environment from the project toolchain.

Loads the nearest `ocx.toml` above the current directory (the walk `--project` describes) and its `ocx.lock`, composes the environment of the selected bindings, and runs the command in it. Only that file's lock is composed.

Inside a nested project, the enclosing project's packages are not part of the child environment. Name that project with `ocx --project <dir> exec`.

Names are binding names from `ocx.toml`, not OCI identifiers; for those use `ocx package exec`.

`--` is mandatory. Optional binding names come before it.

The command and its arguments, at least one token forwarded unchanged, come after it. A missing `--` or an empty command exits 64.

With names, only those bindings must resolve and install. An unrelated binding that ships no leaf for this host (exit 78) does not block the run.

Without names, every binding in scope is composed.

A binding missing from the object store is installed during composition, and one a `[[trust.policy]]` covers is signature-verified first (opt out with `OCX_NO_VERIFY`). Entries compose in `-g` selection order after `all` expansion, then alphabetically by binding name within a group.

Later entries are prepended, so the last group listed has its `bin/` searched first on `PATH`.

The command is looked up once, before the child starts. Only the composed packages' own `PATH` contributions are searched, never the ambient `PATH` or the working directory.

A match that is itself an ocx launcher is refused. `--env PATH=...` replaces the composed `PATH` outright; write `--env PATH:path=...` to prepend.

- Alias: `x`

```text
ocx exec [OPTIONS] [NAMES]... -- <ARGV>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--clean` |  | Start with a clean environment containing only the package variables, instead of inheriting the current shell environment |
| `--consent` |  | Record a consent stamp (the default unless OCX_NO_CONSENT is set) |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--group`, `-g` | `GROUPS` | Restrict the composition to the named group(s) |
| `--lazy-mode` | `never\|always` | Control when a package's content downloads: now, or on first use |
| `--no-consent` |  | Run without consenting to this project's shell activation |
| `--no-pinned` |  | Compose through the toolchain links instead of digest paths |
| `--pinned` |  | Compose digest paths instead of the toolchain links |
| `--records-dir` | `DIRECTORY` | Directory to write this invocation's execution record into |
| `--records-name` | `TEMPLATE` | Filename template for the record, e.g. `{time}-{host}-{pid}.json` |

## `ocx init` {#init}

Create a minimal ocx.toml in the current directory.

Writes a non-interactive skeleton - a `#:schema` directive and an empty `[tools]` table - into the directory `--project` or `OCX_PROJECT` selects (else the current directory), or into `$OCX_HOME` under `--global`. Use `ocx add` afterwards, or edit the file directly; comments and declaration order survive every mutation.

If `ocx.toml` (or a symlink at that path) already exists the command exits 64 and overwrites nothing. A `--project` path that does not exist exits 79 rather than being created.

Records a consent stamp for the new project, with an empty source set because there is no lock yet. The next shell prompt in that directory applies it, and the first `ocx add` re-records it.

`--no-consent` skips the stamp (consent later with `ocx shell allow`) and `OCX_NO_CONSENT` makes that the default; the flag outranks the variable.

Under `--global` no stamp is written, since the ocx home toolchain is always consented. `ocx --global add` and its siblings also create the global file, so `ocx --global init` is for scaffolding it ahead of the first binding.

```text
ocx init [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--consent` |  | Record a consent stamp (the default unless OCX_NO_CONSENT is set) |
| `--no-consent` |  | Run without consenting to this project's shell activation |

## `ocx inspect` {#inspect}

Inspect what the project toolchain resolves to, without installing.

The toolchain-tier counterpart to `ocx package inspect`: the same report and flags, keyed by `ocx.toml` binding instead of raw identifier. It adds the project's composed `env` as an ordered array: `[env]`, then each selected group's `[env]` in `-g` order, then `--env`.

Every declaration is kept, so one key can repeat. Selects the bindings in the requested groups, narrows them to any NAMEs given, and reports each.

Nothing is installed and neither project file is written.

By default each binding lists the platform candidates `ocx.lock` pins. The report is a pure projection of the two project files: no registry is contacted, no host leaf is chosen, and `-p` is inert.

`--resolve` selects a platform exactly as on the OCI-tier command.

A cold `--closure` costs one config-blob fetch per selected binding; a warm one works under `--offline`. Needs a current lock: exit 78 when absent, 65 when it no longer matches `ocx.toml`; use `ocx status` for those states.

Under `--resolve`, an unnamed sibling that ships no leaf for this host does not block the report.

```text
ocx inspect [OPTIONS] [NAMES]...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--closure` |  | Compute each binding's dependency closure from metadata alone, without installing |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--group`, `-g` | `GROUPS` | Restrict the composition to the named group(s) |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--resolve` |  | Select this host's leaf and emit its metadata plus the OCI resolution chain |

## `ocx lock` {#lock}

Resolve package tags to digests and write ocx.lock.

Resolves every binding in the nearest `ocx.toml` to per-platform leaf digests and writes `ocx.lock` beside it. It is a whole-file reconcile.

When the lock is current, every pin is carried forward verbatim and nothing moves, even if a moving tag has moved upstream. A lock is current when its `declaration_hash` over the canonicalized `ocx.toml` matches.

When the config drifted, every declared tag is re-resolved and a moving tag may advance. To force-advance pins on a current lock, use `ocx update`.

Each binding records its registry and repository plus a table of every platform the publisher ships, whatever OS the command runs on. A lock committed on Linux is therefore complete for macOS and Windows runners.

The command is transactional: either every binding resolves and the file is rewritten atomically, or nothing is written.

When no binding's resolved content changed, `generated_at` is preserved so diffs stay minimal. After a successful write it prints a one-line stderr advisory when `.gitattributes` does not declare `ocx.lock merge=union`.

Re-renders the project toolchain into `<project>/.ocx/toolchain/`, or under the `toolchain_dir` root when one is configured. The `links/<group>/<entry>` tree and the `shells/default/bin` launchers reached through the `active` link match the lock just written.

That directory carries its own `.gitignore`, so `git status` stays clean.

```text
ocx lock [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--check` |  | Verify `ocx.lock` is current relative to `ocx.toml` and exit |
| `--no-pull` |  | Skip materialization; resolve against local state only. Materialization is deferred to `ocx pull` or the first `ocx exec` / direnv hit |
| `--no-purge` |  | Leave unlinked packages on disk for `ocx clean`. The default |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--pull` |  | Materialize resolved packages into the object store, installing on a local miss |
| `--purge` |  | Also delete each package this command unlinks, when nothing else references it |

## `ocx login` {#login}

Authenticate to a registry and persist credentials.

Credentials go to the same `~/.docker/config.json` that `docker login` and `oras login` write, so a credential written by any of the three is readable by the others. The registry defaults to `OCX_DEFAULT_REGISTRY` (`ocx.sh`) when omitted.

Storage tiers, highest priority first: `credHelpers[REGISTRY]` (a per-registry helper), `credsStore` (the global default helper), then a plaintext base64 `auths[REGISTRY].auth` entry, which needs `--allow-insecure-store`.

`--auth-type` is reserved for a future OIDC or browser flow and exits 64 today. Plain HTTP is not a login flag.

The probe uses whatever scheme the rest of the binary would: HTTPS unless the host is named by `[registries.<name>] insecure` or `OCX_INSECURE_REGISTRIES`.

`--format json` reports `registry` and `username`. `registry` is the bare `host[:port]`, so `https://ghcr.io/v1/` reports `ghcr.io`; a value with no parseable `host[:port]` exits 64.

```text
ocx login [OPTIONS] [REGISTRY]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--allow-insecure-store` |  | Allow plaintext fallback to `~/.docker/config.json` `auths` when no native helper is configured. Default: refuse, exit `ConfigError(78)` with install hint |
| `--no-verify` |  | Skip verification |
| `--password-stdin` |  | Read password/token from stdin. Required in non-interactive contexts |
| `--username`, `-u` | `USERNAME` | Username for the registry. Prompted when omitted on a TTY |
| `--verify` |  | Verify the operation against the registry before committing it (default) |

## `ocx logout` {#logout}

Remove credentials for a registry.

Exits 0 whether or not the registry was logged in, matching `docker logout`, `oras logout` and `helm registry logout`. A CI cleanup script therefore does not fail when an earlier step already removed the credentials.

The registry defaults to `OCX_DEFAULT_REGISTRY` when omitted.

```text
ocx logout [REGISTRY]
```

## `ocx pull` {#pull}

Pre-warm the object store, then render the project toolchain.

Fetches every digest-pinned entry the project `ocx.lock` declares into the local object store. Then it renders the project toolchain into `<project>/.ocx/toolchain/`, or under the `toolchain_dir` root when one is configured.

That is one link per `links/<group>/<entry>`, plus the `shells/default/bin` launchers for the default group, reached through the `active` link. That directory carries its own `.gitignore`, so `git status` stays clean.

This is the primary way a project toolchain is rendered; `--dry-run` reports the delta and writes nothing.

The package store's own `candidate` and `current` symlinks are left alone: those move only when you install or select a package.

Every package comes from the digest-pinned `ocx.lock`, never from the index, which makes this the primitive for reproducible CI setups; use `ocx package pull` for OCI identifiers. It never changes the lock's content: re-resolution is `ocx update`, rewriting from the config is `ocx lock`.

After a successful pull it re-saves the file to advance its mtime.

```text
ocx pull [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--consent` |  | Record a consent stamp (the default unless OCX_NO_CONSENT is set) |
| `--dry-run` |  | Preview which locked packages are cached vs. would be fetched |
| `--group`, `-g` | `GROUPS` | Restrict the pull to the named group(s) |
| `--lazy-mode` | `never\|always` | Control when a package's content downloads: now, or on first use |
| `--no-consent` |  | Run without consenting to this project's shell activation |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |

## `ocx remove` {#remove}

Remove one or more package bindings from ocx.toml.

Each argument is a bare binding name (`cmake`), a name with a tag (`kitware/cmake:3.28`) or a fully-qualified identifier (`ocx.sh/kitware/cmake:3.28`). An identifier is reduced to its repository basename and matched against the `ocx.toml` map key.

The tag and registry only locate the entry. A binding added under an explicit name (`ocx add glab=...`) is matched only by that name.

Fails with exit 79 when an argument matches no binding; the removals are staged together, so `ocx.toml` is left unchanged.

When one name appears in more than one group (for example in `[tools]` and `[group.ci]`), the command cannot tell which to drop and exits 64. Pass `--group` to name the target.

Drops each binding from `ocx.lock` as well. Then it re-renders the project toolchain into `<project>/.ocx/toolchain/`, or under the `toolchain_dir` root when one is configured.

The removed tools leave the `links/<group>/<entry>` tree and the `shells/default/bin` launchers reached through the `active` link. That directory carries its own `.gitignore`, so `git status` stays clean.

Only the reference is removed; `ocx clean`, or `--purge` right away, deletes a package nothing else references.

- Alias: `rm`

```text
ocx remove [OPTIONS] <IDENTIFIER>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--group`, `-g` | `NAME` | Target a specific group. Use `default` to target the implicit `[tools]` table, or a named group (e.g. `ci`) to target `[group.ci]`. Without this flag, all groups are searched; if the binding appears in more than one group an error is returned |
| `--no-purge` |  | Leave unlinked packages on disk for `ocx clean`. The default |
| `--purge` |  | Also delete each package this command unlinks, when nothing else references it |

## `ocx status` {#status}

Show what ocx.toml and ocx.lock declare, without resolving anything.

Offline and read-only: no registry is contacted, no package metadata is read, and the JSON report selects no platform. No relative `path` value is anchored to the project root.

It answers on projects other commands refuse. A missing `ocx.lock` (exit 78 for `ocx pull` and `ocx exec`), a drifted one (exit 65) and an unparseable one are reported as payload, and the command still exits 0.

There is no `-g` and no NAME argument: the report is a keyed object a caller narrows itself. `--offline` is accepted and inert.

Plain output marks a binding only where something needs attention: `not locked`, `not locked for this platform`, `ambiguous for this platform` or `orphaned in ocx.lock`. Group headings appear once the project declares more than one group.

The JSON report carries `lock` (with `current` and `declaration_hash`), `groups.<name>.tools` and `package_settings`; the published reports schema describes it.

For what the declarations resolve to on this host, use `ocx inspect`.

```text
ocx status [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--verbose`, `-v` |  | Show each binding's host digest and locked platform count |

## `ocx update` {#update}

Re-resolve declared tags against the registry; whole file or a subset.

Resolves declared tags live against the registry by default (the root `--remote` flag is redundant here, but accepted). It records the result in `ocx.lock` only and never modifies the local index tag snapshot.

A moving tag (`:latest`, `:3`) advances to wherever it points today.

Under `--frozen`, resolution is capped at the local index snapshot (an unsnapshotted tag exits 81); `--offline` forbids network access. Pass binding names or `-g/--group` to advance only part of the toolchain and freeze the rest: `ocx update ripgrep` advances one binding, `ocx update -g ci` advances a whole group.

A scoped update needs an existing `ocx.lock` (exit 78), refuses a drifted `ocx.toml` (exit 65), and rejects an unknown group or name (exit 64). With no arguments every declared tag is re-resolved, even when the lock is current, and an unchanged result rewrites the lock byte-identically.

The command is transactional: any resolution failure writes nothing. It moves only what a declared tag points to and never edits the declaration; to move the tag itself, use `ocx upgrade`.

`--format json` emits `changes`, `unchanged` and `metadata_changed`; `--verbose` lists one row per platform. See https://ocx.sh/docs/guides/update-tools/#project-update-report.

Because it holds both the old lock and the new one, it reports what moved. A table lists the bindings whose pull identifier changed.

Those that held still are summarized behind a hint line unless `--verbose` lists them too. Unlike `ocx lock`, which resolves through the local index and only when `ocx.toml` drifted, `ocx update` consults the registry every time.

Re-renders the project toolchain into `<project>/.ocx/toolchain/`, or under the `toolchain_dir` root when one is configured. The `links/<group>/<entry>` tree and the `shells/default/bin` launchers reached through the `active` link follow the advanced lock.

That directory carries its own `.gitignore`, so `git status` stays clean.

```text
ocx update [OPTIONS] [NAMES]...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--check` |  | Verify the candidate lock would match the predecessor and exit; exits 65 when a pin would change |
| `--group`, `-g` | `GROUPS` | Advance every binding in the named group(s); freeze the rest |
| `--no-pull` |  | Skip materialization; resolve against local state only. Materialization is deferred to `ocx pull` or the first `ocx exec` / direnv hit |
| `--no-purge` |  | Leave unlinked packages on disk for `ocx clean`. The default |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--pull` |  | Materialize resolved packages into the object store, installing on a local miss |
| `--purge` |  | Also delete each package this command unlinks, when nothing else references it |
| `--verbose`, `-v` |  | List the bindings that did not move as well as the ones that did |

## `ocx upgrade` {#upgrade}

Move declared tags in ocx.toml to the newest published release.

`ocx update` moves the lock to where a tag points now; `ocx upgrade` moves the tag itself. A binding keeps its variant and precision and stays within its major: `cmake:3.28` moves to `cmake:3.29`, and `cmake:3` has nothing to move to until `--major` lets it cross to `cmake:4`.

A newer major is listed as a hint either way. A binding that cannot move is left alone and reported with one reason: `digest_pinned`, `latest`, `not_a_version`, `prerelease_or_build` or `up_to_date`.

The retagged bindings are then re-locked and the project toolchain re-rendered, as `ocx update` does.

Pass binding names or `-g/--group` to upgrade only part of the toolchain. `--check` writes nothing and exits 65 when a tag would move.

Needs an existing `ocx.lock` (exit 78), refuses a drifted `ocx.toml` (exit 65), rejects an unknown group or name (exit 64), and is refused under `--frozen` or `--offline` (exit 81).

Each binding keeps its variant: a `debug-3.28` binding considers only `debug-*` tags, and the new tag must be strictly newer than the declared one. A binding that cannot move is a report row with its reason, never an error.

Comments and formatting in `ocx.toml` survive the edit. Tag listings come from the registry every time and never touch the local index.

Only the retagged bindings are re-resolved and every other pin carries forward. When nothing would move, neither file is written.

After writing, the new pins are materialized for the host platform, as `ocx update` does.

```text
ocx upgrade [OPTIONS] [NAMES]...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--check` |  | Report the tags that would move and exit without writing |
| `--group`, `-g` | `GROUPS` | Upgrade every binding in the named group(s); leave the rest |
| `--major` |  | Allow a tag to move to a newer major version |
| `--no-purge` |  | Leave unlinked packages on disk for `ocx clean`. The default |
| `--purge` |  | Also delete each package this command unlinks, when nothing else references it |
| `--verbose`, `-v` |  | List the bindings that were left alone as well as the ones that moved |

## `ocx version` {#version}

Print the version of ocx.

Without flags it prints a bare `major.minor.patch` string for scripts. `--verbose` prints a multi-line build provenance summary.

JSON output always carries the populated subset of provenance fields, whether or not `--verbose` is given.

```text
ocx version [OPTIONS]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--verbose`, `-v` |  | Emit enriched build provenance - commit, dirty flag, build time, target, rustc, CI run URL. JSON output always includes the populated subset; this flag only affects plain text |
