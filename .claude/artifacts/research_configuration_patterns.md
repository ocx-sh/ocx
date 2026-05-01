# Research: Configuration File Patterns for CLI Tools

**Date:** 2026-04-12 (updated)
**Domain:** configuration, packaging, cli-design
**Triggered by:** Planning configuration system for OCX (swarm-plan)

## Direct Answer

Surveyed configuration patterns across uv, Cargo, Bun, mise, Poetry, Docker Compose, Git, systemd, EditorConfig, and direnv. Strong convergence on TOML format, XDG paths, multi-tier discovery, env var overrides, and named-section conventions. Specific deep-dives below for the four patterns OCX needs to decide on now.

## Key Findings — Tier Discovery

| Tier | Path | Purpose |
|------|------|---------|
| System | `/etc/ocx/config.toml` | Machine-wide defaults (Docker, managed envs) |
| User | `$XDG_CONFIG_HOME/ocx/config.toml` → `~/.config/ocx/config.toml` | Personal defaults |
| OCX_HOME | `$OCX_HOME/config.toml` | Co-located with data, redistributable |
| Project (future #33) | CWD walk for `ocx.toml` | Project toolchain pins |

**Merge semantics**: Cargo model — scalar nearest-wins, tables merged key-by-key, `[patches]`-style atomic sections replaced wholesale, env vars beat all files.

## Deep Dive 1 — Named Registry Tables

**Pattern surveyed**: Cargo `[registry]` + `[registries.<name>]`, uv `[[index]]`, Poetry `[[source]]`, pip `[global]`/`[install]`.

**Cargo's two-section split is the winner**:

```toml
[registry]                    # plain table — global registry-subsystem settings
default = "ocx.sh"
# Future: timeout, retry, default-credential-provider, etc.

[registries.ghcr]             # named entry (plural form)
url = "ghcr.io"
```

**Critical TOML constraint**: A single key cannot be both a plain table and a parent of sub-tables. `[registry] default = "x"` and `[registry.foo] url = "..."` cannot coexist — TOML rejects it. The `[registry]` (singular) + `[registries.<name>]` (plural) split is the only clean way to have both global registry settings and named registry entries.

**uv's `[[index]]` array model loses on multi-tier merging**: arrays cannot be surgically overridden by a higher tier; the entire array must be re-specified. Cargo's named-table model allows precise key-level overrides across tiers.

**Recommendation**: Adopt Cargo's split. Lock in `[registries.<name>]` (plural) as the future named-tables key now to avoid a future rename.

**Sources**: [Cargo config reference](https://doc.rust-lang.org/cargo/reference/config.html), [Cargo RFC 2141](https://rust-lang.github.io/rfcs/2141-alternative-registries.html), [uv issue #8828](https://github.com/astral-sh/uv/issues/8828), [Poetry repositories docs](https://python-poetry.org/docs/repositories/).

## Deep Dive 2 — `$VAR` Interpolation

**Pattern surveyed**: Bun (`$VAR`), mise (`${VAR}` with opt-in), Cargo (rejected — issue #10789), Docker Compose (`${VAR:-default}`, `${VAR:?error}`, `$$` escape), Kubernetes (`$(VAR)`), systemd (`${VAR}`).

**Strong recommendation: do NOT add string interpolation. Keep the named indirection pattern OCX already uses.**

OCX's existing config schema has the right pattern:

```toml
auth = { type = "env", token = "CUSTOM" }
```

The string `"CUSTOM"` is an **env var name**, resolved at runtime. The config never contains the secret. This is the **Named Indirection Pattern**, also used by Cargo (`credential.helper`) and AWS CLI (`credential_process`). It is strictly safer than `${VAR}` interpolation:

| Concern | `$VAR` interpolation | Named indirection |
|---------|---------------------|-------------------|
| Config-file logging leaks secrets | Risk | Safe (only var names in file) |
| Escape sequences (`$$`) needed | Yes | No |
| Schema clarity (which fields are env-resolved?) | Implicit/global | Explicit per-field type |
| Parser complexity | High (engine + edge cases) | Zero |
| False positives in secret scanners | Yes (`${...}` patterns) | No |

**Cargo's reasoning is sound**: TOML is a data format, not a template language. Mixing the two creates a parser that is neither.

**If interpolation is ever needed** (e.g., for path fields varying per machine), adopt Docker Compose syntax precisely: `${VAR}` only (no bare `$VAR`), `$$` escape, `${VAR:-default}`, `${VAR:?error}`. Restrict to documented fields, never apply globally.

**Sources**: [Docker Compose interpolation spec](https://docs.docker.com/reference/compose-file/interpolation/), [Cargo issue #10789](https://github.com/rust-lang/cargo/issues/10789), [Bun issue #9541](https://github.com/oven-sh/bun/issues/9541), [mise env_shell_expand](https://mise.jdx.dev/configuration.html), [TOML spec issue #255](https://github.com/toml-lang/toml/issues/255).

## Deep Dive 3 — `include` Key

**Pattern surveyed**: Cargo `include = [...]` (stabilized Oct 2025), Git `[includeIf]`, systemd drop-in dirs, nginx `include`, kustomize `resources:`.

**Strong recommendation: defer indefinitely. The multi-tier model already provides layering for free.**

Reasons to defer:
1. **Multi-tier solves the same problem.** System → user → OCX_HOME + `OCX_CONFIG` covers every real composition need OCX has.
2. **High implementation complexity.** Cycle detection, relative path resolution, error attribution across files, optional vs. required, recursion limits. Cargo spent years stabilizing it.
3. **Wrong target audience.** OCX is automation-first; CI uses `OCX_CONFIG` to a single explicit file. `include` is a human-ergonomics feature.
4. **Drop-in directories (`config.d/*.toml`) are even lower value** for OCX — they solve coordination between many uncoordinated contributors, which OCX doesn't have.

**If added later** (e.g., for enterprise OCX_HOME bundles), follow Cargo's model exactly:
- Array of `{ path, optional }` inline tables, paths relative to including file
- Including file wins over included files (included = defaults)
- No globs, no conditional includes
- Cycle detection with clear error

**Sources**: [Cargo config reference](https://doc.rust-lang.org/cargo/reference/config.html), [Cargo PR #16285](https://github.com/rust-lang/cargo/pull/16285), [git-config docs](https://git-scm.com/docs/git-config), [systemd.unit man page](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html).

## Deep Dive 4 — CWD Walk for Project Discovery

**Pattern surveyed**: Cargo (load all in chain), mise (load all + `MISE_CEILING_PATHS`), uv (load nearest only), EditorConfig (`root = true` stop), direnv (trust model), Git (no walk, fixed tiers).

**Recommendations for the future #33 `ocx.toml` project tier**:

1. **Walk start**: CWD at invocation
2. **Walk stop**: Filesystem root, with optional `OCX_CEILING` env var (mise's pattern) for CI/Docker reproducibility
3. **Load strategy**: NEAREST `ocx.toml` only (uv model), not all files in chain
   - Rationale: OCX is a backend tool — surprising cascading is hard to debug
   - Composition across tiers happens via system/user/OCX_HOME, not stacked `ocx.toml`
   - Sidesteps the "project silently overrides infra patches" problem
4. **No `.git` boundary**: No surveyed tool stops at `.git` — it breaks nested workspaces

**Critical: 3 loader hooks needed NOW to accommodate CWD walk later without rewrite.**

The `ConfigLoader` design must:

1. **Keep tier ordering as `Vec<PathBuf>` internally**, not hardcoded N-tier logic. Adding the project tier later = inserting into the vec.
2. **Separate discovery from loading**:
   ```rust
   fn discover_paths(explicit: Option<&Path>, cwd: Option<&Path>) -> Vec<PathBuf>;
   fn load_and_merge(paths: Vec<PathBuf>) -> Result<Config>;
   ```
   CWD walk slots into `discover_paths` later.
3. **Pass CWD into the loader as a parameter**, do NOT call `std::env::current_dir()` inside it. Makes the loader testable without filesystem side effects and lets callers control walk root.

These three constraints cost nothing now and eliminate rewrite risk at #33.

**Sources**: [Cargo config reference](https://doc.rust-lang.org/cargo/reference/config.html), [mise configuration](https://mise.jdx.dev/configuration.html), [uv configuration files](https://docs.astral.sh/uv/concepts/configuration-files/), [EditorConfig spec](https://spec.editorconfig.org/index.html), [direnv](https://direnv.net/).

## Anti-Patterns to Avoid

- Deep merge (surprising partial overrides)
- Tokens in config files as literals (use named indirection)
- Global-only config (no project tier)
- INI or YAML format (TOML is the consensus)
- Walking from script location instead of CWD (breaks `cd && ocx ...`)
- Stopping CWD walk at `.git` (breaks monorepos)
- Loading ALL `ocx.toml` files in the walk chain (hard to debug)

## Sources (consolidated)

- [Cargo config reference](https://doc.rust-lang.org/cargo/reference/config.html)
- [Cargo RFC 2141 — Alternative Registries](https://rust-lang.github.io/rfcs/2141-alternative-registries.html)
- [Cargo issue #10789 — env interpolation](https://github.com/rust-lang/cargo/issues/10789)
- [Cargo PR #16285 — disallow glob in include](https://github.com/rust-lang/cargo/pull/16285)
- [uv configuration files](https://docs.astral.sh/uv/concepts/configuration-files/)
- [uv issue #8828 — default index in uv.toml](https://github.com/astral-sh/uv/issues/8828)
- [Bun bunfig reference](https://bun.sh/docs/runtime/bunfig)
- [Bun issue #9541 — interpolation bugs](https://github.com/oven-sh/bun/issues/9541)
- [mise configuration reference](https://mise.jdx.dev/configuration.html)
- [Poetry repositories documentation](https://python-poetry.org/docs/repositories/)
- [Docker Compose interpolation spec](https://docs.docker.com/reference/compose-file/interpolation/)
- [git-config documentation](https://git-scm.com/docs/git-config)
- [systemd.unit man page](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html)
- [EditorConfig spec](https://spec.editorconfig.org/index.html)
- [direnv](https://direnv.net/)
- [TOML spec issue #255 — env interpolation rejected](https://github.com/toml-lang/toml/issues/255)
- [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir-spec-latest.html)
