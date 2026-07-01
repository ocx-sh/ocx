# Manual cross-shell completion + activation testing

Maintainer QA for `ocx self activate` (inline shell completions) and the
`ocx self setup` activation shims. Not part of CI — run locally when
touching completion generation, the activation stream, or the env shims.

Completions are emitted **inline** in the `ocx self activate` output (no file on
disk). The shim (`$OCX_HOME/env.sh` / `env.ps1` / `env.elv` / ...) evals that
output at interactive shell startup.

## 1. Build + self-setup a local binary

`ocx self setup` self-installs the running binary into the content store and
wires all env files — no download, no published release needed. Run it with
`--offline` so it never touches a registry, and `--no-modify-path` so QA never
edits your real login profile.

```sh
cargo build -p ocx                      # debug binary at target/debug/ocx
BIN="$PWD/target/debug/ocx"
export OCX_HOME=/tmp/ocx-qa
rm -rf "$OCX_HOME"

# POSIX / Linux / macOS (writes env.sh, env.fish, env.ps1, env.nu, env.elv):
OCX_HOME="$OCX_HOME" "$BIN" --offline self setup --no-modify-path
```

On Windows (PowerShell), with a Windows `ocx.exe`:

```powershell
$env:OCX_HOME = "$env:TEMP\ocx-qa"
& "C:\path\to\ocx.exe" --offline self setup --no-modify-path
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
- **pwsh on Linux/macOS**: `env.ps1` resolves `ocx` via a StrictMode-safe
  `$env:OS -eq 'Windows_NT'` probe (not `$IsWindows`, which is undefined and
  throws on WinPS 5.1) plus a `$HOME` fallback for the null `$env:USERPROFILE`,
  with forward-slash paths.

### Installing the shells on Fedora / RHEL (dnf)

```sh
sudo dnf install -y fish dash ksh busybox elvish
# nushell is not in the Fedora repos; grab a static release:
#   https://github.com/nushell/nushell/releases  (x86_64-unknown-linux-musl)
```

(Debian/Ubuntu: `sudo apt-get install -y fish dash ksh busybox elvish`.)

## 3. Windows leg (the original-bug platform)

The released bug was a Windows PowerShell 5.1 parse-error cascade (non-ASCII bytes
in install.ps1 / the generated env.ps1 misread under the console codepage) plus a
`$IsWindows` StrictMode crash in env.ps1. Verify the fix two ways: the one-shot
task (drives the real Windows interpreters from WSL) or native PowerShell on a
Windows host.

### One command from WSL

```sh
task rust:verify:windows-activation
```

Cross-builds the x86_64 Windows binary (cargo-xwin, Docker) and runs
`test/manual/test-windows-activation.ps1` under Windows PowerShell 5.1 and - if
installed - PowerShell 7, through WSL interop. It asserts that `ocx self activate`
and the install.ps1-generated `env.ps1` parse and activate cleanly: no parse
cascade, no `$IsWindows`/null-bind crash, completion stream leads with
`using namespace` and is `Invoke-Expression`-safe. On a host without `/mnt/c`
interop it skips with a notice. Override interpreter paths with `OCX_WINPS` /
`OCX_PWSH`; the wrapper is also runnable directly via
`bash test/manual/run-windows-activation.sh`.

### Native PowerShell (on a Windows host)

With a Rust + MSVC toolchain on Windows, build and run the gate directly - the
default `-OcxBin` resolves the native build output:

```powershell
cargo build --release --locked -p ocx_cli --target x86_64-pc-windows-msvc
powershell -NoProfile -File .\test\manual\test-windows-activation.ps1   # WinPS 5.1
pwsh       -NoProfile -File .\test\manual\test-windows-activation.ps1   # PowerShell 7
```

Then in an interactive `powershell` (5.1) and `pwsh` (7): `ocx `<kbd>Tab</kbd>
completes with no startup errors.

## 4. Automated guards (already in CI)

- `cargo test -p ocx` — `app::tests::cli_definition_is_valid` (clap structure),
  `command::self_group::activate::tests::*` (inline generators, zsh compinit guard,
  PowerShell leads with `using namespace`, **completion output is ASCII-only**).
- `cd test && uv run pytest tests/test_self_activate.py tests/test_self_setup.py`
  — inline-emission acceptance + env.ps1 cross-platform (`$env:OS` binary-name
  probe, no `$IsWindows`) + the env shims, versioned fence, and idempotency that
  `ocx self setup` writes.

## 5. Cleanup

```sh
rm -rf /tmp/ocx-qa /tmp/ocx-p7
# remove any `# BEGIN ocx` / `# END ocx` blocks added to your profiles if you
# ran without --no-modify-path
```
