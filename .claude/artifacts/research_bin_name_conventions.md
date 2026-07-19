# Research: Executable-Name Grammar & Windows Extension Policy for Declared-Binaries Metadata

**Date:** 2026-07-19
**Axis:** domain (package-manager conventions survey)
**Consumer:** `adr_declared_binaries_metadata.md`, `plan_declared_binaries.md`
**Researcher:** worker-researcher (sonnet), persisted by orchestrator

## Question

What name grammar and Windows-extension storage policy should OCX's new `binaries` metadata field (and by extension a loosened validation for executable names generally) use? OCX's existing `EntrypointName` slug `^[a-z0-9][a-z0-9_-]*$` (max 64) is known too strict — rejects `python3.13`, `c++`, mixed-case Windows tool names.

## Survey

| Tool | Name grammar | Windows extension policy |
|---|---|---|
| npm `bin` | No documented char restriction on the bin *key* (only package `name` is regex-validated). | Key is **bare, no extension**. `cmd-shim` generates `.cmd`/`.ps1` wrappers per target shell — extension chosen by shim generator, never stored. |
| Cargo `[[bin]] name` | Looser than `[package].name`: hyphens allowed (lib targets must be Rust identifiers, bins need not); no slashes; dots/plus not restricted in practice. | **Bare name** in TOML; `.exe` appended by build/link on Windows target. Never in manifest. |
| Scoop `bin` | Declares on-disk file **with** `.exe` (source truth) + optional bare alias that lands on PATH. | Two-part: target keeps real extension; exposed shim name is bare (`.shim` companion + `.exe` stub indirection). |
| Chocolatey shimgen | No manifest field — auto-shims every `*.exe` in `tools/`, keyed by exe filename (`.ignore`/`.gui` sidecars opt out). | Extension-driven: acts only on `*.exe`; emitted shim also `.exe`, bare-callable via PATH. |
| nixpkgs `meta.mainProgram` | Bare string, explicit (deprecated "assume = pname" default broke often). Resolves `$out/bin/<mainProgram>`. | No Windows story; confirms bare-name-as-contract independent of platform. |
| mise / asdf | Shim names = filenames found in installed `bin/` dirs (plugin `list-bin-paths` or default scan) — no separate declared-name field. | asdf supports per-tool shim templates. |

**Converged pattern:** declared/lookup name is always **bare** (no `.exe`/`.cmd`); extension handling is pushed into the platform-specific shim/build/link/resolve step, never into cross-platform metadata.

## Recommendation for OCX

### Grammar (new `BinaryName` type, looser than `EntrypointName`)

- ASCII printable, no whitespace anywhere
- Forbidden chars: `/ \ < > : " | ? *` (Windows-reserved filename chars — OCX materializes names as real files on Windows)
- No leading `-` (flag-lookalike shell hazard in naive `"$@"` wrappers)
- No leading or trailing `.` (leading = Unix hidden-file ambiguity; trailing = silently stripped by Windows filesystem → on-disk collision)
- Non-empty, max 64 bytes (unchanged — char class was the problem, not length)
- Case-insensitively must not equal reserved Windows device names: `CON PRN AUX NUL COM0–COM9 LPT0–LPT9`
- Case **preserved as declared** (mixed case legit for Windows-native tools), but validation rejects two names in the same package differing only by case-fold (Windows/APFS case-insensitive filesystems would silently collide shim files)

Verified: `python3.13`, `c++`, `clang++`, `clang-format-18`, `7z`, `MSBuild` pass; `.hidden`, `-flag`, `foo.`, `con` rejected.

### Windows storage: bare name (`uv`), never on-disk filename (`uv.exe`)

1. OCX's Windows resolver already probes extensions (always incl. `.EXE`) — bare→concrete is its job; storing extensions duplicates it with drift risk.
2. Platform parity: Unix entries carry no extension; `.exe`-bearing entries would need special-case stripping at every user-input comparison (`ocx run uv`).
3. All five surveyed ecosystems independently converged on bare-name-as-contract — boring-tech choice, not novel.

## Pitfalls

- **Case-fold collisions**: reject at validation, don't just document — a self-consistent package can still emit colliding files on Windows.
- **PATHEXT ordering hazard**: stray `foo.com` shadows `foo.exe` under classic PATHEXT order. OCX sidesteps by owning its resolver — **but the create-side scan's Windows extension allowlist must be a fixed constant, not read from `%PATHEXT%`** (note: `env.rs::resolve_command_windows` reads PATHEXT from the *composed child env*, guaranteeing `.EXE`; the scan should mirror the fixed probe set, not the env var).
- **Trailing dot/space**: Windows `CreateFile` strips them — grammar's leading/trailing rules exist for this reason.
- **Non-ASCII names**: out of scope v1 (YAGNI) — no meaningful real-world demand across surveyed ecosystems; Windows console/codepage handling inconsistent.

## Sources

npm package.json + cmd-shim + package-name-guidelines docs; Cargo manifest reference + `targets.rs` + issue #1450; Scoop App-Manifests wiki; Chocolatey shimgen README + shim docs; nixpkgs meta-attributes docs; asdf plugin-create docs; mise shims docs.
