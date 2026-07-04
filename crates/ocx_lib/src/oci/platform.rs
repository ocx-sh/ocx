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
/// field names (`"os"`, `"architecture"`, `"variant"`, `"os.version"`,
/// `"os.features"`). Deserialization validates that the OS and architecture
/// are in OCX's supported set, rejecting unsupported values from registries
/// (e.g. `ppc64le`, `s390x`).
///
/// # String format
///
/// [`Display`] and [`FromStr`] use the human-readable slash-separated format
/// used in CLI `--platform` flags: `"linux/amd64"`, `"darwin/arm64/v8"`,
/// or `"any"` for platform-agnostic packages.
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
        /// Minimum OS version required, e.g. `"10.0.14393.1066"` for Windows.
        /// Implementations may refuse manifests where os_version is not known
        /// to work with the host OS version.
        os_version: Option<String>,
        /// Mandatory OS features required for this image. For Windows, the
        /// OCI spec defines `"win32k"` (image requires `win32k.sys` on the
        /// host, which is missing on Nano Server). For other operating
        /// systems, values are implementation-defined.
        os_features: Option<Vec<String>>,
        /// Required CPU features, e.g. `["sse4", "aes"]`. Defined by the
        /// [OCI Image Index specification](https://github.com/opencontainers/image-spec/blob/main/image-index.md#image-index-property-descriptions).
        features: Option<Vec<String>>,
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
            Self::Specific {
                os,
                arch,
                variant,
                os_version,
                ..
            } => {
                let mut segments = vec![os.to_string(), arch.to_string()];
                if let Some(variant) = variant {
                    segments.push(variant.clone());
                }
                if let Some(os_version) = os_version {
                    segments.push(os_version.clone());
                }
                segments
            }
        }
    }

    /// Checks if this platform is compatible with `other`.
    ///
    /// Currently this is strict equality. Future versions may implement
    /// richer matching: `Any` matching any `Specific`, os_version range
    /// comparisons, or os_features subset checks.
    pub fn matches(&self, other: &Platform) -> bool {
        self == other
    }

    /// Compares only the OS and architecture of two `Specific` platforms,
    /// ignoring `variant`, `os_version`, `os_features`, and `features`.
    ///
    /// [`current`](Self::current) can only ever populate `os`/`arch` (the
    /// remaining fields are always `None`), so a full-struct [`matches`](Self::matches)
    /// would wrongly reject a host-runnable image-index entry that merely carries
    /// an `os_version` or `os_features` refinement. The host-only symlink gate
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
    pub fn current() -> Option<Self> {
        let os = OperatingSystem::current()?;
        let arch = Architecture::current()?;
        Some(Self::Specific {
            os,
            arch,
            variant: None,
            os_version: None,
            os_features: None,
            features: None,
        })
    }

    /// Returns a lossless, injective key string for this platform, used as
    /// the `ocx.lock` V2 `platforms` map key and for host-platform lookup.
    ///
    /// The common case is byte-identical to [`Display`](std::fmt::Display)
    /// (`"any"`, `"linux/amd64"`, `"darwin/arm64/v8"`). Unlike `Display` —
    /// which drops `os_features` and `features` — this encoding extends the
    /// `Display` form with those fields **only when present**, so two
    /// advertised platforms that differ solely in `os_features`/`features`
    /// can never collide on the same key (Codex R1: a lossy key would
    /// convert a `select`-installable multi-variant package into a hard lock
    /// failure).
    ///
    /// # Injectivity
    ///
    /// The encoding is **structurally injective for arbitrary field values**,
    /// not just for the common platforms. Every registry-controlled string —
    /// `variant`, `os_version`, and each `os_features`/`features` value — is
    /// percent-escaped ([`escape_feature_component`]) before it enters the key.
    /// Escaping the base segments (not only the suffix values) is what closes
    /// the marker-forging hole: a crafted `os_version` such as `"10;osf=win32k"`
    /// encodes as `10%3Bosf%3Dwin32k`, so it cannot forge a `;osf=` marker and
    /// collide with a genuine `os_version = "10"` + `os_features = ["win32k"]`
    /// platform (Finding 2). The same base escaping covers `/`, the
    /// base-segment separator: a `variant = "v8/10"` (no `os_version`) encodes
    /// as `v8%2F10`, so it cannot forge the extra slash and collide with
    /// `variant = "v8", os_version = "10"` (Block B). The `,` separator inside a single feature value is
    /// likewise escaped, so `["a,b"]` (one element holding a comma) encodes as
    /// `a%2Cb` while `["a","b"]` (two elements) encodes as `a,b` — distinct
    /// keys. The `;osf=` / `;feat=` markers themselves are emitted only by this
    /// method (never from an escaped value), so the encoding also never collides
    /// with a marker-free base key.
    ///
    /// Escaping is **byte-transparent for the common case**: legitimate
    /// `variant` (`v7`, `v8`), `os_version` (`10.0.14393.1066`), and feature
    /// names (`win32k`, `sse4`, `aes`) contain none of `% , ; =`, so the
    /// typical `linux/amd64` / `darwin/arm64/v8` keys are byte-identical to
    /// [`Display`](std::fmt::Display).
    ///
    /// This is the only key encoding the lock uses; `Platform` is never
    /// serialized as a value, so no `impl JsonSchema for Platform`, no
    /// `Ord`, and no `canonical_set()` are required.
    pub fn lock_key(&self) -> String {
        let (os, arch, variant, os_version, os_features, features) = match self {
            // `Any` carries no registry-controlled fields → nothing to escape.
            Self::Any => return ANY_STR.to_string(),
            Self::Specific {
                os,
                arch,
                variant,
                os_version,
                os_features,
                features,
            } => (os, arch, variant, os_version, os_features, features),
        };

        // Base mirrors `Display` (`os/arch[/variant[/os_version]]`) but escapes
        // the registry-controlled `variant` and `os_version` segments so they
        // cannot forge a `;osf=`/`;feat=` marker. `os`/`arch` are closed enums
        // (no special characters) and pass through verbatim.
        let mut key = format!("{os}/{arch}");
        if let Some(variant) = variant {
            key.push('/');
            key.push_str(&escape_feature_component(variant));
        }
        if let Some(os_version) = os_version {
            key.push('/');
            key.push_str(&escape_feature_component(os_version));
        }

        // `os_features`/`features` are appended as explicit, parseable suffixes
        // only when present, so two children differing only in those fields get
        // distinct keys.
        if let Some(values) = os_features {
            key.push_str(";osf=");
            key.push_str(&join_feature_values(values));
        }
        if let Some(values) = features {
            key.push_str(";feat=");
            key.push_str(&join_feature_values(values));
        }
        key
    }

    /// Returns the default supported-platform list for the current host.
    ///
    /// The list always contains at most two entries:
    /// - [`Platform::current()`] — the host-native platform, when detectable.
    /// - [`Platform::any()`] — the platform-agnostic fallback, always present.
    ///
    /// This is the canonical source of truth for "which platforms should this
    /// host install?". Both the CLI install path and the self-update path
    /// consume this helper so the set stays in sync.
    pub fn supported_set() -> Vec<Self> {
        let mut platforms = Vec::new();
        if let Some(platform) = Self::current() {
            platforms.push(platform);
        }
        platforms.push(Self::any());
        platforms
    }

    /// Returns the full supported-platform matrix: every OS/architecture
    /// combination OCX ships and tests, regardless of which platform is
    /// running the command.
    ///
    /// Distinct from [`supported_set`](Self::supported_set), which is scoped
    /// to the *current host* (its own platform plus [`any()`](Self::any)).
    /// `all_supported` is the single source of truth for "every platform a
    /// team might run" — consumed by `ocx patch sync`'s no-`--platform`
    /// default, which must pin multi-platform manifests covering every
    /// platform a team runs (mirroring `ocx lock`), not just the host that
    /// happens to run the sync. A host-only default would silently miss
    /// non-host companions and break an offline or required-patch launch on
    /// a teammate's machine running a different platform.
    ///
    /// The five concrete OS/arch ship targets come first, kept in sync with
    /// `product-context.md` "Platform support": `linux/amd64`, `linux/arm64`,
    /// `darwin/amd64`, `darwin/arm64`, `windows/amd64`. [`Any`](Self::Any)
    /// trails the list as a platform-agnostic fallback, mirroring the
    /// suitable-first / `any`-last shape of [`supported_set`](Self::supported_set):
    /// [`select`](crate::oci::Index::select) iterates the list as a preference
    /// order and short-circuits on the first match, so the trailing `Any` is
    /// consulted only when no concrete platform matched. That lets an
    /// `any`-published companion (CA bundle, tool config) resolve during
    /// `ocx patch sync` while normal multi-platform companions still match
    /// their specific manifest first.
    pub fn all_supported() -> Vec<Self> {
        vec![
            Self::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Amd64,
                variant: None,
                os_version: None,
                os_features: None,
                features: None,
            },
            Self::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Arm64,
                variant: None,
                os_version: None,
                os_features: None,
                features: None,
            },
            Self::Specific {
                os: OperatingSystem::Darwin,
                arch: Architecture::Amd64,
                variant: None,
                os_version: None,
                os_features: None,
                features: None,
            },
            Self::Specific {
                os: OperatingSystem::Darwin,
                arch: Architecture::Arm64,
                variant: None,
                os_version: None,
                os_features: None,
                features: None,
            },
            Self::Specific {
                os: OperatingSystem::Windows,
                arch: Architecture::Amd64,
                variant: None,
                os_version: None,
                os_features: None,
                features: None,
            },
            // Trailing platform-agnostic fallback (mirrors `supported_set`): only
            // reached by `select` when no concrete platform above matched.
            Self::Any,
        ]
    }
}

/// Defaults to [`Platform::Any`] (platform-agnostic).
impl Default for Platform {
    fn default() -> Self {
        Self::any()
    }
}

/// Join a feature-value vector for [`Platform::lock_key`] with a `,` separator,
/// percent-escaping each value first so the join is injective.
///
/// Without per-component escaping the `,` join is ambiguous: `["a,b"]` and
/// `["a","b"]` both naively render `a,b`. Escaping the separator inside each
/// value (`a%2Cb` vs `a,b`) restores injectivity over arbitrary values.
fn join_feature_values(values: &[String]) -> String {
    values
        .iter()
        .map(|value| escape_feature_component(value))
        .collect::<Vec<_>>()
        .join(",")
}

/// Percent-escape every character that carries structural meaning anywhere in a
/// [`Platform::lock_key`] — both the slash-separated base segments and the
/// suffix feature values — so a registry-controlled string can never forge a
/// separator or marker.
///
/// Escapes `%` (the escape introducer — first, so escapes are unambiguous),
/// `/` (the base-segment separator), `,` (the suffix value separator), `;` (the
/// marker separator), and `=` (the marker delimiter). Escaping `/` is what keeps
/// the base injective: without it a `variant = "v8/10"` (no `os_version`) would
/// stringify to `…/v8/10` and collide with `variant = "v8", os_version = "10"`,
/// since the base joins segments with `/`. All other bytes pass through verbatim,
/// so the common ASCII values — `variant` (`v7`, `v8`), `os_version`
/// (`10.0.14393.1066`), feature names (`win32k`, `sse4`, `aes`) — are unchanged,
/// preserving the `lock_key`-equals-`Display` common case.
fn escape_feature_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '/' => out.push_str("%2F"),
            ',' => out.push_str("%2C"),
            ';' => out.push_str("%3B"),
            '=' => out.push_str("%3D"),
            other => out.push(other),
        }
    }
    out
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any => write!(f, "{}", ANY_STR),
            Self::Specific {
                os,
                arch,
                variant,
                os_version,
                ..
            } => {
                write!(f, "{}/{}", os, arch)?;
                if let Some(variant) = variant {
                    write!(f, "/{}", variant)?;
                }
                if let Some(os_version) = os_version {
                    write!(f, "/{}", os_version)?;
                }
                Ok(())
            }
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = PlatformError;

    fn from_str(value: &str) -> std::result::Result<Self, PlatformError> {
        if value == ANY_STR {
            return Ok(Self::Any);
        }

        let parts: Vec<&str> = value.split('/').collect();
        if parts.len() < 2 || parts.len() > 4 {
            return Err(PlatformError {
                input: value.to_string(),
                kind: PlatformErrorKind::InvalidFormat,
            });
        }

        let os_str = parts[0];
        let arch_str = parts[1];
        if parts.len() == 2 && os_str == ANY_STR && arch_str == ANY_STR {
            return Ok(Self::Any);
        }

        let os: OperatingSystem = os_str.parse().map_err(|kind| PlatformError {
            input: value.to_string(),
            kind,
        })?;

        let arch: Architecture = arch_str.parse().map_err(|kind| PlatformError {
            input: value.to_string(),
            kind,
        })?;

        let variant = if parts.len() > 2 {
            Some(parts[2].to_string())
        } else {
            None
        };

        let os_version = if parts.len() > 3 {
            Some(parts[3].to_string())
        } else {
            None
        };

        Ok(Self::Specific {
            os,
            arch,
            variant,
            os_version,
            os_features: None,
            features: None,
        })
    }
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
                os_version,
                os_features,
                features,
            } => native::Platform {
                os: (*os).into(),
                architecture: (*arch).into(),
                variant: variant.clone(),
                features: features.clone(),
                os_version: os_version.clone(),
                os_features: os_features.clone(),
            },
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
            if os == ANY_STR && arch == ANY_STR && platform.variant.is_none() && platform.os_version.is_none() {
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

        Ok(Self::Specific {
            os,
            arch,
            variant: platform.variant,
            os_version: platform.os_version,
            os_features: platform.os_features,
            features: platform.features,
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

    // --- Display / FromStr ---

    #[test]
    fn display_fromstr_roundtrip() {
        let cases = [
            "any",
            "linux/amd64",
            "darwin/arm64",
            "windows/amd64",
            "linux/amd64/v8",
            "linux/arm64/v8/10.0.14393",
        ];
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

    #[test]
    fn any_any_parses_as_any() {
        let platform: Platform = "any/any".parse().unwrap();
        assert!(platform.is_any());
    }

    // --- matches() ---

    #[test]
    fn matches_equal_is_true() {
        let a: Platform = "linux/amd64".parse().unwrap();
        let b: Platform = "linux/amd64".parse().unwrap();
        assert!(a.matches(&b));
    }

    #[test]
    fn matches_unequal_is_false() {
        let a: Platform = "linux/amd64".parse().unwrap();
        let b: Platform = "darwin/arm64".parse().unwrap();
        assert!(!a.matches(&b));
    }

    #[test]
    fn matches_any_equals_any() {
        assert!(Platform::Any.matches(&Platform::Any));
    }

    #[test]
    fn matches_any_does_not_match_specific() {
        let specific: Platform = "linux/amd64".parse().unwrap();
        assert!(!Platform::Any.matches(&specific));
        assert!(!specific.matches(&Platform::Any));
    }

    /// Returns `linux/amd64` with a non-`None` `os_version` — the shape a
    /// mirrored image-index entry carries but [`Platform::current`] can never
    /// produce.
    fn linux_amd64_with_os_version() -> Platform {
        let base: Platform = "linux/amd64".parse().unwrap();
        let Platform::Specific { os, arch, .. } = base else {
            panic!("linux/amd64 parses to Specific");
        };
        Platform::Specific {
            os,
            arch,
            variant: None,
            os_version: Some("10.0.14393".to_string()),
            os_features: None,
            features: None,
        }
    }

    #[test]
    fn same_os_arch_ignores_refinement_fields() {
        let plain: Platform = "linux/amd64".parse().unwrap();
        // os+arch equal, differ only in os_version → same_os_arch, unlike `matches`.
        assert!(plain.same_os_arch(&linux_amd64_with_os_version()));
        assert!(!plain.matches(&linux_amd64_with_os_version()));
        // Different arch / os → not same.
        assert!(!plain.same_os_arch(&"linux/arm64".parse().unwrap()));
        assert!(!plain.same_os_arch(&"windows/amd64".parse().unwrap()));
        // Any algebra.
        assert!(Platform::Any.same_os_arch(&Platform::Any));
        assert!(!Platform::Any.same_os_arch(&plain));
    }

    /// Regression (issue #179): the host-only gate compares os+arch only, so a
    /// host-runnable platform carrying an `os_version` is NOT suppressed.
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

        // Key case: matching os+arch with an os_version refinement still runs.
        assert!(Platform::host_can_run_on(Some(&linux_amd64_with_os_version()), host));

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

    #[test]
    fn segments_with_variant_and_os_version() {
        let platform: Platform = "linux/arm64/v8/10.0.14393".parse().unwrap();
        assert_eq!(platform.segments(), vec!["linux", "arm64", "v8", "10.0.14393"]);
    }

    // --- ascii_segments() ---

    #[test]
    fn ascii_segments_lowercases() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: Some("V8".to_string()),
            os_version: None,
            os_features: None,
            features: None,
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

    // --- all_supported() ---

    #[test]
    fn all_supported_returns_five_concrete_then_any_fallback() {
        let platforms = Platform::all_supported();
        let displayed: Vec<String> = platforms.iter().map(ToString::to_string).collect();
        assert_eq!(
            displayed,
            vec![
                "linux/amd64",
                "linux/arm64",
                "darwin/amd64",
                "darwin/arm64",
                "windows/amd64",
                "any"
            ],
            "all_supported must return the five concrete platforms in canonical order, then `any` last"
        );
    }

    #[test]
    fn all_supported_includes_any_as_trailing_fallback() {
        let platforms = Platform::all_supported();
        assert!(
            platforms.last().is_some_and(Platform::is_any),
            "all_supported must end with the platform-agnostic `any` fallback so an `any`-published \
             companion resolves when no concrete platform matches; got {:?}",
            platforms.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
    }

    /// `all_supported` is distinct from `supported_set` — the latter is
    /// scoped to the host (its own platform + `any`, at most 2 entries),
    /// while `all_supported` carries the full concrete matrix plus `any`.
    /// The distinction is concrete breadth, not `any`-presence (both now
    /// end with `any`).
    #[test]
    fn all_supported_is_distinct_from_supported_set() {
        assert_ne!(Platform::all_supported().len(), Platform::supported_set().len());
    }

    // --- lock_key (V2 map key encoding) ---

    /// The common case is byte-identical to `Display` — `lock_key` must not
    /// bloat the typical `linux/amd64` / `darwin/arm64/v8` / `any` keys.
    #[test]
    fn lock_key_common_case_equals_display() {
        for case in ["any", "linux/amd64", "darwin/arm64", "windows/amd64", "linux/arm64/v8"] {
            let platform: Platform = case.parse().unwrap();
            assert_eq!(
                platform.lock_key(),
                platform.to_string(),
                "lock_key must equal Display for the common case '{case}'"
            );
        }
    }

    /// Lossless + injective: two advertised children that differ ONLY in
    /// `os_features` must produce DISTINCT keys — a lossy key (plain Display)
    /// would collide them and convert a `select`-installable multi-variant
    /// package into a hard lock failure (Codex R1).
    #[test]
    fn lock_key_distinguishes_os_features() {
        let base = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: None,
            features: None,
        };
        let with_win32k = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: Some(vec!["win32k".to_string()]),
            features: None,
        };
        assert_ne!(
            base.lock_key(),
            with_win32k.lock_key(),
            "platforms differing only in os_features must get distinct lock keys"
        );
    }

    /// Lossless + injective: two advertised children that differ ONLY in CPU
    /// `features` must produce DISTINCT keys.
    #[test]
    fn lock_key_distinguishes_cpu_features() {
        let base = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: None,
            features: None,
        };
        let with_sse4 = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: None,
            features: Some(vec!["sse4".to_string()]),
        };
        assert_ne!(
            base.lock_key(),
            with_sse4.lock_key(),
            "platforms differing only in CPU features must get distinct lock keys"
        );
    }

    /// Injectivity for separator-bearing feature VALUES (Codex R1 / F4): a
    /// single value that itself contains the `,` separator must NOT alias a
    /// two-element vector. Without per-component escaping, `["a,b"]` and
    /// `["a","b"]` both naively render `;feat=a,b` and collide — which turns a
    /// VALID multi-variant manifest into a spurious `DuplicatePlatformKey` lock
    /// failure. The escaped encoding keeps them distinct.
    #[test]
    fn lock_key_injective_for_separator_bearing_feature_values() {
        let with_features = |features: Vec<String>| Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: None,
            features: Some(features),
        };
        let one_element_with_comma = with_features(vec!["a,b".to_string()]);
        let two_elements = with_features(vec!["a".to_string(), "b".to_string()]);
        assert_ne!(
            one_element_with_comma.lock_key(),
            two_elements.lock_key(),
            "`[\"a,b\"]` (one value containing a comma) must not collide with `[\"a\",\"b\"]` (two values)"
        );

        // Same hazard via the os_features vector and via a value that contains
        // the literal marker text `;feat=` (which must be escaped, not forge a
        // second marker).
        let osf_one = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: Some(vec!["x,y".to_string()]),
            features: None,
        };
        let osf_two = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: Some(vec!["x".to_string(), "y".to_string()]),
            features: None,
        };
        assert_ne!(
            osf_one.lock_key(),
            osf_two.lock_key(),
            "separator-bearing os_features values must not collide with a multi-element vector"
        );

        let forged_marker = with_features(vec![";feat=win32k".to_string()]);
        let plain = with_features(vec!["win32k".to_string()]);
        assert_ne!(
            forged_marker.lock_key(),
            plain.lock_key(),
            "a feature value containing the literal `;feat=` marker text must not forge a collision"
        );
    }

    /// Injectivity for marker-bearing `os_version` / `variant` VALUES (Codex
    /// R1 backstop — Finding 2): the registry-controlled `variant` and
    /// `os_version` strings enter the key base via `Display`. Without escaping
    /// them, a crafted `os_version` such as `"10;osf=win32k"` forges a `;osf=`
    /// marker, so a platform with that bare `os_version` (and no real
    /// `os_features`) collides on one key with a DIFFERENT platform that has a
    /// plain `os_version` plus a genuine `os_features = ["win32k"]`. Two
    /// distinct advertised platforms must NOT share a `lock_key`.
    #[test]
    fn lock_key_injective_for_marker_bearing_os_version() {
        // Platform A: os_version forges a `;osf=win32k` marker, no real os_features.
        let forged = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10;osf=win32k".to_string()),
            os_features: None,
            features: None,
        };
        // Platform B: plain os_version `10`, genuine os_features `["win32k"]`.
        let genuine = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10".to_string()),
            os_features: Some(vec!["win32k".to_string()]),
            features: None,
        };
        assert_ne!(
            forged.lock_key(),
            genuine.lock_key(),
            "a marker-forging os_version must not collide with a genuine os_features platform"
        );
    }

    /// Same hazard via the registry-controlled `variant` string: a crafted
    /// `variant = "v8;feat=sse4"` must not forge a `;feat=` marker that
    /// collides with a genuine `features = ["sse4"]` platform.
    #[test]
    fn lock_key_injective_for_marker_bearing_variant() {
        let forged = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: Some("v8;feat=sse4".to_string()),
            os_version: None,
            os_features: None,
            features: None,
        };
        let genuine = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: Some("v8".to_string()),
            os_version: None,
            os_features: None,
            features: Some(vec!["sse4".to_string()]),
        };
        assert_ne!(
            forged.lock_key(),
            genuine.lock_key(),
            "a marker-forging variant must not collide with a genuine features platform"
        );
    }

    /// Injectivity for slash-bearing `variant` VALUES (Block B): the base is
    /// joined with `/`, so a `variant = "v8/10"` (no `os_version`) would
    /// stringify to `…/v8/10` — byte-identical to `variant = "v8",
    /// os_version = "10"` — unless the base segments escape `/`. Two distinct
    /// advertised platforms must NOT share a `lock_key`, or a valid
    /// multi-variant manifest becomes a spurious `DuplicatePlatformKey` lock
    /// failure.
    #[test]
    fn lock_key_injective_for_slash_bearing_variant() {
        // Platform A: variant carries an embedded slash, no os_version.
        let forged = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: Some("v8/10".to_string()),
            os_version: None,
            os_features: None,
            features: None,
        };
        // Platform B: plain variant `v8`, separate os_version `10`.
        let genuine = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: Some("v8".to_string()),
            os_version: Some("10".to_string()),
            os_features: None,
            features: None,
        };
        assert_ne!(
            forged.lock_key(),
            genuine.lock_key(),
            "a slash-bearing variant must not forge an os_version segment and collide"
        );
    }

    /// Same hazard via the registry-controlled `os_version` string: a crafted
    /// `os_version` containing a `/` must not forge an extra base segment. A
    /// platform with `os_version = "10/x"` (no `os_features`) must stay distinct
    /// from any other platform whose base segments would otherwise align.
    #[test]
    fn lock_key_injective_for_slash_bearing_os_version() {
        // Platform A: os_version itself contains a slash.
        let forged = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: Some("v3".to_string()),
            os_version: Some("10/x".to_string()),
            os_features: None,
            features: None,
        };
        // Platform B: variant `v3/10`, os_version `x` — the naive (unescaped)
        // base join of both is `windows/amd64/v3/10/x`, so they would collide
        // without `/` escaping.
        let genuine = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: Some("v3/10".to_string()),
            os_version: Some("x".to_string()),
            os_features: None,
            features: None,
        };
        assert_ne!(
            forged.lock_key(),
            genuine.lock_key(),
            "a slash-bearing os_version must not forge an extra base segment and collide"
        );
    }

    /// The escape is confined to feature values: a value with no structural
    /// characters round-trips verbatim, so common feature names are unchanged
    /// and the key stays human-readable.
    #[test]
    fn lock_key_leaves_plain_feature_values_unescaped() {
        let plain = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: Some(vec!["win32k".to_string()]),
            features: Some(vec!["sse4".to_string(), "aes".to_string()]),
        };
        assert_eq!(
            plain.lock_key(),
            "windows/amd64;osf=win32k;feat=sse4,aes",
            "plain ASCII feature names must pass through unescaped"
        );
    }

    /// Injectivity backstop: a set of platforms that are pairwise distinct
    /// (including the features-only differences) must yield pairwise-distinct
    /// keys — the structural no-collision guarantee the dup-key guard relies
    /// on.
    #[test]
    fn lock_key_is_injective_over_distinct_platforms() {
        let win = |os_features: Option<Vec<String>>, features: Option<Vec<String>>| Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features,
            features,
        };
        let platforms = vec![
            "linux/amd64".parse::<Platform>().unwrap(),
            "linux/arm64".parse::<Platform>().unwrap(),
            "darwin/arm64".parse::<Platform>().unwrap(),
            Platform::Any,
            win(None, None),
            win(Some(vec!["win32k".to_string()]), None),
            win(None, Some(vec!["sse4".to_string()])),
        ];
        let mut keys = std::collections::HashSet::new();
        for platform in &platforms {
            assert!(
                keys.insert(platform.lock_key()),
                "lock_key collision for {platform:?}; keys so far: {keys:?}"
            );
        }
        assert_eq!(
            keys.len(),
            platforms.len(),
            "every distinct platform must get a distinct key"
        );
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
    fn serde_os_version_field_name() {
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393".to_string()),
            os_features: None,
            features: None,
        };
        let json = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["os.version"], "10.0.14393");
        let parsed: Platform = serde_json::from_str(&json).unwrap();
        assert_eq!(platform, parsed);
    }

    #[test]
    fn serde_features_accepted_and_roundtripped() {
        let json = r#"{"os":"linux","architecture":"amd64","features":["sse4","aes"]}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        match &platform {
            Platform::Specific { features, .. } => {
                assert_eq!(features.as_deref(), Some(&["sse4".to_string(), "aes".to_string()][..]));
            }
            _ => panic!("expected Specific"),
        }
        let json_out = serde_json::to_string(&platform).unwrap();
        let reparsed: Platform = serde_json::from_str(&json_out).unwrap();
        assert_eq!(platform, reparsed);
    }

    #[test]
    fn serde_os_features_accepted_and_roundtripped() {
        let json = r#"{"os":"windows","architecture":"amd64","os.features":["win32k"]}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(os_features.as_deref(), Some(&["win32k".to_string()][..]));
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
                    os_version: None,
                    os_features: None,
                    features: None,
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
            os_version: None,
            os_features: None,
            features: None,
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
                os_version: None,
                os_features: None,
                features: None,
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
                os_version: None,
                os_features: None,
                features: None,
            }
        );
    }

    #[test]
    fn serde_deserialize_platform_with_os_version() {
        let json = r#"{"architecture":"amd64","os":"windows","os.version":"10.0.14393.1066"}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Windows,
                arch: Architecture::Amd64,
                variant: None,
                os_version: Some("10.0.14393.1066".to_string()),
                os_features: None,
                features: None,
            }
        );
    }

    #[test]
    fn serde_deserialize_platform_with_all_optional_fields() {
        let json = r#"{
            "architecture":"amd64",
            "os":"windows",
            "variant":"v3",
            "os.version":"10.0.14393.1066",
            "os.features":["win32k"],
            "features":["sse4"]
        }"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Windows,
                arch: Architecture::Amd64,
                variant: Some("v3".to_string()),
                os_version: Some("10.0.14393.1066".to_string()),
                os_features: Some(vec!["win32k".to_string()]),
                features: Some(vec!["sse4".to_string()]),
            }
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
            os_version: None,
            os_features: None,
            features: None,
        };
        let native_plat: native::Platform = platform.clone().into();
        assert_eq!(native_plat.variant, Some("v8".to_string()));
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_with_os_version() {
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393.1066".to_string()),
            os_features: None,
            features: None,
        };
        let native_plat: native::Platform = platform.clone().into();
        assert_eq!(native_plat.os_version, Some("10.0.14393.1066".to_string()));
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_with_os_features() {
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393.1066".to_string()),
            os_features: Some(vec!["win32k".to_string()]),
            features: None,
        };
        let native_plat: native::Platform = platform.clone().into();
        assert_eq!(native_plat.os_features, Some(vec!["win32k".to_string()]));
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_with_features() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: None,
            features: Some(vec!["sse4".to_string(), "aes".to_string()]),
        };
        let native_plat: native::Platform = platform.clone().into();
        assert_eq!(native_plat.features, Some(vec!["sse4".to_string(), "aes".to_string()]));
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_with_all_optional_fields() {
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: Some("v3".to_string()),
            os_version: Some("10.0.14393.1066".to_string()),
            os_features: Some(vec!["win32k".to_string()]),
            features: Some(vec!["sse4".to_string()]),
        };
        let native_plat: native::Platform = platform.clone().into();
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    /// Any valid OCI platform with a supported OS/arch must survive a
    /// lossless roundtrip: native → Platform → native == original.
    #[test]
    fn native_lossless_roundtrip_full_fidelity() {
        let original = native::Platform {
            os: native::Os::Windows,
            architecture: native::Arch::Amd64,
            variant: Some("v3".to_string()),
            features: Some(vec!["sse4".to_string(), "aes".to_string()]),
            os_version: Some("10.0.14393.1066".to_string()),
            os_features: Some(vec!["win32k".to_string()]),
        };
        let ocx = Platform::try_from(original.clone()).unwrap();
        let roundtripped: native::Platform = ocx.into();
        assert_eq!(original, roundtripped);
    }

    #[test]
    fn native_roundtrip_all_supported_combinations() {
        for os in OperatingSystem::VARIANTS {
            for arch in Architecture::VARIANTS {
                let platform = Platform::Specific {
                    os: *os,
                    arch: *arch,
                    variant: None,
                    os_version: None,
                    os_features: None,
                    features: None,
                };
                let native_plat: native::Platform = platform.clone().into();
                let back = Platform::try_from(native_plat).unwrap();
                assert_eq!(platform, back, "native roundtrip failed for {}/{}", os, arch);
            }
        }
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

    #[test]
    fn native_option_none_is_any() {
        let platform = Platform::try_from(None::<native::Platform>).unwrap();
        assert!(platform.is_any());
    }
}
