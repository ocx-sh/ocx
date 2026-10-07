// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared machinery for the `ocx index` verbs: the refresh fan-out, failure logging, aggregation.
//!
//! An in-flight ceiling stated in two places drifts, so the bounded loop lives here once and no
//! verb owns its own fan-out.

use futures::StreamExt;

use crate::api::data::{sanitize_error_chain, sanitize_for_terminal};

/// Bounded concurrency for the per-package refresh fan-out. Nested inside
/// [`ocx_index::TAG_REFRESH_CONCURRENCY`], it caps a run at 512 in-flight requests, since the
/// fan-out is over the flattened set, not per registry.
pub(super) const INDEX_REFRESH_CONCURRENCY: usize = 8;

/// The one place the `index` verbs render a failure; both arguments are sanitized here, `subject`
/// even when it looks like argv. `sanitize_error_chain`, never `{error:#}`: a `thiserror` `Display`
/// ignores the alternate flag and drops the cause.
pub(super) fn log_failure(action: &str, subject: &str, error: &anyhow::Error) {
    log::error!(
        "{action} '{}': {}",
        sanitize_for_terminal(subject),
        sanitize_error_chain(error.as_ref())
    );
}

/// The `--frozen` refusal `ocx index sync` and `ocx index update` share (exit 81).
pub(crate) fn policy_blocked(operation: &str) -> crate::app::CommandError {
    crate::app::CommandError::new(
        format!("{operation} discovers new digests and cannot run in frozen mode; re-run it without --frozen"),
        ocx_exit::ExitCode::PolicyBlocked,
    )
}

/// Says so when a source lists no packages: the refresh emits no stdout, so otherwise "the mirror
/// is empty" and "the snapshot worked" look the same. A warning, as the run continues at exit 0.
pub(super) fn log_empty_enumeration(registry: &str) {
    log::warn!(
        "'{}' listed no packages; nothing was refreshed for it.",
        sanitize_for_terminal(registry)
    );
}

/// Records which provenance a registry's enumeration took: published-versus-derived is decided by
/// string equality, and a mismatch silently takes the other branch.
///
/// Lives here, not in `index_sync`, so the verbs' "no log of their own" guard stays a zero count.
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

/// Refreshes every identifier in `packages`, bounded at [`INDEX_REFRESH_CONCURRENCY`]; logs every
/// failure and returns the lowest-index one, never a partial report, so a nonzero exit carries no
/// SUCCESS-shaped stdout.
pub(super) async fn refresh_packages(
    local_index: &ocx_index::LocalIndex,
    index_sources: &[ocx_index::OcxIndex],
    oci_index: &ocx_index::Index,
    packages: &[ocx_oci::PackageRef],
) -> Option<anyhow::Error> {
    let failures = refresh_each(local_index, index_sources, oci_index, packages).await;
    for (input_index, error) in &failures {
        log_failure("Failed to update index for", &packages[*input_index].to_string(), error);
    }
    first_failure(failures)
}

/// Best-effort refresh of the tags a push just wrote, from `oci_index` alone. A failure only logs
/// at debug: the push already landed, and an ERROR line would read as if it had not.
pub(super) async fn refresh_pushed_tags(
    local_index: &ocx_index::LocalIndex,
    oci_index: &ocx_index::Index,
    packages: &[ocx_oci::PackageRef],
) {
    for (input_index, error) in refresh_each(local_index, &[], oci_index, packages).await {
        log::debug!(
            "the local index still pins the old digest of '{}': {}",
            sanitize_for_terminal(&packages[input_index].to_string()),
            sanitize_error_chain(error.as_ref())
        );
    }
}

/// The one bounded fan-out; failures carry their input index, in completion order.
async fn refresh_each(
    local_index: &ocx_index::LocalIndex,
    index_sources: &[ocx_index::OcxIndex],
    oci_index: &ocx_index::Index,
    packages: &[ocx_oci::PackageRef],
) -> Vec<(usize, anyhow::Error)> {
    // No spawn, so the borrows need no per-task clone; results arrive out of order.
    let results: Vec<(usize, anyhow::Result<()>)> = futures::stream::iter(packages.iter().enumerate())
        .map(|(input_index, identifier)| async move {
            // `jurisdiction`, not a namespace comparison, so routing cannot disagree with the chain's
            // own resolve verdict.
            let mut selected = None;
            for source in index_sources {
                if source.jurisdiction(identifier) != ocx_index::Jurisdiction::Outside {
                    selected = Some(ocx_index::Index::from_source(source.clone()));
                    break;
                }
            }
            let source = selected.unwrap_or_else(|| oci_index.clone());
            (
                input_index,
                local_index
                    .refresh_tags(identifier, &source)
                    .await
                    .map_err(anyhow::Error::from),
            )
        })
        .buffer_unordered(INDEX_REFRESH_CONCURRENCY)
        .collect()
        .await;

    results
        .into_iter()
        .filter_map(|(input_index, result)| result.err().map(|error| (input_index, error)))
        .collect()
}

/// The lowest-index failure, or `None`; a sort, since the fan-out completes out of order. Shared
/// with `index_regenerate`.
pub(super) fn first_failure(mut failures: Vec<(usize, anyhow::Error)>) -> Option<anyhow::Error> {
    failures.sort_by_key(|(input_index, _)| *input_index);
    failures.into_iter().next().map(|(_, error)| error)
}

/// Refreshes site-patch descriptors for the installed bases; best-effort, a failure only warns.
///
/// No `--frozen` check here: callers must refuse the whole command under `--frozen` before calling.
/// Skipped offline, though `sync_patches` checks too: its `OfflineMode` error would warn on every
/// offline refresh.
pub(super) async fn sync_patch_descriptors(manager: &ocx_package_manager::PackageManager) {
    if manager.patches().is_none() || manager.is_offline() {
        return;
    }
    let host = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
    match manager.sync_patches(&[host]).await {
        Ok(_report) => log::debug!("index refresh: patch descriptor sync completed"),
        Err(error) => {
            // Non-fatal, so it never reaches `main.rs`; remote-derived, so sanitized.
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
    //! `design_spec_servable_index_snapshot.md` and from a CWE-150 finding
    //! two review panels routed to this work package.
    //!
    //! The fan-out's behaviour — peak in-flight requests over a large catalog —
    //! is measured in the acceptance suite, which can run a counting
    //! stub source. What is pinned here is that there is exactly one fan-out in
    //! the `index` verb family, that it is sized by the constant, and that no
    //! verb renders a failure except through the funnel.

    use super::*;

    // ── one bounded loop ─────────────────────────────────────────────────────

    #[test]
    fn the_stated_ceiling_is_the_product_of_the_two_real_constants() {
        // Both factors, read from where they live. The earlier form multiplied
        // by a hardcoded `64`, which made the assertion `8 * 64 == 512` — true
        // at compile time whatever `TAG_REFRESH_CONCURRENCY` had become. It is
        // `pub` in ocx_lib for exactly this reason.
        assert_eq!(
            INDEX_REFRESH_CONCURRENCY * ocx_index::TAG_REFRESH_CONCURRENCY,
            512,
            "C-024 states ≤ 512 in-flight requests; both factors must be read, not restated"
        );
    }

    #[test]
    fn there_is_exactly_one_fan_out_and_no_join_set() {
        // The ceiling allows one bounded fan-out for every caller. A second one beside
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
        // actually holds is measured instead, over two registries, in the acceptance suite.
        // What this still buys is a fast, local failure for the obvious spellings.
        //
        // Scoped to the `index` family — a file named `index*`, or any file in a
        // directory named `index*` — rather than the whole crate: `update.rs`,
        // `remove.rs` and the package verbs own legitimate fan-outs, and an
        // exemption list naming all of them would be a list of everything that
        // uses concurrency, which pins nothing. The residual hole is a helper
        // under a NON-`index` name; the measurement in the acceptance suite is what covers
        // that, and covers this guard's whole failure mode besides.
        //
        // Two exemptions, both fan-outs the ceiling does not govern: `index_catalog.rs`
        // lists tags per repository (`JoinSet`), `index_list.rs` reads local
        // roots (`join_all`). Neither refreshes, so neither multiplies the
        // per-package ceiling — read-only is the whole reason, and it is not
        // "unbounded is fine here": `index_catalog.rs` carries its own bound on
        // its own permit class (`CATALOG_TAG_CONCURRENCY`), deliberately not
        // this module's, so an inner fan-out can never contend with an ancestor
        // holding the same class. The list is asserted non-vacuous below: a
        // rename that empties it fails here rather than silently exempting
        // nothing.
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

    // ── the aggregation rule ─────────────────────────────────────────────────

    /// A distinguishable error that needs no I/O to build.
    fn failure(operation: &'static str) -> anyhow::Error {
        super::policy_blocked(operation).into()
    }

    #[test]
    fn the_lowest_input_index_failure_becomes_the_process_error() {
        // The aggregation rule, and the identical rule `index_regenerate`
        // states for its own loop. The fan-out completes out of order, so the vector
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
            body.contains("sanitize_error_chain(error.as_ref())"),
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
