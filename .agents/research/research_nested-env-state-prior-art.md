# Research: nested environment state in environment/toolchain managers

## Metadata

**Date:** 2026-11-06
**Domain:** cli
**Triggered by:** discussion `.agents/discussions/exec-env-model.md` (prior-art web scan lane)
**Expires:** 2027-05-06

## Direct Answer

| Tool | State shape | Selectors via env? | Nesting | Cross-version stance | Clean opt-out |
|---|---|---|---|---|---|
| direnv | Opaque `DIRENV_DIFF` (gzenv `{p,n}`), `DIRENV_WATCHES`, `DIRENV_DIR` | No (cwd-derived) | Revert previous diff, apply new; no stack | Not documented | `DIRENV_DISABLE` (no revert); `direnv exec / tmux` |
| mise | Opaque `__MISE_DIFF` + `__MISE_SESSION` (base64·zlib·MessagePack, `v` field); `__MISE_ACTIVE_WRAPPERS` | `MISE_*` config vars; since May 2026 `exec`/`run` write `__MISE_DIFF` into children | Reverse inherited diff to pristine, rebuild | Explicit `v`; legacy v1 read; decode failure → warn + default | Not found |
| conda | Many: `CONDA_SHLVL`, `CONDA_PREFIX`, `CONDA_PREFIX_{n}`, `CONDA_STACKED_{n}`, `__CONDA_ENV_VARS_{n}` | `CONDA_PREFIX` / `CONDA_DEFAULT_ENV` effectively | Stack or replace (`--stack`/`--no-stack`/`auto_stack`), depth counter, pop on deactivate | Deactivate falls back to a state file for older conda | Not found |
| pixi | `PIXI_ENVIRONMENT_NAME`, `PIXI_PROJECT_MANIFEST`, `PIXI_IN_SHELL` | Yes, gated on `PIXI_IN_SHELL` (PR 1080) | Flag-guarded default, warns on manifest mismatch | Not found | Not found |
| nix | `IN_NIX_SHELL` (`pure`/`impure`); `nix shell` sets none | No | No nesting design | n/a | `nix-shell --pure` (+`--keep`); `nix develop --ignore-environment` |
| devbox | `DEVBOX_SHELL_ENABLED`, `DEVBOX_SHELL_CONFIG` | config as input | Nested-shell warning requested (issue 9) | Not found | `devbox shell --pure` |
| flox | `_FLOX_ACTIVE_ENVIRONMENTS` (JSON array), `FLOX_PROMPT_ENVIRONMENTS`, `FLOX_ENV` | Not found | Layered subshells; same env re-activation "attaches" | Explicitly an implementation detail that may change | Not found |
| rustup/cargo | `RUSTUP_TOOLCHAIN`, `RUSTUP_TOOLCHAIN_SOURCE`; cargo sets `CARGO`, `RUSTC`… | Yes (`RUSTUP_TOOLCHAIN` 2nd in precedence) | Proxy recursion limit | `RUSTUP_TOOLCHAIN_SOURCE` "don't set it yourself" | `rustup run <tc>` as nesting-safe form |
| pyenv/asdf | `PYENV_VERSION`, `ASDF_<TOOL>_VERSION` | Yes | Replace | Not found | Not found |
| pipenv/poetry | `PIPENV_ACTIVE`, `POETRY_ACTIVE` | Not found | Refuse ("already activated") | Not found | Not found |
| virtualenv/uv | `VIRTUAL_ENV`, `_OLD_VIRTUAL_PATH` | uv ignores `VIRTUAL_ENV` for project commands unless `--active` | Replace (`deactivate nondestructive` first) | Not found | uv `--no-active` |
| Bazel `bazel run` | `BUILD_WORKSPACE_DIRECTORY`, `BUILD_WORKING_DIRECTORY` | invocation-describing, not selecting | Not found | Not found | `--test_env` explicit |

## Key Findings

1. **Opaque state converges on diff + version.**
   - mise `__MISE_SESSION`: base64, zlib, MessagePack, version field. A decode failure warns and uses the default; values are hashed. `ensure_env_fits` rejects any var of 128 KiB or more (execve limit). [hook_env.rs](https://docs.rs/crate/mise/latest/source/src/hook_env.rs)
   - direnv `DIRENV_DIFF` is gzenv. [env_diff.go](https://github.com/direnv/direnv/blob/master/internal/cmd/env_diff.go)
   - flox splits a machine JSON var from a human display var. [flox-activate](https://flox.dev/docs/man/flox-activate/)
2. **Nested-exec leak fixed by putting the diff in the child (mise, May 2026).** [PR 9765](https://github.com/jdx/mise/pull/9765)
   - Bug: a nested `mise -C <dir> exec` inherited the outer toolset's install paths as user PATH, so the wrong version ran.
   - Fix: `exec`/`run` embed `__MISE_DIFF` so the nested mise reverses the outer layer.
   - Behaviour change: the outer tools are dropped from the inner PATH.
3. **Selectors in env leak in practice.**
   - rustup 1.25 exported `RUSTC` to children; a nested `cargo +esp` used the wrong compiler. Reverted; re-enable still open. ([#3031](https://github.com/rust-lang/rustup/issues/3031), [#3035](https://github.com/rust-lang/rustup/issues/3035), 2022, re-verify)
   - pyenv `PYENV_VERSION=system` leaked into spawned shells, breaking auto-activation. Maintainers unsure whether `pyenv exec` should set it. ([#2733](https://github.com/pyenv/pyenv/issues/2733), 2023, re-verify)
   - pixi maintainers feared "unexpectedly" running in another env, so the selector is gated behind `PIXI_IN_SHELL` with a manifest check. ([#889](https://github.com/prefix-dev/pixi/issues/889), [PR 1080](https://github.com/prefix-dev/pixi/pull/1080))
   - uv deliberately ignores inherited `VIRTUAL_ENV`. [uv docs](https://docs.astral.sh/uv/concepts/projects/config/)
   - Volta "covers its tracks". [docs](https://docs.volta.sh/guide/understanding)
4. **Long-lived descendants.**
   - direnv's wiki: state leaks into tmux and gets mangled. Fix: `alias tmux='direnv exec / tmux'` (unload before launch), or unset `DIRENV_*` in the tmux config. The config route cannot restore the pre-activation env. [wiki](https://github.com/direnv/direnv/wiki/Tmux)
   - pipenv stale `PIPENV_ACTIVE` refuses the next shell. [#6220](https://github.com/pypa/pipenv/issues/6220)
5. **Nesting semantics span four models.**
   - Depth counter with opt-in stack (conda; `auto_stack` threshold; inconsistent state raises hard errors). [activate.py](https://raw.githubusercontent.com/conda/conda/main/conda/activate.py)
   - Snapshot-and-reverse (direnv, mise, virtualenv).
   - Refuse (pipenv, poetry).
   - Layer with attach (flox). [layering](https://flox.dev/blog/layering-and-composing-flox-environments/)
6. **Cross-version.**
   - mise versions its session and degrades to default.
   - conda keeps a state-file fallback for older activations.
   - flox disclaims the format.
   - rustup marks `RUSTUP_TOOLCHAIN_SOURCE` unstable.
   - No tool promises full cross-version compatibility of internal state.
7. **Clean opt-out.** `nix-shell --pure`/`--keep`, `devbox shell --pure`, `nix develop --ignore-environment` ("blunt enough to break the shell"). No manual describes `--pure` inside an outer activation; the pure child is a fresh start.

## negative

- No direnv E2BIG report found.
- Volta recursion variable unconfirmed.
- rustup exporting `RUSTUP_TOOLCHAIN` to children unconfirmed (docs name only `RUSTUP_TOOLCHAIN_SOURCE`).
- No confirmed conda tmux leak.
- No clean flag for mise.
- No maintainer decision on nix nesting markers.
- Bazel nested invocation behaviour undocumented.
- Several WebSearch summaries contained unsupported inferences; excluded.

## leads

- flox-activations Rust component (attach design, state-dir version handling).
- mise PR #9850 / discussion #9754 (task env across nested hook-env).
- rustup recursion-guard code (PRs #2812/#2958).
- conda `auto_stack` history.
- tmux `update-environment`.
- vscode-python#25163 (IDE env collection as a leak path).
- uv `--active` design discussion.

## Recommendation

None. This is a findings-only lane.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| https://github.com/jdx/mise/pull/9765 | Repo | 2026-05 | Nested exec leak and fix |
| https://docs.rs/crate/mise/latest/source/src/hook_env.rs | Source | 2026 | Session encoding, version, size guard |
| https://github.com/direnv/direnv/wiki/Tmux | Docs | n/a | Long-lived descendants |
| https://raw.githubusercontent.com/conda/conda/main/conda/activate.py | Source | 2026 | Stack/replace/depth |
| https://flox.dev/docs/man/flox-activate/ | Docs | 2026 | Layering, format disclaimer |
| https://github.com/rust-lang/rustup/issues/3031 | Issue | 2022 | Env-transported selector leak |
| https://github.com/pyenv/pyenv/issues/2733 | Issue | 2023 | Selector leak into subshells |
| https://github.com/prefix-dev/pixi/issues/889 | Issue | 2024 | Gated selector |
| https://docs.astral.sh/uv/concepts/projects/config/ | Docs | 2026 | Ignore inherited marker |
| https://nix.dev/manual/nix/stable/command-ref/nix-shell.html | Docs | 2026 | `--pure` |
