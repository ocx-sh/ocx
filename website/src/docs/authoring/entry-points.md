---
outline: deep
---
# Entry Points

`entrypoints` are named launchers OCX generates at install time. Each entry becomes a tiny script in the package's `entrypoints/` directory; when the package is selected via `ocx select`, those scripts land on the consumer's PATH as bare commands. The headline reason to declare them is **dependency encapsulation**: the launcher carries the package's own dep graph and runs in a clean environment, so two tools that share a runtime — Python, Node, the JVM — stop fighting over a single ambient version.

This page covers the publisher decisions: when to declare entrypoints at all, how to pick names that don't collide with the rest of the ecosystem, and how the `target` template threads dependency paths through.

## Why Encapsulate Through a Launcher {#why}

Imagine two tools you publish are JavaScript executables that depend on Node, and two more are Java tools that depend on a JDK. A consumer who installs all four and exposes their `bin/` directories ends up with four PATH entries and a single inherited `node` / `java` from the shell — whichever runtime the consumer happens to have first on PATH. The four tools now share one runtime. Upgrade Node and one tool breaks. Use the wrong JDK and another fails silently.

That conflict is structural, not a configuration mistake. Bare-binary exposure leaks a tool's private execution environment to the consumer's shell, and the consumer's shell is the one place where every package's deps collide.

Entry points cut the exposure surface to the binary alone. The launcher script that lands on PATH carries:

- a baked path to this package's root,
- a re-entry through [`ocx launcher exec`][cmd-exec], which reads `metadata.json` and composes the package's [private surface env][in-depth-environments] (its own env entries plus the env contributed by its declared dependencies) on top of the inherited shell environment, then resolves `target` against that PATH.

So when a JS tool's launcher runs, the dep's pinned [Node.js][nodejs] takes priority on PATH for the process it launches — the digest declared in `dependencies[]`. A second JS tool with a different Node pin runs the same way, with its own pinned interpreter winning the resolver race. Neither launcher exposes its pinned Node as a bare PATH entry on the consumer's shell, so the two tools never fight over a single ambient interpreter.

::: tip Mental model
Without entrypoints, packages publish *their environment* and consumers compose. With entrypoints, packages publish *executables*; the environment stays inside. That is the encapsulation dividend — and it is the only way two tools that share a runtime can coexist on one machine without a version manager arbitrating between them.
:::

## When to Declare Entry Points {#when}

Reach for `entrypoints` when one of these is true:

- **Your tool depends on a runtime another tool also depends on.** [Python][python] scripts, [Node.js][nodejs] CLIs, [JVM][jvm] tools, [Ruby][ruby] gems — anything where the executable is meaningless without a specific interpreter version on PATH. The launcher pins the interpreter inside the package; consumers never see the conflict.
- **Your tool needs to find a dependency at runtime.** A wrapper that calls into a sibling package's binary uses `${deps.NAME.installPath}` to point at it. OCX validates every `${deps.*}` reference at publish time; at exec time `ocx launcher exec` re-resolves the template against the dep's actual install path and runs the resulting binary.
- **You want the package to run with the env it declared.** The launcher re-enters via [`ocx launcher exec`][cmd-exec], which composes the package's declared env (its own entries plus the env contributed by its declared dependencies) on top of the inherited shell. PATH-based exposure cannot do this — the launched binary just inherits whatever the consumer's shell carried in.
- **Bare-binary exposure would leak too much.** A toolchain that ships fifty binaries but only wants three on PATH declares the three as entrypoints and leaves `bin/` private — consumers see exactly the public surface.

Skip `entrypoints` for self-contained static binaries ([Go][go] binaries, [Rust][rust] binaries, anything with no dynamic interpreter dep). [CMake][cmake], [ripgrep][ripgrep], [mold][mold] — these have no runtime to conflict over, so a public `${installPath}/bin` on PATH is enough. Encapsulation costs nothing in those cases, but it also wins nothing.

Packages that declare entrypoints typically demote `${installPath}/bin` from `public` to `private` (see [env surface][authoring-env-surface]). OCX prepends each installed package's `entrypoints/` directory to PATH at exec time (consumers see it as `<symlink-root>/current/entrypoints`), replacing the bare-bin exposure.

## Naming and Collisions {#naming}

Entry-point names must match `^[a-z0-9][a-z0-9_-]*$` and stay under 64 characters. The string is what consumers type at the shell, so it should read like a top-level command. `cmake-gen` is fine; `__internal-helper` is not — names starting with `_` are rejected.

Collisions are the failure mode publishers underestimate. OCX checks for them at two distinct points — at install time (within the package being installed and its transitive deps) and at compose time (when `ocx exec` or `ocx env` is given two or more roots) — and surfaces an `EntrypointCollision` error rather than silently picking one. `ocx select` itself never picks owners; it only flips the candidate symlink. The avoid-collisions rules:

- **Match the upstream binary name when wrapping a single tool.** If you ship [CMake][cmake], declare `cmake`, `ctest`, `cpack` — that's what users expect on PATH.
- **Namespace internal launchers.** A wrapper for `myorg/build-tools` should declare `myorg-build` or `mbt` rather than a generic `build` that any other tool might also want.
- **Look at the [package catalog][catalog]** before publishing public packages. Names already in use by upstream tools are the high-collision risk surface.

The full collision-detection mechanic, error format, and the cross-platform launcher caveats live in [entry points in depth][in-depth-entry-points] (see also `select`'s [collision section][select-collision] for how detection relates to the candidate symlink).

## Target Templates {#target}

The `target` field is the path OCX resolves at install time. It supports the same two placeholders as `env` values:

- `${installPath}` — this package's content directory.
- `${deps.NAME.installPath}` — a declared dependency's content directory, where `NAME` is the dependency's repository basename or its explicit `name` field.

A simple wrapper around a single bundled binary:

```json
{
  "entrypoints": [
    { "name": "cmake", "target": "${installPath}/bin/cmake" }
  ]
}
```

A meta-package that exposes a tool from a dependency without re-bundling it:

```json
{
  "dependencies": [
    { "identifier": "ocx.sh/cmake:3.28@sha256:abc...", "name": "cmake", "visibility": "public" }
  ],
  "entrypoints": [
    { "name": "cmake", "target": "${deps.cmake.installPath}/bin/cmake" }
  ]
}
```

`target` resolves to a filesystem path, not an interpreter import like [Python's entry-points convention][python-entry-points]. OCX validates the template at publish time (every `${deps.*}` reference must resolve to a declared dependency) and verifies dep roots exist at install time. Resolution of the *target binary* itself happens at exec time when `ocx launcher exec` reads `metadata.json` and the consumer types the launcher name.

::: warning Target binary not stat'd at install
A typo in the path under `${installPath}` (e.g. `bin/cmke`) will install cleanly and only fail when the launcher is invoked. Test the launcher with `ocx exec <pkg> -- <name>` after install to catch missing-target bugs early.
:::

### Worked Example: Python Script with a Pinned Interpreter {#target-python-example}

The encapsulation story is sharpest when the executable is a script rather than a native binary. Imagine packaging `mytool`, a [Python][python] CLI whose only file is `bin/mytool` containing:

```python
#!/usr/bin/env python3
import sys
print("mytool", "running on", sys.version)
```

The shebang resolves `python3` from PATH at exec time. Without entrypoints — bare `bin/` exposed as `public` PATH — every consumer's PATH ends up with `python3` resolving to whichever Python was first on PATH (the consumer's system Python, a [`mise`][mise]-managed Python, another OCX package's Python). Two such tools installed side-by-side are one upgrade away from breaking.

Entrypoints encapsulate by pinning Python *inside the launcher's environment*. Declare CPython as a `private` dependency, declare the script as an entry point, and demote `bin/` to private:

```json
{
  "type": "bundle",
  "version": 1,
  "dependencies": [
    {
      "identifier": "ocx.sh/cpython:3.13@sha256:abc...",
      "name": "cpython",
      "visibility": "private"
    }
  ],
  "env": [
    { "key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "private" }
  ],
  "entrypoints": [
    { "name": "mytool", "target": "${installPath}/bin/mytool" }
  ]
}
```

What happens at exec time:

1. Consumer types `mytool`. The launcher script in this package's `entrypoints/` runs.
2. The launcher re-enters via `ocx launcher exec` with the package root baked in. OCX composes the *private* surface env: PATH from the cpython dep prepended, this package's `bin/` prepended on top.
3. OCX exec's `bin/mytool`. The script's shebang triggers `/usr/bin/env python3`, which resolves `python3` against the composed PATH — finding the dep's pinned CPython, not the consumer's ambient one.

The consumer never sees `python3` on their shell PATH. Two Python tools installed side-by-side each carry their own pinned interpreter inside their launcher; neither leaks. That is the encapsulation dividend, made concrete.

`visibility: private` on the dep edge is the right choice here — your launcher needs `cpython`, but the consumer never wants to discover "this tool happens to use Python." Switch to `public` only if the consumer is supposed to compose the dep themselves (rare; that's what bare `cpython:3.13` is for).

::: tip Native binaries don't need any of this
A statically-linked [Go][go] or [Rust][rust] binary has no interpreter to pin. `entrypoints` adds nothing — bare `bin/` on PATH works. Encapsulation only earns its keep when there's a runtime to encapsulate.
:::

::: info Multi-platform launchers
A single `entrypoints` declaration covers every platform of the package. OCX generates `.sh` launchers for Unix shells and `.cmd` launchers for Windows from the same metadata. The Git Bash and PowerShell caveats live in [entry points in depth][in-depth-entry-points]; the [PATHEXT][cmd-exec-pathext] note on `ocx exec` covers the Windows command-line lookup.
:::

## See Also {#see-also}

- [Entry points reference][reference-entry-points] — every field, every constraint
- [Entry points in depth][in-depth-entry-points] — launcher mechanics, synth-PATH, multi-owner reporting
- [Env surface][authoring-env-surface] — when to demote `bin/` from public to private
- [`select` reference][select-collision] — collision detection at select time

<!-- external -->
[python-entry-points]: https://packaging.python.org/en/latest/specifications/entry-points/
[python]: https://www.python.org/
[nodejs]: https://nodejs.org/
[jvm]: https://openjdk.org/
[ruby]: https://www.ruby-lang.org/
[go]: https://go.dev/
[rust]: https://www.rust-lang.org/
[cmake]: https://cmake.org/
[ripgrep]: https://github.com/BurntSushi/ripgrep
[mold]: https://github.com/rui314/mold
[mise]: https://mise.jdx.dev/

<!-- reference -->
[reference-entry-points]: ../reference/metadata.md#entry-points
[select-collision]: ../reference/command-line.md#select-entry-point-collision

<!-- commands -->
[cmd-exec]: ../reference/command-line.md#exec
[cmd-exec-pathext]: ../reference/command-line.md#exec

<!-- in-depth -->
[in-depth-entry-points]: ../in-depth/entry-points.md
[in-depth-environments]: ../in-depth/environments.md

<!-- internal -->
[catalog]: ../catalog.md

<!-- authoring -->
[authoring-env-surface]: ./env-surface.md
