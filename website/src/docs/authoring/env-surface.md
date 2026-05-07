---
outline: deep
---
# Env Surface

The environment variables your package declares are the contract between you and every consumer. Get the surface right and dependent packages compose cleanly; get it wrong and consumers chase phantom paths or leak internal state into their shells. This page covers the publisher decisions: which variables to declare, which to mark visible, and how to migrate packages that pre-date the entry-visibility field.

## Path Variables vs Constants {#types}

Most env entries fall into one of two patterns: **path variables** that prepend a directory onto an existing PATH-like list, and **constant variables** that replace the value outright. The distinction matters at composition time. Path entries from multiple packages stack cleanly — `bin/` from one package, `bin/` from another — both end up on the consumer's PATH. Constants don't stack: when two packages both declare `JAVA_HOME`, the [last one in dependency order wins][in-depth-environments-last-wins]. The full composition model lives in the [environments in depth][in-depth-environments] page.

Pick `path` for anything that is a directory list (`PATH`, `MANPATH`, `LD_LIBRARY_PATH`, `PKG_CONFIG_PATH`). Pick `constant` for tool-prefix vars (`JAVA_HOME`, `CARGO_HOME`, `CMAKE_ROOT`) and version markers. The [metadata reference][reference-env] documents both shapes in full.

## Templates and Dependency Paths {#templates}

Two placeholders are available inside any env `value` template, resolved at exec time when `ocx exec` or `ocx env` composes the package's environment:

- `${installPath}` resolves to the absolute path of the package's own `content/` directory.
- `${deps.NAME.installPath}` resolves to a declared dependency's `content/` directory, where `NAME` is the last path segment of the dependency's OCI repository or its explicit `name` field.

The second placeholder is the publisher's escape hatch for declaring "I need to find my dependency's files." A wrapper package that bundles a configuration generator and points at its dependency's binary uses `${deps.cmake.installPath}/bin/cmake-gen` to keep the path stable across registry layouts:

```json
{
  "env": [
    { "key": "MYTOOL_HELPER", "type": "constant", "value": "${deps.cmake.installPath}/bin/cmake-gen", "visibility": "public" }
  ]
}
```

OCX validates every `${deps.*}` reference both locally during `ocx package create --metadata <file>` (no network needed) and again during `ocx package push` — a typo gets caught before the manifest reaches the registry. Only `${installPath}` and `${deps.NAME.installPath}` are recognized; any other `${...}` token is rejected at publish time.

## Choosing Visibility {#visibility}

Each `env` entry carries a `visibility` field that controls which surface it contributes to. The model is two surfaces, not a single visibility flag:

- **Interface surface** — what consumers see when they run [`ocx exec mypkg -- <cmd>`][cmd-exec] or compose `mypkg` as a dependency. PATH entries marked `public`, [`JAVA_HOME`][java], every variable a downstream caller depends on lives here.
- **Private surface** — what the package's own [generated launchers][in-depth-entry-points] see at exec time. Internal flags, lock-file paths, and any variable a consumer should never observe live here.

Three values map onto the two surfaces:

| Value | Interface surface | Private surface | Use case |
|---|---|---|---|
| `private` (default) | No | Yes | Internal paths the package's own launchers need; not part of the public contract. |
| `public` | Yes | Yes | Variables consumers should see — `PATH`, `JAVA_HOME`, tool-specific prefix paths. Both surfaces. |
| `interface` | Yes | No | Values forwarded to consumers but not used by the package's own runtime — `PKG_CONFIG_PATH`, library include hints. |

The `sealed` value is rejected at parse time on `env` entries — a declared entry that contributes to neither surface is dead configuration. The deeper mechanic — how publisher-declared visibility composes with [dependency-edge visibility][reference-deps-visibility] during the resolution walk — lives in [environments in depth][in-depth-environments-two-surfaces].

::: tip Most variables are public
For a typical bare-binary package ([`cmake`][cmake], [`node`][nodejs], [`uv`][uv]), the entries you declare are exactly the ones consumers need: `PATH`, the tool's `*_HOME`, occasionally a `MANPATH`. Mark them all `"visibility": "public"`. The encapsulation dividend kicks in for *additional* internal vars added later — those default to `private` and stay hidden from consumers without any extra annotation.
:::

## Last-Wins for Constants {#last-wins}

When two packages on the same composition both declare the same constant variable (for example, two [Java][java] distributions each declaring `JAVA_HOME`), exactly one wins in `ocx exec` / `ocx env`: the last one in topological dependency order. The first declaration is replaced silently in the main composition path. The full rule, including how transitive resolution preserves order, lives in [environments in depth][in-depth-environments-last-wins]. (The `ocx shell profile load` and `ocx ci export` commands run an extra `ConstantTracker` pass that does emit a warning when truly unrelated TC entries collide.)

Treat conflicting constants as a publisher signal. If your package declares `JAVA_HOME` and a sibling package already does too, the deployment is asking two tools to share one slot — the consumer needs to pick one to depend on and seal the other's env, not both publishers fighting over the same key.

## Migrating from Implicitly Public {#migrating}

Entry visibility (`private` / `public` / `interface` on each `env` entry) arrived with the entry-points feature release. Before that, all env vars were implicitly public — every declared variable reached consumers without annotation.

The migration cost is a one-time annotation pass on your `metadata.json`. Most tools want `PATH`, `JAVA_HOME`, and similar vars visible to consumers — mark those `"visibility": "public"`. Any var you add after the migration that you intentionally want private gets no annotation (the default is `private`). That is the encapsulation dividend: new internal vars stay hidden without any extra work.

This breaking change ships in the same release as entry points — one migration window, not two.

### What the diff looks like {#migrating-diff}

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

### Decision guide {#migrating-decision}

| Variable pattern | Recommended visibility |
|---|---|
| `PATH` pointing at `${installPath}/bin` | `"public"` (consumers need it on their PATH) |
| [`JAVA_HOME`][java], [`CMAKE_ROOT`][cmake], tool prefix vars | `"public"` (consumers reference these by name) |
| `MANPATH`, `PKG_CONFIG_PATH`, `ACLOCAL_PATH` | `"public"` if consumers need the content; `"interface"` if the package itself does not use these paths |
| Internal flags (`_MY_TOOL_INIT`, lock files, IPC paths) | `"private"` (default — no annotation needed) |

::: tip Packages with entrypoints
If your package declares [`entrypoints`][authoring-entry-points], consumers reach the launchers via each installed package's `entrypoints/` directory — OCX prepends it to PATH automatically at exec time (consumers see the path as `<symlink-root>/current/entrypoints`). You do not need to keep `PATH += ${installPath}/bin` as `"public"` for consumer PATH resolution once entrypoints are declared. The `${installPath}/bin` path entry can be `"private"` (launcher-only) if the entry-point launcher is the sole intended consumer interface.
:::

## See Also {#see-also}

- [`env` reference][reference-env] — every field on a `env` entry
- [Environments in depth][in-depth-environments] — composition order, edge filter, conflicting constants
- [Entry points][authoring-entry-points] — when entrypoints replace exposed `PATH` entries
- [Dependency-edge visibility][reference-deps-visibility] — how dep declarations propagate env

<!-- external -->
[cmake]: https://cmake.org/
[java]: https://www.java.com/
[nodejs]: https://nodejs.org/
[uv]: https://docs.astral.sh/uv/

<!-- reference -->
[reference-env]: ../reference/metadata.md#env
[reference-deps-visibility]: ../reference/metadata.md#dependencies-visibility

<!-- in-depth -->
[in-depth-environments]: ../in-depth/environments.md
[in-depth-environments-two-surfaces]: ../in-depth/environments.md#two-surfaces
[in-depth-environments-last-wins]: ../in-depth/environments.md#last-wins
[in-depth-entry-points]: ../in-depth/entry-points.md

<!-- commands -->
[cmd-exec]: ../reference/command-line.md#exec

<!-- authoring -->
[authoring-entry-points]: ./entry-points.md
