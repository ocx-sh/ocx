# ADR: User-Declarable Environment Variables in the Project Toolchain

<!--
Architecture Decision Record — MADR format.
Owner: Architect (/architect). Handoff: Builder (/builder), QA (/qa-engineer), Security Auditor (/security-auditor).
One-way door on the user-visible `ocx.toml` schema — the group-table shape can only be chosen once.
-->

## Metadata

**Status:** Proposed
**Date:** 2026-07-25
**Deciders:** @michael-herwig, architect
**GitHub:** related #175 (interpolation follow-up), #73 (`${self.installPath}` alias), #193 (Dockerfile env import)
**Relates to:** `adr_project_toolchain_config.md` (the `[tools]`/`[group.*]` schema this extends), `adr_global_toolchain_tier.md` (the `--global` tier), `adr_patch_env_resolution_uniformity.md` (`PatchScope` — the structural precedent this extends), `adr_two_env_composition.md` (interface/private surfaces), `adr_entrypoint_args_interpolation.md` (the `Usage`/`AllowedTokens` gate that deferred interpolation will land on)
**Tech Strategy Alignment:**
- [x] Rust 2024, `thiserror` in lib / `anyhow` in CLI, no new runtime dependencies. Extends an existing user-visible config surface; no new persisted wire format beyond one optional forwarded env key.
**Domain Tags:** cli, package-manager, security, api

---

## Context

`ocx.toml` declares tools. It cannot declare environment variables. Users have three needs it does not serve:

1. **Project-wide constants** — `SOURCE_DATE_EPOCH`, `RUSTFLAGS`, a `NODE_ENV`, a license-server URL that is project-specific rather than site-wide.
2. **Group-scoped constants** — `CI=1` when the `ci` group is selected, a different `SOURCE_DATE_EPOCH` for `release`.
3. **A project-local `PATH` entry** — `node_modules/.bin`, `./scripts`, a vendored helper directory that is not an OCX package and never will be.

Today the only channels are:

- **The ambient shell.** Works on POSIX (`FOO=bar ocx run -- cmd`). Does not work on Windows: neither PowerShell (`$env:FOO='bar'`) nor `cmd.exe` (`set FOO=bar`) has a per-invocation prefix — both mutate session state that persists after the command. Confirmed against Microsoft's environment-provider documentation. It also does not work at all under `--clean`, and it does not work for a caller that builds an argv array (a GitHub Action, a Bazel rule, a Python script) and cannot inject shell syntax. OCX's stated primary users are exactly those callers.
- **A `[patches]` companion package.** Correct for operator/site tier (corporate CA bundle, proxy, license endpoint) and deliberately heavyweight — it requires publishing an OCI artifact. Wrong for "this project needs `CI=1`."
- **`direnv`.** Real, and OCX integrates with it, but it is POSIX-shell-only, it is not read by `ocx run` in CI, and it puts project configuration in a second file outside the one the project already has.

The gap is narrow and well-defined: a declarative, cross-platform, checked-in place to say "this project's tools run with these variables set," plus a programmatic per-invocation override for the tool-for-tools case.

This ADR is a **one-way door on the `ocx.toml` group-table shape**. Every other decision here is reversible pre-1.0; the choice of how `[group.<name>]` grows a second dimension is not, because it determines whether every existing `ocx.toml` in the wild keeps parsing.

---

## Decision Drivers

| # | Driver | Weight |
|---|--------|:---:|
| D-a | ~~**No break for existing `ocx.toml` files**~~ — **retired**, see the note under Considered Options. Weighted 25% against an assumed installed base; the actual base is 2–3 users with near-zero named-group usage. Weight redistributed to D-e | ~~25%~~ 0% |
| D-b | **Uniformity across every env-composing surface** — `run`, `env`, `direnv export` must behave identically or we reproduce the `adr_patch_env_resolution_uniformity.md` bug class | 20% |
| D-c | **Hermeticity of package-composed env is not weakened** — a project-declared variable must not become a channel by which packages absorb host state | 20% |
| D-d | **Self-reconfiguration is impossible** — a checked-in file must not be able to change how `ocx` itself resolves | 15% |
| D-e | **Simplicity now, extensibility later** — v1 ships the smallest thing that serves the three needs; interpolation is a separate, larger decision. One code path, not two | 35% |
| D-f | **Windows parity** — the feature must be fully usable with no POSIX shell anywhere in the picture | 10% |

Constraint: pre-1.0, no back-compat shims for unreleased behavior. But `ocx.toml` is a **published, user-authored surface** — files exist in the wild — so the schema axis specifically does carry a compatibility obligation that internal APIs do not.

---

## Industry Context & Research

**Research artifact:** [`research_project_env_declaration.md`](./research_project_env_declaration.md) — seven tools surveyed (Cargo, mise, direnv, go-task, GitHub Actions, Bazel, docker-compose) plus npm/Nix/asdf/uv negatives.

**Key findings driving this ADR:**

1. **"Most specific layer applied last" is universal.** Taskfile (task > global > OS), GitHub Actions (step > job > workflow), docker-compose (`environment:` > `env_file:`), Bazel (invocation > rc) all agree. Our ordering is mainstream, not novel.
2. **mise is retreating from templating.** Tera functions for task-argument definition are deprecated as of 2026.5.0 with removal in 2026.11.0, explicitly because shell-escaping differs per shell and produced unpredictable behavior. The one tool that went furthest on interpolation is walking it back. This is direct evidence for deferring interpolation rather than merely conservative instinct.
3. **go-task has exactly the bug our path-modifier prevents.** go-task/task#449: hand-rolled `PATH: "{{.X}}:$PATH"` breaks across shells, breaks on Windows separator differences, and double-prepends on re-entry because there is no idempotent add. **OCX wraps go-task** — this is a first-person argument, not an analogy.
4. **Only GitHub Actions protects its own control variables.** Of seven tools surveyed, only GHA hard-blocks `GITHUB_*`/`RUNNER_*` from being set through the same surface that declares user env. Cargo, mise, Taskfile, Bazel and docker-compose all leave it open. mise has two 2026 CVEs of precisely this shape — local untrusted config reaching `credential_command` *before* the trust check ran (GHSA-436v-8fw5-4mj8). Config governing its own governance is a known, materialized vulnerability class.
5. **GitHub Actions shipped the same bug twice.** `::set-env::` stdout injection (deprecated Oct 2020) → replaced by the `$GITHUB_ENV` file channel → that channel then had its own delimiter-breakout vulnerability (CVE-2022-35954, `@actions/core` < 1.9.1). Two generations of one bug class in one feature's history. Any side-channel env write deserves paranoia.
6. **Trust models split on invocation trigger, not on file content.** Tools that activate on `cd` (direnv, mise) gate on explicit trust. Tools that activate on explicit invocation (Cargo, Taskfile, Bazel, docker-compose) do not gate at all — their reasoning being that you already trusted the tool with arbitrary build logic. There is no consensus to inherit; the choice must be made deliberately.

---

## Considered Options — Group-Table Shape (the one-way door)

> **Driver D-a is retired.** D-a ("no break for existing `ocx.toml` files") was weighted 25% on the assumption of an unknown-but-nonzero installed base. Owner input (2026-07-25) supplies the missing number: **two to three users total, and named groups are barely used at all** — most projects declare only the default `[tools]` table, which this change does not touch. The blast radius is a handful of files, each fixable in under a minute by a person who can be told directly. D-a's weight redistributes to D-e (simplicity), which inverts the recommendation from Option A to Option C.

### Option A — Type-discriminated union

Within `[group.<name>]`, a **string** value is a tool binding; a **table** value is a reserved section (`tools` or `env`). Flat and nested merge.

| Pros | Cons |
|------|------|
| Zero break for existing files | **Requires a hand-rolled value-first deserializer** — `#[serde(untagged)]` is unusable per `mirror.rs:34-38` |
| No reserved key names needed | Two accepted spellings, needing either a permanent duality or a deprecation schedule to retire one |
| | Needs a merge rule and a cross-form duplicate error |
| | Serializer must choose which form to emit |
| | A `[group.ci.tolos]` typo needs hand-written unknown-key rejection |

### Option B — Separate top-level table keyed by group

`[env]` for the default group, `[group-env.<name>]` for named groups; `[group.<name>]` untouched.

| Pros | Cons |
|------|------|
| Zero break, zero deserializer work | Two parallel namespaces the user must keep in sync; `[group.ci]` and `[group-env.ci]` drift silently |
| Trivially simple | A typo in the group name creates a silently-orphaned env block with no error |
| | Does not extend to a third per-group dimension |

### Option C — `[group.<name>.tools]` only, breaking (RECOMMENDED)

`[group.<name>]` contains exactly two optional sub-tables, `tools` and `env`. Tool bindings declared directly under `[group.<name>]` are a **parse error**.

```toml
[tools]                       # default group's tools — unchanged
foo = "ocx.sh/foo:1"

[env]                         # default group's env — new
CI = "1"

[group.ci.tools]              # named group's tools
bar = "ocx.sh/bar:1"

[group.ci.env]                # named group's env
SOURCE_DATE_EPOCH = "0"
```

| Pros | Cons |
|------|------|
| **The group value becomes a plain `#[derive(Deserialize)]` struct with `deny_unknown_fields`** — the entire hand-rolled union disappears | Breaks existing files that declare bindings directly under `[group.<name>]` |
| A `[group.ci.tolos]` typo is rejected **by serde, for free** — no hand-written unknown-key branch | |
| No merge rule, no cross-form duplicate error, no serializer form decision, no deprecation schedule, no removal decision to defer | |
| One code path, permanently — the thing dual support was going to cost | |
| A tool named `env` or `tools` is expressible without any special handling: it is a key inside `tools`, which is a plain `BTreeMap<String, Identifier>` | |
| Schema symmetry: `[tools]`/`[env]` at top level, `[group.X.tools]`/`[group.X.env]` nested | |

### Option D — Reserved key names with rejection

Treat `env`/`tools` as reserved binding names in the flat form, error if a tool uses them.

| Pros | Cons |
|------|------|
| Simple mental model | Makes a legitimate tool name unusable — `env` is a real coreutils binary |
| | Solves a problem neither A nor C has |

**Decision: Option C.** With D-a retired, C dominates A on every remaining axis. The decisive point is one that only becomes visible once the flat form is rejected rather than merely deprecated: **Option A's hand-rolled union exists solely to accept the flat form.** Removing the form removes the machinery — the deserializer, the merge rule, the cross-form duplicate error, the serializer's form choice, and the hand-written unknown-key rejection all collapse into one derive with `deny_unknown_fields`. Dual support was never just "two spellings in the docs"; it was two code paths plus a deprecation schedule plus a deferred removal decision, all to spare a handful of files a one-line edit.

This is also what `feedback_refactor_as_if_never_existed` prescribes directly: pre-1.0, refactor as though the removed form never existed — no vestiges, no shims, no compat naming.

---

## Considered Options — Env Value Grammar

### Option A — String shorthand plus typed table (RECOMMENDED)

```toml
[env]
CI = "1"                                            # string → constant
JAVA_OPTS = { type = "constant", value = "-Xmx2g" } # explicit constant
PATH = { type = "path", value = "node_modules/.bin" }
```

`constant` replaces; `path` prepends. Relative values on `type = "path"` resolve against the project root (the directory holding `ocx.toml`); absolute values pass through.

| Pros | Cons |
|------|------|
| Reuses the existing `Modifier` concept OCX already ships for package metadata — one vocabulary, not two | The one hand-rolled string-or-table union in this change (the group table needs none under Option C) |
| PATH prepending is expressible **without** any interpolation, which is what makes deferring interpolation viable | |
| Relative-path resolution against project root removes the need for a `${projectRoot}` token | |
| Matches mise `_.path` and direnv `PATH_add`; closes the go-task#449 gap | |

### Option B — Cargo's `{ value, force, relative }`

| Pros | Cons |
|------|------|
| Proven exactly as-is in a Rust-ecosystem tool | `relative` resolves a path but does **not** prepend — it cannot express "add to PATH", which is need #3 |
| `force` gives per-entry control over ambient precedence | Would still need a separate path mechanism, so it is Option A plus extra |

### Option C — Strings only, `PATH` handled by a separate table

| Pros | Cons |
|------|------|
| Simplest possible v1 | A second table for one modifier is worse than a `type` field |
| | Does not generalize if a third modifier ever appears |

**Decision: Option A**, with `force` semantics folded into the ordering decision below rather than exposed as a per-entry flag (see Q6).

> **This shorthand earns its keep where the flat group form did not, and the test is the same one.** A permanent shorthand is justified when the short form is materially shorter *and* is the common case. `CI = "1"` against `CI = { type = "constant", value = "1" }` clears that bar decisively — constants are the overwhelming majority of entries, and the table form is four times the width. `[group.ci]` against `[group.ci.tools]` did not. Applying one criterion consistently yields opposite answers here, which is the point.

---

## Decision

### Schema

**S1.** `ocx.toml` gains a top-level `[env]` table (the default group's environment) and `[group.<name>.env]` for named groups. Applies identically to the `--global` tier file at `$OCX_HOME/ocx.toml`.

**S2 — `[group.<name>]` holds exactly two optional sub-tables, `tools` and `env`.** Nothing else. A tool binding declared directly under `[group.<name>]` is a parse error (S8). `ProjectConfig.groups` becomes `BTreeMap<String, Group>` where:

```rust
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Group {
    #[serde(default)] tools: BTreeMap<String, Identifier>,   // raw pass: BTreeMap<String, String>
    #[serde(default)] env: ProjectEnv,
}
```

Consequences, all of which are *absences* of machinery an earlier draft required:

- **No hand-rolled group deserializer.** A plain derive suffices; the value-first union pattern is not needed for the group table at all. (It is still needed once, for the env *value* grammar — S5.)
- **Unknown-key rejection is free.** `[group.ci.tolos]` fails via `deny_unknown_fields` with serde's own message naming the offending key. No `UnknownGroupSection` error variant, no hand-written branch, and no risk of copying the `[mirrors]` leniency that degrades an unrecognized key to `EmptyEntry` (`mirror.rs:1031-1040`) — a behavior that would have silently vanished a whole tool set.
- **No reserved names, no merge rule, no cross-form duplicate error.** A tool named `env` or `tools` is just a key inside `Group::tools`, which is a plain map. There is one place bindings can live, so nothing merges and nothing can collide across forms.
- **No serializer form decision.** One shape in, one shape out.

The two-pass parse structure is unchanged: `RawProjectConfig` keeps string-valued maps (`RawGroup { tools: BTreeMap<String, String>, env: ... }`), and `parse_tool_map` (`config.rs:470-506`) validates identifiers on the second pass exactly as today — it simply walks `raw_group.tools` instead of `raw_group` directly.

**S3.** `[group.default]` and `[group.all]` remain rejected at parse (`project/config.rs:406-430`); nothing here changes those reservations. `[env]` at top level *is* the default group's env, exactly as `[tools]` is the default group's tools.

**S8 — the flat form is removed now, not deprecated.**

Tool bindings directly under `[group.<name>]` are a parse error as of this change. No dual-parse window, no deprecation warning, no removal decision deferred to 1.0.0.

**Why break rather than deprecate.** An earlier draft of this ADR proposed a two-stage deprecation (accept both now, warn at 1.0.0, remove later), reasoning that `ocx.toml` is user-authored data in cloned repositories and that a hard break surfaces as a parse error on a file the user never touched. That reasoning was sound in structure and wrong in magnitude: it assumed an unknown-but-nonzero installed base. Owner input supplies the number — **two to three users, and named groups are barely used**; the default `[tools]` table, which the vast majority of projects use exclusively, is untouched by this change. The population that can break is a handful of files belonging to people who can be told directly.

Against that, dual support costs a hand-rolled deserializer, a merge rule, a cross-form duplicate error, a serializer form choice, a hand-written unknown-key branch, a warning whose timing has to be reasoned about against `feedback_no_warn_on_common_benign`, and a removal decision deferred to a release that is months out. Two code paths and a schedule, to spare a handful of files a one-line edit. `feedback_refactor_as_if_never_existed` is directly on point: pre-1.0, refactor as though the removed form never existed — no vestiges, no shims, no compat naming.

**The error message is the entire migration story**, so it carries the fix rather than merely reporting the fault:

```
error: group `ci` declares tool bindings directly
  --> ocx.toml
   |
   |  [group.ci]
   |  bar = "ocx.sh/bar:1"
   |
   = tool bindings belong under `[group.ci.tools]`
   = `[group.ci]` holds only the `tools` and `env` sub-tables
```

Classified `ExitCode::ConfigError` (78). No migration prose in the user docs (`feedback_no_migration_prose_in_docs`) — the docs describe only the current schema; the `CHANGELOG.md` entry is where the breaking change is recorded.

**Passive migration is not available and is not needed.** The rejected draft leaned on `mutate.rs`'s full-file rewrite (`config_to_toml_string`, `mutate.rs:28-31`) to convert files on the next `ocx add`. That path is unreachable when the file fails to parse — `add_binding` loads the config before mutating it. Recorded here because the mechanism is genuinely useful for a *non-breaking* schema normalization and will be reached for again; the precondition is that the old form still parses.

> One fact from that analysis survives and stays load-bearing elsewhere: `declaration_hash` canonicalizes from semantic `name → identifier` pairs (`hash.rs:57-75`), never from TOML text. Reshaping a file's group tables therefore does not change its hash and cannot stale `ocx.lock` — which is why the hand-edit this change forces on those two or three users triggers no re-lock. See H1.

**S9 — the published `project` schema URL stays `v1.json`; its meaning changes.** `ocx_schema` derives the `project` schema from `ProjectConfig` and publishes it at `https://ocx.sh/schemas/project/v1.json` (`ocx_schema/src/lib.rs:52`), and `init_project` writes that URL as a taplo `#:schema` directive on the first line of every generated `ocx.toml` (`mutate.rs:470`, pinned by `init_project_emits_schema_directive_on_first_line`). Existing files on disk therefore point at a URL whose contents this change alters.

Keep `v1`. Minting `v2` would strand every existing file's directive on a URL nobody republishes, which is strictly worse than redefining one that is regenerated on every website build and gitignored in-tree. Pre-1.0, the schema tracks the type; version proliferation is a post-1.0 concern.

The redefinition is a **feature** for the migration: `taplo.toml:12-16` binds `**/ocx.toml` to this URL, so any taplo-enabled editor validates every `ocx.toml` live. The two or three affected files get red-underlined in-editor — with the offending table named — before their owner ever runs `ocx`. Better first contact with this break than a parse error, and it costs nothing.

**S9a — the env value schema must be hand-written, and the reason is a bug already shipped.**

`schemars` cannot infer a schema for a string-or-table union from the *normalized* Rust struct, because the normalized struct only describes the table arm. **The shipped `config` schema almost certainly has exactly this defect today** — stated at high confidence from construction, but see the verification note below:

- `MirrorConfig` (`config/mirror.rs:137`) derives `schemars::JsonSchema` on the normalized `{ registry: Option<String>, index: Option<String> }` form.
- The accepted TOML is a union: a bare string sets both roles, a table splits them (`config.rs:52-58`).
- `taplo.toml:4-10` binds `https://ocx.sh/schemas/config/v1.json` to every `config.toml`.

So the documented, supported bare-string form —

```toml
[mirrors]
"ghcr.io" = "corp.example.com/proxy"
```

— is reported invalid by any taplo-enabled editor. The one struct that hand-rolls its deserializer *specifically* to avoid untagged-union failure modes then misrepresents itself as table-only to every schema consumer.

> **Verification status: CONFIRMED by execution** (2026-07-25). `cargo run -p ocx_schema -- config` emits `$defs.MirrorConfig` as a plain `{"type": "object", "properties": {"registry": …, "index": …}}` with **no `oneOf` and no `anyOf`** — the bare-string arm is absent from the published schema entirely. The struct's own doc comment, carried into the schema `description`, even *describes* the union it fails to encode ("the string-or-table union value it is built from is parsed by `parse_mirror_value` … never by a derive on this struct"). Mechanism as expected: `#[serde(deserialize_with = …)]` is invisible to `#[derive(JsonSchema)]`, which reads only the field's Rust type.

Filed as its own issue; not fixed here, but it is the direct evidence for the requirement below.

**Requirement.** The env value field carries `#[schemars(schema_with = "...")]` emitting an explicit `oneOf: [ {type: string}, {type: object, properties: {type, value}, required: [type, value], additionalProperties: false} ]`. Same technique as `Var.visibility`'s `entry_visibility_schema` (`package/metadata/visibility.rs`), applied for a different reason: `visibility` narrows a type's value set, this one widens a struct's accepted shapes.

The stakes are higher here than for `[mirrors]`. The project schema is bound live to every `ocx.toml`, and the string form is the *common* case — `CI = "1"` is what nearly every entry will look like. A derived schema would red-underline almost every correct `[env]` file in the user's editor. A verification item pins it: **a schema-validation test asserting both `CI = "1"` and `CI = { type = "path", value = "bin" }` validate green against the generated `project` schema.**

**Only the string arm is new work.** `Modifier` (`package/metadata/env/modifier.rs:11-18`) is a `#[serde(tag = "type")]` enum that schemars already derives cleanly into a discriminated `oneOf` — the `{ type = "…", value = "…" }` arm comes for free from the existing type. The hand-written fragment adds the bare-string alternative around it. Style to imitate: the `json_schema!` macro fragments in `metadata/binary.rs:242-253` and `metadata/visibility.rs:196-201`, which emit explicit schema by hand rather than deriving from a struct shape.

Build the union fragment as a **reusable helper**, not an inline one-off. The `[mirrors]` fix then becomes a two-line application of the same helper in a follow-up. Landing both together is not required — what matters is that the helper exists before a second union-schema style can be invented independently.

Add a fourth entry to the "Custom `JsonSchema` Implementations" list in `subsystem-metadata-schema.md`; that rule requires new deviations be documented there.

**S9b — three schema-surface facts the plan must not inherit silently.**

1. **Do not rename the group wire key.** `ProjectConfig.groups` carries `#[serde(rename = "group")]` (`config.rs:74`) — the Rust field is plural, the TOML table is singular, and `[group.<name>]` is what users write. The restructure changes the group *value* shape only. Renaming the key would be a second, gratuitous break landing in the same change.
2. **Tighten the schema test's existing hedge.** `schema_outputs.rs:72` currently tolerates *either* `groups` or `group` as the emitted property name. That hedge predates this change and should be resolved to assert the actual key while the file is being touched anyway — a test that accepts both spellings cannot catch a rename regression.
3. **taplo is effectively dead in CI, so the editor-facing surface has no gate.** The only consumer is `test/tests/test_taplo_project_toolchain.py`, which skips entirely when `taplo` is absent from `PATH` and validates only synthetic `tmp_path` fixtures — never the repository's own `ocx.toml`. Nothing in `verify-basic.yml` or `verify-deep.yml` invokes taplo at all. So S9's claim that editors red-underline the old group form holds for *users* with taplo configured, but CI would not catch a schema/reality divergence. Pinning `taplo` through `ocx.toml` and validating the repo's own file in CI is a cheap, separate win — noted as a follow-up, not scoped here.

**S5.** Env values use the Option A grammar. String → `constant`. Table → `{ type = "path" | "constant", value = "..." }`. `path` prepends via `Env::add_path` (`env.rs:260-273`), which already routes through `utility::path::move_to_front` and is therefore idempotent on re-entry — the go-task#449 failure is structurally impossible. Relative `path` values resolve against the project root; absolute pass through. `PATH_SEPARATOR` (`env.rs:187/190`) handles the Windows separator difference.

**S6.** A new lightweight type in `crates/ocx_lib/src/project/` — **not** a reuse of `package::metadata::env::Var`. `Var` carries a `visibility` axis and `${installPath}` templating, neither of which has project-tier meaning.

> **`[env]` has no visibility axis, and this must be stated in the schema docs.** Package env vars are gated interface/private through `composer::carrier_crosses` (`composer.rs:161-171`) because a package can be a dependency of another package. A project is not a package and is never a dependency of anything, so there is no edge to cross and no surface to gate. A reviewer or user expecting a `visibility` field will not find one, by design. Correspondingly, `--self` has **no effect** on project `[env]` entries: they are emitted on both surfaces.

**S7.** Reuse for application: `Env::set` / `Env::add_path` / `Env::apply_entries` (`env.rs:256-377`), `ModifierKind` for `--format json` output consistency, `utility::path::move_to_front` for PATH dedup.

### Composition

**C1.** Project and group env are **materialized as ordinary `Entry` values** (`package/metadata/env/entry.rs:8-15`) and appended to the `Vec<Entry>` returned by the `resolve_env*` family, after the package-composed entries and after the patch-companion overlay. They are not a parallel channel.

> This is the load-bearing implementation decision and it falls out of a fact the discovery pass established: *every* consumer — `Env::apply_entries` (`run.rs:226`, `exec.rs:84`, `launcher/exec.rs:166`, `package_test.rs:233`, `patch_test.rs:189`), `conventions::emit_lines` (`toolchain_env.rs:343`, `env.rs:141`, `direnv_export.rs:138`), and `conventions::export_ci` (`conventions.rs:206-215`) — consumes exactly one `&[Entry]`. Appending to that vector makes every surface uniform **by construction** rather than by three parallel correct implementations. It is also the smallest diff.

> **Entry order is an observable, pinned contract — verify before relying on append position.** `ocx --format json env` emits the entry list in vector order, and acceptance tests assert that order **exactly**: `_env_json_keys` / `_global_env_json_keys` (`test/tests/test_toolchain_env.py:1072, :1096`) with explicit key-order pins at `:1152` and `:1169`. Appending project/group entries therefore changes observable output and will break those assertions — expected, since the entries are genuinely new, but it means:
>
> 1. Append position is a **semantic** choice, not an implementation detail. C2's ordering must be realized as vector position, and the order-pinned tests updated to encode the new contract deliberately rather than patched until green.
> 2. **Verified, and the CI surface diverges — C1's "uniform by construction" wording is downgraded accordingly.** The append itself stays ratified; what does not hold is that all three consumers realize the same precedence.
>
> **C1a — `ci::prepend_existing` inverts path precedence relative to `Env::add_path`.** `prepend_existing` (`ci.rs:71-80`) prepends the whole buffered block, then `VecExt::unique` keeps the **first** occurrence — so among several path values for one key, the *first accumulated* holds the front. `Env::add_path` calls `utility::path::move_to_front` once per entry (`utility/path.rs:43-65`), so the *last applied* holds the front. An appended stage-4/5 project path entry is therefore **highest** precedence under `ocx run` and **lowest** under `--ci`, directly contradicting C2. GitHub's `$GITHUB_PATH` channel happens to agree (Vec order plus runner LIFO, `github_flavor.rs:59-64`); GitLab has no path channel, so even `PATH` inherits the reversal.
>
> **Decision:** fix the direction in `prepend_existing` — one shared function, ~10 lines — as a **separate commit landing before** the project-env work, deliberately flipping the four tests that pin the current direction. It is a pre-existing inconsistency this feature merely makes visible, and correcting it inside the feature commit would bury a behavior change for existing CI users inside an unrelated diff.
>
> Bucket ordering itself (`github_flavor.rs:66-76` drains `path_entries` → `buffered_paths` → `buffered_constants`; `gitlab_flavor.rs:87-93` paths-then-constants) leaves Constant-after-Path effectively correct, since constants drain last and the later line wins. Path-after-Constant is clobbered — pre-existing, unreachable from `[env]` alone, accepted.

**C2.** Application order:

```
1. ambient inherited env       (skipped under --clean)
2. package-composed env        (group order, then alphabetical within group)
3. patch-companion overlay     (existing, unchanged)
4. project [env]
5. group [env]                 (in -g selection order; later group wins)
6. --env KEY=VALUE             (highest)
```

Stages 4-6 are appended to the same entry vector in that order. Constants replace, path entries prepend — so a stage-4 path entry lands ahead of stage-2 package paths. Stage-5 later-group-wins matches the composition-order rule already documented in `website/src/docs/reference/env-composition.md`.

**C3.** ~~Project env must be applied outside `ConstantTracker`.~~ **Void — the premise was wrong.**

> **Correction from discovery.** `ConstantTracker` (`package/metadata/env/conflict.rs`) is not consulted anywhere in `composer::compose` or the `resolve_env*` family. It is used *only* by the two CI flavor writers (`ci/github_flavor.rs:11,34,48`, `ci/gitlab_flavor.rs:11,40,72`), and there it **warns** rather than blocking. Every non-CI path — `ocx run`, `ocx exec`, `ocx env`, `ocx direnv export`, `ocx launcher exec` — performs silent last-write-wins with **no collision detection at all**.
>
> Consequence: C1's append ordering already produces the intended override semantics with no special handling. There is nothing to route around. The finding is not that the design needed changing but that a **pre-existing gap** was mischaracterized as a guard.

**C4.** Collision reporting. Because C3's tracker does not cover the process-env paths, a project `[env]` key that shadows a package-declared constant is silent today and would remain silent. **Decision: emit a `debug`-level log, not a warning**, on project-over-package constant shadowing. Rationale: shadowing is the *declared intent* of the feature (that is what C2's ordering is for), so warning on the happy path violates `feedback_no_warn_on_common_benign`. The CI flavors' existing `ConstantTracker` warning is scoped to package-vs-package collisions and stays as-is; project entries must be excluded from that tracker so a deliberate override does not surface as a CI warning.

**C5.** Uniformity enforcement extends the `PatchScope` pattern rather than paralleling it. `PatchScope::Project(BTreeSet<String>)` (`package_manager/tasks/resolve.rs:58-85`) already encodes exactly the distinction needed — "is a `ProjectConfig` in scope, and if so what does it contribute" — and is already a **required** parameter on every `resolve_env*` entry point, with every one of the eleven call sites passing an explicit variant. Widening its `Project` payload to carry the project/group env contribution alongside the `no-patches` opt-out makes omitting project env structurally impossible for a new caller, at the cost of one enum-payload change instead of a second parallel required parameter.

> `adr_patch_env_resolution_uniformity.md` carries `Status: Proposed`, but its Resolution section (2026-07-02) and the current source both confirm `PatchScope` is fully implemented and shipped. That ADR's status line should be corrected to `Accepted` as a drive-by in whatever PR lands this.

### CLI

**L1.** `ocx run --env KEY[:TYPE]=VALUE`, repeatable. Split on the **first** `=`, so `FOO=a=b` yields `FOO` → `a=b`. `TYPE` is `constant` or `path`; omitted it is `constant`, so a plain `KEY=VALUE` is unchanged.

> **Amended.** L1 originally read "Constant modifier only — no path form from the CLI in v1" and recorded **no rationale** — unargued minimalism rather than a defended exclusion. Two things argue against it, and both point the same way.
>
> First, the exclusion contradicted **D9**, the argument for the flag existing at all: PowerShell and cmd have no per-invocation env prefix, and OCX is a backend tool for tools that build argv arrays and *cannot inject shell syntax*. A caller who cannot inject shell syntax also cannot write `$PATH` / `%PATH%` into a value — so "prepend a directory to PATH for this invocation" was inexpressible from the CLI. D9 applies to path prepending with **more** force than to constants, since a constant at least has a working spelling.
>
> Second, the natural attempt — `--env PATH=/opt/tools/bin` — is a stage-6 constant, so it *replaces* the composed PATH and every package `bin/` and `entrypoints/` directory disappears with no warning. **That behavior is kept**: no name-based special case for `PATH`, because a name-triggered modifier switch is exactly the kind of surprise `ModifierKind` exists to make explicit. `:path` is now the correct spelling and every doc surface names the hazard.
>
> **The one deliberate divergence from the file form**: a relative `:path` value resolves against the **current working directory**, not the project root. The `ocx.toml` form anchors to the project root (D4) because a checked-in file must mean the same thing from any subdirectory; a flag has no such obligation — it is composed by whatever script is invoking ocx, and cwd is the one base such a script can compute. Both surfaces document the other's rule.
>
> Grammar is unambiguous by construction: `is_valid_env_key` (X2) admits no `:`, so a colon in the segment *before the first `=`* is always a type marker, and a Windows value like `C:\tools\bin` is untouched because only that segment is inspected. An unknown or empty `TYPE` exits **64** (`UsageError`) — CLI misuse — where the file form's `EnvUnknownModifier` exits 78, a config-shape fault *in a file*. The reserved-key gate (X1) runs on the key **after** the qualifier is stripped, so `--env OCX_INDEX:path=…` is still rejected.
>
> No wire-format or schema change: the `OCX_ENV` payload (R1) already carries a `"type"` per entry, so a `:path` override forwards to `ocx launcher exec` unchanged. `ModifierKind` gained a `FromStr` — the inverse of its existing `Display` — so the file parser and the CLI parser share one grammar instead of hand-rolling a second copy.
>
> *Prior art in-repo:* no `KEY:TYPE=VALUE` form existed. The nearest shape is `LayerRef`'s `./libs.tar.gz:strip=1,prefix=share` (`publisher/layer_ref.rs`), where a colon splits a base value from a settings tail — adjacent, not the same.

**L2.** Bare `--env FOO` (docker-style ambient pass-through) is rejected in v1 with `ExitCode::UsageError` (64). It has meaning only under `--clean`, and admitting it later is additive.

> **No in-repo precedent exists for a repeatable `KEY=VALUE` flag.** Discovery grepped the whole `ocx_cli` tree: zero matches. The nearest shape is `project::compose::parse_positional` (`project/compose.rs:93-124`, `[name=]identifier` split on first `=`), but it has **zero callers in `ocx_cli`** — it is unit-tested dead code from the CLI's perspective. This flag will therefore be the first of its kind and needs its own clap `value_parser`. The dead `parse_positional` should be raised as a separate cleanup issue, not folded in here.

**L3.** ~~Scope: `--env` lands on `ocx run` in v1. `ocx env` and `ocx exec` do not get it — `ocx env` emits rather than executes (a caller can post-process), and `ocx exec` is OCI-tier where the ambient shell is the caller's own concern. Additive later if asked.~~

> **L3 AMENDED — `--env` lands on every env-composing command, both tiers.** The escape hatch L3 itself wrote ("additive later if asked") was taken. Two of the original three reasons did not survive contact:
>
> - *"`ocx env` emits rather than executes (a caller can post-process)."* `ocx run` **never** prints — it diverges into `execvp` — so a stage-6 environment was observable only by executing it. Post-processing presupposes something to post-process. OCX is a backend tool for tools (D9): a caller that builds an argv array must be able to **export** the environment it would otherwise **execute** in, and until now it could not. The pinning test is differential — `ocx env --env X` and `ocx run --env X` are asserted against one shared oracle, so a divergence between them fails.
> - *"`ocx exec` is OCI-tier where the ambient shell is the caller's own concern."* This conflated the tier boundary with the flag. `--env` is a **per-invocation CLI argument, not project configuration**. Adding it to `ocx package exec` / `package env` / `package test` / `patch test` makes nothing read `ocx.toml`: those commands carry `EnvScope::Package`, which has no `no_patches` and no project entries by construction. The verification item below and `env-composition.md`'s "never reads any `ocx.toml`" both remain true and unedited.
>
> Two mechanical consequences the amendment forced:
>
> - `PatchScope` became `EnvScope`. The old type conflated "is a project in scope" with "does the caller contribute env" — `NoProjectContext::project_env()` returned an empty slice *by construction* — so an OCI-tier override could not ride it. `NoProjectContext` is now `Package { env }`; both arms stay struct variants, so C5's compile-forcing property survives and extends to overrides.
> - `ocx package exec` gained `set_forwarded_env`, which it never had. Its launcher re-entry is the same R1 failure the project tier closes, reached by the same path.
>
> **`ocx patch why` is deliberately excluded** — it applies no env and spawns nothing; it reports which companion contributed which key. An override row would have no companion to attribute.
>
> Separately, `--self` was **removed from `ocx run`** (see S6's amendment note below).

### Lock and hashing

**H1.** `[env]` is **excluded from `declaration_hash`**. Verified against source: `hash.rs:50-86` builds its canonical JSON from `config.tools` and `config.groups` **only**; `config.packages` is never read, pinned by the regression test `declaration_hash_unchanged_by_no_patches` (`config.rs:655-673`). `DECLARATION_HASH_VERSION` (currently `1`, `hash.rs:21`) does **not** bump.

> **This exclusion is a decision, not an accident, and the ADR records it as such.** Because `hash.rs` reads only two fields, a new field is excluded by *default* — a reviewer must positively choose to include it. Silently doing nothing produces the right outcome here but for the wrong reason. The right reason: `[env]` does not change *which packages resolve*, so it cannot make `ocx.lock` stale. An env edit must not force a re-lock. This is the same reasoning already recorded for `[package]` at `config.rs:81-83`. Add a regression test `declaration_hash_unchanged_by_env` mirroring the existing `no_patches` one, so a future refactor cannot wire it in by accident.

### Security

**X1.** Keys matching `OCX_*` or `__OCX_*` are **rejected** in project and group `[env]` and in `--env`, with `ExitCode::ConfigError` (78) for the file form and `UsageError` (64) for the flag form. Without this, a checked-in file can set `OCX_DEFAULT_REGISTRY`, `OCX_INDEX`, `OCX_MIRRORS`, `OCX_PATCHES`, `OCX_OFFLINE` or `OCX_ALLOW_YANKED` and reconfigure how `ocx` itself resolves — and `apply_ocx_config` (`env.rs:303-363`) would forward the result to every child. Rejection is at parse for the file form, so the error names the offending key and its location.

> Of seven tools surveyed, only GitHub Actions closes this. mise left it open and has two 2026 CVEs of exactly this causal shape. Closing it is the single most defensible deviation-from-the-field in this ADR.

**X2.** All keys validate through the existing shared `env::is_valid_env_key` (`env.rs:560-570`, POSIX `[A-Za-z_][A-Za-z0-9_]*`). Do not write a second validator — this one is already the single gate for both the shell emitters and both CI flavors, and its doc comment (`env.rs:544-559`) enumerates the injection classes it defends: shell `export` breakout, `$GITHUB_ENV` newline second-variable injection (CWE-77), GitLab JSON-lines key-field corruption.

**X3.** Trust boundary — **no gate in v1**, deliberately. `ocx run` is an explicit invocation, matching Cargo/Taskfile/Bazel, all of which run config-declared env unconditionally on the reasoning that invoking a build tool already extends trust. `ocx direnv export` fires on `cd`, which is the case that *would* need a gate — but direnv's own `direnv allow` already gates it one layer up, and duplicating that would be a second prompt for one action. Documented, not defaulted.

> The residual exposure is honest and bounded: a cloned repo's `ocx.toml` can put a repo-controlled directory at the front of `PATH` for `ocx run`. That same file already names the packages `ocx run` installs and executes, so the marginal capability gained is small — it is the Makefile/npm-script trust boundary, not a new one. X1 is what keeps it from escalating into control over `ocx`'s own resolution.

---

## Open Questions — Resolved

**Q1 — Serializer write-back form. Dissolved by S8.**

There is one accepted shape, so there is nothing for the serializer to choose. `toml::to_string_pretty` over the `Group` struct emits `[group.<name>.tools]` / `[group.<name>.env]` and cannot emit anything else.

Two mechanism facts from discovery remain worth carrying, since they govern the write paths this feature touches:

- There are **two** write paths, not one. `init_project` writes a fixed literal template via tempfile+rename (`mutate.rs:42-71, 445-479`) — that template must be updated to the nested shape as part of this change, or `ocx init` emits a file the parser rejects. `add_binding`/`remove_binding` rewrite **in place** through the lock-owning `LockedFile::replace_bytes` (`mutate.rs:248-283, 311-327`), deliberately *not* tempfile+rename, because rotating the inode would strand the flock on Windows (`mutate.rs:270-274`).
- Both are comment-lossy. That is pre-existing, and it becomes user-visible the day `[env]` ships, because people annotate env blocks (`# needed until upstream fixes X`) in a way nobody annotates a list of tool pins. Filed separately as a `toml_edit` migration; not a blocker here.

**Comment loss is now in scope enough to need a decision, and the decision is: separate issue, blocking on nothing here.** `ocx add` already discards hand-written comments today, and nobody has complained — because nobody hand-annotates a list of tool pins. People *will* annotate `[env]` blocks (`# needed until upstream fixes X`), so the existing behavior becomes visible the day this ships. Migrating to `toml_edit` is a self-contained change that should not gate this ADR, but it should be filed before this ships so it is a known trade rather than a support surprise.

**Q2 — `--global` tier semantics.** The global file's own `[env]` applies when the global tier is the one being resolved (`ocx --global run`, `ocx --global env`) and **never** composes into a project-tier resolution. This is not a new rule; it is strict isolation applied unchanged. The global env resolution path (`resolve_global_pinned_env`, `toolchain_env.rs:515-526`) already re-loads `$OCX_HOME/ocx.toml` into a fresh `ProjectConfig` and is tolerant of parse failure (degrading to an empty opt-out) — the same load site carries `[env]`, and the same tolerance applies.

**Q3 — `ocx env --ci=github` and `$GITHUB_ENV`.** **Yes, project `[env]` is emitted**, on uniformity grounds (C1 makes it automatic — the CI writers consume the same `&[Entry]`), with the security posture stated explicitly rather than inherited:

- X1's `OCX_*` rejection has already fired at parse, so nothing resolution-affecting can reach the sink.
- X2's `is_valid_env_key` gate fires per entry in `github_flavor::write_entry` (`github_flavor.rs:86-89`), warning and skipping rather than aborting.
- The `$GITHUB_PATH` newline rejection (`github_flavor.rs:96-99`) already covers CWE-77/CWE-426 for path-kind values.

The honest framing: a repository that can write `ocx.toml` can already write the workflow file that invokes `ocx`. This adds no capability an attacker with commit access lacks. It *would* matter for a `pull_request_target`-style workflow running a fork's `ocx.toml` — and that is a documentation obligation on the CI page, not a reason to break uniformity.

**Q4 — `required` on the path modifier.** **Deferred, recorded.** `Var`'s `Path` modifier has a `required: bool` that makes a missing directory an error (`package/metadata/env/path.rs`, enforced in `resolver.rs:50-102`). The project-tier analog is plausible (`node_modules/.bin` before `npm install` has run) but the failure mode is unclear — hard error on every `ocx run` before a first install would be hostile. Ship without it; the table form means adding `required = true` later is purely additive.

**Q5 — Naming.** `[env]`. Matches Cargo, mise, Taskfile, docker-compose, and GitHub Actions. `[environment]` matches nothing and is longer.

**Q6 — Ambient-vs-declared default direction (raised by research, not in the original brief).** Cargo's `force` defaults to **false**: a variable already present in the process environment beats the `[env]` declaration. Our C2 ordering is the opposite — the project file wins over ambient.

**Decision: project wins, no `force` flag.** Reasoning:

- Cargo's default exists because `.cargo/config.toml` is frequently a *machine-level* file (`~/.cargo/config.toml`) where "don't clobber what CI deliberately set" is right. `ocx.toml` is a *project* file, checked in alongside the code, and its whole purpose is to state what this project needs.
- The competing case — CI sets `CI=true`, project declares `CI=1`, who wins? — is real but resolves the same way: if the project declared it, the project meant it. A user who wants ambient to win has stage 6 (`--env`) and, failing that, simply does not declare the variable.
- A `force` flag is the "let the author decide" answer and is genuinely tempting. It is rejected for v1 on YAGNI grounds and because it is purely additive: `{ type = "constant", value = "1", force = false }` can be introduced later without changing the meaning of any file written against v1.

**Q7 — Fresh-clone trust boundary (raised by research).** Resolved at X3.

---

## Consequences

### Positive

- The three motivating needs are served with one table and one flag.
- Windows reaches parity with POSIX for the first time on per-invocation env — driver D-f, previously unmet by any mechanism.
- C1's entry-vector append means uniformity is structural, not maintained: a future env-composing surface gets project env for free.
- X1 closes a hazard six of seven surveyed tools left open.
- Zero existing `ocx.toml` files break.

### Negative / accepted costs

- **Breaking**: `ocx.toml` files declaring tool bindings directly under `[group.<name>]` fail to parse until hand-edited. The error message carries the fix; lock-neutral, so no re-lock follows.
- **The in-repo migration is the real cost, not the user-file migration.** S8's "handful of files" is accurate for *users* — a full sweep of `/home/mherwig/dev` found exactly **two** real projects affected (`find_ocx/examples/project/ocx.toml`, `www-setup/ocx.toml`), and confirmed all four ocx worktrees plus `ocx-mirror` are unaffected because their `[group]` table is bare and empty, which deserializes to an empty map under both schemas. But **19 files need edits** for the build and suite to stay green: 2 user `ocx.toml`, 2 website docs, 11 pytest fixture files, 4 Rust source files. A work package of its own, not a line item. The full inventory belongs in the plan artifact.
- **Three migration classes, not one**, and only the first is a mechanical find-replace:
  1. *Raw fixture rewrites* — literal `[group.X]` TOML in test bodies (`test_project_pull.py` ×7, `test_project_run.py` ×6, `test_lock.py` ×3, `test_pull_progress.py` ×2, `test_update.py:493`, `test_project_config.py:395`, `test_project_remove.py:182`, `test_project_hooks.py:133`, `test_toolchain_env.py:1062`, plus ~9 inline Rust tests in `project/config.rs`).
  2. *Assertion drift* — tests asserting on **CLI-generated** `ocx.toml` content (`test_project_groups.py:134`, `test_project_add.py:136,782`). These do not write TOML; they assert the serializer's output contains `[group.ci]`. They must flip to `[group.ci.tools]`, and they are the tests that prove the serializer change is observable.
  3. *Generated recordings* — `website/src/_scripts/user-guide/project-groups.sh` drives its cast via `ocx add -g ci` (CLI, not raw TOML), so it self-heals functionally, but the **recording it produces will differ** and needs regeneration through the one-tree convergence gate.

> **Additional Rust surface beyond `config.rs`.** The `Group` restructure is not confined to the schema type: `project/mutate.rs` reaches into the flat group map directly (`cfg.groups.get("ci").contains_key(...)` stops compiling once the value is a struct), and `project/resolve.rs:70,460` walks group contents during resolution. `hash.rs:70-75` needs a one-line edit (`group.tools.iter()`).
>
> ~~`project/lock.rs:153,167,1229` walks them during lock canonicalization.~~ **Wrong — struck.** An earlier revision of this ADR relayed that citation unverified. `project/lock.rs` contains **zero** references to `ProjectConfig.groups`: `:153` and `:167` are doc comments on `LockedTool`, `:1229` is a comment inside a test, and `LockedTool.group` is a plain `String` column on a flat `Vec`. There is exactly one canonicalization path (`hash.rs:50-86`, memoized via `OnceLock` at `config.rs:194-197`); `lock.rs` only stores and version-gates the resulting string. **H1 stands unchanged — no `DECLARATION_HASH_VERSION` bump.**
>
> Related: the frozen corpus case `hash_corpus_case_3` (`hash.rs:217-237`) **already** pins "a group reshape does not change the hash." The corresponding verification bullet below is therefore covered by an existing test; no new one is needed for that specific property.
- `ocx init`'s literal template must be updated in the same change or it emits a file its own parser rejects (Q1).
- Two new hand-rolled deserializers (group union, env value union). Mitigated by both following one existing in-repo pattern.
- Comment loss on `ocx add` becomes user-visible. Filed separately (Q1).
- A pre-existing gap is now documented and not fixed: no constant-collision detection in any non-CI path (C3). This ADR does not close it — it only stops mischaracterizing it.

### Risks

**R1 — Launcher re-entry drops project `[env]`. Real, concrete, needs a decision in the design spec.**

A generated entrypoint launcher invoked from `PATH` re-enters via `ocx launcher exec` with a synthetic content-addressed identifier and **no `ProjectConfig` by construction** (`command/launcher/exec.rs:86-95`). This is structurally identical to the hole `adr_patch_env_resolution_uniformity.md` found for `no-patches`, which was closed by forwarding the opt-out over `OCX_PATCHES` (including the resolved content digests, since the launcher has no repository path to match on).

Three options, to be settled in the design spec:

1. **Accept the asymmetry for v1.** Under `ocx run`, project env applies. Under a direnv-activated shell, `ocx direnv export` has already put the variables in the shell, so the launcher inherits them anyway. The gap is narrow: a launcher invoked from a shell where neither `ocx run` nor direnv activation happened. Cheapest; documented as a known limitation.
2. **Forward over a new `OCX_ENV` wire key**, mirroring `OCX_PATCHES`. Complete, and C1's `Entry` shape serializes cleanly. Costs a new forwarded env key and its precedence rules — and note that a forwarded env map is itself attack surface that X1/X2 must gate on the *decode* side too, not just at parse.
3. **Piggyback on `apply_ocx_config`** (`env.rs:303-363`) as an additional resolution-affecting field. Structurally similar to option 2 with less new machinery, but muddies that function's stated contract of carrying only resolution-affecting configuration.

~~Recommendation: option 1 for v1.~~ **Overturned — build option 2 in v1.** The design pass found the option-1 reasoning wrong in both directions:

- **The launcher is the primary path, not a corner case.** A package with entrypoints has its synthetic `entrypoints/` PATH entry pushed *last* precisely so it lands at the *front* and shadows `bin/` — stated outright in the source: *"synth-PATH last so entrypoints/ ends up at the front of PATH and shadows bin/"* (`composer.rs:625-633`, and the root form at `:665-672`). So under `ocx run`, a tool with entrypoints resolves **through the launcher**. That is the normal case for any package that declares entrypoints at all.
- **The failure is silent, not visible.** The launcher's `Env::new()` (`launcher/exec.rs:165`) inherits project env from the parent, then `apply_entries` re-applies the *package's own* entries **on top** — silently reverting exactly the overrides C4 names as the feature's declared intent. The user sees a tool running with the publisher's value and no signal that their override was discarded. The ADR's own criterion — "silent divergence is much worse and may change the recommendation" — is met.
- **The gap the ADR named as the cost is the case where absence is correct.** A launcher invoked from a plain shell with no activation *should not* see project env.
- **`--env` has the identical hole.** L1 is stage 6 of the same payload, so an explicit per-invocation override is discarded on the same path.

Option 1 therefore does not ship a documented asymmetry; it ships a feature that silently does not work on its primary path.

**Decode-side gating is mandatory and must fail closed on the whole payload.** `patches_from_env`'s `registry` fail-close (`config/patch.rs:344-356`) is the correct template; its `no_patches` leniency (`:375-379`) is **not** — that leniency is only safe because a forged value can merely *suppress* an overlay. Concrete seam: `apply_ocx_config` runs after `apply_entries` but only overwrites keys it knows (`env.rs:303-363`), so a forged `OCX_DEFAULT_REGISTRY` arriving via the payload would survive into the child and reach any grandchild `ocx`. X1 and X2 apply identically on decode.

Honest scoping of that risk: a process that can set `OCX_ENV` already controls the child's environment outright, so the gate grants no new capability against that attacker. It exists to keep the forwarded map out of **ocx's own resolution surface** — a narrower and achievable goal. Also needs an `OCX_ENV` **remove** branch in `apply_ocx_config` so a stale shell export cannot leak into an unrelated invocation. The field does *not* join `OcxConfigView` — that would be option 3, and it would muddy that type's stated contract of carrying only resolution-affecting configuration.

**Planning consequence: R1 and L1 are coupled and cannot be split across waves.** `--env` is stage 6 of the forwarded payload, so the encode side in `run.rs` and the decode/gate/apply side in `launcher/exec.rs` are one work package. Adds two verification items and an `OCX_ENV` entry in `reference/environment.md`.

**R1a — no version discriminator on the `OCX_ENV` envelope; strict `kind` parsing instead.**

`OCX_PATCHES` carries no version field (`config/patch.rs:341-379`). It evolves additively: one mandatory sentinel (`registry`, absent → hard error, which is what proves the payload came from `encode_patches` rather than injection) plus optional fields that default when absent (`system_required` → `false`, `no_patches` → empty). Adopt that envelope shape, with the same mandatory-sentinel role.

A version discriminator would not buy anything here, because the envelope is not where `OCX_ENV` can break across versions. The payload is a list of `Entry`-shaped records, and the one field that *cannot* evolve additively is `kind`: if a later ocx introduces a third modifier (an `append`, say), an older ocx decoding that payload must **not** silently treat the unknown kind as `constant` — that would apply a value with the wrong semantics and produce a wrong environment with no signal. An envelope version would only tell the old binary "this is newer than me", which it cannot act on any better than rejecting the unknown kind directly.

**Therefore: unknown `kind` is a hard decode error, not a defaulted field.** This is precisely where `OCX_PATCHES`' leniency must not be copied, and for the same reason its `no_patches` leniency *is* safe there: a forged or unparseable `no_patches` can only *suppress* an overlay, whereas a misread `kind` actively sets the wrong value.

Scoping note: the cross-version case is rarer than it looks. `ocx run` spawning a launcher that re-enters `ocx launcher exec` is normally the **same binary**; versions diverge only when the launcher on `PATH` resolves to a different ocx installation than the one that spawned it. Real, but not the common path — which is why fail-closed decode is sufficient and a version envelope is over-engineering.

**R2 — `PatchScope` payload widening touches eleven call sites.** All eleven already pass an explicit variant, so the change is mechanical and compiler-enforced. Low risk, non-zero churn.

**R3 — Pinning test for the deferred hermeticity constraint must land now, not with interpolation.** See below.

---

## Hermeticity Constraint (records a boundary for the deferred work)

Interpolation is out of scope (see below), but it constrains this ADR's ordering and must be recorded here, because C2 is where a future implementer would naturally get it wrong.

When `${env.VAR}` eventually lands, **package-env values must resolve against a package-only accumulator** — the entry set inside `package_manager::composer::compose` before it reaches `Env::apply_entries` — never `std::env`, never the process `Env` under construction.

C2's ordering alone does **not** achieve this. Stage 1 seeds the process environment from ambient (`Env::new()` at `command/run.rs:225`) *before* stage 2 applies package entries. A naive "resolve against what has been composed so far" would therefore read the host environment into published package metadata, making a package's behavior depend on state present in no lock, no digest and no config file. That is the reproducibility hole strict isolation exists to close, reopened one layer down.

**Pinning test, to be written as part of this ADR's implementation and not deferred with the feature:**

> Package-composed env is byte-identical with and without `--clean`, for the same lock and the same digests.

This assertion is meaningful today (it should already hold) and fails loudly the moment ambient leaks into package env resolution. Writing it now costs one test and permanently guards the boundary.

**Docs corollary:** `--clean` is **not** the hermeticity boundary. It controls only what the child process inherits. Hermeticity of package env comes from the resolver's scope. These will be conflated unless stated.

**The asymmetry is intended and is what the capability gate exists to express:**

| Site | May read |
|---|---|
| package env value | package accumulator only |
| entrypoint `args` (#175) | package accumulator only |
| project / group `[env]` | ambient + packages + earlier project stages |
| `ocx run --env` | nothing — literal by design |

Project `[env]` reading ambient is correct: it is the user's own file, already excluded from `declaration_hash` (H1), and is the deliberate escape hatch. Published package metadata is the opposite — an artifact many machines consume, so it must not absorb machine state.

---

## Out of Scope

**Interpolation, entirely.** v1 values are literal. Three surfaces turn out to be one problem, all landing on the same `Usage` → `AllowedTokens` capability gate in `package/metadata/template.rs`: package env values, entrypoint `args` (#175), and project/group `[env]`. They get one follow-up and one ADR. #73 (`${self.installPath}` alias) folds in there too.

S5's path modifier is what makes this deferral viable — PATH prepending, the one case that genuinely needs a dynamic value, is expressible without a template engine, and relative-path resolution against project root removes the `${projectRoot}` motivation.

**Also out of scope:** ~~`--env` on `ocx env`/`ocx exec` (L3);~~ *(shipped — see the L3 amendment)* `required` on path entries (Q4); `force` per-entry ambient control (Q6); `toml_edit` migration for comment preservation (Q1); a fresh-clone trust gate (X3); closing the non-CI constant-collision gap (C3/C4).

---

## Documentation Surfaces

Every surface below is touched by this change and must be updated in the same PR:

| Surface | Change |
|---|---|
| `website/src/docs/reference/configuration.md` | New `[env]` and `[group.<name>.env]` documentation; `[group.<name>.tools]` as the only tool-binding location; value grammar; `OCX_*` rejection. Describes the current schema only — no "former form" prose, per `feedback_no_migration_prose_in_docs` |
| `CHANGELOG.md` | The one place the breaking group-table change is recorded (S8) — changelog, not user docs |
| `crates/ocx_lib/src/project/mutate.rs` | `init_project`'s literal template (`mutate.rs:469-475`) must emit the nested shape (Q1) |
| Any `ocx.toml` in this repo and in `ocx-mirror` | Dogfood: convert before merge; this repo's own file has an empty `[group]` and is unaffected, but verify |
| `website/src/docs/reference/env-composition.md` | C2's six-stage ordering table; the `--clean`-is-not-hermeticity corollary; `--self` has no effect on project env (S6) |
| `website/src/docs/reference/command-line.md` | `ocx run --env` flag; exit codes 64 (bare `--env FOO`) and 78 (`OCX_*` key) |
| `website/src/docs/reference/environment.md` | Cross-reference that `OCX_*` keys cannot be set from `ocx.toml` |
| `website/src/docs/user-guide.md` | Use-case-first section: project constants, group-scoped env, project-local PATH entry |
| `website/src/docs/in-depth/project.md` | Composition-order worked example extended with project/group env stages |
| `website/src/docs/docker.md` | Whether `[env]` changes the `#193` bootstrap recommendation |
| **`project` JSON Schema** (`crates/ocx_schema/src/lib.rs:52`) | Derived from `ProjectConfig`, published at `https://ocx.sh/schemas/project/v1.json`. New top-level `env`; `Group { tools, env }` replaces the bare binding map. Needs `#[derive(schemars::JsonSchema)]` on `Group` and `ProjectEnv`, and a `#[schemars(schema_with = ...)]` on the env value field (string-or-table union — schemars cannot infer it, same treatment as `Var.visibility`). Regenerate and eyeball the output |
| `crates/ocx_schema/tests/schema_outputs.rs` | Snapshot/shape assertions for the `project` schema |
| `#:schema` directive in `init_project` (`mutate.rs:470`) | URL stays `project/v1.json` — see the note below; the pinned test `init_project_emits_schema_directive_on_first_line` (`mutate.rs:751-761`) keeps it anchored |
| `crates/ocx_cli` `--help` text | `--env` per `quality-cli-help.md` two-register rule |
| `.claude/rules/subsystem-package-manager.md`, `subsystem-cli.md` | Module-map rows for the new project-env type and composition stage |
| `.claude/artifacts/adr_patch_env_resolution_uniformity.md` | Drive-by: correct `Status: Proposed` → `Accepted` (C5) |

---

## Verification

- [ ] A tool binding directly under `[group.X]` is a parse error naming the group and pointing at `[group.X.tools]`; exits 78 (S8)
- [ ] `[group.X.tolos]` (typo) is rejected by `deny_unknown_fields`, naming the key (S2) — no hand-written branch involved
- [ ] A tool named `env` and a tool named `tools` both parse inside `[group.X.tools]` (S2)
- [ ] `[group.X]` with only `env` and no `tools` parses; and with neither, parses as an empty group
- [ ] `ocx init` emits a file its own parser accepts (Q1 — guards the template/parser split)
- [ ] `declaration_hash` is unchanged by a group's TOML reshape (`hash.rs:57-75`) — hand-edited files trigger no re-lock
- [ ] `declaration_hash_unchanged_by_env` — mirrors `declaration_hash_unchanged_by_no_patches` (H1)
- [ ] **Both env value forms validate green against the generated `project` schema** — `CI = "1"` and `CI = { type = "path", value = "bin" }` (S9a). Guards the editor-facing surface `taplo.toml:12-16` binds live; a derived schema fails the string arm, which is the common case
- [ ] `[group.X.tolos]` (typo) errors with the group name and offending key, not silently (S4 deviation)
- [ ] A tool named `env` parses in both flat and nested form
- [ ] `declaration_hash_unchanged_by_env` — mirrors `declaration_hash_unchanged_by_no_patches` (H1)
- [ ] `OCX_*` and `__OCX_*` keys rejected in `[env]`, `[group.X.env]`, and `--env` (X1)
- [ ] `--env FOO=a=b` yields `FOO` → `a=b`; bare `--env FOO` exits 64 (L1/L2)
- [ ] `--env KEY:path=…` prepends while `--env KEY=…` replaces; unknown/empty `TYPE` exits 64; `--env OCX_*:path=…` still rejected (L1 amendment)
- [ ] Relative `--env KEY:path=…` resolves against CWD, not the project root — assert from a subdirectory (the inverse of the file-form check below)
- [ ] Relative `type = "path"` resolves against project root, not CWD — assert from a subdirectory
- [ ] Path entry is idempotent across repeated `direnv export` re-evaluation (`move_to_front`)
- [ ] Project constant overrides a package constant of the same key (C2); CI export does not warn on it (C4)
- [ ] `ocx run`, `ocx env --shell`, `ocx env --ci=github`, `ocx direnv export` all emit project env identically (C1)
- [ ] `ocx exec` / `ocx package env` emit **no** project env (OCI tier reads no `ocx.toml`) — still true after the L3 amendment: they carry `--env` overrides only, and a test pins that an `ocx.toml` beside the invocation contributes nothing
- [ ] `ocx env --env X` exports exactly what `ocx run --env X` executes with; likewise `ocx package env --env X` vs `ocx package exec --env X` (L3 amendment — assert against one shared oracle, not two independent expectations)
- [ ] A `--env` override on `ocx package exec` survives a generated entrypoint launcher. **The override must target a key the package itself declares** — one it does not declare survives the hop by plain inheritance, so a test using such a key passes with forwarding disabled and proves nothing
- [ ] `ocx direnv export -g <group>` composes that group's `[env]`, and an unknown `-g` exits 64
- [ ] `--global` env applies to `ocx --global run` and never to project resolution (Q2)
- [ ] ~~`--self` does not change project env emission (S6)~~ — **vacuous since `--self` was removed from `ocx run`.** S6's substance stands (project env has no visibility axis); there is simply no flag left on the project tier to vary. The removal is its own decision: the self view *drops* a package's `entrypoints/` from PATH, because launchers exist for consumers and a package running itself calls `bin/` directly. A toolchain is a consumer of every tool it declares, so `ocx run --self` composed a strictly worse toolchain rather than a fuller one. The flag stays on `ocx package exec` / `package env` / `package test`, where a package's own surface is the thing being asked about
- [ ] **Package-composed env byte-identical with and without `--clean`** (hermeticity pin)
- [ ] Windows: separator handling and PowerShell/`cmd` emission for a path entry

---

## Handoff

- **Design spec** (`design_spec_project_env.md`) — required before implementation; must settle R1 (launcher forwarding, three options) and the exact `PatchScope` payload shape from C5.
- **Security Auditor** — X1/X2/X3 and Q3's `$GITHUB_ENV` reasoning warrant a review pass; new attack surface is small but the CI sink is a known-bad neighborhood.
- **QA Engineer** — the Verification list above, with the `--clean` parity test called out as the one that guards a constraint whose feature has not shipped yet.
- **Follow-up issues to file** (owner previews first): interpolation across the three surfaces (comment on #175); `toml_edit` comment preservation; dead `project::compose::parse_positional`; `PatchScope` ADR status correction.
