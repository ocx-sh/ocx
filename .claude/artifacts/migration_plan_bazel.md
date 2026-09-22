# Bazel migration plan

Decided: 2026-09-21 · Bazel pin: 9.2.0 · LTS stage on that date: Active

Satisfies **C-002** of [`plan_bazel_build_adoption.md`](./plan_bazel_build_adoption.md),
under the `go` recorded in [`decision_bazel_adoption.md`](./decision_bazel_adoption.md)
— read that file's Verdict and its caveat before reading this one as a promise of a win.
The binding design is [`adr_bazel_build_adoption.md`](./adr_bazel_build_adoption.md).

**How the LTS stage was determined**, because "Active" is the kind of word that gets
recalled rather than read: the vendor's own support matrix at `https://bazel.build/release`,
fetched 2026-09-21, prints *"Bazel 9 · Active · 9.2.0 · Dec 2028"*, with Bazel 10 at
**Rolling** and Bazel 8 at **Maintenance** — and that page defines Active as "the current
active LTS release". Corroborated against the pinned binary, which prints `bazel 9.2.0`
(`~/.ocx/symlinks/ocx.sh/bazelbuild/bazel/candidates/9.2.0/content/bazel --version`,
run 2026-09-21), and against the release feed, where 9.3.0 is at rc2 and no 10.x GA exists.

### Order

1. **Rust unit tests — `//crates/...` — the pilot.** 20 workspace members, each a
   `rust_library` plus a `rust_test` over its inline `#[cfg(test)]` tests, plus one
   `rust_test` per `crates/<name>/tests/*.rs`; the three `external/` patched submodules
   get `rust_library` only. Shippable artifact: **a per-crate cache skip on the Rust
   unit-test graph**, demonstrable on `--disk_cache` alone (C-029), so nothing in the
   pilot waits on the owner-gated remote realm. Target and BUILD-file counts are
   **WP-12's generated output, never a constant in this file** — the plan's re-derivation
   already moved the ADR's 57/54/23/34 to 55/52/22/33 once.
2. **The CI lane swap — `verify-basic.yml`'s `smoke` job and `verify-deep.yml`'s Linux
   leg — blocked on: WP-30's step-level cold/warm measurement of `Smoke (Linux)`.**
   This is a real block, not a sequencing note. Per M-01's matrix arithmetic,
   `verify-deep`'s build stage is `max(windows, macos, linux)` and Bazel takes only the
   Linux leg, so **stage 2's contribution to `verify-deep`'s 3407 s median is
   structurally zero**; the only surface where a win can land is `Smoke (Linux)`
   (1195–1263 s) minus its lint, build and non-test portions. If WP-30 measures that
   compile does not dominate there, **the swap does not land** and `.verify:build-test`
   keeps its fourteen steps.
3. **Casts and the website — blocked on:** stages 1–2 green, plus the `rules_ocx` tool
   path (`ocx.project(name = "tools")` and the `@tools//:` surface). The 39 cast
   *recording* targets need no `agg` and no published mirror; GIF rendering through
   `@tools//:agg` is split out and carries the owner precondition alone. The site rule
   ships with `no-remote-cache`, and removing that tag is gated on the three hermeticity
   proofs, not on a green build.
4. **Acceptance — the 172 `test/tests/test_*.py` modules as `sh_test`s — blocked on:**
   stage 3 and the hermeticity harness. One `sh_test` per test module (never a
   `SCOPED_ROWS` row), `exclusive`-tagged, with cache-result caching **deliberately off**
   — so this stage buys ordering and selection, not caching.

### Pins

| Thing | Value | Verified by |
|---|---|---|
| Bazel | 9.2.0 | `ocx.lock` is the **pin authority** — a per-platform digest, `sha256:252fc12c947fd30b67c2f522f56cd297efef03c85495905242fdceb4ff808c0c` (M-07), which is strictly stronger than a semver string. The resolved binary answers `bazel 9.2.0` to `--version` (run 2026-09-21). `cat .bazelversion` is a **format** check only (BZL-FLAG-01) and is the left-hand side of `bazel:pin:check`. **Measured caveat, 2026-09-21: neither `.bazelversion` nor an `ocx.lock` bazel row exists in the tree yet** — WP-10 writes both, and until it does, `cat .bazelversion` verifies nothing |
| rules_rust | 0.74.0 | `curl -s https://bcr.bazel.build/modules/rules_rust/metadata.json`, re-read 2026-09-21: `.versions[-1]` is `0.74.0`, so the pin is the newest BCR release |
| Rust toolchain | 1.95.0 | `rust.toolchain(versions = ["1.95.0"])` in `MODULE.bazel` (WP-10). A **second, parallel pin** beside `rust-toolchain.toml`'s `channel = "1.95.0"` (read 2026-09-21), because rules_rust ignores that file ([#2753](https://github.com/bazelbuild/rules_rust/issues/2753), open). Whoever bumps one bumps the other in the same commit; C-026 is the check that makes that honest |
| rules_shell | 0.8.0 | `curl -s https://bcr.bazel.build/modules/rules_shell/metadata.json`, re-read 2026-09-21: `.versions[-1]` is `0.8.0` |
| buildifier_prebuilt | 8.5.1.4 | `curl -s https://bcr.bazel.build/modules/buildifier_prebuilt/metadata.json`, re-read 2026-09-21: BCR carries `8.5.1.4` **and a newer `10.0.1`**. The pin is a deliberate C-004 decision, not drift — recorded here so a later bump is taken as a decision rather than as catching up |
| rules_ocx | `git_override` @ `9ced5ffb79a77d37d6f479498bf32517a49b3dc7` | Branch `bazel-9`, [ocx-sh/rules_ocx#15](https://github.com/ocx-sh/rules_ocx/pull/15), `bazel test //...` green at 80/80 on 9.2.0 (M-05). **Not on BCR**, so there is no registry version to verify against — the commit is the pin, and `ocx.project()`'s `name` attribute is mandatory |

`bazel_dep`s carry an **explicit version** each; every crate-universe instance carries an
explicit `lockfile =` (BZL-RUST-01). `compatibility_level` is non-functional on 9.1.0+ and
nothing here builds on it.

### Lock mechanism per language

| Language | Bazel-side lock | Regenerated by | Freshness gate |
|---|---|---|---|
| Rust | `MODULE.bazel.lock`, **committed, never gitignored** (BZL-MOD-01), over `Cargo.lock` as the source of truth via `crate.from_cargo` | `bazel mod deps --lockfile_mode=update` | `--lockfile_mode=error` on its **own CI leg**, plus `task bazel:build:drift`. A `.bazelversion` bump commit that does not carry the regenerated lock is BZL-MOD-06, and the lock is discarded and rewritten with nothing printed — including across a Maintenance patch bump |
| Python (acceptance) | none — `uv.lock` stays the only lock; Bazel wraps execution, not resolution | `uv lock` | `uv sync --frozen` |
| TypeScript (website) | none — `bun.lock` stays the only lock | `bun install` | `bun install --frozen-lockfile` |

No `npm_translate_lock` and no `bun.lock` ingestion: there is no `bun_lock` attribute and
no ingestion path (BZL-JS-01, BZL-JS-03), so the site is a coarse Starlark rule with the
exemption named in a comment in `website/site.bzl` rather than a rules_js port. **Never
hand-edit a Bazel-side lockfile to fix a drift** (BZL-ARCH-24) — regenerate it.

### First CI lane

- **Name:** the `smoke` job in `.github/workflows/verify-basic.yml`.
- **Trigger:** `pull_request`, and `push` to `main`.
- **Exact command:** `bazel test //crates/...`
- **Gates on:** that command exiting **0**, and `task bazel:pin:check`,
  `task bazel:build:drift` and `task bazel:tag:guard` each exiting **0**. The three gate
  tasks run in `cmds:`, never in `preconditions:` — `task --force` skips `preconditions:`.
- **Credentials:** the read credential only; the **write** credential never on a
  pull-request lane (BZL-CACHE-01), `--remote_upload_local_results=false` on every
  non-write lane, and no `--credential_helper` in any tracked rc file (BZL-CACHE-04) — the
  rc is written by a CI-only action.
- **What the lane cannot gate on:** a cache **outage**. Measured on both majors, the
  cache-only shape logs a WARNING, builds locally and exits **0**, and no fallback flag
  changes it (BZL-CACHE-26). A lane that must fail on an outage needs a check outside
  Bazel; that is S-008's assertion, not this exit code.

### Deferred, with the condition that reopens it

- **Target selection — reopens when the whole-repo median passes ~40 min, and that
  tripwire is already past on one workflow.** Stated honestly rather than as a clean
  deferral: `verify-deep`'s median is **3407 s ≈ 57 min** and `verify-basic`'s is
  **1731 s ≈ 29 min**. The ~40 min / ~300-target figure is a **derived** tripwire from one
  team's stated pre-adoption context (BZL-CI-01), not a published threshold, and it is
  about target selection rather than adoption. **Target selection is deferred by plan
  scope anyway**: both production tools document real false-negative classes, and below
  the switch point that correctness risk is unpaid-for (BZL-CI-21, BZL-CI-22). So the
  condition here is not "the median" alone — it is the median **plus** a decision to
  accept the false-negative class.
- **Remote execution — reopens when the BZL-CACHE-07 gate passes, and it cannot pass as
  the tool path stands.** M-05 measured that `rules_ocx` launchers exec an **absolute**
  `$OCX_HOME/packages/…/content/<bin>` path, which no remote executor can resolve. RBE is
  therefore **structurally impossible**, not merely unscheduled, and no future reader can
  reopen it without replacing the tool path itself. Remote **caching** is unaffected and
  is stage 2 of 4.
- **`gazelle_rust` BUILD generation — reopens on a BCR release carrying the Bazel-9 fix
  `d1ae032` plus a documented `[patch.crates-io]` story.** Until then all 22 BUILD files
  are hand-written, which `go-no-go.md:98-100` makes the correct default below Production
  rather than a failure. **The standing cost, stated not buried:** every new
  `crates/*/tests/*.rs` and every `Cargo.toml` dep-edge change reds `bazel:build:drift`
  until a human hand-edits a BUILD file. The drift gate makes that tax loud; it does not
  remove it, and WP-12 measures the dep-edge churn so the steady-state cost is a number
  before it is accepted.
- **Modelling the `external/` submodule coupling in Bzlmod — reopens when one of the three
  forks declares its own `MODULE.bazel`.** All three are mode-`160000` submodules that are
  also `[patch.crates-io]` build dependencies, and **none declares a `MODULE.bazel`**
  (verified 2026-09-21), so BZL-ARCH-28's precondition is unmet and the coupling stays
  invisible to `bazel mod` until that file exists. WP-11 picks the interim route.
- **Python and TypeScript generation — not adopted, and no condition is offered.** The
  Python extension is Production only when paired with a pytest wrapper, and the
  acceptance stage deliberately wraps execution instead; the JS plugin's prebuilt
  distribution is still `0.0.x`. Both ecosystems keep their own lock and their own
  resolver, per the table above.
