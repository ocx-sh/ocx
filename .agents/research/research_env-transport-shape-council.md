# Research: council on the shape of ocx-to-ocx transported state

## Metadata

**Date:** 2026-11-06
**Domain:** cli
**Triggered by:** discussion `.agents/discussions/exec-env-model.md` (council lane, 3 seats: premortem, operability, simplicity; blind to each other and to any stated preference)
**Expires:** 2027-02-06 (re-run if the inventory changes)

Question put to the seats: how should OCX structure the state one ocx process passes to a descendant ocx process?
- A: one opaque, version-stamped internal variable; public `OCX_*` are user inputs only.
- B: a small fixed set of named internal variables, each with a declared inheritance class enforced in one place.
- C: today's shape (~26 forwarded public `OCX_*` plus `__OCX_*`) with registry and docs corrected.

Inputs: `.agents/research/research_ocx-exec-env-transport-inventory.md`, `.agents/research/research_nested-env-state-prior-art.md`.

## Direct Answer

All three seats chose **B, built on A's invariant**: ocx never writes a public `OCX_*` variable into a child. Confidence: premortem about 65%, operability about 70%, simplicity about 70%.

Synthesis — B with A's invariant has three classes:
1. One opaque, version-stamped exec carrier for all ocx-to-ocx exec state. Same-version only.
2. The hook ledger, kept separate, with its existing cross-version revert-read promise.
3. A plain, raw-readable binary-pin (and home) channel for the sh trampoline and Windows shim.

One projection function enforces the classes. One diagnostic dumps them. One table-driven test pins every variable's class behaviour.

## Key Findings

### Agreements (all three seats)

- **Pure A is unreachable.** The sh trampoline (`${OCX_BINARY_PIN:-…}`), the Windows shim (raw `OCX_HOME`/`OCX_BINARY_PIN`, size budget) and the generated hook (ledger presence) read state before any ocx runs and cannot decode a blob. A therefore collapses into B plus a blob.
- **The ledger must stay separate.** It must survive an ocx upgrade in a live shell (ledger.rs rule: ship a `v-1` revert-read arm or live shells lose their revert set). Folding it into a same-version-only carrier breaks that.
- **C keeps the confirmed leak source.** Public variables used as transport caused the incidents, and declarations nobody enforces drift again. Five written contracts already contradict each other.
- **B's real simplification is one enforcement seam.** A new spawn path needs one rule, and a leak has one place to appear. Today there are about 7 scrub sites.

### Divergence

The seats split on the "compatible within one ocx version" requirement:
- **Premortem:** reject it as written. Its failure story for A: a long-lived pane outlives an upgrade, the new ocx decodes the old blob as absent, and a build silently runs the wrong toolset. Every internal variable should fail closed on mismatch.
- **Operability:** narrow it. The ledger is an explicit cross-version exception, and every other variable fails closed and is ignorable. Opaque blobs hurt 2 a.m. debugging unless a same-version decode command exists. Watch execve's 128 KiB and Windows' 32,767-character limits.
- **Simplicity:** it holds for the exec carrier if a mismatch decodes as absent. "Absent" fails open on behaviour: a pane silently loses outer offline, mirrors or patches. It fails closed on security: dropping `OCX_NO_VERIFY` is stricter. What "absent" means needs a deliberate decision.

### Costs named

- A dump/inspect command for the opaque carrier.
- A decision on mismatch semantics.
- `OCX_HOME` absolutization is itself a write and needs a deliberate class.
- Mixed-version trees: a pinned project ocx and a global ocx in one tree. [A: the binary pin normally keeps one binary per tree; not verified by the seats.]

### What would change the verdict

- Premortem: if the live-descendant-across-upgrade case is out of scope, A's main failure disappears.
- Simplicity: whether flag-to-env forwarding can move into the carrier without behaviour change needs a leaf-`env` diff on a built binary.

## negative

No seat executed a scenario. All traces rest on the inventory's [I]-graded nesting findings.

## leads

- Leaf-`env` diff of the nesting variants on a built main binary.
- Windows shim byte budget.
- Mixed-version tree reachability given `OCX_BINARY_PIN`.

## Recommendation

Synthesis: B with A's invariant, in the three classes above. A mismatch of the exec carrier needs an explicit decision.

## Sources

The two input artifacts above. Plus https://github.com/jdx/mise/pull/9765, https://docs.rs/crate/mise/latest/source/src/hook_env.rs, https://github.com/direnv/direnv/wiki/Tmux.
