# Research: Git-Style Plugin Pattern for OCX CLI

**Date:** 2026-05-27
**Domain:** cli, packaging
**Triggered by:** Should `ocx-mirror`, future `ocx-mcp`, and adjacent tools be discovered as PATH plugins (`ocx mirror` dispatches to `ocx-mirror` binary) rather than built-in subcommands or independently-named binaries?

---

## 1. Precedent Survey

### Tool Comparison Matrix

| Tool | Discovery | First-class dir | Help integration | Env injected to plugin | Builtin shadow rule |
|------|-----------|----------------|------------------|----------------------|---------------------|
| **git** | PATH scan `git-{cmd}` | `GIT_EXEC_PATH` | `git help foo` → `git-foo --help`; not in `git --help` | Inherits environment | Built-ins always win |
| **cargo** | PATH scan `cargo-{cmd}`; `$CARGO_HOME/bin` first | `$CARGO_HOME/bin` | `cargo --list` shows built-ins + plugins; `cargo help foo` → `cargo-foo foo --help` | `CARGO` env var set | Built-ins always win |
| **kubectl** | PATH scan `kubectl-{cmd}`; longest-prefix wins | None (pure PATH) | `kubectl plugin list` separate; NOT in `kubectl --help` | None | Built-ins always win; cannot shadow |
| **helm** | `$HELM_PLUGINS` fixed dir; `plugin.yaml` manifest per plugin | `$HELM_PLUGINS` | `helm help <plugin>` reads manifest; no binary invocation needed for help | `HELM_PLUGIN_NAME`, `HELM_PLUGIN_DIR`, `HELM_BIN`, `HELM_NAMESPACE`, `HELM_DEBUG`, `KUBECONFIG` | Built-ins always win |
| **gh** | Fixed user-scoped dir (NOT PATH scan); repo naming `gh-{name}` | `~/.local/share/gh/extensions/` | `gh extension list` separate; NOT in `gh --help` | Token + env forwarded | Built-ins always win |
| **pixi** | PATH scan `pixi-{cmd}` + `pixi global` dirs | `pixi global` bin dirs | `pixi --list` shows extensions alongside built-ins | All args forwarded | Built-ins always win |

### Argument passing convention

(cargo/git/kubectl/pixi): `argv[0]` = binary name (`ocx-mirror`), `argv[1]` = subcommand name repeated (`mirror`), `argv[2..n]` = user's args. Plugins can be written as standalone CLIs; `argv[1]` can be ignored or used to distinguish invocation context.

### Environment forwarding for OCX

At minimum, plugins should receive the full `OcxConfigView` env var set: `OCX_HOME`, `OCX_DEFAULT_REGISTRY`, `OCX_OFFLINE`, `OCX_REMOTE`, `OCX_INDEX`, `OCX_BINARY_PIN`. These are already defined in `crates/ocx_lib/src/env.rs` and forwarded via `Env::apply_ocx_config()`. The dispatch code can reuse that mechanism directly.

### Version/compatibility enforcement

No surveyed tool enforces it at runtime. Cargo relies on semver; helm has a `plugin.yaml` version field but no enforcement matrix. Pre-1.0 OCX can accept this gap; post-1.0 it warrants a `OCX_PLUGIN_API_VERSION` env var convention.

---

## 2. Clap Support Analysis

### `allow_external_subcommands` — derive API pattern

```rust
#[derive(Subcommand)]
pub enum Command {
    // ... all existing 17 variants unchanged ...

    #[command(external_subcommand)]
    External(Vec<OsString>),   // captures any unrecognized subcommand name + args
}
```

When no built-in matches: `External(vec!["mirror", "--spec", "cmake.yml"])` is produced. `args[0]` is always the unrecognized subcommand name.

This is the **exact pattern** used in [clap's canonical git-derive.rs example](https://github.com/clap-rs/clap/blob/master/examples/git-derive.rs). Stable since clap 4.0. OCX is already on `clap >= 4.5.57` — no version floor change needed.

### Interaction with `--help`

- `ocx --help` — static help printed from `Cli::command()`; does NOT list external subcommands. Plugins are invisible unless explicitly added to help text (see §5 below).
- `ocx mirror --help` — routes to `External(["mirror", "--help"])`; dispatch code must detect `--help` and forward to `ocx-mirror mirror --help` (or just exec the binary and let it handle `--help`).
- "Did you mean" suggestions — still fire for mistyped built-in names; truly unknown names route to `External` silently.

### `clap_complete` (shell completions) — known gap

`clap_complete::generate()` is called at runtime against the static `Cli::command()` tree. External subcommands produce no completion entries. `ocx [TAB]` will complete only built-in subcommands. This is the same limitation in cargo, kubectl, and pixi — all accept it. Dynamic completion requires shell-side scripting (e.g., call `ocx plugin list --names-only` from the completion function). Not blocking for adoption.

### `clap_mangen` (man pages)

Same static-only limitation. Plugin man pages ship with the plugin binary.

### `ocx_schema` (JSON schema)

Unaffected — schema generation reads `ocx_lib` types, not clap structure.

### How cargo does it (source-level)

`find_external_subcommand()` constructs the filename `cargo-{name}` + platform suffix, scans `search_directories()` (returns `$CARGO_HOME/bin` first, then system PATH dirs). `execute_external_subcommand()` builds a `ProcessBuilder`, sets `CARGO` env var, calls `exec_replace()` (replaces current process). Help: `cargo help foo` → invokes `cargo-foo foo --help`. [Source: cargo main.rs](https://github.com/rust-lang/cargo/blob/master/src/bin/cargo/main.rs)

---

## 3. OCX Current CLI Structure (Relevant Facts)

From reading `crates/ocx_cli/src/`:

```
Cli {
    context: ContextOptions   // global flags: --global, --project, --offline, --remote, --format, --color, --log-level, --index
    command: Option<Command>  // optional — None → print help
}

Command enum: 17 variants (Env, Add, Clean, Direnv, Index, About, Init, Lock, Login,
                            Logout, Upgrade, Launcher, Package, Pull, Remove, Run,
                            Shell, Version)
```

**Dispatch path:** `App::run()` → static bypass (Version, ShellCompletion, None/help skip `Context::try_init()`) → `Context::try_init()` → `command.execute(context)`.

The static bypass matters: external subcommand dispatch would **not** create a `Context` (file structure, config loader, package manager). Instead it would exec the plugin binary with env vars. This is cleaner — plugins don't need OCX's internal Rust context.

**Minimal implementation change:** add `External(Vec<OsString>)` variant to `Command` enum, handle it in `Command::execute()` before any context operations, scan `$OCX_HOME/bin/ocx-{args[0]}` then PATH for `ocx-{args[0]}`, exec via `std::process::Command` with `OcxConfigView` env vars forwarded.

---

## 4. `ocx_mirror` Coupling Analysis

Grepping `use ocx_lib::` across all `crates/ocx_mirror/src/**/*.rs`:

| `ocx_lib` API used | Module | Nature |
|--------------------|--------|--------|
| `Publisher::push_cascade()` | `publisher` | Internal OCI publish pipeline with cascade tag logic |
| `LayerRef` | `publisher` | Internal bundle reference type |
| `BundleBuilder` | `package::bundle` | Creates OCX bundle archives (internal format, not a user-facing command) |
| `Metadata` | `package::metadata` | Reads/writes OCX metadata JSON |
| `Archive`, `ExtractOptions` | `archive` | Multi-backend archive extraction |
| `Platform` | `oci` | OCX platform abstraction |
| `Digest` | `oci` | Content-hash type |
| `ClientBuilder` | `oci` | Authenticated OCI client factory |
| `annotations` | `oci` | OCX OCI annotation constants |
| `Version`, `BuildTimestampFormat` | `package::version` | OCX version parsing + build timestamp injection |
| `DataInterface`, `Printer`, `LogSettings`, `ColorMode`, `ProgressMode`, `LogLevel` | `cli` | Shared CLI display layer |
| `progress::ProgressManager`, `Spinner` | `cli::progress` | Shared progress bars |
| `ExitCode` | `cli` | sysexits-aligned exit codes |

### Assessment

`ocx_mirror` is a **library consumer of `ocx_lib` internal APIs**, not an end-user tool. It cannot be reimplemented as a plugin that calls `ocx package push` or standard OCI tools. It needs `Publisher::push_cascade()`, `BundleBuilder`, `Archive`, and `Metadata` — none of which are exposed via the `ocx` CLI.

### Practical implication

The plugin model for `ocx-mirror` is about **UX discovery** (`ocx mirror` dispatches to the `ocx-mirror` binary), NOT about technical decoupling. `ocx_mirror` must remain co-built with `ocx_lib`. Independent release cadence is only possible if `ocx_lib` publishes a stable crate to crates.io with semver guarantees — not planned pre-1.0.

### ADR note

`adr_ocx_mirror.md` already documents the decision to keep `ocx_mirror` as a separate binary crate sharing `ocx_lib`. The plugin pattern doesn't contradict this ADR; it extends the distribution story.

---

## 5. Help Integration Options

| Approach | How | Pros | Cons |
|----------|-----|------|------|
| **PATH scan on `--help`** | Scan before printing; append "Plugins" section | Always current | Latency; PATH-state dependent |
| **`$OCX_HOME/plugins/` manifest** | Each plugin writes `{name}.meta.json`; read at `--help` time | Fast, offline-friendly | Plugin cooperation protocol needed |
| **`ocx plugin list` separate command** | Scans and lists; main `--help` unchanged | Clean separation | Discovery problem: users must know the command exists |
| **Hybrid: known dir + fallback PATH** | `$OCX_HOME/bin/ocx-*` first (OCX-managed), then PATH | Fast for first-party; extensible for third-party | Two-phase lookup logic |

### Recommended: Hybrid

Scan `$OCX_HOME/bin/` for `ocx-*` binaries as the canonical first-party plugin location. This is where `ocx package install --select` already puts binaries (via the `symlinks/.../current/bin/` path that `env.sh` puts on `$PATH`). No separate plugin directory needed — OCX's own install mechanism handles it. Show these in `ocx --help` under a "Plugins" section. For third-party, fall back to PATH scan on explicit `ocx plugin list` only (not on every `--help`).

Helm's manifest approach (`plugin.yaml`) is worth adopting for rich help integration: each plugin binary is accompanied by a sidecar `{name}.json` in `$OCX_HOME/bin/` that OCX reads for description/version display without invoking the binary.

---

## 6. Trade-offs for OCX

### Pros

- Smaller core `ocx` binary (excludes `ocx_mirror`'s reqwest, octocrab, serde_yaml_ng, regex, sha2 transitive deps)
- `ocx install ocx-sh/ocx-mirror` as dogfood demonstration
- Third-party plugin ecosystem (`ocx-sbom`, `ocx-sign`, `ocx-audit` written by anyone)
- Plugins bypass `Context::try_init()` — no config load, no FileStructure init for tools that don't need it
- Independent UX versioning (mirror spec format can rev without core CLI changes)

### Cons

- Shell completions for plugins are unsolvable with static clap_complete (accepted by cargo/pixi/kubectl — not a blocker)
- Version skew between `ocx` and `ocx-mirror` when both are co-versioned but installed separately (manageable pre-1.0; plan for post-1.0 compatibility layer)
- Help fragmentation unless manifest convention adopted
- Security: PATH exec risk — mitigated by scanning `$OCX_HOME/bin/` first (OCX-controlled, world-writable risks low since it lives in `$HOME`)
- Testing: acceptance tests must ensure `ocx-mirror` on PATH; more CI matrix complexity

### Hybrid boundary (recommended)

| Built-in (Command enum) | External (plugin) |
|------------------------|-------------------|
| install, pull, lock, upgrade, add, remove, run, env, clean | mirror |
| package group (push/create/inspect) | mcp (future) |
| index, login, logout, init, about, version, shell, direnv | sbom, sign, audit (future) |

Dividing line: **Is this for a day-to-day user of OCX packages, or for an operator/publisher?** Operators can install a plugin; end users shouldn't need to.

---

## 7. One-Way-Door Analysis

| Decision | Reversible pre-1.0? | Reasoning |
|----------|---------------------|-----------|
| Add `External` variant to `Command` enum | Yes | Easy to remove before 1.0; after 1.0, third-party plugins may depend on `ocx-foo` convention |
| Remove built-in subcommand in favor of plugin | No (after 1.0) | Scripts calling `ocx mirror` that update `ocx` without updating `ocx-mirror` break |
| `$OCX_HOME/bin/ocx-*` as canonical plugin location | Borderline | Documenting this location creates a de-facto contract; low-risk since pre-1.0 |
| Env var forwarding convention | Yes pre-1.0 / No post-1.0 | Plugin authors depend on `OCX_HOME` being set; removing it is breaking |
| Plugin manifest sidecar (`.meta.json`) | Yes | Internal convention; easy to extend schema |

### Prototype-reversible path

1. Add `External(Vec<OsString>)` to `Command` enum (~5 lines in `command.rs`)
2. Handle in `App::run()` before `Context::try_init()` — scan `$OCX_HOME/bin/ocx-{name}` then PATH, exec with `OcxConfigView` env vars forwarded
3. Ship `ocx-mirror` as an OCX package (`ocx install ocx-sh/ocx-mirror:latest`)
4. Keep `ocx_mirror` in the same workspace and release cycle — no independent cadence yet
5. Evaluate after 3 months: if the UX is good and no version-skew incidents, formalize the contract for 1.0

This is **fully reversible before 1.0**: remove the `External` variant, make `mirror` a built-in, done.

---

## Industry Momentum Summary

- **Established:** cargo plugin pattern (git-style PATH scan) — the Rust ecosystem standard since 2015
- **Trending:** pixi adopted cargo's exact pattern in 2025 (PR #3968, explicitly cited cargo as reference); confirms the pattern is the right choice for new Rust tool managers
- **Declining:** manifest-registry approach (helm-style `$HELM_PLUGINS`) — more setup friction for plugin authors, not gaining adoption outside Helm
- **Emerging:** dynamic completion integration (running `ocx plugin list` from the shell completion script) — not yet standardized but solvable post-1.0

---

## Sources

| Source | Type | Relevance |
|--------|------|-----------|
| [Cargo Book: External Tools](https://doc.rust-lang.org/cargo/reference/external-tools.html) | Official docs | Canonical cargo plugin model: discovery, arg passing, env |
| [Rust Book ch14-05: Extending Cargo](https://doc.rust-lang.org/book/ch14-05-extending-cargo.html) | Official docs | `cargo --list`, naming, install via `cargo install` |
| [clap git-derive.rs example](https://github.com/clap-rs/clap/blob/master/examples/git-derive.rs) | Source | Exact derive pattern for `#[command(external_subcommand)]` |
| [clap DeepWiki: Subcommands §7.3](https://deepwiki.com/clap-rs/clap/7.3-subcommands) | Analysis (Nov 2025) | ExternalSubcommand variant requirements, wildcard match implementation |
| [Helm Plugins Guide](https://helm.sh/docs/topics/plugins/) | Official docs | `plugin.yaml` manifest, env injection, help-without-binary-invoke |
| [kubectl plugins docs](https://kubernetes.io/docs/tasks/extend-kubectl/kubectl-plugins/) | Official docs | PATH scan, longest-prefix match, shadow conflict warnings |
| [pixi extensions intro](http://pixi.prefix.dev/latest/integration/extensions/introduction/) | Official docs | `pixi --list` shows extensions; `pixi global` as install location |
| [pixi PR #3968](https://github.com/prefix-dev/pixi/pull/3968) | Source PR (2025) | "Follows cargo's proven pattern"; most recent Rust tool to adopt |
| [pixi PR #4365](https://github.com/prefix-dev/pixi/pull/4365) | Source PR (2025) | Adds `pixi global` dirs to search path alongside PATH |
| [gh extension docs](https://docs.github.com/en/github-cli/github-cli/using-github-cli-extensions) | Official docs | Fixed-dir model, `gh extension list` as separate command |
| [clap_complete crate](https://crates.io/crates/clap_complete) | Crate | Static generation; external subcommands produce no completions |
| [cargo main.rs on GitHub](https://github.com/rust-lang/cargo/blob/master/src/bin/cargo/main.rs) | Source | `find_external_subcommand()`, `execute_external_subcommand()`, `CARGO` env var |
