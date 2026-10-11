---
title: "Metadata: Dependencies"
---
<!-- doc_type: reference -->

The `dependencies` array of [`metadata.json`](./index.md#format) declares the packages a package needs at runtime.

## Dependencies {#dependencies}

Every dependency identifier is pinned by OCI digest in the published form. The same dependency graph is therefore reproduced on every machine regardless of the current registry state.

- Limit: a package declares at most 256 dependencies, and a longer array is rejected when the file is read (`too many dependencies`, exit 65)
- The platform a bundle targets is not part of this file. On the wire it lives in the [OCI Image Index][oci-image-index]. Between [`ocx package create`][cmd-package-create] and [`ocx package push`][cmd-package-push] or [`ocx package test`][cmd-package-test] it lives in a build receipt, readable with [`ocx package receipt`][cmd-package-receipt].
- Authoring sidecar versus published form: the digest is optional while authoring and mandatory once published. The published form is a strict subset of the authoring form, so an already-published `metadata.json` is a valid sidecar for `ocx package create` and `ocx package push` and needs no migration.

See [Dependencies in depth][in-depth-dependencies] for the pin rules and [Declaring dependencies][authoring-dependencies] for the publisher workflow.

### Dependency Entry {#dependencies-entry}

| Field | Type | Required | Description |
|---|---|---|---|
| `identifier` | string | Yes | OCX identifier with an explicit registry; one without is rejected at deserialization. In the authoring sidecar the digest is optional, and a tag-only identifier tells [`ocx package create`][cmd-package-create] to resolve it. In the published form the digest is mandatory and must reference a platform manifest, never an OCI Image Index. The tag is always advisory. E.g. `ocx.sh/java:21@sha256:a1b2c3d4e5f6...`. |
| `name` | string | No | Short name used to reference this dependency in `${deps.NAME.installPath}` templates, instead of the repository basename. Must match `^[a-z0-9][a-z0-9_-]*$` and be at most 64 characters. |
| `visibility` | string | No | Controls how the dependency's environment variables propagate. Default: `sealed`. See [Visibility](#dependencies-visibility). |

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH", "type": "path", "required": true, "value": "${installPath}/bin", "visibility": "public" },
    { "key": "JDK_HOME", "type": "constant", "value": "${deps.java.installPath}", "visibility": "public" }
  ],
  "dependencies": [
    {
      "identifier": "ocx.sh/java:21@sha256:a1b2c3d4e5f6...",
      "visibility": "private"
    },
    {
      "identifier": "ocx.sh/kitware/cmake:3.28@sha256:f6e5d4c3b2a1...",
      "name": "cmake"
    }
  ]
}
```

### Visibility {#dependencies-visibility}

A dependency's `visibility` controls how its environment variables propagate through the dependency chain. It has two boolean axes: `private` (visible to the package's own runtime) and `interface` (propagated to consumers).

| Value | Private surface (`--self`) | Interface surface (default) | Use case |
|---|---|---|---|
| `sealed` (default) | No | No | Structural dependency: content accessed by path, not env. Most deps. |
| `private` | Yes | No | Package's own shims need the dep's env; consumers don't. |
| `public` | Yes | Yes | Both the package and consumers need the dep's env. |
| `interface` | No | Yes | Meta-packages that forward env to consumers without using it. |

- `has_private()` is `true` for `private` and `public`. `has_interface()` is `true` for `public` and `interface`.
- The [composer][env-composition-edge-filter] uses these accessors to gate entry emission per surface at exec time.

#### Transitive Propagation {#dependencies-through-edge}

Along a chain (Root, Dep, Transitive), `Visibility::through_edge(child_eff)` yields `sealed` when the child's effective visibility does not export to consumers (`child_eff.has_interface() == false`), and the edge unchanged otherwise.

| Edge | Child effective | Result (from root) |
|---|---|---|
| `public` | `public` / `interface` | `public` |
| `public` | `private` / `sealed` | `sealed` |
| `private` | `public` / `interface` | `private` |
| `private` | `private` / `sealed` | `sealed` |
| `interface` | `public` / `interface` | `interface` |
| `interface` | `private` / `sealed` | `sealed` |
| `sealed` | any | `sealed` |

#### Diamond Merge {#dependencies-merge}

When two paths reach the same dependency, the most open visibility wins: each axis is OR-merged independently via `Visibility::merge`. It is computed at install time and stored in `resolve.json` (see [Env Composition: Edge Filter][env-composition-edge-filter]).

### Ordering {#dependencies-ordering}

Array position defines the canonical order for environment composition, and the first entry's environment is applied first. The dependency graph is walked depth first with dependencies before dependents, and a package reached twice keeps its first position.

<!-- external -->
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md

<!-- commands -->
[cmd-package-create]: ../command-line/package-publishing.md#package-create
[cmd-package-push]: ../command-line/package-publishing.md#package-push
[cmd-package-test]: ../command-line/package-publishing.md#package-test
[cmd-package-receipt]: ../command-line/package-consuming.md#package-receipt

<!-- guides -->
[in-depth-dependencies]: ../../explanation/dependencies.mdx
[authoring-dependencies]: ../../publishing/declare-dependencies.md
[env-composition-edge-filter]: ../../explanation/environments.mdx#edge-filter
