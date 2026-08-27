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
//! Two caches, one in front of the other.
//!
//! **Process cache.** Detection runs once at context init and memoizes into an
//! `OnceLock` for the lifetime of the process. Embedders using `ocx_lib` as a
//! library, or a future daemon mode, must invalidate or work around it.
//!
//! **Persisted record** (`$OCX_HOME/state/host/capabilities.json`, Linux only).
//! Every `ocx` invocation is a fresh process, so the `OnceLock` alone left every
//! command re-running the whole discovery pipeline — 15.4 ms of it, measured on
//! a per-prompt shell reconcile that contains no other libc-dependent work
//! (ocx-sh/ocx#340). The host's libc set is a per-host constant between package
//! installs, so the answer is recorded on disk beside the referrers-capability
//! and trust-root caches and in the same shape: atomic write, TTL-gated
//! fail-open read, anything unusable treated as a miss.
//!
//! Freshness has two independent gates, because the two ways a record can go
//! wrong are not equally dangerous:
//!
//! - **A libc was REMOVED or REPLACED.** The record would name a family the
//!   host can no longer execute, and OCX would select an artifact that cannot
//!   launch — a resolution failure, not a slow command. Closed exactly, and not
//!   by the clock: the record carries every loader that classified positive
//!   together with the file identity it had at the time (device, inode, size,
//!   mtime — all off the `stat` the check needs anyway), and is honoured only
//!   while every one of them is still the same file at the same path.
//!   Uninstalling a libc removes its loader; reinstalling or swapping one keeps
//!   the path but changes its identity. Either way the very next invocation
//!   re-detects. Existence alone would not do: a libc replaced in place is the
//!   ordinary case (package reinstall, container-layer swap), and the path
//!   survives it while the executable behind it may now be a different libc.
//! - **A libc was ADDED.** The record under-reports, so a package shipped only
//!   for the new family resolves to `FeatureMismatch` (exit 65, which names the
//!   platforms that *are* available) instead of installing. Recoverable and
//!   self-diagnosing, so this is the direction the TTL clock bounds.
//!
//! The record states its evidence and nothing else. Its `os.features` answer is
//! **derived** from the loaders it recorded, never stored beside them, so a
//! record naming a family that no recorded loader classified as is not something
//! the reader has to reject — it is not expressible. An empty loader list still
//! parses and claims nothing, but it is never *written*: a pass that classified
//! nothing is not recorded at all, because the record cannot tell it apart from
//! a pass that could not look (see [`record_detection`]).
//!
//! The `__OCX_TEST_LIBC` seam is neither read from nor written to the record: a
//! forced libc set can never be persisted onto a real host, and a record can
//! never override the seam.
//!
//! ## Security
//!
//! ### The persisted record is not a trust boundary
//!
//! The record is a `0o600` file inside the user's own `$OCX_HOME`, written and
//! read by the same user. Anyone who can write it can already do considerably
//! worse — replace an installed binary, edit `config.toml`, rewrite a symlink —
//! so it is not defended as attacker-controlled input and the checks below are
//! not a mitigation for one. What they *do* enforce is that the reader accepts
//! only records the writer could have produced: a claim with no evidence behind
//! it, a stale format version, or a stray field all mean the file did not come
//! from this code, and the answer to that is to probe, never to guess. Both
//! `serde(deny_unknown_fields)` and the `RecordVersion` tag exist for that, not
//! for an adversary. A record that names real, unmodified loaders but lies about
//! which family each one classified as is still believed — detecting that needs
//! the probe the record exists to avoid.
//!
//! ### Detection inputs
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

#[cfg(target_os = "linux")]
use serde_repr::{Deserialize_repr, Serialize_repr};

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
            if let Some(libcs) = test_libc_override() {
                return Self { libcs };
            }
        }

        Self {
            libcs: run_detection().await.libcs(),
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
    /// entries with empty `os_features`).
    ///
    /// Three tiers, cheapest first: the process `OnceLock` (2nd+ call in the
    /// same process), the persisted `$OCX_HOME/state/host/capabilities.json`
    /// record (2nd+ invocation on the same host inside the TTL), then the full
    /// discovery-then-identify pipeline. See the module-level "Cache lifecycle"
    /// note for what invalidates the persisted tier — a slow answer is
    /// acceptable here, a wrong one is not.
    pub async fn detect_and_cache() -> Self {
        // Fast path: cache already populated (2nd+ call in same process).
        // Reconstruct `HostCapabilities` from the cached feature tags instead
        // of re-running the discovery-then-identify pipeline.
        if let Some(cached) = CACHED_OS_FEATURES.get() {
            return Self {
                libcs: decode_libc_tags(cached),
            };
        }
        let capabilities = detect_with_persisted_record().await;
        init_cache(&capabilities);
        capabilities
    }
}

/// Decode `os.features` tags back into the libc families they name, dropping
/// anything that is not a recognised `libc.*` tag.
///
/// Shared by the two fast paths that reconstruct a [`HostCapabilities`] from
/// tags — the process `OnceLock` and the persisted record — so they cannot
/// disagree about what a tag means.
fn decode_libc_tags<'tags>(tags: impl IntoIterator<Item = &'tags String>) -> BTreeSet<LibcFlavor> {
    tags.into_iter()
        .filter_map(|tag| LibcFlavor::from_os_feature_tag(tag))
        .filter(|flavor| !matches!(flavor, LibcFlavor::Unknown(_)))
        .collect()
}

/// The `__OCX_TEST_LIBC` seam value, decoded, when the variable is set.
///
/// Extracted so [`HostCapabilities::detect`] and the persisted-record path check
/// the same condition: an unset variable must fall through to the real probe,
/// and a set one must never reach (or be reached from) the on-disk record.
#[cfg(any(test, feature = "__testing"))]
fn test_libc_override() -> Option<BTreeSet<LibcFlavor>> {
    std::env::var("__OCX_TEST_LIBC")
        .ok()
        .map(|value| parse_test_libc_set(&value))
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

/// What one detection pass found: every loader that classified, paired with the
/// family it classified as.
///
/// The libc set is **derived** from that evidence ([`Detection::libcs`]) rather
/// than carried beside it, so the two can never disagree — the same property the
/// persisted record inherits by recording only this list. The loaders are
/// carried out of the pipeline rather than discarded because the record
/// re-checks them: a loader uninstalled or replaced since is what invalidates a
/// record, and no clock can see that. See the module-level "Cache lifecycle"
/// note.
#[derive(Debug, Default)]
struct Detection {
    /// Every loader that classified positive with the family its `--version`
    /// banner identified, sorted by path so the persisted record is byte-stable
    /// across runs.
    classified: Vec<(std::path::PathBuf, LibcFlavor)>,
}

impl Detection {
    /// The families this pass found, derived from the loaders that classified.
    ///
    /// A `BTreeSet` makes the answer deterministic and independent of probe
    /// scheduling, and unions duplicates — a dual-libc host with a glibc and a
    /// musl loader reports both, a host with two glibc loaders reports one
    /// family.
    fn libcs(&self) -> BTreeSet<LibcFlavor> {
        self.classified.iter().map(|(_, flavor)| flavor.clone()).collect()
    }
}

/// Probe the host for every libc family it provides.
///
/// Returns an empty result immediately on non-Linux targets without spawning any
/// subprocess. On Linux it runs the discovery-then-identify pipeline: discover
/// candidate loader paths (a system binary's `PT_INTERP` ∪ an arch-filtered
/// directory scan ∪ the hardcoded allowlist, deduplicated by canonical path),
/// then classify each by its `--version` banner and union every positive into
/// the set — no early abort, no first-wins.
#[cfg(target_os = "linux")]
async fn run_detection() -> Detection {
    use tokio::task::JoinSet;

    let candidate_paths = discover_loader_paths().await;

    // Classify every discovered loader concurrently and union the positives.
    // No early abort: a host with both glibc and musl loaders must report
    // {Glibc, Musl}. A probe task panicking must not crash detection — treat a
    // join failure as "found nothing".
    let mut probes: JoinSet<Option<(std::path::PathBuf, LibcFlavor)>> = JoinSet::new();
    for path in candidate_paths {
        // SECURITY: `path` comes only from `discover_loader_paths` — the
        // `PT_INTERP` of a fixed system-binary allowlist, an arch-filtered scan
        // of canonical loader directories, or the hardcoded loader allowlist —
        // never user input. It is spawned solely with `--version` (and a
        // `/bin/true` confirmation), each bounded by `PROBE_TIMEOUT`.
        probes.spawn(probe_loader(path));
    }

    let mut classified = Vec::new();
    while let Some(joined) = probes.join_next().await {
        if let Ok(Some(found)) = joined {
            classified.push(found);
        }
    }
    // `join_next` yields in completion order, which is scheduling-dependent.
    // Sort so the persisted record is byte-stable across runs.
    classified.sort();

    // NixOS / empty result: if nothing matched and `/nix` exists, the host is
    // very likely a NixOS box without a nix-ld FHS shim and with statically
    // linked probe binaries. Note it and degrade to the empty set (Any-only
    // matching; `--platform` override available). Never an error.
    if classified.is_empty() && tokio::fs::try_exists("/nix").await.unwrap_or(false) {
        tracing::debug!(
            "no libc loader discovered (PT_INTERP, directory scan, and FHS \
             allowlist all empty) but /nix exists; likely NixOS without a \
             nix-ld FHS shim — degrading to Any-only matching"
        );
    }

    Detection { classified }
}

/// Non-Linux platforms have a single fixed libc family per OS, so OCX does not
/// probe them. Returns an empty result without spawning any subprocess.
#[cfg(not(target_os = "linux"))]
async fn run_detection() -> Detection {
    Detection::default()
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
///
/// The whole walk runs in **one** `spawn_blocking` over `std::fs`, not as
/// `tokio::fs` calls per entry. `tokio::fs` `asyncify`s every operation onto the
/// blocking pool, so the per-entry `file_type()` this scan needs was one
/// executor round-trip apiece for a `d_type` read that costs no syscall at all —
/// ~7,800 of them on a usrmerge x86_64 host, measured at 15.2 ms against 2.5 ms
/// for the identical `std::fs` walk (ocx-sh/ocx#340). That is the "no blocking
/// I/O in async" rule read the right way round: short, local, uncontended
/// filesystem work belongs on one blocking thread, not spread across thousands
/// of hand-offs.
#[cfg(target_os = "linux")]
async fn glob_loader_paths() -> Vec<std::path::PathBuf> {
    match tokio::task::spawn_blocking(scan_loader_dirs).await {
        Ok(found) => found,
        Err(join_error) => {
            // The scan is one of three discovery sources; losing it degrades
            // discovery rather than failing detection, exactly as an unreadable
            // directory already does.
            tracing::debug!("loader directory scan did not complete ({join_error}); continuing without it");
            Vec::new()
        }
    }
}

/// Blocking body of [`glob_loader_paths`].
#[cfg(target_os = "linux")]
fn scan_loader_dirs() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for base in dedup_scan_roots(LOADER_SCAN_DIRS) {
        // Single pass over the base dir. `read_dir` follows a symlinked base
        // (`/lib` → `/usr/lib` on usrmerge). Each entry is either a subdirectory
        // (multiarch triplet dir — scanned one level deep, never further) or a
        // candidate loader file. `file_type()` does not follow symlinks, so a
        // symlinked subdir reports as a symlink (not a dir) and falls through to
        // the loader-file check — the common case (a symlinked loader *file*
        // such as /lib/ld-musl-x86_64.so.1) is included and later deduped by
        // canonical path; real multiarch dirs are not symlinks, so bounding the
        // recursion to genuine dirs loses nothing.
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

/// Reduce `dirs` to the distinct filesystem trees they name, keeping the first
/// spelling of each.
///
/// On every usrmerge distribution `/lib` → `/usr/lib` and `/lib64` →
/// `/usr/lib64`, so [`LOADER_SCAN_DIRS`]' four entries name two real trees and
/// the scan walked each of them twice. A path that does not exist, or fails to
/// canonicalize, keeps its literal form as its own identity — so a
/// non-usrmerge host still scans all four, and two distinct missing paths do not
/// collapse into one.
///
/// Blocking; runs inside [`glob_loader_paths`]'s `spawn_blocking`.
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

/// Append every non-directory entry of `dir` whose filename matches a
/// current-architecture loader fragment to `out`.
///
/// Blocking; runs inside [`glob_loader_paths`]'s `spawn_blocking`.
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
/// A positive result hands `path` back with the family, because the persisted
/// record keys its invalidation on exactly the loaders that classified.
///
/// SECURITY: `path` is always a discovery-sourced loader path (never user
/// input); only `--version` / `/bin/true` are passed, each bounded by the
/// timeout so a wedged loader cannot stall OCX startup.
#[cfg(target_os = "linux")]
async fn probe_loader(path: std::path::PathBuf) -> Option<(std::path::PathBuf, LibcFlavor)> {
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
    let class = classify_loader_banner(&banner, loader_name, output.status.code());

    match class {
        BannerClass::Identified(flavor) => Some((path, flavor)),
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
            confirm.status.success().then_some((path, LibcFlavor::Glibc))
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

// ── Persisted host-capability record ──────────────────────────────────────

/// Persisted-record TTL: 24 hours.
///
/// The clock bounds one direction only — a libc **added** since the record was
/// written; a libc **removed or replaced** invalidates the record immediately
/// through [`HostCapabilityRecord::evidence_still_holds`], not through this
/// constant (module-level "Cache lifecycle" note).
///
/// It was one hour until 2026-08-27, on the reasoning that a full re-detect is
/// "unmeasurable beside the ~3.6 ms an `ocx` process costs to start at all".
/// That reasoning did not survive the reconciler: the per-prompt reconcile is
/// budgeted at `exec_floor + 3 ms` (C-044), and `test/bench/shell_latency.py`
/// measures the **cold** detect — this record deleted before the spawn — at
/// Δ 3.659–4.732 ms, over that budget on its own. So the first prompt of every
/// TTL period lands over budget, and at one hour that was once an hour, per
/// host, on a path whose whole contract is that a user never notices it.
///
/// Lengthening the clock is the fix rather than refreshing off the prompt path,
/// because there is no off-prompt path to refresh on: every `ocx` is a fresh
/// short-lived process that exits as soon as it has emitted, so a detached
/// background refresh would be killed before it finished and would buy a
/// complexity budget for nothing.
///
/// What 24 h costs is bounded, and it is the *recoverable* direction by
/// construction: a libc **added** since the record was written makes the record
/// under-report, which surfaces as `FeatureMismatch` (exit 65) naming the
/// platforms that *are* available — self-diagnosing, and cleared by deleting
/// `$OCX_HOME/state/host/capabilities.json`. The dangerous direction — a libc
/// removed or replaced, where OCX would select an artifact that cannot launch —
/// is not on this clock at all and still invalidates on the very next
/// invocation.
///
/// Now the same 24 h as the trust-root cache. The note this replaces argued for
/// something shorter, on the grounds that a local answer can change under the
/// user's hands between two prompts while a remote's cannot. True, but it is the
/// argument for `evidence_still_holds`, which is what actually catches those
/// changes; the clock only ever covered the one case that check cannot see.
///
/// A record already on disk carries its own `ttl_seconds`, and
/// [`HostCapabilityRecord::is_fresh`] clamps with `min`, so raising this
/// constant never extends an existing record — the longer lifetime starts with
/// the next one written.
#[cfg(target_os = "linux")]
const TTL_SECS: u64 = 86_400;

/// On-disk format version of the host-capability record.
///
/// `serde_repr` refuses an unrecognised integer on deserialise by itself, so a
/// record written by another ocx is a clean miss with no hand-written check to
/// forget. Bumping this is the entire migration story: the record is per-host
/// derived state behind a 1-hour TTL, so invalidating every existing one costs
/// exactly one re-probe per host.
///
/// V1 (never represented here) stored `os_features` and `loaders` as two
/// independent lists, which let a record claim a libc family no recorded loader
/// had classified as, and keyed loader validity on path existence alone, which
/// a replace-in-place survives.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
enum RecordVersion {
    /// Evidence-only: one entry per classified loader carrying the family it
    /// identified and the file identity it had when it did.
    V2 = 2,
}

/// The identity a loader file had at the moment it classified.
///
/// Recorded so the record can tell "the same loader is still there" from
/// "something else is at that path now". Every field comes off the same `stat`
/// the presence check already performs, so re-checking all four costs nothing
/// beyond what checking existence alone cost.
///
/// **What it does not catch:** an overwrite that preserves the inode, the byte
/// length *and* the mtime to the nanosecond — which takes a deliberate
/// `touch -r` after writing an identically sized file. This is not a content
/// hash on purpose: hashing each loader on every invocation measured 0.41 ms for
/// one 960 KB glibc loader with a warm page cache, against the ~2.3 ms the whole
/// record saves, so the exact answer would spend a fifth of the saving (more on
/// a dual-libc host, more again on a cold cache) closing a case no package
/// manager produces. Every ordinary replacement moves at least one field: a
/// write-new-then-`rename` install moves the inode, an in-place rewrite moves
/// the mtime, a container-layer or bind-mount swap moves the device — and two
/// libc loaders are never the same size.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LoaderIdentity {
    /// Device the loader lives on.
    device: u64,
    /// Inode number.
    inode: u64,
    /// Byte length.
    size: u64,
    /// Modification time, whole seconds since the epoch.
    mtime_seconds: i64,
    /// Modification time, nanosecond remainder — the axis that catches an
    /// in-place rewrite of identical length.
    mtime_nanoseconds: i64,
}

#[cfg(target_os = "linux")]
impl LoaderIdentity {
    /// Read the identity out of a `stat` result.
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
    /// Absolute path the loader was probed at.
    path: String,
    /// The canonical `os.features` tag this loader's `--version` banner
    /// identified (`libc.glibc` / `libc.musl`). Held as the tag rather than a
    /// serialized [`LibcFlavor`] so the record routes through the one
    /// round-tripping mapping ([`LibcFlavor::os_feature_tag`] /
    /// [`LibcFlavor::from_os_feature_tag`]) instead of minting a second
    /// encoding that could drift from it.
    feature: String,
    /// The loader's file identity when it classified.
    identity: LoaderIdentity,
}

/// A detection result recorded on disk at
/// `$OCX_HOME/state/host/capabilities.json`.
///
/// Advisory and fail-open in every direction: missing, unreadable, corrupt,
/// expired or evidence-invalidated all mean "miss", and a miss simply re-runs
/// detection. Nothing here can turn a slow command into a failed one.
///
/// `deny_unknown_fields` here and on every nested struct is not a defence
/// against an attacker (see the module-level "The persisted record is not a
/// trust boundary" note) — it is how the reader refuses a record this writer
/// could not have produced. A stray key means the file came from somewhere
/// else, and the only safe reading of somewhere else is "probe now". The
/// project-wide ban on `deny_unknown_fields` covers the `Config` tree, whose
/// forward-compatibility matters because one file is fleet-wide state; this is
/// machine-local derived state with a version tag and a 1-hour TTL, where a
/// refusal costs one re-probe.
#[cfg(target_os = "linux")]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostCapabilityRecord {
    /// On-disk format version; an unrecognised value fails to deserialise,
    /// which is a miss.
    version: RecordVersion,
    /// Every loader that classified positive, sorted by path.
    ///
    /// This is the record's **only** statement about the host, and the
    /// `os.features` answer is derived from it
    /// ([`HostCapabilityRecord::libcs`]) rather than stored beside it — so a
    /// record claiming a family no recorded loader produced is not rejected,
    /// it is unrepresentable. An empty list parses and, being evidence rather
    /// than assertion, claims nothing — but [`record_detection`] never writes
    /// one, since no record can distinguish a host on which nothing classified
    /// from a pass that could not look.
    loaders: Vec<LoaderRecord>,
    /// Wall-clock time of the detection this record captures (UTC).
    detected_at: std::time::SystemTime,
    /// TTL in seconds, clamped to [`TTL_SECS`] on read.
    ttl_seconds: u64,
}

#[cfg(target_os = "linux")]
impl HostCapabilityRecord {
    /// Capture a detection pass as a record stamped now, or `None` when a
    /// classified loader's identity cannot be read.
    ///
    /// Re-stats each loader rather than carrying its identity out of the probe:
    /// this is the write path, which has just run the whole discovery pipeline,
    /// so one to three extra `stat`s are free, and it keeps the Linux-only
    /// [`LoaderIdentity`] out of the cross-platform [`Detection`]. A loader that
    /// vanished between its probe and here means the host changed mid-detection
    /// — recording a partial answer would then be honoured for an hour, so
    /// record nothing and let the next invocation re-detect.
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

    /// The libc families this record's own evidence supports.
    ///
    /// Every family named here is one a recorded loader classified as, because
    /// there is nothing else to derive it from. An unrecognised `feature` tag
    /// decodes to [`LibcFlavor::Unknown`] and is dropped, so a value this
    /// binary does not understand contributes nothing rather than being
    /// believed.
    fn libcs(&self) -> BTreeSet<LibcFlavor> {
        decode_libc_tags(self.loaders.iter().map(|loader| &loader.feature))
    }

    /// True while the record is inside its (clamped) TTL.
    ///
    /// Both halves of the lifetime come off disk, so neither is trusted: a
    /// `detected_at` in the future (rewound clock, hand-edited file) reads as
    /// stale, and `ttl_seconds` is clamped so a record can shorten its own
    /// lifetime and never extend it.
    fn is_fresh(&self) -> bool {
        match std::time::SystemTime::now().duration_since(self.detected_at) {
            Ok(elapsed) => elapsed < std::time::Duration::from_secs(self.ttl_seconds.min(TTL_SECS)),
            Err(_) => false,
        }
    }

    /// True while every loader that classified for this record is still the
    /// same file at the same path.
    ///
    /// This is the gate that closes the dangerous staleness direction. A record
    /// naming a libc the host can no longer execute would make OCX select an
    /// artifact that cannot launch, and no TTL short enough to bound that is
    /// short enough to be worth caching under — so the change is detected
    /// directly instead.
    ///
    /// Existence is not the check. Uninstalling a libc removes its loader, but
    /// *replacing* one keeps the path: a package reinstall, an upgrade, a
    /// container-layer swap all leave a file at the recorded path whose contents
    /// this record never saw, and the executable there may belong to a different
    /// libc or not run at all. So the recorded [`LoaderIdentity`] is compared,
    /// not merely probed for presence — the same one `stat` per classified
    /// loader (one to three on a real host) an existence check cost, against the
    /// several thousand a full re-detect walks.
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

/// `$OCX_HOME/state/host/capabilities.json`, or `None` when no home resolves.
///
/// Derived from [`crate::file_structure::default_ocx_root`] rather than taken as
/// a parameter because detection also runs on the static-command bypass
/// (`ocx version --verbose`), which builds no `FileStructure` — the same reason
/// `FileStructure::new` derives its own root. A `None` here is not an error:
/// detection just runs uncached.
#[cfg(target_os = "linux")]
fn record_path() -> Option<std::path::PathBuf> {
    let root = crate::file_structure::default_ocx_root()?;
    let state = crate::file_structure::StateStore::new(root.join("state"));
    Some(state.host_capabilities_file())
}

/// Read the record at `path`, or `None` for any reason it cannot be used.
#[cfg(target_os = "linux")]
async fn read_record(path: &std::path::Path) -> Option<HostCapabilityRecord> {
    let bytes = tokio::fs::read(path).await.ok()?;
    // A record OCX cannot decode is a record from another version, a damaged
    // one, or one this writer could not have produced (an unknown field, a
    // `version` outside `RecordVersion`). Every one of those means the answer
    // has to be probed rather than read — never an error surfaced from a cache.
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

/// Persist `record` at `path`, best-effort.
///
/// Every failure is a debug log and nothing else: the record is an optimization,
/// so a read-only or full `$OCX_HOME` must cost a slow command, never a failed
/// one. Written through [`crate::utility::fs::write_bytes_atomic`] (private
/// tempfile + rename) so a concurrent reader never sees a partial record — the
/// same write the referrers and trust-root caches use.
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
    match tokio::task::spawn_blocking(move || crate::utility::fs::write_bytes_atomic(&path, &bytes)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!("could not write the host libc record: {error}"),
        Err(join_error) => tracing::debug!("host libc record write did not complete: {join_error}"),
    }
}

/// Detect, consulting and refreshing the persisted record.
#[cfg(target_os = "linux")]
async fn detect_with_persisted_record() -> HostCapabilities {
    // The test seam is checked before any disk access, so a forced libc set is
    // never written onto a real host's record and a record can never override
    // the seam.
    #[cfg(any(test, feature = "__testing"))]
    {
        if let Some(libcs) = test_libc_override() {
            return HostCapabilities { libcs };
        }
    }

    let Some(path) = record_path() else {
        return HostCapabilities {
            libcs: run_detection().await.libcs(),
        };
    };
    if let Some(record) = read_record(&path).await {
        return HostCapabilities { libcs: record.libcs() };
    }
    let detection = run_detection().await;
    record_detection(path, &detection).await;
    HostCapabilities {
        libcs: detection.libcs(),
    }
}

/// Persist `detection` at `path`, unless it classified nothing.
///
/// The writer cannot record "I could not look" — the record has one shape, and
/// an empty loader list read back through
/// [`HostCapabilityRecord::evidence_still_holds`] is vacuously valid, so nothing
/// short of the TTL can dislodge it. A *degraded* pass produces exactly that
/// empty list: the directory scan losing its `spawn_blocking` join
/// ([`glob_loader_paths`]), or every probe hitting [`PROBE_TIMEOUT`] on a loaded
/// runner. Latching one would answer `os.features` with the empty set for an
/// hour, and a package published only for glibc then fails to resolve
/// (`FeatureMismatch`, exit 65) on every install until it expires.
///
/// So an empty classification is not recorded. It costs one re-detect per
/// invocation — precisely what every invocation paid before the record existed —
/// and it keeps "could not look" from being persisted as "looked and found
/// nothing".
///
/// ponytail: a host that genuinely has no libc loader (a static-only image where
/// `PT_INTERP`, the directory scan, and the FHS allowlist all come up empty)
/// therefore never caches, paying the ~2.7 ms detection every invocation. Fixing
/// that needs a second record shape that distinguishes a completed pass from a
/// degraded one; a real complaint from such a host is what would justify it.
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

/// Non-Linux detection spawns no subprocess and returns the empty set
/// immediately, so reading a file would cost strictly more than the detection it
/// would replace. No record is read or written there.
#[cfg(not(target_os = "linux"))]
async fn detect_with_persisted_record() -> HostCapabilities {
    HostCapabilities::detect().await
}

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

    /// The record lands where this module's header documents it.
    ///
    /// The path belongs to `StateStore` — it is the state root's layout — but
    /// the contract is stated here, and every test below hand-builds
    /// `state/host/capabilities.json` rather than asking for it. Without this
    /// the accessor could be repointed and nothing would notice: the tests
    /// would keep proving that `read_record` reads whatever path they were
    /// handed. Pinning it from this module means a change in `state_store.rs`
    /// reds a test in the module whose documentation it would falsify.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_record_lands_where_this_module_documents_it() {
        let state = crate::file_structure::StateStore::new(std::path::Path::new("/o").join("state"));
        assert_eq!(
            state.host_capabilities_file(),
            std::path::Path::new("/o/state/host/capabilities.json")
        );
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
