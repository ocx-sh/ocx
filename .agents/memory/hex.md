# hex — swarm memory

Maintained by the hex skills. Small by contract: pointers and preferences,
not copies. Team-shared — commit it.

## Pointers

- Verification: `CLAUDE.md` › "Build & Development" — run `task verify`
  (full gate) after implementation; `task` = fast check. Subsystem-scoped
  gates per each `.claude/rules/subsystem-*.md` › "Quality Gate".
- Plan / ADR conventions: `CLAUDE.md` › "Workflow" — planning flow
  ADR → Design Spec → Plan; artifacts in `.claude/artifacts/` (patterns
  `adr_<topic>.md`, `design_spec_<comp>.md`, `plan_<task>.md`), templates
  in `.claude/templates/artifacts/`. Plan Status protocol:
  `.claude/rules/meta-ai-config.md` › "Plan Status Protocol" (active plans
  in `.claude/state/plans/`, pointer in `.claude/state/current_plan.md`).
- Product knowledge: `.claude/rules/product-context.md` — canonical
  identity doc (positioning, users, competitors), indexed from `CLAUDE.md`.
- Key rules: catalog `.claude/rules.md` ("By concern" table); architecture
  boundaries + ADR index `.claude/rules/arch-principles.md`.
  Security-sensitive paths: `.github/workflows/**`, `.github/actions/**`
  (`.claude/rules/quality-security.md`), `crates/ocx_lib/src/oci/**`
  (auth/SSRF/wire formats per the CLAUDE.md model policy).
- Worktrees: default `.agents/worktrees/` (gitignored, `.gitignore:50`).
- Constitution: `.claude/rules/arch-principles.md` (optional gate; plans
  checked against it when present).

## Preferences

```yaml
# hex config, vocabulary v2. Unknown keys warn once and are ignored.
models:
  fast-balanced: sonnet
  deep-reasoning: opus
  overrides:
    reviewer:quality: deep-reasoning
    reviewer:security: deep-reasoning
    reviewer:performance: deep-reasoning
    reviewer:spec: deep-reasoning
adversary: codex:rescue
perspectives:
  always:
    - role: reviewer:security
      when: "{.github/workflows/**,.github/actions/**,crates/ocx_lib/src/oci/**}"
research-axes:
  - registry ecosystems / OCI spec evolution
  - package-manager UX (mise, asdf, volta, proto)
  - shell integration mechanisms
```

- Review is never downgraded to save cost — the reviewer overrides above
  encode the CLAUDE.md "MODEL POLICY — NON-NEGOTIABLE" table.
- Fable/Mythos is the session orchestrator only, never a spawn target
  (CLAUDE.md model policy; matches models.md Rule 4).

## Memory

- **Active plan (hex-plan, 2026-08-09):**
  `.claude/state/plans/plan_interpolation_token_grammar.md` — ocx#303, tier high,
  State `plan-approved`. Design record
  `.claude/artifacts/adr_interpolation_token_grammar.md`; research
  `.claude/artifacts/research_interpolation_token_grammar.md`.
  Next: `/hex-execute .claude/state/plans/plan_interpolation_token_grammar.md`.
  Review converged at the round-3 cap: panel (spec/architect/SOTA) → 29 fixes → cross-model
  Codex gate + re-validation → 13 more → final check, one block-tier defect closed.
  **Then the owner reversed the central decision** (2026-08-09, after review): OCX claims
  every `${…}`; no foreign-token pass-through, no reserved-root set, `$${…}` the only escape.
  Reason: pass-through makes OCX's namespace hostage to other tools' vocabularies. Plus D14 —
  refusal scoped to resolve/publish, read-only paths (incl. `pull`/`install`) stay permissive.
  ADR + plan rewritten; #221 amended on GitHub. **D14 and the reversal are unreviewed** — the
  panel ran against the superseded design.
  **`.claude/state/current_plan.md` deliberately NOT repointed** — it still names
  `plan_servable_index_snapshot.md`, which is mid-execution with a dirty worktree at
  `.agents/worktrees/wp8`. Repointing would hijack that run's `/next`. The owner decides
  which plan owns the pointer.
- **Note for the next `/hex-init`:** a worker went idle without delivering its report and
  had to be pulled with `SendMessage`; treat "idle" as "not reported" and pull, do not
  assume completion.
