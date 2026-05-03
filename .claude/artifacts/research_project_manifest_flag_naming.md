---
title: Research — Naming the explicit project-file flag for `ocx`
date: 2026-04-19
status: research
scope: Supports Amendment G (pending) of `adr_project_toolchain_config.md`
---

# Research: Explicit project-file flag naming

## Question

What should the CLI flag and environment variable be called that lets a user point
`ocx` at a specific `ocx.toml` file (or project directory) instead of walking up
from `cwd`? The file is fixed as `ocx.toml` (like `pyproject.toml`, `Cargo.toml`) —
the flag selects *which* file / directory, not its name.

OCX's existing ambient-config loader (`ConfigLoader`) already owns `--config` / `-c`
and `OCX_CONFIG` for the tier-config TOML (`config.toml`). That namespace is
taken — the project-file flag needs a distinct name.

## Prior art survey

### File-pointing convention (Cargo family)

| Tool | Flag | Env var | Notes |
|---|---|---|---|
| Cargo | `--manifest-path <PATH>` | `CARGO_MANIFEST_PATH` (recent) | Points at `Cargo.toml` directly |
| cargo-nextest | `--manifest-path` | (inherits Cargo) | Same convention |
| cargo-dist | `--manifest-path` | (inherits Cargo) | Same convention |

Rationale: the manifest file name is fixed, so the flag value is the file path.

### Directory-pointing convention (Python / task-runner family)

| Tool | Flag | Env var | Notes |
|---|---|---|---|
| uv | `--project <DIR>` | `UV_PROJECT` | Directory containing `pyproject.toml` |
| PDM | `-p, --project <PATH>` | `PDM_PROJECT` | Project root directory |
| Poetry | `-C, --directory <PATH>` | — | Generic "chdir before running" |
| Hatch | `-p, --project <NAME>` | — | *Named* project (different semantics) |
| Rye | (uses `pyproject.toml` discovery; no explicit flag) | — | — |

Rationale: the project root is a meaningful identity; multiple files live there.

### Task runners

| Tool | Flag | Env var |
|---|---|---|
| Just | `--justfile <PATH>`, `-f` | `JUST_JUSTFILE` |
| Task | `--taskfile <PATH>`, `-t` | `TASK_X_TASKFILE` |
| Make | `-f, --file <PATH>` / `--makefile` | — |

Pattern: `--<filename>` for the manifest file.

### Polyglot tool managers (OCX's peer group)

| Tool | Flag | Env var | Notes |
|---|---|---|---|
| mise | `-C, --cd <DIR>` | `MISE_CWD` | Not a manifest flag — pre-chdir |
| asdf | (uses `.tool-versions` discovery only) | — | No explicit flag |
| devbox | `-c, --config <PATH>` | — | File-pointing |
| pkgx | (auto-discovery only) | — | — |
| Nix flakes | `--flake <PATH>` (flake ref) | — | File or URI |

Note: mise and asdf, the closest functional analogues, do *not* offer an explicit
manifest-path flag — they rely on discovery + `cd`.

### JS / TS

| Tool | Flag | Env var |
|---|---|---|
| pnpm | `-C, --dir <DIR>`, `-w, --workspace-root` | — |
| Bun | `--cwd <DIR>` | — |
| Yarn | `--cwd <DIR>` | — |
| npm | `--prefix <DIR>` | `npm_config_prefix` |

Pattern: directory, not file.

### Infra / DevOps

| Tool | Flag | Env var |
|---|---|---|
| Docker Compose | `-f, --file <FILE>` | `COMPOSE_FILE` |
| kubectl | `-f, --filename <FILE>` | — |
| Terraform | `-chdir=<DIR>` | — |
| Deno | `-c, --config <FILE>` | — |

## Analysis against OCX's constraints

| Criterion | File-pointing (`--project-file`) | Directory-pointing (`--project`) |
|---|---|---|
| Matches existing `--config FILE` / `OCX_CONFIG` pattern | ✓ direct mirror | ✗ different shape |
| Matches closest peer (mise/asdf) | ≈ (neither has one) | ≈ (neither has one) |
| Matches closest Rust peer (Cargo) | ✓ (`--manifest-path` equivalent) | ✗ |
| Matches closest Python peer (uv, PDM) | ✗ | ✓ |
| Disambiguates from `--config` / `-c` | ✓ | ✓ |
| Allows pointing at a `.toml` outside a dir (rare) | ✓ | ✗ (requires rename) |
| Error surface if user passes the wrong shape | "expected file, got dir" | "expected dir, got file" |
| Future: multi-project workspace walking | neutral | slightly nicer (dir is the identity) |

## Recommendation

**`--project <FILE>` with `OCX_PROJECT` env var and `OCX_NO_PROJECT` kill switch.**

The true one-to-one mirror of the existing `ConfigLoader`:

| | Tier config (existing) | Project config (this decision) |
|---|---|---|
| CLI flag | `--config <FILE>` | `--project <FILE>` |
| Env var | `OCX_CONFIG` | `OCX_PROJECT` |
| Kill switch | `OCX_NO_CONFIG` | `OCX_NO_PROJECT` |
| Empty-string escape | `OCX_CONFIG=""` → unset | `OCX_PROJECT=""` → unset |
| Kill switch prunes explicit CLI flag | no (CLI beats kill switch) | no (same rule) |

Reasons ranked by weight:

1. **Exact structural mirror of the existing tier loader.** `ConfigLoader`
   already established the flag / env-var / kill-switch / empty-string shape
   in OCX. Re-using that shape verbatim means one mental model, not two.
   Note: the CLI flag drops the `-file` suffix to match `--config` (not
   `--config-file`); the `-file` lives only on the env var.
2. **Matches the closest Rust peer semantically.** `cargo --manifest-path`
   takes a file path for the same reason: the filename is fixed, so the
   file is the honest unit.
3. **No short form.** `-c` is taken (tier config); `-p` is used across OCX
   for `--platform`. Avoid both.
4. **Value type is file path, not directory.** Diverges from uv/PDM's
   `--project <DIR>` but is internally consistent with `--config <FILE>`,
   which is the pattern OCX users encounter first. Flagged as a known
   wrinkle for users coming from Python tooling.

## Resolved design decisions (for Amendment G)

| # | Question | Decision | Rationale |
|---|---|---|---|
| 1 | Kill switch | `OCX_NO_PROJECT=1` exists | Parity with `OCX_NO_CONFIG` |
| 2 | Empty-string escape | `OCX_PROJECT=""` → unset | Parity with `OCX_CONFIG` |
| 3 | Precedence | `--project` > `OCX_PROJECT` > CWD walk | Matches `--config` / `OCX_CONFIG` / tier discovery |
| 4 | Does kill switch prune explicit CLI flag? | No — CLI flag beats kill switch | Matches `OCX_NO_CONFIG=1` + `--config <FILE>` behavior in `loader.rs` |
| 5 | Ceiling path applies to explicit paths? | No — `OCX_CEILING_PATH` only bounds CWD walk | Explicit paths are user intent; ceiling is for auto-discovery safety |
| 6 | Symlink policy (explicit paths) | Follow symlinks | Matches `--config` "trusted caller intent" policy |
| 7 | Symlink policy (CWD walk) | Reject symlinks | Matches tier-config discovery policy |
| 8 | Basename enforcement | Any filename accepted via `--project` / env | Matches Cargo `--manifest-path`; fixtures need this |
| 9 | Missing file exit code | `NotFound` (79) | Matches `--config` missing-file behavior |
| 10 | Interaction with `-g <group>` | No change — composition is unaffected | Amendment A governs composition; this amendment only chooses which `ocx.toml` is read |

## Test strategy

**Unit tests** — in the project-tier loader module (mirror the existing
`config/loader.rs` test matrix):

- `--project <valid>` → loads the file
- `--project <missing>` → `NotFound` (79)
- `OCX_PROJECT=<valid>` → loads the file
- `OCX_PROJECT=""` → treated as unset (falls through to CWD walk)
- `OCX_NO_PROJECT=1` → skips CWD walk + env-var path
- `OCX_NO_PROJECT=1` + `--project <valid>` → still loads (CLI beats kill switch)
- `OCX_NO_PROJECT=1` + `OCX_PROJECT=<valid>` → env-var path still loads
  (mirrors `OCX_NO_CONFIG=1` + `OCX_CONFIG` behavior)
- Precedence: `--project` beats `OCX_PROJECT` beats CWD walk
- Explicit path escapes `OCX_CEILING_PATH` (ceiling set above target; loads anyway)
- Symlink via `--project` → accepted
- Symlink discovered via CWD walk → rejected
- Non-`ocx.toml` basename via `--project` → accepted

**Acceptance test** — in `test/tests/test_project_config.py`:

- End-to-end: `ocx exec -g default --project fixture/ocx.toml -- cmd` succeeds
  without relying on CWD walk
- Subprocess env override: `OCX_PROJECT=fixture/ocx.toml ocx exec -g default -- cmd`

## Sources

- Cargo reference: https://doc.rust-lang.org/cargo/commands/cargo.html
- uv CLI reference: https://docs.astral.sh/uv/reference/cli/
- PDM CLI reference: https://pdm-project.org/en/latest/reference/cli/
- mise CLI: https://mise.jdx.dev/cli/
- Docker Compose CLI: https://docs.docker.com/reference/cli/docker/compose/
- Just manual: https://just.systems/man/en/
- Task docs: https://taskfile.dev/reference/cli/
- pnpm CLI: https://pnpm.io/cli/install
