use serde::{Deserialize, Serialize};

use crate::prelude::*;

type PatchRest = (Option<String>, Option<String>);
type MinorRest = Option<(u32, PatchRest)>;

#[derive(Clone, Hash, PartialEq, Eq)]
pub struct Version {
    /// major version, always present, consequently there is no representation of a 'latest' version
    major: u32,
    /// tuple of (minor, tuple of (patch, (build, prerelease))), ensuring that minor is only present if patch is present, and patch is only present if prerelease is present
    rest: Option<(u32, MinorRest)>,
}

/// Close to semver version, but with rolling parent versions and no build info.
/// Prereleases are not supported for rolling versions, ie. '1-alpha' is not a valid version.
impl Version {
    pub fn new_major(major: u32) -> Self {
        Self { major, rest: None }
    }

    pub fn new_minor(major: u32, minor: u32) -> Self {
        Self {
            major,
            rest: Some((minor, None)),
        }
    }

    pub fn new_patch(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            rest: Some((minor, Some((patch, (None, None))))),
        }
    }

    pub fn new_build(major: u32, minor: u32, patch: u32, build: impl Into<String>) -> Self {
        Self {
            major,
            rest: Some((minor, Some((patch, (Some(build.into()), None))))),
        }
    }

    pub fn new_prerelease(major: u32, minor: u32, patch: u32, prerelease: impl Into<String>) -> Self {
        Self {
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
            major,
            rest: Some((minor, Some((patch, (Some(build.into()), Some(prerelease.into())))))),
        }
    }

    /// Returns the parent version, or None if this version is a major version with no minor version.
    pub fn parent(&self) -> Option<Self> {
        if let Some((minor, patch)) = &self.rest {
            if let Some((patch, (build, prerelease))) = patch {
                if build.is_some() {
                    Some(Self {
                        major: self.major,
                        rest: Some((*minor, Some((*patch, (None, prerelease.clone()))))),
                    })
                } else if prerelease.is_some() {
                    Some(Self {
                        major: self.major,
                        rest: Some((*minor, Some((*patch, (None, None))))),
                    })
                } else {
                    Some(Self {
                        major: self.major,
                        rest: Some((*minor, None)),
                    })
                }
            } else {
                Some(Self {
                    major: self.major,
                    rest: None,
                })
            }
        } else {
            None
        }
    }

    /// Returns a list of versions that should be considered for cascading, starting with the current version
    /// and ending with the latest parent version that should be cascaded to.
    ///
    /// Attention: This function is not platform aware.
    /// The caller is responsible for ensuring that the versions in `others` are all from the same platform as `self`.
    /// Otherwise, versions may be not cascaded although they are the latest version for a specific platform.
    pub fn cascade(&self, others: impl IntoIterator<Item = Self>) -> (Vec<Self>, bool) {
        use std::{
            collections::BTreeSet,
            ops::Bound::{Excluded, Unbounded},
        };
        let others = others.into_iter().collect::<BTreeSet<_>>();

        // special case for pre-releases
        // pre-releases shall never cascade beyond the pre-release level, only the build may cascade.
        if self.has_prerelease() {
            if self.has_build() {
                if let Some(version) = others.range((Excluded(self), Unbounded)).next()
                    && version.major() == self.major()
                    && version.minor() == self.minor()
                    && version.patch() == self.patch()
                    && version.prerelease() == self.prerelease()
                    && version.build() != self.build()
                {
                    // there is a larger version, that only differs by the build fragment
                    // we are not the latest pre-release and do not cascade to the parent version.
                    return (vec![self.clone()], false);
                }
                // fallback, we are the latest pre-release and cascade to the parent version without build.
                return (
                    vec![
                        self.clone(),
                        self.parent()
                            .expect("Versions with build fragment shall always have a parent."),
                    ],
                    false,
                );
            }
            return (vec![self.clone()], false);
        }

        let mut versions = vec![self.clone()];
        let mut current = Some(self.clone());
        let mut is_latest = false;

        while let Some(current_version) = &current {
            let parent_version = current_version.parent();
            let least_greater = match others.range((Excluded(current_version), Unbounded)).next() {
                Some(version) => version,
                None => {
                    if let Some(parent_version) = parent_version {
                        versions.push(parent_version.clone());
                        current = Some(parent_version);
                        continue;
                    } else {
                        is_latest = true;
                        break;
                    }
                }
            };

            // if there is no successor with the same level of specificity, we are the latest version for this level,
            // and we should cascade to the parent version.
            if parent_version <= Some(least_greater.clone()) {
                if let Some(parent_version) = parent_version {
                    versions.push(parent_version.clone());
                    current = Some(parent_version);
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        (versions, is_latest)
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

    pub fn parse(value: &str) -> Option<Self> {
        use regex::Regex;
        use std::sync::LazyLock;

        static VERSION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"^(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*)(-([0-9a-zA-Z]+))?(\+([0-9a-zA-Z]+))?)?)?$",
            )
            .expect("Invalid version regex!")
        });

        let captures = VERSION_REGEX.captures(value)?;

        let major = captures.get(1).unwrap().as_str().parse::<u32>().ok()?;
        let minor = match captures.get(3).map(|m| m.as_str()) {
            Some("") | None => return Some(Version { major, rest: None }),
            Some(minor) => match minor.parse::<u32>().ok() {
                None => return Some(Version { major, rest: None }),
                Some(minor) => minor,
            },
        };
        let patch = match captures.get(5).map(|m| m.as_str()) {
            Some("") | None => {
                return Some(Version {
                    major,
                    rest: Some((minor, None)),
                });
            }
            Some(patch) => match patch.parse::<u32>().ok() {
                None => {
                    return Some(Version {
                        major,
                        rest: Some((minor, None)),
                    });
                }
                Some(patch) => patch,
            },
        };
        let prerelease = captures.get(7).map(|x| x.as_str().to_string());
        let build = captures.get(9).map(|x| x.as_str().to_string());
        Some(Version {
            major,
            rest: Some((minor, Some((patch, (build, prerelease))))),
        })
    }
}

impl Ord for Version {
    fn cmp(&self, rhs: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        let lhs = self;
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
            None => Err(Error::PackageVersionInvalid(value.into())),
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut version = self.major.to_string();

        if let Some((minor, rest)) = self.rest.as_ref() {
            version.push_str(&format!(".{}", minor));
            if let Some((patch, rest)) = rest {
                version.push_str(&format!(".{}", patch));
                if let (_, Some(prerelease)) = rest {
                    version.push_str(&format!("-{}", prerelease));
                }
                if let (Some(build), _) = rest {
                    version.push_str(&format!("+{}", build));
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
        let version = Version::parse("1.2.3+20260216").unwrap();
        assert!(!version.is_rolling());
        let version = Version::parse("1.2.3-alpha+20260216").unwrap();
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
    fn test_version_cascade() {
        let version_build = Version::new_build(1, 7, 3, "20260216");
        let version_build_prev = Version::new_build(1, 7, 3, "20260215");
        let version_build_next = Version::new_build(1, 7, 3, "20260217");
        let version_patch = Version::new_patch(1, 7, 3);
        let version_patch_prev = Version::new_patch(1, 7, 2);
        let version_patch_next = Version::new_patch(1, 7, 4);
        let version_minor = Version::new_minor(1, 7);
        let version_minor_prev = Version::new_minor(1, 6);
        let version_minor_next = Version::new_minor(1, 8);
        let version_major = Version::new_major(1);
        let version_major_prev = Version::new_major(0);
        let version_major_next = Version::new_major(2);

        let latest_major = (
            vec![
                version_build.clone(),
                version_patch.clone(),
                version_minor.clone(),
                version_major.clone(),
            ],
            true,
        );
        let latest_minor = (
            vec![
                version_build.clone(),
                version_patch.clone(),
                version_minor.clone(),
                version_major.clone(),
            ],
            false,
        );
        let latest_patch = (
            vec![version_build.clone(), version_patch.clone(), version_minor.clone()],
            false,
        );
        let latest_build = (vec![version_build.clone(), version_patch.clone()], false);
        let interim = (vec![version_build.clone()], false);

        assert_eq!(version_build.cascade(vec![]), latest_major);
        assert_eq!(version_build.cascade(vec![version_build_prev]), latest_major);
        assert_eq!(version_build.cascade(vec![version_patch_prev]), latest_major);
        assert_eq!(version_build.cascade(vec![version_minor_prev]), latest_major);
        assert_eq!(version_build.cascade(vec![version_major_prev]), latest_major);

        assert_eq!(version_build.cascade(vec![version_build_next]), interim);
        assert_eq!(version_build.cascade(vec![version_patch_next]), latest_build);
        assert_eq!(version_build.cascade(vec![version_minor_next]), latest_patch);
        assert_eq!(version_build.cascade(vec![version_major_next]), latest_minor);

        // pre-releases
        let version_prerelease = Version::new_prerelease(1, 7, 3, "beta");
        let version_prerelease_prev = Version::new_prerelease(1, 7, 3, "20260215");
        let version_prerelease_next = Version::new_prerelease(1, 7, 3, "20260217");

        let latest_prerelease = (vec![version_prerelease.clone()], false);
        let interim_prerelease = (vec![version_prerelease.clone()], false);

        assert_eq!(version_prerelease.cascade(vec![]), latest_prerelease);
        assert_eq!(
            version_prerelease.cascade(vec![version_prerelease_prev.clone()]),
            latest_prerelease
        );
        assert_eq!(
            version_prerelease.cascade(vec![version_prerelease_next.clone()]),
            interim_prerelease
        );

        // pre-releases with build
        let version_prerelease_build = Version::new_prerelease_with_build(1, 7, 3, "beta", "20260216");
        let version_prerelease_build_prev = Version::new_prerelease_with_build(1, 7, 3, "beta", "20260215");
        let version_prerelease_build_next = Version::new_prerelease_with_build(1, 7, 3, "beta", "20260217");
        let version_prerelease_build_parent = Version::new_prerelease(1, 7, 3, "beta");
        let version_prerelease_build_parent_prev = Version::new_prerelease(1, 7, 3, "alpha");
        let version_prerelease_build_parent_next = Version::new_prerelease(1, 7, 3, "gamma");

        let latest_prerelease_build = (
            vec![
                version_prerelease_build.clone(),
                version_prerelease_build_parent.clone(),
            ],
            false,
        );
        let interim_prerelease_build = (vec![version_prerelease_build.clone()], false);

        assert_eq!(version_prerelease_build.cascade(vec![]), latest_prerelease_build);
        assert_eq!(
            version_prerelease_build.cascade(vec![version_prerelease_build_prev.clone()]),
            latest_prerelease_build
        );
        assert_eq!(
            version_prerelease_build.cascade(vec![version_prerelease_build_parent_prev.clone()]),
            latest_prerelease_build
        );
        assert_eq!(
            version_prerelease_build.cascade(vec![version_prerelease_build_next.clone()]),
            interim_prerelease_build
        );
        assert_eq!(
            version_prerelease_build.cascade(vec![version_prerelease_build_parent_next.clone()]),
            latest_prerelease_build
        );
    }
}
