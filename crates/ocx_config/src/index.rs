// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The default index base URL — the value `[registries."<ns>"] index` overrides.
//!
//! It sits in the config tier and not beside the client that fetches through it
//! because it is a **configuration default**: the loader hands it out when no
//! namespace names an index of its own, and `oci::index` reads it back the same
//! way it reads every other configured value. Owning it here is what keeps
//! `ocx_config` from naming `ocx_index`, a direction the crate map forbids and
//! the loader used to take four times over just to spell one URL.

/// Default base URL when no `[registries."<ns>"] index` field is configured.
pub const DEFAULT_INDEX_BASE_URL: &str = "https://index.ocx.sh";
