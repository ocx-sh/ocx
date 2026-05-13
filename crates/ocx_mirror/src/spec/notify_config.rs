// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Notification configuration for the test pipeline.
//!
//! [`NotifyConfig`] declares which channels receive post-run notifications.
//! Currently only Discord webhooks are supported.

use serde::Deserialize;

/// Discord webhook notification settings.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordConfig {
    /// Name of the GitHub Actions secret that holds the webhook URL
    /// (e.g. `DISCORD_WEBHOOK_URL`). Must match `^[A-Z][A-Z0-9_]+$`.
    /// The renderer rejects any value containing `discord.com`,
    /// `discordapp.com`, or matching `^https?://` (R3 mitigation).
    pub webhook_secret: String,
}

/// Top-level notification block.
#[derive(Debug, Clone, Deserialize)]
pub struct NotifyConfig {
    /// Discord webhook configuration.
    #[serde(default)]
    pub discord: Option<DiscordConfig>,
}
