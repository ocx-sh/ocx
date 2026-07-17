// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Host libc detection for platform-aware OCI index resolution.
//!
//! Enumerates the libc families present on the current host and maps them to
//! `os.features` tag values consumed by [`super::is_compatible`].
//!
//! ## Self-linkage vs. host capability
//!
//! OCX installs **foreign** binaries, so the question detection answers is
//! "what libc families can this host execute?" — NOT "what is ocx itself
//! linked against?". These are orthogonal: a static-musl ocx on a pure-glibc
//! Ubuntu host still runs glibc binaries fine. That rules out self-linkage
//! probes (`/proc/self/maps`, `dl_iterate_phdr`) — those report ocx's own
//! compile-time linkage, not host capability. OCX discovers the host's dynamic
//! loaders on disk (read a system binary's `PT_INTERP`, scan canonical loader
//! directories) and probes each instead.
//!
//! ## Detection algorithm — discovery-then-identify, set union
//!
//! Detection runs in two stages. **Discovery** produces a deduplicated set of
//! candidate loader paths from three sources, in priority order:
//!
//! 1. **`PT_INTERP` (primary).** Read the ELF program headers of an ordered
//!    allowlist of guaranteed-present, dynamically linked system binaries
//!    (`/usr/bin/env`, `/bin/sh`, `/bin/ls`) and extract the `PT_INTERP`
//!    string — the host's exact native loader path. This works wherever the
//!    loader lives, including non-FHS layouts (NixOS `/nix/store`, Gentoo
//!    Prefix, Homebrew-on-Linux, custom sysroots). A statically linked binary
//!    (busybox `/bin/sh` on a minimal Alpine image) carries no `PT_INTERP` and
//!    is skipped.
//! 2. **Arch-filtered directory scan.** Scan the canonical loader directories
//!    (`/lib`, `/lib64`, `/usr/lib`, `/usr/lib64`, plus their immediate
//!    multiarch subdirectories) for loader files whose name matches the current
//!    architecture (`ld-linux-x86-64` / `ld-musl-x86_64` on x86_64). Catches
//!    multi-libc hosts the single-binary `PT_INTERP` step misses; foreign-arch
//!    loaders (dpkg-multiarch) are filtered out by the arch name fragment.
//! 3. **Hardcoded allowlist (fallback).** [`GLIBC_LOADERS`] / [`MUSL_LOADERS`]
//!    — a last resort for the rare host where neither source above fired.
//!
//! **Identification** then classifies each discovered loader purely by its
//! `--version` banner, independent of which source produced the path. Probes
//! run concurrently in a [`tokio::task::JoinSet`] and **every** positive result
//! is unioned into a sorted [`std::collections::BTreeSet`] of [`LibcFlavor`] —
//! no early abort, no "first probe to complete wins". A host with both glibc
//! and musl loaders (Ubuntu + `musl-tools`, multi-target CI runners) advertises
//! `{Glibc, Musl}`. Determinism falls out of the set plus its sorted iteration
//! order, independent of probe scheduling.
//!
//! Classifying by banner — not by which path the loader sits at — makes the
//! Alpine gcompat case fall out for free: the gcompat stub sits at the glibc
//! loader path but prints the **musl** banner, so it classifies as `Musl`. The
//! ADR "identity, not equivalence" rule is preserved by construction, not by a
//! special-case exclusion.
//!
//! Banner-parsing mechanics (the `--version` strings, musl's non-zero exit by
//! design, the Ubuntu 20.04 exit-127 → `{loader} /bin/true` confirmation) are
//! ported from cargo-bins/cargo-binstall `crates/detect-targets`; the discovery
//! stage and the set-union aggregation are OCX's.
//!
//! Detection is Linux-only; macOS and Windows always return an empty set
//! without spawning subprocesses.
//!
//! ## Known limitation — detect-env ≠ exec-env
//!
//! Detection answers "what libc can *this host* run?". When the resolved binary
//! ultimately runs in a *different* namespace (distrobox/toolbox, a
//! bind-mounted container, install-here-run-there), that target namespace may
//! provide a different libc set than the one detected here. OCX's normal
//! `ocx exec` runs on the same host/kernel, so the gap does not bite the common
//! path. Exec-time / target-namespace detection is deferred to a separate ADR.
//!
//! ## NixOS / empty result
//!
//! On NixOS without nix-ld the loaders live under `/nix/store/...` and nothing
//! sits at the FHS paths, so the set ends up empty. When `/nix` exists we
//! `tracing::debug!` a note and return the empty set — never an error. An
//! empty set yields empty `os_features`, degrading to `Any`-only matching;
//! the user can override with `--platform`. nix-ld installs an FHS shim at the
//! canonical loader path, so detection then works normally.
//!
//! See `.claude/artifacts/research_libc_detection_robustness.md` for the
//! discovery-mechanism comparison (PT_INTERP / scan / allowlist / ldconfig /
//! getconf / os-release) and the virtualization failure-mode survey, and
//! `research_libc_detection_methods.md` for the original probe model.
//!
//! ## Cache lifecycle
//!
//! One-shot CLI invocation assumption. Detection runs once at context init
//! and caches via `OnceLock` for the lifetime of the process. Embedders
//! using `ocx_lib` as a library or future daemon mode must invalidate or
//! work around the cache.
//!
//! ## Security
//!
//! `PT_INTERP` reads are constrained to a fixed allowlist of system binaries
//! ([`INTERP_PROBE_BINARIES`], never user-supplied paths); the loader path that
//! read yields, and every path from the directory scan and the hardcoded
//! allowlist, is spawned only with `--version` (or a `/bin/true` confirmation),
//! each bounded by [`PROBE_TIMEOUT`]. No detection input ever comes from user
//! data. Same threat model as cargo-binstall's `detect-targets`, widened only
//! to spawn the loader a present system binary names as its own interpreter.

use std::collections::BTreeSet;
use std::sync::OnceLock;

/// A libc family identified for the current host or read off a manifest.
///
/// v1 uses unit variants for the two families OCX detects ([`Glibc`](Self::Glibc),
/// [`Musl`](Self::Musl)). Future tuple forms (e.g. `Glibc(GlibcVersion)`) are
/// deferred — see the implementation plan notes. Migrating from unit to tuple
/// is a breaking API change; acceptable pre-1.0.
///
/// [`Unknown`](Self::Unknown) carries a `libc.*` tag suffix OCX does not
/// recognise. It never arises from host detection (the probes only ever emit
/// `Glibc` / `Musl`); it exists solely so *interpreting* an inbound `libc.*`
/// `os.features` tag is total and lossless. Unknown families carry no
/// semantic meaning for matching — they simply fail the subset check.
///
/// Derives `Ord`/`PartialOrd` so a [`BTreeSet<LibcFlavor>`] iterates in a
/// stable, deterministic order regardless of probe scheduling. The unit
/// variants sort first; `Unknown` sorts after them (and amongst itself by its
/// inner string), keeping iteration deterministic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LibcFlavor {
    /// GNU libc (glibc). Detected when a discovered dynamic loader's
    /// `--version` banner identifies it as glibc (`GNU libc` / `GLIBC`).
    Glibc,
    /// musl libc. Detected when a discovered dynamic loader's `--version`
    /// banner identifies it as musl (`musl libc`).
    Musl,
    /// A `libc.*` tag OCX does not recognise (e.g. `libc.uclibc`). Only
    /// produced when parsing a foreign `os.features` tag; never emitted by
    /// host detection. The inner string is the suffix after `libc.`.
    Unknown(String),
}

impl LibcFlavor {
    /// Render this family as its canonical `os.features` tag.
    ///
    /// - [`Glibc`](Self::Glibc) → `"libc.glibc"`
    /// - [`Musl`](Self::Musl) → `"libc.musl"`
    /// - [`Unknown(s)`](Self::Unknown) → `"libc.{s}"`
    ///
    /// This is the single source of truth for the forward (family → tag)
    /// direction; both [`HostCapabilities::os_features`] and
    /// [`cached_libc_labels`] route through it so the wire tags cannot drift
    /// from the reverse mapping in [`from_os_feature_tag`](Self::from_os_feature_tag).
    pub fn os_feature_tag(&self) -> String {
        match self {
            Self::Glibc => "libc.glibc".to_string(),
            Self::Musl => "libc.musl".to_string(),
            Self::Unknown(suffix) => format!("libc.{suffix}"),
        }
    }

    /// Parse a `libc.*` `os.features` tag back into a [`LibcFlavor`].
    ///
    /// Strips the `libc.` prefix, then maps `"glibc"` → [`Glibc`](Self::Glibc),
    /// `"musl"` → [`Musl`](Self::Musl), and any other suffix →
    /// [`Unknown`](Self::Unknown). A tag that does not start with `libc.`
    /// (e.g. `gpu.cuda`, `win32k`) is not a libc tag and yields `None` — it is
    /// the caller's job to treat that as a non-libc feature.
    ///
    /// This is the inverse of [`os_feature_tag`](Self::os_feature_tag) and the
    /// single source of truth for the reverse (tag → family) direction.
    pub fn from_os_feature_tag(tag: &str) -> Option<Self> {
        let suffix = tag.strip_prefix("libc.")?;
        Some(match suffix {
            "glibc" => Self::Glibc,
            "musl" => Self::Musl,
            other => Self::Unknown(other.to_string()),
        })
    }
}

/// A typed view of a single OCI `platform.os.features` tag.
///
/// `os.features` is an open string namespace: the `libc.*` slots carry libc
/// identity ([`Feature::Libc`]), and anything else is an opaque feature OCX
/// does not interpret ([`Feature::Other`]). Parsing is total and never errors —
/// unrecognised features simply carry no semantic meaning.
///
/// Used where features are *interpreted for reporting* (e.g. extracting the
/// host libc for `ocx about` / `ocx version`). Subset **matching** in the
/// index/platform resolution path stays string-based and is unaffected by this
/// model: an [`Other`](Self::Other) or [`Unknown`](LibcFlavor::Unknown) feature
/// just fails to match, it never errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feature {
    /// A `libc.*` feature, decoded into a [`LibcFlavor`] (known or
    /// [`Unknown`](LibcFlavor::Unknown)).
    Libc(LibcFlavor),
    /// Any non-`libc.*` feature, carried verbatim. OCX assigns it no meaning.
    Other(String),
}

impl Feature {
    /// Interpret a single `os.features` tag. Never errors.
    ///
    /// A `libc.*` tag becomes [`Feature::Libc`] (with the family decoded, or
    /// [`Unknown`](LibcFlavor::Unknown) for an unrecognised suffix). Every
    /// other tag becomes [`Feature::Other`] carrying the raw string.
    pub fn parse(tag: &str) -> Self {
        match LibcFlavor::from_os_feature_tag(tag) {
            Some(flavor) => Self::Libc(flavor),
            None => Self::Other(tag.to_string()),
        }
    }
}

/// Detected capabilities of the current host relevant to platform selection.
///
/// At v1 this carries only the set of libc families. Future fields (e.g. CPU
/// microarch feature level) may be added here under new ADRs.
#[derive(Debug, Clone)]
pub struct HostCapabilities {
    /// The set of libc families the host can execute. Empty when detection is
    /// not applicable (non-Linux) or found nothing (NixOS, corrupt output, no
    /// recognised loader). A `BTreeSet` gives deterministic, sorted iteration.
    pub libcs: BTreeSet<LibcFlavor>,
}

impl HostCapabilities {
    /// Detect host capabilities via discovery-then-identify probing.
    ///
    /// On Linux: discovers candidate loader paths (a system binary's
    /// `PT_INTERP` ∪ an arch-filtered directory scan ∪ the hardcoded
    /// allowlist), spawns each with `--version` (or a `/bin/true` fallback),
    /// and unions every positively identified libc family into the set.
    ///
    /// On non-Linux platforms (macOS, Windows): returns immediately with an
    /// empty set — no subprocesses are spawned.
    ///
    /// Failures (missing loaders, corrupt output, subprocess errors) are
    /// handled gracefully and contribute nothing to the set rather than
    /// producing an error.
    pub async fn detect() -> Self {
        // Test-only seam: `__OCX_TEST_LIBC` short-circuits the real probe so
        // acceptance tests can force a deterministic libc result in CI without
        // a real Alpine / glibc host. Gated behind `cfg(test)` or the
        // `__testing` Cargo feature so release artifacts physically lack the
        // path. Canonical project seam pattern — mirrors `__OCX_SELF_IMAGE` in
        // `package_manager/tasks/update_check.rs`. See `subsystem-tests.md`
        // "Test-Only Seams".
        //
        // Value is a comma-separated set of family tokens:
        //   "glibc"       → {Glibc}
        //   "musl"        → {Musl}
        //   "glibc,musl"  → {Glibc, Musl}
        //   "none" / ""   → {} (force undetected)
        // Once the var is set, the seam never falls through to the real probe.
        #[cfg(any(test, feature = "__testing"))]
        {
            if let Ok(value) = std::env::var("__OCX_TEST_LIBC") {
                return Self {
                    libcs: parse_test_libc_set(&value),
                };
            }
        }

        Self {
            libcs: detect_libcs().await,
        }
    }

    /// Map the detected libc set to `os.features` tag values for OCI platform
    /// matching.
    ///
    /// Return values:
    /// - `[]` — no libc detected; `Platform::current()` will leave
    ///   `os_features` empty, causing subset matching to accept only entries
    ///   with empty `os_features` (legacy un-tagged packages).
    /// - `["libc.glibc"]` — only GNU libc detected.
    /// - `["libc.musl"]` — only musl libc detected.
    /// - `["libc.glibc", "libc.musl"]` — a genuine dual-libc host.
    ///
    /// The returned `Vec` is sorted (the `BTreeSet` iterates in order), so a
    /// dual-libc host advertises every family it provides and matches both a
    /// `libc.glibc`- and a `libc.musl`-tagged index entry.
    pub fn os_features(&self) -> Vec<String> {
        self.libcs.iter().map(LibcFlavor::os_feature_tag).collect()
    }

    /// Detect host capabilities and populate the process-wide `os_features`
    /// cache consumed by [`Platform::current`](super::platform::Platform::current).
    ///
    /// This is the single entry point CLI context initialization calls at
    /// startup. Detection failure is not an error — an empty set caches as an
    /// empty `Vec`, which is a valid state (subset matching then accepts only
    /// entries with empty `os_features`). The cache is a one-shot `OnceLock`
    /// for the process lifetime; see the module-level "Cache lifecycle" note.
    ///
    /// Short-circuits to a no-op on any call after the first — the `OnceLock`
    /// is already set, so re-running the full loader-probe pipeline would be
    /// wasted work.
    pub async fn detect_and_cache() -> Self {
        // Fast path: cache already populated (2nd+ call in same process).
        // Reconstruct `HostCapabilities` from the cached feature tags instead
        // of re-running the discovery-then-identify pipeline.
        if let Some(cached) = CACHED_OS_FEATURES.get() {
            let libcs = cached
                .iter()
                .filter_map(|tag| LibcFlavor::from_os_feature_tag(tag))
                .filter(|f| !matches!(f, LibcFlavor::Unknown(_)))
                .collect();
            return Self { libcs };
        }
        let capabilities = Self::detect().await;
        init_cache(&capabilities);
        capabilities
    }
}

/// Parse the `__OCX_TEST_LIBC` seam value into a libc set.
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

/// Probe the host for every libc family it provides.
///
/// Returns an empty set immediately on non-Linux targets without spawning any
/// subprocess. On Linux it runs the discovery-then-identify pipeline: discover
/// candidate loader paths (a system binary's `PT_INTERP` ∪ an arch-filtered
/// directory scan ∪ the hardcoded allowlist, deduplicated by canonical path),
/// then classify each by its `--version` banner and union every positive into
/// the set — no early abort, no first-wins.
#[cfg(target_os = "linux")]
async fn detect_libcs() -> BTreeSet<LibcFlavor> {
    use tokio::task::JoinSet;

    let candidate_paths = discover_loader_paths().await;

    // Classify every discovered loader concurrently and union the positives.
    // No early abort: a host with both glibc and musl loaders must report
    // {Glibc, Musl}. A probe task panicking must not crash detection — treat a
    // join failure as "found nothing".
    let mut probes: JoinSet<Option<LibcFlavor>> = JoinSet::new();
    for path in candidate_paths {
        // SECURITY: `path` comes only from `discover_loader_paths` — the
        // `PT_INTERP` of a fixed system-binary allowlist, an arch-filtered scan
        // of canonical loader directories, or the hardcoded loader allowlist —
        // never user input. It is spawned solely with `--version` (and a
        // `/bin/true` confirmation), each bounded by `PROBE_TIMEOUT`.
        probes.spawn(probe_loader(path));
    }

    let mut libcs = BTreeSet::new();
    while let Some(joined) = probes.join_next().await {
        if let Ok(Some(flavor)) = joined {
            libcs.insert(flavor);
        }
    }

    // NixOS / empty result: if nothing matched and `/nix` exists, the host is
    // very likely a NixOS box without a nix-ld FHS shim and with statically
    // linked probe binaries. Note it and degrade to the empty set (Any-only
    // matching; `--platform` override available). Never an error.
    if libcs.is_empty() && tokio::fs::try_exists("/nix").await.unwrap_or(false) {
        tracing::debug!(
            "no libc loader discovered (PT_INTERP, directory scan, and FHS \
             allowlist all empty) but /nix exists; likely NixOS without a \
             nix-ld FHS shim — degrading to Any-only matching"
        );
    }

    libcs
}

/// Non-Linux platforms have a single fixed libc family per OS, so OCX does not
/// probe them. Returns an empty set without spawning any subprocess.
#[cfg(not(target_os = "linux"))]
async fn detect_libcs() -> BTreeSet<LibcFlavor> {
    BTreeSet::new()
}

/// Discover candidate dynamic-loader paths from three sources, deduplicated by
/// canonical path so a symlink and its target are never probed twice.
///
/// Sources, in priority order:
/// 1. The `PT_INTERP` of the first dynamically linked binary in
///    [`INTERP_PROBE_BINARIES`] — the host's exact native loader, found wherever
///    it lives (NixOS `/nix/store`, Gentoo Prefix, custom sysroots).
/// 2. An arch-filtered scan of [`LOADER_SCAN_DIRS`] (and their immediate
///    multiarch subdirectories) — catches additional libc families on a
///    multi-libc host.
/// 3. The hardcoded [`GLIBC_LOADERS`] / [`MUSL_LOADERS`] allowlist — fallback
///    for the rare host where neither source above fired.
#[cfg(target_os = "linux")]
async fn discover_loader_paths() -> Vec<std::path::PathBuf> {
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut discovered: Vec<std::path::PathBuf> = Vec::new();

    // Source 1: PT_INTERP of a guaranteed-present system binary. The first
    // binary that yields an interpreter wins — it is the host's native loader.
    for binary in INTERP_PROBE_BINARIES {
        if let Some(interpreter) = read_pt_interp(binary).await {
            consider_path(std::path::PathBuf::from(interpreter), &mut seen, &mut discovered).await;
            break;
        }
    }

    // Source 2: arch-filtered directory scan.
    for path in glob_loader_paths().await {
        consider_path(path, &mut seen, &mut discovered).await;
    }

    // Source 3: hardcoded allowlist fallback.
    for path in GLIBC_LOADERS.iter().chain(MUSL_LOADERS) {
        consider_path(std::path::PathBuf::from(*path), &mut seen, &mut discovered).await;
    }

    discovered
}

/// Push `path` onto `discovered` unless a path canonicalizing to the same
/// target has already been recorded.
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

/// Ordered allowlist of guaranteed-present, dynamically linked system binaries
/// whose `PT_INTERP` reveals the host's native loader path.
///
/// SECURITY: this is a fixed list, never user input — the loader path it yields
/// is later spawned with `--version`. Order matters: the first binary with a
/// `PT_INTERP` wins. A statically linked binary (busybox `/bin/sh` on a minimal
/// Alpine image) carries no `PT_INTERP`; it is skipped and discovery falls
/// through to the next entry, then to the scan / allowlist sources.
#[cfg(target_os = "linux")]
const INTERP_PROBE_BINARIES: &[&str] = &["/usr/bin/env", "/bin/sh", "/bin/ls"];

/// Read the `PT_INTERP` (dynamic loader path) embedded in the ELF at `path`.
///
/// Returns `None` when the file is absent, is not a parseable ELF, or carries
/// no `PT_INTERP` segment (a statically linked binary). `path` is always an
/// entry of [`INTERP_PROBE_BINARIES`] — never user input.
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
        // The interpreter is a NUL-terminated string; take everything up to the
        // first NUL and reject an empty result.
        let interpreter = raw.split(|&byte| byte == 0).next()?;
        if interpreter.is_empty() {
            return None;
        }
        let interpreter = String::from_utf8_lossy(interpreter).into_owned();
        // SECURITY (CWE-426): the interpreter path is later spawned; reject a
        // non-absolute one so it can never resolve against `$PATH` or the CWD. A
        // genuine ELF always names an absolute loader (the module Security note
        // and the unit tests both assume this).
        if !std::path::Path::new(&interpreter).is_absolute() {
            return None;
        }
        return Some(interpreter);
    }
    None
}

/// Canonical base directories scanned by the directory-scan discovery source.
/// Each is scanned for loader files directly and one level down (Debian/Ubuntu
/// multiarch triplet dirs such as `/lib/x86_64-linux-gnu`).
#[cfg(target_os = "linux")]
const LOADER_SCAN_DIRS: &[&str] = &["/lib", "/lib64", "/usr/lib", "/usr/lib64"];

/// Scan [`LOADER_SCAN_DIRS`] for files whose name matches a current-arch loader
/// fragment. Bounded to one level of subdirectory nesting.
#[cfg(target_os = "linux")]
async fn glob_loader_paths() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for dir in LOADER_SCAN_DIRS {
        let base = std::path::Path::new(dir);
        // Single pass over the base dir. `read_dir` follows a symlinked base
        // (`/lib` → `/usr/lib` on usrmerge). Each entry is either a subdirectory
        // (multiarch triplet dir — scanned one level deep, never further) or a
        // candidate loader file. `file_type()` does not follow symlinks, so a
        // symlinked subdir reports as a symlink (not a dir) and falls through to
        // the loader-file check — the common case (a symlinked loader *file*
        // such as /lib/ld-musl-x86_64.so.1) is included and later deduped by
        // canonical path; real multiarch dirs are not symlinks, so bounding the
        // recursion to genuine dirs loses nothing.
        let Ok(mut entries) = tokio::fs::read_dir(base).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_dir() {
                collect_loader_files(&entry.path(), &mut found).await;
            } else if entry
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_current_arch_loader_name)
            {
                found.push(entry.path());
            }
        }
    }
    found
}

/// Append every non-directory entry of `dir` whose filename matches a
/// current-architecture loader fragment to `out`.
#[cfg(target_os = "linux")]
async fn collect_loader_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_current_arch_loader_name(name)
            && entry
                .file_type()
                .await
                .map(|file_type| !file_type.is_dir())
                .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

/// True when `name` is a dynamic-loader filename for the **current**
/// architecture. Foreign-arch loaders (e.g. an `aarch64` loader present via
/// dpkg-multiarch on an x86_64 host) do not match, keeping them out of the
/// candidate set.
#[cfg(target_os = "linux")]
fn is_current_arch_loader_name(name: &str) -> bool {
    LIBC_FAMILIES
        .iter()
        .flat_map(|family| family.loader_name_fragments)
        .any(|fragment| name.contains(fragment))
}

/// Resolve `path` to its canonical form and record it; return `true` when this
/// canonical path has not been seen yet (so it should be probed).
///
/// Paths that do not exist (or fail to canonicalize) are recorded under their
/// literal form, so a missing loader is left for the caller's existence check
/// rather than collapsing distinct missing paths together.
#[cfg(target_os = "linux")]
async fn dedup_unseen(path: &std::path::Path, seen: &mut std::collections::HashSet<std::path::PathBuf>) -> bool {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_path_buf());
    seen.insert(canonical)
}

/// Per-probe subprocess timeout. Probes run concurrently and all are awaited,
/// so on a healthy host the wall-clock cost is the slowest matching probe. The
/// timeout only bites a hung or wedged loader. 1 s is tight enough to bound
/// `detect_libcs` (and thus `Context::try_init`) while leaving ample headroom
/// for process-spawn overhead on cold/loaded CI runners — 10 ms would produce
/// false negatives on slow runners. Precedent:
/// `update_check.rs::query_installed_version` uses 5 s for a version-query
/// subprocess; loader `--version` is expected much faster so 1 s is appropriate.
#[cfg(target_os = "linux")]
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// A libc family OCX can identify on the host, expressed as a table row so a
/// third family (uClibc-ng, Bionic) is a one-row addition: its loader-name
/// fragments (for the directory-scan arch filter) plus its `--version` banner
/// predicate.
#[cfg(target_os = "linux")]
struct LibcFamily {
    /// The family this row identifies.
    flavor: LibcFlavor,
    /// Current-architecture loader filename fragments (e.g. `ld-linux-x86-64`).
    /// Used to arch-filter the directory scan and, for glibc, as the name
    /// heuristic in the exit-127 confirmation path.
    loader_name_fragments: &'static [&'static str],
    /// Returns true when the combined `--version` banner identifies this family.
    banner_matches: fn(&str) -> bool,
}

/// Family identification table. Adding uClibc-ng / Bionic later is one row plus
/// its loader-name fragments — no other code changes.
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

/// glibc's loader prints a `GNU libc` / `GLIBC` banner on `--version`.
#[cfg(target_os = "linux")]
fn glibc_banner_matches(banner: &str) -> bool {
    banner.contains("GNU libc") || banner.contains("GLIBC")
}

/// musl's loader prints a `musl libc` banner (to stderr, exit non-zero by
/// design — the exit status is deliberately ignored).
#[cfg(target_os = "linux")]
fn musl_banner_matches(banner: &str) -> bool {
    banner.contains("musl libc")
}

/// The verdict of classifying a loader from its `--version` banner.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum BannerClass {
    /// The banner positively identifies a libc family.
    Identified(LibcFlavor),
    /// No banner match, the loader name looks like glibc, and it exited 127
    /// (Ubuntu 20.04 / glibc 2.31 quirk). The caller confirms by running
    /// `{loader} /bin/true`.
    GlibcNeedsConfirmation,
    /// The banner identifies no known family.
    Unrecognized,
}

/// Classify a loader purely from its `--version` output, filename, and exit
/// code — table-driven over [`LIBC_FAMILIES`], independent of which discovery
/// source produced the path.
///
/// Banner match takes precedence over the filename, so an Alpine gcompat stub
/// sitting at the glibc loader path but printing the musl banner classifies as
/// [`Musl`](LibcFlavor::Musl) — the ADR "identity, not equivalence" rule by
/// construction. A glibc loader that exits 127 with no banner (Ubuntu 20.04)
/// yields [`GlibcNeedsConfirmation`](BannerClass::GlibcNeedsConfirmation); the
/// async caller resolves it with `{loader} /bin/true`.
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

/// True when `loader_name` matches a current-arch glibc loader fragment.
#[cfg(target_os = "linux")]
fn loader_name_looks_glibc(loader_name: &str) -> bool {
    GLIBC_LOADER_FRAGMENTS
        .iter()
        .any(|fragment| loader_name.contains(fragment))
}

/// Identify the libc family of a single discovered loader at `path`.
///
/// Spawns `{path} --version` under [`PROBE_TIMEOUT`], classifies the banner via
/// [`classify_loader_banner`], and resolves the Ubuntu 20.04 exit-127 case with
/// a `{path} /bin/true` confirmation. Returns `None` when the loader is absent,
/// fails to execute, times out, or identifies no known family — never panics.
///
/// SECURITY: `path` is always a discovery-sourced loader path (never user
/// input); only `--version` / `/bin/true` are passed, each bounded by the
/// timeout so a wedged loader cannot stall OCX startup.
#[cfg(target_os = "linux")]
async fn probe_loader(path: std::path::PathBuf) -> Option<LibcFlavor> {
    // Skip the spawn entirely if the loader is not present.
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

    match classify_loader_banner(&banner, loader_name, output.status.code()) {
        BannerClass::Identified(flavor) => Some(flavor),
        BannerClass::GlibcNeedsConfirmation => {
            // Confirm the loader is a live glibc loader by running it on
            // `/bin/true`; exit 0 means it can actually launch a glibc program.
            let confirm = tokio::time::timeout(
                PROBE_TIMEOUT,
                tokio::process::Command::new(&path).arg("/bin/true").output(),
            )
            .await
            .ok()?
            .ok()?;
            confirm.status.success().then_some(LibcFlavor::Glibc)
        }
        BannerClass::Unrecognized => None,
    }
}

/// Canonical glibc dynamic-loader paths for the build target architecture,
/// multiarch-aware (multiarch symlink + real file + Fedora usrmerge). Dedup by
/// canonical path before spawning so a symlink and its target are not probed
/// twice.
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

/// Canonical musl dynamic-loader path for the build target architecture. musl
/// uses a single syslibdir path per arch.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MUSL_LOADERS: &[&str] = &["/lib/ld-musl-x86_64.so.1"];
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const MUSL_LOADERS: &[&str] = &["/lib/ld-musl-aarch64.so.1"];

// Other Linux architectures (arm, riscv64, …) are outside OCX's supported
// platform set (`Architecture::current` returns `None` there), so host libc
// detection has no entries to match against. Empty allowlists keep the probe a
// no-op without an architecture-specific loader table.
#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const GLIBC_LOADERS: &[&str] = &[];
#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const MUSL_LOADERS: &[&str] = &[];

/// Current-architecture loader filename fragments per family. Used to
/// arch-filter the directory scan (a foreign-arch multiarch loader does not
/// match) and, for glibc, as the name heuristic in the exit-127 confirmation
/// path. The fragment is the arch-specific stem of the canonical loader name
/// (`ld-linux-x86-64.so.2` → `ld-linux-x86-64`).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const GLIBC_LOADER_FRAGMENTS: &[&str] = &["ld-linux-x86-64"];
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MUSL_LOADER_FRAGMENTS: &[&str] = &["ld-musl-x86_64"];

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const GLIBC_LOADER_FRAGMENTS: &[&str] = &["ld-linux-aarch64"];
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const MUSL_LOADER_FRAGMENTS: &[&str] = &["ld-musl-aarch64"];

// Unsupported architectures: empty fragments mirror the empty loader
// allowlists, so the directory scan and name heuristic match nothing.
#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const GLIBC_LOADER_FRAGMENTS: &[&str] = &[];
#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
const MUSL_LOADER_FRAGMENTS: &[&str] = &[];

// Process-wide cache for the detected os_features. Populated once by
// `HostCapabilities::detect()` during context init, then read by every
// `Platform::current()` call for the process lifetime.
static CACHED_OS_FEATURES: OnceLock<Vec<String>> = OnceLock::new();

/// Return the process-cached `os_features` value populated by a prior
/// `HostCapabilities::detect()` call.
///
/// Returns an empty `Vec` when the cache has not been populated yet or when
/// detection found no recognised libc.
///
/// This function is intentionally `pub(crate)` — it is only called from
/// `Platform::current()` within this crate. External callers that need libc
/// information should use `HostCapabilities::detect()` directly.
pub(crate) fn cached_os_features() -> Vec<String> {
    CACHED_OS_FEATURES.get().cloned().unwrap_or_default()
}

/// Return the detected libc `os.features` tags from the process-wide cache, in
/// deterministic sorted order. Empty when libc was undetected or the cache has
/// not been populated.
///
/// Reads the **same** cache that [`Platform::current`](super::platform::Platform::current)
/// consumes for index resolution, so `ocx version` / `ocx about` report
/// exactly the libc tags the resolver would select against. Interprets each
/// cached feature via [`Feature::parse`] and keeps only the `libc.*` ones,
/// re-rendering them through [`LibcFlavor::os_feature_tag`] so the output is
/// the canonical full tag (`"libc.glibc"` / `"libc.musl"`). Non-libc features
/// are dropped — they carry no libc meaning.
pub fn cached_libc_labels() -> Vec<String> {
    cached_os_features()
        .iter()
        .filter_map(|tag| match Feature::parse(tag) {
            Feature::Libc(flavor) => Some(flavor.os_feature_tag()),
            Feature::Other(_) => None,
        })
        .collect()
}

/// Populate the process-wide `os_features` cache from a detection result.
///
/// Called once during CLI context initialization (`Context::try_init`) after
/// [`HostCapabilities::detect`] resolves. Idempotent: the first call wins and
/// subsequent calls are no-ops (the cache is a one-shot `OnceLock`), so a
/// double-init cannot corrupt the cached value.
///
/// Private because only [`HostCapabilities::detect_and_cache`] drives the
/// cache; library consumers that need libc data call
/// [`HostCapabilities::detect`] directly rather than relying on this
/// process-global cache.
fn init_cache(capabilities: &HostCapabilities) {
    // Ignore the result: a second init is a benign no-op. The first writer
    // (context init) establishes the value for the process lifetime.
    let _ = CACHED_OS_FEATURES.set(capabilities.os_features());
}

// ── Unit tests for HostCapabilities ───────────────────────────────────────
//
// Because detection probes the real filesystem (ld.so), tests that depend on
// real filesystem layout are marked `#[ignore]` — they require the actual host
// loader to be present. The main test vector uses the `__OCX_TEST_LIBC` env
// var as the detection short-circuit for reproducible CI results.
//
// `__OCX_TEST_LIBC` values (comma-separated set):
//   "glibc"       → {Glibc}
//   "musl"        → {Musl}
//   "glibc,musl"  → {Glibc, Musl}
//   "none" / ""   → {} (undetectable)
//   unset         → real probe (default)

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

    // ── __OCX_TEST_LIBC override cases ─────────────────────────────────
    //
    // All override cases are consolidated into ONE test so exactly one test
    // function owns the process-global `__OCX_TEST_LIBC` variable. Running the
    // cases sequentially within a single `#[test]` provides the same ordering
    // guarantee as `serial_test` without a new dependency. Precedent:
    // `update_check.rs` uses the same pattern for `__OCX_SELF_IMAGE`.

    #[tokio::test]
    async fn detect_with_ocx_test_libc_override_cases() {
        // SAFETY: this is the only test that touches __OCX_TEST_LIBC; serial
        // scope of a single #[test] provides ordering.
        unsafe { std::env::set_var("__OCX_TEST_LIBC", "glibc") };
        let caps = HostCapabilities::detect().await;
        // SAFETY: see above.
        unsafe { std::env::remove_var("__OCX_TEST_LIBC") };
        assert_eq!(caps.libcs, glibc_only(), "__OCX_TEST_LIBC=glibc must yield {{Glibc}}");

        // SAFETY: see above.
        unsafe { std::env::set_var("__OCX_TEST_LIBC", "musl") };
        let caps = HostCapabilities::detect().await;
        // SAFETY: see above.
        unsafe { std::env::remove_var("__OCX_TEST_LIBC") };
        assert_eq!(caps.libcs, musl_only(), "__OCX_TEST_LIBC=musl must yield {{Musl}}");

        // SAFETY: see above.
        unsafe { std::env::set_var("__OCX_TEST_LIBC", "glibc,musl") };
        let caps = HostCapabilities::detect().await;
        // SAFETY: see above.
        unsafe { std::env::remove_var("__OCX_TEST_LIBC") };
        assert_eq!(
            caps.libcs,
            both(),
            "__OCX_TEST_LIBC=glibc,musl must yield {{Glibc, Musl}}"
        );
        assert_eq!(
            caps.os_features(),
            vec!["libc.glibc".to_string(), "libc.musl".to_string()],
            "dual-libc host must advertise both os.features tags, sorted"
        );

        // SAFETY: see above.
        unsafe { std::env::set_var("__OCX_TEST_LIBC", "none") };
        let caps = HostCapabilities::detect().await;
        // SAFETY: see above.
        unsafe { std::env::remove_var("__OCX_TEST_LIBC") };
        assert!(caps.libcs.is_empty(), "__OCX_TEST_LIBC=none must yield an empty set");
        assert!(caps.os_features().is_empty(), "empty set must yield empty os_features");
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
    /// assertion holds on all targets because __OCX_TEST_LIBC is not set in
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
        // __OCX_TEST_LIBC unset — real probe.
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
        // __OCX_TEST_LIBC unset — exercises the real error path in detect().
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
