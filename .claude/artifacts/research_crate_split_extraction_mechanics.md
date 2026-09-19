# Research: mechanics of executing the ocx crate-split extraction commit-by-commit

## Metadata

**Date:** 2026-09-16
**Domain:** packaging / CI-CD / testing (Rust workspace refactor mechanics)
**Triggered by:** `adr_crate_split_workspace.md` Implementation Plan (phases 0, 0.5, 1, 2) — this artifact
covers execution mechanics the ADR and its prior-art research (`research_crate_split_prior_art.md`,
`research_crate_decomposition_patterns.md`) do not: commit shape, git rename fidelity, orphan-rule
mechanics, cross-crate test support, and parallel-worktree conflict surface.
**Expires:** 2027-03-16

## Direct Answer

Ten decisive recommendations, expanded below with evidence:

1. Hard-cut, no re-export shim, one squashed commit per crate (ADR already bans facades — this extends
   that ruling to the *transition*, not just the end state).
2. Two working-commits per extraction (pure `git mv`, then edits) before the final squash/checkpoint;
   register each landed move's SHA in `.git-blame-ignore-revs` immediately after.
3. Delete the wide `Error` enum in the same commit that adds the replacement per-crate enums — no bridging
   period, matching project doctrine.
4. Confirmed: orphan rule permits `impl ClassifyExitCode for ForeignType` in `ocx_cli`; ship one file per
   source crate under `crates/ocx_cli/src/exit/`.
5. Model `ocx_test_support` on `uv-test`'s shape (a crate consumed only via `[dev-dependencies]`), not
   `sqlx-test`'s (which tolerates a dev-dependency cycle) or tokio's `test-util` feature (which leaks via
   feature unification).
6. Confirm regex + witness for the harness, but port the raw-string/nesting fix this repo's own
   `edge_inventory.py` already needed, plus a corpus ratio sanity check, into the permanent harness.
7. Scaffold commit: `members = ["crates/*"]` glob + empty crate shells + alphabetically-sorted
   `[workspace.dependencies]`. Residual conflict: `ocx_cli/Cargo.toml` and `ocx_lib/src/lib.rs`'s `mod`
   list, both trivially auto-mergeable if sorted and merged one crate at a time (already the plan's
   design).
8. Gate every extracted crate's `cargo doc --all-features` under `-D rustdoc::broken_intra_doc_links`;
   grep moved files for `[crate::…]`/`[super::…]` doc links before deleting the boundary test.
9. `unreachable_pub = "warn"` from the first commit of each new crate (extends the ADR's own phase-0.8
   ratchet); keep the private-supertrait pattern for `OciTransport` — RFC 3323's `impl(crate)` syntax is
   still nightly-only as of today.
10. Checklist below; the tracing-target and `RUST_LOG`/`OCX_LOG` module-path change is real and
    live in this codebase (confirmed against `log_settings.rs`) — it belongs in the top section.

## Observable-behaviour risks the plan must guard

These are risks where the crate split changes something a *user or script* can observe, not just
internal structure — they need an explicit callout in the plan, not just a boundary test.

| Risk | Why it's real here | Grep the planner should run |
|---|---|---|
| **`tracing` target names change with crate name.** Every unadorned `tracing::info!`/`debug!`/`warn!` call's default target is `module_path!()` — `ocx_lib::oci::client` today, `ocx_oci::client` after extraction. `RUST_LOG`/`OCX_LOG` directives filtering on a module-path prefix silently stop matching. | Confirmed live: `crates/ocx_lib/src/cli/log_settings.rs:143-146` builds a real `tracing_subscriber::EnvFilter` from the `OCX_LOG`/`RUST_LOG` cascade documented at `website/src/docs/reference/environment.md:1132`. No explicit `target = "…"` override exists anywhere in `crates/ocx_lib/src` (checked — 0 hits), so every target is the default module path. | `grep -rn 'target\s*=\s*"' crates/ocx_lib/src` (confirm still 0 before each extraction); `grep -rn 'RUST_LOG=ocx_lib' -r .github/ test/ website/` for any hardcoded filter string naming the old crate (none found in docs today, but re-check per extraction; also check consumer repos — `ocx-mirror`, CI dashboards, and any owner-side debugging alias outside this repo). |
| **`module_path!()` / `std::any::type_name::<T>()` in output.** If either appears in an error `Display`/`Debug` impl or a rendered string, the printed text changes when the type's home crate changes — this is wire/output-visible, not internal. | Checked — 0 hits in `crates/ocx_lib/src` today, so currently clean. Must stay clean; a future extraction accidentally introducing one (e.g. a generic logging helper someone adds) would be a silent regression. | `grep -rn 'module_path!\|type_name::<\|any::type_name' crates/ocx_*/src` — run after, not just before, each extraction. |
| **`{:?}` (Debug) output is never a stability guarantee** ([`std::fmt::Debug` docs](https://doc.rust-lang.org/std/fmt/trait.Debug.html): "Derived Debug formats are not stable, and so may change with future Rust versions"), but an *ocx acceptance test* asserting on one would still break. Found: five Python acceptance tests reference `ocx_lib::…` paths in **comments/docstrings only** (`test_self_setup.py:71,91`, `test_self_update.py:198,217,227,241`, `test_config_setup.py:8`, `test_package_create_extract.py:45`, `test_index_ocx_sh.py:1474`) as traceability pointers to the Rust source, not as string assertions on program output. None of the ten hits assert on rendered text — this is a doc-staleness risk, not a behaviour break, but every comment goes stale the day the referenced module moves. | `grep -rn 'ocx_lib::' test/tests/*.py` after each extraction; update the traceability comment in the same commit, and separately `grep -rn '"ocx_lib::' test/tests/*.py` to make sure no *string literal* (as opposed to comment) ever asserts on a crate-qualified path. |
| **`schemars`-derived JSON Schema output must stay byte-identical** (ADR/system-design's own "byte-parity" promise for published formats). 50 files in `crates/ocx_lib/src` derive `JsonSchema`. `schemars`' default `$ref`/`title` naming is the bare Rust type name, not the module or crate path, so two same-named types now living in different crates do not collide in the schema output *by name* — but reordering of definitions (`$defs`) as a side effect of Rust's non-deterministic type-registration order across a differently-linked crate graph is a real risk category schemars users hit. | Concrete grep + diff, not just a grep: `cargo run -p ocx_schema > /tmp/schema.before.json` before an extraction touching a schema-derived type, same after, `diff -u /tmp/schema.before.json /tmp/schema.after.json` — must be empty for any commit that isn't intentionally changing the metadata schema. Wire this into `task verify` as a golden-file check, not a one-off manual diff. |

## 1. Move-a-module commit shape

**Recommendation: hard-cut per crate, no re-export shim, land as one squashed commit per crate (tens to
~150 files is normal); no batched deprecation window — that carve-out in `CLAUDE.md` is reserved for the
*public CLI/wire surface*, and internal module paths are explicitly "free to change... never add a
compat shim... as if it never existed."**

Evidence, pulled live via `gh api` against the real repositories (not summarized from memory):

| Project | PR | Files | Diff (+/-) | Shim? |
|---|---|---|---|---|
| ruff (ty) | [#24471 "Break the semantic index out into its own crate"](https://github.com/astral-sh/ruff/pull/24471) | 129 | +2028/-1693 | No — single squashed commit `150cf4b0`, old paths hard-cut |
| ruff (ty) | [#22106 "Move module resolver code into its own crate"](https://github.com/astral-sh/ruff/pull/22106) | 64 | +730/-559 | No |
| ruff | [#1816 "Split off ruff_cli crate from ruff library"](https://github.com/astral-sh/ruff/pull/1816) | 27 | +330/-282 | No — PR body: *"Please preserve the commits when merging"* (multi-commit intent), landed as 1 commit |
| uv | [#5236 "Move workspace abstractions to uv-workspace crate"](https://github.com/astral-sh/uv/pull/5236) | 32 | +141/-72 | No — PR body: *"These are really different from the rest of the existing crate as evidenced by the bifurcation in the requirements"* |
| uv | [#2579 "Move requirements resolution into its own crate"](https://github.com/astral-sh/uv/pull/2579), [#649](https://github.com/astral-sh/uv/pull/649), [#647](https://github.com/astral-sh/uv/pull/647), [#5028](https://github.com/astral-sh/uv/pull/5028), [#946](https://github.com/astral-sh/uv/pull/946), [#255](https://github.com/astral-sh/uv/pull/255), [#258](https://github.com/astral-sh/uv/pull/258), [#4020](https://github.com/astral-sh/uv/pull/4020), [#10126](https://github.com/astral-sh/uv/pull/10126), [#15482](https://github.com/astral-sh/uv/pull/15482) | — | — | uv's own crate directory (`uv-test`, confirmed present in `crates/`) shows the pattern of dozens of small, titled "Move X into its own/named crate" PRs across years — never a shim |
| rust-analyzer | [#2126 "Move ids to hir_def crate"](https://github.com/rust-lang/rust-analyzer/pull/2126) | 23 | +402/-363 | No — but **internally structured as 3 sequential commits**: `move ty interning to ty` → `introduce ra_hir_def` → `Move ids to hir_def crate`, i.e. scaffold-then-move-then-adjust as one reviewable unit |
| rust-analyzer | [#2112 "start ra_hir_def crate"](https://github.com/rust-lang/rust-analyzer/pull/2112) | 12 | +181/-131 | No — the scaffold PR that #2126 built on |

None of the seven real extraction PRs found across three unrelated large Rust monorepos kept a
`pub use old_path::*` compatibility shim past the extraction commit. This matches the ADR's own "no
facade crate" ruling for exactly the reasoning wasmtime-style facades exist for (independent consumer
versioning) — ocx's situation (lockstep-versioned satellites, no crates.io publication) is the same
situation ruff/uv/rust-analyzer are in internally.

**Recommendation for the 17-step series:** land each of the 17 extractions as **one commit per crate**
(matching the observed 12–129 file / 141–3721 diff-line range — none of this is unusually large by
industry standard), but *compose* that commit locally as two working commits before the final push —
see Q2 — because that is what preserves rename detection, not because history needs to show both.

## 2. Keeping `git blame`/rename detection through `git mv`

**Recommendation: a two-commit shape per extraction — commit A is a pure `git mv` (zero content edits,
so similarity is ~100% and `-M` attaches at any threshold), commit B is the import/wiring fixups on the
files at their new path. Add the crate's move commit SHA(s) to `.git-blame-ignore-revs` in a small,
separate follow-up, mirroring the exact pattern already live in ruff's own repo.**

Evidence:

- `git mv` [does not itself record renames](https://git-scm.com/docs/git-mv) — Git detects renames at
  diff/blame time by content similarity (`-M`, default threshold 50%; `-M100%` = exact only). A commit
  that moves a file *and* rewrites most of its `use` lines in the same commit can drop a small file below
  the similarity threshold, defeating detection for exactly the files most affected by an import-path
  rewrite — the two-commit split avoids this entirely for commit A, and commit B's edits are then diffed
  against the file at its *new*, already-attributed path, so blame chains through cleanly.
- `git log --follow` and `git blame -C`/`-M` are heuristics per invocation, not stored metadata — a
  directory-wide move (dozens of files in one commit) is exactly the case rename heuristics are least
  reliable for if content also changes in the same diff.
- **`.git-blame-ignore-revs` is real, current, load-bearing practice for exactly this scenario** — verified
  by fetching the live file from both repositories:
  - `rust-lang/rust`'s `.git-blame-ignore-revs` is dominated by formatting passes but includes structural
    moves too, e.g. `# std: move futex implementations into sys::sync::futex`.
  - **`astral-sh/ruff`'s `.git-blame-ignore-revs`** (fetched live) is populated almost entirely by
    crate/module extractions, one line-comment + one SHA per entry: `# Move module resolver code into
    its own crate` → [`b4c2825a`](https://github.com/astral-sh/ruff/commit/b4c2825afdd8c1010c3a5859521629d2d1e0a6df),
    `# Break the semantic index out into its own crate` →
    [`46199407`](https://github.com/astral-sh/ruff/commit/461994073e2f6cac13a28e60b06038ba50214ffc). The
    entry is added in its own tiny follow-up PR —
    [#24607](https://github.com/astral-sh/ruff/pull/24607), **1 file changed, +2 lines** — landed right
    after the extraction PR merged.
- General guidance corroborates: `.git-blame-ignore-revs` (Git ≥2.23) is [industry-standard for exactly
  mass-reformat/mass-move commits](https://www.michaelheap.com/git-ignore-rev/), honored natively by
  GitHub and GitLab blame views.

**Recommendation for the commit split:** during `hex-execute`, checkpoint commit A (`git mv` only) before
starting commit B's edits — `task checkpoint` amends a single commit today per this repo's workflow, so
this needs an explicit non-amend commit boundary between the move and the edits (a plain `git commit`
between the two, not a checkpoint amend) so both survive to review, then let `/hex-finalize`'s existing
conventional-commit composition decide the final squashed shape per the repo's own git workflow. Add the
`.git-blame-ignore-revs` line for the crate's landed move SHA(s) in the *same* PR series, last commit,
one line per crate — ruff's own file shows one-comment-one-SHA is sufficient, no batching needed.

## 3. Error-type dissolution patterns

**Recommendation: introduce the per-crate `thiserror` enum and delete the corresponding slice of the wide
`ocx_lib::Error` in the *same* commit — no bridging `From<New> for Old` shim — matching the project's own
"no compatibility shim" doctrine (`CLAUDE.md` § Stability tiers) rather than the more common OSS pattern
of a temporary bridge.**

- [`thiserror`](https://github.com/dtolnay/thiserror) (dtolnay, the de facto standard for typed library
  errors) is the right choice per the ADR's own E1–E6 contract; nothing in this research contradicts it.
  Common industry practice elsewhere (e.g. large service codebases moving off `anyhow`) *does* stage a
  bridging period where the old wide enum grows a `#[from]` arm wrapping the new typed error, precisely
  so `?` keeps compiling at every call site while call sites are migrated gradually. **That staged
  approach is the wrong shape for ocx specifically**: it is designed for external-consumer safety (a
  library whose callers can't all update atomically), and ocx's own doctrine says the opposite —
  internal code has zero stability and dissolves in one shot, so the bridge only adds a temporary type
  nobody needs before deleting it a commit later. Skip the bridge.
- **The `?`-operator ambiguity trap named in the question does not actually occur** from splitting one
  `From<SourceError> for WideError` into several `From<SourceError> for Crate1Error` /
  `... for Crate2Error` impls, because [`?` desugars to `From::from` resolved against the *single* return
  type of the enclosing function](https://doc.rust-lang.org/std/ops/trait.Try.html) — there is exactly one
  candidate impl per call site once the wide enum is gone, because each function now returns exactly one
  crate's error type. The real, common failure mode is the opposite: deleting the wide enum's blanket
  `impl From<io::Error> for Error` removes an implicit conversion every `?` site relied on, and every
  site that needs it must gain its own crate-local `#[from]` arm — but this is a **compile error**, not a
  silent bug, so the migration is self-checking: `cargo build` on the extracted crate is the completeness
  gate, no separate audit needed.
- **`impl From<ocx_oci::Error> for ocx_config::Error` (a `#[source]` boundary) is permitted by the orphan
  rule, and trivially so — this is not the interesting case the orphan rule restricts.** The
  [Rust reference on trait implementations](https://doc.rust-lang.org/reference/items/implementations.html)
  states the coherence condition as: an impl is allowed if the trait is local, **or** the implementing
  (`Self`) type is local. Here `Self` = `ocx_config::Error`, defined in `ocx_config` — local — so the impl
  is allowed regardless of `From`'s generic parameter (`ocx_oci::Error`) being foreign. The orphan rule's
  bite is reserved for `impl ForeignTrait for ForeignType` (needing the newtype pattern); `From<Foreign>
  for Local` is exactly the shape orphan-rule examples use to illustrate what's *always* fine. Document
  this once in `quality-rust-errors.md` so the next reviewer doesn't re-litigate it per extraction.

## 4. Orphan rule + `ClassifyExitCode` relocation

**Confirmed: moving `ClassifyExitCode` into `ocx_cli` and implementing it there for the 64 foreign error
types across all sixteen library crates is permitted — the trait becomes local to `ocx_cli`, satisfying
the same "trait is local" branch of the coherence rule cited above.** This is exactly the case the orphan
rule is designed to allow (a downstream application-layer crate classifying library errors it doesn't
own) — the restriction exists to stop *two crates* from conflicting over the same impl, and `ocx_cli` is
the only crate ever allowed to write these impls per contract E3.

Coherence pitfalls to avoid while writing the 64 impls:

- **No blanket impl.** `impl<T: std::error::Error> ClassifyExitCode for T` would conflict with every
  specific `impl ClassifyExitCode for ocx_oci::client::error::ClientError` the same crate also writes —
  Rust's overlap check rejects a blanket impl coexisting with specific impls for types the blanket already
  covers. The trait's existing default method (`fn classify(&self) -> Option<ExitCode> { None }`, per
  the system design's own snippet) is the *correct* fallback mechanism — a default method, not a second
  blanket impl.
- **`downcast_ref` ladders and `#[non_exhaustive]` don't interact badly** — `#[non_exhaustive]` only
  blocks *exhaustive matching without a wildcard arm* at the enum-variant level; it has no effect on
  `dyn Error + 'static` + `.downcast_ref::<ConcreteType>()`, which matches on the concrete type, not its
  variants. The real requirement is that every per-type `match` arm inside a `ClassifyExitCode` impl
  needs its own `_ => ExitCode::…` fallback if the source enum is (or becomes) `#[non_exhaustive]` — flag
  this in code review for the 64 impls, not as a mechanical gate.

**Recommendation for `crates/ocx_cli/src/exit/` shape: one file per source crate**
(`exit/ocx_oci.rs`, `exit/ocx_config.rs`, `exit/ocx_package_manager.rs`, …), not one ladder file.
Rationale, stated for review and merge-conflict surface specifically (per the task's ask): the current
`try_classify` function is a single 42-`use`-line, 55-`try_downcast!`-entry ladder — exactly the shape
that turns every one of the 17 parallel extraction work packages into an edit of the *same* file. Splitting
by source crate makes each extraction's CLI-side classification commit touch exactly one new file it
alone owns, with zero overlap against any other in-flight WP — directly satisfying the
`workflow-swarm.md` "file-disjoint work packages" requirement this ADR's own phase 2 depends on for
parallel agents. A thin `exit/mod.rs` re-exports the trait and (if genuinely needed) a manual
`try_classify`-equivalent dispatcher built from the per-file impls; that dispatcher is the one place that
*does* touch every extraction, so keep it a mechanical, append-only list (one line per crate) to keep its
conflict cost to "add a line," not "resolve a ladder."

## 5. Cross-crate `#[cfg(test)]` support

**Recommendation: model `ocx_test_support` on `uv-test`'s shape — a crate consumed only via
`[dev-dependencies]`, sitting structurally *below* every crate that uses it — not on `sqlx-test`'s shape
(tolerates a dev-dependency cycle) or tokio's `test-util` *feature* (leaks through feature unification).
For the env-override hook: a `OnceLock`-backed override map checked first by the accessor, set only from
`ocx_test_support`, never behind `#[cfg(test)]` in the crate being overridden.**

Verified live from both real crates' manifests:

- **`uv-test`** ([`astral-sh/uv/crates/uv-test`](https://github.com/astral-sh/uv/tree/main/crates/uv-test))
  is a real crate in uv's own workspace (confirmed present among uv's 71 crates). Its `Cargo.toml` lists
  regular `[dependencies]` on `uv-cache`, `uv-client`, `uv-configuration`, `uv-fs`, `uv-python`, etc. — i.e.
  it depends *down* on the utility/domain crates it wraps, and is itself pulled in only by higher-level
  crates' `[dev-dependencies]`. No cycle: `uv-test` never appears in anyone's regular `[dependencies]`.
- **`sqlx-test`** ([`sqlx-test/Cargo.toml`](https://github.com/transact-rs/sqlx/blob/main/sqlx-test/Cargo.toml))
  depends on the *umbrella* `sqlx` crate itself (`path = ".."`), which in turn depends on the backend
  crates (`sqlx-postgres`, etc.) — this **is** a dev-dependency cycle (backend crate →[dev]→ `sqlx-test`
  →[normal]→ `sqlx` →[normal]→ backend crate), tolerated because Cargo permits dev-dependency cycles and
  `sqlx-test` is only ever used from `tests/` integration binaries, never from an in-crate `#[cfg(test)]`
  unit test that would need the backend crate to already exist to build its own dev-dependency.
- **Tokio's `test-util`** is a *feature*, not a crate, and [Cargo's feature unification means one
  dev-dependency enabling `test-util` turns on the mockable clock for the entire build graph](
  https://github.com/tokio-rs/tokio/issues/4035) — a real, cited defect class for exactly the "leaks into
  release binaries" risk the ADR's own phase-0.5 goal (prove the env-override hook is *absent* from
  release builds) needs to avoid. A Cargo *feature* cannot give that guarantee; a crate boundary
  consumed exclusively via `[dev-dependencies]` can, because `[dev-dependencies]` are never compiled into
  a release build of a dependent crate — this is the one mechanism Cargo actually enforces here.

**Rule ocx should adopt (stronger than either real-world precedent):** `ocx_test_support` should carry
**zero `ocx_*` dependencies** — narrower than `uv-test`, which needs domain crates because it builds CLI
test fixtures. `ocx_test_support`'s only job per the ADR (env-override hook, `crates/ocx_lib/test/`'s
former contents) doesn't need any ocx domain type, so it can sit beneath every single crate in the
workspace trivially, with no dev-dependency-cycle reasoning required at all — this is simpler than either
cited precedent, not just a copy of one.

**Env-override hook — recommend a runtime `OnceLock`, not a `#[cfg(test)]` item:** `#[cfg(test)]` is
resolved *per crate compilation* — a `#[cfg(test)]` item inside `ocx_config` does not exist when
`ocx_sign`'s test binary links `ocx_config` as a normal dependency, because `ocx_config` is compiled
without `--cfg test` for that link. This is the actual reason the ADR phrases 0.5 as "a runtime-injectable
hook," not "a `#[cfg(test)]` fn" — `cfg(test)` cannot cross a crate boundary by construction. Concretely:
a `static OVERRIDES: OnceLock<RwLock<HashMap<&'static str, String>>>` compiled unconditionally into the
accessor's crate, checked before `std::env::var`; `ocx_test_support::env_override::set(key, val)` /
`clear()` are the only writers, and they live in a crate that ships only as a dev-dependency, so a release
binary never links code that calls `set`. **Prove it's live from another crate's test**, per the ADR's own
stated validation: a positive test in `ocx_sign` sets the override and asserts the read reflects it
without the real env var set, and a **negative** control — the same test with the override call deleted —
must fail against the real environment (the "control can pin the defect" pattern this repo's own memory
already names). This is the direct answer to "prove the seam is live from another crate's test."

## 6. Boundary tests during the layering phase

**Recommendation: confirm regex + witness for the phase-0.1 harness, with one hardening amendment forced
by evidence already sitting in this repo's own artifacts — port the raw-string/nested-comment fix
`edge_inventory.py` needed, plus its corpus-wide sanity ratio, into the *permanent* Rust harness, not just
the throwaway Python census script.**

Real tools exist and were considered against the ADR's ruling:

- [`cargo-modules --acyclic`](https://crates.io/crates/cargo-modules) proves acyclicity via a real
  dependency graph (`syn`-based), but only *after* crates exist — useless during phase 1, when the
  boundary is inside one crate's module tree, which is exactly the gap the ADR names.
- [`cargo-archtest`](https://crates.io/crates/cargo-archtest) is a genuine syn-based layer-rule checker
  (`MayNotAccess`/`MayOnlyAccess` rules) — closer to what's needed, but it is a new third-party dependency
  for a project whose own doctrine (`quality-core.md` "Don't Own Non-Domain Code") asks "does a library
  already solve this" — it does, partially, but adds an external tool to a verification-critical path for
  17 boundary tests that each need a one-off, project-specific forbidden-set, which `cargo-archtest`'s
  rule DSL would still need to be told per-boundary. The marginal win over the existing `syn::parse_file`-
  free regex approach is not obviously worth the new dependency for this project's specific need
  (temporary, deleted-on-extraction tests, not a permanent architecture-fitness function).
- **The regex approach already has a proven, *found-in-this-repo* failure mode**, per the edge inventory's
  own § 8 caveats: an earlier version of `edge_inventory.py`'s comment/string stripper — tracking `"` state
  continuously across a whole file — hit a raw-string test fixture in `crates/ocx_lib/src/trust.rs`
  (`r#"scope = { includ = ["ghcr.io/acme/*"] }"#`) and **silently discarded 94% of the file** (156,784 raw
  bytes → 10,145 cleaned bytes) as one giant "unterminated comment," before a whole-corpus cleaned/raw
  ratio sanity sweep caught it. This is the exact self-matching/false-negative failure class
  `quality-core.md`'s "Unchecked Green" section warns about, materialized for real in this project, not a
  hypothetical.

**Verdict: confirm regex + witness for the phase-0.1 harness, but require the fix, not the original
bug.** Concretely: the harness's string/comment stripper must use the corrected single-pass state machine
(per-`#`-count raw-string scanning, not continuous naive quote-tracking across the file) that
`edge_inventory.py` now has, and the harness's four mandatory properties (system design § 12.1) should
gain a **fifth**: a whole-scanned-corpus cleaned-byte/raw-byte ratio assertion (e.g. cleaned bytes must be
≥ 60% of raw bytes across the whole subtree, tunable), so a future desync fails loud instead of returning
a false "no forbidden import found." This reuses code already fixed in this repo rather than adding
`syn` as a new dependency — the cheaper, already-proven fix.

## 7. Parallel extraction by multiple agents

**Recommendation for the scaffold commit:**

1. **Root `Cargo.toml`: switch `[workspace] members` to a glob, `members = ["crates/*"]`, once, in the
   scaffold commit — never touched again by any extraction WP.** [Cargo natively supports glob patterns in
   `members`](https://doc.rust-lang.org/book/ch14-03-cargo-workspaces.html); this is the standard mitigation
   for exactly this conflict class, confirmed by community practice ("keeping dependency tables
   alphabetically sorted allows concurrent PRs to land on different lines and merge cleanly" generalizes
   directly to "using a glob means there's no member line to conflict on at all").
2. **Pre-create all 17(+2) crate directories as empty shells** (`Cargo.toml` with name/version/`publish =
   false`/`lints.workspace = true`, empty `src/lib.rs`) in the scaffold commit, so registering a new crate
   never requires editing a shared file — each WP's own `Cargo.toml` for its crate already exists and is
   the WP's own file to fill in.
3. **Pre-register `[workspace.dependencies]` entries, alphabetically, for every future crate name** with
   its path pinned — so a WP adding itself as a dependency of another (already-scaffolded) crate touches
   one alphabetically-placed line, not a contested insertion point.
4. **`Cargo.lock`: never hand-merge.** Regenerate via `cargo build`/`cargo check` after each merge; this is
   the standard Cargo answer to lockfile conflicts, and the pain point is real and cited even for `cargo`
   itself — [rust-lang/cargo#1818 "Add a custom Git merge tool for Cargo.locks and Cargo.tomls"](
  https://github.com/rust-lang/cargo/issues/1818) is a nine-year-open issue precisely because no merge
  tool beats "delete and regenerate" for a machine-generated file.

**Residual conflicts that remain, named rather than hidden:**

- **`crates/ocx_cli/Cargo.toml`** is not file-disjoint across WPs — every extraction that the CLI needs to
  consume adds one dependency line there. This is a *textual* conflict (two WPs both add a line near each
  other) but not a *semantic* one — alphabetically sorting the `[dependencies]` table makes each addition
  land at a distinct, mostly-non-adjacent line, and Git's three-way merge auto-resolves two insertions at
  different lines in the same file without conflict as long as neither insertion's diff context (default
  3 lines) overlaps the other's. Treat any conflict here as expected and mechanical ("keep both lines"),
  not a plan failure.
- **`crates/ocx_lib/src/lib.rs`'s `mod` list** is genuinely shared and shrinks by one line per extraction.
  Git's merge algorithm handles two branches each *deleting a different, non-adjacent line* from the same
  file cleanly (this is ordinary 3-way merge behavior, not a special case) — the mitigation is keeping the
  list alphabetically sorted (as `Cargo.toml` dependency tables already should be) so each deleted `mod`
  line's neighbors stay stable across WPs. The plan's own design already avoids needing Git to resolve
  this concurrently at all: phase 2 states extraction happens "one crate per commit series" merged in
  **serialized topological order** (per `workflow-swarm.md`'s merge discipline) — so in practice no two
  WPs' edits to `lib.rs` are ever merged as siblings; each rebases onto the previous extraction's already-
  landed removal. The scaffold's job is to make the *root* manifest and crate registration parallel-safe;
  `lib.rs`'s shrinking `mod` list is handled by serialization, not by file-disjointness, and that's an
  accurate, not optimistic, description of the mitigation.

## 8. Doc links across the move

**Recommendation: gate every extracted crate's docs with `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links"
cargo doc --all-features --no-deps` as part of that crate's `task verify`, and grep every moved file for
relative doc links before the boundary test guarding that module is deleted.**

- [`rustdoc::broken_intra_doc_links`](https://doc.rust-lang.org/rustdoc/lints.html) is a **rustdoc** lint,
  not a rustc or clippy lint — `cargo clippy -D warnings` never catches it, and neither does a plain
  `cargo build`. It must be its own gate (`cargo doc`), which is not currently implied by any of the
  ADR's phase-0 tooling steps — this is a gap the plan should close explicitly, one line in phase 0's task
  table.
- **`--all-features` matters specifically because of feature-gated doc links** — a link inside a
  `#[cfg(feature = "x")]`-gated item resolves fine when docs are built with that feature on and silently
  reports nothing when built with default features only, hiding exactly the broken-link case a
  feature-gated module (several exist across the 17 crates, e.g. `shell/hook.rs`'s `__testing` feature)
  would trigger.
- **Doc links do not auto-fix on a move.** A doc comment written as `` [`crate::foo::Bar`] `` or
  `` [`super::Bar`] `` inside a file `git mv`'d into a new crate still resolves relative to the *old* crate
  root/module tree textually, and rustdoc will report it broken once the item has actually moved out from
  under that path — but only if the lint is denied; otherwise it's a silent broken link in the published
  docs. Recommend a per-extraction grep — `` grep -rn '\[`\?crate::\|\[`\?super::' <moved files> `` — as a
  checklist item, resolved by rewriting to the absolute new-crate path (e.g.
  `` [`ocx_oci::client::error::ClientError`] ``) in the same commit as the move.

## 9. `pub(crate)` → `pub` widening discipline

**Recommendation: turn on `unreachable_pub = "warn"` (workspace lint) from the *first* commit of every
newly-created crate — extending the ADR's own phase-0.8 ratchet baseline to apply per-crate at birth, not
only after the fact — and keep the private-supertrait (sealed-trait) pattern for `OciTransport`, because
RFC 3323's proposed `impl(crate)` syntax is not stabilized as of today.**

- `unreachable_pub` is [an "allow by default" rustc lint recommending `pub(crate)` for items visible only
  within the crate](https://github.com/rust-lang/rust/issues/110922) — starting a new crate with it at
  `warn` from commit one means every accidentally-`pub` item is caught the moment it's written, which is
  strictly cheaper than an after-the-fact audit script over an already-large surface. It has known
  false-positive edges (a `pub fn` inside a `pub(crate)` struct is flagged even though it's already
  unreachable via the outer type — [rust-lang/rust#110922](https://github.com/rust-lang/rust/issues/110922))
  — acceptable noise for a ratchet, not a blocker.
- **RFC 3323 ("Restrictions") — `impl(crate)`/sealed-trait sugar — is real and current but not stable.**
  Verified from the RFC text itself and the [Inside Rust "Call for testing" post dated 2026-08-10](
  https://blog.rust-lang.org/inside-rust/2026/08/10/call-for-testing-impl-and-mut-restrictions/): the
  `impl_restriction` feature (`pub impl(crate) trait Foo`) is on nightly for testing as of one month before
  this research, explicitly replacing the private-supertrait pattern the ADR's `OciTransport` sealing step
  already plans to use. **This does not change the ADR's plan** — nightly-only, syntax still under
  discussion — but it is worth a one-line note in the plan that this is a near-term deprecation candidate:
  when `impl_restriction` stabilizes, `OciTransport`'s sealed-trait boilerplate becomes a single
  `pub(crate)`-style annotation, a cheap future cleanup, not a re-architecture.
- uv and ruff enforce minimal public surface less through a named tool and more through the workspace-wide
  `[lints.workspace]` table plus the same `unreachable_pub`-class lints at `warn`/`deny`; no evidence found
  of either project using a bespoke audit script beyond the compiler's own lint — reinforcing that the
  compiler lint is the industry-standard mechanism here, not a custom tool.

## 10. Known pitfalls checklist

- **`#[macro_export]` macros are the *safe* case if they use `$crate::`** — [`$crate` is specifically
  designed to always expand to the macro's *defining* crate](https://doc.rust-lang.org/reference/macros-by-example.html),
  so it re-resolves correctly after the crate is renamed/moved. The trap is a macro that hardcodes
  `crate::` or a bare relative path instead of `$crate::` — that breaks the moment the macro is invoked
  from outside its original crate. Grep: `` grep -rn 'macro_rules!' -A20 crates/*/src `` and manually check
  every internal path reference inside for a missing `$crate` prefix.
- **`include_str!`/`include_bytes!` are resolved relative to the *file*, which moves correctly with a
  `git mv` of that file — the trap is `..`-traversal reaching *outside* the crate being moved** (a shared
  `fixtures/` directory elsewhere in the old single-crate tree). Grep every `include_str!|include_bytes!`
  for `\.\./\.\./` before each extraction and move the referenced file with the crate if it's `..`-reached.
- **`env!("CARGO_MANIFEST_DIR")` re-resolves correctly per crate automatically** (Cargo sets it per
  compilation unit) — safe on its own; unsafe only combined with a hardcoded `../` offset baked in via
  `concat!`. Grep `CARGO_MANIFEST_DIR` for adjacent `..` literals.
- **`#[path = "..."]` module attributes point at a location that does not move with a directory `git mv`**
  unless updated in the same commit — grep `` grep -rn '#\[path' `` before every extraction.
- **`build.rs`** — same class as `include_str!`: safe if paths are built from `CARGO_MANIFEST_DIR`, unsafe
  if hardcoded relative.
- **`tests/fixtures` relocation is already named in the ADR** (`index_wire_conformance.rs` +
  `tests/fixtures/{index_wire,live_index_ocx_sh}` moving to `crates/ocx_index/tests/`) — the pitfall not
  yet named is that `test/scripts/sync_index_conformance.sh`'s own hardcoded output path must move in the
  *same* commit, or the next re-vendor run writes fixtures to the old (soon orphaned) location.
- **Feature-gated `mod`s and orphaned files are a silent-loss trap, not a compile error.** Rust does not
  error on a `.rs` file that physically exists under `src/` but is never reached by any `mod` declaration
  — it is simply excluded from the build, silently. `cargo build` succeeding after an extraction is
  therefore **not** proof every moved file is still compiled. Reuse the edge inventory's own instrument —
  it already asserts "files scanned: 388… 0 unmapped files" — as a per-crate post-extraction check: every
  physical `.rs` file under the new crate's `src/` must be reachable from `lib.rs` via some `mod` chain.
- **`#[cfg(test)]` items are invisible to `tests/*.rs` integration tests** (compiled as a separate crate
  seeing only `pub` items) — this is the same root cause as Q5's env-override hook, cross-referenced here
  because it also bites any *other* `#[cfg(test)]` helper someone is tempted to reach for during the
  split instead of routing through `ocx_test_support`.
- **`serde(remote = "...")` mirrors a foreign type's field list by hand** — if the mirrored type is itself
  being moved/renamed in the same split, the remote definition's fields can silently drift from the real
  struct (compiles, produces a wrong wire shape). High severity given the ADR's own OCI/index byte-parity
  promise. Grep `remote = ` across the tree before any move touching a `serde`-remote'd type.
- **`schemars` cross-crate schema drift** — see the top-level Observable-behaviour section; the concrete
  gate is a byte-diff of `cargo run -p ocx_schema`'s output before/after, not a grep.
- **`tracing` target / `RUST_LOG` module-path change, `module_path!`/`type_name` in output, and `{:?}`
  assertions on crate-qualified paths** — all covered in the top-level Observable-behaviour section with
  the concrete greps already run against this repo.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| [astral-sh/ruff PR #24471](https://github.com/astral-sh/ruff/pull/24471) | GitHub PR (fetched via `gh api`) | 2026 | Real 129-file/3721-line single-commit crate extraction, commit shape evidence |
| [astral-sh/ruff PR #22106](https://github.com/astral-sh/ruff/pull/22106) | GitHub PR | 2026 | Real 64-file crate extraction |
| [astral-sh/ruff PR #1816](https://github.com/astral-sh/ruff/pull/1816) | GitHub PR | historical | `ruff_cli` split, "preserve the commits" request |
| [astral-sh/ruff PR #24607](https://github.com/astral-sh/ruff/pull/24607) | GitHub PR | 2026 | `.git-blame-ignore-revs` follow-up, 1 file/+2 lines |
| [astral-sh/ruff `.git-blame-ignore-revs`](https://github.com/astral-sh/ruff/blob/main/.git-blame-ignore-revs) | Repo file (fetched live) | 2026 | Real, current, all-crate-move-entries blame-ignore file |
| [rust-lang/rust `.git-blame-ignore-revs`](https://github.com/rust-lang/rust/blob/master/.git-blame-ignore-revs) | Repo file (fetched live) | 2026 | Confirms convention used by the Rust project itself |
| [astral-sh/uv PR #5236](https://github.com/astral-sh/uv/pull/5236) | GitHub PR | historical | `uv-workspace` crate extraction, 32 files |
| [astral-sh/uv crates directory](https://github.com/astral-sh/uv/tree/main/crates) | Repo listing (fetched live) | 2026 | Confirms `uv-test` crate exists among 71 crates |
| [astral-sh/uv `crates/uv-test/Cargo.toml`](https://github.com/astral-sh/uv/blob/main/crates/uv-test/Cargo.toml) | Repo file (fetched live) | 2026 | `uv-test` dependency direction (depends down, consumed as dev-dep) |
| [transact-rs/sqlx `sqlx-test/Cargo.toml`](https://github.com/transact-rs/sqlx/blob/main/sqlx-test/Cargo.toml) | Repo file (fetched live) | 2026 | `sqlx-test`'s dev-dependency-cycle shape (depends on umbrella crate) |
| [tokio-rs/tokio issue #4035](https://github.com/tokio-rs/tokio/issues/4035) | GitHub issue | — | `test-util` feature unification leak |
| [rust-lang/rust-analyzer PR #2126](https://github.com/rust-lang/rust-analyzer/pull/2126) | GitHub PR | historical | 3-commit internal sequence for `hir_def` crate start |
| [rust-lang/rust-analyzer PR #2112](https://github.com/rust-lang/rust-analyzer/pull/2112) | GitHub PR | historical | Scaffold-first commit for a new crate |
| [git-scm.com git-mv docs](https://git-scm.com/docs/git-mv) | Official docs | — | `git mv` mechanics, no explicit rename metadata |
| [michaelheap.com — git blame --ignore-rev](https://www.michaelheap.com/git-ignore-rev/) | Blog | — | `.git-blame-ignore-revs` general usage pattern |
| [Rust Reference — Implementations (orphan rule / coherence)](https://doc.rust-lang.org/reference/items/implementations.html) | Official reference | — | Orphan rule exact condition: trait local OR type local |
| [dtolnay/thiserror](https://github.com/dtolnay/thiserror) | Official repo | — | Per-crate typed error derive, `#[source]`/`#[from]` |
| [std::fmt::Debug docs](https://doc.rust-lang.org/std/fmt/trait.Debug.html) | Official docs | — | Derived Debug output is explicitly not stable |
| [Rust Reference — Macros by example (`$crate`)](https://doc.rust-lang.org/reference/macros-by-example.html) | Official reference | — | `$crate` re-resolves correctly after a crate rename |
| [rustdoc book — Lints](https://doc.rust-lang.org/rustdoc/lints.html) | Official docs | — | `rustdoc::broken_intra_doc_links` is rustdoc-only, not caught by clippy |
| [rust-lang/cargo issue #1818](https://github.com/rust-lang/cargo/issues/1818) | GitHub issue | — | `Cargo.lock`/`Cargo.toml` merge-conflict pain, still open |
| [Cargo Book — Workspaces](https://doc.rust-lang.org/book/ch14-03-cargo-workspaces.html) | Official docs | — | `members` glob pattern support |
| [rust-lang/rfcs — RFC 3323 Restrictions](https://rust-lang.github.io/rfcs/3323-restrictions.html) | RFC text | — | `impl_restriction`/`mut_restriction`, sealed-trait replacement syntax |
| [Inside Rust blog — Call for testing: impl and mut restrictions](https://blog.rust-lang.org/inside-rust/2026/08/10/call-for-testing-impl-and-mut-restrictions/) | Official Rust blog | 2026-08-10 | Confirms RFC 3323 is nightly-only as of one month before this research |
| [rust-lang/rust issue #110922](https://github.com/rust-lang/rust/issues/110922) | GitHub issue | — | `unreachable_pub` known false-positive edge |
| `crates/ocx_lib/src/cli/log_settings.rs` (this repo) | Source (read live) | 2026-09-16 | Confirms real `EnvFilter` construction from `OCX_LOG`/`RUST_LOG` |
| `.claude/artifacts/discover_crate_split_edge_inventory.md` § 8 (this repo) | Internal artifact | 2026-09-16 | Real raw-string comment-stripper near-miss, motivates the harness hardening |
