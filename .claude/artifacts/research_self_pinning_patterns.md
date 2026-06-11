# Research: Self-Pinning Patterns (rustup / mise / uv / deno / bun / pixi)

**Date:** 2026-06-11
**Scope:** Issue #156 — `ocx self setup` version parameter (tag or digest) + bootstrap-binary adoption. Supplements `adr_self_setup.md` Industry Context and `research_self_setup.md`.

## Per-Tool Findings

### rustup / rustup-init
- Initial setup: `rustup-init` **copies the running executable** into `$CARGO_HOME/bin/rustup` via `std::env::current_exe()` + `fs::copy` — the only confirmed adopt-running-binary pattern. Only toolchain is downloaded.
- Self-upgrade inverts: downloads fresh `rustup-init`, runs hidden `--self-replace` flag to swap.
- Windows: deferred-delete pattern (locked running exe).
- No env/flag to pin the installer's own version — pin by pinning the rustup-init artifact URL.
- Sources: rustup `src/cli/self_update.rs`; rustup#3328

### mise
- `mise self-update [VERSION]` — **positional** version arg; `--force`, `--yes`.
- Install script pin: `MISE_VERSION=v2025.12.0` env; script embeds checksums (committed-script reproducibility pattern).
- GPG sig for install script; aqua-registry tools get minisign/cosign/SLSA.
- No digest pinning for mise itself.
- Sources: mise.jdx.dev/cli/self-update.html, /installing-mise.html

### uv
- `uv self update [TARGET_VERSION]` — **positional** (PR #7252). Install script pins via URL path (`astral.sh/uv/0.11.20/install.ps1`).
- `UV_UNMANAGED_INSTALL` disables self-update; package-manager installs disable it too.
- No checksum/digest flag; no dry-run (#9828 open).
- **Instructive failure**: #16519 — `uv self update X` fails in mirrored/offline envs (always hits CDN). A local-index-honoring pin avoids this.

### deno / bun
- `deno upgrade --version 1.37.0`; canary: `--canary --version <commit-sha>` — **closest analog to digest pinning** (opaque hash as version).
- `deno upgrade --checksum <sha256>` — only first-class integrity flag found; post-hoc verification, not pre-specification.
- `--dry-run`, `--output <path>`. Downloads fresh, verifies-runs before replace; never adopts.
- `bun upgrade --version`, `--canary`/`--stable` channels.

### pixi
- `pixi self-update --version <V>` (flag style), `--dry-run`, `--force`.
- Uses **`self-replace` crate** for Windows swap (`.__selfdelete__.exe` trick: move aside, FILE_FLAG_DELETE_ON_CLOSE copy, spawn, parent-deletes). Production-tested.

## Synthesis

1. **Flag naming**: positional `[VERSION]` is the converging modern convention (uv, mise, deno-ish); `--version <V>` flag style declining (pixi, bun).
2. **Digest precedents**: deno canary commit-sha proves hash-as-version UX. No surveyed tool accepts full `name:tag@sha256:` — OCX's `@sha256:` identifier syntax is native and ahead here.
3. **Adopt-vs-download**: only rustup-init adopts, and only at initial install (never on upgrade — running binary can't replace itself). All others download. Integrity gap when adopting: adopted binary lacks manifest binding.
4. **OCX-specific blocker for adoption**: CAS layer identity = digest of the **layer tar**, not of extracted files. An extracted running binary cannot be re-verified against the OCI manifest without the original tar → adopted package is permanently unverifiable (confirms `adr_self_setup.md` Decision 2B rejection).
5. **Offline pin**: honor local index for pinned versions (avoid uv#16519 failure mode); OCX `--frozen` + digest pin already models this.
6. **`--dry-run` on self-ops is expected** — OCX already has it on `self setup`.

## Recommendation Hints for OCX

1. Optional positional `[VERSION]` on `ocx self setup`: `1.2.3`, `@sha256:<hex>`, `1.2.3@sha256:<hex>` (OCX identifier grammar subset).
2. Tag pin → bypass `find_latest_version`, resolve via index (ChainMode policy applies: `--frozen`/`--offline` → exit 81).
3. Digest pin → bypass tag resolution (existing digest fast path).
4. Adoption: recommend **rejecting** move/adopt mode (CAS verifiability blocker, #4 above); air-gapped served by mirrors + digest pin. If ever pursued: rustup-init initial-install-only model + `self-replace` crate for Windows.
5. Surface resolved digest in JSON output — enables #150's installer-passes-digest flow.

## Sources

- https://github.com/rust-lang/rustup/blob/main/src/cli/self_update.rs
- https://github.com/rust-lang/rustup/issues/3328
- https://mise.jdx.dev/cli/self-update.html / https://mise.jdx.dev/installing-mise.html
- https://docs.astral.sh/uv/reference/installer/ ; uv issues #6642, #16519
- https://docs.deno.com/runtime/reference/cli/upgrade/
- https://bun.com/docs/guides/util/upgrade
- http://pixi.prefix.dev/dev/reference/cli/pixi/self-update/
- https://docs.rs/self-replace/latest/self_replace/
