// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Deprecated command spellings, kept alive for exactly one release pair.
//!
//! Every name renamed in 0.6 dispatches through this module, so one grep finds
//! the whole set and 0.7 removes it by deleting this file together with the
//! hidden `Command` / `Package` variants that call it. Nothing else may depend
//! on it.
//!
//! An old spelling is a *hidden command*, never a clap alias: `ArgMatches`
//! reports the canonical name, so an alias is invisible to the code and could
//! not warn. The hidden variant also keeps its own
//! [`crate::app::canonical_command_name`] arm reporting the **old** string, so
//! the frozen v1 error envelope still distinguishes a deprecated invocation
//! from a current one and nothing already emitted by a released binary changes
//! meaning.

use crate::app::Context;

/// The release that deletes this module and every spelling in it.
const REMOVAL_RELEASE: &str = "0.7";

/// Warn on stderr that `old` has been renamed to `new`.
///
/// Fires once per invocation by construction — one process dispatches one
/// command. Routed through [`Context::ui`], so the notice never reaches stdout
/// and degrades to `log::warn!` when quiet or non-interactive.
pub fn warn_renamed(context: &Context, old: &str, new: &str) {
    context.ui().warn(format!(
        "`ocx {old}` is renamed to `ocx {new}` and is removed in {REMOVAL_RELEASE}"
    ));
}
