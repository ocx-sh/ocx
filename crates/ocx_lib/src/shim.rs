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

/// Hard upper bound on the embedded shim size, enforced fail-closed by the
/// compile-time assertion below (Windows builds only).
///
/// 512 KiB ceiling: the hermetic cargo-zigbuild output with the pinned
/// stable Zig is ~208–284 KiB (x86_64 is the larger; build-std/strip is
/// less aggressive on stable Zig than on the dev toolchain used in the
/// PoC). Reproducibility is the priority — the size is an accepted
/// trade-off (see adr_shim_hermetic_zigbuild.md); shrinking it (verify
/// build-std/immediate-abort efficacy on stable Zig) is a tracked
/// follow-up, not a blocker.
pub const SHIM_SIZE_BUDGET: usize = 512 * 1024;

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
pub const SHIM_SHA256: &str = "f4ec623a601e08efd544c1add68416685c37f3fcc540922a141c9069ec6935b8";

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const SHIM_BYTES: &[u8] = include_bytes!("shims/ocx-shim-aarch64.exe");

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const SHIM_SHA256: &str = "9aebb7516449ec146b6ccbf3cb971e95cc57f82cbfafa4d605d7e96cae74ee2a";

#[cfg(not(target_os = "windows"))]
pub const SHIM_BYTES: &[u8] = &[];

#[cfg(not(target_os = "windows"))]
pub const SHIM_SHA256: &str = "";

/// Whether `image` carries a Win32 VERSIONINFO resource (C-019).
///
/// # Why this exists
///
/// One blob is hardlinked and served under N well-known tool names (`cmake`,
/// `ctest`, `clang-format`, …). A VERSIONINFO resource carries a fixed
/// `OriginalFilename` string, so the moment the blob grows one, every one of
/// those names disagrees with the name baked into the image it runs — the
/// image-vs-`OriginalFilename` mismatch behavioural detections key on
/// (MITRE ATT&CK T1036.005, masquerading as a legitimate name). It would
/// appear across every shimmed name at once, from a single build-flag change,
/// with nothing in the diff to show for it.
///
/// The committed blobs carry no such resource today, which is exactly why this
/// is a **canary** — same family as [`SHIM_SHA256`]: it does not fix anything,
/// it notices when the property stops holding. It must land with the blob
/// refresh, since a published blob is as one-way as the `.shim` format.
///
/// # What it does, and why that is not a PE parser
///
/// A byte scan for the UTF-16LE encodings of the two structure keys a
/// VERSIONINFO resource cannot omit — `VS_VERSION_INFO` (the root block) and
/// `StringFileInfo` (the block holding `OriginalFilename`). Both are literal,
/// fixed strings written verbatim into `.rsrc` by every toolchain that emits
/// the resource.
///
/// Deliberately NOT a PE resource-directory walk: parsing the section table,
/// the resource tree and its three levels of directory entries would be a
/// hand-owned parser for an external binary format
/// (`quality-core.md` "Don't Own Non-Domain Code", Block-tier for wire
/// formats), owned for the sake of one boolean. A substring search is the
/// "few lines with no edge cases" rung instead. What that costs, stated so
/// nobody mistakes this for more than it is: the scan is one-directional. A
/// hit is conclusive; a miss is conclusive only for toolchains that write
/// those keys literally, which is all of them today.
///
/// `image` is taken as a parameter rather than read from [`SHIM_BYTES`] so the
/// scanner can be shown both red and green on the Linux CI host, where
/// `SHIM_BYTES` is empty and a canary applied to it would be indistinguishable
/// from one that never ran (`quality-core.md` "Unchecked Green").
pub fn contains_version_resource(image: &[u8]) -> bool {
    /// The two structure keys a VERSIONINFO resource cannot omit, in the
    /// UTF-16LE form a resource compiler writes into `.rsrc`.
    const STRUCTURE_KEYS: [&str; 2] = ["VS_VERSION_INFO", "StringFileInfo"];

    STRUCTURE_KEYS.iter().any(|key| {
        let needle: Vec<u8> = key.encode_utf16().flat_map(u16::to_le_bytes).collect();
        // Byte-aligned, not `chunks(2)`: a `.rsrc` key's offset relative to the
        // start of the file is not guaranteed to be even, and a u16-aligned
        // scan would miss half of them while passing every synthetic fixture
        // that happens to land on an even offset.
        image.windows(needle.len()).any(|window| window == needle)
    })
}

/// Whether an already-published shim blob of `published_len` bytes may be
/// served as-is, or must be republished because it cannot be `embedded`.
///
/// # Why a length check at all
///
/// `ShimBinStore::ensure` publishes "only when absent" and decides absence by
/// **existence**, so a present-but-truncated blob is never repaired — it is
/// hardlinked by every subsequent launcher instead. On Windows that blob is an
/// *executed binary*, so one torn write from a crashed earlier run silently
/// breaks every lazy launcher with no recovery path, and existence cannot tell
/// that state from a healthy one. The store's own precedent is stricter:
/// `BlobStore::persist_bytes` re-checks by byte comparison precisely so a
/// corrupt entry can heal.
///
/// Length, not a full digest, because truncation is the realistic corruption
/// and comparing one `u64` costs nothing on a path taken by every launcher
/// generation. A `SHIM_SHA256` verify is the thorough form and would mean
/// hashing 200-300 KiB per call.
///
/// # The empty-`embedded` clause
///
/// An empty `embedded` admits every length. Off Windows no blob is embedded,
/// so a length carries no information there and the pre-check stays
/// existence-only — which is also what keeps the store's existing
/// specification tests (`ensure_does_not_rewrite_a_blob_that_is_already_published`
/// and the lost-race test, both of which park a short sentinel at the
/// published path and require it to survive) meaningful rather than
/// self-defeating. On Windows, where the blob is what actually runs, the
/// comparison is live.
///
/// `embedded` is a parameter rather than [`SHIM_BYTES`] read directly, so both
/// outcomes are reachable in a host test.
pub fn published_blob_is_intact(published_len: u64, embedded: &[u8]) -> bool {
    embedded.is_empty() || published_len == embedded.len() as u64
}

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
    use super::{SHIM_BYTES, SHIM_SHA256, contains_version_resource, published_blob_is_intact};
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

    // ── C-019 VERSIONINFO-absence canary ──────────────────────────────────
    //
    // The canary's own detection method is what these rows pin, on inputs the
    // test owns. Applying it to `SHIM_BYTES` alone would be the Unchecked
    // Green `quality-core.md` names: off Windows that slice is empty, so a
    // green there is indistinguishable from a scanner that never ran. Every
    // row below therefore feeds a synthetic image and demonstrates BOTH
    // outcomes; the assertion against the real blobs is the last test, and it
    // reads them from disk so it runs — and scans both arches — on every host.

    /// The UTF-16LE encoding a Win32 resource compiler writes for a structure
    /// key — the exact byte form [`contains_version_resource`] scans for, and
    /// the reason an ASCII spelling of the same key must NOT match.
    fn utf16le(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// A blob with no resource section worth speaking of: an `MZ` stub and
    /// filler. The negative control every positive row is built from.
    fn image_without_version_resource() -> Vec<u8> {
        let mut image = b"MZ\x90\x00\x03\x00\x00\x00".to_vec();
        image.extend(std::iter::repeat_n(0u8, 512));
        image.extend(utf16le("CompanyName"));
        image.extend(utf16le("ocx-shim.exe"));
        image
    }

    #[test]
    fn contains_version_resource_fires_on_the_root_block_key() {
        let mut image = image_without_version_resource();
        image.extend(utf16le("VS_VERSION_INFO"));
        image.extend(std::iter::repeat_n(0u8, 32));
        assert!(
            contains_version_resource(&image),
            "an image carrying the UTF-16LE root block key must be reported as \
             carrying a version resource — this is the regression the canary exists for"
        );
    }

    #[test]
    fn contains_version_resource_fires_on_the_string_block_key() {
        // `StringFileInfo` is the block that holds `OriginalFilename`, the
        // string that would disagree with every hardlinked tool name at once
        // (T1036.005). Either key alone is a hit.
        let mut image = image_without_version_resource();
        image.extend(utf16le("StringFileInfo"));
        assert!(
            contains_version_resource(&image),
            "the block holding OriginalFilename must be detected on its own, \
             not only alongside the root block key"
        );
    }

    #[test]
    fn contains_version_resource_finds_a_key_at_an_odd_byte_offset() {
        // A scan implemented as `chunks(2)` over u16 pairs is aligned to even
        // offsets and would miss this, while passing every other row here.
        let mut image = vec![0xAA];
        image.extend(utf16le("VS_VERSION_INFO"));
        assert_eq!(image.len() % 2, 1, "fixture must place the key at an odd offset");
        assert!(
            contains_version_resource(&image),
            "the scan must be byte-aligned, not u16-aligned — a real .rsrc \
             offset is not guaranteed to be even relative to the file start"
        );
    }

    #[test]
    fn contains_version_resource_is_false_without_either_key() {
        assert!(
            !contains_version_resource(&image_without_version_resource()),
            "an image with no VERSIONINFO structure key must not trip the canary"
        );
        assert!(!contains_version_resource(&[]), "an empty image carries no resource");
    }

    #[test]
    fn contains_version_resource_ignores_the_ascii_spelling_of_the_keys() {
        // The keys live in `.rsrc` as UTF-16LE. A plain ASCII occurrence — a
        // string constant, a linker comment, this crate's own source embedded
        // in a debug section — is not a version resource, and a naive
        // byte-search for the ASCII form would report every one of them.
        let mut image = image_without_version_resource();
        image.extend_from_slice(b"VS_VERSION_INFO");
        image.extend_from_slice(b"StringFileInfo");
        assert!(
            !contains_version_resource(&image),
            "the scan is for the UTF-16LE encoding specifically; an ASCII \
             occurrence of the same text is not a resource and must not fire"
        );
    }

    /// C-019 itself, on the bytes that actually ship — **on every host**.
    ///
    /// It reads the committed blobs from disk rather than [`SHIM_BYTES`],
    /// which is the whole point: that constant is `&[]` off Windows, so a
    /// canary applied to it would be green on the only host CI actually runs,
    /// and that green would be indistinguishable from a scan that never
    /// happened (`quality-core.md` "Unchecked Green", sitting on a security
    /// canary). The blobs are ordinary committed files, so reading them is
    /// host-independent and every Linux run genuinely scans both arches — not
    /// only the one the running build embeds.
    ///
    /// Fail-closed in the same shape as the SHA canary: an unreadable or empty
    /// blob is a failure, and the loop's own coverage is asserted afterwards so
    /// an empty directory cannot pass vacuously.
    #[test]
    fn committed_shim_blobs_carry_no_version_resource() {
        let shims_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shims");
        let mut scanned: Vec<String> = Vec::new();

        for entry in std::fs::read_dir(&shims_dir).unwrap_or_else(|e| {
            panic!(
                "FAIL-CLOSED: the committed shim blobs must be readable at {}: {e}",
                shims_dir.display()
            )
        }) {
            let path = entry.expect("readable directory entry").path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("exe") {
                continue;
            }
            let image = std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            assert!(
                !image.is_empty(),
                "FAIL-CLOSED: {} is empty, so scanning it proves nothing",
                path.display()
            );
            assert!(
                !contains_version_resource(&image),
                "{} has grown a VERSIONINFO resource. One blob is hardlinked \
                 under N tool names, so its fixed OriginalFilename now disagrees \
                 with every one of them at once (T1036.005). Refresh the blob \
                 without the version resource, or take the mismatch as a \
                 deliberate decision and retire this canary.",
                path.display()
            );
            scanned.push(
                path.file_name()
                    .expect("a file has a name")
                    .to_string_lossy()
                    .into_owned(),
            );
        }

        // Without this the loop body could execute zero times and the test
        // would still pass — the exact shape the canary exists to avoid. A
        // membership check rather than an equality one, so a third arch added
        // later is scanned by the loop without having to be listed twice.
        for required in ["ocx-shim-x86_64.exe", "ocx-shim-aarch64.exe"] {
            assert!(
                scanned.iter().any(|name| name == required),
                "the canary must have scanned {required}; it scanned {scanned:?}"
            );
        }
    }

    // ── C-001 corrupt-blob pre-check ──────────────────────────────────────
    //
    // `ShimBinStore::ensure` decides "already published" by existence, so a
    // torn write is served forever. `published_blob_is_intact` is the predicate
    // that pre-check consults. Its empty-`embedded` clause makes it inert off
    // Windows — which is the whole reason `embedded` is a parameter: the rows
    // below drive it red and green from a NON-EMPTY fixture on the Linux host,
    // where the shipped call site can never do so.

    /// The six-byte fixture the length rows compare against. Its content is
    /// irrelevant — only `len()` is read — but it must not be empty, or every
    /// row below would take the escape clause and assert nothing.
    const EMBEDDED: &[u8] = b"abcdef";

    #[test]
    fn published_blob_is_intact_rejects_a_truncated_blob() {
        assert!(!EMBEDDED.is_empty(), "the fixture must exercise the live comparison");
        assert!(
            !published_blob_is_intact(3, EMBEDDED),
            "a blob shorter than the embedded bytes is a torn write and must be \
             republished, not hardlinked into every launcher"
        );
    }

    #[test]
    fn published_blob_is_intact_rejects_a_zero_length_blob() {
        // The realistic corruption: a crashed run left the target created but
        // unwritten. Existence alone cannot tell this from a healthy blob.
        assert!(
            !published_blob_is_intact(0, EMBEDDED),
            "a zero-length published blob must never be served as intact"
        );
    }

    #[test]
    fn published_blob_is_intact_rejects_an_overlong_blob() {
        assert!(
            !published_blob_is_intact(9, EMBEDDED),
            "the comparison is equality, not a lower bound — a longer blob is \
             not the embedded blob either"
        );
    }

    #[test]
    fn published_blob_is_intact_admits_an_exact_length_match() {
        assert!(
            published_blob_is_intact(EMBEDDED.len() as u64, EMBEDDED),
            "a blob of exactly the embedded length is served as-is; this is the \
             common path, taken by every launcher generation"
        );
    }

    #[test]
    fn published_blob_is_intact_admits_every_length_when_nothing_is_embedded() {
        // Load-bearing, not a shortcut. Off Windows `SHIM_BYTES` is empty, so a
        // length carries no information; two `ShimBinStore` specification tests
        // park a 17-byte sentinel at the published path and require it to
        // survive. Without this clause a length check would republish over it
        // and red both.
        for published_len in [0u64, 17, 329_000, u64::MAX] {
            assert!(
                published_blob_is_intact(published_len, &[]),
                "with nothing embedded there is no length to compare against, so \
                 {published_len} must be admitted and the pre-check stay existence-only"
            );
        }
    }
}
