# Meta-Plan: /swarm-review on `feat/config-system`

## Classification

- **Tier**: max (confident — multiple max-tier signals fire)
- **Baseline**: `main` (default — no `--base` passed)
- **Target**: `HEAD` on branch `feat/config-system`

### Diff metrics

- 57 files changed
- +5279 / -160 lines
- 3 commits:
  - `5192781 feat(config): add async layered configuration system with registry resolution`
  - `9bd778f feat(cli): typed exit codes, error normalization, and config review polish`
  - `464ca5f fix(cli): static commands survive malformed ambient config`

### Subsystems touched (≥ 10)

| Subsystem | Paths |
|---|---|
| Config (new) | `crates/ocx_lib/src/config.rs`, `config/loader.rs`, `config/registry.rs`, `config/error.rs` |
| CLI lib classification | `crates/ocx_lib/src/cli.rs`, `cli/classify.rs`, `cli/exit_code.rs` |
| CLI shell | `crates/ocx_cli/src/app.rs`, `app/context.rs`, `app/context_options.rs`, `command/*` |
| OCI | `oci/client.rs`, `oci/pinned_identifier.rs`, `oci/*/error.rs` (client, digest, identifier, index, platform) |
| Storage / package | `file_structure/error.rs`, `package/error.rs`, `package_manager/error.rs` |
| Archive / compression / auth / ci / profile | `*/error.rs` — mass error-type normalization |
| Mirror | `crates/ocx_mirror/src/error.rs`, `main.rs` |
| Dependencies | `crates/ocx_lib/Cargo.toml` (+ `dirs = "6"`), `Cargo.lock` |
| License policy | `deny.toml` (+ MPL-2.0) |
| AI config / artifacts | `.claude/rules.md`, `.claude/rules/quality-rust*.md`, `.claude/artifacts/*.md` |
| Docs (website) | `website/.vitepress/config.mts`, `src/docs/getting-started.md`, `user-guide.md`, `reference/*.md` |
| Acceptance tests | `test/tests/test_config.py`, `test_offline.py` |

### Structural markers that fire

- Large diff: 57 files (>15) → **max** by size alone
- Cross-subsystem: ≥ 2 subsystems → **max**
- `crates/ocx_lib/src/oci/**` touched → `--breadth=full` minimum, `--codex` security floor
- `crates/ocx_lib/src/package_manager/**` touched → `--breadth=adversarial` at high+
- `Cargo.toml` dependency added (`dirs = "6"`) → `--breadth=full` supply-chain scrutiny
- `deny.toml` license allow-list widened (`MPL-2.0`) → `--breadth=full` + license review
- Mass `error.rs` rewrite across ~12 modules → potential public-API breakage on `ocx_lib`
- New config subsystem is a **one-way door** for ambient configuration semantics

No labels applied (not a PR target).

## Baseline resolution

- Source: default (neither `--base` passed nor PR target)
- Value: `main`
- Verified: `git rev-parse --verify main` → `9b26a6c…`

## Overlays (final resolved config)

| Axis | Value | Source |
|---|---|---|
| `breadth` | `adversarial` | tier=max default |
| `reviewer` | `opus` | tier=max + `--breadth=adversarial` |
| `doc-reviewer` | `sonnet` | diff touches `user-guide.md` → haiku trigger does NOT fire |
| `rca` | `on` (> Suggest) | tier=max default |
| `codex` | `on` (mandatory) | tier=max default |

## Workers per perspective (8-worker ceiling)

**Stage 1 — Correctness (parallel, 2 workers)**

| Perspective | Worker | Model | Focus |
|---|---|---|---|
| Spec-compliance | `worker-reviewer` | opus | `spec-compliance`, phase=`post-implementation` |
| Test coverage | `worker-reviewer` | opus | `quality`, lens=test-coverage |

**Stage 2 — Adversarial panel (parallel, 6 workers)**

| Perspective | Worker | Model | Focus / lens |
|---|---|---|---|
| Quality + CLI-UX | `worker-reviewer` | opus | `quality`, CLI-UX lens (new config flags, exit codes) |
| Security | `worker-reviewer` | opus | `security` (config loading, file permissions, OCI paths) |
| Performance | `worker-reviewer` | opus | `performance` (async loader, registry resolution) |
| Documentation | `worker-doc-reviewer` | sonnet | full trigger matrix (CLI / env vars / user-guide / reference) |
| Architecture | `worker-architect` | opus | SOLID, subsystem boundaries, dep direction, ADR compliance |
| SOTA / gap-check | `worker-researcher` | sonnet | how do Cargo / rustup / uv / Helm do layered config? |

Total: 8 parallel workers — at the ceiling.

**Phase 5 — Cross-model gate (mandatory at max)**

- `codex-adversary` with `scope=code-diff --base main` — one-shot.
- Triage: Actionable → reported; Deferred → surfaced; Stated-convention / Trivia → dropped with count.
- Skip path: log `Cross-model gate skipped: <reason>` in Summary.

**Phase 4 — RCA**

- Five Whys applied to every finding above Suggest (max-tier coverage).
- Cluster findings by systemic cause; note shared roots.

## Estimated cost

Rough per-worker tokens (conservative):

| Phase | Workers | Model | Notes |
|---|---|---|---|
| Stage 1 | 2 | opus | full diff read + spec/test-coverage audit |
| Stage 2 | 6 | 5×opus + 1×sonnet | broad adversarial panel; researcher uses WebFetch/Search |
| RCA | inline | — | orchestrator folds findings |
| Codex | 1 | external | code-diff scope, ~5k line diff |

Expect this to be the most expensive review the repo has received. The max-tier gate exists so you can downgrade if the spend is not warranted.

## Not Doing

- **No auto-fixes** — review is read-only. Findings will be reported, not patched.
- **No commits** — this skill only produces a report.
- **No full `task verify`** — diff-scoped review; no build/test invocation beyond what workers need to verify specific claims.
- **No PR creation / GitHub writes** — pure local review.

## Ready to proceed?

Accept to run the full max-tier panel. Edit to downgrade (e.g., `high` with `--no-codex` for cheaper fast sweep, or split into per-commit reviews). Cancel to abort.
