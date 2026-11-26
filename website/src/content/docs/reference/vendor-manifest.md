---
title: "Vendor manifest"
---
<!-- doc_type: reference -->

The `manifest.json` that [`ocx vendor`][cmd-vendor] and [`ocx package vendor`][cmd-package-vendor] write at the top of the output directory, and the path rules every field in it obeys Schema: [`https://ocx.sh/schemas/vendor-manifest/v1.json`][schema-vendor-manifest]. Usage: [Vendor packages][guide-vendor].

## Conventions {#vendor-manifest-conventions}

The manifest follows the rules of every machine-readable document OCX writes.

- Unset optional fields are omitted, enums are open and keys are snake_case.
- Object keys follow declaration order and every list is sorted by the key the tables state, so equal content gives equal bytes.
- `@VENDOR_DIR@` (field `dir_token`) stands for the absolute path of the output directory. It is the only non-literal text a value may contain.

## Path rules {#vendor-manifest-paths}

Every path field obeys these rules, enforced when the manifest is written.

- Relative to the output directory, separated by `/`, with no leading `/`.
- No empty, `.` or `..` segment.
- For a `windows/*` platform: no `:` or `\`, no control character (U+0000 to U+001F), none of `<>"|?*`, no segment that is a device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1` to `COM9`, `LPT1` to `LPT9`, with any extension) and no segment ending in `.` or a space.
- A value that breaks a rule, a symlink whose target leaves its package, or a value holding `@VENDOR_DIR@`, NUL or non-UTF-8 text exits 65 (see [Errors](#vendor-manifest-errors)).

## Top-level fields {#vendor-manifest-fields}

| Field | Type | Description |
|---|---|---|
| `schema_version` | integer, always `1` | Changes only for a breaking change to the document. |
| `content` | `present` or `absent` | `absent` after `--manifest-only`: no package file, launcher or sidecar exists. |
| `platform` | string | Canonical platform string of the run, for example `linux/amd64+libc.glibc`. |
| `available_platforms` | string[], sorted | Platforms on which every selected package has a leaf, computed before leaf selection. |
| `vendor_id` | string | `sha256:` and the hex SHA-256 of `platform`, a newline, then the sorted package digests joined by newlines. Two directories with equal `vendor_id` hold the same files. |
| `dir_token` | string, always `@VENDOR_DIR@` | The token described in [Conventions](#vendor-manifest-conventions). |
| `packages` | object[], sorted by `digest` | Every package of the closure, once. |
| `environments` | object[] | The selection's environment first, then entry-point environments sorted by `id`. |
| `selections` | object[] | The selections that were vendored. One in version 1. |
| `tools` | object[], sorted by `selection`, then `name` | Every exposed tool name. |

## Packages {#vendor-manifest-packages}

| Field | Type | Description |
|---|---|---|
| `identifier` | string | Pinned identifier `registry/repository[:tag]@sha256:<hex>`. |
| `digest` | string | Digest of the platform manifest. |
| `package_dir` | string | `packages/<registry>/<algorithm>/<2 hex>/<30 hex>`. |
| `content_dir` | string | `<package_dir>/content`. |
| `dependencies` | string[], sorted | Digests of the direct dependencies. |
| `names_declared` | boolean | `false` when the package's metadata declares neither binaries nor entry points, so no tool comes from it. |
| `entrypoints` | object[], sorted by `name` | The package's own launchers under `<package_dir>/entrypoints/`. |
| `tree` | object | Present only when `content` is `present`. |

### Entry points {#vendor-manifest-entrypoints}

| Field | Type | Description |
|---|---|---|
| `name` | string | Entry-point name. |
| `launcher_path` | string | Launcher under `<package_dir>/entrypoints/`; ends in `.exe` for a Windows platform. |
| `environment` | string | Id of the entry point's private environment. |

### Tree {#vendor-manifest-tree}

| Field | Type | Description |
|---|---|---|
| `files` | `{path, executable}`[], sorted by `path` | Every regular file under `content_dir`. |
| `links` | `{path, target}`[], sorted by `path` | Every symlink. `target` is relative and resolves inside `content_dir`. Listed even under `--omit-links`. |
| `link_free_dirs` | string[], sorted | Top-most directories under `content_dir` that hold no link at or below them. |

Empty directories are written on disk and are not listed.

## Environments {#vendor-manifest-environments}

| Field | Type | Description |
|---|---|---|
| `id` | string | Opaque. `selection:default`, or `entrypoint:<digest>:<name>`. |
| `scope` | object | `{type: "selection", selection}` or `{type: "entrypoint", package, name}`. |
| `items` | object[] | Variable entries in application order. |

Each item has `key`, `value` and `kind` (`path`, `constant` or `list`), as in the [`ocx env`][cmd-env] report. A `list` item also has `separator`.

A selection environment equals `ocx --format json env --pinned` (project) or `ocx --format json package env` (packages), with each home package path replaced by `@VENDOR_DIR@/packages/…/`. [Patches][patches-how] never reach it.

## Selections {#vendor-manifest-selections}

| Field | Type | Description |
|---|---|---|
| `id` | string | `default`. |
| `groups` | string[] | Groups as given. |
| `bindings` | string[] | Bindings as given, with those resolved from `-g` included. |
| `roots` | string[] | Digests of the selected packages, in selection order. |
| `environment` | string | Id of the composed environment. |
| `launcher_dir` | string | `selections/<id>/shells/default/bin`. |

## Tools {#vendor-manifest-tools}

| Field | Type | Description |
|---|---|---|
| `name` | string | Name the tool is exposed under. |
| `selection` | string | Id of the selection that exposes it. |
| `kind` | `binary` or `entrypoint` | Where the name comes from. |
| `package` | string | Digest of the owning package. When two packages expose a name, the last one walked owns it. |
| `environment` | string | Id of the selection's environment. |
| `target_path` | string | The binary file, or the package's entry-point launcher. |
| `launcher_path` | string | `<launcher_dir>/<name>`, with `.exe` on Windows. Omitted when no launcher exists for `platform`, for example `wasip1/wasm`. |

## Errors {#vendor-manifest-errors}

| Condition | Exit code | `error.detail` |
|---|---|---|
| The output directory exists and is not an empty directory, or is a symlink | 64 | `vendor_output_not_empty` |
| A value holds `@VENDOR_DIR@`, NUL, non-UTF-8, or the home's or output directory's absolute path | 65 | `vendor_value_unrepresentable` |
| A program, launcher or link target breaks the path rules | 65 | `vendor_path_unrepresentable` |
| A selected binding has no leaf for the platform at the locked version | 78 | existing resolution slug |

Exit codes shared with every command are in [Exit codes][exit-codes].

## Example {#vendor-manifest-example}

The result of `ocx vendor -o out -p linux/amd64+libc.glibc jq`, with made-up digests.

```json
{
  "schema_version": 1,
  "content": "present",
  "platform": "linux/amd64+libc.glibc",
  "available_platforms": ["darwin/arm64", "linux/amd64+libc.glibc", "windows/amd64"],
  "vendor_id": "sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a",
  "dir_token": "@VENDOR_DIR@",
  "packages": [
    {
      "identifier": "ocx.sh/jqlang/jq:1.8.1@sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988",
      "digest": "sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988",
      "package_dir": "packages/ocx.sh/sha256/1f/2e3d4c5b6a79881f2e3d4c5b6a7988",
      "content_dir": "packages/ocx.sh/sha256/1f/2e3d4c5b6a79881f2e3d4c5b6a7988/content",
      "dependencies": [],
      "names_declared": true,
      "entrypoints": [],
      "tree": {
        "files": [{"path": "packages/ocx.sh/sha256/1f/2e3d4c5b6a7988/content/bin/jq", "executable": true}],
        "links": [{"path": "packages/ocx.sh/sha256/1f/2e3d4c5b6a7988/content/bin/jq-1.8", "target": "jq"}],
        "link_free_dirs": ["packages/ocx.sh/sha256/1f/2e3d4c5b6a7988/content/share"]
      }
    }
  ],
  "environments": [
    {
      "id": "selection:default",
      "scope": {"type": "selection", "selection": "default"},
      "items": [
        {"key": "PATH", "value": "@VENDOR_DIR@/packages/ocx.sh/sha256/1f/2e3d4c5b6a7988/content/bin", "kind": "path"}
      ]
    }
  ],
  "selections": [
    {"id": "default", "groups": [], "bindings": ["jq"], "roots": ["sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988"], "environment": "selection:default", "launcher_dir": "selections/default/shells/default/bin"}
  ],
  "tools": [
    {"name": "jq", "selection": "default", "kind": "binary", "package": "sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988", "environment": "selection:default", "target_path": "packages/ocx.sh/sha256/1f/2e3d4c5b6a7988/content/bin/jq", "launcher_path": "selections/default/shells/default/bin/jq"}
  ]
}
```

<!-- commands -->
[cmd-vendor]: ./command-line/commands.md#vendor
[cmd-package-vendor]: ./command-line/package-publishing.md#package-vendor
[cmd-env]: ./command-line/commands.md#env-root

<!-- external -->
[schema-vendor-manifest]: https://ocx.sh/schemas/vendor-manifest/v1.json

<!-- internal -->
[guide-vendor]: ../guides/vendor.mdx
[exit-codes]: ./exit-codes.md
[patches-how]: ../explanation/patches.mdx#patches-how
