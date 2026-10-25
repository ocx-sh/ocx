---
title: "Add named launchers"
description: "Declare entry points so consumers get bare commands that run in the package's own pinned environment."
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

By the end of this page your package declares named launchers. OCX generates one small script per name at install time. Each script lands on the consumer's PATH as a bare command once the consumer selects the package with `ocx package select` or `ocx package install --select`. The launcher runs the command in the package's own environment, so two packages that share a runtime stop fighting over one ambient interpreter.

You need a package with a `bin/` directory and, for scripted tools, a [dependency][authoring-dependencies] that provides the interpreter. The [concept page][in-depth-entry-points] explains how launchers work, and the [field reference][reference-entry-points] lists every key.

## Why encapsulate through a launcher {#why}

Say two of your packages are JavaScript tools that depend on Node, and two more are Java tools that depend on a JDK. A consumer who exposes all four `bin/` directories gets one inherited `node` and one `java`, whichever the shell finds first. Upgrade Node and one tool breaks. Use the wrong JDK and another fails silently.

An entry point cuts the exposure down to the launcher alone. The launcher re-enters through `ocx launcher exec`, an internal entry point hidden from help that the [concept page][in-depth-entry-points] describes. It composes the package's [private surface][in-depth-environments] on top of the shell environment. That surface holds the package's own `env` entries plus those of its declared dependencies. The pinned [Node.js][nodejs] from `dependencies[]` wins on the composed PATH, and it never appears as a bare entry on the consumer's shell.

## When to declare entry points {#when}

Declare `entrypoints` when one of these is true:

- **Your package depends on a runtime another package also uses.** [Python][python] scripts, [Node.js][nodejs] CLIs, [JVM][jvm] tools and [Ruby][ruby] gems are meaningless without a specific interpreter on PATH.
- **Your package must find a dependency at run time.** Declare the dependency with `visibility: private` and use `${deps.NAME.installPath}` in an `env` value. The launcher resolves the name against that PATH.
- **The tool must run with the env you declared.** PATH exposure cannot do that, because the launched binary inherits whatever the consumer's shell carried in.
- **Exposing `bin/` would leak too much.** A toolchain with fifty binaries can declare three entry points and keep `bin/` private.

Skip entry points for self-contained static binaries such as [Go programs][go] or [Rust][rust] builds. [CMake][cmake], [ripgrep][ripgrep] and [mold][mold] have no runtime to conflict over, so a public `${installPath}/bin` on PATH is enough.

A package with entry points usually demotes `${installPath}/bin` from `public` to `private` (see [declare the environment][authoring-env-surface]). OCX prepends each installed package's `entrypoints/` directory to PATH at exec time, and consumers see it as `<symlink-root>/current/entrypoints`.

## Pick names that do not collide {#naming}

The name is what consumers type at the shell, so it should read like a top-level command. The [reference][reference-entry-points] gives the exact name grammar. `cmake-gen` is fine, and `__internal-helper` is rejected.

OCX checks for collisions at install time and when `ocx package exec` or `ocx env` receives two or more roots. It reports an `EntrypointCollision` error and never picks one silently. Three habits avoid the error:

- **Match the upstream binary name** when you wrap a single upstream binary. If you ship [CMake][cmake], declare `cmake`, `ctest` and `cpack`.
- **Namespace internal launchers.** A wrapper for `myorg/build-tools` should declare `myorg-build` or `mbt`, not a generic `build`.
- **Search the [package catalog][catalog]** before you publish a public package. Names that upstream projects already use are the high-risk set.

[Entry points in depth][in-depth-entry-points] covers the error format and the [collision rules][select-collision].

## Point the launcher at the binary {#dispatch}

Each key of `entrypoints` is a command name, and the value is `{}` in the common case. The launcher searches the composed `PATH` for the key. That search honors `PATHEXT` on Windows, so a packaged `tool.bat` or `tool.cmd` resolves by bare name. Declare the binary's location once through `env`, and every entry name picks it up. The reference lists the optional `command` and `args` fields.

To wrap a single bundled binary, put `bin/` on the PATH and declare the name:

```json
{
  "env": [
    { "key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "private" }
  ],
  "entrypoints": {
    "cmake": {}
  }
}
```

To expose a binary from a dependency without re-bundling it, put the dependency's `bin/` on the composed PATH:

```json
{
  "dependencies": [
    { "identifier": "ocx.sh/kitware/cmake:3.28@sha256:abc...", "name": "cmake", "visibility": "public" }
  ],
  "env": [
    { "key": "PATH", "type": "path", "value": "${deps.cmake.installPath}/bin", "visibility": "private" }
  ],
  "entrypoints": {
    "cmake": {}
  }
}
```

:::caution[OCX does not check the binary at install]
A typo such as `cmke` for `cmake` installs cleanly and fails only when the launcher runs. Test every launcher with `ocx package exec <pkg> -- <name>` after install.
:::

### Bake leading arguments {#args}

Use `args` to put fixed arguments in front of the ones the user types. The launcher prepends them on every call. Use `command` when the dispatched binary has another name than the launcher:

```json
{
  "entrypoints": {
    "mytool": { "command": "python", "args": ["${installPath}/app/main.py"] }
  }
}
```

Running `mytool a b` runs `python <content>/app/main.py a b`.

- Each `args` element is one argv token. There is no word splitting, so a path with spaces stays one argument.
- `${installPath}` works in `args`, with its render modifiers. `${deps.*}` and `${self.env.*}` are refused at publish time.
- `command` is a plain name, never a path, and it takes no interpolation. The launcher resolves it on the composed `PATH`.
- To run a dependency's binary, put that dependency's `bin/` on the composed `PATH` through `env`, as in the dependency example above.

### Worked example: a Python script with a pinned interpreter {#target-python-example}

Take `mytool`, a [Python][python] CLI whose only file is `bin/mytool`:

```python
#!/usr/bin/env python3
import sys
print("mytool", "running on", sys.version)
```

The shebang resolves `python3` from PATH at exec time. With `bin/` exposed as `public`, that is whichever Python the consumer's shell finds first. A system Python, a [`mise`][mise]-managed one or another OCX package's Python can all win. Declare CPython as a private dependency, declare the script as an entry point, and demote `bin/` to private:

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
  "entrypoints": {
    "mytool": {}
  }
}
```

At exec time this happens:

1. The consumer types `mytool`, and the launcher in `entrypoints/` runs.
2. The launcher re-enters through `ocx launcher exec`. OCX composes the private env: the cpython PATH first, then this package's `bin/` on top.
3. OCX runs `bin/mytool`. The shebang resolves `python3` against the composed PATH and finds the pinned CPython.

The consumer never sees `python3` on their shell PATH. Keep the dependency edge `private`, because consumers should not have to learn that the package uses Python. Use `public` only when consumers should compose the dependency themselves.

## Declare the executables {#binaries}

Tools such as CI matrices and [Bazel][bazel] toolchain rules want to know whether `cmake` resolves on PATH without installing the package. The optional `binaries` array answers that with bare names. List the names that an interface-visible [`path`][reference-env-path] variable puts on PATH:

```json
{
  "env": [
    { "key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "public" }
  ],
  "binaries": ["cmake", "cpack", "ctest"]
}
```

The array is a claim, and OCX does not verify it. [`ocx package push`][cmd-package-push] streams layers without unpacking them, and a package can reuse layers by digest. No step ever holds the full content tree, so a check against disk would break the streaming model. Other ecosystems make the same trade: [npm's `bin`][npm-bin], [Cargo's `[[bin]]`][cargo-bin] and [nixpkgs' `meta.mainProgram`][nixpkgs-main-program].

[`ocx package create`][cmd-package-create] with `--bin-scan` fills or checks the claim against the tree it can see. Declare layers added at push time by hand. The [reference][reference-binaries-writing] has the name grammar.

An omitted `binaries` means nobody declared the field, and `"binaries": []` asserts that the package puts no executables on PATH. Never list a launcher name that only exists as an entry point. `binaries` names files you shipped, and `entrypoints` names launchers OCX generates. The same name may appear in both, as with `cmake` in the examples above. The reference explains [the difference][reference-binaries].

## See also {#see-also}

- [Entry points reference][reference-entry-points] for every field and constraint
- [Executables reference][reference-binaries] for the `binaries` field and its name grammar
- [Entry points in depth][in-depth-entry-points] for launcher mechanics, synth-PATH and multi-owner reporting
- [Declare the environment][authoring-env-surface] for demoting `bin/` from public to private
- [`select` collisions][select-collision] for detection at select time

<!-- external -->
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
[bazel]: https://bazel.build/
[npm-bin]: https://docs.npmjs.com/cli/v11/configuring-npm/package-json#bin
[cargo-bin]: https://doc.rust-lang.org/cargo/reference/cargo-targets.html#binaries
[nixpkgs-main-program]: https://nixos.org/manual/nixpkgs/stable/#var-meta-mainProgram
[catalog]: https://index.ocx.sh

<!-- reference -->
[reference-entry-points]: ../reference/metadata/entry-points.md#entry-points
[reference-binaries]: ../reference/metadata/entry-points.md#executables
[reference-binaries-writing]: ../reference/metadata/entry-points.md#executables-writing
[reference-env-path]: ../reference/metadata/env.md#env-path
[select-collision]: ../explanation/entry-points.md#collisions

<!-- commands -->
[cmd-package-create]: ../reference/command-line/package-publishing.md#package-create
[cmd-package-push]: ../reference/command-line/package-publishing.md#package-push

<!-- in-depth -->
[in-depth-entry-points]: ../explanation/entry-points.md
[in-depth-environments]: ../explanation/environments.mdx

<!-- authoring -->
[authoring-env-surface]: ./env-surface.md
[authoring-dependencies]: ./declare-dependencies.md
