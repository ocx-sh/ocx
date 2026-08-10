# Review Round 1 — Interpolation Token Grammar

Branch `hex/interpolation-token-grammar`, baseline `main` (e454ce83).
Panel: 7 opus reviewers (spec+convergence, test-coverage, security, performance,
user-feedback, architecture, docs). Verdict: **Request Changes** (one Block).

Work packages below are **file-disjoint**. Every item cites the reviewer's own
file:line. Do not widen scope beyond the listed files.

---

## WP-A — Rust code (Block + 3 Warn)

### A1 [Block] Unbounded `${self.env.KEY}` expansion — CWE-400/409/776

`crates/ocx_lib/src/package/metadata/template.rs:354,369` +
`crates/ocx_lib/src/package_manager/composer.rs:604-614`.

`lookup_self_env` substitutes the referenced var's already-**resolved** value
(`entry.value.clone()`), and `composer.rs:614` pushes every resolved entry into
`declared_before` unconditionally. So `B = "${self.env.A}${self.env.A}"` doubles.
Measured by a reviewer against the real `EnvResolver`: `A0 8B → A11 16,384B →
A22 33,554,432B` from **648 bytes** of authored metadata. `validate_for_publish`
accepts a 41-var document (3,880 bytes serialized) — the publish gate never
resolves, so it cannot catch this. Reachable from a **transitive dependency's**
metadata on every `ocx env` / `ocx run` / `ocx package exec` / launcher re-entry.

Fix: enforce a byte budget on the resolved output inside
`TemplateResolver::resolve_inner` (after each `push_str`, template.rs:265-269).
New `TemplateError` variant, classified `ExitCode::DataError` (65) alongside the
other refusals at template.rs:586-600. Constant `MAX_RESOLVED_VALUE_BYTES = 64 *
1024` with a doc comment stating it is an interface number the owner may move.

A per-value cap is sufficient: the chain manifests as one var exceeding the cap,
so it errors at the first offender. Do **not** add an `Env.variables` count cap —
that would refuse already-published documents (see Deferred D-8).

Resolve-time is the only place that works. One budget here closes every sink;
guarding each of the six commands would be six guards for one root cause.

Tests: a unit test with a doubling chain asserting the new variant + 65, and a
test that a value just under the cap still resolves. **Prove it red** — remove the
budget check, watch the test fail, restore, re-run.

### A2 [Warn] Unknown-token message echoes control bytes, unbounded and doubled — CWE-117/150

`crates/ocx_lib/src/package/metadata/template.rs:541,466-469` +
`template/scanner.rs:327-332`.

The token body admits any byte except `}`, and `unknown_token` stores it verbatim,
so newlines and ANSI escapes reach stderr raw — a forged multi-line error is
constructible. The `Escape` branch **doubles** the volume via `scanner::escape`
(a 1,000,003-byte token produced a 2,000,077-byte message).

The echo class is pre-existing (`main`'s `UnknownPlaceholder`); the diff adds the
doubling and owns the construction site. The repo already sets this standard —
`RelativePath::parse` rejects control characters citing CWE-117
(`utility/fs/path.rs:242-248`).

Fix: truncate `source` to a fixed prefix on a char boundary and escape control
characters before storing; apply the same to the `Escape` branch, or emit a
generic `$${…}` once truncated.

### A3 [Warn] `find_in_self_env_scope` is O(V×T) on publisher-controlled input

`crates/ocx_lib/src/package/metadata/template.rs:387-403`.

`matches.next()` is called a second time (:399) for ambiguity detection, so the
whole scope is walked per token even on a unique hit. Callers: the composer per
resolved var (composer.rs:604/606) and the publish gate (validation.rs:282).
`scope` grows by one per var; `load_object_data` applies no size cap locally and
the 4 MiB fetch ceiling still admits ~90k minimal vars → ~8×10⁹ comparisons.
Independent of A1: this bites with many *small* vars, so the byte budget does not
close it.

Fix: maintain a `HashMap<&str, (first_index, count)>` incrementally beside
`declared_before`; lookup and the declared-twice test both become O(1) and both
`UndefinedSelfEnvRef` / `AmbiguousSelfEnvRef` stay derivable. Same treatment for
the publish gate's `Vec<&str>` scope (validation.rs:258). Keep
`find_in_self_env_scope`'s observable semantics byte-identical.

### A4 [Warn] `libc_lint` maps literals back to offsets with raw pointer arithmetic

`crates/ocx_lib/src/package/libc_lint.rs:286-299`.

`literal.as_ptr() as usize - base` relies on every `Segment::Literal` borrowing
from the scanned input at its own position — true today, enforced nowhere, and
living one module from the code that could break it. A scanner change returning
a non-borrowed literal underflows the subtraction and panics on the next slice
rather than failing a test.

Fix: move the split into the scanner (`scanner::split_outside_tokens(value, ':')`)
or give `Segment::Literal` its byte range. The module doc already argues that
"which bytes lie inside a `${…}` is the scanner's answer and never this module's" —
the offset mapping is the same knowledge.

### A5 [Suggest] `mod render;` is private while `RenderModifier` is publicly reachable

`crates/ocx_lib/src/package/metadata/template.rs:19` — reachable through
`scanner::Token::modifier` but unnameable by a consumer (`libc_lint` is reduced to
`modifier.is_none()`). Make it `pub mod render;` matching `scanner`, or re-export
`RenderModifier` from `template`.

---

## WP-B — Rust tests

All in `crates/ocx_lib/src/package/metadata/template/scanner.rs` unless noted.

- **B1 [Warn]** `validation.rs:282` — the publish gate's `AmbiguousSelfEnvRef` leg
  is implemented but untested (only the undefined leg is, at validation.rs:1029).
  Add: vars `A`, `A`, `B="${self.env.A}"` through `validate_for_publish`,
  asserting `AmbiguousSelfEnvRef { key: "A" }`. (Closes convergence gap C-021.)
- **B2 [Warn]** `:104-111` claims `DependencyName::try_from` is used *because* a
  65-byte name passes a pattern check and fails the conversion — but no test feeds
  a NAME > 64 bytes, so replacing the guard at `:283` with a `SLUG_PATTERN` match
  reds nothing. Add a C-008 row: body `deps.<"a"×65>.installPath` →
  `UnknownToken`, verbatim token asserted.
- **B3 [Warn]** `test/tests/test_env.py:542-549` — the `ocx package info` leg has
  no reachable red state. Beyond the rc==0 half the docstring already concedes,
  the JSON key is the **raw CLI argument** echoed back
  (`command/package_info.rs:134`, serialized as a map key at
  `api/data/package_description.rs:79-88`), so `short in json.loads(...)` holds on
  every successful invocation regardless of package state. Delete both assertions
  and the paragraph claiming they prove anything; the falsifiable half is the
  `inspect` leg at :535-540. This reverses an earlier decision of mine to keep the
  leg — the second vacuity reason is new evidence.
- **B4 [Warn]** `test/tests/test_env.py:562` — the only acceptance test of
  create-time refusal uses a `constant` var, so the libc lint's scan scope is
  empty and the ordering invariant asserted in `package_create.rs:137-143` is
  untested (moving the gate below the lint keeps rc 65 and identical stderr). Add
  a leg declaring an interface `Path` var `PATH = "${workspaceFolder}/bin"` on a
  Linux platform, asserting stderr names the token and does **not** report an
  unresolvable scan scope.
- **B5 [Suggest]** Add `':'` to `ROUND_TRIP_ALPHABET` (:986) — the round trip
  currently never sees a modifier separator.
- **B6 [Suggest]** Add the `${installpath}` near-miss hint assertion (S-009,
  ADR:1321) — the token is in the C-004 rejection corpus (:760) but no test
  asserts its hint, so a case-only near-miss regression would not red.
- **B7 [Suggest]** Add a C-008 row for `${installPath:}` (empty modifier) →
  `UnknownModifier { modifier: "" }`, currently a user-visible message unpinned.
- **B8 [Suggest]** Add `${installPath}${self.installPath}` to the C-035 list — two
  directly adjacent recognised tokens are never scanned unescaped.
- **B9 [Suggest]** Add `${日本語}` to the C-004 corpus as `UnknownToken` —
  `root_run`'s byte slice (:393-399) is never exercised on a multi-byte body.
- **B10 [Suggest]** `template/render.rs:198-202` re-derives its expectation with
  the same `cfg!(windows)` predicate the implementation uses (:71-73), so it
  discriminates an arm swap but not a predicate change. Drop it, or assert
  `render(v, m, Host::current()) == render(v, m, <host named for this target>)`.

---

## WP-C — User docs (`website/src/docs/`)

5 Critical, 2 High, 4 accuracy. Do not add migration prose — pre-1.0 breaks just
break.

- **C1 [Critical]** `authoring/env-surface.md:50,65` — still teaches "Two
  placeholders are available" / "Only `${installPath}` and
  `${deps.NAME.installPath}` are recognized". Four bodies now exist
  (`scanner.rs:219-224`): `installPath`, `self.installPath`, `self.env.KEY`,
  `deps.NAME.installPath`, each with an optional `:native`/`:posix`. The page is
  also silent on `$${`, the only escape and the only thing between a publisher
  shipping another tool's `${…}` and a hard 65. **This page was not touched by the
  diff at all.**
- **C2 [High]** `reference/metadata.md:223-225` and `:228-229` — all six command
  names in the read-path sentence are wrong. `ocx install`, `ocx which`,
  `ocx deps` are deleted root forms (exit 64); `ocx pull` and `ocx inspect` are
  live root commands with *different* meanings from the `ocx package` verbs the
  links point at. Prefix all six with `ocx package …`; same for `create`/`push`
  at :228-229. Commit 8fe2e618 fixed exactly this in the ADR and missed the doc.
- **C3 [High]** `reference/metadata.md:218` — the branch-3 example is wrong:
  `${deps.cmake.version}` routes to `UnknownField` (`scanner.rs:288`), never to
  `UnknownTokenHint::SupportedBodies`. Split into two bullets: a recognised root
  with an **illegal body** (`${self.env.A B}`) lists supported bodies; a
  recognised namespace with an **unknown leaf** (`${deps.cmake.version}`) names
  the field and lists the leaves that exist.
- **C4 [Warn]** `reference/metadata.md:226` — "echoing the token verbatim" is
  false for five of the six commands; only `inspect` echoes. The branch's own
  acceptance docstring records that `ocx package info` reads only the
  `__ocx.desc` tag and never the package metadata.
- **C5 [Warn]** `reference/metadata.md:702` — binaries-scan eligibility omits
  `${self.installPath}` and does not say a render modifier silently shortens the
  claim (`template.rs:115` returns `None` when `modifier.is_some()`,
  `bin_scan.rs:100` skips the var). On Linux/`any` the publisher is told via
  `ModifierBearingScanScope`; on darwin/windows `check_declared_libc` early-returns
  (`libc_lint.rs:125-127`) and **nothing** tells them.
- **C6 [Critical]** `reference/env-composition.md` + `in-depth/environments.md` —
  D8's resolve-then-gate resolves **every** declared var, crossing or not, so a
  template fault in a private-only var now fails composition on the interface
  surface where it previously never ran. An already-published package that
  composed fine can now exit 65. Neither page says this.
- **C7 [Critical]** `reference/metadata.md` — a refused token in a baked
  entrypoint `args` element aborts the **installed launcher at run time** with
  exit 65 (`launcher/exec.rs:169-174` + the `classify.rs` ladder rung). The
  refusing set at :222-230 omits this path entirely.
- **C8 [Critical]** `reference/metadata.md` (Render Modifiers) — a render modifier
  on an interface `PATH` value (`${self.installPath:posix}/bin`) makes
  `ocx package create` fail the libc lint with exit 65 on Linux and `any`. The
  section recommends `:posix` and never mentions that the one place a path value
  usually lives refuses it.
- **C9 [Critical]** `reference/command-line.md#package-create` — the binaries scan
  now accepts `${self.installPath}/bin` and **silently** skips a modifier-bearing
  segment, narrowing the auto-filled `binaries` with no diagnostic.
- **C10 [Medium]** `reference/command-line.md#deps` — `ocx package deps` no longer
  hides a package whose env names an undeclared dep (was: skipped with a
  corrupted-install warning). Behaviour inverted; neither rule is documented.
- **C11 [Medium]** `in-depth/environments.md:99` — "`private` entries of a dep …
  are never forwarded" is now misleading: the *entry* is not forwarded, but its
  resolved **value** can be, via an `interface` var's `${self.env.KEY}`
  (composer.rs:127-138). `env-composition.md:149` states the mechanism; the page
  making the encapsulation promise does not.
- **C12 [Accuracy]** `reference/command-line.md:2691` — "only considers
  `${installPath}`-rooted path variables"; now also `${self.installPath}`-rooted,
  and it contradicts `metadata.md:177-183`, which tells authors to prefer
  `${self.installPath}`.
- **C13 [Accuracy]** `reference/command-line.md:725` — "resolves `${installPath}`
  in any baked entry-point args"; also resolves `${self.installPath}` and
  `:native`/`:posix`, and now **refuses** with 65 rather than passing an unknown
  token through.
- **C14 [Medium]** `reference/metadata.md:227` — the `[`ocx env`][cmd-env]` link
  resolves to the **package-tier** `ocx package env` (command-line.md:676). The
  root toolchain-tier command is at :582 (`#env-root`), and `[cmd-env-root]` is
  already defined at metadata.md:943.

---

## WP-D — AI config + design record

- **D1 [Warn]** `.claude/rules/subsystem-package.md:37` and `:114-119` — both
  describe the pre-D8 world: the signature is now `resolve(var, self_env)` plus a
  new `resolve_without_emit_assertions`, and "the composer calls this per-var
  unconditionally **after** surface-gating" was inverted by D8 to
  resolve-then-gate (composer.rs:123-138). The ADR's own Documentation & Schema
  Surfaces section required these rows in this commit.
- **D2 [Suggest]** `.claude/rules/subsystem-package.md:27` — `bin_scan` row still
  says "`${installPath}`-rooted", which D4 widened to the alias (the code doc at
  `bin_scan.rs:32-38` was updated, the rule row was not).
- **D3 [Medium]** `.claude/rules/subsystem-package-manager.md:46` — the
  `composer.rs` row describes neither resolve-then-gate nor the self-env
  accumulator. File untouched by the diff.
- **D4 [Suggest]** `crates/ocx_cli/src/command/deps.rs:297` — comment still calls
  the gate "tampered or invalid metadata"; after D14 it is only "structurally
  unreadable". The test directly below it was already rewritten to say exactly
  that.
- **D5 [Warn]** ADR D10 (`adr_interpolation_token_grammar.md:670-671`) says a
  modifier-bearing segment lands in `unresolvable` and names
  `UnresolvableScanScope`. What shipped is a **third** outcome:
  `ScanScope.modifier_bearing` + `LibcLintError::ModifierBearingScanScope`
  (`libc_lint.rs:149-152,211,374,682`). Exit code unchanged (65), so behaviour is
  safe, but the design record does not have it. Add the sentence recording the
  two-list split and the second variant, and mark the WP3 deferred item
  ("UnresolvableScanScope names the segment but not the reason") delivered.
  (Convergence gap: D10 `contradicts`.)
- **D6 [Warn]** ADR C-034 mandates "one proptest: for arbitrary UTF-8 s". No
  proptest exists anywhere (0 matches in every Cargo.toml and Cargo.lock). What
  shipped enumerates all strings over `['$','{','}','\\','/','a']` to length 4
  plus six non-ASCII clusters — which does answer the ADR's stated worry, and is
  arguably stronger, but the swap is recorded nowhere. Amend C-034 to record the
  enumerated form and why it is at least as strong. (Convergence gap: C-034
  `contradicts`.)
- **D7 [Suggest]** ADR front matter is still `**Status:** Proposed` with the whole
  design implemented and merged. Repo precedent exists for both `Accepted` and
  `Accepted (post-hoc — implementation already merged)`.

---

## WP-E — Commit subjects (applied during the squash, not as edits)

`cliff.toml` renders one bullet per commit from the **subject alone**, and the
project forbids migration prose in user docs — so these strings are the entire
break announcement.

- **E1 [Warn]** `3ff52fc4` — ``feat(metadata)!: every `${…}` in package metadata
  follows one grammar`` describes the feature, not the break. Replace with:
  ``feat(metadata)!: an unrecognised ${…} in package metadata is refused, exit 65 — escape it as $${``
- **E2 [Suggest]** `92afe4ee` — `feat(metadata): render interpolation token paths
  as native or posix` is not greppable in release notes. Replace with:
  ``feat(metadata): an interpolation token can render its path with :native or :posix``
- **E3 [Warn]** `b4770c3e` `merge: WP1 render seam` is a **merge commit whose
  subject matches no cliff parser**, so it renders a bogus `### Merge` section
  with `- WP1 render seam` as a release-note bullet. The squash removes it.
  (`rtk`'s `git log --oneline` silently omitted this commit — always verify branch
  history through `rtk proxy`.)

---

## Deferred — human judgement required

- **D-1** The numeric expansion budget in A1, and per-value vs
  per-package-cumulative. A budget can refuse documents already in registries,
  against "already-published packages must keep resolving on the read path".
  Shipping with a constant; the owner moves the number.
- **D-2** `package_push.rs:194,208` — the publish gate leaves no type-level
  evidence: `validate_for_publish` returns the same `ValidMetadata` every read
  path constructs, erased with `.into()` before `Publisher`. "Was this
  token-checked?" is a call-order convention in two CLI files — the exact defect
  the ADR's Constitution Gate records having already occurred once. Extract the
  four-step create/push compile pipeline into an `ocx_lib` entry point (which also
  answers the lib-hosts-orchestration doctrine), or accept the convention?
- **D-3** `bin_scan.rs:94` vs `libc_lint.rs:374` take opposite policies on the
  identical `None` from `classify_install_path_rooted_dir`: one silently degrades
  a binaries claim, the other refuses the publish. Agreeing means either loosening
  a security-adjacent lint or turning a best-effort claim into a publish blocker
  on macOS and Windows.
- **D-4** `package_create.rs:126-143` — `resolve_binaries` runs before
  `validate_for_publish`. Verified to produce no wrong outcome, message quality
  only. Accept permanently, or reorder despite `resolve_binaries` mutating the
  metadata the gate then validates?
- **D-5** `template.rs:115` — is "a modifier-bearing `PATH` value is out of scan
  scope" a deliberate contract or an accident of the classifier's exact-shape
  match? `${installPath:posix}/bin` and `${installPath}/bin` name the same
  directory. Accepting the modifier would make `ModifierBearingScanScope` dead
  code. The ADR does not address modifier × scan-scope at all.
- **D-6** `tasks/pull_local.rs:1150-1213` —
  `pull_coordinator_coalesces_concurrent_same_digest_writers` failed once in two
  full `cargo test -p ocx_lib --lib` runs (delta=4, expected 1), passed 3/3 in
  isolation. The global `WRITE_BLOB_CALL_COUNT` is guarded by an opt-in lock only
  some callers take. **Outside `main...HEAD`.** Fix here (scope creep) or file it?
- **D-7** `scanner.rs:644-648` — a wall-clock `elapsed < 2s` budget over a 512 KiB
  input runs on every `cargo test`. Accept on the slowest CI runner, or move the
  timing half behind `#[ignore]` and keep the byte-identity assertion?
- **D-8** An `Env.variables` count cap mirroring `MAX_DEPENDENCIES`. Defence in
  depth only (256 vars still permits 2²⁵⁵ without A1), and it risks refusing
  already-published documents.
- **D-9** Pre-existing adjacent drift, not this diff: `docs-style.md:64` wants
  analogies in a `:::info` callout (metadata.md:122-126, env-composition.md:135);
  `roadmap.md:30` still marks "Path interpolation" planned against ocx-sh/ocx#32,
  which is closed.

---

## Convergence

- `C-021` **partial** — publish-gate `AmbiguousSelfEnvRef` leg implemented, untested (→ B1)
- `C-034` **contradicts** — enumerated corpus shipped where a proptest was mandated (→ D6)
- `D10` **contradicts** — `modifier_bearing` split + second error variant absent from the design record (→ D5)

64/66 contract + scenario IDs satisfied (all 39 C-### and all 27 S-### checked
against code, not against the plan's own coverage table).

## Cross-model adversarial pass

**Deferred to the final round** — not skipped. Running the one-shot `codex:rescue`
gate against a diff that is about to be rewritten by these fixes would spend the
pass on a superseded artifact. It gates the diff that actually ships.
