# ADR: Entrypoint Baked Args + `${installPath}` Interpolation

## Metadata

**Status:** Proposed
**Date:** 2026-06-28
**Deciders:** Michael Herwig (architect: /architect, opus)
**GitHub Issue:** [ocx-sh/ocx#83](https://github.com/ocx-sh/ocx/issues/83) — **re-scope** (original `target`-template design is dead; see §1)
**Handover:** [`handover_entrypoint_args_env.md`](./handover_entrypoint_args_env.md)
**Related ADRs:** [`adr_package_entry_points.md`](./adr_package_entry_points.md) (entrypoints + launcher ABI), [`adr_deps_name_interpolation.md`](./adr_deps_name_interpolation.md) (`${deps.NAME.*}` in env values — informs the D3 exclusion)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024 / Tokio; no new language, framework, or dependency)
**Domain Tags:** metadata · package-manager · cli
**Reversibility Classification:** **Two-Way Door.** `args` is an additive-optional field; `${installPath}`-only interpolation is a strict subset of the env-value token grammar. Widening later (e.g. adding `${deps.*}` to args) is additive. No launcher wire-ABI change — resolution happens inside `ocx launcher exec`, the launcher body is untouched.

---

## Context

OCX entrypoints already let a package expose an invocable name that dispatches a
*different* binary: `Entrypoint { command: Option<EntrypointName> }`, resolved on
the composed `PATH` by `Entrypoints::dispatch_command(name)` inside
`ocx launcher exec` (`crates/ocx_cli/src/command/launcher/exec.rs`). This was the
core ask of issue #83 — and it already shipped.

What is still missing is the ability to bake **fixed leading arguments** into an
entrypoint. The driving use case (issue #83, NucaChance): run a Python script via
an interpreter, i.e. `uv run <script>` or `python <script>`, from an installed
launcher. Today an entrypoint can dispatch `python` but cannot say "…and always
pass `app/main.py` as the first argument." The only workaround is a hand-written
wrapper script shipped inside the package — exactly the indirection entrypoints
exist to remove.

This ADR adds:

1. An additive `args: Vec<String>` field on `Entrypoint`.
2. `${installPath}` interpolation in those args (and **only** that token).

so that:

```json
"entrypoints": {
  "mytool": { "command": "python", "args": ["${installPath}/app/main.py"] }
}
```

dispatches `python <content>/app/main.py "$@"` — baked args first, user args appended.

### Scope boundary (firm)

Baked args are an **entrypoint/launcher** concept. They are resolved and prepended
**only** on the launcher path (`ocx launcher exec '<pkg-root>' -- <argv0> [args…]`).
They do **not** apply to `ocx run -- cmd` or `ocx package exec -- cmd`, which take a
user-supplied command directly and never consult the entrypoint command/args
mapping. This keeps the change localized to one runtime site.

---

## Decision Drivers

- **Backend-first** — launchers run in CI loops; resolution must be cheap and
  shell-free. Args pass literally to `execvp` (no word-splitting, no `$VAR`).
- **Environmental composition is the resolution authority** — `command` resolves on
  the composed `PATH` gated by visibility. Baking an absolute interpreter path would
  create a *second* authority that bypasses visibility and breaks on Windows
  (no PATHEXT resolution). The feature must not erode this.
- **Offline-first / immutable / content-addressed** — `${installPath}` is the
  read-only, hardlinked CAS content store. Any runtime that needs to *write*
  (a venv, a cache) cannot write there; the design must not pretend otherwise.
- **YAGNI / KISS** — interpolation in args should cover the dominant real need
  (locating files the package itself shipped) and nothing speculative.
- **No launcher wire-ABI churn** — the `launcher exec` golden bodies + the shim
  wire canary (`subsystem-package-manager.md`) must not move.

---

## Industry Context & Research

The pattern "wrapper that injects fixed leading flags, then forwards user args" is
mature and ubiquitous; this is **boring technology**, no innovation token spent:

| Tool | Mechanism | Baked args? | Interpolation? |
|------|-----------|-------------|----------------|
| **Nix `makeWrapper --add-flags`** | shell wrapper prepends fixed flags, `exec "$@"` | **Yes** — direct analog | Store path baked at build |
| Python `console_scripts` / pipx | entry point = `module:func` | Yes (the callable) | n/a |
| Git aliases (`alias.co = checkout -q`) | prepend subcommand + flags | Yes | No |
| npm `cmd-shim` / bin | shim forwards `$@` | No (pure forward) | No |
| Cargo `[[bin]]` | name → binary | No | No |

**Key insight:** Nix's `makeWrapper --add-flags` is the precise precedent — a wrapper
that bakes fixed leading args and interpolates the build-time store path. OCX's
`command` + `args` + `${installPath}` is the same shape, expressed declaratively in
metadata instead of a generated shell script. No surveyed tool exposes a *dependency*
path inside the baked args; they reference only the package's own tree — which is
exactly the D3 restriction below.

---

## §1. Issue #83 freshness — what is stale

Issue #83 predates the `command` field. Two of its three factual claims are no
longer true (verified against current code):

| Issue claim | Reality |
|---|---|
| A `target` template (`${installPath}/bin/foo`, `${deps.NAME.installPath}/bin/foo`) is parsed at install in `generate.rs` | **No `target` field exists.** `generate.rs` validates only a safe `pkg_root` + entry name. |
| `exec.rs` recursively `resolve_command`s the launcher basename → hang | **Stale.** `exec.rs` maps `argv0`→divergent command via `dispatch_command`, then resolves on `PATH`. |
| `target` is decorative; can't expose a dep binary under a different name without a shim | **False.** The `command` field does exactly this. |

The `command` field already resolved #83's core complaint. The live work is the
narrower **baked-args + `${installPath}` interpolation** in this ADR. The manual
workaround `test/manual/packages/cross-layer-entrypoint` (a `bin/wrap-leaf-a` shim,
empty `{}` entry) is now obsolete and can drop the shim for `{"command": "leaf-a"}`.

---

## Decisions

### D1 — `command` stays a PATH-resolved slug; NO interpolation, NO path form

`command` remains `Option<EntrypointName>` (slug `^[a-z0-9][a-z0-9_-]*$`), resolved
on the composed `PATH` by `dispatch_command`. It is **not** interpolated and cannot
be a path.

**Rationale.** The composed `PATH` + visibility is the single resolution authority:
a package reaches its own `content/bin`, a dep's binary via `interface`/`public`,
or any private dep via the launcher's `self_view=true` surface. Hardwiring
`${installPath}/bin/foo` into `command` would (a) bypass visibility, (b) break
Windows PATHEXT resolution, (c) re-introduce a path-injection surface that the
`EntrypointName` newtype currently forecloses. If a publisher feels they need an
absolute dispatch path, that is a visibility smell — fix the exposure, not the
command. (Documentation note, not a feature.)

### D2 — add `args: Vec<String>`, array form

```rust
pub struct Entrypoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<EntrypointName>,
    /// Fixed leading arguments prepended before user-supplied args when the
    /// generated launcher dispatches `command`. Each element is one argv token
    /// (no shell word-splitting). `${installPath}` is interpolated to the
    /// package's content directory; no other interpolation is supported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
}
```

Array form (not a single command-line string): each element is one argv token, so
there is no shell-split or quoting ambiguity — paths with spaces just work. Reject
any single-string alternative.

`#[serde(default, skip_serializing_if = "Vec::is_empty")]` keeps existing `{}` and
`{"command": …}` **byte-identical** on the wire (additive, backward-compatible).
`args` may appear with or without `command`; absent `command` ⇒ dispatch the
entrypoint name with baked args.

### D3 — args interpolation: `${installPath}` ONLY (no `${deps.*}`)

Args support exactly one token, `${installPath}` → the consuming package's `content/`
directory. `${deps.NAME.installPath}` is **rejected** (at publish time with a
dedicated error; see Error Taxonomy).

**Rationale.** `${installPath}` is self-referential — the files *you* shipped — with
zero coupling; it is the dominant real need (script location, bundled assets). A
`${deps.*}` token in an arg reaches into a dependency's internal layout, bypassing
the dep's declared env/visibility exposure contract — the same anti-pattern as D1.
Dependencies should expose paths through their **interface env vars**; the consuming
tool reads those at runtime.

The asymmetry vs env values (which *do* allow `${deps.*}`) is justified by layer:
env values are the **declarative composition** surface (dep refs belong there); args
are **imperative invocation** params that consume the already-composed env. YAGNI:
add `${deps.*}` to args later only if a genuine dep-path-in-arg case appears — that
widening is additive and non-breaking.

### D4 — NO per-entrypoint env; use the `env` block + `private` visibility

Package-wide knobs (`UV_FROZEN`, a cache dir, a python pin) are declared as
`constant`/`path` vars in the existing `env` block with `visibility: "private"`.
The launcher composes with `self_view=true`, so `private` vars **are** present in
the launcher's environment (`composer::compose` → `has_private()`), with zero new
mechanism. `private` is the exactly-correct axis: "this package needs it; consumers
must not inherit it" (a `UV_FROZEN` must never leak to a dependent package).

A per-entrypoint env block would add a second env authority plus precedence rules.
Defer until a real multi-entrypoint-divergent-env case exists. One env authority.

### D5 — config division

| Kind | Surface |
|------|---------|
| **Package-uniform** (`UV_FROZEN`, cache dir, python pin, `PYTHONPATH`) | `env` block (`private`/`interface`) |
| **Per-entrypoint variation** (which project, which script) | `args` (e.g. `["run", "${installPath}/proj-a/app.py"]`) |

Consequence: env-driven config is one value per package, so "one *env-driven* uv
project per package." But per-entrypoint `args` can select different scripts/projects
under `${installPath}`, so **multiple project layouts per package are expressible via
args**. The only residual limit is a knob that is env-only (no CLI flag) *and* must
differ per entrypoint — none is known for uv. That single condition is the precise
tripwire that would justify revisiting D4.

### D6 — interpolation capability model: `Usage` intent → `AllowedTokens` → one engine

The two interpolation consumers permit different token subsets:

| Consumer | `${installPath}` | `${deps.NAME.*}` |
|---|---|---|
| env values | ✓ | ✓ |
| entrypoint args | ✓ | ✗ |

**Do not** encode "args forbid deps" implicitly (passing an empty `dep_contexts`
map) nor by a hand-rolled second scan in validation. Both duplicate logic and make
the policy invisible/misleading (a `${deps.*}` in an arg would surface as
`UnknownDependencyRef { declared: [] }`).

Instead, model the subset as a **first-class capability**, with the call site
expressing *intent* and the engine operating on *capabilities*:

```rust
// template.rs — capability set the engine actually gates on (SRP: no consumer identity)
#[derive(Clone, Copy)]
pub struct AllowedTokens { pub deps: bool }   // installPath is always allowed

// intent enum the call sites use; maps into the capability set
pub enum Usage { Environment, EntryPointArgs }
impl From<Usage> for AllowedTokens {
    fn from(u: Usage) -> Self {
        match u {
            Usage::Environment   => AllowedTokens { deps: true },
            Usage::EntryPointArgs => AllowedTokens { deps: false },
        }
    }
}

// resolver accepts either an intent or a raw cap set; internals only see AllowedTokens
impl<'a> TemplateResolver<'a> {
    pub fn usage(self, u: impl Into<AllowedTokens>) -> Self { /* set self.allowed */ }
}
```

- Call site reads `TemplateResolver::new(content, deps).usage(Usage::EntryPointArgs)`.
- A recognized-but-disallowed token (e.g. `${deps.*}` under `EntryPointArgs`) yields a
  single clear engine error — `TemplateError::DisallowedToken { token }` — used by
  **both** runtime-resolve and publish-validate. No empty-map trick, no duplicated scan.
- **Gate-before-regex (correctness, not just UX — validated by research).** The cap
  check must fire as the **first** step when `!allowed.deps` and the template contains
  `${deps.`, *before* `DEP_TOKEN_PATTERN` matching/substitution. Reason: `${deps.*}`
  matches the dep regex, so an ungated resolver handed real `dep_contexts` in args
  context would actually *resolve* a dep path — violating D3. The gate (not an empty
  map) is what makes passing real `dep_contexts` safe. Without early gating the case
  also collapses to a misleading `UnknownPlaceholder` — the documented GitHub Actions
  mistake (`Unrecognized named-value` for a disallowed-but-known token). See
  [`research_interpolation_capability.md`](./research_interpolation_capability.md).
- **One tokenizer** scans `${…}` segments and classifies them (`InstallPath` / `Dep` /
  unknown); `resolve` (runtime, substitutes) and `validate` (publish, no FS) both drive
  it. `validate_env_tokens` is refactored to route through it (Two-Hats: refactor +
  characterization tests **first**, then add the args consumer).
- Extensibility: new consumer = new `Usage` variant + `From` arm; new token kind = new
  `AllowedTokens` field + tokenizer arm. No trait-object registry (YAGNI for two kinds).

---

## Tension — venv-writable-state: direction A vs B

`${installPath}` is read-only, hardlinked CAS. A Python runtime's needs split:

| uv/python op | Location | Read-only OK? |
|---|---|---|
| read `pyproject.toml` / `uv.lock` / script / `site-packages` | `${installPath}` | ✓ |
| keep lock unchanged | — | `UV_FROZEN=1` ✓ |
| create/sync venv (`UV_PROJECT_ENVIRONMENT`) | needs WRITE | ✗ not `${installPath}` |
| cache (`UV_CACHE_DIR`) | needs WRITE | ✗ not `${installPath}` |

### Option A — runtime-resolve (uv builds the venv at first run)

Ship `pyproject.toml` + `uv.lock`; `uv run` creates the venv at first invocation
into a writable directory; `UV_FROZEN=1` pins the lock.

| Pros | Cons |
|------|------|
| Smaller package (no vendored deps) | Needs network on first run (against offline/hermetic ethos) **or** a pre-warmed cache |
| Familiar `uv run` flow | OCX must define + manage a **writable per-package runtime state directory** — a new storage concept (GC implications, cache invalidation, path discovery) for one use case |
| | uv still creates a venv; the writable location is the publisher's problem unless ocx provides one |

### Option B — vendored, venv-free (RECOMMENDED)

Vendor dependencies into the package at build/publish time; at runtime run the
interpreter directly against a vendored `site-packages`, no venv, no uv:

- **Build (publisher):** `uv pip install --target site-packages …` (or export+vendor
  wheels) into the package tree; uv is used only at publish time.
- **Metadata:** `env: PYTHONPATH = ${installPath}/site-packages` (path, `private`);
  `dependencies: [python (interface)]`; `entrypoints: { mytool: { command: "python",
  args: ["${installPath}/app/main.py"] } }`.
- **Runtime:** launcher → `ocx launcher exec` → composes env (`PYTHONPATH` set,
  `python` on `PATH`) → `execvp python <content>/app/main.py "$@"`. No venv, no write,
  fully offline.

| Pros | Cons |
|------|------|
| Aligns with offline-first, immutable, content-addressed, clean-env principles | Larger package (deps vendored) |
| Zero new ocx machinery beyond `args` | Native-extension wheels are platform-specific → publish per-platform |
| Works today; fully hermetic | — |
| Matches NucaChance's own steer ("include all needed libs … more inline with the project's direction") | — |

### Decision Outcome — **B (vendored, venv-free)**

**Chosen:** Option B as the recommended Python-packaging pattern; **defer A.**

**Rationale.** The `args` metadata feature ships **regardless** — it is needed for
both A and B. The A/B choice only decides *how the Python use case is packaged*, and
A's prerequisite (an ocx-managed writable per-package state directory) is a large new
storage surface — GC reachability, cache invalidation, a discovery contract — spent
on a single use case. That is an innovation-token spend and a YAGNI violation. B
delivers the use case today with no new ocx concept, stays hermetic and offline, and
is the content-addressed-native shape.

**Native wheels.** When a dependency ships a native extension, publish per platform
using OCX's existing multi-platform manifest support (one identifier, per-OS/arch
package). Document this caveat in the user guide.

**A is not foreclosed.** Should a real need arise, "ocx-provided writable per-package
runtime state dir" is a separate, additive future feature; the `args` work here does
not block it and requires no rework. An advanced publisher can already approximate A
today by pointing `UV_PROJECT_ENVIRONMENT` at a writable location of their choosing
(e.g. under `$TMPDIR`) with `UV_FROZEN=1` — documented as advanced, not the default,
and not the manual test.

---

## Component Contracts

Each is a testable invariant (tester writes failing tests before implementation;
reviewer checks the final diff satisfies each).

1. **`Entrypoint` deser with `args`** — `{"command":"python","args":["run","x"]}`
   → `Entrypoint { command: Some("python"), args: ["run","x"] }`.
2. **`Entrypoint` deser without `args`** — `{}` and `{"command":"x"}` → `args == []`;
   round-trip serialization is **byte-identical** to today (empty `args` omitted).
3. **`Entrypoint` accessor** — `args()` returns `&[String]`.
4. **`args` without `command`** — `{"args":["--flag"]}` is valid; dispatch command is
   the entrypoint name; baked args still apply.
5. **Runtime prepend order** — launcher invoked as `mytool U1 U2` with
   `args:["B1","B2"]` runs `command B1 B2 U1 U2` (baked first, user appended,
   left-to-right, no reordering).
6. **`${installPath}` resolves in args** — `["${installPath}/app/main.py"]` →
   `["<content>/app/main.py"]` where `<content>` is `info.dir().content()`.
7. **Multiple/embedded tokens** — `["--root=${installPath}", "${installPath}/x"]`
   each resolve; `${installPath}` may appear more than once per element.
8. **`${deps.*}` in args rejected at publish** — any `${deps.…}` token in any arg →
   `ValidMetadata::try_from` errors with the dedicated "deps not allowed in entrypoint
   args" message naming the entrypoint and the offending arg.
9. **Unknown placeholder in args rejected at publish** — `${installpath}` (wrong
   case), `${foo}` → publish-time `UnknownPlaceholder`-class error.
10. **Runtime defense-in-depth** — a `${deps.*}` arg that somehow reached runtime
    resolves through a `TemplateResolver` built with an **empty** `dep_contexts`
    map and fails (never silently passes through or resolves).
11. **Launcher body unchanged** — `body.rs` goldens + the shim wire canary are
    untouched by this change (guardrail: if the diff touches them, the design was
    violated).
12. **`composer::check_entrypoints` unaffected** — collision gate uses `eps.names()`
    only; adding `args` changes nothing there.
13. **Schema** — `task schema:generate` emits `args` as an additive-optional
    `array<string>` on the entrypoint value object; the `Entrypoints` manual schema
    description mentions `args`.

---

## Error Taxonomy

Two publish-time rejections, surfaced from `ValidMetadata::try_from` via a new
`validate_entrypoint_args` pass in `metadata/validation.rs` (sibling to
`validate_env_tokens`), wrapped in a new `package::error::Error` variant carrying
entrypoint context:

```rust
// package/error.rs — parallels EnvVarInterpolation { var_key, source }
EntrypointArgInterpolation {
    entrypoint: String,   // invocable name
    arg: String,          // the offending raw arg element
    source: TemplateError,
}
```

- **`${deps.*}` in an arg** → a **dedicated, clear** engine error, NOT the misleading
  `UnknownDependencyRef { declared: [] }` (undeclared) nor `UnknownPlaceholder` (typo).
  Add `TemplateError::DisallowedToken { token: String }` (carries the offending token
  text) with message: `token '{token}' is not permitted here; '${deps.*}' is only valid
  in env values`. Classify as `ExitCode::DataError` (65). Raised by the engine's
  capability gate (D6) under `Usage::EntryPointArgs`, fired before the dep regex — so the
  same message fires at publish and at runtime, from one place, and a real `dep_contexts`
  can never be substituted into an arg.
- **Other unknown `${…}`** (wrong case, typo) → reuse
  `TemplateError::UnknownPlaceholder` (already `DataError`).

Detection is driven by the **shared tokenizer** (D6) under `Usage::EntryPointArgs`,
not a hand-rolled per-consumer scan: a `Dep` token under a cap set with `deps: false`
→ `DepsNotAllowed`; any unrecognized `${…}` → `UnknownPlaceholder`. No filesystem
access at publish time (pure syntax check); `validate_env_tokens` keeps its
dep-declared/ambiguous/field checks layered on top of the same scan.

Error message style follows `C-GOOD-ERR` (lowercase, no trailing period, acronyms
preserved) per `quality-rust-errors.md`.

---

## Runtime Resolution (the one behavioral change)

In `crates/ocx_cli/src/command/launcher/exec.rs::execute`, after the existing
`command = …dispatch_command(argv0)` step:

1. The content path = `${installPath}` anchor is `validated.join("content")`
   (`validated` is the canonical pkg-root already in scope; `info` is consumed by
   `resolve_env` at the line above, so derive the path from `validated` rather than
   re-ordering the move). Equivalent to `info.dir().content()`.
2. Look up the `Entrypoint` for `argv0` (`Entrypoints::get`); if it has `args`,
   resolve each element with
   `TemplateResolver::new(&content_path, &HashMap::new()).usage(Usage::EntryPointArgs)`.
   Pass an **empty** `dep_contexts` — none is in scope at exec time, and none is needed.
   The capability gate (D6) rejects any `${deps.*}` with `DisallowedToken` *regardless*
   of whether `dep_contexts` is populated; the gate — not the empty map — is the safety
   mechanism (so a hypothetical populated map would also be rejected). Do not
   reconstruct a dep-context map from `info`.
3. Prepend resolved baked args before the forwarded user `args`; pass the combined
   slice to `run_with_env` → `child_process::exec(&resolved_command, &full_args, env)`.

Resolution is `${installPath}`-only and shell-free; args reach `execvp` literally.
No change to `run_with_env`'s signature beyond the args slice it already receives.

A small accessor is needed on `Entrypoints` to fetch the full `&Entrypoint` for a
name (today only `dispatch_command(name) -> &str` is exposed). Add
`Entrypoints::get(name) -> Option<&Entrypoint>` (or `entry(name)`); `dispatch_command`
can be expressed in terms of it without behavior change.

---

## Schema Evolution Stance

Additive-optional only. Old `ocx` reading new metadata with `args` parses and ignores
the field (it deserializes into an `Entrypoint` that older code lacks) — same forward-
compat path as `dependencies`/`command` before it. Empty `args` is omitted on
serialize. No removal, no rename, no compat shim. Pre-1.0: no migration prose in user
docs (per `feedback_no_migration_prose_in_docs`).

---

## UX Scenarios

| Scenario | Outcome |
|---|---|
| `{command:"python", args:["${installPath}/app/main.py"]}`, launcher `mytool a b` | runs `python <content>/app/main.py a b` |
| `{args:["--config","${installPath}/cfg.toml"]}` (no `command`), launcher `tool x` | runs `tool --config <content>/cfg.toml x` |
| `args:["run","${deps.uv.installPath}/x"]` | **`package create`/`push` fails**: deps not allowed in entrypoint args |
| `args:["${installpath}/x"]` (wrong case) | `package create`/`push` fails: unknown placeholder `${installpath}` |
| existing package `{}` / `{command:…}` | unchanged byte-for-byte; installs + dispatches as today |
| `ocx package info <pkg>` | entrypoint node shows `command` annotation; optionally `args` (display-only, see surfaces) |

---

## Security

- Baked args are publisher-authored and resolved with **one** injected value,
  `${installPath}` — an ocx-controlled, content-addressed path. The template engine
  already rejects an `install_path` that itself contains `${` (`UnknownPlaceholder`,
  defense-in-depth, see `template.rs`).
- Args pass to `execvp` **literally** (`child_process::exec` — `env_clear` + `envs`,
  no shell), so there is no word-splitting or `$VAR`/`%VAR%` expansion surface.
- No new attack surface beyond the existing env-value `${installPath}` substitution.
  Worth a light security-auditor pass on the new validation + runtime prepend, but no
  new class of risk is introduced.

---

## Implementation Surfaces

**Code**
- `crates/ocx_lib/src/package/metadata/entrypoint.rs` — add `args` field + `///` doc +
  `args()` accessor + `Entrypoints::get()` accessor; unit tests (serde, byte-identity,
  args-without-command).
- `crates/ocx_lib/src/package/metadata/validation.rs` — new `validate_entrypoint_args`,
  wired into `ValidMetadata::try_from` after `validate_env_tokens`.
- `crates/ocx_lib/src/package/metadata/template.rs` — add `Usage` enum +
  `AllowedTokens` + `From<Usage>` + `.usage()` builder; add
  `TemplateError::DisallowedToken { token: String }`; classify `DataError`.
- `crates/ocx_lib/src/package/error.rs` — add `EntrypointArgInterpolation` variant
  (carries `entrypoint`, `arg`, `#[source] TemplateError`).
- `crates/ocx_cli/src/command/launcher/exec.rs` — resolve baked args (empty
  dep_contexts) + prepend before user args.
- `crates/ocx_cli/src/api/data/package_inspect.rs` — *(optional)* annotate `args` in
  the entrypoint inspect node.

**Schema**
- `task schema:generate` → regenerates `website/src/public/schemas/metadata/v1.json`
  (gitignored, build-time). Update the `Entrypoints` manual-schema description prose to
  mention `args`.

**Docs**
- `website/src/docs/reference/metadata.md` — entrypoint section: add `args`, the
  `${installPath}`-only rule, the `command` slug rule, the deps-not-allowed note.
- `website/src/docs/user-guide.md` — *(if warranted)* a "package a script/tool"
  narrative: vendored venv-free Python pattern (Option B), the per-platform native-
  wheel caveat.
- `website/src/docs/reference/command-line.md` — only if `launcher exec` user-facing
  notes change (hidden subcommand; likely no).

**Rules (same-commit, per catalog)**
- `.claude/rules/subsystem-package.md` — `entrypoint.rs` row (mention `args` +
  `${installPath}` interpolation), `template.rs`/`validation.rs` rows.
- `.claude/rules/subsystem-metadata-schema.md` — if any new custom schema bits.
- `.claude/rules/subsystem-package-manager.md` / `subsystem-cli-commands.md` —
  `launcher exec` dispatch description (now resolves + prepends baked args).

**Tests**
- Unit: `entrypoint.rs` (args serde + byte-identity), `template.rs`/`validation.rs`
  (`${installPath}` resolves; `${deps.*}` rejected with dedicated error; unknown
  placeholder rejected).
- Acceptance: `test/tests/test_entrypoints.py` — extend
  `test_launcher_invocation_runs_target_and_forwards_args` pattern: an entrypoint with
  baked `${installPath}` args resolves and forwards user args *after* baked args.
- Manual: `test/manual/packages/<name>` — `metadata.in.json` (+ `@@FQ_DIGEST@@`
  convention) demonstrating the venv-free Option-B shape (or a minimal interpreter +
  baked `${installPath}` script).
- **Guardrail:** `body.rs` goldens + shim wire canary MUST NOT change.

---

## Validation

- [ ] `task verify` passes (no regressions); existing entrypoint serde/dispatch tests
      stay green (byte-identity preserved).
- [ ] New unit tests cover all Component Contracts 1–10, 13.
- [ ] Acceptance test proves baked-args-then-user-args ordering end-to-end (Contract 5/6).
- [ ] `task schema:generate` emits `args` as additive-optional; schema description
      mentions it.
- [ ] `body.rs` goldens + shim wire canary unchanged (Contract 11).
- [ ] Docs updated: `metadata.md` entrypoint section; user-guide Option-B pattern if added.

---

## Consequences

**Positive**
- Entrypoints can run interpreters/multiplexers against shipped scripts/assets — the
  uv/python use case and any "wrapper with fixed leading flags" case (the makeWrapper
  shape) work declaratively, no in-package wrapper script.
- Fully additive; existing packages unchanged byte-for-byte; no launcher ABI churn.
- One resolution authority preserved (D1); deps stay behind their visibility contract
  (D3).

**Negative**
- Option B vendoring grows package size and pushes native-wheel packages to per-
  platform publishing (documented; uses existing multi-platform support).
- A genuine env-only-knob-that-differs-per-entrypoint need would require revisiting D4
  (no such need known for uv).

**Risks**
- Publishers will reflexively try `${deps.*}` in args (it works in env values). The
  dedicated publish-time error (not `UnknownDependencyRef{declared:[]}`) is the
  mitigation — it must name the entrypoint, the arg, and point to "env values only."

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-28 | Michael Herwig (architect) | Initial draft from `handover_entrypoint_args_env.md`; formalizes D1–D5, resolves venv-write tension to Option B, adds dedicated deps-not-allowed error |
