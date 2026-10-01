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

/// The result of a platform parse or validation, which can fail no other way.
pub type Result<T> = std::result::Result<T, PlatformError>;

const ANY_STR: &str = "any";

/// Every `(os, arch)` pairing OCX accepts; the enums' cross product is not the legal set.
///
/// Wasm pairs are distribution labels only, reachable solely through an explicit `--platform`.
pub const SUPPORTED_PAIRS: &[(OperatingSystem, Architecture)] = &[
    (OperatingSystem::Linux, Architecture::Amd64),
    (OperatingSystem::Linux, Architecture::Arm64),
    (OperatingSystem::Darwin, Architecture::Amd64),
    (OperatingSystem::Darwin, Architecture::Arm64),
    (OperatingSystem::Windows, Architecture::Amd64),
    (OperatingSystem::Windows, Architecture::Arm64),
    (OperatingSystem::Wasip1, Architecture::Wasm),
    (OperatingSystem::Wasip2, Architecture::Wasm),
];

/// Rejects an `(os, arch)` pairing absent from [`SUPPORTED_PAIRS`].
///
/// Every site building a `Specific` from separate `os` and `arch` must call
/// this, or an unsupported pair enters the model.
fn validate_pair(os: OperatingSystem, arch: Architecture) -> std::result::Result<(), PlatformErrorKind> {
    if SUPPORTED_PAIRS.contains(&(os, arch)) {
        Ok(())
    } else {
        Err(PlatformErrorKind::UnsupportedPair { os, arch })
    }
}

/// Target platform for an OCX package: the OCI platform object over closed enums.
///
/// [`std::fmt::Display`]/[`std::str::FromStr`] are the one canonical, injective
/// grammar for `--platform`, `ocx.lock` and pin keys; paths use [`segments`](Self::segments):
/// ```text
/// os/arch[/variant][+feature[,feature...]]      |      any
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Platform-agnostic package, an OCX extension serialized as `{"os":"any","architecture":"any"}`.
    Any,

    Specific {
        os: OperatingSystem,
        arch: Architecture,
        /// CPU variant, e.g. `"v7"` for ARM ([OCI platform variants](https://github.com/opencontainers/image-spec/blob/main/image-index.md#platform-variants)).
        variant: Option<String>,
        /// Features the image requires of the host (e.g. `libc.glibc`); empty means none declared.
        os_features: Vec<String>,
    },
}

impl Platform {
    pub fn any() -> Self {
        Self::Any
    }

    pub fn is_any(&self) -> bool {
        matches!(self, Self::Any)
    }

    /// A bare image manifest carries no platform, so it is treated as platform-agnostic.
    pub fn from_image_manifest(_manifest: &native::ImageManifest) -> Self {
        Self::Any
    }

    /// The platform an image-index descriptor offers, or `None` for a non-candidate.
    ///
    /// A missing `platform` key must not answer `Any`: an `Any` offer matches
    /// everything, so one attestation entry would become a universal match.
    pub fn candidate_from_descriptor(entry: &native::ImageIndexEntry) -> Option<Self> {
        let declared = entry.platform.as_ref()?;
        Self::try_from(declared.clone()).ok()
    }

    /// The selectable platforms of an image index; non-candidates are skipped, never an error.
    pub fn from_image_index(manifest: &native::ImageIndex) -> Vec<Self> {
        manifest
            .manifests
            .iter()
            .filter_map(|entry| {
                let candidate = Self::candidate_from_descriptor(entry);
                if candidate.is_none() {
                    log::debug!("skipping non-candidate image-index descriptor {}", entry.digest);
                }
                candidate
            })
            .collect()
    }

    pub fn from_manifest(manifest: &native::Manifest) -> Vec<Self> {
        match manifest {
            native::Manifest::Image(image_manifest) => vec![Self::from_image_manifest(image_manifest)],
            native::Manifest::ImageIndex(image_index) => Self::from_image_index(image_index),
        }
    }

    /// The `os.features` tags; empty for `Any`.
    pub fn os_features(&self) -> &[String] {
        match self {
            Self::Any => &[],
            Self::Specific { os_features, .. } => os_features,
        }
    }

    /// Path segments: `["any"]` or `["linux", "arm64", "v8"]`.
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

    /// Compares only OS and architecture; `Any` equals only `Any`.
    ///
    /// Full equality would reject a host-runnable entry that merely carries a
    /// `variant` refinement [`current`](Self::current) never populates.
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

    /// Whether a package that resolved to `platform` runs on this host.
    ///
    /// `None`, `Any` and an undeterminable host all answer `true`: never suppress on an unknown.
    pub fn host_can_run(platform: Option<&Platform>) -> bool {
        Self::host_can_run_on(platform, Self::current().as_ref())
    }

    /// [`host_can_run`](Self::host_can_run) against an explicit host.
    pub fn host_can_run_on(platform: Option<&Platform>, host: Option<&Platform>) -> bool {
        match platform {
            None => true,
            Some(p) if p.is_any() => true,
            Some(p) => host.is_none_or(|h| h.same_os_arch(p)),
        }
    }

    pub fn ascii_segments(&self) -> Vec<String> {
        self.segments().into_iter().map(|s| s.to_ascii_lowercase()).collect()
    }

    /// The current host's platform; `None` only for an unsupported OS or architecture.
    ///
    /// An undetected libc yields empty `os_features`, which matches only offers declaring none.
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

    /// This platform with every `os.features` entry in `feature`'s namespace replaced by `feature`.
    ///
    /// Replace, never union: features are ANDed by [`is_compatible`], so
    /// `libc.glibc` plus `libc.musl` names a platform no host satisfies.
    pub fn with_os_feature(&self, feature: &str) -> Self {
        let Self::Specific {
            os,
            arch,
            variant,
            os_features,
        } = self
        else {
            return Self::Any;
        };
        let namespace = feature_namespace(feature);
        let mut os_features: Vec<String> = os_features
            .iter()
            // Checked on both sides, or two dotless tags collide on `None` and one evicts the other.
            .filter(|tag| namespace.is_none() || feature_namespace(tag) != namespace)
            .cloned()
            .collect();
        os_features.push(feature.to_string());
        Self::Specific {
            os: *os,
            arch: *arch,
            variant: variant.clone(),
            os_features,
        }
    }
}

/// The text before a tag's first `.`; `None` for a dotless tag, which is not
/// the namespace it spells, or a declared bare `libc` gets evicted.
fn feature_namespace(feature: &str) -> Option<&str> {
    feature.split_once('.').map(|(namespace, _leaf)| namespace)
}

impl Default for Platform {
    fn default() -> Self {
        Self::any()
    }
}

/// Whether `offered` satisfies `required`; directed, not symmetric.
///
/// An `Any` offer satisfies everything, an `Any` requirement only an `Any`
/// offer; `offered.os_features` ⊆ `required.os_features`, and a declared
/// `variant` must match. See `adr_platform_model_unification.md` D1 for the table.
pub fn is_compatible(required: &Platform, offered: &Platform) -> bool {
    // Before the `required`-is-`Any` arm, or an `Any` requirement refuses an `Any` offer.
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
        return false;
    };

    if required_os != offered_os || required_arch != offered_arch {
        return false;
    }

    if offered_variant.is_some() && offered_variant != required_variant {
        return false;
    }

    offered_os_features
        .iter()
        .all(|feature| required_os_features.contains(feature))
}

/// Lexicographic score `(is_specific, refinements, os_features)`, higher wins; only meaningful for an [`is_compatible`] offer.
///
/// Without the refinement axis a `variant`-exact offer ties an unconstrained
/// one and selection turns spuriously `Ambiguous`.
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

/// Outcome of [`select_best`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection<T> {
    Found(T),
    /// Candidates tied at the top score; the caller must disambiguate.
    Ambiguous(Vec<T>),
    None,
}

/// Selects the top-[`compatibility_score`] [`is_compatible`] candidate for `required`.
///
/// Fresh-resolve, lock-read and authoring pinning must all route through this,
/// or they answer differently for the same candidates.
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

        // A raw `+` is always the feature-list introducer; a value's own `+` is escaped to `%2B`.
        let (core, os_features) = match value.split_once('+') {
            Some((head, feature_list)) => (head, split_feature_list(feature_list).ok_or_else(invalid)?),
            None => (value, Vec::new()),
        };

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
        validate_pair(os, arch).map_err(|kind| PlatformError {
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
            os_features: normalize_os_features(&os_features),
        })
    }
}

/// Sort + dedup `os_features`; cascade eviction compares them as positional `Vec`s.
fn normalize_os_features(os_features: &[String]) -> Vec<String> {
    if os_features.len() <= 1 {
        return os_features.to_vec();
    }
    let mut features = os_features.to_vec();
    features.sort();
    features.dedup();
    features
}

// --- Canonical grammar escaping (`adr_platform_model_unification.md` D2) ---

/// Percent-escapes the grammar's structural characters `% / + ,`.
///
/// Every registry-controlled value entering the grammar must pass through
/// this, or it forges a separator and misparses.
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

/// Reverses [`escape_platform_component`]; `None` on any other `%` escape.
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

/// Splits and unescapes a feature list; `None` for an empty list or an empty token.
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
                // Unnormalized, cascade eviction's positional compare misses the prior entry and bloats the index.
                let normalized = normalize_os_features(os_features);
                native::Platform {
                    os: (*os).into(),
                    architecture: (*arch).into(),
                    variant: variant.clone(),
                    features: None,
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
    type Error = PlatformError;

    fn try_from(platform: native::Platform) -> Result<Self> {
        let input = platform.to_string();

        if let (native::Os::Other(os), native::Arch::Other(arch)) = (&platform.os, &platform.architecture) {
            if os == ANY_STR && arch == ANY_STR && platform.variant.is_none() {
                return Ok(Platform::Any);
            }
            return Err(PlatformError {
                input,
                kind: PlatformErrorKind::Unsupported(platform.to_string()),
            });
        }

        let os = OperatingSystem::try_from(platform.os).map_err(|kind| PlatformError {
            input: input.clone(),
            kind,
        })?;
        let arch = Architecture::try_from(platform.architecture).map_err(|kind| PlatformError {
            input: input.clone(),
            kind,
        })?;
        validate_pair(os, arch).map_err(|kind| PlatformError {
            input: input.clone(),
            kind,
        })?;

        // Warn and drop, never refuse, or manifests from stale tooling become unreadable.
        if platform.features.is_some() {
            tracing::warn!(
                "dropping RESERVED `features` field from platform '{}' (OCI v1.1.1 reserves it)",
                input
            );
        }

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
            // Unnormalized, duplicates inflate the selection score's feature count.
            os_features: normalize_os_features(&platform.os_features.unwrap_or_default()),
        })
    }
}

impl TryFrom<Option<native::Platform>> for Platform {
    type Error = PlatformError;

    fn try_from(platform: Option<native::Platform>) -> Result<Self> {
        match platform {
            Some(p) => Self::try_from(p),
            None => Ok(Self::default()),
        }
    }
}

/// Renders a raw [`native::Platform`] in the canonical grammar, total over unsupported platforms.
///
/// Display-only: no round-trip, and `features`/`os_version` are dropped.
pub fn render_native_platform(platform: &native::Platform) -> String {
    let mut rendered = format!("{}/{}", platform.os, platform.architecture);
    if let Some(variant) = platform.variant.as_deref().filter(|value| !value.is_empty()) {
        rendered.push('/');
        rendered.push_str(&escape_platform_component(variant));
    }
    let os_features = normalize_os_features(platform.os_features.as_deref().unwrap_or_default());
    if !os_features.is_empty() {
        rendered.push('+');
        for (index, feature) in os_features.iter().enumerate() {
            if index > 0 {
                rendered.push(',');
            }
            rendered.push_str(&escape_platform_component(feature));
        }
    }
    rendered
}

impl Serialize for Platform {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        native::Platform::from(self).serialize(serializer)
    }
}

// The schema mirrors what `Serialize` writes, the OCI object, not this enum's variants.
impl schemars::JsonSchema for Platform {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Platform".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "An OCI image-spec platform object. `Platform::Any` writes os and architecture as \"any\".",
            "properties": {
                "architecture": {"type": "string"},
                "os": {"type": "string"},
                "os.version": {"type": "string"},
                "os.features": {"type": "array", "items": {"type": "string"}},
                "variant": {"type": "string"},
            },
            "required": ["architecture", "os"],
        })
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

    // --- supported (os, arch) pairs ---

    #[test]
    fn from_str_rejects_unsupported_pairs() {
        // Both halves parse on their own; the pairing is what is refused.
        // `wasm` only ever pairs with a `wasip*` OS, and a `wasip*` OS only
        // ever pairs with `wasm`.
        for value in [
            "wasip1/amd64",
            "wasip2/arm64",
            "linux/wasm",
            "windows/wasm",
            "darwin/wasm",
        ] {
            let parsed = value.parse::<Platform>();
            assert!(parsed.is_err(), "'{value}' must not parse: {parsed:?}");
        }
    }

    #[test]
    fn from_str_rejects_unsupported_pair_with_the_pair_error_kind() {
        let error = "wasip1/amd64".parse::<Platform>().expect_err("pairing is refused");
        assert!(
            matches!(
                error.kind,
                PlatformErrorKind::UnsupportedPair {
                    os: OperatingSystem::Wasip1,
                    arch: Architecture::Amd64,
                }
            ),
            "expected UnsupportedPair, got {:?}",
            error.kind
        );
        // The message must name the way out, not just the refusal.
        let message = error.to_string();
        assert!(
            message.contains("wasip1/wasm"),
            "message should list supported pairs: {message}"
        );
    }

    #[test]
    fn from_str_accepts_wasm_pairs() {
        for value in ["wasip1/wasm", "wasip2/wasm"] {
            let parsed: Platform = value.parse().unwrap_or_else(|e| panic!("'{value}' must parse: {e}"));
            assert_eq!(parsed.to_string(), value, "canonical round-trip for {value}");
        }
    }

    #[test]
    fn from_str_accepts_every_supported_pair() {
        for (os, arch) in SUPPORTED_PAIRS {
            let value = format!("{os}/{arch}");
            let parsed: Platform = value.parse().unwrap_or_else(|e| panic!("'{value}' must parse: {e}"));
            assert_eq!(parsed.to_string(), value);
        }
    }

    #[test]
    fn supported_pairs_covers_the_variant_cross_product_exactly_once() {
        // Every pair must name a real variant combination, and no pair may
        // appear twice — the array is the single source of truth for the
        // validation, so a duplicate would silently pass unnoticed.
        let mut seen = std::collections::BTreeSet::new();
        for (os, arch) in SUPPORTED_PAIRS {
            assert!(OperatingSystem::VARIANTS.contains(os), "{os} missing from VARIANTS");
            assert!(Architecture::VARIANTS.contains(arch), "{arch} missing from VARIANTS");
            assert!(seen.insert((os, arch)), "duplicate pair {os}/{arch}");
        }
    }

    #[test]
    fn native_conversion_accepts_wasi_and_rejects_a_bad_pair() {
        // The second choke point: a platform arriving off the wire.
        let native_plat = native_platform(native::Os::Other("wasip1".to_string()), native::Arch::Wasm);
        assert_eq!(
            Platform::try_from(native_plat).expect("wasip1/wasm is supported"),
            Platform::Specific {
                os: OperatingSystem::Wasip1,
                arch: Architecture::Wasm,
                variant: None,
                os_features: Vec::new(),
            }
        );

        let bad_pair = native_platform(native::Os::Linux, native::Arch::Wasm);
        assert!(
            Platform::try_from(bad_pair).is_err(),
            "linux/wasm is not a supported pair"
        );
    }

    #[test]
    fn native_conversion_still_rejects_an_unknown_other_os() {
        let junk = native_platform(native::Os::Other("junk".to_string()), native::Arch::Amd64);
        assert!(Platform::try_from(junk).is_err());
    }

    #[test]
    fn candidate_from_descriptor_still_skips_an_unsupported_pair() {
        // The read path stays tolerant: a foreign index entry naming a pair
        // OCX does not support is not a candidate, never a hard error.
        let entry = descriptor(
            "sha256:aa",
            Some(native_platform(native::Os::Linux, native::Arch::Wasm)),
        );
        assert_eq!(Platform::candidate_from_descriptor(&entry), None);
    }

    #[test]
    fn wasm_is_incompatible_with_a_native_host() {
        let host: Platform = "linux/amd64".parse().expect("parses");
        let wasm: Platform = "wasip1/wasm".parse().expect("parses");
        assert!(
            !is_compatible(&host, &wasm),
            "a wasm offer must not satisfy a native host"
        );
        assert!(
            !is_compatible(&wasm, &host),
            "a native offer must not satisfy a wasm requirement"
        );
        assert!(is_compatible(&wasm, &wasm), "wasip1/wasm satisfies itself");
        assert!(
            !is_compatible(&wasm, &"wasip2/wasm".parse::<Platform>().expect("parses")),
            "the two WASI ABIs are distinct targets"
        );
        // An `any` offer still satisfies a wasm requirement (D1 unchanged).
        assert!(is_compatible(&wasm, &Platform::Any));
    }

    // --- with_os_feature: namespace replacement, round-tripping ---

    #[test]
    fn with_os_feature_renders_a_paste_ready_platform() {
        let declared: Platform = "linux/amd64".parse().expect("parses");
        let suggested = declared.with_os_feature("libc.glibc");
        assert_eq!(suggested.to_string(), "linux/amd64+libc.glibc");
        // Paste-ready means exactly this: the rendered value parses back to
        // the same platform, so a user can hand it to `--platform` verbatim.
        assert_eq!(
            suggested.to_string().parse::<Platform>().expect("round-trips"),
            suggested
        );
    }

    #[test]
    fn with_os_feature_replaces_the_namespace_rather_than_anding_it() {
        // Unioning would emit `linux/arm64/v8+libc.glibc,libc.musl`, which
        // subset matching ANDs — no single-libc host resolves it, so the
        // paste-ready value would be unresolvable while looking authoritative.
        let declared: Platform = "linux/arm64/v8+libc.musl".parse().expect("parses");
        assert_eq!(
            declared.with_os_feature("libc.glibc").to_string(),
            "linux/arm64/v8+libc.glibc",
            "the result must name one member of the namespace, never two ANDed"
        );
    }

    #[test]
    fn with_os_feature_preserves_other_namespaces() {
        // Only the incoming feature's own namespace is replaced; any other
        // feature the publisher declared is theirs and survives.
        let declared: Platform = "windows/amd64+win32k".parse().expect("parses");
        assert_eq!(
            declared.with_os_feature("libc.glibc").to_string(),
            "windows/amd64+libc.glibc,win32k"
        );
    }

    #[test]
    fn with_os_feature_replaces_every_member_of_the_namespace_not_a_known_list() {
        // The filter is the dotted namespace, not an enumeration of the
        // families OCX happens to model today: an unrecognised `libc.*` tag
        // is still a libc claim and must be replaced, or the result ANDs two
        // libc families and resolves nowhere.
        let declared: Platform = "linux/amd64+libc.musl,libc.uclibc,gpu.cuda".parse().expect("parses");
        assert_eq!(
            declared.with_os_feature("libc.glibc").to_string(),
            "linux/amd64+gpu.cuda,libc.glibc",
            "every `libc.*` entry goes, and only the `libc` namespace"
        );
    }

    #[test]
    fn with_os_feature_keeps_a_dotless_tag_that_spells_a_namespace() {
        // `libc` is a bare feature the publisher declared, not a member of the
        // `libc.*` family, so `libc.glibc` must not evict it. Treating a
        // dotless tag as its own namespace drops it — the one input on which
        // the namespace filter and the `libc.`-prefix filter it replaced can
        // disagree, and `declared_libcs` still uses the other one.
        let declared: Platform = "linux/amd64+libc".parse().expect("parses");
        assert_eq!(
            declared.with_os_feature("libc.glibc").to_string(),
            "linux/amd64+libc,libc.glibc",
            "a dotless tag has no namespace and cannot be evicted by a dotted feature"
        );
    }

    #[test]
    fn with_os_feature_on_any_is_unchanged() {
        // `any` carries no fields; total rather than panicking.
        assert_eq!(Platform::Any.with_os_feature("libc.glibc"), Platform::Any);
    }

    #[test]
    fn feature_namespace_splits_on_the_first_dot_only() {
        assert_eq!(feature_namespace("libc.glibc"), Some("libc"));
        assert_eq!(feature_namespace("a.b.c"), Some("a"));
        assert_eq!(feature_namespace("win32k"), None, "no dot: no namespace at all");
        assert_eq!(
            feature_namespace("libc"),
            None,
            "spelling a namespace is not being in one"
        );
        assert_eq!(feature_namespace(""), None);
    }

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

    // --- render_native_platform: canonical grammar for the raw fork type ---

    #[test]
    fn render_native_platform_appends_a_single_os_feature() {
        let platform = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::Amd64,
            os_version: None,
            os_features: Some(vec!["libc.glibc".to_string()]),
            variant: None,
            features: None,
        };
        assert_eq!(render_native_platform(&platform), "linux/amd64+libc.glibc");
    }

    #[test]
    fn render_native_platform_renders_a_platform_ocx_cannot_construct() {
        // freebsd/amd64 is real but unsupported by OCX's closed `Platform`
        // enum — `TryFrom<native::Platform>` refuses it — yet the cascade
        // fold deliberately keeps such platforms around rather than reading
        // "we failed to parse it" as "it has no platform"
        // (`cascade::graph::platform_slot`'s doc comment), so the plain
        // table must still render one.
        let platform = native::Platform {
            os: native::Os::FreeBSD,
            architecture: native::Arch::Amd64,
            os_version: None,
            os_features: None,
            variant: None,
            features: None,
        };
        assert_eq!(render_native_platform(&platform), "freebsd/amd64");
        assert!(
            Platform::try_from(platform).is_err(),
            "freebsd is unsupported by the typed Platform"
        );
    }

    #[test]
    fn render_native_platform_includes_a_non_empty_variant() {
        let platform = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::ARM64,
            os_version: None,
            os_features: None,
            variant: Some("v8".to_string()),
            features: None,
        };
        assert_eq!(render_native_platform(&platform), "linux/arm64/v8");
    }

    #[test]
    fn render_native_platform_skips_an_empty_variant() {
        let platform = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::Amd64,
            os_version: None,
            os_features: None,
            variant: Some(String::new()),
            features: None,
        };
        assert_eq!(
            render_native_platform(&platform),
            "linux/amd64",
            "an empty variant must not render a trailing slash with nothing after it"
        );
    }

    #[test]
    fn render_native_platform_normalizes_unsorted_duplicate_features() {
        let platform = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::Amd64,
            os_version: None,
            os_features: Some(vec!["b.y".to_string(), "a.x".to_string(), "a.x".to_string()]),
            variant: None,
            features: None,
        };
        assert_eq!(render_native_platform(&platform), "linux/amd64+a.x,b.y");
    }

    #[test]
    fn render_native_platform_has_no_suffix_for_empty_features() {
        let platform = native::Platform {
            os: native::Os::Windows,
            architecture: native::Arch::Amd64,
            os_version: None,
            os_features: Some(Vec::new()),
            variant: None,
            features: None,
        };
        assert_eq!(render_native_platform(&platform), "windows/amd64");
    }

    #[test]
    fn render_native_platform_agrees_with_the_typed_display_for_a_supported_platform() {
        let platform = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::ARM64,
            os_version: None,
            os_features: Some(vec!["libc.musl".to_string()]),
            variant: Some("v8".to_string()),
            features: None,
        };
        let typed = Platform::try_from(platform.clone()).expect("linux/arm64/v8 is supported");
        assert_eq!(
            render_native_platform(&platform),
            typed.to_string(),
            "the two grammars must not drift apart for a platform both can represent"
        );
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

    /// Build an image-index descriptor with an optional platform.
    fn descriptor(digest: &str, platform: Option<native::Platform>) -> native::ImageIndexEntry {
        native::ImageIndexEntry {
            media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
            digest: digest.to_string(),
            size: 100,
            platform,
            artifact_type: None,
            annotations: None,
        }
    }

    fn native_platform(os: native::Os, architecture: native::Arch) -> native::Platform {
        native::Platform {
            os,
            architecture,
            variant: None,
            features: None,
            os_version: None,
            os_features: None,
        }
    }

    fn image_index(manifests: Vec<native::ImageIndexEntry>) -> native::ImageIndex {
        native::ImageIndex {
            schema_version: 2,
            media_type: None,
            manifests,
            artifact_type: None,
            annotations: None,
        }
    }

    #[test]
    fn from_image_index_extracts_platforms() {
        let index = image_index(vec![
            descriptor(
                "sha256:aaa",
                Some(native_platform(native::Os::Linux, native::Arch::Amd64)),
            ),
            descriptor(
                "sha256:bbb",
                Some(native_platform(native::Os::Darwin, native::Arch::ARM64)),
            ),
        ]);
        let platforms = Platform::from_image_index(&index);
        assert_eq!(platforms.len(), 2);
        assert_eq!(platforms[0].to_string(), "linux/amd64");
        assert_eq!(platforms[1].to_string(), "darwin/arm64");
    }

    #[test]
    fn platform_from_image_index_does_not_abort_on_an_unsupported_descriptor() {
        // N-15: an attestation descriptor declares the placeholder
        // `unknown/unknown`, which `TryFrom<native::Platform>` refuses
        // (`ANY_STR` is "any", so `unknown` never reaches the `Any` arm).
        // Propagating that refusal aborted the WHOLE listing for the package —
        // one descriptor nobody asked to select made `ocx index list
        // --platforms` fail outright. It is skipped, not raised.
        let index = image_index(vec![
            descriptor(
                "sha256:aaa",
                Some(native_platform(native::Os::Linux, native::Arch::Amd64)),
            ),
            descriptor("sha256:att", Some(native_platform("unknown".into(), "unknown".into()))),
            descriptor(
                "sha256:bsd",
                Some(native_platform(native::Os::FreeBSD, native::Arch::Amd64)),
            ),
        ]);
        let platforms = Platform::from_image_index(&index);
        assert_eq!(
            platforms.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec!["linux/amd64".to_string()],
            "the real platform is listed; the unsupported descriptors are skipped, not raised"
        );
    }

    #[test]
    fn index_list_platforms_skips_attestation_and_platformless_descriptors() {
        // The exact enumeration `ocx index list --platforms` drives
        // (`command/index_list.rs` -> `Platform::from_manifest`). A descriptor
        // with NO `platform` key used to render as a platform row via
        // `TryFrom<Option<..>>`'s `Ok(Self::default())`; it is not a platform at
        // all, so it is not listed.
        let manifest = native::Manifest::ImageIndex(image_index(vec![
            descriptor(
                "sha256:aaa",
                Some(native_platform(native::Os::Linux, native::Arch::Amd64)),
            ),
            descriptor("sha256:att", Some(native_platform("unknown".into(), "unknown".into()))),
            descriptor("sha256:none", None),
        ]));
        let rows: Vec<String> = Platform::from_manifest(&manifest)
            .into_iter()
            .map(|platform| platform.to_string())
            .collect();
        assert_eq!(rows, vec!["linux/amd64".to_string()]);
        assert!(
            !rows.iter().any(|row| row == "any"),
            "a platform-less descriptor must not render as a platform row"
        );
    }

    #[test]
    fn candidate_from_descriptor_refuses_both_non_candidate_shapes() {
        assert!(
            Platform::candidate_from_descriptor(&descriptor("sha256:none", None)).is_none(),
            "no platform key -> not a candidate (an `Any` OFFER would satisfy every requirement)"
        );
        assert!(
            Platform::candidate_from_descriptor(&descriptor(
                "sha256:att",
                Some(native_platform("unknown".into(), "unknown".into()))
            ))
            .is_none(),
            "unknown/unknown -> not a candidate"
        );
        assert_eq!(
            Platform::candidate_from_descriptor(&descriptor(
                "sha256:aaa",
                Some(native_platform(native::Os::Linux, native::Arch::Amd64))
            ))
            .map(|platform| platform.to_string()),
            Some("linux/amd64".to_string())
        );
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
        // Iterates `SUPPORTED_PAIRS`, not the `VARIANTS` cross product: the
        // cross product now contains pairings deserialization rejects by
        // design (`linux/wasm`, `wasip1/amd64`).
        for (os, arch) in SUPPORTED_PAIRS {
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

    #[test]
    fn serde_rejects_an_unsupported_pair_from_registry() {
        let json = r#"{"architecture":"wasm","os":"linux"}"#;
        assert!(serde_json::from_str::<Platform>(json).is_err());
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
        // Same reason as `serde_roundtrip_all_supported_combinations`: the
        // `VARIANTS` cross product contains pairings `TryFrom` refuses.
        for (os, arch) in SUPPORTED_PAIRS {
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
