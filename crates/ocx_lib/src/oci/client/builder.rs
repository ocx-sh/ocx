use crate::{auth, oci};

use super::Client;

pub struct ClientBuilder {
    auth: auth::Auth,
    config: oci::native::ClientConfig,
}

impl ClientBuilder {
    pub fn new() -> Self {
        ClientBuilder {
            auth: auth::Auth::default(),
            config: oci::native::ClientConfig::default(),
        }
    }

    /// Registries that should be contacted over plain HTTP instead of HTTPS.
    pub fn plain_http_registries(mut self, registries: Vec<String>) -> Self {
        if !registries.is_empty() {
            self.config.protocol = oci::native::ClientProtocol::HttpsExcept(registries);
        }
        self
    }

    pub fn build(self) -> Client {
        Client {
            auth: self.auth,
            client: oci::native::Client::new(self.config),
            lock_timeout: std::time::Duration::from_secs(30),
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
