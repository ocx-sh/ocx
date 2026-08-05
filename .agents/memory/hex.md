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
