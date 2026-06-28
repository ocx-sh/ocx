---
outline: deep
---
# Entry Points

Most installed packages end up driven through one or two commands — `cmake`, `ctest`, `node`, `go` — but OCX's clean-env execution model normally routes every invocation through [`ocx package exec <package> -- <command>`][cmd-exec]. That is the right default for reproducibility, but it adds friction when humans sit in front of a shell and want to type `cmake --build .` without remembering which package owns `cmake`.

Entry points close that gap. A publisher declares named launchers in [`metadata.json`][metadata-ref]; at install time OCX generates a small script per launcher that re-enters the package through the internal `ocx launcher exec` subcommand. Users add one directory to `$PATH` and every declared name becomes a top-level command, with the same clean-env guarantees as a direct `ocx package exec` call.

This page explains the inner workings of entry points — how launchers are generated, how they integrate with [env composition][env-composition], how collisions are detected, and how transitive dependencies contribute their launchers via the synth-PATH mechanism. For the field-level schema see the [metadata reference][metadata-entry-points]; for the command-line flags that wrap launchers at runtime see [`ocx package exec`][cmd-exec] and [`ocx package select`][cmd-select].

## Declaring Entry Points {#declaring}

Entry points live as a sibling object on the bundle metadata, keyed by the command name users will invoke:

```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "CMAKE_ROOT", "type": "constant", "value": "${installPath}/share/cmake" }
  ],
  "entrypoints": {
    "cmake": {},
    "ctest": {}
  }
}
```

`entrypoints` is a JSON object keyed by command name; the value object carries the optional `command` and `args` fields — omitting both yields `{}` as the value. The map shape mirrors Cargo's `[dependencies.X]`, Compose's `services:`, and GitHub Actions's `jobs:` — uniqueness within a package follows from JSON object key semantics. At runtime the launcher resolves the entry's name against the composed `PATH` from the package's [`env`][metadata-env] block, so the publisher declares the binary's location once via `env` and the launcher exec resolver picks it up from there. The complete field reference lives at [Entry Points in the metadata reference][metadata-entry-points].

At install time OCX writes the launchers for every declared name into a sibling [`entrypoints/` directory][fs-packages] inside the content-addressed package directory, regardless of which platform is currently installing. POSIX hosts get a `.sh` launcher (mode `0755`); Windows hosts get a native `<name>.exe` shim plus a one-line `<name>.shim` sidecar that carries the absolute package root. Generating every shape on every platform keeps publishers from having to fork metadata per host OS.

::: tip Declare the commands users will actually type
Only promote user-facing commands to entry points. Internal helpers, wrapper scripts, and build-time binaries belong in `${installPath}/bin` (discoverable through `ocx package exec` or explicit path lookup) rather than on `$PATH`. Every launcher consumes a global name across every package a user has selected — be deliberate about the ones you spend.
:::

## Interface-Only Surface {#interface-only}

Entry points are **always on the interface surface** — they represent the commands a package exposes to consumers. A package's launchers appear on `PATH` only under the consumer view (default `ocx package exec`, `--self` off).

Under `--self`, the root's own launchers are **suppressed** from its `PATH` to prevent a launcher from finding itself on `PATH` and recursing infinitely. The private surface is for a package's internal runtime env (compilers, runtimes, shared libraries), not its own CLI commands.

Transitive dependency launchers that pass the [interface edge filter][edge-filter] do appear in both views via the synth-PATH mechanism described below.

## Name Rules and Collisions {#collisions}

Names are validated at metadata deserialization time. The regex is `^[a-z0-9][a-z0-9_-]*$` — lowercase slug, digits, `-` and `_` allowed, must start with an alphanumeric — and the name must be at most 64 characters. Capital letters, dots, slashes, leading separators, and names exceeding the limit are rejected with a descriptive error so invalid metadata never reaches the registry.

Uniqueness is enforced at three layers:

- **Within a package** — the map shape gives intra-package uniqueness via JSON object key semantics, and duplicate keys in the on-wire JSON are rejected at deserialization (rather than silently last-wins, the `serde_json` default). A bundle that lists `"cmake"` twice fails to parse and never reaches the registry.
- **Across the interface surface (install time)** — when [`ocx package install`][cmd-install] generates launchers, it iterates the transitive closure in topological order and reports all owners when an entry-point name collides on the composed interface surface. Two packages with the same entrypoint name on the interface surface produce an install error. See [Multi-Owner Collision Reporting](#multi-owner) below for the exact error shape.
- **Across selected packages (select time)** — two different installed packages that both declare `cmake` coexist on disk, but only one can contribute `cmake` to `$PATH` at a time. Collisions are detected when the user runs [`ocx package select`][cmd-select] (or `ocx package install --select`) — the command reports the conflict and refuses to flip `current` until the user deselects the competing package.

A name that appears as an entrypoint in a `private`-edge dep is **not an install error**. Private-surface duplicates do not collide at install time — they resolve at runtime by topological PATH order. The dep whose `entrypoints/` directory appears first in the composed `PATH` wins the lookup. This is intentional: `private`-edge deps are invisible to consumers and their launchers never appear on the consumer's `PATH`. Launcher naming conflicts among private deps are the package's internal concern.

::: warning Choose names carefully for binaries that other tools also ship
Short, common names (`ls`, `gcc`, `tar`) create more cross-package collisions than they are worth. Where possible, mirror the upstream binary name exactly — most registries will have one canonical publisher per tool, and entry-point collisions at select time are a clear signal that two packages are competing for the same slot.
:::

## How Launchers Work at Runtime {#runtime}

On Unix a generated launcher is a deliberately small `sh` script. It is exactly:

```sh
#!/bin/sh
# Generated by ocx at install time. Do not edit.
exec "${OCX_BINARY_PIN:-ocx}" launcher exec '/home/alice/.ocx/packages/ocx.sh/sha256/ab/c123…' -- "$(basename "$0")" "$@"
```

On Windows the launcher is a native compiled `<name>.exe` shim accompanied by a `<name>.shim` sidecar. The sidecar is one UTF-8 line — the absolute package root, the same string the `.sh` body inlines:

```text
C:\Users\alice\.ocx\packages\ocx.sh\sha256\ab\c123…
```

The shim derives its own entry-point name from its filename (the file stem, minus the final `.exe`), reads the sibling `<stem>.shim` to learn the package root, and spawns `ocx launcher exec "<pkg-root>" -- "<stem>" <args…>` directly via the Win32 [`CreateProcessW`][createprocessw] API. It never routes through `cmd.exe`, so caller arguments are forwarded verbatim with no second command-line parse.

Only the absolute package-root path is baked (into the `.sh` body on Unix, into the `.shim` sidecar on Windows). The `ocx launcher exec` subcommand (hidden from `ocx --help`) forces the [self view][visibility-views] internally — private + public + interface entries all visible to the launched binary — so internal helpers (compilers, runtimes, shared libraries) are available without any flag baked into the launcher.

The launcher injects its own entry-point name — `$(basename "$0")` on Unix, the `.exe` file stem on Windows — as the first positional after `--`. `launcher exec` reads that argv0, composes the runtime env (including `PATH`), and uses the standard PATH search — `Env::resolve_command` — to locate the executable. The optional `command` field overrides the dispatch target when the binary name differs from the invocable name; when absent, the entry-point name is dispatched directly on the composed `PATH` from the package's [`env`][metadata-env] block.

Both flavors use `OCX_BINARY_PIN` when the variable is set, falling back to plain `ocx` on `$PATH` otherwise. The Unix `${OCX_BINARY_PIN:-ocx}` form treats an empty value as unset; the Windows shim follows `cmd`-style `IF DEFINED` semantics, so a variable that is present but empty still takes the pinned branch. The variable is propagated from the parent `ocx` process to all child processes via `env::Env::apply_ocx_config`, so a user-level OCX upgrade that sets `OCX_BINARY_PIN` takes effect for every launcher without touching the launchers on disk.

The stable wire ABI is the `launcher exec` subcommand name pair and the positional shape (`<pkg-root> -- <argv0> [args...]`). Both are frozen so launchers generated by older OCX releases keep working after the tool is upgraded.

::: details Why bake the package root instead of resolving current at runtime?
A launcher whose body expanded `current` on every invocation would drift the moment the user selected a different tag. Baking the resolved package root at install time means a candidate's launcher always points at the candidate's content, even while `current` floats. This matches the broader OCX model: [candidates][fs-symlinks] are immutable pins, `current` is mutable state. The entry-point name is read from the launcher's own filename so OCX never has to bake a second path that could drift out of sync with the first.
:::

::: warning Characters that fail launcher generation
The same character set is rejected for every launcher OCX writes, regardless of which platform is doing the install — every shape is generated on every host. The forbidden characters in the package-root path are: single quote (`'`), double quote (`"`), newline (`\n`), carriage return (`\r`), and NUL (`\0`). The set protects two written-to-disk artifacts: the single-quoted package-root literal in the Unix `.sh` body — `'` cannot be escaped inside a single-quoted `sh` string — and the one-line `.shim` sidecar the Windows shim reads, where `\n`, `\r`, and `\0` would corrupt the single-line contract. Double quote stays forbidden because it has to be quoted for both the `.sh` body and the native shim's `CreateProcessW` command line. Backslash and percent are both *allowed*: ordinary Windows paths (`C:\Users\…`) and folders containing `%` (e.g. `100%real`) must work. The Windows shim reads the sidecar verbatim and spawns via `CreateProcessW` with no `cmd.exe`, so neither `\` nor `%` carries special meaning. The check fires at launcher generation time so packages installed into a path containing any of these never produce a corrupt launcher — install fails loudly first.
:::

## Synth-PATH Mechanism {#synth-path}

Entry points from transitive interface-visible dependencies become available without each intermediate package manually re-exporting them. When the [composer][env-composition] encounters a dep whose effective visibility has the interface axis set ([`has_interface()`][edge-filter] is true), and that dep declares entrypoints, the composer synthesizes a `PATH ⊳ <dep-pkg-root>/entrypoints` entry.

The synth-PATH entry for a dep is emitted in the same position as that dep's other env contributions — in topological order before the root. The root's own synth-PATH entry (consumer surface only) is emitted last, giving the root's launchers PATH priority over all transitive deps' launchers.

This mechanism lets a top-level package depend on a runtime that ships its own commands (e.g., a CMake bundle that depends on a Ninja runtime) and have those commands surface automatically on the consumer's `$PATH` without redeclaring them.

## PATH Integration {#path}

Launchers are harmless files until something puts them on `$PATH`. OCX does that through the per-repo `current` symlink (which targets the package root) plus the `entrypoints/` subdirectory inside it:

- [`ocx package select <package>`][cmd-select] flips `current` to the selected package root. Tools that need launchers reach them via `{registry}/{repo}/current/entrypoints` — there is no separate symlink for entry points; the same anchor exposes content (`current/content`), launchers (`current/entrypoints`), and metadata (`current/metadata.json`).
- [`ocx package deselect <package>`][cmd-deselect] and [`ocx package uninstall <package>`][cmd-uninstall] remove `current` as part of their cleanup. Both operations are idempotent — an already-absent symlink is not an error.
- [`ocx package env --shell=bash <package>`][cmd-package-env] emits eval-safe export lines for the named package, including a `PATH ⊳ current/entrypoints` entry for every selected package that declares entry points. The global form `eval "$(ocx --global env --shell=bash)"` activates the full toolchain when packages are managed via `ocx.toml`.

The typical shell wire-up adds one of these to a login dotfile:

<<< @/_scripts/entry-points/path-integration.sh{sh}

From the user's perspective, once the shell is open, `cmake --build .` resolves to your launcher, which resolves to your binary, all inside a clean environment.

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

### Windows {#windows}

The Windows launcher is a native compiled `.exe` shim, not a script. When a shell resolves `cmake` it finds `cmake.exe` (`.EXE` is unconditionally in the default Windows `PATHEXT`, so no `PATHEXT` configuration is ever needed for launcher discovery). The shim reads its `cmake.shim` sidecar for the package root and spawns `ocx launcher exec` through [`CreateProcessW`][createprocessw], passing caller arguments straight through with Win32 [`CommandLineToArgvW`][cmdlinetoargv]-compatible quoting. Because nothing re-parses the command line through `cmd.exe`, arguments survive verbatim — spaces, Unicode, shell metacharacters (`&`, `|`, `^`, `<`, `>`), and literal empty strings (`cmake "" --version`) all reach the tool exactly as written. This closes the `BatBadBut` / [CVE-2024-24576][batbadbut] `%*` argument-injection class for default resolution; automation that interpolates untrusted strings (a CI branch name, a build label) into launcher arguments is not exposed through OCX's own launchers.

The shim is transparent: it propagates the child's exit code unchanged (full 32-bit passthrough), forwards stdin/stdout/stderr to the real console, and uses a Win32 job object so a force-killed launcher reaps its child tree rather than orphaning it. Errors before the child starts go to stderr as a single `ocx-shim:` line with a [sysexits][sysexits]-aligned exit code. A missing sidecar is recoverable: re-running [`ocx install`][cmd-install] or [`ocx select`][cmd-select] regenerates the entrypoint, and that hint is printed on the stderr line itself.

When `ocx` cannot be resolved, the message depends on how resolution was attempted. With no [`OCX_BINARY_PIN`][env-ocx-binary-pin] set and `ocx` not on `$PATH`, the shim prints `ocx-shim: ocx not found (set OCX_BINARY_PIN or add ocx to PATH)`. When `OCX_BINARY_PIN` *is* set but points at a path that does not exist, the shim names that path instead — `ocx-shim: pinned ocx not found: C:\path\to\ocx.exe (OCX_BINARY_PIN points at a missing path)` — because the generic "add ocx to PATH" hint would be misleading there.

The shim's pre-child exit codes are stable and scriptable:

| Code | Meaning | Operator action |
|------|---------|-----------------|
| `69` | `ocx` unavailable (not pinned and not on `$PATH`, or the pinned path is missing) | Put `ocx` on `$PATH` or fix [`OCX_BINARY_PIN`][env-ocx-binary-pin] before any launcher runs |
| `74` | `CreateProcessW` failed — the Win32 error code is on the stderr line | Inspect the reported Win32 code; usually a corrupt binary or a transient OS condition — re-run, then re-`ocx install` |
| `77` | Permission denied (`ERROR_ACCESS_DENIED`) starting the child | Check execution policy / AppLocker / antivirus blocking the launcher; grant execute on the package root |
| `78` | Bad or missing `.shim` sidecar (absent, empty, oversized, not absolute, or corrupt) | Re-run [`ocx install`][cmd-install] or [`ocx select`][cmd-select] to regenerate the entrypoint |

The child's own exit code passes through untouched on the success path, so a non-zero value above is unambiguously a launcher-stage failure, not the tool's.

### PowerShell {#powershell}

PowerShell resolves `cmake` to `cmake.exe` on `$PATH` and runs it directly — there is no intervening `cmd.exe`, so PowerShell's own `%VAR%`-in-double-quotes expansion quirk does not apply to OCX launchers. OCX does not generate a `.ps1` variant: the native `.exe` shim already covers the entire PowerShell user base with no script-language argument-forwarding caveats.

The practical rule is: PowerShell users get this for free from the installer, which writes `$OCX_HOME/env.ps1` and adds a block to `$PROFILE` that sources it. On every session `env.ps1` runs `Invoke-Expression ((& ocx --global env --shell=pwsh) | Out-String)`, and the resulting `$PATH` entries pick up the `.exe` launchers automatically.

### Git Bash and MSYS2 {#git-bash}

On Windows, POSIX-emulation shells (Git Bash, MSYS2, Cygwin) resolve `cmake.exe` directly by its explicit extension — these shells do not consult `PATHEXT`, so the native `.exe` is exactly what they need with no extra `.sh` launcher. The same shim runs identically from `cmd.exe`, PowerShell, and a Git Bash prompt because none of them route the call through `cmd.exe`.

::: warning `ocx` must be resolvable
Every launcher delegates to `ocx launcher exec`. If the shim cannot find `ocx` it exits with code `69` and one of two messages: with no [`OCX_BINARY_PIN`][env-ocx-binary-pin] set and `ocx` off `$PATH`, `ocx-shim: ocx not found (set OCX_BINARY_PIN or add ocx to PATH)`; with `OCX_BINARY_PIN` set to a path that does not exist, `ocx-shim: pinned ocx not found: <path> (OCX_BINARY_PIN points at a missing path)`. Installation flows that put OCX itself on `$PATH` (for example, an `ocx` package installed with `--select`, or a system-level install) must be in place *before* any launcher is invoked. The [installation guide][install] covers the supported bootstrap paths.
:::

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
  "entrypoints": {
    "cmake": {},
    "ctest": {}
  }
}
```

And on the consumer side:

<<< @/_scripts/entry-points/example-install.sh{sh}

Switching to a different version is a single `ocx package select`:

<<< @/_scripts/entry-points/example-select.sh{sh}

No re-sourcing dotfiles. The `ocx package env --shell` output is stable — it emits exports for whichever packages are currently selected. The stable `current/entrypoints` path was already on `$PATH`; the select just re-points the `current` symlink at the new package root, and the next shell open picks up the updated env output automatically.

<!-- in-depth -->
[env-composition]: ./environments.md
[edge-filter]: ./environments.md#edge-filter
[visibility-views]: ./environments.md#visibility-views

<!-- external -->
[sdkman]: https://sdkman.io/
[asdf]: https://asdf-vm.com/
[createprocessw]: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw
[cmdlinetoargv]: https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-commandlinetoargvw
[batbadbut]: https://github.com/rust-lang/rust/security/advisories/GHSA-q455-m56c-85mh
[sysexits]: https://man.freebsd.org/cgi/man.cgi?sysexits

<!-- commands -->
[cmd-exec]: ../reference/command-line.md#package-exec
[cmd-install]: ../reference/command-line.md#package-install
[cmd-select]: ../reference/command-line.md#package-select
[cmd-deselect]: ../reference/command-line.md#package-deselect
[cmd-uninstall]: ../reference/command-line.md#package-uninstall
[cmd-package-env]: ../reference/command-line.md#package-env

<!-- environment -->
[env-ocx-home]: ../reference/environment.md#ocx-home
[env-ocx-binary-pin]: ../reference/environment.md#ocx-binary-pin

<!-- reference pages -->
[metadata-ref]: ../reference/metadata.md
[metadata-entry-points]: ../reference/metadata.md#entry-points
[metadata-env]: ../reference/metadata.md#env

<!-- user guide -->
[install]: ../installation.md
[fs-packages]: ../user-guide.md#file-structure-packages
[fs-symlinks]: ../user-guide.md#file-structure-symlinks
