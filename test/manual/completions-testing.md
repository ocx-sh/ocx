# Manual cross-shell completion + activation testing

Maintainer QA for `ocx self activate` (inline shell completions) and the
`install.sh` / `install.ps1` activation shims. Not part of CI — run locally when
touching completion generation, the activation stream, or the env shims.

Completions are emitted **inline** in the `ocx self activate` output (no file on
disk). The shim (`$OCX_HOME/env.sh` / `env.ps1` / `env.elv` / ...) evals that
output at interactive shell startup.

## 1. Build + test-install a local binary

`__OCX_TESTING_INSTALL_BINARY` (double-underscore = test-only) makes the installer
skip the download + registry bootstrap and place your freshly built binary as the
candidate, then wire all env files. No network, no published release needed.

```sh
cargo build -p ocx                      # debug binary at target/debug/ocx
BIN="$PWD/target/debug/ocx"
export OCX_HOME=/tmp/ocx-qa
rm -rf "$OCX_HOME"

# POSIX / Linux / macOS (writes env.sh, env.fish, env.ps1, env.nu, env.elv):
__OCX_TESTING_INSTALL_BINARY="$BIN" OCX_HOME="$OCX_HOME" \
  sh website/src/public/install.sh --no-modify-path
```

On Windows (PowerShell), with a Windows `ocx.exe`:

```powershell
$env:__OCX_TESTING_INSTALL_BINARY = "C:\path\to\ocx.exe"
$env:OCX_HOME = "$env:TEMP\ocx-qa"
& { iex (Get-Content -Raw website\src\public\install.ps1) }   # or run the script directly
```

## 2. Per-shell checks

For each shell, open a **fresh interactive session**, source the env file (or eval
the activation directly), then confirm `ocx ` + <kbd>Tab</kbd> completes and stderr
is clean. The quick non-interactive probes below cover the structural contract;
real Tab-completion needs an interactive session.

| Shell | Completion? | Quick probe |
|-------|-------------|-------------|
| bash | yes | `bash -i -c '. "$OCX_HOME/env.sh"; complete -p ocx'` -> shows `_ocx` |
| zsh | yes | `zsh -i -c '. "$OCX_HOME/env.sh"'` -> exit 0, **no** `command not found: compdef` |
| fish | yes | `fish -c 'source "$OCX_HOME/env.fish"; complete -C "ocx "'` -> completions listed |
| elvish | yes (interactive only) | open interactive `elvish`, `ocx `<kbd>Tab</kbd> -> completes. (clap's elvish completion uses the interactive-only `edit:` module, so it cannot be exercised by `elvish -c`.) |
| nushell | no (env only) | `nu -c 'source "$OCX_HOME/env.nu"'` -> exit 0, PATH updated |
| dash / sh | no (env only) | `dash -c '. "$OCX_HOME/env.sh"'` -> exit 0 |
| ksh | no (env only) | `ksh -c '. "$OCX_HOME/env.sh"'` -> exit 0 |
| ash (busybox) | no (env only) | `busybox ash -c '. "$OCX_HOME/env.sh"'` -> exit 0 |
| pwsh (Linux/macOS) | yes | `pwsh -NoProfile -Command '. "$env:OCX_HOME/env.ps1"; Get-Command ocx'` -> resolves `ocx` (not `ocx.exe`), no `USERPROFILE` null error |

Key regressions this guards:
- **zsh**: the inlined completion self-loads `compinit` before clap's trailing
  `compdef`, so sourcing before the user's own `compinit` (e.g. from `.zprofile`)
  does not error.
- **PowerShell**: the completion is emitted **first** so its `using namespace`
  leads the stream and `Invoke-Expression` accepts it.
- **pwsh on Linux/macOS**: `env.ps1` resolves `ocx` via `$IsWindows` + a `$HOME`
  fallback for the null `$env:USERPROFILE`, with forward-slash paths.

### Installing the shells on Fedora / RHEL (dnf)

```sh
sudo dnf install -y fish dash ksh busybox elvish
# nushell is not in the Fedora repos; grab a static release:
#   https://github.com/nushell/nushell/releases  (x86_64-unknown-linux-musl)
```

(Debian/Ubuntu: `sudo apt-get install -y fish dash ksh busybox elvish`.)

## 3. Windows leg (the original-bug platform)

The released bug was a Windows PowerShell 5.1 parse-error cascade. From WSL you can
drive the Windows host's interpreters directly:

```sh
# Generate the activation stream, then load it under each interpreter.
ocx self activate --shell=powershell --completion > /tmp/act.ps1

# Windows PowerShell 5.1 (the Win10/11 default) via WSL interop:
powershell.exe -NoProfile -Command "Invoke-Expression (Get-Content -Raw -Encoding UTF8 '\\wsl.localhost\<distro>\tmp\act.ps1'); 'OK'"
# Expect: OK, no "Missing ')'" / "using statement" parse errors.
```

Then in an interactive `powershell.exe` (5.1) and `pwsh` (7): `ocx `<kbd>Tab</kbd>
completes.

## 4. Automated guards (already in CI)

- `cargo test -p ocx` — `app::tests::cli_definition_is_valid` (clap structure),
  `command::self_group::activate::tests::*` (inline generators, zsh compinit guard,
  PowerShell leads with `using namespace`, **completion output is ASCII-only**).
- `cd test && uv run pytest tests/test_self_activate.py tests/test_install_sh.py`
  — inline-emission acceptance + env.ps1 cross-platform + `__OCX_TESTING_INSTALL_BINARY`.

## 5. Cleanup

```sh
rm -rf /tmp/ocx-qa /tmp/ocx-p7
# remove any `# BEGIN ocx` / `# END ocx` blocks added to your profiles if you
# ran without --no-modify-path
```
