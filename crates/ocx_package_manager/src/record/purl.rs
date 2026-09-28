// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Package URL rendering for the `uri` field of a record descriptor:
//! `pkg:oci/<name>@sha256:<hex>?repository_url=<registry>/<repository>&tag=<tag>&arch=<arch>`.
//!
//! The version colon stays unencoded, as ECMA-427 §5.4 requires; qualifiers serialize alphabetically, so assert on
//! parsed qualifiers (`adr_exec_resolution_record.md` § "Rationale from code: launch").

use packageurl::PackageUrl;

use ocx_oci::{PinnedPackageRef, Platform};

/// The registered purl type; OCX packages are OCI artifacts, and an invented `pkg:ocx` would join with nothing.
const PURL_TYPE: &str = "oci";

/// Repository prefix of the digest-only placeholder `PackageManager::install_info_from_package_root` mints.
const PLACEHOLDER_REPOSITORY_PREFIX: &str = "file-url-mode/";

/// Whether `identifier` names a published package rather than a local placeholder, whose registry and repository
/// are not facts about the package and must never be emitted as identity.
pub fn has_logical_identity(identifier: &PinnedPackageRef) -> bool {
    !identifier.repository().starts_with(PLACEHOLDER_REPOSITORY_PREFIX)
}

/// Render a pinned identifier as a package URL, or `None` when it has no logical identity; `platform` contributes
/// the `arch` qualifier.
///
/// A construction rejection is logged at debug and treated as absent identity, never failing the invocation.
pub fn package_url(identifier: &PinnedPackageRef, platform: Option<&Platform>) -> Option<String> {
    if !has_logical_identity(identifier) {
        return None;
    }
    match render(identifier, platform) {
        Ok(purl) => Some(purl),
        Err(error) => {
            tracing::debug!(%identifier, %error, "package URL construction rejected; recording digest-only identity");
            None
        }
    }
}

fn render(identifier: &PinnedPackageRef, platform: Option<&Platform>) -> packageurl::Result<String> {
    let mut purl = PackageUrl::new(PURL_TYPE, identifier.name())?;
    purl.with_version(identifier.digest().to_string())?;
    purl.add_qualifier("repository_url", repository_url(identifier))?;
    if let Some(tag) = identifier.tag() {
        purl.add_qualifier("tag", tag.to_string())?;
    }
    if let Some(Platform::Specific { arch, .. }) = platform {
        purl.add_qualifier("arch", arch.to_string())?;
    }
    Ok(purl.to_string())
}

/// Registry plus the full repository path, repeating the name segment as the `oci` purl type's own test cases do
/// (`docker.io/library/debian` for `debian`); it is what keeps `a/cli` and `b/cli` apart.
fn repository_url(identifier: &PinnedPackageRef) -> String {
    format!("{}/{}", identifier.registry(), identifier.repository())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use super::*;
    use ocx_oci::{Architecture, Digest, OperatingSystem, PackageRef};

    const LEAF_HEX: &str = "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0";

    fn pinned(repository: &str, registry: &str, tag: Option<&str>) -> PinnedPackageRef {
        let mut identifier = PackageRef::new_registry(repository, registry);
        if let Some(tag) = tag {
            identifier = identifier.clone_with_tag(tag);
        }
        PinnedPackageRef::try_from(identifier.clone_with_digest(Digest::Sha256(LEAF_HEX.to_string())))
            .expect("digest present")
    }

    fn linux_amd64() -> Platform {
        Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_features: vec!["libc.glibc".to_string()],
        }
    }

    /// Parse the rendered purl back, so assertions never depend on qualifier
    /// order: the crate emits them alphabetically, not in authored order.
    fn qualifiers(purl: &str) -> HashMap<String, String> {
        PackageUrl::from_str(purl)
            .expect("rendered purl must parse")
            .qualifiers()
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn type_is_oci_never_an_invented_ocx_type() {
        let purl = package_url(&pinned("ocx/cmake", "index.ocx.sh", None), None).expect("purl");
        assert!(purl.starts_with("pkg:oci/"), "{purl}");
        assert_eq!(PackageUrl::from_str(&purl).expect("parses").ty(), "oci");
    }

    #[test]
    fn version_carries_the_digest_with_an_unencoded_colon() {
        let purl = package_url(&pinned("ocx/cmake", "index.ocx.sh", None), None).expect("purl");
        assert!(
            purl.contains(&format!("@sha256:{LEAF_HEX}")),
            "colon must survive unencoded: {purl}"
        );
        assert!(!purl.contains("sha256%3A"), "colon must not be percent-encoded: {purl}");
        assert_eq!(
            PackageUrl::from_str(&purl).expect("parses").version(),
            Some(format!("sha256:{LEAF_HEX}").as_str()),
            "the unencoded colon must round-trip through the parser"
        );
    }

    #[test]
    fn name_is_the_last_repository_segment() {
        let purl = package_url(&pinned("ocx/cmake", "index.ocx.sh", None), None).expect("purl");
        assert_eq!(PackageUrl::from_str(&purl).expect("parses").name(), "cmake");
    }

    #[test]
    fn repository_url_qualifier_disambiguates_equal_names() {
        let first = package_url(&pinned("a/cli", "ocx.sh", None), None).expect("purl");
        let second = package_url(&pinned("b/cli", "ocx.sh", None), None).expect("purl");

        assert_eq!(PackageUrl::from_str(&first).expect("parses").name(), "cli");
        assert_eq!(PackageUrl::from_str(&second).expect("parses").name(), "cli");
        assert_eq!(
            qualifiers(&first).get("repository_url").map(String::as_str),
            Some("ocx.sh/a/cli")
        );
        assert_eq!(
            qualifiers(&second).get("repository_url").map(String::as_str),
            Some("ocx.sh/b/cli")
        );
        assert_ne!(first, second, "name alone is not identity");
    }

    /// A single-segment repository still repeats into the qualifier — the `oci`
    /// type's `pkg:oci/debian?repository_url=docker.io/library/debian` shape has
    /// no special case for how many segments the repository happens to carry.
    #[test]
    fn repository_url_carries_the_full_repository_for_a_single_segment_repository() {
        let purl = package_url(&pinned("solver", "internal.corp.example", None), None).expect("purl");
        assert_eq!(
            qualifiers(&purl).get("repository_url").map(String::as_str),
            Some("internal.corp.example/solver")
        );
    }

    #[test]
    fn tag_qualifier_is_absent_when_no_tag_was_resolved() {
        let purl = package_url(&pinned("ocx/cmake", "index.ocx.sh", None), None).expect("purl");
        assert!(
            !qualifiers(&purl).contains_key("tag"),
            "a project-tier record has no tag to report: {purl}"
        );
    }

    #[test]
    fn tag_qualifier_is_present_when_a_tag_was_resolved() {
        let purl = package_url(&pinned("solver", "internal.corp.example", Some("2024.3")), None).expect("purl");
        assert_eq!(qualifiers(&purl).get("tag").map(String::as_str), Some("2024.3"));
    }

    #[test]
    fn arch_qualifier_follows_the_resolved_platform() {
        let platform = linux_amd64();
        let purl = package_url(&pinned("ocx/cmake", "index.ocx.sh", None), Some(&platform)).expect("purl");
        assert_eq!(qualifiers(&purl).get("arch").map(String::as_str), Some("amd64"));
    }

    #[test]
    fn arch_qualifier_is_absent_without_a_specific_platform() {
        let identifier = pinned("ocx/cmake", "index.ocx.sh", None);
        let unknown = package_url(&identifier, None).expect("purl");
        let any = package_url(&identifier, Some(&Platform::any())).expect("purl");
        assert!(!qualifiers(&unknown).contains_key("arch"), "{unknown}");
        assert!(!qualifiers(&any).contains_key("arch"), "{any}");
    }

    #[test]
    fn qualifier_order_is_alphabetical_not_authored() {
        let platform = linux_amd64();
        let purl = package_url(&pinned("ocx/cmake", "index.ocx.sh", Some("3.28")), Some(&platform)).expect("purl");
        let query = purl.split_once('?').expect("qualifiers present").1;
        let keys: Vec<&str> = query.split('&').filter_map(|pair| pair.split('=').next()).collect();
        assert_eq!(
            keys,
            vec!["arch", "repository_url", "tag"],
            "documented so no test or doc asserts the ADR's authored order: {purl}"
        );
    }

    #[test]
    fn placeholder_identity_yields_no_purl() {
        let identifier = pinned(&format!("file-url-mode/{LEAF_HEX}"), "ocx.sh", None);
        assert!(!has_logical_identity(&identifier));
        assert_eq!(
            package_url(&identifier, Some(&linux_amd64())),
            None,
            "a content-addressed placeholder must never be dressed up as a published identity"
        );
    }
}
