// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Embedded prebuilt `ocx-shim` executable bytes (`adr_windows_exe_shim.md` Contract 3).
//!
//! Refresh on every `crates/ocx_shim` change: take the `ocx-shim-fresh-<target>` artifacts from that
//! change's `build-windows-shims.yml` run (or `task rust:shim:build TARGET=<arch>-pc-windows-gnullvm`),
//! copy them to `shims/`, update [`SHIM_SHA256`].

// Blobs built with rustc 1.95.0, cargo-zigbuild 0.22.3, Zig 0.16.0 (PyPI `ziglang==0.16.0`, sha256
// 2317bbb91798556d9d0f38aabdac23db83f0979b25f767259ae474546724087c): nothing else pins Zig.

/// Upper bound on the embedded shim size, asserted at compile time on Windows.
pub const SHIM_SIZE_BUDGET: usize = 512 * 1024;

/// The prebuilt `ocx-shim` for the target arch; empty off Windows.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const SHIM_BYTES: &[u8] = include_bytes!("shims/ocx-shim-x86_64.exe");

/// SHA-256 of the committed blob: a corruption canary, not a provenance control; empty off Windows.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const SHIM_SHA256: &str = "fc124d0dd2d1bcf29e5b504132695b5b478b96aa8c0470857baa2227ca26ccb1";

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const SHIM_BYTES: &[u8] = include_bytes!("shims/ocx-shim-aarch64.exe");

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const SHIM_SHA256: &str = "1766a13861870fddc465201773a22ca07aba30866a6c3bdac93ad13e53cd0596";

#[cfg(not(target_os = "windows"))]
pub const SHIM_BYTES: &[u8] = &[];

#[cfg(not(target_os = "windows"))]
pub const SHIM_SHA256: &str = "";

/// Whether `image` carries a Win32 VERSIONINFO resource, whose `OriginalFilename` would contradict every hardlinked name.
pub fn contains_version_resource(image: &[u8]) -> bool {
    const STRUCTURE_KEYS: [&str; 2] = ["VS_VERSION_INFO", "StringFileInfo"];

    STRUCTURE_KEYS.iter().any(|key| {
        let needle: Vec<u8> = key.encode_utf16().flat_map(u16::to_le_bytes).collect();
        // Byte-aligned, not `chunks(2)`: a key may sit at an odd offset, which a u16 scan misses.
        image.windows(needle.len()).any(|window| window == needle)
    })
}

/// Whether a published blob of `published_len` bytes matches `embedded`; an empty `embedded` admits every length.
pub fn published_blob_is_intact(published_len: u64, embedded: &[u8]) -> bool {
    embedded.is_empty() || published_len == embedded.len() as u64
}

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

    // ── C-035 blob-freshness canary ───────────────────────────────────────

    /// The committed blobs must carry the `.exec` grammar `crates/ocx_shim`
    /// gained in WP-6 (`plan_toolchain_activation.md` C-035, D-V4).
    ///
    /// # Why a canary is needed at all
    ///
    /// The blobs are committed binaries embedded by `include_bytes!`, so
    /// **changing `crates/ocx_shim/src/{core,main}.rs` changes nothing that
    /// ships** until the blob is rebuilt. Every existing check passes on an
    /// untouched blob beside changed source: the PE magic is unchanged, the
    /// size budget is unchanged, and `sha256(blob) == SHIM_SHA256` holds
    /// precisely *because* nothing was rebuilt. Worse, all three are
    /// `_on_windows`-gated and do not run on the Linux leg at all. Without this
    /// row, a Windows trampoline would silently keep the two-sidecar shim: no
    /// `.exec` probe, no root-flag arm, no `OCX_GLOBAL` strip, and no test
    /// anywhere would notice.
    ///
    /// # Both states were observed on the real artifact
    ///
    /// It was **red on the blobs WP-6 inherited** (exit 101: neither literal
    /// present in either arch) and went green the moment WP-14 rebuilt them
    /// through `task rust:shim:build`, so no mutation is needed to prove it
    /// discriminates (D-V4). It carried `#[ignore]` for exactly that window —
    /// a born-red canary in the default set would have left `task rust:verify`
    /// red for every work package between WP-6 and WP-14. That window closed
    /// with the refresh, and the attribute went with it: nothing in
    /// `taskfiles/` or `.github/workflows/` passes `--ignored`, so leaving it
    /// would have made this the one check whose green is indistinguishable
    /// from never having run (`quality-core.md` "Unchecked Green").
    ///
    /// # What reds it from here on
    ///
    /// A future `crates/ocx_shim` source change with no blob refresh — the
    /// exact drift this row exists for. Also a truncated or zero-byte blob, a
    /// deleted `ocx-shim-{x86_64,aarch64}.exe` (the required-filename loop),
    /// an unreadable `src/shims`, and any rebuild whose root-flag arm stops
    /// emitting either literal.
    ///
    /// # What it scans for
    ///
    /// Two `&'static str` literals `build_child_command_line`'s root-flag arm
    /// emits and no earlier shim could contain, in their plain-ASCII rodata
    /// form. Deliberately not a PE parser and not a symbol table walk — the
    /// same "few lines with no edge cases" rung
    /// [`contains_version_resource`] takes, and for the same reason.
    ///
    /// Read from disk rather than from [`SHIM_BYTES`], and fail-closed on an
    /// empty file, so it scans **both** arches on every host instead of only
    /// the one the running build embeds — off Windows that constant is `&[]`,
    /// and a canary applied to it would be green on the only host CI runs
    /// (`quality-core.md` "Unchecked Green").
    #[test]
    fn committed_shim_blobs_carry_the_exec_sidecar_grammar() {
        /// Literals introduced by C-032's root-flag arm. Both, not either: a
        /// single short needle could match by coincidence in a 200 KiB image,
        /// and requiring both makes a partial rebuild visible too.
        const EXEC_GRAMMAR_LITERALS: [&str; 2] = [" --project ", " --global"];

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
            for literal in EXEC_GRAMMAR_LITERALS {
                let needle = literal.as_bytes();
                assert!(
                    image.windows(needle.len()).any(|window| window == needle),
                    "{} predates the `.exec` sidecar grammar: it carries no `{literal}`. \
                     The shim SOURCE changed but the committed blob did not, so Windows \
                     trampolines still run the two-sidecar shim — no `.exec` probe, no \
                     root-flag arm, no OCX_GLOBAL strip. Rebuild both arches via \
                     `task rust:shim:build` and re-record SHIM_SHA256.",
                    path.display()
                );
            }
            scanned.push(
                path.file_name()
                    .expect("a file has a name")
                    .to_string_lossy()
                    .into_owned(),
            );
        }

        // Without this the loop body could execute zero times and the test
        // would still pass — the exact shape the canary exists to avoid.
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
