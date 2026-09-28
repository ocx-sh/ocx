// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `--platform` optionality rule sign, attest and verify share.
//!
//! Nothing here may read a clock: the golden keyless fixtures carry an
//! already-expired Fulcio certificate, so a wall-clock check reds them every run.

use crate::{Digest, Manifest, OciIdentifier, Platform, Selection, select_best};

/// What a sign, attest or verify run acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignTarget {
    pub subject_digest: Digest,
    /// The index `--platform` narrowed through, if any; an index signature
    /// covers a child only when the child was reached through that index.
    pub enclosing_index: Option<Digest>,
}

/// Apply the `--platform` optionality rule: absent → act on the resolved
/// object as-is; present → narrow into the index and act on that child.
///
/// Branches on `children` (`None` for a bare manifest), never the reference's
/// form, via [`select_best`] (`adr_platform_model_unification.md`).
///
/// # Errors
/// [`ResolveTargetError::NotAnIndex`] when a platform was requested and the
/// resolved object is a bare manifest; [`ResolveTargetError::PlatformNotFound`]
/// when no child is compatible; [`ResolveTargetError::AmbiguousPlatform`] when
/// more than one child is equally best.
pub fn resolve_sign_target(
    resolved_digest: &Digest,
    children: Option<&[(Platform, Digest)]>,
    platform: Option<&Platform>,
) -> Result<SignTarget, ResolveTargetError> {
    let Some(platform) = platform else {
        return Ok(SignTarget {
            subject_digest: resolved_digest.clone(),
            enclosing_index: None,
        });
    };
    let Some(children) = children else {
        return Err(ResolveTargetError::NotAnIndex {
            platform: platform.to_string(),
        });
    };
    let candidates: Vec<(Digest, Platform)> = children
        .iter()
        .map(|(offered, digest)| (digest.clone(), offered.clone()))
        .collect();
    match select_best(platform, &candidates) {
        Selection::Found(child) => Ok(SignTarget {
            subject_digest: child,
            enclosing_index: Some(resolved_digest.clone()),
        }),
        Selection::Ambiguous(_) => Err(ResolveTargetError::AmbiguousPlatform {
            platform: platform.to_string(),
        }),
        Selection::None => Err(ResolveTargetError::PlatformNotFound {
            platform: platform.to_string(),
        }),
    }
}

/// What a caller's resolution hands a signing pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSubject {
    pub target: SignTarget,
    /// Verify's membership set; empty for a bare manifest and on sign/attest.
    pub index_members: Vec<Digest>,
    pub physical: OciIdentifier,
}

impl SignTarget {
    /// [`resolve_sign_target`] over a resolved manifest's platform candidates.
    ///
    /// # Errors
    /// [`ResolveTargetError`], exactly as [`resolve_sign_target`] raises it.
    pub fn from_resolved(
        resolved_digest: &Digest,
        manifest: &Manifest,
        platform: Option<&Platform>,
    ) -> Result<Self, ResolveTargetError> {
        let children: Option<Vec<(Platform, Digest)>> = match manifest {
            Manifest::ImageIndex(index) => Some(
                index
                    .manifests
                    .iter()
                    .filter_map(|entry| {
                        // Same rule as `Index::fetch_candidates`: a platformless entry is a referrer.
                        let platform = Platform::candidate_from_descriptor(entry)?;
                        Some((platform, Digest::try_from(entry.digest.clone()).ok()?))
                    })
                    .collect(),
            ),
            Manifest::Image(_) => None,
        };
        resolve_sign_target(resolved_digest, children.as_deref(), platform)
    }
}

/// Every digest a resolved object lists, referrer entries included.
///
/// Narrowing this to `--platform` candidates would refuse membership for a
/// child the signed index demonstrably lists.
#[must_use]
pub fn index_members(manifest: &Manifest) -> Vec<Digest> {
    match manifest {
        Manifest::ImageIndex(index) => index
            .manifests
            .iter()
            .filter_map(|entry| Digest::try_from(entry.digest.clone()).ok())
            .collect(),
        Manifest::Image(_) => Vec::new(),
    }
}

/// Why the `--platform` narrowing could not name a single object.
// Not `#[non_exhaustive]`: sign and verify match it exhaustively, and a `_` arm
// would silently swallow a new variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveTargetError {
    #[error("--platform {platform} was given but the reference resolved to a single manifest, not an index")]
    NotAnIndex { platform: String },
    #[error("no manifest for platform {platform}")]
    PlatformNotFound { platform: String },
    #[error("platform {platform} matches more than one manifest")]
    AmbiguousPlatform { platform: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> Digest {
        Digest::Sha256(format!("{byte:064x}"))
    }

    fn platform(value: &str) -> Platform {
        value.parse::<Platform>().expect("test platform parses")
    }

    /// One T-13 row: the reference form that produced the resolution (label
    /// only — the function never sees it), the resolution outcome, the
    /// requested platform, and the answer owed.
    type Row<'a> = (
        &'a str,
        Option<&'a [(Platform, Digest)]>,
        Option<Platform>,
        Result<SignTarget, ResolveTargetError>,
    );

    /// T-13. Every row names the **reference form** that produced its
    /// resolution, and two rows deliberately disagree with it: a *tag* that
    /// resolved to a bare manifest, and a *digest* that resolved to an index.
    /// An implementation that branched on the reference instead of on
    /// `children` would answer those two wrongly while the agreeing rows still
    /// passed — which is exactly what makes them load-bearing.
    #[test]
    fn resolve_sign_target_branches_on_resolution_not_on_reference_form() {
        let resolved = digest(0xaa);
        let amd64 = digest(0x01);
        let arm64 = digest(0x02);
        let twin = digest(0x03);

        let index = [
            (platform("linux/amd64"), amd64.clone()),
            (platform("linux/arm64"), arm64.clone()),
        ];
        let foreign = [(platform("windows/amd64"), amd64.clone())];
        let tied = [
            (platform("linux/amd64"), amd64.clone()),
            (platform("linux/amd64"), twin.clone()),
        ];
        let empty: [(Platform, Digest); 0] = [];

        let acts_on_resolved = Ok(SignTarget {
            subject_digest: resolved.clone(),
            enclosing_index: None,
        });
        let narrowed = Ok(SignTarget {
            subject_digest: amd64.clone(),
            enclosing_index: Some(resolved.clone()),
        });
        let not_an_index = Err(ResolveTargetError::NotAnIndex {
            platform: "linux/amd64".into(),
        });
        let not_found = Err(ResolveTargetError::PlatformNotFound {
            platform: "linux/amd64".into(),
        });

        let rows: [Row<'_>; 9] = [
            // Reference form and resolved shape AGREE.
            (
                "tag -> bare manifest, no platform",
                None,
                None,
                acts_on_resolved.clone(),
            ),
            (
                "tag -> index, no platform (the index itself is signed)",
                Some(&index),
                None,
                acts_on_resolved.clone(),
            ),
            (
                "tag -> index, platform narrows into a compatible child",
                Some(&index),
                Some(platform("linux/amd64")),
                narrowed.clone(),
            ),
            (
                "digest -> bare manifest, platform",
                None,
                Some(platform("linux/amd64")),
                not_an_index.clone(),
            ),
            // Reference form and resolved shape DISAGREE — the T-13 rows.
            (
                "TAG -> bare manifest, platform (E-05, load-bearing)",
                None,
                Some(platform("linux/amd64")),
                not_an_index.clone(),
            ),
            (
                "DIGEST -> index, platform narrows into a compatible child",
                Some(&index),
                Some(platform("linux/amd64")),
                narrowed.clone(),
            ),
            // Narrowing requested, index cannot answer it.
            (
                "tag -> index with zero children, platform (E-06)",
                Some(&empty),
                Some(platform("linux/amd64")),
                not_found.clone(),
            ),
            (
                "tag -> index with no compatible child, platform",
                Some(&foreign),
                Some(platform("linux/amd64")),
                not_found.clone(),
            ),
            (
                "tag -> index with two equally best children, platform",
                Some(&tied),
                Some(platform("linux/amd64")),
                Err(ResolveTargetError::AmbiguousPlatform {
                    platform: "linux/amd64".into(),
                }),
            ),
        ];

        for (label, children, requested, expected) in rows {
            let actual = resolve_sign_target(&resolved, children, requested.as_ref());
            assert_eq!(actual, expected, "row '{label}'");
        }
    }

    /// The two disagreement rows are the whole point of T-13, so assert the
    /// pair directly too: identical `(children, platform)` inputs must give
    /// identical answers no matter which reference form reached them, and the
    /// answers must follow `children`.
    #[test]
    fn a_tag_does_not_imply_an_index_and_a_digest_does_not_imply_a_manifest() {
        let resolved = digest(0xaa);
        let child = digest(0x01);
        let requested = platform("linux/amd64");
        let index = [(requested.clone(), child.clone())];

        // Reached through a TAG, resolved to a bare manifest: refused, even
        // though a tag is the form an index usually wears.
        assert_eq!(
            resolve_sign_target(&resolved, None, Some(&requested)),
            Err(ResolveTargetError::NotAnIndex {
                platform: "linux/amd64".into(),
            })
        );
        // Reached through a DIGEST, resolved to an index: narrowed, even
        // though a digest names one object exactly.
        assert_eq!(
            resolve_sign_target(&resolved, Some(&index), Some(&requested)),
            Ok(SignTarget {
                subject_digest: child,
                enclosing_index: Some(resolved),
            })
        );
    }

    #[test]
    fn error_messages_name_the_platform() {
        let requested = platform("linux/arm64");
        let resolved = digest(0xaa);
        let message = resolve_sign_target(&resolved, None, Some(&requested))
            .expect_err("a bare manifest cannot be narrowed")
            .to_string();
        assert!(
            message.contains("linux/arm64"),
            "message must name the platform: {message}"
        );
        assert!(
            message.contains("not an index"),
            "message must name the reason: {message}"
        );
    }
}
