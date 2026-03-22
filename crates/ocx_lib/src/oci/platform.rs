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
}

/// Defaults to [`Platform::Any`] (platform-agnostic).
impl Default for Platform {
    fn default() -> Self {
        Self::any()
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
                    annotations: None,
                },
                native::ImageIndexEntry {
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                    digest: "sha256:bbb".to_string(),
                    size: 200,
                    platform: None,
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
