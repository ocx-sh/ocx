---
title: "Metadata"
---
<!-- doc_type: reference -->

Every OCX package carries a `metadata.json` that declares how OCX extracts and configures it at install time. OCX stores it in the [object store][fs-objects] after installation and reads it whenever the package environment is resolved.

## Format {#format}

The metadata file is a JSON object with a `type` discriminator. Only the `bundle` type exists, and ocx rejects an unknown `type` with an error.

### Top-Level Structure {#format-top-level}

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | Yes | Discriminator tag. Must be `"bundle"`. |
| `version` | integer | Yes | Format version. Currently `1`. |
| `strip_components` | integer | No | Leading path components to strip during extraction. See [Extraction](#extraction). |
| `env` | array | No | [Environment variable declarations](./env.md#env). |
| `dependencies` | array | No | [Package dependencies](./dependencies.md#dependencies). |
| `entrypoints` | object | No | [Named entry points](./entry-points.md#entry-points), keyed by command name. |
| `binaries` | array | No | [Declared executable names the package puts on `PATH`](./entry-points.md#executables). |
| `integrations` | object | No | [Vendor-namespaced configuration blocks for tools OCX does not model](./integrations.md#integrations). |

### Minimal Example {#format-minimal}

A package with no environment variables and no extraction options.

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1
}
```

### Full Example {#format-full}

A language runtime with environment variables, archive stripping, a dependency, declared executables and named entry points.

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
      "value": "${installPath}/bin",
      "visibility": "public"
    },
    {
      "key": "JAVA_HOME",
      "type": "constant",
      "value": "${installPath}",
      "visibility": "public"
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
  "binaries": ["cmake", "cpack", "ctest"],
  "entrypoints": {
    "cmake": {},
    "fmt": { "command": "cmake-format" }
  }
}
```

The `cmake` entry point dispatches to a binary named `cmake`, and `fmt` dispatches to `cmake-format`. `binaries` declares separately which executables the interface `PATH` carries, as [Executables](./entry-points.md#executables) describes.

## Extraction {#extraction}

Extraction options control how OCX unpacks the package layers into the object store.

### `strip_components` {#extraction-strip-components}

`strip_components` removes leading path components when the package is assembled, like `tar --strip-components`. It is the package-wide default for any layer that carries no layout of its own.

| Value | Effect |
|---|---|
| omitted / `0` | Extract as-is. |
| `1` | Remove one leading directory: `cmake-3.28/bin` becomes `bin`. |
| `2` | Remove two: `a/b/bin` becomes `bin`. |

- Per-layer override: a `strip`/`prefix` pair in the manifest layer descriptor's annotations, set with `<ref>:strip=N,prefix=P` on [`ocx package push`][cmd-package-push].
- There is no `layers` field in this schema. Per-layer layout lives in the manifest, not in `metadata.json`.

See [layer layout][cmd-package-push-layout] for the grammar and the fallback chain.

## JSON Schema {#json-schema}

The schema is generated from the Rust source types and published at [`https://ocx.sh/schemas/metadata/v1.json`][schema-url].

- Editor integration, local generation and validation: see [Schemas][machine-interface-schemas].

## OCI Platform Fields {#oci-platform}

The OCI platform descriptor, the `platform` object in an [OCI Image Index][oci-image-index] entry, is not part of `metadata.json`. The publisher declares it at push time and OCX consumes it at index resolution time.

See [Platforms][reference-platforms] for the string form (`--platform`, lock keys, the build receipt's `platform` field) and the compatibility relation.

### `os.features` and libc tagging {#oci-platform-os-features}

OCX encodes the libc family requirement of a Linux binary in the `os.features` array of the platform descriptor. A static binary carries no `os.features` declaration, and the absent or empty set matches every Linux host.

| Value | Meaning |
|---|---|
| `libc.glibc` | Binary links [GNU libc (glibc)][glibc]. Requires glibc on the installing host. |
| `libc.musl` | Binary links [musl libc][musl]. Requires musl on the installing host. |

```json
{
  "platform": {
    "architecture": "amd64",
    "os": "linux",
    "os.features": ["libc.glibc"]
  }
}
```

- Normalization: OCX sorts and deduplicates `os_features` before serialization, so spec or push order does not affect the wire format.
- RESERVED `features`: the OCI v1.1.1 top-level `platform.features` field is RESERVED. OCX never serializes it and drops any value found in a foreign manifest with a warning.

See [libc Differentiation][authoring-libc] for the publisher workflow.

<!-- external -->
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[glibc]: https://www.gnu.org/software/libc/
[musl]: https://musl.libc.org/

<!-- schemas -->
[schema-url]: /schemas/metadata/v1.json

<!-- commands -->
[cmd-package-push]: ../command-line/package-publishing.md#package-push
[cmd-package-push-layout]: ../../publishing/build-and-push.mdx#layer-layout

<!-- reference -->
[reference-platforms]: ../platforms.md
[machine-interface-schemas]: ../machine-interface.md#machine-interface-schemas

<!-- guides -->
[authoring-libc]: ../../publishing/libc-variants.md#libc
[fs-objects]: ../../explanation/storage.mdx#packages
