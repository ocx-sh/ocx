# ADR: Toolchain Activation — Rendered Homes, Launcher Trampolines, and Session PATH

## Metadata

- **Status**: Accepted 2026-09-05 — all open questions resolved (§ *Decided 2026-09-05*); the owner directed implementation the same day, under the autonomous-mode mandate, which is the acceptance
- **Review passes**: Round 1 — spec, quality-adversarial and security (Opus) plus a SOTA-gap pass (Sonnet); then an Opus re-validation of the corrected text. **A cross-model gate then ran once against the finished artifacts (Codex `sol`, one-shot, no looping).** It found two Blocks the same-family panel had missed and three lesser items: the render stamp authenticated `bin/` but not the `<group>/<entry>` links that select which package a trampoline dispatches; round 1's own H9 fix would have made the reconciler **delete** the session-PATH registration it had just written, because `repair_owned_segments` treats `$OCX_HOME` as a deletion authority; the macOS LaunchAgent froze a PATH snapshot at setup and tore the whole variable down on removal; `toolchain-dir`'s ownership check said nothing about writable ancestors; and the work-package DAG let the renderer start before the config types it compiles against existed. All five are applied below and each is marked *(cross-model gate)* at its site.
- **Date**: 2026-09-04
- **Deciders**: Owner + Principal Architect session (decisions D1–D8 converged in discussion and are owner-approved; recorded, structured and stress-tested here)
- **GitHub Issues**: [#359](https://github.com/ocx-sh/ocx/issues/359) (hookless shims mode — **closed by this ADR**: its second reopening signal, "an environment where the prompt hook cannot be installed becomes a target", is this record's trigger, and `activate = "bin"` plus trampolines on the session PATH is that mode; `env` stays the default by measurement, § *Context*), [#189](https://github.com/ocx-sh/ocx/issues/189) (stable links from a toolchain — delivered by the predecessor ADR, whose layout this one amends), [#170](https://github.com/ocx-sh/ocx/issues/170) (native project-toolchain hook, closed by the shipped reconciler — the case this ADR completes for shells that never run a hook)
- **Tech Strategy**: ☑ aligned (Rust 2024; no new crates — `windows-sys` and `junction` are already workspace dependencies, `windows-sys` needs three additional features: `Win32_System_Registry` and `Win32_UI_WindowsAndMessaging` for the session-PATH writer, `Win32_System_Environment` for the shim's `SetEnvironmentVariableW` strip, none of them in the current list at `Cargo.toml:228-236`)
- **Domain Tags**: package-manager, file-structure, cli, config, windows, security
- **Amends**: [`adr_project_toolchain_links.md`](./adr_project_toolchain_links.md) — layout (`bin/` moves inside `toolchain/`), config (`[toolchain] dir`/`links` replaced by `pinned` + root-level `toolchain-dir`), consumer-matrix row 3, and the render trigger. That ADR stays **Proposed**; the two are approved and implemented together.
- **Joins**: [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md) — the shipped per-prompt reconciler. Independent tracks: this ADR adds `owned_prefixes` entries and one new import mode, and changes none of the reconciler's decisions, ledger, consent model or commands.
- **Related**: [`adr_declared_binaries_metadata.md`](./adr_declared_binaries_metadata.md) (the `binaries` claim that makes names knowable before pull), [`adr_lazy_package_loading.md`](./adr_lazy_package_loading.md) (the launcher/shim producer this reuses), [`adr_windows_exe_shim.md`](./adr_windows_exe_shim.md) (the `.shim`/`.shimref` sidecar grammar — **this record adds a third sidecar, `.exec`, on the same five shared read rules**, § *Trampoline contract*), [`adr_self_setup.md`](./adr_self_setup.md) (the profile writer session PATH extends), [`adr_two_env_composition.md`](./adr_two_env_composition.md) (the surface algebra the name set reuses).

### Reconciliation with `handshake_toolchain_cli.md` §4 — **confirmed by the owner, 2026-09-05**

The signed handshake states (`handshake_toolchain_cli.md:204`) that there is "no stale per-tool static render, no `$OCX_HOME/init.*`". That clause binds the **env-activation mechanism**: it replaced a per-tool static file, written once at install time and read as the environment, with `eval "$(ocx --global env --shell=sh)"` at shell start. A trampoline is not an env render. It carries no composed environment in its bytes: it re-enters **`ocx exec` against its own home** on every invocation (D2, as rewritten by D-9), so the environment it runs in is composed from that home's `ocx.toml` and lock at the moment of the call — the same composition `eval "$(ocx env --shell=sh)"` performs, reached by a different door. It is healed or re-rendered on every path that would put it on PATH. Nothing about it can go stale in the way `$OCX_HOME/init.<shell>` could.

This ADR therefore does not contradict §4 — but the reading is stated here explicitly rather than asserted by omission, because the handshake is a signed record and the surface it names is adjacent. **The owner confirmed this reading on 2026-09-05** (§ *Decided*, D-5).

## Context

The shipped per-prompt reconciler (`adr_shell_env_overhaul.md`) puts a composed toolchain into an interactive shell correctly and cheaply. Everything that is not an interactive shell is unserved:

- **Corporate rollout on Windows and on GUI desktops.** A user who installs ocx and never opens a terminal has no ocx on PATH at all. Nothing ocx writes today touches a session-level PATH — there is no `HKCU\Environment` write, no `~/.config/environment.d/` file and no macOS LaunchAgent anywhere in the tree.
- **The clean-shell coworker.** A contributor who wants the project's tools on PATH but refuses a per-prompt hook that also exports `JAVA_HOME`, `GOROOT` and every package-declared variable has no middle setting. Today the choice is the whole envelope or nothing.
- **Hookless IDE and CI.** An IDE process started before (or outside) any shell never re-runs the hook, and a CI step's env does not survive to the next step without `--ci`. mise's own IDE-integration documentation states the same conclusion for the same reason: hook mode "cannot be relied on" once an IDE process has started, so "IDEs work better with shims."
- **Measured cost of the alternative.** [#359](https://github.com/ocx-sh/ocx/issues/359) asks whether a shim directory should replace per-prompt reconciliation outright. The reconcile no-op path measures **4.8 ms quiet / 7.6 ms in CI**, which is cheap enough that replacement is not the argument — coverage is.

The predecessor ADR ([#189](https://github.com/ocx-sh/ocx/issues/189)) already establishes a per-home link tree that makes composed paths stable across version bumps. What it does not do is put anything on PATH by itself: its consumer matrix row 3 ("plain `bin/` binaries → linked bin dir") names a directory nobody renders and nothing populates. This ADR renders it, decides what goes in it, and decides how it reaches a PATH that no shell hook wrote.

## Decision Drivers

- **DD1 — one home, one rendered tree.** Every toolchain (global `$OCX_HOME`, project `<project>/.ocx/`) is a *home*; whatever a home exposes is derived state under that home, regenerated from the lock, never hand-maintained.
- **DD2 — the hook stays the best answer where it runs.** Trampolines are the fallback for contexts the hook cannot reach, never a competitor to it.
- **DD3 — no new mechanism where one exists.** The launcher generator, the sidecar grammar's five shared read rules, the surface algebra, the ladder shape, the state-stamp root and the RC-block state machine are all shipped. This work extends them. **One disclosed addition**: D-9 makes the trampoline's verb `ocx exec`, which neither shipped sidecar can express, so Windows gains a third sidecar extension on the same shared rules (§ *Trampoline contract*).
- **DD4 — the consent model is not widened.** `ocx pull`, `ocx env`, `ocx exec` and a trampoline invocation are explicit user acts and stay ungated, exactly as today. A trampoline **records** consent exactly as `ocx exec` does and gates on nothing (D2, *Consequences*). Only the ambient per-prompt reconciler is consent-gated, and this ADR does not move that line.
- **DD5 — corporate operability.** A fleet operator must be able to set placement and activation mode from a `config.toml` tier they already control, without re-provisioning hosts.
- **DD6 — never block.** Render failure degrades to "compose digest paths, exit 0"; a missing trampoline means a name is not on PATH until the next render, never a failed command.
  - **One stated asymmetry** *(restated for D-9)*. The **trampoline** path self-heals: it re-enters `ocx exec`, whose composition materializes on miss (`Materialization::Install`, `crates/ocx_cli/src/command/toolchain_exec.rs:227-229`), so a package `ocx clean --force` collected is re-fetched rather than failing. What does not self-heal is the **dereference** consumer — `--force` bypasses the project registry entirely (`tasks/clean.rs:165` and its surrounding comment), so a `<group>/<entry>` link value already exported into a live shell (a `JAVA_HOME`) points at a collected root until the next composing trigger re-installs and re-heals. The hint names `ocx pull`. No new machinery: re-rendering is what `pull` already does.
- **DD7 — export/execute parity.** `ocx env` and `ocx exec` compose against one oracle in both lanes. A trampoline is a third consumer of the same composition, not a fourth semantics — and under D-9 that is **literally true rather than approximately true**: the trampoline's body *is* a call to `ocx exec`, so alignment is by shared code path, not by a flag the renderer must remember to bake. Validation item 11 is the oracle.

## Industry Context & Research

Three research artifacts, all dated 2026-09-04:

- [`research_toolchain_activation_competitive.md`](./research_toolchain_activation_competitive.md) — name-keyed bin/shim directories across rustup, mise, asdf, proto, Volta, aqua, scoop, Homebrew, Nix, pipx, `uv tool`, pkgx.
- [`research_toolchain_activation_shell_session.md`](./research_toolchain_activation_shell_session.md) — session PATH visibility for GUI apps per platform, and POSIX trampoline self-location.
- [`research_toolchain_activation_security.md`](./research_toolchain_activation_security.md) — threat model against trampolines, session PATH, link trees and installer writes.

**Trending approaches.** Every actively maintained tool surveyed ships *both* a hook/activate mode and a bin/shim mode, and documents when to use which. None frames them as alternatives. mise (Thoughtworks "Adopt", ~397k monthly active users) is the closest prior art to this ADR's `activate = env | bin`; its two modes map almost 1:1.

**Key insight.** A generated launcher that re-resolves its target at exec time — rather than a static symlink baked at install time — is the field's converged design (rustup's argv0-dispatch proxies, mise/asdf/proto shims, aqua-proxy's hardlink dispatch, scoop's `shim.exe` + `.shim` sidecar). D1/D2 land squarely inside it rather than inventing anything.

**Two further prior-art points added in review:**

- [**pixi**](https://pixi.prefix.dev/latest/global_tools/trampolines/) ships exactly this shape for its global tools — a per-name trampoline that reads a sibling manifest and execs the real binary — and its [issue #2462](https://github.com/prefix-dev/pixi/issues/2462) (hardlink-count failure on EFS) is direct corroboration for keeping the POSIX arm a **script** rather than a hardlinked binary, as D2 does. The Windows arm keeps the hardlink because `ShimBinStore` already owns that publish and Windows has no script equivalent.
- [**winget**](https://github.com/microsoft/winget-cli/discussions/5720) puts `%LocalAppData%\Microsoft\WinGet\Links` on the user PATH as a flat, non-cwd-aware global directory — another member of the static-global-dir camp below, and the closest Windows-native precedent for D5's registry write.

**The one axis where the field splits, and where this design deliberately sits.** Two camps:

| Camp | Members | Model |
|---|---|---|
| **cwd-aware global shim dir** | mise, asdf, proto, Volta | One global shim dir on PATH; each shim walks up from cwd for a config file on **every exec** and resolves the project's version there. There is no separate project bin dir — the global one *is* project-aware by construction. |
| **static global bin dir, project scoping elsewhere** | rustup (`~/.cargo/bin`), Homebrew prefix `bin/`, Nix profile `bin/`, pipx / `uv tool` (`~/.local/bin`), winget (`WinGet\Links`) | One flat, non-cwd-aware global bin dir; a *separate* mechanism (rustup's `rust-toolchain.toml` inside the toolchain-selection layer, direnv, `nix develop`) handles per-project selection. |

**D8 puts ocx in the second camp, as a consequence of the two-tier home model rather than an oversight.** The polyglot version managers build cwd-awareness into the shim because they have exactly one shim dir and it must serve every project. ocx's project tier already gets its own physical `.ocx/toolchain/bin`, so the problem those tools solve inside the shim does not exist here: which toolchain applies is a *directory-selection* question, not a *version-selection* question inside one shim.

**Corroborating detail folded into the decisions below:**

- Installer PATH handling is platform-forked exactly as D5 assumes: rustup writes `HKEY_CURRENT_USER\Environment` on Windows and appends profile sourcing on Unix. Solved and low-risk — with four specific correctness traps (§ *Session PATH*).
- Collision handling across the field is uniformly "last/highest wins silently", never a hard error. scoop is the outlier and gets it wrong (silent cross-app overwrite, unresolved since [ScoopInstaller/Scoop#1261](https://github.com/ScoopInstaller/Scoop/issues/1261)). D2's last-walked-wins matches the disciplined majority.
- "Shims are catastrophically slow" is 2024-era evidence. asdf's Feb-2025 Go rewrite cut per-exec overhead from ~120 ms to ~20 ms. scoop's published `shim.exe` numbers (+24 ms C# / +103 ms Rust versus direct exec) are the working expectation band for a native Windows launcher.
- **`ocx`'s "consent before parse" ordering already defends the exact class that produced CVE-2026-35533** (mise, CVSS 7.7, fixed 2026.6.4). `adr_shell_env_overhaul.md` Decision 4 states the fix as a rule; this ADR reads no project bytes before that point, so the property is inherited unchanged.
- The sidecar design (32 KiB cap, single-line byte-exact grammar, digest-pinned identifiers, canonicalized component-wise containment, reader re-validates independently of the writer) is **stronger than every shim mechanism surveyed**. Its five shared read rules are reused verbatim by the new `.exec` sidecar; nothing is adopted from the prior art.

## Considered Options

Weights reflect the drivers: reaching hookless consumers is the reason the work exists; the dereference class is the one thing only a stable directory can serve; security and operability are the corporate-rollout gates.

| Criterion (weight) | A — cwd-aware global shim dir | B — per-home rendered toolchain + trampolines **(chosen)** | C — symlink-to-real-binary bin dir | D — hook-only + `ocx exec` (status quo) |
|---|---|---|---|---|
| Reaches hookless consumers: IDE, GUI, CI, clean shell (**5**) | 5 | 4 | 2 | 1 |
| Serves the dereference class (`JAVA_HOME`, SDK homes) (**4**) | 5 | 5 | 3 | 2 |
| Per-exec cost (**3**) | 2 | 4 | 5 | 5 |
| Corporate / managed-tier operability (**4**) | 4 | 5 | 3 | 1 |
| Security surface added (**4**) | 2 | 4 | 2 | 5 |
| Implementation cost — new mechanisms introduced (**3**) | 2 | 3 | 4 | 5 |
| Reversibility (**2**) | 3 | 4 | 4 | 5 |
| **Weighted total** | **87** | **105** | **77** | **77** |

Option A's dereference row was rescored from 1 to 5 in review: A keeps the `<group>/<entry>` link tree exactly as B does — cwd-awareness is a property of the *shim*, not of the link tree — so it serves `JAVA_HOME` just as well. **B still wins by 18**, and it wins on the axes A is actually weak on: per-exec cost (a full compose on every invocation against ocx's measured ≈21 ms recompose) and security (a cwd walk means the directory you `cd` into decides what `cmake` resolves to, which is the consent boundary D4 holds).

### Option A — cwd-aware global shim dir (mise / asdf / proto / Volta)

One `$OCX_HOME/toolchain/bin` on PATH; each trampoline walks up from cwd for an `ocx.toml`, resolves that project's lock, and execs.

| Pros | Cons |
|---|---|
| A single PATH entry serves every project with no per-project PATH management at all — the best hookless coverage of any option | Every exec pays a cwd walk plus a full compose. ocx's project recompose measures ≈21 ms; a build fanning out thousands of tool invocations multiplies that |
| Keeps the link tree, so the dereference class is served identically to B | **Consent inversion.** A cwd-walking trampoline would have to evaluate consent inside every exec, or bypass it — and bypassing it means `cd`-ing into a clone changes what `cmake` resolves to |
| Matches the most popular tools in the space | Deferred as a future capability, not rejected outright (see *Deferred*) |

### Option B — per-home rendered toolchain with launcher trampolines **(chosen)**

`<home>/toolchain/` holds `bin/<name>` trampolines for the default group and `<group>/<entry>/` links to package roots. A trampoline re-enters `ocx exec` against its own home, so it runs in the composed toolchain environment (D2, as decided by D-9); links serve the dereference class.

| Pros | Cons |
|---|---|
| One tree answers all three consumer classes | A project's `toolchain/bin` still needs *some* channel to reach PATH — hook, IDE workspace setting, `$GITHUB_PATH`, or `.envrc` |
| Reuses the shipped launcher generator, the sidecar grammar's shared rules, the surface algebra, the ladder shape and the state-stamp root; one new store field, one new launcher body, one new sidecar extension | Trampoline `bin/` is a whole-directory reconcile — stale names must be pruned, which is a delete path in derived state |
| Zero per-exec cost on the common path: an activated shell's real bin dirs shadow the session-PATH trampoline dir entirely | Every invocation that does reach a trampoline pays one full composition — the environment is read from `ocx.toml` and the lock, never baked (D2) |
| `activate` and `pinned` are ordinary `ocx.toml` keys read at re-entry, so a config edit takes effect with no re-render; `toolchain-dir` is settable from any `config.toml` tier including `[managed]` | Both arms bake an absolute **home selector**, so the tree is position-dependent on both platforms — `toolchain-dir` moves a home away from `<project>/.ocx/`, so the selector cannot be derived from `$0`. Moving a home is fixed by re-rendering |
| Names come from the user's own lock | A dependency's `binaries` claim can contribute a name the user never typed — deliberate (§ *Decided*, D-1) and accepted as R4 |

### Option C — symlink-to-real-binary bin dir (Homebrew / Nix profile)

`bin/<name>` is a symlink straight to `packages/<digest>/content/bin/<name>`; no launcher, no per-exec logic.

| Pros | Cons |
|---|---|
| Zero per-exec overhead — the cheapest possible dispatch | **Carries no environment.** A tool needing `JAVA_HOME`, a dependency's `lib/` on `LD_LIBRARY_PATH`, or a patched env resolves to a broken binary. Clean-env execution is Principle #4 of the product |
| Simplest to implement | Still needs the `<group>/<entry>` link tree anyway for the dereference class, so it removes no machinery |

**Its security score of 2 is not the Windows privilege point.** It is the surface: per-**file** symlinks written into a checkout multiply the link-following and TOCTOU surface by the number of exposed names, against a link tree whose entries are directories with one containment rule each — and, unlike a trampoline, a symlink hands the target no exec-time re-validation seam at all. The Windows privilege requirement (file symlinks need privilege; junctions are directory-only) is a **portability** defect and is scored in the NFR table's *Portability* row, not here. It was the ground on which the predecessor ADR already rejected this shape as its Option B.

### Option D — hook-only, wrap everything in `ocx exec` (status quo)

| Pros | Cons |
|---|---|
| Nothing new to build, nothing new to attack | Every motivating case stays unserved: GUI apps, corporate Windows rollout, hookless IDE, clean-shell coworker |
| Per-exec cost is zero | `ocx exec -- <tool>` is a wrapper the user must remember at every call site; it does not reach a process someone else's tooling spawns |

### Recommendation

**Option B.** The only option that serves all three consumer classes at once, the only one compatible with the consent model as it stands, and it costs one new store field plus one new launcher body because every other part already exists. Option A is the natural successor once a cwd-aware answer is wanted, and is explicitly deferred rather than rejected.

## Decision Outcome

D1–D8 are **owner-approved** and are recorded here, each with the alternative it displaced and what it costs.

### D1 — Layout: every toolchain has a home, and the home renders a tree

Every toolchain has a *home*: `$OCX_HOME` (global) or `<project>/.ocx/` (project). The rendered toolchain lives at `<home>/toolchain/`.

*Alternatives considered:* a single flattened farm keyed by project hash under `$OCX_HOME` (rejected in the predecessor ADR); rendering into a shared machine-level bin dir for both tiers (rejected — a project's derived state in the machine home is the containment failure D2 of the predecessor ADR exists to avoid).

*Consequences:* one tree per home, self-gitignored, deletable, regenerated. `.ocx/index/` untouched. The tree sits inside an attacker-controlled repository, which is why heal-before-emit (predecessor ADR) and the render stamp (§ *Render and heal triggers*) are load-bearing.

### D2 — A trampoline runs in the configured toolchain environment

*(Rewritten 2026-09-05 — owner decision D-9, § Decided. The original D2 re-entered `ocx launcher exec` against the `<group>/<entry>` link, so a trampoline carried the per-package closure env only and project `[env]` did not ride it.)*

A trampoline re-enters **`ocx exec` against its own home**. The environment it runs in is therefore the composed toolchain environment — byte for byte what `ocx env` prints and what `ocx exec` already runs children in, project `[env]` and every sibling package's variables included. A trampoline is the **third consumer of one composition**, not a fourth semantics (DD7).

**The body, POSIX, and why each part is there:**

```
unset OCX_GLOBAL OCX_PROJECT
exec "${OCX_BINARY_PIN:-ocx}" --project '<abs project root>' exec -- "$(basename "$0")" "$@"
```

with `--global` in place of `--project '<…>'` for the global home.

- **The home selector is baked, single-quoted.** It cannot be derived from `$0`: `toolchain-dir` relocates a project's home to `<toolchain-dir>/<project-key>/toolchain`, so walking up from the trampoline reaches the keyed tree, never the project the tree describes. Single quotes keep the injection rule the shipped bodies already rely on (§ *Trampoline contract*).
- **The `unset` makes the baked selector the only selector — and it is the difference between working and exiting 64.** The chain is root-level `--global`/`OCX_GLOBAL` ▸ `--project`/`OCX_PROJECT` ▸ cwd walk (`crates/ocx_cli/src/app/project_context.rs:126`, `:275`, `:623`), and the two selectors are **mutually exclusive, including through the environment**: `check_global_project_exclusivity` (`crates/ocx_cli/src/app/context.rs:1217-1228`) rejects a set `OCX_GLOBAL` beside an explicit `--project` — and a set `OCX_PROJECT` beside `--global` — with `UsageError`, exit 64. So without the `unset`, a caller who happens to export `OCX_GLOBAL=1` makes **every project trampoline on their PATH fail** with a usage error about flags they never typed, and an exported `OCX_PROJECT` does the same to every global one. Stripping both first is what makes a trampoline's answer a function of its own home and nothing in the caller's environment. It is bound to that home, never to cwd (D8, unchanged).
- **`--project` and `--global` are root flags**, before the subcommand (`crates/ocx_cli/src/command/toolchain_exec.rs:479-484`), which is why the body spells them there.
- **The re-entry target is the toolchain-tier `ocx exec`** (`ToolchainExec`, `crates/ocx_cli/src/command/toolchain_exec.rs`) — never the OCI-tier `ocx package exec` in `command/exec.rs`, which takes package identifiers and reads no `ocx.toml`.

**Zero baked flags.** `pinned`, `lazy-mode`, group selection and project `[env]` are all read at re-entry, through exactly the ladders `ocx env` uses. Baking `--pinned` would freeze a render-time value into a file and make a trampoline disagree with `ocx env` the moment someone edits `ocx.toml` — precisely the staleness class this record exists to close. Group selection is not baked either: an empty selection expands to `DEFAULT_GROUP` (`toolchain_exec.rs:158-162`), which is the exposed set `bin/` renders. `activate` is not a composition input at all.

> *The owner asked whether the body should pass `ocx env --activate env [--pinned]`-style arguments.* **No.** Alignment is by construction: the trampoline calls the same command, which loads the same file through the same ladders, so there is nothing left for a flag to align. A flag would be a second place for the answer to live. Validation item 11 — `env` ≡ `exec` in both lanes under all three `activate` values — is the oracle that keeps them one.

**Extensibility, stated because it was asked.** The interface a trampoline depends on is `ocx exec`'s grammar plus `ocx.toml`; the sidecar and the body carry the minimum that grammar needs — a home selector and the tool stem. A future `expose = [groups]` key, or a per-hook / per-bin group configuration, is an `ocx.toml` change: it alters which names render and what `ocx exec` composes, while every sidecar byte and every trampoline body stays exactly as written. Only a future **per-group bin layout** — `<home>/toolchain/<group>/bin/` in place of one flat `bin/` — would need a new selector in the body, and that is a render-layout decision taken then, not a slot reserved now.

**The name set is metadata-declared, and dependencies are in.** The set is the `binaries` ∪ `entrypoints` claims of the default group's roots **and of every interface-admitted dependency** — exactly the shipped surface algebra, never a directory scan. The decision brief's original clause said "dependency binaries excluded"; the owner struck that on 2026-09-05 and restated the rule in the owner's own words (§ *Decided 2026-09-05*, D-1). § *Name set* below is the contract.

*Alternatives considered:* re-entering `ocx launcher exec` against the `<group>/<entry>` link — the original D2, struck by the owner: it gives a working binary with the environment its own package declares and nothing else, so a `mvn` trampoline runs without the `JAVA_HOME` the JDK beside it declares, and project `[env]` never rides; baking the composed session env into the trampoline (rejected — it goes stale immediately and would put project `[env]` on a global-tier PATH entry); a compiled per-name proxy binary on POSIX (rejected — the `env.*` shim precedent is a script for auditability, corroborated by pixi's EFS hardlink failure; a compiled dispatcher is a deferred optimization).

*Consequences:* a trampoline gives the same environment an activated shell gives, which is what makes `activate = "bin"` a coherent mode rather than a degraded one. Three follow-on obligations, each carried below: it costs **one full composition per invocation** — load `ocx.toml`, load the lock, compose — which is why the composed-env-cache trigger in *Deferred* is measured (§ *Non-Functional Requirements*, validation item 37); it makes command **resolution** a loop surface, so the composed lookup PATH must exclude both trampoline directories (validation item 22); and it makes a trampoline invocation a consent **recorder** exactly as `ocx exec` is, since `load_project_with_lock_consenting` (`crates/ocx_cli/src/app/project_context.rs:345`) stamps and never gates — consistent with DD4, which widens nothing.

### D3 — Configuration: two `ocx.toml` keys, one `config.toml` key

`activate` and `pinned` are toolchain-level `ocx.toml` keys beside `lazy-mode`. `toolchain-dir` is a `config.toml` root-level key available in every tier including `[managed]`.

*Alternatives considered:* `links`/`bin`/`stable-paths` booleans (rejected — three booleans encode six states badly); a `--global-env` flag (rejected); per-group `activate`/`pinned` (rejected — `lazy-report`'s precedent: a tier that cannot be read where it is resolved is a defect); project-level `toolchain-dir` (rejected in the predecessor ADR). Placing `activate` in `config.toml` instead is [Open question 3](#open-questions).

*Consequences:* `[shell] hook` keeps its own meaning and is untouched. The `[toolchain]` section the predecessor ADR proposed never ships.

### D4 — Materialization: `ocx pull` renders the home

`ocx pull` renders `<home>/toolchain/` always. `lazy-mode` governs *content*, never whether the tree exists. `add`, `remove`, `lock` and `update` re-render. Emitters heal before emit. A fresh clone reaches a working tree with `ocx pull`.

**Consent is not materialization.** `[shell.consent]`, `ocx shell allow` and subtree `/*` grants gate the per-prompt reconciler and nothing else.

*Alternatives considered:* rendering on first compose (rejected — makes a read path a writer); rendering on materialization state change (rejected — links target the package root in both states).

*Consequences:* this places ocx inside the norm every surveyed tool converges on — gate the ambient trigger, not the explicit command.

### D5 — Session PATH: registered by `ocx self setup`, per platform

`ocx self setup` registers **`ocx_install_bin_path`** — `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin`, resolved via the symlink store (`crates/ocx_cli/src/command/self_group/activate.rs:801-804`, hardcoded in the same shape at `crates/ocx_lib/src/setup/shims.rs:37`) — and `$OCX_HOME/toolchain/bin`, in that order, at **session** level. Only the global tier is registered; a project's `.ocx/toolchain/bin` never reaches a session PATH by ocx's own hand.

> **There is no `$OCX_HOME/bin`.** It does not exist on disk (`crates/ocx_lib/src/file_structure.rs:63-88` lists nine stores plus `locks`, none of them a `bin`), and `handshake_toolchain_cli.md:38` and `:263` deleted the name together with `UserBinStore` as part of the discarded user-wide detour. Every earlier draft of D1 and D5 that named it was wrong; `ocx_install_bin_path` is the correct spelling and is what `ocx self activate` already prepends.

*Alternatives considered:* registering the project tier too (rejected — it would let a cloned repository reach a machine-wide PATH); `/etc/paths.d` on macOS (rejected — login-shell-only, needs root, no effect on GUI apps); `~/.pam_environment` on Linux (rejected — deprecated since pam_env 1.5.0).

*Consequences:* `OCX_NO_MODIFY_PATH` applies and suppresses the whole arm. Ordering matters for more than tidiness — see D6's two-entry rule.

### D6 — Hook interplay: one new import mode, everything else unchanged

The reconciler imports each toolchain according to its `activate`. Ledger, three-way plan, consent evaluation and `ocx shell {state,allow,revoke}` are untouched.

**The global tier's two session directories are always desired, in every `activate` mode.** On the resulting PATH, `ocx_install_bin_path` precedes both trampoline directories and `$OCX_HOME/toolchain/bin` comes last of the three; in `bin` mode the project's `<home>/toolchain/bin` sits between them (§ *Hook import per `activate`*, validation item 9, and the design record's Flow C, which state the same order). They are session-level facts, not activation decisions — `activate = "none"` for the global toolchain still keeps them.

> **This revises round 1's H9, which was wrong** — and the cross-model pass caught it. `owned_prefixes` is `$OCX_HOME`, and `repair_owned_segments` (`crates/ocx_lib/src/shell/reconcile/plan.rs:627-673`) removes *every* PATH segment under an owned prefix that the desired set does not contribute (`contributed_elements`, `plan.rs:690`). `$OCX_HOME/toolchain/bin` sits under `$OCX_HOME`. So "never emitted in `env` mode" did not mean "left alone" — it meant the first reconcile **strips the session-PATH registration D5 had just written**, and every `→ none` transition subtracts it again. Keeping the two entries permanently desired is what makes D5 survive the hook.

**The project tier's `<home>/toolchain/bin` is emitted only in `bin` mode**, after the stamp and link-heal gates below. In `env` and `none` it is rendered and not emitted.

Ordering is load-bearing, not tidiness. A trampoline reached through the session PATH runs with no outer ocx, so `OCX_BINARY_PIN` is unset — it is set only by `Env::apply_ocx_config` (`crates/ocx_lib/src/env.rs:615`, from `cfg.self_exe`) when an outer ocx spawns the child — and the body's `${OCX_BINARY_PIN:-ocx}` falls back to a bare PATH lookup of `ocx`. `ocx_install_bin_path` first means that lookup can never resolve into a directory a repository controls (CWE-426).

*Alternatives considered:* making `bin` mode a superset of `env` mode (rejected — the clean-shell request is precisely *no variables*); excluding the global entries from the desired set in `env` mode (rejected — that was H9, and it silently deletes the session PATH, see the box above).

*Consequences:* the "exposed set" is today the default group, hardcoded (`crates/ocx_lib/src/activation.rs:569`, `DEFAULT_GROUP` from `crates/ocx_lib/src/project/internal.rs:16`). A future `expose = [groups]` key would feed `env` and `bin` alike. That door stays open; nothing is added for it now.

### D7 — Default `activate = env`, following — deliberate

Environment variables are why the hook exists: `JAVA_HOME` and SDK homes serve consumers *outside* the toolchain. Links make those values stable across upgrades. No behaviour breaks against today.

*Alternatives considered:* defaulting to `bin` (rejected — it silently drops every declared variable for every existing user); defaulting to `pinned = true` (rejected — discards the predecessor ADR's entire payoff).

### D8 — The global `bin/` composes the global toolchain only

No cwd walk. A project's `.ocx/toolchain/bin` reaches PATH through the hook (`activate = bin`), an IDE workspace setting, CI's `$GITHUB_PATH`, or `.envrc`'s `PATH_add` — **each of those recipes runs after `ocx pull`**, never before, because until the first render the directory holds whatever the repository committed.

**The hardcoded recipe is only valid while `toolchain-dir` is unset.** With `toolchain-dir` configured, a project's home is `<toolchain-dir>/<project-key>/toolchain`, which no integration can spell by hand. Integrations must query it: `ocx shell state` prints the resolved home, and under `--format json` that value is a machine-readable contract field (§ *CLI surface*). The `${workspaceFolder}/.ocx/toolchain/bin` form stays documented as the default-configuration shorthand, with the query named beside it.

*Alternatives considered:* Option A above.

*Consequences:* this is the rustup / Homebrew / Nix / winget camp. **One security residual**: an IDE or devcontainer feature putting a workspace `toolchain/bin` on a terminal's PATH does so entirely outside ocx's consent model (CWE-426). Recorded in the *Residual-risk register*.

---

## Component Contracts

Precise enough for `/hex-plan` to decompose without re-deriving anything.

### On-disk layout grammar

```
<home>/toolchain/
├── .gitignore              # "*" — written on first render, one code path for both tiers
├── bin/                    # trampolines for the DEFAULT group only (D2, D6)
│   ├── <name>              # POSIX: generated launcher script (0755)
│   ├── <name>.exe          # Windows: hardlink from ShimBinStore
│   └── <name>.exec         #   … + sidecar: one home selector, one line (D2)
└── <group>/                # "default" + every named group selected by -g
    └── <entry>/            # lock-entry name → link to $OCX_HOME/packages/<…digest root…>
                            #   dir symlink; NTFS junction on Windows, absolute target
```

**Store versus home — one type each, and they are not the same type.**

- **`ToolchainStore`** is the **global** tree only: `$OCX_HOME/toolchain/`, a `FileStructure` field built once in `with_root`, never re-constructed by a literal path join, outside the GC graph (the `ShimBinStore` posture).
- **A project home is a value, not a store.** `resolve_toolchain_home(project_dir: &Path, config: &Config) -> ToolchainHome` yields `<project>/.ocx/toolchain` by default, or `<toolchain-dir>/<project-key>/toolchain` when `toolchain-dir` is set, with `<project-key>` = `ReferenceManager::name_for_path` over the canonical project directory — the same 16-hex derivation `projects/` and `state/projects/<key>/` already use. This is what the predecessor ADR means by "project-rooted trees are managed via `project/`, not as stores"; the three records now say it in the same words.

Other layout facts:

- `bin` is **reserved as a group name and as a tool name** (like `default` and `all`), so a future per-group `<group>/bin/` stays possible without a layout break.
- `<group>/<entry>` links are written with `symlink::replace_atomic` under the predecessor ADR's containment policy (ARCH-4c), take no `refs/symlinks/` back-reference, and target the package root in **both** materialization states.
- **Repointing a link is atomic on POSIX only.** `rename_replace` (`crates/ocx_lib/src/symlink.rs:203-222`) documents `rename(2)` as atomic; the Windows arm (`:283-300`) is a **remove-then-rename with a bounded non-atomic window**, and its convergence argument rests on "concurrent same-hash installers stage an *equivalent* junction" — that is, on cooperating ocx writers. Inside an attacker-writable project home that premise does not hold. Residual row R8.

### Reserved and validated names

| Name | Rejected as | Site | Exit code |
|---|---|---|---|
| `default` | group | `crates/ocx_lib/src/project/config.rs:612` (exists) | 78 |
| `all` | group | `crates/ocx_lib/src/project/config.rs:627` (exists) | 78 |
| `bin` | group **and** tool | group check beside `:612`/`:627`; **tool check is new** | 78 |

**A strict charset validator for group names and `[tools]` keys is new, and is a security control, not tidiness.** Neither is validated today: `parse_tool_map` (`crates/ocx_lib/src/project/config.rs:818`) validates values only, and the parse path (`:642-645`) inserts every `[group.<name>]` key into the map after checking only that it is not `default` or `all`.

**What it defends changed with D-9; the rule did not.** The original justification was that both strings are baked verbatim into a generated shell script — under D-9 the trampoline body carries neither (§ *Trampoline contract*). Two reasons survive and are sufficient on their own: both become **path components** of the rendered tree (`<home>/toolchain/<group>/<entry>`), where a `/`, a `..` or a control byte escapes the very home the containment policy is written against; and in following mode `ocx env` emits those paths as **environment values**, so a group name still reaches an `eval`-ed export line, by a second route. Both must satisfy:

```
^[A-Za-z0-9][A-Za-z0-9._-]*$      (max 64 bytes)
```

**Only the length bound is shared with the slug grammar.** `SLUG_MAX_LEN = 64` (`crates/ocx_lib/src/package/metadata/slug.rs:18`) is reused verbatim; `SLUG_PATTERN_STR = ^[a-z0-9][a-z0-9_-]*$` (`slug.rs:17`) is **not** — the charset above is deliberately wider by uppercase and `.`, because these keys name existing user-authored bindings (`MSBuild`, `python3.13`) and narrowing them to the slug grammar would break projects for a reason unrelated to the injection this validator exists to close.

A violation is a `ProjectErrorKind` in the same class as `ReservedGroupName` (exit **78**), naming the offending key. The validator fires for `[tools]`, for every `[group.<g>].tools`, and for every `[group.<g>]` name. It is the same new `[tools]`-key validator the `bin` reservation needs — one validator, two reasons.

**Reserved-name comparison folds ASCII case on every platform.** `BinaryName` is case-preserving and derives `Ord` over the raw string (`crates/ocx_lib/src/package/metadata/binary.rs:20-23`), and the existing `ocx` refusal is a raw `==` (`crates/ocx_lib/src/package_manager/tasks/prepare_lazy.rs:307`), so `Ocx` slips through today. Every reserved-name and refusal check compares the ASCII-lowercased name. Separately, the **name set dedupes on the case-folded key where the target filesystem is case-insensitive** (Windows, default APFS), with the same last-walked-first winner — otherwise `Make` and `make` render two entries that are one file.

### `ocx.toml` keys and the activate × pinned matrix

```toml
# global $OCX_HOME/ocx.toml or project ocx.toml — top-level, beside lazy-mode
activate = "env"    # "env" | "bin" | "none";  default "env"
pinned   = false    # bool;                    default false
```

| | `pinned = false` (following, default) | `pinned = true` |
|---|---|---|
| **`activate = "env"`** (default) | Hook exports declared vars and prepends real bin dirs as **link paths**, which land ahead of everything. Global `bin/`: **rendered and in the desired set**, as in every mode. Project `bin/`: **rendered, not emitted**. | Hook exports **digest paths** and digest-resolved vars. **No `<group>/<entry>` links rendered.** Same `bin/` treatment. |
| **`activate = "bin"`** | No variables, no patches env. The project's `<home>/toolchain/bin` is emitted between `ocx_install_bin_path` and `$OCX_HOME/toolchain/bin` (§ *Hook import per `activate`*), after the stamp and link-heal gates. Links rendered, for the dereference consumers. | Same, with no links rendered. |
| **`activate = "none"`** | Hook contributes nothing **for this toolchain**. The two global session entries are unaffected — they are session-level, not activation. The tree is **still rendered** by `pull`. | Same, with no links rendered. |

**No cell of that matrix changes a trampoline body** *(D-9)*. A trampoline bakes no digest and no link path — it reads `pinned` at re-entry through the ladder below — so flipping `pinned` changes which paths the *composition* yields and leaves `bin/` byte-identical. Under the old D2 this table had trampolines churning on every version bump; they no longer do.

Two properties fall out and are contracts:

1. **Rendering is independent of `activate`.** `activate` decides only what the reconciler imports.
2. **`pinned` is a property of every emitter, not of a mode.** `ocx env`, `ocx exec`, `ocx direnv export` and the hook in `env` mode all give digest paths under `pinned = true` — and a trampoline inherits that by being one of them, not by baking anything.

### Resolution ladders

One generic type, not two copies. `LazyModeLadder` (`crates/ocx_lib/src/lazy.rs:210-272`) is the shape; the two new keys use a parameterized `Ladder<T>` with a `resolve(self, floor: T) -> T` most-specific-first `.or()` chain and a **literal** floor. Whether `LazyModeLadder` itself is refactored onto `Ladder<LazyMode>` is a mechanical follow-up, not a prerequisite.

| Key | Tiers, most specific first | Floor |
|---|---|---|
| `pinned` | `--pinned` (on `ocx env` / `ocx exec`) ▸ `ocx.toml` `pinned` ▸ `OCX_TOOLCHAIN_PINNED` | `false` |
| `activate` | `ocx.toml` `activate` ▸ `OCX_TOOLCHAIN_ACTIVATE` | `env` |

**Naming rule** *(owner, 2026-09-05 — § Decided, D-6)*: **inside `ocx.toml` a key is bare, because the file is the toolchain**; outside it the noun is explicit. So the keys stay `activate` and `pinned`, the env floors are `OCX_TOOLCHAIN_ACTIVATE` and `OCX_TOOLCHAIN_PINNED`, and the setup flag is `--toolchain-activate`. The per-call `--pinned` on `ocx env` / `ocx exec` stays bare: both are toolchain-tier commands, so the context is already the toolchain.

`OCX_LAZY_MODE` is **not** an exception to this rule: lazy mode governs the package layer as well as the toolchain (owner, 2026-09-05), so its noun is correctly unqualified and it stays as spelled.

**The environment variable is the weakest tier, not an override.** `OCX_TOOLCHAIN_ACTIVATE` and `OCX_TOOLCHAIN_PINNED` are *defaults for a toolchain that states none* — identical to `OCX_LAZY_MODE`. This surprises people who expect env to win, so it is stated in `environment.md` in those words. Both readers fold case and warn-and-fall-back on an unrecognized value.

`activate` has **no per-call CLI flag**: no command's behaviour changes per invocation with it — the hook reads it. `ocx self setup --toolchain-activate <mode>` is a persistence flag, not a per-call one.

### `config.toml` placement key

```toml
# root-level scalar, any tier including [managed]
toolchain-dir = "~/.cache/ocx/toolchain"          # Unix
# toolchain-dir = "%LOCALAPPDATA%\\ocx\\toolchain"  # Windows
```

- A **root**, not a literal path: project trees land at `<root>/<project-key>/toolchain/`, so one fleet-wide value never collides across projects.
- **The global home ignores it.** `$OCX_HOME/toolchain/` never moves.
- `OCX_TOOLCHAIN_DIR` is the env spelling; resolution-affecting, so it joins `OcxConfigView` and `Env::apply_ocx_config` (`crates/ocx_lib/src/env.rs:609-687`) and is documented in `environment.md`.
- Forward-compat: no `deny_unknown_fields` anywhere in the `Config` tree, so an older binary ignores the key.

**Three refusals at config parse, all security controls** (exit **78**, `ConfigError`, per `quality-rust-exit_codes.md`):

1. **The root must resolve inside the user's own home or inside `$OCX_HOME`.** After canonicalization it is required to be a descendant of `$HOME` (Unix) / `%USERPROFILE%` (Windows), or of `$OCX_HOME` — compared component-wise, never as a string prefix. Anything else is refused. *(added 2026-09-05, cross-model gate)* This is the load-bearing check, and it subsumes the enumeration below.
2. **Never a filesystem root or a system prefix**, kept as a defence in depth for the case where `$HOME` itself is set to something absurd: refused if it resolves to `/`, or to any of `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`, `/etc`, `/var`, `/opt`, `/boot`, `/dev`, `/proc`, `/sys`, `/System`, `/Library`, `/Applications`, `/private`, a bare drive root (`C:\`), `%SystemRoot%` and its subtree, `C:\Program Files`, `C:\Program Files (x86)`, or `C:\ProgramData`. The reason is B2 below: an owned prefix is a *deletion* authority.
3. **The resolved root must be owner-owned and not group- or world-writable.** On Unix, `st_uid` equals the effective uid and the mode carries neither `0o020` nor `0o002`; on Windows, the directory's owner is the current user. Refused with an error naming the path and the failing property.

**Why containment, and not a stronger ancestor check.** Checks 2 and 3 look only at the resolved root itself, so they say nothing about its **ancestors**: a world-writable directory anywhere on the path can be renamed or replaced between parse and use, and on Windows an owner SID says nothing about a DACL that grants another principal write access. A `/tmp/ocx-tc` root passes both and is trivially hijackable. Check 1 removes that class wholesale, without the portability problem — a full component walk with reparse-point rejection and Windows effective-ACL evaluation is a real piece of work with its own failure modes, and confining the root to a hierarchy the user already owns end to end buys the same property for one canonicalize-and-compare.

**Stated residual — a writable ancestor *inside* the user's own home.** If the user has themselves made an intermediate directory group- or world-writable, containment does not save them; a local attacker in that group can still substitute the root. This is accepted: at that point the attacker can equally rewrite `~/.profile`, and OCX is not the weakest link. **Deferred** (§ *Deferred*) is the stronger form: a use-time component walk that rejects symlinks and NTFS reparse points and refuses any ancestor writable by a principal other than the user, with effective-ACL inspection on Windows rather than owner comparison. It is registered as R9.

### CLI surface

Nothing in the signed CLI taxonomy changes. Additions only:

| Command | Change |
|---|---|
| `ocx [--global] pull [-g GROUP]... [--dry-run]` | Renders `<home>/toolchain/` as a **whole-compose pass after roots resolve**. `bin/` covers the **default group** only. `<group>/<entry>` links follow `-g`'s shipped default: **bare `ocx pull` renders links for every `[tools]` and `[group.*]` entry in the lock** (`crates/ocx_cli/src/command/pull.rs:41-49`), `-g` narrows to the named groups. `--dry-run` reports the tree delta, writes nothing, **and performs no heal**. |
| `ocx [--global] {add,remove,lock,update}` | Re-render, same pass, same scope rule: `bin/` = default group, links = every group in the lock. |
| `ocx self setup [--toolchain-activate <env\|bin\|none>]` | Persists `activate` into the **global** `ocx.toml`. **Omitted = untouched.** |
| `ocx env` / `ocx exec` | Gain `--pinned` (`options::Pinned`) — designed by the predecessor ADR, **not yet implemented**; delivered with this work. |
| `ocx shell state` | Prints the **resolved toolchain home** and the effective `activate` / `pinned` values. Under `--format json` the resolved home is a machine-readable field, and that is a **contract**: it is how an IDE, a devcontainer feature or a CI step discovers the home when `toolchain-dir` is configured (D8). Still read-only, still never eval-able. |

**Named boundary change.** `ocx self setup --activate` makes `self setup` a writer of the **global toolchain manifest**, which until now only `add`/`remove`/`lock`/`update` touched. It routes through `project::mutate` under `acquire_project_lock_for_file`, never a second `toml_edit` site, and **creates `$OCX_HOME/ocx.toml` when absent** with only that key — the "missing file is the create case" rule `setup::shell_config::set_flag` (`crates/ocx_lib/src/setup/shell_config.rs:70`) already applies to `config.toml`. Without creation, `--activate` silently no-ops on a fresh install, which is exactly when a corporate rollout sets it.

### Trampoline contract

**One body per exposed name — no variants.** There is no linked / pinned / deferred split at this layer any more: `pinned` and `lazy-mode` are read at re-entry (D2), and a deferred tool is materialized *inside* `ocx exec`'s composition through the shipped lazy shim slots, never by the trampoline. That collapse is what turns the three-row Windows sidecar table below into one row.

**POSIX.** A generated `sh` script, mode 0755. Shape (not code):

```
#!/bin/sh
# Generated by ocx at install time. Do not edit.
#  unset OCX_GLOBAL OCX_PROJECT
#  exec "${OCX_BINARY_PIN:-ocx}" --project '<abs project root>' exec -- "$(basename "$0")" "$@"
#     … the global home renders `--global` in place of `--project '<…>'`
```

**The baked home selector is a single-quoted literal — never interpolated inside double quotes.** `LauncherSafeString` (`crates/ocx_lib/src/package_manager/launcher/safety.rs`) rejects exactly `' " \n \r \0`; `$`, backtick and `\` are deliberately admissible, and `%` is explicitly pinned as valid (`safety.rs:99-115`, the `100%real` test). Its whole premise is the **single-quoted** context the shipped bodies use (`body.rs:68` and `:107`, both `'{…}'`). A double-quoted `"<abs project root>"` would make `$(…)` anywhere in a project's own directory path a command substitution running at every invocation. The single-quoted form is what makes the rejection set sufficient, and the writer **refuses**, naming the path, when the project root contains a `'` or a newline — the same refusal the macOS session-PATH writer makes for the same reason (§ *Session PATH*).

Other properties:

- **Only the home selector is baked.** No group, no entry, no digest, no flag. The tree is therefore *position-dependent* — a moved home needs a re-render, which `ocx pull` is — and this is deliberate: `toolchain-dir` puts a project's home at `<toolchain-dir>/<project-key>/toolchain`, from which no `$0` walk can recover the project directory `ocx exec` must be pointed at.
- **`$0` is used for the tool name only, not for self-location.** `$(basename "$0")` names the tool; the home comes from the baked selector. This removes the whole `readlink -f` / symlinked-elsewhere class the earlier link-relative design had to defend against ([rustup#2858](https://github.com/rust-lang/rustup/issues/2858), [#3136](https://github.com/rust-lang/rustup/issues/3136), [#4224](https://github.com/rust-lang/rustup/issues/4224)) — a trampoline symlinked from an unrelated directory resolves the same home, by construction rather than by a portable symlink walk.
- **`exec -a` argv0 spoofing is bounded.** A caller can hand the trampoline any `argv[0]` and `$(basename "$0")` forwards it — but the forwarded name is only a **name to resolve on the composed PATH** of the home the baked selector names. A spoofed name that the composition does not provide fails at command resolution (exit 65, item 22); it cannot reach outside the composition, because resolution excludes the ambient PATH (same item).
- **Cannot promise:** nothing about `$0` beyond the basename, which is the only thing this body reads from it.

**Windows — a third sidecar, `<stem>.exec`.** The earlier "no new grammar, reuse `.shim`/`.shimref`" clause is **superseded**: `.shim`'s file extension *is* its verb selector (`SIDECAR_PROBE_ORDER`, `crates/ocx_shim/src/core.rs:114-115`), and that verb is `launcher exec` (`core.rs:384`), which is no longer what a trampoline calls. Rather than overload an existing extension with a second meaning — the one thing the frozen grammar's design refuses — D-9 adds a third:

| Sidecar | Payload (one line) | Wire verb | Containment |
|---|---|---|---|
| `<stem>.exec` | an absolute path (the project root), **or** the literal `global` | `WIRE_SUBCOMMAND_EXEC` = `exec` | none — the value is a project selector, not a package root, and `ocx` re-resolves it through the ordinary project chain |

- **The five shared read rules are reused verbatim** — 32 KiB cap, exactly one stripped terminator, non-empty after the strip, no `0x00`/`0x0A`/`0x0D`, valid UTF-8 (`parse_one_line`, `core.rs:145-195`). Only the clause is new, and it is: `is_absolute_path` (the `.shim` clause, `core.rs:207`) **or** byte-equality with `global`.
- **The emitted child command line differs structurally from the two shipped arms**, and this is the one place the new sidecar is not a drop-in: `build_child_command_line` (`core.rs:421`) emits `<program> <verb> "<value>" -- "<stem>" <argv…>`, value *after* verb. A project selector is a **root** flag, so the `.exec` arm emits `<program> --project "<root>" exec -- "<stem>" <argv…>`, and the global arm emits `<program> --global exec -- "<stem>" <argv…>` with no value at all. Every token still goes through the `CommandLineToArgvW` quoter, unchanged.
- **The shim strips `OCX_GLOBAL` and `OCX_PROJECT` from the child environment** — the Windows counterpart of the POSIX `unset`, and load-bearing for the same reason. `CreateProcessW` is called with `lpEnvironment = NULL` (`crates/ocx_shim/src/main.rs:640`), so the child inherits the shim's own block; two `SetEnvironmentVariableW(name, NULL)` calls before the spawn are the whole implementation. That needs a third `windows-sys` feature, `Win32_System_Environment` (the workspace list at `Cargo.toml:228-236` carries neither it nor the two the session-PATH writer needs). Still no new crate.
- Write order is `.exe` before sidecar, sequenced inside one per-entry task — the shipped `write_shim_exe_then_sidecar` postcondition.
- **Asymmetry, stated:** the `.exec` sidecar holds an absolute project root, so a moved project needs a re-render. The POSIX body bakes the same value and has the same property; the two arms are symmetric here, where the earlier design was not.

**`.shimref` has a producer, and it is not the trampoline.** D-2 (the Windows `lazy-mode` floor is lifted in this work) stands, but its producer is re-anchored: since a trampoline bakes no pinned identifier, the first `.shimref` writer is the **Windows shim-slot arm of `prepare_lazy`** — the same producer the POSIX side already has. `LazyModeLadder::resolve_for_host` forces the mode to `Never` on Windows today (`crates/ocx_lib/src/lazy.rs:255-268`) and its own comment names the removal condition exactly: *"Deleted in the same change that adds the Windows shim PRODUCER."* WP-6 writes that producer and removes the `cfg!(windows)` branch together, and **Windows deferred loading ships here** with its own acceptance coverage (validation item 15a): a Windows host with `lazy-mode = "always"` gets `.shimref` sidecars in its shim slots, `launcher shim` materializes on first invocation, and the composed environment matches the eager one.

**Wire-ABI canary.** **One new wire token**, `WIRE_SUBCOMMAND_EXEC` — the third, beside `WIRE_SUBCOMMAND` (`launcher exec`) and `WIRE_SUBCOMMAND_SHIM` (`launcher shim`), and the third to carry the same two-producer problem: `ocx_lib` cannot depend on the shim crate, so nothing the compiler sees binds the shim's literal to the POSIX body's. The paired-golden set therefore gains **one pair** — a byte-exact assertion on the trampoline body in `body.rs`, beside `launcher_wire_token_is_bound_to_shim_producer` (`body.rs:242`) and `launcher_shim_wire_token_is_bound_to_shim_producer` (`body.rs:358`), and its restatement from the shim side beside `shim_wire_token_matches_sh_body` and `shim_ref_wire_token_matches_sh_shim_body` (`crates/ocx_shim/src/main.rs`). The two shipped tokens keep their producers untouched: `entrypoints/` launchers and lazy shim slots still emit `launcher exec` and `launcher shim` respectively, and nothing in this record changes that.

### Name set

**Name set — the owner's rule, verbatim** (2026-09-05): *"we do not add what is on PATH — that would require detecting executables on PATH at install time; we use the metadata description of packages including their dependencies, which are all entrypoints and binaries (the metadata claims) on the interface surface. It is very important that dependencies are included (if on the dependent's interface surface)."*

Three things follow, and they are the contract: the set comes from **metadata claims only**, never from probing a filesystem or a `PATH`; it is `binaries` ∪ `entrypoints`; and an **interface-admitted dependency contributes its claims like a root does**. That is precisely the shipped algebra, which is why nothing is re-derived here.

**Name set.** The consumer (interface) surface of the **default group's** roots *and their admitted dependencies*: the closure is pre-filtered by `inspect::admitted_on_surface(node, /* self_view */ false)` (`crates/ocx_lib/src/package_manager/tasks/inspect.rs:550`), then the shared name-set function unions each admitted node's `binaries` claim and declared entry points through `composer::carrier_crosses` (`crates/ocx_lib/src/package_manager/composer.rs:163-199`). **The algebra is reused, never re-derived.**

**`interface_shim_names` is refactored to carry ownership, and moves.** It returns `BTreeSet<BinaryName>` today (`crates/ocx_lib/src/package_manager/tasks/prepare_lazy.rs:270-317`), which discards which package claimed which name — and the render needs that for the collision rule and for `ocx inspect` reporting. It moves to `package_manager/tasks/toolchain_names.rs` as `exposed_names`, returning a map from name to owner; `prepare_lazy` consumes the keys and is otherwise unchanged. **The design record is the type authority** — `system_design_toolchain_activation.md` § *Type signatures* carries `NameOwner`, including its `shadowed` field. **One algebra, one name-set function, four consumers**: `ocx env`, `inspect --closure`, `prepare_lazy`, and the rendered `bin/`.

One deliberate divergence in policy, which makes the function parameterized rather than copied: **`ShimNamesNotEnumerable` must not be fatal here.** A package claiming neither `binaries` nor entry points is a refusal for a *deferred* tool but is ordinary for a render — it contributes no names. The policy is a parameter (`Refuse` for `prepare_lazy`, `Skip` for render).

**A package with no `binaries` claim gets no trampolines. Full stop.** There is no post-pull scan fallback. `scan_interface_binaries` (`crates/ocx_lib/src/package/bin_scan.rs:53-63`, `:92-110`) is an **authoring-time** function: it takes `&AuthoringMetadata`, and it scans install-path-rooted, interface-visible `Path` env targets rather than a package's `content/bin`. Re-purposing it at render time would mean a second, differently-shaped scanner for a case `ocx inspect` already reports honestly as `binaries_complete = false` (`tasks/inspect.rs:132-151`), and which the mirrored fleet already closes by auto-scanning at `ocx package create`. Dropped as YAGNI.

**Collision rules — one tier. No refusal, no warning.** *(Owner decision, 2026-09-05 — see § Decided, D-4.)*

| Case | Behaviour |
|---|---|
| Any name collision at all — with `ocx` itself, with a privilege name, with a system tool, or across two packages in one composition | **Debug level, and visible in `ocx inspect`.** Last tool walked wins, matching composed-PATH order (`env-composition.md:221`). Never a refusal. Never a warning. |

**Why nothing is refused.** Two drafts tried to draw a line here and both were wrong. The first warned when a non-root package claimed a name on a denylist (`git`, `curl`, `ssh`, `sh`, …); round 1 killed it, because a toolchain that ships `git` is the product's purpose, so the warning fires forever on a legitimate steady state — the "WARN on a common benign state" the owner's doctrine forbids. The second kept hard refusals for `ocx` and five privilege names; the owner struck those too: **a project may legitimately pin its own `ocx` and shadow the name for itself, so everything has to keep working.** A refusal there breaks a supported use case to defend against a threat the trust model already covers. YAGNI.

**This relaxes a shipped refusal, deliberately.** `interface_shim_names` returns `PackageErrorKind::ShimNameShadowsOcx` today when a claimed name is the literal `ocx` (`crates/ocx_lib/src/package_manager/tasks/prepare_lazy.rs:307-312`). **Shim slots and trampolines follow one rule**, so that refusal becomes a debug-level note in the same work package that moves the function (WP-5). The one test that flips is `interface_shim_names_refuses_the_literal_ocx_name` (`prepare_lazy.rs:666`) — renamed and inverted to assert the name is admitted and logged. `PackageErrorKind::ShimNameShadowsOcx` is deleted rather than deprecated, per the internal-code stability tier; no `ShimNameReserved` kind is ever introduced.

**What actually bounds name squatting** is not a denylist: the names come from a lock the user pinned to digests, the hook path is consent-gated, and putting a trampoline directory on PATH is the user's own choice. Recorded as an accepted residual (R4) with the npm bin-entry harvesting precedent as the trigger for revisiting it.

### Hook import per `activate`

Two global entries are desired **unconditionally, in every row below**: `ocx_install_bin_path` and `$OCX_HOME/toolchain/bin`. They are the ones `ocx self setup` registered at session level (D5), and dropping them from the desired set is what deletes that registration (D6).

**Relative order among these three entries**, front to back on the resulting PATH:

1. `ocx_install_bin_path` — ahead of both trampoline directories, so a trampoline's `${OCX_BINARY_PIN:-ocx}` fallback can never resolve `ocx` out of a repository-controlled directory.
2. The project's `<home>/toolchain/bin` — only in `bin` mode, so a project tool shadows the global one.
3. `$OCX_HOME/toolchain/bin` — always present.

This fixes their order relative to *each other*. It says nothing about where the block sits overall: in `env` mode the real bin dirs still prepend ahead of all three under move-to-front semantics (`env-composition.md:221`), which is exactly why a trampoline never runs in an activated shell.

| `activate` | What the reconciler emits **in addition**, for that toolchain |
|---|---|
| `env` | Today's entries, with **link paths** in place of digest paths when following. Not the project's `toolchain/bin`. |
| `bin` | One path entry: the project's `<home>/toolchain/bin`, at position 2 above. No variables. No patches env. Emitted **only** after the render-stamp check *and* the link heal below both pass. |
| `none` | Nothing. |

**`owned_prefixes` — narrow, and never a configured root as given.** `plan(desired, current, ledger, owned_prefixes)` (`crates/ocx_lib/src/shell/reconcile/plan.rs:97`) treats an owned prefix as a **deletion authority**: `repair_owned_segments` (`plan.rs:627-673`) removes *every* PATH segment under an owned prefix that the desired set does not declare. Owning a bare `toolchain-dir` of `/usr` would therefore strip `/usr/bin` from the user's PATH on the next prompt. Accordingly:

- `$OCX_HOME` stays the global prefix, unchanged (`crates/ocx_cli/src/command/self_group/activate.rs:352` supplies it today).
- The project prefix added is **`<home>` for the consented, in-scope project only** — `<project>/.ocx/toolchain/` or `<toolchain-dir>/<project-key>/toolchain/`. Never the bare `toolchain-dir` root, never a project that consent refused, never a project that is not the one in scope.
- The `toolchain-dir` parse refusals above are the second line of defence, for the case where the key names a system prefix at all.

Heal runs **strictly after consent**, unchanged. A consent-refused project's tree is left untouched.

### Render and heal triggers

| Trigger | Renders `bin/` | Renders `<group>/<entry>` | Heals |
|---|---|---|---|
| `ocx [--global] pull` | ✔ (default group) | ✔ (every group in the lock; `-g` narrows) | ✔ |
| `ocx [--global] {add,remove,lock,update}` | ✔ (default group) | ✔ (every group in the lock) | ✔ |
| `ocx env` / `ocx exec` / `ocx direnv export` / `self activate --reconcile` | ✘ | ✘ | ✔ (heal-before-emit) |
| `ocx pull --dry-run` | ✘ | ✘ | ✘ (**no heal** — dry-run writes nothing, including repairs) |
| `ocx inspect` / `ocx status` / `ocx shell state` | ✘ | ✘ | ✘ (neither emits nor composes) |

**`bin/` is a whole-directory reconcile at render**: write the computed name set, prune every name no longer in it. Per-file atomic writes and per-file removals, no completeness marker — nothing consumes `bin/`'s completeness, so a crash mid-render leaves a directory missing some names until the next render. Directory-level atomic publish is rejected: `move_dir` calls `remove_dir_all` on the destination and is banned by the atomic-publish-with-convergence rule.

**A render stamp gates `bin` mode, and it is the fix for the committed-`bin/` attack.** The threat is concrete and is the one `adr_shell_env_overhaul.md:195` names: a consented clone that force-commits `.ocx/toolchain/bin/cmake` would, without this, reach PATH-front on the first prompt, before any `ocx pull` had rendered or pruned anything. Heal as the predecessor ADR defines it does not cover it — heal verifies `<group>/<entry>` links, and this attack ships no links at all.

Contract:

- Every render writes a **render stamp** beside the existing consent stamp, under `state/projects/<key>/` for a project home and under `$OCX_HOME/state/` for the global one — the shipped `StateStore` root, no new store. The stamp records the resolved home, the computed name set, and a fingerprint of the rendered `bin/` tree.
- **The fingerprint covers exactly this**: the sorted list of `bin/` entry names, plus per entry a content hash of the POSIX launcher body; on Windows, the `.exe`'s hardlink identity against the `ShimBinStore` blob and the sidecar's bytes; **and the sorted `<group>/<entry>` → digest-root pairs the default group's trampolines dispatch through**. **No mtime**, so a checkout that rewrites timestamps does not invalidate it.
- **The link targets stay in the fingerprint, for a narrower reason than before** *(rewritten 2026-09-05, D-9)*. A trampoline dispatches nothing through `<group>/<entry>` any more — it re-enters `ocx exec`, which composes from the lock — so a repointed link can no longer make the wrong package run *through a trampoline*. The links are authenticated because they are still **consumed**: an `env`-mode PATH prepend and every dereference value (`JAVA_HOME`, an SDK home) resolve through them, and `bin` mode's heal needs a fingerprint to know it has work. A branch switch or a second checkout can repoint a link at a **different but entirely valid** installed package while every byte under `bin/` stays identical, and no consumer of the link holds lock context to tell one legitimately-installed package from another. So `bin/` bytes alone do not pin the tree, and both halves stay in the stamp.
- **Therefore `bin` mode also heals the links before emitting** — the same lock-only heal the predecessor ADR defines for composing emitters, narrowed to the default group: `readlink` each `<group>/<entry>` and compare against the lock-derived digest root, `lock_scoped` repoint on mismatch. **Pure lock arithmetic: no compose, no metadata read, no network.** Cost is N `readlink` calls per prompt for N default-group entries, alongside the `bin/` hashes. The fingerprint check runs first and usually answers before the walk is needed.
- **In `bin` mode the reconciler emits `<home>/toolchain/bin` only when a stamp exists for that home *and* the directory's fingerprint matches it.** The fingerprint **is recomputed from the directory on each prompt** — a fingerprint can only be matched by recomputing it — but that recomputation is **a directory read plus N small file hashes: no compose, no metadata read, no network**. `bin/` holds one entry per exposed name, so N is the toolchain's tool count; the cost sits well inside the 4.8 ms no-op budget, unlike the compose a byte-exact re-render would need. No stamp, or a mismatch, means the entry is not emitted, one debug line, and no PATH change. A fresh clone is therefore **inert for `bin` mode** until `ocx pull` renders — the same posture consent already gives, for the same reason.
- Repair is `ocx pull`'s job, not the prompt's. The next render reconciles and prunes.

> **Deviation from the review's literal instruction, stated.** B3 asked for a byte-exact re-render of `bin/` on every emit path that would put it on PATH. Computing the expected trampoline for every name requires the name set, which requires a resolved metadata read — a full compose on **every prompt**, against a reconciler whose no-op path is budgeted at 4.8 ms. The stamp delivers the same security property (a foreign or unrendered `bin/` never reaches PATH) at a fingerprint's cost, and it is strictly stronger on the fresh-clone case, which the byte-exact compare would have *allowed* to emit as soon as one legitimate name matched.

`<group>/<entry>` links keep the predecessor ADR's rule: temp name plus rename, per-entry, idempotent under concurrent healers — atomic on POSIX, bounded-window on Windows (R8).

**Registration invariant, carried from the predecessor ADR and restated against the `pull` trigger.** Materializing a **project** tree registers the project in the `projects/` GC ledger; the **global** tree registers nothing — `register` is a no-op for `$OCX_HOME` under the no-self-link invariant (ARCH-1b), and the global lock is rooted implicitly. Since `pull` is now a render trigger, `ocx pull` in a project is a ledger writer where it previously was only through its lock save. Both directions carry a validation item.

### Session PATH, per platform

`ocx self setup` registers `ocx_install_bin_path`, then `$OCX_HOME/toolchain/bin`.

**Encoding is per format, and what cannot be encoded is refused before writing**, with an error naming the offending path. This matters because `%` is admissible in a package root by design (`safety.rs:99-115`) and `$OCX_HOME` is user-chosen.

| Format | Structure | Refused before writing | Escaped |
|---|---|---|---|
| `~/.config/environment.d/ocx.conf` | line-structured `KEY=VALUE`, with `${FOO}` / `${FOO:-x}` expansion at session-manager read time | a path containing `\n`, `\r` (injects an unrelated variable line) or `$` (expands to something else at read) | — |
| `~/Library/LaunchAgents/sh.ocx.path.plist` | XML | control characters XML 1.0 forbids | `&`, `<`, `>` XML-escaped. launchd **silently no-loads** a malformed plist, so escaping — not refusal — is the only outcome a user could diagnose |
| `HKCU\Environment\Path` (`REG_EXPAND_SZ`) | `%…%` pairs expand at read time | a path containing `%` — **no escape exists** in `REG_EXPAND_SZ`, so a `100%real` home cannot be represented and is refused rather than silently mangled | — |

**Windows.**
- Direct registry write to `HKCU\Environment\Path`. **Never `setx`** — it truncates silently at 1024 characters and has corrupted real users' PATH in shipped installers ([desktop/desktop#18176](https://github.com/desktop/desktop/issues/18176)).
- Write `REG_EXPAND_SZ` **unconditionally**. Read the existing type only to decide *how to merge*. rustup shipped `REG_SZ` here and broke `%VAR%` expansion for every *other* entry already on PATH ([rust-lang/rustup#261](https://github.com/rust-lang/rustup/issues/261)).
- **Idempotent by presence test, not by append.** Merge rule: split the existing value on `;`, drop empty segments, drop any existing occurrence of either of our two directories (compared case-insensitively, as Windows paths are), then prepend the two in order and rejoin. A second `ocx self setup` is a no-op and the value does not grow.
- Broadcast `WM_SETTINGCHANGE` with `SendMessageTimeoutW(HWND_BROADCAST, …, "Environment", SMTO_ABORTIFHUNG, 5000, …)`.
- `windows-sys` is already a workspace dependency (`Cargo.toml:228`) but carries neither `Win32_System_Registry` nor `Win32_UI_WindowsAndMessaging`. Two feature additions, **no new crate**.
- **Cannot promise:** an already-open terminal or IDE seeing the change without restart; winning a collision against an entry on the **System** PATH — Windows resolves System before User and write order cannot change that.

**Linux.**
- `~/.config/environment.d/ocx.conf` with a `PATH=…:$PATH` prepend, **and** the existing `~/.profile` managed block. **Co-primary, not primary-and-fallback**: `environment.d` reaches only processes under `systemd --user`, confirmed for GNOME and KDE Plasma Wayland; LightDM (by default), SDDM and non-systemd desktops never read it.
- `~/.pam_environment` is not written — deprecated since pam_env 1.5.0.
- Owner-only-write permission hygiene (CWE-732).
- **Cannot promise:** PATH visibility on a desktop that is neither a systemd-user session nor profile-sourcing (a bare i3/sway started outside any Xsession wrapper); or inside a **Flatpak- or Snap-sandboxed** application, whose PATH is set by the sandbox and is not inherited from the session ([Flatpak sandbox](https://github.com/flatpak/flatpak/wiki/Sandbox)).

**macOS.**
- A `RunAtLoad=true` LaunchAgent at `~/Library/LaunchAgents/sh.ocx.path.plist`. Its `ProgramArguments` are **`/bin/sh -c <merge script>`**, not `launchctl setenv PATH <literal>`.
- **It must be a load-time merge, not a setup-time snapshot.** launchd runs no shell of its own, so a plist argument is passed verbatim to `execve` and `$PATH` inside it is four literal characters. Baking a composed literal at `ocx self setup` time therefore freezes whatever PATH existed *that day* and replays it at **every subsequent login**, silently reverting any change another tool made in between. The `/bin/sh -c` form is what makes the value a function of the *then-current* session instead:

  ```sh
  # ProgramArguments: ["/bin/sh", "-c", <this>]
  b='OCX_BIN'; t='OCX_TC'
  cur=$(/bin/launchctl getenv PATH)
  [ -n "$cur" ] || cur=/usr/bin:/bin:/usr/sbin:/sbin
  # remove any prior occurrence of our two segments, then re-insert them in front
  out=$(printf '%s\n' "$cur" | tr ':' '\n' | grep -vxF "$b" | grep -vxF "$t" | paste -sd: -)
  /bin/launchctl setenv PATH "$b:$t${out:+:$out}"
  ```

  **Each directory appears exactly once in the script, single-quoted, and every later use is a variable expansion.** That binding is the whole safety argument: inside `'…'` nothing expands, and `"$b"` expands the variable without re-scanning its value, so a `$`, a backtick or a `\` in `$OCX_HOME` is inert at both sites. Interpolating the directories directly into the final double-quoted word — which an earlier draft did — would have re-opened B1 in a second place, because a double-quoted word *does* expand `$` and backtick.

  **`LauncherSafeString` is not the guarantee here.** It rejects only `'`, `"`, `\n`, `\r`, `\0` (`crates/ocx_lib/src/package_manager/launcher/safety.rs:47`), which is exactly the set a **single-quoted** context needs and nothing more; it says nothing about `$`, backtick or `\`. The trampoline body relies on it for the same reason and no other (§ *Trampoline contract*).

  **Refusal, matching the other two writers.** The macOS writer refuses before writing, naming the path, when `$OCX_HOME` contains a `'` (unrepresentable in a single-quoted shell word, and there is no escape for it inside one) or a newline. `&`, `<` and `>` are XML-escaped rather than refused; any character XML 1.0 forbids outright is refused, as the encoding table above states. Remove-then-prepend makes a second load a no-op rather than an accumulating prefix.
- **The two segments are the only thing this writer owns.** Everything else in PATH is copied through the pipeline unchanged, so a tool that appended to the GUI PATH after the agent was installed keeps its entry across the next login.
- **Plist mode must be 0644** — launchd refuses a plist with "dubious permissions" and its failure mode is a *silent non-load*, so this needs its own test rather than a code review.
- `/etc/paths.d` is deliberately not used: login-shell-only, needs root, no effect on GUI apps.
- **Cannot promise:** retroactive visibility for GUI apps already running when the agent loads — `launchctl setenv` reaches processes started afterwards; and ordering against another tool that also calls `launchctl setenv PATH` later in the same session, which simply wins by running last.

**Removal is a library contract, not yet a command, and it is subtractive on every platform.** Each of the three writers has a counterpart: `SessionPathOutcome::Removed` and `deregister_session_path(ocx_home, directories, dry_run)`, which removes **exactly the two segments its sibling added** and is a no-op when neither is present. It never clears, resets or unsets a whole variable — the same remove-then-rejoin arithmetic the Windows merge rule uses, run without the prepend. A foreign segment that was on PATH before `ocx self setup` ran is still there after deregistration.

**`launchctl unsetenv PATH` is specifically forbidden.** It deletes the entire session-wide value rather than OCX's contribution to it, which would strip every other tool's segment from every GUI application launched afterwards. macOS deregistration is: `launchctl unload` the agent, delete the plist, read the current value with `launchctl getenv PATH`, subtract only the two OCX segments, and `launchctl setenv PATH` the remainder — writing nothing at all if the read comes back empty. A validation item asserts a foreign segment survives.

There is **no new CLI grammar for it now**. Until `ocx self uninstall` exists (*Deferred*), the manual reversal path is these three locations:

| Platform | Remove |
|---|---|
| Windows | the two segments from `HKCU\Environment\Path` — edit the value, do not delete it — then re-broadcast `WM_SETTINGCHANGE` |
| Linux | `~/.config/environment.d/ocx.conf` (the `~/.profile` block is `ocx self setup`'s existing managed fence) |
| macOS | `launchctl unload ~/Library/LaunchAgents/sh.ocx.path.plist`, delete that file, then log out and back in. **Not `launchctl unsetenv PATH`** — that empties the session PATH for every application, not just OCX's part of it |

### Errors and exit codes

| Condition | Kind | Exit |
|---|---|---|
| A claimed name collides with `ocx`, a privilege name, a system tool, or another package | **not an error** — debug line, last walked wins, visible in `ocx inspect`. `PackageErrorKind::ShimNameShadowsOcx` is **deleted** (see § *Name set*) | 0 |
| An entry-point name is not a valid `BinaryName` | `PackageErrorKind::ShimNameInvalid` (exists) | 65 |
| A **lazy shim slot** is invoked under a name its package does not claim | `PackageErrorKind::ShimNameNotClaimed` (exists — `crates/ocx_cli/src/command/launcher/shim.rs:318-325`) | 65 |
| A **trampoline** is invoked under a name the composition does not provide | Not a `ShimName` kind at all: the re-entry is `ocx exec`, which resolves the forwarded name on the **composed** PATH (`process_env.resolve_command`, `crates/ocx_cli/src/command/toolchain_exec.rs:329` → `crates/ocx_lib/src/env.rs:800`). Under D-9 that resolution loses its OS fallback for a bare name (item 22), so the failure is `env::CommandResolutionError` | 65 |
| A trampoline's baked home no longer holds an `ocx.toml` / a lock / a current lock | `ocx exec`'s own contract, inherited unchanged: `ProjectContextError::{NoProject, LockMissing, StaleLock}` | 64 / 78 / 65 |
| A node enumerates no names, during a **deferred** prepare | `PackageErrorKind::ShimNamesNotEnumerable` (exists) | 65 |
| A node enumerates no names, during a **render** | not an error — contributes nothing | 0 |
| `bin` used as a group or tool name | new `ProjectErrorKind`, `ReservedGroupName` class | 78 |
| A group name or `[tools]` key violates the charset | new `ProjectErrorKind`, same class | 78 |
| `toolchain-dir` resolves outside `$HOME`/`%USERPROFILE%` and outside `$OCX_HOME` | `ConfigError` | 78 |
| `toolchain-dir` names a root/system prefix, or is not owner-owned / is group- or world-writable | `ConfigError` | 78 |
| `OCX_HOME` cannot be encoded for a session-PATH format | `ConfigError`, naming path and format | 78 |
| `activate` / `pinned` value unparseable in `ocx.toml` | `ProjectErrorKind` (TOML shape class) | 78 |
| `OCX_TOOLCHAIN_ACTIVATE` / `OCX_TOOLCHAIN_PINNED` unrecognized | warn once, tier treated as absent | 0 |
| Render write failure (read-only checkout, foreign-owned dir, lock timeout) | none — skip and continue, compose digest paths | 0 |
| Session PATH write failure | none — warn, advise re-running `ocx self setup` | 0 |
| A dangling link root is handed to `launcher exec` | `UsageError` from `validate_launcher_pkg_root` (exists). **No longer reachable from a trampoline** — that path is `ocx exec`, which installs on miss — but it stays reachable from an `entrypoints/` launcher and a lazy shim slot, whose semantics D-9 does not touch | 64 |

## Non-Functional Requirements

| NFR | Target | How it is met | Evidence |
|---|---|---|---|
| **Latency — activated shell** | No change | `env` mode never emits `toolchain/bin`; the hook's real bin dirs precede the session-PATH global trampoline dir | Reconcile no-op stays 4.8 ms quiet / 7.6 ms CI ([#359](https://github.com/ocx-sh/ocx/issues/359)) |
| **Latency — `bin` mode per prompt** | One directory read plus N small file hashes | The `bin/` fingerprint is recomputed from disk each prompt and compared against the stamp — **no compose and no metadata read**. N is the exposed-name count | § *Render and heal triggers* |
| **Latency — render** | One resolved metadata read per root, cache-first | Same read `prepare_lazy` and `inspect --closure` already perform | `ocx.lock` carries no name data, so the read is structural |
| **Latency — trampoline exec** | Unmeasured; accepted, escalate on measurement. **Owner trigger (2026-09-05): above 10 ms of ocx-side re-entry overhead, file the composed-env-cache follow-up issue** | Per invocation, D-9 pays: process start, `ocx.toml` + `ocx.lock` load, and a **full compose** — the same work `ocx exec` does, because it *is* `ocx exec`. On top of that POSIX pays a `system()`-shaped cost and Windows the scoop band (+24 ms to +103 ms). Measured by **validation item 37** on the trampoline path, not on `ocx exec` in isolation | Escalates to the deferred env-cache or compiled-proxy item, not to a redesign |
| **Operability — corporate** | One key per concern, fleet-settable — **with one gap** | `toolchain-dir` and `activate` reach a fleet through `[managed]`; but **the session-PATH registration is a per-user act with no fleet trigger**. `[managed]` cannot run `ocx self setup`, so an operator must invoke it in **user context** (an Intune or GPO user-context script — [per-user environment deployment](https://cdm.iamcloud.info/docs/Content/Configuration/EV_DeployingEnvironmentVariablesMI.htm)). Recorded as residual R9. | `[managed]` syncs `config.toml`, not commands |
| **Security** | No new ambient trust | Consent model untouched (a trampoline records consent, never gates — DD4); session PATH global-tier-only; the `.exec` sidecar reuses the five shared read rules but **carries no containment of its own**, because its clause is a project selector rather than a package root — what bounds it is that `ocx` re-resolves the selector through the ordinary project chain and composes from a lock the user pinned to digests. `ocx_install_bin_path` precedes every trampoline dir | mise CVE-2026-35533 class defended by inherited ordering; a tampered `.exec` sidecar is the pre-existing repo-writable-`bin/` class, gated by the render stamp (R1, R3); name shadowing accepted as R4 per D-4 |
| **Cost** | Zero new dependencies | `windows-sys` gains three features: `Win32_System_Registry` + `Win32_UI_WindowsAndMessaging` (session PATH) and `Win32_System_Environment` (the shim's `OCX_GLOBAL`/`OCX_PROJECT` strip) | `Cargo.toml:228-236` carries none of the three |
| **Portability** | No privileged operation anywhere | Directory junctions on Windows (no privilege); `HKCU` not `HKLM`; user LaunchAgent; user-owned files. **This is the row on which Option C fails**: per-file symlinks need privilege on Windows, and junctions cannot substitute because they are directory-only | The predecessor ADR rejected that shape on the same ground |
| **Reversibility** | One config value, one directory, three documented un-writes | `activate = "none"` makes the hook ignore the tree; deleting `toolchain/` is safe and self-healing; `deregister_session_path` un-does the three session writes as a library contract, with the manual locations documented until `ocx self uninstall` lands | § *Session PATH* |

## Migration and Rollout

**Existing users see no behaviour change by default.** `activate` defaults to `env`, `pinned` to `false`.

**Three breaks ship. The changelog entry for each is the commit subject — never `CHANGELOG.md`, which is generated by `git-cliff` at release time.**

| Break | Who observes it | Commit subject |
|---|---|---|
| `bin` reserved as a group and tool name; group names and `[tools]` keys gain a charset | A project using `[group.bin]`, `bin = "…"`, or a group name outside `[A-Za-z0-9][A-Za-z0-9._-]*` — now exit 78 at parse | `feat(project)!: reserve "bin" and validate group and tool names against a strict charset` |
| `ocx self setup` gains a session-PATH side effect | Anyone re-running `self setup`; opt out with `--no-modify-path` or `OCX_NO_MODIFY_PATH` | `feat(setup)!: register the global toolchain bin dir at session level per platform` |
| `ocx env` / `.envrc` / `--ci` / `--format json` emit link paths instead of digest paths (the predecessor ADR's PATH surface change) | Any script diffing emitted env across versions | `feat(env)!: emit stable toolchain link paths instead of digest paths` |
| A package claiming the name `ocx` is no longer refused | Anyone who relied on the refusal; a project pinning its own `ocx` now works, which is the point | `fix(package): stop refusing a package that claims the name "ocx"` |

**Two features ship alongside**, not breaks:

| Feature | Commit subject |
|---|---|
| Windows deferred loading, unblocked by the Windows shim-slot `.shimref` producer in `prepare_lazy` | `feat(lazy): support deferred package loading on Windows` |
| Toolchain activation itself | `feat(toolchain): render a toolchain home with launcher trampolines and activate it per project` |

**Amendments to `adr_project_toolchain_links.md`** land in the same change.

**Documentation surfaces, enumerated:**

| Surface | Change |
|---|---|
| `website/src/docs/reference/command-line.md` | `pull` — render semantics, `-g` scope, `--dry-run` (no heal); `self setup` — `--toolchain-activate` and the session-PATH arm; `env` / `exec` — `--pinned`; `shell state` — the resolved-home field |
| `website/src/docs/reference/configuration.md` | `toolchain-dir` root-level key, its containment rule and its three refusals; `ocx.toml` `activate` and `pinned`; `bin` and the name charset in the reserved-name list |
| `website/src/docs/reference/environment.md` | `OCX_TOOLCHAIN_ACTIVATE`, `OCX_TOOLCHAIN_PINNED`, `OCX_TOOLCHAIN_DIR` — each stating that the variable is the **weakest** tier |
| `website/src/docs/reference/env-composition.md` | The activate × pinned matrix; that a trampoline runs in the **composed toolchain environment** — project `[env]` and sibling packages included — because it re-enters `ocx exec` against its home; the collision and refusal rules |
| `website/src/docs/in-depth/storage.md` | `<home>/toolchain/` layout, `bin/` and `<group>/<entry>/`, the render stamp, outside the GC graph |
| `website/src/docs/user-guide.md` | The three activation modes; the clean-shell and IDE/CI stories, each paired with "after `ocx pull`"; the session-PATH promise and its cannot-promise clauses |
| `crates/ocx_schema` | `task schema` regeneration |
| `.claude/rules/subsystem-file-structure.md` | The new store field, `ToolchainHome` as a value, the GC-exempt posture, the render stamp under `state/` |
| `.claude/rules/subsystem-package-manager.md` | The render pass, the name-set refactor, the third sidecar (`.exec`) and the third wire token in the canary rule |
| `.claude/rules/subsystem-cli.md` | `self setup --toolchain-activate`, `pull` render semantics, `--pinned`, `shell state`'s resolved-home field |

## Testing Strategy

The whole feature is a **cache of a lock**, so the only failure that matters is **staleness**: a tree that disagrees with the lock, the metadata surface or the config it was rendered from. Everything below organizes the enumerated checks in § *Validation* around that one question. Item numbers refer to that list; nothing is restated here.

**The invariant, stated once.** *Every trigger that reads the lock converges the rendered toolchain to `render(lock, metadata interface surface, config)` — additions **and** deletions, links **and** trampolines — and never leaves it partially converged (per-entry atomic publish, stamp written last).*

Two halves of that deserve emphasis because they are the ones implementations drop:

- **Prune is part of heal, not an afterthought.** Four orphan classes, each with its own trigger: an **orphan link** (its entry or group was removed or renamed), an **orphan trampoline** (its name is no longer claimed by anything on the surface), a **repointed link** (the entry's digest changed), and a **rewritten trampoline** — which under D-9 has exactly two causes: the baked selector changing (the project directory itself moved or was renamed) and a body-format change across an ocx upgrade. Neither `pinned` nor a digest bump rewrites a body any more, which is why item 33 asserts `bin/` stays byte-identical across a `pinned` flip. A `toolchain-dir` change is *not* a third cause: it renders a fresh tree at a new home whose bodies are byte-identical, because the selector names the project, not the home.
- **The stamp is written last.** A crash between the first entry and the stamp leaves a tree that is *detectably* incomplete rather than one that looks finished. That ordering is what makes "partially converged" a state the hook can refuse to emit from.

### Staleness matrix

Columns are the triggers. **Composing** triggers hold the resolved lock *and* the metadata surface, so they can compute the full desired set and therefore prune; **non-composing** ones cannot, and the matrix says what they do instead.

The last column changed with D-9 and is the reason the matrix is worth re-reading: a trampoline **composes**, so most mutations reach it with no render at all. What it can never do is add or remove a *file* in `bin/`, which is the whole residual staleness surface.

| Mutation | `ocx pull` (either `--lazy-mode`; `--dry-run` reports only) | `add` / `remove` / `lock` / `update` | `ocx env` / `exec` / `direnv export` (composing) | Hook, `env` mode (composing) | Hook, `bin` mode (**non-composing**) | Trampoline: `ocx exec` re-entry (**composing**) |
|---|---|---|---|---|---|---|
| Entry added | link + trampoline written, stamp rewritten | same | same (heal-before-emit) | same | **no render.** Stamp mismatches ⇒ **emits nothing** | no file appears until a render, but every existing trampoline composes against the new lock |
| Entry removed | link + trampoline **pruned** | same | same | same | stamp mismatches ⇒ emits nothing | prunes nothing; a surviving trampoline whose name the composition no longer provides fails at resolution (65, item 22) |
| Entry digest changed (`ocx update`, or a `git pull` that rewrites `ocx.lock`) | link repointed; body unchanged (no digest is ever baked) | same | same | same | **link healed** (lock-only readlink walk), stamp still gates `bin/` | **follows it with no re-render** — the digest comes from the lock at re-entry |
| Group removed or renamed | whole `<group>/` pruned; trampolines for the default group only | same | same | same | stamp mismatches ⇒ emits nothing | follows the new default group; prunes nothing |
| Tool key renamed | old link + trampoline pruned, new pair written | same | same | same | stamp mismatches ⇒ emits nothing | the new name needs a render; the old one fails at resolution (65) |
| `binaries` / `entrypoints` claim changed (a name appears or disappears) | trampoline set recomputed from the surface; links untouched | same | same | same | stamp mismatches ⇒ emits nothing | a name the composition does not provide fails at command resolution (item 22) |
| `pinned` flipped | links **deleted** when true / re-rendered when false; **no body changes** — `pinned` is read at re-entry | same | same | same | stamp mismatches ⇒ emits nothing | **takes effect immediately**, with no re-render |
| `activate` flipped | no tree change — rendering is independent of `activate` | no tree change | no tree change | emission changes only | emission changes only | unaffected — `activate` is not a composition input (item 11) |
| `toolchain-dir` set or unset | renders into the **newly resolved** home; the abandoned tree is **left in place, never deleted** (see below) | same | same | same | stamp is per-home, so the new home simply has none until a render | unaffected: the baked selector names the **project**, which `toolchain-dir` does not move, so even an abandoned tree's trampolines still compose correctly |
| Branch switch at the same path, different lock | full converge | same | same | same | link heal + stamp gate; mismatch ⇒ emits nothing | **follows the switch with no re-render**; a name the new lock does not provide fails at resolution (65) |
| Project deleted or moved | n/a | n/a | n/a | n/a | n/a | the baked absolute project root stops resolving: exit 64 (`NoProject`) until a re-render. The `projects/` ledger still prunes a keyed tree when it reports that entry dead (predecessor ADR) |
| Global vs project tier | `--global` selects the home; otherwise identical | identical | identical | both tiers | project tree gated per project; the two global session entries are unconditional | the body renders `--global` instead of `--project '<root>'`; otherwise identical |

**The `bin`-mode stale window is designed, not accidental — and it is tested as such.** The prompt path deliberately does not compose, because composing per prompt is what the 4.8 ms budget forbids. So between a lock change and the next composing trigger, a `bin`-mode shell has a rendered tree the stamp no longer matches. The designed outcome is **emit nothing and print one hint naming `ocx pull`** — not emit a stale entry, and not silently repair. Stale trampolines are therefore **not pruned on the prompt path**. The window closes at the next `pull`, `add`, `remove`, `lock`, `update`, `env`, `exec` or `direnv export`.

**The abandoned-tree rule.** Render and prune act **only inside the resolved home**. Changing `toolchain-dir` re-resolves the home; ocx never deletes a tree at a location it no longer resolves to, because a mistaken `[managed]` push would otherwise destroy trees across a fleet. The abandoned `<project>/.ocx/toolchain/` is self-contained, gitignored and safe to delete by hand, and `ocx shell state` prints which home is live. A stale PATH segment pointing into the abandoned tree is exactly the **lost-ledger repair residual** the predecessor ADR already states, and is not re-litigated here.

### Test layering

| Layer | Where | What it proves |
|---|---|---|
| **1. Unit — `render` as a pure function** | `#[cfg(test)]` beside the renderer | `render` is a function of (lock, surface, config) alone: a **golden-tree snapshot per matrix row**; **idempotence**, `render(render(x)) == render(x)` (item 12); **prune exactness** — nothing outside the computed set survives and nothing inside it is missing (item 31); the stamp fingerprint covers names, bodies and link targets and **ignores mtime** (item 32) |
| **2. Acceptance — the staleness harness** | `test/tests/test_toolchain_activation.py`, per `subsystem-tests.md` | One fixture repository with **two branches carrying different locks**. For each matrix row: switch branch, run each trigger, assert the on-disk tree *and* the emitted PATH. **Every row must be shown observably stale before the trigger and converged after** — a green that never saw red is not a check (`quality-core.md`, *Unchecked Green*) |
| **3. Parity oracle** | `test/tests/test_env.py` (item 11) | Following and pinned emit the same PATH modulo path spelling, for **every** composing emitter, under all three `activate` values |
| **4. Shell matrix** | the reconciler's existing per-shell tests | The `bin`-mode stamp gate across bash, zsh, fish, pwsh, nu and elvish: on mismatch every shell emits nothing (items 8, 28, 29, 35) |
| **5. Windows CI leg** | the existing Windows job | Junction repoint, the `.exec` sidecar's write and read (item 15), case-folded dedupe (item 16), and the **Windows deferred-loading** coverage the lifted floor now requires (item 15a) |
| **6. GC** | acceptance (items 23, 25) | `ocx clean` never touches a live tree; `--force` can collect a package a link still names, which the trampoline path re-installs and the dereference consumer does not (item 15) |

**Negative controls — the mutations that must turn each layer red.** Without these the suite is a habit, not a check.

| Mutation | Must red |
|---|---|
| Renderer skips the prune pass | item 31 (orphan link and orphan trampoline both survive), and every matrix row whose delta is a deletion |
| `RenderStamp.link_fingerprint` removed | item 27, the branch switch — `bin/` bytes are identical, so only the link fingerprint can catch it |
| Stamp written *before* the entries instead of last | the crash-mid-render row: a partially rendered tree reports as complete |
| Stamp fingerprint includes mtime | item 32 — a fresh clone that rewrites timestamps invalidates a correct tree |
| Prompt path made to prune | item 35 — the designed stale window disappears, and the 4.8 ms budget goes with it |
| `ShimNameShadowsOcx` refusal restored | item 16 |
| `cfg!(windows)` floor restored in `resolve_for_host` | item 15a |
| The OS fallback restored at `env.rs:825-829`, or a trampoline directory left on the resolution PATH | item 22 — the fork loop returns |
| The `unset` dropped from the body, or the shim's two `SetEnvironmentVariableW` strips removed | item 38 — an exported `OCX_GLOBAL=1` makes every project trampoline exit 64 |
| `--pinned` (or any composition flag) baked into the body | item 33 — flipping `pinned` in `ocx.toml` stops taking effect without a re-render, and item 11's parity breaks with it |

A mutation that fails to red means a second guard is covering the same property, not that the check is weak — keep mutating until one reds.

## Validation

Every check must be demonstrated **red and green** on inputs under test control. § *Testing Strategy* above organizes these by mutation class and test layer; the numbering here is the stable reference.

1. **Trampoline symlinked elsewhere.** Symlink a rendered trampoline from an unrelated directory, invoke through it, assert the same home composes. Red: derive the home from `$0` instead of baking the selector, then invoke through the symlink.
2. **Trampoline body injection.** *(rewritten 2026-09-05, D-9 — the baked value is the project root, not the group/entry pair.)* Render a project whose directory path contains `$(touch /tmp/pwned)` and a backtick: assert the generated body carries that path inside single quotes and that nothing executes on invocation. Assert separately that a project root containing a `'` or a newline is **refused at render**, naming the path. Red: emit the root inside double quotes and observe the substitution run on every invocation. The group-name and `[tools]`-key charset is item 17; it no longer defends the body (§ *Reserved and validated names*).
3. **`REG_EXPAND_SZ` on Windows.** Seed `HKCU\Environment\Path` with a `%SystemRoot%`-bearing entry, run `self setup`, assert the type is `REG_EXPAND_SZ` and the seeded entry still expands. Red: write `REG_SZ`.
4. **Registry idempotency.** Two consecutive `self setup` runs leave a byte-identical `Path` value with each directory present once.
5. **Hostile `OCX_HOME` per format.** Four homes — containing `\n`, `$`, `&`, `%` — each asserted against each of the three writers: refused with the offending path named (newline/`$` for `environment.d`, `%` for the registry), XML-escaped and still loadable for the plist. Red: write the raw value and observe the injected variable line, the mangled expansion, or the silent plist no-load.
6. **macOS merge happens at load, not at setup.** Seed `launchctl getenv PATH`, run `self setup`, then change the session value to include a foreign segment, then load the agent again: assert the resulting PATH carries the two OCX directories once each in front **and the foreign segment added after setup**. Red: bake the composed literal into the plist at setup time and observe the foreign segment gone. A second load does not duplicate either directory.
7. **LaunchAgent permissions.** A plist written with any mode other than 0644 is caught by the test — launchd's failure mode is a silent non-load.
8. **Committed hostile `bin/`.** A fixture repository force-commits `.ocx/toolchain/bin/cmake`; grant consent; assert that on the first prompt in `bin` mode **no** PATH entry is emitted (no render stamp) and the hostile file never reaches PATH; then `ocx pull` and assert it is replaced or pruned and the entry is emitted. Red: emit the entry without the stamp check and observe the hostile binary PATH-front.
9. **`bin` mode emits three entries, in order.** Assert `ocx_install_bin_path`, then the project's `<home>/toolchain/bin`, then `$OCX_HOME/toolchain/bin`; that a bare `ocx` lookup from a trampoline started without `OCX_BINARY_PIN` resolves to ocx's own binary; and that a tool present in both toolchains resolves to the project's.
10. **`env` mode emits no *project* trampoline dir.** Assert the project's `<home>/toolchain/bin` appears in no `env`-mode entry, while its tree is nonetheless rendered on disk. Scoped to the project tier deliberately: the two global session entries are desired in every mode, and item 28 asserts exactly that.
11. **Parity oracle.** `test/tests/test_env.py` asserts `ocx env` ≡ `ocx exec` in both lanes and under each of the three `activate` values.
12. **Render idempotence and following-stability.** Two consecutive `ocx pull` runs produce a byte-identical tree. After `ocx update`, every `bin/<name>` body is byte-identical and the link repointed. **The positive control that proves the byte-identical assertion discriminates cannot be a `pinned` flip any more** *(D-9 — a body bakes no digest, so `pinned` leaves it identical; item 33 asserts that on purpose)*: use the one input that does rewrite a body, the **baked selector** — re-render the same project from a different absolute directory (move or copy the checkout) and assert every body changed. Note what is *not* a control: setting `toolchain-dir` moves the home but not the project, so it produces a fresh tree whose bodies are byte-identical to the old one's.
13. **Prune.** `ocx remove` a tool; its trampoline is gone and no other name changed.
14. **Heal after `git pull`.** A committed `.ocx/toolchain/` pointing outside `$OCX_HOME/packages` is healed before emitting on every composing emit path; a consent-refused project's tree is untouched; **`--dry-run` performs no heal** (asserted by observing the poisoned link still present afterwards).
15. **The `.exec` sidecar, and a collected package.** *(rewritten 2026-09-05, D-9.)* Two arms, both built here. **Grammar**: a `<stem>.exec` holding an absolute project root, and one holding the literal `global`, each dispatch `ocx --project "<root>" exec` / `ocx --global exec`; the five shared read rules reject an over-cap, multi-line, NUL-bearing or non-UTF-8 body exactly as they do for `.shim` (exit 78), and a value that is neither absolute nor `global` is refused. Red: accept a relative value and observe the child resolve a project from the shim's cwd. **Collected package**: `ocx clean --force` a package a `<group>/<entry>` link names, then invoke its trampoline — it **succeeds**, because the re-entry installs on miss; then assert the *dereference* path is the one still stale, and that the next composing emit heals it. Red: give the trampoline a `launcher exec` verb against the link and observe exit 64.
15a. **Windows deferred loading, end to end.** *(added 2026-09-05, owner decision D-2; producer re-anchored 2026-09-05 by D-9.)* On a Windows host with `lazy-mode = "always"`: the floor no longer forces `Never`, every deferred tool gets a `.shimref` in its **shim slot** (the trampoline bakes no identifier and produces none), first invocation materializes the package through `launcher shim`, and the resulting environment is identical to the eager composition. Red: keep the `cfg!(windows)` branch in `resolve_for_host` and observe the mode silently downgrade with only a debug line.
16. **Name collisions never refuse.** *(rewritten 2026-09-05, owner decision D-4.)* A package claiming `ocx`, `Ocx`, `sudo` or `pkexec` renders a trampoline like any other name, emits a debug line, and appears in `ocx inspect` with its shadowed rivals. Assert exit 0 and the file present. Red: keep the `ShimNameShadowsOcx` refusal and observe exit 65. On a case-insensitive filesystem, `Make` and `make` dedupe to one entry with the last-walked winner.
17. **`bin` reservation and charset.** `[group.bin]`, `bin = "…"` in `[tools]` and in `[group.<g>].tools`, and a group named `a b` each exit 78. Red: remove the validator and observe `[tools] bin` parse cleanly.
18. **`toolchain-dir` refusals.** A root outside `$HOME` and outside `$OCX_HOME` exits 78 **even when it is owner-owned and mode 0700** — the containment check, asserted on `/tmp/ocx-tc` created by the test user. `/`, `/usr`, `C:\Windows` each exit 78; a directory under `$HOME` with mode 0777 exits 78; `~/.cache/ocx/toolchain` is accepted. Red: drop the containment check and watch `/tmp/ocx-tc` pass.
19. **`owned_prefixes` scope.** With `toolchain-dir` set, assert the reconciler owns `<toolchain-dir>/<project-key>/toolchain/` and **not** `<toolchain-dir>`; assert a sibling project's segment is never removed; assert no project prefix is owned before consent is granted.
20. **`activate` matrix.** All six cells, asserting what the reconciler emits.
21. **Ladder precedence.** `--pinned` beats `ocx.toml`; `ocx.toml` beats `OCX_TOOLCHAIN_PINNED`; an unrecognized `OCX_TOOLCHAIN_ACTIVATE` warns once and leaves the floor in effect.
22. **No self-referential loop (mise v2026.3.18 class).** *(rewritten 2026-09-05, D-9 — the re-entry is now `ocx exec`, which resolves on a PATH that the trampoline directories are themselves on, so this stops being an inherited property and becomes a contract this work must write.)* Two changes on the `ocx exec` path, both asserted here:
    - **Command resolution excludes both trampoline directories.** `process_env.resolve_command` (`crates/ocx_cli/src/command/toolchain_exec.rs:329` → `crates/ocx_lib/src/env.rs:800`) runs `which_in` over the composed child `PATH`; the **lookup** copy drops `$OCX_HOME/toolchain/bin` and the resolved home's `toolchain/bin`. The **child's** `PATH` is unchanged, so a tool that spawns another tool still resolves it through a trampoline — nesting works, self-reference does not.
    - **The OS fallback becomes an error for a bare name.** `resolve_command` today returns `PathBuf::from(command)` with a `log::warn` when `which_in` finds nothing (`env.rs:825-829`), which hands the name to `execvp` and the **ambient** PATH — the trampoline that just ran. That fallback becomes `env::CommandResolutionError` (65) for a name carrying no path separator. A path-bearing command (`./hello`) keeps today's behaviour.

    Assert: a trampoline invoked under a name the composition does not provide exits **65** and spawns nothing; with the global and a project trampoline directory both on PATH, invoking a name present in both terminates in one hop; and a tool that invokes a *sibling* tool by name still succeeds through the child's PATH. Red: keep the fallback, remove the package binary from the composition, and observe the fork loop. ([mise v2026.3.18](https://newreleases.io/project/github/jdx/mise/release/v2026.3.18) shipped exactly this hang.) Separately, a lazy shim slot's unclaimed name stays `ShimNameNotClaimed` (65, `shim.rs:318-325`) — untouched by D-9.
23. **Registration invariant.** Rendering a project tree creates its `projects/` ledger entry (positive); rendering the global tree creates none (negative); both survive `ocx clean`; no `refs/symlinks/` back-reference appears for any toolchain link.
24. **Read-only checkout.** Render skipped, digest paths composed, exit 0.
25. **GC.** `$OCX_HOME/toolchain/` survives `ocx clean` and `ocx clean --force`.
26. **Repointed link with `bin/` preserved.** *(added 2026-09-05, cross-model gate; trampoline arm struck 2026-09-05 by D-9)* Render a project in `bin` mode, then — leaving every byte under `bin/` untouched — repoint `<group>/<entry>` at a **different, validly installed** package under `$OCX_HOME/packages`. Assert the hook either stays inert or repairs the link before emitting. Red: fingerprint only `bin/`, and observe the stale stamp match and the repointed link reach PATH unhealed. **The "wrong package runs through the trampoline" arm no longer applies** and must not be asserted: a trampoline composes from the lock, so it runs the lock-selected package whatever the link says. What the link still decides is every **dereference** value the hook emits, which is what this item now protects.
27. **Branch switch at the same path.** Same project directory, two branches whose locks select different digests for one tool. Switch branches without running `pull`, open a fresh prompt: assert the stamp mismatches and the link is healed or the entry withheld, never a silent dereference to the other branch's package. Paired positive assertion *(D-9)*: invoking the trampoline directly across the switch runs the **new** branch's package with no `pull` at all, because the composition reads the new lock.
28. **Fresh shells in all three modes.** A first prompt in a shell with no prior OCX ledger, under `env`, `bin` and `none`: the two global session entries survive in every case, and only the `bin` row adds the project's `<home>/toolchain/bin`.
29. **All six mode transitions.** For each ordered pair drawn from `{env, bin, none}`, change the mode and reconcile: assert the two global entries are present before and after, that the project entry appears exactly in the `bin` states, and that a **foreign** PATH segment planted by the test is never removed in any transition. Red: leave the global entries out of the desired set in `env`/`none` and observe `repair_owned_segments` strip the session-PATH registration (`plan.rs:97-135`) — this is the round-1 H9 defect, and this item is its regression test.
30. **Deregistration is subtractive.** On each platform, plant a foreign segment beside the two OCX segments, run `deregister_session_path`, and assert the foreign segment is still there and only the two OCX ones are gone. Red on macOS: `launchctl unsetenv PATH` and observe the whole value disappear.
31. **Prune exactness, all four orphan classes.** *(added 2026-09-05.)* Starting from a converged tree, apply each mutation and assert the tree equals `render(...)` exactly — nothing outside the computed set survives, nothing inside it is missing: remove an entry (orphan link **and** orphan trampoline both gone); rename a group (whole old `<group>/` gone); rename a tool key (old pair gone, new pair present); drop a `binaries` claim so a name disappears (trampoline gone, link untouched). Red: skip the prune pass and observe each orphan survive.
32. **Stamp fingerprint coverage and mtime independence.** The fingerprint changes when an entry name, a launcher body, or a `<group>/<entry>` link target changes; it does **not** change when only mtimes are rewritten. Red for the second half: fold mtime into the fingerprint and watch a `touch -r`-shifted but byte-identical tree invalidate.
33. **`pinned` flip changes the links and nothing in `bin/`.** *(rewritten 2026-09-05, D-9.)* Flip `pinned` false → true: all `<group>/<entry>` links are deleted, `bin/` still renders, and **every trampoline body is byte-identical** — no digest is baked. Flip back: the links reappear. Both directions assert the **link half** of the stamp changed and the `bin/` half did not. Then the behavioural half, which is the point of not baking: with `pinned = true` and **no re-render at all**, invoke a trampoline and assert it composes digest paths; flip back in `ocx.toml`, invoke again, assert link paths — the flag took effect through the ladder at re-entry. Red: bake `--pinned` into the body and observe the second invocation still emit digest paths.
34. **Abandoned tree on a `toolchain-dir` change.** Set `toolchain-dir` on a project that already has `<project>/.ocx/toolchain/`: the new home renders, the old tree is **still present and untouched**, and `ocx shell state` names the new home. Red: prune the old location and observe a tree deleted by a config change alone.
35. **The `bin`-mode stale window.** With a converged tree and a consented project in `bin` mode, mutate the lock without running any composing trigger, then open a fresh prompt: assert **no PATH entry is emitted**, one hint naming `ocx pull` is printed, and the stale trampolines are **still on disk** (the prompt path does not prune). Then run `ocx pull` and assert the entry appears. Red: emit on stamp mismatch and observe the stale entry PATH-front.
36. **Project deleted or moved.** Delete a project directory that had a `toolchain-dir`-keyed tree; assert the keyed tree is pruned once the `projects/` ledger reports the entry dead, and that a sibling project's keyed tree is untouched. Paired *(D-9)*: a trampoline whose baked project root no longer exists exits **64** (`NoProject`) with a message naming the path, and never falls back to a cwd walk.
37. **Re-entry overhead on the trampoline path.** *(added 2026-09-05, D-9.)* Measure the ocx-side cost of a trampoline invocation — from the trampoline's `exec` to the child's first instruction — against a direct `ocx exec -- <tool>` and against invoking the package binary by absolute path, on a fixture with a realistic tool count. The number lands in the NFR row *Latency — trampoline exec*, on both POSIX and Windows. **This is the owner's 10 ms trigger** (§ *Deferred*, composed-env cache): above 10 ms of ocx-side overhead the follow-up issue is filed. Red control for the harness itself: measure a no-op binary invoked directly and confirm the instrument reports near zero, so a green is distinguishable from a measurement that never ran.
38. **The baked selector is the only selector.** *(added 2026-09-05, D-9.)* With a rendered **project** trampoline, export `OCX_GLOBAL=1` in the caller's environment and invoke it: assert the project's own toolchain composes and the exit code is 0. Repeat for a **global** trampoline with `OCX_PROJECT` exported at an unrelated project. Repeat both on Windows, where the strip is the shim's two `SetEnvironmentVariableW(name, NULL)` calls rather than the body's `unset`. Red: drop the `unset` (or the strips) and observe **exit 64** — `check_global_project_exclusivity` (`crates/ocx_cli/src/app/context.rs:1217-1228`) refuses the baked selector beside the ambient one, so every trampoline on that PATH breaks for a caller who exported one variable.

## Implementation Plan

Work packages are file-disjoint. Contract-first: WP-1 and WP-2 define the types every other package compiles against.

| WP | Scope | Depends on | Files (disjoint) |
|---|---|---|---|
| **WP-1** | `ToolchainStore` as a `FileStructure` field; `ToolchainHome` value + `resolve_toolchain_home`; `.gitignore`; GC-exempt declaration; render-stamp accessors on `StateStore` | — | `file_structure/toolchain_store.rs`, `file_structure.rs`, `file_structure/state_store.rs`, `project/toolchain_home.rs` |
| **WP-2** | Generic `Ladder<T>`; `ActivateMode`; `OCX_TOOLCHAIN_ACTIVATE` / `OCX_TOOLCHAIN_PINNED` readers; **`env::CommandResolutionError` and the fallible `resolve_command`** (item 22 — the bare-name OS fallback at `env.rs:825-829` becomes an error), plus the one compile-forced adaptation in `ocx_lib`: `update_check.rs:511` keeps today's behaviour by mapping the error back to the literal `ocx` **at that call site**, which is the point — the fallback stops being ambient and becomes one caller's stated choice | — | `ladder.rs`, `activate.rs`, `env/keys.rs`, `env.rs`, `package_manager/tasks/update_check.rs` |
| **WP-3** | `ocx.toml` `activate`/`pinned`; `bin` reserved as group **and** tool name; the group-name and `[tools]`-key charset validator | WP-2 | `project/config.rs`, `project/error.rs` |
| **WP-4** | `config.toml` `toolchain-dir` + `OCX_TOOLCHAIN_DIR`; all three parse refusals — **containment inside `$HOME`/`%USERPROFILE%` or `$OCX_HOME`** (the load-bearing one), root/system-prefix, ownership/permission; `Config::merge` wiring; schema regen | WP-2 | `config/*.rs`, `crates/ocx_schema` |
| **WP-5** | Name set: `interface_shim_names` refactored to return per-name ownership and moved to `exposed_names`; not-enumerable policy parameter; **the `ocx` refusal relaxed to a debug note and `PackageErrorKind::ShimNameShadowsOcx` deleted**, flipping `interface_shim_names_refuses_the_literal_ocx_name` (`prepare_lazy.rs:666`); **the Windows shim-slot `.shimref` producer** *(re-anchored here by D-9 — the trampoline bakes no pinned identifier and produces none)*, which WP-6's floor removal is conditional on | WP-1 | `package_manager/tasks/prepare_lazy.rs`, `package_manager/error.rs`, new `tasks/toolchain_names.rs` |
| **WP-6** | Trampoline bodies: `unix_trampoline_body` (the `unset` + single-quoted home selector); Windows `.exe` + the **new `.exec` sidecar** — writer, reader clause, `WIRE_SUBCOMMAND_EXEC`, the root-flag-first child-line arm in `build_child_command_line`, and the `OCX_GLOBAL`/`OCX_PROJECT` strip before the spawn; **removal of the `cfg!(windows)` floor in `LazyModeLadder::resolve_for_host`** (`lazy.rs:255-268`), whose precondition is WP-5's producer landing in the same wave; one new paired byte-exact golden | WP-1, WP-5 | `package_manager/launcher/body.rs`, `launcher/generate.rs`, `lazy.rs`, `crates/ocx_shim/src/{core,main}.rs` |
| **WP-7** | Renderer: whole-compose pass, `bin/` reconcile with prune, link writer, render stamp (names + bodies + link targets), `--dry-run` report | WP-1, WP-3, WP-4, WP-5, WP-6 | `package_manager/tasks/render_toolchain.rs` |
| **WP-8** | CLI: `pull` render + `--dry-run`; `add`/`remove`/`lock`/`update` re-render; `options::Pinned`; `shell state` resolved-home field; **the trampoline-directory exclusion from `ocx exec`'s lookup PATH and the three CLI `resolve_command` call sites** WP-2's signature change forces (`toolchain_exec.rs:329`, `command/exec.rs:166`, `launcher/shim.rs:163` — each propagating 65 instead of handing a bare name to `execvp`) | WP-2, WP-3, WP-4, WP-7 | `command/pull.rs`, `command/toolchain_env.rs`, `command/toolchain_exec.rs`, `command/shell_state.rs`, `options/pinned.rs`, `command/exec.rs`, `command/launcher/shim.rs` |
| **WP-9** | Reconciler: import per `activate`; unconditional global session entries; two-entry `bin` mode; stamp gate + lock-only link heal; consent-scoped `owned_prefixes` | WP-2, WP-3, WP-4, WP-7 | `activation.rs`, `command/self_group/activate.rs` |
| **WP-10** | `self setup --toolchain-activate` writing the global `ocx.toml` via `project::mutate` (create-if-absent) | WP-3 | `setup.rs`, `project/mutate.rs`, `command/self_group/setup.rs` |
| **WP-11** | Session PATH: per-format encoding validators; Windows registry + idempotent merge + broadcast; Linux `environment.d`; macOS LaunchAgent with the **load-time merge script**; subtractive `deregister_session_path` | WP-1 | `setup/session_path.rs`, `setup/session_path/{windows,linux,macos}.rs` |
| **WP-12** | Acceptance tests (§ *Validation*), parity oracle extension | WP-8, WP-9, WP-11 | `test/tests/test_toolchain_activation.py`, `test/tests/test_env.py` |
| **WP-13** | Documentation surfaces + predecessor-ADR amendments + rule updates | WP-8 | `website/src/docs/**`, `.claude/rules/**`, `.claude/artifacts/adr_project_toolchain_links.md` |

DAG: `{WP-1, WP-2}` → `{WP-3, WP-4, WP-5, WP-6, WP-11}` → `WP-7` → `{WP-8, WP-9, WP-10}` → `{WP-12, WP-13}`.

**One same-wave edge, and it is a release precondition rather than a compile one** *(D-9)*: WP-6 removes the Windows `lazy-mode` floor, which must not ship without WP-5's Windows shim-slot `.shimref` producer. The two are file-disjoint and both sit in wave 2, so they parallelize — but neither lands alone, and item 15a is the check that says so.

**Contract stubs each prerequisite exports** — *(added 2026-09-05, cross-model gate; WP-7's dependency on WP-3 and WP-4 was missing, and WP-8/WP-9 inherited the hole)*. A wave compiles only against names an earlier wave has already stubbed, so each of these is a signature landing in wave 1 or 2 with `unimplemented!()` behind it:

| Exported by | Stub the later waves compile against |
|---|---|
| WP-1 | `ToolchainStore`, `ToolchainHome`, `resolve_toolchain_home(&Path, &Config) -> Result<ToolchainHome>`, `StateStore::{render_stamp, set_render_stamp}`, `RenderStamp` |
| WP-2 | `Ladder<T>`, `ActivateMode`, `Pinned`, the two env readers, `env::CommandResolutionError` and the fallible `resolve_command` signature |
| WP-3 | `ToolchainConfig::{activate, pinned}` as *fields on the parsed `ocx.toml` type*, `ProjectErrorKind::ReservedGroupName`, `validate_entry_name` |
| WP-4 | `Config::toolchain_dir() -> Option<&Path>` and the `ConfigError` variant its refusals return |
| WP-5 | `exposed_names(&[ClosureNode], NotEnumerablePolicy) -> Result<BTreeMap<BinaryName, NameOwner>, _>`, `NameOwner`; the deletion of `PackageErrorKind::ShimNameShadowsOcx` |
| WP-6 | `unix_trampoline_body(&TrampolineTarget) -> String`, the `.exec` sidecar writer and `WIRE_SUBCOMMAND_EXEC`, `LazyModeLadder::resolve_for_host` without the Windows floor |
| WP-7 | `render(home, desired, stamp) -> Result<Vec<(RenderedItem, RenderOutcome)>>`, and the `heal_links(home, lock) -> Result<usize>` that WP-9's hook path calls |

WP-7 needs WP-3 for the parsed `activate`/`pinned` fields it renders from and WP-4 for the resolved home's root; WP-8 and WP-9 consume all three. Without those edges, a renderer worktree starting in wave 2 would compile against config types that do not exist yet.

## Deferred (named, out of scope)

- **cwd-aware global trampolines** (Option A). Blocked on deciding how consent is evaluated inside a per-exec path.
- **Composed-env cache keyed by (lock digest, config hash)** for make-heavy hookless use. A performance optimization with an owner-set trigger (2026-09-05): file a follow-up issue **only if** the measured trampoline re-entry overhead exceeds **10 ms**. D-9 makes this the most likely of the deferred items to be reached, because every invocation now composes; **validation item 37** is the measurement and the number lands in the NFR row *Latency — trampoline exec* in WP-12.
- **Shim-slot ↔ trampoline unification.** `ShimStore`'s `bin/` and `toolchain/bin` generate near-identical launchers from one name set into two trees. It **cannot land in WP-6**: the two trees have different lifetimes and different roots — a shim slot is a **GC root held by a lock pin** (`CasTier::Shim`, retained by any registered project's lock), while `toolchain/bin` is derived state outside the GC graph entirely. Unifying the *writers* without unifying those two lifetimes would put a GC-rooted artifact and a disposable one behind one publish path. Kept deferred.
- **`pull --lazy-mode` cleanup.**
- **Per-group exposure (`expose = [groups]`).** Note what D-9 already settles about it: adding the key changes which names render and what `ocx exec` composes, and touches no sidecar byte and no trampoline body (D2, *Extensibility*).
- **A compiled POSIX proxy** in place of the `sh` trampoline, if fan-out latency justifies it.
- **`ocx self uninstall`**, which would give `deregister_session_path` a CLI surface. Tracked as [#413](https://github.com/ocx-sh/ocx/issues/413).
- **A shadow-name denylist or warning tier.** YAGNI. Tracked as [#414](https://github.com/ocx-sh/ocx/issues/414). The trigger to revisit is a real-world analogue of npm bin-entry harvesting ([safedep](https://safedep.io/google-dep-confusion-bin-harvesting/)), where a transitive dependency's declared bin entry silently shadows a system tool.
- **A use-time path-component walk for `toolchain-dir`** — reject symlinks and NTFS reparse points on every component, refuse any ancestor writable by a principal other than the user, and on Windows evaluate the effective DACL rather than comparing the owner SID. Containment inside the user's home (§ *`config.toml` placement key*) buys most of the property for a fraction of the portability risk, so this waits for a concrete demand.

## Residual-Risk Register

| # | Risk | CWE | Status |
|---|---|---|---|
| R1 | A repo-writable directory (`<project>/.ocx/toolchain/bin`) that can hold attacker-authored executables until the first `ocx pull` renders and prunes it — and that an IDE, `.envrc` or `$GITHUB_PATH` recipe may put on PATH outside ocx's consent model | CWE-426 / CWE-427 | **Mitigated for `bin` mode, accepted elsewhere.** The render stamp makes an unrendered `bin/` unemittable by the hook. Every D8 recipe is documented as "after `ocx pull`". ocx has no visibility into IDE-managed PATH composition, so the IDE path stays a stated residual |
| R2 | Live-writer TOCTOU on a link tree in a hostile repository | CWE-59 / CWE-61 / CWE-367 | **Accepted, pre-existing** (predecessor ADR). Mitigation for a hostile live repository is the pinned lane |
| R3 | Sidecar tampering | CWE-59 | **Mitigated, stronger than the prior art.** 32 KiB cap, byte-exact grammar, digest-pinned identifiers, component-wise post-canonicalization containment, reader re-validates independently. **One stated narrowing for the new `.exec` sidecar** *(D-9)*: its clause is an absolute project root or the literal `global`, so there is no package root to contain — a tampered value redirects the re-entry at another project's `ocx.toml`, which is the repo-writable-`bin/` class R1 already covers and the render stamp already gates for `bin` mode |
| R4 | **Name squatting: a root's or a transitive dependency's `binaries` claim shadows a system tool, `ocx` itself, or a privilege name on a PATH the user opted into** | CWE-426 | **Accepted, owner decision 2026-09-05** (§ Decided, D-4). No denylist, no refusal, no warning — a project may legitimately pin its own `ocx` and shadow the name for itself, so a refusal would break a supported case. What bounds it instead: the names come from a lock the user pinned to digests, the hook path is consent-gated, and putting a trampoline directory on PATH is the user's own act. Every collision is debug plus `ocx inspect`, last walked wins. Revisit trigger: a real analogue of npm bin-entry harvesting ([safedep](https://safedep.io/google-dep-confusion-bin-harvesting/)) — *Deferred* carries the issue |
| R5 | Session-PATH files written with wrong permissions or unescaped content | CWE-732 / CWE-77 | **Mitigated.** Owner-only-write hygiene; per-format encoding refusals and escaping; the LaunchAgent 0644 requirement has its own test |
| R6 | Windows System PATH always precedes User PATH | — | **Cannot promise.** Stated in user docs |
| R7 | A digest root a running process already resolved through is collected by `ocx clean` | — | **Pre-existing GC-vs-running-process exposure**, unchanged. *(Reworded 2026-09-05: under D-9 no trampoline bakes a digest root, so the exposure is the ordinary composed-path one, reached through a trampoline like through any other emitter.)* |
| R8 | **Hostile concurrent writer in a project home on Windows.** Junction repoint is remove-then-rename with a bounded non-atomic window, and its convergence argument assumes cooperating ocx writers (`symlink.rs:283-300`) | CWE-367 / CWE-59 | **Accepted, newly stated.** A writer inside the repository can win that window. Mitigation is the pinned lane, as for R2 |
| R9 | **`toolchain-dir` under an attacker-writable or system directory**, or under a directory with a writable **ancestor** that can be renamed or substituted between parse and use | CWE-379 / CWE-732 / CWE-367 | **Mitigated at parse by containment** *(strengthened 2026-09-05)*. The root must resolve inside `$HOME`/`%USERPROFILE%` or `$OCX_HOME`; root and system prefixes stay refused; the root must be owner-owned and not group- or world-writable. **Residual:** an ancestor the user has themselves made writable inside their own home. The stronger form — a use-time component walk rejecting symlinks and reparse points, with Windows effective-ACL evaluation — is *Deferred* |
| R10 | **No fleet trigger for session-PATH registration.** `[managed]` can push `toolchain-dir` and `activate` but cannot run `ocx self setup`; an operator must invoke it in user context | — | **Accepted, stated.** Documented as an Intune/GPO user-context script step |

## Open Questions

**None.** Q4 was answered on 2026-09-05 and is recorded as D-9 below; D-1 to D-8 were closed the same day.

## Decided 2026-09-05

Every question this record raised, and every item it had left to the owner, was answered on 2026-09-05. Recorded here so the reasoning survives the decision.

**D-1 — the name set is metadata-declared, and dependencies are in.** *(Closes former Q1.)* The owner's words: *"we do not add what is on PATH — that would require detecting executables on PATH at install time; we use the metadata description of packages including their dependencies, which are all entrypoints and binaries (the metadata claims) on the interface surface. It is very important that dependencies are included (if on the dependent's interface surface)."* The decision brief's "dependency binaries excluded" gloss is struck. The rendered `bin/` uses exactly the shipped surface algebra — `inspect::admitted_on_surface` then `composer::carrier_crosses` — so `ocx env`, `inspect --closure`, `prepare_lazy` and `bin/` cannot disagree on what a name resolves to. § *Name set* is the contract.

**D-2 — the Windows lazy-mode floor is lifted inside this work.** *(Closes former Q2. Producer re-anchored 2026-09-05 by D-9.)* `LazyModeLadder::resolve_for_host` forces `Never` on Windows only because nothing writes a `.shimref` (`crates/ocx_lib/src/lazy.rs:255-268`, whose comment names the removal condition exactly). **The decision stands; its producer moved.** Under D-9 a trampoline bakes no pinned identifier and therefore emits no `.shimref` — so the first Windows `.shimref` writer is the **shim-slot arm of `prepare_lazy`**, which WP-5 now carries, and the floor removal in WP-6 is conditional on it landing in the same wave. Windows deferred loading still ships here, with its own acceptance coverage (validation item 15a) and its own commit subject. The record's earlier "trampolines are the first `.shimref` producer" claim is **struck**.

**D-3 — `activate` stays an `ocx.toml` key.** *(Closes former Q3.)* The toolchain file declares the toolchain's *shape*; `config.toml` declares whether the hook loads at all. A project that pins `activate = "env"` outranking a user who prefers `bin` is the intended reading, not a defect. The clean-shell case is still served: for the global tier through the user's own global `ocx.toml`, and for every project that leaves the key unset through the `OCX_TOOLCHAIN_ACTIVATE` floor.

**D-4 — no denylist, no refusal, no warning on name collisions.** The owner: *"not required; a project may pin its own ocx and shadow `ocx` for itself, so everything must work; at most a warning, never refuse; YAGNI."* And since a warning on a legitimate steady state is itself forbidden, the tier is debug plus `ocx inspect`. `PackageErrorKind::ShimNameReserved` is never introduced, and the **shipped** `ShimNameShadowsOcx` refusal is relaxed in the same work package — shim slots and trampolines follow one rule. Accepted as residual R4, with the npm bin-harvesting precedent as the revisit trigger.

**D-5 — the `handshake_toolchain_cli.md` §4 reading is confirmed.** *(Strengthened by D-9.)* A trampoline is not a stale per-tool env render: it carries no composed environment in its bytes, it **re-enters `ocx exec` against its own home** on every invocation — so the environment is composed from `ocx.toml` and the lock at call time — and it is healed or re-rendered on every path that could put it on PATH. D-9 makes the reading stronger rather than weaker: a trampoline now reaches the *same* composition the handshake's `eval "$(ocx --global env --shell=sh)"` reaches, by a different door. The Metadata section keeps the reconciliation as the record of that decision.

**D-6 — naming rule: bare inside `ocx.toml`, explicit outside it.** Keys stay `activate` and `pinned` because the file *is* the toolchain. Outside it the noun is spelled: `OCX_TOOLCHAIN_ACTIVATE`, `OCX_TOOLCHAIN_PINNED`, `ocx self setup --toolchain-activate`. The per-call `--pinned` on `ocx env` / `ocx exec` stays bare — both are toolchain-tier commands. `OCX_LAZY_MODE` is not an exception: lazy mode spans the package layer too, so the bare noun is correct and no rename is planned (owner, 2026-09-05; struck the earlier *Deferred* entry).

**D-7 — the session-PATH work package ships with the rest.** It was flagged as gateable because it is the only arm writing outside `$OCX_HOME` and the only one with three per-platform failure modes. The owner chose to ship it together. Every per-platform validation item stays.

**D-8 — `ocx self uninstall` stays deferred, tracked as [#413](https://github.com/ocx-sh/ocx/issues/413).** `deregister_session_path` remains a library contract with no CLI surface, and the manual removal locations stay documented.

**D-9 — a trampoline runs in the configured toolchain environment.** *(Closes former Q4.)* The owner's words: *"toolchain env is correct."* D2 is rewritten accordingly: a trampoline re-enters `ocx exec` against its own home — baked selector, `unset OCX_GLOBAL OCX_PROJECT` first — instead of `ocx launcher exec` against a `<group>/<entry>` link, so it runs in the composed toolchain environment rather than one package's closure.

Two follow-up questions the owner asked, answered here because the answers are the contract:

- ***Would it run `ocx env --activate env [--pinned]`? Should those arguments be added?*** **No.** Alignment is by construction, not by argument: the trampoline calls the same command, which loads the same `ocx.toml` through the same ladders. Every composition input — `pinned`, `lazy-mode`, group selection, project `[env]` — is read at re-entry, so a config edit takes effect with no re-render, and there is no baked value that can drift. **Validation item 11 is the oracle** (`ocx env` ≡ `ocx exec`, both lanes, all three `activate` values); `activate` is not a composition input at all.
- ***Is this extensible enough for later configuration?*** **Yes, and the seam is `ocx.toml`, not the sidecar.** A future `expose = [groups]`, or a per-hook / per-bin group configuration, changes which names render and what `ocx exec` composes, while every sidecar byte and every trampoline body stays as written. Only a per-**group** bin layout (`<home>/toolchain/<group>/bin/`) would need a new selector, and that is a render-layout decision taken then.

**What it costs, stated plainly:** one full composition per invocation (the reason the env-cache trigger is measured — item 37); one new Windows sidecar, `.exec`, because `.shim`'s extension *is* its `launcher exec` verb; a real loop surface where there was none, closed by item 22; and D-2's `.shimref` producer re-anchored to the Windows shim slots. Every other decision is unaffected.

## Deferred to Owner

Not open questions — decisions already taken that the owner may reverse at handoff.

| Item | Decision taken | Note |
|---|---|---|
| — | Nothing outstanding | Every item previously listed here was decided on 2026-09-05; see § *Decided 2026-09-05*. The `--activate` naming overlap with `ocx self activate` is gone as a side effect of D-6: the flag is now `--toolchain-activate` |
| Scope grown by D-9 | **Accepted 2026-09-05 (autonomous mandate).** The `.exec` sidecar and the shim's env strip ship in WP-6; `resolve_command` becomes fallible for a bare name across all callers (WP-2, WP-8); the Windows `.shimref` producer moves to WP-5's shim-slot arm with WP-6's floor removal gated on it (same wave, item 15a) | A consequence priced and taken, not a question. The sidecar is the first new grammar in the `ocx_shim` wire surface since it froze, and the resolution change touches three call sites beyond the trampoline path (`command/exec.rs`, `launcher/shim.rs`, `update_check.rs`) |

## Amendment — 2026-09-06 (implementation, WP-13b)

**Append-only.** D1–D9 and § *Decided 2026-09-05* are left exactly as they were written; nothing above this line is rewritten. Where the implementation moved a decision, the correction is stated here and the superseded sentence is quoted, so the record shows both what was decided and what shipped. Authority for the corrections is the merged code on the feature branch, cited by symbol; authority for the divergences is `.claude/state/plans/plan_toolchain_activation.md` § *Divergences from the ADR* — untracked by design (`.claude/state/` is gitignored), so it is named rather than linked.

### A. Corrections to this record

| # | What this record says | What ships | Source |
|---|---|---|---|
| **A-1** | **R9** records `toolchain-dir`'s ownership control as *"Mitigated at parse by containment"*, with no platform qualifier: *"the root must be owner-owned and not group- or world-writable."* | **Unix only.** `refuse_unsound_ownership` has a real `#[cfg(unix)]` arm and a `#[cfg(not(unix))]` arm that is a documented no-op returning `Ok(())` — reading a directory's owner SID needs `GetNamedSecurityInfoW`, whose `windows-sys` feature the shipping work package did not own. `ToolchainRootError::NotOwnerOwned`'s doc states the gap by name rather than hiding it, and must not be deleted when the arm lands. **R9's mitigation therefore reads: containment on every platform, ownership on Unix.** On Windows the whole of R9 rests on containment — which matters, because the corporate persona this key exists for lives on Windows | R-W31, R-W30; `crates/ocx_lib/src/config.rs` `refuse_unsound_ownership`, `ToolchainRootError::NotOwnerOwned` |
| **A-2** | The PATH-ordering rule justifies putting `ocx_install_bin_path` first as *"what stops a trampoline's `${OCX_BINARY_PIN:-ocx}` fallback resolving `ocx` out of a repository-controlled directory (CWE-426)"* | **That premise is void, and the ordering is re-justified.** D-V19 replaced the bare-name fallback with an absolute install path baked at render time, so there is no bare-name lookup on that arm to defend. The ordering stands on two other grounds: a user typing `ocx` at a prompt must reach the installed binary and not a repository's, and the bare word `ocx` survives as the render-time fallback of last resort when the install path does not exist — narrower than before, not gone. The CWE-426 argument is retained for **that** residual arm only | R-W28, D-V19; C-060 |
| **A-3** | Nothing. The record discusses `PATH`-segment semantics nowhere | **An empty `PATH` segment no longer resolves a command out of the working directory.** `resolve_command` drops empty segments from its **lookup copy**; the child's `PATH` is passed through untouched. POSIX reads an empty field as `.`, so a trailing or doubled separator anywhere in an inherited `PATH` was a CWE-426 primitive on every bare-name resolution. The change is **not** trampoline-scoped: it is reachable from `ocx package exec` and `ocx launcher exec` through `Env::inherited()`. It ships under its own commit subject and is recorded here because it is a decision, not a refactor | R-W8, D-V15 |
| **A-4** | § *`config.toml` placement key* shows `# toolchain-dir = "%LOCALAPPDATA%\\ocx\\toolchain"  # Windows`, commented out | **Two rules, both absolute.** A **leading `~` only** expands, through the one shipped `config/shell.rs` `expand_against` seam — never a second expander. **No `%VAR%` expansion happens on any platform.** A literal `%LOCALAPPDATA%` value is not a variable: it is a relative path, refused at parse with exit 78. The commented line is illustrative of *placement*, never of *syntax*, and must not reach user documentation as a working form | R-W26, D-V16 |
| **A-5** | D2 and § *Trampoline contract* quote a body of `exec "${OCX_BINARY_PIN:-ocx}" --project '<abs project root>' exec -- "$(basename "$0")" "$@"` under the header `# Generated by ocx at install time. Do not edit.` | **Three changes, all shipped.** (a) The invoked name is `"${0##*/}"`, a shell builtin — a forked `basename` would have resolved through the ambient `PATH`, which is the class this feature exists to close, and the same correction applies to the two pre-existing launcher bodies (D-V3). (b) Re-entry goes through a **baked absolute install path** assigned on its own single-quoted line, `__ocx_binary='<path>'`, then `"${OCX_BINARY_PIN:-$__ocx_binary}"`; the bare word `ocx` survives only as a render-time fallback when that path does not exist (D-V19). (c) **Line 2 is exactly `env::TRAMPOLINE_MARKER`** (`# ocx-toolchain-trampoline`), interpolated from `env.rs` rather than re-spelled, and the loop-guard predicate anchors it to the whole of that line (D-V12, D-V23). The shipped body is quoted in [`system_design_toolchain_activation.md`](./system_design_toolchain_activation.md) § *Launcher bodies* | D-V3, D-V12, D-V19, D-V23; `package_manager/launcher/body.rs::unix_trampoline_body` |
| **A-6** | The macOS removal recipe names the launchd **domain**, `gui/$(id -u)` | **The target is the service:** `launchctl bootout gui/<uid>/sh.ocx.path`, then delete the plist. Booting out the domain tears down the user's whole GUI session. `launchctl unsetenv PATH` stays forbidden, unchanged | D-V26 |
| **A-7** | D-V4 justifies refreshing the Windows shim blob through the documented local flow *because CI re-verifies it with a byte-equality rebuild* | **CI does no such thing, in two ways.** Byte-equality was abandoned — cargo-zigbuild's PE link is nondeterministic — and per [`adr_shim_hermetic_zigbuild.md`](./adr_shim_hermetic_zigbuild.md) Addendum 2 the gate is **provenance and inputs**: a PE-magic check, a size budget, a SHA-256 corruption canary, and SLSA attestation. And `build-windows-shims.yml`'s `acceptance-windows` job builds a **native msvc debug** shim under `OCX_SHIM_BINARY`, so **no job on the pull request ever executes the committed cross-built blob** — functional validation of those bytes lands with the Windows dispatch acceptance work, not on the PR. Neither fact changes D-V4's decision; both change what it may claim CI does | R-W34; `.github/workflows/build-windows-shims.yml` |
| **A-8** | C-038 specifies the Windows registry read as `RegGetValueW` with `RRF_RT_REG_EXPAND_SZ \| RRF_NOEXPAND` | **The shipped flags are `RRF_RT_REG_EXPAND_SZ \| RRF_RT_REG_SZ \| RRF_NOEXPAND`, and the contract's wording was what was wrong.** With the two-flag pair the call returns `ERROR_UNSUPPORTED_TYPE` against the `REG_SZ` value rustup leaves on real machines — which would have made C-038's own validation row, requiring the read to succeed there, unreachable. The **write** stays unconditional `REG_EXPAND_SZ` for registration; **deregistration writes back under the type it read** (D-V30), which C-038 does not cover because it describes registration | R-W38, D-V30; `crates/ocx_lib/src/setup/session_path/windows.rs` |
| **A-9** | The latency budget's supporting field data — *"AV/EDR-managed hosts at 128 ms, up to 578 ms cold"* — carried no citation | **Unverifiable; do not cite it as a budget.** What is published points the other way: mise's own shims documentation says users are *"unlikely to notice a performance difference between shims and `mise activate`"*, and [mise discussion #6279](https://github.com/jdx/mise/discussions/6279) measures `handle_shim` at 609–734 µs against ~80 ms of shell startup. The enforced ceiling is the one in the repository — `RECONCILE_BUDGET_MS = 10.0` in `test/bench/shell_latency.py`, the owner's trigger — and the corporate-host figure is an **unmeasured hypothesis**, not a gate | R-W16, D-V5 |
| **A-10** | Nothing. The record does not state which gate compiles the Windows arms | **Local and CI coverage differ, and the difference is worth writing down.** `task check:windows-cfg` is `cargo check -p ocx_shim` for the two MSVC targets and nothing else — deliberately — so **no local gate type-checks `ocx_lib`'s `cfg(windows)` arms**: the renderer's Windows publish path, the session-PATH writers, `Env::pathext`. On CI they are covered: `verify-basic.yml`'s `smoke-windows` job runs `cargo nextest run --workspace --locked --profile ci` natively on `windows-latest` for every pull request to `main`, so a `#[cfg(windows)]` unit test there is **compiled and executed** and is not an unchecked green. The earlier reading — that no gate at all sees them — was falsified and the widening spike was dropped on that basis | R-W45, D-V34; `taskfiles/rust.taskfile.yml`, `.github/workflows/verify-basic.yml` |
| **A-11** | R1 and R8 bound the hostile-repository exposure to a repo-writable `bin/` and to a junction-repoint race | **One further class was found and closed during implementation, and belongs beside them.** `owned_home` gave a **repo-committed symlink deletion authority over the whole filesystem**: the home is built lexically, the owned prefix was then canonicalised and matched with `starts_with`, so a clone committing `.ocx/toolchain -> /` put `/` in the owned set and the reconciler stripped **every ambient `PATH` segment on every prompt**, in every `activate` mode, with consent granted being the premise the scenario already assumes. Fixed in the shipping package; recorded here because the register is where the class belongs, not because it is open | D-V38 |
| **A-12** | **D-3** promises that *"the clean-shell case is still served: for the global tier through the user's own global `ocx.toml`"*, and the shipping code did not keep it — `activate_mode` had one caller, over a consenting **project's** own file, so a global `activate = "bin"` composed a full env envelope anyway | **The global tier reads `activate`, and the gap is closed.** The per-prompt hook resolves the ladder over `$OCX_HOME/ocx.toml` through the same `activate_mode` and calls `resolve_global_pinned_env` only in `env` mode; in `bin` and `none` it contributes nothing. The gate sits at the hook, **not** inside the resolver, because that resolver's other caller — `ocx --global env` / `ocx --global exec` — is an **explicit user request**: `activate` governs how a toolchain reaches a shell automatically, never what a typed command prints. A missing, unreadable or unparseable global file leaves the **file tier absent** and falls through to `OCX_TOOLCHAIN_ACTIVATE` and then `ACTIVATE_FLOOR`, so a corrupt global config still composes and no prompt ever fails over it. **Consequence worth stating: at the global tier `bin` and `none` are the same `PATH`.** C-059 makes `ocx_install_bin_path` and `$OCX_HOME/toolchain/bin` desired unconditionally in every mode — dropping them would have `repair_owned_segments` delete the registration `ocx self setup` wrote — so under either value the global toolchain stays reachable through its trampolines and nothing else is composed. The two modes part company only at the project tier | `ocx_cli::command::self_group::activate::global_prompt_entries`, `global_activate_mode`; `ocx_lib::activation::activate_mode` |
| **A-13** | WP-7's contracts row names `heal_links(home, lock) -> Result<usize>`, a plain repair count | **The count could not say "I refused".** `Ok(0)` meant both "nothing needed repair" and "a whole-tree symlink gate refused and nothing under `home` was read or written", so no caller could tell them apart and the guard protected only the write path. The return type is now `HealOutcome` — `Healed(usize)` or `Refused(reason)` — carrying `#[must_use = "a refused tree was never entered — reading through it composes an attacker's links"]`; with the workspace's `warnings = "deny"` that makes `heal_links(..).await?;` as a bare statement, the shape both callers used, a **build failure**. Per-entry degradations (lock timeout, read-only tree, failed `readlink`) stay *inside* `Healed` — they are C-067 states the composing side already degrades one entry at a time, not a verdict on the tree. The exposed caller was **not** the composer, whose read-path `refuse_symlinked_home` re-guard already covered it (proved by a mutation that stayed green with the composer's `Refused` arm deleted), but `activation::bin_mode_entry`, which healed, ignored the count and returned `Some(<home>/bin)` for a tree relocated behind a symlinked `<project>/.ocx` by `rename` — inode and mtime preserved, so the stamp still matched, and `bin_stamp_matches` guards only `bin/` itself being a link, never a component above it. `owned_project_home` refuses the same tree one level up in the live session flow, so this was a second-guard gap rather than an open hole; now pinned by `a_refused_heal_withholds_the_entry`. |
| **A-14** | Nothing. § *Deferred*'s composed-env-cache bullet and the NFR row *Latency — trampoline exec* both still carry their pre-#423 text — correctly, since this record is append-only — but no row states what the closure they anticipate actually rested on | **The 10.046 ms → 8.069 ms medians that closed [#423](https://github.com/ocx-sh/ocx/issues/423) without building the composed-env cache ([#424](https://github.com/ocx-sh/ocx/issues/424)) are real, and the reasoning for pinning the *budget* rather than the *mechanism* stands independent of any single number.** An unbuilt cache carries no invalidation bug, where a real one would need to key on the lock, the config (`activate`, `toolchain-dir`), the render stamp, the selected groups, the tier and the platform — which is why `REENTRY_OWNER_TRIGGER_MS` (`test/bench/shell_latency.py`) pins the budget as an assertion, not a promise about how it stays met. **What the closure did not say: both medians were taken on the offline shape only.** `measure_reentry` forces `OCX_OFFLINE` on every series (`shell_latency.py:347-356`); the shipped trampoline body emits no such flag (`launcher/body.rs:285-291`). A review measurement dated 2026-09-06 found the no-flag shape at a 46.7 ms median against the offline shape's 13.4 ms on the same harness with the flag removed — the shipped shape was unmeasured when #423 closed, and the harness itself is being corrected separately, so it is that corrected number, not the 46.7 ms figure, that should be trusted going forward. Neither `:779` nor `:582` distinguishes the two shapes; this row is that distinction, stated once, for both | perf review pass, 2026-09-06; `test/bench/shell_latency.py:347-356`, `:672-706`; `launcher/body.rs:285-291`; [#423](https://github.com/ocx-sh/ocx/issues/423), [#424](https://github.com/ocx-sh/ocx/issues/424) |

| **A-15** | **A-12** records the global tier's `activate` gap as closed, naming the per-prompt hook as where the gate sits | **A-12 gated one of the two emitters.** Two moments put a global environment into a shell — the per-prompt reconciler, which A-12 fixed, and the **login stream** `ocx self activate` emits at shell start, which `$OCX_HOME/env.sh` evaluates. That stream pushed `format_global_env_eval` unconditionally, so a global `activate = "bin"` still composed a full env envelope on every shell start — the very symptom A-12 records as closed, arriving through the other door. An interactive shell repaired itself at its first prompt; a script, an `ssh host cmd`, a git hook and the `sh` arm (which registers no hook at all) never did, which is the process class `bin` mode exists for. **The gate now sits at both emitters, on A-12's own reasoning and by its own resolver** (`global_activate_mode`, one small TOML read on the startup path, infallible by construction so no login can fail over a corrupt file); `ocx --global env` / `ocx --global exec` are untouched and keep composing under any mode. **One thing changed beyond the gate**: the login stream now also prepends `$OCX_HOME/toolchain/bin` itself, ahead of `ocx_install_bin_path` so the installed `ocx` still lands in front. C-059 makes that directory desired in every mode at the prompt, but a login shell had been relying entirely on the OS session-level registration `ocx self setup` writes — absent or ignored in a container, on a host without systemd user env, under `--no-modify-path`, and in any shell that never renders a prompt to be repaired at | `ocx_cli::command::self_group::activate::{run_startup, activation_lines}`; tests `a12_the_login_stream_composes_the_global_env_in_env_mode_only`, `c059_the_login_stream_prepends_the_global_toolchain_bin_in_every_mode`, `c060_the_login_stream_puts_the_install_bin_in_front_of_the_toolchain_bin`, `test/tests/test_self_activate.py` TEST-A12 |

### B. Divergence index — D-V1 … D-V40

Every divergence the implementation recorded, and whether it touches this record's text. "Plan-level" means the routing, file sets or rulings moved and no decision here changed. Two identifiers were once used twice and are renumbered **here**: an earlier `D-V27` reference to the LaunchAgent `__OCX_TESTING_LAUNCHCTL` seam now reads **D-V39**, and an earlier `D-V28` reference to the `environment.d` refusal set now reads **D-V40**.

| D-V | What diverged | Touches this record |
|---|---|---|
| 1 | Loop guard is item 22 **plus** a home-count-independent refusal that identifies a trampoline by the file itself | Narrows item 22 — the exclusion set is still home-derived, never a literal join |
| 2 | macOS plist is a constant template with two validated interpolations and no escaper; refusal set widens to `&`, `<`, `>` and XML-1.0-forbidden controls | Widens D5's encoding refusals |
| 3 | All three POSIX launcher bodies resolve the invoked name with `${0##*/}` | **A-5** |
| 4 | Blob refresh is its own work package through the documented local flow | Decision stands; its CI premise corrected — **A-7** |
| 5 | `RECONCILE_BUDGET_MS = 10.0` named as the enforced ceiling, plus a `bin`-mode row with two non-vacuity assertions | Names the gate behind the latency NFR — **A-9** |
| 6 | `lazy.rs` moves so the `.shimref` producer and the floor removal are one commit | Plan-level; D-2's producer was already re-anchored by D-9 |
| 7 | WP-10 folds into WP-11; the test and documentation packages split | Plan-level |
| 8 | One `ocx_lib` orchestration function sequences `commit` then `render_toolchain` for all four mutating commands | Plan-level (C-054) |
| 9 | The following-lane link-path emission gets its own work package over `composer.rs` | The record's third shipping break, isolated; in flight when this amendment was written |
| 10 | `resolve_toolchain_home` takes the resolved root, not the whole `Config` | Design-level; C-002's two-branch behaviour unchanged |
| 11 | The three `ocx_cli` `resolve_command` call sites move packages | Plan-level (C-057) |
| 12 | C-069's POSIX signal is a marker constant, not the generated header; the Windows signal is `.exec` specifically | **A-5** |
| 13 | `RenderStamp` gains a per-entry record and a project identity; the global stamp lives at `$OCX_HOME/state/`, the project stamp under `state/projects/<key>/` | Sharpens D4's stamp contract |
| 14 | `ToolchainStore::entry` is fallible and validates its own inputs; one grammar serves both tiers; `.gitignore` is ensure-present | Design-level (C-001, C-004, C-013, C-015) |
| 15 | C-069's Windows blob-content clause struck, leaving `.exec` as the sole signal; **`resolve_command` drops empty `PATH` segments from the lookup copy** | **A-3** |
| 16 | `ToolchainRoot::resolve` has no filesystem side effect, accepts a not-yet-existing root, expands a leading `~` only, and refuses `toolchain-dir = $OCX_HOME` exactly | **A-4** |
| 17 | C-015's ASCII case fold applies to the shipped `default` and `all` group checks too | Widens the interface break the reserved-name decision causes |
| 18 | The case fold moves out of `exposed_names` into a sibling pure function; the winner keeps its original spelling | Design-level (C-021, C-025) |
| 19 | The trampoline re-enters ocx through the absolute install path, single-quoted, with a bare `ocx` render-time fallback | **A-2, A-5** |
| 20 | The `OCX_GLOBAL`/`OCX_PROJECT` strip is scoped to the `.exec` sidecar path, not every shim dispatch | Narrows D-9's Windows clause |
| 21 | `TrampolineTarget::Project` and the `.exec` writer refuse a non-absolute project root at render | Design-level (C-028, C-031) |
| 22 | Five wave-2 file-set extensions granted | Plan-level |
| 23 | The POSIX marker predicate anchors to the whole of line 2 | **A-5** |
| 24 | `NameOwner` gains `walk_index`; the fold's winner is the greatest walk index, never the greatest key | Makes D-4's "last walked wins" precise |
| 25 | A non-UTF-8 path is refused at render, at every writer | A refusal this record does not list |
| 26 | macOS deregistration targets the **service**, `gui/<uid>/sh.ocx.path` | **A-6** |
| 27 | `NotADirectory` refuses a `toolchain-dir` root whose nearest existing path is not a directory; the ancestor walk climbs on `ENOTDIR` as it did on `ENOENT` | A third `toolchain-dir` refusal beyond C-017–C-019 |
| 28 | Wave-3 rulings RUL-21…RUL-37 settle what the renderer's contracts left open — including that `pinned = true` suppresses the **whole** link pass, writes and prunes | Corrects the design record's Flow A note 3 |
| 29 | A relative directory is refused for **every** session-PATH format, before the platform dispatch | A refusal C-037 does not name, being a property of the value rather than of a format |
| 30 | Windows deregistration writes back under the type it read | **A-8** |
| 31 | Wave-3b rulings RUL-38…RUL-47 settle the renderer's implementation and review | Plan-level |
| 32 | The quoted-Windows-`PATH`-segment defect was re-diagnosed: the **re-join** loses the quotes, not the split | Correctness fix under D6's reconciler |
| 33 | Wave-3b rulings RUL-48…RUL-61 settle the CLI wiring's stub | Plan-level |
| 34 | The gate-widening spike is **dropped**: its premise was falsified | **A-10** |
| 35 | Rulings RUL-62…RUL-84 — load-bearing: `--project` accepts a **directory**, without which every rendered trampoline exited 74, because the body bakes `--project '<abs project root>'` | A shipped CLI grammar widening this record does not mention |
| 36 | The reconciler's file set gains a stamp constructor and a second hunk in the renderer | Plan-level |
| 37 | The reconciler's summary arm for the two unconditional session directories folded to `-`, so a fresh shell's first prompt printed `ocx: -PATH` on the prompt ocx started setting it; flipped to `+` | A user-visible defect from C-059, fixed |
| 38 | `owned_home` handed a repo-committed symlink deletion authority over the whole filesystem | **A-11** |
| 39 | The generated LaunchAgent carries a `__OCX_TESTING_LAUNCHCTL` seam instead of naming `/bin/launchctl` absolutely | Testability seam; D5 unchanged |
| 40 | The `environment.d` refusal set is derived from systemd's `parse_env_file`, not the man page's prose, and adds `\\` and a leading space | Widens D5's Linux encoding refusals |

## Links

- Predecessor, amended here: [`adr_project_toolchain_links.md`](./adr_project_toolchain_links.md)
- Companion design record: [`system_design_toolchain_activation.md`](./system_design_toolchain_activation.md)
- Research: [`research_toolchain_activation_competitive.md`](./research_toolchain_activation_competitive.md), [`research_toolchain_activation_shell_session.md`](./research_toolchain_activation_shell_session.md), [`research_toolchain_activation_security.md`](./research_toolchain_activation_security.md)
- Related records: [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md), [`adr_declared_binaries_metadata.md`](./adr_declared_binaries_metadata.md), [`adr_lazy_package_loading.md`](./adr_lazy_package_loading.md), [`adr_windows_exe_shim.md`](./adr_windows_exe_shim.md), [`adr_self_setup.md`](./adr_self_setup.md), [`adr_two_env_composition.md`](./adr_two_env_composition.md), [`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md)
- Issues: [ocx-sh/ocx#359](https://github.com/ocx-sh/ocx/issues/359), [ocx-sh/ocx#189](https://github.com/ocx-sh/ocx/issues/189), [ocx-sh/ocx#170](https://github.com/ocx-sh/ocx/issues/170), [ocx-sh/ocx#193](https://github.com/ocx-sh/ocx/issues/193)
- External: [mise — Shims](https://mise.jdx.dev/dev-tools/shims.html), [mise — IDE Integration](https://mise.jdx.dev/ide-integration.html), [mise GHSA-436v-8fw5-4mj8](https://github.com/jdx/mise/security/advisories/GHSA-436v-8fw5-4mj8), [mise v2026.3.18](https://newreleases.io/project/github/jdx/mise/release/v2026.3.18), [rustup Concepts](https://rust-lang.github.io/rustup/concepts/index.html), [rust-lang/rustup#261](https://github.com/rust-lang/rustup/issues/261), [rust-lang/rustup#4224](https://github.com/rust-lang/rustup/issues/4224), [ScoopInstaller/Shim](https://github.com/ScoopInstaller/Shim/blob/main/README.md), [pixi trampolines](https://pixi.prefix.dev/latest/global_tools/trampolines/), [prefix-dev/pixi#2462](https://github.com/prefix-dev/pixi/issues/2462), [winget Links directory](https://github.com/microsoft/winget-cli/discussions/5720), [safedep — npm Bin Entry Harvesting](https://safedep.io/google-dep-confusion-bin-harvesting/), [environment.d(5)](https://man7.org/linux/man-pages/man5/environment.d.5.html), [WM_SETTINGCHANGE](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-settingchange), [launchd.info](https://www.launchd.info/), [Flatpak sandbox](https://github.com/flatpak/flatpak/wiki/Sandbox), [per-user environment deployment](https://cdm.iamcloud.info/docs/Content/Configuration/EV_DeployingEnvironmentVariablesMI.htm)

---

## Changelog

| Date | Change |
|---|---|
| 2026-09-04 | Initial record. D1–D8 captured from the owner-approved decision brief, structured into component contracts, and stress-tested against the three research axes and the current tree. Three open questions raised against settled decisions. |
| 2026-09-05 | **Review round 1 applied** (spec, quality, security, SOTA). Blocks: trampoline body moved to a single-quoted concatenation plus a new group-name/`[tools]`-key charset validator; `owned_prefixes` narrowed to the consented in-scope project home with root/system-prefix and ownership refusals on `toolchain-dir`; a render stamp added so `bin` mode cannot emit an unrendered or foreign `bin/`, plus the two-entry ordering rule that keeps a bare `ocx` lookup off a repo-controlled directory; the macOS LaunchAgent gained a composed-value contract (launchd runs no shell); `deregister_session_path` added as a library contract with the three manual removal locations documented. Highs: `ToolchainStore` (global field) split from `ToolchainHome` (project value) and reconciled across all three records; `env` mode never emits the trampoline dir; case folding on every reserved-name check and on case-insensitive-filesystem dedupe; the denylist WARN deleted in favour of hard refusals for `ocx` and five privilege names; the `bin_scan` fallback dropped as YAGNI; `interface_shim_names` refactored to carry ownership; per-format encoding contracts for the three session-PATH writers; the Windows junction "atomic" claim corrected; `toolchain-dir` interaction with the hardcoded IDE recipe stated with `ocx shell state --format json` as the query contract; a no-self-referential-loop validation item for the mise v2026.3.18 hang class. Warns: Option A rescored (87, B still wins by 18) and Option C's security score justified with the privilege argument moved to a Portability NFR row; anchor and citation fixes (`launcher::shim_body`, second producer of `WIRE_SUBCOMMAND`, `env.rs:609-687`, `exec.rs:276-342`); `$OCX_HOME/bin` corrected to `ocx_install_bin_path` throughout and the old Q3 retired as factually resolved; pixi and winget added as prior art; Flatpak/Snap and the absent fleet trigger added to the cannot-promise and residual lists; three migration breaks enumerated with commit subjects. New Q3 raised: `activate` in `ocx.toml` means a project outranks the user. |
| 2026-09-05 | **Cross-model gate applied** (Codex `sol`, one-shot). C1 — the render stamp now fingerprints the sorted `<group>/<entry>` → digest-root pairs as well as `bin/`, and `bin` mode heals the default group's links (lock-only readlink walk, `lock_scoped` repoint, no compose, no metadata read) before emitting; two validation items added for a repointed link with `bin/` preserved and for a branch switch at the same path. C2 — the two global session entries (`ocx_install_bin_path` and `$OCX_HOME/toolchain/bin`) are **always desired and emitted first in every `activate` mode**, reversing round 1's H9, which would have had the first reconcile strip D5's registration; three validation items added for fresh shells and all six mode transitions. C3 — the macOS LaunchAgent became a `/bin/sh -c` **load-time merge** against the then-current PATH instead of a setup-time snapshot, and deregistration is subtractive on all three platforms with `launchctl unsetenv PATH` explicitly forbidden. C4 — `toolchain-dir` must resolve inside `$HOME`/`%USERPROFILE%` or `$OCX_HOME`, refused at parse otherwise; the writable-ancestor residual is stated and the component walk with reparse-point rejection and Windows effective-ACL inspection is deferred; R9 restated. C5 — WP-7 now depends on WP-3 and WP-4, propagated to WP-8/WP-9, with the contract stubs each prerequisite exports tabulated. |
| 2026-09-05 | **Accepted.** Status flipped under the owner's autonomous-mode directive to implement this record. Issues filed: [#413](https://github.com/ocx-sh/ocx/issues/413) `ocx self uninstall`, [#414](https://github.com/ocx-sh/ocx/issues/414) shadow-name warning tier. The D-9 scope growth (`.exec` sidecar, fallible `resolve_command`, `.shimref` producer in WP-5) is taken as priced. |
| 2026-09-05 | **Owner follow-up on the deferred list.** `OCX_LAZY_MODE` rename struck — lazy mode governs the package layer too, so the bare noun is correct. [#359](https://github.com/ocx-sh/ocx/issues/359) moves from *Deferred* to **closed by this ADR** (`activate = "bin"` is the hookless mode its second signal asked for). Composed-env cache gets an owner trigger: follow-up issue only above 10 ms measured re-entry overhead. `ocx self uninstall` and the shadow-name denylist become GitHub issues. **Q4 opened**: trampoline env — package closure (D2 as written) versus the configured toolchain env (owner expectation); recommendation recorded, decision pending. |
| 2026-09-05 | **Owner decisions applied; open questions go to zero.** D-1 the name set is metadata-declared `binaries` ∪ `entrypoints` of the roots **and every interface-admitted dependency**, never a directory scan, with the owner's wording recorded verbatim and D2's "dependency binaries excluded" gloss struck. D-2 the Windows `lazy-mode` floor (`lazy.rs:255-268`) is removed in WP-6 alongside the first `.shimref` producer, so Windows deferred loading ships here with its own acceptance item and commit subject. D-3 `activate` stays an `ocx.toml` key. D-4 the denylist is dropped entirely — no `ShimNameReserved`, no refusal, no warning tier; the shipped `ShimNameShadowsOcx` refusal (`prepare_lazy.rs:307-312`) is relaxed to a debug note in WP-5, flipping `interface_shim_names_refuses_the_literal_ocx_name` (`prepare_lazy.rs:666`), and name squatting is accepted as R4 with the npm bin-harvesting precedent as the revisit trigger. D-5 the handshake §4 reading is confirmed. D-6 naming rule — keys stay bare inside `ocx.toml`, the env floors become `OCX_TOOLCHAIN_ACTIVATE` / `OCX_TOOLCHAIN_PINNED` and the flag becomes `--toolchain-activate`, with `OCX_LAZY_MODE` noted as the untouched pre-existing exception and its rename deferred. D-7 the session-PATH work package ships with the rest. D-8 `ocx self uninstall` stays deferred. |
| 2026-09-05 (latest) | **D-9 applied — a trampoline runs in the configured toolchain environment.** *(Owner: "toolchain env is correct.")* D2 rewritten: the body is `unset OCX_GLOBAL OCX_PROJECT` then `exec ocx --project '<abs project root>' exec -- "$(basename "$0")" "$@"` (`--global` for the global home), with the selector baked because `toolchain-dir` puts the home where no `$0` walk can find the project, and the `unset` because `check_global_project_exclusivity` (`app/context.rs:1217-1228`) would otherwise make an exported `OCX_GLOBAL` exit 64 on every project trampoline. **Zero baked flags** — `pinned`, `lazy-mode`, group selection and project `[env]` are read at re-entry through the same ladders `ocx env` uses, so alignment is by shared code path and item 11 is the oracle; the owner's "should it pass `--activate env`/`--pinned`?" is answered **no**, and extensibility is an `ocx.toml` seam that touches no sidecar byte. Consequences applied throughout: the linked/pinned/deferred **sidecar split collapses** to one body per name; Windows gains a **third sidecar** `<stem>.exec` (same five shared read rules, clause = absolute path or the literal `global`, verb `WIRE_SUBCOMMAND_EXEC`, root-flag-first child line, plus a `SetEnvironmentVariableW` strip of both selectors) — the earlier "no new grammar" clause is superseded because `.shim`'s extension *is* its `launcher exec` verb; **D-2's `.shimref` producer is re-anchored** from the trampoline to the Windows shim-slot arm of `prepare_lazy` (WP-5), with WP-6's floor removal conditional on it; **item 22 rewritten** as a contract this work must write — the lookup PATH excludes both trampoline directories and `resolve_command`'s bare-name OS fallback (`env.rs:825-829`) becomes `CommandResolutionError` (65), with `update_check.rs:511` keeping the old behaviour explicitly at its own call site; **item 26's trampoline arm struck** (link targets stay in the fingerprint for the dereference consumers and the heal, not for dispatch); item 33 inverted (`pinned` leaves `bin/` byte-identical and takes effect with no re-render); items 1, 2, 12, 15, 15a, 27, 36 adjusted; items **37** (re-entry overhead — the owner's 10 ms trigger) and **38** (the baked selector is the only selector) added; the staleness matrix's last column re-headed *Trampoline: `ocx exec` re-entry (composing)* and every cell rewritten; DD6's asymmetry, DD7, D-5, R3, R7, the errors table, the NFR rows and the `windows-sys` feature count updated. Q4 closed as **D-9**; Open Questions returns to none. |
| 2026-09-05 | **§ Testing Strategy added** (owner request), centred on staleness. States the convergence invariant once — every lock-reading trigger converges the tree to `render(lock, surface, config)`, additions and deletions, links and trampolines, never partially, stamp written last — and names the four orphan classes so prune is part of heal rather than an afterthought. Adds a mutation-by-trigger matrix (twelve mutation classes against six triggers, composing versus non-composing) whose cells give the on-disk delta and the PATH effect; states the **`bin`-mode stale window** as designed rather than accidental (stamp mismatch emits nothing plus a hint, and the prompt path never prunes) and the **abandoned-tree rule** for a changed `toolchain-dir` (render and prune act only inside the resolved home; a tree at a no-longer-resolved location is never deleted). Adds a six-layer test table folding the existing validation items in by number, and a negative-control table naming the mutation that must red each layer. Validation items 31–36 added for prune exactness, stamp coverage and mtime independence, the `pinned` flip, the abandoned tree, the stale window, and project deletion. Item 16 rewritten — it still asserted the denylist refusals that decision D-4 removed. |
| 2026-09-06 | **A-12 recorded: the global toolchain tier honours `activate`.** The per-prompt hook resolves the ladder over `$OCX_HOME/ocx.toml` through `activation::activate_mode` and composes the global tier only in `env` mode, closing the gap between D-3's promise and the shipping code. The gate sits at the hook, never in `resolve_global_pinned_env`, so `ocx --global env` / `ocx --global exec` — explicit user requests — keep composing under any mode. A malformed global file leaves the file tier absent and still composes. Consequence stated: at the global tier `bin` and `none` produce the same `PATH`, because C-059 keeps both session directories desired in every mode. § B renumbered to D-V1 … D-V40 — the two `D-V27`/`D-V28` collisions become **D-V39** (LaunchAgent `__OCX_TESTING_LAUNCHCTL` seam) and **D-V40** (`environment.d` refusal set). |
| 2026-09-06 | **Item 37 measured and answered — the 10 ms trigger becomes a GATE, and no composed-env cache is built.** The NFR row *Latency — trampoline exec* read "Unmeasured; accepted, escalate on measurement"; it is now measured. [#423](https://github.com/ocx-sh/ocx/issues/423) recorded a 10.046 ms median (8.529-14.290 ms, eight observations) and filed the follow-up the trigger asks for. [#424](https://github.com/ocx-sh/ocx/issues/424)'s store probe — `PackageManager::find` answering an already-stored digest from path arithmetic instead of resolving — brought it to an **8.069 ms median (7.800-8.569 ms, eight observations)**, with the spread collapsing 5.761 -> 0.769 ms. **#423 is therefore closed without the cache**: the budget is met, and an unbuilt cache has no invalidation bug, where a real one is keyed on the lock, the config (including `activate` and `toolchain-dir`), the render stamp, the selected groups, the tier and the platform — a stale-environment surface that fails silently. What ships instead is the *budget as an assertion*: `REENTRY_OWNER_TRIGGER_MS` is a third gate in `_reentry_gates` (`test/bench/shell_latency.py`), so the win cannot regress unobserved. Pinning the budget rather than the mechanism is deliberate — it stays true whether a store probe meets it today or a cache meets it later. **Note for a future reader**: the bench forces `OCX_OFFLINE` on all four series, so #424's ~175 ms index dial was never in this number; what moved was `resolve_top_manifest`'s blob read and JSON parse plus a `refs/blobs` upsert, per tool, per invocation. That store probe is load-bearing for this budget. A slower runner abstains rather than flakes — `_budget_gate` rule 3 returns INCONCLUSIVE when the bare-exec floor scatters wider than the margin. |
| 2026-09-07 | **A-15 recorded: the login stream honours `activate` too, and carries the session directory itself.** A-12 gated the per-prompt emitter; the login stream `ocx self activate` emits at shell start kept composing the global environment in every mode, so `activate = "bin"` was inert for every process that never reaches a prompt — scripts, `ssh host cmd`, git hooks, the `sh` arm. The same resolver now gates both emitters. Separately, that stream prepends `$OCX_HOME/toolchain/bin` unconditionally (behind `ocx_install_bin_path`), so C-059's session directory no longer depends on the OS registration alone. `ocx --global env` / `ocx --global exec` unchanged. |
