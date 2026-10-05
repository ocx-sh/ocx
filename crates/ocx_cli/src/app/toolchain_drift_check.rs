// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::Context;

/// Background notice that a locked tool's tag has moved since `ocx.lock` was written.
///
/// Never fails the command. Not implemented yet: returns without probing.
pub async fn check_for_toolchain_drift(_ctx: &Context) {}
