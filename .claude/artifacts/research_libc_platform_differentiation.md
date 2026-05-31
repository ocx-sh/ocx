# Research: libc Differentiation in OCI Platform Selection

Date: 2026-05-28
Status: input for ADR `adr_platform_libc_os_features.md`
Sources: two `worker-researcher` agents (OCI spec + ecosystem; host-side detection).

---

## 1. OCI Image-Spec Current State (v1.1.1, Nov 2025)

Normative text quoted from `image-index.md` v1.1.1:

| Field | Spec status |
|---|---|
| `os` (REQUIRED) | Go `GOOS` values |
| `architecture` (REQUIRED) | Go `GOARCH` values |
| `variant` (OPTIONAL) | Platform Variants table; PR #1172 (March 2024) added `amd64 v1/v2/v3/...`, `arm64 v8, v8.1, ...`, `riscv64`, `ppc64le` |
| `os.version` (OPTIONAL) | "implementation-defined. e.g. `10.0.14393.1066` on `windows`." Implementations MAY refuse manifests where `os.version` is not known to work with host. |
| `os.features` (OPTIONAL) | For `windows`: SHOULD use `win32k`. **For non-windows: "values are implementation-defined and SHOULD be submitted to this specification for standardization."** |
| `features` (OPTIONAL) | **"This property is RESERVED for future versions of the specification."** |

Issue #1147 (Nov 2023) proposed deprecating `os.features` — closed not-planned.
Issue #1216 (open) proposes annotation-based platform descriptors via ORAS — single-manifest artifacts only, no libc scope.
**No active spec proposal addresses libc differentiation.** Genuine ecosystem gap.

containerd/moby implement variant max-compatibility selection (moby PR #43434, merged April 2022, shipped in 23.0+). `os.features` only consulted for Windows `win32k`.

## 2. Ecosystem Conventions for libc

Universal pattern: separate tags or separate URL buckets. No project uses OCI platform fields for libc.

| Project | libc differentiation | OCI fields used |
|---|---|---|
| Alpine vs Wolfi | Separate image repos | none |
| Docker Hardened | Separate tags (`:1.0`, `:1.0-alpine`) | none |
| uv / ruff / cargo-binstall | Rust target triple in URL + `ldd` probe | none (not OCI) |
| cargo-dist | `dist-manifest.json` three-bucket model (`linux-gnu`, `linux-musl-dynamic`, `linux-musl-static`) | none |
| Gentoo binary packages | Separate repo path (`/binpackages/x86-64-v3/`) | none |
| RHEL 9 / 10 | Distro baseline bump | none |

## 3. Host-Side libc Detection (cargo-binstall pattern)

Reference implementation: `cargo-bins/cargo-binstall` crate `detect-targets`, `src/detect/linux.rs`.

Algorithm:
1. Probe ld.so paths in parallel (Tokio): `/lib/ld-linux-{arch}.so.2`, `/lib64/ld-linux-{arch}.so.2`, `/lib/{arch}-linux-gnu/ld-linux-{arch}.so.2`, `/usr/lib64/libc.so.6`, `/usr/lib/{arch}-linux-gnu/libc.so.6`.
2. Spawn loader with `--version`. Inspect exit code + stdout/stderr:
   - Exit 0 + stdout contains `GLIBC` or `GNU libc` → glibc.
   - Exit 1 + stdout = gcompat stub string → glibc-compat (project decides whether to claim).
   - Exit 1 + stderr contains `musl libc` → musl.
   - Exit 127 → Ubuntu 20.04 quirk; fall back to spawning `/bin/true`.
3. Version parse from same stdout: `"ld-linux-x86-64.so.2 (GNU libc) 2.35"`.

Edge cases handled: static musl binary on glibc host, Alpine+gcompat, Ubuntu 20.04 quirk.
Blind spot: NixOS (PT_INTERP in `/nix/store`). Fall back to conservative default.

Failure mode if mismatch: ld.so prints `version 'GLIBC_2.28' not found` at startup, exits 127 before `main()`. Clean failure, no silent corruption.

## 4. Prior Art: Python Wheel Tags (manylinux/musllinux)

PEP 600 / PEP 656:
- `manylinux_X_Y_arch` = "needs glibc ≥ X.Y on arch". Resolution: `(host_major, host_minor) >= (tag_major, tag_minor)`.
- `musllinux_X_Y_arch` = "needs musl ≥ X.Y on arch".
- Wheels emitted in descending version order — newest-compatible-libc wheel ranks above older.

Closest existing prior art for OCX's problem shape. Maps cleanly onto multiple per-libc-version manifest entries with subset/superset selection.

## 5. CPU Microarchitecture Levels (deferred but contextual)

x86-64-v1/v2/v3/v4 spec-blessed via OCI `variant` field (PR #1172). Levels:

| Level | Required features |
|---|---|
| v1 | fxsr, sse, sse2 |
| v2 | cmpxchg16b, popcnt, sse3, ssse3, sse4.1, sse4.2 |
| v3 | avx, avx2, bmi1, bmi2, f16c, fma, lzcnt, movbe, xsave |
| v4 | avx512bw, avx512cd, avx512dq, avx512f, avx512vl |

Runtime detection: `std::arch::is_x86_feature_detected!` (stable, CPUID-backed). ARM equivalent: `is_aarch64_feature_detected!` (Linux-only, auxv-backed); useless on macOS — Apple Silicon homogeneous, treat uniformly.

RHEL 10 baseline = v3. Gentoo ships v3 binpkgs since Feb 2024. Rust default x86-64 target still v1 as of 1.86.

**Out of scope for this ADR.** Variant matching is its own decision, orthogonal to libc.

## 6. Rejected Alternatives (full reasoning)

### Annotations on manifest descriptor (`io.ocx.libc`)
Rejected. Breaks single-pass OCI index resolution. Annotations live on descriptor, not platform. Index resolution selects by platform alone. Annotation-based selection would force multi-fetch + client-side re-filtering. Breaks `containerd`/`oras` interop.

### `variant` field (`variant: "musl"`)
Rejected. Collides with spec-defined CPU-microarch values. Mixes ABI and ISA in one field.

### `features` field
Rejected. RESERVED for future spec versions per OCI v1.1.1. Must remain unpopulated.

### `linux.musl` / `linux.glibc` (OS-prefixed namespace)
Rejected. libc is OS-orthogonal. Windows has Cygwin/MSYS2/UCRT, all `os: windows`. macOS has libSystem. Prefix-by-OS fits the linux-only case but doesn't extend.

### Tag suffixes only (`tool:1.0-musl`)
Rejected as **primary** mechanism. Useful as human-readable alias. Loses auto-resolution — user must know suffix taxonomy. Industry convention but inferior to in-manifest tagging for tools that resolve programmatically.

### Nix-style bundled libc + patchelf
Rejected for v1. macOS/Windows lack patchelf equivalents — cross-platform parity dies. Closure bloat (~50 MB libc closure per package). Mission creep; OCX is distributor, not toolchain builder. Static-musl achieves equivalent "runs anywhere" without machinery.

## 7. References

- [opencontainers/image-spec image-index.md v1.1.1](https://github.com/opencontainers/image-spec/blob/v1.1.1/image-index.md)
- [image-spec PR #1172 — amd64 variants table](https://github.com/opencontainers/image-spec/pull/1172)
- [image-spec issue #1147 — Deprecate os.features (closed not-planned)](https://github.com/opencontainers/image-spec/issues/1147)
- [image-spec issue #1216 — Identifying Platform-Specific OCI Artifacts](https://github.com/opencontainers/image-spec/issues/1216)
- [moby PR #43434 — variant matching](https://github.com/moby/moby/pull/43434)
- [cargo-binstall detect-targets/src/detect/linux.rs](https://github.com/cargo-bins/cargo-binstall/blob/main/crates/detect-targets/src/detect/linux.rs)
- [PEP 600 — manylinux](https://peps.python.org/pep-0600/)
- [PEP 656 — musllinux](https://peps.python.org/pep-0656/)
- [is_x86_feature_detected!](https://doc.rust-lang.org/stable/std/arch/macro.is_x86_feature_detected.html)
- [Red Hat: x86-64-v3 for RHEL 10](https://developers.redhat.com/articles/2024/01/02/exploring-x86-64-v3-red-hat-enterprise-linux-10)
