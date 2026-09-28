// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The default index base URL — the value `[registries."<ns>"] index` overrides.
//!
//! Lives here, not in `ocx_index`, because the crate map forbids `ocx_config` naming `ocx_index`.

/// Default base URL when no `[registries."<ns>"] index` field is configured.
pub const DEFAULT_INDEX_BASE_URL: &str = "https://index.ocx.sh";
