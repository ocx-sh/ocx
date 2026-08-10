// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared machinery for the `ocx index` verbs: the per-package refresh fan-out,
//! the failure-reporting funnel, and the batch aggregation rule.
//!
//! `update` and `sync` end in the same work — a list of identifiers, each
//! refreshed against whichever source answers for it. `update` gets its list
//! from argv, `sync` from one or more registries' own catalogs. C-024 caps that
//! work at a stated ceiling of in-flight requests, and a ceiling stated in two
//! places is a ceiling that drifts, so the loop lives here once and no verb
//! owns a fan-out. `regenerate` shares only the aggregation rule.
//!
//! Named `index_common` rather than for a verb: `<command>_<subcommand>.rs` is a
//! leaf in this crate (`subsystem-cli.md`), and `patch_common.rs` is the
//! precedent for the shared-helper slot. A file named for a verb that does not
//! exist reads as a leaf and pre-books the name of the next one.

use futures::StreamExt;
use ocx_lib::{log, oci, oci::index};

use crate::api::data::{sanitize_error_chain, sanitize_for_terminal};

/// Bounded concurrency for the per-package refresh fan-out
/// (`adr_servable_index_snapshot.md` C-024).
///
/// Nested inside [`ocx_lib::oci::index::TAG_REFRESH_CONCURRENCY`], giving a
/// stated ceiling of **≤ 512 in-flight requests** however large the work set is
/// — an argv list, one catalog, or every catalog a single `ocx index sync`
/// names, since the fan-out is over the flattened set rather than per registry.
pub(super) const INDEX_REFRESH_CONCURRENCY: usize = 8;

/// The one place in the `index` verbs that renders a failure to the operator.
///
/// A funnel rather than a rule: both halves are neutralized here, so a caller
/// cannot get it wrong, and the verbs' own guards assert they emit no log of
/// their own. The count-form guard this replaced was satisfiable by two
/// sanitizer calls in one macro paying for a second macro with none — a
/// reviewer wrote the evasion out in full.
///
/// `subject` is sanitized even when the caller believes it is argv: under
/// `ocx index sync` an identifier is built from a foreign catalog key, and the
/// distinction is exactly the kind that stops being true after a refactor.
/// `sanitize_error_chain`, not `{error:#}`: an `ocx_lib::Error`'s `thiserror`
/// Display ignores the alternate flag and would drop the cause.
pub(super) fn log_failure(action: &str, subject: &str, error: &ocx_lib::Error) {
    log::error!(
        "{action} '{}': {}",
        sanitize_for_terminal(subject),
        sanitize_error_chain(error)
    );
}

/// Says so when a source answers with an empty package set.
///
/// Not a failure: C-013 draws its line at *absent* versus *empty*, and a source
/// that served an empty catalog has answered the question. But the refresh path
/// emits no stdout payload, so without this line "the mirror is empty" and "the
/// snapshot worked" are the same observation — exit 0 and silence. Five
/// instances of silent empty success have been found on this plan; this is the
/// door that was still open.
///
/// A warning rather than an error because the run continues and the exit stays
/// 0, and `warn` is the lowest level an operator sees by default.
pub(super) fn log_empty_enumeration(registry: &str) {
    log::warn!(
        "'{}' listed no packages; nothing was refreshed for it.",
        sanitize_for_terminal(registry)
    );
}

/// Records which provenance a registry's enumeration took.
///
/// The whole ADR turns on published-versus-derived, and the choice is made by
/// matching a registry name with string equality against the configured
/// namespaces. A case difference or a stray character silently takes the other
/// branch — a registry's repository listing instead of the curated catalog, a
/// different package set, and exit 0 — so the branch says which one it took.
///
/// Debug rather than info: the common case is one correct line per registry per
/// run. Two functions rather than one taking a bool, because a bare `true` at
/// the call site names nothing.
///
/// Here rather than in `index_sync`, and that placement is the point. These were
/// the last two log macros outside this file, and a guard that let them stay had
/// to be a rule about *how* they interpolate — which a reviewer defeated with one
/// sanitized argument paying for a raw one in the same macro, the same evasion
/// [`log_failure`] already exists to retire. With them here, the verbs' guard is
/// a zero count over an empty vocabulary, which has nothing to pay with.
pub(super) fn log_published_enumeration(registry: &str) {
    log::debug!(
        "'{}' is served by a published index; enumerating its catalog",
        sanitize_for_terminal(registry)
    );
}

/// The other half of [`log_published_enumeration`].
pub(super) fn log_derived_enumeration(registry: &str) {
    log::debug!(
        "'{}' matches no configured index source; enumerating its repository listing",
        sanitize_for_terminal(registry)
    );
}

/// Refreshes every identifier in `packages`, bounded at
/// [`INDEX_REFRESH_CONCURRENCY`], and returns the **lowest-index** failure.
///
/// Every failure is logged here through [`log_failure`]; only the returned one
/// reaches `main.rs`'s boundary, so this is the sole reporter for all the
/// others. The lowest-index rule makes the process exit deterministic whatever
/// order the refreshes completed in — the same rule `ocx index regenerate`
/// states for its own loop, which is why [`first_failure`] lives here too.
///
/// Returning the error rather than a partial report is deliberate: an action
/// command with a nonzero exit emits no SUCCESS-shaped payload on stdout.
///
/// Takes the local index rather than the whole `Context`: the only capability
/// this needs is `refresh_tags`, and a narrower parameter is one fewer thing a
/// future edit here can reach for.
pub(super) async fn refresh_packages(
    local_index: &index::LocalIndex,
    index_sources: &[index::OcxIndex],
    oci_index: &index::Index,
    packages: &[oci::Identifier],
) -> Option<ocx_lib::Error> {
    // The stream combinator does not spawn, so a panic in a refresh unwinds this
    // caller directly and the borrow of `local_index` and `packages` is fine
    // without a clone per task. Results arrive out of order, hence the sort below.
    let results: Vec<(usize, ocx_lib::Result<()>)> = futures::stream::iter(packages.iter().enumerate())
        .map(|(input_index, identifier)| async move {
            // Route to the index source that will answer for this package, if
            // any; otherwise refresh against the registry. Asking `jurisdiction`
            // rather than comparing namespaces keeps this from being a second,
            // independent guess: a registry mismatch already answers `Outside`,
            // and so does a name the source's published `config.json` says its
            // grammar cannot express — which then reroutes to the registry
            // instead of dying in `refresh_derived`. It reads `config.json`, so
            // it belongs inside the bounded region, not ahead of it.
            let mut selected = None;
            for source in index_sources {
                if source.jurisdiction(identifier).await != index::Jurisdiction::Outside {
                    selected = Some(index::Index::from_source(source.clone()));
                    break;
                }
            }
            let source = selected.unwrap_or_else(|| oci_index.clone());
            (input_index, local_index.refresh_tags(identifier, &source).await)
        })
        .buffer_unordered(INDEX_REFRESH_CONCURRENCY)
        .collect()
        .await;

    let mut failures: Vec<(usize, ocx_lib::Error)> = Vec::new();
    for (input_index, result) in results {
        if let Err(error) = result {
            log_failure("Failed to update index for", &packages[input_index].to_string(), &error);
            failures.push((input_index, error));
        }
    }

    first_failure(failures)
}

/// The lowest-index failure, or `None`.
///
/// A function rather than "take the first, results are in order": the fan-out
/// completes out of order, and making the rule a property of the code rather
/// than of the loop's shape is what keeps the exit deterministic. Shared with
/// `index_regenerate`, whose C-010 states the identical rule — two copies of a
/// contract are two copies to drift.
pub(super) fn first_failure(mut failures: Vec<(usize, ocx_lib::Error)>) -> Option<ocx_lib::Error> {
    failures.sort_by_key(|(input_index, _)| *input_index);
    failures.into_iter().next().map(|(_, error)| error)
}

/// Refreshes site-patch descriptors for the installed bases, when the patch tier
/// is active.
///
/// Best-effort: a failure (offline, registry unreachable, required-companion
/// error) is logged as a warning and never fails the calling command — the tag
/// refresh is the primary job. Keeps descriptor metadata fresh after every index
/// refresh without requiring a separate `ocx patch sync`.
///
/// Only runs when a `[patches]` section is configured and the manager is online;
/// `sync_patches` checks the latter itself, but skipping the call entirely
/// avoids the `OfflineMode` error allocation. `--frozen` needs no condition:
/// both callers refuse the whole command ahead of any refresh, so a frozen
/// invocation never reaches this.
///
/// Takes the manager rather than the whole `Context` for the same reason
/// [`refresh_packages`] takes the local index.
pub(super) async fn sync_patch_descriptors(manager: &ocx_lib::package_manager::PackageManager) {
    if manager.patches().is_none() || manager.is_offline() {
        return;
    }
    let host = oci::Platform::current().unwrap_or_else(oci::Platform::any);
    match manager.sync_patches(&[host]).await {
        Ok(_report) => log::debug!("index refresh: patch descriptor sync completed"),
        Err(error) => {
            // Non-fatal, so it never reaches `main.rs`'s boundary, and
            // `sync_patches` contacts registries — the same grounds the other
            // remote-derived sites are admitted on.
            log::warn!(
                "index refresh: patch descriptor sync failed (non-fatal): {}",
                sanitize_error_chain(&error)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    //! Structural specification tests for the shared machinery, written from
    //! `design_spec_servable_index_snapshot.md` C-024 and from the CWE-150
    //! finding two review panels routed to this work package.
    //!
    //! The fan-out's behaviour — peak in-flight requests over a large catalog —
    //! is measured in the acceptance suite (S-004), which can run a counting
    //! stub source. What is pinned here is that there is exactly one fan-out in
    //! the `index` verb family, that it is sized by the constant, and that no
    //! verb renders a failure except through the funnel.

    use super::*;

    // ── C-024 — one bounded loop ─────────────────────────────────────────────

    #[test]
    fn the_stated_ceiling_is_the_product_of_the_two_real_constants() {
        // Both factors, read from where they live. The earlier form multiplied
        // by a hardcoded `64`, which made the assertion `8 * 64 == 512` — true
        // at compile time whatever `TAG_REFRESH_CONCURRENCY` had become. It is
        // `pub` in ocx_lib for exactly this reason.
        assert_eq!(
            INDEX_REFRESH_CONCURRENCY * ocx_lib::oci::index::TAG_REFRESH_CONCURRENCY,
            512,
            "C-024 states ≤ 512 in-flight requests; both factors must be read, not restated"
        );
    }

    #[test]
    fn there_is_exactly_one_fan_out_and_no_join_set() {
        // C-024 allows one bounded fan-out for every caller. A second one beside
        // it — the obvious way to add a per-registry loop to `index sync` —
        // would multiply the ceiling by the number of registries.
        let body = module_code();
        // Receiver-agnostic: the earlier needle was `.buffer_unordered(`, which
        // a reviewer evaded with the UFCS form
        // `futures::StreamExt::buffer_unordered(stream, 100_000)`.
        assert_eq!(
            body.matches("buffer_unordered(").count(),
            1,
            "one bounded loop serves every caller"
        );
        // Sized by the constant, not by a literal — `buffer_unordered(100_000)`
        // is nominally bounded and effectively is not.
        assert_eq!(
            body.matches("buffer_unordered(INDEX_REFRESH_CONCURRENCY)").count(),
            1,
            "the ceiling C-024 states is INDEX_REFRESH_CONCURRENCY's, so the call must name it"
        );
    }

    #[test]
    fn no_index_module_outside_this_one_grows_a_refresh_fan_out() {
        // The escape route the per-file guards conceded and this change then
        // took: each needle list was scoped to one named file, so moving the
        // fan-out into a NEW helper module satisfied every one of them. Scanning
        // the directory closes that — a module that does not exist yet is
        // covered — and it walks recursively over every `.rs`, not just
        // `index*.rs`, because `snapshot_fanout.rs` and `index_sync/fanout.rs`
        // were both invisible to the narrower form.
        //
        // This guard is a name denylist over an open vocabulary and therefore
        // cannot be complete: a reviewer defeated it with `tokio::join!` over a
        // one-line helper, 1024 in flight and every needle green. The claim it
        // actually holds is measured instead, over two registries, in S-004.
        // What this still buys is a fast, local failure for the obvious spellings.
        //
        // Scoped to the `index` family — a file named `index*`, or any file in a
        // directory named `index*` — rather than the whole crate: `update.rs`,
        // `remove.rs` and the package verbs own legitimate fan-outs, and an
        // exemption list naming all of them would be a list of everything that
        // uses concurrency, which pins nothing. The residual hole is a helper
        // under a NON-`index` name; the measurement in S-004 is what covers
        // that, and covers this guard's whole failure mode besides.
        //
        // Two exemptions, both fan-outs C-024 does not govern: `index_catalog.rs`
        // resolves tags for one package (`JoinSet`), `index_list.rs` reads local
        // roots (`join_all`). Neither refreshes, so neither multiplies the
        // per-package ceiling. The list is asserted non-vacuous below: a rename
        // that empties it fails here rather than silently exempting nothing.
        let exempt = ["index_common.rs", "index_catalog.rs", "index_list.rs"];
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/command");
        let mut scanned = Vec::new();
        let mut seen_exempt = Vec::new();
        for path in rust_sources_under(&directory) {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let in_index_directory = path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|parent| parent.to_str())
                .is_some_and(|parent| parent.starts_with("index"));
            if !name.starts_with("index") && !in_index_directory {
                continue;
            }
            if exempt.contains(&name.as_str()) {
                seen_exempt.push(name);
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("a readable module");
            // Same truncation defence as `the_scan_window_is_not_truncatable`,
            // applied to every scanned file: this guard slices at the first
            // `#[cfg(test)]` too, so a marker placed early in a NEW module would
            // hide a fan-out from every needle below.
            assert_first_cfg_test_is_the_test_module(&name, &source);
            let production = strip_comments(source.split("#[cfg(test)]").next().unwrap_or_default());
            for forbidden in [
                "buffer_unordered",
                "JoinSet",
                "task::spawn",
                "spawn(",
                "FuturesUnordered",
                "FuturesOrdered",
                "for_each_concurrent",
                "buffered(",
                "join_all",
                "future::join(",
                // The macro forms, and the `try_` half of every combinator:
                // `join_all` already catches `try_join_all`, but `try_join(`
                // and `tokio::join!` were both unnamed.
                "try_join",
                "join!(",
                // Path-qualified: a bare `select_all` matches `deselect_all`,
                // which several package verbs legitimately call.
                "::select_all(",
            ] {
                assert!(
                    !production.contains(forbidden),
                    "`{forbidden}` in {name}: the refresh fan-out belongs to index_common.rs, once — \
                     C-024's ceiling is stated for the whole `index` verb family"
                );
            }
            scanned.push(name);
        }
        assert_eq!(
            seen_exempt.len(),
            exempt.len(),
            "an exempted module was renamed or removed; the exemption list must name real files, \
             or it silently stops exempting anything and starts hiding a real fan-out"
        );
        assert!(
            scanned.len() >= 3,
            "expected the index verb family to be scanned (found {scanned:?}); a rename that \
             empties this scan would make the guard vacuous"
        );
    }

    // ── C-012 / C-010 — the aggregation rule ────────────────────────────────

    /// A distinguishable `ocx_lib::Error` that needs no I/O to build.
    fn failure(operation: &'static str) -> ocx_lib::Error {
        ocx_lib::Error::PolicyBlocked {
            operation,
            policy: "aggregation-test",
        }
    }

    #[test]
    fn the_lowest_input_index_failure_becomes_the_process_error() {
        // C-012's Aggregation row, and C-010's identical rule for
        // `index_regenerate`. The fan-out completes out of order, so the vector
        // arrives out of order — this is what makes the exit code the same
        // across repeated runs of the same broken input.
        let chosen = first_failure(vec![
            (3, failure("fourth")),
            (1, failure("second")),
            (0, failure("first")),
            (2, failure("third")),
        ])
        .expect("four failures make one process error");
        assert!(
            chosen.to_string().contains("first"),
            "the lowest input index wins, not the first to complete: got `{chosen}`"
        );
    }

    #[test]
    fn no_failure_is_no_error() {
        assert!(
            first_failure(Vec::new()).is_none(),
            "an empty failure set must not manufacture an error"
        );
    }

    // ── CWE-150 — one funnel, and nothing else prints ────────────────────────

    #[test]
    fn the_funnel_neutralizes_both_halves() {
        // Not a count: the count form was satisfiable by two sanitizer calls in
        // one macro paying for a second macro with none. These are the two
        // arguments of the one `log::error!` in this module.
        let body = module_code();
        assert!(
            body.contains("sanitize_for_terminal(subject)"),
            "the subject half carries a foreign catalog key under `index sync`"
        );
        assert!(
            body.contains("sanitize_error_chain(error)"),
            "the chain half quotes names read off a foreign tree"
        );
        assert_eq!(
            body.matches("log::error!").count(),
            1,
            "one error site in this module, and it is the funnel"
        );
        assert_eq!(
            body.matches("log::warn!").count(),
            2,
            "two warn sites in this module: the piggyback's failure and the empty-enumeration \
             notice, each rendering its foreign half through a sanitizer"
        );
        assert!(
            body.contains("sanitize_error_chain(&error)"),
            "the piggyback's warn carries an error and must render its chain sanitized"
        );
        assert!(
            body.contains("sanitize_for_terminal(registry)"),
            "the empty-enumeration notice quotes a registry name and must neutralize it"
        );
        for raw in [
            "{error:#}",
            "{error}",
            "{:#}",
            "{:?}",
            "error.to_string()",
            "log::info!",
            "eprintln!",
        ] {
            assert!(
                !body.contains(raw),
                "`{raw}` renders an error or a foreign name without the funnel's sanitizers"
            );
        }
    }

    #[test]
    fn the_scan_window_is_not_truncatable() {
        // Every structural guard in this crate slices at the FIRST `#[cfg(test)]`,
        // so a marker attached to anything earlier blinds every negative
        // assertion in that module at once while the positive ones keep
        // matching. One line per module that builds such a window closes it.
        //
        // The list is every module that slices this way, not just the `index`
        // family: `main.rs`'s window feeds the assertion that
        // `sanitize_for_terminal` appears exactly once at the single boundary
        // every failing command exits through, which is the highest-value
        // structural claim in the crate and was unguarded.
        for (name, source) in [
            ("index_common.rs", include_str!("index_common.rs")),
            ("index_sync.rs", include_str!("index_sync.rs")),
            ("index_update.rs", include_str!("index_update.rs")),
            ("index_regenerate.rs", include_str!("index_regenerate.rs")),
            ("index_catalog.rs", include_str!("index_catalog.rs")),
            ("main.rs", include_str!("../main.rs")),
            ("api/data/index.rs", include_str!("../api/data/index.rs")),
        ] {
            assert!(
                source.contains("#[cfg(test)]"),
                "{name} is listed here because its guards slice a scan window, so it must have a \
                 test module; if its guards were deleted, delete its row"
            );
            assert_first_cfg_test_is_the_test_module(name, source);
        }
    }

    /// Asserts `source`'s first `#[cfg(test)]` is the one on `mod tests`.
    ///
    /// Counting occurrences would not do: each guarded module names the marker
    /// again inside its own test half, in the very `split` call being defended.
    /// What must hold is that the FIRST marker is the test module's — anything
    /// earlier is an item attached in the production half, which moves the
    /// window's end up to it.
    fn assert_first_cfg_test_is_the_test_module(name: &str, source: &str) {
        // A module with no test module has no window to truncate. The named list
        // above asserts its own members have one by construction — they are the
        // modules whose guards do the slicing — while the directory scan calls
        // this over every file it walks, most of which have no tests at all.
        let Some((_, rest)) = source.split_once("#[cfg(test)]") else {
            return;
        };
        assert!(
            rest.trim_start().starts_with("mod tests {"),
            "{name}: the first `#[cfg(test)]` must be the test module's. The scan window is \
             everything before it, so a marker attached to anything earlier truncates the \
             window and every negative assertion over that module passes vacuously"
        );
    }

    /// Every `.rs` file under `directory`, recursively.
    ///
    /// Recursive because `read_dir` alone left `command/index_sync/fanout.rs`
    /// invisible to the fan-out scan — the same escape route one directory down.
    fn rust_sources_under(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(directory).expect("a readable directory") {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                found.extend(rust_sources_under(&path));
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
        found
    }

    /// This module's non-test source, comment lines dropped — the structural
    /// assertions are about code that must not exist, and a comment must stay
    /// free to name the forms the code may not use.
    fn module_code() -> String {
        strip_comments(
            include_str!("index_common.rs")
                .split("#[cfg(test)]")
                .next()
                .expect("the module has a non-test half"),
        )
    }

    /// Drops whole-line comments. Shared with the sibling guard modules through
    /// copy rather than import, because each one `include_str!`s its own source.
    fn strip_comments(source: &str) -> String {
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
