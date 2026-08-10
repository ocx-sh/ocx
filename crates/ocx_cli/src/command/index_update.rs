// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::oci::index;

use crate::command::index_common;
use crate::options;

/// The one command that moves a pin for the packages you name.
///
/// The local index copy binds a tag to a digest and a package to a physical
/// registry; resolving, installing and running all read it as it stands. This
/// command changes it, and only for the packages named on the command line —
/// `ocx index sync` is the same work over a registry's whole catalog, sharing
/// this command's refresh loop ([`index_common`]). The user-facing help lives
/// on the `Index::Update` variant, which is what clap renders.
#[derive(Parser)]
pub struct IndexUpdate {
    #[clap(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl IndexUpdate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // `ocx index update` refreshes tags via `LocalIndex::refresh_tags`
        // (`adr_index_indirection.md` Decision H): writes the tag → digest root document plus each tag's
        // dispatch object (`o/<algo>/<hex>.json`) into the local index
        // collection — never the leaf platform manifest, which is fetched
        // into the machine-global blob store on demand (A3) — so a version
        // choice resolves fully offline afterwards.
        // Offline is checked first — the accessor IS the offline gate and
        // constructs nothing — so `--offline --frozen` keeps reporting the
        // stricter posture.
        let remote_index = context.oci_index()?;

        // `--frozen` refuses the package tier's discovery verb — the one verb
        // the flag scopes to. An index update exists to learn a NEW tag →
        // digest binding and write it into the local index; a freeze exists to
        // stop exactly that, so reporting success while moving pins would make
        // the flag meaningless where it applies most directly. Placed before
        // any index-source or refresh work so nothing is fetched and no pin can
        // move. Read straight off the invocation's policy view: the package
        // manager carries no frozen posture, because no other tier consults one.
        //
        // This is the ONLY frozen gate in this command; a second gate beside it
        // is what `exactly_one_frozen_gate` exists to refuse.
        if context.config_view().frozen {
            return Err(ocx_lib::Error::PolicyBlocked {
                operation: "`ocx index update`",
                policy: "frozen",
            }
            .into());
        }

        let oci_index = index::Index::from_remote(remote_index.clone());
        // Per-namespace static-file index sources, when online. A package in an
        // index-bearing namespace refreshes through the two-hop index path
        // rather than the registry (`adr_index_indirection.md` F5a — kind per
        // NAMESPACE); every other package refreshes against the registry.
        let index_sources = context.index_sources();

        let packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        // Any failure → the input-order-first error, so `classify_error`
        // (main.rs) derives a deterministic nonzero exit. No stdout report: this
        // is an action command with no payload; the aggregated error on stderr
        // is the batch signal.
        if let Some(error) =
            index_common::refresh_packages(context.local_index(), index_sources, &oci_index, &packages).await
        {
            return Err(error.into());
        }

        // Piggyback: keep the patch tier's descriptors in step with the index
        // this command just refreshed. Best-effort and non-fatal; see
        // [`index_common::sync_patch_descriptors`].
        index_common::sync_patch_descriptors(context.manager()).await;

        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    //! Specification tests for `ocx index update`'s CLI contract, written from
    //! `design_spec_servable_index_snapshot.md` C-012 and C-024.
    //!
    //! The refresh fan-out itself is `index_common.rs`'s, and its guards live
    //! there; what is pinned here is this command's grammar and that it grew no
    //! fan-out or policy gate of its own.

    use super::*;
    use clap::CommandFactory;

    /// Parses `argv` (with a leading command name) against this command's own
    /// clap definition. `cli::clap::parse` turns every non-help clap error into
    /// `ExitCode::UsageError` (64), so an `Err` here is exit 64.
    fn parse(argv: &[&str]) -> Result<clap::ArgMatches, clap::Error> {
        IndexUpdate::command().try_get_matches_from(argv)
    }

    // ── C-012 — grammar ──────────────────────────────────────────────────────

    #[test]
    fn at_least_one_package_is_required() {
        // The catalog shape left this verb when `ocx index sync` took it, so the
        // positional carries the requirement again rather than an `ArgGroup`.
        let error = parse(&["update"]).expect_err("a bare `ocx index update` names no work set");
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn packages_are_variadic() {
        let matches = parse(&["update", "cmake", "ninja"]).expect("the argv shape is unchanged");
        assert_eq!(
            matches
                .get_many::<options::Identifier>("packages")
                .expect("packages bound")
                .count(),
            2
        );
    }

    #[test]
    fn the_catalog_shape_is_gone_from_this_verb() {
        // `--from-catalog` was promoted to `ocx index sync`. Leaving a hidden
        // alias here would restore the two-commands-in-one shape the promotion
        // removed, and would need its exclusion rules back with it.
        parse(&["update", "--from-catalog", "ocx.sh"]).expect_err("--from-catalog belongs to `index sync` now");
        parse(&["update", "--dry-run", "cmake"]).expect_err("--dry-run went with it");
    }

    // ── C-012 — exactly one `--frozen` gate ─────────────────────────────────

    #[test]
    fn exactly_one_frozen_gate() {
        let body = module_code();
        assert_eq!(
            body.matches("Error::PolicyBlocked").count(),
            1,
            "one --frozen gate, ahead of the refresh"
        );
        assert_eq!(
            body.matches("context.config_view()").count(),
            1,
            "the policy view is read once, by that gate"
        );
    }

    // ── C-024 — this command owns no fan-out of its own ─────────────────────

    #[test]
    fn the_refresh_loop_is_the_shared_one() {
        let body = module_code();
        assert!(
            body.contains("index_common::refresh_packages("),
            "the refresh must go through the shared bounded loop"
        );
        for forbidden in [
            "buffer_unordered",
            "JoinSet",
            "task::spawn",
            "spawn(",
            "FuturesUnordered",
            // Not a fan-out combinator by name, and the way through the rest of
            // this list: two joined `refresh_packages` calls run 1024 in flight.
            "FuturesOrdered",
            "future::join(",
            // The macro forms, and the `try_` half of every combinator — see
            // the same addition in `index_sync.rs`.
            "try_join",
            "join!(",
            "select_all",
            "for_each_concurrent",
            "buffered(",
            "join_all",
        ] {
            assert!(
                !body.contains(forbidden),
                "`{forbidden}` in index_update.rs: the fan-out belongs to index_common.rs, once"
            );
        }
        assert_eq!(
            body.matches("index_common::refresh_packages(").count(),
            1,
            "one call to the shared loop: two of them run 2 x 512 in flight with no new needle"
        );
    }

    // ── CWE-150 — no unsanitized error prose ────────────────────────────────

    #[test]
    fn this_module_prints_no_error_of_its_own() {
        // Every failure this command reports is reported by
        // `index_common::log_failure`, sanitized there. A log line added here
        // would need its own sanitizer, which is why the funnel exists.
        let body = module_code();
        // `log::` rather than a list of levels. A reviewer's injected
        // `log::debug!` of a foreign catalog key walked through the
        // error/warn/info form of this list in the sibling module, and naming
        // two more levels would only move the hole: `debug` and `trace` reach
        // the same terminal, and the operator who raised the verbosity to
        // diagnose a suspicious catalog is exactly the one reading them. This
        // module emits nothing of its own at any level, so the rule is total.
        for raw in [
            "log::",
            "eprintln!",
            "println!",
            "{error:#}",
            "{error}",
            "{:#}",
            "{:?}",
            "error.to_string()",
        ] {
            assert!(
                !body.contains(raw),
                "`{raw}` in index_update.rs: operator-facing failure prose goes through \
                 `index_common::log_failure`, which is the sanitized one"
            );
        }
    }

    /// This module's non-test source. The structural assertions above are about
    /// code that must not exist (a second gate, a local fan-out).
    fn module_source() -> &'static str {
        include_str!("index_update.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half")
    }

    /// [`module_source`] with comment lines dropped, for assertions about forms
    /// a comment is entitled to name while the code is not.
    fn module_code() -> String {
        module_source()
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
