// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Package URL rendering for the `uri` field of a record descriptor.
//!
//! `pkg:oci/<name>@sha256:<hex>?repository_url=<host>/<path>&tag=<tag>&arch=<arch>`
//! — the registered `oci` purl type, never an invented `pkg:ocx`, because OCX
//! packages *are* OCI artifacts. The semantics match with zero impedance: the
//! purl version is the sha256 digest, and `tag` is documented as "the artifact
//! tag that may have been associated with the digest at the time", which is
//! exactly OCX's digest-is-identity / tag-is-advisory model.
//!
//! Four properties this depends on, each verified rather than assumed:
//!
//! - The colon in the version is emitted **unencoded** (`sha256:3f7a…`). The
//!   crate's encode set excludes `b':'`, and the round trip holds. The spec's
//!   canonical build rule says percent-encode, the `oci` type's own examples
//!   contradict each other, and the upstream issue is open; the real `pkg:oci`
//!   producers at scale emit unencoded, and a literal colon parses correctly
//!   under both spec-correct and naive parsers. Documented so nobody "fixes" it.
//! - Qualifiers serialize in **alphabetical** order, not authored order, so
//!   `arch` precedes `repository_url` precedes `tag`. Assert on parsed
//!   qualifiers, never on a literal string.
//! - The name is the **last repository segment** and is already correct by
//!   construction: repository segments are lowercase-only at the parser, so
//!   purl's lowercasing rules need no normalization layer here.
//! - A `tag` qualifier appears only when a tag was genuinely resolved. A
//!   project-tier record has none — the lock stores a bare repository plus a
//!   per-platform digest and rejects a tag at validation — and synthesising one
//!   would be the first lie in an audit record.
//!
//! What this buys is **identity, not scanning**: a stable standardised string
//! that joins across tools. Vulnerability lookup is not among them — no
//! mainstream scanner resolves a whole-artifact `pkg:oci` to CVEs.

use packageurl::PackageUrl;

use crate::oci::{PinnedIdentifier, Platform};

/// The purl type OCX packages are published under. OCX packages *are* OCI
/// artifacts, so the registered type applies and an invented `pkg:ocx` would
/// join with nothing.
const PURL_TYPE: &str = "oci";

/// Repository prefix of the placeholder identifier a package-root frame mints.
///
/// Minted by `PackageManager::install_info_from_package_root`, which has only a
/// content digest to work with: package directories are content-shared and
/// deliberately do not persist the root identifier, so there is no registry or
/// repository to recover. The prefix is the marker that an identifier is a local
/// placeholder rather than a published identity.
const PLACEHOLDER_REPOSITORY_PREFIX: &str = "file-url-mode/";

/// Whether `identifier` names a published package rather than a locally minted
/// content-addressed placeholder.
///
/// A placeholder identifier's registry is whatever default registry happened to
/// be configured and its repository is a digest — neither is a fact about where
/// the package came from, so nothing derived from it (a purl, an index source)
/// may be emitted as identity.
pub fn has_logical_identity(identifier: &PinnedIdentifier) -> bool {
    !identifier.repository().starts_with(PLACEHOLDER_REPOSITORY_PREFIX)
}

/// Render a pinned identifier as a package URL, or `None` when it has no
/// logical identity to express.
///
/// `platform` contributes the `arch` qualifier; `None` omits it.
///
/// `None` — rather than an error — is the honest encoding for the launcher
/// frame, whose identifier is a synthetic content-addressed placeholder: package
/// directories are content-shared and carry no registry or repository, so no
/// purl can be constructed and the descriptor omits `uri` entirely. A truthful
/// partial record beats a fabricated complete one, and the digest alone still
/// identifies the resource.
///
/// A construction rejection from a well-formed identifier is unreachable by the
/// name-and-segment invariants above; should one occur it is logged at debug and
/// treated as absent identity, because an environmental surprise here must never
/// fail the invocation.
pub fn package_url(identifier: &PinnedIdentifier, platform: Option<&Platform>) -> Option<String> {
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

/// Build the purl, keeping the crate's rejections in one place so the caller
/// above can degrade rather than propagate.
fn render(identifier: &PinnedIdentifier, platform: Option<&Platform>) -> packageurl::Result<String> {
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

/// The registry plus every repository segment *above* the name.
///
/// `index.ocx.sh/ocx/cmake` yields `index.ocx.sh/ocx`; a single-segment
/// repository yields the bare registry. Identity is name + `repository_url` +
/// digest, so this qualifier is what keeps `a/cli` and `b/cli` apart.
fn repository_url(identifier: &PinnedIdentifier) -> String {
    match identifier.repository().rsplit_once('/') {
        Some((namespace, _)) => format!("{}/{namespace}", identifier.registry()),
        None => identifier.registry().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use super::*;
    use crate::oci::{Architecture, Digest, Identifier, OperatingSystem};

    const LEAF_HEX: &str = "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0";

    fn pinned(repository: &str, registry: &str, tag: Option<&str>) -> PinnedIdentifier {
        let mut identifier = Identifier::new_registry(repository, registry);
        if let Some(tag) = tag {
            identifier = identifier.clone_with_tag(tag);
        }
        PinnedIdentifier::try_from(identifier.clone_with_digest(Digest::Sha256(LEAF_HEX.to_string())))
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
            Some("ocx.sh/a")
        );
        assert_eq!(
            qualifiers(&second).get("repository_url").map(String::as_str),
            Some("ocx.sh/b")
        );
        assert_ne!(first, second, "name alone is not identity");
    }

    #[test]
    fn repository_url_is_the_bare_registry_for_a_single_segment_repository() {
        let purl = package_url(&pinned("solver", "internal.corp.example", None), None).expect("purl");
        assert_eq!(
            qualifiers(&purl).get("repository_url").map(String::as_str),
            Some("internal.corp.example")
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
