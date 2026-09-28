// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Attaching a referrer, on a registry that indexes referrers and on one that does not.

use crate::sign::error::SignErrorKind;
use crate::sign::state::SigningStatePaths;
use ocx_oci::client::OciTransport;
use ocx_oci::client::error::ClientError;
use ocx_oci::referrer::capability::{ReferrersApiCapability, ReferrersSupport};
use ocx_oci::{Descriptor, Digest, OCI_IMAGE_MEDIA_TYPE, native};

/// The Referrers-API verdict for the host a referrer is about to be pushed to, via the capability cache.
///
/// `image` must be the write reference, or a mirror's verdict is cached against the upstream.
pub(crate) async fn referrers_capability(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject_digest: &Digest,
    state: &SigningStatePaths,
    no_cache: bool,
) -> Result<ReferrersSupport, SignErrorKind> {
    let cached = if no_cache {
        None
    } else {
        ReferrersApiCapability::from_cache(
            image.resolve_registry(),
            &state.referrers_capability_file(image.resolve_registry()),
        )
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
            let _ = probed
                .write_cache(&state.referrers_capability_file(&probed.registry))
                .await;
            probed
        }
    };
    Ok(capability.supported)
}

/// Push a referrer manifest and, on a registry without the Referrers API, name it in
/// the OCI tag-schema fallback index too (`adr_oci_referrers_signing_v1.md § Amendment 10`).
///
/// `write_image` must come from `Client::transport_write_reference`, or the fallback index lands on a mirror.
///
/// # Errors
///
/// The manifest PUT's error, or on an `Unsupported` registry the append's.
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

    // Skipped when supported: the mutable fallback tag would add an attacker-writable second source of truth.
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
/// Everything but `ReferrersUnsupported` stays intact under `Internal`, so the cause supplies its own exit code.
pub fn map_client_error(error: ClientError) -> SignErrorKind {
    match error {
        ClientError::ReferrersUnsupported { .. } => SignErrorKind::ReferrersUnsupported,
        other => SignErrorKind::Internal(Box::new(other)),
    }
}
