// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Attaching a referrer, on a registry that indexes referrers and on one that
//! does not.
//!
//! The sign and attest pipelines carried byte-identical copies of a
//! capability gate and a referrer write. They are one thing, so they live here
//! once: a registry's Referrers-API verdict decides whether the manifest PUT is
//! enough, or whether the OCI tag-schema fallback index also has to name it.
//!
//! Both pipelines used to *refuse* an `Unsupported` verdict with exit 84. That
//! gate is gone — see [`attach_referrer`] and
//! `adr_oci_referrers_signing_v1.md` Amendment 10.

use crate::file_structure::StateStore;
use crate::oci::client::OciTransport;
use crate::oci::client::error::ClientError;
use crate::oci::referrer::capability::{ReferrersApiCapability, ReferrersSupport};
use crate::oci::sign::error::SignErrorKind;
use crate::oci::{Descriptor, Digest, OCI_IMAGE_MEDIA_TYPE, native};

/// The Referrers-API verdict for the host a referrer is about to be pushed to,
/// consulting (and refreshing) the per-registry capability cache.
///
/// Returns the verdict rather than refusing on it: the caller decides what an
/// `Unsupported` registry costs, and since Amendment 10 the answer is "a second
/// write", not "a failure".
///
/// `image` must be the **write** reference. The cache key is the host actually
/// probed, so a mirrored registry would otherwise cache the mirror's verdict
/// against the upstream the referrer lands on.
pub(crate) async fn referrers_capability(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject_digest: &Digest,
    state: &StateStore,
    no_cache: bool,
) -> Result<ReferrersSupport, SignErrorKind> {
    let cached = if no_cache {
        None
    } else {
        ReferrersApiCapability::from_cache(image.resolve_registry(), state)
            .await
            .ok()
            .flatten()
            .filter(ReferrersApiCapability::is_fresh)
    };
    let capability = match cached {
        Some(hit) => hit,
        None => {
            let probed = ReferrersApiCapability::probe(transport, image, subject_digest)
                .await
                .map_err(map_client_error)?;
            // Best-effort cache write; a failure here must not fail the sign.
            let _ = probed.write_cache(state).await;
            probed
        }
    };
    Ok(capability.supported)
}

/// Push a referrer manifest and, on a registry without the Referrers API, name
/// it in the OCI tag-schema fallback index as well.
///
/// The Unsupported verdict no longer refuses the operation: the OCI referrers
/// tag-schema fallback (`list_referrers_with_fallback` /
/// `append_referrer_fallback_index`) serves a registry without the Referrers
/// API. See `adr_oci_referrers_signing_v1.md`, Amendment 10 — the fallback
/// index is a mutable tag anyone with push access authors, and the residual
/// attack surface that reverses S1-F is recorded there.
///
/// `write_image` must come from `Client::transport_write_reference`.
/// [`super::super::client::sibling_tag_reference`] propagates whatever host it
/// is handed, so a mirrored read reference here would PUT the fallback index to
/// the mirror — deciding on one host while writing to another, the CWE-345/367
/// class `oci/client.rs` names at its addressing seams.
///
/// The append is skipped, not merely unnecessary, when the API is supported: a
/// registry that serves `/v2/<name>/referrers/<digest>` computes the listing
/// itself, and writing the mutable tag beside it would add an attacker-writable
/// second source of truth for a subject that already has an authoritative one.
///
/// # Errors
///
/// Whatever the manifest PUT raises, and — only on an `Unsupported` registry —
/// whatever the append raises. Exit 84 now means "the Referrers API is absent
/// **and** the fallback write was refused" (spec D3): the append's own
/// `ReferrersUnsupported` is the only path left to it on the write side.
pub(crate) async fn attach_referrer(
    transport: &dyn OciTransport,
    write_image: &native::Reference,
    subject_digest: &Digest,
    manifest_bytes: &[u8],
    support: ReferrersSupport,
) -> Result<(Digest, Descriptor), SignErrorKind> {
    let referrer_descriptor = transport
        .push_referrer_manifest(write_image, subject_digest, manifest_bytes, OCI_IMAGE_MEDIA_TYPE)
        .await
        .map_err(map_client_error)?;

    if support == ReferrersSupport::Unsupported {
        transport
            .append_referrer_fallback_index(write_image, subject_digest, &referrer_descriptor)
            .await
            .map_err(map_client_error)?;
    }

    let referrer_digest =
        Digest::try_from(referrer_descriptor.digest.as_str()).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
    Ok((referrer_digest, referrer_descriptor))
}

/// Map an OCI client error into the sign taxonomy.
///
/// Only `ReferrersUnsupported` has a faithful sign-side kind. Everything else
/// keeps its `ClientError` intact under `Internal` rather than being flattened
/// into a kind that would misdescribe it — `SignError::classify` defers on
/// `Internal`, so the wrapped cause supplies its own exit code (401 → 80,
/// 5xx → 69, transient → 75).
pub(crate) fn map_client_error(error: ClientError) -> SignErrorKind {
    match error {
        ClientError::ReferrersUnsupported { .. } => SignErrorKind::ReferrersUnsupported,
        other => SignErrorKind::Internal(Box::new(other)),
    }
}
