// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Host libc detection: the libc families the host can *execute*, as `os.features` tags.
//!
//! Never what ocx itself links against, which answers the wrong question.
//! Algorithm: `subsystem-oci.md` § libc Differentiation. Linux-only.

use std::collections::BTreeSet;
use std::sync::OnceLock;

#[cfg(target_os = "linux")]
use serde_repr::{Deserialize_repr, Serialize_repr};

/// A libc family identified for the current host or read off a manifest.
///
/// `Ord` keeps a `BTreeSet` of flavors in a stable order regardless of probe scheduling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LibcFlavor {
    Glibc,
    Musl,
    /// A foreign `libc.*` suffix (e.g. `uclibc`); never produced by host detection.
    Unknown(String),
}

impl LibcFlavor {
    /// The canonical `os.features` tag (`libc.glibc`); the one family → tag mapping.
    pub fn os_feature_tag(&self) -> String {
        match self {
            Self::Glibc => "libc.glibc".to_string(),
            Self::Musl => "libc.musl".to_string(),
            Self::Unknown(suffix) => format!("libc.{suffix}"),
        }
    }

    /// Inverse of [`os_feature_tag`](Self::os_feature_tag); `None` for a non-`libc.` tag.
    pub fn from_os_feature_tag(tag: &str) -> Option<Self> {
        let suffix = tag.strip_prefix("libc.")?;
        Some(match suffix {
            "glibc" => Self::Glibc,
            "musl" => Self::Musl,
            other => Self::Unknown(other.to_string()),
        })
    }
}

/// A typed view of one `os.features` tag, for reporting only; matching stays string-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feature {
    Libc(LibcFlavor),
    /// Any non-`libc.*` feature, carried verbatim.
    Other(String),
}

impl Feature {
    pub fn parse(tag: &str) -> Self {
        match LibcFlavor::from_os_feature_tag(tag) {
            Some(flavor) => Self::Libc(flavor),
            None => Self::Other(tag.to_string()),
        }
    }
}

/// Detected capabilities of the current host relevant to platform selection.
#[derive(Debug, Clone)]
pub struct HostCapabilities {
    /// Empty off Linux or when detection found nothing.
    pub libcs: BTreeSet<LibcFlavor>,
}

impl HostCapabilities {
    /// Detect host capabilities; a failed probe contributes nothing, never an error.
    pub async fn detect() -> Self {
        // Test-only seam (subsystem-tests.md § Test-Only Seams):
        // `__OCX_TESTING_LIBC` short-circuits the real probe with comma-separated
        // family tokens ("glibc"/"musl"/"glibc,musl"/"none" or "" for {}); once
        // set, never falls through to the real probe.
        #[cfg(any(test, feature = "__testing"))]
        {
            if let Some(libcs) = test_libc_override() {
                return Self { libcs };
            }
        }

        Self {
            libcs: run_detection().await.libcs(),
        }
    }

    /// The detected libc set as sorted `os.features` tags.
    pub fn os_features(&self) -> Vec<String> {
        self.libcs.iter().map(LibcFlavor::os_feature_tag).collect()
    }

    /// Detect and populate the process cache [`Platform::current`](super::platform::Platform::current) reads.
    ///
    /// `record` is the persisted cache file; `None` detects uncached.
    pub async fn detect_and_cache(record: Option<&std::path::Path>) -> Self {
        if let Some(cached) = CACHED_OS_FEATURES.get() {
            return Self {
                libcs: decode_libc_tags(cached),
            };
        }
        let capabilities = detect_with_persisted_record(record).await;
        init_cache(&capabilities);
        capabilities
    }
}

/// Decode tags into known libc families; shared by both fast paths so they cannot disagree.
fn decode_libc_tags<'tags>(tags: impl IntoIterator<Item = &'tags String>) -> BTreeSet<LibcFlavor> {
    tags.into_iter()
        .filter_map(|tag| LibcFlavor::from_os_feature_tag(tag))
        .filter(|flavor| !matches!(flavor, LibcFlavor::Unknown(_)))
        .collect()
}

/// The `__OCX_TESTING_LIBC` seam value, decoded, when the variable is set.
///
/// Extracted so [`HostCapabilities::detect`] and the persisted-record path check
/// the same condition: an unset variable must fall through to the real probe,
/// and a set one must never reach (or be reached from) the on-disk record.
#[cfg(any(test, feature = "__testing"))]
fn test_libc_override() -> Option<BTreeSet<LibcFlavor>> {
    ocx_env::__OCX_TESTING_LIBC
        .get_raw()
        .and_then(|value| value.into_string().ok())
        .map(|value| parse_test_libc_set(&value))
}

/// Parse the `__OCX_TESTING_LIBC` seam value into a libc set.
///
/// Comma-separated family tokens; unknown tokens (including `none` and empty
/// strings) contribute nothing, so `"none"` and `""` both yield the empty set.
#[cfg(any(test, feature = "__testing"))]
fn parse_test_libc_set(value: &str) -> BTreeSet<LibcFlavor> {
    value
        .split(',')
        .filter_map(|token| match token.trim() {
            "glibc" => Some(LibcFlavor::Glibc),
            "musl" => Some(LibcFlavor::Musl),
            _ => None,
        })
        .collect()
}

/// What one detection pass found: every loader that classified, with its family.
///
/// The loaders are kept because the record re-checks them: a replaced loader
/// invalidates a record, which no clock can see.
#[derive(Debug, Default)]
struct Detection {
    /// Sorted by path, so the persisted record is byte-stable.
    classified: Vec<(std::path::PathBuf, LibcFlavor)>,
}

impl Detection {
    fn libcs(&self) -> BTreeSet<LibcFlavor> {
        self.classified.iter().map(|(_, flavor)| flavor.clone()).collect()
    }
}

#[cfg(target_os = "linux")]
async fn run_detection() -> Detection {
    use tokio::task::JoinSet;

    let candidate_paths = discover_loader_paths().await;

    // No early abort, or a dual-libc host reports one family; a panicked probe found nothing.
    let mut probes: JoinSet<Option<(std::path::PathBuf, LibcFlavor)>> = JoinSet::new();
    for path in candidate_paths {
        // SECURITY: `path` comes only from `discover_loader_paths`, never user input.
        probes.spawn(probe_loader(path));
    }

    let mut classified = Vec::new();
    while let Some(joined) = probes.join_next().await {
        if let Ok(Some(found)) = joined {
            classified.push(found);
        }
    }
    // `join_next` yields in completion order; unsorted, the persisted record churns.
    classified.sort();

    if classified.is_empty() && tokio::fs::try_exists("/nix").await.unwrap_or(false) {
        tracing::debug!(
            "no libc loader discovered (PT_INTERP, directory scan, and FHS \
             allowlist all empty) but /nix exists; likely NixOS without a \
             nix-ld FHS shim — degrading to Any-only matching"
        );
    }

    Detection { classified }
}

#[cfg(not(target_os = "linux"))]
async fn run_detection() -> Detection {
    Detection::default()
}

/// Candidate loader paths, deduplicated by canonical path; order: `subsystem-oci.md` § libc Differentiation.
#[cfg(target_os = "linux")]
async fn discover_loader_paths() -> Vec<std::path::PathBuf> {
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut discovered: Vec<std::path::PathBuf> = Vec::new();

    // The first binary with an interpreter names the host's native loader.
    for binary in INTERP_PROBE_BINARIES {
        if let Some(interpreter) = read_pt_interp(binary).await {
            consider_path(std::path::PathBuf::from(interpreter), &mut seen, &mut discovered).await;
            break;
        }
    }

    for path in glob_loader_paths().await {
        consider_path(path, &mut seen, &mut discovered).await;
    }

    for path in GLIBC_LOADERS.iter().chain(MUSL_LOADERS) {
        consider_path(std::path::PathBuf::from(*path), &mut seen, &mut discovered).await;
    }

    discovered
}

#[cfg(target_os = "linux")]
async fn consider_path(
    path: std::path::PathBuf,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    discovered: &mut Vec<std::path::PathBuf>,
) {
    if dedup_unseen(&path, seen).await {
        discovered.push(path);
    }
}

/// System binaries whose `PT_INTERP` names the host's native loader; first hit wins.
///
/// SECURITY: a fixed list, never user input — the loader it yields is spawned.
#[cfg(target_os = "linux")]
const INTERP_PROBE_BINARIES: &[&str] = &["/usr/bin/env", "/bin/sh", "/bin/ls"];

/// The `PT_INTERP` of the ELF at `path`; `None` when absent, unparseable or static.
#[cfg(target_os = "linux")]
async fn read_pt_interp(path: &str) -> Option<String> {
    let data = tokio::fs::read(path).await.ok()?;
    let elf = elf::ElfBytes::<elf::endian::AnyEndian>::minimal_parse(&data).ok()?;
    let segments = elf.segments()?;
    for program_header in segments {
        if program_header.p_type != elf::abi::PT_INTERP {
            continue;
        }
        let start = usize::try_from(program_header.p_offset).ok()?;
        let length = usize::try_from(program_header.p_filesz).ok()?;
        let raw = data.get(start..start.checked_add(length)?)?;
        let interpreter = raw.split(|&byte| byte == 0).next()?;
        if interpreter.is_empty() {
            return None;
        }
        let interpreter = String::from_utf8_lossy(interpreter).into_owned();
        // SECURITY (CWE-426): the path is spawned; a relative one would resolve against `$PATH` or the CWD.
        if !std::path::Path::new(&interpreter).is_absolute() {
            return None;
        }
        return Some(interpreter);
    }
    None
}

/// Scanned directly and one level down, for multiarch dirs like `/lib/x86_64-linux-gnu`.
#[cfg(target_os = "linux")]
const LOADER_SCAN_DIRS: &[&str] = &["/lib", "/lib64", "/usr/lib", "/usr/lib64"];

/// Scan [`LOADER_SCAN_DIRS`] for current-arch loader files.
///
/// One `spawn_blocking` over `std::fs`: per-entry `tokio::fs` measured 15.2 ms
/// against 2.5 ms on a usrmerge host, on the startup path.
#[cfg(target_os = "linux")]
async fn glob_loader_paths() -> Vec<std::path::PathBuf> {
    match tokio::task::spawn_blocking(scan_loader_dirs).await {
        Ok(found) => found,
        Err(join_error) => {
            tracing::debug!("loader directory scan did not complete ({join_error}); continuing without it");
            Vec::new()
        }
    }
}

#[cfg(target_os = "linux")]
fn scan_loader_dirs() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for base in dedup_scan_roots(LOADER_SCAN_DIRS) {
        // A symlinked subdir is not recursed, since `file_type()` does not follow it; real multiarch dirs are never symlinks.
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                collect_loader_files(&path, &mut found);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_current_arch_loader_name)
            {
                found.push(path);
            }
        }
    }
    found
}

/// Reduce `dirs` to the distinct trees they name, so usrmerge is not walked twice.
///
/// An uncanonicalizable path keeps its literal identity, or two missing paths collapse into one.
#[cfg(target_os = "linux")]
fn dedup_scan_roots(dirs: &[&str]) -> Vec<std::path::PathBuf> {
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut roots = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let base = std::path::PathBuf::from(dir);
        let canonical = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
        if seen.insert(canonical) {
            roots.push(base);
        }
    }
    roots
}

#[cfg(target_os = "linux")]
fn collect_loader_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_current_arch_loader_name(name) && entry.file_type().map(|file_type| !file_type.is_dir()).unwrap_or(false)
        {
            out.push(path);
        }
    }
}

/// True for a current-architecture loader filename; a foreign-arch multiarch loader does not match.
#[cfg(target_os = "linux")]
fn is_current_arch_loader_name(name: &str) -> bool {
    LIBC_FAMILIES
        .iter()
        .flat_map(|family| family.loader_name_fragments)
        .any(|fragment| name.contains(fragment))
}

/// Record `path` canonically; `true` when not seen yet. A missing path keeps
/// its literal form, or distinct missing paths collapse together.
#[cfg(target_os = "linux")]
async fn dedup_unseen(path: &std::path::Path, seen: &mut std::collections::HashSet<std::path::PathBuf>) -> bool {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_path_buf());
    seen.insert(canonical)
}

/// Per-probe timeout, bounding context init against a wedged loader; far
/// shorter yields false negatives on loaded CI runners.
#[cfg(target_os = "linux")]
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// One row of the libc family identification table.
#[cfg(target_os = "linux")]
struct LibcFamily {
    flavor: LibcFlavor,
    /// Current-architecture loader filename fragments (e.g. `ld-linux-x86-64`).
    loader_name_fragments: &'static [&'static str],
    banner_matches: fn(&str) -> bool,
}

#[cfg(target_os = "linux")]
const LIBC_FAMILIES: &[LibcFamily] = &[
    LibcFamily {
        flavor: LibcFlavor::Glibc,
        loader_name_fragments: GLIBC_LOADER_FRAGMENTS,
        banner_matches: glibc_banner_matches,
    },
    LibcFamily {
        flavor: LibcFlavor::Musl,
        loader_name_fragments: MUSL_LOADER_FRAGMENTS,
        banner_matches: musl_banner_matches,
    },
];

#[cfg(target_os = "linux")]
fn glibc_banner_matches(banner: &str) -> bool {
    banner.contains("GNU libc") || banner.contains("GLIBC")
}

/// musl's banner comes with a non-zero exit, so the status is ignored.
#[cfg(target_os = "linux")]
fn musl_banner_matches(banner: &str) -> bool {
    banner.contains("musl libc")
}

#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum BannerClass {
    Identified(LibcFlavor),
    /// A glibc-named loader exited 127 with no banner (Ubuntu 20.04); confirm with `{loader} /bin/true`.
    GlibcNeedsConfirmation,
    Unrecognized,
}

/// Classify a loader from its `--version` output, filename and exit code.
///
/// The banner outranks the filename, or a gcompat stub at the glibc path misclassifies as glibc.
#[cfg(target_os = "linux")]
fn classify_loader_banner(banner: &str, loader_name: &str, exit_code: Option<i32>) -> BannerClass {
    for family in LIBC_FAMILIES {
        if (family.banner_matches)(banner) {
            return BannerClass::Identified(family.flavor.clone());
        }
    }
    if exit_code == Some(127) && loader_name_looks_glibc(loader_name) {
        return BannerClass::GlibcNeedsConfirmation;
    }
    BannerClass::Unrecognized
}

#[cfg(target_os = "linux")]
fn loader_name_looks_glibc(loader_name: &str) -> bool {
    GLIBC_LOADER_FRAGMENTS
        .iter()
        .any(|fragment| loader_name.contains(fragment))
}

/// The libc family of the loader at `path`, or `None` when it is absent, fails or times out.
///
/// SECURITY: every spawn is bounded by [`PROBE_TIMEOUT`], or a wedged loader stalls startup.
#[cfg(target_os = "linux")]
#[expect(
    clippy::disallowed_types,
    reason = "libc detection runs a discovered loader with `--version` to classify its banner"
)]
async fn probe_loader(path: std::path::PathBuf) -> Option<(std::path::PathBuf, LibcFlavor)> {
    if tokio::fs::metadata(&path).await.is_err() {
        return None;
    }

    let output = tokio::time::timeout(
        PROBE_TIMEOUT,
        tokio::process::Command::new(&path).arg("--version").output(),
    )
    .await
    .ok()? // timeout → None
    .ok()?; // spawn/IO error → None

    // Inspect both streams: glibc prints its banner to stdout, musl to stderr.
    let mut banner = String::from_utf8_lossy(&output.stdout).into_owned();
    banner.push_str(&String::from_utf8_lossy(&output.stderr));
    let loader_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    let class = classify_loader_banner(&banner, loader_name, output.status.code());

    match class {
        BannerClass::Identified(flavor) => Some((path, flavor)),
        BannerClass::GlibcNeedsConfirmation => {
            let confirm = tokio::time::timeout(
                PROBE_TIMEOUT,
                tokio::process::Command::new(&path).arg("/bin/true").output(),
            )
            .await
            .ok()?
            .ok()?;
            confirm.status.success().then_some((path, LibcFlavor::Glibc))
        }
        BannerClass::Unrecognized => None,
    }
}

/// Canonical glibc loader paths for the build target architecture.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const GLIBC_LOADERS: &[&str] = &[
    "/lib/ld-linux-x86-64.so.2",
    "/lib64/ld-linux-x86-64.so.2",
    "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    "/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    "/usr/lib64/ld-linux-x86-64.so.2",
];
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const GLIBC_LOADERS: &[&str] = &[
    "/lib/ld-linux-aarch64.so.1",
    "/lib64/ld-linux-aarch64.so.1",
    "/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
    "/usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
    "/usr/lib64/ld-linux-aarch64.so.1",
];

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MUSL_LOADERS: &[&str] = &["/lib/ld-musl-x86_64.so.1"];
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const MUSL_LOADERS: &[&str] = &["/lib/ld-musl-aarch64.so.1"];

// Unsupported architectures: `Architecture::current` is `None` there, so nothing is probed.
#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const GLIBC_LOADERS: &[&str] = &[];
#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const MUSL_LOADERS: &[&str] = &[];

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const GLIBC_LOADER_FRAGMENTS: &[&str] = &["ld-linux-x86-64"];
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MUSL_LOADER_FRAGMENTS: &[&str] = &["ld-musl-x86_64"];

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const GLIBC_LOADER_FRAGMENTS: &[&str] = &["ld-linux-aarch64"];
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const MUSL_LOADER_FRAGMENTS: &[&str] = &["ld-musl-aarch64"];

#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const GLIBC_LOADER_FRAGMENTS: &[&str] = &[];
#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const MUSL_LOADER_FRAGMENTS: &[&str] = &[];

// ── Persisted host-capability record ──────────────────────────────────────

/// Persisted-record TTL; bounds only a libc *added* since the record was written.
///
/// A removed or replaced one invalidates through [`HostCapabilityRecord::evidence_still_holds`].
/// Rationale: `adr_platform_libc_os_features.md` § Rationale from code: ocx_oci.
#[cfg(target_os = "linux")]
const TTL_SECS: u64 = 86_400;

/// On-disk record version; an unknown value fails to parse, a clean miss, so bumping it is the whole migration.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
enum RecordVersion {
    V2 = 2,
}

/// A loader file's `stat` identity when it classified: tells a replaced loader from the same one.
///
/// Not a content hash: hashing costs a fifth of the record's whole saving, and
/// every ordinary replacement moves inode, mtime, device or size.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LoaderIdentity {
    device: u64,
    inode: u64,
    size: u64,
    mtime_seconds: i64,
    /// Catches an in-place rewrite of identical length.
    mtime_nanoseconds: i64,
}

#[cfg(target_os = "linux")]
impl LoaderIdentity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            mtime_seconds: metadata.mtime(),
            mtime_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

/// One loader that classified positive, and the evidence that it did.
#[cfg(target_os = "linux")]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LoaderRecord {
    path: String,
    /// The tag, not a serialized `LibcFlavor`, or a second encoding drifts from the one mapping.
    feature: String,
    identity: LoaderIdentity,
}

/// A detection result persisted at `$OCX_HOME/state/host/capabilities.json`.
///
/// Fail-open: any unusable record is a miss and re-detects, never an error.
/// `deny_unknown_fields` refuses a record this writer could not have produced.
#[cfg(target_os = "linux")]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostCapabilityRecord {
    version: RecordVersion,
    /// The record's only claim; families derive from it, so one no loader produced is unrepresentable.
    loaders: Vec<LoaderRecord>,
    detected_at: std::time::SystemTime,
    /// Clamped to [`TTL_SECS`] on read.
    ttl_seconds: u64,
}

#[cfg(target_os = "linux")]
impl HostCapabilityRecord {
    /// Capture a detection pass stamped now, or `None` when a loader vanished
    /// mid-detection: a partial answer would be honoured for the whole TTL.
    async fn capture(detection: &Detection) -> Option<Self> {
        let mut loaders = Vec::with_capacity(detection.classified.len());
        for (path, flavor) in &detection.classified {
            let metadata = match tokio::fs::metadata(path).await {
                Ok(metadata) => metadata,
                Err(error) => {
                    tracing::debug!(
                        "{} changed while detection ran ({error}); not recording the host libc set",
                        path.display()
                    );
                    return None;
                }
            };
            loaders.push(LoaderRecord {
                path: path.to_string_lossy().into_owned(),
                feature: flavor.os_feature_tag(),
                identity: LoaderIdentity::of(&metadata),
            });
        }
        Some(Self {
            version: RecordVersion::V2,
            loaders,
            detected_at: std::time::SystemTime::now(),
            ttl_seconds: TTL_SECS,
        })
    }

    /// The families the recorded loaders support; an unrecognised tag is dropped, never believed.
    fn libcs(&self) -> BTreeSet<LibcFlavor> {
        decode_libc_tags(self.loaders.iter().map(|loader| &loader.feature))
    }

    /// True inside the clamped TTL; a future `detected_at` reads as stale, never as fresh forever.
    fn is_fresh(&self) -> bool {
        match std::time::SystemTime::now().duration_since(self.detected_at) {
            Ok(elapsed) => elapsed < std::time::Duration::from_secs(self.ttl_seconds.min(TTL_SECS)),
            Err(_) => false,
        }
    }

    /// True while every recorded loader is still the same file at the same path.
    ///
    /// Compares identity, not presence: a replaced libc keeps its path, and a
    /// stale record selects an artifact that cannot launch.
    async fn evidence_still_holds(&self) -> bool {
        for loader in &self.loaders {
            let Ok(metadata) = tokio::fs::metadata(&loader.path).await else {
                return false;
            };
            if LoaderIdentity::of(&metadata) != loader.identity {
                return false;
            }
        }
        true
    }
}

/// Read the record at `path`, or `None` for any reason it cannot be used.
#[cfg(target_os = "linux")]
async fn read_record(path: &std::path::Path) -> Option<HostCapabilityRecord> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let record: HostCapabilityRecord = match serde_json::from_slice(&bytes) {
        Ok(record) => record,
        Err(error) => {
            tracing::debug!("recorded host libc set is not readable by this ocx ({error}); re-detecting");
            return None;
        }
    };
    if !record.is_fresh() {
        return None;
    }
    if !record.evidence_still_holds().await {
        tracing::debug!("a loader the recorded host libc set rests on is gone or replaced; re-detecting");
        return None;
    }
    Some(record)
}

/// Persist `record` at `path`, best-effort: a read-only `$OCX_HOME` costs a slow command, never a failed one.
#[cfg(target_os = "linux")]
async fn write_record(path: std::path::PathBuf, record: &HostCapabilityRecord) {
    let Some(parent) = path.parent().map(std::path::Path::to_path_buf) else {
        return;
    };
    let bytes = match serde_json::to_vec(record) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::debug!("could not encode the host libc record: {error}");
            return;
        }
    };
    if let Err(error) = tokio::fs::create_dir_all(&parent).await {
        tracing::debug!(
            "could not create {} for the host libc record: {error}",
            parent.display()
        );
        return;
    }
    match tokio::task::spawn_blocking(move || ocx_util::fs::write_bytes_atomic(&path, &bytes)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!("could not write the host libc record: {error}"),
        Err(join_error) => tracing::debug!("host libc record write did not complete: {join_error}"),
    }
}

/// Detect, consulting and refreshing the persisted record at `record`.
///
/// `None` detects uncached: the static-command bypass builds no `FileStructure`.
#[cfg(target_os = "linux")]
async fn detect_with_persisted_record(record: Option<&std::path::Path>) -> HostCapabilities {
    // The test seam is checked before any disk access, so a forced libc set is
    // never written onto a real host's record and a record can never override
    // the seam.
    #[cfg(any(test, feature = "__testing"))]
    {
        if let Some(libcs) = test_libc_override() {
            return HostCapabilities { libcs };
        }
    }

    let Some(path) = record else {
        return HostCapabilities {
            libcs: run_detection().await.libcs(),
        };
    };
    if let Some(recorded) = read_record(path).await {
        return HostCapabilities {
            libcs: recorded.libcs(),
        };
    }
    let detection = run_detection().await;
    record_detection(path.to_path_buf(), &detection).await;
    HostCapabilities {
        libcs: detection.libcs(),
    }
}

/// Persist `detection` at `path`, unless it classified nothing: a degraded pass
/// reads back as vacuously valid, persisting "could not look" for the whole TTL.
///
/// ponytail: a host with genuinely no libc loader never caches, paying full
/// detection every invocation — a second record shape distinguishing a
/// completed pass from a degraded one would fix it, if it ever complains.
#[cfg(target_os = "linux")]
async fn record_detection(path: std::path::PathBuf, detection: &Detection) {
    if detection.classified.is_empty() {
        tracing::debug!(
            "detection classified no libc loader; not recording it — a degraded pass and an \
             empty host are indistinguishable in the record, so re-detect next invocation"
        );
        return;
    }
    if let Some(record) = HostCapabilityRecord::capture(detection).await {
        write_record(path, &record).await;
    }
}

#[cfg(not(target_os = "linux"))]
async fn detect_with_persisted_record(_record: Option<&std::path::Path>) -> HostCapabilities {
    HostCapabilities::detect().await
}

static CACHED_OS_FEATURES: OnceLock<Vec<String>> = OnceLock::new();

/// The process-cached `os_features`; empty before [`HostCapabilities::detect_and_cache`] runs.
pub(crate) fn cached_os_features() -> Vec<String> {
    CACHED_OS_FEATURES.get().cloned().unwrap_or_default()
}

/// The cached libc tags, sorted; the same cache the resolver selects against, so reports match it.
pub fn cached_libc_labels() -> Vec<String> {
    cached_os_features()
        .iter()
        .filter_map(|tag| match Feature::parse(tag) {
            Feature::Libc(flavor) => Some(flavor.os_feature_tag()),
            Feature::Other(_) => None,
        })
        .collect()
}

fn init_cache(capabilities: &HostCapabilities) {
    // A second init is a benign no-op; the first value holds for the process.
    let _ = CACHED_OS_FEATURES.set(capabilities.os_features());
}

// Unit tests for HostCapabilities. Detection probes the real filesystem
// (ld.so), so tests needing an actual host loader are `#[ignore]`d; the main
// vector uses the `__OCX_TESTING_LIBC` short-circuit (`HostCapabilities::detect`'s
// doc comment) for reproducible CI results.

#[cfg(test)]
mod tests {
    use super::*;

    fn glibc_only() -> BTreeSet<LibcFlavor> {
        BTreeSet::from([LibcFlavor::Glibc])
    }

    fn musl_only() -> BTreeSet<LibcFlavor> {
        BTreeSet::from([LibcFlavor::Musl])
    }

    fn both() -> BTreeSet<LibcFlavor> {
        BTreeSet::from([LibcFlavor::Glibc, LibcFlavor::Musl])
    }

    // ── __OCX_TESTING_LIBC override cases ─────────────────────────────────

    #[tokio::test]
    async fn detect_with_ocx_test_libc_override_cases() {
        let env = ocx_env::overrides::lock();
        let key = &ocx_env::__OCX_TESTING_LIBC;
        env.set(key, "glibc");
        let caps = HostCapabilities::detect().await;
        assert_eq!(
            caps.libcs,
            glibc_only(),
            "__OCX_TESTING_LIBC=glibc must yield {{Glibc}}"
        );

        env.set(key, "musl");
        let caps = HostCapabilities::detect().await;
        assert_eq!(caps.libcs, musl_only(), "__OCX_TESTING_LIBC=musl must yield {{Musl}}");

        env.set(key, "glibc,musl");
        let caps = HostCapabilities::detect().await;
        assert_eq!(
            caps.libcs,
            both(),
            "__OCX_TESTING_LIBC=glibc,musl must yield {{Glibc, Musl}}"
        );
        assert_eq!(
            caps.os_features(),
            vec!["libc.glibc".to_string(), "libc.musl".to_string()],
            "dual-libc host must advertise both os.features tags, sorted"
        );

        env.set(key, "none");
        let caps = HostCapabilities::detect().await;
        assert!(caps.libcs.is_empty(), "__OCX_TESTING_LIBC=none must yield an empty set");
        assert!(caps.os_features().is_empty(), "empty set must yield empty os_features");

        env.set(key, "");
        let caps = HostCapabilities::detect().await;
        assert!(
            caps.libcs.is_empty(),
            "a set-but-empty __OCX_TESTING_LIBC must yield an empty set"
        );
    }

    // ── os_features() mapping ────────────────────────────────────────

    #[test]
    fn os_features_glibc_returns_libc_glibc_tag() {
        let caps = HostCapabilities { libcs: glibc_only() };
        assert_eq!(
            caps.os_features(),
            vec!["libc.glibc".to_string()],
            "Glibc must map to [\"libc.glibc\"]"
        );
    }

    #[test]
    fn os_features_musl_returns_libc_musl_tag() {
        let caps = HostCapabilities { libcs: musl_only() };
        assert_eq!(
            caps.os_features(),
            vec!["libc.musl".to_string()],
            "Musl must map to [\"libc.musl\"]"
        );
    }

    #[test]
    fn os_features_dual_libc_returns_both_tags_sorted() {
        let caps = HostCapabilities { libcs: both() };
        assert_eq!(
            caps.os_features(),
            vec!["libc.glibc".to_string(), "libc.musl".to_string()],
            "dual-libc host must map to both tags, sorted"
        );
    }

    #[test]
    fn os_features_empty_returns_none() {
        let caps = HostCapabilities { libcs: BTreeSet::new() };
        assert!(
            caps.os_features().is_empty(),
            "empty libc set must yield empty os_features"
        );
    }

    // ── LibcFlavor canonical tag mapping ─────────────────────────────

    #[test]
    fn os_feature_tag_renders_canonical_tags() {
        assert_eq!(LibcFlavor::Glibc.os_feature_tag(), "libc.glibc");
        assert_eq!(LibcFlavor::Musl.os_feature_tag(), "libc.musl");
        assert_eq!(
            LibcFlavor::Unknown("uclibc".to_string()).os_feature_tag(),
            "libc.uclibc"
        );
    }

    #[test]
    fn from_os_feature_tag_decodes_known_and_unknown() {
        assert_eq!(LibcFlavor::from_os_feature_tag("libc.glibc"), Some(LibcFlavor::Glibc));
        assert_eq!(LibcFlavor::from_os_feature_tag("libc.musl"), Some(LibcFlavor::Musl));
        assert_eq!(
            LibcFlavor::from_os_feature_tag("libc.uclibc"),
            Some(LibcFlavor::Unknown("uclibc".to_string())),
            "an unrecognised libc.* suffix must decode to Unknown carrying the suffix"
        );
    }

    #[test]
    fn from_os_feature_tag_rejects_non_libc_tags() {
        assert_eq!(
            LibcFlavor::from_os_feature_tag("gpu.cuda"),
            None,
            "a non-libc.* tag is not a libc tag"
        );
        assert_eq!(LibcFlavor::from_os_feature_tag("win32k"), None);
        assert_eq!(
            LibcFlavor::from_os_feature_tag("glibc"),
            None,
            "bare suffix without prefix is not a tag"
        );
    }

    #[test]
    fn os_feature_tag_round_trips_for_all_variants() {
        // The `Unknown("uclibc")` row reserves the `libc.*` namespace: an
        // inbound family OCX does not actively probe still parses losslessly.
        // Host detection never emits `Unknown` — only `Glibc` / `Musl`.
        for flavor in [
            LibcFlavor::Glibc,
            LibcFlavor::Musl,
            LibcFlavor::Unknown("uclibc".to_string()),
        ] {
            let tag = flavor.os_feature_tag();
            assert_eq!(
                LibcFlavor::from_os_feature_tag(&tag),
                Some(flavor.clone()),
                "tag round-trip failed for {flavor:?}"
            );
        }
    }

    // ── Feature lenient interpretation ───────────────────────────────

    #[test]
    fn feature_parse_libc_tags() {
        assert_eq!(Feature::parse("libc.glibc"), Feature::Libc(LibcFlavor::Glibc));
        assert_eq!(Feature::parse("libc.musl"), Feature::Libc(LibcFlavor::Musl));
        assert_eq!(
            Feature::parse("libc.uclibc"),
            Feature::Libc(LibcFlavor::Unknown("uclibc".to_string())),
            "an unrecognised libc.* feature is still a Libc feature, carried as Unknown"
        );
    }

    #[test]
    fn feature_parse_non_libc_is_other() {
        assert_eq!(Feature::parse("gpu.cuda"), Feature::Other("gpu.cuda".to_string()));
        assert_eq!(Feature::parse("win32k"), Feature::Other("win32k".to_string()));
    }

    // ── Non-Linux platform (compile-time gate) ───────────────────────

    /// On non-Linux platforms, detect() must return an empty set without
    /// spawning subprocesses. Compiled and exercised on every platform; the
    /// assertion holds on all targets because __OCX_TESTING_LIBC is not set in
    /// this test (we rely on impl to return an empty set on non-Linux).
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn detect_on_non_linux_returns_empty() {
        let caps = HostCapabilities::detect().await;
        assert!(caps.libcs.is_empty(), "detect() on non-Linux must return an empty set");
    }

    // ── Real filesystem probe cases (ignored — require real host loader) ──

    /// Alpine+gcompat: ld.so identity is the musl linker; gcompat does NOT
    /// promote to glibc (the glibc probe requires a real glibc banner, which
    /// the gcompat stub does not emit).
    /// To run: install gcompat on an Alpine container and un-ignore.
    /// Ref: ADR §"gcompat / equivalents"; predictability rule — identity, not
    /// equivalence.
    #[tokio::test]
    #[ignore = "requires real Alpine+gcompat host; exercises ld.so probe path"]
    async fn detect_on_alpine_gcompat_host_returns_musl_only() {
        // __OCX_TESTING_LIBC unset — real probe.
        let caps = HostCapabilities::detect().await;
        assert_eq!(
            caps.libcs,
            musl_only(),
            "Alpine+gcompat host must detect as musl only (identity, not equivalence)"
        );
    }

    /// NixOS: no ld.so at canonical paths; detection must return an empty set
    /// gracefully (and debug-log when /nix exists).
    #[tokio::test]
    #[ignore = "requires NixOS or a container with no /lib/ld-linux-*.so paths"]
    async fn detect_on_nixos_returns_empty() {
        let caps = HostCapabilities::detect().await;
        assert!(
            caps.libcs.is_empty(),
            "NixOS/minimal container with no canonical loader paths must yield an empty set"
        );
    }

    /// Corrupt --version output: detection must return an empty set without
    /// panicking.
    #[tokio::test]
    #[ignore = "requires a loader that outputs corrupt --version; exercises error-handling path"]
    async fn detect_with_corrupt_loader_output_returns_empty_no_panic() {
        // __OCX_TESTING_LIBC unset — exercises the real error path in detect().
        let caps = HostCapabilities::detect().await;
        assert!(caps.libcs.is_empty(), "corrupt loader output must yield an empty set");
    }

    // ── Discovery-then-identify unit tests (Linux-only internals) ─────────
    //
    // These exercise the discovery/identification helpers directly. They are
    // gated to Linux because the helpers only exist there; the banner-only
    // classification cases are architecture-independent, the name-heuristic and
    // glob-filter cases are gated further to the supported architectures.

    /// `read_pt_interp` extracts the loader path from a dynamically linked ELF.
    /// The Rust test binary itself is dynamically linked on the standard
    /// `*-unknown-linux-gnu` toolchain, so it carries a `PT_INTERP`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn read_pt_interp_extracts_loader_from_dynamic_binary() {
        let exe = std::env::current_exe().expect("test binary path");
        let interp = read_pt_interp(exe.to_str().expect("utf-8 exe path")).await;
        let loader = interp.expect("dynamically linked test binary must carry a PT_INTERP");
        assert!(
            loader.starts_with('/'),
            "PT_INTERP must be an absolute path: {loader:?}"
        );
        assert!(
            loader.contains("ld-"),
            "PT_INTERP must name a dynamic loader: {loader:?}"
        );
    }

    /// A present non-ELF file yields `None` (parse fails), never a panic.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn read_pt_interp_returns_none_for_non_elf_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("not-an-elf");
        tokio::fs::write(&file, b"clearly not an ELF binary")
            .await
            .expect("write fixture");
        assert_eq!(
            read_pt_interp(file.to_str().expect("utf-8 path")).await,
            None,
            "a non-ELF file must yield None"
        );
    }

    /// A missing file yields `None` (read fails), never a panic.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn read_pt_interp_returns_none_for_missing_file() {
        assert_eq!(read_pt_interp("/nonexistent/ocx-no-such-binary").await, None);
    }

    /// glibc banners (`GNU libc`, `GLIBC`) classify as `Glibc` regardless of
    /// the loader filename or exit code.
    #[cfg(target_os = "linux")]
    #[test]
    fn classify_loader_banner_identifies_glibc() {
        assert_eq!(
            classify_loader_banner(
                "ld.so (GNU libc) stable release version 2.39",
                "ld-linux-x86-64.so.2",
                Some(0),
            ),
            BannerClass::Identified(LibcFlavor::Glibc),
        );
        assert_eq!(
            classify_loader_banner("Used GLIBC 2.31 symbols", "anything", Some(0)),
            BannerClass::Identified(LibcFlavor::Glibc),
        );
    }

    /// musl's banner classifies as `Musl` even though its loader exits non-zero
    /// by design (exit status ignored).
    #[cfg(target_os = "linux")]
    #[test]
    fn classify_loader_banner_identifies_musl() {
        assert_eq!(
            classify_loader_banner("musl libc (x86_64)\nVersion 1.2.5", "ld-musl-x86_64.so.1", Some(1)),
            BannerClass::Identified(LibcFlavor::Musl),
        );
    }

    /// A gcompat stub sits at the glibc loader path but prints the musl banner.
    /// Banner wins over filename → `Musl` ("identity, not equivalence").
    #[cfg(target_os = "linux")]
    #[test]
    fn classify_loader_banner_gcompat_stub_classifies_as_musl() {
        assert_eq!(
            classify_loader_banner("musl libc (x86_64)", "ld-linux-x86-64.so.2", Some(1)),
            BannerClass::Identified(LibcFlavor::Musl),
            "gcompat stub at the glibc path must classify as musl by its banner"
        );
    }

    /// Output that matches no banner classifies as `Unrecognized`.
    #[cfg(target_os = "linux")]
    #[test]
    fn classify_loader_banner_junk_is_unrecognized() {
        assert_eq!(
            classify_loader_banner("totally unrelated output", "ld-linux-x86-64.so.2", Some(0)),
            BannerClass::Unrecognized,
        );
    }

    /// Ubuntu 20.04 quirk: a glibc-looking loader that exits 127 with no banner
    /// defers to the `/bin/true` confirmation; a non-glibc name does not.
    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[test]
    fn classify_loader_banner_exit_127_glibc_name_needs_confirmation() {
        let glibc_name = format!("{}.so.2", GLIBC_LOADER_FRAGMENTS[0]);
        assert_eq!(
            classify_loader_banner("", &glibc_name, Some(127)),
            BannerClass::GlibcNeedsConfirmation,
            "exit 127 with a glibc-looking loader name defers to the /bin/true confirmation"
        );
        // Exit 127 with a non-glibc name is not a glibc confirmation candidate.
        let musl_name = format!("{}.so.1", MUSL_LOADER_FRAGMENTS[0]);
        assert_eq!(
            classify_loader_banner("", &musl_name, Some(127)),
            BannerClass::Unrecognized,
        );
    }

    /// The glob arch filter accepts current-arch loader names and rejects
    /// foreign-arch loaders and non-loader files.
    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[test]
    fn is_current_arch_loader_name_filters_by_architecture() {
        let glibc_name = format!("{}.so.2", GLIBC_LOADER_FRAGMENTS[0]);
        let musl_name = format!("{}.so.1", MUSL_LOADER_FRAGMENTS[0]);
        assert!(
            is_current_arch_loader_name(&glibc_name),
            "current-arch glibc loader accepted"
        );
        assert!(
            is_current_arch_loader_name(&musl_name),
            "current-arch musl loader accepted"
        );
        // A foreign architecture's loader name must be rejected.
        assert!(!is_current_arch_loader_name("ld-linux-sparc64.so.1"));
        // Non-loader files are rejected.
        assert!(!is_current_arch_loader_name("libc.so.6"));
        assert!(!is_current_arch_loader_name("README"));
    }

    /// On x86_64 the aarch64 loader is foreign and must be filtered out.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn is_current_arch_loader_name_rejects_foreign_aarch64_on_x86_64() {
        assert!(!is_current_arch_loader_name("ld-linux-aarch64.so.1"));
    }

    /// `dedup_unseen` reports the first sighting of a canonical path as unseen
    /// and every subsequent sighting as seen.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dedup_unseen_reports_first_sighting_only() {
        let mut seen = std::collections::HashSet::new();
        let path = std::path::Path::new("/nonexistent/ocx-dedup-probe.so");
        assert!(dedup_unseen(path, &mut seen).await, "first sighting is unseen");
        assert!(!dedup_unseen(path, &mut seen).await, "second sighting is seen");
    }

    /// Discovery deduplicates overlapping sources: no two returned paths
    /// canonicalize to the same target.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn discover_loader_paths_has_no_duplicate_canonical_paths() {
        let paths = discover_loader_paths().await;
        let mut canonical = std::collections::HashSet::new();
        for path in &paths {
            let resolved = tokio::fs::canonicalize(path).await.unwrap_or_else(|_| path.clone());
            assert!(
                canonical.insert(resolved.clone()),
                "discovery returned a duplicate canonical loader path: {resolved:?}"
            );
        }
    }

    // ── Directory-scan root dedup (usrmerge) ─────────────────────────────

    /// A usrmerge host spells one tree two ways (`/lib` -> `/usr/lib`), and the
    /// scan must walk it once. Exercised against a tempdir rather than the real
    /// FHS paths so the assertion holds on a non-usrmerge host too.
    #[cfg(target_os = "linux")]
    #[test]
    fn dedup_scan_roots_collapses_a_symlinked_duplicate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("usr").join("lib");
        std::fs::create_dir_all(&real).expect("create real dir");
        let link = dir.path().join("lib");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let real_text = real.to_str().expect("utf-8 path");
        let link_text = link.to_str().expect("utf-8 path");
        let roots = dedup_scan_roots(&[link_text, real_text]);
        assert_eq!(
            roots,
            vec![link],
            "a symlink and its target name one tree: only the first spelling is scanned"
        );
    }

    /// Two genuinely distinct trees are both kept — the dedup must not turn a
    /// non-usrmerge host into a half-scanned one.
    #[cfg(target_os = "linux")]
    #[test]
    fn dedup_scan_roots_keeps_distinct_trees() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("lib");
        let second = dir.path().join("lib64");
        std::fs::create_dir_all(&first).expect("create first");
        std::fs::create_dir_all(&second).expect("create second");

        let roots = dedup_scan_roots(&[
            first.to_str().expect("utf-8 path"),
            second.to_str().expect("utf-8 path"),
        ]);
        assert_eq!(roots, vec![first, second], "distinct trees are both scanned");
    }

    /// Two paths that do not exist canonicalize to nothing, so they must fall
    /// back to their literal identity rather than collapsing together.
    #[cfg(target_os = "linux")]
    #[test]
    fn dedup_scan_roots_keeps_distinct_missing_paths_apart() {
        let roots = dedup_scan_roots(&["/nonexistent/ocx-scan-a", "/nonexistent/ocx-scan-b"]);
        assert_eq!(roots.len(), 2, "two missing paths are two identities, not one");
    }

    // ── Persisted host-capability record ─────────────────────────────────

    /// A glibc record capturing `loaders`, each of which must exist — the
    /// capture reads their file identities.
    #[cfg(target_os = "linux")]
    async fn record_for(loaders: Vec<String>) -> HostCapabilityRecord {
        HostCapabilityRecord::capture(&Detection {
            classified: loaders
                .into_iter()
                .map(|path| (std::path::PathBuf::from(path), LibcFlavor::Glibc))
                .collect(),
        })
        .await
        .expect("every loader in the fixture exists, so capture must succeed")
    }

    /// Create a file that stands in for a dynamic loader.
    #[cfg(target_os = "linux")]
    async fn write_fake_loader(path: &std::path::Path, contents: &[u8]) {
        tokio::fs::write(path, contents).await.expect("write fake loader");
    }

    /// A written record reads back with the same libc set.
    ///
    /// Linux-only like every helper it calls: `record_for`, `write_fake_loader`,
    /// `write_record` and `read_record` are all `cfg(target_os = "linux")`,
    /// because the record only exists where libc detection does.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn record_round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The record's own loader must exist for the read to honour it, so
        // point it at a file this test controls.
        let loader = dir.path().join("ld-fake.so");
        write_fake_loader(&loader, b"not really a loader").await;
        let path = dir.path().join("state").join("host").join("capabilities.json");

        let record = record_for(vec![loader.to_string_lossy().into_owned()]).await;
        write_record(path.clone(), &record).await;
        let loaded = read_record(&path).await.expect("a fresh record must read back");
        assert_eq!(
            loaded.libcs(),
            glibc_only(),
            "the recorded libc set must survive the round trip"
        );
        assert_eq!(
            loaded
                .loaders
                .iter()
                .map(|entry| entry.feature.as_str())
                .collect::<Vec<_>>(),
            vec!["libc.glibc"],
            "each recorded loader carries the canonical os.features tag it classified as"
        );
    }

    /// A degraded pass classifies nothing: the directory scan can lose its
    /// `spawn_blocking` join, and every probe can hit `PROBE_TIMEOUT` on a
    /// loaded runner. The record has no shape for "could not look", and an
    /// empty loader list is vacuously valid on read, so latching one would
    /// answer `os.features` with the empty set until the TTL expired —
    /// `FeatureMismatch`, exit 65, on every install for an hour.
    ///
    /// Both polarities are pinned. Without the positive control the guard could
    /// degenerate into "never record" and pass just as well.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_detection_that_classified_nothing_is_not_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state").join("host").join("capabilities.json");

        record_detection(path.clone(), &Detection::default()).await;
        assert!(
            !tokio::fs::try_exists(&path).await.expect("stat the record path"),
            "a pass that classified nothing must leave no record — it is indistinguishable from \
             a pass that could not look, and reading it back would pin the empty libc set for a \
             full TTL"
        );

        let loader = dir.path().join("ld-fake.so");
        write_fake_loader(&loader, b"the loader that classified as glibc").await;
        record_detection(
            path.clone(),
            &Detection {
                classified: vec![(loader, LibcFlavor::Glibc)],
            },
        )
        .await;
        assert_eq!(
            read_record(&path)
                .await
                .expect("a pass that classified a loader must be recorded")
                .libcs(),
            glibc_only(),
            "the guard must refuse the empty answer only, never suppress recording outright"
        );
    }

    /// The gate that closes the dangerous staleness direction: a record naming a
    /// loader that has since been uninstalled must be a miss, so the next
    /// invocation re-detects instead of selecting an artifact that cannot launch.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn record_naming_a_removed_loader_is_a_miss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let loader = dir.path().join("ld-fake.so");
        write_fake_loader(&loader, b"not really a loader").await;
        let path = dir.path().join("state").join("host").join("capabilities.json");
        let record = record_for(vec![loader.to_string_lossy().into_owned()]).await;
        write_record(path.clone(), &record).await;

        // Green while the loader is present...
        assert!(
            read_record(&path).await.is_some(),
            "a record whose loaders all exist must be honoured"
        );

        // ...and a miss the moment it is gone. Same record, same clock — only
        // the loader changed, which is exactly what uninstalling a libc does.
        tokio::fs::remove_file(&loader).await.expect("remove loader");
        assert!(
            read_record(&path).await.is_none(),
            "a record naming a loader that no longer exists must not be honoured"
        );
    }

    /// A dual-libc host records both families and decodes both back. The
    /// record must not be able to collapse `{Glibc, Musl}` into one family —
    /// that would silently narrow which artifacts resolve on a host that can
    /// run both.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn record_round_trips_a_dual_libc_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let glibc_loader = dir.path().join("ld-linux-fake.so.2");
        let musl_loader = dir.path().join("ld-musl-fake.so.1");
        write_fake_loader(&glibc_loader, b"not really a loader").await;
        // A different length, as two real libc loaders always have.
        write_fake_loader(&musl_loader, b"not really a loader either, and a different size").await;
        let path = dir.path().join("state").join("host").join("capabilities.json");

        let detection = Detection {
            classified: vec![
                (glibc_loader.clone(), LibcFlavor::Glibc),
                (musl_loader.clone(), LibcFlavor::Musl),
            ],
        };
        let record = HostCapabilityRecord::capture(&detection)
            .await
            .expect("both fixture loaders exist");
        write_record(path.clone(), &record).await;

        let loaded = read_record(&path)
            .await
            .expect("a fresh dual-libc record must read back");
        assert_eq!(loaded.libcs(), both(), "both families must survive the round trip");
        assert_eq!(
            loaded
                .loaders
                .iter()
                .map(|entry| entry.feature.as_str())
                .collect::<Vec<_>>(),
            vec!["libc.glibc", "libc.musl"],
            "a dual-libc record records both loaders, each with the family it classified as"
        );

        // Losing EITHER loader invalidates the whole record: the surviving
        // family is still correct, but "which families does this host have" is
        // no longer a question the record can answer.
        tokio::fs::remove_file(&musl_loader).await.expect("remove musl loader");
        assert!(
            read_record(&path).await.is_none(),
            "removing one of two loaders must invalidate the record, not silently keep the other"
        );
    }

    /// A missing record file is a miss, not an error.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn absent_record_is_a_miss() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_record(&dir.path().join("nope.json")).await.is_none());
    }

    /// A record that cannot be decoded is a miss, not an error — that is how a
    /// format change refreshes itself.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn corrupt_record_is_a_miss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("capabilities.json");
        tokio::fs::write(&path, b"{not json").await.expect("write junk");
        assert!(read_record(&path).await.is_none());
    }

    /// Both halves of the lifetime come off disk, so neither is trusted: an
    /// expired record is stale, one claiming a huge TTL is clamped rather than
    /// pinned forever, and one stamped in the future (rewound clock) is stale.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn record_freshness_is_clamped_in_both_directions() {
        let mut record = record_for(Vec::new()).await;

        record.detected_at = std::time::SystemTime::now() - std::time::Duration::from_secs(TTL_SECS + 60);
        assert!(!record.is_fresh(), "a record past its TTL is stale");

        record.ttl_seconds = u64::MAX;
        assert!(
            !record.is_fresh(),
            "a record may shorten its own lifetime, never extend it past TTL_SECS"
        );

        record.ttl_seconds = TTL_SECS;
        record.detected_at = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        assert!(!record.is_fresh(), "a record stamped in the future is stale");

        record.detected_at = std::time::SystemTime::now();
        assert!(record.is_fresh(), "a just-written record is fresh");
    }

    /// The 2026-08-27 lengthening, pinned by the one observation that separates
    /// it from the hour it replaced.
    ///
    /// A cold detect is measured **over** the C-044 per-prompt budget
    /// (`test/bench/shell_latency.py`, Δ 3.659–4.732 ms against 3 ms), so every
    /// TTL expiry puts one real user prompt over budget. At one hour that was
    /// hourly, per host. Asserting the constant's value would be a tautology;
    /// asserting that a record written earlier the same day still answers is
    /// the property, and it is red at 3600 and green at 86400.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_record_written_hours_ago_is_still_fresh() {
        let mut record = record_for(Vec::new()).await;
        record.ttl_seconds = TTL_SECS;
        record.detected_at = std::time::SystemTime::now() - std::time::Duration::from_secs(6 * 3600);
        assert!(
            record.is_fresh(),
            "a capability record written six hours ago must still answer: re-detecting costs more \
             than the whole per-prompt budget, and the direction this clock bounds — a libc ADDED \
             since — is the recoverable one (FeatureMismatch, exit 65, self-diagnosing)"
        );
    }

    // ── Evidence binding: a claim needs a loader behind it ───────────────

    /// The vacuous-record defect. A syntactically valid record declaring a libc
    /// while recording no loader that classified as one passed every check under
    /// the old two-independent-lists format: an existence check over an empty
    /// loader list is vacuously true, so `os_features` was believed outright and
    /// OCX would select glibc artifacts on a host it had never probed.
    ///
    /// The fix is structural rather than a new check — the feature set is
    /// derived from the recorded evidence — so this test pins both halves: the
    /// old shape is refused, and the current shape cannot express the claim.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_record_claiming_a_libc_it_has_no_evidence_for_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("capabilities.json");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after the epoch")
            .as_secs();

        // Exactly the record the adversarial gate described: well-formed,
        // stamped now, inside the TTL, declaring glibc, backed by nothing.
        let forged = format!(
            concat!(
                r#"{{"os_features":["libc.glibc"],"loaders":[],"#,
                r#""detected_at":{{"secs_since_epoch":{now},"nanos_since_epoch":0}},"#,
                r#""ttl_seconds":3600}}"#
            ),
            now = now
        );
        tokio::fs::write(&path, forged.as_bytes())
            .await
            .expect("write the forged record");
        assert!(
            read_record(&path).await.is_none(),
            "a record declaring a libc no recorded loader classified as must be a miss, \
             so the caller probes instead of selecting artifacts for a libc that may not be here"
        );

        // And in the current format the claim has nowhere to live: an empty
        // evidence list parses, and asserts nothing.
        let evidence_free = format!(
            concat!(
                r#"{{"version":2,"loaders":[],"#,
                r#""detected_at":{{"secs_since_epoch":{now},"nanos_since_epoch":0}},"#,
                r#""ttl_seconds":3600}}"#
            ),
            now = now
        );
        tokio::fs::write(&path, evidence_free.as_bytes())
            .await
            .expect("write the evidence-free record");
        let loaded = read_record(&path)
            .await
            .expect("a host on which nothing classified is a valid recorded answer");
        assert!(
            loaded.libcs().is_empty(),
            "no evidence must mean no claim — never a family the record was not given a loader for"
        );
    }

    /// Each guard on "this writer could have produced this file" is proven to
    /// red on its own, so neither is silently doing the other's work: a stale
    /// `version` is refused with no stray fields present, and a stray field is
    /// refused with the correct `version` present.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_record_this_writer_could_not_have_produced_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("capabilities.json");
        let loader = dir.path().join("ld-fake.so");
        write_fake_loader(&loader, b"the loader that classified as glibc").await;
        let record = record_for(vec![loader.to_string_lossy().into_owned()]).await;
        let bytes = serde_json::to_vec(&record).expect("encode record");
        let json = String::from_utf8(bytes).expect("record is UTF-8");

        // Control: unmodified, it reads back.
        tokio::fs::write(&path, json.as_bytes()).await.expect("write record");
        assert!(
            read_record(&path).await.is_some(),
            "the unmodified record must be honoured, or the two mutations below prove nothing"
        );

        // Version axis alone: same bytes, older format tag.
        let older = json.replace(r#""version":2"#, r#""version":1"#);
        assert_ne!(older, json, "the version mutation must land");
        tokio::fs::write(&path, older.as_bytes())
            .await
            .expect("write v1 record");
        assert!(
            read_record(&path).await.is_none(),
            "a record from another format generation must be a miss, not parsed by guesswork"
        );

        // Unknown-field axis alone: correct version, one key this writer never
        // emits — including the `os_features` key the old format carried.
        let stray = json.replace(r#""version":2"#, r#""version":2,"os_features":["libc.glibc"]"#);
        assert_ne!(stray, json, "the stray-field mutation must land");
        tokio::fs::write(&path, stray.as_bytes())
            .await
            .expect("write stray-field record");
        assert!(
            read_record(&path).await.is_none(),
            "a field this writer never emits means the file came from somewhere else; probe, do not read"
        );
    }

    // ── Loader identity: existence is not freshness ──────────────────────

    /// Replacing a libc **in place** keeps the loader's path, so an existence
    /// check cannot see it — yet the executable there may now belong to a
    /// different libc. This fixture isolates the mtime axis: same inode, same
    /// byte length, contents rewritten.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_loader_rewritten_in_place_invalidates_the_record() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let loader = dir.path().join("ld-fake.so");
        write_fake_loader(&loader, b"the loader that classified as glibc").await;
        let before = tokio::fs::metadata(&loader).await.expect("stat the loader");

        let path = dir.path().join("state").join("host").join("capabilities.json");
        let record = record_for(vec![loader.to_string_lossy().into_owned()]).await;
        write_record(path.clone(), &record).await;
        assert!(
            read_record(&path).await.is_some(),
            "a record whose loader is untouched must be honoured"
        );

        // Same path, same length, different content — a reinstall that writes
        // through the existing inode.
        write_fake_loader(&loader, b"a DIFFERENT libc altogether samelen").await;
        let after = tokio::fs::metadata(&loader).await.expect("stat the replacement");
        assert_eq!(
            after.len(),
            before.len(),
            "the fixture must isolate the mtime axis: equal sizes"
        );
        assert_eq!(
            after.ino(),
            before.ino(),
            "the fixture must isolate the mtime axis: equal inodes"
        );
        assert_ne!(
            after.modified().expect("mtime"),
            before.modified().expect("mtime"),
            "an in-place rewrite must move the mtime, or this test proves nothing"
        );

        assert!(
            read_record(&path).await.is_none(),
            "a loader replaced in place must invalidate the record — the path surviving says nothing \
             about what now executes there"
        );
    }

    /// The shape an ordinary package install takes: write the new file beside
    /// the old one, then `rename` over it. This fixture restores the original
    /// length *and* mtime on the replacement, so the inode is the only field
    /// left to notice the swap.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_loader_replaced_by_rename_invalidates_the_record() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let loader = dir.path().join("ld-fake.so");
        write_fake_loader(&loader, b"the loader that classified as glibc").await;
        let before = tokio::fs::metadata(&loader).await.expect("stat the loader");

        let path = dir.path().join("state").join("host").join("capabilities.json");
        let record = record_for(vec![loader.to_string_lossy().into_owned()]).await;
        write_record(path.clone(), &record).await;
        assert!(
            read_record(&path).await.is_some(),
            "a record whose loader is untouched must be honoured"
        );

        let replacement = dir.path().join("ld-fake.so.new");
        write_fake_loader(&replacement, b"a DIFFERENT libc altogether samelen").await;
        let times = std::fs::FileTimes::new()
            .set_accessed(before.accessed().expect("atime"))
            .set_modified(before.modified().expect("mtime"));
        let handle = std::fs::File::options()
            .write(true)
            .open(&replacement)
            .expect("open the replacement");
        handle.set_times(times).expect("restore the original timestamps");
        drop(handle);
        tokio::fs::rename(&replacement, &loader)
            .await
            .expect("rename over the loader");

        let after = tokio::fs::metadata(&loader).await.expect("stat the replacement");
        assert_eq!(
            after.len(),
            before.len(),
            "the fixture must isolate the inode axis: equal sizes"
        );
        assert_eq!(
            after.modified().expect("mtime"),
            before.modified().expect("mtime"),
            "the fixture must isolate the inode axis: equal mtimes"
        );
        assert_ne!(
            after.ino(),
            before.ino(),
            "a rename-replace must move the inode, or this test proves nothing"
        );

        assert!(
            read_record(&path).await.is_none(),
            "a loader replaced by rename must invalidate the record even when the timestamps are restored"
        );
    }

    /// Every real loader path is a symlink (`/lib64/ld-linux-x86-64.so.2` →
    /// the versioned file), so retargeting one is how a container-layer swap or
    /// an alternatives switch changes what executes without touching the
    /// recorded path at all.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_loader_symlink_retargeted_invalidates_the_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let glibc_file = dir.path().join("ld-linux-real.so.2");
        let other_file = dir.path().join("ld-musl-real.so.1");
        write_fake_loader(&glibc_file, b"the loader that classified as glibc").await;
        write_fake_loader(&other_file, b"a DIFFERENT libc altogether samelen").await;
        let loader = dir.path().join("ld-fake.so");
        tokio::fs::symlink(&glibc_file, &loader)
            .await
            .expect("link the loader path at the glibc file");

        let path = dir.path().join("state").join("host").join("capabilities.json");
        let record = record_for(vec![loader.to_string_lossy().into_owned()]).await;
        write_record(path.clone(), &record).await;
        assert!(
            read_record(&path).await.is_some(),
            "a record whose loader symlink still points at the probed file must be honoured"
        );

        tokio::fs::remove_file(&loader).await.expect("drop the old link");
        tokio::fs::symlink(&other_file, &loader)
            .await
            .expect("retarget the loader path");
        assert!(
            read_record(&path).await.is_none(),
            "retargeting the loader symlink must invalidate the record — the recorded path is \
             unchanged but a different file executes there now"
        );
    }

    // ── Real-host markers (ignored; un-ignore to run on the named host) ───

    /// NixOS without nix-ld: the native loader lives under `/nix/store`. The
    /// PT_INTERP discovery source reads it from a system binary, so detection
    /// reports the real family (glibc on stock NixOS) where the old FHS-only
    /// allowlist found nothing. To run: execute on a NixOS box and un-ignore.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires a real NixOS host; exercises PT_INTERP /nix/store discovery"]
    async fn detect_on_nixos_via_pt_interp_reports_glibc() {
        let caps = HostCapabilities::detect().await;
        assert!(
            caps.libcs.contains(&LibcFlavor::Glibc),
            "stock NixOS links glibc; PT_INTERP discovery must find the /nix/store loader"
        );
    }

    /// Gentoo Prefix: the loader lives under the prefix root, not an FHS path.
    /// PT_INTERP discovery must still find it. To run: execute inside a Gentoo
    /// Prefix and un-ignore.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires a Gentoo Prefix host; exercises non-FHS PT_INTERP discovery"]
    async fn detect_on_gentoo_prefix_via_pt_interp_reports_glibc() {
        let caps = HostCapabilities::detect().await;
        assert!(
            caps.libcs.contains(&LibcFlavor::Glibc),
            "Gentoo Prefix glibc must be discovered via PT_INTERP, not the FHS allowlist"
        );
    }
}
