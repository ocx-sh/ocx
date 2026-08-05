// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The build receipt `ocx package create --metadata` writes beside the bundle,
//! and the platform resolution `ocx package push` / `ocx package test` derive
//! from it.
//!
//! The receipt is a **build artifact**, not package metadata: it records the
//! platform `create` resolved dependency pins and scanned binaries against, so
//! the two commands that consume a freshly built bundle default to the same
//! target instead of guessing one. It never travels to a registry, has no
//! published JSON Schema, and nothing on the install path reads it.
//!
//! Resolution is the pure [`resolve_target_platform`] table:
//!
//! | receipt | `--platform` | outcome |
//! |---------|--------------|---------|
//! | recorded | absent | the recorded platform, silently |
//! | recorded | equal | the recorded platform, silently |
//! | recorded | different | the explicit value, with a warning naming both |
//! | absent | given | the explicit value, with a notice that nothing cross-checked it |
//! | absent | absent | [`UsageError`] (64) — nothing determines the target |

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use anyhow::Context as _;
use ocx_lib::cli::{UsageError, UserInterface};
use ocx_lib::oci;

/// Known versions of the build-receipt format.
///
/// `serde_repr` rejects an unknown number at deserialize, so a receipt written
/// by a newer ocx fails loudly instead of being read as if it were V1.
/// No `Default`: a defaulted version would let a future `#[serde(default)]`
/// read a version-less receipt as V1 instead of rejecting it.
#[derive(Debug, Clone, Copy, Serialize_repr, Deserialize_repr, PartialEq, Eq)]
#[repr(u8)]
pub enum ReceiptVersion {
    V1 = 1,
}

/// What `ocx package create --metadata` recorded about the build.
///
/// Deliberately carries no `schemars::JsonSchema` derive: the receipt is a
/// local handoff between two commands in one build, not a format publishers
/// author or registries serve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildReceipt {
    /// The version of the build-receipt format.
    pub version: ReceiptVersion,

    /// The platform `ocx package create` resolved this bundle against, in the
    /// canonical grammar (`linux/amd64`, `linux/amd64+libc.glibc`, `any`, ...).
    #[serde(with = "platform_field")]
    pub platform: oci::Platform,
}

impl BuildReceipt {
    /// A receipt recording `platform` in the current format version.
    pub fn new(platform: oci::Platform) -> Self {
        Self {
            version: ReceiptVersion::V1,
            platform,
        }
    }
}

/// Reads the build receipt at `path`.
///
/// An absent file is `Ok(None)` — that is the ordinary "built without
/// `--metadata`, or handed a bundle from elsewhere" case. Every other failure
/// propagates: a receipt that exists but cannot be read or parsed must never
/// degrade into "there is no receipt", which would silently swap a checked
/// platform for an unchecked one.
///
/// # Errors
///
/// I/O failures other than not-found (74), and a malformed or unknown-version
/// receipt (65).
pub async fn read(path: &Path) -> anyhow::Result<Option<BuildReceipt>> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ocx_lib::error::file_error(path, error).into()),
    };
    let receipt: BuildReceipt = serde_json::from_slice(&bytes)
        .map_err(ocx_lib::Error::from)
        .with_context(|| format!("reading the build receipt {}", path.display()))?;
    Ok(Some(receipt))
}

/// Resolves the platform `ocx package push` / `ocx package test` operate on.
///
/// `recorded` is the receipt's platform (`None` when no receipt was found),
/// `explicit` the `--platform` flag, and `receipt_path` the file that was
/// looked for — used only to make the no-receipt notice name it.
///
/// Returns the resolved platform plus the advisory the caller should emit, if
/// any. Both callers route the advisory through [`PlatformAdvisory::emit`], so
/// wording and severity cannot drift between them.
///
/// # Errors
///
/// [`UsageError`] (64) when there is neither a receipt nor an explicit
/// `--platform`: nothing determines which OCI slot the bundle belongs in, and
/// guessing the host's platform would mislabel every cross-built artifact.
pub fn resolve_target_platform(
    recorded: Option<oci::Platform>,
    explicit: Option<oci::Platform>,
    receipt_path: Option<&Path>,
) -> Result<(oci::Platform, Option<PlatformAdvisory>), UsageError> {
    match (recorded, explicit) {
        (Some(recorded), None) => Ok((recorded, None)),
        (Some(recorded), Some(explicit)) if recorded == explicit => Ok((recorded, None)),
        (Some(recorded), Some(explicit)) => {
            let advisory = PlatformAdvisory::ExplicitOverridesReceipt {
                recorded,
                explicit: explicit.clone(),
            };
            Ok((explicit, Some(advisory)))
        }
        (None, Some(explicit)) => {
            let advisory = PlatformAdvisory::NoReceipt {
                receipt: receipt_path.map(Path::to_path_buf),
            };
            Ok((explicit, Some(advisory)))
        }
        (None, None) => Err(UsageError::new(
            "--platform is required when there is no build receipt beside the bundle; \
             run `ocx package create --metadata <FILE> --platform <PLATFORM>` to write one, \
             or pass --platform explicitly",
        )),
    }
}

/// Something the user should know about how the target platform was decided.
///
/// The wording lives here rather than at each call site because `ocx package
/// push` and `ocx package test` answer the same question and must say the same
/// thing about it.
#[derive(Debug)]
pub enum PlatformAdvisory {
    /// An explicit `--platform` disagreed with the receipt and won.
    ExplicitOverridesReceipt {
        recorded: oci::Platform,
        explicit: oci::Platform,
    },
    /// No receipt was found, so the explicit `--platform` was taken on trust.
    NoReceipt { receipt: Option<PathBuf> },
}

impl PlatformAdvisory {
    /// Emits the advisory on the diagnostic channel at its own severity.
    ///
    /// An override is a warning (the publisher is contradicting what the build
    /// actually resolved); an absent receipt is a notice (nothing is wrong,
    /// there was simply nothing to cross-check against).
    pub fn emit(&self, ui: &UserInterface) {
        match self {
            PlatformAdvisory::ExplicitOverridesReceipt { .. } => ui.warn(self),
            PlatformAdvisory::NoReceipt { .. } => ui.status("note", self),
        }
    }
}

impl std::fmt::Display for PlatformAdvisory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlatformAdvisory::ExplicitOverridesReceipt { recorded, explicit } => write!(
                f,
                "--platform '{explicit}' overrides the build receipt, which records '{recorded}' \
                 as the platform `ocx package create` resolved this bundle against"
            ),
            PlatformAdvisory::NoReceipt { receipt: Some(path) } => write!(
                f,
                "no build receipt beside the bundle (expected '{}'); \
                 the platform was not validated against the build",
                path.display()
            ),
            PlatformAdvisory::NoReceipt { receipt: None } => {
                f.write_str("no build receipt beside the bundle; the platform was not validated against the build")
            }
        }
    }
}

/// Serializes [`BuildReceipt::platform`] as its canonical grammar string — the
/// same encoding used for `ocx.lock` keys and `--platform` — rather than
/// [`oci::Platform`]'s own `Serialize`, which goes through the OCI JSON object
/// shape (`{"os":...,"architecture":...}`).
mod platform_field {
    use std::str::FromStr;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use ocx_lib::oci::Platform;

    pub fn serialize<S: Serializer>(value: &Platform, serializer: S) -> Result<S::Ok, S::Error> {
        value.to_string().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Platform, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Platform::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Aliased: `crate::app::classify_error` takes an `anyhow` chain, this one
    // takes a `&dyn Error`. Two names, no guessing which is in scope.
    use ocx_lib::cli::{ExitCode, classify_error as classify_typed_error};

    fn platform(value: &str) -> oci::Platform {
        value.parse().expect("platform parses")
    }

    fn resolve(
        recorded: Option<&str>,
        explicit: Option<&str>,
    ) -> Result<(oci::Platform, Option<PlatformAdvisory>), UsageError> {
        resolve_target_platform(
            recorded.map(platform),
            explicit.map(platform),
            Some(Path::new("/build/pkg-receipt.json")),
        )
    }

    // ── the resolution table, one test per row ────────────────────────────

    #[test]
    fn receipt_alone_supplies_the_platform_silently() {
        let (resolved, advisory) = resolve(Some("linux/amd64"), None).expect("row 1 resolves");
        assert_eq!(resolved.to_string(), "linux/amd64");
        assert!(advisory.is_none(), "an unopposed receipt says nothing");
    }

    #[test]
    fn an_agreeing_explicit_platform_is_silent() {
        let (resolved, advisory) = resolve(Some("linux/amd64"), Some("linux/amd64")).expect("row 2 resolves");
        assert_eq!(resolved.to_string(), "linux/amd64");
        assert!(
            advisory.is_none(),
            "restating what the receipt already says is not worth a diagnostic"
        );
    }

    #[test]
    fn a_disagreeing_explicit_platform_wins_and_warns_naming_both() {
        let (resolved, advisory) = resolve(Some("linux/amd64"), Some("darwin/arm64")).expect("row 3 resolves");
        assert_eq!(resolved.to_string(), "darwin/arm64", "the explicit value wins");
        let message = advisory.expect("an override must be advised").to_string();
        assert!(
            message.contains("linux/amd64") && message.contains("darwin/arm64"),
            "the warning must name both platforms: {message}"
        );
    }

    #[test]
    fn no_receipt_takes_the_explicit_platform_and_says_it_was_unchecked() {
        let (resolved, advisory) = resolve(None, Some("linux/amd64")).expect("row 4 resolves");
        assert_eq!(resolved.to_string(), "linux/amd64");
        let message = advisory.expect("an unvalidated platform must be advised").to_string();
        assert!(
            message.contains("pkg-receipt.json") && message.contains("not validated"),
            "the notice must name the absent receipt: {message}"
        );
    }

    #[test]
    fn no_receipt_and_no_platform_is_a_usage_error() {
        let error = resolve(None, None).expect_err("row 5 must be rejected");
        assert!(
            error.to_string().contains("--platform"),
            "the rejection must name the flag: {error}"
        );
        assert_eq!(
            classify_typed_error(&error as &(dyn std::error::Error + 'static)),
            ExitCode::UsageError
        );
    }

    #[test]
    fn the_no_receipt_notice_survives_an_underivable_receipt_path() {
        // Config-only pushes have no file layer, so there is no path to name.
        let (_, advisory) =
            resolve_target_platform(None, Some(platform("any")), None).expect("row 4 without a path resolves");
        let message = advisory.expect("still advised").to_string();
        assert!(message.contains("no build receipt"), "unexpected: {message}");
    }

    // ── wire format ──────────────────────────────────────────────────────

    #[test]
    fn a_feature_bearing_platform_round_trips_through_the_canonical_grammar() {
        let receipt = BuildReceipt::new(platform("linux/amd64+libc.glibc"));
        let json = serde_json::to_string(&receipt).expect("receipt serializes");
        assert_eq!(
            json, r#"{"version":1,"platform":"linux/amd64+libc.glibc"}"#,
            "the receipt must carry the canonical grammar string, not the OCI object shape"
        );

        let parsed: BuildReceipt = serde_json::from_str(&json).expect("receipt parses");
        assert_eq!(parsed.platform, receipt.platform);
        assert_eq!(parsed.version, ReceiptVersion::V1);
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

    // ── read ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn an_absent_receipt_reads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("pkg-receipt.json");
        assert!(read(&missing).await.expect("absent is not an error").is_none());
    }

    #[tokio::test]
    async fn a_present_receipt_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pkg-receipt.json");
        tokio::fs::write(&path, r#"{"version":1,"platform":"darwin/arm64"}"#)
            .await
            .expect("write receipt");

        let receipt = read(&path).await.expect("reads").expect("present");
        assert_eq!(receipt.platform.to_string(), "darwin/arm64");
    }

    #[tokio::test]
    async fn a_corrupt_receipt_propagates_instead_of_reading_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pkg-receipt.json");
        tokio::fs::write(&path, "{not json").await.expect("write receipt");

        let error = read(&path)
            .await
            .expect_err("a receipt that exists but cannot be parsed must never degrade to `no receipt`");
        assert_eq!(crate::app::classify_error(error.as_ref()), ExitCode::DataError);
    }
}
