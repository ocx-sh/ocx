# Handover — Entrypoint baked args + interpolation (uv-run-python case)

**Audience:** `/architect` (design spec / ADR) → then implementation.
**Status:** Design direction agreed in discussion; ready for architecture.
**Driving issue:** [#83](https://github.com/ocx-sh/ocx/issues/83) — but issue is stale, see §1.
**Driving use case:** Run a Python script via a vendored/dep `uv` from an installed ocx package (issue #83 comment by NucaChance).

---

## 0. TL;DR for the architect

The feature: let a package's entrypoint **bake fixed arguments** (with `${installPath}` interpolation), so an installed launcher can dispatch e.g. `uv run <shipped-script>` instead of only a bare binary name.

Agreed decisions (locked in discussion):

- **D1** — `command` stays a PATH-resolved **slug**, NO interpolation, NO path form. (Composition + visibility already resolve binaries.)
- **D2** — Add `args: Vec<String>` to `Entrypoint` (JSON **array**, not a command-line string).
- **D3** — `args` interpolation = **`${installPath}` ONLY**. No `${deps.*}`.
- **D4** — NO per-entrypoint env. Package-wide env via the existing `env` block + `private` visibility.
- **D5** — Division of config: package-uniform → `env` block; per-entrypoint variation → `args`.

Open question for the architect to resolve: **A (runtime-resolve) vs B (vendored)** for the venv-writable-state problem (§5). This is a packaging-direction call; the metadata feature (D1–D5) is needed regardless.

Key implementation property: **baked args are resolved inside the `ocx launcher exec` Rust subcommand, NOT baked into the generated launcher script.** So the launcher `.sh`/`.exe` body is UNCHANGED → no wire-ABI / golden-test churn. The change is contained to `entrypoint.rs` + `launcher/exec.rs` + template wiring.

---

## 1. Issue #83 freshness — what's stale

Issue was written before the `command` field landed. Two of its three factual claims no longer hold:

| Issue claim | Reality (verified) |
|---|---|
| `target` template (`${installPath}/bin/foo`, `${deps.NAME.installPath}/bin/foo`) parsed at install in `generate.rs` as defense-in-depth | **No `target` field exists.** `generate.rs` validates only `pkg_root` safe-string + entry name. |
| `exec.rs:88` runs `resolve_command(command)` where `command` = launcher basename → recursion hang | **Stale.** `exec.rs` now reads metadata, maps `argv0`→divergent command via `dispatch_command`, THEN resolves on PATH. |
| `target` is decorative; can't expose a dep binary under a different name without a shim | **False.** The `command` field does exactly this. |

**The `command` field already fixed #83's core complaint** (target drives invocation; cross-layer dispatch). The manual workaround `test/manual/packages/cross-layer-entrypoint` (a `bin/wrap-leaf-a` shim, empty `{}` entry) is now obsolete — could drop the shim and use `{"command": "leaf-a"}`.

**Recommendation:** re-scope #83. Its `target`-template design is dead. The live work is the narrower **baked-args + arg-interpolation** described here.

---

## 2. Current state of the code (file/line anchors)

### Entrypoint metadata — `crates/ocx_lib/src/package/metadata/entrypoint.rs`
- `Entrypoint` struct (lines ~104-133): only field is `command: Option<EntrypointName>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`). This is where `args` gets added.
- `EntrypointName` (lines ~11-102): slug newtype `^[a-z0-9][a-z0-9_-]*$`, max 64 bytes (= `SLUG_MAX_LEN`). Used for both the map key and `command`. Rejects `/`, `..`, uppercase, unicode at parse.
- `Entrypoints` (lines ~135-222): `BTreeMap<EntrypointName, Entrypoint>` with custom `Deserialize` (rejects duplicate keys), custom `Serialize`, manual `JsonSchema`. `dispatch_command(name) -> &str` (lines ~209-221) maps an invocable name to its command (falls back to name).
- Existing tests at bottom cover slug validation, serde round-trips, `command` divergent dispatch. **Add `args` tests here.**

### Bundle container — `crates/ocx_lib/src/package/metadata/bundle.rs`
- `Bundle { version, strip_components, env, dependencies, entrypoints }`. `entrypoints` field is `#[serde(skip_serializing_if = "Entrypoints::is_empty", default)]`.
- `metadata.rs` exposes `entrypoints() -> Option<&Entrypoints>`.

### Launcher dispatch (the runtime path) — `crates/ocx_cli/src/command/launcher/exec.rs`
This is the **primary change site.** Current flow in `execute` (lines ~40-77):
1. Validate `pkg_root` (must be under `$OCX_HOME/packages/` or `temp/test/`, contain `metadata.json`).
2. `info = manager.install_info_from_package_root(&validated)` — gives the content path / install path.
3. `entries = manager.resolve_env(&[info], /* self_view = */ true)` — composed env, **private surface included**.
4. Split `argv` → `argv0` (launcher filename) + `args` (user args).
5. `command = metadata.entrypoints().map_or(argv0, |eps| eps.dispatch_command(argv0))`.
6. `run_with_env(entries, args, command, config_view)`.

`run_with_env` (lines ~90-108): builds `Env`, applies entries + ocx config, `resolved = process_env.resolve_command(command)`, then `child_process::exec(&resolved, args, process_env)`.

**Where baked args plug in:** after step 5, look up the `Entrypoint` for `argv0`, resolve its `args` via a `TemplateResolver` (installPath only), and prepend the resolved baked args before the forwarded user `args`. Need the install path (available from `info` in step 2).

### Interpolation engine — `crates/ocx_lib/src/package/metadata/template.rs`
- `TemplateResolver<'a> { install_path, dep_contexts }`. `resolve(template) -> Result<String, TemplateError>`.
- Supports `${installPath}` and `${deps.NAME.installPath}`. Has a security guard: rejects an `install_path` that itself contains `${...}` (UnknownPlaceholder).
- **Currently wired ONLY to env-var values via `EnvResolver`.** Its own doc-comment says "future metadata features (e.g., entry points)" — never connected.
- **For D3 (installPath only):** construct with an **empty `dep_contexts` map** → any `${deps.*}` token fails with `UnknownDependencyRef`. Cheap enforcement. Consider a dedicated, clearer error (`deps not allowed in entrypoint args`) rather than reusing the env error — architect's call. Related design: `adr_deps_name_interpolation.md`.

### Child process — `crates/ocx_lib/src/utility/child_process.rs`
- `exec(program, args, env)` — `Command::new(program).args(args).env_clear().envs(env)`; Unix `execvp`, Windows spawn+wait+exit. **Args passed literally — no shell, no `$VAR` expansion.** (This is why per-entrypoint env-var references in args do NOT expand — see D4 rationale.)
- Array `args` ⇒ each element is one argv token. No word-splitting, paths-with-spaces just work.

### Composition / visibility — `crates/ocx_lib/src/package_manager/composer.rs`
- `compose(roots, store, self_view)`: surface gating — `if self_view { has_private() } else { has_interface() }`.
- Launcher uses `self_view = true` ⇒ **`private` vars ARE in the launcher's env.** This is what makes D4 work with zero new mechanism.
- Visibility model: `crates/ocx_lib/src/package/metadata/visibility.rs` — two axes (`private`, `interface`); constants SEALED/PRIVATE/INTERFACE/PUBLIC.

### Launcher generation — `crates/ocx_lib/src/package_manager/launcher/generate.rs` + `body.rs`
- Generates one launcher per entry. Body bakes `pkg_root` only, forwards `argv` to `ocx launcher exec`. **No target/args baked.**
- `body.rs` has golden tests + a wire-ABI canary (`launcher exec` token) paired with `crates/ocx_shim/src/core.rs` `WIRE_SUBCOMMAND`. **Baked-args change does NOT touch the launcher body** (resolution happens inside `launcher exec`), so these goldens should NOT change. If a diff touches them, something went wrong.

---

## 3. What works today vs what's missing

**Works:**
- Different executable per entrypoint via `command` slug (PATH-resolved).
- User-arg forwarding (`mytool foo bar` → `foo bar` reach the binary). Uniform across launcher, `ocx run`, `ocx package exec`.
- Cross-layer dispatch (dep binary on composed PATH via interface visibility).

**Missing (this feature):**
- Baked/fixed args on an entrypoint (`run script.py`).
- Any interpolation in entrypoint fields (`TemplateResolver` unwired from entrypoints).

---

## 4. Agreed design decisions + rationale

### D1 — `command`: slug, PATH-resolved, NO interpolation
- Composed PATH is the resolution authority; visibility decides reachability (own `content/bin`, or dep bin via `interface`/`public`; private deps also reachable under launcher's `self_view=true`).
- Hardwiring `${installPath}/bin/foo` = a second authority that bypasses visibility, and breaks Windows (no PATHEXT extension resolution).
- Keeps `command` typed as `EntrypointName` — no path-injection surface.
- If a publisher needs an absolute dispatch path → smell; fix via visibility. (Doc note, not a feature.)

### D2 — add `args: Vec<String>`, array form
- Each element = one argv token. No shell-split, no quoting hell. Reject any single-command-line-string alternative.
- Additive + backward-compatible: `#[serde(default, skip_serializing_if = "Vec::is_empty")]` keeps existing `{}` and `{"command": ...}` byte-identical.

### D3 — args interpolation: `${installPath}` ONLY
- `${installPath}` is self-referential (files YOU shipped) — zero coupling. The dominant real need (script location, assets).
- `${deps.NAME.installPath}` in an arg reaches into a dep's internal layout → same anti-pattern as D1, bypasses the dep's env/visibility exposure contract. Deps should expose paths via interface env vars; the consuming tool reads those at runtime.
- Asymmetry vs env values (which DO allow `${deps.*}`) is justified by layer: env values are the **declarative composition** surface (dep refs belong there); args are **imperative invocation** params (consume the composed env, don't re-derive dep paths).
- YAGNI: add `${deps.*}` later only if a genuine dep-path-in-arg case appears (additive).

### D4 — NO per-entrypoint env; use `env` block + `private` visibility
- The env block + visibility already does this. Private vars compose into the launcher's `self_view=true` env. `UV_FROZEN`, cache dir, python pin → `constant` vars, `visibility: "private"`.
- `private` = "package needs it, consumers don't inherit it" — exactly correct (UV_FROZEN must never leak to a dependent package).
- Per-entrypoint env = second env authority + precedence rules. Defer until a real multi-entrypoint-divergent-env case. One env authority, not two.

### D5 — config division
- **Package-uniform** (`UV_FROZEN`, cache, python pin) → `env` block.
- **Per-entrypoint variation** (which project, which script, venv path) → `args` (e.g. `uv run --project ${installPath}/proj-a ${installPath}/proj-a/app.py`).
- Consequence: env-driven config is one-value-per-package, so "one env-driven uv project per package." But per-entrypoint args can select different projects → **multiple pyproject.tomls per package IS expressible via args.** True residual limit only if a knob is env-only (no flag) AND must differ per entrypoint — that's the precise tripwire to revisit D4. None known for uv.

---

## 5. OPEN: venv-writable-state — direction A vs B (architect to decide)

`${installPath}` is the read-only, hardlinked CAS content store. uv's needs split:

| uv op | Location | OK? |
|---|---|---|
| read `pyproject.toml`/`uv.lock` | `${installPath}` | ✓ |
| update lock | — | blocked by `UV_FROZEN=1` ✓ |
| create/use venv (`UV_PROJECT_ENVIRONMENT`) | WRITE | ✗ not installPath |
| cache (`UV_CACHE_DIR`) | WRITE | ✗ not installPath |

Two directions:
- **A — runtime resolve:** ship pyproject+lock; uv builds venv at first run into a writable dir (XDG cache or an ocx-provided per-package state dir); `UV_FROZEN` keeps the lock stable. Cost: needs network on first run (against ocx offline/hermetic ethos) or a pre-warmed cache; ocx must provide a writable per-package state location.
- **B — fully vendored:** ship deps inside the package; run `python` directly (or `uv run --no-sync`); uv used only at publish/build time. Aligned with ocx (immutable, offline, content-addressed). Matches NucaChance's own "include all needed libs … more inline with the project's direction."

This is orthogonal to D1–D5 (the metadata feature is needed either way) but decides what the manual test ships and whether the uv-run case is viable as first imagined. **Lean: B** for an ocx-native python package, but architect should weigh.

---

## 6. Implementation surfaces (for the eventual plan)

Code:
- `crates/ocx_lib/src/package/metadata/entrypoint.rs` — add `args` field + accessor + doc comments + `JsonSchema` (the manual `Entrypoints` schema string mentions field semantics — update it) + validation + unit tests.
- `crates/ocx_cli/src/command/launcher/exec.rs` — resolve baked args (installPath-only `TemplateResolver`, empty dep_contexts), prepend before user args. Need install path from `info`.
- `crates/ocx_lib/src/package/metadata/template.rs` — possibly a dedicated entrypoint-arg resolve entry / clearer "deps not allowed" error. Or just reuse with empty deps.
- Possibly an install-time validation pass (publish-time) that interpolates args against installPath to catch bad placeholders early (mirror the env-value validation in `validation.rs`). Architect's call.

Schema:
- `task schema:generate` regenerates `website/src/public/schemas/metadata/v1.json` (gitignored, build-time).

Docs (enumerate per memory pref — plans must list doc surfaces):
- `website/src/docs/reference/metadata.md` — entrypoint section: add `args`, interpolation scope, the `command` slug rule.
- `website/src/docs/reference/command-line.md` — only if `launcher exec` user-facing notes change (it's hidden; likely no).
- `website/src/docs/user-guide.md` — if a "package a script/tool" narrative is warranted.
- New ADR: `.claude/artifacts/adr_entrypoint_args_interpolation.md` (decisions D1–D5 + A/B outcome).

Rules to update (same-commit, per catalog):
- `.claude/rules/subsystem-package.md` (entrypoint.rs row, template.rs row).
- `.claude/rules/subsystem-metadata-schema.md` (if new custom schema bits).
- `.claude/rules/subsystem-package-manager.md` / `subsystem-cli-commands.md` (`launcher exec` dispatch description).

Tests:
- Unit: `entrypoint.rs` (args serde, validation), template wiring (installPath resolves, `${deps.*}` rejected).
- Acceptance: `test/tests/test_entrypoints.py` — new scenario: entrypoint with baked args + `${installPath}` resolves and forwards user args after baked args. Existing `test_launcher_dispatches_divergent_command` + `test_launcher_invocation_runs_target_and_forwards_args` are the patterns to extend.
- Manual: `test/manual/packages/uv-run-python` (or per A/B) — `metadata.in.json` + script, `@@FQ_DIGEST@@` convention (see existing `cross-layer-entrypoint/metadata.in.json`).
- Golden launcher bodies (`body.rs`) + wire-ABI canary: **must NOT change** (resolution is in `launcher exec`, not the launcher body). Use as a guardrail.

Backward-compat: additive field; pre-1.0 so no migration prose in docs (memory pref). Empty `args` omitted on serialize.

Scope clarifier for the architect: baked args are an **entrypoint/launcher** concept — they apply when invoking an installed launcher (candidate/current symlink on user PATH → `ocx launcher exec`). They do NOT apply to `ocx run -- cmd` / `ocx package exec -- cmd`, which take a user command directly and never consult the entrypoint command/args mapping.

---

## 7. Prior art to read first
- `.claude/artifacts/adr_package_entry_points.md` — the entrypoints design (stable surfaces, launcher ABI).
- `.claude/artifacts/adr_deps_name_interpolation.md` — `${deps.NAME.*}` interpolation design (informs the D3 exclusion).
- `.claude/rules/subsystem-package.md`, `subsystem-package-manager.md`, `subsystem-cli-commands.md`.
- Target metadata shape (agreed):
  ```json
  {
    "type": "bundle",
    "version": 1,
    "env": [
      { "key": "UV_FROZEN", "type": "constant", "value": "1", "visibility": "private" }
    ],
    "dependencies": [
      { "identifier": "ocx.sh/astral/uv:0.5@sha256:...", "visibility": "interface", "name": "uv" }
    ],
    "entrypoints": {
      "mytool": { "command": "uv", "args": ["run", "${installPath}/app/main.py"] }
    }
  }
  ```
