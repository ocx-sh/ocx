# Toolchain introspection: `ocx status` + `ocx inspect`

## Context

[ocx-sh/ocx#259](https://github.com/ocx-sh/ocx/issues/259). A foreign tool cannot read project
toolchain state without side effects. Every structured view today is a byproduct of a command
that writes or installs:

| Want | Today | Cost |
|---|---|---|
| declared bindings + pins | `ocx lock --format json` | writes `ocx.lock`, pulls by default |
| composed env | `ocx --format json env` | installs by default |
| "is the lock current?" | `ocx lock --check` | pure read, **exit-code only**, no payload |
| binding → resolved surface | *nothing* | `which`/`deps` never read `ocx.toml` |

So integrators either mutate project state to read it, or re-parse `ocx.toml`/`ocx.lock`
themselves — reimplementing group semantics, host-leaf selection and env composition.

Two commands close it, split by **failure contract**, not by convenience:

- **`ocx status`** — what the files say. Offline, no flock, no staleness gate. Must succeed on a
  missing or drifted lock; that state *is* the payload.
- **`ocx inspect`** — what would happen. Resolved surface, same schema as `ocx package inspect`.
  Requires a current lock (78 / 65), because without a pin there is no stable answer.

Two of the four `ProjectConfig` surfaces (`env`, `packages`) are excluded from
`declaration_hash` by design, so nothing lock-derived can report them. That is why one command
cannot hold both contracts.

## Settled decisions

Do not reopen these during implementation.

1. **`status` takes no selectors.** `-g` changes the answer in `inspect` (composition scope); in
   `status` it would only hide rows from a flat map the consumer can filter itself.
2. **`default` is a group like any other.** Root `[tools]`/`[env]` land in `groups.default`.
   Verified: `project_env_entries` adds root `[env]` first, then `continue`s on `DEFAULT_GROUP`
   in the loop — because root `[env]` *is* that group's env.
3. **Objects in `status`, arrays in `inspect`.** A key cannot repeat in a TOML table; it can
   repeat in a composed env (`CI` from `[group.ci.env]` and from `--env`). An object cannot
   represent the second case.
4. **No `effective` / `conflicting` / `source` field.** Those are per-`ModifierKind` judgments
   that a third modifier type would break. Position is the provenance; the merged answer is
   `ocx env`, which materializes because package env values are `${installPath}`-templated.
5. **`--env` appears in the output.** Swap the verb, keep the picture — the process building
   argv is often not the process reading the JSON.
6. **Conflicts: payload always, exit 65.** `DataError` is what compose *already* returns for the
   identical condition — `DependencyError::Conflict` and `PackageErrorKind::EntrypointCollision`
   both classify to `DataError` (`package_manager/error.rs:310`, `:284`). So
   `ocx inspect --closure` exits exactly where `ocx run` would.
7. **Break `ocx package inspect`; do not let `ocx inspect` diverge.** One schema.

## Wire contracts

### `ocx status`

```json
{
  "project": "/home/mherwig/dev/ocx/ocx.toml",
  "lock": {
    "present": true, "current": false, "lock_version": 1,
    "declaration_hash": "sha256:67d0ab…",
    "config_hash":      "sha256:91ffcc…",
    "generated_by": "ocx 0.3.7", "generated_at": "2026-06-14T23:29:57Z"
  },
  "groups": {
    "default": {
      "tools": {
        "go-task": {"declared": "ocx.sh/go-task:3",
                    "platforms": {"linux/amd64": "sha256:fcfad8…", "darwin/arm64": "sha256:7ab019…"}},
        "newtool": {"declared": "ocx.sh/newtool:1"},
        "oldtool": {"platforms": {"linux/amd64": "sha256:11c0d6…"}}
      },
      "env": {"CI": {"type": "constant", "value": "1"}}
    },
    "ci": {"tools": {}, "env": {"PATH": {"type": "path", "value": "node_modules/.bin"}}}
  },
  "package_settings": {"ocx.sh/foo:1": {"no-patches": true}}
}
```

- Absence is the signal — no `platforms` = unlocked, no `declared` = orphaned. Matches the
  existing `ClosureNode.binaries` convention ("key absent on the wire means undeclared"). No
  invented state enum.
- No lock → `"lock": {"present": false}`, every tool carries only `declared`, **exit 0**.
- All platforms, **no host-leaf selection** — that is resolution, and belongs to `inspect`.
- `env` values are **verbatim**: `"node_modules/.bin"`, not the absolute form.
  `ProjectEnv::to_entries(project_root)` resolves relative `path` values; status must not call it.
- No `materialized` field — `ocx pull --dry-run --format json` already reports cached vs
  would-fetch.

### `ocx inspect` and `ocx package inspect` — one envelope

```json
{
  "platform": "linux/amd64",
  "packages": [
    {"name": "shellcheck",
     "identifier": "ocx.sh/shellcheck:0.11",
     "pinned_identifier": "ocx.sh/shellcheck@sha256:5238fe…",
     "pinned_digest": "sha256:5238fe…",
     "platform": "linux/amd64",
     "metadata": {…}, "layers": […],
     "resolution": {"chain": [{"digest": "…", "role": "manifest", "media_type": "…", "size": 1043},
                              {"digest": "…", "role": "config",   "media_type": "…", "size": 512}]},
     "closure": {"deps": […], "surface": {"interface": {…}, "private": {…}},
                 "conflicts": {"entrypoints": [], "repositories": []}}}
  ],
  "env": [
    {"key": "CI",   "type": "constant", "value": "1"},
    {"key": "PATH", "type": "path",     "value": "/home/mherwig/dev/ocx/node_modules/.bin"},
    {"key": "CI",   "type": "constant", "value": "0"}
  ]
}
```

- `name` — binding for `ocx inspect`, raw request string for `ocx package inspect`.
- `packages` in selection order (group order, lock order within a group) / input order.
- `env` in resolution order: `[env]` → each `-g` in flag order → `--env`. Empty array when
  nothing applies. `ocx package inspect` fills it with `--env` only.
- No `index` role in `chain` — the lock pins a leaf, so the walk never touches an index.
- Package env stays inside `closure.surface.env` (`{key, type, package}`, no value).

## What breaks

`ocx package inspect --format json` only. Plain output unchanged (already renders trees in
input order).

1. Top level `{"<raw-id>": {…}}` → `{platform, packages: [...], env: [...]}`. Today
   `PackageInspects` serializes a `Vec` through `serialize_map` and its doc claims the object is
   *"preserving input order"* — an array wearing an object costume, relying on parser behaviour
   JSON does not guarantee.
2. Each entry gains `name` (the former object key) and `pinned_identifier`.
3. Non-empty `closure.conflicts` exits 65, contradicting the current doc *"Inspect stays a view,
   not a gate — exit 0 either way."*

Per-package bodies are otherwise byte-identical — `--resolve` and `--closure` keep their exact
meaning and sub-shapes.

Additive: `--env` and top-level `platform` on `ocx package inspect`.

Note: `ocx status` / `ocx inspect` become real subcommands, so an `ocx-status` / `ocx-inspect`
plugin on `PATH` stops being reachable via `Command::External`. Worth one CHANGELOG clause.

## Phases

Contract-first: stub the wire types, verify they compile and serialize, specify tests, then
implement. P0/P1/P3 are file-disjoint and can run in parallel; P2 depends on P0 + P3.

### P0 — shared envelope (`ocx package inspect`)

- `crates/ocx_cli/src/api/data/package_inspect.rs` — replace `PackageInspects` with an
  `InspectReport { platform, packages: Vec<…>, env: Vec<EnvEntry> }` used by **both** commands.
  Hand-rolled `Serialize` goes away (plain derive over a `Vec`). Add `name` +
  `pinned_identifier` to the entry; `pinned` is already in every `Body` variant, so
  `pinned_identifier` is `pinned.to_string()`.
  Reuse `api::data::env::EnvEntry` for the env array — its `source` is patch provenance and is
  skipped when `None`, giving exactly `{key, type, value}`.
- `crates/ocx_cli/src/command/package_inspect.rs` — flatten `options::EnvOverride`; return
  `ExitCode::from(ocx_lib::cli::ExitCode::DataError)` when any entry's `closure.conflicts` is
  non-empty, after reporting.
- `test/tests/test_package_inspect.py`, `test_package_inspect_closure.py` — `[pkg.short]`
  indexing → lookup by `name` in `packages`. Add one small helper rather than repeating the
  lookup (~35 call sites across the two files).

### P1 — `ocx status` (independent)

- `crates/ocx_cli/src/api/data/status.rs` (new) — wire types. **Do not reuse `ProjectEnv`'s
  `Serialize`**: it emits the TOML shorthand (constant → bare string, path → `{type, value}`),
  a union type a JSON consumer would have to branch on. Normalize to `{type, value}` always.
- `crates/ocx_cli/src/command/status.rs` (new) — `ProjectConfig::from_path` +
  `ProjectLock::from_path` (returns `Option`, absence is not an error). Explicitly **not**
  `load_project_with_lock`, whose staleness gate exits 65. No flock, no network.
  `current` = stored `declaration_hash` vs `config.declaration_hash_cached()`.
- `crates/ocx_cli/src/command.rs` — one `Status` variant + one match arm.
- `test/tests/test_status.py` (new).

### P2 — `ocx inspect` (depends on P0 + P3)

- `crates/ocx_cli/src/command/inspect.rs` (new) — `load_project_with_lock` →
  `select_tool_set` → filter by NAME positionals → `resolve_selected_tools` →
  `manager.inspect_all(ids, platform, InspectOptions { resolve, closure })` → `InspectReport`.
  Flatten `GroupSelection`, `PlatformOption`, `EnvOverride`. Validate with the existing
  `ensure_group_segments_nonempty` / `ensure_groups_known` (unknown group or name → 64,
  matching `update`).
  Env array = `project_env_entries(&config, &config_path, &expanded)` ++ `EnvOverride::entries()`.
- `crates/ocx_cli/src/command.rs` — one `Inspect` variant + one match arm.
- `test/tests/test_inspect.py` (new).

### P3 — `select_tool_set` collision fix (independent bug, `run`/`env` today)

`crates/ocx_lib/src/project/compose.rs`: `DuplicateToolAcrossSelectedGroups` is raised inside the
group-expansion loop — before any NAME filter and before positional overrides are applied. So a
subset that excludes the colliding binding still errors, and an explicit `X=id` override that
would resolve the collision still errors. Move the check into one pass over the final selection.
The `select` / `resolve` seam was built for exactly this (its doc: *"a caller that filtered the
selection to a NAME subset never trips on an unrelated sibling"*) but only `NoHostLeaf` is
currently subset-safe. Unit tests live in the same file.

### P4 — docs + CHANGELOG

- `website/src/docs/reference/command-line.md` — new `{#status}` and `{#inspect}` sections with
  `**Usage**` / `**Options**` blocks (the shape `test_doc_command_reference.py` gates). Rewrite
  the `{#package-inspect}` section: four `jq '.["mytool:1.0.0"]…'` examples (lines ~2682–2694),
  the *"stays exit 0 either way"* sentence (~2676), plus the new `env` / `name` /
  `pinned_identifier` / `--env` / `platform` fields.
- `test/tests/test_doc_command_reference.py` — add both anchors to `NEW_COMMAND_ANCHORS`.
- `.claude/rules/subsystem-cli-commands.md` — add both to the toolchain-tier table (the rule
  is authority for the taxonomy; catalog rules require same-commit updates).
- `CHANGELOG.md` — `### Added` for both commands, `### Changed` for the `package inspect`
  envelope + exit-code break and the plugin-shadowing clause.
- `website/src/docs/in-depth/project.md` and `reference/env-composition.md` — cross-link from
  the project-tier and env-composition narratives.

No `crates/ocx_schema` change: it emits schemas for `ProjectConfig` / `ProjectLock`
(`ocx.toml` / `ocx.lock` editor validation), not CLI report types. No doc-script/cast work:
`test/doc_scripts/` has no inspect scenario.

## Verification

```sh
task verify --force                                   # never piped — pipeline exit code is tail's
cargo build --features ocx/__testing && cp target/debug/ocx test/bin/ocx
cd test && uv run pytest tests/test_status.py tests/test_inspect.py \
                        tests/test_package_inspect.py tests/test_package_inspect_closure.py \
                        tests/test_doc_command_reference.py -v
```

End-to-end against this repo:

```sh
ocx --format json status | jq '.lock.current, .groups.default.tools["go-task"].platforms'
ocx --format json inspect --closure -g default go-task | jq '.packages[0].pinned_identifier, .env'
ocx --format json package inspect --closure --env CI=1 ocx.sh/go-task:3 | jq '.packages[0].name, .env'
```

Discriminating checks — each must be shown red before green:

- **Missing lock**: `mv ocx.lock /tmp && ocx status` → exit 0, `"lock": {"present": false}`;
  `ocx inspect` → exit 78. Restore.
- **Drift**: append a binding to `ocx.toml` without re-locking → `status` exit 0 with
  `current: false` and the new tool carrying only `declared`; `ocx inspect` → exit 65.
- **Conflicts exit code**: construct two tools claiming one entrypoint name; assert `ocx inspect
  --closure` exits 65 **and** the payload still contains the populated `conflicts.entrypoints`.
  Revert the exit-code line and confirm the test goes red — a payload assertion alone passes
  whether or not the exit code was wired.
- **P3 subset**: two groups colliding on binding `X`; `ocx run -g a,b Y -- true` must succeed
  (today it errors), while `ocx run -g a,b X -- true` still errors.
