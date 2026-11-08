# Research: community reports of env leaks into nested and long-lived descendants

## Metadata

**Date:** 2026-11-06
**Domain:** cli
**Triggered by:** discussion `.agents/discussions/exec-env-model.md` (community threads lane)
**Expires:** 2027-05-06. Most git, cargo and rustup sources are 2012–2022: established history, not current reports.

Response patterns: (a) strip at every spawn site; (b) never write the variable; (c) ignore inherited unless opted in; (d) version-stamp or fingerprint and discard; (e) wrap long-lived processes in a clean launcher.

## Direct Answer

| Tool | Leaked | Symptom | Response | Link |
|---|---|---|---|---|
| git hooks → git in another repo | `GIT_INDEX_FILE`, `GIT_DIR`, `GIT_WORK_TREE` | another repo's HEAD paired with the hook repo's index | (a) `unset $(git rev-parse --local-env-vars)`; (b) git 2.18 stopped setting `GIT_DIR` for pre-commit | https://public-inbox.org/git/20180826004150.GA31168@sigill.intra.peff.net/ |
| git submodules | repo-local `GIT_*` | foreach/add misbehave | (a) git clears its fixed list on descent; later exports `GIT_DIR=.git` so a broken submodule errors instead of falling back to the superproject | https://timplab.syktsu.ru/admin/git/commit/10f5c52656 |
| cargo-llvm-cov | `CARGO_ENCODED_RUSTFLAGS` | nested cargo in `build.rs` inherits instrumentation flags | (a) per-site `env_remove`; parent cannot fix (cargo re-sets it) | https://github.com/taiki-e/cargo-llvm-cov/issues/212 |
| cargo helpers | `CARGO*`, `RUST*` | wrong toolchain, needless rebuilds | (a) wholesale strips; windows-drivers-rs reverted one after breakage; assay-cli keeps user-set ones | https://github.com/microsoft/windows-drivers-rs/pull/609 |
| rustup 1.25.0 | `RUSTC`, `RUSTDOC` | inner `+toolchain` silently overridden | (b) reverted in 1.25.1 | https://blog.rust-lang.org/2022/07/12/Rustup-1.25.1/ |
| npm/npx/pnpm | `npm_config_*`, `npm_execpath`, `INIT_CWD` | npx-filled config breaks nested npx; inconsistency across tools | (a)/(c) | https://gitlab.com/antora/antora-collector/-/work_items/50 |
| Bundler | `BUNDLE_GEMFILE` | subshells and Puma workers inherit the parent's Gemfile | (a) `Bundler.with_unbundled_env` restores the pre-activation env | https://bundler.io/man/bundle-exec.1.html |
| uv | `VIRTUAL_ENV` | another project's venv drives project commands | (c) ignore + warn, `--active` opt-in | https://github.com/astral-sh/uv/pull/6864 |
| direnv + tmux | `DIRENV_*` | tmux captures the backup, restores it wrongly later | (e) `direnv exec / tmux`; the wiki admits it doesn't revert a pre-tmux activation | https://github.com/direnv/direnv/issues/82 |
| tmux server | any activation var | new panes get the server-start env; sessions cross-contaminate | (e) kill server, `update-environment` | Unix SE 565853 mirror |
| yarn (nixpkgs) | `YARN_IGNORE_PATH` | avoid re-dispatch via an inherited selection | (c) | https://code.ornl.gov/nix/nixpkgs/-/commit/42bb244ece82f27ccb8e9aa0e04ee7c48bcedd25 |

## Key Findings

1. **git is the closest analogue.**
   - It names a fixed set of "repository-local" variables (`git rev-parse --local-env-vars`) and clears that set before working in another repository.
   - It stopped writing `GIT_DIR` for pre-commit (never-write).
   - Clearing has its own hazard: auto-discovery can land on the wrong repo, so git exports an explicit value to disable discovery.
2. **The only well-documented "set it so children are faster, then revert" case** is rustup 1.25 `RUSTC`: a selector-like variable overrode an inner explicit choice.
3. **uv** declined to honour an inherited activation marker for project commands because a forgotten activation silently drove project operations. It added a warning plus `--active`.
4. **Long-lived descendants keep their launch-time environment.** Panes never update (tmux). Wrap-in-clean-launcher advice is incomplete by direnv's own admission.
5. **No community report found of pattern (d)** (version-stamp and discard). Absence is not proof.

## negative

- Most sources are 2012–2022.
- Many findings come from search summaries; the Jeff King lore thread returned 403.
- Nothing on zellij.
- VS Code and JetBrains reports concern activation failing, not stale activation.

## leads

- git `prepare_other_repo_env` source.
- Bundler `with_unbundled_env` history.
- tmux `update-environment` / zellij docs.
- VS Code `environmentVariableCollection`.
- Corepack re-dispatch (`COREPACK_*`, `YARN_IGNORE_PATH`).

## Recommendation

None. This is a findings-only lane.

## Sources

See the links in the table. Additional: https://git-scm.com/docs/git-rev-parse, https://github.com/rust-lang/rustup/issues/3036, https://github.com/npm/cli/pull/6762, https://github.com/pnpm/pnpm/issues/7042, https://github.com/nvm-sh/nvm/issues/1873, https://github.com/direnv/direnv/wiki/Tmux, https://internals.rust-lang.org/t/introduce-env-var-cargo-run-id-for-identifying-related-processes-threads/21134.
