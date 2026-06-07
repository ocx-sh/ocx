// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-mirror pipeline notify` — read `run-summary.json` and POST Discord
//! webhook notifications per the D10 taxonomy.
//!
//! One embed per version (so a field never trips Discord's 1024-char cap with
//! many releases), batched into messages of ≤10 embeds. Any message carrying a
//! partial/failed version is prefixed with an in-message `<@id>` mention when
//! `OCX_MIRROR_DISCORD_USER_ID` is set.

use std::path::PathBuf;

use ocx_lib::cli::DataInterface;

use crate::discord::{
    self, AllowedMentions, DiscordEmbed, DiscordEmbedAuthor, DiscordEmbedField, DiscordEmbedThumbnail,
    DiscordWebhookPayload,
};
use crate::error::MirrorError;
use crate::run_summary::{RunSummary, VersionStatus, VersionSummary};

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

/// Conventional env var carrying the Discord user ID (snowflake) to mention on
/// failures. Non-secret — the renderer inlines `notify.discord.user_id` into
/// the notify job env under this fixed name. Unset / empty → no mention.
pub(crate) const USER_ID_ENV_VAR: &str = "OCX_MIRROR_DISCORD_USER_ID";

impl Notify {
    pub async fn execute(&self, _printer: &DataInterface) -> Result<(), MirrorError> {
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

        // Optional mention target — non-secret, inlined into the workflow env.
        let user_id = std::env::var(USER_ID_ENV_VAR)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());

        let messages = build_messages(&summary, user_id.as_deref());
        if messages.is_empty() {
            tracing::debug!("no notifiable versions in run summary; nothing to send");
            return Ok(());
        }

        // Resolve webhook URL from the fixed environment variable.
        // URL is never logged — only the env var name may appear in messages.
        let webhook_url = std::env::var(WEBHOOK_ENV_VAR).map_err(|_| {
            MirrorError::SpecUsageError(format!(
                "environment variable '{WEBHOOK_ENV_VAR}' is not set; export it to the Discord webhook URL before running notify"
            ))
        })?;

        for payload in &messages {
            discord::post(&webhook_url, payload).await?;
        }
        Ok(())
    }
}

/// Maximum length of a single Discord embed field value.
const DISCORD_FIELD_VALUE_LIMIT: usize = 1024;

/// Maximum embeds Discord accepts in a single webhook message.
const MAX_EMBEDS_PER_MESSAGE: usize = 10;

/// Build the Discord messages for a run.
///
/// Emits one embed per version that has rows (pushed / failed / excluded),
/// batched into messages of ≤10 embeds. A message that carries any
/// partial/failed version is prefixed with `<@id>` (scoped via
/// `allowed_mentions`) when `user_id` is set. Returns an empty vec when no
/// version is notifiable (caller treats that as silent).
fn build_messages(summary: &RunSummary, user_id: Option<&str>) -> Vec<DiscordWebhookPayload> {
    let mut embeds: Vec<(DiscordEmbed, bool)> = Vec::new();
    for version in &summary.versions {
        // Decorate the first *emitted* embed with the author strip + thumbnail
        // so a multi-version run doesn't render a column of repeated logos.
        // Keyed on `embeds.is_empty()`, not the loop index: versions are stored
        // oldest-first, so versions[0] is often an already-published
        // SkippedExisting version that yields no embed — the decoration must
        // land on the first version that actually produces one.
        if let Some(embed) = build_version_embed(summary, version, embeds.is_empty()) {
            let is_red = matches!(version.status, VersionStatus::Partial | VersionStatus::Failed);
            embeds.push((embed, is_red));
        }
    }
    if embeds.is_empty() {
        return Vec::new();
    }

    embeds
        .chunks(MAX_EMBEDS_PER_MESSAGE)
        .map(|chunk| {
            let chunk_has_red = chunk.iter().any(|(_, red)| *red);
            let embeds: Vec<DiscordEmbed> = chunk.iter().map(|(embed, _)| embed.clone()).collect();
            // Ping only when the message carries a failed/partial version AND a
            // user id is configured. `parse: []` + explicit `users` scopes the
            // ping to that one user (no @everyone / role escalation).
            let (content, allowed_mentions) = match user_id.filter(|_| chunk_has_red) {
                Some(id) => (
                    Some(format!("<@{id}>")),
                    Some(AllowedMentions {
                        parse: vec![],
                        users: Some(vec![id.to_string()]),
                    }),
                ),
                None => (None, None),
            };
            DiscordWebhookPayload {
                embeds,
                content,
                allowed_mentions,
            }
        })
        .collect()
}

/// Build the embed for a single version, or `None` when it has no rows to show
/// (a skipped-existing version with no excluded platforms).
///
/// The title carries `{identifier}: {version} {state}`; the body is two inline
/// columns, Platform | Status. Status holds the 🟢/🔴/🚫 chip (linked to the
/// responsible GHA job) or a 🔒 row for a deliberately-excluded platform.
fn build_version_embed(summary: &RunSummary, version: &VersionSummary, decorate: bool) -> Option<DiscordEmbed> {
    let mut platforms: Vec<String> = Vec::new();
    let mut statuses: Vec<String> = Vec::new();

    for platform in &version.platforms_pushed {
        platforms.push(format!("`{platform}`"));
        statuses.push(outcome_cell(STATUS_SUCCESS, summary.push_job_url.as_deref()));
    }
    for failure in &version.platforms_failed {
        platforms.push(format!("`{}`", failure.platform));
        statuses.push(outcome_cell(
            status_glyph_for_reason(&failure.reason),
            failure.job_url.as_deref(),
        ));
    }
    for excluded in &version.platforms_excluded {
        platforms.push(format!("`{}`", excluded.platform));
        statuses.push(excluded_cell(excluded.reason.as_deref()));
    }

    if platforms.is_empty() {
        return None;
    }

    let (color, state) = version_color_and_state(&version.status);
    // Title is `{identifier}: {version} {state}`. Empty `target` falls back to
    // `mirror` so notify keeps a readable title even for a legacy summary.
    let identifier = if summary.target.trim().is_empty() {
        summary.mirror.as_str()
    } else {
        summary.target.as_str()
    };

    Some(DiscordEmbed {
        title: format!("{identifier}: {} {state}", version.version),
        color,
        url: Some(summary.run_url.clone()),
        description: None,
        author: decorate.then(|| build_author(summary)).flatten(),
        thumbnail: decorate.then(|| build_thumbnail(summary.logo_url.as_deref())).flatten(),
        fields: vec![
            DiscordEmbedField {
                name: "Platform".to_string(),
                value: clip_to_field_limit(&platforms.join("\n")),
                inline: true,
            },
            DiscordEmbedField {
                name: "Status".to_string(),
                value: clip_to_field_limit(&statuses.join("\n")),
                inline: true,
            },
        ],
    })
}

/// Per-version color + state label for the embed title.
fn version_color_and_state(status: &VersionStatus) -> (u32, &'static str) {
    match status {
        VersionStatus::Published => (discord::colors::GREEN, "published"),
        VersionStatus::Partial => (discord::colors::YELLOW, "partial"),
        VersionStatus::Failed => (discord::colors::RED, "failed"),
        // Reached only when a skipped version still carries a 🔒 excluded row;
        // nothing failed this run, so render it green/informational.
        VersionStatus::SkippedExisting => (discord::colors::GREEN, "up to date"),
        VersionStatus::SkippedExecutor => (discord::colors::RED, "no executor"),
    }
}

/// Status icon for a row's terminal state. Code-styled (wrapped in
/// backticks at render time) so the chip matches the Platform column's rhythm.
const STATUS_SUCCESS: &str = "🟢";
const STATUS_FAIL: &str = "🔴";
const STATUS_MISSING: &str = "🚫";
/// A deliberately-excluded (`broken`) platform — not a failure, the gap is
/// declared in the spec via `platforms.<p>.exclude`.
const STATUS_EXCLUDED: &str = "🔒";

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

/// Render a 🔒 excluded-platform row: the lock chip plus the reason when given.
/// Not linked — there is no job to point at, the pair was never built.
fn excluded_cell(reason: Option<&str>) -> String {
    let chip = format!("`{STATUS_EXCLUDED}`");
    match reason.map(str::trim).filter(|s| !s.is_empty()) {
        Some(reason) => format!("{chip} {reason}"),
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
///
/// Discord rejects any embed field whose value exceeds 1024 chars with HTTP
/// 400, so clipping is load-bearing — the cap itself isn't optional. The clip
/// rounds down to a UTF-8 char boundary so multi-byte emoji (🟢/🔴/🚫/🔒) at the
/// budget index don't panic in `s[..]`, then trims back to the last newline so
/// the cut lands between rows rather than mid-cell.
fn clip_to_field_limit(s: &str) -> String {
    if s.len() <= DISCORD_FIELD_VALUE_LIMIT {
        return s.to_string();
    }
    let boundary = (0..=DISCORD_FIELD_VALUE_LIMIT)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    let mut clipped = s[..boundary].to_string();
    if let Some(pos) = clipped.rfind('\n') {
        clipped.truncate(pos);
    }
    clipped
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::discord::colors;
    use crate::run_summary::{
        ExcludedPlatform, PlatformFailure, RunSummary, TestFailure, VersionStatus, VersionSummary,
    };

    // ── §3.9 S9: notify subcommand tests ──────────────────────────────────

    fn write_run_summary(summary: &RunSummary) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        let json = serde_json::to_string_pretty(summary).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        f
    }

    fn version(status: VersionStatus, version: &str) -> VersionSummary {
        VersionSummary {
            version: version.to_string(),
            status,
            platforms_pushed: vec![],
            platforms_failed: vec![],
            cascade_tags_written: vec![],
            test_failures: vec![],
            platforms_excluded: vec![],
        }
    }

    fn run_summary(versions: Vec<VersionSummary>, any_new_green: bool, any_red: bool) -> RunSummary {
        RunSummary {
            schema_version: 1,
            mirror: "shfmt".to_string(),
            target: "ocx.sh/shfmt".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-shfmt/actions/runs/1".to_string(),
            push_job_url: None,
            source_url: None,
            logo_url: None,
            versions,
            any_red,
            any_new_green,
        }
    }

    fn make_all_skipped_summary() -> RunSummary {
        run_summary(vec![version(VersionStatus::SkippedExisting, "3.7.0")], false, false)
    }

    fn make_all_green_summary() -> RunSummary {
        let mut v = version(VersionStatus::Published, "3.7.0");
        v.platforms_pushed = vec!["linux/amd64".to_string(), "darwin/arm64".to_string()];
        v.cascade_tags_written = vec![
            "3.7.0".to_string(),
            "3.7".to_string(),
            "3".to_string(),
            "latest".to_string(),
        ];
        let mut summary = run_summary(vec![v], true, false);
        summary.run_url = "https://github.com/ocx-sh/mirror-shfmt/actions/runs/2".to_string();
        summary
    }

    fn make_partial_summary() -> RunSummary {
        let failure = PlatformFailure {
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
        };
        let mut v = version(VersionStatus::Partial, "3.7.0");
        v.platforms_pushed = vec!["linux/amd64".to_string()];
        v.platforms_failed = vec![failure];
        v.cascade_tags_written = vec!["3.7.0".to_string()];
        let mut summary = run_summary(vec![v], true, true);
        summary.run_url = "https://github.com/ocx-sh/mirror-shfmt/actions/runs/3".to_string();
        summary
    }

    fn make_all_failed_summary() -> RunSummary {
        let failure = PlatformFailure {
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
        };
        let mut v = version(VersionStatus::Failed, "3.7.0");
        v.platforms_failed = vec![failure];
        let mut summary = run_summary(vec![v], false, true);
        summary.run_url = "https://github.com/ocx-sh/mirror-shfmt/actions/runs/4".to_string();
        summary
    }

    /// Serialises every test that mutates the shared `OCX_MIRROR_DISCORD_HOOK`
    /// / `OCX_MIRROR_DISCORD_USER_ID` process env vars.
    fn webhook_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// RAII guard: holds the env lock and sets `OCX_MIRROR_DISCORD_HOOK` to
    /// `url` for its lifetime; clears both notify env vars on drop.
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
            unsafe {
                std::env::remove_var(WEBHOOK_ENV_VAR);
                std::env::remove_var(USER_ID_ENV_VAR);
            }
            Self { _lock: lock }
        }
    }
    impl Drop for WebhookEnvGuard {
        fn drop(&mut self) {
            // SAFETY: lock still held until self is fully dropped.
            unsafe {
                std::env::remove_var(WEBHOOK_ENV_VAR);
                std::env::remove_var(USER_ID_ENV_VAR);
            }
        }
    }

    fn run_notify_sync(summary: &RunSummary) -> Result<(), MirrorError> {
        let f = write_run_summary(summary);
        let printer = ocx_lib::cli::DataInterface::new(ocx_lib::cli::Printer::new(false, false));
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async { cmd.execute(&printer).await });
        let _ = f; // keep alive
        result
    }

    // ── Message-construction tests (no HTTP, no env var needed) ────────────

    #[test]
    fn notify_silent_when_all_skipped_existing() {
        // §3.9: all skipped_existing + no test_failures → silent (exit 0, no POST).
        let _guard = WebhookEnvGuard::unset();
        let result = run_notify_sync(&make_all_skipped_summary());
        assert!(
            matches!(result, Ok(())),
            "all-skipped summary must be silent (exit 0, no POST): {result:?}"
        );
    }

    #[test]
    fn notify_missing_env_var_returns_spec_usage_error() {
        // OCX_MIRROR_DISCORD_HOOK unset → SpecUsageError (exit 64) when a POST is due.
        let _guard = WebhookEnvGuard::unset();
        let result = run_notify_sync(&make_all_green_summary());
        assert!(
            matches!(result, Err(MirrorError::SpecUsageError(_))),
            "unset webhook env var must return SpecUsageError: {result:?}"
        );
    }

    fn only_embed(messages: &[DiscordWebhookPayload]) -> &DiscordEmbed {
        assert_eq!(messages.len(), 1, "expected a single message: {messages:?}");
        assert_eq!(messages[0].embeds.len(), 1, "expected a single embed");
        &messages[0].embeds[0]
    }
    fn col_platform(embed: &DiscordEmbed) -> &DiscordEmbedField {
        &embed.fields[0]
    }
    fn col_status(embed: &DiscordEmbed) -> &DiscordEmbedField {
        &embed.fields[1]
    }

    #[test]
    fn green_version_embed_has_green_color_and_title() {
        let messages = build_messages(&make_all_green_summary(), None);
        let embed = only_embed(&messages);
        assert_eq!(embed.color, colors::GREEN);
        assert_eq!(embed.title, "ocx.sh/shfmt: 3.7.0 published");
        // No mention: all green.
        assert!(messages[0].content.is_none());
        assert!(messages[0].allowed_mentions.is_none());
    }

    #[test]
    fn partial_version_embed_has_yellow_color_and_title() {
        let messages = build_messages(&make_partial_summary(), None);
        let embed = only_embed(&messages);
        assert_eq!(embed.color, colors::YELLOW);
        assert_eq!(embed.title, "ocx.sh/shfmt: 3.7.0 partial");
    }

    #[test]
    fn failed_version_embed_has_red_color_and_title() {
        let messages = build_messages(&make_all_failed_summary(), None);
        let embed = only_embed(&messages);
        assert_eq!(embed.color, colors::RED);
        assert_eq!(embed.title, "ocx.sh/shfmt: 3.7.0 failed");
    }

    #[test]
    fn title_falls_back_to_mirror_when_target_empty() {
        let mut summary = make_all_green_summary();
        summary.target = String::new();
        let messages = build_messages(&summary, None);
        assert_eq!(only_embed(&messages).title, "shfmt: 3.7.0 published");
    }

    #[test]
    fn embed_has_exactly_two_inline_columns() {
        let messages = build_messages(&make_partial_summary(), None);
        let embed = only_embed(&messages);
        let names: Vec<&str> = embed.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Platform", "Status"]);
        for f in &embed.fields {
            assert!(f.inline, "every column must be inline: {f:?}");
        }
    }

    #[test]
    fn green_embed_lists_each_platform_with_chip() {
        let messages = build_messages(&make_all_green_summary(), None);
        let embed = only_embed(&messages);
        assert_eq!(col_platform(embed).value, "`linux/amd64`\n`darwin/arm64`");
        assert_eq!(col_status(embed).value, "`🟢`\n`🟢`");
    }

    #[test]
    fn green_status_cell_links_to_push_job_url() {
        let mut summary = make_all_green_summary();
        summary.push_job_url = Some("https://github.com/ocx-sh/mirror-shfmt/actions/runs/2/job/3".to_string());
        let messages = build_messages(&summary, None);
        let embed = only_embed(&messages);
        let row = "[`🟢`](https://github.com/ocx-sh/mirror-shfmt/actions/runs/2/job/3)";
        assert_eq!(col_status(embed).value, format!("{row}\n{row}"));
    }

    #[test]
    fn failed_status_cell_wraps_chip_in_link_when_job_url_present() {
        let mut summary = make_all_failed_summary();
        let job_url = "https://github.com/ocx-sh/mirror-shfmt/actions/runs/42/job/7";
        summary.versions[0].platforms_failed[0].job_url = Some(job_url.to_string());
        let messages = build_messages(&summary, None);
        let embed = only_embed(&messages);
        assert_eq!(col_status(embed).value, format!("[`🔴`]({job_url})"));
    }

    #[test]
    fn missing_bundle_reason_uses_no_entry_glyph() {
        let mut summary = make_all_failed_summary();
        summary.versions[0].platforms_failed[0].reason = "missing_bundle".to_string();
        let messages = build_messages(&summary, None);
        assert_eq!(col_status(only_embed(&messages)).value, "`🚫`");
    }

    // ── 🔒 excluded-platform rows ──────────────────────────────────────────

    #[test]
    fn excluded_platform_renders_lock_row_with_reason() {
        let mut summary = make_all_green_summary();
        summary.versions[0].platforms_excluded = vec![ExcludedPlatform {
            platform: "windows/arm64".to_string(),
            reason: Some("aarch64-windows build-exe segfault".to_string()),
        }];
        let messages = build_messages(&summary, None);
        let embed = only_embed(&messages);
        assert!(
            col_platform(embed).value.contains("`windows/arm64`"),
            "excluded platform must appear in the Platform column: {}",
            col_platform(embed).value,
        );
        assert!(
            col_status(embed)
                .value
                .contains("`🔒` aarch64-windows build-exe segfault"),
            "🔒 row must carry the reason: {}",
            col_status(embed).value,
        );
    }

    #[test]
    fn excluded_platform_without_reason_renders_bare_lock() {
        let mut summary = make_all_green_summary();
        summary.versions[0].platforms_excluded = vec![ExcludedPlatform {
            platform: "windows/arm64".to_string(),
            reason: None,
        }];
        let messages = build_messages(&summary, None);
        let status = &col_status(only_embed(&messages)).value;
        assert!(status.contains("`🔒`"), "bare 🔒 chip expected: {status}");
        assert!(status.ends_with("`🔒`"), "no trailing reason text: {status}");
    }

    // ── In-message mention ─────────────────────────────────────────────────

    #[test]
    fn no_mention_when_user_id_absent_even_on_failure() {
        let messages = build_messages(&make_partial_summary(), None);
        assert!(messages[0].content.is_none(), "no content without a user id");
        assert!(messages[0].allowed_mentions.is_none());
    }

    #[test]
    fn no_mention_when_all_green_even_with_user_id() {
        let messages = build_messages(&make_all_green_summary(), Some("123456789012345678"));
        assert!(
            messages[0].content.is_none(),
            "all-green message must not ping: {:?}",
            messages[0].content
        );
        assert!(messages[0].allowed_mentions.is_none());
    }

    #[test]
    fn partial_message_pings_user_scoped() {
        let id = "123456789012345678";
        let messages = build_messages(&make_partial_summary(), Some(id));
        assert_eq!(messages[0].content.as_deref(), Some("<@123456789012345678>"));
        let allowed = messages[0].allowed_mentions.as_ref().expect("ping must scope mentions");
        assert!(allowed.parse.is_empty(), "parse must be empty so only the user pings");
        assert_eq!(
            allowed.users.as_deref(),
            Some(["123456789012345678".to_string()].as_slice())
        );
    }

    #[test]
    fn failed_message_pings_user() {
        let messages = build_messages(&make_all_failed_summary(), Some("123456789012345678"));
        assert_eq!(messages[0].content.as_deref(), Some("<@123456789012345678>"));
        assert!(messages[0].allowed_mentions.is_some());
    }

    // ── Per-version embeds + ≤10/message batching ──────────────────────────

    #[test]
    fn one_embed_per_version() {
        let versions = vec![
            {
                let mut v = version(VersionStatus::Published, "3.7.0");
                v.platforms_pushed = vec!["linux/amd64".to_string()];
                v
            },
            {
                let mut v = version(VersionStatus::Published, "3.8.0");
                v.platforms_pushed = vec!["linux/amd64".to_string()];
                v
            },
        ];
        let summary = run_summary(versions, true, false);
        let messages = build_messages(&summary, None);
        assert_eq!(messages.len(), 1, "two versions fit in one message");
        assert_eq!(messages[0].embeds.len(), 2, "one embed per version");
        assert_eq!(messages[0].embeds[0].title, "ocx.sh/shfmt: 3.7.0 published");
        assert_eq!(messages[0].embeds[1].title, "ocx.sh/shfmt: 3.8.0 published");
    }

    #[test]
    fn skipped_existing_versions_produce_no_embed() {
        let versions = vec![
            {
                let mut v = version(VersionStatus::Published, "3.8.0");
                v.platforms_pushed = vec!["linux/amd64".to_string()];
                v
            },
            version(VersionStatus::SkippedExisting, "3.7.0"),
        ];
        let summary = run_summary(versions, true, false);
        let messages = build_messages(&summary, None);
        assert_eq!(messages[0].embeds.len(), 1, "skipped-existing version yields no embed");
        assert_eq!(messages[0].embeds[0].title, "ocx.sh/shfmt: 3.8.0 published");
    }

    #[test]
    fn embeds_batch_into_messages_of_at_most_ten() {
        // 11 published versions → 2 messages (10 + 1).
        let versions: Vec<VersionSummary> = (0..11)
            .map(|i| {
                let mut v = version(VersionStatus::Published, &format!("1.0.{i}"));
                v.platforms_pushed = vec!["linux/amd64".to_string()];
                v
            })
            .collect();
        let summary = run_summary(versions, true, false);
        let messages = build_messages(&summary, None);
        assert_eq!(messages.len(), 2, "11 versions must spill into a second message");
        assert_eq!(messages[0].embeds.len(), 10);
        assert_eq!(messages[1].embeds.len(), 1);
    }

    #[test]
    fn each_message_with_a_failure_carries_the_ping() {
        let id = "123456789012345678";
        // 11 versions, the 11th (second message) is the only failure.
        let mut versions: Vec<VersionSummary> = (0..10)
            .map(|i| {
                let mut v = version(VersionStatus::Published, &format!("1.0.{i}"));
                v.platforms_pushed = vec!["linux/amd64".to_string()];
                v
            })
            .collect();
        let mut failing = version(VersionStatus::Failed, "1.0.99");
        failing.platforms_failed = vec![PlatformFailure {
            platform: "linux/amd64".to_string(),
            reason: "test_failed".to_string(),
            failed_tests: vec![],
            job_url: None,
        }];
        versions.push(failing);
        let summary = run_summary(versions, true, true);
        let messages = build_messages(&summary, Some(id));
        assert_eq!(messages.len(), 2);
        // First message is all green → no ping.
        assert!(messages[0].content.is_none(), "all-green first message must not ping");
        // Second message carries the failure → ping.
        assert_eq!(messages[1].content.as_deref(), Some("<@123456789012345678>"));
    }

    #[test]
    fn only_first_embed_carries_author_and_thumbnail() {
        let mut summary = make_all_green_summary();
        summary.source_url = Some("https://github.com/mvdan/sh".to_string());
        summary.logo_url = Some("https://raw.githubusercontent.com/ocx-sh/mirror-shfmt/abc/logo.png".to_string());
        summary.versions.push({
            let mut v = version(VersionStatus::Published, "3.8.0");
            v.platforms_pushed = vec!["linux/amd64".to_string()];
            v
        });
        let messages = build_messages(&summary, None);
        let embeds = &messages[0].embeds;
        assert_eq!(embeds.len(), 2);
        assert!(embeds[0].author.is_some(), "first embed carries the author strip");
        assert!(embeds[0].thumbnail.is_some(), "first embed carries the thumbnail");
        assert!(embeds[1].author.is_none(), "later embeds drop the author strip");
        assert!(embeds[1].thumbnail.is_none(), "later embeds drop the thumbnail");
    }

    #[test]
    fn first_emitted_embed_decorated_when_leading_version_yields_no_embed() {
        // Regression: versions are stored oldest-first, so versions[0] is often
        // an already-published SkippedExisting version that produces no embed.
        // The author strip + thumbnail must decorate the first *emitted* embed,
        // not summary.versions[0]. (Bug: decorate was keyed on the loop index,
        // so the sole visible embed lost its author + thumbnail.)
        let mut summary = make_all_green_summary();
        summary.source_url = Some("https://github.com/mvdan/sh".to_string());
        summary.logo_url = Some("https://raw.githubusercontent.com/ocx-sh/mirror-shfmt/abc/logo.png".to_string());
        // Prepend an older, already-mirrored version with no rows → no embed.
        summary
            .versions
            .insert(0, version(VersionStatus::SkippedExisting, "3.6.0"));
        let messages = build_messages(&summary, None);
        let embeds = &messages[0].embeds;
        assert_eq!(embeds.len(), 1, "only the published version yields an embed");
        assert!(
            embeds[0].author.is_some(),
            "the first emitted embed must carry the author strip"
        );
        assert!(
            embeds[0].thumbnail.is_some(),
            "the first emitted embed must carry the thumbnail"
        );
    }

    #[test]
    fn build_author_renders_github_owner_and_repo_with_avatar() {
        let mut summary = make_all_green_summary();
        summary.source_url = Some("https://github.com/mvdan/sh".to_string());
        let author = build_author(&summary).expect("github source_url must yield author");
        assert_eq!(author.name, "mvdan/sh");
        assert_eq!(author.url.as_deref(), Some("https://github.com/mvdan/sh"));
        assert_eq!(author.icon_url.as_deref(), Some("https://github.com/mvdan.png?size=64"));
    }

    #[test]
    fn build_author_uses_generic_label_for_non_github_url() {
        let mut summary = make_all_green_summary();
        summary.source_url = Some("https://example.org/project".to_string());
        let author = build_author(&summary).expect("non-empty source_url must yield author");
        assert_eq!(author.name, "View source");
        assert!(author.icon_url.is_none());
    }

    #[test]
    fn build_thumbnail_omits_when_logo_url_unset() {
        assert!(build_thumbnail(None).is_none());
        assert!(build_thumbnail(Some("")).is_none());
        assert!(build_thumbnail(Some("   ")).is_none());
    }

    #[test]
    fn webhook_env_var_name_is_conventional() {
        assert_eq!(WEBHOOK_ENV_VAR, "OCX_MIRROR_DISCORD_HOOK");
        assert_eq!(USER_ID_ENV_VAR, "OCX_MIRROR_DISCORD_USER_ID");
    }

    // ── HTTP-interaction tests (local TCP server) ──────────────────────────

    fn ensure_crypto_provider() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    /// Spawn a minimal HTTP server that accepts one request and responds with `status_code`.
    async fn one_shot_server(status_code: u16) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/webhook");

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let response = format!("HTTP/1.1 {status_code} \r\nContent-Length: 0\r\n\r\n");
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
        let printer = ocx_lib::cli::DataInterface::new(ocx_lib::cli::Printer::new(false, false));
        let cmd = Notify {
            run_summary: f.path().to_path_buf(),
        };
        let result = cmd.execute(&printer).await;
        let _ = f;
        result
    }

    #[tokio::test]
    async fn notify_posts_green_embed_for_all_new_green() {
        let result = post_to_stub(&make_all_green_summary(), 204).await;
        assert!(matches!(result, Ok(())), "2xx response must yield Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn notify_posts_yellow_embed_for_partial() {
        let result = post_to_stub(&make_partial_summary(), 200).await;
        assert!(matches!(result, Ok(())), "2xx response must yield Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn notify_posts_red_embed_for_all_failed() {
        let result = post_to_stub(&make_all_failed_summary(), 200).await;
        assert!(matches!(result, Ok(())), "2xx response must yield Ok(()): {result:?}");
    }

    #[tokio::test]
    async fn notify_discord_5xx_returns_webhook_unavailable() {
        let result = post_to_stub(&make_all_green_summary(), 503).await;
        assert!(
            matches!(result, Err(MirrorError::WebhookUnavailable(_))),
            "5xx must return WebhookUnavailable: {result:?}"
        );
    }

    #[tokio::test]
    async fn notify_discord_401_returns_webhook_permission_denied() {
        let result = post_to_stub(&make_all_green_summary(), 401).await;
        assert!(
            matches!(result, Err(MirrorError::WebhookPermissionDenied(_))),
            "401 must return WebhookPermissionDenied: {result:?}"
        );
    }

    #[tokio::test]
    async fn notify_discord_403_returns_webhook_permission_denied() {
        let result = post_to_stub(&make_all_green_summary(), 403).await;
        assert!(
            matches!(result, Err(MirrorError::WebhookPermissionDenied(_))),
            "403 must return WebhookPermissionDenied: {result:?}"
        );
    }

    // Regression: clip panicked at `s[..budget]` when the byte at `budget`
    // landed inside a multi-byte emoji codepoint (🟢 = 4 bytes).
    #[test]
    fn clip_to_field_limit_handles_emoji_at_byte_boundary() {
        let cell = "[`🟢`](https://example.com/job/1)";
        let mut s = String::new();
        while s.len() <= DISCORD_FIELD_VALUE_LIMIT {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(cell);
        }
        let clipped = clip_to_field_limit(&s);
        assert!(clipped.len() <= DISCORD_FIELD_VALUE_LIMIT);
        assert!(clipped.is_char_boundary(clipped.len()));
    }
}
