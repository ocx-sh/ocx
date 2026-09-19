# Research: ocx layout and gate archaeology

## Metadata

**Date:** 2026-09-06
**Domain:** cli
**Triggered by:** /hex-discuss crate-split-and-verify-tiers
**Expires:** 2027-03-06

### Direct answer

`ocx_schema` and `ocx_shim` were carved out for narrow, load-bearing reasons
(build-only codegen; Windows-only pure/Win32 split for host-testability), not
as a general "split the workspace" precedent. The one *repo-level* split
(`ocx-mirror`, 2026-06-12) is documented as a repeatable template
(`adr_cli_repo_split_template.md`) and is the closest analogue to a
`ocx_lib`/`ocx_core` boundary question — its consumer (`ocx-mirror`) vendors
`ocx_lib` as a path dependency and imports across nearly every top-level
module, and ocx's own `arch-principles.md` records that reach as "known
drift" with a stated CLI-migration target, not as an endorsed pattern. The
verify gate is a two-phase, sequenced pipeline (`330d03d9`, 2026-04-12) with
a 5-minute commit-verified TTL and `verify:mark` bypass for rebase mechanics
(`3a4ccc53`) — no scoped/fast-gate attempt was ever reverted; the only
recorded full-gate cost figure is ~25 min wall (5959 unit + 2664 acceptance).
`oci` and `package_manager` are the two most frequently co-changed top-level
modules in `ocx_lib/src` over the last six months.

### Key findings

1. **`ocx_schema` (`af27a250`, 2026-03-12)** — added to generate JSON Schema
   from `ocx_lib` metadata types via `schemars`, behind an optional
   `jsonschema` feature. Motivation stated in the commit body: schema
   generation is build-tooling, not runtime logic, and needed its own crate
   for the codegen boundary. `crates/ocx_schema/Cargo.toml`.
2. **`ocx_shim` (`45f2d0fa`, 2026-05-18)** — new crate for the Windows native
   `.exe` launcher shim (closes a BatBadBut/CVE-2024-24576 class). Commit
   body states the split reason explicitly: "pure/Win32 split so wire-ABI,
   sidecar parse, stem derivation, quoting are host-tested on Linux CI" —
   i.e., the carve-out exists to let Windows-only logic be unit-tested on a
   non-Windows CI host, not for reuse or layering reasons.
3. **Follow-on friction, `ocx_schema`** — `0b072dee` (2026-07-27,
   `ci(test): stop the acceptance job compiling ocx_schema`, HEAD-adjacent;
   two earlier reverts/retries of the same fix exist in history at
   `27394061`/`2880577a`/`df107823`). Root cause per commit body:
   `test_schema_generation.py` built `target/release/ocx_schema` from its own
   pytest fixture with no timeout whenever the binary was missing; acceptance
   CI jobs carry no Rust cache, so a missing artifact silently became a cold
   full `ocx_lib` compile inside a job "that has no business compiling
   anything," racing every xdist worker on the same `target/` lock. Failure
   mode: the run "stalls just short of 100% with no failing test to point
   at." Fix: CI now uploads a prebuilt `ocx_schema` binary instead of letting
   the test fixture build it; the fixture still allows a local convenience
   build (bounded, and refused outright under `CI=true`).
4. **`ocx-mirror` repo split (`1476192c`, 2026-06-12)** — `crates/ocx_mirror`
   extracted via `git filter-repo` into `ocx-sh/ocx-mirror`. Commit body:
   "workspace member removed, five mirror-only workspace deps pruned
   (octocrab, quick-junit, url, reqwest, rustls)... pattern recorded in
   `adr_cli_repo_split_template.md` for future satellite CLIs (ocx-mcp,
   ocx-dist)." 24,469 lines net removed from this repo in that commit.
5. **`adr_cli_repo_split_template.md`** (`.claude/artifacts/`, accepted
   2026-06-12) — records the *repeatable contract*, not a one-off. Decision
   drivers: independent release cadence, "mirror development against
   unreleased `ocx_lib` without publishing crates," keep ocx CI lean,
   repeatability for `ocx-mcp`/`ocx-dist`. Explicit line: "`ocx_lib` is not
   on crates.io and should not need to be." Chosen option (of 3): satellite
   vendors ocx as a git submodule, `ocx_lib` consumed as a **path
   dependency** (`ocx_lib = { path = "external/ocx/crates/ocx_lib" }`) — the
   same shape ocx itself uses for its own vendored forks, reversed. Rejected
   option 3 was publishing `ocx_lib` to crates.io outright, on the grounds
   that pre-1.0 API churn would force release ceremony and "publishes an API
   surface ocx does not want to stabilize yet."
6. **`ocx_lib` is not a published library" is load-bearing project doctrine**,
   stated verbatim in `CLAUDE.md` ("Stability tiers": "`ocx_lib` is not a
   published library; the binary is the only consumer") and echoed
   independently in the ADR above and in `ocx-mirror`'s own `CLAUDE.md`
   comment ("`ocx_lib` is consumed as a path dependency into the ocx git
   submodule ... NOT a published crate").
7. **ocx's own `arch-principles.md` records the consumer relationship as
   drift, not architecture**: `.claude/rules/arch-principles.md:28` — "Known
   drift: `ocx-mirror` reaches into operational internals — migration target
   is CLI for operations (pending refactor)." Line 17 of the same table:
   "(mirror tool) | Moved to own repo ... vendors ocx as submodule, `ocx_lib`
   path dep | —".
8. **The consumer side (`ocx-mirror`) independently corroborates and argues
   against widening `ocx_lib`'s surface.** Its own `adr_mirror_signing.md`
   (2026-09, in `/home/mherwig/dev/ocx-mirror/.claude/artifacts/`) states: "Do
   not widen ocx. ocx's `arch-principles.md` records ocx-mirror's existing
   reach into operational internals as *known drift* with a CLI migration
   target. Every seam this ADR asks for must justify not being a CLI call."
   It then adds exactly one new `pub` function
   (`ocx_lib::oci::client::native_transport`) as a *named, scoped exception*
   to what it calls ocx's "operations go through the CLI" doctrine, arguing
   in its options table that the alternative (making `NativeTransport`
   public) would be worse because it "publishes a concrete type ... every
   future field on it becomes a semver concern."

### Carve-out precedents

| Crate | Commit | Date | Stated reason | Friction since |
|---|---|---|---|---|
| `ocx_schema` | `af27a250` | 2026-03-12 | Build-only JSON Schema codegen via `schemars`, optional feature flag; keep codegen out of the runtime lib | Acceptance CI accidentally compiled it from `ocx_lib` on every run until `0b072dee` (2026-07-27) supplied a prebuilt binary instead — root cause was a no-timeout pytest fixture, not the crate boundary itself |
| `ocx_shim` | `45f2d0fa` | 2026-05-18 | Windows-only `.exe` launcher; pure/Win32 split lets wire-ABI and quoting logic be **unit-tested on Linux CI** | Excluded from `cargo-dist` plan same week (`20d5ecb4`, 2026-05-27) — a distribution-profile carve-out, separate concern from the crate split itself |
| `ocx_mirror` → `ocx-sh/ocx-mirror` | `1476192c` | 2026-06-12 | Independent product, own release cadence, no reverse dep from ocx; codified as `adr_cli_repo_split_template.md` for `ocx-mcp`/`ocx-dist` reuse | Consumer keeps hitting the boundary ever since (see Sibling consumer below) — every new mirror capability that needs an `ocx_lib` internal is a discrete ADR decision, not a mechanical bump |

None of the three carve-outs were motivated by "the workspace/`ocx_lib` is
too big" in the sense of a layering complaint — each was driven by a
concrete external constraint (build-only tooling, cross-compilation
testability, independent release cadence). No commit or ADR anywhere in this
repo's history proposes splitting `ocx_lib` itself into smaller in-workspace
crates (e.g. an `ocx_core`/vocabulary crate) — that phrase does not appear in
any commit message, ADR, or rule file found in this search.

### Sibling consumer (ocx-mirror)

`/home/mherwig/dev/ocx-mirror/Cargo.toml`: explicit `[workspace] exclude =
["external/ocx"]` (documented reason inline: without it, the outer workspace
claims the vendored subtree and `ocx`'s own `.workspace = true`
inheritance fails to resolve). `ocx_lib` is a path dep into
`external/ocx/crates/ocx_lib` (git submodule); a second in-repo crate,
`ocx_python` (PEP 751 lock parsing), is a normal workspace member. Dependency
feature lists for anything shared with `ocx_lib` (tokio, clap, serde_json
`preserve_order`, reqwest major) are copied verbatim from ocx's
`[workspace.dependencies]`, with an inline comment warning that
`preserve_order` in particular is inherited only via Cargo feature
unification today and must be pinned explicitly so it "survives an upstream
change."

`ocx_lib::` imports across 804 non-vendored `.rs` files, grouped by top-level
module (occurrence counts, deduplicated by leaf symbol):

| Module | Approx. import count | Representative symbols |
|---|---|---|
| `oci` | ~140 | `Platform`, `Identifier`, `Client`, `Manifest`, `Digest`, `Algorithm`, `Descriptor`, `index::`, `native::*`, `ssrf::host_is_trusted`, `client::OciTransport`/`native_transport` |
| `cli` | ~90 | `ExitCode`, `DataInterface`, `Printer::new`, `progress::ProgressManager` |
| `package` | ~70 | `version::Version`, `metadata::*` (dependency, authoring, binary, entrypoint, integrations, template, bundle, libc_lint), `info::Info`, `tag::*` |
| top-level | ~30 | `Version`, `Result`, `Error::OciClient`, `resolve_mirror_map`, `log` (~22) |
| `publisher` | ~10 | `Publisher` |
| `env` | ~13 | `var`, `keys::CREDENTIAL_KEYS`, `insecure_registries` |
| `auth` | ~6 | `Auth::new`, `get_env_auth` |
| `file_structure` | ~6 | `IndexStore` |
| `utility` | ~7 | `string_ext::StringExt`, `tls::seed_embedded_roots`, `fs::persist_temp_file`/`path_exists_lossy` |
| `archive` | 2 | `Archive::extract` |

The `oci` module dominates, consistent with ocx-mirror's job (pushing
mirrored releases into OCI registries), followed by `cli` — notably, the
mirror consumes `ocx_lib::cli` types (`ExitCode`, `DataInterface`, `Printer`)
directly rather than re-implementing output formatting, which is a real
cross-cutting reuse case inside a crate CLAUDE.md elsewhere calls "thin."

Stated doctrine, quoted directly:

- `ocx-mirror/CLAUDE.md`: "Principle: every ocx-mirror capability is
  reachable as **one command** an operator can paste into any CI system;
  rendered pipelines are a GitHub-only convenience, never the only path."
- `ocx-mirror/.claude/rules/workflow-git.md:77`: "`external/ocx` is a
  **vendored read-only submodule** — never commit inside it. Bumping
  `ocx_lib` = bumping the submodule pointer ... the pointer change commits in
  this repo as `build:`."
- `ocx-mirror/.claude/rules/workflow-swarm.md:21`: "ocx-mirror has shared
  pipeline helpers (`src/pipeline.rs`), spec config types (`src/spec/`), and
  `ocx_lib` as a path dep ... Check with Grep before inventing."
- `ocx-mirror/.claude/artifacts/adr_mirror_signing.md:20`: "One deviation
  justified below: D5 widens one `ocx_lib` symbol, against ocx's `operations
  go through the CLI` doctrine. Named, scoped, and argued in D5."
- Same ADR, Decision Drivers: "Do not widen ocx. ocx's `arch-principles.md`
  records ocx-mirror's existing reach into operational internals as *known
  drift* with a CLI migration target. Every seam this ADR asks for must
  justify not being a CLI call."

No `.agents/memory/hex.md` file exists in `/home/mherwig/dev/ocx-mirror` —
that repo has no cross-session hex memory store to check.

### Gate history

- **`b5d995f5`** (2026-03-22, `chore: harden hooks with commit gates,
  conventional format validation, and generated file protection`) — earliest
  commit-gate hardening found; subject only quoted (body empty in this
  search), but it precedes and sets up the two-phase model below.
- **`330d03d9`** (2026-04-12, `chore(taskfiles): subsystem-scoped verify,
  tree layout, caching, and tool templates`) — introduces the **two-phase
  verify** structure still in place: "Phase 1 (parallel): format, clippy,
  shell lint, claude verify. Phase 2 (sequential): license, build, unit
  tests, acceptance tests." Also introduces subsystem-scoped verify ("AI-aware
  verification: subsystem verify during dev, full verify at end") and Quality
  Gate sections in every `subsystem-*.md` rule. Current `taskfile.yml`
  (`verify:` task) still matches this shape exactly: `.verify:lint` →
  `.verify:build-test` → `.verify:mark`.
- **`4b2b5a02`** (2026-04-12, same day, `chore(claude): two-phase commit
  workflow + rule catalog refactor`) — introduces the **two-phase commit
  model**: `/commit` (working phase, rolling "Checkpoint" amends) vs.
  `/finalize` (rebasing phase, strict Conventional Commits). This is the
  commit-workflow two-phase split, distinct from but coincident with the
  verify-task two-phase split above (both landed the same day).
  Also introduces `.claude/rules.md` as the read-on-demand rule catalog.
- **`3a4ccc53`** (2026-05-27, `chore(tasks): public verify:mark for rebase
  workflows`) — adds the public `task verify:mark` alias so a rebase that
  only touches cherry-pick/merge-context (no semantic diff) can re-arm the
  commit-verified gate "when the subsequent commits only carry ...
  changes with no semantic diff. Avoids re-running the full verify pipeline
  per commit during a multi-step rebase."
- **`ff7c4abc`** (2026-05-27, `fix(ci): build smoke artifact with __testing
  feature; parallelize acceptance tests`) — introduces `task test:parallel`
  (pytest-xdist `-n auto`), cutting the acceptance run from "~400s -> ~150s
  on 4-core runners" per the commit body. Same commit renames the test-seam
  feature to the `__testing` convention.
- **`pre_commit_verification.py`** (current, `.claude/hooks/`) — the
  `commit-verified` gate: "Blocks the commit unless `task verify` was run
  recently (**5-minute TTL**)." Fires only when the Bash command contains a
  `git commit` invocation *and* the resolved cwd's git root matches this
  project's root (so a commit in an unrelated sibling repo, e.g. a mirror
  candidate checkout, does not trip it). Implementation detail: the mark file
  is `.claude/hooks/.state/commit-verified`, written by `.verify:mark`
  (`date +%s > ...`) as the final step of `task verify`.
- **`subsystem-tests.md` Quality Gate** (current text, line 225): "During
  review-fix loops, run `task test:parallel` — not full `task verify`. Direct
  `uv run pytest` never builds: it runs the existing `test/bin/ocx` (stale
  after Rust changes — refresh via `task test` / `task test:parallel`)."
- **`workflow-git.md` Quality Gate** (current text, lines 67-74): "Every
  commit on branch must pass `task verify` before landing on `main`.
  `pre_commit_verification.py` hook enforces on tip commit," with the escape
  hatch spelled out as running `task verify` then
  `echo $(date +%s) > .claude/hooks/.state/commit-verified` — i.e., the same
  mechanism `verify:mark` wraps.

### Negative — no reverted or abandoned fast-gate attempt found

Searched `git log --all -i --grep='revert|scope|subset|fast gate|smoke'`
across full history: the only true `Revert` commits found
(`0041fa3d`/`fd7380a4`, 2026-06-02) revert an unrelated mirror archive-asset
feature, not a verify/gate mechanism. No commit message anywhere proposes,
then walks back, a scoped/subset test tier or a faster substitute gate. The
subsystem-scoped verify introduced in `330d03d9` and the rebase-only
`verify:mark` bypass in `3a4ccc53` both appear to have stuck without
reversal — they are still the current mechanism (`taskfile.yml:verify`,
`pre_commit_verification.py`, `subsystem-tests.md:225`). This is a genuine
negative: if a faster/scoped gate was tried and abandoned, it left no trace
in commit messages, ADRs, or the two gate-policy rule files checked.

### leads:

- **Module co-change data** (below) suggests `oci` and `package_manager` are
  the most tightly coupled top-level modules by commit — worth checking
  whether a hypothetical `ocx_lib` split boundary would cut through that pair,
  which would make it an expensive line to draw. A dedicated lane should
  compute the same co-change table restricted to *only* the files a proposed
  split boundary would separate, not all of `ocx_lib/src`.
- **`ocx-mirror`'s `native_transport` exception (D5) is the single closest
  precedent for "what happens when a satellite needs a new `ocx_lib` public
  symbol post-split"** — worth a follow-up read of the full D5a option table
  in `adr_mirror_signing.md` (lines ~800-900) if the discussion needs to
  reason about how a widened `ocx_lib` API surface gets governed once
  `ocx-mirror` (or a hypothetical `ocx_core` consumer) is on the other side of
  a hard crate boundary.
- **No hex.md exists in ocx-mirror** — if the crate-split discussion wants
  cross-session cost evidence from the satellite's own execution history, it
  is not centrally recorded there; would need to search that repo's
  `.claude/state/plans/` and `.claude/artifacts/` directly.

### Module churn (top-level, `crates/ocx_lib/src`, last 6 months, by commit-touch count)

| Module | Commits touching |
|---|---|
| `oci` | 597 |
| `package_manager` | 383 |
| `package` | 233 |
| `project` | 117 |
| `cli` | 91 |
| `file_structure` | 79 |
| `utility` | 72 |
| `config` | 64 |
| `env.rs` | 34 |
| `package_manager.rs` | 33 |
| `setup` | 32 |
| `oci.rs` | 28 |
| `lib.rs` | 28 |
| `error.rs` | 27 |
| `archive` | 27 |
| `announce` | 27 |
| `shell.rs` | 24 |
| `forge` | 22 |
| `publisher.rs` | 21 |
| `ci` | 21 |

### Churn and co-change

Top 10 most frequent co-changed top-level-module pairs, 341 commits touching
`crates/ocx_lib/src` in the last 6 months, counted per-commit (a commit
touching 3 modules counts once for each of the 3 pairs):

| Pair | Co-occurrences |
|---|---|
| `oci` + `package_manager` | 49 |
| `oci` + `package` | 47 |
| `package` + `package_manager` | 40 |
| `package_manager` + `package_manager.rs` | 33 |
| `file_structure` + `package_manager` | 30 |
| `cli` + `package_manager` | 29 |
| `package_manager` + `utility` | 27 |
| `oci` + `project` | 27 |
| `oci` + `oci.rs` | 27 |
| `cli` + `oci` | 26 |

`oci` and `package_manager` are both the two highest-churn modules
individually and the most frequently co-changed pair — a typical commit in
this codebase crosses that boundary about once every seven commits to either
module.
