// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Embedded prebuilt `ocx-shim` launcher blobs, one per target triple (`adr_windows_exe_shim.md` Contract 3).
//!
//! Every `ocx` embeds every blob so it can vendor for any platform. Refresh on every `crates/ocx_shim` or
//! `crates/ocx_env_apply` change: take the `ocx-shim-fresh-<target>` artifacts from that change's
//! `build-windows-shims.yml` run (or `task rust:shim:build TARGET=<triple>`), copy them to `shims/`, update
//! the matching sha256 in the test module's `RECORDED_SHA256`, and write `python3 scripts/shim_inputs.py
//! --print` to `shim_inputs.sha256`. A change to any other build input (toolchain, `shim` profile, the
//! launcher's dependencies, the Zig or cargo-zigbuild pin) needs the same refresh; that digest is how CI sees one.

// Blobs built with rustc 1.95.0, cargo-zigbuild 0.22.3, Zig 0.16.0 (PyPI `ziglang==0.16.0`, sha256
// 2317bbb91798556d9d0f38aabdac23db83f0979b25f767259ae474546724087c): nothing else pins Zig.

use ocx_oci::{Architecture, OperatingSystem, Platform};

/// Upper bound on each embedded blob, checked per blob by a unit test. The unified launcher carries four record kinds, the image-path read and the runfiles manifest lookup (largest: x86_64 Windows, 534016 bytes); the earlier 256 to 512 KiB raise is in `adr_shim_hermetic_zigbuild.md`.
pub const SHIM_SIZE_BUDGET: usize = 640 * 1024;

/// One embedded launcher: its target triple and bytes.
struct Blob {
    target: &'static str,
    bytes: &'static [u8],
}

// Blobs cut by `build-windows-shims.yml`; the SLSA attestation of that workflow run is their provenance.
const BLOBS: [Blob; 6] = [
    Blob {
        target: "x86_64-unknown-linux-musl",
        bytes: include_bytes!("shims/ocx-shim-x86_64-unknown-linux-musl"),
    },
    Blob {
        target: "aarch64-unknown-linux-musl",
        bytes: include_bytes!("shims/ocx-shim-aarch64-unknown-linux-musl"),
    },
    Blob {
        target: "x86_64-apple-darwin",
        bytes: include_bytes!("shims/ocx-shim-x86_64-apple-darwin"),
    },
    Blob {
        target: "aarch64-apple-darwin",
        bytes: include_bytes!("shims/ocx-shim-aarch64-apple-darwin"),
    },
    Blob {
        target: "x86_64-pc-windows-gnullvm",
        bytes: include_bytes!("shims/ocx-shim-x86_64-pc-windows-gnullvm.exe"),
    },
    Blob {
        target: "aarch64-pc-windows-gnullvm",
        bytes: include_bytes!("shims/ocx-shim-aarch64-pc-windows-gnullvm.exe"),
    },
];

/// The launcher blob serving `platform`, one per target triple; `None` when none exists (e.g. `wasip1/wasm`).
///
/// Libc, variant and OS features do not select a blob: the musl build runs on glibc hosts too.
pub fn blob_for(platform: &Platform) -> Option<&'static [u8]> {
    let Platform::Specific { os, arch, .. } = platform else {
        return None;
    };
    let target = match (os, arch) {
        (OperatingSystem::Linux, Architecture::Amd64) => "x86_64-unknown-linux-musl",
        (OperatingSystem::Linux, Architecture::Arm64) => "aarch64-unknown-linux-musl",
        (OperatingSystem::Darwin, Architecture::Amd64) => "x86_64-apple-darwin",
        (OperatingSystem::Darwin, Architecture::Arm64) => "aarch64-apple-darwin",
        (OperatingSystem::Windows, Architecture::Amd64) => "x86_64-pc-windows-gnullvm",
        (OperatingSystem::Windows, Architecture::Arm64) => "aarch64-pc-windows-gnullvm",
        _ => return None,
    };
    BLOBS.iter().find(|blob| blob.target == target).map(|blob| blob.bytes)
}

/// The blob for the platform this binary runs on; `None` when the host has no launcher target.
pub fn host_blob() -> Option<&'static [u8]> {
    blob_for(&Platform::current()?)
}

/// Whether `image` carries a Win32 VERSIONINFO resource, whose `OriginalFilename` would contradict every hardlinked name.
pub fn contains_version_resource(image: &[u8]) -> bool {
    const STRUCTURE_KEYS: [&str; 2] = ["VS_VERSION_INFO", "StringFileInfo"];

    STRUCTURE_KEYS.iter().any(|key| {
        let needle: Vec<u8> = key.encode_utf16().flat_map(u16::to_le_bytes).collect();
        // Byte-aligned, not `chunks(2)`: a key may sit at an odd offset, which a u16 scan misses.
        image.windows(needle.len()).any(|window| window == needle)
    })
}

/// Whether a published blob of `published_len` bytes matches the length of `embedded`.
pub fn published_blob_is_intact(published_len: u64, embedded: &[u8]) -> bool {
    published_len == embedded.len() as u64
}

#[cfg(test)]
mod tests {
    use super::{BLOBS, SHIM_SIZE_BUDGET, blob_for, contains_version_resource, host_blob, published_blob_is_intact};

    /// Each committed blob's sha256: a corruption canary, not a provenance control. Refresh with the blob.
    const RECORDED_SHA256: [(&str, &str); 6] = [
        (
            "x86_64-unknown-linux-musl",
            "7b8ccff775b2fa863782bbd4fd9f315552475b4569fcf897069365a9e5bcce47",
        ),
        (
            "aarch64-unknown-linux-musl",
            "94dde06a6a8e98a5171656151e6b3660e2ae3d3b81758b9cd85e6626e9a255d8",
        ),
        (
            "x86_64-apple-darwin",
            "dbc746aebf838830cafb1d0b74a941ea549f997f56b5e12232548d60af796c28",
        ),
        (
            "aarch64-apple-darwin",
            "7d746097373894986a1925a76d7bf5559b168b6793dd6a5b6f17eac986344741",
        ),
        (
            "x86_64-pc-windows-gnullvm",
            "59fb072835d217c5f3ea148f3a4a154213f773edbe7df40f850701d9b0657857",
        ),
        (
            "aarch64-pc-windows-gnullvm",
            "4ba744896b297f6aeab0413de1a2cd3cc4a036ce57bd36be637b04779773a03e",
        ),
    ];

    // ── fail-closed corruption canary and size budget, per blob ───────────
    //
    // An empty blob or a drifted digest is a failure, never a skip: it catches a truncated
    // `include_bytes!`, a wrong relative path or a partial checkout. A corruption canary, not a
    // provenance control (SLSA attestation is the provenance control).

    #[test]
    fn every_blob_matches_its_recorded_sha256() {
        use sha2::{Digest, Sha256};

        assert_eq!(
            RECORDED_SHA256.len(),
            BLOBS.len(),
            "one recorded digest per embedded blob"
        );
        for blob in &BLOBS {
            assert!(!blob.bytes.is_empty(), "FAIL-CLOSED: the {} blob is empty", blob.target);
            let (_, recorded) = RECORDED_SHA256
                .iter()
                .find(|(target, _)| *target == blob.target)
                .unwrap_or_else(|| panic!("{}: no recorded sha256", blob.target));
            let computed = Sha256::digest(blob.bytes)
                .iter()
                .fold(String::with_capacity(64), |mut hex, byte| {
                    use std::fmt::Write as _;
                    write!(hex, "{byte:02x}").expect("writing to a String is infallible");
                    hex
                });
            assert_eq!(
                computed, *recorded,
                "{}: the committed blob has drifted from its recorded sha256",
                blob.target
            );
        }
    }

    #[test]
    fn every_blob_is_within_the_size_budget() {
        for blob in &BLOBS {
            assert!(
                blob.bytes.len() <= SHIM_SIZE_BUDGET,
                "{}: {} bytes exceeds SHIM_SIZE_BUDGET ({SHIM_SIZE_BUDGET} bytes)",
                blob.target,
                blob.bytes.len()
            );
        }
    }

    #[test]
    fn every_blob_is_the_image_format_of_its_target() {
        for blob in &BLOBS {
            let (kind, magic): (&str, &[u8]) = if blob.target.ends_with("windows-gnullvm") {
                ("PE", b"MZ")
            } else if blob.target.ends_with("linux-musl") {
                ("ELF", b"\x7fELF")
            } else {
                ("64-bit Mach-O", &[0xcf, 0xfa, 0xed, 0xfe])
            };
            assert!(blob.bytes.starts_with(magic), "{} is not a {kind} image", blob.target);
        }
    }

    #[test]
    fn host_blob_serves_the_machine_running_the_tests() {
        assert!(
            host_blob().is_some(),
            "every supported development host has a launcher blob"
        );
    }

    // ── C-019 VERSIONINFO-absence canary ──────────────────────────────────
    //
    // These rows pin the canary's own detection method on synthetic images, demonstrating BOTH
    // outcomes; the last row applies it to the real Windows blobs.

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

    /// C-019 itself, on the bytes that actually ship. Fail-closed: an empty blob or a missing Windows target fails.
    #[test]
    fn committed_windows_blobs_carry_no_version_resource() {
        let windows: Vec<_> = BLOBS.iter().filter(|b| b.target.ends_with("windows-gnullvm")).collect();
        assert_eq!(windows.len(), 2, "both Windows blobs must be scanned");
        for blob in windows {
            assert!(
                !blob.bytes.is_empty(),
                "FAIL-CLOSED: {} is empty, so scanning it proves nothing",
                blob.target
            );
            assert!(
                !contains_version_resource(blob.bytes),
                "{} has grown a VERSIONINFO resource. One blob is hardlinked \
                 under N tool names, so its fixed OriginalFilename now disagrees \
                 with every one of them at once (T1036.005). Refresh the blob \
                 without the version resource, or take the mismatch as a \
                 deliberate decision and retire this canary.",
                blob.target
            );
        }
    }

    // ── C-035 blob-freshness canary ───────────────────────────────────────

    /// The committed Windows blobs must be built from the current launcher source.
    ///
    /// The blobs are committed binaries, so changing `crates/ocx_shim/src/**` changes nothing that ships until
    /// the blob is rebuilt, and the size, magic and sha256 checks all pass on an untouched blob. It scans for
    /// three literals the launcher emits as separate argv elements or names (`--project`, `--global`, `.launch`),
    /// the last of which no pre-unified-launcher blob contains; not a PE parser. Reds on a shim source change
    /// with no blob refresh, a truncated blob, or a rebuild that stops emitting any of them.
    #[test]
    fn committed_windows_blobs_carry_the_launcher_literals() {
        /// All, not any: one short needle could match by coincidence, and a stale blob shows.
        const LAUNCHER_LITERALS: [&str; 3] = ["--project", "--global", ".launch"];

        let windows: Vec<_> = BLOBS.iter().filter(|b| b.target.ends_with("windows-gnullvm")).collect();
        assert_eq!(windows.len(), 2, "both Windows blobs must be scanned");
        for blob in windows {
            assert!(
                !blob.bytes.is_empty(),
                "FAIL-CLOSED: {} is empty, so scanning it proves nothing",
                blob.target
            );
            for literal in LAUNCHER_LITERALS {
                let needle = literal.as_bytes();
                assert!(
                    blob.bytes.windows(needle.len()).any(|window| window == needle),
                    "{} predates the current launcher: it carries no `{literal}`. The launcher SOURCE \
                     changed but the committed blob did not. Rebuild via `task rust:shim:build` and \
                     re-record its sha256 in RECORDED_SHA256.",
                    blob.target
                );
            }
        }
    }

    // ── C-019 / V-26: the arm64 darwin launcher is signed ─────────────────

    const MACHO_MAGIC_64: u32 = 0xfeed_facf;
    const LC_CODE_SIGNATURE: u32 = 0x1d;

    /// The `(cputype, load command ids)` of a thin little-endian 64-bit Mach-O image.
    fn macho_load_commands(image: &[u8]) -> (u32, Vec<u32>) {
        let word = |at: usize| -> u32 {
            u32::from_le_bytes(
                image
                    .get(at..at + 4)
                    .unwrap_or_else(|| panic!("Mach-O image truncated at byte {at}"))
                    .try_into()
                    .expect("a 4-byte slice"),
            )
        };
        assert_eq!(word(0), MACHO_MAGIC_64, "not a thin little-endian 64-bit Mach-O image");
        let (cputype, ncmds) = (word(4), word(16));
        let mut at = 32; // `mach_header_64` is 32 bytes
        let mut commands = Vec::new();
        for _ in 0..ncmds {
            commands.push(word(at));
            at += word(at + 4) as usize;
        }
        (cputype, commands)
    }

    fn darwin_blob(target: &str) -> &'static [u8] {
        BLOBS
            .iter()
            .find(|blob| blob.target == target)
            .unwrap_or_else(|| panic!("no {target} blob"))
            .bytes
    }

    #[test]
    fn macho_parser_reads_the_load_commands_it_is_given() {
        let mut image = Vec::new();
        for word in [MACHO_MAGIC_64, 7, 0, 2, 2, 24, 0, 0] {
            image.extend(word.to_le_bytes());
        }
        for (command, size) in [(0x19_u32, 8_u32), (LC_CODE_SIGNATURE, 16)] {
            image.extend(command.to_le_bytes());
            image.extend(size.to_le_bytes());
            image.extend(std::iter::repeat_n(0u8, size as usize - 8));
        }
        assert_eq!(macho_load_commands(&image), (7, vec![0x19, LC_CODE_SIGNATURE]));
    }

    /// The `aarch64-apple-darwin` blob from the Linux cross build carries a code signature (arm64 macOS kills an
    /// unsigned image); `x86_64-apple-darwin` carries none, which is the control showing the check can go red.
    #[test]
    fn only_the_arm64_darwin_blob_carries_a_code_signature() {
        const CPU_TYPE_X86_64: u32 = 0x0100_0007;
        const CPU_TYPE_ARM64: u32 = 0x0100_000c;

        let (arm_cpu, arm_commands) = macho_load_commands(darwin_blob("aarch64-apple-darwin"));
        let (x86_cpu, x86_commands) = macho_load_commands(darwin_blob("x86_64-apple-darwin"));

        assert_eq!(arm_cpu, CPU_TYPE_ARM64, "the aarch64 blob must be an arm64 image");
        assert_eq!(x86_cpu, CPU_TYPE_X86_64, "the x86_64 blob must be an x86_64 image");
        assert!(
            arm_commands.len() > 1 && x86_commands.len() > 1,
            "both parses must have read real load commands"
        );
        assert!(
            arm_commands.contains(&LC_CODE_SIGNATURE),
            "the aarch64-apple-darwin blob lacks LC_CODE_SIGNATURE: arm64 macOS refuses to run it"
        );
        assert!(
            !x86_commands.contains(&LC_CODE_SIGNATURE),
            "the x86_64-apple-darwin blob carries a code signature, so the arm64 check above proves nothing"
        );
    }

    // ── C-001 corrupt-blob pre-check ──────────────────────────────────────
    //
    // `ShimBinStore::ensure` decides "already published" by existence, so a
    // torn write is served forever. `published_blob_is_intact` is the predicate
    // that pre-check consults, driven here red and green from a fixture.

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

    /// One blob per target triple, each within budget; a libc feature does not change the triple.
    #[test]
    fn blob_for_serves_the_six_launcher_targets() {
        for platform in [
            "linux/amd64",
            "linux/arm64",
            "darwin/amd64",
            "darwin/arm64",
            "windows/amd64",
            "windows/arm64",
        ] {
            let parsed: ocx_oci::Platform = platform.parse().unwrap();
            let blob = blob_for(&parsed).unwrap_or_else(|| panic!("no launcher blob for {platform}"));
            assert!(!blob.is_empty(), "{platform}");
            assert!(blob.len() <= super::SHIM_SIZE_BUDGET, "{platform}");
        }
        let glibc: ocx_oci::Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let bare: ocx_oci::Platform = "linux/amd64".parse().unwrap();
        assert_eq!(blob_for(&glibc), blob_for(&bare));
    }

    #[test]
    fn blob_for_serves_each_platform_its_own_target_image() {
        let image_start = |platform: &str| -> Vec<u8> {
            let parsed: ocx_oci::Platform = platform.parse().unwrap();
            blob_for(&parsed).unwrap()[..4].to_vec()
        };
        assert_eq!(image_start("linux/arm64"), b"\x7fELF");
        assert_eq!(image_start("darwin/arm64"), [0xcf, 0xfa, 0xed, 0xfe]);
        assert_eq!(&image_start("windows/amd64")[..2], b"MZ");
        let target_of = |platform: &str| {
            let parsed: ocx_oci::Platform = platform.parse().unwrap();
            BLOBS
                .iter()
                .find(|blob| blob.bytes == blob_for(&parsed).unwrap())
                .unwrap()
                .target
        };
        assert_eq!(target_of("linux/amd64"), "x86_64-unknown-linux-musl");
        assert_eq!(target_of("linux/arm64"), "aarch64-unknown-linux-musl");
        assert_eq!(target_of("darwin/amd64"), "x86_64-apple-darwin");
        assert_eq!(target_of("darwin/arm64"), "aarch64-apple-darwin");
        assert_eq!(target_of("windows/amd64"), "x86_64-pc-windows-gnullvm");
        assert_eq!(target_of("windows/arm64"), "aarch64-pc-windows-gnullvm");
    }

    #[test]
    fn blob_for_has_no_blob_for_wasm() {
        for platform in ["wasip1/wasm", "wasip2/wasm"] {
            let parsed: ocx_oci::Platform = platform.parse().unwrap();
            assert_eq!(blob_for(&parsed), None, "{platform}");
        }
    }

    #[test]
    fn blob_for_has_no_blob_for_the_any_platform() {
        assert_eq!(blob_for(&ocx_oci::Platform::any()), None);
    }
}
