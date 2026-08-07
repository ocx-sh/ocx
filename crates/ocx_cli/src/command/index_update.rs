// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, oci::index};

use crate::options;

/// The one command that moves a pin.
///
/// The local index copy binds a tag to a digest and a package to a physical
/// registry; resolving, installing and running all read it as it stands. This
/// command is the only thing that changes it, and only for the packages named.
/// The user-facing help lives on the `Index::Update` variant, which is what
/// clap renders.
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

        // Tag each refresh with its input index so any failures can be surfaced
        // in input order. `refresh_tags` returns `crate::Result<()>` (not a
        // PackageManager op), so `drain_package_tasks` does not fit; the
        // index-tagged fan-out is inlined here (same shape as `package info`).
        let mut join_set: tokio::task::JoinSet<(usize, ocx_lib::Result<()>)> = tokio::task::JoinSet::new();
        for (index, identifier) in packages.iter().enumerate() {
            // Route to the index source that will answer for this package, if
            // any; otherwise refresh against the registry. Asking `jurisdiction`
            // rather than comparing namespaces keeps this from being a second,
            // independent guess: a registry mismatch already answers `Outside`,
            // and so does a name the source's published `config.json` says its
            // grammar cannot express — which then reroutes to the registry
            // instead of dying in `refresh_derived`.
            let mut selected = None;
            for source in index_sources {
                if source.jurisdiction(identifier).await != index::Jurisdiction::Outside {
                    selected = Some(index::Index::from_source(source.clone()));
                    break;
                }
            }
            let source = selected.unwrap_or_else(|| oci_index.clone());
            let context = context.clone();
            let identifier = identifier.clone();
            join_set.spawn(async move { (index, context.local_index().refresh_tags(&identifier, &source).await) });
        }

        let mut failures: Vec<(usize, ocx_lib::Error)> = Vec::new();
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((_, Ok(()))) => {}
                Ok((index, Err(e))) => {
                    log::error!("Failed to update index for '{}': {e:#}", packages[index]);
                    failures.push((index, e));
                }
                Err(join_err) => {
                    // A refresh task panicked — abort the rest and propagate,
                    // matching the `install_all` JoinSet panic precedent.
                    join_set.abort_all();
                    std::panic::resume_unwind(join_err.into_panic());
                }
            }
        }

        // Any failure → return the input-order-first error so `classify_error`
        // (main.rs) derives a deterministic nonzero exit. No stdout report: this
        // is an action command with no payload; the aggregated error on stderr
        // is the batch signal.
        if !failures.is_empty() {
            failures.sort_by_key(|(index, _)| *index);
            let (_, error) = failures.into_iter().next().expect("failures is non-empty");
            return Err(error.into());
        }

        // ── Piggyback: refresh site-patch descriptors when the patch tier is active. ──
        //
        // After the tag index is refreshed, also re-fetch patch descriptors for all
        // known installed bases so the patch tier stays in sync with the rest of the
        // index. This is best-effort: a sync failure (offline, registry unreachable,
        // required-companion error) is logged as a warning and does NOT fail the
        // index-update command — the tag refresh is the primary job.
        //
        // Only runs when:
        //   1. A `[patches]` section is configured (manager.patches().is_some()), AND
        //   2. The manager is online (is_offline() is false — sync_patches checks
        //      this internally, but we skip the call entirely when offline to avoid
        //      the OfflineMode error allocation overhead).
        //
        // `--frozen` needs no condition here: the policy gate at the top of this
        // function already refused the whole command, so a frozen invocation
        // never reaches the piggyback.
        //
        // ADR decision: piggyback keeps descriptor metadata fresh after every index
        // refresh without requiring users to remember a separate `ocx patch sync`.
        if context.manager().patches().is_some() && !context.manager().is_offline() {
            let host = oci::Platform::current().unwrap_or_else(oci::Platform::any);
            match context.manager().sync_patches(&[host]).await {
                Ok(_report) => {
                    log::debug!("index update: patch descriptor sync completed");
                }
                Err(error) => {
                    log::warn!("index update: patch descriptor sync failed (non-fatal): {error}");
                }
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
