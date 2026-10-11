---
title: "Project Groups"
description: "How named groups split one ocx.toml into subsets of tools, and why a binding is identified by its group and its name."
---
<!-- doc_type: explanation -->

This page explains how named groups split one `ocx.toml` into subsets of tools, and why a binding is identified by its group and its name together.

## Groups {#groups}

Not every contributor needs every package. CI needs `shellcheck` and `shfmt`; the release pipeline needs `goreleaser`; daily development needs neither. Named groups let `ocx.toml` describe these subsets without forcing every workstation to download release tooling on first checkout:

```toml
[tools]
cmake = "ocx.sh/kitware/cmake:3.28"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck/shellcheck:0.11"
shfmt      = "ocx.sh/shfmt/shfmt:3.7"

[group.release.tools]
goreleaser = "ocx.sh/goreleaser/goreleaser:2.0"
```

The top-level `[tools]` table is the implicit `default` group. Each named `[group.<name>]` holds exactly two optional sub-tables: `tools`, which adds to the default group's bindings, and `env` (see [Project and group `[env]`][project-env]). A binding declared directly under `[group.<name>]` — outside its `tools` sub-table — is a parse error naming the group. `[group.default]` is reserved and produces a parse error regardless of contents — there is no ambiguity between "implicit default" and "named default."

A group name must be non-empty. It starts with an alphanumeric character and continues with alphanumerics, `.`, `_` or `-`, the same grammar as [binding names](../guides/project-toolchain.mdx#project-names).

Pass `--group` (repeatable, comma-separated) to scope a command:

```shell
ocx pull -g ci,lint               # CI runner — only what's needed for lint jobs
ocx pull -g release               # release runner — only release packages
ocx lock                          # workstation — every group resolved
```

### Per-group binding identity {#groups-binding-identity}

The same binding name may appear in the default `[tools]` table and in any named `[group.*.tools]` table simultaneously — the identity of a binding is `(group, name)`, not `name` alone. This lets a project pin one version of a package for daily workstation use and a different version in `ci` without conflict:

```toml
[tools]
shfmt = "ocx.sh/shfmt/shfmt:3.7"       # workstation default

[group.ci.tools]
shfmt = "ocx.sh/shfmt/shfmt:3.13"      # CI: pinned to a newer build
```

When a binding name is unambiguous (present in exactly one group), [`ocx remove shfmt`][cmd-remove] finds it automatically. When the same name exists in multiple groups, pass `--group` to disambiguate:

```shell
ocx remove shfmt                  # ok — unambiguous (only in [tools])
ocx remove --group ci shfmt       # ok — removes from [group.ci] only
ocx remove shfmt                  # error — ambiguous (exists in [tools] and [group.ci])
```

Without `--group`, an ambiguous remove exits with code 64 and names every group that holds the binding.

[cmd-remove]: ../reference/command-line/commands.md#remove
[project-env]: ./project-toolchain.md#running-project-env
