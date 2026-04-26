---
layout: doc
outline: deep
---
# Metadata {#metadata}

Every OCX package includes a `metadata.json` file that declares how OCX should extract
and configure the package at install time. Publishers create this file alongside the
package archive; OCX stores it in the [object store][fs-objects] after installation and
reads it whenever the package environment is resolved.

A formal [JSON Schema][schema-url] is available for editor autocompletion and validation.
Add a `$schema` field to get instant feedback in VS Code, JetBrains, and other editors
that support [JSON Schema][json-schema]:

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1
}
```

## Format {#format}

### Top-Level Structure {#format-top-level}

The metadata file is a JSON object with a `type` discriminator. Currently only the
`bundle` type is supported.

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | Yes | Discriminator tag. Must be `"bundle"`. |
| `version` | integer | Yes | Format version. Must be `1`. |
| `strip_components` | integer | No | Leading path components to strip during extraction. |
| `env` | array | No | [Environment variable declarations](#env). |
| `dependencies` | array | No | [Package dependencies](#dependencies) pinned by digest. |
| `entrypoints` | array | No | Named entry points for generating launcher scripts. |

::: details Why a type discriminator?
The `type` field allows future metadata formats (e.g. `"manifest"`, `"virtual"`) without
breaking existing packages. Parsers that encounter an unknown type can reject the file with
a clear error rather than silently misinterpreting fields.
:::

### Minimal Example {#format-minimal}

A package with no environment variables and no extraction options:

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1
}
```

### Full Example {#format-full}

A language runtime with multiple environment variables, archive stripping, a dependency, and named entry points:

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1,
  "strip_components": 1,
  "env": [
    {
      "key": "PATH",
      "type": "path",
      "required": true,
      "value": "${installPath}/bin"
    },
    {
      "key": "JAVA_HOME",
      "type": "constant",
      "value": "${installPath}"
    },
    {
      "key": "LD_LIBRARY_PATH",
      "type": "path",
      "required": false,
      "value": "${installPath}/lib"
    }
  ],
  "dependencies": [
    {
      "identifier": "ocx.sh/gcc:13@sha256:a1b2c3d4e5f6..."
    }
  ],
  "entrypoints": [
    {
      "name": "cmake",
      "target": "${installPath}/bin/cmake"
    }
  ]
}
```

## Environment Variables {#env}

The `env` array declares environment variables that OCX exposes when running
commands with the package (via [`ocx exec`][cmd-exec] or [`ocx env`][cmd-env]).

Each entry is an object with a `key`, a `type` (`path` or `constant`), and a `value`
template. Two placeholders are available in `value`:

- **`${installPath}`** — replaced with the absolute path to this package's content directory.
- **`${deps.NAME.installPath}`** — replaced with the absolute path to a declared dependency's content directory, where `NAME` is the dependency's repository basename (or its `alias` if one is declared). Useful for pointing consumers at a dependency's installation directory.

`${installPath}` and `${deps.NAME.installPath}` may appear multiple times and can be combined in the same value (e.g. `${installPath}/bin:${deps.cmake.installPath}/bin`). OCX validates at publish time that every `${deps.*}` reference names a declared dependency.

### Path Variables {#env-path}

Path variables are **prepended** to any existing value of the environment variable,
separated by the platform path delimiter.

| Field | Type | Required | Description |
|---|---|---|---|
| `key` | string | Yes | Environment variable name. |
| `type` | string | Yes | Must be `"path"`. |
| `required` | boolean | No | If `true`, the resolved path must exist on disk. Defaults to `false`. |
| `value` | string | Yes | Value template. Supports `${installPath}` and `${deps.NAME.installPath}`. |

```json
{
  "key": "PATH",
  "type": "path",
  "required": true,
  "value": "${installPath}/bin"
}
```

When `required` is `true` and the resolved path does not exist, the operation fails
with an error. Set `required` to `false` for optional paths like `lib/` directories
that may not be present on all platforms.

### Constant Variables {#env-constant}

Constant variables **replace** any existing value of the environment variable.

| Field | Type | Required | Description |
|---|---|---|---|
| `key` | string | Yes | Environment variable name. |
| `type` | string | Yes | Must be `"constant"`. |
| `value` | string | Yes | Value template. Supports `${installPath}` and `${deps.NAME.installPath}`. |

```json
{
  "key": "JAVA_HOME",
  "type": "constant",
  "value": "${installPath}"
}
```

Constants are useful for home directory variables (`JAVA_HOME`, `CARGO_HOME`) and
fixed values that do not depend on the install path (e.g. a version string).

## Dependencies {#dependencies}

The `dependencies` array declares packages that must be present for this package to function.
Each dependency is pinned by <Tooltip term="OCI digest">A SHA-256 fingerprint identifying a specific
build. The digest — not the tag — is the authoritative identifier. The tag is advisory, included for
readability and future update tooling.</Tooltip>, ensuring the same dependency graph is reproduced
on every machine regardless of the current registry state.

### Dependency Entry {#dependencies-entry}

| Field | Type | Required | Description |
|---|---|---|---|
| `identifier` | string | Yes | Fully qualified pinned OCX identifier including the registry and an inline OCI digest (`@sha256:…`). The tag is advisory; only the digest is authoritative. The digest may reference an Image Index (platform resolution at install time) or a single manifest. e.g. `ocx.sh/java:21@sha256:a1b2c3d4e5f6...`, `ghcr.io/myorg/tool@sha256:...`. |
| `alias` | string | No | Short name used to reference this dependency in `${deps.ALIAS.installPath}` templates. When set, the alias is used instead of the repository basename. Useful when two dependencies share the same basename (e.g. `myorg/cmake` and `upstream/cmake`) or when the basename is long. |
| `visibility` | string | No | Controls how the dependency's environment variables propagate. Default: `sealed`. See [Visibility](#dependencies-visibility). |

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH", "type": "path", "required": true, "value": "${installPath}/bin" },
    { "key": "JDK_HOME", "type": "constant", "value": "${deps.java.installPath}" }
  ],
  "dependencies": [
    {
      "identifier": "ocx.sh/java:21@sha256:a1b2c3d4e5f6...",
      "visibility": "private"
    },
    {
      "identifier": "ocx.sh/cmake:3.28@sha256:f6e5d4c3b2a1...",
      "alias": "cmake"
    }
  ]
}
```

### Visibility {#dependencies-visibility}

Each dependency's `visibility` field controls how its environment variables propagate through the
dependency chain. The model is inspired by [CMake's `target_link_libraries`][cmake-tll] visibility
(PUBLIC/PRIVATE/INTERFACE).

| Value | Self-execution | Consumer propagation | Use case |
|---|---|---|---|
| `sealed` (default) | No | No | Structural dependency — content accessed by path, not env. Most deps. |
| `private` | Yes | No | Package's own shims need the dep's env, consumers don't. |
| `public` | Yes | Yes | Both the package and consumers need the dep's env. |
| `interface` | No | Yes | Meta-packages that forward env to consumers without using it. |

::: details Transitive Propagation

When dependencies form a chain (Root → Dep → Transitive), visibility propagates using a simple
rule: **if the child exports (consumer-visible), the result equals the parent's edge; otherwise
the result is `sealed`.**

| Parent → Dep | Dep → Transitive | Transitive from Parent |
|---|---|---|
| `public` | `public` / `interface` | `public` |
| `public` | `private` / `sealed` | `sealed` |
| `private` | `public` / `interface` | `private` |
| `private` | `private` / `sealed` | `sealed` |
| `interface` | `public` / `interface` | `interface` |
| `interface` | `private` / `sealed` | `sealed` |
| `sealed` | any | `sealed` |

When two paths reach the same dependency (diamond), the most open visibility wins — each axis
(self-visible, consumer-visible) is OR-merged independently.
:::

### Ordering {#dependencies-ordering}

Array position defines the canonical order for environment composition. Dependencies are processed
in array order — the first entry's environment is applied first. This ordering is preserved
through transitive resolution: the full dependency graph is <Tooltip term="topologically sorted">
Kahn's algorithm with a lexicographic tiebreaker on identifier. Dependencies appear before their
dependents in the final list.</Tooltip>, deduplicated, and applied in that deterministic sequence.

### Registry Requirement {#dependencies-registry}

Every dependency identifier must include an explicit registry (`ocx.sh/java:21@sha256:a1b2c3d4e5f6...`, not just `java:21@sha256:a1b2c3d4e5f6...`).
Default registry resolution is not applied because the consumer may have a different default registry
than the publisher. Identifiers without an explicit registry are rejected at deserialization.

### No Version Ranges {#dependencies-no-version-ranges}

The digest is the complete truth — there is nothing to resolve. The tag portion of the identifier
is purely informational: it records what the publisher pinned against and enables future update
tooling, but is never used for resolution.

See [Dependencies][ug-dependencies] in the user guide for how dependencies affect
installation, environment composition, and garbage collection from a user's perspective.

## Entry Points {#entry-points}

The `entrypoints` array declares named launchers that `ocx install` generates at install time. Each
launcher is a small shell script (or `.cmd` on Windows) placed in an `entrypoints/` directory inside
the package directory. When the package is selected with `--select`, the per-repo `current` symlink
is flipped to the package root and consumers traverse `current/entrypoints` from the same anchor to
add the launchers to `PATH`.

Each launcher calls [`ocx exec`][cmd-exec] with a `file://<package-root>` URI baked at install time,
preserving clean-environment execution semantics on every invocation.

### Entry Point Fields {#entry-points-fields}

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | Yes | The launcher name. Must match `^[a-z0-9][a-z0-9_-]*$`. Used as the script filename and the command users invoke. |
| `target` | string | Yes | Template string for the binary to execute. Supports the same placeholders as [`env`](#env) values. |

### Template Substitution {#entry-points-template}

The `target` field supports the same placeholders as [environment variable values](#env):

- **`${installPath}`** — replaced with the absolute path to this package's content directory.
- **`${deps.NAME.installPath}`** — replaced with a declared dependency's content directory, where `NAME` is the repository basename or `alias`.

### Disk Layout {#entry-points-disk-layout}

Generated launchers land in `entrypoints/` inside the package directory (a sibling of `content/`).
When the package is selected with `ocx install --select` or `ocx select`, the per-repo `current`
symlink is flipped to that package root, and consumers reach the launchers via
`{registry}/{repo}/current/entrypoints`. Packages with no entrypoints produce no `entrypoints/`
directory, so the same `current/entrypoints` path simply does not exist for them.

### Uniqueness {#entry-points-uniqueness}

Duplicate `name` values within the same `entrypoints` array are rejected at deserialization with
a descriptive error. Name collisions across different currently-selected packages are detected at
select time.

### Example {#entry-points-example}

```json
{
  "entrypoints": [
    { "name": "cmake", "target": "${installPath}/bin/cmake" },
    { "name": "ctest", "target": "${installPath}/bin/ctest" }
  ]
}
```

## Extraction {#extraction}

### `strip_components` {#extraction-strip-components}

Many upstream archives wrap their content in a single top-level directory
(e.g. `cmake-3.28/bin/cmake`). Rather than repackaging, set `strip_components` to
remove leading path components during extraction — analogous to `tar --strip-components`.

| Value | Effect |
|---|---|
| omitted / `0` | Extract as-is. |
| `1` | Remove one leading directory: `cmake-3.28/bin` → `bin`. |
| `2` | Remove two: `a/b/bin` → `bin`. |

## JSON Schema {#json-schema}

The schema is generated from the Rust source types and published at:

**[`https://ocx.sh/schemas/metadata/v1.json`][schema-url]**

### Editor Integration {#json-schema-editors}

Add `$schema` to the top of your `metadata.json` for instant validation and
autocompletion:

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json"
}
```

### Generating Locally {#json-schema-local}

The schema is a build artifact generated from the OCX source. To regenerate:

```sh
task schema:generate
```

This writes the schema to `website/src/public/schemas/metadata/v1.json`.

### Validation {#json-schema-validation}

Validate a metadata file against the schema using [check-jsonschema][check-jsonschema]:

```sh
uvx check-jsonschema --schemafile https://ocx.sh/schemas/metadata/v1.json metadata.json
```

## Version History {#version-history}

### Version 1 (current) {#version-1}

Initial release. Supports `path` and `constant` variable types, `strip_components`
for archive extraction, `${installPath}` template substitution, and optional
`dependencies` for declaring digest-pinned package dependencies.

<!-- external -->
[json-schema]: https://json-schema.org/
[check-jsonschema]: https://github.com/python-jsonschema/check-jsonschema
[cmake-tll]: https://cmake.org/cmake/help/latest/command/target_link_libraries.html

<!-- schema -->
[schema-url]: /schemas/metadata/v1.json

<!-- commands -->
[cmd-exec]: ./command-line.md#exec
[cmd-env]: ./command-line.md#env

<!-- internal -->
[fs-objects]: ../user-guide.md#file-structure-objects
[ug-dependencies]: ../user-guide.md#dependencies
