// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Discord webhook payload types and HTTP POST helper.
//!
//! Used by `ocx-mirror pipeline notify` to report pipeline outcomes.
//!
//! # Payload shape
//!
//! ```json
//! {
//!   "embeds": [{ "title": "...", "color": 3066993, "fields": [...], "url": "..." }]
//! }
//! ```
//!
//! No `username` field is sent: Discord uses the webhook's configured bot
//! name (e.g. "Captain Mirror") so server admins control the bot identity.
//!
//! # Color codes
//!
//! | Status | Color | Hex |
//! |---|---|---|
//! | Published | Green | `0x2ECC71` (3066993) |
//! | Partial | Yellow | `0xF1C40F` (15844367) |
//! | Failed | Red | `0xE74C3C` (15158332) |

use serde::Serialize;

use crate::error::MirrorError;

/// A single field in a Discord embed.
#[derive(Debug, Clone, Serialize)]
pub struct DiscordEmbedField {
    /// Field label.
    pub name: String,
    /// Field value.
    pub value: String,
    /// When `true`, Discord renders this field inline with adjacent inline fields.
    pub inline: bool,
}

/// Thumbnail image attached to a Discord embed.
#[derive(Debug, Clone, Serialize)]
pub struct DiscordEmbedThumbnail {
    /// Public HTTPS URL of the image (Discord fetches it server-side).
    pub url: String,
}

/// Author strip rendered at the top of a Discord embed (above the title).
///
/// Renders as a small icon plus a clickable name. We use it to surface the
/// upstream project homepage, since Discord's `thumbnail` field is decorative
/// only — there is no way to make the thumbnail image itself a hyperlink.
#[derive(Debug, Clone, Serialize)]
pub struct DiscordEmbedAuthor {
    /// Display text (clickable when `url` is set).
    pub name: String,
    /// URL the author name links to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Small icon shown to the left of `name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

/// A Discord embed object.
#[derive(Debug, Clone, Serialize)]
pub struct DiscordEmbed {
    /// Embed title (appears as the clickable link text when `url` is set).
    pub title: String,
    /// Decimal RGB color (e.g. `3066993` for `#2ECC71` green).
    pub color: u32,
    /// Optional URL that makes `title` a clickable link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Optional free-form text between the title and the fields. Used to
    /// surface the canonical OCI identifier (e.g. `ocx.sh/shfmt`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional author strip rendered above the title — used to link the
    /// embed back to the upstream project page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<DiscordEmbedAuthor>,
    /// Optional thumbnail image rendered in the top-right of the embed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<DiscordEmbedThumbnail>,
    /// Structured fields shown as a grid below the description.
    pub fields: Vec<DiscordEmbedField>,
}

/// Full Discord webhook payload.
///
/// No `username` override: Discord uses the webhook's configured bot name.
#[derive(Debug, Clone, Serialize)]
pub struct DiscordWebhookPayload {
    /// Up to 10 embeds (Discord limit).
    pub embeds: Vec<DiscordEmbed>,
}

/// POST a [`DiscordWebhookPayload`] to `webhook_url`.
///
/// # Errors
///
/// - [`MirrorError::WebhookUnavailable`] on 5xx or network timeout.
/// - [`MirrorError::WebhookPermissionDenied`] on 401/403.
pub async fn post(webhook_url: &str, payload: &DiscordWebhookPayload) -> Result<(), MirrorError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| MirrorError::WebhookUnavailable(format!("failed to build HTTP client: {e}")))?;

    // Do not log the URL — webhook secret must not appear in traces or logs.
    tracing::debug!("posting to webhook");

    let response = client.post(webhook_url).json(payload).send().await.map_err(|e| {
        if e.is_timeout() {
            MirrorError::WebhookUnavailable(format!("request timed out: {e}"))
        } else {
            MirrorError::WebhookUnavailable(format!("network error: {e}"))
        }
    })?;

    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    let code = status.as_u16();
    if code == 401 || code == 403 {
        return Err(MirrorError::WebhookPermissionDenied(format!(
            "HTTP {code}: check webhook secret rotation"
        )));
    }
    if status.is_server_error() {
        return Err(MirrorError::WebhookUnavailable(format!("HTTP {code}: server error")));
    }
    // Other 4xx
    Err(MirrorError::WebhookUnavailable(format!(
        "HTTP {code}: unexpected client error"
    )))
}

/// Discord color codes per D10 taxonomy.
pub mod colors {
    /// Green: `#2ECC71` (3066993) — published successfully.
    pub const GREEN: u32 = 0x2ECC71;
    /// Yellow: `#F1C40F` (15844367) — partial push (some platforms failed).
    pub const YELLOW: u32 = 0xF1C40F;
    /// Red: `#E74C3C` (15158332) — all platforms failed.
    pub const RED: u32 = 0xE74C3C;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── §3.9 S9: Discord payload type tests ────────────────────────────────

    #[test]
    fn discord_embed_field_serializes_correctly() {
        // §3.9: Embed field structure matches design spec §2.5 payload shape
        let field = DiscordEmbedField {
            name: "Platforms".to_string(),
            value: "linux/amd64, darwin/arm64".to_string(),
            inline: false,
        };
        let value: serde_json::Value = serde_json::to_value(&field).unwrap();
        assert_eq!(value["name"].as_str().unwrap(), "Platforms");
        assert_eq!(value["value"].as_str().unwrap(), "linux/amd64, darwin/arm64");
        assert!(!value["inline"].as_bool().unwrap());
    }

    #[test]
    fn discord_embed_serializes_with_all_required_fields() {
        // §3.9: Discord embed JSON shape matches design spec §2.5
        let embed = DiscordEmbed {
            title: "📦 cmake: published 3.29.0".to_string(),
            color: colors::GREEN,
            url: Some("https://github.com/ocx-sh/mirror-cmake/actions/runs/12345".to_string()),
            description: None,
            author: None,
            thumbnail: None,
            fields: vec![
                DiscordEmbedField {
                    name: "Platforms".to_string(),
                    value: "linux/amd64, linux/arm64".to_string(),
                    inline: false,
                },
                DiscordEmbedField {
                    name: "Cascade".to_string(),
                    value: "3.29.0, 3.29, 3, latest".to_string(),
                    inline: false,
                },
            ],
        };

        let value: serde_json::Value = serde_json::to_value(&embed).unwrap();
        assert_eq!(value["title"].as_str().unwrap(), "📦 cmake: published 3.29.0");
        assert_eq!(value["color"].as_u64().unwrap(), colors::GREEN as u64);
        assert!(value.get("url").is_some(), "url must be present");
        assert_eq!(value["fields"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn discord_embed_url_omitted_when_none() {
        // §3.9: url field is skip_serializing_if = None
        let embed = DiscordEmbed {
            title: "Test".to_string(),
            color: colors::RED,
            url: None,
            description: None,
            author: None,
            thumbnail: None,
            fields: vec![],
        };
        let value: serde_json::Value = serde_json::to_value(&embed).unwrap();
        assert!(value.get("url").is_none(), "url must be absent when None (not null)");
    }

    #[test]
    fn discord_embed_thumbnail_omitted_when_none() {
        // Thumbnail is optional and must be absent (not null) when unset.
        let embed = DiscordEmbed {
            title: "Test".to_string(),
            color: colors::GREEN,
            url: None,
            description: None,
            author: None,
            thumbnail: None,
            fields: vec![],
        };
        let value: serde_json::Value = serde_json::to_value(&embed).unwrap();
        assert!(
            value.get("thumbnail").is_none(),
            "thumbnail must be absent when None: {value}"
        );
    }

    #[test]
    fn discord_embed_thumbnail_serializes_with_url() {
        let embed = DiscordEmbed {
            title: "Test".to_string(),
            color: colors::GREEN,
            url: None,
            description: None,
            author: None,
            thumbnail: Some(DiscordEmbedThumbnail {
                url: "https://raw.githubusercontent.com/owner/repo/main/logo.svg".to_string(),
            }),
            fields: vec![],
        };
        let value: serde_json::Value = serde_json::to_value(&embed).unwrap();
        let thumb = value.get("thumbnail").expect("thumbnail must be present when Some");
        assert_eq!(
            thumb["url"].as_str().unwrap(),
            "https://raw.githubusercontent.com/owner/repo/main/logo.svg"
        );
    }

    #[test]
    fn discord_payload_serializes_without_username_field() {
        // Webhook bot name (e.g. "Captain Mirror") is owned by server admins;
        // payload never carries a `username` override.
        let payload = DiscordWebhookPayload { embeds: vec![] };
        let value: serde_json::Value = serde_json::to_value(&payload).unwrap();
        assert!(value.get("username").is_none(), "username must be absent: {value}");
    }

    #[test]
    fn green_color_matches_design_spec() {
        // §3.9: Color codes must match design spec §2.5 exactly
        assert_eq!(colors::GREEN, 0x2ECC71, "GREEN must be 0x2ECC71 (3066993)");
        assert_eq!(colors::GREEN, 3_066_993);
    }

    #[test]
    fn yellow_color_matches_design_spec() {
        assert_eq!(colors::YELLOW, 0xF1C40F, "YELLOW must be 0xF1C40F (15844367)");
        assert_eq!(colors::YELLOW, 15_844_367);
    }

    #[test]
    fn red_color_matches_design_spec() {
        assert_eq!(colors::RED, 0xE74C3C, "RED must be 0xE74C3C (15158332)");
        assert_eq!(colors::RED, 15_158_332);
    }

    // ── §3.9 S9: discord::post() HTTP tests ────────────────────────────────

    /// Install the rustls crypto provider if not already installed.
    ///
    /// Tests run without `main()`, so the provider must be initialized explicitly.
    /// `install_default()` returns `Err` if already set — silently ignore.
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    /// Spawn a minimal one-shot HTTP server that accepts one request and responds
    /// with `status_code`. Returns the server URL.
    async fn one_shot_http_server(status_code: u16) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/webhook");

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let _ = stream.read(&mut buf).await;
            let response = format!("HTTP/1.1 {status_code} \r\nContent-Length: 0\r\n\r\n");
            let _ = stream.write_all(response.as_bytes()).await;
        });

        url
    }

    #[tokio::test]
    async fn discord_post_2xx_returns_ok() {
        // §3.9: 2xx response from Discord → Ok(())
        ensure_crypto_provider();
        let url = one_shot_http_server(204).await;
        let payload = DiscordWebhookPayload { embeds: vec![] };
        let result = post(&url, &payload).await;
        assert!(matches!(result, Ok(())), "2xx must return Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn discord_post_5xx_returns_webhook_unavailable() {
        // §3.9: 5xx → WebhookUnavailable (exit 69)
        ensure_crypto_provider();
        let url = one_shot_http_server(503).await;
        let payload = DiscordWebhookPayload { embeds: vec![] };
        let result = post(&url, &payload).await;
        assert!(
            matches!(result, Err(MirrorError::WebhookUnavailable(_))),
            "5xx must return WebhookUnavailable: {result:?}"
        );
    }

    #[tokio::test]
    async fn discord_post_401_returns_permission_denied() {
        // §3.9: 401 → WebhookPermissionDenied (exit 77)
        ensure_crypto_provider();
        let url = one_shot_http_server(401).await;
        let payload = DiscordWebhookPayload { embeds: vec![] };
        let result = post(&url, &payload).await;
        assert!(
            matches!(result, Err(MirrorError::WebhookPermissionDenied(_))),
            "401 must return WebhookPermissionDenied: {result:?}"
        );
    }

    #[tokio::test]
    async fn discord_post_403_returns_permission_denied() {
        // §3.9: 403 → WebhookPermissionDenied (exit 77)
        ensure_crypto_provider();
        let url = one_shot_http_server(403).await;
        let payload = DiscordWebhookPayload { embeds: vec![] };
        let result = post(&url, &payload).await;
        assert!(
            matches!(result, Err(MirrorError::WebhookPermissionDenied(_))),
            "403 must return WebhookPermissionDenied: {result:?}"
        );
    }
}
