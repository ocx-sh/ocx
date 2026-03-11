# Plan: OCX Index Path Override (`--index` / `OCX_INDEX`)

## Context

The last commit documented the local index and its benefit for locking in CI environments (GitHub Actions, Bazel rules, devcontainer features). These tools bundle a frozen index snapshot alongside their release artifacts and need to point `ocx` at that snapshot — not the user's global `~/.ocx/index/`.

The `--index` CLI flag already exists in `context_options.rs` and is correctly wired in `context.rs`. What is missing:

1. An `OCX_INDEX` environment variable (CI tools set env vars, not CLI flags)
2. Documentation of `--index` in `reference/command-line.md`
3. Documentation of `OCX_INDEX` in `reference/environment.md`
4. User-guide prose explaining how tool authors use `--index` / `OCX_INDEX`

---

## Scope

**In scope:**
- Adding `OCX_INDEX` env var support (one-line change via clap `env` attribute)
- Documenting `--index` in `command-line.md`
- Documenting `OCX_INDEX` in `environment.md`
- Updating user-guide locking + indices sections

**Out of scope:**
- Changing the object store path
- Changing the install store path
- Any changes to `OCX_HOME` behavior

---

## Implementation

### 1. Code: Add `OCX_INDEX` Env Var

**File:** `crates/ocx_cli/src/app/context_options.rs`

Change the `--index` field from:
```rust
/// Alternative path to the local index directory (ignored if --remote is set)
#[arg(long, value_name = "PATH")]
pub index: Option<std::path::PathBuf>,
```

to:
```rust
/// Alternative path to the local index directory (ignored if --remote is set).
///
/// Can also be set via the OCX_INDEX environment variable.
#[arg(long, value_name = "PATH", env = "OCX_INDEX")]
pub index: Option<std::path::PathBuf>,
```

Clap's `env` attribute reads `OCX_INDEX` from the environment if `--index` is not provided.
CLI flag takes precedence over env var (clap's default behavior).

**No changes needed to `context.rs`** — the existing `options.index` path override logic is already correct.

---

## Documentation

### 2. `reference/command-line.md`: Add `--index` to General Options

Insert after `### --remote {#arg-remote}` and before `### --candidate / --current {#path-resolution}`:

```markdown
### `--index` {#arg-index}

Override the path to the [local index][fs-index] directory for this invocation.
By default, ocx reads the local index from `$OCX_HOME/index/` (typically `~/.ocx/index/`).

```shell
ocx --index /path/to/bundled/index install cmake:3.28
```

This flag is useful when the [local index][fs-index] is bundled alongside a tool rather than
living inside `OCX_HOME`. This is the typical setup for [GitHub Actions][github-actions-docs],
[Bazel rules][bazel-rules], and [devcontainer features][devcontainer-features] that ship a
frozen index snapshot as part of their release.

The flag has no effect when [`--remote`][arg-remote] is set — the remote registry is queried
directly and the local index is not consulted.

The same override can be configured persistently via the [`OCX_INDEX`][env-ocx-index]
environment variable. The `--index` flag takes precedence when both are set.
```

### 3. `reference/environment.md`: Add `OCX_INDEX`

Insert after `### OCX_HOME {#ocx-home}` and before `### OCX_INSECURE_REGISTRIES {#ocx-insecure-registries}`:

```markdown
### `OCX_INDEX` {#ocx-index}

Override the path to the [local index][fs-index] directory.
By default, OCX reads the local index from `$OCX_HOME/index/` (typically `~/.ocx/index/`).

```sh
export OCX_INDEX="/path/to/bundled/index"
```

This variable is intended for environments where the index snapshot is bundled alongside
a tool rather than stored in `OCX_HOME` — for example inside a [GitHub Action][github-actions-docs],
[Bazel rule][bazel-rules], or [devcontainer feature][devcontainer-features].

The command line option [`--index`][arg-index] takes precedence over this variable.
This variable has no effect when [`--remote`][arg-remote] or [`OCX_REMOTE`][env-ocx-remote]
is set.
```

Also add link definitions at the bottom of `environment.md`:
```markdown
[arg-index]: command-line.md#arg-index
```

### 4. `user-guide.md`: Locking Section (`#versioning-locking`)

The current locking section describes bundled index snapshots conceptually but never explains the mechanism — how does `ocx` know where to find the bundled index? Add a sentence after the tip box (after line 339) that bridges the concept to the mechanism:

```markdown
Tool authors configure `ocx` to read the bundled snapshot by setting the
[`OCX_INDEX`][env-ocx-index] environment variable — or the [`--index`][arg-index] flag — to
the path where the snapshot is stored within the action or rule's layout. Consumers do not need
to know this detail; it is transparent to the `ocx install cmake:3.28` call.
```

### 5. `user-guide.md`: Local Index Section (`#indices-local`)

After the existing description of `~/.ocx/index/` (around line 357-361), add a note about overriding the path:

```markdown
When the index is bundled inside a tool rather than living in `OCX_HOME`, point `ocx` at it
using [`--index`][arg-index] or [`OCX_INDEX`][env-ocx-index]. The object store and install
symlinks are unaffected — only tag and manifest resolution changes.
```

### 6. `user-guide.md`: Active Index Table (`#indices-selected`)

Update the active index mode table to add the `--index` / `OCX_INDEX` row:

| Mode | Flag / Env | Source | Network? |
|---|---|---|---|
| Default | *(none)* | `$OCX_HOME/index/` | No (unless fetching a new binary) |
| Custom path | [`--index`][arg-index] / [`OCX_INDEX`][env-ocx-index] | Provided path | No |
| Remote | [`--remote`][arg-remote] | OCI registry | Yes |
| Offline | [`--offline`][arg-offline] | Local snapshot | Never |

Add a note: "When `--remote` is active, `--index` and `OCX_INDEX` are ignored."

### 7. Link Definitions

Add to the link definition blocks in both `user-guide.md` and `environment.md`:

```markdown
<!-- commands -->
[arg-index]: ./reference/command-line.md#arg-index

<!-- environment -->
[env-ocx-index]: ./reference/environment.md#ocx-index
```

---

## Implementation Order

1. **Code** (`context_options.rs`) — trivial one-line change, add `env = "OCX_INDEX"` attribute
2. **`command-line.md`** — add `--index` section
3. **`environment.md`** — add `OCX_INDEX` section + link ref
4. **`user-guide.md`** — update locking section, local index section, active index table

All four can be done in a single commit after `task verify` passes.

---

## Verification

- `cargo check` — confirm clap compiles with `env` attribute
- Manual test: `OCX_INDEX=/tmp/test-index ocx install cmake:3.28` — confirm it reads from `/tmp/test-index` rather than `~/.ocx/index/`
- `task verify` — full quality gate

---

## Notes

- The `--index` flag ignores `OCX_HOME` — it replaces the entire index root path, not just a subdirectory suffix. This is intentional: bundled indexes live in arbitrary locations.
- When `--remote` is set, `--index` is silently ignored (existing behavior, consistent with the existing comment in `context_options.rs`). This should be documented explicitly.
- No migration or compatibility concern: `--index` is additive; existing users who do not set it see no behavior change.
