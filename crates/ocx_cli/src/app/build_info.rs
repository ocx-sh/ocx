// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Compile-time-baked build provenance.
//!
//! [`Provenance::current`] returns the build metadata embedded into the
//! binary by [`build.rs`](../../build.rs): git commit + dirty flag, build
//! timestamp + profile + target + rustc version, GitHub Actions run URL
//! (when built in CI), and release channel. Every field is [`Option`]
//! because a tarball checkout without `.git/` or a local `cargo build`
//! outside CI cannot populate the missing piece, and an absent field must
//! omit cleanly from `ocx version --format json`.
//!
//! ## Hermetic-subprocess invariant
//!
//! Every value here is `option_env!()` — resolved at compile time and
//! baked into the binary as `&'static str` constants. None of these
//! accessors read from runtime `std::env`, so the
//! `query_installed_version` subprocess path
//! (`update_check.rs::query_installed_version`, which uses `env_clear()`
//! before spawning `ocx --format json version`) still receives the same
//! values it would in any other invocation.

use serde::Serialize;

/// Length of the abbreviated git SHA shown to humans (8 hex chars).
const SHORT_SHA_LEN: usize = 8;

/// Full build provenance for the running binary.
///
/// JSON shape (all sub-objects optional; absent in JSON when source data
/// was unavailable at build time):
///
/// ```json
/// {
///   "channel": "dev",
///   "commit": { "sha": "…", "short": "…", "describe": "…",
///               "dirty": false, "timestamp": "…" },
///   "build":  { "timestamp": "…", "profile": "release",
///               "target":    "…", "rustc":   "…" },
///   "ci":     { "provider":   "github-actions",
///               "run_url":    "…", "workflow": "…",
///               "ref":        "…", "sha":      "…" }
/// }
/// ```
#[derive(Serialize)]
pub struct Provenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiInfo>,
}

/// Git commit metadata baked at build time.
#[derive(Serialize)]
pub struct CommitInfo {
    /// Full 40-character SHA-1.
    pub sha: String,
    /// 8-character abbreviated SHA — convenience for humans.
    pub short: String,
    /// `git describe --tags --dirty` output, including any `-dirty` suffix.
    pub describe: String,
    /// `true` if the working tree had uncommitted changes when built.
    pub dirty: bool,
    /// ISO-8601 commit timestamp (author date).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// Build environment metadata (timestamp, profile, target, rustc).
#[derive(Serialize)]
pub struct BuildInfo {
    /// ISO-8601 UTC build timestamp.
    pub timestamp: String,
    /// `"release"` or `"debug"` — derived from the `CARGO_DEBUG` flag.
    pub profile: &'static str,
    /// Target triple the binary was compiled for.
    pub target: String,
    /// `rustc` version that compiled the binary.
    pub rustc: String,
}

/// GitHub Actions context. Present only when built under a GitHub
/// workflow that exported the standard `GITHUB_*` env vars.
#[derive(Serialize)]
pub struct CiInfo {
    pub provider: &'static str,
    /// Direct link to the run that produced this binary.
    /// Composed as `{server_url}/{repository}/actions/runs/{run_id}`.
    pub run_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

impl Provenance {
    /// Build the [`Provenance`] for the running binary by reading the
    /// compile-time-baked env vars.
    pub fn current() -> Self {
        Self {
            channel: option_env!("__OCX_BUILD_CHANNEL"),
            commit: commit_info(),
            build: build_info(),
            ci: ci_info(),
        }
    }
}

fn commit_info() -> Option<CommitInfo> {
    let sha = option_env!("VERGEN_GIT_SHA")?;
    let describe = option_env!("VERGEN_GIT_DESCRIBE").unwrap_or(sha);
    let dirty = matches!(option_env!("VERGEN_GIT_DIRTY"), Some("true"));
    let short = sha.get(..SHORT_SHA_LEN).unwrap_or(sha).to_owned();
    Some(CommitInfo {
        sha: sha.to_owned(),
        short,
        describe: describe.to_owned(),
        dirty,
        timestamp: option_env!("VERGEN_GIT_COMMIT_TIMESTAMP").map(str::to_owned),
    })
}

fn build_info() -> Option<BuildInfo> {
    let timestamp = option_env!("VERGEN_BUILD_TIMESTAMP")?.to_owned();
    let target = option_env!("VERGEN_CARGO_TARGET_TRIPLE")?.to_owned();
    let rustc = option_env!("VERGEN_RUSTC_SEMVER")?.to_owned();
    let profile = match option_env!("VERGEN_CARGO_DEBUG") {
        Some("true") => "debug",
        _ => "release",
    };
    Some(BuildInfo {
        timestamp,
        profile,
        target,
        rustc,
    })
}

fn ci_info() -> Option<CiInfo> {
    let server_url = option_env!("GITHUB_SERVER_URL")?;
    let repository = option_env!("GITHUB_REPOSITORY")?;
    let run_id = option_env!("GITHUB_RUN_ID")?;
    let run_url = format!("{server_url}/{repository}/actions/runs/{run_id}");
    Some(CiInfo {
        provider: "github-actions",
        run_url,
        workflow: option_env!("GITHUB_WORKFLOW").map(str::to_owned),
        git_ref: option_env!("GITHUB_REF").map(str::to_owned),
        sha: option_env!("GITHUB_SHA").map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Provenance::current()` is callable in unit tests. Each optional
    /// block is populated when its backing env vars happen to be exported
    /// at test-binary build time (`option_env!` is compile-time):
    ///
    /// - `build` — present when vergen-gix is active (CI + most dev builds),
    ///   absent only in stripped/offline builds.
    /// - `ci` — present when the test binary is compiled inside CI itself
    ///   (GitHub Actions exports `GITHUB_*`); absent locally.
    /// - `channel` — present only when `__OCX_BUILD_CHANNEL` is set at
    ///   compile time (cargo-dist release jobs).
    ///
    /// Assert structural invariants on each populated block instead of
    /// presence, so the test passes in every build environment.
    #[test]
    fn current_is_callable() {
        let info = Provenance::current();
        if let Some(build) = &info.build {
            assert!(!build.target.is_empty(), "target triple must be non-empty");
            assert!(!build.rustc.is_empty(), "rustc semver must be non-empty");
        }
        if let Some(ci) = &info.ci {
            assert!(!ci.provider.is_empty(), "CI provider must be non-empty");
            assert!(!ci.run_url.is_empty(), "CI run URL must be non-empty");
        }
        if let Some(channel) = info.channel {
            assert!(!channel.is_empty(), "channel must be non-empty when present");
        }
    }

    /// JSON output skips empty blocks rather than emitting `null` —
    /// matches the `#[serde(skip_serializing_if = "Option::is_none")]`
    /// contract documented on `Provenance`.
    #[test]
    fn empty_provenance_serializes_to_empty_object() {
        let info = Provenance {
            channel: None,
            commit: None,
            build: None,
            ci: None,
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value, serde_json::json!({}));
    }

    /// A populated `CiInfo` round-trips with the `ref` field renamed
    /// (Rust keyword) — `git_ref` field in struct, `"ref"` key in JSON.
    #[test]
    fn ci_info_renames_ref_field() {
        let ci = CiInfo {
            provider: "github-actions",
            run_url: "https://example/run".into(),
            workflow: None,
            git_ref: Some("refs/heads/main".into()),
            sha: None,
        };
        let value = serde_json::to_value(&ci).unwrap();
        assert_eq!(value.get("ref").and_then(|v| v.as_str()), Some("refs/heads/main"));
        assert!(value.get("git_ref").is_none(), "Rust field name must not leak");
    }
}
