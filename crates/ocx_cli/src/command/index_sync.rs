// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_index;

use crate::api::data::index::{CatalogPreview, CatalogPreviewEntry};
use crate::command::index_common;

/// Refreshes every package one or more registries' own catalogs name, through `update`'s refresh
/// loop, so one bounded ceiling covers a run over any number of registries.
#[derive(Parser)]
pub struct IndexSync {
    /// Registries whose catalog names the packages to refresh.
    #[clap(required = true, num_args = 1.., value_name = "REGISTRY")]
    registries: Vec<String>,

    /// Print the packages this would refresh, and refresh none of them
    ///
    /// Enumeration still runs, so this contacts the source, and `--offline` and
    /// `--frozen` still refuse it.
    #[clap(long)]
    dry_run: bool,
}

impl IndexSync {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Offline first (the accessor is the offline gate), so `--offline --frozen` reports the stricter
        // posture; `--dry-run` still contacts the source.
        let remote_index = context.oci_index()?;

        // Before any fetch: bulk tag → digest binding is what a freeze stops. The only frozen gate here;
        // `exactly_one_frozen_gate` fails a second one.
        if context.config_view().frozen {
            return Err(index_common::policy_blocked("`ocx index sync`").into());
        }

        let oci_index = ocx_index::Index::from_remote(remote_index.clone());
        // Index-bearing namespaces refresh through their index source, the rest against the registry
        // (`adr_index_indirection.md`).
        let index_sources = context.index_sources();

        // Deduplicated before any fetch, or a repeat refreshes twice and blocks on the first's lock; the
        // registry list, not the packages, so the dry-run preview and the wet path agree.
        let mut seen = std::collections::HashSet::new();
        let registries: Vec<&String> = self.registries.iter().filter(|r| seen.insert(*r)).collect();

        // All enumerated before any is refused, so one unreachable source does not cost the others.
        let mut enumerated = Vec::with_capacity(registries.len());
        let mut failures: Vec<(usize, anyhow::Error)> = Vec::new();
        for (input_index, registry) in registries.iter().enumerate() {
            match enumerate_catalog(index_sources, &oci_index, registry).await {
                Ok(packages) => {
                    // Logged: a pull token without catalog scope often answers an empty listing, not 401.
                    if packages.is_empty() {
                        index_common::log_empty_enumeration(registry);
                    }
                    enumerated.push(CatalogPreviewEntry::new((*registry).clone(), packages));
                }
                Err(error) => {
                    // Every failure but the returned one is reported only here.
                    index_common::log_failure("Failed to enumerate the catalog for", registry, &error);
                    failures.push((input_index, error));
                }
            }
        }

        // Before the refresh and the patch piggyback, which write outside the index home.
        if self.dry_run {
            // Fails rather than print a partial set that reads as success.
            if let Some(error) = index_common::first_failure(failures) {
                return Err(error);
            }
            context.api().report(&CatalogPreview::new(enumerated))?;
            return Ok(ExitCode::SUCCESS);
        }

        // Flattened across registries so one bounded fan-out caps the whole run.
        let packages: Vec<ocx_oci::PackageRef> = enumerated
            .iter()
            .flat_map(|entry| {
                entry
                    .packages
                    .iter()
                    .map(|repository| ocx_oci::PackageRef::new_registry(repository, &entry.registry))
            })
            .collect();

        let refresh_failure =
            index_common::refresh_packages(context.local_index(), index_sources, &oci_index, &packages).await;

        // Enumeration failure outranks refresh failure: that registry contributed no work at all.
        if let Some(error) = index_common::first_failure(failures).or(refresh_failure) {
            return Err(error);
        }

        index_common::sync_patch_descriptors(context.manager()).await;

        Ok(ExitCode::SUCCESS)
    }
}

/// Enumerates one registry's package set, live from the source; never the local copy, which would
/// make the command a no-op against the drift it exists to close.
///
/// # Errors
///
/// A refusing listing endpoint surfaces its error: no fall-through to the registry, and never an
/// empty-set success.
async fn enumerate_catalog(
    index_sources: &[ocx_index::OcxIndex],
    oci_index: &ocx_index::Index,
    registry: &str,
) -> anyhow::Result<Vec<String>> {
    let mut packages = match index_sources.iter().find(|source| source.serves_registry(registry)) {
        // `_strict`: an absent catalog is not an empty one, and the tolerant read would exit 0 having
        // refreshed nothing.
        Some(source) => {
            index_common::log_published_enumeration(registry);
            source
                .fetch_catalog_strict()
                .await?
                .into_keys()
                .collect::<Vec<String>>()
        }
        // Derived: the registry's repository listing.
        None => {
            index_common::log_derived_enumeration(registry);
            oci_index.list_repositories(registry).await?
        }
    };
    // Foreign keys, and `new_registry` validates nothing: unchecked, `../../..` resolves outside the
    // index base path in the request URL.
    for key in &packages {
        // `validate_repository`, not parse-then-discard: parsing splits the tag off first, so
        // `ns/pkg:<anything>` passes while `new_registry` adopts the whole key.
        ocx_oci::PackageRef::validate_repository(key).map_err(|error| {
            ocx_index::error::Error::MalformedCatalogKey {
                index_source: registry.to_string(),
                key: key.clone(),
                reason: error.to_string(),
            }
        })?;
    }
    // Neither branch is ordered; sorted here so the preview and the refresh both inherit the order a
    // deterministic exit needs.
    packages.sort();
    Ok(packages)
}

#[cfg(test)]
mod tests {
    //! Specification tests for `ocx index sync`'s CLI contract, written from
    //! `design_spec_servable_index_snapshot.md`.
    //!
    //! The refresh itself needs a stub index source and a temp index home, which
    //! is acceptance-tier work covered by the acceptance suite; what is pinned
    //! here is the grammar, the identifier shape the loop is fed, and the
    //! structural claims the contracts make about code that must *not* exist.

    use super::*;
    use clap::{CommandFactory, FromArgMatches};

    /// Parses `argv` (with a leading command name) against this command's own
    /// clap definition. `clap_parse::parse` turns every non-help clap error into
    /// `ExitCode::UsageError` (64), so an `Err` here is exit 64.
    fn parse(argv: &[&str]) -> Result<clap::ArgMatches, clap::Error> {
        IndexSync::command().try_get_matches_from(argv)
    }

    // ── grammar ──────────────────────────────────────────────────────────────

    #[test]
    fn a_registry_is_required() {
        let error = parse(&["sync"]).expect_err("sync with no registry names no work set");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument,
            "the whole point of the verb is the registry it is given"
        );
    }

    #[test]
    fn registries_are_variadic() {
        // The ergonomic half of promoting the flag to a verb: several registries
        // are positionals, not a repeated flag.
        let matches = parse(&["sync", "ocx.sh", "corp.example"]).expect("registries are variadic");
        let parsed = IndexSync::from_arg_matches(&matches).expect("binds");
        assert_eq!(parsed.registries, ["ocx.sh", "corp.example"]);
        assert!(!parsed.dry_run);
    }

    #[test]
    fn dry_run_needs_no_companion_flag() {
        // Under `index update --from-catalog` this shape needed `requires` and
        // `conflicts_with` to keep it away from positional packages. A verb whose
        // only positionals ARE registries needs neither.
        let matches = parse(&["sync", "ocx.sh", "--dry-run"]).expect("the dry-run shape");
        let parsed = IndexSync::from_arg_matches(&matches).expect("binds");
        assert!(parsed.dry_run);
        assert_eq!(parsed.registries, ["ocx.sh"]);
    }

    // ── scope per enumerated package ────────────────────────────────────────

    #[test]
    fn an_enumerated_repository_becomes_a_bare_identifier() {
        // `refresh_published` reads the scope off the identifier's shape:
        // `Some(tag) => RootScope::Tag`, `None => RootScope::Package`. The
        // contract wants `Package`, so what this command builds per catalog key
        // must carry no tag — the whole contract turns on this one `None`.
        let identifier = ocx_oci::PackageRef::new_registry("kitware/cmake", "ocx.sh");
        assert!(
            identifier.tag().is_none(),
            "a tagged identifier would narrow the refresh to one tag (RootScope::Tag)"
        );
        assert_eq!(identifier.repository(), "kitware/cmake");
        assert_eq!(identifier.registry(), "ocx.sh");

        // And that THIS command builds them that way. The assertions above are
        // properties of the constructor: the flatten could switch to
        // `clone_with_tag` and every one of them would still pass while the
        // package-scoped merge silently became a per-tag one.
        let body = module_code();
        assert!(
            body.contains("ocx_oci::PackageRef::new_registry(repository, &entry.registry)"),
            "the flatten must build the bare form; the behavioural half is covered by the \
             acceptance suite"
        );
        for narrowing in ["clone_with_tag", "tag_or_latest", "clone_with_digest"] {
            assert!(
                !body.contains(narrowing),
                "`{narrowing}` would select RootScope::Tag and turn the snapshot into a per-tag refresh"
            );
        }
    }

    // ── exactly one `--frozen` gate ──────────────────────────────────────────

    #[test]
    fn exactly_one_frozen_gate() {
        // A second gate added "for the dry-run path" would be dead code that
        // silently drifts from the first.
        let body = module_code();
        assert_eq!(
            body.matches("policy_blocked(").count(),
            1,
            "one --frozen gate, ahead of enumeration and refresh alike"
        );
        assert_eq!(
            body.matches("context.config_view()").count(),
            1,
            "the policy view is read once, by that gate"
        );
    }

    // ── this command owns no fan-out of its own ──────────────────────────────

    #[test]
    fn the_refresh_loop_is_the_shared_one() {
        // The ceiling is stated in `index_common.rs` and covers a run over any
        // number of registries only while the fan-out is over the flattened set.
        // A per-registry loop here would multiply it by the argument count.
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
            // `futures::future::join` is not a fan-out combinator by name and
            // was the reviewer's way through the earlier list: two joined
            // `refresh_packages` calls run 1024 in flight while every needle
            // above still misses.
            "FuturesOrdered",
            "future::join(",
            // The macro forms of the same thing, and the `try_` half of every
            // combinator: `join_all` already catches `try_join_all`, but
            // `try_join(` and `tokio::join!` were both unnamed.
            "try_join",
            "join!(",
            "select_all",
            "for_each_concurrent",
            "buffered(",
            "join_all",
        ] {
            assert!(
                !body.contains(forbidden),
                "`{forbidden}` in index_sync.rs: the fan-out belongs to index_common.rs, once"
            );
        }
        assert_eq!(
            body.matches("index_common::refresh_packages(").count(),
            1,
            "one call to the shared loop: two of them run 2 x 512 in flight with no new needle"
        );
    }

    // ── the dry-run return precedes both the refresh and the piggyback ───────

    #[test]
    fn the_piggyback_runs_last_of_all_and_only_on_success() {
        // Two contracts, one ordering: the piggyback does network I/O and
        // writes OUTSIDE the index home, so the acceptance suite's "index home
        // untouched" assertion cannot catch it under `--dry-run`. The
        // patch-descriptors contract: it runs only when the whole command
        // succeeded, so a nine-of-ten-registry run leaves descriptors untouched.
        //
        // The earlier form compared only `dry_run_return < piggyback`, which a
        // reviewer satisfied by moving the piggyback ABOVE the aggregation gate
        // — still below the dry-run block, still green, and now firing on every
        // failed run. Its own behavioural blind spot is why: the piggyback's
        // failure is a WARN, and the one acceptance test with a failing wet run
        // reads stderr through a filter on " ERROR ".
        let body = module_code();
        let dry_run_return = body
            .find("if self.dry_run {")
            .expect("the --dry-run early return is the contract");
        let refresh = body
            .find("index_common::refresh_packages(")
            .expect("the refresh call is still here");
        let aggregation = body
            .find("index_common::first_failure(failures).or(refresh_failure)")
            .expect("the aggregation gate is still here");
        let piggyback = body
            .find("sync_patch_descriptors(")
            .expect("the piggyback is still here");
        assert!(
            dry_run_return < refresh,
            "--dry-run must return before the refresh loop, not merely before the piggyback"
        );
        assert!(
            refresh < aggregation && aggregation < piggyback,
            "the piggyback must sit after the aggregation gate: above it, a run that is about to \
             exit non-zero still syncs patch descriptors"
        );
        // And the gate must actually return, or ordering buys nothing.
        let gate = &body[aggregation..piggyback];
        assert!(
            // No `.into()`: the failures were once `ocx_lib::Error` against an
            // `anyhow::Result` signature. They are `anyhow::Error` now, so the
            // conversion is gone and clippy refuses it — keeping it in the
            // needle would red on the only spelling that compiles.
            gate.contains("return Err(error);"),
            "the aggregation gate must return, not merely compute"
        );
    }

    // ── authoritative stop on an enumeration failure ─────────────────────────

    #[test]
    fn enumeration_failures_propagate_rather_than_yielding_an_empty_set() {
        // "A registry whose listing endpoint refuses surfaces that source's
        // error under the authoritative-stop rule — no fall-through, no
        // empty-set success." An auth-refusing registry silently reporting zero
        // packages and exit 0 is this plan's recurring failure shape, and it is
        // one `.unwrap_or_default()` away.
        //
        // The behavioural test lives in the acceptance suite; this is
        // the structural guard beside it. Scoped to `enumerate_catalog`, whose
        // whole body is the enumeration.
        let enumeration = enumerate_catalog_body();
        // Whitespace removed for the pairing needles: `cargo fmt` breaks a call
        // chain across lines as soon as a longer call pushes it over the width
        // limit, and `fetch_catalog_strict()` is already split today. A
        // single-line needle would silently stop matching — passing a negative
        // assertion and failing a positive one for a reason that has nothing to
        // do with the contract.
        let squeezed: String = enumeration.chars().filter(|c| !c.is_whitespace()).collect();
        // Pairings, not a budget: the `.await?` COUNT this replaced was
        // satisfiable by a `match` on the strict fetch whose `Err(_)` arm falls
        // through to the registry listing — one `?` becomes a match and another
        // `?` arrives, the count stays at 2, and the authoritative stop is gone.
        assert_eq!(
            squeezed.matches("fetch_catalog_strict().await?").count(),
            1,
            "the published branch must use the STRICT fetch AND propagate: the tolerant fetch maps \
             an absent catalog document to an empty catalog, which is the empty-set success the \
             authoritative-stop rule forbids, and it happens one call BELOW this function where \
             the scan cannot see it"
        );
        assert!(
            !enumeration.contains("fetch_catalog()"),
            "the tolerant fetch has no place in an enumeration that then acts on the result"
        );
        assert_eq!(
            squeezed.matches("list_repositories(registry).await?").count(),
            1,
            "the derived branch must propagate with `?`"
        );
        assert_eq!(
            squeezed.matches(".await?").count(),
            2,
            "both enumeration awaits propagate; a third await here needs its own `?` review"
        );
        for swallow in [
            "unwrap_or_default",
            "unwrap_or_else",
            "unwrap_or(",
            ".ok()",
            // A fall-through arm is how a `?` becomes a swallow while every
            // needle above still matches.
            "Err(_)",
            "Err(_e)",
            "if let Ok(",
        ] {
            assert!(
                !enumeration.contains(swallow),
                "`{swallow}` in enumerate_catalog would turn a refused listing into an empty-set success"
            );
        }
    }

    #[test]
    fn the_enumerated_set_is_sorted_before_either_path_sees_it() {
        // The aggregation rule: "the lowest input index wins" is only
        // deterministic if the input order is, and neither branch supplies one
        // — the published branch drains a map's keys. The sort lives here rather
        // than in `CatalogPreviewEntry::new` so the WET path inherits it; the
        // report sorts independently, which is exactly why deleting this line
        // left every test green when a reviewer tried it.
        let enumeration = enumerate_catalog_body();
        assert!(
            enumeration.contains("packages.sort();"),
            "enumerate_catalog must sort: the report's own sort covers the preview only, so \
             without this the refresh order — and therefore the exit code — varies run to run"
        );
        let validation = enumeration
            .find("validate_repository")
            .expect("the key validation is still here");
        let sort = enumeration.find("packages.sort();").expect("checked above");
        assert!(
            validation < sort,
            "validate before sorting: a rejected key must cost no work that follows it"
        );
    }

    /// `enumerate_catalog`'s body, comment lines dropped.
    ///
    /// Bounded at both ends. The open-ended slice this replaced ran to the end of
    /// the file, so anything appended below the function silently entered the
    /// scan and could satisfy a positive needle from outside the function the
    /// contract is about.
    fn enumerate_catalog_body() -> String {
        let body = module_code();
        let start = body
            .find("async fn enumerate_catalog")
            .expect("the enumeration function is still here");
        let end = body[start..]
            .find("\n}\n")
            .map(|offset| start + offset)
            .expect("the function has a closing brace at column 0");
        body[start..end].to_string()
    }

    #[test]
    fn one_registrys_failure_does_not_skip_the_others() {
        // The batch rule: enumerate every registry, then decide. An early `?` on
        // the enumeration — the shape this replaced — made a single unreachable
        // source cost every later registry its snapshot, which is the whole
        // reason the verb takes several.
        let body = module_code();
        let loop_body = &body[body
            .find("for (input_index, registry) in registries.iter().enumerate()")
            .expect("the enumeration loop is still here")
            ..body
                .find("if self.dry_run {")
                .expect("the dry-run return is still here")];
        assert!(
            loop_body.contains("failures.push((input_index, error))"),
            "a failed registry must be recorded and the loop continue"
        );
        assert!(
            !loop_body.contains("return Err"),
            "returning from inside the loop abandons every registry after the first failure"
        );
        assert!(
            !loop_body.contains("break"),
            "breaking out of the loop abandons every registry after the first failure"
        );
    }

    // ── CWE-150 — the operator-facing log line ──────────────────────────────

    #[test]
    fn this_module_prints_no_failure_except_through_the_funnel() {
        // The count form this replaced — one sanitizer call per log macro — was
        // satisfiable by putting two sanitizer calls in one macro and paying for
        // a second macro with none. There is nothing to count now: this module
        // emits no log of its own, and `index_common::log_failure` sanitizes
        // both halves at the single site its own guard pins.
        //
        // Code only: the comments here quote the very forms the denylist below
        // refuses, which is the right thing for a comment to do and would
        // otherwise fail its own test.
        let body = module_code();
        assert!(
            body.contains("index_common::log_failure("),
            "a failed registry must still be reported — it is the only report for every failure \
             that does not become the process error"
        );
        // A ZERO count, not a rule about how a log line interpolates. Two
        // rules were tried here and a reviewer walked through both: naming
        // `error`/`warn`/`info` missed `debug`, and requiring
        // `sanitize_for_terminal(` per call was paid off by one sanitized
        // argument covering a raw one in the same macro —
        // `log::debug!("'{}' enumerated '{key}'", sanitize_for_terminal(registry))`
        // — which is the same evasion `index_common::log_failure` exists to
        // retire. A vocabulary that must stay empty has nothing to pay with, so
        // the two enumeration lines moved to `index_common` instead.
        //
        // The macro forms are listed alongside `log::` because an alias
        // (`use log as logger;`) defeats a prefix check and not a
        // suffix one.
        for raw in [
            "log::",
            "::error!",
            "::warn!",
            "::info!",
            "::debug!",
            "::trace!",
            "eprintln!",
            "println!",
            "{error:#}",
            "{error}",
            "{:#}",
            "{:?}",
        ] {
            assert!(
                !body.contains(raw),
                "`{raw}` in index_sync.rs: this module emits no log of its own at any level. \
                 Operator-facing prose goes through an `index_common` helper, which sanitizes \
                 at the single site its own guard pins."
            );
        }
        // `error.to_string()` is deliberately NOT on that list here: the one
        // occurrence builds `MalformedCatalogKey`'s `reason` field, which is an
        // error being constructed rather than rendered. It reaches the operator
        // through the funnel or `main.rs`'s boundary like any other chain, and
        // both sanitize.
    }

    // ── CWE-22 / CWE-150 — the catalog key is validated as it is USED ───────

    #[test]
    fn a_catalog_key_is_validated_verbatim_and_not_as_a_decomposition() {
        // The defect: `parse_with_default_registry(key, registry)` with the
        // result DISCARDED validated a decomposition — the tag and digest are
        // split off before the character-class, uppercase and length guards run
        // — while `PackageRef::new_registry` then adopted the raw key. A key of
        // `ns/pkg:\u{202e}gnp.exe` passed and reached a log line and a request
        // URL intact. `PackageRef::validate_repository` applies every guard to
        // the string as given; its own behavioural tests live beside it in
        // ocx_lib.
        let body = module_code();
        assert!(
            body.contains("ocx_oci::PackageRef::validate_repository(key)"),
            "the key must be validated as the repository it becomes"
        );
        assert!(
            !body.contains("parse_with_default_registry"),
            "parsing and discarding validates a decomposition of the key, not the key"
        );
        // Fail-closed: `?`, not a filter. Dropping the poisoned key and
        // refreshing the rest would report success over a catalog someone
        // tampered with.
        let enumeration = enumerate_catalog_body();
        assert!(
            enumeration.contains("MalformedCatalogKey"),
            "a refused key is a named error, not a silent skip"
        );
        for skip in ["retain(", "filter(", "continue"] {
            assert!(
                !enumeration.contains(skip),
                "`{skip}` in enumerate_catalog would drop a poisoned key and report success \
                 over the rest of a tampered catalog"
            );
        }
    }

    // ── aggregation precedence ────────────────────────────────────────────────

    #[test]
    fn an_enumeration_failure_outranks_a_refresh_failure() {
        // The aggregation rule shipped with nothing pinning it. The rule is
        // the whole of this one expression: `.or()` evaluates the enumeration
        // side first, so a registry that contributed no work at all outranks a
        // package that could not be fetched, whatever their argument positions.
        // Swapping the operands compiles, passes every other test here, and
        // silently inverts the contract.
        //
        // The lowest-index rule WITHIN each kind is `first_failure`'s, tested in
        // `index_common`; the acceptance suite checks the end-to-end exit code.
        let body = module_code();
        assert!(
            body.contains("index_common::first_failure(failures).or(refresh_failure)"),
            "the enumeration failure must be the `.or()` receiver — that is the precedence"
        );
    }

    #[test]
    fn the_registry_list_is_deduplicated_before_anything_is_fetched() {
        // Deduplicating the REGISTRY list rather than the flattened package set
        // is what makes `--dry-run` and the wet path agree: the earlier form
        // deduplicated after enumeration, so a repeated registry was fetched
        // twice and printed twice while the refresh ran once, and the preview
        // overstated the work by exactly the duplicate.
        //
        // `filter` over a `HashSet::insert` keeps first-seen order; collecting
        // into a `HashSet` would not, and every argument-order claim in this
        // command rides on it. The acceptance suite covers the behavioural half.
        let body = module_code();
        assert!(
            body.contains("self.registries.iter().filter(|r| seen.insert(*r))"),
            "the registry list must be deduplicated in argument order, before enumeration"
        );
        let dedup = body
            .find("let registries: Vec<&String>")
            .expect("the dedup is still here");
        let enumeration = body
            .find("for (input_index, registry) in registries.iter().enumerate()")
            .expect("the enumeration loop is still here");
        assert!(
            dedup < enumeration,
            "deduplicating after enumeration costs the duplicate registry a second round trip \
             and prints it twice under --dry-run"
        );
        assert!(
            !body.contains("packages.retain("),
            "a second dedup over the flattened set is dead once the registry list is deduplicated, \
             and two identifiers differing only by registry are two packages, correctly"
        );
    }

    /// This module's non-test source. The structural assertions above are about
    /// code that must not exist (a second gate, a local fan-out) or about an
    /// ordering no behavioural test can observe without a live patch tier.
    fn module_source() -> &'static str {
        include_str!("index_sync.rs")
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
