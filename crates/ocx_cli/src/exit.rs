// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error -> [`ExitCode`] classification for the `ocx` binary.

use ocx_exit::{ClassifyErrorKind, ClassifyExitCode, Decision, Detail, ExitCode};

use crate::app::CliRefusal;
use crate::app::project_context::ProjectContextError;

mod classify;
#[cfg(test)]
mod cli_input;
#[cfg(test)]
mod ocx_announce;
#[cfg(test)]
mod ocx_config;
#[cfg(test)]
mod ocx_env;
#[cfg(test)]
mod ocx_index;
#[cfg(test)]
mod ocx_oci;
#[cfg(test)]
mod ocx_package;
#[cfg(test)]
mod ocx_package_manager;
#[cfg(test)]
mod ocx_project;
#[cfg(test)]
mod ocx_setup;
#[cfg(test)]
mod ocx_shell;
#[cfg(test)]
mod ocx_sign;
#[cfg(test)]
mod ocx_store;
#[cfg(test)]
mod ocx_util;

// Registry order: the first family's description wins for a slug two families share. Ladder order is
// not precedence, every downcast being exact. `: rows_only` lists a type without a rung.
ocx_exit::families!(
    crate::error::UsageError,
    crate::error::MetadataResolutionError,
    crate::error::RetiredEnvError,
    // ocx_package
    ::ocx_package::publisher::CopyError,
    ::ocx_oci::layer_ref::LayerRefParseError,
    ::ocx_package::publisher::PublishGateError,
    ::ocx_package::bin_scan::BinScanError,
    ::ocx_package::dependency_pinning::DependencyPinningError,
    ::ocx_package::error::Error,
    ::ocx_package::prune::PruneError,
    ::ocx_package::libc_lint::LibcLintError,
    ::ocx_package::metadata::authoring::AuthoringError,
    ::ocx_package::metadata::dependency::DependencyError: rows_only,
    ::ocx_package::metadata::template::TemplateError,
    // ocx_util
    ::ocx_util::archive::Error,
    ::ocx_util::compression::error::Error,
    ::ocx_util::singleflight::Error,
    ::ocx_util::fs::EmptyOrAbsentError,
    ::ocx_util::fs::path::PathEscapeError,
    ::ocx_util::fs::SameFilesystemError,
    ::ocx_util::boolean_string::BooleanStringError,
    ::ocx_util::error::FileError,
    ::ocx_util::error::SerializationError,
    ::ocx_util::fs::SymlinkWalkError,
    ::ocx_util::error::Error,
    // ocx_project
    ::ocx_package_manager::activation::SessionError,
    ::ocx_project::error::Error,
    ::ocx_project::ProjectErrorKind: rows_only,
    ::ocx_project::LockCurrency: rows_only,
    ::ocx_project::registry::error::Error: rows_only,
    ::ocx_project::registry::ProjectRegistryErrorKind: rows_only,
    // ocx_config
    ::ocx_config::ToolchainRootError: rows_only,
    ::ocx_package::metadata::env::apply::ListSeparatorError,
    ::ocx_config::env::CommandResolutionError,
    ::ocx_package::metadata::env::apply::ForwardedEnvError,
    ::ocx_config::managed_config::ManagedConfigFetchError,
    ::ocx_config::managed_config::ManagedConfigPersistError,
    ::ocx_config::managed_config::ManagedConfigUpdateError,
    ::ocx_package_manager::managed_config::ManagedConfigPublishError,
    ::ocx_config::edit::EditError,
    ::ocx_config::error::Error,
    ::ocx_config::managed::ManagedConfigError,
    ::ocx_config::mirror::MirrorConfigError,
    ::ocx_config::patch::PatchConfigError,
    ::ocx_package::launch::LaunchIdentityError,
    ::ocx_config::tls::TlsError,
    // ocx_env
    ::ocx_env::InvalidEnv,
    // ocx_oci
    ::ocx_oci::endpoint::UrlRejection,
    ::ocx_oci::layer_layout::LayerLayoutError,
    ::ocx_oci::pinned_package_ref::PinnedIdentifierError,
    ::ocx_oci::ssrf::SsrfError,
    ::ocx_oci::client::error::ClientError,
    ::ocx_oci::digest::error::DigestError,
    ::ocx_oci::platform::error::PlatformError,
    ::ocx_oci::package_ref::error::IdentifierError,
    ::ocx_oci::auth::error::AuthError,
    // ocx_index
    ::ocx_index::error::Error,
    // ocx_announce
    ::ocx_announce::claim::ClaimError,
    ::ocx_announce::announce::AnnounceError,
    ::ocx_announce::forge::ForgeError,
    // ocx_package_manager
    ::ocx_package_manager::launch::LaunchError,
    ::ocx_package_manager::patch::PatchError,
    ::ocx_package_manager::error::Error,
    ::ocx_package_manager::error::PackageErrorKind,
    ::ocx_package_manager::error::DependencyError,
    ::ocx_package_manager::record::RecordsError,
    // ocx_store
    ::ocx_store::file_structure::error::Error,
    ::ocx_store::file_structure::ToolchainPathError,
    // ocx_shell
    ::ocx_shell::ci::error::Error,
    // ocx_setup
    ::ocx_setup::error::Error,
    ::ocx_setup::session_path::SessionPathError: rows_only,
    // ocx_sign
    ::ocx_sign::verify::VerifyError,
    ::ocx_sign::sign::SignError,
    // the CLI-local pass answers these before the ladder is reached
    ProjectContextError: rows_only,
    CliRefusal: rows_only,
);

/// One cause's verdict: the exit code and the `error.detail` slug of the same error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Verdict {
    code: ExitCode,
    detail: &'static str,
}

/// Resolve a classified cause into its verdict.
///
/// A `Detail::Chain` answers with the first cause of its `from` the library ladder classifies (that cause's
/// code and slug), falling back to `code` and its fallback row.
fn resolve(decision: Decision<'_>) -> Verdict {
    match decision.detail {
        Detail::Fixed(entry) => Verdict {
            code: decision.code,
            detail: entry.slug,
        },
        Detail::Chain { fallback, from } => library_decision(from).unwrap_or(Verdict {
            code: decision.code,
            detail: fallback.slug,
        }),
    }
}

/// The `error.detail` slug a detail names, its chain walked as [`resolve`] walks it.
pub(crate) fn detail_slug(detail: Detail<'_>) -> &'static str {
    match detail {
        Detail::Fixed(entry) => entry.slug,
        Detail::Chain { fallback, from } => library_decision(from).map_or(fallback.slug, |verdict| verdict.detail),
    }
}

/// Classify an error chain into an [`ExitCode`], CLI-local types first; the exit-code authority for `main.rs`.
pub fn classify_error(err: &(dyn std::error::Error + 'static)) -> ExitCode {
    classify_decision(err).0
}

/// The exit code for `err` and the `error.detail` slug of the cause that decided it.
///
/// Two passes on purpose: merged into one walk, an earlier library cause would outrank a CLI-local one.
pub fn classify_decision(err: &(dyn std::error::Error + 'static)) -> (ExitCode, Option<&'static str>) {
    for cause in std::iter::successors(Some(err), |e| e.source()) {
        if let Some(verdict) = cli_local::<ProjectContextError>(cause).or_else(|| cli_local::<CliRefusal>(cause)) {
            return (verdict.code, Some(verdict.detail));
        }
    }
    library_decision(err).map_or((ExitCode::Failure, None), |verdict| {
        (verdict.code, Some(verdict.detail))
    })
}

/// The verdict of `cause` when it is the CLI-local type `E` and classifies.
fn cli_local<E>(cause: &(dyn std::error::Error + 'static)) -> Option<Verdict>
where
    E: ClassifyExitCode + ClassifyErrorKind + std::error::Error + 'static,
{
    let error = cause.downcast_ref::<E>()?;
    let code = error.classify()?;
    Some(resolve(Decision {
        code,
        detail: error.kind_detail(),
    }))
}

/// Classify a [`std::error::Error`] chain against the library ladder alone, else [`ExitCode::Failure`].
///
/// For aggregating commands folding per-package library failures, whose chains carry no CLI-local cause.
pub fn classify_library_error(err: &(dyn std::error::Error + 'static)) -> ExitCode {
    library_decision(err).map_or(ExitCode::Failure, |verdict| verdict.code)
}

/// The library ladder's verdict on the first cause in `err`'s chain it classifies.
fn library_decision(err: &(dyn std::error::Error + 'static)) -> Option<Verdict> {
    std::iter::successors(Some(err), |cause| cause.source()).find_map(|cause| {
        try_classify(cause)
            .map(resolve)
            .or_else(|| classify::try_downcast(cause))
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::ExitCode;

    // ── `error.detail` registry ──────────────────────────────────────────────

    /// Asserts `error` reports `slug` and that the registry files `slug` under the code `error` exits with.
    ///
    /// The registry, not the family's own table: a delegating arm answers with its cause's slug.
    pub(crate) fn assert_detail<K: ocx_exit::ClassifyErrorKind + std::error::Error>(error: &K, slug: &str) {
        assert_eq!(super::detail_slug(error.kind_detail()), slug, "{error:?}");
        let registry = super::detail_registry();
        let row = registry
            .iter()
            .find(|row| row.slug == slug)
            .unwrap_or_else(|| panic!("{error:?} produces `{slug}`, which no DETAILS table lists"));
        assert_eq!(
            row.exit_code,
            super::classify_error(error),
            "{error:?}: `{slug}` is registered under one exit code and exits with another"
        );
    }

    /// One slug means one exit code, across families too: consumers key the code off the slug.
    ///
    /// Reds on: two rows sharing a slug under different codes, a family listing a slug twice, and a
    /// slug that is not snake_case.
    #[test]
    fn one_slug_carries_one_exit_code() {
        let registry = super::detail_registry();
        assert!(
            !registry.is_empty(),
            "the registry reads empty, so no comparison against it means anything"
        );
        let mut code_of: BTreeMap<&str, (ExitCode, &str)> = BTreeMap::new();
        let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
        for row in &registry {
            assert!(
                !row.slug.is_empty()
                    && row
                        .slug
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                    && !row.slug.starts_with('_')
                    && !row.slug.ends_with('_'),
                "`{}` is not snake_case",
                row.slug
            );
            assert!(
                seen.insert((row.family, row.slug)),
                "{} lists `{}` twice",
                row.family,
                row.slug
            );
            let (code, family) = *code_of.entry(row.slug).or_insert((row.exit_code, row.family));
            assert_eq!(
                code, row.exit_code,
                "`{}` exits {code:?} in {family} but {:?} in {}",
                row.slug, row.exit_code, row.family
            );
        }
    }

    /// A code a consumer can branch on names at least three causes, or it is a one-cause code in disguise.
    ///
    /// The registry is not deduplicated (a slug shared by several enums appears once per enum), so slugs are
    /// counted as a set per code. Reds on: any `ExitCode` but `Success` with fewer than three distinct slugs,
    /// and one with none.
    #[test]
    fn every_exit_code_carries_at_least_three_distinct_detail_slugs() {
        let registry = super::detail_registry();
        assert!(
            !registry.is_empty(),
            "the registry reads empty, so every code would count 0"
        );
        let mut slugs: BTreeMap<u8, BTreeSet<&str>> = BTreeMap::new();
        for row in &registry {
            slugs.entry(row.exit_code as u8).or_default().insert(row.slug);
        }
        let thin: Vec<String> = ExitCode::ALL
            .iter()
            .filter(|code| **code != ExitCode::Success)
            .filter_map(|code| {
                let count = slugs.get(&(*code as u8)).map_or(0, BTreeSet::len);
                (count < 3).then(|| format!("{code:?} ({}): {count} distinct slugs", *code as u8))
            })
            .collect();
        assert!(thin.is_empty(), "codes under three distinct slugs: {thin:?}");
    }

    /// No registry row files a slug under a number `ocx_exit::RETIRED` withdrew.
    ///
    /// Reds on: a detail row whose code carries a retired number.
    #[test]
    fn no_registered_detail_exits_a_retired_number() {
        let registry = super::detail_registry();
        assert!(!registry.is_empty(), "the registry reads empty, so nothing was checked");
        let reused: Vec<String> = registry
            .iter()
            .filter(|row| ocx_exit::RETIRED.contains(&(row.exit_code as u8)))
            .map(|row| format!("`{}` ({}) exits retired {}", row.slug, row.family, row.exit_code as u8))
            .collect();
        assert!(reused.is_empty(), "{}", reused.join("\n"));
    }

    /// `(family, slug, has a source)` of one instance, the family read off its `DETAILS` rows.
    pub(crate) fn deferring_row<K: ocx_exit::ClassifyErrorKind + std::error::Error>(
        kind: K,
    ) -> (String, &'static str, bool) {
        let family = K::DETAILS.first().map_or("", |row| row.family);
        (
            family.to_owned(),
            super::detail_slug(kind.kind_detail()),
            kind.source().is_some(),
        )
    }

    /// One instance per literal-slug variant whose `classify` arm answers `None`.
    fn deferring_literal_variants() -> Vec<(String, &'static str, bool)> {
        super::ocx_announce::tests::deferring_literal_variants()
    }

    /// Reds on: a literal-slug variant whose `classify` defers while it carries a source, so the
    /// chain walk could answer a code other than the slug's exit 1.
    #[test]
    fn every_deferring_literal_variant_is_source_less() {
        let sourced: Vec<_> = deferring_literal_variants()
            .into_iter()
            .filter(|(_, _, has_source)| *has_source)
            .collect();
        assert!(sourced.is_empty(), "deferring yet sourced: {sourced:?}");
    }

    /// A slug several families register reads the same in each: the document publishes one.
    ///
    /// Reds on: two rows of one slug whose summaries differ.
    #[test]
    fn a_shared_slug_carries_one_description() {
        let registry = super::detail_registry();
        let mut summary_of: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
        let mut defects = Vec::new();
        let mut shared = 0;
        for row in &registry {
            let (summary, family) = *summary_of.entry(row.slug).or_insert((row.summary, row.family));
            if family != row.family {
                shared += 1;
            }
            if summary != row.summary {
                defects.push(format!(
                    "`{}`: {family} says {summary:?}, {} says {:?}",
                    row.slug, row.family, row.summary
                ));
            }
        }
        assert!(shared >= 10, "only {shared} shared rows; the registry read short");
        assert!(defects.is_empty(), "{}", defects.join("\n"));
    }

    /// `detail_registry()` as `detail_rows.txt` holds it: sorted, not deduplicated, `family` left out.
    fn rendered_detail_rows() -> String {
        let mut rows: Vec<String> = super::detail_registry()
            .into_iter()
            .map(|entry| format!("{}\t{:?}\t{}\n", entry.slug, entry.exit_code, entry.summary))
            .collect();
        rows.sort();
        rows.concat()
    }

    /// Reds on: a slug, its exit code or its summary changing, or a row dropped or duplicated — the
    /// errors golden deduplicates by slug and cannot see either.
    #[test]
    fn detail_rows_snapshot_matches_the_registry() {
        let snapshot = include_str!("exit/detail_rows.txt");
        assert_eq!(rendered_detail_rows(), snapshot);
    }
}

#[cfg(test)]
mod resolver_tests {
    use ocx_exit::{Classify, ExitCode};

    use super::{Decision, Verdict, detail_slug, resolve};

    /// One variant per `Detail` form a derive can produce.
    #[derive(Debug, thiserror::Error, Classify)]
    enum Fixture {
        #[error("fixed")]
        #[exit(DataError, slug = "fixture_fixed", summary = "A fixed slug")]
        Fixed,
        /// `classify` defers; the walk starts at the error itself.
        #[error("chained")]
        #[exit(chain, fallback(Failure, slug = "fixture_fallback", summary = "No classified cause"))]
        Chained(#[source] std::io::Error),
        /// `classify` decides; the cause may overrule it. `source()` skips the payload.
        #[error("wrapped")]
        #[exit(
            chain = 0,
            fallback(Failure, slug = "fixture_fallback", summary = "No classified cause")
        )]
        Wrapped(std::io::Error),
    }

    mod ladder {
        ocx_exit::families!(super::Fixture);
    }

    fn permission_denied() -> std::io::Error {
        std::io::Error::from(std::io::ErrorKind::PermissionDenied)
    }

    fn unclassified() -> std::io::Error {
        std::io::Error::other("unclassified")
    }

    fn rung(cause: &(dyn std::error::Error + 'static)) -> Option<(ExitCode, &'static str)> {
        ladder::try_classify(cause)
            .map(resolve)
            .map(|Verdict { code, detail }| (code, detail))
    }

    #[test]
    fn the_slug_of_a_fixed_detail_is_its_slug() {
        let error = Fixture::Fixed;
        assert_eq!(
            detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(&error)),
            "fixture_fixed"
        );
    }

    #[test]
    fn the_slug_of_a_chain_is_the_first_classified_causes_slug() {
        for error in [
            Fixture::Chained(permission_denied()),
            Fixture::Wrapped(permission_denied()),
        ] {
            assert_eq!(
                detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(&error)),
                "permission_denied"
            );
        }
    }

    #[test]
    fn the_slug_of_a_chain_with_no_classified_cause_is_the_fallback() {
        for error in [Fixture::Chained(unclassified()), Fixture::Wrapped(unclassified())] {
            assert_eq!(
                detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(&error)),
                "fixture_fallback"
            );
        }
    }

    #[test]
    fn the_rung_decides_a_fixed_type() {
        assert_eq!(rung(&Fixture::Fixed), Some((ExitCode::DataError, "fixture_fixed")));
    }

    #[test]
    fn the_rung_lets_a_deferring_chain_fall_through_to_the_walker() {
        assert_eq!(rung(&Fixture::Chained(permission_denied())), None);
    }

    #[test]
    fn the_rung_answers_with_the_cause_of_a_deciding_chain() {
        assert_eq!(
            rung(&Fixture::Wrapped(permission_denied())),
            Some((ExitCode::PermissionDenied, "permission_denied"))
        );
    }

    #[test]
    fn the_rung_falls_back_to_the_row_when_the_chain_finds_no_cause() {
        assert_eq!(
            rung(&Fixture::Wrapped(unclassified())),
            Some((ExitCode::Failure, "fixture_fallback"))
        );
    }

    #[test]
    fn the_rung_falls_through_on_another_type() {
        assert!(rung(&std::fmt::Error).is_none());
    }

    #[test]
    fn resolve_keeps_the_decided_code_for_a_fixed_row() {
        let error = Fixture::Fixed;
        let verdict = resolve(Decision {
            code: ExitCode::DataError,
            detail: ocx_exit::ClassifyErrorKind::kind_detail(&error),
        });
        assert_eq!(verdict.code, ExitCode::DataError);
    }
}
