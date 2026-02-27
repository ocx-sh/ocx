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
