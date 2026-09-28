// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The build receipt `ocx package create` writes beside the bundle (platform and identifier), so
//! `push`/`test` need not restate them. A build artifact: never published, no JSON Schema.
//!
//! A fallback per value, never an authority: a given flag wins; an absent flag takes the recorded
//! value, or is a [`UsageError`] (64) when nothing records it.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::error::UsageError;
use anyhow::Context as _;
use ocx_oci::layer_ref::LayerRef;

/// Known versions of the build-receipt format; an unknown number fails to deserialize.
///
/// No `Default`: it would let a `#[serde(default)]` read a version-less receipt as V1.
#[derive(Debug, Clone, Copy, Serialize_repr, Deserialize_repr, PartialEq, Eq)]
#[repr(u8)]
pub enum ReceiptVersion {
    V1 = 1,
}

/// What `ocx package create` recorded; each half is absent when `create` was not given it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildReceipt {
    pub version: ReceiptVersion,

    /// `create --platform`, in the canonical grammar (`linux/amd64+libc.glibc`, `any`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "platform_field")]
    pub platform: Option<ocx_oci::Platform>,

    /// `create --identifier`, resolved against the default registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<ocx_oci::PackageRef>,
}

impl BuildReceipt {
    /// A current-version receipt; `None` when `create` knew neither half.
    pub fn new(platform: Option<ocx_oci::Platform>, identifier: Option<ocx_oci::PackageRef>) -> Option<Self> {
        (platform.is_some() || identifier.is_some()).then_some(Self {
            version: ReceiptVersion::V1,
            platform,
            identifier,
        })
    }
}

/// Reads the build receipt beside the bundle. Call it only once a flag is missing, so an
/// invocation that states everything cannot fail on a file it does not need.
///
/// An absent file or no anchoring layer is `Ok(None)`; every other failure propagates, or a
/// recorded value would degrade into a usage error about a flag the publisher had no reason to pass.
///
/// # Errors
///
/// I/O failures other than not-found (74), and a malformed or unknown-version
/// receipt (65).
pub async fn read_beside_bundle(layers: &[LayerRef]) -> anyhow::Result<Option<BuildReceipt>> {
    match crate::conventions::resolve_receipt_path(layers) {
        Some(path) => read(&path).await,
        None => Ok(None),
    }
}

/// Reads the receipt at `path`: `Ok(None)` when absent, a `DataError`
/// (65) when the file is not a receipt this build can read.
pub async fn read(path: &Path) -> anyhow::Result<Option<BuildReceipt>> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ocx_util::error::FileError::new(path, error).into()),
    };
    let receipt: BuildReceipt = serde_json::from_slice(&bytes)
        .map_err(ocx_util::error::SerializationError::from)
        .with_context(|| format!("reading the build receipt {}", path.display()))?;
    Ok(Some(receipt))
}

/// Resolves the platform `push`/`test` operate on; `explicit` (`--platform`) wins without being
/// compared against the receipt.
///
/// # Errors
///
/// [`UsageError`] (64) when neither names a platform; guessing the host's would mislabel every
/// cross-built artifact.
pub fn resolve_target_platform(
    explicit: Option<ocx_oci::Platform>,
    receipt: Option<&BuildReceipt>,
) -> Result<ocx_oci::Platform, UsageError> {
    if let Some(explicit) = explicit {
        return Ok(explicit);
    }
    match receipt.and_then(|receipt| receipt.platform.clone()) {
        Some(recorded) => {
            log::debug!("using the platform {recorded} recorded in the build receipt");
            Ok(recorded)
        }
        None => Err(UsageError::new(
            "--platform is required: the build receipt beside the bundle records no platform; \
             pass --platform, or run `ocx package create --platform <PLATFORM>` so the build \
             records one",
        )),
    }
}

/// Resolves the identifier `push` publishes under, on [`resolve_target_platform`]'s contract.
///
/// A tagless `-i repo` takes the receipt's tag only when the receipt names the same registry and
/// repository; otherwise push's `latest` default applies.
///
/// # Errors
///
/// [`UsageError`] (64) when neither `--identifier` nor the receipt names one.
pub fn resolve_target_identifier(
    explicit: Option<ocx_oci::PackageRef>,
    receipt: Option<&BuildReceipt>,
) -> Result<ocx_oci::PackageRef, UsageError> {
    if let Some(explicit) = explicit {
        // Same repository only, or a receipt about another repository picks this one's version.
        if explicit.tag().is_none()
            && explicit.digest().is_none()
            && let Some(recorded) = receipt.and_then(|receipt| receipt.identifier.as_ref())
            && recorded.registry() == explicit.registry()
            && recorded.repository() == explicit.repository()
            && let Some(tag) = recorded.tag()
        {
            log::debug!("using the tag {tag} recorded in the build receipt");
            return Ok(explicit.clone_with_tag(tag));
        }
        return Ok(explicit);
    }
    match receipt.and_then(|receipt| receipt.identifier.clone()) {
        Some(recorded) => {
            log::debug!("using the identifier {recorded} recorded in the build receipt");
            Ok(recorded)
        }
        None => Err(UsageError::new(
            "--identifier (-i) is required: the build receipt beside the bundle records no \
             identifier; pass --identifier, or run `ocx package create --identifier <IDENTIFIER>` \
             so the build records one",
        )),
    }
}

/// [`BuildReceipt::platform`] as its canonical grammar string (as `ocx.lock` keys), not
/// [`ocx_oci::Platform`]'s OCI JSON object shape.
mod platform_field {
    use std::str::FromStr;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use ocx_oci::Platform;

    pub fn serialize<S: Serializer>(value: &Option<Platform>, serializer: S) -> Result<S::Ok, S::Error> {
        value.as_ref().map(Platform::to_string).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Platform>, D::Error> {
        let raw = Option::<String>::deserialize(deserializer)?;
        raw.map(|raw| Platform::from_str(&raw))
            .transpose()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Aliased: `crate::exit::classify_error` takes an `anyhow` chain, this one
    // takes a `&dyn Error`. Two names, no guessing which is in scope.
    use crate::exit::classify_library_error as classify_typed_error;
    use ocx_exit::ExitCode;

    fn platform(value: &str) -> ocx_oci::Platform {
        value.parse().expect("platform parses")
    }

    fn identifier(value: &str) -> ocx_oci::PackageRef {
        value.parse().expect("identifier parses")
    }

    fn receipt(platform_value: Option<&str>, identifier_value: Option<&str>) -> BuildReceipt {
        BuildReceipt {
            version: ReceiptVersion::V1,
            platform: platform_value.map(platform),
            identifier: identifier_value.map(identifier),
        }
    }

    // ── the resolution table: the flag wins, the receipt fills a gap ───────
    //
    // Both resolvers return the value alone. There is no advisory channel to
    // assert on, which IS the contract: an explicit flag is answered in
    // silence, and a receipt that disagrees with it is never even compared.

    #[test]
    fn an_explicit_platform_wins_over_a_disagreeing_receipt() {
        let recorded = receipt(Some("linux/amd64"), None);
        let resolved =
            resolve_target_platform(Some(platform("darwin/arm64")), Some(&recorded)).expect("the flag resolves");
        assert_eq!(resolved.to_string(), "darwin/arm64");
    }

    #[test]
    fn the_receipt_supplies_the_platform_the_flag_omitted() {
        let recorded = receipt(Some("linux/amd64"), None);
        let resolved = resolve_target_platform(None, Some(&recorded)).expect("the receipt resolves");
        assert_eq!(resolved.to_string(), "linux/amd64");
    }

    #[test]
    fn no_platform_flag_and_nothing_recorded_is_a_usage_error() {
        let platform_less = receipt(None, Some("ocx.sh/ocx/cli:1.2.3"));
        for recorded in [None, Some(&platform_less)] {
            let error = resolve_target_platform(None, recorded).expect_err("nothing determines the platform");
            assert!(
                error.to_string().contains("--platform"),
                "the rejection must name the flag: {error}"
            );
            assert_eq!(
                classify_typed_error(&error as &(dyn std::error::Error + 'static)),
                ExitCode::UsageError
            );
        }
    }

    #[test]
    fn an_explicit_identifier_wins_over_a_disagreeing_receipt() {
        let recorded = receipt(None, Some("ocx.sh/ocx/cli:1.2.3"));
        let resolved = resolve_target_identifier(Some(identifier("localhost:5000/app:2.0.0")), Some(&recorded))
            .expect("the flag resolves");
        assert_eq!(resolved.to_string(), "localhost:5000/app:2.0.0");
    }

    #[test]
    fn the_receipt_supplies_the_identifier_the_flag_omitted() {
        let recorded = receipt(None, Some("ocx.sh/ocx/cli:1.2.3"));
        let resolved = resolve_target_identifier(None, Some(&recorded)).expect("the receipt resolves");
        assert_eq!(resolved.to_string(), "ocx.sh/ocx/cli:1.2.3");
    }

    #[test]
    fn a_tagless_flag_takes_the_version_the_receipt_recorded_for_the_same_repository() {
        let recorded = receipt(None, Some("localhost:5000/app:2.0.0"));
        let resolved = resolve_target_identifier(Some(identifier("localhost:5000/app")), Some(&recorded))
            .expect("the tagless flag resolves");
        assert_eq!(
            resolved.to_string(),
            "localhost:5000/app:2.0.0",
            "the repository comes from the flag, the version from the receipt"
        );
    }

    #[test]
    fn a_tagless_flag_for_a_different_repository_takes_nothing_from_the_receipt() {
        let recorded = receipt(None, Some("localhost:5000/app:2.0.0"));
        let resolved = resolve_target_identifier(Some(identifier("localhost:5000/other")), Some(&recorded))
            .expect("the tagless flag resolves");
        assert_eq!(
            resolved.to_string(),
            "localhost:5000/other",
            "a receipt about a different repository must not supply this push's version"
        );
        assert!(resolved.tag().is_none());
    }

    #[test]
    fn a_tagged_flag_keeps_its_own_version_over_the_receipt() {
        let recorded = receipt(None, Some("localhost:5000/app:2.0.0"));
        let resolved = resolve_target_identifier(Some(identifier("localhost:5000/app:1.0.0")), Some(&recorded))
            .expect("the tagged flag resolves");
        assert_eq!(resolved.to_string(), "localhost:5000/app:1.0.0");
    }

    #[test]
    fn no_identifier_flag_and_nothing_recorded_is_a_usage_error() {
        let identifier_less = receipt(Some("linux/amd64"), None);
        for recorded in [None, Some(&identifier_less)] {
            let error = resolve_target_identifier(None, recorded).expect_err("nothing determines the identifier");
            assert!(
                error.to_string().contains("--identifier"),
                "the rejection must name the flag: {error}"
            );
            assert_eq!(
                classify_typed_error(&error as &(dyn std::error::Error + 'static)),
                ExitCode::UsageError
            );
        }
    }

    // ── wire format ──────────────────────────────────────────────────────

    #[test]
    fn both_fields_round_trip_through_their_canonical_strings() {
        let value = receipt(Some("linux/amd64+libc.glibc"), Some("ocx.sh/ocx/cli:1.2.3"));
        let json = serde_json::to_string(&value).expect("receipt serializes");
        assert_eq!(
            json, r#"{"version":1,"platform":"linux/amd64+libc.glibc","identifier":"ocx.sh/ocx/cli:1.2.3"}"#,
            "the receipt must carry the canonical grammar strings, not the OCI object shapes"
        );

        let parsed: BuildReceipt = serde_json::from_str(&json).expect("receipt parses");
        assert_eq!(parsed.platform, value.platform);
        assert_eq!(parsed.identifier, value.identifier);
        assert_eq!(parsed.version, ReceiptVersion::V1);
    }

    #[test]
    fn an_unrecorded_field_is_absent_from_the_wire_and_reads_back_as_none() {
        let json = serde_json::to_string(&receipt(Some("linux/amd64"), None)).expect("receipt serializes");
        assert_eq!(json, r#"{"version":1,"platform":"linux/amd64"}"#);

        let parsed: BuildReceipt = serde_json::from_str(r#"{"version":1}"#).expect("both fields are optional");
        assert!(parsed.platform.is_none() && parsed.identifier.is_none());
    }

    #[test]
    fn an_unknown_version_is_rejected() {
        let error = serde_json::from_str::<BuildReceipt>(r#"{"version":2,"platform":"linux/amd64"}"#)
            .expect_err("a receipt from a newer ocx must not be read as V1");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn a_non_canonical_platform_string_is_rejected() {
        assert!(
            serde_json::from_str::<BuildReceipt>(r#"{"version":1,"platform":"linux/amd64;osf=libc.glibc"}"#).is_err(),
            "only the canonical platform grammar parses"
        );
    }

    #[test]
    fn a_malformed_identifier_string_is_rejected() {
        assert!(
            serde_json::from_str::<BuildReceipt>(r#"{"version":1,"identifier":"NOT AN IDENTIFIER"}"#).is_err(),
            "only a canonical identifier parses"
        );
    }

    // ── read ─────────────────────────────────────────────────────────────

    fn layer(path: &std::path::Path) -> ocx_oci::layer_ref::LayerRef {
        path.to_string_lossy().parse().expect("layer ref parses")
    }

    #[tokio::test]
    async fn an_absent_receipt_reads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle = dir.path().join("pkg.tar.gz");
        assert!(
            read_beside_bundle(&[layer(&bundle)])
                .await
                .expect("absent is not an error")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_present_receipt_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle = dir.path().join("pkg.tar.gz");
        tokio::fs::write(
            dir.path().join("pkg-receipt.json"),
            r#"{"version":1,"platform":"darwin/arm64","identifier":"ocx.sh/ocx/cli:1.2.3"}"#,
        )
        .await
        .expect("write receipt");

        let receipt = read_beside_bundle(&[layer(&bundle)])
            .await
            .expect("reads")
            .expect("present");
        assert_eq!(receipt.platform.expect("platform").to_string(), "darwin/arm64");
        assert_eq!(
            receipt.identifier.expect("identifier").to_string(),
            "ocx.sh/ocx/cli:1.2.3"
        );
    }

    #[tokio::test]
    async fn a_corrupt_receipt_propagates_instead_of_reading_as_absent() {
        // Only reached when the receipt is actually needed: the call sites
        // skip this read entirely when the flags already supply everything,
        // so a corrupt receipt cannot fail a fully explicit invocation.
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle = dir.path().join("pkg.tar.gz");
        tokio::fs::write(dir.path().join("pkg-receipt.json"), "{not json")
            .await
            .expect("write receipt");

        let error = read_beside_bundle(&[layer(&bundle)])
            .await
            .expect_err("a receipt that exists but cannot be parsed must never degrade to `no receipt`");
        assert_eq!(crate::exit::classify_error(error.as_ref()), ExitCode::DataError);
    }
}
