// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::prelude::*;

type PatchRest = (Option<String>, Option<String>);
type MinorRest = Option<(u32, PatchRest)>;

#[derive(Clone, Hash, PartialEq, Eq)]
pub struct Version {
    /// Optional variant prefix (e.g., "debug", "pgo.lto"). None = default variant.
    variant: Option<String>,
    /// major version, always present, consequently there is no representation of a 'latest' version
    major: u32,
    /// tuple of (minor, tuple of (patch, (build, prerelease))), ensuring that minor is only present if patch is present, and patch is only present if prerelease is present
    rest: Option<(u32, MinorRest)>,
}

/// Close to semver version, but with rolling parent versions and no build info.
/// Prereleases are not supported for rolling versions, ie. '1-alpha' is not a valid version.
///
/// Optionally carries a variant prefix (e.g., `debug-3.12.5`). Variants define
/// how a binary was built (optimization profile, feature set) and are orthogonal
/// to platform (which defines where it runs). See `adr_variants.md`.
impl Version {
    pub fn new_major(major: u32) -> Self {
        Self {
            variant: None,
            major,
            rest: None,
        }
    }

    pub fn new_minor(major: u32, minor: u32) -> Self {
        Self {
            variant: None,
            major,
            rest: Some((minor, None)),
        }
    }

    pub fn new_patch(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            variant: None,
            major,
            rest: Some((minor, Some((patch, (None, None))))),
        }
    }

    pub fn new_build(major: u32, minor: u32, patch: u32, build: impl Into<String>) -> Self {
        Self {
            variant: None,
            major,
            rest: Some((minor, Some((patch, (Some(build.into()), None))))),
        }
    }

    pub fn new_prerelease(major: u32, minor: u32, patch: u32, prerelease: impl Into<String>) -> Self {
        Self {
            variant: None,
            major,
            rest: Some((minor, Some((patch, (None, Some(prerelease.into())))))),
        }
    }

    pub fn new_prerelease_with_build(
        major: u32,
        minor: u32,
        patch: u32,
        prerelease: impl Into<String>,
        build: impl Into<String>,
    ) -> Self {
        Self {
            variant: None,
            major,
            rest: Some((minor, Some((patch, (Some(build.into()), Some(prerelease.into())))))),
        }
    }

    /// Returns the parent version, or None if this version is a major version with no minor version.
    /// The variant is preserved through the parent chain.
    pub fn parent(&self) -> Option<Self> {
        if let Some((minor, patch)) = &self.rest {
            if let Some((patch, (build, prerelease))) = patch {
                if build.is_some() {
                    Some(Self {
                        variant: self.variant.clone(),
                        major: self.major,
                        rest: Some((*minor, Some((*patch, (None, prerelease.clone()))))),
                    })
                } else if prerelease.is_some() {
                    Some(Self {
                        variant: self.variant.clone(),
                        major: self.major,
                        rest: Some((*minor, Some((*patch, (None, None))))),
                    })
                } else {
                    Some(Self {
                        variant: self.variant.clone(),
                        major: self.major,
                        rest: Some((*minor, None)),
                    })
                }
            } else {
                Some(Self {
                    variant: self.variant.clone(),
                    major: self.major,
                    rest: None,
                })
            }
        } else {
            None
        }
    }

    pub fn major(&self) -> u32 {
        self.major
    }

    pub fn minor(&self) -> Option<u32> {
        if let Some((minor, _)) = self.rest {
            Some(minor)
        } else {
            None
        }
    }

    pub fn has_minor(&self) -> bool {
        matches!(self.rest, Some((_, _)))
    }

    pub fn patch(&self) -> Option<u32> {
        if let Some((_, Some((patch, _)))) = self.rest {
            Some(patch)
        } else {
            None
        }
    }

    pub fn has_patch(&self) -> bool {
        matches!(self.rest, Some((_, Some((_, _)))))
    }

    pub fn build(&self) -> Option<String> {
        if let Some((_, Some((_, (Some(build), _))))) = &self.rest {
            Some(build.clone())
        } else {
            None
        }
    }

    pub fn has_build(&self) -> bool {
        matches!(self.rest, Some((_, Some((_, (Some(_), _))))))
    }

    pub fn prerelease(&self) -> Option<String> {
        if let Some((_, Some((_, (_, Some(prerelease)))))) = &self.rest {
            Some(prerelease.clone())
        } else {
            None
        }
    }

    pub fn has_prerelease(&self) -> bool {
        matches!(self.rest, Some((_, Some((_, (_, Some(_)))))))
    }

    pub fn is_rolling(&self) -> bool {
        !matches!(&self.rest, Some((_, Some((_, (Some(_), _))))))
    }

    /// Returns the variant name, if any (e.g., `Some("debug")`, `Some("pgo.lto")`).
    pub fn variant(&self) -> Option<&str> {
        self.variant.as_deref()
    }

    /// Returns true if this version has a variant prefix.
    pub fn has_variant(&self) -> bool {
        self.variant.is_some()
    }

    /// Returns a copy of this version with the given variant prefix.
    pub fn with_variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = Some(variant.into());
        self
    }

    /// Returns a copy of this version with the variant stripped.
    pub fn without_variant(&self) -> Version {
        Version {
            variant: None,
            ..self.clone()
        }
    }

    /// Parses a version string, optionally with a variant prefix.
    ///
    /// Variant format: `<variant>-<version>` where variant matches `[a-z][a-z0-9.]*`
    /// and the boundary is the first `-` followed by a digit.
    ///
    /// Examples:
    /// - `"3.12.5"` → Version { variant: None, major: 3, ... }
    /// - `"debug-3.12.5"` → Version { variant: Some("debug"), major: 3, ... }
    /// - `"pgo.lto-3.12.5_b1"` → Version { variant: Some("pgo.lto"), major: 3, build: "b1" }
    /// - `"debug"` → None (bare variant name, no version — falls into Tag::Other)
    pub fn parse(value: &str) -> Option<Self> {
        use regex::Regex;
        use std::sync::LazyLock;

        static VERSION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"^(([a-z][a-z0-9.]*)-)?(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*)(-([0-9a-zA-Z]+))?([_+]([0-9a-zA-Z]+))?)?)?$",
            )
            .expect("Invalid version regex!")
        });

        let captures = VERSION_REGEX.captures(value)?;

        let variant = match captures.get(2).map(|m| m.as_str()) {
            Some("latest") => return None, // "latest" is reserved and not a valid variant
            Some(variant) => Some(variant.to_string()),
            None => None,
        };

        let major = captures
            .get(3)
            .expect("group 3 always captures")
            .as_str()
            .parse::<u32>()
            .ok()?;
        let minor = match captures.get(5).map(|m| m.as_str()) {
            Some("") | None => {
                return Some(Version {
                    variant,
                    major,
                    rest: None,
                });
            }
            Some(minor) => match minor.parse::<u32>().ok() {
                None => {
                    return Some(Version {
                        variant,
                        major,
                        rest: None,
                    });
                }
                Some(minor) => minor,
            },
        };
        let patch = match captures.get(7).map(|m| m.as_str()) {
            Some("") | None => {
                return Some(Version {
                    variant,
                    major,
                    rest: Some((minor, None)),
                });
            }
            Some(patch) => match patch.parse::<u32>().ok() {
                None => {
                    return Some(Version {
                        variant,
                        major,
                        rest: Some((minor, None)),
                    });
                }
                Some(patch) => patch,
            },
        };
        let prerelease = captures.get(9).map(|x| x.as_str().to_string());
        let build = captures.get(11).map(|x| x.as_str().to_string());
        Some(Version {
            variant,
            major,
            rest: Some((minor, Some((patch, (build, prerelease))))),
        })
    }
}

impl Ord for Version {
    fn cmp(&self, rhs: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        let lhs = self;

        // Variant sorts: Some("debug") < Some("pgo.lto") < None
        // None (default) sorts last so it appears first in reverse-sorted listings.
        match (&lhs.variant, &rhs.variant) {
            (None, None) => {}
            (None, Some(_)) => return Ordering::Greater,
            (Some(_), None) => return Ordering::Less,
            (Some(a), Some(b)) => match a.cmp(b) {
                Ordering::Equal => {}
                ordering => return ordering,
            },
        }

        // major
        match lhs.major.cmp(&rhs.major) {
            Ordering::Equal => {}
            ordering => return ordering,
        };

        // minor
        let (lhs_minor, lhs_rest) = match lhs.rest.as_ref() {
            Some(minor) => minor,
            None => {
                return if rhs.rest.is_some() {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                };
            }
        };
        let (rhs_minor, rhs_rest) = match rhs.rest.as_ref() {
            Some(minor) => minor,
            None => return Ordering::Less,
        };
        match lhs_minor.cmp(rhs_minor) {
            Ordering::Equal => {}
            ordering => return ordering,
        };

        // patch
        let (lhs_patch, lhs_rest) = match lhs_rest {
            Some(patch) => patch,
            None => {
                return if rhs.patch().is_some() {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                };
            }
        };
        let (rhs_patch, rhs_rest) = match rhs_rest {
            Some(patch) => patch,
            None => return Ordering::Less,
        };
        match lhs_patch.cmp(rhs_patch) {
            Ordering::Equal => {}
            ordering => return ordering,
        };

        // prerelease & build
        let (lhs_build, lhs_prerelease) = lhs_rest;
        let (rhs_build, rhs_prerelease) = rhs_rest;

        match (&lhs_prerelease, &rhs_prerelease) {
            (Some(lhs_prerelease), Some(rhs_prerelease)) => match lhs_prerelease.cmp(rhs_prerelease) {
                Ordering::Equal => {}
                ordering => return ordering,
            },
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (None, None) => {}
        };
        match (&lhs_build, &rhs_build) {
            (Some(lhs_build), Some(rhs_build)) => lhs_build.cmp(rhs_build),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, rhs: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(rhs))
    }
}

impl TryFrom<&str> for Version {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self> {
        match Self::parse(value) {
            Some(version) => Ok(version),
            None => Err(super::error::Error::VersionInvalid(value.into()).into()),
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(variant) = &self.variant {
            write!(f, "{}-", variant)?;
        }

        let mut version = self.major.to_string();

        if let Some((minor, rest)) = self.rest.as_ref() {
            version.push_str(&format!(".{}", minor));
            if let Some((patch, rest)) = rest {
                version.push_str(&format!(".{}", patch));
                if let (_, Some(prerelease)) = rest {
                    version.push_str(&format!("-{}", prerelease));
                }
                if let (Some(build), _) = rest {
                    version.push_str(&format!("_{}", build));
                }
            }
        }

        write!(f, "{}", version)
    }
}

impl std::fmt::Debug for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Version({})", self)
    }
}

impl From<Version> for String {
    fn from(val: Version) -> Self {
        val.to_string()
    }
}

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_string().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::try_from(s.as_str()).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_parsing() {
        let expected_version = Version::new_prerelease(1, 2, 3, "alpha".to_string());
        assert_eq!("1.2.3-alpha", expected_version.to_string());
        assert_eq!(Version::parse("1.2.3-alpha").unwrap(), expected_version);
        assert_eq!(
            Version::parse(expected_version.to_string().as_ref()).unwrap(),
            expected_version
        );
        let expected_version = expected_version.parent().expect("Expected parent version");
        assert_eq!("1.2.3", expected_version.to_string());
        assert_eq!(Version::parse("1.2.3").unwrap(), expected_version);
        assert_eq!(
            Version::parse(expected_version.to_string().as_ref()).unwrap(),
            expected_version
        );
        let expected_version = expected_version.parent().expect("Expected parent version");
        assert_eq!("1.2", expected_version.to_string());
        assert_eq!(Version::parse("1.2").unwrap(), expected_version);
        assert_eq!(
            Version::parse(expected_version.to_string().as_ref()).unwrap(),
            expected_version
        );
        let expected_version = expected_version.parent().expect("Expected parent version");
        assert_eq!("1", expected_version.to_string());
        assert_eq!(Version::parse("1").unwrap(), expected_version);
        assert_eq!(
            Version::parse(expected_version.to_string().as_ref()).unwrap(),
            expected_version
        );
        assert!(expected_version.parent().is_none());
    }

    #[test]
    fn test_version_getters() {
        let version = Version::new_prerelease(1, 2, 3, "alpha".to_string());
        assert_eq!(version.major(), 1);
        assert_eq!(version.minor(), Some(2));
        assert_eq!(version.patch(), Some(3));
        assert_eq!(version.prerelease(), Some("alpha".to_string()));

        let version = version.parent().expect("Expected parent version");
        assert_eq!(version.prerelease(), None);
        let version = version.parent().expect("Expected parent version");
        assert_eq!(version.prerelease(), None);
        assert_eq!(version.patch(), None);
        let version = version.parent().expect("Expected parent version");
        assert_eq!(version.prerelease(), None);
        assert_eq!(version.patch(), None);
        assert_eq!(version.minor(), None);
    }

    #[test]
    fn test_version_is_rolling() {
        let version = Version::parse("1").unwrap();
        assert!(version.is_rolling());
        let version = Version::parse("1.2").unwrap();
        assert!(version.is_rolling());
        let version = Version::parse("1.2.3").unwrap();
        assert!(version.is_rolling());
        let version = Version::parse("1.2.3-alpha").unwrap();
        assert!(version.is_rolling());
        let version = Version::parse("1.2.3_20260216").unwrap();
        assert!(!version.is_rolling());
        let version = Version::parse("1.2.3+20260216").unwrap();
        assert!(!version.is_rolling());
        assert_eq!(version.to_string(), "1.2.3_20260216");
        let version = Version::parse("1.2.3-alpha+20260216").unwrap();
        assert!(!version.is_rolling());
        assert_eq!(version.to_string(), "1.2.3-alpha_20260216");
        let version = Version::parse("1.2.3-alpha_20260216").unwrap();
        assert!(!version.is_rolling());
    }

    #[test]
    fn test_version_ordering() {
        let version_1 = Version::parse("1").unwrap();
        let version_1_2 = Version::parse("1.2").unwrap();
        let version_1_2_3 = Version::parse("1.2.3").unwrap();
        let version_1_2_3_alpha = Version::parse("1.2.3-alpha").unwrap();

        assert!(version_1_2_3_alpha < version_1_2_3);
        assert!(version_1_2_3 > version_1_2_3_alpha);
        assert!(version_1_2_3 < version_1_2);
        assert!(version_1_2 > version_1_2_3);
        assert!(version_1_2 < version_1);
        assert!(version_1 > version_1_2);

        let version_2 = Version::new_major(2);
        for version in &[version_1, version_1_2, version_1_2_3, version_1_2_3_alpha] {
            assert!(version < &version_2);
            assert!(&version_2 > version);
        }
    }

    #[test]
    fn test_has_fragment() {
        let version = Version::new_prerelease_with_build(1, 2, 3, "alpha", "build");
        assert!(version.has_minor());
        assert!(version.has_patch());
        assert!(version.has_prerelease());
        assert!(version.has_build());

        let version = Version::new_build(1, 2, 3, "build");
        assert!(version.has_minor());
        assert!(version.has_patch());
        assert!(!version.has_prerelease());
        assert!(version.has_build());

        let version = Version::new_prerelease(1, 2, 3, "alpha".to_string());
        assert!(version.has_minor());
        assert!(version.has_patch());
        assert!(version.has_prerelease());
        assert!(!version.has_build());

        let version = Version::new_minor(1, 2);
        assert!(version.has_minor());
        assert!(!version.has_patch());
        assert!(!version.has_prerelease());
        assert!(!version.has_build());

        let version = Version::new_major(1);
        assert!(!version.has_minor());
        assert!(!version.has_patch());
        assert!(!version.has_prerelease());
        assert!(!version.has_build());
    }

    #[test]
    fn test_build_separator_normalization() {
        // Underscore input
        let v1 = Version::parse("1.2.3_build").unwrap();
        assert_eq!(v1.to_string(), "1.2.3_build");

        // Plus input normalizes to underscore
        let v2 = Version::parse("1.2.3+build").unwrap();
        assert_eq!(v2.to_string(), "1.2.3_build");

        // Both parse to the same value
        assert_eq!(v1, v2);

        // With prerelease
        let v3 = Version::parse("1.2.3-alpha_build").unwrap();
        let v4 = Version::parse("1.2.3-alpha+build").unwrap();
        assert_eq!(v3, v4);
        assert_eq!(v3.to_string(), "1.2.3-alpha_build");
        assert_eq!(v4.to_string(), "1.2.3-alpha_build");

        // Round-trip
        let v = Version::new_build(1, 2, 3, "b1");
        assert_eq!(Version::parse(&v.to_string()), Some(v));
    }

    #[test]
    fn test_version_display_uses_underscore() {
        assert_eq!(Version::new_build(1, 2, 3, "b1").to_string(), "1.2.3_b1");
        assert_eq!(
            Version::new_prerelease_with_build(1, 2, 3, "alpha", "b1").to_string(),
            "1.2.3-alpha_b1"
        );
    }

    // ── Variant parsing tests ─────────────────────────────────────

    #[test]
    fn variant_parse_basic() {
        let v = Version::parse("debug-3.12.5").unwrap();
        assert_eq!(v.variant(), Some("debug"));
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), Some(12));
        assert_eq!(v.patch(), Some(5));
        assert!(!v.has_prerelease());
        assert!(!v.has_build());
    }

    #[test]
    fn variant_parse_dotted_name() {
        let v = Version::parse("pgo.lto-3.12.5_b1").unwrap();
        assert_eq!(v.variant(), Some("pgo.lto"));
        assert_eq!(v.major(), 3);
        assert_eq!(v.build(), Some("b1".to_string()));
    }

    #[test]
    fn variant_parse_with_prerelease_and_build() {
        let v = Version::parse("debug-3.12.5-alpha_b1").unwrap();
        assert_eq!(v.variant(), Some("debug"));
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), Some(12));
        assert_eq!(v.patch(), Some(5));
        assert_eq!(v.prerelease(), Some("alpha".to_string()));
        assert_eq!(v.build(), Some("b1".to_string()));
    }

    #[test]
    fn variant_parse_major_only() {
        let v = Version::parse("debug-3").unwrap();
        assert_eq!(v.variant(), Some("debug"));
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), None);
    }

    #[test]
    fn variant_parse_minor_only() {
        let v = Version::parse("debug-3.12").unwrap();
        assert_eq!(v.variant(), Some("debug"));
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), Some(12));
        assert_eq!(v.patch(), None);
    }

    #[test]
    fn variant_parse_rejects_bare_variant() {
        assert!(Version::parse("debug").is_none());
        assert!(Version::parse("pgo.lto").is_none());
    }

    #[test]
    fn variant_parse_rejects_no_digit_after_hyphen() {
        assert!(Version::parse("debug-latest").is_none());
        assert!(Version::parse("debug-abc").is_none());
    }

    #[test]
    fn variant_parse_rejects_uppercase() {
        assert!(Version::parse("DEBUG-3.12").is_none());
        assert!(Version::parse("Debug-3.12").is_none());
    }

    #[test]
    fn variant_parse_rejects_reserved_latest() {
        assert!(Version::parse("latest-3.12").is_none());
    }

    #[test]
    fn variant_parse_canary_is_valid_variant() {
        // "canary" is not reserved — it's just a variant name
        let v = Version::parse("canary-3.12").unwrap();
        assert_eq!(v.variant(), Some("canary"));
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), Some(12));
    }

    #[test]
    fn variant_parse_rejects_invalid_variant_chars() {
        // Hyphens not allowed in variant names
        assert!(Version::parse("my-variant-3.12").is_none());
        // Underscores not allowed in variant names
        assert!(Version::parse("my_variant-3.12").is_none());
    }

    #[test]
    fn variant_display_round_trip() {
        let cases = [
            "debug-3.12.5",
            "pgo.lto-3.12.5_b1",
            "debug-3.12.5-alpha_b1",
            "debug-3",
            "debug-3.12",
            "slim-1.0.0",
        ];
        for case in cases {
            let v = Version::parse(case).unwrap();
            assert_eq!(v.to_string(), case, "Round-trip failed for {case}");
            assert_eq!(Version::parse(&v.to_string()), Some(v), "Re-parse failed for {case}");
        }
    }

    #[test]
    fn variant_non_variant_versions_have_no_variant() {
        for tag in ["3.28.1", "3.28.1-alpha", "3.28.1_b1", "3.28.1-alpha_b1", "1", "1.2"] {
            let v = Version::parse(tag).unwrap();
            assert_eq!(v.variant(), None, "Expected no variant for {tag}");
            assert!(!v.has_variant());
        }
    }

    // ── Variant ordering tests ────────────────────────────────────

    #[test]
    fn variant_ordering_none_after_some() {
        let default = Version::new_major(3);
        let debug = Version::parse("debug-3").unwrap();
        assert!(default > debug, "None (default) variant should sort after Some variant");
    }

    #[test]
    fn variant_ordering_alphabetical() {
        let debug = Version::parse("debug-3").unwrap();
        let pgo = Version::parse("pgo-3").unwrap();
        let slim = Version::parse("slim-3").unwrap();
        assert!(debug < pgo);
        assert!(pgo < slim);
    }

    #[test]
    fn variant_ordering_same_variant_uses_version() {
        let v1 = Version::parse("debug-3.28.0_b1").unwrap();
        let v2 = Version::parse("debug-3.28.1_b1").unwrap();
        let v3 = Version::parse("debug-3.29.0_b1").unwrap();
        assert!(v1 < v2);
        assert!(v2 < v3);
    }

    #[test]
    fn variant_ordering_btreeset_clusters() {
        use std::collections::BTreeSet;
        let versions: BTreeSet<Version> = [
            "debug-3.28.0_b1",
            "3.28.0_b1",
            "pgo-3.28.0_b1",
            "debug-3.29.0_b1",
            "3.29.0_b1",
            "pgo-1.0.0_b1",
        ]
        .iter()
        .map(|s| Version::parse(s).unwrap())
        .collect();

        let ordered: Vec<_> = versions.iter().map(|v| v.to_string()).collect();
        // Named variants first (alphabetical), then None (default) last
        assert_eq!(
            ordered,
            vec![
                "debug-3.28.0_b1",
                "debug-3.29.0_b1",
                "pgo-1.0.0_b1",
                "pgo-3.28.0_b1",
                "3.28.0_b1",
                "3.29.0_b1",
            ]
        );
    }

    // ── Variant parent chain tests ────────────────────────────────

    #[test]
    fn variant_parent_preserves_variant() {
        let v = Version::parse("debug-3.12.5_b1").unwrap();
        assert_eq!(v.variant(), Some("debug"));

        let p1 = v.parent().unwrap(); // debug-3.12.5
        assert_eq!(p1.to_string(), "debug-3.12.5");
        assert_eq!(p1.variant(), Some("debug"));

        let p2 = p1.parent().unwrap(); // debug-3.12
        assert_eq!(p2.to_string(), "debug-3.12");
        assert_eq!(p2.variant(), Some("debug"));

        let p3 = p2.parent().unwrap(); // debug-3
        assert_eq!(p3.to_string(), "debug-3");
        assert_eq!(p3.variant(), Some("debug"));

        // Major has no parent
        assert!(p3.parent().is_none());
    }

    #[test]
    fn variant_parent_with_prerelease() {
        let v = Version::parse("debug-3.12.5-alpha_b1").unwrap();

        let p1 = v.parent().unwrap(); // debug-3.12.5-alpha
        assert_eq!(p1.to_string(), "debug-3.12.5-alpha");
        assert_eq!(p1.variant(), Some("debug"));

        let p2 = p1.parent().unwrap(); // debug-3.12.5
        assert_eq!(p2.to_string(), "debug-3.12.5");
        assert_eq!(p2.variant(), Some("debug"));
    }

    // ── Variant track tests ───────────────────────────────────────

    #[test]
    fn variant_same_track() {
        let v1 = Version::parse("debug-3.28.0_b1").unwrap();
        let v2 = Version::parse("debug-3.29.0_b1").unwrap();
        assert_eq!(v1.variant(), v2.variant());
    }

    #[test]
    fn variant_different_tracks() {
        let debug = Version::parse("debug-3.28.0_b1").unwrap();
        let pgo = Version::parse("pgo-3.28.0_b1").unwrap();
        let default = Version::new_build(3, 28, 0, "b1");
        assert_ne!(debug.variant(), pgo.variant());
        assert_ne!(debug.variant(), default.variant());
        assert_ne!(pgo.variant(), default.variant());
    }

    #[test]
    fn variant_default_track() {
        let v1 = Version::new_build(3, 28, 0, "b1");
        let v2 = Version::new_build(3, 29, 0, "b1");
        assert_eq!(v1.variant(), v2.variant());
    }

    #[test]
    fn without_variant_strips_variant() {
        let v = Version::parse("debug-3.12.5_b1").unwrap();
        assert_eq!(v.variant(), Some("debug"));
        let bare = v.without_variant();
        assert_eq!(bare.variant(), None);
        assert_eq!(bare.to_string(), "3.12.5_b1");
        assert_eq!(bare.major(), 3);
    }

    #[test]
    fn with_variant_sets_variant() {
        let v = Version::new_patch(3, 12, 5).with_variant("debug");
        assert_eq!(v.variant(), Some("debug"));
        assert_eq!(v.to_string(), "debug-3.12.5");
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), Some(12));
        assert_eq!(v.patch(), Some(5));
    }

    #[test]
    fn with_variant_replaces_existing() {
        let v = Version::parse("debug-3.12.5").unwrap().with_variant("pgo");
        assert_eq!(v.variant(), Some("pgo"));
        assert_eq!(v.to_string(), "pgo-3.12.5");
    }

    #[test]
    fn with_variant_preserves_prerelease_and_build() {
        let v = Version::new_prerelease_with_build(1, 2, 3, "alpha", "b1").with_variant("slim");
        assert_eq!(v.variant(), Some("slim"));
        assert_eq!(v.prerelease(), Some("alpha".to_string()));
        assert_eq!(v.build(), Some("b1".to_string()));
        assert_eq!(v.to_string(), "slim-1.2.3-alpha_b1");
    }

    #[test]
    fn with_variant_on_major_only() {
        let v = Version::new_major(5).with_variant("canary");
        assert_eq!(v.variant(), Some("canary"));
        assert_eq!(v.to_string(), "canary-5");
        assert_eq!(v.minor(), None);
    }

    #[test]
    fn without_variant_noop_for_default() {
        let v = Version::parse("3.12.5_b1").unwrap();
        let bare = v.without_variant();
        assert_eq!(bare.to_string(), "3.12.5_b1");
        assert_eq!(bare.variant(), None);
    }
}
