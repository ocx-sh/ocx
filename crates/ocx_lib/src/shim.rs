// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Embedded prebuilt `ocx-shim` executable bytes (ADR Contract 3).
//!
//! Crate-root cross-cutting module (peer of [`crate::hardlink`],
//! [`crate::symlink`], [`crate::utility::child_process`] per
//! `arch-principles.md` "Cross-Cutting Modules"). The only consumer is
//! `package_manager::launcher::generate`, which writes [`SHIM_BYTES`] verbatim
//! as `<name>.exe` on Windows.
//!
//! The committed blob is built and refreshed out-of-band (uv/pixi model, no
//! `build.rs`, no network). One blob per Windows arch is selected via `cfg`;
//! non-Windows targets embed nothing so the launcher emission is skipped there
//! and `ocx` carries zero shim weight on Linux/macOS.
//!
//! # Blob refresh-PR flow
//!
//! The committed blobs and their recorded [`SHIM_SHA256`] digests are
//! refreshed in a **dedicated PR** whenever `crates/ocx_shim` source changes:
//!
//! 1. `cargo xwin build -p ocx_shim --profile shim --target x86_64-pc-windows-msvc`
//! 2. `cargo xwin build -p ocx_shim --profile shim --target aarch64-pc-windows-msvc`
//!    (the `shim` profile in the workspace `Cargo.toml` strips symbols)
//! 3. Copy each `target/<triple>/shim/ocx-shim.exe` to
//!    `crates/ocx_lib/src/shims/ocx-shim-<arch>.exe`.
//! 4. Record `sha256sum` of each blob in the per-arch `SHIM_SHA256` below.
//! 5. CI (`build-windows-shims.yml`) reproducibly rebuilds and asserts
//!    byte-equality + `gh attestation verify` (the real provenance control;
//!    the SHA here is only a corruption canary — see ADR §"SHA256 = corruption
//!    canary").
//!
//! See `.claude/artifacts/adr_windows_exe_shim.md` Contract 3 and
//! `system_design_windows_exe_shim.md` §5.

/// Hard upper bound on the embedded shim size. The realistic target is
/// < 80 KiB (`opt-level="z"`, `lto`, `panic="abort"`, `strip`); this budget
/// is the fail-closed ceiling enforced by the compile-time assertion below
/// (Windows builds only) and a CI `cargo-bloat` check in Phase 4.
pub const SHIM_SIZE_BUDGET: usize = 256 * 1024;

/// Verbatim bytes of the prebuilt `ocx-shim` executable for the target arch.
///
/// Empty on non-Windows targets — no shim is emitted there, so `ocx` carries
/// no shim weight off Windows.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const SHIM_BYTES: &[u8] = include_bytes!("shims/ocx-shim-x86_64.exe");

/// Recorded SHA-256 of the committed blob (lowercase hex). Corruption canary
/// for the blob↔source drift guard test (truncated `include_bytes!`, wrong
/// path, partial checkout) — NOT a provenance control. Empty on non-Windows.
///
/// Refreshed in the dedicated blob-refresh PR (see module docs); the
/// `shim_blob_matches_recorded_sha256_fail_closed_on_windows` test fails
/// closed if this drifts from `sha256(SHIM_BYTES)`.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const SHIM_SHA256: &str = "58b7cb6d5b180216a7f4689517ce5d2495ac3c909edafffb57feea36263a0215";

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const SHIM_BYTES: &[u8] = include_bytes!("shims/ocx-shim-aarch64.exe");

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const SHIM_SHA256: &str = "708aa445d882badd05cce67b70d69d29b946f63a3b0ed3e1039a6e169ada850b";

#[cfg(not(target_os = "windows"))]
pub const SHIM_BYTES: &[u8] = &[];

#[cfg(not(target_os = "windows"))]
pub const SHIM_SHA256: &str = "";

// Fail-closed size guard. Only meaningful on Windows builds (the only targets
// that embed a non-empty blob); cfg-gated so non-Windows `cargo check` is not
// affected. The assertion is evaluated at compile time.
#[cfg(all(target_os = "windows", any(target_arch = "x86_64", target_arch = "aarch64")))]
const _: () = assert!(
    SHIM_BYTES.len() <= SHIM_SIZE_BUDGET,
    "embedded ocx-shim blob exceeds SHIM_SIZE_BUDGET"
);

#[cfg(test)]
mod tests {
    use super::{SHIM_BYTES, SHIM_SHA256};
    // `SHIM_SIZE_BUDGET` is only asserted on Windows builds (the only targets
    // that embed a non-empty blob); importing it unconditionally would be an
    // unused import on the Linux CI host.
    #[cfg(all(target_os = "windows", any(target_arch = "x86_64", target_arch = "aarch64")))]
    use super::SHIM_SIZE_BUDGET;

    // ── F-1 fail-closed corruption canary (Phase 3.1) ─────────────────────
    //
    // Plan Progress Log F-1 (Warn, Specify-actionable): the blob↔SHA guard
    // must be FAIL-CLOSED on Windows — an empty SHA or empty blob is a test
    // FAILURE on a Windows build, NOT a skip. This catches a truncated
    // `include_bytes!`, a wrong relative path, or a partial checkout. It is a
    // corruption canary, NOT a provenance control (4.4 adds SLSA attestation;
    // see ADR §"SHA256 = corruption canary").
    //
    // Today, against the 0-byte placeholder blobs + empty SHA on a Windows
    // build, this test FAILS — that is the correct failing-spec state. Phase
    // 4.3 fills the real bytes + digest atomically (commit blob, record SHA
    // in the same change), turning it green.

    #[cfg(all(target_os = "windows", any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[test]
    fn shim_blob_matches_recorded_sha256_fail_closed_on_windows() {
        use sha2::{Digest, Sha256};

        assert!(
            !SHIM_BYTES.is_empty(),
            "FAIL-CLOSED: embedded ocx-shim blob is empty on a Windows build — \
             a 0-byte placeholder or a wrong `include_bytes!` path. This MUST \
             fail (not skip) until Phase 4.3 commits the real blob."
        );
        assert_eq!(
            SHIM_SHA256.len(),
            64,
            "FAIL-CLOSED: SHIM_SHA256 must be a 64-char lowercase hex digest on \
             a Windows build; empty/short = unrecorded blob (test FAILURE, not skip)"
        );
        let computed = {
            let mut hasher = Sha256::new();
            hasher.update(SHIM_BYTES);
            let digest = hasher.finalize();
            let mut hex = String::with_capacity(64);
            for byte in digest {
                use std::fmt::Write as _;
                write!(hex, "{byte:02x}").expect("writing to a String is infallible");
            }
            hex
        };
        assert_eq!(
            computed, SHIM_SHA256,
            "corruption canary: sha256(SHIM_BYTES) must equal the recorded \
             SHIM_SHA256 — committed blob has drifted from its recorded digest"
        );
    }

    #[cfg(all(target_os = "windows", any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[test]
    fn shim_blob_within_size_budget_on_windows() {
        assert!(
            !SHIM_BYTES.is_empty(),
            "FAIL-CLOSED: blob must be non-empty before the size assertion is meaningful"
        );
        assert!(
            SHIM_BYTES.len() <= SHIM_SIZE_BUDGET,
            "embedded ocx-shim blob ({} bytes) exceeds SHIM_SIZE_BUDGET ({} bytes)",
            SHIM_BYTES.len(),
            SHIM_SIZE_BUDGET
        );
    }

    // On non-Windows targets `ocx` carries zero shim weight: the blob and the
    // SHA are both empty. This is the inverse contract — it MUST hold on the
    // Linux CI host (and is the host-runnable half of the F-1 spec).
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn shim_blob_is_empty_off_windows() {
        assert!(
            SHIM_BYTES.is_empty(),
            "non-Windows builds must embed no shim bytes (zero weight off Windows)"
        );
        assert!(
            SHIM_SHA256.is_empty(),
            "non-Windows builds must record no SHA (no blob to guard)"
        );
    }
}
