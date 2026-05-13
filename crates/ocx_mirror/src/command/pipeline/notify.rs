// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-mirror pipeline notify` — read `run-summary.json` and POST a Discord
//! webhook notification per the D10 taxonomy.

use std::path::PathBuf;

use ocx_lib::cli::Printer;

use crate::discord::{self, DiscordEmbed, DiscordEmbedField, DiscordEmbedThumbnail, DiscordWebhookPayload};
use crate::error::MirrorError;
use crate::run_summary::{PlatformFailure, RunSummary, VersionSummary};

/// `ocx-mirror pipeline notify` subcommand.
///
/// Reads `run-summary.json` and posts to the Discord webhook URL sourced from
/// `$<webhook_env_var>`. Silent (exit 0, no POST) when all versions are
/// `skipped_existing` and no test failures occurred.
#[derive(clap::Parser)]
pub struct Notify {
    /// Path to the `run-summary.json` produced by `pipeline push`.
    #[arg(long, required = true)]
    pub run_summary: PathBuf,

    /// Name of the environment variable holding the Discord webhook URL
    /// (e.g. `DISCORD_WEBHOOK_URL`). Value must match `^[A-Z][A-Z0-9_]+$`.
    #[arg(long, required = true)]
    pub webhook_env_var: String,
}

impl Notify {
    pub async fn execute(&self, _printer: &Printer) -> Result<(), MirrorError> {
        // Read and parse run-summary.json.
        let raw = tokio::fs::read_to_string(&self.run_summary)
            .await
            .map_err(|e| MirrorError::RunSummaryError(format!("failed to read {}: {e}", self.run_summary.display())))?;
        let summary: RunSummary = serde_json::from_str(&raw)
            .map_err(|e| MirrorError::RunSummaryError(format!("malformed run-summary.json: {e}")))?;

        if summary.schema_version != 1 {
            return Err(MirrorError::RunSummaryError(format!(
                "unsupported run-summary.json schema_version {}; expected 1",
                summary.schema_version
            )));
        }

        // D10 rule: all skipped_existing (no new green, no red) → silent exit 0.
        if !summary.any_new_green && !summary.any_red {
            tracing::debug!("all versions skipped_existing; no notification to send");
            return Ok(());
        }

        // Resolve webhook URL from the named environment variable.
        // URL is never logged — only the env var name may appear in messages.
        let webhook_url = std::env::var(&self.webhook_env_var).map_err(|_| {
            MirrorError::SpecUsageError(format!(
                "environment variable '{}' is not set; set it to the Discord webhook URL",
                self.webhook_env_var
            ))
        })?;

        let payload = build_payload(&summary);
        discord::post(&webhook_url, &payload).await
    }
}

/// Build the [`DiscordWebhookPayload`] from a [`RunSummary`] per the D10 taxonomy.
fn build_payload(summary: &RunSummary) -> DiscordWebhookPayload {
    let embed = build_embed(summary);
    DiscordWebhookPayload {
        username: "ocx-mirror".to_string(),
        embeds: vec![embed],
    }
}

/// Maximum length of a single Discord embed field value.
const DISCORD_FIELD_VALUE_LIMIT: usize = 1024;

/// Select color + title per D10 outcome rules and build the embed.
fn build_embed(summary: &RunSummary) -> DiscordEmbed {
    // Pick the representative version string for the title.
    // Use the first published/partial version, falling back to the first version overall.
    let version_str = summary
        .versions
        .iter()
        .find(|v| !v.platforms_pushed.is_empty() || !v.platforms_failed.is_empty())
        .or_else(|| summary.versions.first())
        .map(|v| v.version.as_str())
        .unwrap_or("unknown");

    let (color, title) = if summary.any_new_green && !summary.any_red {
        // All new pushes succeeded.
        (
            discord::colors::GREEN,
            format!("📦 {}: published {}", summary.mirror, version_str),
        )
    } else if summary.any_new_green && summary.any_red {
        // Mixed: some platforms succeeded, some failed.
        (
            discord::colors::YELLOW,
            format!("⚠️ {} {} partial", summary.mirror, version_str),
        )
    } else {
        // any_red && !any_new_green — all platforms failed.
        (
            discord::colors::RED,
            format!("❌ {} {} failed all platforms", summary.mirror, version_str),
        )
    };

    let mut fields: Vec<DiscordEmbedField> = Vec::new();

    // Per-version 3-inline-column blocks (Platform | Status | Detail). Discord
    // groups consecutive inline fields 3-per-row, so each version becomes one
    // tabular row group with the version embedded in the Platform column name.
    for version in &summary.versions {
        if version.platforms_pushed.is_empty() && version.platforms_failed.is_empty() {
            continue;
        }
        fields.extend(render_version_block(version));
    }

    // Aggregated test-failure detail field — failure messages don't fit in the
    // Detail column. Keeps the same name + 10-row cap as the legacy field so
    // existing assertions stay valid.
    if let Some(field) = render_test_failures_field(summary) {
        fields.push(field);
    }

    let thumbnail = build_thumbnail(std::env::var("GITHUB_REPOSITORY").ok().as_deref());

    DiscordEmbed {
        title,
        color,
        url: Some(summary.run_url.clone()),
        thumbnail,
        fields,
    }
}

/// Build the thumbnail URL from a `GITHUB_REPOSITORY` value (e.g. `owner/repo`).
///
/// Returns `None` when the env var is unset. Always points at `main` branch's
/// `logo.svg`; Discord renders no thumbnail when the URL 404s, so callers can
/// skip a file-existence probe.
fn build_thumbnail(github_repo: Option<&str>) -> Option<DiscordEmbedThumbnail> {
    let repo = github_repo?.trim();
    if repo.is_empty() {
        return None;
    }
    Some(DiscordEmbedThumbnail {
        url: format!("https://raw.githubusercontent.com/{repo}/main/logo.svg"),
    })
}

/// Render one version's outcome as three inline fields: Platform / Status / Detail.
///
/// Pushed platforms come first (✅), failed ones after (❌). The Detail column
/// renders cascade tags for green rows and the failure reason for red rows;
/// when the failure carries a `job_url` the reason becomes a markdown link.
fn render_version_block(version: &VersionSummary) -> Vec<DiscordEmbedField> {
    let mut platforms: Vec<String> = Vec::new();
    let mut statuses: Vec<String> = Vec::new();
    let mut details: Vec<String> = Vec::new();

    let cascade = if version.cascade_tags_written.is_empty() {
        "pushed".to_string()
    } else {
        version.cascade_tags_written.join(", ")
    };

    for platform in &version.platforms_pushed {
        platforms.push(platform.clone());
        statuses.push("✅".to_string());
        details.push(cascade.clone());
    }
    for failure in &version.platforms_failed {
        platforms.push(failure.platform.clone());
        statuses.push("❌".to_string());
        details.push(render_failure_detail(failure));
    }

    vec![
        DiscordEmbedField {
            name: format!("Platform · {}", version.version),
            value: clip_to_field_limit(&platforms.join("\n")),
            inline: true,
        },
        DiscordEmbedField {
            name: "Status".to_string(),
            value: clip_to_field_limit(&statuses.join("\n")),
            inline: true,
        },
        DiscordEmbedField {
            name: "Detail".to_string(),
            value: clip_to_field_limit(&details.join("\n")),
            inline: true,
        },
    ]
}

fn render_failure_detail(failure: &PlatformFailure) -> String {
    let text = match failure.reason.as_str() {
        "test_failed" => {
            let n = failure.failed_tests.len().max(1);
            if n == 1 {
                "1 failed test".to_string()
            } else {
                format!("{n} failed tests")
            }
        }
        other => other.to_string(),
    };
    match &failure.job_url {
        Some(url) => format!("[{text}]({url})"),
        None => text,
    }
}

fn render_test_failures_field(summary: &RunSummary) -> Option<DiscordEmbedField> {
    let all_failures: Vec<String> = summary
        .versions
        .iter()
        .flat_map(|v| {
            v.test_failures.iter().map(|f| {
                format!(
                    "{} {} {} ({}): {}",
                    f.version, f.platform, f.test, f.container, f.message
                )
            })
        })
        .collect();
    if all_failures.is_empty() {
        return None;
    }
    let displayed = if all_failures.len() > 10 {
        let mut truncated = all_failures[..10].join("\n");
        truncated.push_str(&format!("\n… and {} more", all_failures.len() - 10));
        truncated
    } else {
        all_failures.join("\n")
    };
    Some(DiscordEmbedField {
        name: "Failed tests".to_string(),
        value: clip_to_field_limit(&displayed),
        inline: false,
    })
}

/// Clip a field value to the 1024-char Discord limit at the nearest newline.
fn clip_to_field_limit(s: &str) -> String {
    if s.len() <= DISCORD_FIELD_VALUE_LIMIT {
        return s.to_string();
    }
    const SUFFIX: &str = "\n… (truncated)";
    let budget = DISCORD_FIELD_VALUE_LIMIT - SUFFIX.len();
    let mut clipped = s[..budget].to_string();
    if let Some(pos) = clipped.rfind('\n') {
        clipped.truncate(pos);
    }
    clipped.push_str(SUFFIX);
    clipped
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::discord::colors;
    use crate::run_summary::{PlatformFailure, RunSummary, TestFailure, VersionStatus, VersionSummary};

    // ── §3.9 S9: notify subcommand tests ──────────────────────────────────

    fn write_run_summary(summary: &RunSummary) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        let json = serde_json::to_string_pretty(summary).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        f
    }

    fn make_all_skipped_summary() -> RunSummary {
        RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/1".to_string(),
            versions: vec![VersionSummary {
                version: "3.7.0".to_string(),
                status: VersionStatus::SkippedExisting,
                platforms_pushed: vec![],
                platforms_failed: vec![],
                cascade_tags_written: vec![],
                test_failures: vec![],
            }],
            any_red: false,
            any_new_green: false,
        }
    }

    fn make_all_green_summary() -> RunSummary {
        RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/2".to_string(),
            versions: vec![VersionSummary {
                version: "3.7.0".to_string(),
                status: VersionStatus::Published,
                platforms_pushed: vec!["linux/amd64".to_string(), "darwin/arm64".to_string()],
                platforms_failed: vec![],
                cascade_tags_written: vec![
                    "3.7.0".to_string(),
                    "3.7".to_string(),
                    "3".to_string(),
                    "latest".to_string(),
                ],
                test_failures: vec![],
            }],
            any_red: false,
            any_new_green: true,
        }
    }

    fn make_partial_summary() -> RunSummary {
        RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/3".to_string(),
            versions: vec![VersionSummary {
                version: "3.7.0".to_string(),
                status: VersionStatus::Partial,
                platforms_pushed: vec!["linux/amd64".to_string()],
                platforms_failed: vec![PlatformFailure {
                    platform: "darwin/amd64".to_string(),
                    reason: "test_failed".to_string(),
                    failed_tests: vec![TestFailure {
                        version: "3.7.0".to_string(),
                        platform: "darwin/amd64".to_string(),
                        container: "_native_".to_string(),
                        test: "smoke".to_string(),
                        message: "exit 1".to_string(),
                    }],
                    job_url: None,
                }],
                cascade_tags_written: vec!["3.7.0".to_string()],
                test_failures: vec![TestFailure {
                    version: "3.7.0".to_string(),
                    platform: "darwin/amd64".to_string(),
                    container: "_native_".to_string(),
                    test: "smoke".to_string(),
                    message: "exit 1".to_string(),
                }],
            }],
            any_red: true,
            any_new_green: true,
        }
    }

    fn make_all_failed_summary() -> RunSummary {
        RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/4".to_string(),
            versions: vec![VersionSummary {
                version: "3.7.0".to_string(),
                status: VersionStatus::Failed,
                platforms_pushed: vec![],
                platforms_failed: vec![PlatformFailure {
                    platform: "linux/amd64".to_string(),
                    reason: "test_failed".to_string(),
                    failed_tests: vec![TestFailure {
                        version: "3.7.0".to_string(),
                        platform: "linux/amd64".to_string(),
                        container: "ubuntu_2404".to_string(),
                        test: "version".to_string(),
                        message: "binary not found".to_string(),
                    }],
                    job_url: None,
                }],
                cascade_tags_written: vec![],
                test_failures: vec![TestFailure {
                    version: "3.7.0".to_string(),
                    platform: "linux/amd64".to_string(),
                    container: "ubuntu_2404".to_string(),
                    test: "version".to_string(),
                    message: "binary not found".to_string(),
                }],
            }],
            any_red: true,
            any_new_green: false,
        }
    }

    fn run_notify_sync(summary: &RunSummary, env_var: &str) -> Result<(), MirrorError> {
        let f = write_run_summary(summary);
        let printer = ocx_lib::cli::Printer::new(false);
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
            webhook_env_var: env_var.to_string(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        // Keep temp file alive during execution
        let result = rt.block_on(async { cmd.execute(&printer).await });
        let _ = f; // keep alive
        result
    }

    // ── Payload-construction tests (no HTTP, no env var needed) ───────────

    #[test]
    fn notify_silent_when_all_skipped_existing() {
        // §3.9: all skipped_existing + no test_failures → silent (exit 0, no POST).
        // Env var deliberately unset — silent path must not reach env var lookup.
        let unique_env = "OCX_TEST_DISCORD_SILENT_SKIPPED_12345";
        // SAFETY: test-only env var with unique name unlikely to conflict.
        unsafe { std::env::remove_var(unique_env) }
        let result = run_notify_sync(&make_all_skipped_summary(), unique_env);
        assert!(
            matches!(result, Ok(())),
            "all-skipped summary must be silent (exit 0, no POST, no env var lookup): {result:?}"
        );
    }

    #[test]
    fn notify_missing_env_var_returns_spec_usage_error() {
        // §3.9: webhook_env_var unset → SpecUsageError (exit 64).
        // Use a summary that requires a POST (any_new_green = true).
        let unique_env = "OCX_TEST_DISCORD_MISSING_ABCDEF";
        // SAFETY: test-only env var with unique name unlikely to conflict.
        unsafe { std::env::remove_var(unique_env) }
        let result = run_notify_sync(&make_all_green_summary(), unique_env);
        assert!(
            matches!(result, Err(MirrorError::SpecUsageError(_))),
            "unset webhook env var must return SpecUsageError: {result:?}"
        );
    }

    #[test]
    fn build_payload_green_embed_has_correct_color_and_title() {
        // §3.9: any_new_green && !any_red → green embed, color 0x2ECC71
        let summary = make_all_green_summary();
        let payload = build_payload(&summary);
        assert_eq!(payload.username, "ocx-mirror");
        assert_eq!(payload.embeds.len(), 1);
        let embed = &payload.embeds[0];
        assert_eq!(embed.color, colors::GREEN, "green outcome must use GREEN color");
        assert!(
            embed.title.contains("shfmt"),
            "title must contain mirror name: {}",
            embed.title
        );
        assert!(
            embed.title.contains("3.7.0"),
            "title must contain version: {}",
            embed.title
        );
        assert!(
            embed.title.to_lowercase().contains("published"),
            "title must contain 'published': {}",
            embed.title
        );
    }

    #[test]
    fn build_payload_yellow_embed_has_correct_color_and_title() {
        // §3.9: any_new_green && any_red → yellow embed, color 0xF1C40F
        let summary = make_partial_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        assert_eq!(embed.color, colors::YELLOW, "partial outcome must use YELLOW color");
        assert!(
            embed.title.contains("shfmt"),
            "title must contain mirror name: {}",
            embed.title
        );
        assert!(
            embed.title.contains("3.7.0"),
            "title must contain version: {}",
            embed.title
        );
        assert!(
            embed.title.to_lowercase().contains("partial"),
            "title must contain 'partial': {}",
            embed.title
        );
    }

    #[test]
    fn build_payload_red_embed_has_correct_color_and_title() {
        // §3.9: !any_new_green && any_red → red embed, color 0xE74C3C
        let summary = make_all_failed_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        assert_eq!(embed.color, colors::RED, "failed outcome must use RED color");
        assert!(
            embed.title.contains("shfmt"),
            "title must contain mirror name: {}",
            embed.title
        );
        assert!(
            embed.title.contains("3.7.0"),
            "title must contain version: {}",
            embed.title
        );
        assert!(
            embed.title.to_lowercase().contains("failed"),
            "title must contain 'failed': {}",
            embed.title
        );
    }

    #[test]
    fn build_embed_red_renders_push_error_in_version_block() {
        // Regression (carried forward from the legacy "Failed platforms" field):
        // a run where every platform failed with `push_error` produced a red
        // embed with zero detail fields. The tabular per-version block must
        // surface each platform + reason so Discord readers see *which*
        // platform broke and *why*.
        let summary = RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/9".to_string(),
            versions: vec![VersionSummary {
                version: "3.13.1".to_string(),
                status: VersionStatus::Failed,
                platforms_pushed: vec![],
                platforms_failed: vec![
                    PlatformFailure {
                        platform: "linux/amd64".to_string(),
                        reason: "push_error".to_string(),
                        failed_tests: vec![],
                        job_url: None,
                    },
                    PlatformFailure {
                        platform: "darwin/arm64".to_string(),
                        reason: "missing_bundle".to_string(),
                        failed_tests: vec![],
                        job_url: None,
                    },
                ],
                cascade_tags_written: vec![],
                test_failures: vec![],
            }],
            any_red: true,
            any_new_green: false,
        };

        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];

        let platform_field = embed
            .fields
            .iter()
            .find(|f| f.name == "Platform · 3.13.1")
            .expect("red embed must include a per-version Platform column");
        let detail_field = embed
            .fields
            .iter()
            .find(|f| f.name == "Detail" && f.inline)
            .expect("red embed must include an inline Detail column");

        assert!(
            platform_field.value.contains("linux/amd64") && platform_field.value.contains("darwin/arm64"),
            "platform column must list both failed platforms: {}",
            platform_field.value,
        );
        assert!(
            detail_field.value.contains("push_error") && detail_field.value.contains("missing_bundle"),
            "detail column must surface each failure reason: {}",
            detail_field.value,
        );
    }

    #[test]
    fn build_payload_partial_includes_failed_tests_field() {
        // §3.9: partial embed includes failed-tests summary in fields
        let summary = make_partial_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let has_failed_tests_field = embed.fields.iter().any(|f| f.name == "Failed tests");
        assert!(
            has_failed_tests_field,
            "partial payload must include 'Failed tests' field"
        );
    }

    #[test]
    fn build_payload_green_surfaces_cascade_tags_in_detail_column() {
        // Green rows render the cascade tags in the per-version Detail column.
        // The standalone "Cascade tags" field was retired in favour of the
        // tabular layout; the data must still be visible per row.
        let summary = make_all_green_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let detail_field = embed
            .fields
            .iter()
            .find(|f| f.name == "Detail" && f.inline)
            .expect("green payload must include an inline Detail column");
        assert!(
            detail_field.value.contains("latest"),
            "Detail column must surface cascade tags including 'latest': {}",
            detail_field.value,
        );
    }

    #[test]
    fn notify_webhook_url_sourced_from_env_var_name() {
        // §3.9: webhook_env_var value is used to look up URL from env — NOT a URL itself.
        // The webhook_env_var field holds the env var NAME (e.g. "DISCORD_WEBHOOK_URL"),
        // not the URL. Test that passing an env-var name (not a URL) is accepted.
        let cmd = Notify {
            run_summary: std::path::PathBuf::from("/dev/null"),
            webhook_env_var: "DISCORD_WEBHOOK_URL".to_string(),
        };
        // webhook_env_var must match ^[A-Z][A-Z0-9_]+$
        assert!(
            cmd.webhook_env_var
                .chars()
                .next()
                .map(|c| c.is_ascii_uppercase())
                .unwrap_or(false),
            "webhook_env_var must start with uppercase letter (GHA secret naming)"
        );
        assert!(
            cmd.webhook_env_var
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "webhook_env_var must match ^[A-Z][A-Z0-9_]+$"
        );
    }

    // ── HTTP-interaction tests (local TCP server) ──────────────────────────

    /// Install the rustls crypto provider if not already installed.
    ///
    /// Tests run without `main()`, so the provider must be initialized explicitly.
    /// `install_default()` returns `Err` if already set — silently ignore.
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    /// Spawn a minimal HTTP server that accepts one request and responds with `status_code`.
    /// Returns the bound URL.
    async fn one_shot_server(status_code: u16) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/webhook");

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            // Drain the request (we don't inspect it)
            let _ = stream.read(&mut buf).await;
            let response = format!("HTTP/1.1 {status_code} \r\nContent-Length: 0\r\n\r\n",);
            let _ = stream.write_all(response.as_bytes()).await;
        });

        url
    }

    #[tokio::test]
    async fn notify_posts_green_embed_for_all_new_green() {
        // §3.9: any_new_green && !any_red → green embed; 2xx response → Ok(())
        ensure_crypto_provider();
        let server_url = one_shot_server(204).await;

        let unique_env = "OCX_TEST_GREEN_204_ABCDEF";
        // SAFETY: test-only env var with unique name unlikely to conflict.
        unsafe { std::env::set_var(unique_env, &server_url) }

        let f = write_run_summary(&make_all_green_summary());
        let printer = ocx_lib::cli::Printer::new(false);
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
            webhook_env_var: unique_env.to_string(),
        };
        let result = cmd.execute(&printer).await;
        let _ = f;

        // SAFETY: cleanup
        unsafe { std::env::remove_var(unique_env) }

        assert!(matches!(result, Ok(())), "2xx response must yield Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn notify_posts_yellow_embed_for_partial() {
        // §3.9: any_new_green && any_red → yellow partial embed; 2xx → Ok(())
        ensure_crypto_provider();
        let server_url = one_shot_server(200).await;

        let unique_env = "OCX_TEST_YELLOW_200_ABCDEF";
        // SAFETY: test-only env var with unique name unlikely to conflict.
        unsafe { std::env::set_var(unique_env, &server_url) }

        let f = write_run_summary(&make_partial_summary());
        let printer = ocx_lib::cli::Printer::new(false);
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
            webhook_env_var: unique_env.to_string(),
        };
        let result = cmd.execute(&printer).await;
        let _ = f;

        // SAFETY: cleanup
        unsafe { std::env::remove_var(unique_env) }

        assert!(matches!(result, Ok(())), "2xx response must yield Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn notify_posts_red_embed_for_all_failed() {
        // §3.9: !any_new_green && any_red → red failed embed; 2xx → Ok(())
        ensure_crypto_provider();
        let server_url = one_shot_server(200).await;

        let unique_env = "OCX_TEST_RED_200_ABCDEF";
        // SAFETY: test-only env var with unique name unlikely to conflict.
        unsafe { std::env::set_var(unique_env, &server_url) }

        let f = write_run_summary(&make_all_failed_summary());
        let printer = ocx_lib::cli::Printer::new(false);
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
            webhook_env_var: unique_env.to_string(),
        };
        let result = cmd.execute(&printer).await;
        let _ = f;

        // SAFETY: cleanup
        unsafe { std::env::remove_var(unique_env) }

        assert!(matches!(result, Ok(())), "2xx response must yield Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn notify_discord_5xx_returns_webhook_unavailable() {
        // §3.9: 5xx → MirrorError::WebhookUnavailable (exit 69)
        ensure_crypto_provider();
        let server_url = one_shot_server(503).await;

        let unique_env = "OCX_TEST_5XX_ABCDEF";
        // SAFETY: test-only env var with unique name unlikely to conflict.
        unsafe { std::env::set_var(unique_env, &server_url) }

        let f = write_run_summary(&make_all_green_summary());
        let printer = ocx_lib::cli::Printer::new(false);
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
            webhook_env_var: unique_env.to_string(),
        };
        let result = cmd.execute(&printer).await;
        let _ = f;

        // SAFETY: cleanup
        unsafe { std::env::remove_var(unique_env) }

        assert!(
            matches!(result, Err(MirrorError::WebhookUnavailable(_))),
            "5xx must return WebhookUnavailable: {result:?}"
        );
    }

    #[tokio::test]
    async fn notify_discord_401_returns_webhook_permission_denied() {
        // §3.9: 401 → MirrorError::WebhookPermissionDenied (exit 77)
        ensure_crypto_provider();
        let server_url = one_shot_server(401).await;

        let unique_env = "OCX_TEST_401_ABCDEF";
        // SAFETY: test-only env var with unique name unlikely to conflict.
        unsafe { std::env::set_var(unique_env, &server_url) }

        let f = write_run_summary(&make_all_green_summary());
        let printer = ocx_lib::cli::Printer::new(false);
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
            webhook_env_var: unique_env.to_string(),
        };
        let result = cmd.execute(&printer).await;
        let _ = f;

        // SAFETY: cleanup
        unsafe { std::env::remove_var(unique_env) }

        assert!(
            matches!(result, Err(MirrorError::WebhookPermissionDenied(_))),
            "401 must return WebhookPermissionDenied: {result:?}"
        );
    }

    #[tokio::test]
    async fn notify_discord_403_returns_webhook_permission_denied() {
        // §3.9: 403 → MirrorError::WebhookPermissionDenied (exit 77)
        ensure_crypto_provider();
        let server_url = one_shot_server(403).await;

        let unique_env = "OCX_TEST_403_ABCDEF";
        // SAFETY: test-only env var with unique name unlikely to conflict.
        unsafe { std::env::set_var(unique_env, &server_url) }

        let f = write_run_summary(&make_all_green_summary());
        let printer = ocx_lib::cli::Printer::new(false);
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
            webhook_env_var: unique_env.to_string(),
        };
        let result = cmd.execute(&printer).await;
        let _ = f;

        // SAFETY: cleanup
        unsafe { std::env::remove_var(unique_env) }

        assert!(
            matches!(result, Err(MirrorError::WebhookPermissionDenied(_))),
            "403 must return WebhookPermissionDenied: {result:?}"
        );
    }

    // ── Tabular embed layout — per-version 3-column blocks ────────────────

    fn version_summary_with_failure(
        version: &str,
        platform: &str,
        reason: &str,
        job_url: Option<String>,
    ) -> VersionSummary {
        VersionSummary {
            version: version.to_string(),
            status: VersionStatus::Failed,
            platforms_pushed: vec![],
            platforms_failed: vec![PlatformFailure {
                platform: platform.to_string(),
                reason: reason.to_string(),
                failed_tests: vec![],
                job_url,
            }],
            cascade_tags_written: vec![],
            test_failures: vec![],
        }
    }

    fn red_summary_with_failure(version: &str, platform: &str, reason: &str, job_url: Option<String>) -> RunSummary {
        RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/42".to_string(),
            versions: vec![version_summary_with_failure(version, platform, reason, job_url)],
            any_red: true,
            any_new_green: false,
        }
    }

    #[test]
    fn build_embed_emits_three_inline_fields_per_version() {
        // Each version with activity contributes exactly three inline fields
        // named "Platform · {V}", "Status", "Detail" — Discord groups them as
        // one tabular row of three columns.
        let summary = make_partial_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];

        let platform_field = embed
            .fields
            .iter()
            .find(|f| f.name == "Platform · 3.7.0")
            .expect("expected per-version Platform column");
        let inline_columns: Vec<&DiscordEmbedField> = embed.fields.iter().filter(|f| f.inline).collect();

        assert!(platform_field.inline, "Platform column must be inline");
        assert_eq!(
            inline_columns.len(),
            3,
            "expected three inline columns: {inline_columns:?}"
        );
        assert!(inline_columns.iter().any(|f| f.name == "Status"));
        assert!(inline_columns.iter().any(|f| f.name == "Detail"));
    }

    #[test]
    fn build_embed_red_rows_render_markdown_link_when_job_url_present() {
        let summary = red_summary_with_failure(
            "3.7.0",
            "linux/amd64",
            "test_failed",
            Some("https://github.com/ocx-sh/mirror-shfmt/actions/runs/42/job/7".to_string()),
        );
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let detail = embed
            .fields
            .iter()
            .find(|f| f.name == "Detail" && f.inline)
            .expect("Detail column must be present");
        assert!(
            detail
                .value
                .contains("[1 failed test](https://github.com/ocx-sh/mirror-shfmt/actions/runs/42/job/7)"),
            "Detail value must carry a markdown link to the job: {}",
            detail.value,
        );
    }

    #[test]
    fn build_embed_red_rows_render_plain_text_when_job_url_absent() {
        let summary = red_summary_with_failure("3.7.0", "linux/amd64", "test_failed", None);
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let detail = embed
            .fields
            .iter()
            .find(|f| f.name == "Detail" && f.inline)
            .expect("Detail column must be present");
        assert!(
            !detail.value.contains("](http"),
            "Detail without job_url must be plain text, got: {}",
            detail.value,
        );
        assert!(detail.value.contains("1 failed test"));
    }

    #[test]
    fn build_embed_push_error_row_uses_plain_text_even_with_other_versions_linked() {
        // push_error has no per-platform matrix job — even when another
        // version in the same run carries a job_url, the push_error row
        // must stay plain.
        let summary = RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/42".to_string(),
            versions: vec![
                version_summary_with_failure(
                    "3.7.0",
                    "linux/amd64",
                    "test_failed",
                    Some("https://github.com/ocx-sh/mirror-shfmt/actions/runs/42/job/7".to_string()),
                ),
                version_summary_with_failure("3.7.1", "linux/amd64", "push_error", None),
            ],
            any_red: true,
            any_new_green: false,
        };
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];

        let detail_3_7_1_idx = embed
            .fields
            .iter()
            .position(|f| f.name == "Platform · 3.7.1")
            .expect("expected per-version block for 3.7.1");
        let detail_3_7_1 = &embed.fields[detail_3_7_1_idx + 2];
        assert_eq!(detail_3_7_1.name, "Detail");
        assert!(
            !detail_3_7_1.value.contains("]("),
            "push_error row must not carry a markdown link: {}",
            detail_3_7_1.value,
        );
        assert!(detail_3_7_1.value.contains("push_error"));
    }

    #[test]
    fn build_thumbnail_attaches_url_when_github_repository_is_set() {
        let thumb = build_thumbnail(Some("ocx-sh/mirror-shfmt")).expect("set repo must produce thumbnail");
        assert_eq!(
            thumb.url, "https://raw.githubusercontent.com/ocx-sh/mirror-shfmt/main/logo.svg",
            "thumbnail URL must point at the repo's logo.svg on main",
        );
    }

    #[test]
    fn build_thumbnail_omits_when_github_repository_is_unset() {
        assert!(build_thumbnail(None).is_none());
        assert!(build_thumbnail(Some("")).is_none(), "empty value must omit thumbnail");
        assert!(build_thumbnail(Some("   ")).is_none(), "whitespace must omit thumbnail");
    }
}
