// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-mirror pipeline notify` — read `run-summary.json` and POST a Discord
//! webhook notification per the D10 taxonomy.

use std::path::PathBuf;

use ocx_lib::cli::Printer;

use crate::discord::{
    self, DiscordEmbed, DiscordEmbedAuthor, DiscordEmbedField, DiscordEmbedThumbnail, DiscordWebhookPayload,
};
use crate::error::MirrorError;
use crate::run_summary::RunSummary;

/// `ocx-mirror pipeline notify` subcommand.
///
/// Reads `run-summary.json` and posts to the Discord webhook URL sourced from
/// `$OCX_MIRROR_DISCORD_HOOK`. Silent (exit 0, no POST) when all versions are
/// `skipped_existing` and no test failures occurred.
#[derive(clap::Parser)]
pub struct Notify {
    /// Path to the `run-summary.json` produced by `pipeline push`.
    #[arg(long, required = true)]
    pub run_summary: PathBuf,
}

/// Conventional env var carrying the Discord webhook URL at runtime.
///
/// Hardcoded by design — spec's `notify.discord.webhook_secret` controls which
/// GitHub Actions secret maps onto this fixed name in the rendered workflow.
/// Keeping the local env var name fixed removes a layer of indirection (no
/// per-mirror flag, no env-name plumbing through the workflow template).
pub(crate) const WEBHOOK_ENV_VAR: &str = "OCX_MIRROR_DISCORD_HOOK";

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

        // Resolve webhook URL from the fixed environment variable.
        // URL is never logged — only the env var name may appear in messages.
        let webhook_url = std::env::var(WEBHOOK_ENV_VAR).map_err(|_| {
            MirrorError::SpecUsageError(format!(
                "environment variable '{WEBHOOK_ENV_VAR}' is not set; export it to the Discord webhook URL before running notify"
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
        username: None,
        embeds: vec![embed],
    }
}

/// Maximum length of a single Discord embed field value.
const DISCORD_FIELD_VALUE_LIMIT: usize = 1024;

/// Select color + title per D10 outcome rules and build the embed.
fn build_embed(summary: &RunSummary) -> DiscordEmbed {
    // Title is `{target}: {state}` — identifier + outcome only. No icon, no
    // mirror name, no version (versions surface in the field table below).
    // Empty `target` falls back to `mirror` so notify keeps emitting a
    // readable title even if a legacy summary lacks the field.
    let identifier = if summary.target.trim().is_empty() {
        summary.mirror.as_str()
    } else {
        summary.target.as_str()
    };

    let (color, title) = if summary.any_new_green && !summary.any_red {
        (discord::colors::GREEN, format!("{identifier}: published"))
    } else if summary.any_new_green && summary.any_red {
        (discord::colors::YELLOW, format!("{identifier}: partial"))
    } else {
        (discord::colors::RED, format!("{identifier}: failed"))
    };

    // Single 3-column table covers green + red rows alike. Discord caps
    // inline groups at three-per-row, so the failures table was previously
    // wrapping to a second visual row and looking detached; merging the
    // outcome icon and the job link into one rightmost cell keeps everything
    // on one row.
    let fields = render_results_table(summary);

    DiscordEmbed {
        title,
        color,
        url: Some(summary.run_url.clone()),
        // No description: title already carries the canonical identifier
        // (`{target}: {state}`), so a separate identifier line would just
        // duplicate it.
        description: None,
        author: build_author(summary),
        thumbnail: build_thumbnail(summary.logo_url.as_deref()),
        fields,
    }
}

/// Render the run as a single 3-column inline table: Version | Platform | Status.
///
/// Discord groups consecutive `inline: true` fields 3-per-row, so the three
/// fields here render as a single table covering every `(version, platform)`
/// pair across the whole run — rather than one repeating table per version.
/// Status sits in the rightmost column and carries the markdown link to the
/// responsible GHA job (green → `push_job_url`; red → the failure's own
/// `job_url`). The Version column shows only the full upstream version
/// (e.g. `3.8.0`); cascade tags (`3.8`, `3`, `latest`) are intentionally
/// elided to keep the table compact.
fn render_results_table(summary: &RunSummary) -> Vec<DiscordEmbedField> {
    let mut versions: Vec<String> = Vec::new();
    let mut platforms: Vec<String> = Vec::new();
    let mut outcomes: Vec<String> = Vec::new();

    for version in &summary.versions {
        if version.platforms_pushed.is_empty() && version.platforms_failed.is_empty() {
            continue;
        }
        for platform in &version.platforms_pushed {
            versions.push(format!("`{}`", version.version));
            platforms.push(format!("`{}`", platform));
            outcomes.push(outcome_cell(STATUS_SUCCESS, summary.push_job_url.as_deref()));
        }
        for failure in &version.platforms_failed {
            versions.push(format!("`{}`", version.version));
            platforms.push(format!("`{}`", failure.platform));
            outcomes.push(outcome_cell(
                status_glyph_for_reason(&failure.reason),
                failure.job_url.as_deref(),
            ));
        }
    }

    if versions.is_empty() {
        return Vec::new();
    }

    // Three inline columns: Version, Platform, Status. Discord caps inline
    // field groups at three-per-row so this is the maximum width a single
    // table can occupy without wrapping. The Status cell renders the
    // outcome chip directly inside a markdown link to the responsible GHA
    // job — clicking the chip jumps to the job logs, no separate link icon
    // required.
    vec![
        DiscordEmbedField {
            name: "Version".to_string(),
            value: clip_to_field_limit(&versions.join("\n")),
            inline: true,
        },
        DiscordEmbedField {
            name: "Platform".to_string(),
            value: clip_to_field_limit(&platforms.join("\n")),
            inline: true,
        },
        DiscordEmbedField {
            name: "Status".to_string(),
            value: clip_to_field_limit(&outcomes.join("\n")),
            inline: true,
        },
    ]
}

/// Status icon for a row's terminal state. Code-styled (wrapped in
/// backticks at render time) so the chip matches the Version and Platform
/// columns' visual rhythm.
const STATUS_SUCCESS: &str = "🟢";
const STATUS_FAIL: &str = "🔴";
const STATUS_MISSING: &str = "🚫";

/// Pick the right Status icon for a `PlatformFailure.reason`.
///
/// `missing_bundle` / `missing_junit` express "expected artifact never
/// arrived" — a different shade of failure from a test that ran and failed.
/// The `🚫` glyph distinguishes them from genuine test/push failures.
fn status_glyph_for_reason(reason: &str) -> &'static str {
    match reason {
        "missing_bundle" | "missing_junit" => STATUS_MISSING,
        _ => STATUS_FAIL,
    }
}

/// Render the Status cell: a backtick-wrapped icon, made clickable when a
/// job URL is available. Inside markdown link text Discord still parses
/// inline code formatting, so `[``X``](url)` renders as a clickable
/// code-styled chip. Absent URL collapses to the plain code chip.
fn outcome_cell(glyph: &str, url: Option<&str>) -> String {
    let chip = format!("`{glyph}`");
    match url.map(str::trim).filter(|s| !s.is_empty()) {
        Some(u) => format!("[{chip}]({u})"),
        None => chip,
    }
}

/// Build the embed author strip — a clickable link to the upstream project.
///
/// Renders only when `source_url` is set on the summary. Discord embed
/// thumbnails are decorative and cannot be hyperlinked; the author strip is
/// the conventional place for "click to view source". When the source URL
/// points at github.com we attach the owner's avatar as the author icon so
/// the strip renders with a recognisable face beside the link text.
fn build_author(summary: &RunSummary) -> Option<DiscordEmbedAuthor> {
    let url = summary.source_url.as_deref()?.trim();
    if url.is_empty() {
        return None;
    }
    let (name, icon_url) = match github_owner_repo(url) {
        Some((owner, repo)) => (
            format!("{owner}/{repo}"),
            Some(format!("https://github.com/{owner}.png?size=64")),
        ),
        None => ("View source".to_string(), None),
    };
    Some(DiscordEmbedAuthor {
        name,
        url: Some(url.to_string()),
        icon_url,
    })
}

/// Extract `(owner, repo)` from a github.com URL like
/// `https://github.com/mvdan/sh`. Returns `None` for non-github URLs or
/// malformed paths.
fn github_owner_repo(url: &str) -> Option<(&str, &str)> {
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let mut parts = path.trim_end_matches('/').splitn(3, '/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

/// Build the embed thumbnail from the run summary's `logo_url` field.
///
/// `pipeline push` computes the URL from `GITHUB_REPOSITORY` + `GITHUB_SHA`
/// so the link is pinned to the commit that produced the run (and therefore
/// resolves even when the mirror repo's `logo.png` hasn't landed on `main`
/// yet). Returns `None` when the field is unset or blank — Discord renders
/// the embed without a thumbnail in that case.
fn build_thumbnail(logo_url: Option<&str>) -> Option<DiscordEmbedThumbnail> {
    let url = logo_url?.trim();
    if url.is_empty() {
        return None;
    }
    Some(DiscordEmbedThumbnail { url: url.to_string() })
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
            push_job_url: None,
            source_url: None,
            logo_url: None,
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
            push_job_url: None,
            source_url: None,
            logo_url: None,
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
            push_job_url: None,
            source_url: None,
            logo_url: None,
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
            push_job_url: None,
            source_url: None,
            logo_url: None,
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

    /// Serialises every test that mutates the shared `OCX_MIRROR_DISCORD_HOOK`
    /// process env var. The env var name is now hardcoded (was per-test unique
    /// strings before the autodetect refactor), so concurrent tests would race
    /// on set/remove without this lock.
    fn webhook_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// RAII guard: holds the env lock and sets `OCX_MIRROR_DISCORD_HOOK` to
    /// `url` for its lifetime; clears the variable on drop. Use one per test
    /// that needs a webhook URL injected.
    struct WebhookEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl WebhookEnvGuard {
        fn set(url: &str) -> Self {
            let lock = webhook_env_lock();
            // SAFETY: env mutation is serialised by the held lock.
            unsafe { std::env::set_var(WEBHOOK_ENV_VAR, url) }
            Self { _lock: lock }
        }
        fn unset() -> Self {
            let lock = webhook_env_lock();
            // SAFETY: env mutation is serialised by the held lock.
            unsafe { std::env::remove_var(WEBHOOK_ENV_VAR) }
            Self { _lock: lock }
        }
    }
    impl Drop for WebhookEnvGuard {
        fn drop(&mut self) {
            // SAFETY: lock still held until self is fully dropped.
            unsafe { std::env::remove_var(WEBHOOK_ENV_VAR) }
        }
    }

    fn run_notify_sync(summary: &RunSummary) -> Result<(), MirrorError> {
        let f = write_run_summary(summary);
        let printer = ocx_lib::cli::Printer::new(false);
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
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
        let _guard = WebhookEnvGuard::unset();
        let result = run_notify_sync(&make_all_skipped_summary());
        assert!(
            matches!(result, Ok(())),
            "all-skipped summary must be silent (exit 0, no POST, no env var lookup): {result:?}"
        );
    }

    #[test]
    fn notify_missing_env_var_returns_spec_usage_error() {
        // OCX_MIRROR_DISCORD_HOOK unset → SpecUsageError (exit 64).
        // Use a summary that requires a POST (any_new_green = true).
        let _guard = WebhookEnvGuard::unset();
        let result = run_notify_sync(&make_all_green_summary());
        assert!(
            matches!(result, Err(MirrorError::SpecUsageError(_))),
            "unset webhook env var must return SpecUsageError: {result:?}"
        );
    }

    #[test]
    fn build_payload_green_embed_has_correct_color_and_title() {
        // Title is `{target}: published` — no icon, no version, no mirror name.
        let summary = make_all_green_summary();
        let payload = build_payload(&summary);
        assert_eq!(
            payload.username, None,
            "webhook owns the bot name; payload must not override"
        );
        assert_eq!(payload.embeds.len(), 1);
        let embed = &payload.embeds[0];
        assert_eq!(embed.color, colors::GREEN, "green outcome must use GREEN color");
        assert_eq!(embed.title, "ocx.sh/shfmt: published");
    }

    #[test]
    fn build_payload_yellow_embed_has_correct_color_and_title() {
        let summary = make_partial_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        assert_eq!(embed.color, colors::YELLOW, "partial outcome must use YELLOW color");
        assert_eq!(embed.title, "ocx.sh/shfmt: partial");
    }

    #[test]
    fn build_payload_red_embed_has_correct_color_and_title() {
        let summary = make_all_failed_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        assert_eq!(embed.color, colors::RED, "failed outcome must use RED color");
        assert_eq!(embed.title, "ocx.sh/shfmt: failed");
    }

    #[test]
    fn build_payload_title_falls_back_to_mirror_when_target_empty() {
        let mut summary = make_all_green_summary();
        summary.target = String::new();
        let payload = build_payload(&summary);
        assert_eq!(payload.embeds[0].title, "shfmt: published");
    }

    #[test]
    fn build_author_renders_github_owner_and_repo_with_avatar() {
        let mut summary = make_all_green_summary();
        summary.source_url = Some("https://github.com/mvdan/sh".to_string());
        let author = build_author(&summary).expect("github source_url must yield author");
        assert_eq!(author.name, "mvdan/sh");
        assert_eq!(author.url.as_deref(), Some("https://github.com/mvdan/sh"));
        assert_eq!(
            author.icon_url.as_deref(),
            Some("https://github.com/mvdan.png?size=64"),
            "github author should use the owner's GH avatar as icon"
        );
    }

    #[test]
    fn build_author_uses_generic_label_for_non_github_url() {
        let mut summary = make_all_green_summary();
        summary.source_url = Some("https://example.org/project".to_string());
        let author = build_author(&summary).expect("non-empty source_url must yield author");
        assert_eq!(author.name, "View source");
        assert_eq!(author.url.as_deref(), Some("https://example.org/project"));
        assert!(author.icon_url.is_none(), "non-github URLs do not get an icon");
    }

    #[test]
    fn build_author_is_none_when_source_url_unset() {
        let summary = make_all_green_summary();
        assert!(build_author(&summary).is_none());
    }

    #[test]
    fn build_embed_red_renders_failed_platforms_in_table() {
        // A run where every platform failed surfaces each platform and
        // version in the combined Version/Platform/Outcome table. Outcome
        // cell is `<status chip> [🔗](url)` when a job_url is set.
        let summary = RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/9".to_string(),
            push_job_url: None,
            source_url: None,
            logo_url: None,
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
        let t = results_table(embed);

        assert_eq!(
            col_version(t).value,
            "`3.13.1`\n`3.13.1`",
            "Version values wrapped in backticks, joined by single newline",
        );
        assert!(
            col_platform(t).value.contains("`linux/amd64`") && col_platform(t).value.contains("`darwin/arm64`"),
            "Platform column must list both failed platforms wrapped in backticks: {}",
            col_platform(t).value,
        );
        // Both failures use the `missing_bundle` reason class, so both rows
        // render with the 🚫 glyph rather than 🔴.
        assert_eq!(
            col_outcome(t).value.matches(STATUS_MISSING).count(),
            1,
            "darwin/arm64 missing_bundle row uses 🚫: {}",
            col_outcome(t).value,
        );
        assert_eq!(
            col_outcome(t).value.matches(STATUS_FAIL).count(),
            1,
            "linux/amd64 push_error row uses 🔴: {}",
            col_outcome(t).value,
        );
    }

    // ── Table-shape helpers — single 3-column table: Version, Platform,
    //    Outcome (status chip + clickable link icon in a header-less cell) ──

    fn results_table(embed: &DiscordEmbed) -> &[DiscordEmbedField] {
        &embed.fields[..3]
    }
    fn col_version(t: &[DiscordEmbedField]) -> &DiscordEmbedField {
        &t[0]
    }
    fn col_platform(t: &[DiscordEmbedField]) -> &DiscordEmbedField {
        &t[1]
    }
    fn col_outcome(t: &[DiscordEmbedField]) -> &DiscordEmbedField {
        &t[2]
    }

    #[test]
    fn build_payload_partial_failure_row_includes_link_in_outcome_cell() {
        // Container-tier failure surfaces in the same single results table
        // as the green rows. The Outcome cell pairs the ❌ chip with a
        // clickable globe icon pointing at the matrix-job URL — no separate
        // failures table, no Environment column.
        let mut summary = make_partial_summary();
        summary.versions[0].platforms_failed[0].job_url =
            Some("https://github.com/ocx-sh/mirror-shfmt/actions/runs/42/job/77".to_string());
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let t = results_table(embed);

        for f in t {
            assert!(f.inline, "every column must be inline: {:?}", f);
        }
        assert_eq!(
            col_outcome(t).name,
            "Status",
            "Outcome column carries the Status header"
        );
        let outcome_lines: Vec<&str> = col_outcome(t).value.split('\n').collect();
        assert!(
            outcome_lines.contains(&"[`🔴`](https://github.com/ocx-sh/mirror-shfmt/actions/runs/42/job/77)"),
            "Status cell must wrap a backtick-styled icon inside a markdown link: {}",
            col_outcome(t).value,
        );
    }

    #[test]
    fn build_payload_no_separate_failures_table() {
        // Single 3-column table covers everything; embed never has more than
        // three inline fields regardless of mix of pushed / failed rows.
        let summary = make_partial_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let inline_count = embed.fields.iter().filter(|f| f.inline).count();
        assert_eq!(inline_count, 3, "exactly three inline columns: {:?}", embed.fields);
    }

    #[test]
    fn build_embed_green_table_lists_full_version_and_each_platform() {
        // The combined table renders one row per (version, platform). The
        // Version column shows the full upstream version (e.g. `3.7.0`) —
        // cascade tags (`3.7`, `3`, `latest`) are intentionally elided.
        let summary = make_all_green_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let t = results_table(embed);
        assert_eq!(
            col_version(t).value,
            "`3.7.0`\n`3.7.0`",
            "Version repeats once per platform row"
        );
        assert!(
            !col_version(t).value.contains("latest") && !col_version(t).value.contains("`3.7`"),
            "Version column must not include cascade tags: {}",
            col_version(t).value,
        );
        assert_eq!(col_platform(t).value, "`linux/amd64`\n`darwin/arm64`");
    }

    #[test]
    fn webhook_env_var_name_is_conventional() {
        // The local env var name is hardcoded by convention (was a per-mirror
        // CLI flag before the autodetect refactor). Lock in the value so a
        // future rename triggers a coordinated update of the workflow renderer.
        assert_eq!(WEBHOOK_ENV_VAR, "OCX_MIRROR_DISCORD_HOOK");
        assert!(
            WEBHOOK_ENV_VAR
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "WEBHOOK_ENV_VAR must match GHA secret naming ^[A-Z][A-Z0-9_]+$"
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

    /// Drive `Notify::execute` against a stub TCP server bound to `OCX_MIRROR_DISCORD_HOOK`.
    async fn post_to_stub(summary: &RunSummary, status_code: u16) -> Result<(), MirrorError> {
        ensure_crypto_provider();
        let server_url = one_shot_server(status_code).await;
        let _guard = WebhookEnvGuard::set(&server_url);

        let f = write_run_summary(summary);
        let printer = ocx_lib::cli::Printer::new(false);
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
        };
        let result = cmd.execute(&printer).await;
        let _ = f;
        result
    }

    #[tokio::test]
    async fn notify_posts_green_embed_for_all_new_green() {
        // §3.9: any_new_green && !any_red → green embed; 2xx response → Ok(())
        let result = post_to_stub(&make_all_green_summary(), 204).await;
        assert!(matches!(result, Ok(())), "2xx response must yield Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn notify_posts_yellow_embed_for_partial() {
        // §3.9: any_new_green && any_red → yellow partial embed; 2xx → Ok(())
        let result = post_to_stub(&make_partial_summary(), 200).await;
        assert!(matches!(result, Ok(())), "2xx response must yield Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn notify_posts_red_embed_for_all_failed() {
        // §3.9: !any_new_green && any_red → red failed embed; 2xx → Ok(())
        let result = post_to_stub(&make_all_failed_summary(), 200).await;
        assert!(matches!(result, Ok(())), "2xx response must yield Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn notify_discord_5xx_returns_webhook_unavailable() {
        // §3.9: 5xx → MirrorError::WebhookUnavailable (exit 69)
        let result = post_to_stub(&make_all_green_summary(), 503).await;
        assert!(
            matches!(result, Err(MirrorError::WebhookUnavailable(_))),
            "5xx must return WebhookUnavailable: {result:?}"
        );
    }

    #[tokio::test]
    async fn notify_discord_401_returns_webhook_permission_denied() {
        // §3.9: 401 → MirrorError::WebhookPermissionDenied (exit 77)
        let result = post_to_stub(&make_all_green_summary(), 401).await;
        assert!(
            matches!(result, Err(MirrorError::WebhookPermissionDenied(_))),
            "401 must return WebhookPermissionDenied: {result:?}"
        );
    }

    #[tokio::test]
    async fn notify_discord_403_returns_webhook_permission_denied() {
        // §3.9: 403 → MirrorError::WebhookPermissionDenied (exit 77)
        let result = post_to_stub(&make_all_green_summary(), 403).await;
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
            push_job_url: None,
            source_url: None,
            logo_url: None,
            versions: vec![version_summary_with_failure(version, platform, reason, job_url)],
            any_red: true,
            any_new_green: false,
        }
    }

    #[test]
    fn build_embed_emits_single_table_for_all_versions() {
        // Exactly three inline columns regardless of version count: Version,
        // Platform, Outcome (header-less). Discord caps inline groups at
        // three per visual row, so this fits on one row.
        let mut summary = make_all_green_summary();
        summary.versions.push(VersionSummary {
            version: "3.8.0".to_string(),
            status: VersionStatus::Published,
            platforms_pushed: vec!["linux/amd64".to_string()],
            platforms_failed: vec![],
            cascade_tags_written: vec!["3.8.0".to_string(), "3.8".to_string()],
            test_failures: vec![],
        });

        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let inline: Vec<&DiscordEmbedField> = embed.fields.iter().filter(|f| f.inline).collect();
        let names: Vec<&str> = inline.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Version", "Platform", "Status"],
            "three inline columns: Version, Platform, Status",
        );
    }

    #[test]
    fn build_embed_red_status_cell_wraps_chip_in_link_when_job_url_present() {
        // Status cell: backtick-wrapped icon inside a markdown link.
        let job_url = "https://github.com/ocx-sh/mirror-shfmt/actions/runs/42/job/7";
        let summary = red_summary_with_failure("3.7.0", "linux/amd64", "test_failed", Some(job_url.to_string()));
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let t = results_table(embed);
        assert_eq!(col_outcome(t).value, format!("[`🔴`]({job_url})"));
    }

    #[test]
    fn build_embed_red_status_cell_renders_plain_chip_when_job_url_absent() {
        let summary = red_summary_with_failure("3.7.0", "linux/amd64", "test_failed", None);
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let t = results_table(embed);
        assert_eq!(
            col_outcome(t).value,
            "`🔴`",
            "absent job_url → plain code chip, no link"
        );
    }

    #[test]
    fn build_embed_missing_bundle_uses_no_entry_glyph() {
        // `missing_bundle` is a distinct shade of failure — the artifact
        // never arrived — so the Status cell renders 🚫 rather than 🔴.
        let summary = red_summary_with_failure("3.7.0", "linux/amd64", "missing_bundle", None);
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let t = results_table(embed);
        assert_eq!(col_outcome(t).value, "`🚫`");
    }

    #[test]
    fn build_embed_missing_junit_uses_no_entry_glyph() {
        let summary = red_summary_with_failure("3.7.0", "linux/amd64", "missing_junit", None);
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let t = results_table(embed);
        assert_eq!(col_outcome(t).value, "`🚫`");
    }

    #[test]
    fn build_embed_green_status_cell_links_to_push_job_url() {
        let mut summary = make_all_green_summary();
        summary.push_job_url = Some("https://github.com/ocx-sh/mirror-shfmt/actions/runs/2/job/3".to_string());
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let t = results_table(embed);
        let expected_row = "[`🟢`](https://github.com/ocx-sh/mirror-shfmt/actions/runs/2/job/3)";
        assert_eq!(
            col_outcome(t).value,
            format!("{expected_row}\n{expected_row}"),
            "Status cell wraps 🟢 chip inside a clickable link per row",
        );
    }

    #[test]
    fn build_embed_green_status_cell_renders_plain_chip_when_push_job_url_absent() {
        let summary = make_all_green_summary(); // push_job_url already None
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let t = results_table(embed);
        assert_eq!(
            col_outcome(t).value,
            "`🟢`\n`🟢`",
            "absent push_job_url → plain chip per row, no link"
        );
    }

    #[test]
    fn build_embed_push_error_row_uses_stamped_push_job_url() {
        // push_error / missing_bundle failures have `job_url` stamped by
        // `pipeline push` (from `OCX_MIRROR_JOB_URL`). The Outcome cell
        // therefore renders the linked globe icon even though no per-
        // platform matrix job exists for the push step.
        let push_url = "https://github.com/ocx-sh/mirror-shfmt/actions/runs/42/job/99";
        let summary = RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/42".to_string(),
            push_job_url: Some(push_url.to_string()),
            source_url: None,
            logo_url: None,
            versions: vec![version_summary_with_failure(
                "3.7.1",
                "linux/amd64",
                "push_error",
                Some(push_url.to_string()),
            )],
            any_red: true,
            any_new_green: false,
        };
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let t = results_table(embed);
        assert_eq!(col_outcome(t).value, format!("[`🔴`]({push_url})"));
    }

    #[test]
    fn build_thumbnail_attaches_url_from_summary_logo_url() {
        let url = "https://raw.githubusercontent.com/ocx-sh/mirror-shfmt/abc123/logo.png";
        let thumb = build_thumbnail(Some(url)).expect("set logo_url must produce thumbnail");
        assert_eq!(thumb.url, url, "thumbnail URL passes through unchanged");
    }

    #[test]
    fn build_thumbnail_omits_when_logo_url_unset() {
        assert!(build_thumbnail(None).is_none());
        assert!(build_thumbnail(Some("")).is_none(), "empty value must omit thumbnail");
        assert!(build_thumbnail(Some("   ")).is_none(), "whitespace must omit thumbnail");
    }

    #[test]
    fn build_embed_omits_description_to_avoid_duplicating_identifier() {
        // Title already carries `{target}: {state}` — repeating the identifier
        // in description would be redundant.
        let summary = make_all_green_summary();
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        assert!(
            embed.description.is_none(),
            "description must always be omitted: {:?}",
            embed.description
        );
    }

    #[test]
    fn build_embed_uses_logo_url_for_thumbnail() {
        let url = "https://raw.githubusercontent.com/ocx-sh/mirror-shfmt/deadbeef/logo.png";
        let mut summary = make_all_green_summary();
        summary.logo_url = Some(url.to_string());
        let payload = build_payload(&summary);
        let embed = &payload.embeds[0];
        let thumb = embed.thumbnail.as_ref().expect("logo_url must produce thumbnail");
        assert_eq!(thumb.url, url);
    }
}
