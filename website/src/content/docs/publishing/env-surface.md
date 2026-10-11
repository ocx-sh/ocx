---
title: "Declare the environment"
description: "Choose env variable types, interpolation tokens and visibility so consumers get exactly the variables your package should publish."
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

By the end of this page the `env` block of your `metadata.json` declares every variable your package contributes, each with the right type and visibility. The block is the contract between you and every consumer. A good surface composes cleanly with dependent packages, and a bad one leaks internal state into consumer shells. You need a bundle and a `metadata.json` to edit. The [metadata reference][reference-env] documents every field, and [environments in depth][in-depth-environments] owns the composition rules.

## Pick the variable type {#types}

Most entries are one of two shapes. A **path** variable prepends a directory onto a PATH-like list. A **constant** variable replaces the value outright. Path entries from several packages stack, so every package's `bin/` ends up on the consumer's PATH. Constants do not stack, and when two packages declare `JAVA_HOME`, the [last one in dependency order wins][in-depth-environments-last-wins].

Pick `path` for directory lists such as `PATH`, `MANPATH`, `LD_LIBRARY_PATH` and `PKG_CONFIG_PATH`. Pick `constant` for prefix variables such as `JAVA_HOME`, `CARGO_HOME` and `CMAKE_ROOT`, and for version markers. Neither shape fits an option-list variable, so use [`list`](#lists) for those.

## Append to option lists {#lists}

Some variables are a flat, ordered list of flags that a runtime reads at once. Examples are [`JDK_JAVA_OPTIONS`][jdk-java-options], [`JAVA_TOOL_OPTIONS`][java-tool-options], [`NODE_OPTIONS`][node-options] and [Go's `GODEBUG`][godebug-doc]. A `path` entry joins with the platform path separator and prepends, which is wrong for a space-joined or comma-joined string. A `constant` entry replaces the whole value, so the flags your dependency declared for itself vanish.

The `list` type **appends** your contribution, joined by a `separator` you choose. It first removes any earlier occurrence of the same contribution. Two packages that contribute `-Xmx1g` and `-ea` to `JDK_JAVA_OPTIONS` both survive. If one package re-declares a flag, the value does not grow.

```json
{
  "key": "JDK_JAVA_OPTIONS",
  "type": "list",
  "separator": " ",
  "value": "-Xmx2g",
  "visibility": "interface"
}
```

The `separator` is required here, unlike in [`ocx.toml`][config-project-env] or [`--env`][cmd-run]. Package metadata is the wire, and no human is present to be told what was assumed. It must be non-empty and must not contain `=`, a newline or a carriage return. Every downstream export is line-oriented, and a separator that ends a line stops being a delimiter. See [Separator Is Required][reference-env-list-separator] for the full rule, including per-key agreement across a composition.

### Check how the consumer resolves duplicates {#lists-consumer-semantics}

The `list` type appends. Whether the last contribution wins depends on how the consuming tool resolves duplicates, and OCX never parses list elements to enforce it. This survey covers common option-list variables:

| Variable | Consumer behavior |
|---|---|
| [`JDK_JAVA_OPTIONS`][jdk-java-options], [`NODE_OPTIONS`][node-options] (scalar flags), [`GODEBUG`][godebug-doc] | Last occurrence wins, so append order gives the override you expect. |
| [`RUST_LOG`][env-filter] | **Most-specific target wins**, not append order. A broader directive appended after a narrower one (`warn` after `my_crate::noisy=trace`) does not silence it. Treat `list` on `RUST_LOG` as layering directives, not overriding them. |
| `-I`/`-L` search-path flags on a compiler command line | **First occurrence wins.** A later `-I` adds a fallback search directory and never an override. The polarity is the reverse of the other rows. |

:::caution[`NODE_OPTIONS` has no quoting mechanism]
[Node.js accepts no escaping in `NODE_OPTIONS`][node-21575]. A value that needs quoting on a real command line breaks for the consumer, and the producer cannot fix it. A path with a space is one example. Keep `list` values for `NODE_OPTIONS` free of whitespace.
:::

`JDK_JAVA_OPTIONS` has no such problem, because [the JVM launcher defines a quote grammar][jdk-java-options]. Single or double quotes wrap an argument that contains whitespace, and the launcher strips the pair. Quote a contribution the way you would on a command line. OCX's dedup never parses list elements, so a quoted value passes through intact.

## Reference install paths with tokens {#templates}

A `value` cannot hardcode an install path, because content-addressed storage puts the same package at a different path on every machine. The `${...}` tokens name "wherever OCX put me" or "wherever my dependency landed". The install-path tokens take an optional `:native` or `:posix` [render modifier][reference-env-render] that controls path-separator rendering only.

- `${installPath}` (and its exact alias `${self.installPath}`) is this package's own `content/` directory.
- `${self.env.KEY}` is the resolved value of this package's own `KEY` variable, declared earlier in the same `env` array. It takes no modifier, because OCX cannot know whether the variable holds a path.
- `${deps.NAME.installPath}` is a declared dependency's `content/` directory. `NAME` is the last path segment of the dependency's repository or its explicit [`name`][authoring-deps-name] field.

A wrapper that points at its dependency's binary uses `${deps.cmake.installPath}` to keep the path stable across registry layouts:

```json
{
  "env": [
    { "key": "MYTOOL_HELPER", "type": "constant", "value": "${deps.cmake.installPath}/bin/cmake-gen", "visibility": "public" }
  ]
}
```

The `${...}` namespace is closed. OCX refuses any occurrence that is not one of the three tokens above. To ship another tool's own `${...}` syntax verbatim, such as `${workspaceFolder}` from [devcontainer.json][devcontainer-vars], write `$${`. Then `$${workspaceFolder}` publishes as the literal text `${workspaceFolder}`. That is the only escape.

OCX validates every reference during `ocx package create --metadata <file>`, with no network, and again during `ocx package push`. A typo never reaches the registry. See [Interpolation Tokens][reference-env-interpolation] for the grammar and the exact refusal.

:::tip[A `${self.env.KEY}` chain has a byte ceiling]
Each `${self.env.KEY}` substitutes the already-resolved value, so a chain that repeats a reference doubles with every hop. A resolved value is capped at 64 KiB. OCX refuses a longer one with exit 65 at composition time.
:::

## Choose visibility {#visibility}

Each `env` entry carries a `visibility` field that selects which surface it contributes to:

- **Interface surface:** what consumers see when they run [`ocx package exec mypkg -- <cmd>`][cmd-exec] or compose `mypkg` as a dependency. Public PATH entries and [`JAVA_HOME`][java] live here.
- **Private surface:** what the package's own [generated launchers][in-depth-entry-points] see at exec time. Internal flags and lock-file paths live here.

| Value | Interface surface | Private surface | Use case |
|---|---|---|---|
| `private` (default) | No | Yes | Internal paths the package's own launchers need. |
| `public` | Yes | Yes | Variables consumers should see, such as `PATH`, `JAVA_HOME` and prefix paths. |
| `interface` | Yes | No | Values forwarded to consumers but unused by the package, such as `PKG_CONFIG_PATH` and include hints. |

OCX rejects `sealed` on `env` entries at parse time, because an entry that contributes to neither surface is dead configuration. The way entry visibility combines with [dependency-edge visibility][reference-deps-visibility] is in [environments in depth][in-depth-environments-two-surfaces].

:::tip[Most variables are public]
A bare-binary package such as [`cmake`][cmake], [`node`][nodejs] or [`uv`][uv] declares exactly what consumers need: `PATH`, the `*_HOME` variable and sometimes `MANPATH`. Mark them all `"visibility": "public"`. An entry without a `visibility` field is `private`, so consumers see none of your variables until you mark the ones they need. Internal variables stay hidden by default.<a id="migrating"></a><a id="migrating-diff"></a>
:::

### Decision guide {#migrating-decision}

| Variable pattern | Recommended visibility |
|---|---|
| `PATH` pointing at `${installPath}/bin` | `"public"` (consumers need it on their PATH) |
| [`JAVA_HOME`][java], [`CMAKE_ROOT`][cmake], prefix vars | `"public"` (consumers reference these by name) |
| `MANPATH`, `PKG_CONFIG_PATH`, `ACLOCAL_PATH` | `"public"` if consumers need the content, `"interface"` if the package does not use these paths |
| Internal flags (`_MY_TOOL_INIT`, lock files, IPC paths) | `"private"` (default, no annotation needed) |

## Avoid last-wins conflicts {#last-wins}

Say two [Java][java] distributions in one composition both set `JAVA_HOME`. The last one in topological dependency order wins in `ocx package exec` and `ocx env`. The first is replaced silently. The `--ci` flag of `ocx env` and `ocx package env` also runs a `ConstantTracker` pass that warns when unrelated entries collide. The full rule is in [environments in depth][in-depth-environments-last-wins].

Treat a conflicting constant as a signal. If a sibling package already declares your `JAVA_HOME`, two publishers are fighting over one slot. The consumer should depend on one of them and seal the other's env.

## Check what consumers receive {#check-visibility}

Run [`ocx package env`][cmd-package-env] on the pushed package to see the consumer view. Add `--self` to see the private view:

```sh
ocx package env ghcr.io/acme/mytool:1.0.0
ocx package env --self ghcr.io/acme/mytool:1.0.0
```

The first command prints a table with `Key`, `Type` and `Value` columns that lists the `public` and `interface` entries. A package that marks `PATH` and `JAVA_HOME` as `"public"` shows both keys there. The second lists the `public` and `private` entries the package's own launchers see.

:::tip[Packages with entrypoints]
If your package declares [`entrypoints`][authoring-entry-points], OCX prepends each installed package's `entrypoints/` directory to PATH at exec time. Consumers see it as `<symlink-root>/current/entrypoints`. The `${installPath}/bin` entry can then be `"private"`, so the launcher is the only consumer interface.
:::

## See also {#see-also}

- [`env` reference][reference-env] for every field on an `env` entry
- [Environments in depth][in-depth-environments] for composition order, the edge filter and conflicting constants
- [Add named launchers][authoring-entry-points] for when entrypoints replace exposed `PATH` entries
- [Dependency-edge visibility][reference-deps-visibility] for how dependency declarations propagate env

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
[devcontainer-vars]: https://containers.dev/implementors/json_reference/

<!-- reference -->
[reference-env]: ../reference/metadata/env.md#env
[reference-env-list-separator]: ../reference/metadata/env.md#env-list-separator
[reference-deps-visibility]: ../reference/metadata/dependencies.md#dependencies-visibility
[reference-env-interpolation]: ../reference/metadata/env.md#env-interpolation
[reference-env-render]: ../reference/metadata/env.md#env-interpolation-render

<!-- in-depth -->
[in-depth-environments]: ../explanation/environments.mdx
[in-depth-environments-two-surfaces]: ../explanation/environments.mdx#two-surfaces
[in-depth-environments-last-wins]: ../explanation/environments.mdx#last-wins
[in-depth-entry-points]: ../explanation/entry-points.md

<!-- commands -->
[cmd-exec]: ../reference/command-line/package-consuming.md#package-exec
[cmd-package-env]: ../reference/command-line/package-consuming.md#package-env
[cmd-run]: ../reference/command-line/commands.md#exec

<!-- configuration -->
[config-project-env]: ../guides/project-toolchain.mdx#project-env

<!-- authoring -->
[authoring-entry-points]: ./entry-points.md
[authoring-deps-name]: ./declare-dependencies.md#name-field
