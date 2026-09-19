// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_oci::Client;

/// Construction inputs for [`super::OciIndex`].
pub struct OciIndexConfig {
    pub client: Client,
}
