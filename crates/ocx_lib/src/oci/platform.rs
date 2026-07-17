// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod architecture;
pub mod error;
pub mod operating_system;

pub use architecture::Architecture;
pub use error::{PlatformError, PlatformErrorKind};
pub use operating_system::OperatingSystem;

use serde::{Deserialize, Serialize};

use super::native;
use crate::Result;

const ANY_STR: &str = "any";

/// Target platform for an OCX package.
///
/// This is OCX's owned representation of the
/// [OCI Image Index platform object](https://github.com/opencontainers/image-spec/blob/main/image-index.md#image-index-property-descriptions).
/// It wraps the same concept but uses closed enums ([`OperatingSystem`],
/// [`Architecture`]) instead of the upstream open enums with `Other(String)`
/// fallback, giving compile-time enforcement of supported values.
///
/// # `Any` — an OCX extension
///
/// The OCI specification does not define a concept of a platform-agnostic
/// package. Every Image Index entry has a concrete `os`/`architecture` pair.
/// OCX extends this with [`Platform::Any`] to represent packages that are
/// not tied to any specific platform (e.g. Java JARs, shell scripts, data
/// bundles). When serialized to OCI JSON, `Any` is represented as
/// `{"os":"any","architecture":"any"}` — these are not valid OCI values but
/// are a convention understood by OCX tooling.
///
/// # Serialization
///
/// Serde goes through [`native::Platform`] to guarantee OCI-compatible JSON
/// field names (`"os"`, `"architecture"`, `"variant"`, `"os.features"`).
/// Deserialization validates that the OS and architecture are in OCX's
/// supported set, rejecting unsupported values from registries (e.g.
/// `ppc64le`, `s390x`).
///
/// # String format
///
/// [`Display`] is the single canonical, lossless, injective string form and
/// the inverse of [`FromStr`] — the same grammar backs the CLI `--platform`
/// flag and every `ocx.lock` / dependency-pin map key:
///
/// ```text
/// os/arch[/variant][+feature[,feature...]]      |      any
/// ```
///
/// `os.features` are a single `+` introducing a comma-separated list (sorted
/// and deduped, so the output is canonical). Every registry-controlled value
/// (`variant`, each feature) is percent-escaped so it can never forge a
/// structural character (`% / + ,`). Examples: `"linux/arm/v7"`,
/// `"windows/amd64"`, `"linux/amd64+libc.glibc,libc.musl"`,
/// `"linux/arm64/v8+libc.glibc"`, `"any"`. `"any"` with any suffix (`any+…`,
/// `any/…`) is an error — the agnostic platform carries no fields.
/// Filesystem paths use [`segments`](Self::segments) /
/// [`ascii_segments`](Self::ascii_segments) instead, so carrying features in
/// `Display` does not affect path building.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Platform-agnostic package. Not part of the OCI spec — this is an OCX
    /// extension for packages that run on any OS and architecture.
    /// Serializes to `{"os":"any","architecture":"any"}` in OCI JSON.
    Any,

    /// A concrete OS/architecture target, corresponding to a single entry
    /// in an OCI Image Index.
    Specific {
        /// Target operating system (required by OCI spec).
        os: OperatingSystem,
        /// Target CPU architecture (required by OCI spec).
        arch: Architecture,
        /// CPU variant, e.g. `"v7"` or `"v8"` for ARM. Defined by the
        /// [OCI platform variants table](https://github.com/opencontainers/image-spec/blob/main/image-index.md#platform-variants).
        variant: Option<String>,
        /// Mandatory OS features required for this image. For Windows, the
        /// OCI spec defines `"win32k"` (image requires `win32k.sys` on the
        /// host, which is missing on Nano Server). For other operating
        /// systems, values are implementation-defined. An empty `Vec` means no
        /// features are declared (undetected / no libc requirement).
        os_features: Vec<String>,
    },
}

impl Platform {
    /// Creates a platform-agnostic platform value.
    ///
    /// Use this to indicate that a package is compatible with any platform,
    /// for example a Java JAR or a data bundle.
    pub fn any() -> Self {
        Self::Any
    }

    /// Returns `true` if this is the platform-agnostic [`Any`](Self::Any) variant.
    ///
    /// This is an OCX concept with no direct equivalent in the OCI spec.
    /// The OCI spec requires every Image Index entry to have a concrete
    /// `os`/`architecture` pair; OCX uses the sentinel values `"any"/"any"`
    /// as a convention for platform-agnostic packages.
    pub fn is_any(&self) -> bool {
        matches!(self, Self::Any)
    }

    /// Extracts the platform from a single-image OCI manifest.
    ///
    /// Single-image manifests (as opposed to multi-platform Image Indexes)
    /// do not carry platform metadata in the manifest itself — the platform
    /// is only known from the Image Index entry that references it. When we
    /// encounter a bare image manifest without index context, we treat it as
    /// platform-agnostic.
    pub fn from_image_manifest(_manifest: &native::ImageManifest) -> Self {
        Self::Any
    }

    /// Extracts all platforms from an OCI Image Index manifest.
    ///
    /// Each entry in the index references a platform-specific image manifest.
    /// Returns an error if any entry's platform uses an OS or architecture
    /// that OCX does not support.
    pub fn from_image_index(manifest: &native::ImageIndex) -> Result<Vec<Self>> {
        let mut platforms = Vec::with_capacity(manifest.manifests.len());
        for entry in &manifest.manifests {
            let platform = Self::try_from(entry.platform.clone())?;
            platforms.push(platform);
        }
        Ok(platforms)
    }

    /// Extracts platforms from any OCI manifest type.
    ///
    /// Dispatches to [`from_image_manifest`](Self::from_image_manifest) for
    /// single images or [`from_image_index`](Self::from_image_index) for
    /// multi-platform indexes.
    pub fn from_manifest(manifest: &native::Manifest) -> Result<Vec<Self>> {
        match manifest {
            native::Manifest::Image(image_manifest) => Ok(vec![Self::from_image_manifest(image_manifest)]),
            native::Manifest::ImageIndex(image_index) => Self::from_image_index(image_index),
        }
    }

    /// Returns the platform components as string segments.
    ///
    /// - `Any` → `["any"]`
    /// - `Specific` → `["linux", "amd64"]` or `["linux", "arm64", "v8"]` etc.
    ///
    /// Used for constructing filesystem paths and display formatting.
    pub fn segments(&self) -> Vec<String> {
        match self {
            Self::Any => vec![ANY_STR.to_string()],
            Self::Specific { os, arch, variant, .. } => {
                let mut segments = vec![os.to_string(), arch.to_string()];
                if let Some(variant) = variant {
                    segments.push(variant.clone());
                }
                segments
            }
        }
    }

    /// Compares only the OS and architecture of two `Specific` platforms,
    /// ignoring `variant` and `os_features`.
    ///
    /// [`current`](Self::current) can only ever populate `os`/`arch` (the
    /// remaining fields are always `None`/empty), so a full-struct equality
    /// comparison (the deleted `matches` semantics) would wrongly reject a
    /// host-runnable image-index entry that merely carries a `variant` or
    /// `os_features` refinement. The host-only symlink gate
    /// (issue #179) therefore compares os+arch only. Two [`Any`](Self::Any)
    /// values compare equal; `Any` vs `Specific` never do.
    pub fn same_os_arch(&self, other: &Platform) -> bool {
        match (self, other) {
            (Self::Any, Self::Any) => true,
            (
                Self::Specific {
                    os: self_os,
                    arch: self_arch,
                    ..
                },
                Self::Specific {
                    os: other_os,
                    arch: other_arch,
                    ..
                },
            ) => self_os == other_os && self_arch == other_arch,
            _ => false,
        }
    }

    /// Whether a package that resolved to `platform` is runnable on the current
    /// host — the host-only symlink gate for `candidates/{tag}` and `current`
    /// (issue #179).
    ///
    /// `None` (undeterminable platform) and [`any`](Self::any) (platform-agnostic
    /// package) are always host-appropriate. A specific platform is
    /// host-appropriate only when its os+arch equal the host's; when the host
    /// itself is undeterminable, the write is allowed — never suppress on an
    /// unknown host.
    pub fn host_can_run(platform: Option<&Platform>) -> bool {
        Self::host_can_run_on(platform, Self::current().as_ref())
    }

    /// Host-injectable core of [`host_can_run`](Self::host_can_run).
    ///
    /// Takes the host platform explicitly so the gating contract can be unit
    /// tested against literal os/arch pairs, independent of the test runner's
    /// actual OS/arch (issue #179).
    pub fn host_can_run_on(platform: Option<&Platform>, host: Option<&Platform>) -> bool {
        match platform {
            None => true,
            Some(p) if p.is_any() => true,
            Some(p) => host.is_none_or(|h| h.same_os_arch(p)),
        }
    }

    /// Returns [`segments`](Self::segments) lowercased to ASCII.
    pub fn ascii_segments(&self) -> Vec<String> {
        self.segments().into_iter().map(|s| s.to_ascii_lowercase()).collect()
    }

    /// Detects the platform of the current host.
    ///
    /// Delegates to [`OperatingSystem::current`] and [`Architecture::current`]
    /// to map Rust's compile-time target to OCI values. Returns `None` if the
    /// host OS or architecture is not in OCX's supported set.
    ///
    /// Note: an unsupported OS or arch returns `None` (no `Platform` at all),
    /// whereas an undetected libc (non-Linux, NixOS, failed probe) returns
    /// `Some(Specific { os_features: <empty>, .. })`. The host is known, but
    /// its libc family is not — subset matching then only matches entries with
    /// empty `os_features`.
    pub fn current() -> Option<Self> {
        let os = OperatingSystem::current()?;
        let arch = Architecture::current()?;
        Some(Self::Specific {
            os,
            arch,
            variant: None,
            os_features: super::host_capabilities::cached_os_features(),
        })
    }
}

/// Defaults to [`Platform::Any`] (platform-agnostic).
impl Default for Platform {
    fn default() -> Self {
        Self::any()
    }
}

// --- D1: directed compatibility relation, scoring, shared selection ---

/// Returns `true` when `offered` satisfies the requirement `required`.
///
/// Read "does `offered` satisfy the requirement `required`?". `required` is
/// what the caller needs satisfied — the host ([`Platform::current`]), an
/// explicit `--platform` value, or a project-lock host lookup key. `offered`
/// is a candidate — an OCI image-index child platform, or a lock/pin map key.
///
/// The relation is **not symmetric**:
///
/// - **`Any` asymmetry.** An `Any` *offer* satisfies every requirement (a
///   platform-agnostic artifact runs anywhere). An `Any` *requirement* is
///   satisfied only by an `Any` offer — an unknown/undetected host can run
///   only platform-agnostic content, never a `Specific` binary.
/// - **`os_features` inversion.** `os_features` are mandatory host
///   capabilities the offered binary demands, so the direction inverts
///   relative to the `required`/`offered` naming:
///   `offered.os_features ⊆ required.os_features` (the host's capabilities
///   must be a superset of what the binary demands). `variant` does **not**
///   invert — it is strict equality, gated on the *offer* declaring it: an
///   offer that leaves the field `None` imposes no constraint.
///
/// See `adr_platform_model_unification.md` D1 for the full truth table.
pub fn is_compatible(required: &Platform, offered: &Platform) -> bool {
    // An `Any` offer satisfies every requirement, checked before the
    // `required`-is-`Any` branch so an `Any` requirement still accepts an
    // `Any` offer.
    if offered.is_any() {
        return true;
    }

    let (
        Platform::Specific {
            os: required_os,
            arch: required_arch,
            variant: required_variant,
            os_features: required_os_features,
        },
        Platform::Specific {
            os: offered_os,
            arch: offered_arch,
            variant: offered_variant,
            os_features: offered_os_features,
        },
    ) = (required, offered)
    else {
        // `offered` is not `Any` (checked above) but `required` is `Any` —
        // an `Any` requirement is satisfiable only by an `Any` offer.
        return false;
    };

    if required_os != offered_os || required_arch != offered_arch {
        return false;
    }

    // Strict equality on `variant`, gated on the offer declaring a value —
    // an offer that leaves it `None` imposes no constraint.
    if offered_variant.is_some() && offered_variant != required_variant {
        return false;
    }

    // Subset, inverted: every feature the offer demands must be present in
    // what the requirement offers as host capabilities.
    offered_os_features
        .iter()
        .all(|feature| required_os_features.contains(feature))
}

/// Lexicographic compatibility score `(is_specific, matched_refinement_count,
/// matched_os_feature_count)` for an `offered` platform, higher wins.
///
/// Meaningful only for an `offered` that [`is_compatible`] with the
/// requirement under evaluation — [`is_compatible`] already guarantees that
/// whenever `offered` declares `variant` it equals the requirement's value
/// (offer-gated strict equality) and that every `os_features` value it
/// carries is a matched subset, so both counts are read straight off
/// `offered` without re-consulting the requirement.
///
/// `matched_refinement_count` (0-1: one point for a declared `variant`)
/// ranks **above** `matched_os_feature_count`: a refinement is a
/// strict-equality constraint the offer opted into, a harder commitment than
/// an `os_features` capability subset, so an offer that pins the exact
/// `variant` outranks an unconstrained offer even when the unconstrained one
/// matches more `os_features` (`(true, 1, 0) > (true, 0, 1)`). Without this
/// axis a `variant`-exact offer and an offer that leaves `variant` unset
/// scored identically whenever their `os_features` counts matched, tying at
/// the maximum score and producing a spurious `Selection::Ambiguous` for
/// what D1's truth table treats as two independently-compatible, rankable
/// offers (rows #8/#9).
///
/// A `Specific` offer with zero matched refinements and zero matched
/// features still outranks `Any` (`(true, 0, 0) > (false, 0, 0)`).
pub fn compatibility_score(offered: &Platform) -> (bool, usize, usize) {
    match offered {
        Platform::Any => (false, 0, 0),
        Platform::Specific {
            variant, os_features, ..
        } => {
            let matched_refinement_count = usize::from(variant.is_some());
            (true, matched_refinement_count, os_features.len())
        }
    }
}

/// Outcome of [`select_best`] — the shared selection contract for every
/// consumer of the D1 compatibility relation (fresh-resolve, lock-read,
/// authoring pin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection<T> {
    /// Exactly one candidate reached the maximum compatibility score.
    Found(T),
    /// Two or more candidates tied at the maximum score — the caller must
    /// disambiguate (e.g. via an explicit `--platform`).
    Ambiguous(Vec<T>),
    /// No candidate is compatible with the required platform.
    None,
}

/// Selects the max-scoring [`is_compatible`] candidate for `required` out of
/// `candidates`, using [`compatibility_score`] as the tiebreaker.
///
/// The single shared helper behind every D1 consumer — fresh-resolve
/// (`Index::select`), lock-read (`lookup_host_leaf`), and authoring pinning
/// (`resolve_for_specific`) all route through this function so the three
/// answer identically for the same `required` + candidate set.
pub fn select_best<T: Clone>(required: &Platform, candidates: &[(T, Platform)]) -> Selection<T> {
    let mut best_score: Option<(bool, usize, usize)> = None;
    let mut best: Vec<T> = Vec::new();

    for (item, offered) in candidates {
        if !is_compatible(required, offered) {
            continue;
        }
        let score = compatibility_score(offered);
        match best_score {
            Some(current) if score < current => continue,
            Some(current) if score == current => best.push(item.clone()),
            _ => {
                best_score = Some(score);
                best = vec![item.clone()];
            }
        }
    }

    match best.len() {
        0 => Selection::None,
        1 => Selection::Found(best[0].clone()),
        _ => Selection::Ambiguous(best),
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any => write!(f, "{}", ANY_STR),
            Self::Specific {
                os,
                arch,
                variant,
                os_features,
            } => {
                write!(f, "{}/{}", os, arch)?;
                if let Some(variant) = variant {
                    write!(f, "/{}", escape_platform_component(variant))?;
                }
                // Append the normalized (sorted + deduped) `os.features` set
                // as a single `+`-introduced, comma-separated list, so the
                // output is canonical and round-trips through `FromStr`.
                // Empty set → no suffix.
                let normalized = normalize_os_features(os_features);
                if !normalized.is_empty() {
                    write!(f, "+")?;
                    for (index, feature) in normalized.iter().enumerate() {
                        if index > 0 {
                            write!(f, ",")?;
                        }
                        write!(f, "{}", escape_platform_component(feature))?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = PlatformError;

    fn from_str(value: &str) -> std::result::Result<Self, PlatformError> {
        let invalid = || PlatformError {
            input: value.to_string(),
            kind: PlatformErrorKind::InvalidFormat,
        };

        if value == ANY_STR {
            return Ok(Self::Any);
        }

        // Peel the optional `+feature-list` suffix first. A raw `+` is
        // always the feature-list introducer: a literal `+` inside a
        // registry-controlled value is percent-escaped to `%2B` by
        // `escape_platform_component`, so it never appears unescaped.
        let (core, os_features) = match value.split_once('+') {
            Some((head, feature_list)) => (head, split_feature_list(feature_list).ok_or_else(invalid)?),
            None => (value, Vec::new()),
        };

        // `os`/`arch`/`variant` are `/`-separated; any `/` inside a `variant`
        // value was escaped to `%2F`, so a literal `/` is always a separator.
        let parts: Vec<&str> = core.split('/').collect();
        if parts.len() < 2 || parts.len() > 3 {
            return Err(invalid());
        }

        let os: OperatingSystem = parts[0].parse().map_err(|kind| PlatformError {
            input: value.to_string(),
            kind,
        })?;
        let arch: Architecture = parts[1].parse().map_err(|kind| PlatformError {
            input: value.to_string(),
            kind,
        })?;

        let variant = match parts.get(2) {
            Some(segment) => {
                let variant = unescape_platform_component(segment).ok_or_else(invalid)?;
                if variant.is_empty() {
                    return Err(invalid());
                }
                Some(variant)
            }
            None => None,
        };

        Ok(Self::Specific {
            os,
            arch,
            variant,
            // Normalize (sort + dedup) so the parsed value matches the
            // canonical wire form used everywhere else.
            os_features: normalize_os_features(&os_features),
        })
    }
}

/// Sort + dedup `os_features` so the value is canonical.
///
/// Returns an empty `Vec` for an empty input. Keeps cascade eviction
/// (positional `Vec` equality on the wire) and `Display` output deterministic
/// regardless of the order the caller supplied.
fn normalize_os_features(os_features: &[String]) -> Vec<String> {
    // Fast path: 0 or 1 element is already sorted and deduplicated.
    if os_features.len() <= 1 {
        return os_features.to_vec();
    }
    let mut features = os_features.to_vec();
    features.sort();
    features.dedup();
    features
}

// --- D2: canonical Display/FromStr grammar escaping ---
//
// The single escaping scheme for the canonical grammar: `Display`/`FromStr`,
// the CLI `--platform` flag, and every `ocx.lock` / dependency-pin map key
// all share this one codec (see `adr_platform_model_unification.md` D2). The
// former `lock_key` family — a second, parallel encoding invented only
// because `Display` used to be lossy — is gone; `Display` is now unconditionally
// injective (see the `features` field deletion note on `Platform::Specific`),
// so no second codec is needed.

/// Percent-escapes every character that is structural in the canonical
/// [`Display`]/[`FromStr`](std::str::FromStr) grammar (D2): `/` (the
/// os/arch/variant separator), `+` (the feature-list introducer), `,` (the
/// feature separator), and `%` itself (the escape introducer).
///
/// Applied to every registry-controlled string that enters the grammar
/// (`variant` and each `os_features` value) so none of them can forge a
/// structural character and misparse. All other bytes pass through verbatim,
/// so the common values (`v7`, `v8`, `libc.glibc`) are byte-identical to
/// their raw form.
fn escape_platform_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '/' => out.push_str("%2F"),
            '+' => out.push_str("%2B"),
            ',' => out.push_str("%2C"),
            other => out.push(other),
        }
    }
    out
}

/// Reverses [`escape_platform_component`]: decodes the four percent-escapes
/// it emits (`%25 %2F %2B %2C`) back to their literal bytes, passing every
/// other character through verbatim.
///
/// Returns `None` on a malformed escape (`%` not followed by one of the four
/// known two-character codes) — the input is not a value this codec produced.
fn unescape_platform_component(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let decoded = match (chars.next(), chars.next()) {
                (Some('2'), Some('5')) => '%',
                (Some('2'), Some('F')) => '/',
                (Some('2'), Some('B')) => '+',
                (Some('2'), Some('C')) => ',',
                _ => return None,
            };
            out.push(decoded);
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

/// Splits a `+`-introduced feature-list segment on unescaped `,` and
/// unescapes each component — the inverse of the `+`-joined [`Display`]
/// suffix.
///
/// Returns `None` for an empty segment (`linux/amd64+`) or an empty feature
/// token (`linux/amd64+a,,b`) — a feature-list carries at least one
/// non-empty value.
fn split_feature_list(segment: &str) -> Option<Vec<String>> {
    if segment.is_empty() {
        return None;
    }
    segment
        .split(',')
        .map(|token| {
            if token.is_empty() {
                None
            } else {
                unescape_platform_component(token)
            }
        })
        .collect()
}

// --- Native conversion impls ---

impl From<&Platform> for native::Platform {
    fn from(p: &Platform) -> Self {
        match p {
            Platform::Any => native::Platform {
                os: native::Os::Other(ANY_STR.to_string()),
                architecture: native::Arch::Other(ANY_STR.to_string()),
                variant: None,
                features: None,
                os_version: None,
                os_features: None,
            },
            Platform::Specific {
                os,
                arch,
                variant,
                os_features,
            } => {
                // Normalize (sort + dedup) so the wire format is canonical.
                // Cascade eviction at `oci/client.rs` compares `os_features`
                // by positional `Vec` equality; a reordered array would
                // otherwise fail to evict the prior entry and bloat the index.
                // Convert OCX's empty-`Vec`-means-none to the native
                // `Option<Vec>` at this boundary so an empty set is omitted
                // from the serialized JSON (`skip_serializing_if`).
                let normalized = normalize_os_features(os_features);
                native::Platform {
                    os: (*os).into(),
                    architecture: (*arch).into(),
                    variant: variant.clone(),
                    // `features` is RESERVED by OCI v1.1.1 and no longer
                    // exists on `Platform::Specific` — always emit `None`.
                    features: None,
                    // `os_version` has no OCX platform-model concept —
                    // always emit `None`.
                    os_version: None,
                    os_features: if normalized.is_empty() { None } else { Some(normalized) },
                }
            }
        }
    }
}

impl From<Platform> for native::Platform {
    fn from(p: Platform) -> Self {
        native::Platform::from(&p)
    }
}

impl TryFrom<native::Platform> for Platform {
    type Error = crate::Error;

    fn try_from(platform: native::Platform) -> Result<Self> {
        let input = platform.to_string();

        if let (native::Os::Other(os), native::Arch::Other(arch)) = (&platform.os, &platform.architecture) {
            if os == ANY_STR && arch == ANY_STR && platform.variant.is_none() {
                return Ok(Platform::Any);
            }
            return Err(PlatformError {
                input,
                kind: PlatformErrorKind::Unsupported(platform.to_string()),
            }
            .into());
        }

        let os = OperatingSystem::try_from(platform.os).map_err(|kind| PlatformError {
            input: input.clone(),
            kind,
        })?;
        let arch = Architecture::try_from(platform.architecture).map_err(|kind| PlatformError {
            input: input.clone(),
            kind,
        })?;

        // `features` is RESERVED by OCI v1.1.1. Foreign manifests written by
        // stale tooling may still carry a value — warn and drop it rather than
        // hard-erroring, so OCX stays interoperable with such registries.
        if platform.features.is_some() {
            tracing::warn!(
                "dropping RESERVED `features` field from platform '{}' (OCI v1.1.1 reserves it)",
                input
            );
        }

        // `os.version` has no OCX platform-model concept. Foreign manifests
        // may still carry a value — warn and drop it, the same pattern as
        // the RESERVED `features` field above.
        if platform.os_version.is_some() {
            tracing::warn!(
                "dropping unsupported `os.version` field from platform '{}' (OCX platform model has no os_version concept)",
                input
            );
        }

        Ok(Self::Specific {
            os,
            arch,
            variant: platform.variant,
            // Convert the native `Option<Vec>` to OCX's empty-means-none `Vec`
            // at this boundary: both `None` and `Some(empty)` become an empty
            // set. Normalize (sort + dedup) inbound: foreign manifests may carry
            // duplicated or unordered `os_features`, and selection scores
            // specificity by `os_features.len()` — an un-deduped array would
            // inflate that score and skew candidate ranking.
            os_features: normalize_os_features(&platform.os_features.unwrap_or_default()),
        })
    }
}

impl TryFrom<Option<native::Platform>> for Platform {
    type Error = crate::Error;

    fn try_from(platform: Option<native::Platform>) -> Result<Self> {
        match platform {
            Some(p) => Self::try_from(p),
            None => Ok(Self::default()),
        }
    }
}

// --- Serde via native::Platform ---
//
// Serialization converts to native::Platform first, ensuring OCI-compatible
// JSON field names. Deserialization goes the reverse path: JSON → native::Platform
// → Platform (via TryFrom, which validates supported OS/arch).

impl Serialize for Platform {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        native::Platform::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Platform {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let native = native::Platform::deserialize(deserializer)?;
        Platform::try_from(native).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- D1: is_compatible / compatibility_score / select_best ---

    /// Truth-table rows #1-16 from `adr_platform_model_unification.md` D1,
    /// reproduced verbatim. `R` = required, `O` = offered.
    #[test]
    fn is_compatible_truth_table() {
        let cases: &[(&str, &str, &str, bool)] = &[
            ("1", "any", "any", true),
            ("2", "any", "linux/amd64", false),
            ("3", "linux/amd64", "any", true),
            ("4", "linux/amd64", "linux/amd64", true),
            ("5", "linux/amd64", "linux/arm64", false),
            ("6", "linux/amd64", "windows/amd64", false),
            ("7", "linux/arm64", "linux/arm64/v8", false),
            ("8", "linux/arm64/v8", "linux/arm64/v8", true),
            ("9", "linux/arm64/v8", "linux/arm64", true),
            ("10", "linux/arm64/v8", "linux/arm64/v7", false),
            ("11", "linux/amd64+libc.glibc", "linux/amd64", true),
            ("12", "linux/amd64+libc.glibc", "linux/amd64+libc.glibc", true),
            ("13", "linux/amd64+libc.glibc", "linux/amd64+libc.musl", false),
            ("14", "linux/amd64", "linux/amd64+libc.glibc", false),
            ("15", "linux/amd64+libc.glibc,libc.musl", "linux/amd64+libc.glibc", true),
            (
                "16",
                "linux/amd64+libc.glibc,libc.musl",
                "linux/amd64+libc.glibc,libc.musl",
                true,
            ),
        ];
        for (row, required, offered, expected) in cases {
            let required: Platform = required.parse().unwrap();
            let offered: Platform = offered.parse().unwrap();
            assert_eq!(
                is_compatible(&required, &offered),
                *expected,
                "truth-table row #{row}: is_compatible({required}, {offered})"
            );
        }
    }

    #[test]
    fn compatibility_score_specific_beats_any() {
        let bare: Platform = "linux/amd64".parse().unwrap();
        assert!(
            compatibility_score(&bare) > compatibility_score(&Platform::Any),
            "(1,0) must outrank (0,0)"
        );
    }

    #[test]
    fn compatibility_score_more_matched_features_win() {
        let bare: Platform = "linux/amd64".parse().unwrap();
        let with_feature: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        assert!(
            compatibility_score(&with_feature) > compatibility_score(&bare),
            "(1,1) must outrank (1,0)"
        );
    }

    /// Authoring-vs-Index parity (a): Specific + `Any` candidates → Specific
    /// wins, no ambiguity.
    #[test]
    fn select_best_specific_beats_any_no_ambiguity() {
        let host: Platform = "linux/amd64".parse().unwrap();
        let candidates = vec![("specific", host.clone()), ("agnostic", Platform::Any)];
        assert_eq!(select_best(&host, &candidates), Selection::Found("specific"));
    }

    /// Authoring-vs-Index parity (b): feature-specific + bare candidates →
    /// feature-specific wins.
    #[test]
    fn select_best_feature_specific_beats_bare() {
        let host: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let bare: Platform = "linux/amd64".parse().unwrap();
        let glibc: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let candidates = vec![("bare", bare), ("glibc", glibc)];
        assert_eq!(select_best(&host, &candidates), Selection::Found("glibc"));
    }

    /// Equal max score → `Ambiguous`: a dual-libc host against separate
    /// single-libc offers both score `(true, 1)`.
    #[test]
    fn select_best_equal_max_score_is_ambiguous() {
        let host: Platform = "linux/amd64+libc.glibc,libc.musl".parse().unwrap();
        let glibc: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let musl: Platform = "linux/amd64+libc.musl".parse().unwrap();
        let result = select_best(&host, &[("glibc", glibc), ("musl", musl)]);
        assert_eq!(result, Selection::Ambiguous(vec!["glibc", "musl"]));
    }

    #[test]
    fn select_best_none_when_no_candidate_compatible() {
        let host: Platform = "linux/amd64".parse().unwrap();
        let windows: Platform = "windows/amd64".parse().unwrap();
        let candidates = vec![("windows", windows)];
        assert_eq!(select_best(&host, &candidates), Selection::None);
    }

    // --- F1 regression: refinement-aware scoring (review round 1) ---

    /// Truth-table rows #8/#9: a `variant`-exact offer and an offer that
    /// leaves `variant` unset are both `is_compatible`, but before the F1
    /// fix they scored identically (`(true, 0)`), tying at the maximum score
    /// and producing a spurious `Ambiguous`. The refinement axis now ranks
    /// the exact match above the unconstrained one.
    #[test]
    fn select_best_variant_exact_beats_unconstrained_no_ambiguity() {
        let host: Platform = "linux/arm64/v8".parse().unwrap();
        let exact: Platform = "linux/arm64/v8".parse().unwrap();
        let unconstrained: Platform = "linux/arm64".parse().unwrap();
        let candidates = vec![("exact", exact), ("unconstrained", unconstrained)];
        assert_eq!(select_best(&host, &candidates), Selection::Found("exact"));
    }

    /// "Bare-vs-bare" sanity: when neither offer declares a `variant` (the
    /// refinement axis is `0` for both), ranking still falls through to the
    /// `os_features` axis exactly as before the F1 fix.
    #[test]
    fn select_best_bare_vs_bare_ranks_by_feature_count() {
        let host: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let bare: Platform = "linux/amd64".parse().unwrap();
        let glibc: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let candidates = vec![("bare", bare), ("glibc", glibc)];
        assert_eq!(select_best(&host, &candidates), Selection::Found("glibc"));
    }

    /// Ordering: the refinement axis ranks ABOVE the `os_features` axis — a
    /// `variant`-exact offer with zero matched features beats an
    /// unconstrained offer that matches one feature.
    #[test]
    fn select_best_refinement_axis_outranks_feature_count() {
        let host: Platform = "linux/arm64/v8+libc.glibc".parse().unwrap();
        let variant_exact_no_features: Platform = "linux/arm64/v8".parse().unwrap();
        let unconstrained_with_feature: Platform = "linux/arm64+libc.glibc".parse().unwrap();
        let candidates = vec![
            ("variant_exact", variant_exact_no_features),
            ("feature_match", unconstrained_with_feature),
        ];
        assert_eq!(select_best(&host, &candidates), Selection::Found("variant_exact"));
    }

    /// Genuinely tied at the refinement axis (two distinct candidate items
    /// both offering the identical `variant`-exact platform) still resolves
    /// to `Ambiguous` — the new axis ranks real differences, it does not
    /// manufacture a winner among equals.
    #[test]
    fn select_best_refinement_tie_still_ambiguous() {
        let host: Platform = "linux/arm64/v8".parse().unwrap();
        let a: Platform = "linux/arm64/v8".parse().unwrap();
        let b: Platform = "linux/arm64/v8".parse().unwrap();
        let result = select_best(&host, &[("a", a), ("b", b)]);
        assert_eq!(result, Selection::Ambiguous(vec!["a", "b"]));
    }

    // --- Display / FromStr ---

    #[test]
    fn display_fromstr_roundtrip() {
        let cases = ["any", "linux/amd64", "darwin/arm64", "windows/amd64", "linux/amd64/v8"];
        for case in cases {
            let platform: Platform = case.parse().unwrap();
            let displayed = platform.to_string();
            let reparsed: Platform = displayed.parse().unwrap();
            assert_eq!(platform, reparsed, "roundtrip failed for '{}'", case);
        }
    }

    #[test]
    fn fromstr_invalid_format() {
        assert!("".parse::<Platform>().is_err());
        assert!("just_one".parse::<Platform>().is_err());
        assert!("a/b/c/d/e".parse::<Platform>().is_err());
        // `os/arch/variant` is the maximum slash-separated depth — a 4th
        // `/`-segment is structurally invalid.
        assert!("linux/amd64/v8/10.0.14393".parse::<Platform>().is_err());
    }

    #[test]
    fn fromstr_invalid_os() {
        let err = "linus/amd64".parse::<Platform>().unwrap_err();
        assert!(matches!(err.kind, PlatformErrorKind::UnsupportedOs { .. }));
    }

    #[test]
    fn fromstr_invalid_arch() {
        let err = "linux/x86".parse::<Platform>().unwrap_err();
        assert!(matches!(err.kind, PlatformErrorKind::UnsupportedArch { .. }));
    }

    /// D2: `any` is a literal token, not an alias family — `"any/any"` (the
    /// old grammar's alternate spelling of `Platform::Any`) is no longer
    /// accepted; `"any"` carries no fields, so any suffix is rejected.
    #[test]
    fn any_rejects_any_suffix() {
        assert!("any/any".parse::<Platform>().is_err(), "`any/…` must be rejected");
        assert!("any/amd64".parse::<Platform>().is_err(), "`any/…` must be rejected");
        assert!("any@1.0".parse::<Platform>().is_err(), "`any@…` must be rejected");
        assert!(
            "any+libc.glibc".parse::<Platform>().is_err(),
            "`any+…` must be rejected"
        );
    }

    // --- FromStr with os.features (`+feature`) suffix ---

    #[test]
    fn fromstr_single_feature() {
        let platform: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(os_features.as_slice(), &["libc.glibc".to_string()]);
            }
            _ => panic!("expected Specific"),
        }
    }

    #[test]
    fn fromstr_feature_with_variant() {
        let platform: Platform = "linux/amd64/v3+libc.musl".parse().unwrap();
        match &platform {
            Platform::Specific {
                variant, os_features, ..
            } => {
                assert_eq!(variant.as_deref(), Some("v3"));
                assert_eq!(os_features.as_slice(), &["libc.musl".to_string()]);
            }
            _ => panic!("expected Specific"),
        }
    }

    #[test]
    fn fromstr_multiple_features_sorted_and_deduped() {
        // Input order `b.y` before `a.x` plus a duplicate; the parsed value
        // must be sorted and deduped.
        let platform: Platform = "linux/amd64+b.y,a.x,a.x".parse().unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(
                    os_features.as_slice(),
                    &["a.x".to_string(), "b.y".to_string()],
                    "features must be sorted + deduped"
                );
            }
            _ => panic!("expected Specific"),
        }
    }

    #[test]
    fn fromstr_dual_libc_features() {
        // A dual-libc `--platform` override carries both libc tags via a
        // single `+`-introduced comma list; they parse into a sorted
        // os_features Vec.
        let platform: Platform = "linux/amd64+libc.glibc,libc.musl".parse().unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(
                    os_features.as_slice(),
                    &["libc.glibc".to_string(), "libc.musl".to_string()],
                    "both libc features must parse, sorted"
                );
            }
            _ => panic!("expected Specific"),
        }
        // Round-trips through Display.
        let reparsed: Platform = platform.to_string().parse().unwrap();
        assert_eq!(platform, reparsed, "dual-libc --platform must round-trip");
    }

    #[test]
    fn fromstr_feature_roundtrips_via_display() {
        for case in [
            "linux/amd64+libc.glibc",
            "linux/amd64/v3+libc.musl",
            "linux/amd64+a.x,b.y",
        ] {
            let platform: Platform = case.parse().unwrap();
            let arg = platform.to_string();
            let reparsed: Platform = arg.parse().unwrap();
            assert_eq!(platform, reparsed, "round-trip via Display failed for '{case}'");
        }
    }

    /// D2 property test: `Platform::from_str(&p.to_string()) == Ok(p)` over
    /// every field combination, including registry-controlled values
    /// carrying each of the four reserved characters (`% / + ,`).
    #[test]
    fn display_fromstr_roundtrip_with_reserved_characters() {
        let cases = vec![
            Platform::Any,
            Platform::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Amd64,
                variant: Some("v8/10".to_string()),
                os_features: Vec::new(),
            },
            Platform::Specific {
                os: OperatingSystem::Windows,
                arch: Architecture::Amd64,
                variant: Some("v8@10".to_string()),
                os_features: Vec::new(),
            },
            Platform::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Amd64,
                variant: None,
                os_features: vec!["a,b".to_string()],
            },
            Platform::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Amd64,
                variant: None,
                os_features: vec!["a".to_string(), "b".to_string()],
            },
            Platform::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Amd64,
                variant: Some("100%".to_string()),
                os_features: vec!["a+b".to_string(), "c%d".to_string()],
            },
        ];
        for platform in &cases {
            let displayed = platform.to_string();
            let reparsed: Platform = displayed
                .parse()
                .unwrap_or_else(|error| panic!("round-trip parse failed for '{displayed}': {error}"));
            assert_eq!(&reparsed, platform, "round-trip failed for '{displayed}'");
        }
    }

    /// Injectivity canary (ported from the `lock_key` family per the Risks
    /// note in `adr_platform_model_unification.md`): a single `os_features`
    /// value containing a `,` must not alias a two-element list under
    /// `Display`.
    #[test]
    fn display_injective_for_comma_bearing_feature_value() {
        let one_value_with_comma = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_features: vec!["a,b".to_string()],
        };
        let two_values = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_features: vec!["a".to_string(), "b".to_string()],
        };
        assert_ne!(
            one_value_with_comma.to_string(),
            two_values.to_string(),
            "a single comma-bearing feature value must not collide with a two-element list"
        );
        assert_eq!(
            one_value_with_comma.to_string().parse::<Platform>().unwrap(),
            one_value_with_comma
        );
        assert_eq!(two_values.to_string().parse::<Platform>().unwrap(), two_values);
    }

    /// `@` carries no grammar meaning — a variant containing it round-trips
    /// as a literal character, unescaped.
    #[test]
    fn display_variant_at_sign_round_trips_unescaped() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: Some("v8@10".to_string()),
            os_features: Vec::new(),
        };
        assert_eq!(
            platform.to_string(),
            "linux/amd64/v8@10",
            "'@' must not be percent-escaped"
        );
        assert_eq!(platform.to_string().parse::<Platform>().unwrap(), platform);
    }

    /// Injectivity canary: a `variant` containing `+` must not forge the
    /// feature-list introducer.
    #[test]
    fn display_injective_for_plus_bearing_variant() {
        let forged_features = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: Some("v3+libc.glibc".to_string()),
            os_features: Vec::new(),
        };
        let genuine_features = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: Some("v3".to_string()),
            os_features: vec!["libc.glibc".to_string()],
        };
        assert_ne!(forged_features.to_string(), genuine_features.to_string());
        assert_eq!(
            forged_features.to_string().parse::<Platform>().unwrap(),
            forged_features
        );
    }

    #[test]
    fn fromstr_empty_feature_token_is_error() {
        assert!(
            "linux/amd64+".parse::<Platform>().is_err(),
            "trailing + must be rejected"
        );
        assert!(
            "linux/amd64+libc.glibc,".parse::<Platform>().is_err(),
            "empty trailing feature token must be rejected"
        );
        assert!(
            "linux/amd64+a,,b".parse::<Platform>().is_err(),
            "empty interior feature token must be rejected"
        );
    }

    // --- F3: malformed percent-escape rejection (unescape_platform_component) ---

    #[test]
    fn unescape_rejects_unknown_escape_code() {
        assert_eq!(
            unescape_platform_component("%zz"),
            None,
            "an unknown two-char escape code must be rejected, not panic"
        );
    }

    #[test]
    fn unescape_rejects_trailing_percent() {
        assert_eq!(
            unescape_platform_component("v8%"),
            None,
            "a bare trailing '%' with nothing to decode must be rejected, not panic"
        );
    }

    #[test]
    fn unescape_rejects_truncated_escape() {
        assert_eq!(
            unescape_platform_component("v8%2"),
            None,
            "a truncated two-char escape (only one hex digit present) must be rejected, not panic"
        );
    }

    #[test]
    fn unescape_roundtrips_escaped_percent() {
        // `%25` is the codec's own encoding for a literal `%` (the escape
        // introducer escaping itself) — decoding it must yield the literal
        // character, and re-escaping must reproduce `%25` exactly.
        assert_eq!(unescape_platform_component("100%25"), Some("100%".to_string()));
        assert_eq!(escape_platform_component("100%"), "100%25");
    }

    #[test]
    fn unescape_handles_consecutive_escaped_introducers() {
        // A decoded value containing further `%` bytes (via `%25`) must not
        // confuse the decoder into misreading the next escape.
        assert_eq!(unescape_platform_component("%25%2F"), Some("%/".to_string()));
    }

    /// The malformed-escape cases above reached via the public parser, on
    /// the `variant` slot — confirms `FromStr` surfaces a clean
    /// `InvalidFormat` error (never a panic) end to end.
    #[test]
    fn fromstr_rejects_malformed_percent_escape_in_variant() {
        assert!(
            "linux/amd64/v8%zz".parse::<Platform>().is_err(),
            "unknown escape code in variant must error, not panic"
        );
        assert!(
            "linux/amd64/v8%".parse::<Platform>().is_err(),
            "trailing % in variant must error, not panic"
        );
        assert!(
            "linux/amd64/v8%2".parse::<Platform>().is_err(),
            "truncated escape in variant must error, not panic"
        );
    }

    #[test]
    fn display_renders_sorted_features() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_features: vec!["b.y".to_string(), "a.x".to_string()],
        };
        assert_eq!(platform.to_string(), "linux/amd64+a.x,b.y");
    }

    #[test]
    fn display_without_features_omits_suffix() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        assert_eq!(platform.to_string(), "linux/amd64");
        assert_eq!(Platform::Any.to_string(), "any");
    }

    #[test]
    fn display_includes_os_features() {
        // Display is now the canonical --platform form: it carries the
        // +features suffix. Filesystem paths use segments()/ascii_segments().
        let platform: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        assert_eq!(platform.to_string(), "linux/amd64+libc.glibc");
    }

    #[test]
    fn same_os_arch_ignores_refinement_fields() {
        let plain: Platform = "linux/amd64".parse().unwrap();
        let with_variant: Platform = "linux/amd64/v3".parse().unwrap();
        // os+arch equal, differ only in variant → same_os_arch ignores the refinement.
        assert!(plain.same_os_arch(&with_variant));
        // Different arch / os → not same.
        assert!(!plain.same_os_arch(&"linux/arm64".parse().unwrap()));
        assert!(!plain.same_os_arch(&"windows/amd64".parse().unwrap()));
        // Any algebra.
        assert!(Platform::Any.same_os_arch(&Platform::Any));
        assert!(!Platform::Any.same_os_arch(&plain));
    }

    /// Regression (issue #179): the host-only gate compares os+arch only, so a
    /// host-runnable platform carrying a `variant` refinement is NOT suppressed.
    #[test]
    fn host_can_run_on_gate() {
        let host: Platform = "linux/amd64".parse().unwrap();
        let host = Some(&host);

        // Undeterminable platform and platform-agnostic package always run.
        assert!(Platform::host_can_run_on(None, host));
        assert!(Platform::host_can_run_on(Some(&Platform::any()), host));

        // Matching os+arch runs; a differing os or arch does not.
        assert!(Platform::host_can_run_on(Some(&"linux/amd64".parse().unwrap()), host));
        assert!(!Platform::host_can_run_on(
            Some(&"windows/amd64".parse().unwrap()),
            host
        ));
        assert!(!Platform::host_can_run_on(Some(&"linux/arm64".parse().unwrap()), host));

        // Key case: matching os+arch with a variant refinement still runs.
        assert!(Platform::host_can_run_on(
            Some(&"linux/amd64/v3".parse().unwrap()),
            host
        ));

        // Unknown host never suppresses.
        let foreign: Platform = "windows/amd64".parse().unwrap();
        assert!(Platform::host_can_run_on(Some(&foreign), None));
    }

    // --- segments() ---

    #[test]
    fn segments_any() {
        assert_eq!(Platform::Any.segments(), vec!["any"]);
    }

    #[test]
    fn segments_specific() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        assert_eq!(platform.segments(), vec!["linux", "amd64"]);
    }

    #[test]
    fn segments_with_variant() {
        let platform: Platform = "linux/arm64/v8".parse().unwrap();
        assert_eq!(platform.segments(), vec!["linux", "arm64", "v8"]);
    }

    // --- ascii_segments() ---

    #[test]
    fn ascii_segments_lowercases() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: Some("V8".to_string()),
            os_features: Vec::new(),
        };
        assert_eq!(platform.ascii_segments(), vec!["linux", "arm64", "v8"]);
    }

    #[test]
    fn ascii_segments_any() {
        assert_eq!(Platform::Any.ascii_segments(), vec!["any"]);
    }

    // --- Misc ---

    #[test]
    fn current_returns_some() {
        let current = Platform::current();
        assert!(current.is_some());
        assert!(!current.unwrap().is_any());
    }

    #[test]
    fn hash_equality() {
        let a: Platform = "linux/amd64".parse().unwrap();
        let b: Platform = "linux/amd64".parse().unwrap();
        let mut set = std::collections::HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn default_is_any() {
        assert!(Platform::default().is_any());
    }

    // --- from_image_manifest / from_image_index / from_manifest ---

    #[test]
    fn from_image_manifest_returns_any() {
        let manifest = native::ImageManifest {
            schema_version: 2,
            media_type: None,
            config: native::OciDescriptor {
                media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                digest: "sha256:abc".to_string(),
                size: 100,
                urls: None,
                artifact_type: None,
                annotations: None,
            },
            layers: vec![],
            annotations: None,
            artifact_type: None,
            subject: None,
        };
        assert!(Platform::from_image_manifest(&manifest).is_any());
    }

    #[test]
    fn from_image_index_extracts_platforms() {
        let index = native::ImageIndex {
            schema_version: 2,
            media_type: None,
            manifests: vec![
                native::ImageIndexEntry {
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                    digest: "sha256:aaa".to_string(),
                    size: 100,
                    platform: Some(native::Platform {
                        os: native::Os::Linux,
                        architecture: native::Arch::Amd64,
                        variant: None,
                        features: None,
                        os_version: None,
                        os_features: None,
                    }),
                    artifact_type: None,
                    annotations: None,
                },
                native::ImageIndexEntry {
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                    digest: "sha256:bbb".to_string(),
                    size: 200,
                    platform: None,
                    artifact_type: None,
                    annotations: None,
                },
            ],
            artifact_type: None,
            annotations: None,
        };
        let platforms = Platform::from_image_index(&index).unwrap();
        assert_eq!(platforms.len(), 2);
        assert_eq!(platforms[0].to_string(), "linux/amd64");
        assert!(platforms[1].is_any());
    }

    #[test]
    fn from_image_index_rejects_unsupported_platform() {
        let index = native::ImageIndex {
            schema_version: 2,
            media_type: None,
            manifests: vec![native::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: "sha256:ccc".to_string(),
                size: 100,
                platform: Some(native::Platform {
                    os: native::Os::FreeBSD,
                    architecture: native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: None,
                }),
                artifact_type: None,
                annotations: None,
            }],
            artifact_type: None,
            annotations: None,
        };
        assert!(Platform::from_image_index(&index).is_err());
    }

    // --- Serde ---

    #[test]
    fn serde_roundtrip_specific() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        let json = serde_json::to_string(&platform).unwrap();
        let parsed: Platform = serde_json::from_str(&json).unwrap();
        assert_eq!(platform, parsed);
    }

    #[test]
    fn serde_oci_field_names() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        let json = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["os"], "linux");
        assert_eq!(value["architecture"], "amd64");
    }

    #[test]
    fn serde_any_serializes_with_any_values() {
        let json = serde_json::to_string(&Platform::Any).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["os"], "any");
        assert_eq!(value["architecture"], "any");
    }

    #[test]
    fn serde_any_roundtrip() {
        let json = serde_json::to_string(&Platform::Any).unwrap();
        let parsed: Platform = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_any());
    }

    #[test]
    fn serde_os_features_accepted_and_roundtripped() {
        let json = r#"{"os":"windows","architecture":"amd64","os.features":["win32k"]}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(os_features.as_slice(), &["win32k".to_string()]);
            }
            _ => panic!("expected Specific"),
        }
        let json_out = serde_json::to_string(&platform).unwrap();
        let reparsed: Platform = serde_json::from_str(&json_out).unwrap();
        assert_eq!(platform, reparsed);
    }

    #[test]
    fn serde_rejects_unsupported_os_from_registry() {
        let json = r#"{"architecture":"ppc64le","os":"linux"}"#;
        assert!(serde_json::from_str::<Platform>(json).is_err());
    }

    #[test]
    fn serde_rejects_unsupported_arch_from_registry() {
        let json = r#"{"architecture":"s390x","os":"linux"}"#;
        assert!(serde_json::from_str::<Platform>(json).is_err());
    }

    #[test]
    fn serde_rejects_unsupported_os() {
        let json = r#"{"architecture":"amd64","os":"freebsd"}"#;
        assert!(serde_json::from_str::<Platform>(json).is_err());
    }

    #[test]
    fn serde_roundtrip_all_supported_combinations() {
        for os in OperatingSystem::VARIANTS {
            for arch in Architecture::VARIANTS {
                let platform = Platform::Specific {
                    os: *os,
                    arch: *arch,
                    variant: None,
                    os_features: Vec::new(),
                };
                let json = serde_json::to_string(&platform).unwrap();
                let parsed: Platform = serde_json::from_str(&json).unwrap();
                assert_eq!(platform, parsed, "roundtrip failed for {}/{}", os, arch);
            }
        }
    }

    #[test]
    fn serde_variant_preserved_through_roundtrip() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: Some("v8".to_string()),
            os_features: Vec::new(),
        };
        let json = serde_json::to_string(&platform).unwrap();
        let parsed: Platform = serde_json::from_str(&json).unwrap();
        assert_eq!(platform, parsed);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["variant"], "v8");
    }

    #[test]
    fn serde_optional_fields_absent_when_none() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        let json = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("variant").is_none());
        assert!(value.get("os.version").is_none());
        assert!(value.get("os.features").is_none());
        assert!(value.get("features").is_none());
    }

    // --- OCI JSON compatibility tests ---

    #[test]
    fn serde_deserialize_oci_index_platform_entry() {
        let json = r#"{"architecture":"amd64","os":"linux"}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Amd64,
                variant: None,
                os_features: Vec::new(),
            }
        );
    }

    #[test]
    fn serde_deserialize_platform_with_variant() {
        let json = r#"{"architecture":"arm64","os":"linux","variant":"v8"}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Arm64,
                variant: Some("v8".to_string()),
                os_features: Vec::new(),
            }
        );
    }

    /// `os.version` has no OCX platform-model concept — a manifest platform
    /// entry carrying it deserializes successfully (warn-and-drop, the same
    /// pattern as the RESERVED `features` field) and the key never
    /// reappears on serialize.
    #[test]
    fn serde_os_version_in_input_json_is_dropped() {
        let json = r#"{"architecture":"amd64","os":"windows","os.version":"10.0.14393.1066"}"#;
        let result = serde_json::from_str::<Platform>(json);
        assert!(
            result.is_ok(),
            "deserializing a platform with `os.version` must not hard-error; err: {:?}",
            result.err()
        );
        let platform = result.unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Windows,
                arch: Architecture::Amd64,
                variant: None,
                os_features: Vec::new(),
            }
        );
        let json_out = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_out).unwrap();
        assert!(
            value.get("os.version").is_none(),
            "`os.version` must not reappear after warn-and-drop deserialization; got: {}",
            json_out
        );
    }

    // --- Native roundtrip (lossless: native → Platform → native) ---

    #[test]
    fn native_roundtrip_specific() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        let native_plat: native::Platform = platform.clone().into();
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_any() {
        let native_plat: native::Platform = Platform::Any.into();
        let back = Platform::try_from(native_plat).unwrap();
        assert!(back.is_any());
    }

    #[test]
    fn native_roundtrip_with_variant() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: Some("v8".to_string()),
            os_features: Vec::new(),
        };
        let native_plat: native::Platform = platform.clone().into();
        assert_eq!(native_plat.variant, Some("v8".to_string()));
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    /// `os.version` has no OCX platform-model concept — dropped on the
    /// native boundary, the same pattern as the RESERVED `features` field
    /// (see `native_features_dropped_in_conversion`).
    #[test]
    fn native_os_version_dropped_in_conversion() {
        let native_plat = native::Platform {
            os: native::Os::Windows,
            architecture: native::Arch::Amd64,
            variant: None,
            features: None,
            os_version: Some("10.0.14393.1066".to_string()),
            os_features: None,
        };
        let platform = Platform::try_from(native_plat).unwrap();
        let roundtripped: native::Platform = native::Platform::from(&platform);
        assert_eq!(
            roundtripped.os_version, None,
            "`os.version` must be emitted as None by From<&Platform> for native::Platform"
        );
    }

    #[test]
    fn native_roundtrip_with_os_features() {
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_features: vec!["win32k".to_string()],
        };
        let native_plat: native::Platform = platform.clone().into();
        assert_eq!(native_plat.os_features, Some(vec!["win32k".to_string()]));
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_with_all_optional_fields() {
        // `features` is RESERVED — deliberately dropped on conversion (see
        // `native_features_dropped_in_conversion`). The remaining fields must
        // survive a native → Platform → native roundtrip.
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: Some("v3".to_string()),
            os_features: vec!["win32k".to_string()],
        };
        let native_plat: native::Platform = platform.clone().into();
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_all_supported_combinations() {
        for os in OperatingSystem::VARIANTS {
            for arch in Architecture::VARIANTS {
                let platform = Platform::Specific {
                    os: *os,
                    arch: *arch,
                    variant: None,
                    os_features: Vec::new(),
                };
                let native_plat: native::Platform = platform.clone().into();
                let back = Platform::try_from(native_plat).unwrap();
                assert_eq!(platform, back, "native roundtrip failed for {}/{}", os, arch);
            }
        }
    }

    // ── serde: RESERVED features field rewrites (3.3) ──────────────

    /// REWRITE of serde_features_accepted_and_roundtripped (was: round-tripped; now: dropped on serialize).
    /// The OCI v1.1.1 `features` field is RESERVED — serializing it is a spec violation.
    /// After impl, a Platform with populated `features` must serialize WITHOUT a `features` key.
    #[test]
    fn serde_features_dropped_on_serialize() {
        // `features` no longer exists on `Platform::Specific` (D2), so the
        // only way to exercise a populated wire value is via a native
        // conversion; what matters here is the serialized wire format.
        let native_plat = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::Amd64,
            variant: None,
            features: Some(vec!["sse4".to_string(), "aes".to_string()]),
            os_version: None,
            os_features: None,
        };
        let platform = Platform::try_from(native_plat.clone()).unwrap();
        let json = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        // After impl: the RESERVED `features` key must NOT appear in the serialized JSON.
        assert!(
            value.get("features").is_none(),
            "RESERVED `features` field must be absent from serialized Platform JSON; got: {}",
            json
        );
    }

    /// Features key in input JSON is ignored/stripped during deserialization.
    #[test]
    fn serde_features_in_input_json_is_ignored() {
        // A registry may emit a `features` value (stale tooling); we must not error.
        let json = r#"{"os":"linux","architecture":"amd64","features":["sse4","aes"]}"#;
        // Deserialization must succeed (warn-and-drop, not hard error).
        let result = serde_json::from_str::<Platform>(json);
        assert!(
            result.is_ok(),
            "Deserializing a platform with RESERVED `features` must not hard-error; err: {:?}",
            result.err()
        );
        // And serialize back: features must not reappear.
        let platform = result.unwrap();
        let json_out = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_out).unwrap();
        assert!(
            value.get("features").is_none(),
            "`features` reappeared after warn-and-drop deserialization; got: {}",
            json_out
        );
    }

    /// REWRITE of serde_deserialize_platform_with_all_optional_fields.
    /// `features` is removed from the test JSON; `os.version` is kept in the
    /// JSON to exercise the warn-and-drop path alongside the other optional
    /// fields, but no longer appears in the expected struct — the existing
    /// `variant`/`os_features` values must still deserialize.
    #[test]
    fn serde_deserialize_platform_with_all_optional_fields_no_features() {
        // `features` intentionally omitted — it is RESERVED.
        let json = r#"{
            "architecture":"amd64",
            "os":"windows",
            "variant":"v3",
            "os.version":"10.0.14393.1066",
            "os.features":["win32k"]
        }"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Windows,
                arch: Architecture::Amd64,
                variant: Some("v3".to_string()),
                os_features: vec!["win32k".to_string()],
            }
        );
    }

    /// REWRITE of native_roundtrip_with_features.
    /// Any populated `features` in a native::Platform must be dropped (not round-tripped)
    /// through From<&Platform> for native::Platform because the field is RESERVED.
    #[test]
    fn native_features_dropped_in_conversion() {
        let native_plat = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::Amd64,
            variant: None,
            features: Some(vec!["sse4".to_string(), "aes".to_string()]),
            os_version: None,
            os_features: None,
        };
        let platform = Platform::try_from(native_plat).unwrap();
        let roundtripped: native::Platform = native::Platform::from(&platform);
        // After impl: features must be None on the wire (RESERVED).
        assert_eq!(
            roundtripped.features, None,
            "RESERVED `features` must be emitted as None by From<&Platform> for native::Platform"
        );
    }

    /// Extension of serde_os_features_accepted_and_roundtripped with libc.* values (3.3).
    #[test]
    fn serde_os_features_libc_values_roundtripped() {
        // glibc
        let json_glibc = r#"{"os":"linux","architecture":"amd64","os.features":["libc.glibc"]}"#;
        let platform: Platform = serde_json::from_str(json_glibc).unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(
                    os_features.as_slice(),
                    &["libc.glibc".to_string()],
                    "libc.glibc must be preserved"
                );
            }
            _ => panic!("expected Specific platform"),
        }
        let json_out = serde_json::to_string(&platform).unwrap();
        let reparsed: Platform = serde_json::from_str(&json_out).unwrap();
        assert_eq!(platform, reparsed, "libc.glibc roundtrip failed");

        // musl
        let json_musl = r#"{"os":"linux","architecture":"amd64","os.features":["libc.musl"]}"#;
        let platform_musl: Platform = serde_json::from_str(json_musl).unwrap();
        match &platform_musl {
            Platform::Specific { os_features, .. } => {
                assert_eq!(
                    os_features.as_slice(),
                    &["libc.musl".to_string()],
                    "libc.musl must be preserved"
                );
            }
            _ => panic!("expected Specific platform"),
        }
        let json_out_musl = serde_json::to_string(&platform_musl).unwrap();
        let reparsed_musl: Platform = serde_json::from_str(&json_out_musl).unwrap();
        assert_eq!(platform_musl, reparsed_musl, "libc.musl roundtrip failed");
    }

    // ── os_features normalization via From<&Platform> for native::Platform (3.7) ──

    /// Roundtrip Platform::Specific with unsorted os_features through native conversion.
    /// The resulting native::Platform.os_features must be sorted + deduped.
    #[test]
    fn native_os_features_sorted_and_deduped_on_conversion() {
        // Unsorted input: "b.x" before "a.y"
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_features: vec!["b.x".to_string(), "a.y".to_string()],
        };
        let native_plat: native::Platform = native::Platform::from(&platform);
        assert_eq!(
            native_plat.os_features,
            Some(vec!["a.y".to_string(), "b.x".to_string()]),
            "os_features must be sorted ascending in native Platform"
        );
    }

    /// Deduplication: duplicate os_features entries must be collapsed to one.
    #[test]
    fn native_os_features_deduped_on_conversion() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_features: vec!["libc.glibc".to_string(), "libc.glibc".to_string()],
        };
        let native_plat: native::Platform = native::Platform::from(&platform);
        assert_eq!(
            native_plat.os_features,
            Some(vec!["libc.glibc".to_string()]),
            "duplicate os_features must be deduped"
        );
    }

    #[test]
    fn deserialize_normalizes_duplicate_and_unsorted_os_features() {
        // A foreign manifest may carry duplicated / unordered os.features.
        // Selection scores specificity by os_features.len(), so an un-normalized
        // inbound array would inflate the score and skew candidate ranking.
        // Deserialization must sort + dedup at the boundary.
        let json = r#"{"os":"linux","architecture":"amd64","os.features":["libc.musl","libc.glibc","libc.musl"]}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => assert_eq!(
                os_features.as_slice(),
                &["libc.glibc".to_string(), "libc.musl".to_string()],
                "inbound os_features must be sorted + deduped so specificity scoring is not skewed"
            ),
            _ => panic!("expected Specific platform"),
        }
    }

    /// UPDATE of native_lossless_roundtrip_full_fidelity.
    /// `features` is removed from the asserted-lossless set (RESERVED —
    /// deliberately dropped); `os_version` is removed too (no OCX
    /// platform-model concept — also deliberately dropped, same pattern).
    #[test]
    fn native_lossless_roundtrip_full_fidelity_without_features_or_os_version() {
        // features is intentionally omitted — it is RESERVED and must not round-trip.
        let original = native::Platform {
            os: native::Os::Windows,
            architecture: native::Arch::Amd64,
            variant: Some("v3".to_string()),
            features: None,                                  // RESERVED — must not be asserted lossless
            os_version: Some("10.0.14393.1066".to_string()), // dropped — must not be asserted lossless
            os_features: Some(vec!["win32k".to_string()]),
        };
        let ocx = Platform::try_from(original.clone()).unwrap();
        let roundtripped: native::Platform = ocx.into();
        // os, architecture, variant, os_features must all survive.
        assert_eq!(roundtripped.os, original.os);
        assert_eq!(roundtripped.architecture, original.architecture);
        assert_eq!(roundtripped.variant, original.variant);
        // os_features is sorted+deduped; since win32k is already sorted+unique it should match.
        assert_eq!(roundtripped.os_features, original.os_features);
        // features must be None (RESERVED, not round-tripped).
        assert_eq!(roundtripped.features, None, "RESERVED features must not round-trip");
        // os_version must be None (dropped, not round-tripped).
        assert_eq!(roundtripped.os_version, None, "os_version must not round-trip");
    }

    // --- Native rejection ---

    #[test]
    fn native_rejects_unsupported_arch() {
        let native_plat = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::PowerPC64le,
            variant: None,
            features: None,
            os_version: None,
            os_features: None,
        };
        assert!(Platform::try_from(native_plat).is_err());
    }

    #[test]
    fn native_rejects_unsupported_os() {
        let native_plat = native::Platform {
            os: native::Os::FreeBSD,
            architecture: native::Arch::Amd64,
            variant: None,
            features: None,
            os_version: None,
            os_features: None,
        };
        assert!(Platform::try_from(native_plat).is_err());
    }

    #[test]
    fn native_rejects_other_os_and_arch() {
        let native_plat = native::Platform {
            os: native::Os::Other("custom-os".to_string()),
            architecture: native::Arch::Other("custom-arch".to_string()),
            variant: None,
            features: None,
            os_version: None,
            os_features: None,
        };
        assert!(Platform::try_from(native_plat).is_err());
    }

    #[test]
    fn native_rejects_any_with_variant() {
        let native_plat = native::Platform {
            os: native::Os::Other("any".to_string()),
            architecture: native::Arch::Other("any".to_string()),
            variant: Some("v7".to_string()),
            features: None,
            os_version: None,
            os_features: None,
        };
        assert!(Platform::try_from(native_plat).is_err());
    }

    /// `os.version` has no OCX platform-model concept, so its presence does
    /// not prevent `{"os":"any","architecture":"any"}` recognition — the
    /// same treatment the RESERVED `features` field already receives on
    /// this path.
    #[test]
    fn native_any_with_os_version_is_still_any() {
        let native_plat = native::Platform {
            os: native::Os::Other("any".to_string()),
            architecture: native::Arch::Other("any".to_string()),
            variant: None,
            features: None,
            os_version: Some("1.0".to_string()),
            os_features: None,
        };
        assert!(Platform::try_from(native_plat).unwrap().is_any());
    }

    #[test]
    fn native_option_none_is_any() {
        let platform = Platform::try_from(None::<native::Platform>).unwrap();
        assert!(platform.is_any());
    }
}
