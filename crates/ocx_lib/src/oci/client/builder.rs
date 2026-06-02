// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::config::mirror::MirrorConfigError;
use crate::{auth, oci};

use super::Client;
use super::MirrorMap;
use super::native_transport::NativeTransport;

/// Default push chunk size for OCX (16 MiB).
///
/// Overrides the oci-client default of 4 MiB to reduce HTTP round-trips
/// when uploading large package archives.
const DEFAULT_PUSH_CHUNK_SIZE: usize = 16 * 1024 * 1024;

pub struct ClientBuilder {
    auth: auth::Auth,
    config: oci::native::ClientConfig,
    lock_timeout: Option<std::time::Duration>,
    progress: crate::cli::progress::ProgressManager,
    mirrors: MirrorMap,
}

impl ClientBuilder {
    pub fn new() -> Self {
        ClientBuilder {
            auth: auth::Auth::default(),
            config: oci::native::ClientConfig {
                push_chunk_size: DEFAULT_PUSH_CHUNK_SIZE,
                ..Default::default()
            },
            lock_timeout: None,
            progress: crate::cli::progress::ProgressManager::disabled(),
            mirrors: MirrorMap::default(),
        }
    }

    /// Creates a client configured from standard environment variables.
    ///
    /// Reads `OCX_INSECURE_REGISTRIES` to configure plain-HTTP registries and
    /// `OCX_MIRRORS` to configure per-host registry mirrors. Progress rendering
    /// is disabled; the CLI uses
    /// [`from_env_with_progress`](Self::from_env_with_progress) to inject
    /// its shared [`ProgressManager`](crate::cli::progress::ProgressManager).
    ///
    /// `from_env*` is the seam through which a forwarded `OCX_MIRRORS` reaches a
    /// child `ocx`. Audit of call sites (2026-06-02): `from_env` is called only
    /// by `crates/ocx_mirror/src/command/sync.rs::sync` (the mirror tool's
    /// publisher client); `from_env_with_progress` is called only by
    /// `crates/ocx_cli/src/app/context.rs::Context::try_init`. The CLI also
    /// passes a `Config`-derived `MirrorMap` through the explicit
    /// [`mirrors`](Self::mirrors) builder; reading `env::mirrors()` here makes
    /// the same map available on the `from_env*` path so a forwarded
    /// `OCX_MIRRORS` reaches a child even when it has no parsed `Config`.
    ///
    /// # Errors
    ///
    /// Returns the [`MirrorConfigError`] from [`resolve_mirror_map`] when the
    /// forwarded `OCX_MIRRORS` is malformed, carries a non-string value, or
    /// names an `http://` mirror whose host is not in `OCX_INSECURE_REGISTRIES`.
    /// A failure aborts client construction rather than degrading to an identity
    /// map — a silent degrade would route reads to the firewall-blocked origin,
    /// the exact anti-goal replace semantics exist to prevent.
    ///
    /// [`resolve_mirror_map`]: crate::config::mirror::resolve_mirror_map
    pub fn from_env() -> Result<Client, MirrorConfigError> {
        Ok(ClientBuilder::new()
            .plain_http_registries(crate::env::insecure_registries())
            .mirrors(mirrors_from_env()?)
            .build())
    }

    /// Like [`from_env`](Self::from_env) but renders transfer progress
    /// through the supplied shared manager.
    ///
    /// # Errors
    ///
    /// Same as [`from_env`](Self::from_env): propagates the [`MirrorConfigError`]
    /// from a malformed or insecure forwarded `OCX_MIRRORS`.
    pub fn from_env_with_progress(
        progress: crate::cli::progress::ProgressManager,
    ) -> Result<Client, MirrorConfigError> {
        Ok(ClientBuilder::new()
            .plain_http_registries(crate::env::insecure_registries())
            .mirrors(mirrors_from_env()?)
            .progress(progress)
            .build())
    }

    /// Sets the shared progress manager used for download/upload bars.
    pub fn progress(mut self, progress: crate::cli::progress::ProgressManager) -> Self {
        self.progress = progress;
        self
    }

    /// Registries that should be contacted over plain HTTP instead of HTTPS.
    pub fn plain_http_registries(mut self, registries: Vec<String>) -> Self {
        if !registries.is_empty() {
            self.config.protocol = oci::native::ClientProtocol::HttpsExcept(registries);
        }
        self
    }

    /// Per-upstream-host registry mirror map, applied on the read path.
    ///
    /// The builder takes an already-parsed [`MirrorMap`] (parsing and the
    /// `url`-required validation happen in the config layer), mirroring the
    /// [`plain_http_registries`](Self::plain_http_registries) precedent of
    /// taking parsed primitives rather than the `Config` tree.
    pub fn mirrors(mut self, mirrors: MirrorMap) -> Self {
        self.mirrors = mirrors;
        self
    }

    /// Maximum chunk size in bytes for chunked blob uploads.
    pub fn push_chunk_size(mut self, size: usize) -> Self {
        self.config.push_chunk_size = size;
        self
    }

    /// Timeout for acquiring file locks during install and status checks.
    pub fn lock_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.lock_timeout = Some(timeout);
        self
    }

    pub fn build(self) -> Client {
        let push_chunk_size = self.config.push_chunk_size;
        let transport = NativeTransport::new(oci::native::Client::new(self.config), self.auth, push_chunk_size);
        Client {
            transport: Box::new(transport),
            lock_timeout: self.lock_timeout.unwrap_or(std::time::Duration::from_secs(30)),
            tag_chunk_size: 100,
            repository_chunk_size: 100,
            progress: self.progress,
            mirrors: self.mirrors,
        }
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl ClientBuilder {
    /// Test-only inspector: the resolved plain-HTTP host list the transport
    /// will be configured with.
    ///
    /// Returns `Some(hosts)` when [`plain_http_registries`](Self::plain_http_registries)
    /// set the protocol to `HttpsExcept(hosts)`, `None` otherwise (default
    /// HTTPS-everywhere). Exposed only under `#[cfg(test)]` so the F2 unit test
    /// can prove a mirror host actually reaches the plain-HTTP transport config
    /// — it is **not** part of the production API. The protocol field is
    /// consumed by [`build`](Self::build) into the transport's `oci::native::Client`,
    /// which exposes no public getter, so this is the only seam that can observe
    /// the resolved value before construction.
    pub(crate) fn plain_http_hosts(&self) -> Option<&[String]> {
        match &self.config.protocol {
            oci::native::ClientProtocol::HttpsExcept(hosts) => Some(hosts.as_slice()),
            _ => None,
        }
    }
}

/// Builds a [`MirrorMap`] from the forwarded `OCX_MIRRORS` env var.
///
/// Delegates to the single lib resolver
/// [`crate::config::mirror::resolve_mirror_map`] with an empty [`Config`] (the
/// `from_env*` path has no parsed config — the mirror map comes entirely from
/// the inherited `OCX_MIRRORS`) and the inherited
/// [`crate::env::insecure_registries`]. Using the same resolver as
/// `Context::try_init` means there is ONE Config→mirror-map transform with one
/// precedence rule and one plain-HTTP gate, not two with divergent handling.
///
/// # Errors
///
/// A resolution failure here (a malformed or non-string forwarded env, or an
/// http mirror whose host the parent did not also forward into
/// `OCX_INSECURE_REGISTRIES`) is a HARD error: it aborts client construction on
/// the child the same way the parent (`Context::try_init`) aborts. Degrading to
/// an empty identity map would route reads to the firewall-blocked origin — the
/// exact anti-goal replace semantics exist to prevent. A well-formed forwarded
/// `OCX_MIRRORS` reaches the child unchanged.
///
/// [`Config`]: crate::config::Config
fn mirrors_from_env() -> Result<MirrorMap, MirrorConfigError> {
    let (entries, _pairs) = crate::config::mirror::resolve_mirror_map(
        &crate::config::Config::default(),
        crate::env::mirrors()?,
        &crate::env::insecure_registries(),
    )?;
    Ok(MirrorMap::new(entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Step 3.1 specification tests ─────────────────────────────────────────

    /// `from_env_with_progress` with `OCX_MIRRORS` set produces a `Client`
    /// whose `mirrors` map is non-empty.
    ///
    /// This forces the `from_env*` wiring — not just the explicit-builder path.
    /// Traces: plan Testing Strategy — "`from_env_with_progress` with
    /// `OCX_MIRRORS` set → built `Client.mirrors` is non-empty"; ADR review
    /// A5a/F4.
    #[test]
    fn from_env_with_progress_reads_ocx_mirrors_env() {
        // Use the test env-lock so we do not mutate std::env and so the test
        // serialises with other env-touching tests (no flaky parallel env race).
        let env = crate::test::env::lock();
        // A valid single-entry JSON object for OCX_MIRRORS.
        env.set(
            crate::env::keys::OCX_MIRRORS,
            r#"{"ghcr.io":"https://mirror.corp/ghcr-remote"}"#,
        );

        let client = ClientBuilder::from_env_with_progress(crate::cli::progress::ProgressManager::disabled())
            .expect("well-formed OCX_MIRRORS must build a client");

        assert!(
            !client.mirrors.is_empty(),
            "from_env_with_progress with OCX_MIRRORS set must produce a non-empty MirrorMap"
        );
    }

    /// A malformed forwarded `OCX_MIRRORS` aborts the child `from_env*` path —
    /// the SAME hard-error contract as the parent (`Context::try_init`).
    ///
    /// The child must never silently degrade to an identity map: a forwarded
    /// map that fails to resolve could otherwise route reads to the
    /// firewall-blocked origin, the exact anti-goal replace semantics prevent.
    ///
    /// Traces: review Cluster-1 fail-loud — child path aborts like the parent.
    #[test]
    fn from_env_with_progress_aborts_on_malformed_ocx_mirrors() {
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_MIRRORS, "this is not valid json {{{");

        let error = ClientBuilder::from_env_with_progress(crate::cli::progress::ProgressManager::disabled())
            .err()
            .expect("a malformed forwarded OCX_MIRRORS must abort the child");
        assert!(
            matches!(error, MirrorConfigError::MalformedEnvJson { .. }),
            "expected MalformedEnvJson, got: {error:?}"
        );
    }

    /// `http://` mirror host flows to `plain_http_registries`.
    ///
    /// When a mirror's URL uses `http://`, its host must reach the client's
    /// plain-HTTP transport config so connections succeed without TLS. Per the
    /// Living Design Record, composition is **explicit** — the operator lists
    /// the mirror host in `OCX_INSECURE_REGISTRIES`; the `http://` scheme alone
    /// never opts out of TLS. This test proves the host actually lands in the
    /// resolved `HttpsExcept` transport list (via the `#[cfg(test)]`
    /// `plain_http_hosts` inspector), not merely that the MirrorMap carries it.
    ///
    /// Traces: plan Testing Strategy — "`http://` mirror host flows to
    /// `plain_http_registries`"; ADR F2.
    #[test]
    fn http_mirror_host_flows_to_plain_http_registries() {
        use crate::config::mirror::ParsedMirror;

        let mirror_map = MirrorMap::new([(
            "ghcr.io".to_string(),
            ParsedMirror {
                protocol: "http".to_string(),
                host: "local-mirror.corp:5001".to_string(),
                path_prefix: String::new(),
            },
        )]);

        let builder = ClientBuilder::new()
            .plain_http_registries(vec!["local-mirror.corp:5001".to_string()])
            .mirrors(mirror_map);

        // Prove the mirror host reached the plain-HTTP transport config — this
        // is the assertion that would FAIL if `plain_http_registries` were a
        // no-op. `scheme_for("local-mirror.corp:5001")` would then return
        // "https" and a plain-HTTP mirror would be unreachable.
        let hosts = builder
            .plain_http_hosts()
            .expect("plain_http_registries must set the protocol to HttpsExcept");
        assert!(
            hosts.iter().any(|h| h == "local-mirror.corp:5001"),
            "the http mirror host must reach the plain-HTTP transport config, got: {hosts:?}"
        );

        // And the built client still carries the mirror map (read-path rewrite).
        let client = builder.build();
        assert!(
            client.mirrors.get("ghcr.io").is_some(),
            "ghcr.io mirror entry must be present on the built client"
        );
    }
}
