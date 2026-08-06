---
outline: deep
---
# Env Surface

The environment variables your package declares are the contract between you and every consumer. Get the surface right and dependent packages compose cleanly; get it wrong and consumers chase phantom paths or leak internal state into their shells. This page covers the publisher decisions: which variables to declare, which to mark visible, and how to migrate packages that pre-date the entry-visibility field.

## Path Variables vs Constants {#types}

Most env entries fall into one of two patterns: **path variables** that prepend a directory onto an existing PATH-like list, and **constant variables** that replace the value outright. The distinction matters at composition time. Path entries from multiple packages stack cleanly — `bin/` from one package, `bin/` from another — both end up on the consumer's PATH. Constants don't stack: when two packages both declare `JAVA_HOME`, the [last one in dependency order wins][in-depth-environments-last-wins]. The full composition model lives in the [environments in depth][in-depth-environments] page.

Pick `path` for anything that is a directory list (`PATH`, `MANPATH`, `LD_LIBRARY_PATH`, `PKG_CONFIG_PATH`). Pick `constant` for tool-prefix vars (`JAVA_HOME`, `CARGO_HOME`, `CMAKE_ROOT`) and version markers. Neither shape fits an **option-list** variable — see [Appending Option Lists](#lists) below. The [metadata reference][reference-env] documents all three shapes in full.

## Appending Option Lists {#lists}

Some variables are neither a directory list nor a single value — they are a flat, ordered list of flags a runtime reads all at once: [`JDK_JAVA_OPTIONS`][jdk-java-options], [`JAVA_TOOL_OPTIONS`][java-tool-options], [`NODE_OPTIONS`][node-options], [Go's `GODEBUG`][godebug-doc]. `path` joins with the platform path separator and prepends — wrong on both counts for a space- or comma-joined option string. `constant` replaces the whole value outright — the flags your dependency's own package declared for itself vanish the moment yours sets the same key.

`list` fits: it **appends** the contribution, joined by an author-chosen `separator`, removing any earlier occurrence of the same contribution first. Two packages contributing `-Xmx1g` and `-ea` to `JDK_JAVA_OPTIONS` both survive; if the same package re-declares one of them (a re-install, a launcher re-entry), the value does not grow.

```json
{
  "key": "JDK_JAVA_OPTIONS",
  "type": "list",
  "separator": " ",
  "value": "-Xmx2g",
  "visibility": "interface"
}
```

`separator` is not optional here the way it is in [`ocx.toml`][config-project-env] or [`--env`][cmd-run]: package metadata is the wire, where no human is present to be told what was assumed, so a publisher must spell it out explicitly. It must be non-empty and must not contain `=`, a newline, or a carriage return — the last two because every downstream export (a CI env file, a shell snippet, a JSON-lines record) is line-oriented, and a separator ending a line stops being a delimiter. See [Separator Is Required][reference-env-list-separator] in the metadata reference for the full rule, including the per-key agreement across a composition.

### Does the consumer actually resolve duplicates last-wins? {#lists-consumer-semantics}

`list` appends. Whether the *last* contribution wins depends entirely on how the consuming tool resolves duplicates in that variable — OCX never parses list elements, so it cannot enforce or verify this on your behalf. A short survey of common option-list variables:

| Variable | Consumer behavior |
|---|---|
| [`JDK_JAVA_OPTIONS`][jdk-java-options], [`NODE_OPTIONS`][node-options] (scalar flags), [`GODEBUG`][godebug-doc] | Last occurrence wins — append order gives the override you expect. |
| [`RUST_LOG`][env-filter] | **Most-specific target wins**, not append order — a broader directive appended after a narrower one (`warn` after `my_crate::noisy=trace`) does not silence it. Treat `list` on `RUST_LOG` as layering directives, not overriding them. |
| `-I`/`-L` search-path flags on a compiler command line | **First occurrence wins** — appending a later `-I` adds a fallback search directory, never an override. The polarity is the reverse of the other rows in this table. |

::: warning `NODE_OPTIONS` has no quoting mechanism
[Node.js accepts no escaping in `NODE_OPTIONS`][node-21575] — a value that would need quoting on a real command line (a path containing a space, for example) breaks for the consumer with no fix available on the producer side. Keep `list` values for `NODE_OPTIONS` free of whitespace.
:::

`JDK_JAVA_OPTIONS` does not have this problem: [the JVM launcher itself defines a quote grammar][jdk-java-options] — single or double quotes wrap an argument containing whitespace, and the launcher strips the pair before use. A publisher who needs a space in a contribution quotes it the same way they would on a command line; OCX's dedup never parses list elements, so a quoted value passes through the fold intact. The quoting is the JVM's own — not something `list` or its `separator` need to account for.

## Templates and Dependency Paths {#templates}

Two placeholders are available inside any env `value` template, resolved at exec time when `ocx package exec` or `ocx env` composes the package's environment:

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

- **Interface surface** — what consumers see when they run [`ocx package exec mypkg -- <cmd>`][cmd-exec] or compose `mypkg` as a dependency. PATH entries marked `public`, [`JAVA_HOME`][java], every variable a downstream caller depends on lives here.
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

When two packages on the same composition both declare the same constant variable (for example, two [Java][java] distributions each declaring `JAVA_HOME`), exactly one wins in `ocx package exec` / `ocx env`: the last one in topological dependency order. The first declaration is replaced silently in the main composition path. The full rule, including how transitive resolution preserves order, lives in [environments in depth][in-depth-environments-last-wins]. (The `ocx ci export` command runs an extra `ConstantTracker` pass that does emit a warning when truly unrelated TC entries collide.)

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

Vars without a `visibility` field now default to `"private"` — they reach the package's own launchers but not consumers. If your package has no declared entrypoints and relies entirely on consumers invoking `ocx package exec PKG -- cmd`, every var a consumer needs must be explicitly `"public"`.

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
[jdk-java-options]: https://docs.oracle.com/en/java/javase/21/docs/specs/man/java.html
[java-tool-options]: https://docs.oracle.com/javase/8/docs/technotes/guides/troubleshoot/envvars002.html
[node-options]: https://nodejs.org/api/cli.html#node_optionsoptions
[node-21575]: https://github.com/nodejs/node/issues/21575
[godebug-doc]: https://go.dev/doc/godebug
[env-filter]: https://docs.rs/env_filter/latest/env_filter/

<!-- reference -->
[reference-env]: ../reference/metadata.md#env
[reference-env-list-separator]: ../reference/metadata.md#env-list-separator
[reference-deps-visibility]: ../reference/metadata.md#dependencies-visibility

<!-- in-depth -->
[in-depth-environments]: ../in-depth/environments.md
[in-depth-environments-two-surfaces]: ../in-depth/environments.md#two-surfaces
[in-depth-environments-last-wins]: ../in-depth/environments.md#last-wins
[in-depth-entry-points]: ../in-depth/entry-points.md

<!-- commands -->
[cmd-exec]: ../reference/command-line.md#package-exec
[cmd-run]: ../reference/command-line.md#run

<!-- configuration -->
[config-project-env]: ../reference/configuration.md#project-config-env

<!-- authoring -->
[authoring-entry-points]: ./entry-points.md
