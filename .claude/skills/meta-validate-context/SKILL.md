---
name: meta-validate-context
description: Verify subsystem context rules match current codebase state. Use to audit freshness of .claude/rules/subsystem-*.md files.
user-invocable: true
argument-hint: "all | subsystem-name"
---

# Validate Context Rules

Checks that `.claude/rules/subsystem-*.md` files accurately describe the current codebase.

## Workflow

For each `subsystem-*.md` rule file:

1. **Read the rule** — Extract mentioned type names, module paths, function names, error variants
2. **Grep the codebase** — Verify each reference still exists
3. **Check for new additions** — Look for new public types/modules not mentioned in the rule
4. **Report** — List stale references and missing additions

## Subsystem Rules to Check

| Rule | Key References to Verify |
|------|------------------------|
| `subsystem-oci.md` | `IndexImpl`, `SelectResult`, `RemoteIndex`, `LocalIndex`, `Identifier`, `Digest`, `Platform`, `Client` |
| `subsystem-file-structure.md` | `FileStructure`, `ObjectStore`, `ObjectDir`, `IndexStore`, `InstallStore`, `SymlinkKind`, `TempStore`, `ReferenceManager` |
| `subsystem-package.md` | `Metadata`, `Bundle`, `Env`, `Var`, `Modifier`, `Exporter`, `BundleBuilder`, `Version`, `Tag` |
| `subsystem-package-manager.md` | `PackageManager`, `PackageError`, `PackageErrorKind` variants (NotFound, SelectionAmbiguous, etc.), task method names |
| `subsystem-cli.md` | `Context`, `Command` enum variants, `Printable` trait, `Api` struct |
| `subsystem-mirror.md` | `MirrorSpec`, `Source` variants, `MirrorTask`, `MirrorResult`, pipeline modules |
| `subsystem-tests.md` | Fixture names in conftest.py, test file names, `OcxRunner` methods, `make_package` params |
| `subsystem-website.md` | Vue component names/props in `theme/index.mts`, VitePress config, task commands, generated content paths |

## Verification Commands

```bash
# Check if a type still exists
grep -r "pub struct TypeName" crates/
grep -r "pub enum TypeName" crates/

# Check if a module still exists
ls crates/ocx_lib/src/module_name/

# Check for new public types not in the rule
grep -rn "^pub struct\|^pub enum\|^pub trait" crates/ocx_lib/src/subsystem/ | grep -v test
```

## Output Format

```markdown
## Context Validation Report

### subsystem-oci.md
- OK: IndexImpl, SelectResult, RemoteIndex, ...
- STALE: [type] — renamed to [new_name] or removed
- MISSING: [new_type] — not documented in rule

### subsystem-file-structure.md
...
```

## When to Run

- After large refactors
- Before major feature branches
- As part of `/code-check` audits
- Periodically (monthly)
