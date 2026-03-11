// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{auth, oci};

use super::Client;
use super::native_transport::NativeTransport;

pub struct ClientBuilder {
    auth: auth::Auth,
    config: oci::native::ClientConfig,
    lock_timeout: Option<std::time::Duration>,
}

impl ClientBuilder {
    pub fn new() -> Self {
        ClientBuilder {
            auth: auth::Auth::default(),
            config: oci::native::ClientConfig::default(),
            lock_timeout: None,
        }
    }

    /// Creates a client configured from standard environment variables.
    ///
    /// Reads `OCX_INSECURE_REGISTRIES` to configure plain-HTTP registries.
    pub fn from_env() -> Client {
        ClientBuilder::new()
            .plain_http_registries(crate::env::insecure_registries())
            .build()
    }

    /// Registries that should be contacted over plain HTTP instead of HTTPS.
    pub fn plain_http_registries(mut self, registries: Vec<String>) -> Self {
        if !registries.is_empty() {
            self.config.protocol = oci::native::ClientProtocol::HttpsExcept(registries);
        }
        self
    }

    /// Timeout for acquiring file locks during install and status checks.
    pub fn lock_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.lock_timeout = Some(timeout);
        self
    }

    pub fn build(self) -> Client {
        let transport = NativeTransport::new(oci::native::Client::new(self.config), self.auth);
        Client {
            transport: Box::new(transport),
            lock_timeout: self.lock_timeout.unwrap_or(std::time::Duration::from_secs(30)),
            tag_chunk_size: 100,
            repository_chunk_size: 100,
        }
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}
