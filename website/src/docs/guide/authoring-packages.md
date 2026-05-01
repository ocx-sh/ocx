---
outline: deep
---
# Authoring Packages

::: warning Placeholder
This section is a placeholder for in-depth guides on authoring OCX packages — designing bundle metadata, declaring dependencies and visibility, choosing entry points, building tarballs, and publishing with [`ocx package push`][cmd-push].

The content is being scoped in a [tracking issue][tracking-issue]. If you want to help shape what lands here, comment on the issue or open a PR.
:::

## Planned Topics {#topics}

The following topics are on the roadmap for this section:

- **Bundle anatomy** — what goes in a tarball, `strip_components`, file layout choices
- **Declaring dependencies** — choosing edge visibility (`public` / `interface` / `private` / `sealed`), pinning by tag vs. digest, when a `name` field is needed
- **Designing the env surface** — entry visibility, when to mark a variable `private`, last-wins constants vs prepended path entries, template substitution
- **Entry points** — picking names, avoiding collisions across the ecosystem, `target` template patterns
- **Building and pushing** — local tarball assembly, [`ocx package push`][cmd-push] usage, `--cascade` for tag aliases
- **Multi-platform packages** — OCI image indexes, per-platform manifests, the publisher view of platform resolution
- **Migration patterns** — adapting Homebrew formulae, repackaging GitHub Releases, mirroring upstream registries

## Migrating a pre-entry-points package to entry visibility {#migration}

Entry visibility (`private` / `public` / `interface` on each `env` entry) arrived with the entry-points feature. Before that, all env vars were implicitly public — every declared variable reached consumers without annotation.

The migration cost is a one-time annotation pass on your `metadata.json`. Most tools want `PATH`, `JAVA_HOME`, and similar vars visible to consumers — mark those `"visibility": "public"`. Any var you add after the migration that you intentionally want private gets no annotation (the default is `private`). That is the encapsulation dividend: new internal vars stay hidden without any extra work.

This breaking change is bundled with the entry-points feature port. One migration window, not two.

### What the diff looks like

Before (no visibility field — each var was implicitly public):

```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH",      "type": "path",     "value": "${installPath}/bin" },
    { "key": "JAVA_HOME", "type": "constant", "value": "${installPath}" },
    { "key": "MANPATH",   "type": "path",     "value": "${installPath}/share/man" }
  ]
}
```

After (explicit `"visibility": "public"` on every var that consumers should see):

```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH",      "type": "path",     "value": "${installPath}/bin",          "visibility": "public" },
    { "key": "JAVA_HOME", "type": "constant", "value": "${installPath}",              "visibility": "public" },
    { "key": "MANPATH",   "type": "path",     "value": "${installPath}/share/man",    "visibility": "public" }
  ]
}
```

Vars without a `visibility` field now default to `"private"` — they reach the package's own launchers but not consumers. If your package has no declared entrypoints and relies entirely on consumers invoking `ocx exec PKG -- cmd`, every var a consumer needs must be explicitly `"public"`.

### Decision guide

| Variable pattern | Recommended visibility |
|---|---|
| `PATH` pointing at `${installPath}/bin` | `"public"` (consumers need it on their PATH) |
| `JAVA_HOME`, `CMAKE_ROOT`, tool prefix vars | `"public"` (consumers reference these by name) |
| `MANPATH`, `PKG_CONFIG_PATH`, `ACLOCAL_PATH` | `"public"` if consumers need the content; `"interface"` if the package itself does not use these paths |
| Internal flags (`_MY_TOOL_INIT`, lock files, IPC paths) | `"private"` (default — no annotation needed) |

::: tip Packages with entrypoints
If your package declares entrypoints, consumers reach the launchers via the synthetic `PATH ⊳ <pkg-root>/entrypoints` entry — OCX adds this automatically at exec time. You do not need to keep `PATH += ${installPath}/bin` as `"public"` for consumer PATH resolution once entrypoints are declared. The `${installPath}/bin` path entry can be `"private"` (launcher-only) if the entry-point launcher is the sole intended consumer interface.
:::

## See Also While This Page Fills In {#interim}

Until the dedicated guides land, the following pages cover the underlying mechanics:

- [Metadata reference][metadata-ref] — every field a publisher can set
- [Environments (in-depth)][env-composition] — how publisher-declared visibility shapes consumer environments
- [Entry Points (in-depth)][entry-points] — full launcher generation and PATH integration
- [`ocx package push` reference][cmd-push] — the publish command

<!-- tracking -->
[tracking-issue]: https://github.com/ocx-sh/ocx/issues/70

<!-- in-depth -->
[env-composition]: ../in-depth/environments.md
[entry-points]: ../in-depth/entry-points.md

<!-- reference -->
[metadata-ref]: ../reference/metadata.md
[cmd-push]: ../reference/command-line.md#package-push
