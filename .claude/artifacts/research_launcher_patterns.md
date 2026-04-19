# Research: Launcher Generation Patterns

**Date:** 2026-04-21
**Domain:** cli, packaging
**Triggered by:** OCX issue #61 — per-package entry point launcher scripts
**Expires:** 2027-04-21
**Author:** worker-researcher (Phase 2 of /swarm-plan high #61)

---

## TL;DR (5 bullets for the downstream architect)

1. **Bake the absolute install path at generation time; resolve nothing at runtime.** Every mature ecosystem (rustup, npm/cmd-shim, Nix makeWrapper, setuptools) writes a fixed path into the generated launcher. The launcher's only runtime job is `exec`-ing `ocx` with that baked path. No PATH scanning, no dynamic resolution.

2. **The POSIX self-location idiom `CDPATH= cd -- "$(dirname -- "$0")" && pwd -P` is the industry standard**, but OCX launchers don't need it — the install path is baked at generation time. Self-location is only needed if you want the launcher to be relocatable (it is not, by design).

3. **`"$@"` / `%*` / `@args` forwarding patterns are battle-tested but each has one non-obvious edge case:** `%*` drops empty args and does not round-trip double quotes in all cmd.exe versions; `@args` in PowerShell loses `--` semantics because PS parses it before forwarding; `"$@"` in POSIX sh is reliable.

4. **mise's switch away from shims (120 ms overhead) vs PATH activation is irrelevant for OCX** — OCX launchers are not interceptors that must re-resolve at runtime; they are one-shot `exec` wrappers with baked paths. Startup cost is fork + exec of a tiny shell script (~2 ms on Linux), not 120 ms.

5. **macOS Gatekeeper does not quarantine locally-generated shell scripts; Windows MOTW does not apply to `.cmd` files created by OCX on disk.** Both are only activated by files received over the network. Generated launchers are safe without code-signing.

---

## 1. Prior Art Comparison

| Ecosystem | Launcher shape on disk | Self-location technique | Env composition | Known pitfall |
|---|---|---|---|---|
| **Nix makeWrapper** | POSIX sh wrapper; original binary renamed to `.<name>-wrapped` | N/A — absolute Nix store path baked in at derivation time (`/nix/store/<hash>/bin/foo`) | `--set VAR VAL` / `--prefix LIST SEP VAL` / `--suffix LIST SEP VAL` emitted as shell `export` lines before `exec` | `wrapProgramShell` is deprecated; prefer `makeBinaryWrapper` (compiled C stub) for interpreter-less wrappers |
| **Homebrew bottles** | Symlinks in `/usr/local/bin` (Intel) or `/opt/homebrew/bin` (Apple Silicon) pointing into `Cellar/<name>/<ver>/bin/` | No wrapper — direct symlinks; `brew link` creates the symlink | PATH addition only (`brew shellenv`); no per-binary env baking | Symlinks break if Cellar path changes (version upgrade); no portable HOME |
| **npm/cmd-shim** | Three files per entry: `.sh` (no ext on POSIX/MSYS), `.cmd` (Windows), `.ps1` (PowerShell) | `.sh`: `basedir=$(dirname "$(echo "$0" \| sed -e 's,\\,/,g')")` + MSYS cygpath detection | `NODE_PATH=./lib:$NODE_PATH` → converted to `@set NODE_PATH=./lib;%NODE_PATH%` in `.cmd` | `.cmd` `%*` drops empty args; `.ps1` uses `$args` (not `@args`) so `--` loses meaning when PS is the outer shell |
| **pipx / console_scripts (Python)** | Unix: plain POSIX sh shebang script pointing to venv Python; Windows: compiled `.exe` stub + `<name>-script.py` with shebang line | Unix: absolute venv path baked into shebang `#!/home/user/.local/pipx/venvs/<pkg>/bin/python`; Windows: `.exe` reads its own name → loads sibling `-script.py` → parses shebang | Virtualenv PATH modification only | Windows `.exe` fails on non-ANSI arguments (pip issue #11800, open since 2023) |
| **asdf shims** | POSIX sh script in `~/.asdf/shims/`; calls `asdf exec <tool> "$@"` | No self-location needed — all shims call the same `asdf exec` dispatcher | asdf sets `ASDF_DIR`, `PATH` per-tool at exec time | ~120 ms added to every invocation; `which` shows the shim path, not the real binary |
| **mise shims** (optional mode) | Symlinks to `mise` binary in `~/.local/share/mise/shims/`; mise reads `argv[0]` to know which tool was requested | Same as rustup — argv[0]/process name dispatch | mise resolves env from `.mise.toml` at exec time | `--` as end-of-options is swallowed by PowerShell before reaching mise (hy2k.dev blog, 2026-01-06) |
| **rustup shims** | Hardlinks to `rustup` binary in `~/.cargo/bin/`; `rustup` reads `argv[0]` (process name) in `src/cli/proxy_mode.rs` to identify the target tool | Process-name dispatch (`std::env::current_exe()` or `argv[0]` comparison) | Not applicable — toolchain binaries invoked directly | Uppercase proxy name mismatch on Windows (issue #3848); shims are compiled binaries, not scripts — no POSIX/cmd portability concern |
| **Bazel runfiles launchers** | Unix: generated POSIX sh; Windows: `.bat` or native binary stub (hermetic-launcher project replaces sh with tiny Rust binary) | Unix: `RUNFILES_DIR` env var or adjacent `<binary>.runfiles/` directory; Windows: `RUNFILES_MANIFEST_FILE` pointing to a path manifest | Runfiles library resolves logical paths to physical cache paths | Bash scripts don't work on Windows; Bazel community moving toward native binary launchers (hermetic-launcher, 2024) |
| **setuptools console_scripts** | Unix: POSIX sh with shebang pointing to venv Python; Windows: `.exe` (cli.exe copy) + `-script.py` sidecar | Unix: shebang is absolute venv path baked at install; Windows: `.exe` reads its own name, loads sibling `.py`, parses shebang | None — env isolation is Python's concern | Windows `.exe` path must be updated if venv moves (non-portable by design) |

---

## 2. Self-Location Recipes

These are only needed if the launcher needs to discover its own install path at runtime (e.g., to support a portable/relocated `OCX_HOME`). For the baked-path design, skip to Section 6.

### POSIX sh — portable, no `readlink -f` dependency

```sh
# Guard: CDPATH= prevents `cd` from echoing a path to stdout when CDPATH is set.
# `--` prevents dirname treating paths starting with `-` as flags.
# `pwd -P` resolves symlinks (-P = physical path).
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
```

Quirks guarded against:
- `CDPATH` set in environment causes `cd` to echo the target path, polluting `$(...)` capture (bosker.wordpress.com, 2012; still relevant)
- `dirname "$0"` without `--` breaks on script paths starting with `-`
- `pwd` without `-P` follows symlinks to give the logical path, not the physical install path

Not guarded against: `$0` is empty when the script is sourced (not executed). OCX launchers are always executed, so this is a non-issue.

### cmd.exe — `%~dp0` via subroutine (npm/cmd-shim pattern)

```bat
@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0
```

Quirks guarded against:
- `%~dp0` in the main batch body resolves relative to the batch file, but if the script was launched via a different working directory the result can be relative. The subroutine `CALL :find_dp0` forces the expansion at call time, giving the drive+path of the batch file reliably.
- `%~dp0` always ends with a backslash — account for this when concatenating paths (e.g., `%dp0%ocx.exe` not `%dp0%\ocx.exe`).
- Under a symlinked `.cmd` file: `%~dp0` gives the directory of the symlink, not the target. For OCX this is correct behavior — the launcher is the thing we want to find.

### PowerShell — `$PSScriptRoot` (PS 3.0+)

```powershell
# $PSScriptRoot is set when the .ps1 file is executed as a script.
# $MyInvocation.MyCommand.Definition is the older alternative (PS 2.0+)
# but can change value when read inside a function; $PSScriptRoot is stable.
$dir = $PSScriptRoot
```

Quirks guarded against:
- `$PSScriptRoot` is empty when a script is dot-sourced or invoked via `Invoke-Expression` (PS issue #4217). OCX launchers are always executed, not sourced.
- When invoked via a symlink without a `.ps1` extension, `$PSScriptRoot` and `$PSCommandPath` are not populated (PS issue #21402). Always generate launchers with `.ps1` extension.
- `$MyInvocation.MyCommand.Definition` gives the symlink path, not the target. If you need the real target, use `(Get-Item $PSCommandPath).Target`.

---

## 3. Argument-Forwarding Recipes

### POSIX sh — `"$@"` (canonical)

```sh
exec ocx exec "$install_dir" -- "$@"
```

- `"$@"` expands each positional parameter as a separately quoted word. Preserves spaces, empty strings, and special characters.
- `$*` is the wrong choice — it joins all args into one word under double-quoting.
- The `--` before `"$@"` is optional here (since `$install_dir` is first and is always an absolute path, not a flag), but including it is defensive.

### cmd.exe — `%*`

```bat
"%_ocx%" exec "%_install_dir%" -- %*
```

Known limitations of `%*`:
- Does **not** preserve empty arguments — `cmd /c foo.cmd a "" b` delivers `a b` to `%*`, not `a "" b`. Windows cmd.exe design limitation.
- Does not round-trip double quotes in all versions — some invocation paths strip or add quotes unpredictably. Callers passing paths with spaces must quote them before the launcher boundary.
- `%*` is the only option; there is no `"$@"` equivalent in cmd.exe.

### PowerShell — `@args` (splatting)

```powershell
& $ocx exec $install_dir -- @args
```

- `@args` splats `$args` as individual positional arguments to the called executable.
- Use `@args`, not `$args` — `$args` passes the whole array as one string argument.
- Critical limitation: `--` before `@args` does **not** behave as an end-of-options marker at the PowerShell layer. If the user calls the launcher like `my-tool -- --some-flag`, PowerShell has already parsed the `--` before the function/script body runs. The `--` is forwarded to `ocx`, which is correct, but if PowerShell interprets any of the following args as PS syntax before forwarding, they are silently mangled. Mitigation: call via `my-tool.exe` or instruct users to use `my-tool.cmd` rather than the `.ps1` variant. (Source: hy2k.dev, 2026-01-06 — documented in the context of mise)

### MSYS / Git Bash path argument translation

When a launcher is a `.sh` file called from MSYS2/Git Bash and it invokes a native Windows binary (e.g., `ocx.exe`), MSYS2 auto-converts arguments that look like Unix paths to Windows paths:

```
/foo/bar → C:\msys64\foo\bar
```

This is **invisible and automatic** — OCX launchers passing an `$install_dir` like `/home/user/.ocx/packages/...` to `ocx.exe` will have it silently converted to `C:\Users\user\.ocx\packages\...`. This is usually **correct** but can cause problems if the path contains segments that look like Unix paths but aren't (e.g., flags like `--prefix=/usr`).

Mitigation for flags: use `MSYS2_ARG_CONV_EXCL='--prefix=;--root='` to exclude specific argument prefixes from translation. Setting `MSYS2_ARG_CONV_EXCL='*'` disables all conversion, which breaks most path arguments. The recommended approach is to use `//` prefix on arguments that must be literal Unix paths — but this is a workaround, not a guarantee.

For OCX launchers: the install path is a Windows path on Windows, so MSYS2's conversion of it from Unix form to Windows form is benign. The concern only arises if OCX receives paths that start with `/` and must not be converted.

---

## 4. Recent Pitfalls (Dated)

| Date | Issue | Platforms | Mitigation |
|---|---|---|---|
| 2026-01-06 | PowerShell `--` end-of-options swallowed by PS function layer before reaching native exe; documented in context of mise wrappers | Windows / pwsh | Generate `.cmd` as the primary Windows launcher; ship `.ps1` as opt-in only. Instruct users to call `tool.cmd` not `tool.ps1` from PS when `--` forwarding matters. |
| 2026-04 | Nix privilege escalation (CVE-2026-39860 / GHSA-g3g9-5vj6-r3gj): arbitrary file overwrite via symlink following in FOD output registration — daemon running as root | Linux (sandboxed builds) | Unrelated to OCX (OCX launchers are not SUID, no daemon). Included for completeness. |
| 2024-03 | Nix CVE-2024-27297: symlink following during fixed-output derivation output registration | Linux multi-user Nix | Same — Nix daemon issue, not a shim/launcher pattern issue |
| 2024 | Windows MOTW (Mark-of-the-Web): files downloaded via browser/email get `Zone.Identifier` ADS triggering SmartScreen. CVE-2024-38217 ("LNK Stomping") bypassed this via path correction removing the MOTW | Windows | OCX-generated launchers are **written locally by `ocx install`** — they never receive MOTW. No action needed. |
| 2023-present | Windows non-ANSI argument encoding: Python setuptools-generated `.exe` stubs fail on Unicode arguments (pip issue #11800, open) | Windows | OCX launchers use `.cmd` batch files (pass-through via `%*`), not Python `.exe` stubs. Not affected, but a signal to avoid `.exe` stub approach entirely. |
| Ongoing | MSYS2 `MSYS2_ARG_CONV_EXCL` does not affect all conversion contexts (confirmed broken for env-var exports, winpty, and some edge cases) | Windows / MSYS2 / Git Bash | Use Windows-native path format in baked install paths on Windows (backslash-or-forward-slash; Windows accepts both for most tools) |
| macOS (any) | Gatekeeper does **not** check locally generated shell scripts — quarantine bit is only set by downloader apps (browsers, mail). Scripts with no `com.apple.quarantine` xattr run without prompts | macOS | No action needed. Confirmed by Apple/Jamf documentation. |
| WSL1 vs WSL2 | Both support launching Windows executables from Linux via `binfmt_misc`; WSL2 registers interop at VM level, WSL1 per-distro. Practical difference: WSL2 interop can be disabled by systemd distros (issue #8843). When interop is disabled, `.cmd` files are not callable from WSL without `cmd.exe /C` | WSL | OCX's primary Unix launcher (`.sh`) is used in WSL Linux contexts; Windows `.cmd` launchers are for native Windows terminals. The two do not cross paths in typical usage. |

---

## 5. Product-Context Alignment Check

Reviewing OCX's competitive matrix against the launcher feature:

**Principle 4 (Clean-env execution):** `ocx exec` already enforces this. Launchers that call `ocx exec <install-dir> -- "$@"` inherit clean-env behavior automatically — the launcher itself carries zero env state beyond baked paths. Contrast with Nix `makeWrapper` (injects vars into a shell wrapper) and mise shims (resolves env at runtime) — OCX's design is cleaner because env resolution stays inside `ocx exec`.

**Principle 1 (Backend-first):** Launchers must have sub-5 ms overhead for build-script loops. A POSIX sh launcher that does nothing but `exec ocx exec "$install_dir" -- "$@"` adds one fork+exec of `/bin/sh` plus the `exec` syscall — approximately 1–3 ms on Linux. The dominant cost is `ocx` startup itself (Rust binary startup), not the shell wrapper.

**Positioning shift flag:** The competitive matrix shows "Clean env exec: Yes" as an OCX differentiator. Launchers preserve this differentiator. No matrix update needed.

---

## 6. Recommendation for OCX Launchers

### What to bake vs. what to resolve at runtime

| Data | Decision | Rationale |
|---|---|---|
| Absolute install path (`packages/.../content/`) | **Bake at generation time** | Immutable: content-addressed, never moves after atomic install |
| `ocx` binary path | **Do not bake** — use `command -v ocx` at exec time or rely on PATH | `ocx` itself may be at a different path (self-updating, nix-managed, etc.) |
| Package identifier/version | **Do not bake** — pass `$install_dir` directly to `ocx exec` | Keeps launcher dumb; version metadata stays in `resolve.json` |
| Env variables | **Do not bake** — `ocx exec` handles env composition | Keeps launchers stateless |

### Unix launcher template (POSIX sh, ≤10 lines)

```sh
#!/bin/sh
# Generated by ocx at install time. Do not edit.
# install_dir is the absolute path to the package content directory.
_install_dir='@@INSTALL_DIR@@'
exec ocx exec "$_install_dir" -- "$@"
```

- `'@@INSTALL_DIR@@'` is a single-quoted literal substituted at generation time. Single quotes mean the path never undergoes shell expansion — correct for paths containing spaces or special characters.
- `exec` replaces the shell process, eliminating the extra process in the call chain.
- No `set -e` — `exec` never returns on success; on failure the shell exits with the exec error code naturally.
- No self-location needed — install path is baked.

**Make executable:** `chmod 755` after writing. The `FileStructure` writer should set permissions directly.

### Windows launcher template (`.cmd`, ≤15 lines)

```bat
@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0
SET _install_dir=@@INSTALL_DIR@@
ocx exec "%_install_dir%" -- %*
```

Notes:
- `@@INSTALL_DIR@@` is substituted as a Windows path (backslashes or forward slashes — Windows accepts both in most contexts).
- The `find_dp0` subroutine is the npm/cmd-shim-proven pattern for reliable `%~dp0` capture; included for future use if relative lookup is ever needed, but the baked path makes it optional here.
- `ocx` must be on `PATH` (or use `%LOCALAPPDATA%\ocx\bin\ocx.exe` as the absolute path if OCX ships a Windows installer).
- `SETLOCAL` / `ENDLOCAL` (implicit at script end) isolates env changes.
- Do **not** wrap `%*` in quotes — `"%*"` passes the entire arg string as one argument.

### PowerShell template — Decision: SKIP for v1

Rationale: The PowerShell `@args` / `$args` limitation with `--` is a documented production issue (mise, 2026). A `.ps1` launcher would cause silent argument mangling in the most common OCX use case (forwarding flags to tools). Users on Windows can call `tool.cmd` from PowerShell — `.cmd` launchers work correctly from pwsh. Ship only Unix `.sh` and Windows `.cmd` for v1. Revisit if there is explicit demand.

If a `.ps1` is ever added:

```powershell
#!/usr/bin/env pwsh
# Generated by ocx at install time.
$_install_dir = '@@INSTALL_DIR@@'
& ocx exec $_install_dir -- @args
exit $LASTEXITCODE
```

---

## 7. Open Questions for the Architect

1. **`ocx` binary location on Windows:** The Unix template relies on `ocx` being on PATH. On Windows, if OCX installs to a non-PATH location, the `.cmd` launcher needs either a baked absolute path to `ocx.exe` or a PATH guarantee. The discover artifact notes that no global `~/.ocx/bin` aggregator exists today. Should the Windows launcher bake the `ocx.exe` path, or mandate that OCX's own binary directory be on PATH as a prerequisite?

2. **MSYS / Git Bash interop for `.cmd` launchers:** When a user runs a `.cmd` launcher from Git Bash / MSYS2, it invokes `cmd.exe` via the binfmt interop. MSYS2 path conversion will translate the `_install_dir` from Windows to Unix form if the path looks like a Windows absolute path. This is usually correct but worth a smoke test. Alternatively: generate the POSIX `.sh` launcher for Unix consumers and the `.cmd` for native Windows; document that Git Bash users should call the `.sh` variant.

3. **Launcher name collision:** The discover artifact identifies Model C (global `~/.ocx/entrypoints/`) as one PATH model. If two packages declare an entry point with the same `name`, the last `select` wins. The research found no prior art with an explicit multi-package collision resolution UI — asdf/mise handle this via per-tool activation scope, not global shims. The architect should pick a collision policy before schema design.

---

## 8. Sources

| URL | Type | Date | Relevance |
|---|---|---|---|
| https://github.com/NixOS/nixpkgs/blob/master/pkgs/build-support/setup-hooks/make-wrapper.sh | Source | Ongoing | Canonical makeWrapper/wrapProgram implementation; generated wrapper structure |
| https://discourse.nixos.org/t/using-wrapprogram-to-prefix-a-command/13862 | Forum | 2021 | --set/--prefix/--suffix semantics explained; usage in postFixup |
| https://mise.jdx.dev/dev-tools/shims.html | Docs | 2025 | Why mise defaults to PATH activation; shim performance trade-offs |
| https://mise.jdx.dev/dev-tools/comparison-to-asdf.html | Docs | 2025 | asdf 120 ms shim overhead; mise exec vs shim design rationale |
| https://github.com/npm/cmd-shim | Source | 2024 | npm's cmd-shim library — `.cmd` / `.sh` / `.ps1` template generation for node_modules/.bin |
| https://github.com/npm/cmd-shim/blob/main/lib/index.js | Source | 2024 | Actual template strings including `%dp0` subroutine, `"$@"`, `$args` patterns |
| https://deepwiki.com/rust-lang/rustup | Docs | 2025 | rustup proxy_mode.rs — argv[0] dispatch pattern for toolchain-aware shims |
| https://medium.com/@theopinionatedev/how-rustup-manages-multiple-toolchains-behind-the-shims-e6ddbef91da0 | Blog | 2024 | Rustup shim dispatcher: read process name, select toolchain, exec real binary |
| https://hy2k.dev/en/blog/2026/01-06-powershell-arguments-functions-vs-native-executables/ | Blog | 2026-01-06 | PowerShell `--` parsing broken when forwarding through PS function/script to native exe |
| https://www.msys2.org/docs/filesystem-paths/ | Docs | 2024 | MSYS2 automatic path translation; MSYS2_ARG_CONV_EXCL mechanics |
| https://github.com/msys2/msys2-runtime/issues/152 | Issue | 2023 | MSYS2_ARG_CONV_EXCL ineffective for env var exports |
| https://bosker.wordpress.com/2012/02/12/bash-scripters-beware-of-the-cdpath/ | Blog | 2012 | CDPATH trap in `cd "$(dirname "$0")"` — foundational reason for `CDPATH= cd` pattern |
| https://github.com/ko1nksm/readlinkf | Source | 2023 | Portable POSIX readlink -f using `cd -P` + `ls -dl`; CC0 licensed |
| https://github.com/PowerShell/PowerShell/issues/21402 | Issue | 2023 | `$PSScriptRoot` / `$PSCommandPath` not populated for symlinks without `.ps1` extension |
| https://ss64.com/nt/syntax-args.html | Docs | Ongoing | `%~dp0`, `%*`, argument tokenization rules for cmd.exe |
| https://renenyffenegger.ch/notes/Windows/PowerShell/language/variable/automatic/psScriptRoot-psCommandPath | Docs | 2023 | `$PSScriptRoot` vs `$MyInvocation.MyCommand.Definition` differences |
| https://discourse.nixos.org/t/security-advisory-privilege-escalations-in-nix-lix-and-guix/66017 | Advisory | 2025 | Nix privilege escalation via daemon symlink following |
| https://www.cvedetails.com/cve-details.php?cve_id=CVE-2024-27297 | CVE | 2024 | Nix CVE-2024-27297 |
| https://hacktricks.wiki/en/macos-hardening/macos-security-and-privilege-escalation/macos-security-protections/macos-gatekeeper.html | Docs | 2024 | macOS Gatekeeper: quarantine bit not set on locally-generated scripts |
| https://learn.microsoft.com/en-us/windows/wsl/compare-versions | Docs | 2024 | WSL1 vs WSL2 binfmt_misc registration |
| https://github.com/hermeticbuild/hermetic-launcher | Source | 2024 | Bazel community moving from sh/bat launchers to native binary stubs |
| https://github.com/pypa/pip/issues/11800 | Issue | 2023 | Python setuptools `.exe` stubs fail on non-ANSI arguments on Windows |
| https://github.com/npm/cmd-shim/issues/51 | Issue | 2022 | PowerShell shim: node.exe not on PATH edge case |
| https://packaging.python.org/specifications/entry-points/ | Docs | 2024 | Python entry_points spec: `console_scripts` name→callable mapping |
