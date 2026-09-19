// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The OCI annotations a push derives from the CI environment it runs in.
//!
//! Every variable is read through [`ocx_util::env::var`] — the *runtime* reader.
//! `app::build_info`'s `GITHUB_*` reads look like the same thing and are not:
//! those are `option_env!`, resolved when ocx itself was compiled, which is
//! exactly what makes that module hermetic. The environment that matters here
//! is the publishing job's, so it must be read at runtime.

use std::collections::BTreeMap;

use crate::ci::CiFlavor;
use ocx_oci::annotations::{CREATED, REVISION, SOURCE, VERSION};
use ocx_oci::referrer::manifest::{bundle_created, pinned_instant};
use ocx_package::version::Version;

/// Builds the annotation set `flavor`'s environment describes.
///
/// `version` is the resolved push identifier's version. Its variant prefix is
/// stripped, so a `full-1.2.3` push annotates `1.2.3` — a variant names a
/// build of a version, not a version of its own. `None` (a tag that is not a
/// version) writes no version key rather than failing the push.
///
/// A variable that is unset or blank yields no key at all:
/// `org.opencontainers.image.source=""` on a published index states, wrongly,
/// that the publisher answered the question. `created` is the one key always
/// written, because it has a source that cannot be missing — the clock.
pub fn for_flavor(flavor: CiFlavor, version: Option<&Version>) -> BTreeMap<String, String> {
    let mut annotations = BTreeMap::new();
    if let Some(source) = source(flavor) {
        annotations.insert(SOURCE.to_string(), source);
    }
    let revision = match flavor {
        CiFlavor::GitHubActions => "GITHUB_SHA",
        CiFlavor::GitLab => "CI_COMMIT_SHA",
    };
    if let Some(revision) = var(revision) {
        annotations.insert(REVISION.to_string(), revision);
    }
    annotations.insert(CREATED.to_string(), created(flavor));
    if let Some(version) = version {
        annotations.insert(VERSION.to_string(), version.without_variant().to_string());
    }
    annotations
}

/// The source repository URL, or `None` when the environment does not name one.
fn source(flavor: CiFlavor) -> Option<String> {
    match flavor {
        // Two variables, one value: GitHub names the forge and the repository
        // separately and publishes no join of them. The trailing-slash trim is
        // for self-hosted GHES, where the server URL is operator-typed.
        CiFlavor::GitHubActions => {
            let server = var("GITHUB_SERVER_URL")?;
            let repository = var("GITHUB_REPOSITORY")?;
            Some(format!("{}/{repository}", server.trim_end_matches('/')))
        }
        CiFlavor::GitLab => var("CI_PROJECT_URL"),
    }
}

/// The instant to stamp, RFC 3339 with seconds precision — the spelling every
/// other `created` OCX writes uses.
///
/// `SOURCE_DATE_EPOCH` outranks CI's own pipeline clock. A build that pins its
/// instant for reproducibility must not be re-stamped here, or the index and
/// the attestation the same push writes disagree about when it happened. Both
/// candidate instants are parsed and re-emitted through [`bundle_created`], so
/// every `created` value shares one spelling regardless of its source.
fn created(flavor: CiFlavor) -> String {
    let instant = pinned_instant().or_else(|| match flavor {
        // GitHub Actions exposes no pipeline-creation timestamp.
        CiFlavor::GitHubActions => None,
        CiFlavor::GitLab => pipeline_created_at(),
    });
    bundle_created(instant.unwrap_or_else(chrono::Utc::now))
}

/// GitLab's `$CI_PIPELINE_CREATED_AT`, parsed as RFC 3339, or `None` when the
/// variable is unset, blank, or not a valid timestamp.
///
/// Parsed and re-emitted through [`bundle_created`] by the caller rather than
/// passed through verbatim, so a GitLab pipeline clock lands in the same
/// spelling every other `created` OCX stamps uses (UTC, seconds precision,
/// literal `Z`) — and a runner that exports a malformed value warns and falls
/// back to the wall clock instead of writing garbage onto the published index.
fn pipeline_created_at() -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = var("CI_PIPELINE_CREATED_AT")?;
    match chrono::DateTime::parse_from_rfc3339(&raw) {
        Ok(parsed) => Some(parsed.with_timezone(&chrono::Utc)),
        Err(_) => {
            // The key alone, never the value: a CI job log is durable and read
            // by more parties than the process environment.
            log::warn!("ignoring malformed CI_PIPELINE_CREATED_AT; stamping the wall clock instead");
            None
        }
    }
}

/// A runtime environment variable, blank treated as absent and surrounding
/// whitespace trimmed (a CI variable interpolated from a file keeps its
/// newline, and an annotation is not the place to discover that).
fn var(key: &str) -> Option<String> {
    ocx_util::env::var(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_util::env::overrides::{self as env, EnvLock};

    /// Every variable [`for_flavor`] can read, so a test states the whole
    /// environment rather than inheriting the developer's own `GITHUB_*`.
    const READS: [&str; 6] = [
        "GITHUB_SERVER_URL",
        "GITHUB_REPOSITORY",
        "GITHUB_SHA",
        "CI_PROJECT_URL",
        "CI_COMMIT_SHA",
        "CI_PIPELINE_CREATED_AT",
    ];

    /// Locks the environment and replaces it wholesale with `vars`;
    /// `SOURCE_DATE_EPOCH` is cleared unless named.
    fn only(vars: &[(&str, &str)]) -> EnvLock {
        let lock = env::lock();
        for key in READS.iter().chain(["SOURCE_DATE_EPOCH"].iter()) {
            lock.remove(*key);
        }
        for (key, value) in vars {
            lock.set(*key, *value);
        }
        lock
    }

    fn version(tag: &str) -> Version {
        Version::parse(tag).expect("fixture tag is a version")
    }

    #[test]
    fn github_actions_joins_the_server_url_and_the_repository() {
        let _lock = only(&[
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "ocx-sh/ocx"),
            ("GITHUB_SHA", "cafebabe"),
        ]);

        let annotations = for_flavor(CiFlavor::GitHubActions, Some(&version("1.2.3")));

        assert_eq!(
            annotations.get(SOURCE).map(String::as_str),
            Some("https://github.com/ocx-sh/ocx")
        );
        assert_eq!(annotations.get(REVISION).map(String::as_str), Some("cafebabe"));
        assert_eq!(annotations.get(VERSION).map(String::as_str), Some("1.2.3"));
        assert!(annotations.contains_key(CREATED), "{annotations:?}");
    }

    #[test]
    fn gitlab_reads_its_own_four_variables() {
        let _lock = only(&[
            ("CI_PROJECT_URL", "https://gitlab.example.com/acme/widget"),
            ("CI_COMMIT_SHA", "0123456789abcdef"),
            ("CI_PIPELINE_CREATED_AT", "2026-09-10T08:30:00Z"),
        ]);

        let annotations = for_flavor(CiFlavor::GitLab, Some(&version("2.0.0")));

        assert_eq!(
            annotations,
            BTreeMap::from([
                (SOURCE.to_string(), "https://gitlab.example.com/acme/widget".to_string()),
                (REVISION.to_string(), "0123456789abcdef".to_string()),
                (CREATED.to_string(), "2026-09-10T08:30:00Z".to_string()),
                (VERSION.to_string(), "2.0.0".to_string()),
            ])
        );
    }

    /// A GitLab job never sets `GITHUB_*`, and a GitHub job never sets `CI_*`
    /// — reading the wrong flavor's variables would annotate someone else's
    /// forge.
    #[test]
    fn a_flavor_reads_only_its_own_variables() {
        let _lock = only(&[
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "ocx-sh/ocx"),
            ("GITHUB_SHA", "cafebabe"),
        ]);

        let annotations = for_flavor(CiFlavor::GitLab, None);

        assert!(!annotations.contains_key(SOURCE), "{annotations:?}");
        assert!(!annotations.contains_key(REVISION), "{annotations:?}");
    }

    #[test]
    fn an_unset_or_blank_variable_writes_no_key() {
        let _lock = only(&[
            ("CI_PROJECT_URL", "  "),
            ("CI_PIPELINE_CREATED_AT", "2026-09-10T08:30:00Z"),
        ]);

        let annotations = for_flavor(CiFlavor::GitLab, None);

        assert_eq!(
            annotations,
            BTreeMap::from([(CREATED.to_string(), "2026-09-10T08:30:00Z".to_string())])
        );
    }

    #[test]
    fn a_variant_tag_annotates_the_bare_version() {
        let _lock = only(&[]);

        let annotations = for_flavor(CiFlavor::GitLab, Some(&version("full-1.2.3")));

        assert_eq!(annotations.get(VERSION).map(String::as_str), Some("1.2.3"));
    }

    #[test]
    fn source_date_epoch_outranks_the_pipeline_clock() {
        let _lock = only(&[
            ("CI_PIPELINE_CREATED_AT", "2026-09-10T08:30:00Z"),
            ("SOURCE_DATE_EPOCH", " 1700000000 "),
        ]);

        let annotations = for_flavor(CiFlavor::GitLab, None);

        assert_eq!(
            annotations.get(CREATED).map(String::as_str),
            Some("2023-11-14T22:13:20Z"),
            "{annotations:?}"
        );
    }

    #[test]
    fn a_malformed_source_date_epoch_falls_back_to_the_pipeline_clock() {
        let _lock = only(&[
            ("CI_PIPELINE_CREATED_AT", "2026-09-10T08:30:00Z"),
            ("SOURCE_DATE_EPOCH", "yesterday"),
        ]);

        let annotations = for_flavor(CiFlavor::GitLab, None);

        assert_eq!(
            annotations.get(CREATED).map(String::as_str),
            Some("2026-09-10T08:30:00Z"),
            "{annotations:?}"
        );
    }

    /// Nothing in the environment still stamps an instant: `created` is the
    /// key with a source that cannot go missing.
    #[test]
    fn an_empty_environment_still_stamps_created() {
        let _lock = only(&[]);

        let annotations = for_flavor(CiFlavor::GitHubActions, None);

        assert_eq!(annotations.len(), 1, "{annotations:?}");
        assert!(annotations.contains_key(CREATED), "{annotations:?}");
    }

    /// **W6.** A self-hosted GHES server URL is operator-typed and routinely
    /// carries a trailing slash; the join must trim it, not double it.
    #[test]
    fn github_enterprise_server_url_trailing_slash_is_trimmed() {
        let _lock = only(&[
            ("GITHUB_SERVER_URL", "https://ghe.corp.example/"),
            ("GITHUB_REPOSITORY", "team/tool"),
        ]);

        let annotations = for_flavor(CiFlavor::GitHubActions, None);

        assert_eq!(
            annotations.get(SOURCE).map(String::as_str),
            Some("https://ghe.corp.example/team/tool"),
            "the trailing slash must be trimmed, not doubled: {annotations:?}"
        );
    }

    /// **W6.** The source URL needs both halves: a server URL with no
    /// repository names nothing, so no source key is written rather than a
    /// truncated `server/` one.
    #[test]
    fn a_partial_github_source_writes_no_source_key() {
        let _lock = only(&[("GITHUB_SERVER_URL", "https://github.com")]);

        let annotations = for_flavor(CiFlavor::GitHubActions, None);

        assert!(
            !annotations.contains_key(SOURCE),
            "a server URL without a repository must not write a truncated source: {annotations:?}"
        );
    }

    /// **H11 (D10).** GitLab's `$CI_PIPELINE_CREATED_AT` is parsed as RFC 3339
    /// and re-emitted, so a sub-second-precision or non-UTC value lands in the
    /// one canonical spelling rather than being passed through verbatim.
    #[test]
    fn a_gitlab_pipeline_timestamp_is_normalized_to_utc_seconds() {
        let _lock = only(&[("CI_PIPELINE_CREATED_AT", "2026-09-10T10:30:00.123456+02:00")]);

        let annotations = for_flavor(CiFlavor::GitLab, None);

        assert_eq!(
            annotations.get(CREATED).map(String::as_str),
            Some("2026-09-10T08:30:00Z"),
            "the offset must convert to UTC and the sub-second precision drop: {annotations:?}"
        );
    }

    /// **H11 (D10).** A runner that exports a garbage `$CI_PIPELINE_CREATED_AT`
    /// must not stamp it onto the published index verbatim: the value is
    /// rejected and the wall clock stamped instead. Asserted by shape (a
    /// canonical RFC 3339 instant that is not the garbage), so it holds whatever
    /// the wall clock reads.
    #[test]
    fn a_malformed_gitlab_pipeline_timestamp_falls_back_to_the_clock() {
        let _lock = only(&[("CI_PIPELINE_CREATED_AT", "last tuesday")]);

        let annotations = for_flavor(CiFlavor::GitLab, None);

        let created = annotations
            .get(CREATED)
            .map(String::as_str)
            .expect("created is always stamped");
        assert_ne!(
            created, "last tuesday",
            "a malformed value must never be passed through verbatim"
        );
        chrono::DateTime::parse_from_rfc3339(created)
            .unwrap_or_else(|error| panic!("created must be canonical RFC 3339, got {created:?}: {error}"));
    }
}
