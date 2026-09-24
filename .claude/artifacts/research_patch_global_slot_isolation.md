# Research: isolating the `patch_global_slot` group

- **Question:** can the acceptance tests that share `@pytest.mark.xdist_group("patch_global_slot")` be isolated per test, so the group and its serialisation go away?
- **Why it matters:** the group serialises about 53 of the 56 s of the verb-edit T1's scoped pytest, so that T1 stays above 120 s (148.4 s) after every build cut. In the concurrent Bazel suite, its four member modules hold one host slot lock in turn. `test_patches` was measured waiting about 93 s.
- **Date:** 2026-09-24, speed-up workstream (`hex/test-speed-tiers--speed`).

## Finding: this is test scaffolding, not a product property

- **The shared state.** The shared state is the global patch descriptor, `<patches.registry>/global:__ocx.patch` (`crates/ocx_package_manager/src/tasks/patch_discovery.rs:985-995`).
- **The product already supports a per-test slot.** `patch_registry_identifier` (`patch_discovery.rs:942-957`) accepts a path prefix on `patches.registry`, so `localhost:5000/<uuid>_patches` gets a slot of its own. `test/tests/test_patch_smoke.py:70` already works this way and runs with no group. `test/lint/test_patch_global_slot.py:29-33,115-116` exempts any `--registry` value that contains a `/`.
- **Where the collision comes from.** The members write `[patches] registry = <bare registry fixture>`. `test_patches.py:92-101` `_write_config` is called that way about 55 times, and there are about 12 `--global` publishes. `test_managed_config.py:318` and `test_frozen.py:511` do the same, and so does `patches__consumer.sh` in `test_doc_scripts`.

## Why it is not fixed here

- **The diff guard blocks the edit.** The fix rewrites existing test bodies and fixtures under `test/tests/`. `scripts/test_diff_guard.py` freezes those files (crate-split DEC-10) and none of its `--tiered-shapes` exceptions covers this edit. Lifting the freeze for it is an owner decision; see `plan_test_speed_tiers.md` § Deferred owner decisions, B2.
- **No harness-level route exists.**
  - `OcxRunner` forwards only a fixed set of environment variables (`test/src/runner.py:70-86`), and no environment variable overrides `patches.registry`.
  - A path-prefixed `REGISTRY` breaks `registry_dir` and the insecure-host checks.
  - A second registry needs an edit to the frozen `docker-compose.yml`. Under xdist, the group would still pin every member to one worker.

## Design, once the owner lifts the freeze for it

1. In `test_patches.py`, add a function-scoped `patch_registry` fixture that returns `f"{registry}/{uuid4().hex}_patches"`. Use it at every call that currently passes the bare registry. Delete `pytestmark` (`:55`) and the cleanup fixture (`:59-90`).
2. Make the same change in `test_frozen.py` (`:426-628`) and `test_managed_config.py` (`:263`, `:318`). Remove `patches__consumer.sh` from `SHARED_SLOT_GROUPS` (`test_doc_scripts.py:75`).
3. Nothing else changes. `test/BUILD.bazel` `module_slots` shrinks, and `scripts/bazel_tag_guard.py` checks it against source. The lint already exempts path-scoped registries.
4. The proof is one new test, which the guard admits today. It publishes two different `match: "*"` global descriptors under prefixes P1 and P2 of one registry, then asserts that each environment resolves only its own companion. Pointing both at the bare registry turns it red deterministically, because the second publish overwrites the first.

## Side finding: two readers outside the group

`test_execution_records.py:2308` and `test_execution_record_standards.py:248` read the bare global slot without joining the group. The lint's own recorded incident (`test_patch_global_slot.py:95-103`) says a reader can be poisoned by a member's `match: "*"` rule. The claim at `test_patches.py:51-53` that readers are harmless is therefore wrong. The per-test design above removes this hazard too. Until it lands, the hazard is the same under xdist and under the concurrent Bazel suite.

## Decision

Keep the group; the lock is correct as it stands. File the migration above as an issue that the owner must unblock through the diff guard. Expected gain: verb T1 about 148 s → about 95-100 s (the scoped pytest spreads across workers), and the member modules no longer serialise in the concurrent suite.

## Outcome (refine pass, 2026-09-24)

The Decision above was superseded: the migration landed on `hex/test-speed-tiers` through reviewed one-line re-points, each admitted by a diff-guard `--allow`.
- Every patch tier other than the group's now names a path of its own under the registry. `test_patches.py` and `test_frozen.py` use `<registry>/p<uuid4>`. `test_managed_config.py`, `test_execution_records.py` and `test_execution_record_standards.py` use `<registry>/<unique repo>_patches`. The `patches-consumer` and `patches-maintainer` cast providers use `<registry>/<prefix>patches`. So a `--global` publish writes `<tier path>/global`, not the bare host's `global` repository.
- `test/lint/test_patch_global_slot.py` judges every patch tier per function. A tier pointed back at the bare registry fails the lint unless that test is in the group.
- The group stays in `test_patches.py` alone (`module_slots`). It guards one real writer, `test_global_descriptor_publishes_at_the_bare_registry_root`: that test publishes `--global` at the bare host's reserved single-segment `global` repository, the shape registry:2 once rejected. The group also holds `test_patch_publish_without_config_errors`, which has no tier to scope.
- The two readers named under "Side finding" read their own tiers now, so the hazard is closed.

## Draft issue

> **test: give each patch acceptance test its own `patches.registry` slot**
>
> The `patch_global_slot` xdist group serialises every test that publishes `--global` into the bare test registry. That caps the verb-edit T1 at about 148 s (budget: 120 s) and serialises four modules in the concurrent Bazel suite. `patches.registry` already accepts a path prefix (`patch_discovery.rs:942-957`), and `test_patch_smoke.py` already runs isolated this way. Migrate `test_patches.py`, `test_frozen.py`, `test_managed_config.py` and `patches__consumer.sh` to a per-test `<registry>/<uuid>_patches`, drop the group, and add the P1/P2 collision test as the red/green proof. This also closes two unguarded readers: `test_execution_records.py:2308` and `test_execution_record_standards.py:248`. It needs a sanctioned `scripts/test_diff_guard.py` exception for existing test bodies. See `.claude/artifacts/research_patch_global_slot_isolation.md`.
