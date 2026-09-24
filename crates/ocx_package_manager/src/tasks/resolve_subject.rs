// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The index half of what a sign, attest or verify run acts on.
//!
//! The three pipelines no longer hold an `&Index` (ADR 1.9): `ocx_sign` sits
//! below `ocx_index` in the crate map, so the subsystem that signs cannot be
//! the one that resolves. What is left in each pipeline is the *decision* —
//! [`sign_target_from_resolution`] and [`verify_target_from_resolution`], the
//! shared `--platform` rule and each verb's taxonomy — and what moved here is
//! the *I/O* around it: one manifest fetch through the index chain, and the
//! index-indirection pointer lookup that follows it.
//!
//! **Order is the contract, not a side effect** (plan DEC-17). Each pipeline
//! invokes the closure exactly where it invoked `resolve_platform_target`, so
//! an argument error still precedes a network one — `AttestPipeline::run_inner`
//! rejects an unsupported predicate before anything resolves, and
//! `test_package_attest.py`'s argument-error cases are the oracle for that.
//! Inside the closure the order is the one the pipelines had: fetch, apply the
//! rule, apply the sha256 floor on the signing side, *then* look the pointer
//! up. A floor moved past the lookup would answer a non-sha256 subject with
//! whatever the lookup raised.
//!
//! Resolution goes **through the index chain**, never the registry transport:
//! the transport path bypasses `guard_local_physical` and the mirror map, and
//! would break `--offline`.

use ocx_index::{Index, IndexOperation};
use ocx_oci::resolve_target::ResolvedSubject;
use ocx_oci::{Digest, Manifest, PackageRef};
use ocx_sign::sign::SignErrorKind;
use ocx_sign::sign::pipeline::{SubjectResolver, sign_target_from_resolution};
use ocx_sign::verify::VerifyErrorKind;
use ocx_sign::verify::pipeline::{VerifySubjectResolver, verify_target_from_resolution};

/// Follow the index-indirection pointer for `subject`.
///
/// [`Index::route`]: the location a rewrite names, or `subject`'s own
/// coordinates when nothing rewrites it. The SSRF floor on whatever comes
/// back is enforced upstream in the shared index choke point
/// (`ChainedIndex::guard_local_physical`) and again at the dial site, by the
/// pipeline, through its `DialPolicy`.
async fn physical_of(index: &Index, subject: &PackageRef) -> ocx_index::error::Result<ocx_oci::OciIdentifier> {
    index.route(subject).await
}

/// The resolution the sign and attest pipelines take.
///
/// `pre_resolved` is the answer the caller already has, when it has one: a
/// `--tags` sweep asks the chain what each tag names in order to skip bare
/// manifests, and re-asking cost every sweep a second fetch per tag (#373). It
/// is the *same* question or nothing — the pair must be what
/// `Index::fetch_manifest(identifier, IndexOperation::Resolve)` answered for
/// this exact identifier, or the subject digest stops being what the reference
/// names. `None` fetches.
pub(crate) fn sign_resolver<'a>(
    index: &'a Index,
    pre_resolved: Option<&'a (Digest, Manifest)>,
) -> Box<SubjectResolver<'a>> {
    Box::new(move |identifier, platform| {
        Box::pin(async move {
            // The caller's resolution when it had one, this closure's
            // otherwise. `fetched` is declared out here so the borrow taken
            // inside the arm can outlive the match.
            let fetched;
            let resolution = match pre_resolved {
                Some(pair) => Some(pair),
                None => {
                    fetched = index
                        .fetch_manifest(identifier, IndexOperation::Resolve)
                        .await
                        .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
                    fetched.as_ref()
                }
            };
            let target = sign_target_from_resolution(resolution, platform)?;
            let subject = identifier.clone_with_digest(target.subject_digest.clone());
            let physical = physical_of(index, &subject)
                .await
                .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
            Ok(ResolvedSubject {
                target,
                // Sign and attest never read it; the membership test is
                // verify's alone.
                index_members: Vec::new(),
                physical,
            })
        })
    })
}

/// The resolution the verify pipeline takes.
///
/// One fetch answers three questions at once: which digest the reference names,
/// whether that object is an index, and — when it is — which children and which
/// members it lists. Before C-008 the children were fetched inside
/// `Index::select` and thrown away, which is why an index signature could not be
/// attributed to a pinned platform manifest at all.
pub(crate) fn verify_resolver<'a>(index: &'a Index) -> Box<VerifySubjectResolver<'a>> {
    Box::new(move |identifier, platform| {
        Box::pin(async move {
            let fetched = index
                .fetch_manifest(identifier, IndexOperation::Resolve)
                .await
                .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;
            let (target, index_members) = verify_target_from_resolution(fetched.as_ref(), platform)?;
            let subject = identifier.clone_with_digest(target.subject_digest.clone());
            let physical = physical_of(index, &subject)
                .await
                .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;
            Ok(ResolvedSubject {
                target,
                index_members,
                physical,
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use ocx_index::IndexImpl;
    use ocx_oci::{INDEX_SCHEMA_VERSION, ImageIndex, ImageIndexEntry, Platform, native};

    fn digest(byte: u8) -> Digest {
        Digest::Sha256(format!("{byte:064x}"))
    }

    fn identifier() -> PackageRef {
        PackageRef::parse("registry.example/acme/tool:1.0").expect("identifier")
    }

    fn platform(value: &str) -> Platform {
        value.parse::<Platform>().expect("test platform parses")
    }

    /// A child descriptor naming `os/arch`, the shape a real image index holds.
    fn child(os: native::Os, architecture: native::Arch, byte: u8) -> ImageIndexEntry {
        ImageIndexEntry {
            digest: digest(byte).to_string(),
            media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            size: 1,
            platform: Some(native::Platform {
                os,
                architecture,
                variant: None,
                features: None,
                os_version: None,
                os_features: None,
            }),
            artifact_type: None,
            annotations: None,
        }
    }

    /// The two-child index the supplied resolution carries.
    fn image_index_manifest() -> Manifest {
        Manifest::ImageIndex(ImageIndex {
            schema_version: INDEX_SCHEMA_VERSION,
            media_type: Some(ocx_oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
            artifact_type: None,
            manifests: vec![
                child(native::Os::Linux, native::Arch::Amd64, 0x01),
                child(native::Os::Linux, native::Arch::ARM64, 0x02),
            ],
            annotations: None,
        })
    }

    /// An index whose resolve finds nothing at all — so a run that fell through
    /// to it answers `TargetNotFound` instead of the supplied subject.
    #[derive(Clone)]
    struct EmptyIndex;

    #[async_trait::async_trait]
    impl IndexImpl for EmptyIndex {
        async fn list_repositories(&self, _: &str) -> ocx_index::error::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _: &PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            Ok(None)
        }

        async fn fetch_manifest(
            &self,
            _: &PackageRef,
            _: IndexOperation,
        ) -> ocx_index::error::Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }

        async fn fetch_manifest_digest(
            &self,
            _: &PackageRef,
            _: IndexOperation,
        ) -> ocx_index::error::Result<Option<Digest>> {
            Ok(None)
        }

        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> ocx_index::error::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// **The `pre_resolved` argument's own contract (#373).** A resolution the
    /// caller supplies is the one acted on, and the index is never asked.
    ///
    /// The sweep tests in `crate::tasks` prove how many resolutions a
    /// run performs and which reference each was for; neither can see *which
    /// answer* the rule then applied. `EmptyIndex` is what makes that visible:
    /// it resolves to nothing, so a run that fell through to it would answer
    /// `TargetNotFound` instead of the supplied subject. Both arms of the
    /// narrowing rule are driven, because the `Some` path has to reach the
    /// supplied manifest's children too — an implementation that used the
    /// supplied digest but re-fetched the children would pass a digest-only
    /// assertion.
    #[tokio::test]
    async fn a_supplied_resolution_is_acted_on_and_the_index_is_never_asked() {
        let index = Index::from_impl(EmptyIndex);
        let supplied = (digest(0xaa), image_index_manifest());
        let identifier = identifier();

        let whole = sign_resolver(&index, Some(&supplied))(&identifier, None)
            .await
            .expect("a supplied resolution needs no fetch");
        assert_eq!(whole.target.subject_digest, digest(0xaa));
        assert_eq!(whole.target.enclosing_index, None, "nothing was narrowed into");

        let arm64 = platform("linux/arm64");
        let narrowed = sign_resolver(&index, Some(&supplied))(&identifier, Some(&arm64))
            .await
            .expect("arm64 is listed in the supplied index");
        assert_eq!(
            narrowed.target.subject_digest,
            digest(0x02),
            "the child came from the supplied manifest"
        );
        assert_eq!(narrowed.target.enclosing_index, Some(digest(0xaa)));
    }

    /// Without a supplied resolution the index IS asked — the control that
    /// keeps the assertion above from passing on a resolver that ignores its
    /// arguments entirely.
    #[tokio::test]
    async fn no_supplied_resolution_falls_through_to_the_index() {
        let index = Index::from_impl(EmptyIndex);
        let identifier = identifier();
        let error = sign_resolver(&index, None)(&identifier, None)
            .await
            .expect_err("EmptyIndex resolves to nothing");
        assert!(
            matches!(&error, SignErrorKind::TargetNotFound { platform } if platform == "any"),
            "expected TargetNotFound from the index, got {error:?}"
        );
    }
}
