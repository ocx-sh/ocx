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

A language runtime with multiple environment variables and archive stripping:

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
  ]
}
```

## Environment Variables {#env}

The `env` array declares environment variables that OCX exposes when running
commands with the package (via [`ocx exec`][cmd-exec] or [`ocx env`][cmd-env]).

Each entry is an object with a `key`, a `type` (`path` or `constant`), and a `value`
template. The `${installPath}` placeholder is replaced with the absolute path to the
package content directory at resolution time.

### Path Variables {#env-path}

Path variables are **prepended** to any existing value of the environment variable,
separated by the platform path delimiter.

| Field | Type | Required | Description |
|---|---|---|---|
| `key` | string | Yes | Environment variable name. |
| `type` | string | Yes | Must be `"path"`. |
| `required` | boolean | Yes | If `true`, the resolved path must exist on disk. |
| `value` | string | Yes | Value template with `${installPath}` substitution. |

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
| `value` | string | Yes | Value template with `${installPath}` substitution. |

```json
{
  "key": "JAVA_HOME",
  "type": "constant",
  "value": "${installPath}"
}
```

Constants are useful for home directory variables (`JAVA_HOME`, `CARGO_HOME`) and
fixed values that do not depend on the install path (e.g. a version string).

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
for archive extraction, and `${installPath}` template substitution.

<!-- external -->
[json-schema]: https://json-schema.org/
[check-jsonschema]: https://github.com/python-jsonschema/check-jsonschema

<!-- schema -->
[schema-url]: /schemas/metadata/v1.json

<!-- commands -->
[cmd-exec]: ./command-line.md#exec
[cmd-env]: ./command-line.md#env

<!-- internal -->
[fs-objects]: ../user-guide.md#file-structure-objects
