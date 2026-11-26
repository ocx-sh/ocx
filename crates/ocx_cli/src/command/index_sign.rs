// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx index sign`: stamps a rendered index's catalog with an expiry and writes a detached signature beside it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{DateTime, Duration, SubsecRound, Utc};
use clap::Parser;
use ocx_index::wire_writer::serialize_catalog_with_extra_members;
use ocx_index::{CATALOG_PATH, CATALOG_SIGNATURE_PATH, CatalogDocument};
use ocx_package_manager::tasks::sign::build_signer;
use ocx_sign::detached::{INDEX_CATALOG_PREDICATE_TYPE, SignedStatement, SigningEndpoints, sign_statement};
use ocx_sign::sign::{DispatchingTokenProvider, SignError, SignErrorKind, Signer, SignerIdentity, TokenProvider};
use ocx_trust::key_ref::KeyBackendKind;
use ocx_util::fs::BoundedReadError;
use ocx_util::fs::path::AbsolutePath;

use crate::api::data::index_sign::IndexSignReport;
use crate::command::package_sign_common;
use crate::options::key::KeyOpt;
use crate::options::rekor_upload::RekorUploadOpt;
use crate::options::sign_endpoints::SignEndpointsOpt;

/// The statement's subject: the catalog's path in the index, not where it sits on this machine.
const SUBJECT_NAME: &str = CATALOG_PATH;

const EXPIRES_HELP: &str = "\
Days from now until the signed catalog stops being trusted.\n\n\
Accepts a whole number from `0` to `366`. `0` expires the catalog immediately.";

#[derive(Parser, Clone)]
#[command(
    about = "Sign an index's catalog",
    long_about = "\
Write the signed catalog of a rendered index.

Reads `<DIR>/c/index.json`, sets its `expires`, and rewrites it. The signature goes to \
`<DIR>/c/index.json.sigstore.json`, a Sigstore bundle over the new catalog bytes that names the \
registry the index serves. A client that checks index signatures fetches both files.

The catalog is signed keylessly (an OIDC identity, Fulcio and Rekor) unless `--key` is given. \
Serve the two files together: a client refuses a catalog whose signature does not match it.

See https://ocx.sh/docs/explanation/signing#index-signatures."
)]
pub struct IndexSign {
    /// Registry the index serves, for example `ocx.sh`.
    ///
    /// Recorded in the signed statement, so a signature for one registry's catalog
    /// is not accepted for another's. Spell it as clients do: lowercase `host[:port]`,
    /// no path and no default port.
    #[clap(long = "registry", value_name = "REGISTRY", required = true, value_parser = canonical_registry)]
    registry: String,

    #[clap(
        long = "expires-in-days",
        value_name = "DAYS",
        default_value_t = 30,
        value_parser = clap::value_parser!(u16).range(0..=366),
        help = "Days until the signed catalog expires (0-366)",
        long_help = EXPIRES_HELP
    )]
    expires_in_days: u16,

    #[clap(flatten)]
    endpoints: SignEndpointsOpt,

    #[clap(flatten)]
    key: KeyOpt,

    #[clap(flatten)]
    rekor_upload: RekorUploadOpt,

    /// Index directory holding `c/index.json`.
    dir: PathBuf,
}

impl IndexSign {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        refuse_when_offline(context.is_offline())?;

        // The identity an error names: the registry's index, since a signing error needs a package reference.
        let subject = ocx_oci::PackageRef::parse(&format!("{}/index", self.registry))
            .map_err(|error| crate::error::UsageError::with_source("--registry is not a valid registry", error))?;
        let key_mode = self.key.is_key_mode();

        // Also the SSRF guard (CWE-918): both endpoints are validated before any client dials them.
        let target = package_sign_common::resolve_sigstore_pair(
            context.config_trust_sigstore(),
            &subject,
            key_mode,
            self.endpoints.fulcio_url(),
            self.endpoints.rekor_url(),
        )?;
        let wrap = |kind: SignErrorKind| SignError::new(subject.clone(), kind);
        let key = self.key.reference().map_err(|kind| wrap(SignErrorKind::from(kind)))?;
        let rekor_upload = self
            .rekor_upload
            .enabled(key_mode, target.configured_rekor_upload)
            .map_err(wrap)?;
        let (fulcio_url, rekor_url) = target.endpoints(rekor_upload)?;

        // Never log the token or put it in error context.
        let override_token = match key_mode {
            true => None,
            false => {
                package_sign_common::resolve_override_token(
                    self.endpoints.identity_token_file(),
                    self.endpoints.identity_token_stdin(),
                    &subject,
                )
                .await?
            }
        };
        let signer = build_signer(key, rekor_upload, rekor_url.clone())
            .await
            .map_err(&wrap)?;
        let trusted_hosts = context
            .manager()
            .index()
            .trusted_hosts_for(self.registry.as_str())
            .to_vec();
        let token = match signer.requires_identity_token() {
            true => {
                let trusted_hosts = trusted_hosts.clone();
                let provider = DispatchingTokenProvider::new(override_token, self.endpoints.no_tty(), trusted_hosts);
                Some(provider.acquire("sigstore").await.map_err(&wrap)?)
            }
            false => None,
        };
        let endpoints = SigningEndpoints {
            fulcio_url: &fulcio_url,
            rekor_url: &rekor_url,
            token: token.as_ref(),
            trusted_hosts: &trusted_hosts,
        };

        let signed = sign_catalog_dir(
            &self.dir,
            &self.registry,
            self.expires_in_days,
            Utc::now(),
            signer.as_ref(),
            &endpoints,
            ocx_oci::client::MAX_INDEX_DOCUMENT_BYTES,
        )
        .await
        .map_err(|error| match error {
            CatalogSignError::Sign(kind) => anyhow::Error::from(wrap(kind)),
            CatalogSignError::Other(error) => error,
        })?;

        context.api().report(&signed.into_report()?)?;
        Ok(ExitCode::SUCCESS)
    }
}

/// The registry as a client compares it, or why this spelling can never match one.
///
/// Refuses rather than normalizes: the signed statement must carry exactly what the operator typed.
fn canonical_registry(input: &str) -> Result<String, String> {
    let refusal =
        || "expected the lowercase `host[:port]` a client names the registry by, with no path or default port";
    input.parse::<ocx_oci::RegistryHost>().map_err(|_| refusal())?;
    // No client spells a rooted FQDN or port 0, but the URL parser keeps both.
    let (host_part, port_part) = input
        .rsplit_once(':')
        .filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or((input, ""));
    if host_part.ends_with('.') || (!port_part.is_empty() && port_part.trim_start_matches('0').is_empty()) {
        return Err(format!("{}; drop the trailing dot or port 0", refusal()));
    }
    let url = ocx_oci::endpoint::Url::parse(&format!("https://{input}")).map_err(|_| refusal())?;
    let host = url.host_str().ok_or_else(refusal)?;
    let canonical = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    if canonical == input {
        Ok(canonical)
    } else {
        Err(format!("{}; write `{canonical}`", refusal()))
    }
}

/// Refuses output a client would refuse to read back.
fn ensure_within_client_cap(bytes: &[u8], cap: usize) -> anyhow::Result<()> {
    if bytes.len() <= cap {
        return Ok(());
    }
    // `index_serialization` (data error): the signed catalog this run would write cannot be a catalog.
    let reason = <serde_json::Error as serde::ser::Error>::custom(format!(
        "the signed catalog is {} bytes, over the {cap}-byte limit clients read; shrink the catalog",
        bytes.len()
    ));
    Err(ocx_index::error::Error::SerializationFailure(reason).into())
}

/// Signing needs Sigstore services and writing a signature is not something a cached run can stand in for.
fn refuse_when_offline(offline: bool) -> anyhow::Result<()> {
    if offline {
        return Err(ocx_package_manager::Error::OfflineMode.into());
    }
    Ok(())
}

/// Why a catalog could not be signed and written.
enum CatalogSignError {
    /// The signer or the transparency log failed; the caller names the package reference.
    Sign(SignErrorKind),
    /// Anything the catalog or its directory raised, already classified.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for CatalogSignError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

/// What a signed directory holds.
struct SignedCatalog {
    catalog_path: PathBuf,
    signature_path: PathBuf,
    catalog_digest: ocx_oci::Digest,
    expires: DateTime<Utc>,
    statement: SignedStatement,
}

impl SignedCatalog {
    fn into_report(self) -> anyhow::Result<IndexSignReport> {
        let (signer, certificate_identity, certificate_oidc_issuer, key_backend, public_key_hint) =
            match self.statement.signer {
                SignerIdentity::Keyless {
                    certificate_identity,
                    certificate_oidc_issuer,
                } => (
                    "keyless-fulcio".to_owned(),
                    Some(certificate_identity),
                    Some(certificate_oidc_issuer),
                    KeyBackendKind::Keyless,
                    None,
                ),
                SignerIdentity::Key {
                    public_key_hint,
                    key_backend,
                } => (key_backend.to_string(), None, None, key_backend, Some(public_key_hint)),
            };
        Ok(IndexSignReport {
            catalog_path: report_path(&self.catalog_path)?,
            signature_path: report_path(&self.signature_path)?,
            catalog_digest: self.catalog_digest,
            expires_at: self.expires.into(),
            signer,
            certificate_identity,
            certificate_oidc_issuer,
            key_backend,
            public_key_hint,
            tlog_index: self.statement.tlog_index,
        })
    }
}

/// The path as the report states it: absolute, resolved against the working directory without touching the disk.
fn report_path(path: &Path) -> anyhow::Result<AbsolutePath> {
    let absolute = std::path::absolute(path).map_err(|error| ocx_util::error::FileError::new(path, error))?;
    AbsolutePath::new(&absolute)
        .ok_or_else(|| ocx_util::error::FileError::new(&absolute, std::io::Error::other("not an absolute path")).into())
}

/// Sets the catalog's `expires`, signs the new bytes in memory, and only then replaces both files.
///
/// A failure before the writes leaves both files as they were. The writes stage both new files beside their targets
/// first, so a failed staging cannot leave one new file beside one old one.
async fn sign_catalog_dir(
    dir: &Path,
    registry: &str,
    expires_in_days: u16,
    now: DateTime<Utc>,
    signer: &dyn Signer,
    endpoints: &SigningEndpoints<'_>,
    max_bytes: usize,
) -> Result<SignedCatalog, CatalogSignError> {
    let catalog_path = dir.join(CATALOG_PATH);
    let signature_path = dir.join(CATALOG_SIGNATURE_PATH);

    let raw = ocx_util::fs::read_bounded_async(&catalog_path, ocx_oci::client::MAX_INDEX_DOCUMENT_BYTES as u64)
        .await
        .map_err(|error| CatalogSignError::Other(file_error(&catalog_path, error).into()))?;
    let malformed = |source| {
        CatalogSignError::Other(
            ocx_index::error::Error::MalformedIndexDocument {
                url: catalog_path.display().to_string(),
                source,
            }
            .into(),
        )
    };
    let document: serde_json::Value = serde_json::from_slice(&raw).map_err(malformed)?;
    let mut catalog: CatalogDocument = serde_json::from_value(document.clone()).map_err(malformed)?;
    // Refuse what a client would refuse to read, before a signature vouches for it.
    catalog
        .clone()
        .into_packages()
        .map_err(|error| CatalogSignError::Other(error.into()))?;
    // Every member the schema does not model rides through the rewrite untouched.
    let mut extra = document.as_object().cloned().unwrap_or_default();
    // `retain`, not `remove`: `Map::remove` swaps the last member into the gap and reorders the rest.
    extra.retain(|name, _| !["format_version", "packages", "expires"].contains(&name.as_str()));
    let expires = (now + Duration::days(i64::from(expires_in_days))).trunc_subsecs(0);
    catalog.expires = Some(expires);
    let bytes = serialize_catalog_with_extra_members(&catalog, &extra);
    ensure_within_client_cap(&bytes, max_bytes)?;

    let predicate = serde_json::json!({ "registry": registry });
    let statement = sign_statement(
        &bytes,
        SUBJECT_NAME,
        INDEX_CATALOG_PREDICATE_TYPE,
        &predicate,
        signer,
        endpoints,
    )
    .await
    .map_err(CatalogSignError::Sign)?;

    let signature = statement.bundle.clone();
    let (catalog_target, signature_target, catalog_bytes) =
        (catalog_path.clone(), signature_path.clone(), bytes.clone());
    tokio::task::spawn_blocking(move || write_pair(&catalog_target, &catalog_bytes, &signature_target, &signature))
        .await
        .map_err(|join| anyhow::anyhow!("catalog write task failed: {join}"))?
        .map_err(anyhow::Error::from)?;

    Ok(SignedCatalog {
        catalog_path,
        signature_path,
        catalog_digest: ocx_oci::Algorithm::Sha256.hash(&bytes),
        expires,
        statement,
    })
}

/// Stages both files beside their targets, then renames the catalog and the signature into place.
///
/// An interruption between the renames leaves the new catalog beside the old signature; running the command again
/// repairs it.
fn write_pair(
    catalog_path: &Path,
    catalog: &[u8],
    signature_path: &Path,
    signature: &[u8],
) -> Result<(), ocx_util::error::FileError> {
    let staged = |target: &Path, bytes: &[u8]| -> Result<tempfile::NamedTempFile, ocx_util::error::FileError> {
        use std::io::Write as _;

        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let io = |error| ocx_util::error::FileError::new(target, error);
        // Published, not private: the files are served as written.
        let mut file = published_temp_file(parent).map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        Ok(file)
    };
    let catalog_tmp = staged(catalog_path, catalog)?;
    let signature_tmp = staged(signature_path, signature)?;
    ocx_util::fs::persist_temp_file(catalog_tmp, catalog_path)
        .map_err(|error| ocx_util::error::FileError::new(catalog_path, error))?;
    ocx_util::fs::persist_temp_file(signature_tmp, signature_path)
        .map_err(|error| ocx_util::error::FileError::new(signature_path, error))
}

fn published_temp_file(dir: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o644))
            .tempfile_in(dir)
    }
    #[cfg(not(unix))]
    {
        tempfile::NamedTempFile::new_in(dir)
    }
}

fn file_error(path: &Path, error: BoundedReadError) -> ocx_util::error::FileError {
    match error {
        BoundedReadError::Io { source, .. } => ocx_util::error::FileError::new(path, source),
        other => ocx_util::error::FileError::new(path, std::io::Error::other(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use chrono::TimeZone as _;
    use ocx_oci::endpoint::Url;
    use ocx_sign::sign::KeySigner;
    use ocx_sign::sign::{KeyBackend, KeyBackendError, OidcToken, SignedBlob, SignedBundle};

    use super::*;

    /// A key backend over fixed bytes: the bundle builder only hashes the public key and the signature is opaque.
    struct FixedKey;

    #[async_trait]
    impl KeyBackend for FixedKey {
        async fn sign_prehash(&self, _digest: &[u8]) -> Result<Vec<u8>, KeyBackendError> {
            Ok(vec![0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01])
        }

        fn public_key_der(&self) -> &[u8] {
            &[1, 2, 3, 4]
        }

        fn kind(&self) -> KeyBackendKind {
            KeyBackendKind::File
        }
    }

    /// A signer that always fails, to prove a failed sign touches nothing.
    struct FailingSigner;

    #[async_trait]
    impl Signer for FailingSigner {
        fn signer_kind(&self) -> &'static str {
            "failing"
        }

        async fn sign_dsse(
            &self,
            _statement_bytes: &[u8],
            _token: Option<&OidcToken>,
            _fulcio_url: &Url,
            _rekor_url: &Url,
        ) -> Result<SignedBundle, SignErrorKind> {
            Err(SignErrorKind::FulcioBadRequest)
        }

        async fn sign_blob(
            &self,
            _payload: &[u8],
            _token: Option<&OidcToken>,
            _fulcio_url: &Url,
            _rekor_url: &Url,
        ) -> Result<SignedBlob, SignErrorKind> {
            Err(SignErrorKind::FulcioBadRequest)
        }
    }

    /// Forwards to a real signer and keeps the statement bytes it was handed.
    struct Recording {
        inner: KeySigner,
        statement: Arc<std::sync::Mutex<Vec<u8>>>,
    }

    #[async_trait]
    impl Signer for Recording {
        async fn sign_dsse(
            &self,
            statement_bytes: &[u8],
            token: Option<&OidcToken>,
            fulcio_url: &Url,
            rekor_url: &Url,
        ) -> Result<SignedBundle, SignErrorKind> {
            *self.statement.lock().expect("lock") = statement_bytes.to_vec();
            self.inner
                .sign_dsse(statement_bytes, token, fulcio_url, rekor_url)
                .await
        }

        async fn sign_blob(
            &self,
            payload: &[u8],
            token: Option<&OidcToken>,
            fulcio_url: &Url,
            rekor_url: &Url,
        ) -> Result<SignedBlob, SignErrorKind> {
            self.inner.sign_blob(payload, token, fulcio_url, rekor_url).await
        }

        fn signer_kind(&self) -> &'static str {
            self.inner.signer_kind()
        }
    }

    const CATALOG: &str = r#"{"format_version":1,"packages":{"cmake":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0)
            .single()
            .expect("a valid instant")
    }

    fn index_dir(catalog: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        if let Some(catalog) = catalog {
            std::fs::create_dir_all(dir.path().join("c")).expect("c dir");
            std::fs::write(dir.path().join(CATALOG_PATH), catalog).expect("catalog");
        }
        dir
    }

    fn endpoints(url: &Url) -> SigningEndpoints<'_> {
        SigningEndpoints {
            fulcio_url: url,
            rekor_url: url,
            token: None,
            trusted_hosts: &[],
        }
    }

    fn key_signer() -> KeySigner {
        KeySigner::new(Arc::new(FixedKey), None)
    }

    async fn sign(dir: &Path, days: u16, signer: &dyn Signer) -> Result<SignedCatalog, CatalogSignError> {
        let url = Url::parse("http://127.0.0.1:5555").expect("url");
        sign_catalog_dir(
            dir,
            "ocx.sh",
            days,
            now(),
            signer,
            &endpoints(&url),
            ocx_oci::client::MAX_INDEX_DOCUMENT_BYTES,
        )
        .await
    }

    fn exit_code(error: CatalogSignError) -> ocx_exit::ExitCode {
        match error {
            CatalogSignError::Other(error) => crate::exit::classify_library_error(error.as_ref()),
            CatalogSignError::Sign(kind) => {
                ocx_exit::ClassifyExitCode::classify(&kind).unwrap_or(ocx_exit::ExitCode::Failure)
            }
        }
    }

    #[tokio::test]
    async fn signing_writes_the_expiring_catalog_and_a_bundle_over_exactly_its_bytes() {
        let dir = index_dir(Some(CATALOG));
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let signer = Recording {
            inner: key_signer(),
            statement: recorded.clone(),
        };
        let signed = sign(dir.path(), 30, &signer).await.unwrap_or_else(|_| panic!("signs"));

        let written = std::fs::read(dir.path().join(CATALOG_PATH)).expect("catalog");
        let document: serde_json::Value = serde_json::from_slice(&written).expect("json");
        assert_eq!(document["expires"], "2026-03-31T12:00:00Z");
        assert_eq!(
            document["packages"]["cmake"],
            serde_json::from_str::<serde_json::Value>(CATALOG).expect("json")["packages"]["cmake"]
        );
        assert_eq!(signed.catalog_digest, ocx_oci::Algorithm::Sha256.hash(&written));

        let bundle: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(CATALOG_SIGNATURE_PATH)).expect("signature"))
                .expect("bundle json");
        assert!(bundle.get("dsseEnvelope").is_some(), "{bundle}");
        let statement: serde_json::Value =
            serde_json::from_slice(&recorded.lock().expect("lock").clone()).expect("statement json");
        assert_eq!(statement["subject"][0]["name"], "c/index.json");
        assert_eq!(
            statement["subject"][0]["digest"]["sha256"],
            ocx_oci::Algorithm::Sha256.hash(&written).hex()
        );
        assert_eq!(statement["predicateType"], INDEX_CATALOG_PREDICATE_TYPE);
        assert_eq!(statement["predicate"]["registry"], "ocx.sh");
    }

    #[tokio::test]
    async fn zero_days_expires_the_catalog_at_signing_time() {
        let dir = index_dir(Some(CATALOG));
        let signed = sign(dir.path(), 0, &key_signer())
            .await
            .unwrap_or_else(|_| panic!("signs"));
        assert_eq!(signed.expires, now());
    }

    #[tokio::test]
    async fn a_failed_sign_leaves_the_catalog_unchanged_and_writes_no_signature() {
        let dir = index_dir(Some(CATALOG));
        let Err(error) = sign(dir.path(), 30, &FailingSigner).await else {
            panic!("a failing signer must fail the command");
        };
        assert!(matches!(error, CatalogSignError::Sign(SignErrorKind::FulcioBadRequest)));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CATALOG_PATH)).expect("catalog"),
            CATALOG
        );
        assert!(!dir.path().join(CATALOG_SIGNATURE_PATH).exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("c")).expect("dir").collect();
        assert_eq!(leftovers.len(), 1, "no temp file may remain: {leftovers:?}");
    }

    #[tokio::test]
    async fn an_unsupported_format_version_is_refused_before_anything_is_written() {
        let unsupported = CATALOG.replace("\"format_version\":1", "\"format_version\":2");
        let dir = index_dir(Some(&unsupported));
        let Err(error) = sign(dir.path(), 30, &key_signer()).await else {
            panic!("an unsupported format_version must fail");
        };
        let CatalogSignError::Other(inner) = &error else {
            panic!("expected a classified catalog error");
        };
        assert!(
            matches!(
                inner.downcast_ref::<ocx_index::error::Error>(),
                Some(ocx_index::error::Error::UnsupportedIndexFormat { version: 2 })
            ),
            "{inner:?}"
        );
        assert_eq!(exit_code(error), ocx_exit::ExitCode::DataError);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CATALOG_PATH)).expect("catalog"),
            unsupported
        );
        assert!(!dir.path().join(CATALOG_SIGNATURE_PATH).exists());
    }

    #[tokio::test]
    async fn an_unparseable_catalog_is_a_data_error_and_stays_as_it_was() {
        let dir = index_dir(Some("{not json"));
        let Err(error) = sign(dir.path(), 30, &key_signer()).await else {
            panic!("a corrupt catalog must fail");
        };
        assert_eq!(exit_code(error), ocx_exit::ExitCode::DataError);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CATALOG_PATH)).expect("catalog"),
            "{not json"
        );
        assert!(!dir.path().join(CATALOG_SIGNATURE_PATH).exists());
    }

    #[tokio::test]
    async fn a_missing_catalog_keeps_its_file_error_classification() {
        let dir = index_dir(None);
        let Err(error) = sign(dir.path(), 30, &key_signer()).await else {
            panic!("a missing catalog must fail");
        };
        assert_eq!(exit_code(error), ocx_exit::ExitCode::IoError);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_published_files_are_world_readable() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = index_dir(Some(CATALOG));
        sign(dir.path(), 30, &key_signer())
            .await
            .unwrap_or_else(|_| panic!("signs"));
        for name in [CATALOG_PATH, CATALOG_SIGNATURE_PATH] {
            let mode = std::fs::metadata(dir.path().join(name))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o044,
                0o044,
                "{name} must stay readable to the group and others: {mode:o}"
            );
        }
    }

    #[test]
    fn offline_is_a_policy_refusal() {
        let error = refuse_when_offline(true).expect_err("offline refuses");
        assert_eq!(
            crate::exit::classify_library_error(error.as_ref()),
            ocx_exit::ExitCode::PolicyBlocked
        );
        assert!(refuse_when_offline(false).is_ok());
    }

    fn parse(extra: &[&str]) -> Result<IndexSign, clap::error::ErrorKind> {
        let mut argv = vec!["sign"];
        argv.extend_from_slice(extra);
        IndexSign::try_parse_from(argv).map_err(|error| error.kind())
    }

    #[test]
    fn the_registry_is_required_and_the_days_default_to_thirty() {
        assert_eq!(
            parse(&["dir"]).err(),
            Some(clap::error::ErrorKind::MissingRequiredArgument)
        );
        let sign = parse(&["--registry", "ocx.sh", "dir"]).expect("parses");
        assert_eq!(sign.expires_in_days, 30);
    }

    #[test]
    fn the_days_are_capped_at_a_leap_year() {
        assert!(parse(&["--registry", "r", "--expires-in-days", "366", "dir"]).is_ok());
        assert!(parse(&["--registry", "r", "--expires-in-days", "0", "dir"]).is_ok());
        assert_eq!(
            parse(&["--registry", "r", "--expires-in-days", "367", "dir"]).err(),
            Some(clap::error::ErrorKind::ValueValidation)
        );
    }

    #[test]
    fn the_package_only_flags_are_not_taken() {
        for flag in [
            &["--signature-format", "bundle"][..],
            &["--no-cache"],
            &["--platform", "linux/amd64"],
            &["--tags", "1"],
        ] {
            let mut argv = vec!["--registry", "r"];
            argv.extend_from_slice(flag);
            argv.push("dir");
            assert_eq!(
                parse(&argv).err(),
                Some(clap::error::ErrorKind::UnknownArgument),
                "{flag:?}"
            );
        }
    }

    #[test]
    fn the_keyless_only_flags_are_refused_beside_a_key() {
        for flag in [
            &["--fulcio-url", "https://f.example"][..],
            &["--identity-token-stdin"],
            &["--no-tty"],
        ] {
            let mut argv = vec!["--registry", "r", "--key", "k.pem"];
            argv.extend_from_slice(flag);
            argv.push("dir");
            assert_eq!(
                parse(&argv).err(),
                Some(clap::error::ErrorKind::ArgumentConflict),
                "{flag:?}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_catalog_members_are_kept_with_their_values() {
        let catalog = r#"{"zeta":1,"format_version":1,"packages":{},"nested":{"a":{"b":[1,2]}},"alpha":2,"expires":"2020-01-01T00:00:00Z","big":3}"#;
        let dir = index_dir(Some(catalog));
        sign(dir.path(), 30, &key_signer())
            .await
            .unwrap_or_else(|_| panic!("signs"));

        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(CATALOG_PATH)).expect("catalog")).expect("json");
        assert_eq!(written["zeta"], 1);
        assert_eq!(written["nested"], serde_json::json!({"a": {"b": [1, 2]}}));
        assert_eq!(written["expires"], "2026-03-31T12:00:00Z");
        let unknown: Vec<&str> = written
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .filter(|name| !["format_version", "packages", "expires"].contains(name))
            .collect();
        assert_eq!(unknown, ["zeta", "nested", "alpha", "big"]);
    }

    #[test]
    fn output_over_the_clients_cap_is_a_data_error() {
        assert!(ensure_within_client_cap(b"1234", 4).is_ok());
        let error = ensure_within_client_cap(b"12345", 4).expect_err("over the cap");
        assert_eq!(
            crate::exit::classify_library_error(error.as_ref()),
            ocx_exit::ExitCode::DataError
        );
        assert!(format!("{error:#}").contains("over the 4-byte limit"), "{error:#}");
    }

    #[tokio::test]
    async fn a_rewrite_past_the_cap_writes_nothing() {
        let dir = index_dir(Some(CATALOG));
        let url = Url::parse("http://127.0.0.1:5555").expect("url");
        let result = sign_catalog_dir(
            dir.path(),
            "ocx.sh",
            30,
            now(),
            &key_signer(),
            &endpoints(&url),
            CATALOG.len(),
        )
        .await;
        let Err(error) = result else {
            panic!("the rewrite is longer than the compact input and must be refused");
        };
        assert_eq!(exit_code(error), ocx_exit::ExitCode::DataError);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CATALOG_PATH)).expect("catalog"),
            CATALOG
        );
        assert!(!dir.path().join(CATALOG_SIGNATURE_PATH).exists());
    }

    #[test]
    fn the_registry_must_be_spelled_as_clients_compare_it() {
        for ok in ["ocx.sh", "localhost:5000", "127.0.0.1:8080", "[::1]:5000", "ghcr.io"] {
            assert_eq!(canonical_registry(ok).as_deref(), Ok(ok), "{ok}");
        }
        for bad in [
            "OCX.SH",
            "ocx.sh:443",
            "ocx.sh/index",
            "https://ocx.sh",
            "",
            "user@ocx.sh",
            "xn--\u{e9}.com",
            "ocx.sh.",
            "ocx.sh..",
            "localhost.",
            "ocx.sh:0",
        ] {
            assert!(canonical_registry(bad).is_err(), "{bad:?}");
        }
        assert!(canonical_registry("OCX.SH").expect_err("refused").contains("ocx.sh"));
    }
}
