# Research: Lazy (on-demand) Tool Materialization via Shims — Prior Art

- **Date:** 2026-08-09
- **Axis:** technology / tools
- **Consumed by:** [`adr_lazy_package_loading.md`](./adr_lazy_package_loading.md), [ocx-sh/ocx#302](https://github.com/ocx-sh/ocx/issues/302)
- **Method:** web research over vendor docs, source, and issue trackers. Gaps are flagged rather than guessed.

## Summary

Three of the four closest analogues (aqua, Volta, Hermit) are **declared-name**
designs: the set of command names comes from a manifest field, not from scanning an
installed tree. The one scanned-name design (mise) documents the resulting
limitation hardest — it cannot lazily bootstrap a tool it has never installed.
OCX's `binaries` field puts it in the declared-name family.

Every surveyed tool converged on **one shared proxy binary with N links to it**,
and every one that started with script stubs rewrote them as a native binary.

## aqua (aquaproj/aqua)

| Question | Answer |
|---|---|
| PATH artifact | One symlink per command name in `$AQUA_ROOT_DIR/bin`, **all pointing at the same `aqua-proxy` binary**. Since v2.30.0: hard links on Windows + `aqua-proxy` (was: shell/`.bat` script stubs). |
| Name source | **Declared** — `files:` in the package's `registry.yaml` entry ("the list of executable files"). Omitted + `repo_owner`/`repo_name` set → defaults to `files: [{name: <repo name>}]`. No archive scan, no error. |
| Trigger | First exec of the symlink → `aqua-proxy` → `aqua exec` → checks install state → downloads to `$AQUA_ROOT_DIR/pkgs` → runs the real binary. Disable via `AQUA_DISABLE_LAZY_INSTALL=true`. |
| Progress | **Opt-in** (`AQUA_PROGRESS_BAR=true`), and broken when checksum/SLSA/cosign verification is on — the bar covers only unarchive, not the pre-verify download ([#2003](https://github.com/aquaproj/aqua/issues/2003)). |
| Exit codes / signals | Not documented — gap. |
| Concurrency | Not documented for the lazy path. `AQUA_MAX_PARALLELISM` (default 5) bounds bulk `aqua i` only. |
| Offline | No offline flag found; open ask for retry-on-transient-error ([#829](https://github.com/aquaproj/aqua/issues/829)). |
| Failure modes | Windows could not execute symlinks in the bin dir at all — forced the v2.30 hardlink+proxy redesign ([#885](https://github.com/aquaproj/aqua/issues/885)). `$AQUA_ROOT_DIR/bin` is shared across all `aqua.yaml` files, so a package must be in an *active* config to resolve. |

Sources: [Lazy Install](https://aquaproj.github.io/docs/reference/lazy-install/) · [aqua-proxy](https://aquaproj.github.io/docs/products/aqua-proxy/) · [Registry Config](https://aquaproj.github.io/docs/reference/registry-config/)

## Volta

| Question | Answer |
|---|---|
| PATH artifact | Symlink per tool name → shared `volta-shim` executable in `~/.volta/bin`. Windows gets no real symlinks by default. |
| Name source | **Declared** — project `package.json` `bin`; core tools (node/npm/yarn/pnpm) natively known, pinned via the `volta` key. |
| Trigger | Shim reads session/argv → `execute_shim` (`volta_core::run`) → resolves pinned version → downloads if absent → runs. Whether the final invocation is `exec()` or spawn is **unconfirmed** from public sources. |
| Exit codes / signals | Not documented — gap. |
| Concurrency | Not documented; [#1744](https://github.com/volta-cli/volta/issues/1744) reports an install that "hangs indefinitely and blocks all other network requests", i.e. observed global serialization. |
| Offline | Hard failure, no cached fallback ([#1506](https://github.com/volta-cli/volta/issues/1506), [#1523](https://github.com/volta-cli/volta/issues/1523), [#197](https://github.com/volta-cli/volta/issues/197)). |
| Failure modes | Windows symlink creation needs Developer Mode or admin (`SeCreateSymbolicLinkPrivilege`) — tracked as "investigate lightweight symlink replacement" ([#579](https://github.com/volta-cli/volta/issues/579)); stale/broken symlinks after reinstall ([#1053](https://github.com/volta-cli/volta/issues/1053)); not every package manager is auto-shimmed ([#1554](https://github.com/volta-cli/volta/issues/1554)). |

Sources: [How Volta Works](https://www.voltajs.com/guide/how-it-works.html) · [volta-shim.rs](https://github.com/volta-cli/volta/blob/main/src/volta-shim.rs)

## Hermit (cashapp/hermit)

| Question | Answer |
|---|---|
| PATH artifact | Real files **committed to the project's git repo** under `./bin/` (the explicit differentiator), plus `activate-hermit`, `hermit`, `hermit.hcl`. Source references `EnvDirFromProxyLink` and "proxy symlink" terminology. Exact stub format not retrievable. |
| Name source | **Declared** — the `binaries` attribute in the package `.hcl` manifest; supports globs, expanded at unpack time. |
| Trigger | Explicit `hermit install <pkg>`, or first invocation of a linked binary, which bootstraps Hermit if necessary, auto-installs, then execs. |
| Exit codes / concurrency / offline | Not covered in retrievable docs — gaps. |
| IDE integration | Well documented: an IntelliJ plugin auto-configures SDKs, run configs, terminal env. VS Code's terminal PATH rewriting **conflicts** with activation — docs instruct nulling `ACTIVE_HERMIT`, `HERMIT_ENV`, `HERMIT_ENV_OPS`, `HERMIT_BIN`. Other editors: "activate in a terminal, then launch the editor from that terminal." |

Sources: [Get Started](https://cashapp.github.io/hermit/usage/get-started/) · [Packaging Tutorial](https://cashapp.github.io/hermit/packaging/tutorial/) · [IDE Integration](https://cashapp.github.io/hermit/usage/ide/)

## mise (jdx/mise)

| Question | Answer |
|---|---|
| PATH artifact | Shims in `~/.local/share/mise/shims` (`%LOCALAPPDATA%\mise\shims`), effectively symlinks to the `mise` binary, dispatching on argv0. |
| Name source | **Scanned** — generated by enumerating every binary provided by an *already-installed* version. No manifest field. |
| Trigger | First exec, gated by `not_found_auto_install` / `MISE_NOT_FOUND_AUTO_INSTALL` (default true) — but per the row above this can only fill a **missing version of a known tool**, never bootstrap a new one. Unresolvable → falls through to the next same-named executable on PATH. |
| Offline | `offline` / `MISE_OFFLINE` / `--offline` blocks all HTTP. Without it, an unreachable network degrades shell startup ([#4598](https://github.com/jdx/mise/discussions/4598), [#4314](https://github.com/jdx/mise/discussions/4314)). |
| Documented caveats | `which`/`command -v` return the shim path (`mise which` is the workaround); most hooks don't fire in shim mode; mise-defined env vars are visible only to mise-managed tools. Docs frame `mise activate` (PATH rewriting) as **primary** and shims as the IDE/CI **fallback**. |
| Auto-install posture | Maintainer: auto-installing during shim execution or the prompt hook "would add unpredictable latency"; a `cd` into a project "shouldn't silently block while downloading" ([#8312](https://github.com/jdx/mise/discussions/8312)). |

Sources: [Shims](https://mise.jdx.dev/dev-tools/shims.html) · [Troubleshooting](https://mise.jdx.dev/troubleshooting.html) · [Settings](https://mise.jdx.dev/configuration/settings.html)

## Secondary

- **asdf** — bash-script shims per executable, each embedding plugin+version metadata, calling `asdf exec`. Names **scanned**, and only for the plugin's own binaries: anything installed indirectly (`npm install -g` inside an asdf node) is invisible until a manual `asdf reshim`. **No lazy install** — a shim errors if the version isn't present. [Core](https://asdf-vm.com/manage/core.html) · [FAQ](https://asdf-vm.com/more/faq.html)
- **proto (moonrepo)** — v0.26 replaced Bash/PowerShell/`.cmd` stubs with one native Rust `proto-shim` using **`execvp` on Unix (true process replacement)**, explicitly to fix broken stdio redirection, interactive prompts, Ctrl+C/signal passthrough and exit-code propagation that script shims got wrong. Two entries: `~/.proto/shims` (re-resolves per invocation) vs `~/.proto/bin` (fixed symlink). [v0.26 blog](https://moonrepo.dev/blog/proto-v0.26)
- **pkgx** — no persistent PATH shim; `pkgx <cmd>` computes an ephemeral env, downloads on first use into `~/.pkgx`, leaves nothing behind. Optional shebang shims (`#!/usr/bin/env -S pkgx -q! node@22`); `pkgm` installs into `/usr/local` for persistence. Trap: bare `make` fetches GNU make from the pantry even when a system `make` exists. [docs](https://docs.pkgx.sh/pkgx/pkgx)
- **Nix `comma` / `command-not-found`** — neither is a PATH shim. `command-not-found` only *prints* a suggestion; `comma` (`, <cmd>`) resolves via a `nix-locate` index and `nix run`s it, discarding the env — closer to `pnpm dlx`. [Discourse](https://discourse.nixos.org/t/auto-install-nix-packages-when-command-cant-be-found/12442)
- **devbox** — no shims at all; `devbox shellenv` prepends a Nix profile dir whose binaries are already fully installed. Eager at `devbox add` time. [Devbox Global](https://www.jetify.com/docs/devbox/devbox-global)
- **Bazel bzlmod** — same "declare now, materialize on first real use" pattern at the build-graph layer: a module extension's repo-defining logic and its fetch run only when a requested target resolves a label into that repo via `use_repo`. Call-by-need over the dependency graph, not PATH interception. [bzlmod docs](https://bazel.build/versions/6.1.0/build/bzlmod)
- **npx / pnpm dlx** — explicit fetch-to-cache, run once, discard. `pnpm dlx` gained a dedicated cache in v9 (April 2024). Nothing on PATH before or after. [discussion](https://github.com/orgs/pnpm/discussions/5820)

## Comparison

| Tool | Name source | Shim kind | Trigger | Offline | Locking |
|---|---|---|---|---|---|
| aqua | **Declared** (`files:`) | symlink/hardlink → shared `aqua-proxy` | first exec | undocumented | undocumented |
| Volta | **Declared** (`bin`) | symlink → shared `volta-shim` | first exec | hard failure | undocumented; observed global stall |
| Hermit | **Declared** (`binaries`, globs) | committed files under project `bin/` | first exec or explicit install | undocumented | undocumented |
| mise | **Scanned** | symlink → `mise` | first exec, gated | `MISE_OFFLINE` | undocumented |
| asdf | Scanned, manual `reshim` | bash script per binary | none — errors | n/a | n/a |
| proto | Declared via plugin | native `proto-shim`, `execvp` | first exec, re-resolves each call | not covered | not covered |
| pkgx | pantry query | none persistent | first exec | not covered | not covered |
| devbox | n/a — eager | none (PATH splice) | n/a | n/a | n/a |
| bzlmod | `use_repo` graph ref | n/a (build-graph laziness) | first label reference | n/a | not researched |

## Design traps everyone hit

1. **`which`/`command -v` lie.** Every tool that documents it admits the shim path is returned; mise ships `mise which` as the workaround.
2. **The bootstrapping paradox.** A scanned-name design cannot materialize a tool with zero installed versions. mise's docs concede auto-install only fills a missing *version* of an already-known tool.
3. **Windows symlinks are second-class everywhere.** aqua rebuilt its proxy mechanism because Windows could not execute symlinks in the bin dir; Volta needs Developer Mode/admin; proto's pre-0.26 Windows shims spanned three incompatible script formats.
4. **Silent-by-default first-run downloads read as hangs.** aqua's progress bar is opt-in *and* broken under verification; Volta users file "hangs indefinitely" issues.
5. **IDEs and shim-based PATH tricks fight each other.** Hermit documents nulling four env vars for VS Code; mise recommends shims *because* PATH-rewrite activation never reaches GUIs.
6. **Shim mode is a reduced-functionality mode.** mise: hooks mostly don't fire, env vars don't propagate to the general shell.
7. **Reshim is an easy-to-forget manual step** for indirectly installed binaries (asdf).
8. **`exec()` vs `spawn()` is a correctness axis, not style.** proto attributes its Ctrl+C/signal and exit-code bugs to script shims and fixes them with `execvp`.

## Gaps

Exit-code/signal passthrough and concurrent-invocation locking are **not documented**
for aqua, Volta, Hermit or mise in any retrievable page. Only proto states its process
model explicitly. Concurrency evidence elsewhere is indirect (GitHub issues), never a
designed locking story.
