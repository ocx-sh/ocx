# Research: Shell Profile Activation for Login-Shell Exporters

**Date:** 2026-05-16
**Context:** Grounds handshake_toolchain_cli.md §4 — in-repo installer writes a thin
`$OCX_HOME/env.sh` (`eval "$(ocx --global env --shell=sh)"`) and appends a `.` line
to the login profile, idempotently.

## Prior-art idempotency mechanisms (ranked)

| Tier | Pattern | Used by | Uninstall |
|------|---------|---------|-----------|
| 1 (adopt) | Block-marker `# BEGIN ocx` / `# END ocx` | conda | yes (strip block) |
| 2 | Sentinel string contains | rustup, proto | no |
| 3 | Line-text grep | nvm, Homebrew installer | no |

conda is the only surveyed tool supporting install + in-place update (path change)
+ clean uninstall. **OCX adopts the block-marker pattern.**

## Recommended `$OCX_HOME/env.sh` (installer-written)

```sh
#!/bin/sh
# Managed by ocx installer — do not edit.
_ocx_bin="${OCX_HOME:-$HOME/.ocx}/installs/ocx.sh/ocx/current/bin/ocx"
if [ -x "$_ocx_bin" ]; then
    eval "$("$_ocx_bin" --global env --shell=sh 2>/dev/null)" || true
fi
unset _ocx_bin
```

- nvm fail-safe bracket (`[ -x ] && … || true`): never aborts a login shell if
  `ocx` absent or global `ocx.toml` broken.
- Resolve `ocx` via `$OCX_HOME` path, not PATH lookup → kills the bootstrap
  chicken-and-egg (PATH not yet set at profile-eval time).
- `unset` the temp so it doesn't leak into the user's env (conda does this).

## Recommended profile block

```sh
# BEGIN ocx
. "${OCX_HOME:-$HOME/.ocx}/env.sh"
# END ocx
```

Guarded append (POSIX sh):
```sh
grep -qF "# BEGIN ocx" "$_profile" 2>/dev/null \
  || printf '\n# BEGIN ocx\n. "%s/env.sh"\n# END ocx\n' "$_ocx_home" >> "$_profile"
```
Uninstall (POSIX, avoid non-portable `sed -i`):
```sh
awk '/^# BEGIN ocx/{p=1} p{next} /^# END ocx/{p=0; next} {print}' "$_profile" >"$t" && mv "$t" "$_profile"
```
Reinstall / OCX_HOME move = uninstall-then-append (two steps).

## POSIX gotchas

- **`.` (dot) not `source`** everywhere — `source` is bash-only; dash (`/bin/sh`
  on Debian/Ubuntu) lacks it (rustup issue #3415). Applies to env.sh, profile
  line, docs.
- `eval "$(cmd)"` is POSIX, works in dash.
- Per-login cost ~10–30ms (ocx spawn + shell). Once per terminal open, not per
  command. Accepted across conda/pyenv/proto. `|| true` mandatory so a broken
  global `ocx.toml` never aborts shell login.

## Shells: one POSIX file vs separate

Share `$OCX_HOME/env.sh` (POSIX `eval`/`.`): **bash, zsh, sh, dash, ash, ksh**.

Need separate syntax:

| Shell | file | profile line | constraint |
|---|---|---|---|
| fish | `env.fish` | `ocx --global env --shell=fish \| source` | pipe-to-source (fish 4.x) |
| PowerShell | `env.ps1` | `Invoke-Expression (& ocx --global env --shell=pwsh)` | resolve `$PROFILE` at runtime, never hardcode |
| nushell | `env.nu` (STATIC) | `source /abs/path/env.nu` | `source` is parse-time — CANNOT eval dynamically; **regenerate static file on every `ocx --global add`** |

Nushell is the one shell the live-eval model cannot serve — plan a static
regenerated file explicitly before claiming nu support. (For OCX scope now,
POSIX `env.sh` is the only mandatory deliverable; fish/pwsh/nu are per-family
extensions, nu deferred.)

## Profile-target decision tree (login-shell scope)

- zsh → `${ZDOTDIR:-$HOME}/.zprofile` (macOS Terminal opens login → fires; Linux
  GUI terminals open non-login → also note `.zshrc` or write both)
- bash → `~/.bash_profile` if it exists else `~/.profile` (login reads first of
  `.bash_profile`/`.bash_login`/`.profile`; never write both)
- sh/dash → `~/.profile`
- fish → `~/.config/fish/config.fish`
- pwsh → `$PROFILE` (runtime)
- nu → `~/.config/nushell/config.nu` + static `env.nu`

## Sources

rustup shell.rs / unix.rs / issue #3415; nvm install.sh; conda initialize.py +
issue #8703 + activation deep-dive; Homebrew shellenv PR #11789; mise getting-
started; proto setup.rs / v0.38; fish source docs; nushell configuration;
Tratt 2024 shell-startup benchmarks.
