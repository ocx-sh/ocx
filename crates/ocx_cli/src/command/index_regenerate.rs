// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use crate::api::data::index::{RegenerateEntry, RegenerateReport};
use crate::app::{CommandError, is_published_namespace};
use crate::command::index_common;

// See `adr_servable_index_snapshot.md` for why this command exists.
/// The one command that rewrites a catalog without consulting a source: it re-derives `c/index.json`
/// from the `p/` walk, the only operation that clears an entry whose root is gone.
#[derive(Parser)]
pub struct IndexRegenerate {
    #[clap(required = true, num_args = 1.., value_name = "REGISTRY")]
    registries: Vec<String>,
}

impl IndexRegenerate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // All validated first, so a typo cannot partially rewrite an earlier registry's catalog.
        for registry in &self.registries {
            ensure_published(context.config(), context.local_mirrors(), registry)?;
        }

        // Consults no source, so no `--frozen`/`--offline` gate: never read the remote index accessor,
        // which is that gate.
        let store = context.local_index().index_store();

        // Sequential: each source's cross-process lock is held only for local I/O.
        let mut entries = Vec::with_capacity(self.registries.len());
        let mut failures: Vec<(usize, anyhow::Error)> = Vec::new();
        for (input_index, registry) in self.registries.iter().enumerate() {
            match ocx_index::regenerate_catalog(store, registry).await {
                Ok(outcome) => entries.push(RegenerateEntry::from(outcome)),
                Err(error) => {
                    let error = anyhow::Error::from(error);
                    // Every failure but the returned one prints only here.
                    index_common::log_failure("Failed to regenerate the catalog for", registry, &error);
                    failures.push((input_index, error));
                }
            }
        }

        // No partial report: a nonzero exit carries no SUCCESS-shaped stdout.
        if let Some(error) = index_common::first_failure(failures) {
            return Err(error);
        }

        context.api().report(&RegenerateReport::new(entries))?;
        Ok(ExitCode::SUCCESS)
    }
}

/// `<REGISTRY>` must name a published index source: a derived namespace has no `c/index.json` by grammar,
/// and minting one would make the subtree resolvable (`adr_index_indirection.md` § On-disk layout).
/// [`is_published_namespace`], not an `index`-presence check, which admits a mirror-pinned default that
/// resolves as plain-OCI. Every namespace sharing the slug must pass too, or a published name keys into a
/// derived twin's subtree.
///
/// # Errors
///
/// [`CommandError`] (78) when the registry or a slug alias is derived, or the registry is not configured.
fn ensure_published(
    config: &ocx_config::Config,
    local_mirrors: Option<&std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>>,
    registry: &str,
) -> Result<(), CommandError> {
    let refuse = |reason: String| {
        Err(CommandError::new(
            format!(
                "{reason} — a derived (plain-OCI) namespace's catalog is its p/ enumeration by grammar, \
                 so it has no c/index.json to regenerate. \
                 Set [registries.\"{registry}\"] index = \"<base-url>\" if it should be a published one."
            ),
            ocx_exit::ExitCode::ConfigError,
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

    // Known gap: an unconfigured registry sharing the slug is invisible; requiring
    // `slugify(registry) == registry` would refuse every port-bearing published registry.
    let slug = ocx_store::file_structure::slugify(registry);
    for (namespace, entry) in registries {
        if namespace != registry
            && ocx_store::file_structure::slugify(namespace) == slug
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
    //! `design_spec_servable_index_snapshot.md`. Each names the contract row it
    //! pins.

    use super::*;
    use crate::exit::ClassifyExitCode;
    use ocx_exit::ExitCode;

    fn config_with(entries: &[(&str, Option<&str>)]) -> ocx_config::Config {
        let mut registries = std::collections::HashMap::new();
        for (namespace, index) in entries {
            registries.insert(
                (*namespace).to_string(),
                ocx_config::RegistryConfig {
                    index: index.map(str::to_string),
                    ..Default::default()
                },
            );
        }
        ocx_config::Config {
            registries: Some(registries),
            ..Default::default()
        }
    }

    /// A `[mirrors."<ns>"]` table pinning each namespace's REGISTRY role — the
    /// locally-authored shape that suppresses a compiled-in index default.
    fn mirrors_pinning(namespaces: &[&str]) -> std::collections::HashMap<String, ocx_config::mirror::MirrorConfig> {
        namespaces
            .iter()
            .map(|namespace| {
                (
                    (*namespace).to_string(),
                    ocx_config::mirror::MirrorConfig {
                        registry: Some("registry.corp.example".to_string()),
                        ..Default::default()
                    },
                )
            })
            .collect()
    }

    // ── The published-only guard ─────────────────────────────────────

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
            ("no.table", ocx_config::Config::default()),
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
            ocx_store::file_structure::slugify("a:b"),
            ocx_store::file_structure::slugify("a_b"),
            "fixture: the two namespaces must actually alias, or this proves nothing"
        );

        let error = ensure_published(&config, None, "a:b")
            .expect_err("a published name must not unlock a derived namespace's subtree");
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));

        // Both published is fine: they still collide, but nothing derived is
        // reachable, so the published-only guarantee is intact.
        let both = config_with(&[("a:b", Some("https://i.invalid")), ("a_b", Some("https://i.invalid"))]);
        assert!(ensure_published(&both, None, "a:b").is_ok());
    }

    // ── Aggregation order ────────────────────────────────────────────

    #[test]
    fn the_lowest_input_index_failure_is_the_process_error() {
        // "Per-registry failures aggregate in input order; the lowest-index
        // error is the process error." Fed out of order, because the ordering
        // must be a property of this function rather than of the sequential
        // loop that happens to feed it today.
        //
        // The aggregation rule lives once, in `index_common::first_failure`, so
        // this test stays here even though the function is shared: without it,
        // this command's own contract would be pinned only by a sibling's test.
        let failures: Vec<(usize, anyhow::Error)> = vec![
            (2, ocx_package_manager::Error::OfflineMode.into()),
            (
                0,
                ocx_package_manager::Error::InternalPathInvalid(std::path::PathBuf::from("/first")).into(),
            ),
            (1, ocx_package_manager::Error::OfflineMode.into()),
        ];
        let error = index_common::first_failure(failures).expect("a non-empty failure list yields an error");
        // Downcast rather than `matches!`: the aggregation now carries
        // `anyhow::Error`, and the property under test is still which
        // *concrete* error won, not merely that one did.
        assert!(
            matches!(
                error.downcast_ref::<ocx_package_manager::Error>(),
                Some(ocx_package_manager::Error::InternalPathInvalid(path)) if path == std::path::Path::new("/first")
            ),
            "input index 0's error must win regardless of completion order, got {error:?}"
        );
        assert!(
            index_common::first_failure(Vec::new()).is_none(),
            "no failures means no process error"
        );
        // And that this command actually uses it: a private copy beside the
        // shared one is how the two contracts drift apart while both tests pass.
        assert!(
            !module_code().contains("fn first_failure"),
            "the aggregation rule is stated once, in index_common.rs"
        );
    }

    // ── Grammar ──────────────────────────────────────────────────────

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

    // ── `--frozen` and `--offline` both permit `regenerate` ──────────

    #[test]
    fn regenerate_adds_no_policy_gate() {
        // This is a contract about code that must NOT exist: `regenerate`
        // consults no source, so neither flag applies and neither gate may be
        // added. A behavioural test cannot observe an absent branch, so this
        // asserts against the module's own source — the same shape as the
        // structural guards in `chain_refs_tests`.
        //
        // `context.oci_index()` is the offline gate (the accessor itself errors
        // under `--offline`) and `index_common::policy_blocked` is the frozen gate;
        // neither may appear here. `config_view` is the struct both flags land
        // in, so reading it at all would be the first half of a gate.
        let body = module_code();
        for forbidden in ["oci_index()", "policy_blocked(", "config_view("] {
            assert!(
                !body.contains(forbidden),
                "`{forbidden}` appears in index_regenerate.rs: C-021 forbids a --frozen or --offline gate here"
            );
        }
    }

    // ── Help text ────────────────────────────────────────────────────

    #[test]
    fn the_update_variants_stale_report_line_is_gone() {
        // This help row: `regenerate`'s help must not copy `Index::Update`'s
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
