# ADR: Project-Level Toolchain Config (`ocx.toml` + `ocx.lock`)

## Metadata

**Status:** Proposed
**Date:** 2026-04-19
**Deciders:** Architect worker (sion worktree), product owner
**GitHub Issue:** #33 (project-tier config walk — hook already wired at `crates/ocx_lib/src/config/loader.rs:33`)
**Related PR-FAQ:** [`pr_faq_project_toolchain.md`](./pr_faq_project_toolchain.md)
**Related PRD:** [`prd_project_toolchain.md`](./prd_project_toolchain.md)
**Tech Strategy Alignment:**
- [x] Decision follows the Rust-2024 / Tokio golden path in `.claude/rules/product-tech-strategy.md`
- [x] Shell integration reuses existing `ProfileBuilder` (no new export generator)
**Domain Tags:** infrastructure | cli | api
**Supersedes:** none
**Superseded By:** —

**One-way-door severity.** `ocx.toml` and `ocx.lock` become VCS artifacts in every consumer's repository the moment this ships. A later format change requires a migration for thousands of checked-in files, so every decision below is made for v1 correctness, not compatibility.

## Context

OCX already provides the three-store model (objects, index, installs) and a package-manager facade coordinating pulls, installs, and env resolution. What it does not yet provide is a **project-local** statement of "these are the tools this repository needs" — the role that `rust-toolchain.toml` plays for rustup, `package.json` + `package-lock.json` for npm, `Pipfile` + `Pipfile.lock` for pipenv, and `mise.toml` + `mise.lock` for mise.

Adding a project layer is the last unlocked piece of the Golden Path for OCX as a backend tool: GitHub Actions, Bazel wrappers, and developer shells all want a deterministic, committed declaration of the toolchain they run against. Today every consumer re-states tools via CLI args (`ocx install cmake:3.28 shellcheck:0.11 ...`) in scripts, producing drift between CI and dev environments.

A project-tier config hook is already in place:

- `ConfigInputs.cwd: Option<&Path>` exists at `crates/ocx_lib/src/config/loader.rs:33` — wired through, unused.
- `ConfigLoader::discover_paths()` at `loader.rs:103` ignores `_inputs` today; the module doc explicitly flags "future tiers (e.g., project-level `ocx.toml` walk in #33) can be added by extending `discover_paths` without rewriting any other function."

The `[tools]` concept does **not** exist in the ambient `Config` struct today. `Config` only carries `[registry]` and `[registries.<name>]` sections.

### Constraints carried into every decision

1. **Always frozen** — the lock is authoritative whenever it exists; OCX never auto-updates pre-existing state.
2. **Pull on demand, not pre-install** — a present `ocx.toml` triggers `pull` (objects), not `install` (symlinks).
3. **Opt-in for all state changes** — no auto-update, no auto-modify; every state change is an explicit user command.
4. **Nearest-only CWD walk** — not a chain-load; the walk stops at the nearest `ocx.toml` or at `OCX_CEILING_PATH`.
5. **Config loader hook already exists** — extend `discover_paths()` only; no invasive refactor.
6. **Named groups consistent** — the same group vocabulary (e.g., `dev`, `ci`, `release`) appears in `ocx.toml` and in the shell profile.
7. **Breaking changes acceptable** — the next OCX release is already breaking; design for correctness.
8. **Hard prerequisite** — the `feature/dependencies` branch must land first (transitive dependency resolution is required for tools that themselves declare runtime deps).

## Decision Drivers

- **D1. Reproducibility.** Two developers on two machines, two months apart, must resolve the identical set of manifest digests from the same `ocx.toml` + `ocx.lock`.
- **D2. Determinism of the lock file.** Machine-written; sorted alphabetically; no formatting churn on repeated `ocx lock` runs.
- **D3. Merge-friendliness.** The lock file's per-tool entries should form an array of tables (Cargo model) so adding a tool is a localized append, not a whole-file rewrite.
- **D4. Fail-closed on drift.** If `ocx.toml` changes without re-running `ocx lock`, every subsequent `ocx exec` fails loudly rather than silently re-resolving from tags.
- **D5. Hermetic execution is the product center.** `ocx exec` is OCX's value proposition as a backend tool; the project config must make it strictly more useful, not bolt on new modes.
- **D6. Interactive shells are secondary.** Direnv-style activation is a supported use case, but the security boundary is that the shell hook never installs or mutates state — it exports variables for tools already present.
- **D7. Config file names must not collide.** `config.toml` is the ambient tiers; `ocx.toml` is the project. They share the loader but are different structs.
- **D8. No reuse of the `Config` struct for project config.** A project is not just "another tier of ambient config" — its schema has `[tools]`, which has no meaning at ambient tiers (with the one deliberate exception of home-level `$OCX_HOME/ocx.toml` as a "default toolchain", which reuses the **project** schema, not the ambient one).

## Industry Context & Research

Research synthesis from three parallel agents (see `.claude/artifacts/research_project_toolchain_*.md` if persisted):

**Lock file prior art reviewed:** Cargo (`Cargo.lock`), PDM (`pdm.lock` with `content_hash`), mise (`mise.lock` per-platform sub-tables with algorithm-prefixed checksums), pnpm (`pnpm-lock.yaml`), yarn v3 (`yarn.lock`), uv (`uv.lock`), asdf (`.tool-versions`), rustup (`rust-toolchain.toml`), Terraform (`.terraform.lock.hcl`).

**Key findings:**

- **TOML wins for the declaration file** — every comparable Rust-adjacent tool uses TOML; users already edit `Cargo.toml`.
- **TOML wins for the lock file** — though yarn and pnpm use YAML, the Rust/Python ecosystems have converged on TOML for machine-written locks (Cargo, PDM, uv, mise). TOML handles the per-platform sub-table case cleanly via nested tables.
- **mise.lock is the closest analogue** for what OCX needs: per-tool, per-platform checksum/digest entries, algorithm-prefixed (`sha256:...`), with a top-level metadata block.
- **PDM's `content_hash`** is the standard mechanism for staleness detection. PDM hashes the resolved dependency section; we adopt the same pattern scoped to `[tools]` and `[group.*]` only.
- **Cargo's array-of-tables** (`[[package]]`) gives line-localized diffs; use `[[tool]]` the same way.
- **OCI-style platform keys** (`linux-amd64`, `darwin-arm64`, `windows-amd64`) align with the digest format already used throughout OCX and match how `oci-spec` presents platforms.
- **Pixi's group model** is the most developer-friendly: `[environments]` and `[feature.*]` compose groups. We take a simplified two-level version — implicit `[tools]` default group + additive `[group.<name>]` — because OCX does not need feature flags or solver conditionals.
- **Activation prior art:** mise's `mise activate`, asdf's `. asdf.sh`, direnv's `.envrc`, and nix-direnv all converge on a "prompt-hook that exports variables for already-installed tools" pattern. None auto-install during activation; they print an error and point at an install command. This is exactly the constraint we need.

**Trending approaches:** Lock files with algorithm-prefixed digests, per-platform sub-tables, content-hash-based staleness detection, and direnv-compatible shell hooks are the 2024–2026 consensus for project toolchain management.

**Key insight:** The hardest design constraint is *not* the schema — it is the **activation model**. Every previous-generation tool (pyenv shims, nvm, rbenv) that auto-installed on activation created security and reproducibility problems. OCX's "always frozen, opt-in state changes" constraint aligns with the direction mise and nix-direnv are moving toward: activation is strictly a variable exporter.

## Considered Options

This ADR covers five load-bearing decisions. Each is presented with its options and a chosen outcome.

---

### Decision 1: `ocx.toml` schema shape

#### Option 1A: Flat `[tools]` + additive `[group.<name>]`

```toml
# ocx.toml at repo root
[tools]
cmake = "3.28"
shellcheck = "0.11"

[group.dev]
shfmt = "3"

[group.ci]
lychee = "0"
```

Semantics:
- `[tools]` is the implicit default group — always pulled, always exported.
- `[group.<name>]` is additive on top of `[tools]` when `--group <name>` is passed.
- Multiple `--group` flags union the named groups with `[tools]`.
- Conflict (same tool at different tags in two active groups) is a hard error at `ocx lock` / `ocx exec`.

| Pros | Cons |
|------|------|
| Trivially readable; matches `Cargo.toml`'s `[dependencies]` + `[dev-dependencies]` mental model | Requires the union semantics to be documented; users coming from `Pipfile` with nested tables may expect profile-overrides |
| Alphabetical table-name ordering is deterministic | Cannot express "only in group X" (every tool listed under `[tools]` is always-on) — acceptable since that is the intent |
| Group names are plain TOML tables — no subschema | Feature flags / conditional tools (platform-gated) are not expressible; if we need them later we extend the key shape |

#### Option 1B: Pixi-style `[environments]` + `[feature.*]`

```toml
[feature.default]
cmake = "3.28"

[feature.dev]
shfmt = "3"

[environments]
default = ["default"]
dev = ["default", "dev"]
```

| Pros | Cons |
|------|------|
| Explicit environment composition; scales to complex matrices | Two levels of indirection (features + environments); every `ocx.toml` author writes the `default → [default]` mapping boilerplate |
| Matches Pixi, the closest prior art in Rust-adjacent conda-flavored tooling | Over-engineered for OCX's backend-tool focus; we do not have solver conditionals, platform filters, or optional features |

#### Option 1C: Inline per-tool tables

```toml
[tools.cmake]
version = "3.28"
groups = ["default", "dev"]

[tools.shellcheck]
version = "0.11"
```

| Pros | Cons |
|------|------|
| Each tool has a namespaced sub-table for future fields (`platforms`, `optional`) | Verbose for the 80 % case of "just pin a version"; every tool needs at minimum 2 lines |
| Group membership is per-tool not per-section | `groups = [...]` with free-text strings invites typos; no way to validate against a closed set of group names |

**Chosen: Option 1A (flat `[tools]` + additive `[group.<name>]`).** Matches the Cargo model users already know, keeps the 80 % case one line per tool, and the additive group semantic is trivially explainable ("default is always on, named groups layer on top"). Future extension to per-tool tables is reserved by documenting the schema as "currently string-only values; future releases may accept inline tables with `{ version = "...", ... }`" — TOML accepts both forms for a given key only if we design it that way from day one, so we commit to **string-only values in v1** with inline-table extension deferred to v2.

---

### Decision 2: `ocx.lock` schema shape

The declaration hash pattern is the staleness detector and must be part of every schema option below.

#### Option 2A: `[[tool]]` array of tables, per-platform inline sub-table

```toml
# ocx.lock — machine written, sorted alphabetically by (name, group)
[metadata]
lock_version = 1
declaration_hash = "sha256:a1b2c3..."
generated_by = "ocx 0.34.0"
generated_at = "2026-04-19T10:30:00Z"

[[tool]]
name = "cmake"
tag = "3.28"
group = "default"
index = "ocx.sh"

[tool.platforms]
linux-amd64   = { manifest_digest = "sha256:111..." }
linux-arm64   = { manifest_digest = "sha256:222..." }
darwin-amd64  = { manifest_digest = "sha256:333..." }
darwin-arm64  = { manifest_digest = "sha256:444..." }
windows-amd64 = { manifest_digest = "sha256:555..." }

[[tool]]
name = "shellcheck"
tag = "0.11"
...
```

| Pros | Cons |
|------|------|
| Cargo-shaped diff (line-localized append when a tool is added) | Inline tables per platform are slightly noisy but each row is one line, which is ideal for diffs |
| `[[tool]]` is alphabetically sortable (stable order under `ocx lock`) | Requires documenting the exact sort key (name ASC, then group ASC) |
| `manifest_digest` per platform is exactly what `pull_all` takes as input | — |

#### Option 2B: Nested `[tool.<name>]` tables (Cargo.lock–like)

```toml
[metadata]
lock_version = 1
declaration_hash = "sha256:a1b2c3..."

[tool.cmake]
tag = "3.28"
group = "default"
index = "ocx.sh"

[tool.cmake.platforms.linux-amd64]
manifest_digest = "sha256:111..."

[tool.cmake.platforms.linux-arm64]
manifest_digest = "sha256:222..."
```

| Pros | Cons |
|------|------|
| Keys are unique by construction (TOML disallows duplicate `[tool.<name>]`) | A tool with 5 platforms emits ~7 tables — a single `ocx lock` diff touches ~35 tables to record one tool change |
| Easy to `[tool.cmake]` lookup | Tool names with `.` (e.g., `some.namespaced/tool`) are awkward; `[tool."..."]` quoting is ugly in diffs |

#### Option 2C: Single `[tools]` table with deeply-nested inline values

```toml
[metadata]
lock_version = 1
declaration_hash = "sha256:..."

[tools]
cmake = { tag = "3.28", group = "default", index = "ocx.sh", platforms = { "linux-amd64" = { manifest_digest = "sha256:111..." }, ... } }
```

| Pros | Cons |
|------|------|
| Maximally compact | Unreadable; one 200-char line per tool; merge conflicts almost guaranteed when two branches add tools |

**Chosen: Option 2A (`[[tool]]` array of tables, per-platform inline sub-table).** It matches Cargo's diff-friendly shape, keeps the per-tool record to under 15 lines, and supports stable alphabetical ordering. The `[metadata]` block carries `lock_version` (serde_repr-encoded u8 for forward-compat rejection), `declaration_hash` (staleness), and `generated_by` (audit).

---

### Decision 3: `declaration_hash` scope

#### Option 3A: Hash only the `[tools]` and `[group.*]` sections

Algorithm: deserialize `ocx.toml` into the project schema, re-serialize **just** the `tools` + `groups` fields to canonical TOML (sorted keys, stripped comments), SHA-256 the result, prefix with `sha256:`.

| Pros | Cons |
|------|------|
| Cosmetic-only changes to `ocx.toml` (comments, key reordering, added `[description]` sections) do **not** invalidate the lock — avoids false "you need to re-lock" prompts | Requires canonicalization logic; bugs in canonicalization can cause false non-invalidation |
| Matches PDM's `content_hash` approach (PDM hashes the resolved dependency section) | Canonicalization has to be locked down in v1 — changing the canonicalization algorithm is itself a breaking change |

#### Option 3B: Hash the entire `ocx.toml` file bytes

| Pros | Cons |
|------|------|
| Dead-simple implementation — no canonicalization | Every trailing-newline fix, comment change, or formatter pass invalidates the lock and requires `ocx lock` to re-run |
| Unambiguous | Hostile to `ocx.toml` authors who want to add comments or organize sections |

#### Option 3C: Hash only the raw `[tools]` section text (byte-precise)

| Pros | Cons |
|------|------|
| Simpler than 3A — no deserialization/re-serialization | Whitespace inside `[tools]` still invalidates; comments inside `[tools]` also invalidate; defeats the point |

**Chosen: Option 3A (hash the parsed, canonicalized `[tools]` + `[group.*]` sections).** This is the right tradeoff for human-authored files: cosmetic changes do not disturb the lock, but any semantic change (add/remove/rename a tool, change a tag) does. Canonicalization rules for v1:

1. Deserialize into `ProjectConfig` (the schema defined in Decision 1).
2. For each group (including the implicit `default`), produce a sorted `Vec<(tool_name, version_string)>`.
3. Serialize to a canonical JSON wire form `{ "default": [...], "group.dev": [...], ... }` (JSON not TOML so the canonicalization is independent of the TOML serializer's opinions about escaping and array style).
4. `sha256` the UTF-8 bytes; hex-encode; prefix `sha256:`.

The canonicalization algorithm is documented in the subsystem rule and locked by a test that asserts "known input → known hash" so any accidental change to canonicalization fails loudly in CI.

---

### Decision 4: Platform enumeration at `ocx lock` time

#### Option 4A: Always enumerate all five platforms

`linux/amd64`, `linux/arm64`, `darwin/amd64`, `darwin/arm64`, `windows/amd64` — always. If the upstream package has no manifest for one of these, the tool is pinned only to the platforms it supports; the lock file records an explicit `unavailable = true` marker for missing ones so the absence is intentional, not accidental.

| Pros | Cons |
|------|------|
| Deterministic: `ocx lock` on a Linux machine produces the identical lock as on macOS | Some tools genuinely only ship Linux binaries; recording `unavailable` for the three others is noise |
| Matches Cargo's platform-independence: `Cargo.lock` on Linux works on macOS | First-run `ocx lock` may take longer (5× HEAD requests per tool) |

#### Option 4B: Opt-in `--platforms` flag on `ocx lock`

`ocx lock` defaults to the current machine's platform; the user passes `--platforms linux/amd64,linux/arm64,...` to enumerate more.

| Pros | Cons |
|------|------|
| Faster on the first run | Lock files become machine-dependent; a dev locking on macOS produces a lock that CI on Linux can't use |
| — | Violates D1 (reproducibility across machines). **Blocker.** |

#### Option 4C: Declared `platforms = [...]` in `ocx.toml`

The user declares which platforms matter in `ocx.toml`; `ocx lock` enumerates exactly those.

```toml
platforms = ["linux/amd64", "linux/arm64", "darwin/arm64"]

[tools]
cmake = "3.28"
```

| Pros | Cons |
|------|------|
| Author controls which platforms to resolve — matches `rust-toolchain.toml`'s `targets` field | Adds one more concept to `ocx.toml` that every new user has to understand |
| Missing platform → explicit error at lock time, not silent | Default value question: if unset, do we fall back to all five or to just-the-current? Either is surprising |

**Chosen: Option 4A with Option 4C as an opt-in override.** The default is "enumerate all five standard platforms" (covers the 95 % case — `ocx.toml` is committed, so it's consumed by CI and dev machines alike). An `ocx.toml` author who knows their toolchain is Linux-only can shrink the platform set with `platforms = ["linux/amd64", "linux/arm64"]`; the lock shrinks correspondingly. Tools that legitimately lack a platform (e.g., a macOS-only notarizer) are recorded with `unavailable = true` on the missing platforms so the intent is explicit. This is the sweet spot between D1 and pragmatic resolve time.

---

### Decision 5: Activation model (the hardest decision)

This is where every previous-generation version manager went wrong. Three options, each with sharp tradeoffs.

#### Option 5A: `ocx exec` only (no interactive activation)

Users run `ocx exec -- cmd` for every tool invocation. No shell hook, no PATH mutation, no direnv integration.

| Pros | Cons |
|------|------|
| Zero new surface area; one code path to maintain | Actively hostile to interactive dev loops — typing `ocx exec -- cmake --build build` instead of `cmake --build build` is friction users will reject |
| Trivial security story | Ignores the reality that developers run tools interactively dozens of times per hour |

#### Option 5B: `ocx shell init <shell>` with a prompt hook + `ocx shell hook` + `ocx generate direnv`

A three-tier activation stack:

1. **Direct:** `ocx exec [--group <name>] -- cmd` — hermetic, primary, always works.
2. **Shell hook:** `ocx shell init bash` (run once) writes a `PROMPT_COMMAND` that calls `ocx shell hook` on every prompt. `ocx shell hook` reads the nearest `ocx.toml`, compares against the previously-exported state, and prints `export`/`unset` lines. It **never installs** — if a tool in the lock is not in the object store, it prints `# ocx: cmake 3.28 not installed; run 'ocx pull' to fetch` to stderr and skips exporting that tool's vars.
3. **direnv:** `ocx generate direnv` writes an `.envrc` of the form `eval "$(ocx --offline shell direnv)"` plus `watch_file ocx.toml ocx.lock`. `ocx shell direnv` is the non-stateful variant — same output as `shell hook` but without the previous-state diff.

**PATH deduplication:** `ocx shell hook` tags every export with a sentinel env var `_OCX_APPLIED="group1:group2:..."` containing a serialized fingerprint of what it last applied. On next invocation, if the fingerprint changed, emit `unset` for everything previously applied before emitting the new exports. This is the mise / direnv convention; it is battle-tested.

**Security boundary (explicit):**

- The hook **WILL** read `ocx.toml` and `ocx.lock` within the current `OCX_CEILING_PATH` walk.
- The hook **WILL** read the object store to produce export lines for tools already pulled (uses `find_plain` — bare object-store probe — not `find`).
- The hook **WILL NOT** contact any registry.
- The hook **WILL NOT** install, pull, or download any new content.
- The hook **WILL NOT** mutate any reference back-links — `refs/blobs/`, `refs/deps/`, `refs/layers/`, `refs/symlinks/` are read-only on the hook path. The full `find` / `link_blobs` write-through path is reserved for state-changing commands (`pull`, `find`, `find_symlink`).
- The hook **WILL NOT** mutate any user-visible filesystem state — no symlinks under `symlinks/`, no new packages, no new layers, no new blobs.
- The hook **WILL** validate every env-var key against the POSIX `[A-Za-z_][A-Za-z0-9_]*` grammar and skip (with a stderr note) any entry whose key fails validation — preventing key-slot injection from malformed package metadata.
- The hook **WILL** print a one-line stderr note when the lock is stale or tools are missing; the note is the only way a user learns they need to run a state-changing command.

| Pros | Cons |
|------|------|
| Covers all three modes of use (hermetic exec, interactive shell, direnv) without duplicating logic — `shell hook`, `shell direnv`, and `ocx exec`'s env composition all reuse `resolve_env()` | Three commands (`shell hook`, `shell direnv`, `shell init`) — discoverability friction |
| Security boundary is tight and verifiable: no network, no mutation | Prompt hook is slow if naively implemented; must cache the last-applied fingerprint |
| Matches mise and nix-direnv's direction | Shell init script has to be regenerated when OCX updates (not every release — only when the hook contract changes) |

#### Option 5C: Shim-based activation (pyenv / rbenv model)

`ocx shell init` writes shim binaries into a `$OCX_HOME/shims/` directory and prepends it to PATH. Each shim is a script that execs into `ocx exec -- <tool>` with the args.

| Pros | Cons |
|------|------|
| Zero prompt overhead — PATH is set once at shell init | Shim invocation is slow (5–30ms per `cmake` call vs. 0ms for a real binary) — unacceptable for hot loops like `make` or `cargo build` that spawn hundreds of subprocesses |
| Works even in non-interactive shells (cron, CI) | Shims break ptrace/strace debugging, confuse IDEs, and produce misleading `which cmake` output |
| — | pyenv/rbenv have spent a decade fighting the consequences of the shim design. Do not repeat. |

**Chosen: Option 5B (prompt hook + direnv + exec).** This is the direction mise has successfully moved toward and the one that respects our "always frozen, opt-in state changes" constraint. The stderr notification for missing tools is the critical safety rail: the user always knows when the environment they are in is incomplete, but the hook itself never takes surprise action.

The three commands are named for their distinct contracts:

| Command | Purpose | Reads previous state? | Writes exports to | Shell init? |
|---|---|---|---|---|
| `ocx shell hook` | Prompt-hook entry point | Yes (diffs against `_OCX_APPLIED`) | stdout (eval'd by shell) | No (called from prompt) |
| `ocx shell direnv` | One-shot export generator | No (stateless) | stdout (eval'd by direnv) | No (called from `.envrc`) |
| `ocx shell init <shell>` | Install the prompt hook | N/A | Shell-specific init code | Yes (user runs once) |

---

### Decision 6: Shell profile redesign

The existing `$OCX_HOME/profile.json` is a flat `Vec<ProfileEntry>` with no group concept. Project config introduces groups; the profile must either adopt them or be deprecated.

#### Option 6A: Keep `profile.json`, add a `groups: Vec<String>` field per entry

| Pros | Cons |
|------|------|
| Backward-compatible on disk; existing `ocx shell profile add` commands keep working | Two separate config systems (profile manifest and `ocx.toml`) both carry group vocabulary — drift risk |
| Low-churn implementation | Users now have to understand "is this tool in my profile or my project config?" |

#### Option 6B: Introduce home-tier `$OCX_HOME/ocx.toml` as the long-term replacement; keep `profile.json` as a legacy code path with a deprecation warning

Home-tier `ocx.toml` uses the same `ProjectConfig` schema. `ocx shell profile load` in v1 continues to work but emits a deprecation notice pointing at `ocx.toml` + `ocx shell init`. In v2 (one or two releases later), `profile.json` is removed.

Shell-init for home-tier `ocx.toml` works identically to project-tier — the hook walks CWD first, then falls back to `$OCX_HOME/ocx.toml`. Both `ocx.toml` files contribute (home is the base, project layers on top).

| Pros | Cons |
|------|------|
| One schema, one mental model, one storage format, one group vocabulary | Two-release deprecation cycle — more work than Option 6A |
| Users migrate by running `ocx profile export > ~/.ocx/ocx.toml` once | Existing profile entries with `Content` mode (pinned to digest) need a schema slot in `ocx.toml` — we must add that |
| Long-term maintenance cost drops (one code path, not two) | — |

**Chosen: Option 6B with an immediate `ocx shell profile generate [--shell bash|zsh|fish]` file-generator variant added in v1.** Specifically:

- **v1 (this feature):** introduce `$OCX_HOME/ocx.toml` support. `profile.json` keeps working. Add `ocx shell profile generate` alongside `ocx shell profile load` — the `generate` variant writes a shell-init snippet to a file path (suitable for `source ~/.ocx/init.bash` in `.bashrc`) instead of requiring an `eval` on every shell startup. Both commands emit a deprecation note pointing at `ocx shell init`.
- **v2 (future breaking release):** remove `profile.json` and the `shell profile {add, remove, list, load}` subcommands. `ocx shell profile generate` may remain as a convenience for users who do not want a prompt-hook.

The `Content` mode (pinned to digest) is expressible in `ocx.toml` as an explicit digest suffix: `cmake = "3.28@sha256:abc123..."`. The parser already accepts this form for identifiers.

---

## Decision Outcome

**Chosen options (one per decision):**

| Decision | Chosen Option |
|----------|---------------|
| 1. `ocx.toml` schema | 1A — flat `[tools]` + additive `[group.<name>]`, string-only values in v1 |
| 2. `ocx.lock` schema | 2A — `[[tool]]` array of tables, inline per-platform sub-table |
| 3. `declaration_hash` scope | 3A — hash canonicalized `[tools]` + `[group.*]` JSON wire form |
| 4. Platform enumeration | 4A + 4C — default to all five platforms, user can override via `platforms = [...]` in `ocx.toml` |
| 5. Activation model | 5B — `ocx exec` + `ocx shell hook` (prompt hook) + `ocx shell direnv` (direnv) + `ocx shell init <shell>` |
| 6. Shell profile evolution | 6B — home-tier `ocx.toml` as the long-term replacement; `profile.json` deprecated in v1, removed in v2 |

### Rationale summary

The schema decisions (1, 2, 3) are guided by Cargo precedent and PDM's `content_hash` pattern — these are the mature, battle-tested shapes for "developer-editable declaration + machine-written lock". Platform enumeration (4) optimizes for the reproducibility invariant (D1) while leaving a pragmatic escape hatch. The activation model (5) is the decision that most strongly expresses OCX's design philosophy — the "always frozen, opt-in state changes" constraint rules out shim activation and rules out auto-install-on-activate, leaving Option 5B as the only path that satisfies both interactive ergonomics and the security boundary. Profile redesign (6) is the predictable consequence of introducing `ocx.toml` — one mental model wins over two forever.

### Quantified impact

| Metric | Before | After | Notes |
|---|---|---|---|
| CI step count to pin a toolchain | ~6 lines (per-tool `ocx install`) | 1 line (commit `ocx.toml` + `ocx.lock`, run `ocx pull`) | Matches Cargo `cargo fetch` UX |
| Reproducibility across machines | Best-effort (tags resolve to whatever is latest on a given day) | Deterministic (digest-pinned in lock) | Fully addresses D1 |
| Staleness detection | None | Hard fail at `ocx exec` when `declaration_hash` mismatches | D4 invariant |
| First-run lock time per tool | N/A | ~5 HEAD requests per tool (one per platform) | ~500ms per tool at default registry round-trip |
| `ocx exec` cold-start overhead | ~30ms (today) | ~30ms (lock read + digest lookup, no resolution) | No regression |

### Consequences

**Positive:**

- One declarative file (`ocx.toml`) fully specifies a project's toolchain; one lock file makes it reproducible.
- GitHub Actions workflows shrink to two lines: `ocx pull` + `ocx exec -- <build command>`.
- Developers get interactive shell activation without giving up the hermetic-exec guarantee.
- The existing config loader hook (`ConfigInputs.cwd`) is finally used; no loader refactor needed.
- Groups give a clean way to separate `dev`-only tools (shellcheck, shfmt) from `ci`-only tools (lychee) without forcing every user into both.
- Shell profile convergence on `ocx.toml` eliminates the long-term maintenance cost of two independent manifests.

**Negative:**

- Users must learn a new file (`ocx.toml`) and a new concept (lock file). Mitigated: the file shape is deliberately Cargo-like for Rust developers and the mental model matches `package.json` / `Cargo.toml` for anyone coming from that background.
- Five new commands in v1 (`ocx lock`, `ocx update`, `ocx shell hook`, `ocx shell direnv`, `ocx shell init`). We accept this because each has a distinct, non-overlapping contract documented above.
- `profile.json`, the `ocx shell profile` CLI surface, and the `ocx_lib::profile` module have been removed entirely as of Units 2a + 2b. The earlier two-release deprecation plan was superseded by a hard break before broader rollout — see plan `auto-findings-md-eventual-fox` Unit 2.

**Risks:**

| Risk | Mitigation |
|---|---|
| Canonicalization algorithm for `declaration_hash` is bug-prone and any change is itself a breaking change | Lock the canonicalization to JSON (not TOML) to avoid TOML-formatter-version drift; add a `known input → known hash` test that is hard to accidentally change |
| Users put `ocx.lock` in `.gitignore` by reflex (treating it like `node_modules`) | Ship an explicit "commit your `ocx.lock`" section in the user guide and emit a one-line note on first `ocx lock` run when the file is not already tracked |
| Prompt hook introduces shell-startup latency | Cache the resolved group fingerprint in `_OCX_APPLIED`; if unchanged, the hook emits zero exports and exits in <5ms |
| `ocx.toml` at the wrong CWD level (too broad) accidentally activates in unrelated checkouts | `OCX_CEILING_PATH` exists as an escape hatch; users can set it to stop the walk at their home directory |
| Groups at home-tier `ocx.toml` conflict with groups at project `ocx.toml` (two `[group.dev]` blocks with different tools) | Project `[group.dev]` shadows home `[group.dev]` wholesale (not merge) — same semantics as `Config::merge`, documented explicitly |
| `ocx lock` takes 30+ seconds in a repo with 20 tools (5 platforms each) | Lock resolution runs in parallel using the existing `JoinSet` pattern from `pull_all`; bounded concurrency matching the OCI client pool |

## Technical Details

### Architecture

```
                        ┌─────────────────────┐
                        │   CLI: ocx exec     │
                        │   / ocx pull        │
                        │   / ocx lock        │
                        │   / ocx update      │
                        │   / ocx shell hook  │
                        │   / ocx shell direnv│
                        │   / ocx shell init  │
                        └──────────┬──────────┘
                                   │
                                   ▼
              ┌─────────────────────────────────────────┐
              │   ProjectConfig loader (new)            │
              │   - CWD walk up to OCX_CEILING_PATH     │
              │   - hooks into ConfigLoader::           │
              │     discover_paths() via ConfigInputs   │
              │     { cwd }                             │
              │   - produces (Option<ProjectConfig>,    │
              │               Option<ProjectLock>)      │
              └──────────┬──────────────────────────────┘
                         │
                         ▼
              ┌─────────────────────────────────────────┐
              │   ProjectResolver (new)                  │
              │   - consumes (config, lock, group sel)  │
              │   - returns Vec<oci::Identifier>        │
              │     (all digest-pinned, from lock)      │
              │   - validates declaration_hash          │
              │   - no registry calls at exec time      │
              └──────────┬──────────────────────────────┘
                         │
                         ▼
              ┌─────────────────────────────────────────┐
              │   PackageManager::pull_all              │
              │   (existing, unmodified)                 │
              └─────────────────────────────────────────┘
```

Three new modules under `crates/ocx_lib/src/`:

- `project/` — `ProjectConfig`, `ProjectLock`, loader walk, canonicalization + hash, group resolution.
- `project/lock/` — lock read/write with the atomic-rename pattern from `profile.rs:142-188`.
- `project/resolver.rs` — "given config + lock + group selection, produce the `Vec<Identifier>` to pass to `pull_all`".

One new CLI module per command under `crates/ocx_cli/src/command/`:

- `lock.rs`, `update.rs`, `shell_hook.rs`, `shell_init.rs`, `shell_direnv.rs`. (`hook_env.rs` was renamed to `shell_hook.rs` per Unit 1; `shell_profile_generate.rs` was deleted with the rest of the profile CLI surface in Unit 2a.)

Existing `crates/ocx_cli/src/command/exec.rs` gains a branch: if `ProjectConfig` is present and `packages` is empty, resolve from project config + lock; otherwise use today's CLI-args path.

### API contract

**`ProjectConfig` (Rust):**

```rust
pub struct ProjectConfig {
    /// Default toolchain (always active).
    pub tools: BTreeMap<String, ToolSpec>,
    /// Named additive groups; `--group NAME` unions these with `tools`.
    pub groups: BTreeMap<String, BTreeMap<String, ToolSpec>>,
    /// Optional platform override. Default: all five standard platforms.
    pub platforms: Option<Vec<oci::Platform>>,
}

pub struct ToolSpec {
    /// User-authored tag (advisory) — e.g. "3.28" or "3.28@sha256:abc..."
    pub version: String,
}

pub struct ProjectLock {
    pub metadata: LockMetadata,
    pub tools: Vec<LockedTool>,   // sorted by (name, group)
}

pub struct LockMetadata {
    pub lock_version: LockVersion,     // serde_repr u8 — rejects unknown versions
    pub declaration_hash_version: u8,  // currently always 1 (see Amendment B4)
    pub declaration_hash: String,      // sha256:<hex>
    pub generated_by: String,          // ocx version string
    pub generated_at: String,          // ISO-8601 UTC
}

pub struct LockedTool {
    pub name: String,
    pub tag: String,                   // advisory, not used for resolution
    pub group: String,                 // "default" for implicit group
    pub index: String,                 // registry hostname
    pub platforms: BTreeMap<String, LockedPlatform>,
}

/// Untagged enum — see Amendment B. Serde discriminates on field shape:
/// `{ manifest_digest }` → Available, `{ unavailable: true }` → Unavailable.
/// The ambiguous `{ manifest_digest, unavailable }` shape is rejected at
/// parse time by a raw-toml probe.
pub enum LockedPlatform {
    Available {
        manifest_digest: String,       // sha256:<hex> — what pull_all consumes
    },
    Unavailable {
        unavailable: bool,             // always `true` on disk (serde discriminator)
    },
}
```

**CLI commands (all new in v1):**

```
ocx lock [--group NAME]...           Resolve tags → digests, write ocx.lock
ocx update [PKG]... [--group NAME]   Opt-in re-resolve for one/all tools
ocx exec [--group NAME] -- CMD       (existing, gains project-config branch)
ocx pull [--group NAME]              (existing, gains project-config branch — no args = pull all tools from lock)
ocx shell hook [--shell SHELL]       Prompt-hook export generator (stateful)
ocx shell direnv [--shell SHELL]     Direnv export generator (stateless)
ocx shell init <bash|zsh|fish|...>   Print shell init snippet for the user's .bashrc etc.
ocx shell profile generate [--shell] (new v1) File-generating variant of shell profile load
ocx generate direnv                  Writes .envrc that eval's shell direnv
```

Exit codes (mapped via `classify_error`):

| Condition | `ExitCode` variant | Numeric |
|---|---|---|
| Lock stale (`declaration_hash` mismatch in exec) | `DataError` | 65 |
| `ocx.lock` missing when `ocx.toml` exists | `ConfigError` | 78 |
| Tool in lock has no manifest for current platform | `NotFound` | 79 |
| Tag unresolvable at `ocx lock` time | `NotFound` | 79 |
| Group name not found in `ocx.toml` | `UsageError` | 64 |

### Data model

`$OCX_HOME/ocx.toml` is the home-tier default toolchain. `./ocx.toml` (or nearest parent up to `OCX_CEILING_PATH`) is the project toolchain. Both use `ProjectConfig`. Precedence: home is base, project layers on top — groups with the same name in both shadow wholesale (project wins).

The ambient `config.toml` tiers (system / user / `$OCX_HOME/config.toml`) remain unchanged and do **not** read `[tools]`. The two file types have non-overlapping schemas enforced by `#[serde(deny_unknown_fields)]` on both structs.

`ocx.lock` is written to the same directory as the consuming `ocx.toml` (project directory for project-tier, `$OCX_HOME` for home-tier). Home-tier lock is the pre-computed one; project-tier lock is the per-project one. They do not merge — the resolver picks the lock adjacent to the `ocx.toml` that "wins" for each tool.

## Implementation Plan

Phased rollout; each phase is a shippable increment. The `feature/dependencies` branch is a hard prerequisite before Phase 1.

1. [ ] **Phase 1 — Loader extension.** Add `project_path(cwd)` to `ConfigLoader` with CWD walk + `OCX_CEILING_PATH` stop. Extend `discover_paths` to call it. Return a typed `(Option<ProjectConfig>, Option<ProjectLock>)` tuple via a new `ProjectLoader` alongside the existing loader. No CLI changes yet.
2. [ ] **Phase 2 — Schema + canonicalization.** `ProjectConfig`, `ProjectLock`, `LockVersion` (serde_repr), canonicalization + hash with frozen test. Atomic write (reuse pattern from `profile.rs:142`).
3. [ ] **Phase 3 — `ocx lock`.** Resolve all tags via existing `Index::resolve`; enumerate platforms; write lock atomically. Parallelized via `JoinSet` following `pull_all`'s shape.
4. [ ] **Phase 4 — `ocx exec` project path.** Branch in `exec.rs` when `packages` is empty and project config is present. Consume the lock; hard-fail on staleness; call `pull_all` with digest-pinned identifiers.
5. [ ] **Phase 5 — `ocx update`.** Opt-in re-resolve for one or all tools.
6. [ ] **Phase 6 — `ocx pull` project path.** No-args `ocx pull` in a project directory pulls everything from the lock.
7. [ ] **Phase 7 — Shell hook trio.** `shell hook`, `shell direnv`, `shell init`. Reuses `ProfileBuilder` for per-shell syntax generation.
8. [ ] **Phase 8 — `ocx generate direnv`.** Writes `.envrc`.
9. [x] **Phase 9 — Home-tier `ocx.toml` + profile removal.** Home-tier `ocx.toml` shipped; the `ocx shell profile` CLI surface was removed in Unit 2a (lib types pending Unit 2b) — see plan `auto-findings-md-eventual-fox`.
10. [ ] **Phase 10 — Docs + website integration.** User guide section, JSON schema generation for `ocx.toml` + `ocx.lock`, taplo auto-completion wiring.

Each phase has its own acceptance test (pytest) and review loop.

## Validation

- [ ] Schema canonicalization produces identical hashes across platforms (frozen test).
- [ ] `ocx lock` is idempotent: re-running produces byte-identical output.
- [ ] `ocx exec` refuses to run with a stale lock, with exit code `DataError` (65).
- [ ] `ocx exec` refuses to run when `ocx.toml` is present and `ocx.lock` is missing, with exit code `ConfigError` (78).
- [ ] `shell hook` never initiates a network call (assert with an offline sandbox test).
- [ ] `shell hook` correctly removes previously-applied exports when the group fingerprint changes.
- [ ] CWD walk stops at `OCX_CEILING_PATH` and does not escape into `/home/$USER/unrelated/`.
- [ ] `ocx.lock` shipped as a byte-identical committed file across CI runners on Linux / macOS / Windows.
- [ ] Acceptance test: full workflow `ocx lock && ocx pull && ocx exec -- cmd` inside a fresh fixture repo.

## Links

- PR-FAQ: [`pr_faq_project_toolchain.md`](./pr_faq_project_toolchain.md)
- PRD: [`prd_project_toolchain.md`](./prd_project_toolchain.md)
- Loader module doc (already flags #33): `crates/ocx_lib/src/config/loader.rs:1-11`
- `ConfigInputs.cwd` hook: `crates/ocx_lib/src/config/loader.rs:33`
- Existing atomic-write pattern for locks: `crates/ocx_lib/src/profile/profile.rs:142-188` (via `ProfileManifest::load_exclusive`)
- `pull_all` (reused unmodified): `crates/ocx_lib/src/package_manager/tasks/pull.rs:111`
- `resolve_env` (reused unmodified): `crates/ocx_lib/src/package_manager/tasks/resolve.rs`
- `ProfileBuilder` (reused for shell export syntax): `crates/ocx_lib/src/shell/profile_builder.rs`
- Exit code taxonomy: `.claude/rules/quality-rust-exit_codes.md`
- Three-layer error pattern: `.claude/rules/quality-rust-errors.md`

---

## Review Addendum (2026-04-19)

Three parallel reviewers (spec-compliance, adversarial architecture, SOTA-gap) produced six block-tier findings. All six are resolved here before the implementation plan is written. Each resolution is a binding amendment to the decisions above.

---

### Amendment A — `ocx exec` unified composition model (supersedes BLOCK-1 resolution; revised 2026-04-19; resolves Arch Finding 3)

**Explicit rule:** `ocx exec` is a pure composition operation. It takes a list of identifiers — groups via `-g/--group`, packages as positionals — and resolves them into an environment. There is no implicit "default group" loading and no arg-count-based mode dispatch. The lock is always consulted for group identifiers; the index is consulted for package identifiers.

**Rationale:** The earlier "CLI args bypass project config entirely" rule created an asymmetric dispatch: same command, two resolution paths, chosen implicitly by whether the caller passed a positional. That violated the principle of least surprise and produced ornate error-precedence rules (`--group` + packages forbidden, `--group` alone required `ocx.toml`, `--offline` didn't change lock requirement). The composition model keeps D5 (hermetic execution) intact — the lock is still authoritative for everything it covers — while making the CLI surface predictable: what you list is exactly what you get. Interactive dev convenience is delivered by the shell hook (`ocx shell init` / `shell hook`, Phase 7), not by magic in `ocx exec`.

**Composition rules (binding):**

1. **Identifiers are either groups or packages.** Groups are specified via `-g NAME` / `--group NAME`, which is repeatable and accepts comma-separated values: `-g ci,lint -g release` resolves to `{ci, lint, release}`. Packages are positionals (`cmake:3.28`). Positionals are never interpreted as group names — this eliminates namespace collisions and makes CLI intent unambiguous.
2. **No implicit default group.** `ocx exec -- cmd` with no identifiers → `UsageError` (64). To load the `[tools]` table, use the reserved group name `-g default`.
3. **Reserved group name `default`.** `-g default` resolves to the top-level `[tools]` table in `ocx.toml`. Writing `[group.default]` in `ocx.toml` is a parse error (collision with the reserved name).
4. **Union semantics with right-most override.** The resolved tool set is the union of (selected groups + explicit packages). When the same tool name appears in multiple inputs, the right-most input wins. Order within a single `-g ci,lint` is left-to-right as written.
5. **Duplicate tool across two selected groups is a `UsageError` (64).** If `-g ci` and `-g lint` both define `shellcheck` at different tags, the user must disambiguate by dropping a group or adding an explicit package override. Silent "last group wins" across groups would hide authoring mistakes in `ocx.toml`.
6. **Explicit package on top of a group with the same tool is permitted.** `-g ci cmake:3.29` where `ci` contains `cmake:3.28` resolves to cmake 3.29 — the explicit positional is an intentional override.
7. **Comma-separated parsing.** Whitespace around commas is trimmed. Empty segments (`-g ci,,lint`) → `UsageError` (64). Duplicate group names across the flattened list (e.g. `-g ci -g ci`) are de-duplicated silently.

**Input validation (in order):**

1. Each requested group name must exist in `ocx.toml` (or be the reserved name `default`) → else `UsageError` (64).
2. Any `-g` flag without a project `ocx.toml` in effect → `UsageError` (64), message: `--group requires an ocx.toml`.
3. Empty arg list (no `-g` and no positionals) → `UsageError` (64), message: `no packages or groups specified`.
4. Empty segment in a comma list → `UsageError` (64).

**Resolution precedence (after validation passes):**

1. Lock-present check when any group is selected → `ConfigError` (78) if `ocx.lock` is absent.
2. Staleness check when any group is selected → `DataError` (65) if `declaration_hash` mismatches.
3. Duplicate tool across two selected groups → `UsageError` (64).
4. Platform-unavailable tool inside a selected group → silently skipped (debug log), consistent with Amendment D.
5. Explicit package identifiers → resolved via the index (today's tag-resolution path); not affected by lock state.

**Removed from the original Amendment A:**

- "CLI args bypass project config entirely" — deleted. Composition replaces bypass.
- "`--group` with explicit packages is a UsageError" — deleted. Composition is the point.

**Preserved:**

- D1 (hermetic execution via lock) — groups always resolve against the lock.
- D5 (explicit is authoritative) — an explicit positional override still wins over a group entry.
- Amendment C (project replaces home in full) — unchanged.
- Amendment D (unavailable semantics, lock atomicity) — unchanged.

---

### Amendment B — Declaration hash canonicalization (resolves BLOCK-2, Arch Finding 1)

The original Decision 3A is amended with four binding additions:

**B1. Include `platforms` in canonicalization.** The `platforms = [...]` field in `ocx.toml` (Decision 4C) changes what `ocx lock` resolves and MUST be part of the canonical form. Omitting it allows a user to change `platforms` without the lock being detected as stale.

**B2. Duplicate tool name across sections is a static parse error.** If the same tool name appears in `[tools]` and in any `[group.<name>]` section, `ProjectConfig` deserialization returns an error immediately (before any lock or exec operation). This eliminates the ambiguity in hash canonicalization for duplicate keys.

**B3. Canonicalization standard is RFC 8785 (JSON Canonicalization Scheme / JCS).** The canonical form is produced by: (a) deserializing `ocx.toml` into `ProjectConfig`; (b) constructing a JSON object where the key `"default"` maps to the sorted `[tools]` entries and `"group.<name>"` keys map to their sorted tool entries, plus a `"platforms"` key mapping to the sorted platform string list; (c) serializing with RFC 8785. The exact `serde_json` crate version and JCS wrapper must be pinned in `Cargo.toml` to prevent canonicalization drift from dependency updates.

**B4. Add `declaration_hash_version = 1` to `[metadata]`.** This allows future canonicalization bug fixes to introduce `declaration_hash_version = 2` without a `lock_version` bump. The `ProjectLock` parser checks `declaration_hash_version` and fails clearly if it encounters a version it does not understand.

**Amended lock metadata struct:**
```rust
pub struct LockMetadata {
    pub lock_version: LockVersion,         // serde_repr u8
    pub declaration_hash_version: u8,      // currently always 1
    pub declaration_hash: String,          // sha256:<hex>
    pub generated_by: String,
    pub generated_at: String,             // preserved when no digest changes; updated only when any digest changes
}
```

**`generated_at` clarification (resolves Spec WARN-3):** `generated_at` is updated only when at least one `manifest_digest` in the `[[tool]]` entries changes. When no digest changes, the entire lock file is byte-identical to the prior run. This is what "idempotent" means in FR-7.

---

### Amendment C — Home-tier vs project-tier composition (resolves Arch Finding 2, Spec WARN-6)

The original Decision 6B and the Data model section are amended with a binding resolution:

**The project tier replaces the home tier entirely when a project `ocx.toml` is found.** The home-tier `$OCX_HOME/ocx.toml` is the fallback used only when no project `ocx.toml` exists in the CWD walk. There is no merging of `[tools]` or shadowing of named groups across tiers. One file wins.

**Rationale:** Merging across tiers creates a reproducibility hole: two developers with different `$OCX_HOME/ocx.toml` contents produce different tool sets from the same committed project. This directly violates D1. "Project replaces home" is the safe, CI-correct semantics, matching how `~/.cargo/config.toml` is superseded by a workspace `.cargo/config.toml` in Cargo.

**Lock file scope:** The project-tier `ocx.lock` contains the **full resolved toolchain** for the project tier. It does not reference the home-tier lock. `ocx exec` in a project directory always reads the project's own `ocx.lock` — never the home lock. The home-tier lock (`$OCX_HOME/ocx.lock`) is only consulted when `ocx exec` is run outside any project directory.

**Correction to FR-4 (PRD):** FR-4's statement "`[tools]` merges with project winning on conflict" is wrong. The correct statement is "project `ocx.toml` replaces home `ocx.toml` in full." The PRD must be updated to match.

---

### Amendment D — `unavailable = true` semantics and `ocx lock` atomicity (resolves Researcher Gaps 4 and 7, Spec BLOCK-3)

**`unavailable = true` semantics (binding):**
- `unavailable = true` in a `[[tool]]` platform entry means **the tool publisher does not ship a manifest for this platform** (the OCI registry returned 404 or 405 for the platform manifest). It is written only when the upstream explicitly has no artifact for that platform.
- `unavailable = true` MUST NOT be written for transient failures (5xx, timeout, network error). Transient failures cause `ocx lock` to fail entirely.
- At `ocx exec` time: if the current platform's entry has `unavailable = true`, the tool is silently skipped (a debug-level log is emitted). `ocx exec` does not fail. This is the correct behavior for a macOS-only tool in a cross-platform lock.

**`ocx lock` atomicity and error handling (binding):**
- `ocx lock` is **fully transactional**: either the complete lock is written atomically (tempfile + rename, per `profile.rs:142`) or the existing `ocx.lock` is left intact. Partial locks are never written.
- Per-platform manifest queries use a **30-second per-tool timeout** (not per-request). If any tool's full platform resolution does not complete within 30 seconds, the entire `ocx lock` fails.
- **Retry policy**: 2 retries with exponential backoff (1s, 2s) for 5xx responses and network timeouts. 404/405 responses are NOT retried — they produce `unavailable = true`.
- After retries are exhausted, `ocx lock` emits a clear error naming the tool and platform, and exits without writing the lock.

---

### Amendment E — `ocx pull` naming (resolves Arch Finding 4)

The top-level `ocx pull` command referenced in the implementation plan is a **new top-level command** (not a rename of `ocx package pull`). Its semantics differ:

- `ocx package pull <pkg>...` — existing command, unchanged: pull specific packages to the object store by explicit identifier.
- `ocx pull [--group <name>]...` — new command: pull all tools declared in `ocx.toml` (filtered by group) from the project-tier `ocx.lock`. Equivalent to "pre-warm the object store for this project." Requires `ocx.toml` + `ocx.lock` to be present.

**`ocx pull` is the CI primitive** analogous to `cargo fetch`: run it once before the build to ensure all tools are in the local store, then run hermetic `ocx exec` offline.

---

### Amendment F — CWD walk default stopping condition (resolves Researcher Gap 6)

The CWD walk algorithm for locating `ocx.toml` is amended:

**Default stopping condition:** Walk up from CWD. Stop at the **first `ocx.toml` found** (nearest-only). Additionally, stop at any `.git/` directory boundary — do not cross into the parent git repository. If no `.git/` boundary is present (non-git workspace), walk up to the filesystem root.

**`OCX_CEILING_PATH`:** If set, the walk stops at this path regardless of `.git/` boundaries. Useful for monorepo setups where the `.git/` boundary is too broad.

**CI ergonomics (no auto-detection).** The walk does NOT auto-detect CI workspace variables (`GITHUB_WORKSPACE`, `CI_PROJECT_DIR`, `BUILDKITE_BUILD_CHECKOUT_PATH`, `BITBUCKET_CLONE_DIR`, etc.). Auto-detection trades implicit "right thing" behaviour for an unbounded list of provider-specific env vars and a silent surprise the next time we forget a provider. The `.git/` boundary already terminates the walk at the repo root in every standard CI checkout, which covers the common case without extra config. CI users who need to override the boundary (monorepo subprojects, fixture sandboxes) should set `OCX_CEILING_PATH` explicitly; the canonical Phase 10 user-guide pattern is:

```sh
# GitHub Actions
export OCX_CEILING_PATH="$GITHUB_WORKSPACE"
# GitLab CI
export OCX_CEILING_PATH="$CI_PROJECT_DIR"
```

Phase 10 docs MUST include the explicit-export pattern above and a one-line note that OCX deliberately does not auto-detect.

**Interaction with home-tier:** The walk never discovers `$OCX_HOME/ocx.toml` via the CWD walk. Home-tier `ocx.toml` is only loaded when the CWD walk finds no project file. They are separate lookup paths.

---

### Amendment G — Explicit project-file flag (resolves explicit-path gap in Amendment F)

Amendment F defines the CWD walk as the sole project-file discovery mechanism. That is sufficient for interactive use but insufficient for CI fixtures, integration tests, and any workflow where the caller needs to point `ocx` at a specific file without relying on `cwd` and `.git/` boundaries. This amendment adds an explicit escape hatch that mirrors the existing `ConfigLoader` inputs one-to-one.

**Binding structural mirror.** The project-tier loader adopts the same triple that the tier loader already exposes (see `crates/ocx_lib/src/config/loader.rs`):

| Role | Tier config (existing) | Project config (this amendment) |
|---|---|---|
| CLI flag | `--config <FILE>` | `--project <FILE>` |
| Env var | `OCX_CONFIG` | `OCX_PROJECT` |
| Kill switch | `OCX_NO_CONFIG` | `OCX_NO_PROJECT` |

The CLI flag name intentionally drops the `-file` suffix to match `--config` (not `--config-file`); the `-file` lives only on the env-var name. Value type is **file path**, matching `--config`.

**G1. CLI flag `--project <FILE>`.** No short form (`-c` is taken by tier config; `-p` is reserved for `--platform` across the codebase). When set, the loader reads this file as the project tier and skips the CWD walk entirely.

**G2. Env var `OCX_PROJECT`.** Same semantics as `--project`. Empty string (`OCX_PROJECT=""`) is treated as unset, matching the `OCX_CONFIG=""` escape hatch.

**G3. Kill switch `OCX_NO_PROJECT=1`.** Skips CWD walk *and* `OCX_PROJECT`. Does NOT prune an explicit `--project` flag — the CLI is trusted caller intent, matching the existing `OCX_NO_CONFIG=1` + `--config <FILE>` behavior covered by `loader.rs` test `no_config_with_cli_flag_still_loads`.

**G4. Precedence.** `--project` > `OCX_PROJECT` > CWD walk. The ceiling path (`OCX_CEILING_PATH`, Amendment F) only bounds the CWD walk. Explicit paths escape it — that is the whole point of the flag.

**G5. Symlink policy.**
- Explicit paths (`--project`, `OCX_PROJECT`) follow symlinks (trusted caller intent, matching `--config`).
- CWD walk rejects symlinks at the discovery step (matching tier-config discovery).

**G6. Basename.** Any filename is accepted via `--project` / `OCX_PROJECT` (matches Cargo `--manifest-path`; fixtures and integration tests need this). The CWD walk still looks for the literal name `ocx.toml` (unchanged from Amendment F).

**G7. File-not-found exit code.** `--project <missing>` or `OCX_PROJECT=<missing>` → `NotFound` (79). Distinct from `ConfigError` (78), which is reserved for parse/schema failures on files that exist. Matches how `--config` handles missing explicit paths today.

**G8. Interaction with Amendment A (composition).** This amendment only decides *which* `ocx.toml` is read. It does not change composition semantics: `-g <group>` without a project file in effect remains a `UsageError` (64), regardless of whether the missing file was "no walk hit" or "kill switch set" or "explicit path pointed at something that turned out not to exist" (the last case errors earlier with `NotFound` 79).

**G9. Non-`NotFound` I/O on explicit paths.** `--project <path>` / `OCX_PROJECT=<path>` where the `path` exists but the OS surfaces a non-`NotFound` error kind (permission denied, path-is-not-a-directory, stale NFS handle, EIO) → `Error::Io` → `IoError` (74). Silently succeeding would be a correctness hole: the explicit path is resolved for its later-phase side effects, so a phantom `Ok(Some(path))` that never materializes a readable file would mask the failure until a downstream consumer stumbles into it. Explicit G7 symmetry: missing file → 79 (NotFound), unreadable file → 74 (IoError); the two codes are distinguishable to automation.

**G10. `.git` boundary fail-closed.** `has_git_dir` treats any non-`NotFound` I/O error (permission denied, EIO) on `<dir>/.git` as *boundary present*, not *boundary absent*. Fail-open would let the CWD walk silently cross a repository boundary when the filesystem cannot confirm absence — weakening the security property that motivates the boundary check. Worktree `.git` linkfiles (plain files, not directories) are handled by the same `symlink_metadata`-based probe and also act as boundaries, matching git's own "any `.git` entry" rule.

**G11. Candidate-hit precedence over `.git` boundary at the same walk level.** At each level the CWD walk checks for `ocx.toml` FIRST; only if no valid file is found does the `.git` boundary fire. Rationale: Amendment F's primary stopping condition is "stop at the first `ocx.toml` found" — the `.git` rule prevents walking UP past a repo root, not accepting a project file AT the repo root. The alternative ordering (boundary first) regresses the common case where `ocx.toml` sits alongside `.git` at a project's root, yielding `None` on every invocation from within the project. Regression test: `project_path_walk_finds_ocx_toml_at_git_root_level` in `crates/ocx_lib/src/config/loader.rs`.

**G12. Explicit paths must resolve to a regular file.** `resolve_explicit_project_path` inspects `metadata().file_type()` and rejects non-file targets (directory, device, FIFO, socket) as `Error::Io` with `ErrorKind::InvalidInput` ("not a regular file") — exit 74. Phase 1 does not yet parse the file, so a silent "successful" discovery of a directory would defer the failure to a later consumer with a misleading read error. Matches G9's shape: phantom success on an explicit path is a correctness hole and must surface at resolve time. Regression test: `project_path_explicit_directory_rejected_as_io`.

**Rationale.** The existing tier-config triple (`--config` / `OCX_CONFIG` / `OCX_NO_CONFIG` / `""` escape hatch) is already the OCX contract for "explicit file path overrides discovery." Duplicating that shape verbatim means users, tests, and future loaders inherit one mental model, not two. Divergence from uv/PDM's `--project <DIR>` convention is a deliberate trade-off in favor of internal consistency with OCX's own `--config`.

---

### Amendment H — Rescind B1: platforms removed from lockfile schema (2026-04-21)

Amendment B1 is rescinded.

B1 required that the `platforms = [...]` field in `ocx.toml` be included in the canonical declaration hash. That field no longer exists: the `platforms` field was removed from the lockfile schema during the Phase 2.1 identifier/pinned/resolve redesign (see commit `ee27ec8 refactor(project)!: redesign ocx.toml/ocx.lock schema for full identifiers`). Platform resolution is now encoded directly in the per-tool `[[tool]]` entries via per-platform `manifest_digest` fields, so a separate top-level `platforms` list is redundant.

**Effect on canonicalization (Amendment B3).** The JSON object used for the declaration hash no longer includes a `"platforms"` key. Only the `"default"` group and named `"group.<name>"` keys are hashed. Amendments B2, B3, and B4 are otherwise unchanged.

**Effect on Amendment D.** The `unavailable = true` field in per-platform entries remains the mechanism for recording that a publisher does not ship a manifest for a given platform. No change to D.

---

### Minor Amendments

**Profile deprecation timeline (Arch Finding 5):** Decision 6B's two-release cycle is retained, but the rationale is now explicit: `profile.json` users are interactive developers who have chosen the `shell profile add` workflow and have tool lists they maintain manually. A one-command migration (`ocx profile export`) exists, but forcing it in the same release that introduces `ocx.toml` risks alienating the interactive-shell user segment that OCX's interactive dev story depends on. The exception to Constraint 7 is justified by "interactive user data vs CI/automation contracts."

**`_OCX_APPLIED` fingerprint format (Arch Finding 6, Deferred):** Specified in the implementation plan, not here. Recommended: `v1:<sha256-hex-of-sorted-(tool_name, manifest_digest, group) triples>`. Length: 68 bytes, fits any shell environment variable.

**`.gitattributes` guidance (Researcher Gap 1):** `ocx lock` prints a one-time note when `ocx.lock` is not listed in `.gitattributes`. Documentation (Phase 10) includes: `echo 'ocx.lock merge=union' >> .gitattributes`. The note fires on every `ocx lock` run where the file is untracked (not just first run).

**SLSA level (Researcher Gap 3):** OCX v1 achieves digest-level pinning (SLSA Level 1 binary integrity). `attestation_digest: Option<String>` is reserved as a comment in `LockedPlatform` for future SLSA Level 3 support; no schema change needed now.

**Blake3 / algorithm prefix (Researcher Gap 5):** `manifest_digest` is an opaque string in `ProjectLock`. No prefix validation at parse time. The OCI pull layer dispatches on the algorithm prefix already. Adding `blake3:` in v2 requires no lock_version bump.

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-19 | Architect worker (sion) | Initial draft |
| 2026-04-19 | Review panel (spec, arch, SOTA) + orchestrator | Review addendum: resolved 6 block-tier findings, 10 warn-tier findings |
| 2026-04-19 | Design session (sion) | Amendment A revised to unified composition model (kills arg-count dispatch, requires `-g default` for `[tools]` table, permits `--group` + packages with union/override semantics) |
| 2026-04-19 | Design session (sion) | Amendment G added: explicit project-file escape hatch (`--project <FILE>` + `OCX_PROJECT` + `OCX_NO_PROJECT`) mirroring the existing tier-config triple one-to-one |
| 2026-04-19 | Swarm execute (Phase 1, Round 2) | Amendments G9 + G10 added: codify `Error::Io` (74) for non-`NotFound` I/O on explicit paths, and fail-closed semantics for `.git` boundary probes |
| 2026-04-19 | Swarm execute (Phase 1, Codex gate) | Amendment G11 added: candidate-hit precedence over `.git` boundary at the same walk level (regression guard against discovery failing at the repo root). Amendment G12 added: explicit project path must resolve to a regular file; directories and non-regular targets surface as `Error::Io` (74) |
| 2026-04-19 | Design session (sion) | Amendment F: documented "no CI workspace auto-detection" policy with explicit `OCX_CEILING_PATH=$GITHUB_WORKSPACE` / `$CI_PROJECT_DIR` pattern; Phase 10 docs requirement |
