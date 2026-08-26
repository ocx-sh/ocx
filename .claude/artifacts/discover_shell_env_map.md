# Discover: Shell/Env Subsystem Map

Input to ADR replacing direnv with native `ocx` shell hook. Brief: `.claude/artifacts/brief_env_overhaul.md`.
Map only — no recommendations. Every claim carries file:line.

## 1. Component Map

| Component | File | Role |
|---|---|---|
| Env model | `crates/ocx_lib/src/env.rs` (3646 lines) | `Env` container, `keys::*` OCX_* var names, reserved-namespace gate, `OCX_ENV` forwarding codec |
| Project env decls | `crates/ocx_lib/src/project/env.rs` | `EnvValue{kind,separator,value}`, `ProjectEnv` — `[env]`/`[group.*.env]` in `ocx.toml` |
| Metadata env | `crates/ocx_lib/src/package/metadata/env/{entry,modifier,list}.rs` | `Entry{key,value,kind,separator}` (canonical resolved-entry type — package/ project/ CLI-override all normalize into this), `ModifierKind{Constant,Path,List}` |
| Shell primitives | `crates/ocx_lib/src/shell.rs` (1838 lines) | `Shell` enum (10 shells), `export_path`, `export_list`, `export_constant`, `unset`, `escape_value` |
| Applied-set tracking | `crates/ocx_lib/src/shell/applied_set.rs` | `AppliedEntry` |
| Emission helper | `crates/ocx_cli/src/conventions.rs:217` | `emit_lines(shell, entries)` — the ONE place `Entry -> shell line` dispatch happens |
| Direnv bridge (to be replaced) | `crates/ocx_cli/src/command/direnv_export.rs`, `direnv_init.rs` | `ocx direnv export` (stateless bash export), `ocx direnv init` (writes `.envrc`) |
| Toolchain env command | `crates/ocx_cli/src/command/toolchain_env.rs` | `ocx env` / `ocx --global env` — project or global-pinned composed env, `--shell`/`--ci` eval-safe output |
| Activation lifecycle | `crates/ocx_cli/src/command/self_group/{setup,activate,update}.rs` | `ocx self setup/activate/update` — shim generation, profile wiring |
| Shim bodies | `crates/ocx_lib/src/setup/shims.rs` | Per-shell `env.*` file contents (byte-identical across installs, no install-time substitution) |
| Composition | `crates/ocx_lib/src/package_manager/composer.rs` (6430 lines) | PATH/shim ordering, `resolve_env*`, materialization |

## 2. Env/PATH Data Flow — `ocx.toml` → emitted shell lines

1. **Declaration** — project `[env]`/`[group.<name>.env]` parsed into `project::env::ProjectEnv` (`EnvValue{kind: ModifierKind, separator: Option<String>, value: String}`) — `crates/ocx_lib/src/project/env.rs:44-62`. Bare string = `Constant`; `{type=path}` prepends; `{type=list}` appends with separator. **Literal only, no interpolation** (`project/env.rs:27`).
2. **Resolution/composition** — `composer.rs` `resolve_env*` walks the dependency closure (packages) + project/group `[env]` + `--env` CLI overrides, producing `Vec<Entry>` (`package/metadata/env/entry.rs`), each `Entry{key,value,kind,separator}`.
3. **List-separator reconciliation** — `env::reconcile_list_separators(entries.iter_mut())` (called in both `direnv_export.rs` and `toolchain_env.rs` right before emission) — a `None` separator on a `List` entry inherits from whichever contributor upstream declared one explicitly; W-11 invariant.
4. **Emission** — `conventions.rs:217 emit_lines(shell, &entries)` is the single dispatch point: `ModifierKind::Path -> Shell::export_path`, `Constant -> export_constant`, `List -> export_list(..., separator.unwrap_or(DEFAULT_SEPARATOR))`. Each per-shell arm returns `Option<String>`; `None` means either an invalid key (skip + stderr note) or (List-only, `Shell::Batch`) "shell cannot express this fold" (skip + stderr note) — **`ocx direnv export`/`ocx env --shell` never fail the prompt on a bad entry, they degrade it**.
5. **Consumers of `emit_lines`**: `direnv_export.rs` (always `Shell::Bash`, direnv sources `.envrc` in a bash sub-shell regardless of interactive shell — `direnv_export.rs:14-16`), `toolchain_env.rs` `--shell=NAME` path, `export_ci` (CI sink, different code path entirely — writes to `$GITHUB_ENV`-style files, not shell lines).

### `export_path` per-shell strategy (idempotent move-to-front), `shell.rs:154-318`
- `Bash`/`Zsh`: pure-builtin `${var//pat/repl}` fixpoint loop, no subprocess.
- `Ash`/`Ksh`/`Dash` (strict POSIX): one `awk` invocation, value passed via `ENVIRON` (not `-v`, which decodes backslashes) to stay byte-exact.
- `Fish`: rebuild list dropping exact-string dup; deliberately avoids `fish_add_path` (skips nonexistent dirs, mangles bracket paths).
- `PowerShell`: split on `[IO.Path]::PathSeparator`, drop empties + dup, prepend.
- `Elvish`: `str:join`/`str:split` (requires elvish 0.16+ `str:` module), raw single-quoted string (double-quoted rejects `\$`/backtick as parse error).
- `Nushell`: `$env.PATH` auto-listified since 0.101; other vars stay string, `describe`-guarded to branch string vs list.
- `Batch` (cmd.exe): single-statement `%VAR:search=%` delete + prepend — case-**insensitive** (matches Windows PATH semantics); the one-time non-dedup caveat: a value that was already the unanchored *last* segment isn't relocated.

### `export_list` (`shell.rs:319-426`) — **`Shell::Batch` returns `None` unconditionally**
cmd.exe's only substring-replace primitive (`%VAR:search=replace%`) is case-insensitive with no case-sensitive form (measured: deletes differently-cased matches), so it cannot implement the case-**sensitive** unique-append the fold requires (list elements are opaque option strings — `-DFOO=1` vs `-Dfoo=1` differ). Emitting nothing beats emitting a statement that either deletes the wrong element or grows unbounded on re-source. PowerShell arm uses ordinal `.NET String.Contains/Replace`, not PS `-replace`/`-eq` (those default case-insensitive).

### Shells lacking `eval` — the load-bearing constraint for the ADR
**Nushell has no string `eval`, and its `source` needs a parse-time-constant path** (`setup/shims.rs:7-9`, `self_group/activate.rs:256`, `:404-405`). Every other shell family's shim is a thin loader that `eval`/`source`/`slurp`s `ocx self activate`'s stdout:
- POSIX (`sh`/`bash`/`zsh`): `eval "$(ocx self activate --shell=sh)"`
- Elvish: `eval (ocx --global env --shell=elvish | slurp)` — **capture-then-eval as a positional arg**, not `| eval` (pipe form gives `eval` zero positional args, raises an error) — `self_group/activate.rs:247-252`.
- Nushell: applies activation as **structured DATA** instead — `load-env` from `ocx --format json --global env` (JSON, not shell-line text) — `setup/shims.rs:184`, `:457`.
- PowerShell: `Invoke-Expression`.

## 3. Activation + Regeneration Lifecycle

### `ocx self setup [VERSION]` (`crates/ocx_cli/src/command/self_group/setup.rs`)
Bootstraps latest/pinned ocx into content store, writes per-shell `env.*` shims into `$OCX_HOME`, appends a **managed activation block** (fence `# >>> ocx v1 <hash8> >>>` ... `# <<< ocx v1 <<<`, `rc_block.rs:114-120`, `rc_block::canonical_hash`) to detected shell profiles. Idempotent/diff-gated. `--no-modify-path` / `OCX_NO_MODIFY_PATH` truthy → shims only, no profile write (opt-out NOT remembered between runs — repeat every invocation). `--force` overwrites a dirty (user-edited) block; without it, dirty → **exit 82 `DirtyRcBlock`** (`arch-principles.md` glossary; distinct from `ConfigError=78` — the RC content is valid but user-modified, not malformed).

### `ocx self activate [--shell[=NAME]] [--completion|--no-completion]` (`self_group/activate.rs`)
Context-free execution path — **called before `Context::try_init`** (`activate.rs:65-72`): only constructs `FileStructure::new()` (reads `OCX_HOME` from env, cheap), never pays `ConfigLoader` file walk / OCI client / `PackageManager` init cost. This is the pre-`Context` fast-path constraint any redesigned per-prompt hook must preserve — it runs on **every shell startup**.

Emission order in `emit_activation` (`activate.rs:125-153`), fixed and load-bearing:
1. **Completions FIRST** (`:126-135`) — PowerShell's `clap_complete` output opens with `using namespace`, which `Invoke-Expression` (the pwsh shim's loader) accepts only as the first statement of the whole script.
2. **PATH prepend** (`emit_path_prepend`, absolute resolved `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin` — no `$VAR` reference, resolved from the binary's perspective since `env.sh` sets `OCX_HOME` before invoking).
3. **Global toolchain env eval** (`emit_global_env_eval` -> `format_global_env_eval`) — `ocx --global env --shell=<name>` (or, Nushell, `--format json --global env` applied via `load-env`).

**No `OCX_ACTIVATED` state guard anywhere** — deliberately removed (`activate.rs:143-152`, test `global_env_eval_has_no_activated_guard_for_any_shell:392-401`): an exported guard leaks into child processes (e.g. a VS Code Remote server whose terminals inherit it) and wrongly suppresses activation in a shell that needs it. Idempotency instead comes from every emitted line being **inherently idempotent**: PATH move-to-front (`export_path`, all 10 shells including Batch via substring-delete), constants as absolute sets. **Any replacement per-prompt hook inherits this same constraint** — it cannot rely on a state-guard env var surviving into a child shell as a correctness mechanism.

### `--completion`/`--no-completion` + `OCX_NO_COMPLETIONS` gate (`options::Completion`, `activate.rs:53-61,100-103`)
`Completion::enabled(interactive: bool)` resolution order: explicit `--completion`/`--no-completion` (`overrides_with`, POSIX last-wins) -> `OCX_NO_COMPLETIONS` -> stderr-TTY auto probe (`std::io::stderr().is_terminal()`). The `env.sh`/`env.ps1` shim does its OWN interactivity check (`$-` / `status is-interactive` / `[Environment]::UserInteractive`) and passes the explicit flag — the binary never probes a stderr the shim may have redirected; the auto/TTY path only serves a direct in-terminal `ocx self activate` call. This exact pattern (`--X`/`--no-X` + `OCX_NO_X` + auto-TTY-probe) is the one the brief's confirmed scope item 3 says the new `--hook`/`--no-hook` + `OCX_NO_HOOK` pair must mirror.

### `ocx self update` — Decision 4C refresh (`self_group/update.rs:112-171`)
After a successful binary swap:
1. `setup::shims::refresh_shims` (blocking, `spawn_blocking`) — rewrites drifted `$OCX_HOME/env.*` shim files to current canonical bytes, **destructive-by-design, no warning** (diff-gated: unchanged bytes = no-op).
2. `setup::refresh_profiles` (`setup.rs:689-697`) — re-applies the managed RC block in **heal-only mode** (`apply_fence(force=false, heal_only=true)`, `setup.rs:653,693,705`): never introduces a block where none exists (a `--no-modify-path` install stays untouched forever), never clobbers a user-edited block (stays `SkippedDirty`), only heals a drifted ocx-authored body (`FormatUpgraded`/`Migrated`).

Both steps are best-effort — failure warns + advises `ocx self setup` but never fails the update.

**Regeneration/lag constraint (brief item 4, confirmed):** `refresh_shell_integration_after_swap` runs in the **OLD binary still in memory** — so a brand-new block body / new shim contract introduced by THIS swap does not heal on this hop; it lands on the **next** `self update` (or a manual `self setup` re-run). A shell process started before an update keeps running its old activation logic for its entire lifetime (env shims are sourced once at shell startup, not re-evaluated). This is the exact "hook logic changes land one `self update` later" constraint the brief calls out as a design constraint the ADR must make explicit.

### `$OCX_HOME/env.sh` -> `ocx self activate` handoff (`setup/shims.rs:1-60`)
`env.sh` (and per-family siblings `env.fish`/`env.nu`/`env.elv`/`env.ps1`) are **byte-identical across all installs** — no install-time substitution; `OCX_HOME` resolved at shim-runtime via `: "${OCX_HOME:=$HOME/.ocx}"` (POSIX) / per-shell equivalent. `write_shims` writes all five files atomically with a diff-gate. `refresh_shims` is the same op under an intent-revealing name for the update post-swap hook — **shims are ocx-owned; a refresh never consults user edits** (unlike the RC block, which has a heal vs. dirty distinction). The POSIX shim self-detects bash vs zsh vs plain sh (`_ocx_shell`) so the right clap completion backend is picked (`sh` -> `Dash` has none).

## 4. Composition — PATH Ordering, Shim Slot (`composer.rs`)

**Ordering invariant (`composer.rs:1080-1102`, C-012/S-004):** consumers apply entries by *prepending* — the LAST entry pushed into the `Vec<Entry>` ends up FIRST in resolved PATH. `emit_shim_slot` (`:1096`) pushes a deferred (lazy) tool's shim `bin/` dir as the FIRST (lowest-precedence) entry of its block, before the root's declared vars and before its synthetic `entrypoints/` prepend — so final resolution order is **`entrypoints/` > `bin/` > `shims/`**. Once a tool materializes, `entrypoints/`/`bin/` (previously empty compose-time paths) become real directories that shadow the shim, and the *same already-exported* environment stops routing through it — no mutation, no re-export needed. This is explicitly called out in the brief as "settled and untouched" — the new hook design must not disturb it.

`emit_shim_slot` gates through `carrier_crosses(Entrypoints::IMPLICIT_VISIBILITY, is_root=true, self_view)` (`:185-195`) — absent under `--self` (a package's own private view bypasses launchers; a shim IS a launcher). `self_view` selects which of a root's two surfaces (interface vs private) is emitted; never affects a dependency (deps always get the interface surface, `carrier_crosses` line 192-194: `else { carrier.has_interface() }`).

## 5. Config Tiers

**Discovery + merge pipeline** (`ConfigLoader::load_with_local_view`, `config/loader.rs:120-183`), lowest to highest precedence:
1. `builtin_defaults()` (`:195` — compiled-in constant, e.g. `ocx.sh` index base URL; deliberately NOT gated on `OCX_NO_CONFIG` — a compiled constant is the reproducible part)
2. discovered file chain (system -> user -> `$OCX_HOME`) — skipped entirely when `OCX_NO_CONFIG=1`
3. **managed-config snapshot** (`fold_managed_tier`, `:301` — identity-gated, folds in AFTER the discovered chain but BELOW `OCX_CONFIG`/`--config`; also suppressed by `OCX_NO_CONFIG=1`)
4. `OCX_CONFIG` env var (empty string = escape hatch, treated as unset)
5. `--config FILE` CLI flag

`Config::merge` (`config.rs:145-183`): scalars = `other` wins when `Some`; tables (`registries`, `mirrors`) merge key-by-key; `[managed]`/`[registry]`/`[patches]` merge recursively via their own `.merge()`. Trust policies (`[[trust.policy]]`) APPEND across tiers (union at storage; masking happens later in `trust::resolve` by specificity).

**`deny_unknown_fields` asymmetry — directly load-bearing for the brief's whitelist decision:**
- **`Config` (root `config.toml`, fleet tier) has NO `deny_unknown_fields` ANYWHERE in its tree** — root sections and every nested table (`config.rs:23-31`). Deliberate: `[managed]` makes one `config.toml` fleet-wide state read by many ocx versions at once, so a payload written for a newer binary must degrade to its known parts on an older one rather than failing the whole file. A change that cannot degrade ships under a new OCI tag, not a stricter parser.
- **`ProjectConfig` (`ocx.toml`, project tier) DOES use `#[serde(deny_unknown_fields)]`** (`project/config.rs:28,72,122,259` — root and multiple nested structs) — a typo in one repo's own checked-in file should be caught immediately, the opposite forward-compat tradeoff from the fleet tier.
- This directly underwrites the brief's owner decision ("Whitelist lives in `config.toml`, never `ocx.toml`") — `config.toml`'s forward-compat/fleet-degradation contract is exactly the shape a rollout-tolerant whitelist needs, and it is architecturally already asymmetric from `ocx.toml` in this respect.

**Schema pipeline**: `crates/ocx_schema` is build-only (`Cargo.toml` — a dev/CI tool, not shipped in the `ocx` binary). `crates/ocx_schema/src/main.rs` generates JSON Schema for `{metadata, config, project, project-lock, patch}` via `schemars::JsonSchema` derives already present on `Config` (`config.rs:36`) and written to `website/src/public/schemas/config/v1.json` (checked-in output, versioned by directory e.g. `project-lock/v3.json`). Typo detection for `config.toml` is the published schema's job (IDE/editor tooling), NOT the deserializer (which tolerates unknowns by design, per above).

## 6. State Surfaces — CRITICAL, Named Open Problem

Three DIFFERENT keying/lifecycle schemes coexist today, confirmed by code:

| Surface | Root | Key derivation | Lifecycle | GC owner |
|---|---|---|---|---|
| **`StateStore` flat feature dirs** (`file_structure/state_store.rs`) | `$OCX_HOME/state/{feature}/` | Feature-specific: `update-check/{slug}` = strict slug of `identifier.to_string()` (`:82-88`); `referrers/{registry_slug}.json` = relaxed slug of registry host (`:141-148`); `trust_root/{authority_slug}.json` = relaxed slug of Rekor authority host; `managed-config/` = fixed subpath (no key — one snapshot machine-wide) | TTL-bound / mtime-as-data (update-check: zero-byte file, mtime = last probe time — atomic-touch via `rename`, cross-platform). Content-bearing files (`managed-config/snapshot.json`, `config.toml`, `pause.json`) written via `write_bytes_atomic` | **NONE — confirmed no GC.** `grep` across `package_manager/tasks/clean.rs` finds zero references to `StateStore`/`state/`; `arch-principles.md` glossary explicitly documents `state/{subsystem}/{key}.json` as "not GC-walked". This directory only grows. |
| **`$OCX_HOME/projects/` symlink ledger** (`project/registry.rs`) | `$OCX_HOME/projects/` (flat, one level) | `name_for_path(canonical_abs_project_dir)` = first 16 hex of SHA-256(canonical project dir path) (`reference_manager.rs:59-63` — single source of truth, ledger reuses it, does not reinvent) | Symlink `{16hex} -> /abs/project/dir` (target = the project DIR, never the lock file or config file); existence-and-resolvability of the target dir IS the liveness signal (no JSON, no version, no whole-file rewrite) | **`ocx clean`** — a `ProbeResult::{Live,Dead,Unknown}` three-state liveness check per entry; `Unknown` (transient I/O error — NFS/automount/permission-flip) is NEVER treated as `Dead` (SEC-1 guard against silent data loss); a `Dead` link is pruned. GC-root semantics: implicit `$OCX_HOME/ocx.lock` root also added for the global toolchain (no self-ledger-entry, `adr_global_toolchain_tier.md` D5 amended). |
| **Proposed 3rd scheme** (brief item 5, not yet built) | `state/activation-consent/<project-key>` | Would need its own key derivation — brief flags this as a NEW scheme unless it reuses `name_for_path` | Undefined | Undefined — would need the ledger's GC or its own |

**The brief's framing is precisely borne out by the code**: the ADR must define ONE project-key derivation (the obvious candidate is `ReferenceManager::name_for_path`, already reused once by the projects ledger) and ONE per-project state dir, GC'd off the `$OCX_HOME/projects/` ledger that already tracks project liveness — rather than inventing a third scheme under `state/activation-consent/`. A consent-stamp file keyed the SAME way as the projects ledger (and physically nested under, or cross-referenced from, `$OCX_HOME/projects/{16hex}/` rather than a parallel `state/` tree) would inherit the existing GC mechanism for free; a `state/`-rooted key invents a fourth scheme with no GC at all (per the confirmed-empty grep above).

## 7. Reusable Primitives the Design Must Not Reinvent

- **`utility::path::move_to_front(existing: &OsStr, value: &OsStr) -> OsString`** (`utility/path.rs:80-95`) — `OsStr`-native (no lossy conversion) move-to-front dedup of a PATH-style value: drops empties + existing exact-`OsStr` occurrence, prepends. Uses `std::env::split_paths` (platform separator). This is the SAME algorithm every `Shell::export_path` arm implements per-shell in generated shell code (`shell.rs:154-318`) — the in-process (`ocx run`/child-env) and out-of-process (emitted shell line) forms are required to agree byte-for-byte (documented at `shell.rs:319-345` for the sibling `export_list`/`append_unique` pair). A reconciler applying entries in-process (rather than emitting shell text) should call this directly rather than re-deriving the fold.
- **`utility::fs::lock_scoped(locks_root, scope, guarded_dir, discriminator, timeout) -> LockedFile`** (`utility/fs/scoped_lock.rs:61-72`) — cross-process advisory lock keyed by a guarded directory's filesystem identity (`{device}:{inode}:{scope}:{discriminator}` SHA-256'd to a 2-level sharded path under `locks_root`), homed in a CENTRAL `locks/` root — never a sidecar next to the guarded data (Locking Policy table in `arch-principles.md`: atomic-rename-replaced data whose inode rotates must use `lock_scoped`, never a persistent sidecar). Any new per-project state directory that gets rewritten (not edited in place) needs this, not `LockedFile` directly.
- **`StateStore` path helpers** (`file_structure/state_store.rs`) — the established pattern for adding a new state surface: a typed accessor method returning a `PathBuf` under `{root}/{feature}/`, with the slug/hash derivation documented inline and a doc-comment describing the atomic-write mechanism used (`write_bytes_atomic` for content-bearing files, bare `rename`-touch for zero-byte mtime-as-data files). A NEW `[shell]` session-carrier or consent-stamp surface should follow this same shape rather than inventing a bespoke file layout — but per Section 6, should route its KEY through `name_for_path`/the projects ledger rather than a locally-invented slug, to get GC for free.

## 8. Prior Decision Records — One-liners + Relationship to New ADR

| ADR/doc | Decision | Relationship to new ADR |
|---|---|---|
| `adr_live_env_reload.md` (Proposed, 2026-08-02) | Typed three-way shell-env reconciler (desired/current/`__OCX_ENV_STATE` ledger), per-prompt hook + `ocx` wrapper via `self activate`, `[shell]` config section, consent stamps for untouched projects; explicitly framed as phase 2 of `adr_project_toolchain_links.md` | **Superseded** by the new ADR per the brief header ("Supersedes as framing") — the phase-2 dependency is dropped as not real, but its substance (adversarial round already done: ledger spoof channel closed, reserved-key gate, `set -u` discipline, pwsh ordering, nushell spike gate, 10-shell mechanics matrix) is explicitly "reuse, do not discard" input, not a clean-slate rewrite |
| `adr_self_setup.md` (Accepted, 2026-06-04) | `ocx self setup` bootstrap + env-shim write + managed RC-block fence machinery; `ExitCode::DirtyRcBlock=82`; `self update` 4C refresh | **Amended** — the new ADR's `--hook`/`--no-hook` flags land on `self setup`/`self activate` per the brief (item 3), extending this ADR's flag/env-var pattern (mirrors the shipped `--completion` triad) rather than replacing its mechanics |
| `adr_idempotent_path_move_to_front.md` (Proposed, 2026-06-19) | Idempotent move-to-front PATH manipulation across every emit surface — `move_to_front` + per-shell `Shell::export_path`; explicitly BLOCKS issue #170 (native per-prompt shell hook) as a hard prerequisite | **Untouched / already-landed prerequisite** — this is the idempotency foundation the whole reconciler design depends on; already implemented per `adr_live_env_reload.md`'s own metadata note ("already implemented... recommend closing" re: issue #26) |
| `adr_managed_config_tier.md` (v2 amended 2026-07-05) | `[managed]` config tier as an ordinary OCX package (config-as-package v2); `ocx config push`/`update`; identity-gated local snapshot | **Untouched directly**, but the brief's "managed config may ship the whitelist" / OD-2 (`[shell]` in managed tier) decisions build ON TOP of this tier's existing mechanics — no change to this ADR's own content needed |
| `adr_global_toolchain_tier.md` (Accepted, 2026-05-15; re-anchored by handshake 2026-05-16) | Explicit `--global` toolchain tier, strict isolation, no implicit home fallback; superseded Amendment C of `adr_project_toolchain_config.md` | **Untouched** — orthogonal tier-selection concern; the reconciler operates within whichever tier is already resolved |
| `adr_project_gc_symlink_ledger.md` (Accepted, 2026-05-15) | Flat symlink store `$OCX_HOME/projects/` as project GC ledger (name = 16-hex SHA-256 of canonical project dir via `ReferenceManager::name_for_path`, target = project dir); supersedes `adr_clean_project_backlinks.md` | **Load-bearing prerequisite, likely amended** — Section 6 above shows this is the ONE GC mechanism state surfaces should route through; the new ADR's per-project state layout (brief item 5) should extend this ledger's liveness model rather than add a fourth scheme |
| `handshake_toolchain_cli.md` (Signed 2026-05-16, amended through 2026-08-19) | AUTHORITY for current CLI model — command taxonomy, root `[--global] env [--shell]`, activation via `$OCX_HOME/env.sh`, `ocx shell` reduced to `{completion}` | `adr_live_env_reload.md`'s own metadata notes it "amends `handshake_toolchain_cli.md` §2 `shell hook` deletion" — the new ADR must state explicitly whether/how it re-amends this authority doc (e.g. does `ocx self activate`/`env.sh` still own activation, or does a new hook command supersede parts of §4?) |
| `adr_project_toolchain_links.md` (Proposed, 2026-08-03) | Stable addressing (`.ocx/toolchain/<group>/<entry>` link tree) for composed toolchains — the OTHER initiative the brief explicitly separates ("Non-goal... separate track, no sequencing claim either way") | **Explicitly decoupled** per brief — solves a disjoint problem class (frozen envs / processes that never re-read env, e.g. a running IDE's `JAVA_HOME`) vs. this ADR's per-prompt shell reach. GitHub issue #189. |

**Also present in `.claude/artifacts/` (not individually requested, found by directory scan), relevant but not deep-dived here:** `research_shell_env_reconciler_and_launcher_farm.md` (prior-art survey: mise/direnv/volta/proto/scoop/pyenv-rbenv-asdf/PowerShell/nushell — explicitly named an "input" by the brief), `research_shell_profile_activation.md` (login-profile idempotent-append patterns, nushell no-eval — grounds `handshake_toolchain_cli.md` §4), `adr_project_toolchain_config.md`, `adr_two_env_composition.md`, `adr_project_env_declaration.md`, `adr_env_modifier_types.md`, `adr_patch_env_resolution_uniformity.md`, `adr_ci_env_export_flag.md`, `design_note_project_env_open_questions.md`, `handover_entrypoint_args_env.md`, `research_env_interpolation_patterns.md`, `research_env_list_consumers.md`, `research_project_env_declaration.md` — these govern env COMPOSITION semantics (modifier types, interpolation, patch overlay) rather than the shell-activation/reconciler surface this brief scopes; likely untouched by the new ADR but worth a scan if the design touches `Entry`/`ModifierKind` shapes.
