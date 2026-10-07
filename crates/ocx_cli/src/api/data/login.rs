// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_oci::RegistryHost;
use serde::Serialize;

use crate::api::Printable;

/// Successful `ocx login` result.
///
/// Plain format: nothing on stdout — the human "Login succeeded" line is a
/// stderr diagnostic emitted via `Api::success`. stdout is the CLI's
/// machine interface and stays empty when there is no parseable payload.
#[derive(Serialize, schemars::JsonSchema)]
pub struct LoginResult {
    /// The registry the credentials were stored for.
    pub registry: RegistryHost,
    /// The user name the credentials were stored under.
    pub username: String,
}

impl Printable for LoginResult {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "LoginResult";

    fn print_plain(&self, _printer: &ocx_console::DataInterface) {
        // Empty: success is reported on stderr.
    }
}

/// Successful `ocx logout` result.
///
/// Plain format: nothing on stdout; success is reported on stderr.
#[derive(Serialize, schemars::JsonSchema)]
pub struct LogoutResult {
    /// The registry whose credentials were removed.
    pub registry: RegistryHost,
}

impl Printable for LogoutResult {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "LogoutResult";

    fn print_plain(&self, _printer: &ocx_console::DataInterface) {
        // Intentionally empty: success is reported on stderr.
    }
}
