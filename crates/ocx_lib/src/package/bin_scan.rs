// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Create-time interface-binaries auto-scan — the `ocx package create`
//! compile step that fills or verifies the `binaries` claim (WP1: `metadata::
//! {Binaries, BinaryName}`) against the on-disk content tree.
//!
//! Sibling of [`super::dependency_pinning`] — the other "compile step" module
//! `ocx package create` runs before archiving. See
//! `adr_declared_binaries_metadata.md` §2.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::oci::{OperatingSystem, Platform};
use crate::package::metadata::authoring::AuthoringMetadata;
use crate::package::metadata::binary::{Binaries, BinaryError, BinaryName};
use crate::package::metadata::env::modifier::Modifier;
use crate::package::metadata::template::classify_install_path_rooted_dir;
use crate::utility::fs::path::join_under_root;
use crate::utility::fs::{DirWalker, WalkDecision};

/// Windows extension allowlist the scan strips before validating a filename
/// stem as a [`BinaryName`].
///
/// Deliberately the same set as `env.rs::resolve_command_windows`'s PATHEXT
/// *fallback*, frozen as a source constant — never read from `%PATHEXT%`.
/// See `adr_declared_binaries_metadata.md` §2 step 3.
pub const BIN_SCAN_WINDOWS_EXTENSIONS: [&str; 4] = [".exe", ".com", ".bat", ".cmd"];

/// Scans `content_root` for interface-surface executables declared reachable
/// via `metadata`'s `${installPath}`-rooted, interface-visible `Path` env
/// vars, for `platform`'s executable-file convention (Unix exec-bit vs.
/// Windows extension allowlist).
///
/// Returns the plain union of discovered names — **not** yet a validated
/// [`crate::package::metadata::Binaries`] collection; case-fold-collision
/// validation happens once, at the call site that turns a scan or an
/// authored list into the typed collection (`adr_declared_binaries_metadata.md`
/// §2 step 4, §5 Decision B).
///
/// # Errors
///
/// Propagates directory-walk I/O failures via [`crate::Error`]. A missing or
/// non-directory scan target contributes zero candidates rather than
/// erroring (§2 step 3, "Existence probed before walk").
pub async fn scan_interface_binaries(
    content_root: &Path,
    metadata: &AuthoringMetadata,
    platform: &Platform,
) -> crate::Result<BTreeSet<BinaryName>> {
    let candidates = collect_candidates(content_root, metadata, platform).await?;
    Ok(candidates
        .into_iter()
        .filter_map(|(name, candidate)| candidate.executable.then_some(name))
        .collect())
}

/// One naming candidate discovered while scanning a target directory: its
/// on-disk path (diagnostic context) and whether it satisfies `platform`'s
/// executable-file convention. A non-executable regular file with a
/// grammar-valid name is still recorded — [`scan_interface_binaries`]
/// filters to executable candidates only, but [`verify_declared_binaries`]
/// needs the non-executable ones too, to distinguish "declared name present
/// but not executable" from "declared name simply absent from disk" (ADR §2
/// mode table, Verify row).
struct Candidate {
    path: PathBuf,
    executable: bool,
}

/// Shared candidate collection behind [`scan_interface_binaries`] and
/// [`verify_declared_binaries`] — one walk of the content tree, one name ->
/// candidate map, two different projections of it.
async fn collect_candidates(
    content_root: &Path,
    metadata: &AuthoringMetadata,
    platform: &Platform,
) -> crate::Result<BTreeMap<BinaryName, Candidate>> {
    let AuthoringMetadata::Bundle(bundle) = metadata;
    let strip = usize::from(bundle.strip_components.unwrap_or(0));
    // Hoisted out of the per-var loop below: the wildcard top-level dirs
    // depend only on `content_root`/`strip`, not on the var being scanned.
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

/// Resolves the `strip`-many wildcard top-level directories `${installPath}`
/// maps onto at extraction time — every directory `strip` levels below
/// `content_root` (see `adr_declared_binaries_metadata.md` §2 step 2). Each
/// returned directory is already confirmed to exist: [`DirWalker`] only
/// discovers real directories via `readdir`, so a missing wildcard level
/// yields an empty list rather than an error.
async fn wildcard_target_dirs(content_root: &Path, strip: usize) -> crate::Result<Vec<PathBuf>> {
    let classify = move |dir: &Path, depth: usize| -> WalkDecision<PathBuf> {
        if depth < strip {
            WalkDecision::descend()
        } else {
            WalkDecision::leaf(dir.to_path_buf())
        }
    };
    DirWalker::new(content_root, classify).max_depth(strip).walk().await
}

/// Lists `dir`'s immediate entries (not recursive — a `PATH` directory never
/// contributes its subdirectories) and records a candidate for each regular
/// file whose name is grammar-valid, merging into `candidates`. A *missing*
/// `dir` contributes zero candidates, not an error (ADR §2 step 3,
/// "existence probed before walk") — but any other I/O failure (permission
/// denied, transient error) propagates: a scan that cannot read its target
/// must never silently bake `binaries: []` into the sidecar, and Verify mode
/// must not pass green without actually reading the content tree.
async fn collect_directory_candidates(
    dir: &Path,
    platform: &Platform,
    candidates: &mut BTreeMap<BinaryName, Candidate>,
) -> crate::Result<()> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(e) => return Err(crate::error::file_error(dir, e)),
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| crate::error::file_error(dir, e))?
    {
        let path = entry.path();
        // `tokio::fs::metadata` follows symlinks; a dangling symlink yields
        // `NotFound` here and is silently excluded, not a hard failure (ADR
        // §2 step 3). Any other metadata failure (e.g. permission denied)
        // propagates instead — same fail-closed rationale as the `read_dir`
        // above.
        let file_metadata = match tokio::fs::metadata(&path).await {
            Ok(file_metadata) => file_metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(crate::error::file_error(&path, e)),
        };
        if !file_metadata.is_file() {
            continue;
        }
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

/// Claims `file_name` under `platform`'s executable-file convention: Unix
/// exec-bit for every non-Windows platform (including `any` — no native OS
/// convention exists for it, so it falls back to the exec-bit check per the
/// ADR edge case table), Windows extension allowlist for a Windows target.
/// Returns `None` when the convention itself doesn't claim the file at all
/// (e.g. a non-allowlisted Windows extension); the boolean tracks whether
/// the claimed name is executable. The [`BinaryName`] grammar check is
/// applied separately by the caller.
fn claim_name(file_name: &str, metadata: &std::fs::Metadata, platform: &Platform) -> Option<(String, bool)> {
    if is_windows_target(platform) {
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

/// True when the compiled host can evaluate `platform`'s executable-file
/// convention. The Windows extension allowlist is pure string matching —
/// host-independent. The Unix exec-bit convention (every non-Windows
/// platform, including `any`) needs a Unix host: there is no portable API to
/// inspect POSIX permission bits from a non-Unix build, so
/// [`unix_is_executable`] can only ever report "not executable" there,
/// silently corrupting both an Auto-mode fill and a Verify-mode diff.
/// `cfg!(unix)` folds to a compile-time constant, so this is trivially `true`
/// on a Unix build.
fn host_can_scan(platform: &Platform) -> bool {
    is_windows_target(platform) || cfg!(unix)
}

/// Strips the first matching allowlisted extension, claiming the bare,
/// case-preserved stem. The match itself is ASCII case-insensitive — Windows
/// resolves `.exe`/`.EXE`/`.Exe` identically — while [`BIN_SCAN_WINDOWS_EXTENSIONS`]
/// stays the canonical lowercase set.
fn windows_claim_name(file_name: &str) -> Option<&str> {
    let lower = file_name.to_ascii_lowercase();
    let ext = BIN_SCAN_WINDOWS_EXTENSIONS.iter().find(|ext| lower.ends_with(*ext))?;
    Some(&file_name[..file_name.len() - ext.len()])
}

#[cfg(unix)]
fn unix_is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // A bare exec-bit test would also claim a directory (0o755 carries `x`
    // meaning "traversable") — the caller already filtered to `is_file()`
    // before reaching here, so this checks the bit only.
    metadata.permissions().mode() & 0o111 != 0
}

// ponytail: the exec bit is a POSIX filesystem concept with no host API on a
// non-Unix build — scanning for the Unix convention from a non-Unix host
// treats every candidate as non-executable rather than erroring, consistent
// with every other best-effort exclusion in this scan. Upgrade if
// cross-host Unix-convention scanning becomes a real requirement.
#[cfg(not(unix))]
fn unix_is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// One-directional diff between `declared` and the executable candidates
/// found under `content_root` — the `--bin-scan` Verify-mode check
/// (`adr_declared_binaries_metadata.md` §2 mode table): a scanned executable
/// absent from `declared` is [`BinScanError::UndeclaredBinary`]; a declared
/// name present on disk but not executable is
/// [`BinScanError::DeclaredNotExecutable`]; a declared name simply absent
/// from disk is legal (no error).
///
/// # Errors
///
/// See variant docs above, plus [`BinScanError::Scan`] for a directory-walk
/// I/O failure.
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

/// Lib-local mirror of the CLI's `--bin-scan`/`--no-bin-scan` tri-state
/// (`ocx_cli::options::BinScanMode`) — duplicated here (rather than
/// depending on the CLI crate) so this module's create-time orchestration
/// decision stays in `ocx_lib` per the lib-hosts-substance/CLI-thin
/// convention. `ocx package create` maps its parsed flag state onto this
/// type before calling [`resolve_binaries`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    /// Neither flag given: scan and fill an absent `binaries` claim; pass a
    /// declared claim through verbatim (no verification).
    Auto,
    /// `--bin-scan`: scan; verify a declared claim one-directionally against
    /// the scan result, or fill an absent one exactly like `Auto`.
    Verify,
    /// `--no-bin-scan`: never scan; the `binaries` field passes through
    /// verbatim regardless of its state.
    Off,
}

/// The create-time interface-binaries orchestration: scans, fills, or
/// verifies `metadata`'s `binaries` claim per `mode` and the authoring
/// field's current state — the full mode table in
/// `adr_declared_binaries_metadata.md` §2 / §2.1 ordering block.
///
/// # Errors
///
/// [`BinScanError::UndeclaredBinary`] / [`BinScanError::DeclaredNotExecutable`]
/// under `Verify` mode with a declared claim that disagrees with the content
/// tree; [`BinScanError::Binary`] if a freshly scanned set fails the
/// case-fold-collision check; [`BinScanError::Scan`] on a directory-walk I/O
/// failure; [`BinScanError::UnsupportedHostScan`] under `Verify` mode when
/// this host cannot evaluate `platform`'s executable-file convention.
pub async fn resolve_binaries(
    content_root: &Path,
    metadata: AuthoringMetadata,
    platform: &Platform,
    mode: ScanMode,
) -> Result<AuthoringMetadata, BinScanError> {
    // Decoupled from `metadata` up front so later arms are free to move
    // `metadata` without fighting a borrow held by this match scrutinee.
    let declared = metadata.binaries().cloned();

    // Every arm that actually scans (`scan_interface_binaries` /
    // `verify_declared_binaries`) is guarded by `host_can_scan(platform)` —
    // `Off` and `Auto` with a declared field never scan by design (ADR §2
    // mode table) and so carry no such guard. Keeping the check inline here,
    // rather than as a standalone up-front predicate, keeps this match the
    // single source of truth for which combinations scan.
    match (mode, declared) {
        (ScanMode::Off, _) => Ok(metadata),
        // Verify always needs a trustworthy scan (to fill or to diff) — fail
        // closed rather than risk a false pass or a false diff.
        (ScanMode::Verify, _) if !host_can_scan(platform) => Err(BinScanError::UnsupportedHostScan {
            platform: platform.clone(),
        }),
        // Auto with an absent field on a host that can't scan: leave it
        // undeclared (`None`) rather than baking a false `Some([])`
        // "publisher asserts zero" claim.
        (ScanMode::Auto, None) if !host_can_scan(platform) => Ok(metadata),
        // Nothing declared: fill, regardless of Auto vs Verify — verification
        // needs a declaration to verify against (ADR §2 Verify row).
        (_, None) => {
            let scanned = scan_interface_binaries(content_root, &metadata, platform).await?;
            let binaries = Binaries::try_from(scanned)?;
            Ok(metadata.with_binaries(binaries))
        }
        (ScanMode::Verify, Some(declared)) => {
            verify_declared_binaries(content_root, &metadata, platform, &declared).await?;
            Ok(metadata)
        }
        // Auto + a declared field: scan is NOT run at all.
        (ScanMode::Auto, Some(_)) => Ok(metadata),
    }
}

/// Errors from the `--bin-scan` (Verify mode) one-directional diff between a
/// scanned candidate set and a declared `binaries` claim, plus the failure
/// modes of the create-time orchestration ([`resolve_binaries`]) that wraps
/// the scan.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BinScanError {
    /// A name found on disk during the scan is absent from the declared
    /// `binaries` list.
    #[error("scanned binary '{name}' at '{}' is not declared in binaries", path.display())]
    UndeclaredBinary { name: BinaryName, path: std::path::PathBuf },
    /// A declared name is present on disk but is not executable under the
    /// active platform's convention.
    #[error("declared binary '{name}' at '{}' is not executable", path.display())]
    DeclaredNotExecutable { name: BinaryName, path: std::path::PathBuf },
    /// A freshly scanned candidate set fails [`Binaries`]' case-fold-collision
    /// validation (ADR §5 Decision B) — the same check a hand-authored
    /// `binaries` array goes through at parse time.
    #[error(transparent)]
    Binary(#[from] BinaryError),
    /// The directory-walk scan itself failed (I/O error).
    #[error("interface-binaries scan failed")]
    Scan(#[from] crate::Error),
    /// `Verify` mode needs a scan, but this host cannot evaluate `platform`'s
    /// executable-file convention (the Unix exec-bit convention scanned from
    /// a non-Unix host — there is no portable API to read POSIX permission
    /// bits off-Unix).
    #[error("cannot scan for '{platform}' executables on this host; hand-author binaries or pass --no-bin-scan")]
    UnsupportedHostScan { platform: Platform },
}

impl crate::cli::ClassifyExitCode for BinScanError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            // Input-data trouble: the declared claim disagrees with the
            // content tree, the scanned set itself is malformed, or this
            // host cannot produce a trustworthy scan for the target platform.
            Self::UndeclaredBinary { .. }
            | Self::DeclaredNotExecutable { .. }
            | Self::Binary(_)
            | Self::UnsupportedHostScan { .. } => Some(crate::cli::ExitCode::DataError),
            // Delegate to the inner cause via the chain walker.
            Self::Scan(_) => None,
        }
    }
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
        assert!(
            matches!(scan_err, crate::Error::InternalFile(_, _)),
            "unexpected: {scan_err:?}"
        );

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
        assert!(
            matches!(scan_err, crate::Error::InternalFile(_, _)),
            "unexpected: {scan_err:?}"
        );

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

    /// Regression (review W2 / Codex#3): scanning a Unix-convention platform
    /// from a non-Unix host must never bake a false `Some([])` claim — Auto
    /// mode leaves the field undeclared instead.
    #[cfg(not(unix))]
    #[tokio::test]
    async fn resolve_binaries_auto_leaves_field_undeclared_when_host_cannot_scan_unix_convention() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = interface_path_metadata("bin", "interface", None);

        let resolved = resolve_binaries(dir.path(), metadata, &linux_platform(), ScanMode::Auto)
            .await
            .expect("Auto mode must not error when the host cannot evaluate the target convention");
        assert!(
            resolved.binaries().is_none(),
            "Auto must leave the field undeclared (None), never bake a false Some([]) claim: {:?}",
            resolved.binaries()
        );
    }

    /// Regression (review W2 / Codex#3): `--bin-scan` Verify mode must fail
    /// closed rather than pass green on an untrustworthy scan (every
    /// candidate would otherwise report as non-executable, which either
    /// hides real undeclared binaries or falsely flags a genuinely
    /// executable declared name as `DeclaredNotExecutable`).
    #[cfg(not(unix))]
    #[tokio::test]
    async fn resolve_binaries_verify_errors_when_host_cannot_scan_unix_convention() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = interface_path_metadata("bin", "interface", None);

        let err = resolve_binaries(dir.path(), metadata, &linux_platform(), ScanMode::Verify)
            .await
            .expect_err("Verify mode must fail closed instead of silently trusting a non-Unix scan");
        assert!(
            matches!(err, BinScanError::UnsupportedHostScan { .. }),
            "unexpected: {err}"
        );
    }
}
