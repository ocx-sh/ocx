# Research: Shell-Env Reconciler & Stable Launcher Farm — Prior Art

**Date:** 2026-08-02
**Scope:** mise, direnv, volta, proto (moonrepo), scoop, pyenv/rbenv/asdf, Homebrew/SDKMAN/update-alternatives, PowerShell/nushell hook conventions
**Consumed by:** `adr_toolchain_farm.md`, `adr_live_env_reload.md`
**Related:** `research_shell_profile_activation.md` (login-profile idempotent-append, nushell no-`eval`), `research_windows_shim_patterns.md` (OCX `.exe`+`.shim` sidecar design), `research_project_env_declaration.md` (project env declaration grammar; flagged mise trust CVEs)

---

## 1. mise

**Hook mechanism.** `mise activate` registers a shell-specific prompt hook that calls `mise hook-env`: wraps `prompt` (PowerShell — §8), `precmd`/`fish_prompt` equivalents on Unix shells, and a **static regenerated file** on Nushell (no `eval` there — §8). [`mise activate`](https://mise.jdx.dev/cli/activate.html), [glossary](https://mise.jdx.dev/glossary.html)

**Fast-path cost strategy.** `hook-env` implements `should_exit_early()`: skip work unless `force`, the reason is `precmd`/`chpwd`, or watched files (config files + `watch_files`, deduped via `BTreeSet`, including the *previous* session's watch files) changed mtime since the cached session. Documented cost: **~4ms when nothing changed, ~14ms on a full reload** — paid every prompt regardless of tool use. [`src/cli/hook_env.rs`](https://github.com/jdx/mise/blob/main/src/cli/hook_env.rs), [comparison-to-asdf](https://mise.jdx.dev/dev-tools/comparison-to-asdf.html)

**State/diff env vars — key finding.** `EnvDiff` (`src/env_diff.rs`) is a **typed, structured diff**, not a byte snapshot:

```
EnvDiff { old: IndexMap<String,String>, new: IndexMap<String,String>, path: Vec<PathBuf> }
EnvDiffOperation::{ Add(String,String), Change(String,String), Remove(String) }
```

`to_patches()` converts the diff into per-key typed operations. Serialization: **MessagePack → Zlib → Base64 (no pad)**, carried in `__MISE_DIFF`. Session metadata (loaded tools, aliases, watch files, config paths) lives in `__MISE_SESSION`; the pre-activation PATH is preserved in `__MISE_ORIG_PATH` so `build_path_operations()` can filter mise's own additions back out — `pre → user_paths → tool_paths → post → post_user` — specifically to avoid claiming ownership of paths that were already the user's. [`src/env_diff.rs`](https://github.com/jdx/mise/blob/main/src/env_diff.rs), [`src/cli/hook_env.rs`](https://github.com/jdx/mise/blob/main/src/cli/hook_env.rs)

**Shims vs PATH mode.** Shims (`~/.local/share/mise/shims`, `%LOCALAPPDATA%\mise\shims` on Windows) are **symlinks to the mise binary itself**; mise inspects `argv[0]`, resolves the version, execs the real binary. Only shim invocations get mise's env — `[env]` does nothing outside a shim call, and cd/enter/leave hooks don't fire in shims mode. PATH/`activate` mode recommended for interactive use; shims for CI/IDEs/non-interactive. `mise reshim` mostly automatic (fires on install/update/remove and after tool-managed global installs like `npm -g`). [shims](https://mise.jdx.dev/dev-tools/shims.html), [shims-how-they-work](https://jdx.dev/posts/2024-04-13-shims-how-they-work-in-mise-en-place/), [FAQ](https://mise.jdx.dev/faq.html)

**Trust model — weaker than it looks.** "Safe" configs (`min_version`, plain-string `[tools]`, template-free `[tasks]`) never need trust since nothing executes at load time. Anything with `[env]`, templates, hooks, or tool options prompts once. Storage: a symlink under `~/.local/share/mise/trusted-configs/` named `<dir>-<file>-<SipHash-of-canonicalized-path>` — **keyed by path, not content**, in default mode: editing an already-trusted `mise.toml` does **not** force re-prompt outside paranoid mode. CI auto-trusts unless paranoid. Real CVE from this shape: [GHSA-436v-8fw5-4mj8](https://github.com/jdx/mise/security/advisories/GHSA-436v-8fw5-4mj8) — local project settings (`trusted_config_paths`, `yes`, `ci`, `paranoid`) were loaded *before* the trust check ran, so a malicious repo could self-declare trust (affected 2026.2.18–2026.6.3, fixed 2026.6.4). Paranoid-mode hash scheme itself is fragile: `PathBuf::with_extension("hash")` replaces the whole `.toml-<siphash>` suffix, so two config files whose parent dirs share a leaf name collide on one hash file ([discussion #4499](https://github.com/jdx/mise/discussions/4499)). [`mise trust`](https://mise.jdx.dev/cli/trust.html), [paranoid](https://mise.jdx.dev/paranoid.html)

**Windows/PowerShell.** pwsh 7+ fully supported; PowerShell 5.1 lacks `LocationChangedEventArgs`, so mise recommends upgrading. mise **wraps** the existing `prompt` function (saves as `$__mise_pwsh_previous_prompt_function`, calls `_mise_hook` first, then delegates), separately registers a `LocationChangedEventArgs` handler for cd-without-prompt, optionally `CommandLookupEventArgs` for command-not-found auto-install. `_mise_hook` pipes `mise hook-env` to `Invoke-Expression`. [`src/shell/pwsh.rs`](https://github.com/jdx/mise/blob/main/src/shell/pwsh.rs), [PowerShell discussion](https://github.com/jdx/mise/discussions/6733)

**Nushell.** No `eval` — activation is **static-file generation**: `mise activate nu | save mise.nu --force`, then `use`d from `config.nu`. [installing-mise](https://mise.jdx.dev/installing-mise.html)

**Adoption.** ~27k+ GitHub stars, overtook asdf in early 2026, 10th most-downloaded Homebrew formula; explicitly positions to replace **both** asdf and direnv. [discussion #7967](https://github.com/jdx/mise/discussions/7967)

## 2. direnv

**Execution model.** `.envrc` loaded into a **bash sub-process** (stdlib + direnvrc + `.envrc`); only the **diff** against the outer shell is exported back. `direnv export SHELL` renders the diff per shell. [direnv.1](https://direnv.net/man/direnv.1.html)

**Diff shape — untyped.** `EnvDiff{Prev, Next map[string]string}` — two flat before/after snapshots; add/remove **inferred at render time**, never stored as a typed operation. [`internal/cmd/env_diff.go`](https://github.com/direnv/direnv/blob/master/internal/cmd/env_diff.go)

**Staleness bookkeeping.** `DIRENV_DIFF` (base64(gzip(JSON))) undoes the last-applied change before applying a new one; `DIRENV_WATCHES` tracks `watch_file` mtimes; `DIRENV_DIR` records the active directory. `DIRENV_*` hidden from the printed diff.

**Allow/deny — content-hashed, stronger than mise default.** `~/.local/share/direnv/allow`; filename = **SHA256 of (absolute `.envrc` path + newline + file contents)**. Editing after allowing re-blocks until `direnv allow`. `direnv.toml [whitelist]` supports `prefix`/`exact` pre-trust with an explicit code-execution warning. [allow storage](https://github.com/orgs/direnv/discussions/1092), [direnv.toml](https://direnv.net/man/direnv.toml.1.html)

**stdlib.** `PATH_add` prepends (explicitly to avoid the "PATH gets replaced" footgun); `source_env`/`source_up` **bypass the security framework** (an allowed `.envrc` can source an unapproved file); `watch_file` adds reload triggers. [stdlib](https://direnv.net/man/direnv-stdlib.1.html)

**Pitfalls.** PATH replaced instead of augmented ([#82](https://github.com/direnv/direnv/issues/82), [#1249](https://github.com/direnv/direnv/issues/1249)); `PATH_add` slow (per-entry subshell, [#671](https://github.com/direnv/direnv/issues/671)) and can hang indefinitely ([#1248](https://github.com/direnv/direnv/issues/1248)); no true enter/exit, only diffing — leave-cleanup best-effort ([#798](https://github.com/direnv/direnv/issues/798)); git-bash PATH separator corruption ([#796](https://github.com/direnv/direnv/issues/796), [#253](https://github.com/direnv/direnv/issues/253)).

**PowerShell/Nushell.** Both supported; nushell ≥0.104 hooks `env_change.PWD` (recommended over `pre_prompt`). [nushell cookbook](https://www.nushell.sh/cookbook/direnv.html)

## 3. Volta

Rust-compiled shim binaries in `~/.volta/bin` / `%LOCALAPPDATA%\Volta\bin`. macOS/Linux: each shim is a **symlink to one shared `volta-shim` binary**, argv0 dispatch. Every invocation walks up from CWD for `package.json#volta`, resolves the pin, execs the real binary. Widely reported **~1ms per shim resolution**; no shell hook at all — staleness isn't a concept. Windows: native `.exe` shims (not `.cmd`). **Trust model: none** — a cloned repo silently drives what runs, zero prompt. [how-it-works](https://www.voltajs.com/guide/how-it-works.html), [Windows shims](https://github.com/volta-cli/volta/issues/176), [markhughes.dev](https://www.markhughes.dev/how-to-use-volta-and-why-you-should-consider-it-over-nvm/)

## 4. proto (moonrepo)

**Hybrid shim/bin split (v0.20).** `~/.proto/shims` = thin wrapper via `proto run` (runtime detection + auto-install, admitted **"upwards of 10x slower"**); `~/.proto/bin` = plain symlinks to the resolved native binary. PATH order `shims:bin`. `proto activate` = shell-hook (bash/zsh `eval`, fish `| source`, pwsh `Invoke-Expression`, nu/elvish via generated+sourced file); v0.50+ `--no-init` defers first activation to first cd/prompt. `.prototools` detection: CLI arg > `PROTO_<TOOL>_VERSION` > local `.prototools` (upward walk, stops at `$HOME`) > global pin > failure. **Windows retrospective:** old `.cmd` shims "cause weird and unexpected problems when an environment expects a real executable" — admitted mistake. [proto-v0.20](https://moonrepo.dev/blog/proto-v0.20), [activate](https://moonrepo.dev/docs/proto/commands/activate), [detection](https://moonrepo.dev/docs/proto/detection)

## 5. scoop

**`current` = directory junction, deliberately.** Per-version install dirs; `current` junction repoints on update. Junctions chosen because **plain symlinks require admin/Developer Mode** on Windows; junctions don't ([PR #3970](https://github.com/ScoopInstaller/Scoop/pull/3970) to switch exists, junctions remain default). Closest one-to-one precedent for a farm dir. Shims: `<name>.exe` (compiled dispatcher) + `<name>.shim` (plaintext sidecar) in one shared `shims/` dir — the shape OCX already adopted. Repair: `scoop reset` reconstructs shims + junction, documented imperfect ([#6316](https://github.com/ScoopInstaller/Scoop/issues/6316)); junctions have own fragility — inaccessible over SSH after a Windows KB ([#6594](https://github.com/ScoopInstaller/Scoop/issues/6594)). [Current-Version-Alias wiki](https://github.com/lukesampson/scoop/wiki/The-'Current'-Version-Alias)

## 6. pyenv / rbenv / asdf

Shared shape: generic shim per tool executable → central resolver.

- **pyenv**: shims = hard-linked copies of one prototype bash script, re-exec `pyenv exec "$0" "$@"`. `.pyenv-shim` temp file doubles as rehash lock; crash/race leaves it behind, wedging every shim — still-open [#2829](https://github.com/pyenv/pyenv/issues/2829), [#2893](https://github.com/pyenv/pyenv/issues/2893).
- **rbenv**: auto-rehash opt-in via plugin — out-of-the-box "installed a gem, binary not found until rehash".
- **asdf**: maintainer-benchmarked **~120–150ms per shim invocation** (bash); 2025 Go rewrite → ~20ms exec, reshim 20s→<1s; `asdf current` still ~330ms, "not fast enough for a shell prompt". [asdf-performance](http://stratus3d.com/blog/2022/08/11/asdf-performance/), [2025 update](http://stratus3d.com/blog/2025/05/02/asdf-performance-improvements/)

**Why mise doesn't default to shims** — a *model* problem: "the real problem is the shim design [itself]". PATH/activate mode has **zero marginal per-exec cost** at the price of a small fixed per-prompt tax; *any* shim design pays per invocation, including subprocess fan-out in builds. [comparison-to-asdf](https://mise.jdx.dev/dev-tools/comparison-to-asdf.html)

## 7. Homebrew / SDKMAN / update-alternatives

- **Homebrew**: immutable Cellar payloads; version-stable `opt/<formula>` symlink is the hardcode-me path — only its *target* moves on upgrade. Repo-wide "current" alias declined repeatedly ([#4947](https://github.com/orgs/Homebrew/discussions/4947), [#8699](https://github.com/Homebrew/brew/issues/8699)).
- **SDKMAN**: `candidates/<tool>/current` plain symlink; on Windows/git-bash fails "Permission denied" without Developer Mode/elevation ([#610](https://github.com/sdkman/sdkman-cli/issues/610), [#1250](https://github.com/sdkman/sdkman-cli/issues/1250)) — the plain-symlink-needs-elevation problem Scoop's junctions dodge.
- **update-alternatives**: public name (`/usr/bin/editor`) → `/etc/alternatives/` → real target; priority + auto/manual state separate. Conceptual ancestor of every stable-path-that-repoints design. [update-alternatives.1](https://manpages.debian.org/bookworm/dpkg/update-alternatives.1.en.html)

## 8. PowerShell + Nushell hook conventions

**PowerShell has no precmd/preexec framework** — the only extension point is the global `prompt` function (must return string-like or PS silently falls back to `PS>`). Every surveyed tool **wraps** the existing `prompt` (save-then-call-through) so multiple tools layer. mise adds `LocationChangedEventArgs` (**PS 7+ only**) and optional `CommandLookupEventArgs`. Starship confirms the constraint (binds PSReadLine Enter-handler for transient prompt, lacking anything lower-level). [about_Prompts](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_prompts), [starship details](https://deepwiki.com/starship/starship/3.2.2-shell-specific-details)

**Nushell**: real hook system (`pre_prompt`, `pre_execution`, `env_change`, `display_output`, `command_not_found`), all **REPL-only**. Hook `env_change.PWD` over `pre_prompt` (efficiency). **No `eval`** → activation must be a **static generated file**, regenerated (not re-evaluated) whenever hook logic changes. [nushell hooks](https://www.nushell.sh/book/hooks.html)

---

## (a) Comparison Table

| Tool | Farm-dir mechanism | Shell reconciler | Staleness detection | Windows story | Trust gate | Diff shape |
|---|---|---|---|---|---|---|
| **mise** | shim = symlink to mise binary, argv0 dispatch | prompt-hook every shell + pwsh wrap/event, static nu file | mtime of configs/watch_files | pwsh 7+ full; 5.1 degraded | path-hash (SipHash); content-hash only paranoid; trust-order CVE | **Typed**: `Add/Change/Remove`, msgpack+zlib+b64 |
| **direnv** | none | bash-subshell capture → diff → export/unset | `DIRENV_DIFF`/`DIRENV_WATCHES` | later addition; POSIX-shaped `.envrc` | path+content SHA256; re-blocks on edit | **Untyped**: `{Prev,Next}` maps |
| **volta** | compiled shims, argv0, per-exec `package.json` walk | none — stateless per-exec | n/a | native `.exe` shims | none | n/a |
| **proto** | `bin/` (fast symlink) + `shims/` (slow wrapper) | shell-hook activate | re-reads `.prototools` | `.cmd` shims = admitted mistake | none | n/a |
| **scoop** | `current/` **junction** per app | none | n/a | junction dodges admin/Dev-Mode | none | n/a |
| **pyenv/rbenv** | hard-linked prototype shims | PATH-prefix only | manual/hooked rehash; stale lock wedge | not primary | none | n/a |
| **asdf** | same, Go rewrite 2025 | PATH-prefix only | manual/hooked reshim | not primary | none | n/a |
| **Homebrew** | `opt/<formula>` stable symlink | none | n/a | n/a | none | n/a |
| **SDKMAN** | `current` plain symlink | none | n/a | fails without Dev-Mode | none | n/a |
| **update-alternatives** | two-hop symlink + priority db | none | n/a | n/a | root-only | n/a |

## (b) Pitfalls to Design Against

1. **Path-hashed trust ≠ content-hashed trust** — mise default trust keyed by path; edit of trusted config doesn't re-prompt; ordering gap produced [GHSA-436v-8fw5-4mj8](https://github.com/jdx/mise/security/advisories/GHSA-436v-8fw5-4mj8). Evaluate consent **before** loading anything config-affecting from the untrusted file.
2. **Hash bookkeeping fragile on its own terms** — mise paranoid hash filename collisions ([#4499](https://github.com/jdx/mise/discussions/4499)).
3. **Value-diff without provenance breaks under concurrent third-party mutation** — direnv stores before/after values, no per-entry ownership ([#82](https://github.com/direnv/direnv/issues/82), [#1249](https://github.com/direnv/direnv/issues/1249)).
4. **Snapshot-diff ≠ real enter/exit — cleanup best-effort** ([direnv #798](https://github.com/direnv/direnv/issues/798), [mise hooks](https://mise.jdx.dev/hooks.html)).
5. **Stale lock files after crash/race wedge everything** — pyenv rehash lock ([#2829](https://github.com/pyenv/pyenv/issues/2829), [#2893](https://github.com/pyenv/pyenv/issues/2893)).
6. **Global-install invisibility** — rbenv/asdf/pyenv shim dirs miss self-installed binaries until reshim; the most-repeated complaint of the shim lineage.
7. **Naive symlink swap is not atomic.** `ln -sf` = `unlink()`+`symlink()` — race window; only `rename()` on the same filesystem is an atomic entry flip (Nix profile switches rely on this). **Concrete farm implementation requirement.** [atomic-symlinks](https://blog.moertel.com/posts/2005-08-22-how-to-change-symlinks-atomically.html), [Nix profiles](https://nix.dev/manual/nix/2.34/package-management/profiles)
8. **Junction vs symlink on Windows is a privilege decision** — SDKMAN fails without Dev-Mode; Scoop's junctions dodge it but have own fragility ([#6594](https://github.com/ScoopInstaller/Scoop/issues/6594)).
9. **`.cmd` shims break real-executable assumptions** — proto's own retrospective; reinforces OCX's existing `.exe`-shim ADR. [proto-v0.20](https://moonrepo.dev/blog/proto-v0.20)
10. **Nushell no `eval`** — hook-logic changes require regenerating a static file; bake regeneration into `self setup`/`self update`. [nushell hooks](https://www.nushell.sh/book/hooks.html)
11. **PowerShell one global `prompt` slot** — wrap, never clobber; PS 5.1 lacks mise's cd-event type. [about_Prompts](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_prompts)
12. **Per-mutation subshells can hang, not just crawl** — direnv `PATH_add` ([#671](https://github.com/direnv/direnv/issues/671), [#1248](https://github.com/direnv/direnv/issues/1248)).

## (c) Typed/Semantic Revert — Novelty Assessment

- **direnv: no** — flat value snapshots, ops inferred at render.
- **mise: closest prior art, genuinely typed — but a value diff, not a provenance diff.** `EnvDiffOperation` proves "typed diff over byte snapshot" is a shipping, validated pattern. But it stores old/new value pairs, never *which package contributed the entry*; its `__MISE_ORIG_PATH` PATH-ownership heuristic is a hand-rolled patch for exactly this gap, and non-PATH `[env]` vars get no equivalent.
- **No surveyed tool tags entries by origin (content-hash/package identity).** All revert models are "restore last-known-old value" — a two-writer assumption.
- **Farm half:** mise/direnv persist no stable PATH farm; volta/proto/scoop/SDKMAN/Homebrew/update-alternatives are the farm lineage; Nix is the atomicity precedent. **No surveyed tool combines a typed shell-env reconciler with a persistent, atomically-repointed launcher farm** — two individually well-precedented lineages; OCX's combination is the novelty, not the primitives.

## Industry Context & Trends

- **Trending:** mise — consolidating asdf's and direnv's niches; its typed-diff + prompt-hook architecture is the most mature reference for the reconciler half.
- **Established:** direnv (content-hash trust gate = reference implementation), Homebrew `opt/`, update-alternatives, pyenv/rbenv (huge base, architecturally frozen).
- **Emerging:** proto — small adoption, but its public retrospectives read as a pre-written pitfalls list.
- **Declining:** asdf (defensive Go rewrite; still not prompt-safe). Volta's zero-prompt trust model is a trend to design *against* given OCX's security-conscious CI positioning.
