// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The error document: what a failed invocation prints on stdout under `--format json`.
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "command": "package sign",
//!   "exit_code": 80,
//!   "error": {
//!     "kind": "auth_error",
//!     "detail": "oidc_token_rejected",
//!     "message": "Fulcio rejected OIDC token: issuer not in trust root",
//!     "context": { "identifier": "ocx.sh/cmake:3.28" }
//!   }
//! }
//! ```

use ocx_exit::ErrorCategory;
use ocx_oci::PackageRef;
use serde::Serialize;

/// Version of the error document's shape; bump only on a rename, removal or re-nesting.
///
/// New keys, new [`ErrorCategory`] variants and renames of a slug no release emitted do not bump.
/// The document stays at 1 until the contract baseline exists: the five `error.kind` values
/// removed and the new meaning of exit 82 (`adr_exit_code_taxonomy.md`) are announced through
/// the commit subject, not a bump.
pub const ERRORS_SCHEMA_VERSION: u32 = 1;

/// The document a failed invocation prints on stdout under `--format json`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ErrorDocument<'a> {
    /// Version of this document's shape.
    pub schema_version: u32,
    /// The command that failed, as its words (`"package sign"`); empty when the command line named none.
    pub command: &'a str,
    /// The process exit code.
    pub exit_code: u8,
    /// What failed.
    pub error: ErrorBody,
}

/// The `error` object of an error document.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ErrorBody {
    /// Coarse error category.
    // `ocx_exit` links no schemars; `ocx_schema` replaces this with its `ErrorCategory` definition.
    #[schemars(with = "String")]
    pub kind: ErrorCategory,
    /// The specific error within its category (e.g., `"oidc_token_rejected"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<&'static str>,
    /// Full user-facing message.
    pub message: String,
    /// The packages the failure is about; always emitted, possibly empty.
    pub context: ErrorContext,
}

/// The packages an error is about.
#[derive(Debug, Default, Serialize, schemars::JsonSchema)]
pub struct ErrorContext {
    /// The package a sign or verify failure is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<PackageRef>,
    /// The package a copy failure reads from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PackageRef>,
    /// The repository a copy failure writes to.
    // An `OciIdentifier` has no conversion to `PackageRef`; its display form is the same grammar.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<PackageRef>")]
    pub target: Option<String>,
}

/// Render an `anyhow::Error` as the pretty-printed error document printed under `--format json`.
///
/// Classified by [`crate::exit::classify_decision`], not the library pass, or a CLI-local
/// `CommandError` reports `1` while the process exits 64; `detail` is the deciding cause's slug.
///
/// # Errors
///
/// Only if `serde_json::to_string_pretty` fails.
pub fn render_error_document(command: &str, err: &anyhow::Error) -> anyhow::Result<String> {
    let err_ref: &(dyn std::error::Error + 'static) = err.as_ref();
    let (exit_code, detail) = crate::exit::classify_decision(err_ref);
    let document = ErrorDocument {
        schema_version: ERRORS_SCHEMA_VERSION,
        command,
        exit_code: exit_code as u8,
        error: ErrorBody {
            kind: exit_code.category(),
            detail,
            message: format!("{err:#}"),
            context: collect_context(err_ref),
        },
    };
    Ok(serde_json::to_string_pretty(&document)?)
}

/// Collect `context` from the first sign, verify or copy error in the chain.
fn collect_context(err: &(dyn std::error::Error + 'static)) -> ErrorContext {
    use ocx_package::publisher::CopyError;
    use ocx_sign::sign::SignError;
    use ocx_sign::verify::VerifyError;

    for cause in std::iter::successors(Some(err), |e| e.source()) {
        if let Some(sign_err) = cause.downcast_ref::<SignError>() {
            return ErrorContext {
                identifier: Some(sign_err.identifier.clone()),
                ..ErrorContext::default()
            };
        }
        if let Some(verify_err) = cause.downcast_ref::<VerifyError>() {
            return ErrorContext {
                identifier: Some(verify_err.identifier.clone()),
                ..ErrorContext::default()
            };
        }
        if let Some(copy_err) = cause.downcast_ref::<CopyError>() {
            return ErrorContext {
                identifier: None,
                source: Some(copy_err.source_identifier.clone()),
                target: Some(copy_err.target_identifier.to_string()),
            };
        }
    }
    ErrorContext::default()
}

#[cfg(test)]
mod tests {
    //! The error document is a published contract that `--format json` consumers match against.
    use super::*;
    use ocx_exit::ExitCode;

    #[test]
    fn schema_version_is_one() {
        assert_eq!(ERRORS_SCHEMA_VERSION, 1);
    }

    #[test]
    fn error_document_golden_shape() {
        let document = ErrorDocument {
            schema_version: ERRORS_SCHEMA_VERSION,
            command: "package sign",
            exit_code: 80,
            error: ErrorBody {
                kind: ErrorCategory::AuthError,
                detail: Some("oidc_token_rejected"),
                message: "Fulcio rejected OIDC token: issuer not in trust root".into(),
                context: ErrorContext {
                    identifier: Some(PackageRef::parse("ocx.sh/cmake:3.28").unwrap()),
                    ..ErrorContext::default()
                },
            },
        };
        let actual = serde_json::to_string(&document).unwrap();
        let expected = concat!(
            r#"{"schema_version":1,"command":"package sign","exit_code":80,"#,
            r#""error":{"kind":"auth_error","detail":"oidc_token_rejected","#,
            r#""message":"Fulcio rejected OIDC token: issuer not in trust root","#,
            r#""context":{"identifier":"ocx.sh/cmake:3.28"}}}"#,
        );
        assert_eq!(actual, expected);
    }

    /// A `--format json` copy failure carries its `detail` slug and both endpoints, rendered end to end
    /// through the collectors a hand-built document never calls.
    #[test]
    fn a_copy_refusal_carries_its_slug_and_both_endpoints() {
        use ocx_package::publisher::{CopyError, CopyErrorKind};

        let error = anyhow::Error::new(CopyError {
            source_identifier: "dev.example.com/acme/tool:1.4.2".parse().expect("source"),
            target_identifier: ocx_oci::OciIdentifier::parse_target(
                "prod.example.com/acme/tool:1.4.2",
                ocx_oci::DEFAULT_REGISTRY,
            )
            .expect("target"),
            kind: CopyErrorKind::IndexNamedByDigest,
        });
        let rendered = render_error_document("package copy", &error).expect("render");
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid json");

        assert_eq!(value["error"]["detail"], "index_named_by_digest", "{rendered}");
        assert_eq!(value["error"]["context"]["source"], "dev.example.com/acme/tool:1.4.2");
        assert_eq!(value["error"]["context"]["target"], "prod.example.com/acme/tool:1.4.2");
        assert_eq!(
            value["exit_code"], 64,
            "a structural refusal is a usage fault: {rendered}"
        );

        // Control: no `CopyError` on the chain, so the keys above are not emitted unconditionally.
        let unrelated = anyhow::anyhow!("dev.example.com/acme/tool:1.4.2 to prod.example.com/acme/tool:1.4.2");
        let control: serde_json::Value =
            serde_json::from_str(&render_error_document("package copy", &unrelated).expect("render")).expect("json");
        assert!(
            control["error"].get("detail").is_none(),
            "control leaked a detail: {control}"
        );
        assert_eq!(
            control["error"]["context"],
            serde_json::json!({}),
            "control leaked context"
        );
    }

    #[test]
    fn error_document_omits_absent_detail_and_context_keys() {
        let document = ErrorDocument {
            schema_version: ERRORS_SCHEMA_VERSION,
            command: "verify",
            exit_code: 79,
            error: ErrorBody {
                kind: ErrorCategory::NotFound,
                detail: None,
                message: "no signatures found for package".into(),
                context: ErrorContext::default(),
            },
        };
        let actual = serde_json::to_string(&document).unwrap();
        assert!(!actual.contains("\"detail\""), "detail should be skipped: {actual}");
        assert!(!actual.contains("\"remediation\""), "{actual}");
        assert!(
            actual.contains("\"context\":{}"),
            "empty context should be `{{}}`: {actual}"
        );
        assert!(actual.contains("\"kind\":\"not_found\""));
    }

    #[test]
    fn render_error_document_produces_the_shape_for_a_synthetic_error() {
        let err = anyhow::anyhow!("synthetic error for document probe");
        let json = render_error_document("package sign", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "package sign");
        assert_eq!(parsed["exit_code"], 1);
        assert_eq!(parsed["error"]["kind"], "internal");
        assert!(
            parsed["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("synthetic error")),
            "message missing from {json}",
        );
        assert!(
            parsed["error"]["context"].is_object(),
            "context must always be an object"
        );
    }

    /// The bytes `--format json` prints, indented like every success report.
    #[test]
    fn render_error_document_is_pretty_printed() {
        let err = anyhow::anyhow!("boom");
        let rendered = render_error_document("", &err).expect("render ok");
        let expected = concat!(
            "{\n",
            "  \"schema_version\": 1,\n",
            "  \"command\": \"\",\n",
            "  \"exit_code\": 1,\n",
            "  \"error\": {\n",
            "    \"kind\": \"internal\",\n",
            "    \"message\": \"boom\",\n",
            "    \"context\": {}\n",
            "  }\n",
            "}",
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn render_error_document_classifies_verify_not_found() {
        // A `VerifyError(NoSignaturesFound)` surfaces as `kind=not_found`,
        // exit 79 — matches the frozen contract test in `test_verify.py`.
        let id = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").unwrap();
        let inner = ocx_sign::verify::VerifyError::new(id, ocx_sign::verify::VerifyErrorKind::NoSignaturesFound);
        let err = anyhow::Error::from(inner);
        let json = render_error_document("verify", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["command"], "verify");
        assert_eq!(parsed["exit_code"], 79);
        assert_eq!(parsed["error"]["kind"], "not_found");
        // Identifier surfaces in context from the SignError/VerifyError chain walk.
        assert_eq!(parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0");
    }

    #[test]
    fn render_error_document_classifies_sign_auth_error() {
        let id = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").unwrap();
        let inner = ocx_sign::sign::SignError::new(id, ocx_sign::sign::SignErrorKind::OidcTokenRejected);
        let err = anyhow::Error::from(inner);
        let json = render_error_document("package sign", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["command"], "package sign");
        assert_eq!(parsed["exit_code"], 80);
        assert_eq!(parsed["error"]["kind"], "auth_error");
        assert_eq!(parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0");
    }

    #[test]
    fn render_error_document_classifies_sign_unsupported_key_backend() {
        // The last link of the exit-82 chain, which the library-side test
        // cannot reach: `render_error_document` is what a `--format json`
        // consumer actually reads, and it derives `error.kind` through
        // `classify_error` -> `ExitCode::category`.
        //
        // Both assertions are load-bearing, and the second is the one that
        // discriminates: map the 82 arm to `ErrorCategory::Internal` and the
        // document still says `"exit_code": 82` while `error.kind` silently
        // becomes `"internal"`. A code-only assertion passes through exactly
        // the failure the dedicated category exists to prevent.
        let id = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").unwrap();
        let rejected = ocx_trust::key_ref::KeyRef::parse("awskms://alias/release")
            .expect_err("awskms is recognised but unimplemented");
        let inner = ocx_sign::sign::SignError::new(id, ocx_sign::sign::SignErrorKind::from(rejected));
        let err = anyhow::Error::from(inner);
        let json = render_error_document("package sign", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 82);
        assert_eq!(
            parsed["error"]["kind"], "unsupported",
            "the document must name the dedicated category, never `internal`: {json}"
        );
        assert_eq!(parsed["error"]["detail"], "unsupported_key_backend");
    }

    #[test]
    fn render_error_document_classifies_verify_unsupported_key_backend() {
        // Verify parses `--key` on its own path, so the same reference must
        // reach the same document through `VerifyErrorKind`. One vocabulary,
        // two taxonomies: a script reads one word for one failure.
        let id = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").unwrap();
        let rejected = ocx_trust::key_ref::KeyRef::parse("awskms://alias/release")
            .expect_err("awskms is recognised but unimplemented");
        let inner = ocx_sign::verify::VerifyError::new(id, ocx_sign::verify::VerifyErrorKind::from(rejected));
        let err = anyhow::Error::from(inner);
        let json = render_error_document("package verify", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 82);
        assert_eq!(
            parsed["error"]["kind"], "unsupported",
            "the document must name the dedicated category, never `internal`: {json}"
        );
        assert_eq!(parsed["error"]["detail"], "unsupported_key_backend");
    }

    #[test]
    fn detail_populated_for_offline_sign_refused() {
        // `error.detail` is what tells this offline refusal from any other 81 without parsing stderr.
        let id = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").unwrap();
        let inner = ocx_sign::sign::SignError::new(id, ocx_sign::sign::SignErrorKind::OfflineSignRefused);
        let err = anyhow::Error::from(inner);
        let json = render_error_document("package sign", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 81);
        assert_eq!(parsed["error"]["kind"], "permission_denied");
        assert_eq!(parsed["error"]["detail"], "offline_sign_refused");
    }

    #[test]
    fn transparency_log_failures_render_by_next_action_with_their_own_detail() {
        // One document per cause: the exit code and `error.kind` say what to do next (retry, fix the log URL, file
        // a bug, drop `--offline`); `error.detail` is the only place the Rekor-specific cause survives.
        use ocx_sign::verify::VerifyErrorKind as V;
        let cases = [
            (
                V::TransparencyLogUnavailable,
                75,
                "temp_fail",
                "transparency_log_unavailable",
            ),
            (
                V::TransparencyLogKeyUnavailable,
                69,
                "unavailable",
                "transparency_log_key_unavailable",
            ),
            (
                V::TransparencyLogResponseInvalid,
                65,
                "data_error",
                "transparency_log_response_invalid",
            ),
            (V::OfflineNoPinnedRekorKey, 81, "permission_denied", "offline_mode"),
        ];
        for (kind, code, category, detail) in cases {
            let id = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").unwrap();
            let err = anyhow::Error::from(ocx_sign::verify::VerifyError::new(id, kind));
            let json = render_error_document("package verify", &err).expect("render ok");
            let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
            assert_eq!(parsed["exit_code"], code, "{detail}: {json}");
            assert_eq!(parsed["error"]["kind"], category, "{detail}: {json}");
            assert_eq!(parsed["error"]["detail"], detail, "{json}");
        }
    }

    #[test]
    fn detail_populated_for_verify_identity_mismatch() {
        // Mirror coverage on the verify side: a reachable VerifyErrorKind variant
        // must surface its snake_case discriminant via `error.detail`.
        let id = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").unwrap();
        let inner = ocx_sign::verify::VerifyError::new(id, ocx_sign::verify::VerifyErrorKind::IdentityMismatch);
        let err = anyhow::Error::from(inner);
        let json = render_error_document("verify", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 77);
        assert_eq!(parsed["error"]["kind"], "permission_denied");
        assert_eq!(parsed["error"]["detail"], "identity_mismatch");
    }

    #[test]
    fn detail_populated_for_a_claim_data_error() {
        // A claim refusal needs its slug, or an SDK can tell it from another 65 only by its message.
        let inner = ocx_announce::claim::ClaimError::RepositoryMismatch {
            committed: "oci://ghcr.io/acme/widget".into(),
            supplied: "oci://quay.io/acme/widget".into(),
        };
        let err = anyhow::Error::from(inner);
        let json = render_error_document("package claim", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 65);
        assert_eq!(parsed["error"]["kind"], "data_error");
        assert_eq!(parsed["error"]["detail"], "repository_mismatch");
    }

    /// `detail` names the error that decided the exit code, for every classified family.
    #[test]
    fn detail_is_the_slug_of_the_error_that_decided_the_exit_code() {
        let err = anyhow::Error::new(ocx_config::mirror::MirrorConfigError::MissingUrl).context("loading config");
        let json = render_error_document("package install", &err).expect("render ok");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["exit_code"], 78, "document: {json}");
        assert_eq!(value["error"]["detail"], "mirror_config_invalid", "document: {json}");
    }

    #[test]
    fn a_command_error_carries_the_code_the_process_exits_with() {
        // The library classifier cannot downcast the CLI-local `CommandError`, so classifying with it
        // reports 1 while the process exits 64. Literals, because comparing the document to the
        // function it calls would pass under any classifier.
        let err = anyhow::Error::new(crate::app::CommandError::new(
            "refusing to write the predicate to a terminal".to_string(),
            ExitCode::UsageError,
        ));
        let json = render_error_document("package sbom", &err).expect("render ok");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(value["exit_code"], 64, "document: {json}");
        assert_eq!(value["error"]["kind"], "usage_error", "document: {json}");
        assert!(
            value["error"].get("detail").is_none(),
            "a command error carries no slug: {json}"
        );
        assert_eq!(
            value["exit_code"].as_u64().expect("exit_code is a number"),
            crate::exit::classify_error(err.as_ref()) as u8 as u64,
            "the document and the process must not disagree",
        );
    }
}
