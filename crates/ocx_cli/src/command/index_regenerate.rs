// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{cli, oci::index};

use crate::api::data::index::{RegenerateEntry, RegenerateReport};
use crate::app::{CommandError, is_published_namespace};
use crate::command::index_common;

/// The one command that rewrites a catalog without consulting a source
/// (`adr_servable_index_snapshot.md` C-010).
///
/// `c/index.json` is derived data — every entry restates a digest the root
/// document on disk already carries — and every other writer only ever *adds*
/// to it. That leaves one drift nothing can repair: an entry naming a package
/// whose root is gone. This command re-derives the whole map from the `p/` walk,
/// which is the only operation that clears such an entry.
///
/// `ocx_cli` builds no index source and opens no client here; it checks what
/// only the CLI can see (that each `<REGISTRY>` is a *published* source) and
/// calls [`index::regenerate_catalog`] per registry. The user-facing help lives
/// on the `Index::Regenerate` variant, which is what clap renders.
#[derive(Parser)]
pub struct IndexRegenerate {
    #[clap(required = true, num_args = 1.., value_name = "REGISTRY")]
    registries: Vec<String>,
}

impl IndexRegenerate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // C-010's published-only guard, run over EVERY argument before the first
        // registry is touched. Pre-flighting it is the difference between
        // refusing a mistyped invocation and refusing it after already having
        // rewritten an earlier registry's catalog.
        for registry in &self.registries {
            ensure_published(context.config(), context.local_mirrors(), registry)?;
        }

        // No `--frozen` gate and no `--offline` gate (C-021). Both flags scope
        // pin movement and source contact; this consults no source, changes no
        // `tags[].content`, and touches no root's `repository`. `context` is
        // read for its config and its index store only — never for the remote
        // index accessor, which IS the offline gate.
        let store = context.local_index().index_store();

        // Sequential, one registry at a time: each call takes that source's
        // cross-process lock for the whole critical section, and the work is
        // local I/O. Failures still carry their input index and go through
        // `first_failure` rather than relying on the loop's order — that
        // ordering is a contract (C-010), and parallelising this loop is the
        // natural next change now that `index update` next door fans out.
        let mut entries = Vec::with_capacity(self.registries.len());
        let mut failures: Vec<(usize, ocx_lib::Error)> = Vec::new();
        for (input_index, registry) in self.registries.iter().enumerate() {
            match index::regenerate_catalog(store, registry).await {
                Ok(outcome) => entries.push(RegenerateEntry::from(outcome)),
                Err(error) => {
                    // Reported HERE as well as at `main.rs`'s boundary, and the
                    // redundancy is only apparent: `first_failure` propagates the
                    // lowest-index error alone, so in a multi-registry run every
                    // OTHER failure is printed on this line and never reaches the
                    // boundary at all. `MalformedRootDocument` quotes a repository
                    // name read off the foreign tree, so each of those chains is
                    // untrusted. Through the shared funnel, which neutralizes both
                    // halves at one site for all three `index` verbs.
                    index_common::log_failure("Failed to regenerate the catalog for", registry, &error);
                    failures.push((input_index, error));
                }
            }
        }

        // No partial report: a nonzero exit with a SUCCESS-shaped payload on
        // stdout is what `index catalog` and `index update` both refuse to
        // emit, and every failure is already on stderr above.
        if let Some(error) = index_common::first_failure(failures) {
            return Err(error.into());
        }

        context.api().report(&RegenerateReport::new(entries))?;
        Ok(ExitCode::SUCCESS)
    }
}

// C-010's aggregation rule — the lowest-index failure is the process error, so
// `classify_error` derives a deterministic exit whatever order the registries
// completed in — is `index_common::first_failure`. It lived here too, verbatim,
// until a review pointed out that a contract stated in two places is a contract
// with two copies to drift: C-010 and C-024's aggregation row are the same rule,
// so they are now the same four lines.

/// C-010's published-only guard: `<REGISTRY>` must name a **published** index
/// source — one carrying `[registries."<ns>"] index`.
///
/// A *derived* (plain-OCI) namespace has no `c/index.json` **by grammar**: its
/// catalog *is* the `p/` enumeration (`adr_index_indirection.md` A2).
/// [`index::regenerate_catalog`] would mint one anyway — it takes a store and a
/// source name and can see no configuration at all — and under Decision A an
/// absent `config.json` reads as format version 1, so that subtree would become
/// both resolvable and enumerable once served: precisely what the grammar
/// denies it.
///
/// The library states this as a precondition (C-007) and leaves the check here
/// because only the CLI can make it. The published/derived split lives in
/// configuration, which `IndexStore` has no access to, and "the subtree has no
/// `c/index.json`" cannot serve as the test — accepting exactly such a tree is
/// one of `regenerate_catalog`'s own preconditions.
///
/// The verdict comes from [`is_published_namespace`] — the **same** predicate
/// `Context::build_index_sources` filters on, not a restatement of it. Both of
/// its conditions matter here: a first cut tested only `index` presence and so
/// admitted a compiled-in default that a local `[mirrors."<ns>"]` entry pins at
/// a registry, which the resolver routes as plain-OCI while this verb minted a
/// `c/index.json` under it.
///
/// **Every _configured_ namespace sharing the argument's slug must pass, not
/// just the one named.** `regenerate_catalog` writes to `slugify(source)`, and
/// `to_relaxed_slug` maps `[^a-zA-Z0-9._-]` to `_` — so `a:b` and `a_b` are two
/// configured namespaces sharing one directory. Checking only the typed name
/// would let a published one act as a key to a derived twin's subtree, which is
/// the outcome this guard exists to prevent whatever route reaches it.
///
/// **Known gap, deliberately not closed here.** The walk can only see namespaces
/// that _have_ a `[registries]` entry. A registry with no entry at all is the
/// most derived shape there is, and `IndexStore` still keys its subtree on the
/// same `slugify` — so a published `[registries."reg.corp:5000"]` beside an
/// in-use unconfigured `reg.corp_5000` aliases undetected. Configuration cannot
/// enumerate it: the store holds slugs, and the slug → name mapping is lossy.
///
/// The sound code-level fix is to require `slugify(registry) == registry` for a
/// regenerate target, but that refuses every port-bearing published registry —
/// a natural on-prem shape — and leaves its operator no remedy, since they
/// cannot rename the host. Refusing a legitimate deployment outright is the
/// larger harm, so the guard's scope is narrowed in this doc instead of the
/// claim being left to overstate the check. Reaching the gap needs an operator
/// to configure a published namespace whose slug collides with an in-use
/// unconfigured one.
///
/// # Errors
///
/// [`CommandError`] classified [`cli::ExitCode::ConfigError`] (78) when the
/// registry, or any configured namespace aliasing its directory, resolves to a
/// derived source — or when it is not configured at all.
fn ensure_published(
    config: &ocx_lib::Config,
    local_mirrors: Option<&std::collections::HashMap<String, ocx_lib::MirrorConfig>>,
    registry: &str,
) -> Result<(), CommandError> {
    let refuse = |reason: String| {
        Err(CommandError::new(
            format!(
                "{reason} — a derived (plain-OCI) namespace's catalog is its p/ enumeration by grammar, \
                 so it has no c/index.json to regenerate. \
                 Set [registries.\"{registry}\"] index = \"<base-url>\" if it should be a published one."
            ),
            cli::ExitCode::ConfigError,
        ))
    };

    let Some(registries) = config.registries.as_ref() else {
        return refuse(format!("'{registry}' is not a published index source"));
    };
    let Some(entry) = registries.get(registry) else {
        return refuse(format!("'{registry}' is not a published index source"));
    };
    if !is_published_namespace(entry, registry, local_mirrors) {
        return refuse(format!("'{registry}' is not a published index source"));
    }

    // Slug aliases share the subtree this would write into, so a derived one
    // among them is a derived target reached under a published name.
    let slug = ocx_lib::file_structure::slugify(registry);
    for (namespace, entry) in registries {
        if namespace != registry
            && ocx_lib::file_structure::slugify(namespace) == slug
            && !is_published_namespace(entry, namespace, local_mirrors)
        {
            return refuse(format!(
                "'{registry}' and the derived namespace '{namespace}' both resolve to the index subtree '{slug}'"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Specification tests for `ocx index regenerate`, written from
    //! `design_spec_servable_index_snapshot.md` C-010 and C-021. Each names the
    //! contract row it pins.

    use super::*;
    use ocx_lib::cli::{ClassifyExitCode, ExitCode};

    fn config_with(entries: &[(&str, Option<&str>)]) -> ocx_lib::Config {
        let mut registries = std::collections::HashMap::new();
        for (namespace, index) in entries {
            registries.insert(
                (*namespace).to_string(),
                ocx_lib::RegistryConfig {
                    index: index.map(str::to_string),
                    ..Default::default()
                },
            );
        }
        ocx_lib::Config {
            registries: Some(registries),
            ..Default::default()
        }
    }

    /// A `[mirrors."<ns>"]` table pinning each namespace's REGISTRY role — the
    /// locally-authored shape that suppresses a compiled-in index default.
    fn mirrors_pinning(namespaces: &[&str]) -> std::collections::HashMap<String, ocx_lib::MirrorConfig> {
        namespaces
            .iter()
            .map(|namespace| {
                (
                    (*namespace).to_string(),
                    ocx_lib::MirrorConfig {
                        registry: Some("registry.corp.example".to_string()),
                        ..Default::default()
                    },
                )
            })
            .collect()
    }

    // ── C-010 — the published-only guard ─────────────────────────────────────

    #[test]
    fn a_published_registry_is_accepted() {
        let config = config_with(&[("ocx.sh", Some("https://index.ocx.sh"))]);
        assert!(
            ensure_published(&config, None, "ocx.sh").is_ok(),
            "a namespace carrying `index` is the published shape this verb repairs"
        );
    }

    #[test]
    fn a_derived_registry_is_refused_with_config_error() {
        // Three ways a namespace is derived, all of which `regenerate_catalog`
        // itself cannot tell apart from a published source: no `index` field, the
        // documented `index = ""` kill switch, and no `[registries]` entry at
        // all. Each must refuse rather than mint a `c/index.json` the grammar
        // says the subtree has none of.
        let cases = [
            ("plain.example", config_with(&[("plain.example", None)])),
            ("killed.example", config_with(&[("killed.example", Some(""))])),
            ("absent.example", config_with(&[("other.example", Some("https://i"))])),
            ("no.table", ocx_lib::Config::default()),
        ];
        for (registry, config) in cases {
            let error = ensure_published(&config, None, registry)
                .expect_err("a derived namespace has no c/index.json by grammar and must be refused");
            assert_eq!(
                error.classify(),
                Some(ExitCode::ConfigError),
                "{registry}: a derived source is an operator configuration mistake (78), not a data or I/O fault"
            );
            assert_eq!(
                ExitCode::ConfigError as u8,
                78,
                "ConfigError must be sysexits EX_CONFIG"
            );
        }
    }

    #[test]
    fn a_mirror_pinned_compiled_default_is_refused() {
        // The second half of `is_published_namespace`, and the half a
        // restated guard missed. `[registries."ocx.sh"] index` arrives from the
        // compiled-in defaults tier; a locally-authored `[mirrors."ocx.sh"]`
        // pinning its registry role suppresses it, and `build_index_sources`
        // logs "it resolves as a plain OCI registry through the mirror". The
        // resolver routes it as derived, so this verb must refuse it — otherwise
        // it mints a `c/index.json` under a namespace the grammar says has none.
        let mut config = config_with(&[("ocx.sh", Some("https://index.ocx.sh"))]);
        config
            .registries
            .as_mut()
            .and_then(|registries| registries.get_mut("ocx.sh"))
            .expect("fixture entry")
            .index_is_compiled_default = true;
        let mirrors = mirrors_pinning(&["ocx.sh"]);

        assert!(
            ensure_published(&config, None, "ocx.sh").is_ok(),
            "with no local mirror pin the compiled default is still a published source"
        );
        let error = ensure_published(&config, Some(&mirrors), "ocx.sh")
            .expect_err("a mirror-pinned compiled default resolves as plain OCI and must be refused");
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
    }

    #[test]
    fn a_derived_slug_alias_is_refused_under_its_published_twin() {
        // The guard is asked about the name typed; `regenerate_catalog` writes
        // to `slugify(name)`. `to_relaxed_slug` maps `[^a-zA-Z0-9._-]` to `_`,
        // so `a:b` and `a_b` share one directory — and checking only the typed
        // name let the published twin act as a key to the derived one's subtree.
        let config = config_with(&[("a:b", Some("https://i.invalid")), ("a_b", None)]);
        assert_eq!(
            ocx_lib::file_structure::slugify("a:b"),
            ocx_lib::file_structure::slugify("a_b"),
            "fixture: the two namespaces must actually alias, or this proves nothing"
        );

        let error = ensure_published(&config, None, "a:b")
            .expect_err("a published name must not unlock a derived namespace's subtree");
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));

        // Both published is fine: they still collide, but nothing derived is
        // reachable, so C-010's guarantee is intact.
        let both = config_with(&[("a:b", Some("https://i.invalid")), ("a_b", Some("https://i.invalid"))]);
        assert!(ensure_published(&both, None, "a:b").is_ok());
    }

    // ── C-010 — aggregation order ────────────────────────────────────────────

    #[test]
    fn the_lowest_input_index_failure_is_the_process_error() {
        // "Per-registry failures aggregate in input order; the lowest-index
        // error is the process error." Fed out of order, because the ordering
        // must be a property of this function rather than of the sequential
        // loop that happens to feed it today.
        //
        // C-010's rule and C-024's aggregation row are the same rule, so they
        // are now the same function; this test stays here because C-010 is a
        // contract of THIS command and would otherwise be pinned only by a
        // sibling's test.
        let failures = vec![
            (2, ocx_lib::Error::OfflineMode),
            (
                0,
                ocx_lib::Error::InternalPathInvalid(std::path::PathBuf::from("/first")),
            ),
            (1, ocx_lib::Error::OfflineMode),
        ];
        let error = index_common::first_failure(failures).expect("a non-empty failure list yields an error");
        assert!(
            matches!(&error, ocx_lib::Error::InternalPathInvalid(path) if path == std::path::Path::new("/first")),
            "input index 0's error must win regardless of completion order, got {error:?}"
        );
        assert!(
            index_common::first_failure(Vec::new()).is_none(),
            "no failures means no process error"
        );
        // And that this command actually uses it: a private copy beside the
        // shared one is how C-010 and C-024 drift apart while both tests pass.
        assert!(
            !module_code().contains("fn first_failure"),
            "the aggregation rule is stated once, in index_common.rs"
        );
    }

    // ── C-010 — grammar ──────────────────────────────────────────────────────

    #[test]
    fn at_least_one_registry_is_required() {
        use clap::CommandFactory;

        let error = IndexRegenerate::command()
            .try_get_matches_from(["regenerate"])
            .expect_err("`ocx index regenerate` with no registry names no work");
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn several_registries_bind_in_argument_order() {
        use clap::{CommandFactory, FromArgMatches};

        let matches = IndexRegenerate::command()
            .try_get_matches_from(["regenerate", "ocx.sh", "corp.example"])
            .expect("the positional is variadic");
        let parsed = IndexRegenerate::from_arg_matches(&matches).expect("binds");
        assert_eq!(
            parsed.registries,
            ["ocx.sh", "corp.example"],
            "argument order is the aggregation order and the report order"
        );
    }

    // ── CWE-150 — error prose leaves through the boundary ───────────────────

    #[test]
    fn the_failure_log_is_neutralized() {
        // This line is NOT covered by `main.rs`'s boundary: `first_failure`
        // returns the lowest-index error alone, so in a multi-registry run every
        // other failure's chain is printed here and nowhere else. Measured — with
        // this site's sanitizer removed and only the boundary in place, the
        // command's own line still emitted a raw U+202E.
        //
        // It now goes through `index_common::log_failure`, which sanitizes the
        // subject and the chain at one site for all three `index` verbs. The
        // count form this replaced — one sanitizer call per log macro — was
        // satisfiable by putting two sanitizer calls in one macro and paying for
        // a second macro with none.
        // Code only: the comments here quote the very forms the denylist below
        // refuses, which is the right thing for a comment to do and would
        // otherwise fail its own test.
        let body = module_code();
        assert!(
            body.contains("index_common::log_failure("),
            "a failed registry must still be reported — this is the only report for every failure \
             that does not become the process error"
        );
        // `log::` rather than a list of levels — see the same rule in
        // `index_update.rs` for why naming `debug` and `trace` would only move
        // the hole. This module emits nothing of its own at any level.
        for raw in ["log::", "eprintln!", "println!", "{error:#}", "{error}", "{:#}", "{:?}"] {
            assert!(
                !body.contains(raw),
                "`{raw}` in index_regenerate.rs: operator-facing failure prose goes through \
                 `index_common::log_failure`, which is the sanitized one"
            );
        }
    }

    // ── C-021 — `--frozen` and `--offline` both permit `regenerate` ──────────

    #[test]
    fn regenerate_adds_no_policy_gate() {
        // C-021 is a contract about code that must NOT exist: `regenerate`
        // consults no source, so neither flag applies and neither gate may be
        // added. A behavioural test cannot observe an absent branch, so this
        // asserts against the module's own source — the same shape as the
        // structural guards in `chain_refs_tests`.
        //
        // `context.oci_index()` is the offline gate (the accessor itself errors
        // under `--offline`) and `Error::PolicyBlocked` is the frozen gate;
        // neither may appear here. `config_view` is the struct both flags land
        // in, so reading it at all would be the first half of a gate.
        let body = module_code();
        for forbidden in ["oci_index()", "PolicyBlocked", "config_view("] {
            assert!(
                !body.contains(forbidden),
                "`{forbidden}` appears in index_regenerate.rs: C-021 forbids a --frozen or --offline gate here"
            );
        }
    }

    // ── C-010 — help text ────────────────────────────────────────────────────

    #[test]
    fn the_update_variants_stale_report_line_is_gone() {
        // C-010's help row: `regenerate`'s help must not copy `Index::Update`'s
        // "Packages with an update waiting are reported afterward" line, which
        // is being deleted because nothing ever reported it. Asserted against
        // the enum that carries both docs, so the line cannot come back on
        // either variant.
        let source = include_str!("index.rs");
        assert!(
            !source.contains("update waiting are reported"),
            "the stale `index update` report claim must not survive on any Index variant"
        );
    }

    /// This module's non-test source, for the structural assertions above.
    fn module_source() -> &'static str {
        include_str!("index_regenerate.rs")
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
