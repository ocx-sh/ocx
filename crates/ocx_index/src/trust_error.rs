// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Why a signed index catalog was refused; each variant declares its exit code and `error.detail` slug.

use chrono::{DateTime, Utc};

/// A refusal under `index_signature = "enforce"`.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum IndexTrustError {
    /// The source serves no signature beside its catalog.
    #[error("{registry} serves no catalog signature")]
    #[exit(
        NotFound,
        slug = "index_signature_not_found",
        summary = "The index serves no signature for its catalog"
    )]
    SignatureNotFound { registry: String },

    /// The signature does not hold over the catalog bytes.
    #[error("{registry} serves a catalog signature that does not verify: {reason}")]
    #[exit(
        DataError,
        slug = "index_signature_invalid",
        summary = "The index catalog's signature does not verify"
    )]
    SignatureInvalid { registry: String, reason: String },

    /// The signature is genuine and its signer is not trusted for this registry.
    #[error("{registry} catalog is signed by a signer that index_publishers does not list")]
    #[exit(
        PermissionDenied,
        slug = "index_publisher_mismatch",
        summary = "The index catalog is signed by a signer that is not trusted"
    )]
    PublisherMismatch { registry: String },

    /// Every `index_publishers` entry is of a kind this build does not know.
    #[error("no index_publishers entry for {registry} is of a kind this ocx understands")]
    #[exit(
        Unsupported,
        slug = "index_publishers_unrecognized",
        summary = "No index publisher entry is of a kind this build understands"
    )]
    PublishersUnrecognized { registry: String },

    /// The catalog's validity window has ended, or it carries none.
    #[error("{registry} catalog is expired{}", expires.map(|at| format!(" since {}", at.to_rfc3339())).unwrap_or_default())]
    #[exit(
        PolicyBlocked,
        slug = "index_catalog_expired",
        summary = "The index catalog's validity window has ended"
    )]
    CatalogExpired {
        registry: String,
        expires: Option<DateTime<Utc>>,
    },

    /// A verified catalog expires earlier than one this machine verified before.
    #[error("{registry} catalog expires {served}, before the {recorded} this machine already verified")]
    #[exit(
        DataError,
        slug = "index_catalog_rollback",
        summary = "The index catalog is older than one already verified"
    )]
    CatalogRollback {
        registry: String,
        served: DateTime<Utc>,
        recorded: DateTime<Utc>,
    },

    /// A root document's digest differs from the one the verified catalog lists.
    #[error("{registry} root for {package} does not match the digest its signed catalog lists")]
    #[exit(
        DataError,
        slug = "index_root_digest_mismatch",
        summary = "A package root differs from the digest its signed catalog lists"
    )]
    RootDigestMismatch { registry: String, package: String },

    /// The verified catalog lists no entry for the package.
    #[error("{registry} signed catalog lists no entry for {package}")]
    #[exit(
        NotFound,
        slug = "index_package_not_in_catalog",
        summary = "The signed catalog lists no entry for the package"
    )]
    PackageNotInCatalog { registry: String, package: String },

    /// The catalog lists a root the server does not serve.
    #[error("{registry} signed catalog lists {package}, which the index does not serve")]
    #[exit(
        NotFound,
        slug = "index_root_not_found",
        summary = "A package the signed catalog lists is not served"
    )]
    RootNotFound { registry: String, package: String },
}
