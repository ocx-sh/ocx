# ADR — CLI Plugin Pattern (Git-Style External Subcommands)

**Status:** Proposed
**Date:** 2026-05-28
**Author:** architect skill (Michael Herwig)
**Supersedes:** —
**Superseded by:** —
**Related:** [`adr_ocx_mirror.md`](./adr_ocx_mirror.md), [`adr_cli_high_low_layering.md`](./adr_cli_high_low_layering.md), [`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md), [`research_plugin_architecture.md`](./research_plugin_architecture.md)

---

## Context

OCX ships multiple binaries today (`ocx`, `ocx-mirror`) and will likely add more in adjacent operator/publisher domains (`ocx-mcp`, `ocx-sbom`, `ocx-sign`). Today `ocx-mirror` is built from `crates/ocx_mirror` but has no install path — it's a developer/CI tool reachable only via `cargo build`. From a user perspective `ocx mirror` exits with "unknown command".

The question: should adjacent tools be **discovered as plugins** (the `ocx` binary dispatches `ocx mirror <args>` to an `ocx-mirror` binary discovered on `$PATH`, in the cargo / git / kubectl pattern), or should each remain an independent CLI invoked directly?

This decision is reversible pre-1.0 but becomes a one-way door for the **public contract** the moment plugins are documented (third-party `ocx-foo` authors depend on it).

### Triggers

- Increasing adjacent-tool surface (mirror today, MCP server / SBOM / signing tomorrow)
- Desire for dogfood: install `ocx-mirror` *via* OCX (`ocx --global add ocx.sh/ocx/mirror`)
- User question: "is this a 1-hour scripting job or real work?" — answer depends on scope choices baked in this ADR

### Non-goals

- Not deciding which tools become plugins beyond `ocx-mirror` (operator-domain tools default to plugin; end-user domain tools default to built-in — but per-tool calls are out of scope here).
- Not re-architecting `ocx_mirror` internals. ADR `adr_ocx_mirror.md` stands: shares `ocx_lib`, co-built with workspace.
- Not introducing a plugin manifest format, sandbox, or capability system.

### Constraints

- **Tech strategy** — clap (already on `>= 4.5.57`), Rust 2024, no new core deps
- **CLI authority** — `handshake_toolchain_cli.md` defines current taxonomy; plugin variant must not collide with reserved names (`package`, `self`, `shell`, `env`, `run`, `add`, `remove`, `lock`, `upgrade`, `pull`, `init`, `index`, `login`, `logout`, `clean`, `direnv`, `about`, `version`, `launcher`)
- **Static-command bypass precedent** — plugins must skip `Context::try_init` (no FileStructure / OCI client init), like `Version` / `Shell::Completion` / `Self_::Activate` do today
- **Env forwarding** — must reuse `OcxConfigView` + `Env::apply_ocx_config` (same contract as every other subprocess spawn)
- **Exit code** — must propagate child status via `utility::child_process::propagate_exit_code` (Unix passthrough, Windows saturate)
- **Pre-1.0** — no API/CLI stability promise; reversibility expected within the next 6 months

---

## Decision

**Adopt the cargo-style external-subcommand pattern**, scoped minimally:

1. **Clap surface** — add `#[command(external_subcommand)] External(Vec<OsString>)` to `command::Command` in `crates/ocx_cli/src/command.rs`. Built-ins always win; unknown names dispatch to `External`.

2. **Discovery** — PATH scan only. No special `$OCX_HOME/bin/` directory. No sidecar manifest. No registry. Plugin binary must be named `ocx-<name>` and resolvable on `$PATH`.

3. **Dispatch** — handle `External` in `App::run()` **before** `Context::try_init()` (joining the four existing static-bypass paths). Resolve binary, build child `std::process::Command`, forward env via `Env::apply_ocx_config(ctx.config_view())`, exec, propagate exit code.

4. **Argument convention** — git pattern: `argv[0] = "ocx-<name>"` (the OS argv[0], set by exec to the binary path), `argv[1..] = user args`. The subcommand name is **dropped**, not re-passed, so `ocx mirror sync` and `ocx help mirror` reach `ocx-mirror` as `sync` and `--help` — identical to invoking the standalone binary directly. (Amended from the original cargo pattern, which re-passed the name as `argv[1]`: a plain clap plugin like `ocx-mirror` — subcommands `sync`/`check`/`validate`, no `mirror` wrapper — would reject its own name as an unrecognized subcommand. Git convention needs no plugin restructuring and matches this ADR's title.)

5. **Help integration** — defer. `ocx --help` lists built-ins only. `ocx <plugin> --help` exec-forwards to the plugin. **No** `ocx plugin list` subcommand in v1. Re-evaluate after 3 months of dogfood.

6. **Shell completions** — known gap; same limitation as cargo/kubectl/pixi. Plugins absent from `clap_complete` output. Accept.

7. **Dogfood roll-out** — package `ocx-mirror` as `ocx.sh/ocx/mirror` on the production registry **after** the dispatch is in place, gated by removing the explicit gate comment in `post-release-oci-publish.yml`. Co-versioned with `ocx` (same release train), no independent cadence.

### What this does NOT decide

- Whether existing built-ins (e.g. `package`, `run`) get **demoted** to plugins (answer: no, never — they are core)
- Plugin API stability commitments (deferred until 1.0)
- Plugin discovery on `ocx --help` (deferred; if needed, kubectl-style `ocx plugin list` separate command)
- Plugin metadata sidecar `{name}.meta.json` (rejected — costly per `--help`, drift-prone, helm-style overreach for OCX scope)
- A central plugin registry, signing, or capability system

---

## Options Considered

| # | Option | Outline |
|---|--------|---------|
| 1 | **External subcommand (chosen)** | Clap `external_subcommand` derive; PATH scan; minimal dispatch |
| 2 | **Keep independent CLIs** | Status quo — users invoke `ocx-mirror` directly; nothing in `ocx` knows about it |
| 3 | **Hard-wired built-ins** | Add `mirror` (and future `mcp`, `sbom`) as native `Command` variants depending on respective crates |
| 4 | **Manifest-driven (helm-style)** | `$OCX_HOME/plugins/{name}/plugin.json`; explicit registration; metadata-rich help |
| 5 | **gh-extension model** | Fixed user dir `$OCX_HOME/extensions/`; `ocx extension install/list/remove` lifecycle |

### Trade-off Matrix

Weighted criteria (1=low importance, 5=high):

| Criterion | Weight | Opt 1 (External) | Opt 2 (Independent) | Opt 3 (Built-in) | Opt 4 (Manifest) | Opt 5 (gh-style) |
|-----------|--------|------------------|---------------------|------------------|------------------|------------------|
| **UX cohesion** (`ocx <thing>` as the entry point) | 5 | 5 | 1 | 5 | 4 | 4 |
| **Implementation cost** (initial + ongoing) | 4 | 5 (~3h) | 5 (zero) | 2 (deep coupling, slow ocx build) | 2 (manifest spec + reader + drift) | 2 (full extension CLI) |
| **Core binary size** | 3 | 5 | 5 | 1 (`ocx_mirror` deps in `ocx`) | 5 | 5 |
| **Independent plugin release cadence** | 2 | 3 (PATH allows it but versions diverge) | 5 | 1 (lockstep) | 5 | 5 |
| **Discoverability of plugins** | 3 | 2 (no list in `--help` v1) | 1 | 5 (native help) | 4 | 4 (`gh extension list`) |
| **Shell completion fidelity** | 2 | 2 (gap, accepted) | 1 | 5 | 3 (could read manifest) | 3 |
| **Reversibility pre-1.0** | 4 | 5 (5-line revert) | 5 | 5 (delete variant) | 2 (manifest schema sticks) | 2 (lifecycle commands stick) |
| **Industry precedent / familiarity** | 3 | 5 (cargo, git, kubectl, pixi) | 4 (subset: each tool standalone) | 4 | 2 (helm only) | 3 (gh only) |
| **Risk of drift / version skew** | 3 | 3 | 5 (no dispatch, no skew) | 5 (lockstep) | 2 (manifest can lie about binary) | 3 |
| **Weighted total** | — | **115** | **89** | **96** | **92** | **88** |

### Rationale

- **Option 2 (status quo)** keeps cost zero but punts on the UX problem permanently. Operator-tool discovery stays a per-tool documentation problem. Rejected: doesn't move the ball.
- **Option 3 (built-in)** drags `ocx_mirror`'s transitive deps (`reqwest`, `octocrab`, `serde_yaml_ng`, `regex`, `sha2`) into every `ocx` install, slows the dev loop, and forces lockstep release. Acceptable for `package`/`run` (core) — wrong for `mirror`/`mcp` (operator).
- **Option 4 (manifest)** is helm's pattern — not gaining adoption outside helm in 2025. Pays a permanent context cost (manifest schema, drift, sidecar reads on `--help`) for a marginal discovery win that gh handles with a single `extension list` command.
- **Option 5 (gh-style)** ships lifecycle commands (`install`/`list`/`remove`) that OCX already has via `ocx package install`. Duplicate machinery. Rejected.
- **Option 1 (external)** matches the Rust-ecosystem standard (cargo since 2015, pixi adopted same pattern in 2025 PR #3968) and is implemented in clap as a one-line derive attribute. The discoverability gap is real but **reversibly addable** later via `ocx plugin list` if dogfood reveals friction.

**Top score: Option 1.** Adopt.

---

## Industry Context

From [`research_plugin_architecture.md`](./research_plugin_architecture.md):

| Tool | Discovery | Help integration | Built-in shadow rule |
|------|-----------|------------------|---------------------|
| **cargo** | PATH scan `cargo-{cmd}`; `$CARGO_HOME/bin` first | `cargo --list` shows built-ins + plugins | Built-ins always win |
| **git** | PATH scan `git-{cmd}` | `git help foo` → `git-foo --help`; not in `git --help` | Built-ins always win |
| **kubectl** | PATH scan `kubectl-{cmd}`; longest-prefix wins | `kubectl plugin list` separate | Built-ins always win |
| **helm** | `$HELM_PLUGINS` fixed dir; `plugin.yaml` manifest | `helm help <plugin>` reads manifest | Built-ins always win |
| **gh** | Fixed user dir `~/.local/share/gh/extensions/` | `gh extension list` separate | Built-ins always win |
| **pixi** | PATH scan + `pixi global` dirs | `pixi --list` shows extensions | Built-ins always win |

- **Established standard:** cargo plugin pattern — Rust ecosystem since 2015
- **Trending 2025:** pixi adopted cargo's exact pattern ([PR #3968](https://github.com/prefix-dev/pixi/pull/3968), [#4365](https://github.com/prefix-dev/pixi/pull/4365)) — most recent Rust tool to validate the direction
- **Universal:** built-ins always shadow plugins; no surveyed tool inverts this
- **Universal gap:** shell completions don't cover external subcommands; accepted by every tool surveyed

OCX choosing Option 1 puts it in the same family as cargo/pixi — high familiarity for Rust users, low surprise.

---

## Consequences

### Positive

- `ocx mirror` works after dogfood roll-out; same for future `ocx mcp` / `ocx sbom`
- Core binary stays slim — adjacent operator tools don't drag deps into `ocx`
- Third-party plugin ecosystem unlocked (`ocx-foo` from any author works)
- Plugins bypass `Context::try_init` — fast dispatch, no FileStructure init for tools that don't need it
- Reuses existing env-forwarding (`Env::apply_ocx_config`) — zero new env-passing logic
- Reversible: removing `External` variant + dispatch = ~10-line revert

### Negative / Risks

- **Shell completion gap** — plugins absent from `clap_complete` output. Mitigation: accept (cargo/kubectl/pixi same); plan v2 dynamic completion via `ocx plugin list --names-only` shell hook if friction observed.
- **Discoverability** — first-time users see no plugin list in `ocx --help`. Mitigation: defer; add `ocx plugin list` only if support requests show this is a real friction point.
- **Version skew** between `ocx` and installed `ocx-mirror`. Mitigation: co-version pre-1.0 (same release train); plan `OCX_PLUGIN_API_VERSION` env protocol post-1.0.
- **PATH exec security** — anything named `ocx-foo` on PATH is exec'd. Mitigation: built-ins always win (no shadowing of core commands); log resolved plugin path at debug level. No new attack surface vs. existing `ocx run` / `ocx package exec` which also exec PATH binaries.
- **Plugin process env = OCX trust boundary.** Plugins inherit the parent env (auth tokens, registry credentials, PATH, HOME) verbatim. This matches cargo / git / kubectl plugin conventions: plugins are first-party or user-vetted extensions of `ocx`, NOT arbitrary user packages. For clean-env semantics use `ocx exec` / `ocx run` which call `child_process::exec` with a constructed env. The trade-off (CWE-214 latent risk) is accepted for plugin discovery UX; users control which plugins they install via `ocx --global add`.
- **`ocx-mirror` distribution change** — must publish to `ocx.sh` (currently gated to `dev.ocx.sh`). Mitigation: explicit phase 3 step; gate stays closed until dispatch lands.
- **Plugin authors depend on argv convention** — git convention drops the subcommand name, so a plugin is just a normal CLI invoked as `ocx-<name> <user args>`; no `argv[1]` name-repeat dependency. Mitigation: document; matches git convention; minimal risk.

### Operational

- **CI** — acceptance test for plugin dispatch must ensure `ocx-mirror` is on PATH (today `OCX_MIRROR_COMMAND` env var; reuse).
- **Docs** — `website/src/docs/reference/command-line.md` gets a "Plugins" section pointing at the dispatch behavior. ADR + research artifact archived.
- **Subsystem rule** — `subsystem-cli.md` Module Map adds `command.rs External variant` row; "Cross-Cutting" section adds Plugin Dispatch heading.

### NFRs

| NFR | Impact | Notes |
|-----|--------|-------|
| **Scalability** | None | One-time PATH lookup per invocation |
| **Latency** | +<5ms typical | One `which`-equivalent PATH walk; no per-`--help` scan |
| **Availability** | Unchanged | Plugin failure isolated to plugin exit code |
| **Security** | Slight increase | PATH exec, mitigated by built-in shadow rule + debug log of resolved path |
| **Cost** | None | No new infra |
| **Operability** | Slight increase | One more thing to teach: "ocx mirror = ocx-mirror plugin". Mitigated by familiar git pattern |

---

## Implementation Sketch

For the eventual `plan_*` artifact / handoff to `/builder`:

### Phase 1 — Dispatch (≈2h)

1. **`crates/ocx_cli/src/command.rs`** — add variant:
   ```rust
   #[command(external_subcommand)]
   External(Vec<OsString>),
   ```
2. **`crates/ocx_cli/src/app.rs`** — add a fifth static-bypass branch after the existing `Self_::Activate` arm (lines ~144-153), **before** `Context::try_init`:
   - Resolve `ocx-{argv[0]}` on PATH (use `which` crate already in workspace, or `std::env::split_paths`)
   - If not found → emit a hint framing `ocx --global add ocx.sh/ocx/<name>` as the official-plugin path, with a generic "put `ocx-<name>` on PATH" fallback for third-party plugins; exit `EX_USAGE = 64`
   - If found → build `std::process::Command`, call `Env::apply_ocx_config(ctx.config_view())`, set argv as cargo does (`[plugin_path, name, ...rest]`), spawn, `propagate_exit_code`
3. **No `Context::try_init`** for `External` — confirms by adding to the same `match` block as the existing bypasses
4. **Plugin not-found message** should mention the canonical install incantation so users self-recover

### Phase 2 — Acceptance test (≈30min)

- `test/tests/test_plugin_dispatch.py`
  - Stage a shell script `ocx-fixture` on PATH that echoes `argv` + selected env vars
  - Run `ocx fixture --foo bar`; assert exit 0, argv layout `[ocx-fixture, fixture, --foo, bar]`, env vars `OCX_HOME` / `OCX_OFFLINE` etc. forwarded
  - Run `ocx no-such-plugin`; assert exit 64, stderr contains install hint
  - Run `ocx mirror` shadow guard test: confirm a real `mirror` built-in could shadow if added later (regression for future-proofing)

### Phase 3 — Dogfood `ocx-mirror` (≈2-4h, after Phase 1 lands)

- In `post-release-oci-publish.yml`, change `variants: '[""]'` → `variants: '["", "mirror"]'`; remove the gating comment
- Write `ocx-mirror`'s `metadata.json` so its `bin/` enters PATH after `ocx --global add ocx.sh/ocx/mirror`
- Update `website/src/docs/reference/command-line.md` with "Plugins" section
- Add subsystem-cli.md cross-reference

### Phase 4 — `ocx plugin list` (deferred; only if friction observed)

- New built-in: `crates/ocx_cli/src/command/plugin.rs` with `list` subcommand
- PATH scan for `ocx-*` (skip names that clash with built-ins); print names + paths
- No sidecar; optionally call `ocx-<name> --plugin-info` for a one-line description (deferred convention)

### Effort summary

| Phase | Time |
|-------|------|
| Phase 1 dispatch | ~2h |
| Phase 2 acceptance test | ~30min |
| Phase 3 dogfood mirror | ~2-4h |
| Phase 4 plugin list (deferred) | ~1h |
| **Total core scope (Phases 1-2)** | **~3h** |
| **Full v1 with dogfood (Phases 1-3)** | **~6h** |

User's "1 hour scripting" intuition is close for Phase 1 alone but understates Phase 2-3 honest acceptance + dogfood work. **Quote `~half a day` for landable v1.**

---

## Reversibility

| Decision | Reversible pre-1.0? | One-way-door after 1.0? | Notes |
|----------|---------------------|-------------------------|-------|
| Add `External(Vec<OsString>)` variant | Yes | Yes (third-party plugins depend on it) | ~10-line revert |
| PATH scan as discovery mechanism | Yes | Yes | Other tools document `$PLUGIN_DIR` and lose the ability to switch |
| Cargo-style argv convention | Yes | Yes | Plugin authors will encode it |
| Env-forwarding via `OcxConfigView` | Yes (set today) | Removing fields is breaking | New fields safe to add |
| Defer `ocx plugin list` | Yes | n/a | Adding it later is non-breaking |
| Defer sidecar manifest | Yes | n/a | Can introduce as opt-in metadata convention later |
| Defer `OCX_PLUGIN_API_VERSION` protocol | Yes (no protocol exists) | Yes if introduced | Address before 1.0 only if friction observed |

### Reversibility plan

Pre-1.0: if dogfood reveals the pattern is wrong, the rollback is mechanical — delete `External` variant + dispatch branch + plugin acceptance test, re-gate `ocx-mirror` publishing. ~30 min revert.

Post-1.0: the contract becomes stable; changes ratchet only by adding (new env vars, new argv positions are unsafe; new env vars are safe).

---

## Open Questions

1. **Plugin not-found UX** — should `ocx mirror` (unknown) suggest `ocx --global add ocx.sh/ocx/mirror`, or generic install help? **Resolved:** the hint frames `ocx --global add ocx.sh/ocx/<name>` as the official-plugin path (first-party plugins publish under `ocx.sh/ocx/*`) and falls back to "put an `ocx-<name>` binary on your PATH" for third-party plugins — no registry probe, since dispatch bypasses OCI-client init and only the official namespace is knowable.
2. **Reserved names** — should we maintain an explicit reserved-name list (`package`, `self`, `shell`, ...) and refuse a plugin that shadows them? Today clap's built-in dispatch shadows naturally. **Lean:** rely on clap; no explicit list until shadowing causes confusion.
3. **`ocx help <plugin>`** — should it forward to `ocx-<plugin> --help`? **Lean:** yes, free behavior — `ocx help foo` parses as `External(["help", "foo"])` today which is wrong. Needs a small special case in dispatch to handle `argv[0] == "help" && argv.len() == 2`. Add in Phase 1.
4. **Plugin-API version env** — defer per above, but record as a 1.0 deliverable.
5. **Windows** — PATH scan must consider `.exe` suffix; `which` crate handles this. No special work needed.

---

## Verification Plan

Hand off to QA after Phase 1 lands:

- [ ] Acceptance test confirms unknown name → exec'd PATH plugin
- [ ] Env vars from `OcxConfigView` reach the plugin's environment
- [ ] Exit code propagation: plugin exits 42 → `ocx` exits 42
- [ ] Built-in shadow: `ocx package` resolves to the built-in even if `ocx-package` exists on PATH
- [ ] Plugin-not-found: exit 64, helpful stderr
- [ ] `ocx-foo --help` forwards (handled by clap automatically once dispatch in place)
- [ ] No regression on `ocx --help` (built-ins still listed)
- [ ] `task verify` green

---

## References

- [`research_plugin_architecture.md`](./research_plugin_architecture.md) — full precedent survey, clap analysis, coupling analysis
- [Cargo Book: External Tools](https://doc.rust-lang.org/cargo/reference/external-tools.html)
- [clap git-derive.rs example](https://github.com/clap-rs/clap/blob/master/examples/git-derive.rs)
- [pixi PR #3968 — adopts cargo plugin pattern](https://github.com/prefix-dev/pixi/pull/3968)
- [`adr_ocx_mirror.md`](./adr_ocx_mirror.md) — keeps `ocx_mirror` as separate binary crate sharing `ocx_lib`
- [`adr_cli_high_low_layering.md`](./adr_cli_high_low_layering.md) — high-level vs OCI-tier layering; plugin pattern slots into the operator side
- [`handshake_toolchain_cli.md`](./handshake_toolchain_cli.md) — current command taxonomy authority
