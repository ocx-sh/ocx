---
outline: deep
---
# Entry Points

Most installed packages end up driven through one or two commands — `cmake`, `ctest`, `node`, `go` — but OCX's clean-env execution model normally routes every invocation through [`ocx exec <package> -- <command>`][cmd-exec]. That is the right default for reproducibility, but it adds friction when humans sit in front of a shell and want to type `cmake --build .` without remembering which package owns `cmake`.

Entry points close that gap. A publisher declares named launchers in [`metadata.json`][metadata-ref]; at install time OCX generates a small script per launcher that re-enters the package through the internal `ocx launcher exec` subcommand. Users add one directory to `$PATH` and every declared name becomes a top-level command, with the same clean-env guarantees as a direct `ocx exec` call.

This page explains the inner workings of entry points — how launchers are generated, how they integrate with [env composition][env-composition], how collisions are detected, and how transitive dependencies contribute their launchers via the synth-PATH mechanism. For the field-level schema see the [metadata reference][metadata-entry-points]; for the command-line flags that wrap launchers at runtime see [`ocx exec`][cmd-exec] and [`ocx shell profile load`][cmd-shell-profile].

## Declaring Entry Points {#declaring}

Entry points live as a sibling array on the bundle metadata:

```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "CMAKE_ROOT", "type": "constant", "value": "${installPath}/share/cmake" }
  ],
  "entrypoints": [
    { "name": "cmake", "target": "${installPath}/bin/cmake" },
    { "name": "ctest", "target": "${installPath}/bin/ctest" }
  ]
}
```

Every entry has exactly two fields — `name` (the launcher filename and the command users type) and `target` (a template string pointing at the executable to run). The complete field reference, including optional schema additions, lives at [Entry Points in the metadata reference][metadata-entry-points].

At install time OCX writes one script per entry into a sibling [`entrypoints/` directory][fs-packages] inside the content-addressed package directory — a POSIX `.sh` launcher (mode `0755`) and a Windows `.cmd` launcher for every declared name, regardless of which platform is currently installing. Generating both shapes on every platform keeps publishers from having to fork metadata per host OS.

::: tip Declare the commands users will actually type
Only promote user-facing commands to entry points. Internal helpers, wrapper scripts, and build-time binaries belong in `${installPath}/bin` (discoverable through `ocx exec` or explicit path lookup) rather than on `$PATH`. Every launcher consumes a global name across every package a user has selected — be deliberate about the ones you spend.
:::

## Interface-Only Surface {#interface-only}

Entry points are **always on the interface surface** — they represent the commands a package exposes to consumers. A package's launchers appear on `PATH` only under the consumer view (default `ocx exec`, `--self` off).

Under `--self`, the root's own launchers are **suppressed** from its `PATH` to prevent a launcher from finding itself on `PATH` and recursing infinitely. The private surface is for a package's internal runtime env (compilers, runtimes, shared libraries), not its own CLI commands.

Transitive dependency launchers that pass the [interface edge filter][edge-filter] do appear in both views via the synth-PATH mechanism described below.

## Name Rules and Collisions {#collisions}

Names are validated at metadata deserialization time. The regex is `^[a-z0-9][a-z0-9_-]*$` — lowercase slug, digits, `-` and `_` allowed, must start with an alphanumeric — and the name must be at most 64 characters. Capital letters, dots, slashes, leading separators, and names exceeding the limit are rejected with a descriptive error so invalid metadata never reaches the registry.

Uniqueness is enforced at three layers:

- **Within a package** — duplicate `name` values in the same `entrypoints` array are rejected at deserialization (so the bundle never publishes in a broken state). A package shipping both a `cmake` and a second `cmake` entry will fail to parse.
- **Across the interface surface (install time)** — when [`ocx install`][cmd-install] generates launchers, it iterates the transitive closure in topological order and reports all owners when an entry-point name collides on the composed interface surface. Two packages with the same entrypoint name on the interface surface produce an install error. See [Multi-Owner Collision Reporting](#multi-owner) below for the exact error shape.
- **Across selected packages (select time)** — two different installed packages that both declare `cmake` coexist on disk, but only one can contribute `cmake` to `$PATH` at a time. Collisions are detected when the user runs [`ocx select`][cmd-select] (or `ocx install --select`) — the command reports the conflict and refuses to flip `current` until the user deselects the competing package.

A name that appears as an entrypoint in a `private`-edge dep is **not an install error**. Private-surface duplicates do not collide at install time — they resolve at runtime by topological PATH order. The dep whose `entrypoints/` directory appears first in the composed `PATH` wins the lookup. This is intentional: `private`-edge deps are invisible to consumers and their launchers never appear on the consumer's `PATH`. Launcher naming conflicts among private deps are the package's internal concern.

::: warning Choose names carefully for binaries that other tools also ship
Short, common names (`ls`, `gcc`, `tar`) create more cross-package collisions than they are worth. Where possible, mirror the upstream binary name exactly — most registries will have one canonical publisher per tool, and entry-point collisions at select time are a clear signal that two packages are competing for the same slot.
:::

## Template Substitution {#template}

The `target` field understands the same placeholders as [environment variable values][metadata-env]:

- `${installPath}` — the absolute path to *this* package's content directory at install time. Baked once into the generated launcher; future selects do not rewrite it.
- `${deps.NAME.installPath}` — the content directory of a declared dependency, where `NAME` is the repository basename (or the dependency's `name` field when one is declared). Expands against the exact digest resolved at install time, so launchers keep pointing at the build the package was originally installed against even after the dependency's tag advances.

Tokens that don't match the `${installPath}` or `${deps.NAME.FIELD}` shapes — anything missing the leading `$`, missing braces, or that is plainly not a placeholder (`{whatever}`, `$installPath`, `${{nested}}`) — pass through as literals on disk. Anything that *does* parse as `${...}` but isn't recognized is rejected at publish time: a `${deps.NAME.FIELD}` token referencing a dependency the package does not declare is rejected, an unsupported field on a declared dependency is rejected, and any other well-formed `${...}` token in an entry-point `target` is rejected as unrecognized. Validation runs at publish, again at install, and a final time when launcher scripts are generated, so invalid metadata never reaches the launcher.

Template expansion happens once at install time. The resolved path is baked into the launcher body, so running the script is a single `exec` call with no template engine in the hot path.

## How Launchers Work at Runtime {#runtime}

A generated launcher is deliberately small. On Unix it is exactly:

```sh
#!/bin/sh
# Generated by ocx at install time. Do not edit.
exec "${OCX_BINARY_PIN:-ocx}" launcher exec '/home/alice/.ocx/packages/ocx.sh/sha256/ab/c123…' -- "$(basename "$0")" "$@"
```

On Windows the equivalent `.cmd` file is:

```bat
@ECHO off
SETLOCAL DisableDelayedExpansion
IF DEFINED OCX_BINARY_PIN (
    "%OCX_BINARY_PIN%" launcher exec "C:\Users\alice\.ocx\packages\ocx.sh\sha256\ab\c123…" -- "%~n0" %*
) ELSE (
    ocx launcher exec "C:\Users\alice\.ocx\packages\ocx.sh\sha256\ab\c123…" -- "%~n0" %*
)
```

Only the absolute package-root path is baked. The `ocx launcher exec` subcommand (hidden from `ocx --help`) forces the [self view][visibility-views] internally — private + public + interface entries all visible to the launched binary — so internal helpers (compilers, runtimes, shared libraries) are available without any flag baked into the script.

The launcher injects its own filename — `$(basename "$0")` on Unix, `%~n0` on Windows — as the first positional after `--`, so `launcher exec` learns which entry point was invoked from the script's filename rather than from a hard-coded `target`. That keeps `target` a publish-time existence assertion, not a runtime dispatch key.

Both flavors use `OCX_BINARY_PIN` when the variable is set, falling back to plain `ocx` on `$PATH` otherwise. The variable is propagated from the parent `ocx` process to all child processes via `env::Env::apply_ocx_config`, so a user-level OCX upgrade that sets `OCX_BINARY_PIN` takes effect for every launcher without touching the launcher bodies on disk.

The stable wire ABI is the `launcher exec` subcommand name pair and the positional shape (`<pkg-root> -- <argv0> [args...]`). Both are frozen so launchers generated by older OCX releases keep working after the tool is upgraded.

::: details Why bake the package root instead of resolving current at runtime?
A launcher whose body expanded `current` on every invocation would drift the moment the user selected a different tag. Baking the resolved package root at install time means a candidate's launcher always points at the candidate's content, even while `current` floats. This matches the broader OCX model: [candidates][fs-symlinks] are immutable pins, `current` is mutable state. The entry-point name is read from the launcher's own filename so OCX never has to bake a second path that could drift out of sync with the first.
:::

::: warning Characters that fail launcher generation
The same character set is rejected for every launcher OCX writes, regardless of which platform is doing the install — both `.sh` and `.cmd` shapes are generated on every host. The forbidden characters in the package-root path or in any resolved `target` are: single quote (`'`), double quote (`"`), percent (`%`), newline (`\n`), carriage return (`\r`), and NUL (`\0`). The double quote is rejected even on Unix because Windows `cmd.exe` shares the rule. Validation runs at publish, again at install, and a final time when launcher scripts are generated, so packages whose paths contain any of these never produce a corrupt launcher — they fail loudly first.
:::

## Synth-PATH Mechanism {#synth-path}

Entry points from transitive interface-visible dependencies become available without each intermediate package manually re-exporting them. When the [composer][env-composition] encounters a dep whose effective visibility has the interface axis set ([`has_interface()`][edge-filter] is true), and that dep declares entrypoints, the composer synthesizes a `PATH ⊳ <dep-pkg-root>/entrypoints` entry.

The synth-PATH entry for a dep is emitted in the same position as that dep's other env contributions — in topological order before the root. The root's own synth-PATH entry (consumer surface only) is emitted last, giving the root's launchers PATH priority over all transitive deps' launchers.

This mechanism lets a top-level package depend on a runtime that ships its own commands (e.g., a CMake bundle that depends on a Ninja runtime) and have those commands surface automatically on the consumer's `$PATH` without redeclaring them.

## PATH Integration {#path}

Launchers are harmless files until something puts them on `$PATH`. OCX does that through the per-repo `current` symlink (which targets the package root) plus the `entrypoints/` subdirectory inside it:

- [`ocx select <package>`][cmd-select] flips `current` to the selected package root. Tools that need launchers reach them via `{registry}/{repo}/current/entrypoints` — there is no separate symlink for entry points; the same anchor exposes content (`current/content`), launchers (`current/entrypoints`), and metadata (`current/metadata.json`).
- [`ocx deselect <package>`][cmd-deselect] and [`ocx uninstall <package>`][cmd-uninstall] remove `current` as part of their cleanup. Both operations are idempotent — an already-absent symlink is not an error.
- [`ocx shell profile load`][cmd-shell-profile] emits a shell-specific `PATH` export for every profile entry whose `current` symlink exists *and* whose metadata has a non-empty `entrypoints` array; the exported path is `{registry}/{repo}/current/entrypoints`. Packages that haven't been `ocx select`ed yet (candidate-mode profile entries) are silently skipped, so profile loading never points `$PATH` at a missing directory.

The typical shell wire-up is a single line in a dotfile:

```sh
# ~/.bashrc — evaluated on every shell start
eval "$(ocx --offline shell profile load)"
```

The `--offline` flag is essential here: profile load runs on every shell startup and must never touch the network. From the user's perspective, once the shell is open, `cmake --build .` resolves to your launcher, which resolves to your binary, all inside a clean environment.

::: info Similar to SDKMAN and asdf shims
[SDKMAN][sdkman] generates shim scripts at `~/.sdkman/candidates/{tool}/current/bin/` and relies on a fixed PATH entry per tool; [asdf][asdf] takes the same approach with a single `~/.asdf/shims/` directory. OCX uses the same idea: one PATH entry per package, anchored on the per-repo `current` symlink, with launchers living under `current/entrypoints`.
:::

## Multi-Owner Collision Reporting {#multi-owner}

When two or more packages in the interface surface declare the same entrypoint name, the install error reports all of them. The collision surfaces on stderr as a single line in the standard `thiserror` `Display` form:

```text
error: entrypoint name collision: 'cmake' declared by 3 packages: ocx.sh/cmake-extras:1.0@sha256:aaa..., ocx.sh/cmake-wrapper:1.0@sha256:bbb..., ocx.sh/cmake:3.28@sha256:ccc...; deselect one before selecting another
```

The line has three parts:

- `'cmake'` — the colliding entry-point name.
- `N packages: …` — every interface-visible owner, comma-separated, each printed as a fully-qualified pinned identifier (`registry/repository:tag@sha256:…`). All N owners are listed so the publisher knows exactly which packages in the dependency graph compete for the name.
- `deselect one before selecting another` — suggested action.

The error is not a structured JSON payload. The collision variant is reported via the same Rust error chain as every other failure (`PackageErrorKind::EntrypointCollision`); the process exits with code `65` (`DataError`).

## Cross-Platform Caveats {#cross-platform}

Launchers are the first OCX artifact whose correctness depends on the user's shell as well as their OS. A few platform-specific quirks are worth calling out.

### Unix shells {#unix}

The Unix launcher is a POSIX `sh` script with the package-root path inlined as a single-quoted literal. The unsafe-character set rejected at generation time (described in the runtime section above) keeps that literal trivially safe — there is no safe way to escape single quotes inside single-quoted strings in `sh`, so OCX refuses to ship a launcher whose package-root path contains one. In practice this only bites packages whose [`OCX_HOME`][env-ocx-home] contains a literal apostrophe, which is vanishingly rare. The launcher is chmod-ed to `0755` as part of the write, so the install is complete with no follow-up `chmod` step.

### Windows (`cmd.exe`) {#windows}

The Windows launcher is a `cmd.exe` batch file. The package-root path is double-quoted on the `ocx launcher exec` line, so a literal `%` in the path would terminate the variable expansion early and change the semantics. The same unified rejection list described above forbids `%` in the package-root path; the rule fires at publish, install, and launcher-generation time so the batch file always parses cleanly.

`cmd.exe`'s `%*` argument expansion silently drops empty arguments. A caller who invokes `cmake "" --version` will see the launcher forward `cmake --version` to `ocx exec`. This is a native `cmd.exe` limitation rather than an OCX design choice; tools that genuinely need empty positional arguments on Windows should call [`ocx exec`][cmd-exec] directly instead of going through a launcher.

::: warning Residual argument-injection risk on Windows
OCX launchers use `SETLOCAL DisableDelayedExpansion`, which closes the registry-level `!VAR!` expansion vector (the narrower `BatBadBut`-class path that requires a prior registry write). However, the `%*` parameter that forwards caller arguments is still re-parsed by `cmd.exe`. Arguments containing metacharacters (`&`, `|`, `^`, `<`, `>`, `(`, `)`) outside double-quoted regions can be interpreted as shell commands. If your automation passes user-controlled strings — for example a CI pipeline interpolating a branch name or a build label — as arguments to an OCX launcher without quoting, those strings are exploitable. Shell-quote all arguments before passing them to OCX launchers. See [`.claude/artifacts/adr_windows_cmd_argv_injection.md`][adr-argv] for the full threat model; a compiled `.exe` shim that bypasses `cmd.exe` entirely is tracked as the definitive follow-up.
:::

### PowerShell {#powershell}

PowerShell invokes `.cmd` files natively — `cmake --version` in a PowerShell prompt resolves to `cmake.cmd` on `$PATH`, runs it under `cmd.exe`, and returns the combined exit code. OCX does **not** generate a `.ps1` variant: a native PowerShell script ran into argument-forwarding quirks around `--` and quoted empty strings during prototyping, and the `.cmd` path avoids every known issue while covering the entire PowerShell user base.

The practical rule is: PowerShell users should install into PowerShell's `$PROFILE` exactly the same `ocx --offline shell profile load` snippet as a `cmd.exe` user. The resulting `$PATH` entries pick up the `.cmd` launchers automatically.

::: warning PowerShell `%`-style variable references in arguments
When you call a `.cmd` launcher from a PowerShell prompt, PowerShell expands `%SystemRoot%`-style references inside double-quoted argument strings **before** the argument reaches `cmd.exe`. For example, `cmake "--install=%SystemRoot%\tools"` will have `%SystemRoot%` replaced by PowerShell with the value of `$env:SystemRoot` before the launcher ever runs. Use the PowerShell `--%-` stop-parsing operator to pass arguments verbatim to the cmd.exe layer:

```powershell
cmake --% --install=%SystemRoot%\tools
```

Everything after `--%-` is forwarded to the underlying process without PowerShell variable or expression expansion. This applies to any argument containing `%VAR%` sequences you want `cmd.exe` (or the tool itself) to expand rather than PowerShell.
:::

### Git Bash and MSYS2 {#git-bash}

On Windows, POSIX-emulation shells (Git Bash, MSYS2, Cygwin) can invoke `.cmd` files directly using the native Windows path baked into the launcher body. Users running these shells do not need an extra `.sh` launcher — the `.cmd` script works from both `cmd.exe` and Git Bash prompts, because both ultimately route the call through `cmd.exe`.

::: warning `ocx` must be on `$PATH`
Every launcher delegates to `ocx launcher exec`. If the user's shell cannot find `ocx` (and `OCX_BINARY_PIN` is not set), the launcher cannot resolve the underlying binary. Installation flows that put OCX itself on `$PATH` (for example, an `ocx` package installed with `--select`, or a system-level install) must be in place *before* any launcher is invoked. The [installation guide][install] covers the supported bootstrap paths.
:::

## Known Limitations {#known-limitations}

### `cmd.exe` drops empty positional arguments {#cmd-empty-args}

`cmd.exe`'s `%*` parameter expansion silently discards empty positional arguments. A caller who passes `cmake "" --verbose` will have the launcher forward `cmake --verbose` to `ocx exec` — the empty string disappears. This is a `cmd.exe` limitation, not an OCX design choice.

Callers that must pass literal empty strings as positional arguments on Windows should bypass the launcher and call [`ocx exec`][cmd-exec] directly:

```bat
ocx exec cmake:3.28 -- cmake "" --verbose
```

Alternatively, pass the empty argument in a PowerShell session using explicit quoting with the `--%-` stop-parsing operator, which lets you control exactly what `cmd.exe` sees:

```powershell
cmake --% "" --verbose
```

Neither workaround applies on Unix: the POSIX `sh` launcher uses `"$@"` which preserves empty arguments verbatim.

## End-to-End Example {#example}

Publishing a CMake package with two launchers looks like this on the publisher side:

```json
{
  "type": "bundle",
  "version": 1,
  "strip_components": 1,
  "env": [
    { "key": "PATH", "type": "path", "required": true, "value": "${installPath}/bin" }
  ],
  "entrypoints": [
    { "name": "cmake", "target": "${installPath}/bin/cmake" },
    { "name": "ctest", "target": "${installPath}/bin/ctest" }
  ]
}
```

And on the consumer side:

```shell
ocx install --select cmake:3.28
eval "$(ocx --offline shell profile load)"   # already in ~/.bashrc in practice

# Both are now top-level commands, running under ocx exec's clean environment.
cmake --version
ctest --output-on-failure
```

Switching to a different version is a single `ocx select`:

```shell
ocx install cmake:3.30
ocx select cmake:3.30     # current flips; next shell command uses 3.30
```

No shell profile rewrite, no re-sourcing dotfiles. The stable `current/entrypoints` path was already on `$PATH`; the select just re-points the `current` symlink at the new package root.

<!-- security -->
[adr-argv]: https://github.com/ocx-sh/ocx/blob/main/.claude/artifacts/adr_windows_cmd_argv_injection.md

<!-- in-depth -->
[env-composition]: ./environments.md
[edge-filter]: ./environments.md#edge-filter
[visibility-views]: ./environments.md#visibility-views

<!-- external -->
[sdkman]: https://sdkman.io/
[asdf]: https://asdf-vm.com/

<!-- commands -->
[cmd-exec]: ../reference/command-line.md#exec
[cmd-install]: ../reference/command-line.md#install
[cmd-select]: ../reference/command-line.md#select
[cmd-deselect]: ../reference/command-line.md#deselect
[cmd-uninstall]: ../reference/command-line.md#uninstall
[cmd-shell-profile]: ../reference/command-line.md#shell-profile-load

<!-- environment -->
[env-ocx-home]: ../reference/environment.md#ocx-home

<!-- reference pages -->
[metadata-ref]: ../reference/metadata.md
[metadata-entry-points]: ../reference/metadata.md#entry-points
[metadata-env]: ../reference/metadata.md#env

<!-- user guide -->
[install]: ../installation.md
[fs-packages]: ../user-guide.md#file-structure-packages
[fs-symlinks]: ../user-guide.md#file-structure-symlinks
