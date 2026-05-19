// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{auth, oci};

use super::Client;
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
        }
    }

    /// Creates a client configured from standard environment variables.
    ///
    /// Reads `OCX_INSECURE_REGISTRIES` to configure plain-HTTP registries.
    /// Progress rendering is disabled; the CLI uses
    /// [`from_env_with_progress`](Self::from_env_with_progress) to inject
    /// its shared [`ProgressManager`](crate::cli::progress::ProgressManager).
    pub fn from_env() -> Client {
        ClientBuilder::new()
            .plain_http_registries(crate::env::insecure_registries())
            .build()
    }

    /// Like [`from_env`](Self::from_env) but renders transfer progress
    /// through the supplied shared manager.
    pub fn from_env_with_progress(progress: crate::cli::progress::ProgressManager) -> Client {
        ClientBuilder::new()
            .plain_http_registries(crate::env::insecure_registries())
            .progress(progress)
            .build()
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
        }
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}
