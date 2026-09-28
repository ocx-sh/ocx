// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Create-time interface-binaries auto-scan: the `ocx package create` compile
//! step that fills or verifies the `binaries` claim against the content tree.
//! See `adr_declared_binaries_metadata.md` § 2.

use crate::error::Error as PackageError;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::metadata::authoring::AuthoringMetadata;
use crate::metadata::binary::{Binaries, BinaryError, BinaryName};
use crate::metadata::env::modifier::Modifier;
use crate::metadata::template::classify_install_path_rooted_dir;
use ocx_oci::{OperatingSystem, Platform};
use ocx_util::fs::path::join_under_root;
use ocx_util::fs::{DirWalker, WalkDecision};

/// Windows extension allowlist the scan strips from a filename stem; frozen, never
/// read from `%PATHEXT%`, or a scan's result depends on the build host.
pub const BIN_SCAN_WINDOWS_EXTENSIONS: [&str; 4] = [".exe", ".com", ".bat", ".cmd"];

/// Scans `content_root` for executables under `platform`'s convention in the
/// directories `metadata`'s install-path-rooted, interface-visible `Path` vars name.
///
/// # Errors
///
/// Directory-walk I/O failures; a missing scan target contributes nothing.
pub async fn scan_interface_binaries(
    content_root: &Path,
    metadata: &AuthoringMetadata,
    platform: &Platform,
) -> Result<BTreeSet<BinaryName>, PackageError> {
    let candidates = collect_candidates(content_root, metadata, platform).await?;
    Ok(candidates
        .into_iter()
        .filter_map(|(name, candidate)| candidate.executable.then_some(name))
        .collect())
}

/// A scanned name; non-executable ones are kept so verify can tell "not
/// executable" from "absent".
struct Candidate {
    path: PathBuf,
    executable: bool,
}

async fn collect_candidates(
    content_root: &Path,
    metadata: &AuthoringMetadata,
    platform: &Platform,
) -> Result<BTreeMap<BinaryName, Candidate>, PackageError> {
    let AuthoringMetadata::Bundle(bundle) = metadata;
    let strip = usize::from(bundle.strip_components.unwrap_or(0));
    let wildcard_dirs = wildcard_target_dirs(content_root, strip).await?;

    let mut candidates: BTreeMap<BinaryName, Candidate> = BTreeMap::new();
    for var in &bundle.env {
        if !var.visibility.has_interface() {
            continue;
        }
        let Modifier::Path(path_var) = &var.modifier else {
            continue;
        };
        let Some(rel) = classify_install_path_rooted_dir(&path_var.value) else {
            continue;
        };
        for wildcard_dir in &wildcard_dirs {
            let Ok(scan_dir) = join_under_root(wildcard_dir, rel.as_path()) else {
                continue;
            };
            collect_directory_candidates(&scan_dir, platform, &mut candidates).await?;
        }
    }
    Ok(candidates)
}

/// Every existing directory `strip` levels below `content_root`, the ones
/// `${installPath}` maps onto after extraction.
pub async fn wildcard_target_dirs(content_root: &Path, strip: usize) -> Result<Vec<PathBuf>, PackageError> {
    let classify = move |dir: &Path, depth: usize| -> WalkDecision<PathBuf> {
        if depth < strip {
            WalkDecision::descend()
        } else {
            WalkDecision::leaf(dir.to_path_buf())
        }
    };
    DirWalker::new(content_root, classify)
        .max_depth(strip)
        .walk()
        .await
        .map_err(Into::into)
}

/// Every regular file directly under `dir`, with its symlink-followed metadata.
///
/// Applies no filter: [`super::libc_lint`] needs every file, so a filter here narrows it.
///
/// # Errors
///
/// Any I/O failure but a missing `dir` or a dangling symlink, which yield nothing;
/// swallowing one would bake `binaries: []` or pass Verify unread.
pub async fn scan_directory_files(dir: &Path) -> Result<Vec<(PathBuf, std::fs::Metadata)>, PackageError> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(e) => return Err(super::error::file_error(dir, e)),
    };
    let mut files = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| super::error::file_error(dir, e))?
    {
        let path = entry.path();
        let file_metadata = match tokio::fs::metadata(&path).await {
            Ok(file_metadata) => file_metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(super::error::file_error(&path, e)),
        };
        if !file_metadata.is_file() {
            continue;
        }
        files.push((path, file_metadata));
    }
    Ok(files)
}

/// Merges into `candidates` each file under `dir` that `platform`'s convention
/// claims and whose stem is a valid [`BinaryName`].
async fn collect_directory_candidates(
    dir: &Path,
    platform: &Platform,
    candidates: &mut BTreeMap<BinaryName, Candidate>,
) -> Result<(), PackageError> {
    for (path, file_metadata) in scan_directory_files(dir).await? {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((claim, executable)) = claim_name(file_name, &file_metadata, platform) else {
            continue;
        };
        let Ok(name) = BinaryName::try_from(claim) else {
            continue;
        };
        candidates
            .entry(name)
            .and_modify(|existing| existing.executable |= executable)
            .or_insert(Candidate { path, executable });
    }
    Ok(())
}

/// Claims `file_name` as `(name, executable)` under `platform`'s convention:
/// nothing for wasm, the extension allowlist for Windows, the exec-bit otherwise.
fn claim_name(file_name: &str, metadata: &std::fs::Metadata, platform: &Platform) -> Option<(String, bool)> {
    if is_wasm_target(platform) {
        None
    } else if is_windows_target(platform) {
        windows_claim_name(file_name).map(|stem| (stem.to_string(), true))
    } else {
        Some((file_name.to_string(), unix_is_executable(metadata)))
    }
}

fn is_windows_target(platform: &Platform) -> bool {
    matches!(
        platform,
        Platform::Specific {
            os: OperatingSystem::Windows,
            ..
        }
    )
}

fn is_wasm_target(platform: &Platform) -> bool {
    matches!(
        platform,
        Platform::Specific {
            os: OperatingSystem::Wasip1 | OperatingSystem::Wasip2,
            ..
        }
    )
}

/// Whether this host can evaluate `platform`'s convention; the exec-bit one needs
/// a Unix host, where [`unix_is_executable`] otherwise reads every file as non-executable.
fn host_can_scan(platform: &Platform) -> bool {
    is_windows_target(platform) || is_wasm_target(platform) || cfg!(unix)
}

/// Strips the first allowlisted extension, matched ASCII case-insensitively as
/// Windows does, keeping the stem's case.
fn windows_claim_name(file_name: &str) -> Option<&str> {
    let lower = file_name.to_ascii_lowercase();
    let ext = BIN_SCAN_WINDOWS_EXTENSIONS.iter().find(|ext| lower.ends_with(*ext))?;
    Some(&file_name[..file_name.len() - ext.len()])
}

// Duplicated by `ocx_config::env`'s `executable_verdict`: change the bit test in both.
#[cfg(unix)]
fn unix_is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // Callers filter to `is_file()` first: a directory's `x` bit means traversable.
    metadata.permissions().mode() & 0o111 != 0
}

// ponytail: no host API for the exec bit on a non-Unix build, so every
// candidate reads as non-executable rather than erroring. Upgrade if
// cross-host Unix-convention scanning becomes a real requirement.
#[cfg(not(unix))]
fn unix_is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// The `--bin-scan` Verify-mode check: every scanned executable must be declared;
/// a declared name absent from disk is legal.
///
/// # Errors
///
/// [`BinScanError::UndeclaredBinary`], [`BinScanError::DeclaredNotExecutable`], or
/// [`BinScanError::Scan`] on a directory-walk I/O failure.
pub async fn verify_declared_binaries(
    content_root: &Path,
    metadata: &AuthoringMetadata,
    platform: &Platform,
    declared: &Binaries,
) -> Result<(), BinScanError> {
    let candidates = collect_candidates(content_root, metadata, platform).await?;
    let declared_names: BTreeSet<&BinaryName> = declared.iter().collect();

    for (name, candidate) in &candidates {
        if candidate.executable && !declared_names.contains(name) {
            return Err(BinScanError::UndeclaredBinary {
                name: name.clone(),
                path: candidate.path.clone(),
            });
        }
    }
    for name in declared.iter() {
        if let Some(candidate) = candidates.get(name)
            && !candidate.executable
        {
            return Err(BinScanError::DeclaredNotExecutable {
                name: name.clone(),
                path: candidate.path.clone(),
            });
        }
    }
    Ok(())
}

/// The `--bin-scan`/`--no-bin-scan` tri-state [`resolve_binaries`] acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    /// Neither flag: fill an absent claim, pass a declared one through.
    Auto,
    /// `--bin-scan`: verify a declared claim, or fill an absent one like `Auto`.
    Verify,
    /// `--no-bin-scan`: never scan.
    Off,
}

/// Fills or verifies `metadata`'s `binaries` claim per `mode`.
///
/// # Errors
///
/// A Verify disagreement, a case-fold collision in a scanned set
/// ([`BinScanError::Binary`]), a walk I/O failure, or
/// [`BinScanError::UnsupportedHostScan`] when a required scan cannot run on this host.
pub async fn resolve_binaries(
    content_root: &Path,
    metadata: AuthoringMetadata,
    platform: &Platform,
    mode: ScanMode,
) -> Result<AuthoringMetadata, BinScanError> {
    resolve_binaries_on_host(content_root, metadata, platform, mode, host_can_scan(platform)).await
}

/// [`resolve_binaries`] with the host verdict injected, so a Unix build can test the "cannot scan" arms.
async fn resolve_binaries_on_host(
    content_root: &Path,
    metadata: AuthoringMetadata,
    platform: &Platform,
    mode: ScanMode,
    host_can_scan: bool,
) -> Result<AuthoringMetadata, BinScanError> {
    let declared = metadata.binaries().cloned();

    match (mode, declared) {
        (ScanMode::Off, _) => Ok(metadata),
        // Refuse, never degrade: Auto would publish a silently absent `binaries`.
        (ScanMode::Verify, _) | (ScanMode::Auto, None) if !host_can_scan => Err(BinScanError::UnsupportedHostScan {
            platform: platform.clone(),
        }),
        (_, None) => {
            let scanned = scan_interface_binaries(content_root, &metadata, platform).await?;
            let binaries = Binaries::try_from(scanned)?;
            Ok(metadata.with_binaries(binaries))
        }
        (ScanMode::Verify, Some(declared)) => {
            verify_declared_binaries(content_root, &metadata, platform, &declared).await?;
            Ok(metadata)
        }
        (ScanMode::Auto, Some(_)) => Ok(metadata),
    }
}

/// Errors from [`resolve_binaries`].
#[derive(Debug, thiserror::Error)]
pub enum BinScanError {
    /// A scanned executable is absent from the declared `binaries`.
    #[error("scanned binary '{name}' at '{}' is not declared in binaries", path.display())]
    UndeclaredBinary { name: BinaryName, path: std::path::PathBuf },
    /// A declared name is on disk but not executable under the platform's convention.
    #[error("declared binary '{name}' at '{}' is not executable", path.display())]
    DeclaredNotExecutable { name: BinaryName, path: std::path::PathBuf },
    /// A scanned set fails [`Binaries`]' case-fold-collision check.
    #[error(transparent)]
    Binary(#[from] BinaryError),
    /// The directory walk failed.
    #[error("interface-binaries scan failed")]
    Scan(#[from] PackageError),
    /// A required scan cannot evaluate `platform`'s convention on this host.
    #[error("cannot scan for '{platform}' executables on this host; hand-author binaries or pass --no-bin-scan")]
    UnsupportedHostScan { platform: Platform },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux_platform() -> Platform {
        "linux/amd64".parse().expect("linux/amd64 parses")
    }

    fn windows_platform() -> Platform {
        "windows/amd64".parse().expect("windows/amd64 parses")
    }

    // Both callers are `#[cfg(unix)]` (the wasm scans assert on unix file
    // modes), so without the same gate this helper is dead code on Windows and
    // `-D warnings` turns that into a build failure.
    #[cfg(unix)]
    fn wasm_platform() -> Platform {
        "wasip1/wasm".parse().expect("wasip1/wasm parses")
    }

    /// Builds `AuthoringMetadata` declaring one interface-visible `Path` var
    /// at `${installPath}/<rel>` — the exact scan-scope shape from ADR §2
    /// step 1. `visibility` is the wire value (`"interface"`, `"public"`,
    /// `"private"`); `strip_components` mirrors `AuthoringBundle.strip_components`.
    fn interface_path_metadata(rel: &str, visibility: &str, strip_components: Option<u8>) -> AuthoringMetadata {
        let strip = strip_components
            .map(|n| format!(r#","strip_components":{n}"#))
            .unwrap_or_default();
        let json = format!(
            r#"{{"type":"bundle","version":1{strip},
                "env":[{{"key":"PATH","type":"path","value":"${{installPath}}/{rel}","required":false,"visibility":"{visibility}"}}]}}"#
        );
        serde_json::from_str(&json).expect("fixture metadata parses")
    }

    #[cfg(unix)]
    fn write_exec_file(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, b"").expect("write fixture file");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod fixture file");
    }

    #[cfg(unix)]
    fn write_nonexec_file(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, b"").expect("write fixture file");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("chmod fixture file");
    }

    fn names(set: &BTreeSet<BinaryName>) -> Vec<&str> {
        set.iter().map(BinaryName::as_str).collect()
    }

    // ── Regular-file + exec-bit filter (is_file() conjunction) ──────────

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_admits_executable_files_excludes_nonexec_files_and_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("cmake"));
        write_nonexec_file(&bin.join("README"));
        // A subdirectory carries the traversable exec bit by default (0o755)
        // but must still be excluded — the scan requires `is_file()`, not
        // just the exec bit (ADR §2 step 3 / edge case table).
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(bin.join("vendor")).unwrap();
        std::fs::set_permissions(bin.join("vendor"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds");

        assert_eq!(
            names(&found),
            vec!["cmake"],
            "only the executable regular file must be claimed; non-exec file and subdirectory excluded"
        );
    }

    // ── Wasm targets claim nothing ──────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_claims_nothing_for_a_wasm_target() {
        // A wasm artifact is data, not a host executable: neither the Unix
        // exec bit nor a Windows extension says anything about it. The scan
        // is skipped by convention, so the claim is deterministically empty
        // even on a tree carrying exec-bit files and `.wasm` modules.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("build-helper"));
        write_nonexec_file(&bin.join("tool.wasm"));
        write_exec_file(&bin.join("other.wasm"));

        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &wasm_platform())
            .await
            .expect("scan succeeds");

        assert!(
            found.is_empty(),
            "a wasm target claims no binaries, got {:?}",
            names(&found)
        );

        // The same tree under a native target still claims — proving the
        // empty result above is the wasm rule, not an unreadable fixture.
        let native = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds");
        assert_eq!(names(&native), vec!["build-helper", "other.wasm"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_binaries_fills_an_empty_claim_for_a_wasm_target() {
        // Auto mode with no declared claim must FILL (an empty list), not
        // refuse: skipping the scan is a convention, not a host limitation,
        // so `UnsupportedHostScan` must not fire.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("build-helper"));

        let metadata = interface_path_metadata("bin", "interface", None);
        let resolved = resolve_binaries(dir.path(), metadata, &wasm_platform(), ScanMode::Auto)
            .await
            .expect("auto fill succeeds for a wasm target");

        let binaries = resolved.binaries().expect("claim is filled, not left absent");
        assert_eq!(binaries.iter().count(), 0, "the filled claim is empty");
    }

    /// The claim `ocx package create -p <platform>` fills for the acceptance
    /// suite's default package tree (`bin/hello`, executable, reached through
    /// a `public` `PATH` entry): empty for a wasm target, `["hello"]` for the
    /// identical tree under `linux/amd64`. The native leg is what makes the
    /// empty wasm claim evidence of the no-scan rule rather than of a tree the
    /// scan could not read.
    #[cfg(unix)]
    #[tokio::test]
    // ported-from: test/tests/test_platform_pairs.py::test_wasm_target_claims_no_binaries
    async fn wasm_target_claims_no_binaries_where_the_same_tree_under_linux_claims_hello() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("hello"));
        let metadata = || -> AuthoringMetadata {
            serde_json::from_str(
                r#"{"type":"bundle","version":1,"env":[
                    {"key":"PATH","type":"path","required":true,"value":"${installPath}/bin","visibility":"public"},
                    {"key":"HELLO_HOME","type":"constant","value":"${installPath}","visibility":"public"}]}"#,
            )
            .expect("fixture metadata parses")
        };
        let claimed = |resolved: &AuthoringMetadata| -> Vec<String> {
            resolved
                .binaries()
                .expect("an auto scan fills the claim rather than leaving it absent")
                .iter()
                .map(|name| name.as_str().to_owned())
                .collect()
        };

        let wasm = resolve_binaries(dir.path(), metadata(), &"wasip1/wasm".parse().unwrap(), ScanMode::Auto)
            .await
            .expect("a wasm target resolves");
        assert_eq!(
            claimed(&wasm),
            Vec::<String>::new(),
            "a wasm target must claim no binaries"
        );

        let native = resolve_binaries(dir.path(), metadata(), &linux_platform(), ScanMode::Auto)
            .await
            .expect("a linux target resolves");
        assert_eq!(
            claimed(&native),
            vec!["hello"],
            "control: the identical tree under linux/amd64 must claim its executable"
        );
    }

    // ── Symlink follow + dangling symlink exclude ───────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_follows_symlink_to_executable_and_excludes_dangling_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("gcc-13"));
        std::os::unix::fs::symlink(bin.join("gcc-13"), bin.join("gcc")).unwrap();
        std::os::unix::fs::symlink(bin.join("does-not-exist"), bin.join("broken")).unwrap();

        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds despite a dangling symlink present");

        assert_eq!(
            names(&found),
            vec!["gcc", "gcc-13"],
            "a symlink to an executable target is a claim named after the LINK; the dangling symlink is excluded"
        );
    }

    // ── strip_components wildcard mapping (0 / 1 / multi-top-dir union) ──

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_strip_components_zero_scans_content_root_directly() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("tool"));

        let metadata = interface_path_metadata("bin", "interface", None); // strip_components absent => 0
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds");
        assert_eq!(names(&found), vec!["tool"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_strip_components_one_scans_under_single_wildcard_top_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("project-1.0").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("tool"));

        let metadata = interface_path_metadata("bin", "interface", Some(1));
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds");
        assert_eq!(names(&found), vec!["tool"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_strip_components_one_unions_across_multiple_top_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let bin_a = dir.path().join("linux-amd64").join("bin");
        let bin_b = dir.path().join("linux-arm64").join("bin");
        std::fs::create_dir_all(&bin_a).unwrap();
        std::fs::create_dir_all(&bin_b).unwrap();
        write_exec_file(&bin_a.join("tool-a"));
        write_exec_file(&bin_b.join("tool-b"));

        let metadata = interface_path_metadata("bin", "interface", Some(1));
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds");
        assert_eq!(
            names(&found),
            vec!["tool-a", "tool-b"],
            "two top-level dirs both containing <rel> must union, not just the first match"
        );
    }

    // ── Interface-only var selection ──────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_excludes_private_visibility_path_var_dir() {
        let dir = tempfile::tempdir().unwrap();
        let libexec = dir.path().join("libexec");
        std::fs::create_dir_all(&libexec).unwrap();
        write_exec_file(&libexec.join("helper"));

        // Private visibility (`has_interface() == false`) — never a scan
        // candidate, ADR §1 "Scope — interface-surface, own-package only".
        let metadata = interface_path_metadata("libexec", "private", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds");
        assert!(
            found.is_empty(),
            "a private-visibility PATH var's directory must never be scanned: {found:?}"
        );
    }

    // ── Combined-path var exclusion ───────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_excludes_combined_path_var_from_scan_scope() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("tool"));

        // Combined with a `${deps.*}` segment — not the exact
        // `${installPath}/<rel>` shape, so it is excluded from scan scope
        // entirely (best-effort, not an error) per ADR §2 step 1.
        let json = r#"{"type":"bundle","version":1,
            "env":[{"key":"PATH","type":"path","value":"${installPath}/bin:${deps.other.installPath}/bin","required":false,"visibility":"interface"}]}"#;
        let metadata: AuthoringMetadata = serde_json::from_str(json).expect("fixture metadata parses");

        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds");
        assert!(
            found.is_empty(),
            "a combined ${{installPath}}+${{deps.*}} var must not be treated as a scan target: {found:?}"
        );
    }

    // ── Windows extension allowlist ─────────────────────────────────────

    /// Pins the allowlist contract cross-platform, independent of host OS —
    /// pure data assertion, no filesystem access.
    #[test]
    fn windows_extension_allowlist_is_fixed() {
        assert_eq!(BIN_SCAN_WINDOWS_EXTENSIONS, [".exe", ".com", ".bat", ".cmd"]);
    }

    #[tokio::test]
    async fn scan_windows_allowlist_claims_bare_name_for_allowed_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["foo.exe", "foo.com", "foo.bat", "foo.cmd"] {
            std::fs::write(bin.join(name), b"").unwrap();
        }

        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &windows_platform())
            .await
            .expect("scan succeeds");
        assert_eq!(
            names(&found),
            vec!["foo"],
            "every allowlisted extension must claim the same bare stem, deduped: {found:?}"
        );
    }

    #[tokio::test]
    async fn scan_windows_allowlist_excludes_non_allowlisted_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["lib.dll", "script.ps1", "readme.txt"] {
            std::fs::write(bin.join(name), b"").unwrap();
        }

        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &windows_platform())
            .await
            .expect("scan succeeds");
        assert!(
            found.is_empty(),
            "non-allowlisted extensions must never be claimed: {found:?}"
        );
    }

    /// Regression (Codex gate): Windows resolves `.exe`/`.EXE`/`.Exe`
    /// identically, so the allowlist match must be ASCII case-insensitive —
    /// a `strip_suffix` against the lowercase literals alone silently missed
    /// `foo.EXE`/`Foo.CoM`, producing zero claims for executables that
    /// resolve under default Windows/PATHEXT convention.
    #[tokio::test]
    async fn scan_windows_allowlist_matches_extension_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("foo.EXE"), b"").unwrap();
        std::fs::write(bin.join("Foo.CoM"), b"").unwrap();

        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &windows_platform())
            .await
            .expect("scan succeeds");
        assert_eq!(
            names(&found),
            vec!["Foo", "foo"],
            "a mixed/upper-case allowlisted extension must still claim the case-preserved stem: {found:?}"
        );
    }

    /// Regression (Codex gate): exclusion of a non-allowlisted extension must
    /// also be case-insensitive, not just the positive-match path.
    #[tokio::test]
    async fn scan_windows_allowlist_excludes_non_allowlisted_extensions_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("foo.DLL"), b"").unwrap();

        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &windows_platform())
            .await
            .expect("scan succeeds");
        assert!(
            found.is_empty(),
            "a non-allowlisted extension must be excluded regardless of case: {found:?}"
        );
    }

    /// Regression (Codex gate): `--bin-scan` Verify-mode must catch a
    /// case-varied allowlisted extension too — an undeclared `bar.EXE` must
    /// be flagged, not silently missed because the extension match failed.
    #[tokio::test]
    async fn verify_flags_undeclared_case_varied_windows_extension() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("bar.EXE"), b"").unwrap();

        let metadata = interface_path_metadata("bin", "interface", None);
        let declared = Binaries::try_from(BTreeSet::new()).unwrap();
        let err = verify_declared_binaries(dir.path(), &metadata, &windows_platform(), &declared)
            .await
            .expect_err("an undeclared executable must be flagged even with a case-varied extension");
        assert!(
            matches!(&err, BinScanError::UndeclaredBinary { name, .. } if name.as_str() == "bar"),
            "unexpected: {err}"
        );
    }

    // ── Non-grammar filename exclusion ────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_excludes_filenames_failing_binary_name_grammar() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("cmake"));
        // `.DS_Store`-style stray file: executable, but its stem starts
        // with `.` — fails BinaryName::try_from, silently excluded (not an
        // error) per ADR §2 step 3 / edge case table.
        write_exec_file(&bin.join(".DS_Store"));

        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds despite a non-grammar filename present");
        assert_eq!(
            names(&found),
            vec!["cmake"],
            "a filename failing the BinaryName grammar must be silently excluded: {found:?}"
        );
    }

    // ── Self-referential symlink (ELOOP, not NotFound) ──────────────────

    /// Regression: a self-referential symlink yields `FilesystemLoop`/`ELOOP`
    /// from `tokio::fs::metadata`, not `NotFound` — the dangling-symlink
    /// exclusion in `collect_directory_candidates` matches only `NotFound`,
    /// so this must propagate as an error rather than be silently skipped
    /// like a dangling symlink (ADR §2 step 3 fail-closed rationale).
    #[cfg(unix)]
    #[tokio::test]
    async fn scan_propagates_metadata_error_for_self_referential_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("tool"));
        std::os::unix::fs::symlink("loop", bin.join("loop")).unwrap();

        let metadata = interface_path_metadata("bin", "interface", None);
        let scan_err = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect_err("a self-referential symlink must propagate as an error, not be silently skipped");
        assert!(matches!(scan_err, PackageError::File(_)), "unexpected: {scan_err:?}");

        let auto_err = resolve_binaries(dir.path(), metadata, &linux_platform(), ScanMode::Auto)
            .await
            .expect_err("Auto mode must propagate the metadata error too, not silently fill binaries");
        assert!(matches!(auto_err, BinScanError::Scan(_)), "unexpected: {auto_err}");
    }

    // ── Wildcard target is a regular file (NotADirectory) ───────────────

    /// A regular file sitting where the wildcard-mapped scan target would be
    /// a directory hits `NotADirectory` on `read_dir`, matched alongside
    /// `NotFound` in `collect_directory_candidates` — zero candidates, not an
    /// error (same "existence probed before walk" rationale as a missing
    /// target dir).
    #[tokio::test]
    async fn scan_target_dir_is_a_regular_file_returns_zero_candidates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bin"), b"").unwrap();

        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("a scan target that is a regular file must not be an error");
        assert!(found.is_empty());
    }

    // ── Empty / nonexistent PATH-target dir ─────────────────────────────

    #[tokio::test]
    async fn scan_missing_target_dir_returns_zero_candidates_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // No "bin" directory created at all.
        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("a missing scan target must not be an error");
        assert!(found.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_empty_target_dir_returns_zero_candidates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        let metadata = interface_path_metadata("bin", "interface", None);
        let found = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect("scan succeeds");
        assert!(found.is_empty());
    }

    // ── Fail-closed scan I/O (unreadable target dir must error, not zero) ──

    /// Regression (max-tier review Cluster 1): a permission error reading the
    /// scan target must propagate, not collapse to zero candidates — the
    /// old behavior let `binaries: []` (a positive "publisher asserts zero"
    /// claim) get baked into the sidecar despite never actually reading the
    /// target, and let `--bin-scan` Verify pass green the same way.
    ///
    /// Root ignores DAC permission bits, so the unreadable condition cannot
    /// be constructed as root — detect via probe (same pattern as
    /// `project::registry` tests) and skip rather than false-failing.
    ///
    /// Restores `bin`'s permissions via an RAII guard rather than a trailing
    /// statement — a panicking assertion partway through must not strand a
    /// root-unreadable directory `tempfile::TempDir`'s own `Drop` then fails
    /// to remove.
    #[cfg(unix)]
    #[tokio::test]
    async fn scan_propagates_permission_denied_reading_target_dir() {
        use std::os::unix::fs::PermissionsExt;

        /// Restores `path` to `0o755` on drop, unconditionally — cleanup that
        /// runs even when the test body panics on an assertion.
        struct RestorePermsOnDrop<'a>(&'a std::path::Path);
        impl Drop for RestorePermsOnDrop<'_> {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec_file(&bin.join("tool"));
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o000)).unwrap();
        let _restore_perms = RestorePermsOnDrop(&bin);

        if std::fs::read_dir(&bin).is_ok() {
            eprintln!("skipping scan_propagates_permission_denied_reading_target_dir: running as root");
            return;
        }

        let metadata = interface_path_metadata("bin", "interface", None);

        let scan_err = scan_interface_binaries(dir.path(), &metadata, &linux_platform())
            .await
            .expect_err("a permission-denied scan target must propagate as an error, not zero candidates");
        assert!(matches!(scan_err, PackageError::File(_)), "unexpected: {scan_err:?}");

        let auto_err = resolve_binaries(dir.path(), metadata.clone(), &linux_platform(), ScanMode::Auto)
            .await
            .expect_err("Auto mode must propagate the scan I/O error too, not silently fill binaries: []");
        assert!(matches!(auto_err, BinScanError::Scan(_)), "unexpected: {auto_err}");

        let verify_err = resolve_binaries(dir.path(), metadata, &linux_platform(), ScanMode::Verify)
            .await
            .expect_err("Verify mode must propagate the scan I/O error too");
        assert!(matches!(verify_err, BinScanError::Scan(_)), "unexpected: {verify_err}");
    }

    // ── Per-invocation independence ─────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_invocations_share_no_state_across_calls() {
        let dir_a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir_a.path().join("bin")).unwrap();
        write_exec_file(&dir_a.path().join("bin").join("alpha"));

        let dir_b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir_b.path().join("bin")).unwrap();
        write_exec_file(&dir_b.path().join("bin").join("beta"));

        let metadata = interface_path_metadata("bin", "interface", None);

        let found_a = scan_interface_binaries(dir_a.path(), &metadata, &linux_platform())
            .await
            .expect("first scan succeeds");
        let found_b = scan_interface_binaries(dir_b.path(), &metadata, &linux_platform())
            .await
            .expect("second scan succeeds");

        assert_eq!(
            names(&found_a),
            vec!["alpha"],
            "first invocation must see only its own tree"
        );
        assert_eq!(
            names(&found_b),
            vec!["beta"],
            "second invocation must see only its own tree, no leakage from the first"
        );
    }

    // ── Cross-host scan capability (host cannot evaluate the target
    // platform's executable convention) ─────────────────────────────────

    /// Compile-time-trivial pass-through: on a Unix host, `host_can_scan`
    /// must never refuse — both conventions are evaluable (regression Codex
    /// #3: guards against accidentally narrowing the check to reject a
    /// valid combination).
    #[cfg(unix)]
    #[test]
    fn host_can_scan_passes_every_platform_on_a_unix_host() {
        assert!(host_can_scan(&linux_platform()));
        assert!(host_can_scan(&windows_platform()));
    }

    // The four arms below run on **every** host, Unix included, by calling
    // `resolve_binaries_on_host` with the verdict injected. Through the public
    // `resolve_binaries`, `host_can_scan` folds `cfg!(unix)` to a compile-time
    // `true` here, so a `#[cfg(not(unix))]` test of these arms would never
    // compile on this host and its green would be indistinguishable from never
    // having run — the exact shape `quality-core.md` "Unchecked Green" names.

    /// A host that cannot evaluate the target's executable convention must
    /// refuse `Auto` with an absent claim, not publish it silently.
    ///
    /// Leaving `binaries` undeclared reads downstream as "this publisher
    /// declared none" — indistinguishable from a deliberate omission, so the
    /// artifact ships with an unchecked claim and nothing said so.
    #[tokio::test]
    async fn resolve_binaries_auto_fails_closed_when_the_host_cannot_scan() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = interface_path_metadata("bin", "interface", None);

        let err = resolve_binaries_on_host(dir.path(), metadata, &linux_platform(), ScanMode::Auto, false)
            .await
            .expect_err("Auto with an absent claim must refuse rather than publish unchecked");
        assert!(
            matches!(err, BinScanError::UnsupportedHostScan { .. }),
            "unexpected: {err}"
        );
        assert!(
            err.to_string().contains("--no-bin-scan"),
            "the refusal must name the way through, or it costs a docs lookup: {err}"
        );
    }

    /// `--bin-scan` Verify mode must fail closed rather than pass green on an
    /// untrustworthy scan (every candidate would otherwise report as
    /// non-executable, which either hides real undeclared binaries or falsely
    /// flags a genuinely executable declared name as `DeclaredNotExecutable`).
    #[tokio::test]
    async fn resolve_binaries_verify_fails_closed_when_the_host_cannot_scan() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = interface_path_metadata("bin", "interface", None);

        let err = resolve_binaries_on_host(dir.path(), metadata, &linux_platform(), ScanMode::Verify, false)
            .await
            .expect_err("Verify must fail closed instead of silently trusting an unevaluable scan");
        assert!(
            matches!(err, BinScanError::UnsupportedHostScan { .. }),
            "unexpected: {err}"
        );
        assert!(err.to_string().contains("--no-bin-scan"), "unexpected: {err}");
    }

    /// The control that keeps the refusal narrow: `Auto` with a claim already
    /// declared never scans by design, so an unscannable host has nothing to
    /// refuse. Without this, widening the guard to every `Auto` would pass.
    #[tokio::test]
    async fn resolve_binaries_auto_passes_a_declared_claim_through_when_the_host_cannot_scan() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = interface_path_metadata("bin", "interface", None)
            .with_binaries(Binaries::try_from(BTreeSet::from([BinaryName::try_from("tool").unwrap()])).unwrap());

        let resolved = resolve_binaries_on_host(dir.path(), metadata, &linux_platform(), ScanMode::Auto, false)
            .await
            .expect("a declared claim needs no scan, so an unscannable host is irrelevant");
        assert_eq!(
            resolved.binaries().map(|binaries| binaries.iter().count()),
            Some(1),
            "the declared claim must pass through verbatim: {:?}",
            resolved.binaries()
        );
    }

    /// The other control: `--no-bin-scan` never scans, so it stays a way
    /// through on exactly the host that cannot scan. This is the remedy the
    /// two refusals above name, so it must actually work.
    #[tokio::test]
    async fn resolve_binaries_off_still_passes_through_when_the_host_cannot_scan() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = interface_path_metadata("bin", "interface", None);

        let resolved = resolve_binaries_on_host(dir.path(), metadata, &linux_platform(), ScanMode::Off, false)
            .await
            .expect("--no-bin-scan is the documented way past an unscannable host");
        assert!(
            resolved.binaries().is_none(),
            "Off passes the field through verbatim, undeclared included: {:?}",
            resolved.binaries()
        );
    }
}
