# OCX — Core

OCX = Rust package manager backed by OCI registries (Docker Hub/GHCR/private) for pre-built binary storage. Backend tool for other tools (CI, Bazel, Python scripts), not end users. Binary: `ocx`. Pre-1.0; metadata + OCI manifest compat is the only stability contract.

## Workspace layout (resolver v3, Rust 2024)

- `crates/ocx_lib/` — core library (most logic lives here).
- `crates/ocx_cli/` (`ocx`) — thin clap shell. Substance belongs in `ocx_lib`.
- `crates/ocx_mirror/` — mirror tool (separate binary `ocx-mirror`).
- `crates/ocx_schema/` — JSON schema generator (build-only).
- `crates/ocx_shim/` — Windows `.exe` launcher shim (committed blob, hermetic zig build).
- `external/rust-oci-client/`, `external/docker_credential/` — local submodules patched via `[patch.crates-io]`; excluded from workspace.

## Subsystems (`crates/ocx_lib/src/<dir>/`)

`oci/`, `file_structure/`, `package/`, `package_manager/`, `script/`, `config/`, `project/`, `publisher/`, `auth/`, `shell/`, `shims/`, `archive/`, `compression/`, `ci/`, `app/`, `cli/`, `command/`, `api/`, `options/`, `utility/`.

Each has a path-scoped rule in `.claude/rules/subsystem-*.md` that auto-loads on edits — read it before substantive work in that area. Catalog: `.claude/rules.md`.

## Project-wide invariants

- No `Co-Authored-By` trailers. Conventional Commits (`feat:`, `fix:`, `chore:` for AI/tooling).
- Work on a branch, never `main`. Never push (human-only trigger).
- Lib hosts orchestration, CLI = thin wrapper — composite tasks live in lib, route installs through `PackageManager`, generalize over `Identifier`.
- No subcommand `--format` divergence; format is context-only.
- Flags precede positional args in CLI.
- Pre-1.0: breaking changes just break. No migration prose in user docs; no compat shims.
- Planning artifacts in `.claude/artifacts/` (templates in `.claude/templates/artifacts/`). Internal — not a substitute for website/code-comment docs.

## Per-area entry points

- Toolchain & build pipeline → `mem:tech_stack`
- Day-to-day commands → `mem:suggested_commands`
- Code style, design principles, AI config conventions → `mem:conventions`
- Pre-commit / pre-merge gates → `mem:task_completion`

## Authoritative docs in-repo

- `CLAUDE.md` — project guide (env vars table, architecture pointer, principles).
- `.claude/rules.md` — rule catalog (by concern / by language / by subsystem / by auto-load path).
- `.claude/rules/product-context.md` — vision, positioning, "why not Homebrew/Nix/ORAS/mise".
- `.claude/rules/arch-principles.md` — design principles, glossary, ADR index.
- `website/src/docs/user-guide.md` — three-store architecture, versioning, locking, auth.
