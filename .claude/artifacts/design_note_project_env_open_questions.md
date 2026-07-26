# Design Note: Three Open Questions from `adr_project_env_declaration.md`

<!--
Decisions-only design note. Settles the two questions the ADR defers (C1's CI-surface
uniformity claim, R1's launcher re-entry) and verifies one H1-adjacent claim.
The ADR's ratified decisions are NOT re-opened here.
-->

## Metadata

**Status:** Proposed
**Date:** 2026-07-25
**Deciders:** architect
**Settles:** `adr_project_env_declaration.md` C1 (order note), R1, and the "verify before assuming" flag under Consequences → Additional Rust surface
**Verification constraint:** the workspace does not compile on `goat` (`crates/ocx_lib/src/oci/client/builder.rs:136` needs `ClientConfig::dns_resolver`, present only on the unmerged submodule branch `ocx/ssrf-resolver-seam`). **Every finding below is derived from source reading, not from executed code.** Claims that would need a run to confirm are marked explicitly.

---

## Q-A — Does `export_ci` preserve entry-vector order?

### Finding 1: bucketing reorders, and the reorder is only *accidentally* harmless

`GitHubFlavor` sorts each entry into one of three buckets at `write_entry` time and drains them in fixed bucket order at flush, independent of vector position (`github_flavor.rs:90-114` buffering, `:66-76` draining):

1. `path_entries` — `Path` kind with key `PATH` → `$GITHUB_PATH`
2. `buffered_paths` — `Path` kind, any other key → `$GITHUB_ENV`
3. `buffered_constants` — `Constant` kind → `$GITHUB_ENV`

`GitLabFlavor` has the same two-bucket split with no path channel (`gitlab_flavor.rs:112-127` buffering, `:87-93` draining — paths pushed into `lines` before constants).

So a `Constant` appended *after* a `Path` on the same key does **not** land in vector position: within `$GITHUB_ENV` every path line precedes every constant line. The *effective* result is nonetheless correct for that direction, because constants are drained last and a later `KEY=value` line overrides an earlier one for the same key.

> **External assumption, not verified in-repo:** that the GitHub runner and the GitLab step-runner apply duplicate keys last-wins. Load-bearing **only** for the mixed-kind case (same key with both a `Path` and a `Constant` entry) — each bucket is an `IndexMap` keyed by name, so within a bucket there is exactly one line per key.

The reverse direction is genuinely wrong: a `Path` entry appended *after* a `Constant` on the same key is clobbered, because the constant line is written last. `Env::apply_entries` would have prepended onto the constant's value (`env.rs:369-377` → `add_path`, `:265-273`). This is pre-existing (two packages, one declaring `FOO` constant and one `FOO` path, already hit it) and project `[env]` only adds a new way to reach it.

### Finding 2: relative precedence among multiple path entries on one key is **reversed** — this is the real problem

- `Env::add_path` → `utility::path::move_to_front` puts the **newest** value at the front (`utility/path.rs:43-65`, tests `:89-97`). Later entry ⇒ higher precedence.
- `ci::prepend_existing` preserves buffered accumulation order — **first** buffered value stays at the front (`ci.rs:71-80`, test `prepend_existing_new_values_precede_existing` at `:142-147`). Earlier entry ⇒ higher precedence.

Concrete, for entries `[Path(LD_LIBRARY_PATH,/pkg1), Path(…,/pkg2), Path(…,/proj)]` with ambient `/existing`:

| Surface | Result |
|---|---|
| `ocx run` (`apply_entries`) | `/proj:/pkg2:/pkg1:/existing` — project first |
| `ocx env --ci=github` (non-`PATH` key) | `/pkg1:/pkg2:/proj:/existing` — project **last** |
| `ocx env --ci=gitlab` (**including `PATH`**) | `/pkg1:/pkg2:/proj:/existing` — project **last** |

The one channel that agrees is GitHub's `$GITHUB_PATH`: `path_entries` is a `Vec` drained in order with keep-last dedup (`github_flavor.rs:65-69`), and the runner prepends each line as it reads (documented at `:59-64`), so the last line has the highest precedence — matching `move_to_front`. GitLab has no such channel, so GitLab's `PATH` goes through `buffered_paths` and inherits the reversal.

### Finding 3: order is an observable contract, confirmed

`test/tests/test_toolchain_env.py:1152` asserts `keys == ["PATH", home["default"], "PATH", home["lint"]]` and `:1169-1176` the three-group equivalent, both exact-list. Helpers at `:1072` / `:1096`. Append position is a semantic choice, as the ADR states.

### Decision A1 — C1's "uniform by construction" claim is **downgraded for the CI surface**; C1 itself stands

Appending project/group entries to the `Vec<Entry>` remains the right implementation and stays ratified: it is the smallest diff, and it is exactly correct for `apply_entries`, `emit_lines`, and GitHub's `$GITHUB_PATH`. But the claim that all consuming surfaces are uniform *by construction* is false for two of the four channels. **The ADR text must be corrected**, not the design.

### Decision A2 — the plan must fix `prepend_existing`'s direction, not work around it

`ci::prepend_existing` is a single 10-line shared function feeding both flavors (`ci.rs:71-80`). Reverse the buffered-value iteration so the last-buffered value lands at the front, matching `move_to_front`. That makes `ocx env --ci=*` agree with `ocx run` for every path key on both providers, and it makes C2's "a stage-4 path entry lands ahead of stage-2 package paths" true on every surface rather than three out of four.

Scope note: this is a **behavior change to an existing shipped surface** (package-vs-package path ordering flips on `--ci`). It is a bug fix — the two surfaces disagree today and one of them must be wrong — but it belongs in its own commit with its own test, ahead of the project-`[env]` work, so the project-env diff is not carrying an unrelated correction. The existing tests that pin the current direction (`ci.rs:142-147`, `github_flavor.rs:348-367`, `:370-392`, `gitlab_flavor.rs:227-248`) must be flipped deliberately, not patched until green.

### Decision A3 — mixed-kind on one key stays divergent, and that is accepted for v1

The `Path`-after-`Constant` clobber (Finding 1) is not worth closing: it needs the CI flavors to keep per-key kind history and interleave two output channels, and it is unreachable from project `[env]` alone (X1 plus a single `[env]` table cannot produce two kinds for one key — only a package/project cross-kind collision can). Record it in the ADR's C3/C4 "pre-existing gap, documented not fixed" paragraph rather than opening a work package.

---

## Q-B — R1, launcher re-entry

### Finding 4: the ADR's characterization of the gap is wrong in both directions

The ADR says project `[env]` "will not reach a tool invoked through a generated entrypoint launcher" and that "the gap is narrow: a launcher invoked from a shell where neither `ocx run` nor direnv activation happened."

Tracing the three paths:

**Path 1 — `ocx run -- foo`.** A package that declares entrypoints gets a synthetic `entrypoints/` PATH entry pushed *after* its declared `bin/` entry, so it lands at the **front** of the child's PATH and shadows `bin/` (`composer.rs:625-633` for deps, `:665-672` for the root; the invariant is spelled out at `:641-653` and pinned by `test_synthetic_entrypoints_path_emitted_after_declared_bin`). So `foo` resolves to the launcher, and the launcher re-enters `ocx launcher exec`. **This is the primary path, not a corner case.**

Inside the re-entry, `run_with_env` does `Env::new()` then `apply_entries` (`launcher/exec.rs:165-166`). `Env::new()` seeds from `std::env::vars_os()` (`env.rs:242-246`) — which under `ocx run` **already carries the parent's project `[env]`**. Then `apply_entries` re-applies the package's own entries on top (`entries` from `resolve_env` at `:90-96`, package-only by construction).

Consequence: a project constant whose key the package also declares is **silently reverted to the package's value**. A project `PATH` entry survives but loses its front position, because the package re-prepends its `entrypoints/` and `bin/`.

**Path 2 — bare `foo` from a direnv-activated shell.** Identical to Path 1: `direnv export` put the project entries in the shell, `Env::new()` inherits them, `apply_entries` re-applies the package on top. The ADR's option-1 rationale ("direnv has already put the variables in the shell, so the launcher inherits them anyway") is right about inheritance and wrong about ordering — inheritance happens *before* the package re-application, not after.

**Path 3 — bare `foo` from a plain shell.** Nothing put a project tool on PATH, so the launcher is only reachable for a globally-installed tool, where project env *should* be absent. **Not a gap.** The one case the ADR names as the gap is the one case where the current behavior is correct.

### Finding 5: the divergence is silent, and it hits exactly the keys the feature exists for

C4 states that project-over-package shadowing is "the *declared intent* of the feature." Under a launcher, that intent is inverted with no signal — no warning, no log, no exit code. The keys most likely to collide (`JAVA_HOME`, `GOROOT`, `PATH`) are precisely the ones a project would override.

The same hole applies to `--env` (L1, stage 6, the *highest*-precedence stage and an explicit per-invocation instruction). `ocx run --env JAVA_HOME=/custom -- foo` silently loses to the package's `JAVA_HOME` whenever `foo` is a launcher.

### Decision B1 — **overturn the ADR's option-1 recommendation. Build option 2 in v1.**

Option 1 is not "accept a narrow documented asymmetry"; it is "ship a feature whose primary use case silently does not work on the primary invocation path." Option 3 stays rejected for the reason the ADR gives — `apply_ocx_config`'s contract is resolution-affecting configuration, and project env is not that.

Forward a new `OCX_ENV` key carrying stages 4-6 (project `[env]`, group `[env]`, `--env`) as a serialized `Vec<Entry>`. `launcher/exec.rs` decodes it beside the existing `patches_from_env()` call (`:86-89`) and appends the decoded entries **after** `resolve_env`'s package entries, restoring stage-4-after-stage-2. `Entry` is `{key, value, kind}` (`package/metadata/env/entry.rs`) and serializes cleanly — no new wire vocabulary.

There is no cheaper correct option. A key-list-only forward does not work: re-prepending a path segment requires the value, since the launcher cannot identify the project's segment inside an inherited `PATH`. Suppressing the launcher's own re-resolution is not available either — the launcher must work when nothing composed (direct `ocx package exec`, plain shell).

### Decision B2 — decode-side gating is mandatory and must fail closed

`patches_from_env` is the right structural template but its *posture* is only partly adequate as a model:

- Adequate: it fail-closes on a missing/non-string `registry` on the reasoning that such a value "was not produced by `encode_patches` (i.e. externally injected / corrupted)" (`config/patch.rs:344-356`).
- Not adequate to copy verbatim: it applies **no content validation** to `no_patches` (`:375-379`) — an arbitrary string array flows straight into the repo-key/digest matcher. That is tolerable there because the worst a forged value does is *suppress* an overlay, and `system_required` (`:370`) guards the case that matters. `OCX_ENV` has no such backstop.

Required on decode, in `launcher/exec.rs`'s decoder:

1. **X1** — reject `OCX_*` / `__OCX_*` keys. Not defense-in-depth: `apply_ocx_config` runs *after* `apply_entries` (`launcher/exec.rs:166-168`) and only overwrites the keys it knows (`env.rs:303-363`), so a forged `OCX_DEFAULT_REGISTRY` — which `apply_ocx_config` does not touch — survives into the child and reaches any grandchild `ocx`. That is the escalation seam.
2. **X2** — every key through the shared `env::is_valid_env_key` (`env.rs:560-570`), same gate as the parse path. Do not write a second validator.
3. **Fail closed on any violation** — discard the whole payload with a `warn`, do not silently skip the offending entry. A conforming producer can never emit an `OCX_*` key (X1 already rejected it at parse), so any occurrence means forgery or corruption, and partial application of a tampered payload is the worse outcome. Same size of code either way.

Threat-model framing, stated so it is not overclaimed: a process that can set `OCX_ENV` on a child can already set that child's environment directly. The wire key grants **no new capability** in the ancestor-sets-it model — this is the same reasoning that makes `OCX_PATCHES` acceptable today. The gating exists for exactly one thing: keeping the forwarded map out of `ocx`'s own resolution surface.

### Decision B3 — stale-value discipline

`apply_ocx_config` must **remove** `OCX_ENV` when no project env is in scope, mirroring its existing set-or-remove branches for `OCX_CONFIG` / `OCX_INDEX` and the reason its doc comment gives (`env.rs:294-298`): "a stale parent-shell export cannot beat the outer ocx's parsed state." Otherwise a user who runs `ocx run -- bash` in project A carries A's `OCX_ENV` into every later launcher invocation in that shell.

This is a `remove` branch only. The field does **not** join `OcxConfigView` — that would be option 3, which stays rejected.

### Migration consequence for the plan

1. **R1 and L1 are coupled.** `--env` is stage 6 of the forwarded payload, so the `--env` flag and the `OCX_ENV` wire key cannot land in separate waves. One work package.
2. **One new work package** beyond the ADR's inventory: encode in `run.rs` + decode/gate/apply in `launcher/exec.rs` + the `apply_ocx_config` remove branch.
3. **Two new verification items:**
   - A project `[env]` constant that shadows a package constant survives through a launcher re-entry — the direct regression test for Finding 4. Fails today.
   - A forged `OCX_ENV` carrying an `OCX_*` key is discarded whole, and the child sees neither that key nor the payload's other entries.
4. **One new documentation surface:** `website/src/docs/reference/environment.md` gains `OCX_ENV` (forwarded, not user-settable), alongside the existing `OCX_PATCHES` row.
5. The ADR's R1 section, its Consequences "Positive" bullet about C1 uniformity, and the Q3 `$GITHUB_ENV` reasoning all need editing to match Decisions A1/A2/B1.

---

## Q-C — H1's "no second canonicalization path"

### Finding 6: the ADR's `lock.rs` citation does not describe a code path

`project/lock.rs` contains **no** reference to `ProjectConfig.groups`. Grepping `groups|declaration_hash` across the file returns only the hash *string* field (`:112-117`), the version gate (`:337-342`), test fixtures, and one doc-comment mention of "duplicate-across-selected-groups" at `:180`. The three cited lines are not walks:

- `:153` and `:167` are **doc comments** on `LockedTool` ("the named `[group.<name>]` key").
- `:1229` is a **comment inside the test** `tools_written_sorted_by_name_then_group` (`:1216-1252`).

`LockedTool.group` is a plain `String` label on a flat `Vec<LockedTool>` (`:163-175`) — the lock file's own per-tool group column, never a traversal of the config's group map.

`declaration_hash` is computed in exactly one place, `hash.rs:50-86`, and memoized behind a `OnceLock` on `ProjectConfig` (`config.rs:194-197`). Its callers are `hook.rs:100`, `config.rs:196`, and tests in `resolve.rs` / `hash.rs`. **`lock.rs` never computes it — it only stores and version-gates the string.**

### Decision C1 — H1 stands unchanged; no `DECLARATION_HASH_VERSION` bump

There is no second canonicalization. The ADR's "verify before assuming" flag resolves clean, and the sentence citing `lock.rs:153,167,1229` as a group-content walk should be **struck** from the Consequences section — it points at doc comments and a test.

### Two carry-overs the plan should inherit

1. **`hash.rs:70-75` is a required edit under S2.** It iterates `config.groups` as `(&String, String)` pairs, which only typechecks while the group value is a bare `BTreeMap<String, Identifier>`. Under `Group { tools, env }` it becomes `group.tools.iter()`. One line — but the frozen-corpus test `hash_corpus_case_3_multi_tools_and_groups` (`hash.rs:217-237`) already pins that the emitted JSON is byte-identical, so **no new "reshape does not change the hash" test is needed**; the corpus is the guard, provided the test helper is updated to build the new struct with the same identifiers. The ADR's Verification bullet "`declaration_hash` is unchanged by a group's TOML reshape" is therefore already covered.
2. **`config.rs:199-204`'s cache-invalidation obligation does not grow.** In-place mutators of `tools`/`groups` must call `invalidate_declaration_hash_cache`. Since `[env]` is excluded from the hash by H1, an `[env]` mutator has nothing to invalidate — but a future reviewer wiring `[env]` into the hash would silently break the cache as well as H1. The ADR's proposed `declaration_hash_unchanged_by_env` regression test guards both; keep it.

---

## Summary of decisions

| ID | Decision |
|---|---|
| **A1** | C1's entry-vector append stays. Its "uniform by construction" claim is corrected: two of four channels diverge. |
| **A2** | Fix `ci::prepend_existing`'s ordering direction (separate commit, ahead of the project-env work). Flip the four tests that pin the current direction deliberately. |
| **A3** | `Path`-after-`Constant` on one key stays divergent on the CI surface. Documented, not fixed. |
| **B1** | **Overturn option 1.** Build option 2 (`OCX_ENV` forwarding) in v1. Option 3 stays rejected. |
| **B2** | Decode-side X1 + X2, failing closed on the whole payload. `patches_from_env`'s `registry` posture is the template; its `no_patches` leniency is not. |
| **B3** | `apply_ocx_config` gains an `OCX_ENV` **remove** branch. The field does not join `OcxConfigView`. |
| **C1** | H1 stands. No `DECLARATION_HASH_VERSION` bump. Strike the `lock.rs:153,167,1229` sentence from the ADR. |
