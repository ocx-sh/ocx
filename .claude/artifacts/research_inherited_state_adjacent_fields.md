# Research: inherited state vs per-invocation selection in adjacent fields

## Metadata

**Date:** 2026-11-06
**Domain:** cli
**Triggered by:** discussion `.agents/discussions/exec-env-model.md` (adjacent fields lane)
**Expires:** 2027-05-06. gnu.org returned HTTP 429, so make rows marked [unverified] rest on snippets and recall.

## Direct Answer

| System | State transport | Nesting | Stale/foreign detection | Cross-version | Hermetic mode |
|---|---|---|---|---|---|
| GNU make | `MAKEFLAGS` (flags + jobserver auth), `MAKELEVEL` counter [unverified]; jobserver = inherited fds or fifo described in env | sub-make reads inherited `MAKEFLAGS`, then its argv [unverified]; last `--jobserver-auth` wins | non-make children inherit pipe fds; users filter the auth word | accepts `fifo:PATH` and `R,W` | clear `MAKEFLAGS` by hand |
| cargo / `jobserver` crate | `CARGO_MAKEFLAGS` (cargo-owned name) | re-exported only via `configure()` | `from_env_ext` validates the fds (`F_GETFD`, `S_IFIFO`), else `Err` | both formats; negative fds = disabled | `configure_make()` clobbers `MAKEFLAGS`/`MFLAGS` |
| shells | `SHLVL` | +1 per shell, −1 before implicit exec | none | edge cases restored in bash 5.2 | `env -i` |
| git | repo-local `GIT_*` fixed list (15 names in `environment.c`) | cleared before running git in another repo | none (hard-coded list) | count-indexed `GIT_CONFIG_COUNT/_KEY_n/_VALUE_n` replaced `GIT_CONFIG_PARAMETERS`; gaps are errors | strips the local list only |
| Bazel | client env filtered into the action env | nested bazel contends for the server lock | none for env | strict-PATH default tried in 0.21, rolled back in 0.22 | `--incompatible_strict_action_env`, `--action_env` re-adds |
| Nix | builder env cleared; `__structuredAttrs` → JSON file | `nix develop` rebuilds the env | — | — | `impureEnvVars` (fixed-output only); `nix develop --ignore-env --keep-env-var` |
| systemd | per-unit lists; `UnsetEnvironment=` applied last | user manager passes its env | `LISTEN_PID` must equal the caller's PID (+ `LISTEN_PIDFDID` since v259); `unset_environment` deletes after reading | optional new field | `PassEnvironment=` allowlist |
| direnv / mise | one opaque diff blob; mise session with `v` | per shell session | mise: hash of `MISE_*`, mtimes, cwd | mise `v`, defaults on missing | direnv fixed ignore list |

## Key Findings

1. **Two-layer pattern: resource in fds or a file, description in env.** make, cargo and systemd keep the live resource (pipe fds, sockets) out of the environment and put only a pointer in it. Nix moves bulk state to a JSON file because env size is limited. When the string survives an exec that dropped the fds, the pointer is stale.
2. **Recipient-scoped validity.** systemd's `LISTEN_PID` makes inherited state valid only for the one process it was meant for. Any other descendant ignores it, and the reader deletes it after use. [sd_listen_fds](https://www.freedesktop.org/software/systemd/man/latest/sd_listen_fds.html)
3. **Argv outranks inherited env.** git `-c` beats `GIT_CONFIG_*`, which beats config files. Bazel `--action_env` is an invocation flag. make parses inherited flags before its own argv [unverified].
4. **A named "local" set cleared at a boundary.** git's `local_repo_env` is a fixed list: clearing is a list iteration at one place. [environment.c](https://raw.githubusercontent.com/git/git/master/environment.c)
5. **State trackers exclude shell-owned variables.** direnv excludes `SHLVL`, `PWD`, `OLDPWD`, `_`, `PS1`, `SHELLOPTS`, `COMP_WORDBREAKS`, `BASH_FUNC_*`, `__fish*`.
6. **Packing a list into one variable.** cargo `CARGO_ENCODED_RUSTFLAGS` uses a `0x1f` separator because whitespace splitting is unreliable.
7. **Size limits.** Linux caps one env string at 128 KiB (`MAX_ARG_STRLEN`, mise guards it). The Windows environment block is 32,767 characters. Nix `passAsFile` exists because env limits are "often a few hundred kilobytes".

## negative

- make manual pages not retrievable (429).
- Bazel nested env inheritance undocumented.
- Nix `.attrs.json` naming not confirmed.
- git `prepare_other_repo_env` body not fetched.
- No depth counters in git, Nix, systemd or Docker.

## leads

- conda `CONDA_SHLVL` stack.
- asdf/pyenv/nvm save-previous-value pairs.
- GNU make 4.4 fifo cross-version history.
- `ARG_MAX`/`MAX_ARG_STRLEN`.
- Windows env block and case-insensitivity.
- `/proc/<pid>/environ` readability for secrets.

## Recommendation

None. This is a findings-only lane.

## Sources

https://docs.rs/jobserver/latest/jobserver/struct.Client.html, https://raw.githubusercontent.com/rust-lang/jobserver-rs/master/src/unix.rs, https://doc.rust-lang.org/cargo/reference/environment-variables.html, https://git-scm.com/docs/git-config, https://raw.githubusercontent.com/git/git/master/environment.c, https://bazel.build/docs/user-manual, https://groups.google.com/g/bazel-discuss/c/AZek-OR1t7A, https://nix.dev/manual/nix/stable/language/advanced-attributes.html, https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-develop.html, https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html, https://www.freedesktop.org/software/systemd/man/latest/sd_listen_fds.html, https://docs.docker.com/reference/cli/docker/container/run/, https://github.com/direnv/direnv/blob/master/internal/cmd/env_diff.go, https://docs.rs/crate/mise/latest/source/src/hook_env.rs, https://mail.gnu.org/archive/html/bug-bash/2022-06/msg00088.html.
