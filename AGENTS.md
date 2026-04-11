# OCX — Agent Entry Point

This is the cross-vendor entry point for AI agents that run **outside** the
Claude Code harness (e.g., Codex CLI, other `AGENTS.md`-aware tools).

**If you are Claude Code, stop reading this file** — use `CLAUDE.md`
instead. It is the authoritative project context and is auto-loaded by the
harness. `AGENTS.md` is a thin pointer that exists only so non-Claude
agents can find the same information without a second copy to maintain.

## Where the real context lives

Read these files in order. They are the **single source of truth** — do
not rely on anything summarized in `AGENTS.md` alone.

1. **`CLAUDE.md`** (repo root) — what OCX is, tech stack, architecture,
   workflow, commit conventions, worktree layout, "never push to remote"
   rule. Start here.
2. **`.claude/rules/product-tech-strategy.md`** — golden-path technology choices.
   Do not suggest deviations.
3. **`.claude/rules/arch-principles.md`** — OCX-specific design
   principles, glossary, ADR index.
4. **`.claude/rules/quality-core.md`** — universal design principles
   (SOLID, DRY, YAGNI), anti-pattern severity tiers
   (Block / Warn / Suggest), refactoring discipline.
5. **`.claude/rules/quality-security.md`** — OCX-specific security attack surfaces
   and OWASP/STRIDE checklist. Consult before any security review.

## Path → subsystem rule map

When reviewing or editing code under a specific path, **read the matching
subsystem rule first** — it contains invariants, gotchas, and design
decisions that are not obvious from the code:

| Path | Subsystem rule |
|---|---|
| `crates/ocx_lib/src/oci/**` | `.claude/rules/subsystem-oci.md` |
| `crates/ocx_lib/src/file_structure/**` | `.claude/rules/subsystem-file-structure.md` |
| `crates/ocx_lib/src/package/**` | `.claude/rules/subsystem-package.md` |
| `crates/ocx_lib/src/package_manager/**` | `.claude/rules/subsystem-package-manager.md` |
| `crates/ocx_cli/src/**` | `.claude/rules/subsystem-cli.md` |
| `crates/ocx_mirror/**` | `.claude/rules/subsystem-mirror.md` |
| `test/**` | `.claude/rules/subsystem-tests.md` |
| `website/**` | `.claude/rules/subsystem-website.md` |
| `.github/workflows/**` | `.claude/rules/subsystem-ci.md` |
| `Cargo.toml`, `deny.toml`, `external/**` | `.claude/rules/subsystem-deps.md` |

## Language-level quality rules

Shareable, project-independent quality guidance is in
`.claude/rules/quality-*.md` — load the one matching the file you are
reviewing:

- `quality-rust.md`, `quality-python.md`, `quality-typescript.md`,
  `quality-bash.md`, `quality-vite.md`

## Adversarial review guidance

When Claude Code delegates an adversarial review to you (via
`/codex:adversarial-review` or `/codex-adversary`):

1. **Load `CLAUDE.md` and the subsystem rules** for the touched paths
   before flagging anything — the author was likely working against them.
2. **Challenge design choices**, not style. `cargo fmt` handles formatting;
   your job is to question whether the chosen approach is right, what
   assumptions it depends on, and where it could fail under real-world
   conditions.
3. **Watch for cross-crate coupling** — especially lifting CLI-specific
   types into `ocx_lib`, which the `crates/ocx_lib` ↔ `crates/ocx_cli`
   boundary is meant to prevent.
4. **Security attack surfaces** are enumerated in `.claude/rules/quality-security.md`.
5. **Do not critique load-bearing conventions** stated in `CLAUDE.md` or
   `product-tech-strategy.md` (Tokio, Rust 2024, OCI-backed storage, never-push-to-
   remote, commit format). They are decisions, not invitations.
6. Return findings with **concrete file paths and line numbers**, grouped
   by severity (Block / Warn / Suggest — definitions in
   `.claude/rules/quality-core.md`).

---

# General Agent Safety (applies to any external agent)

## Non-interactive shell commands

Shell commands like `cp`, `mv`, `rm` may be aliased to `-i` (interactive)
mode on some systems, which would hang an agent waiting for y/n input.
Always use non-interactive flags:

```bash
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

Also: `scp -o BatchMode=yes`, `ssh -o BatchMode=yes`, `apt-get -y`,
`HOMEBREW_NO_AUTO_UPDATE=1 brew …`.

## Session hygiene

- **Never push to remote** — the human decides when to push (CI has real cost).
- All changes must be committed locally on a feature branch.
- **Never commit directly to `main`**.
- Run `task verify` after any implementation change.
