# Research: Lock File Formats for ocx.lock Design

**Date:** 2026-04-19  
**Purpose:** Inform `ocx.lock` schema design for project-level toolchain pinning

---

## Summary Recommendation

**Use TOML. Model the structure after mise + PDM, not pnpm or pixi.**

```toml
# [metadata] block at top — isolates fast-changing declaration_hash
[metadata]
lock_version = 1
declaration_hash = "sha256:<hex-of-ocx-toml-tools-section>"
generated_by = "ocx 0.x.y"

# [[tool]] blocks, alphabetically sorted — merge-friendly (Cargo model)
[[tool]]
name = "cmake"
version = "3.28.0"
index = "ocx.sh"

[tool.platforms.linux-amd64]
manifest_digest = "sha256:<hex>"
[tool.platforms.linux-arm64]
manifest_digest = "sha256:<hex>"
[tool.platforms.darwin-amd64]
manifest_digest = "sha256:<hex>"
[tool.platforms.darwin-arm64]
manifest_digest = "sha256:<hex>"
[tool.platforms.windows-amd64]
manifest_digest = "sha256:<hex>"
```

---

## Per-Tool Format Summaries

### Cargo.lock v4

- **Format**: TOML, flat `[[package]]` array, alphabetically sorted
- **Staleness**: Implicit re-solve when `Cargo.toml` changes (no declaration hash)
- **Per-platform**: None — source tarballs are platform-agnostic
- **Merge UX**: Excellent — independent blocks, alphabetical sort prevents conflicts
- **Determinism**: Byte-for-byte reproducible
- **Key lesson**: Independent `[[package]]` blocks are the gold standard for merge-friendliness

### pnpm-lock.yaml v9

- **Format**: YAML, `importers` + `packages` + `snapshots` sections
- **Staleness**: `pnpmfileChecksum` + `packageExtensionsChecksum` (indirect)
- **Per-platform**: Minimal (npm tarballs mostly platform-agnostic)
- **Merge UX**: Moderate — YAML indentation creates more conflict surface
- **Key lesson**: YAML format is less merge-friendly; v9 broke Dependabot/GitLab during rollout

### pixi.lock v6

- **Format**: YAML, two-section: `environments` (per-platform refs) + `packages` (flat list)
- **Staleness**: Satisfiability check — semantic, not hash-based
- **Per-platform**: Strong — explicit platform keys in `environments` section (e.g., `linux-64`, `osx-arm64`)
- **Merge UX**: Moderate — two-section design means adding a tool touches two distant sections
- **Key lesson**: Multi-platform-first design is correct; two-section split is unnecessarily complex

### mise.lock

- **Format**: TOML, `[[tools.name]]` blocks with `[tools.name.platforms.os-arch]` sub-tables
- **Staleness**: None — tool-presence prune + version compare
- **Per-platform**: Strong — `linux-x64`, `macos-arm64`, etc. with `sha256:` prefixed checksums
- **Merge UX**: Good — TOML blocks are merge-friendly
- **Key lesson**: Closest structural analogue to OCX needs; missing version sentinel and declaration hash

### pdm.lock v4.5

- **Format**: TOML, `[metadata]` block + `[[package]]` array
- **Staleness**: `content_hash = "sha256:<hex>"` in `[metadata]` — SHA-256 of `pyproject.toml`
- **Per-platform**: Optional `[[metadata.targets]]` for platform-specific locking
- **Merge UX**: Good — `[metadata]` conflict is predictable and isolated
- **Key lesson**: `content_hash` in leading `[metadata]` block is the right staleness pattern

---

## Cross-Tool Comparison

| Dimension | Cargo v4 | pnpm v9 | pixi v6 | mise | pdm v4.5 |
|---|---|---|---|---|---|
| Format | TOML | YAML | YAML | TOML | TOML |
| Version sentinel | `version = 4` (root) | `lockfileVersion: '9.0'` | `version: 6` | None | `lock_version = "4.5.0"` in `[metadata]` |
| Declaration hash | None | Indirect | None | None | `content_hash = "sha256:..."` |
| Checksum format | Bare SHA-256 hex | SRI (`sha512-<base64>`) | SHA-256 + MD5 | `sha256:<hex>` or `blake3:<hex>` | SHA-256 hex |
| Per-platform | None | Minimal | Full (per-platform keys) | Full (`[tools.N.platforms.os-arch]`) | Optional `[[metadata.targets]]` |
| Merge friendliness | Excellent | Moderate | Moderate | Good | Good |
| Determinism | Byte-for-byte | Impl-defined | Alphabetically sorted | BTreeMap-sorted | Deterministic |

---

## Design Decisions for ocx.lock

### What to Copy

1. **Cargo's independent `[[tool]]` blocks** — alphabetically sorted, each self-contained; concurrent additions of different tools never conflict
2. **PDM's `[metadata]` block** with `content_hash`/`declaration_hash` — O(1) staleness detection without re-resolution
3. **mise's per-platform sub-tables** — `[tool.platforms.linux-amd64]` keeps all data for a tool together, human-readable
4. **mise's prefixed digest format** — `sha256:<hex>` matches OCI's native digest format, algorithm-explicit, zero conversion

### What to Avoid

- YAML (pixi, pnpm): indentation-sensitive, more merge pain
- Two-section design (pixi): adding a tool requires editing two distant sections
- No version sentinel (mise): cannot distinguish old/new format shapes
- Declaration hash at file end: put it in a leading `[metadata]` block for O(1) staleness check

### Platform Key Format

Use OCI/Go-style `os-arch` (`linux-amd64`, `darwin-arm64`, `windows-amd64`) rather than:
- conda-style (`linux-64`, `osx-arm64`)
- mise-style (`linux-x64`)
- Rationale: OCI is OCX's native ecosystem; these strings match `GOARCH`/`GOOS` conventions

### Declaration Hash Scope

Hash only the `[tools]` section of `ocx.toml` (canonical serialization), not the whole file. Cosmetic changes (comments, whitespace) should not trigger re-lock.

---

## Industry Trends

- **Trending**: Multi-platform lock files (all platforms in one file, CI-derivable)
- **Trending**: Prefixed checksums (`sha256:...`) over bare hex or SRI base64
- **Established**: Declaration hash for cheap staleness detection (PDM)
- **Established**: TOML over YAML for new CLI tool lock files
- **Established**: Alphabetical sort as the determinism contract
- **Emerging**: Blake3 alongside SHA-256 (mise) — defer to v2

---

## Sources

- [Cargo.lock internals](https://internals.rust-lang.org/t/upcoming-changes-to-cargo-lock/14017)
- [mise.lock docs](https://mise.jdx.dev/dev-tools/mise-lock.html)
- [pixi.lock docs](https://pixi.prefix.dev/latest/workspace/lockfile/)
- [PDM Internals: Lock File](https://frostming.com/en/2024/pdm-lockfile/)
- [Lockfile Format Design and Tradeoffs](https://nesbitt.io/2026/01/17/lockfile-format-design-and-tradeoffs.html)
