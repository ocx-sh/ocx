// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use ocx_config::env;
use ocx_config::loader::ConfigInputs;
use ocx_config::loader::ConfigLoader;
use ocx_config::tls::TlsError;
use ocx_config::tls::resolve_extra_roots;
use ocx_config::tls::sigstore_extra_roots;
use ocx_console::{ColorModeConfig, Printer, UserInterface};
use ocx_package::publisher::Publisher;
use ocx_store::file_structure::{self, StateStore};
use ocx_util::tls::ExtraRoots;

use crate::api;
use crate::command::package_sign_common::{SigstoreEndpoint, explicit_trust_root_path, resolve_endpoint};

use super::ContextOptions;

#[derive(Clone)]
pub struct Context {
    offline: bool,
    project_path: Option<PathBuf>,
    remote_client: Option<ocx_oci::Client>,
    oci_index: Option<ocx_index::OciIndex>,
    /// One `index.ocx.sh`-protocol source per index-bearing namespace (`adr_index_indirection.md`),
    /// chained ahead of `oci_index`; empty under `--offline`.
    index_sources: Vec<ocx_index::OcxIndex>,
    /// Registry client built lazily by [`Self::verify_client`], present even under `--offline`,
    /// which blocks only the Sigstore trust services. Shared so every caller reuses one auth
    /// store, token cache and connection pool.
    registry_client_cell: std::sync::Arc<std::sync::OnceLock<ocx_oci::Client>>,
    mirror_map: ocx_oci::MirrorMap,
    local_index: ocx_index::LocalIndex,
    file_structure: file_structure::FileStructure,
    api: api::Api,
    ui: UserInterface,
    default_index: ocx_index::Index,
    manager: ocx_package_manager::PackageManager,
    default_registry: String,
    config_trust: ocx_trust::TrustConfig,
    config_view: env::OcxConfigView,
    /// The `toolchain_dir` root, already past every refusal; `None` for the in-project default.
    toolchain_root: Option<ocx_config::ToolchainRoot>,
    concurrency: ocx_package_manager::Concurrency,
    progress: ocx_console::progress::ProgressManager,
    /// The fully merged config (every tier).
    config: ocx_config::Config,
    /// The tiers a managed payload folds between: `config_base` below it, `config_overlay`
    /// (`OCX_CONFIG` / `--config`) above it. Kept from the one load, since a second load would
    /// re-emit the loader's discovery warnings.
    config_base: ocx_config::Config,
    config_overlay: ocx_config::Config,
    /// The locally-authored `[mirrors]` table [`is_published_namespace`] needs after init; never
    /// the merged view, or a managed payload could revoke the verified index path.
    local_mirrors: Option<std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>>,
    /// Resolved once so every consumer (the required gate, `config update`, the refresh hook)
    /// agrees on one value.
    managed_config_env_override: Option<String>,
    /// Hosts reachable over plain HTTP, resolved once so every plain-HTTP gate agrees on a host.
    insecure_hosts: Vec<String>,
    /// `Some` only when the snapshot identity-matches the effective source.
    managed_config_snapshot: Option<ocx_config::managed_config::ManagedConfigSnapshot>,
    /// Digest of the active patch snapshot's file bytes.
    patch_snapshot_digest: Option<ocx_oci::Digest>,
    /// The `[records]` config tier, already SYSTEM-clamped by the loader. Held raw because a
    /// resolved policy re-folded with the per-command flags would lose the clamp.
    records_config: ocx_package_manager::record::RecordsOptions,
    /// The `OCX_RECORDS_*` tier, read once so every frame of one invocation
    /// folds the same values.
    records_env: ocx_package_manager::record::RecordsOptions,
    /// Extra-CA roots across env and every `config.toml` tier, pre-applied to every registry,
    /// index and forge client.
    extra_roots_merged: ExtraRoots,
    /// Extra-CA roots from env and the local tiers only, so the managed-tier fetch is never
    /// secured by material that tier delivered.
    extra_roots_local: ExtraRoots,
}

/// The two `[managed]` gates `Context::try_init` needs, named so the adjacent `bool`s cannot
/// be transposed.
pub struct ManagedConfigGate {
    /// Fail closed with `SnapshotRequired` (exit 78) when `required = true` and no matching
    /// snapshot exists (`adr_managed_config_tier.md` Decision E). `false` for `ocx config update`
    /// and the `self`/static commands, which must stay reachable to fix that state.
    pub enforce_required: bool,
    /// `true` only for `ocx config setup`, `ocx config update` and `ocx self setup`, which can adopt
    /// a new managed source with no seed and so need the fetch client even when none resolves.
    pub onboarding: bool,
}

impl Context {
    pub async fn try_init(
        options: &ContextOptions,
        color_config: ColorModeConfig,
        managed_config_gate: ManagedConfigGate,
    ) -> anyhow::Result<Context> {
        // Created before the subscriber, or log lines can tear a rendered bar. `in_seam` brings its
        // own scoped subscriber, so it skips every process-global below.
        let in_seam = super::in_seam();
        let progress = if !in_seam && ocx_console::ProgressMode::detect().stderr && !options.quiet {
            ocx_console::progress::ProgressManager::stderr()
        } else {
            ocx_console::progress::ProgressManager::disabled()
        };

        if !in_seam {
            crate::tracing_init::LogSettings::default()
                .with_console_level(options.log_level)
                .with_stderr_color(color_config.stderr)
                .init_with_progress(&progress)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        log::debug!("Creating context with options: {:?}", options);

        // Fills the host-libc cache `Platform::current()` reads; local-only, so it runs offline too.
        if !in_seam {
            ocx_oci::HostCapabilities::detect_and_cache(
                ocx_config::home::default_ocx_root()
                    .map(|root| ocx_store::file_structure::StateStore::new(root.join("state")).host_capabilities_file())
                    .as_deref(),
            )
            .await;
        }

        if options.offline && options.remote {
            // `--offline` overrides `--remote`, so the pair is accepted as pinned-only mode.
            log::info!(
                "--offline --remote: pinned-only mode - tag and catalog lookups will not contact a source. \
                 Tag-addressed resolution attempts must be satisfied locally or by digest-pinned identifiers."
            );
        }

        let project_path = options.project.clone();

        let cwd = ocx_util::env::current_dir()?;
        let loaded_config = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: options.config.as_deref(),
            explicit_project_path: options.project.as_deref(),
            cwd: Some(&cwd),
        })
        .await?;
        let config = loaded_config.merged;
        let local_only_config = loaded_config.local_only;
        let config_base = loaded_config.base;
        let config_overlay = loaded_config.overlay;
        let managed_config_snapshot = loaded_config.managed_config_snapshot;
        let resolved_managed_config = loaded_config.resolved_managed_config;
        let managed_snapshot_state = loaded_config.managed_snapshot_state;

        // Every plain-HTTP gate below shares this one set.
        let insecure_hosts = ocx_config::insecure::insecure_hosts(&config, &env::insecure_registries());

        // The registry role feeds only the OCI client's transport rewrite.
        let resolved_mirrors = ocx_config::mirror::resolve_mirror_map(&config, env::mirrors()?, &insecure_hosts)
            .map_err(anyhow::Error::new)?;
        let mirror_map = ocx_oci::MirrorMap::new(resolved_mirrors.registry.clone());

        let printer = Printer::new(color_config.stdout, color_config.stderr);
        let ui = UserInterface::new(printer, !in_seam && console::Term::stderr().is_term(), options.quiet);
        // Shared with the Context-free `ocx version` bypass so both pick the same default format.
        let api = options.build_api(color_config);

        // Fail-closed: a CA error aborts before any client is built.
        let extra_ca_env = ocx_util::env::var(env::keys::OCX_EXTRA_CA_CERTS).filter(|value| !value.is_empty());
        let (extra_ca_tier_merged, extra_ca_tier_local) = (
            loaded_config.extra_ca_certs_tier,
            loaded_config.extra_ca_certs_tier_local,
        );
        // The local view differs from the merged one only when no env CA is set and the managed
        // tier supplied the key; otherwise it is a clone.
        let needs_walk =
            extra_ca_env.is_some() || config.extra_ca_certs.is_some() || config.extra_ca_certs_pem.is_some();
        let (config, local_only_config, extra_roots_merged, extra_roots_local) = if needs_walk {
            // A `JoinError` here is a panicked pool task, an I/O-class failure, never `NotFound`.
            tokio::task::spawn_blocking(move || {
                let merged = resolve_extra_roots(&config, extra_ca_env.as_deref(), extra_ca_tier_merged)?;
                let views_agree =
                    extra_ca_env.is_some() || extra_ca_tier_merged != Some(ocx_config::ConfigTier::Managed);
                let local = if views_agree {
                    merged.clone()
                } else {
                    resolve_extra_roots(&local_only_config, extra_ca_env.as_deref(), extra_ca_tier_local)?
                };
                Ok::<_, TlsError>((config, local_only_config, merged, local))
            })
            .await
            .map_err(|join| std::io::Error::other(format!("extra CA roots resolution task panicked: {join}")))??
        } else {
            (config, local_only_config, ExtraRoots::default(), ExtraRoots::default())
        };
        ocx_util::tls::install_sigstore_roots(sigstore_extra_roots(
            &extra_roots_merged,
            &extra_roots_local,
            extra_ca_tier_merged,
            resolved_managed_config
                .as_ref()
                .is_some_and(|resolved| resolved.source.digest().is_some()),
        ));

        // A plain-HTTP mirror needs its own host declared insecure; the scheme alone opts nothing
        // out (`adr_oci_registry_mirror.md`).
        let registry_client_cell: std::sync::Arc<std::sync::OnceLock<ocx_oci::Client>> =
            std::sync::Arc::new(std::sync::OnceLock::new());
        // Offline leaves the cell unbuilt, so `remote_client` is `None` and the manager stays offline.
        let (remote_client, oci_index) = if options.offline {
            (None, None)
        } else {
            let client = registry_client_cell
                .get_or_init(|| client_builder(&mirror_map, &progress, &insecure_hosts, &extra_roots_merged).build())
                .clone();
            (
                Some(client.clone()),
                Some(ocx_index::OciIndex::new(ocx_index::OciIndexConfig { client })),
            )
        };
        let file_structure = file_structure::FileStructure::new();
        // `--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index` (`adr_index_indirection.md`). A redirected
        // home may be read-only, so its locks stay under `$OCX_HOME/locks`.
        let index_store = options
            .index
            .clone()
            .or_else(|| ocx_util::env::var(env::keys::OCX_INDEX).map(std::path::PathBuf::from))
            .map(|home| ocx_index::IndexStore::new(home).with_locks_root(file_structure.locks.clone()))
            .unwrap_or_else(|| ocx_index::IndexStore::machine_local(&file_structure));
        // Also gates the offline path: a committed root's yanked tag is refused on a local resolve.
        let allow_yanked = ocx_util::env::flag(env::keys::OCX_ALLOW_YANKED, false);
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: index_store.clone(),
        })
        .with_allow_yanked(allow_yanked)
        // Set on the local index, not per chain, so every chain built from it reads one SSRF
        // exemption; `--offline` builds no sources to carry it.
        .with_trusted_hosts(trusted_hosts_by_namespace(&config))
        // From config, not the built sources: `--offline` builds none, and an index-owned name must
        // still never be read at the host it spells.
        .with_index_namespaces(index_namespaces(&config, local_only_config.mirrors.as_ref()))
        // The dial-site SSRF guard needs a physical pointer's scheme to pick the proxy covering it.
        .with_insecure_hosts(insecure_hosts.clone());

        // Frozen keeps the remote source so digest-pinned content still fetches.
        let online_mode = Self::online_chain_mode(options.frozen, options.remote);
        let index_sources = Self::build_index_sources(
            remote_client.is_some(),
            &config,
            local_only_config.mirrors.as_ref(),
            &resolved_mirrors.index,
            &mirror_map,
            &insecure_hosts,
            &progress,
            &extra_roots_merged,
        )?;
        let (mode, sources) = Self::chain_mode_and_sources(oci_index.as_ref(), &index_sources, online_mode);
        // The blob store lets an installed tool's leaf manifest resolve offline: an absent dispatch
        // object recovers from it before any source walk.
        let selected_index = ocx_index::Index::from_chained_with_content_store(
            local_index.clone(),
            sources,
            mode,
            file_structure.blobs.clone(),
        );

        let default_registry = ocx_util::env::string(
            env::keys::OCX_DEFAULT_REGISTRY,
            config
                .resolved_default_registry()
                .map(str::to_owned)
                .unwrap_or_else(|| ocx_oci::DEFAULT_REGISTRY.into()),
        );

        // `no_patches` is never grafted onto this tier, or it turns ambient and is re-forwarded over
        // `OCX_PATCHES` into unrelated children.
        let resolved_patches = match ocx_config::patch::resolve_patch_config(&config).map_err(anyhow::Error::new)? {
            Some(resolved) => Some(resolved),
            None => ocx_config::patch::patches_from_env().map_err(anyhow::Error::new)?,
        };

        // `OCX_PATCH_SNAPSHOT` is the sole selector, orthogonal to `--frozen`.
        let patch_snapshot_path = ocx_util::env::var(env::keys::OCX_PATCH_SNAPSHOT).map(std::path::PathBuf::from);
        let loaded_patch_snapshot = if let Some(ref path) = patch_snapshot_path {
            ocx_package_manager::patch::PatchSnapshot::read(path)
                .await
                .map_err(anyhow::Error::new)?
        } else {
            None
        };
        let (patch_snapshot, patch_snapshot_digest) = match loaded_patch_snapshot {
            Some((snapshot, digest)) => (Some(snapshot), Some(digest)),
            None => (None, None),
        };

        // Captured raw: the per-command flags fold on top later (see the `records_config` field).
        let records_config = config.records.clone().unwrap_or_default();
        let records_env = ocx_package_manager::record::RecordsOptions::from_env();

        // `OCX_NO_CONFIG=1` is hermetic: it also suppresses the env override read here.
        let no_config = ocx_util::env::flag("OCX_NO_CONFIG", false);
        let managed_config_env_override = if no_config {
            None
        } else {
            ocx_util::env::var(env::keys::OCX_MANAGED_CONFIG)
        };

        // The fetch client uses the local-only mirror view, or the payload's own `[mirrors]` could
        // redirect its own fetch. Built only when a source resolves: it costs a bundled-CA conversion.
        let has_managed_source = managed_config_env_override
            .as_deref()
            .is_some_and(|source| !source.is_empty())
            || config
                .managed
                .as_ref()
                .and_then(|managed| managed.source.as_deref())
                .is_some_and(|source| !source.is_empty());
        // Onboarding commands need the client with no source yet; `ocx self activate` is not one,
        // since it runs on every shell startup.
        let needs_managed_config_client = has_managed_source || managed_config_gate.onboarding;
        let managed_config_client = if options.offline || !needs_managed_config_client {
            None
        } else {
            Some(build_managed_config_client(
                &local_only_config,
                env::mirrors()?,
                &env::insecure_registries(),
                &progress,
                &extra_roots_local,
            )?)
        };

        // Resolved once in the loader from the local-only view (`adr_managed_config_tier.md`
        // Decision A). The loader swallows a resolution error, so a configured-but-unresolvable seed
        // re-resolves here only to surface the typed error (exit 78).
        let resolved_managed_target = match resolved_managed_config {
            Some(resolved) => Some(resolved),
            None if has_managed_source => {
                ocx_config::managed::resolve_managed_target(&config, managed_config_env_override.as_deref())?
            }
            None => None,
        };

        // One loader-reported state feeds both consumers below, so no consumer reads an
        // identity-mismatched snapshot as current and the required gate cannot drift from the merge.
        let snapshot_identity_matches = managed_snapshot_state != ocx_config::managed::ManagedSnapshotState::Unmatched;

        // Identity alone is not the gate: a payload that failed to parse contributes nothing, so it
        // must not satisfy a required tier.
        let managed_config = match resolved_managed_target {
            None => None,
            Some(resolved) => match ocx_config::managed::enforce_required_snapshot(resolved, managed_snapshot_state) {
                Ok(resolved) => Some(resolved),
                Err(_snapshot_required) if !managed_config_gate.enforce_required => None,
                Err(source) => return Err(anyhow::Error::new(source)),
            },
        };

        // Only the identity-matched snapshot is exposed; an unparseable one still surfaces so
        // `config update --check` can diagnose it.
        let managed_config_snapshot = managed_config_snapshot.filter(|_| snapshot_identity_matches);

        let manager = ocx_package_manager::PackageManager::new(
            file_structure.clone(),
            selected_index.clone(),
            remote_client.clone(),
            &default_registry,
        )
        .with_progress(progress.clone())
        .with_patches(resolved_patches.clone())
        .with_patch_snapshot(patch_snapshot)
        .with_managed_config_client(managed_config_client)
        // Companion and site-patch lookups must read the same (`--index`-redirected) store.
        .with_index(index_store);

        // Attached once on the shared manager so every install surface inherits auto-verify.
        let operator_policies = config.trust_policies().to_vec();
        // Read once: auto-verify and the forwarded `config_view` must see the same value.
        let no_verify_env = ocx_util::env::flag(env::keys::OCX_NO_VERIFY, false);
        let auto_verify = if operator_policies.is_empty() {
            None
        } else {
            let client = registry_client_cell
                .get_or_init(|| client_builder(&mirror_map, &progress, &insecure_hosts, &extra_roots_merged).build())
                .clone();
            build_auto_verify(
                operator_policies,
                config.trust.as_ref().and_then(|t| t.sigstore.clone()),
                &client,
                options.offline,
                file_structure.state.clone(),
                no_verify_env,
            )?
            .map(ocx_package_manager::AutoVerify::new)
        };
        let manager = manager.with_auto_verify(auto_verify);

        // Pins child spawns to this binary via `OCX_BINARY_PIN`; on failure the child's
        // `${OCX_BINARY_PIN:-ocx}` degrades to a `$PATH` lookup.
        let self_exe = std::env::current_exe().unwrap_or_else(|e| {
            log::warn!("Could not resolve current exe: {e}");
            std::path::PathBuf::from("ocx")
        });
        let mut config_view = options.as_view(self_exe);
        // The merged pre-role-parse map, so a child re-parses what the parent's transport used.
        config_view.mirrors = resolved_mirrors.merged.into_iter().collect();
        config_view.patches = resolved_patches;
        config_view.patch_snapshot = patch_snapshot_path;
        config_view.managed_config_source = managed_config.as_ref().map(|resolved| resolved.source.to_string());
        // Only the env opt-out is forwarded; `--no-verify` is a one-shot choice.
        config_view.no_verify = no_verify_env;
        // Forwarded, or the child re-reads the config chain the parent pruned and the two frames
        // of one launch resolve against different configuration.
        config_view.no_config = ocx_util::env::flag(env::keys::OCX_NO_CONFIG, false);
        // Forwarded because `apply_ocx_config` is set-or-remove and would strip `OCX_RECORDS_DIR`.
        // The clamp applies during `merge`, so the lock clears only after it.
        let mut forwarded_records = records_config.clone();
        forwarded_records.merge(records_env.clone());
        forwarded_records.required = None;
        forwarded_records.system_locked = false;
        config_view.records = forwarded_records;
        check_global_project_exclusivity(&config_view)?;
        check_frozen_remote_exclusivity(&config_view)?;
        // Resolved here so a refused root exits 78 on every command, read-only ones included; mapped
        // through `ConfigError` so `classify_error` reaches it via `source()`.
        let toolchain_root = ocx_config::ToolchainRoot::resolve(&config).map_err(ocx_config::error::Error::from)?;
        // Forwarded because `apply_ocx_config` is set-or-remove and would strip `OCX_TOOLCHAIN_DIR`,
        // leaving a hermetic child with no root.
        config_view.toolchain_dir = toolchain_root.as_ref().map(|root| root.as_path().to_path_buf());
        let concurrency = resolve_concurrency(options.jobs);

        Ok(Context {
            remote_client,
            oci_index,
            index_sources,
            registry_client_cell,
            mirror_map,
            offline: options.offline,
            project_path,
            file_structure,
            api,
            ui,
            local_index,
            default_index: selected_index,
            manager,
            default_registry,
            config_trust: config.trust.clone().unwrap_or_default(),
            config_view,
            toolchain_root,
            concurrency,
            progress,
            config,
            config_base,
            config_overlay,
            local_mirrors: local_only_config.mirrors.clone(),
            managed_config_env_override,
            insecure_hosts,
            managed_config_snapshot,
            patch_snapshot_digest,
            records_config,
            records_env,
            extra_roots_merged,
            extra_roots_local,
        })
    }

    /// Fold this invocation's `[records]` tiers with `args` (the command's `--records-dir` /
    /// `--records-name`, or [`RecordsOptions::default`]) into a recording policy.
    ///
    /// # Errors
    ///
    /// [`RecordsError`](ocx_package_manager::record::RecordsError) (exit 78) when the winning
    /// filename template is malformed, raised before any child starts.
    pub fn records(
        &self,
        args: ocx_package_manager::record::RecordsOptions,
    ) -> Result<ocx_package_manager::record::RecordingPolicy, ocx_package_manager::record::RecordsError> {
        ocx_package_manager::record::resolve_records(self.records_config.clone(), self.records_env.clone(), args)
    }

    /// Shared span-free progress manager; wrap long operations in its guards.
    pub fn progress(&self) -> &ocx_console::progress::ProgressManager {
        &self.progress
    }

    pub fn is_offline(&self) -> bool {
        self.offline
    }

    /// The explicit `--project` / `OCX_PROJECT` path, if any; pass it to `ProjectConfig::resolve`
    /// so the flag is not silently discarded.
    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    /// The validated `toolchain_dir` root, or `None` for the in-project `<project>/.ocx/toolchain`
    /// default. Ignored by the global tier.
    #[must_use]
    pub fn toolchain_root(&self) -> Option<&ocx_config::ToolchainRoot> {
        self.toolchain_root.as_ref()
    }

    /// Which toolchain tree this invocation renders and, for a project, its canonical directory.
    ///
    /// That path keys both the render and consent stamps, so a second spelling would file one
    /// project under two `state/projects/<key>/` directories.
    ///
    /// # Errors
    ///
    /// The canonicalisation's I/O failure, with the path attached. Derive the scope before
    /// committing anything, so a failure leaves nothing half-done.
    pub async fn toolchain_render_scope(
        &self,
        config_path: &Path,
    ) -> anyhow::Result<ocx_store::file_structure::RenderStampScope> {
        use ocx_store::file_structure::RenderStampScope;

        if self.global() {
            return Ok(RenderStampScope::Global);
        }
        let path = config_path.to_path_buf();
        let directory = tokio::task::spawn_blocking(move || {
            ocx_project::consent::canonical_project_dir(&path)
                .map_err(|error| ocx_util::error::FileError::new(path, error))
        })
        .await??;
        Ok(RenderStampScope::Project(directory))
    }

    /// Whether `--global` / `OCX_GLOBAL` selected `$OCX_HOME/ocx.toml`; exclusive with an
    /// explicit project (see [`check_global_project_exclusivity`]).
    pub fn global(&self) -> bool {
        self.config_view.global
    }

    pub fn remote_client(&self) -> Result<&ocx_oci::Client, ocx_package_manager::Error> {
        self.remote_client
            .as_ref()
            .ok_or(ocx_package_manager::Error::OfflineMode)
    }

    pub fn oci_index(&self) -> Result<&ocx_index::OciIndex, ocx_package_manager::Error> {
        self.oci_index.as_ref().ok_or(ocx_package_manager::Error::OfflineMode)
    }

    pub fn local_index(&self) -> &ocx_index::LocalIndex {
        &self.local_index
    }

    /// Every `index.ocx.sh`-protocol source; empty under `--offline`.
    pub fn index_sources(&self) -> &[ocx_index::OcxIndex] {
        &self.index_sources
    }

    pub fn default_index(&self) -> &ocx_index::Index {
        &self.default_index
    }

    /// The default-mode resolution chain, for callers building an [`Index`] over another content
    /// store (`ocx patch test`'s scratch root).
    ///
    /// Sharing it keeps one identifier from naming two artifacts in one invocation, and it keeps
    /// the invocation's ceiling so `--frozen` still refuses an unindexed tag (exit 81).
    pub fn chain_sources(&self) -> (ocx_index::ChainMode, Vec<ocx_index::Index>) {
        let online_mode = Self::online_chain_mode(self.config_view.frozen, self.config_view.remote);
        Self::chain_mode_and_sources(self.oci_index.as_ref(), &self.index_sources, online_mode)
    }

    /// The online policy ceiling. `--offline` is not an arm: it leaves the remote index unbuilt.
    fn online_chain_mode(frozen: bool, remote: bool) -> ocx_index::ChainMode {
        if frozen {
            ocx_index::ChainMode::Frozen
        } else if remote {
            ocx_index::ChainMode::Remote
        } else {
            ocx_index::ChainMode::Default
        }
    }

    /// Index for `ocx update`: resolves tags live under the `--offline` / `--frozen` ceilings and
    /// never commits tag pointers to the shared local index (`adr_toolchain_update_family.md`).
    pub fn update_index(&self) -> ocx_index::Index {
        let online_mode = if self.config_view.frozen {
            ocx_index::ChainMode::Frozen
        } else {
            ocx_index::ChainMode::Remote
        };
        let (mode, sources) = Self::chain_mode_and_sources(self.oci_index.as_ref(), &self.index_sources, online_mode);
        ocx_index::Index::from_chained_lock_scoped(self.local_index.clone(), sources, mode)
    }

    /// Mode and sources come from one value, so `--offline` never pairs with a built remote.
    fn chain_mode_and_sources(
        oci_index: Option<&ocx_index::OciIndex>,
        index_sources: &[ocx_index::OcxIndex],
        online_mode: ocx_index::ChainMode,
    ) -> (ocx_index::ChainMode, Vec<ocx_index::Index>) {
        match oci_index {
            None => (ocx_index::ChainMode::Offline, Vec::new()),
            Some(remote) => {
                let mut sources = Vec::with_capacity(index_sources.len() + 1);
                // Index sources register before the registry, so an index-bearing namespace resolves through
                // the verified two-hop path and yank gate, never a same-named registry.
                for source in index_sources {
                    sources.push(ocx_index::Index::from_source(source.clone()));
                }
                sources.push(ocx_index::Index::from_remote(remote.clone()));
                (online_mode, sources)
            }
        }
    }

    /// One `index.ocx.sh`-protocol source per [`is_published_namespace`] namespace, sorted for a
    /// deterministic chain order.
    ///
    /// The single place index clients are minted: a plain-`http://` base is refused unless its
    /// host is in `insecure_hosts`.
    #[expect(
        clippy::too_many_arguments,
        reason = "the resolved inputs one invocation settles once: the mode, both mirror views, the two trust sets and the progress sink"
    )]
    fn build_index_sources(
        online: bool,
        config: &ocx_config::Config,
        local_mirrors: Option<&std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>>,
        mirrors_index: &std::collections::BTreeMap<String, ocx_oci::client::mirror_map::ParsedMirror>,
        registry_mirrors: &ocx_oci::MirrorMap,
        insecure_hosts: &[String],
        progress: &ocx_console::progress::ProgressManager,
        extra_roots: &ExtraRoots,
    ) -> anyhow::Result<Vec<ocx_index::OcxIndex>> {
        if !online {
            return Ok(Vec::new());
        }
        let Some(registries) = config.registries.as_ref() else {
            return Ok(Vec::new());
        };

        let mut namespaces: Vec<&String> = registries
            .iter()
            .filter(|(namespace, entry)| is_published_namespace(entry, namespace, local_mirrors))
            .map(|(namespace, _)| namespace)
            .collect();
        namespaces.sort();

        let allow_yanked = ocx_util::env::flag(env::keys::OCX_ALLOW_YANKED, false);
        let mut sources = Vec::with_capacity(namespaces.len());
        for namespace in namespaces {
            // Pins the connect address so a root `repository` host cannot rebind between validate and
            // connect; per namespace, never unioned, so one exemption cannot widen another's.
            let trusted_hosts = trusted_hosts_for(config, namespace);
            let client = client_builder(registry_mirrors, progress, insecure_hosts, extra_roots)
                .ssrf_guard(trusted_hosts.clone())
                .build();
            // `resolve_base_url` settles the transport with the scheme; choosing one here re-derives it.
            let base =
                ocx_index::OcxIndex::resolve_base_url(config, namespace, mirrors_index, insecure_hosts, extra_roots)?;
            sources.push(ocx_index::OcxIndex::new(ocx_index::OcxIndexConfig {
                transport: base.transport,
                base_url: base.url,
                namespace: namespace.clone(),
                client,
                allow_yanked,
                trusted_hosts,
                insecure_hosts: insecure_hosts.to_vec(),
                proxy_rules: ocx_oci::ssrf::proxy_rules(),
            }));
        }
        Ok(sources)
    }

    /// The published-index source for `namespace`, built without any `[mirrors]` index override so
    /// a read names the canonical index. `None` when the namespace has no index.
    pub fn canonical_index_source(&self, namespace: &str) -> anyhow::Result<Option<ocx_index::OcxIndex>> {
        let sources = Self::build_index_sources(
            true,
            &self.config,
            self.local_mirrors.as_ref(),
            &std::collections::BTreeMap::new(),
            &self.mirror_map,
            &self.insecure_hosts,
            &self.progress,
            &self.extra_roots_merged,
        )?;
        Ok(sources.into_iter().find(|source| source.namespace() == namespace))
    }

    pub fn default_registry(&self) -> &str {
        &self.default_registry
    }

    /// Operator-tier trust policies from the merged `config.toml`, authoritative over the
    /// project `ocx.toml` in `ocx package verify`.
    pub fn config_trust_policies(&self) -> &[ocx_trust::TrustPolicy] {
        &self.config_trust.policy
    }

    /// Operator-tier `[trust.sigstore]` from the merged `config.toml`.
    pub fn config_trust_sigstore(&self) -> Option<&ocx_trust::SigstoreTrust> {
        self.config_trust.sigstore.as_ref()
    }

    /// Hosts this invocation may contact over plain HTTP; take this rather than re-deriving it.
    pub fn insecure_hosts(&self) -> &[String] {
        &self.insecure_hosts
    }

    /// The merged extra-CA view, for clients built outside [`Self::try_init`] (the forge clients).
    pub fn extra_roots_merged(&self) -> &ExtraRoots {
        &self.extra_roots_merged
    }

    /// The local-only extra-CA view: the managed-config fetch client's trust set.
    pub fn extra_roots_local(&self) -> &ExtraRoots {
        &self.extra_roots_local
    }

    /// The recipe every registry client of this invocation is built from, so a command's own
    /// client adds only what differs and cannot drift from [`Self::remote_client`]. Only the
    /// managed-config fetch client uses the local view ([`build_managed_config_client`]).
    #[must_use]
    pub fn client_builder(&self) -> ocx_oci::ClientBuilder {
        client_builder(
            &self.mirror_map,
            &self.progress,
            &self.insecure_hosts,
            &self.extra_roots_merged,
        )
    }

    /// The SSRF exemption `registry` declares in the merged config, or none.
    #[must_use]
    pub fn trusted_hosts_for(&self, registry: &str) -> Vec<String> {
        trusted_hosts_for(&self.config, registry)
    }

    /// A [`Publisher`] on the shared client recipe, SSRF-pinned with exactly `registry`'s exemption.
    ///
    /// The one constructor for `ocx package announce` and `ocx package claim`, which both dial a
    /// repository taken from remote-controlled data; a second recipe could lose the pin.
    #[must_use]
    pub fn guarded_publisher(&self, registry: &str) -> Publisher {
        Publisher::new(
            self.client_builder()
                .ssrf_guard(self.trusted_hosts_for(registry))
                .build(),
        )
    }

    pub fn file_structure(&self) -> &file_structure::FileStructure {
        &self.file_structure
    }

    pub fn api(&self) -> &api::Api {
        &self.api
    }

    pub fn ui(&self) -> &UserInterface {
        &self.ui
    }

    pub fn manager(&self) -> &ocx_package_manager::PackageManager {
        &self.manager
    }

    /// Resolution-affecting policy to forward to subprocesses via [`env::Env::apply_ocx_config`].
    pub fn config_view(&self) -> &env::OcxConfigView {
        &self.config_view
    }

    /// Parallel-pull cap: `--jobs` ▸ `OCX_JOBS` ▸ unbounded.
    pub fn concurrency(&self) -> ocx_package_manager::Concurrency {
        self.concurrency
    }

    /// The fully merged config. Resolving `[managed]` from it skips the required-snapshot gate
    /// `try_init` enforces for ordinary commands.
    pub fn config(&self) -> &ocx_config::Config {
        &self.config
    }

    /// The discovered tiers alone, which a managed payload folds onto; with
    /// [`Self::config_overlay`] it lets `ocx config test` reproduce the adoption order.
    pub fn config_base(&self) -> &ocx_config::Config {
        &self.config_base
    }

    /// The explicit `OCX_CONFIG` / `--config` tier alone, which merges on top of a managed payload.
    pub fn config_overlay(&self) -> &ocx_config::Config {
        &self.config_overlay
    }

    /// The locally-authored `[mirrors]` table, for re-asking [`is_published_namespace`] after init.
    pub fn local_mirrors(&self) -> Option<&std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>> {
        self.local_mirrors.as_ref()
    }

    /// The effective `OCX_MANAGED_CONFIG`, gated by `OCX_NO_CONFIG`, with an empty value unset.
    pub fn managed_config_env_override(&self) -> Option<&str> {
        self.managed_config_env_override.as_deref()
    }

    /// The managed-config snapshot belonging to the current tier; `None` on an identity mismatch
    /// or any I/O or parse failure.
    pub fn managed_config_snapshot(&self) -> Option<&ocx_config::managed_config::ManagedConfigSnapshot> {
        self.managed_config_snapshot.as_ref()
    }

    /// Digest of the patch snapshot in force; `None` when `OCX_PATCH_SNAPSHOT` named nothing.
    pub fn patch_snapshot_digest(&self) -> Option<&ocx_oci::Digest> {
        self.patch_snapshot_digest.as_ref()
    }

    /// The registry client `ocx package verify` reads through, built on first call and present
    /// even under `--offline`.
    ///
    /// Offline blocks only the Sigstore trust services (check [`Self::is_offline`]); the artifact
    /// and its signature referrer still come from the registry.
    pub fn verify_client(&self) -> &ocx_oci::Client {
        self.registry_client_cell.get_or_init(|| self.client_builder().build())
    }
}

/// One namespace's `[registries."<ns>"].trusted_hosts` SSRF exemption, or none.
///
/// Free so [`Context::build_index_sources`], which runs before a `Context` exists, reads it
/// the same way announce and claim do.
fn trusted_hosts_for(config: &ocx_config::Config, registry: &str) -> Vec<String> {
    config
        .registries
        .as_ref()
        .and_then(|registries| registries.get(registry))
        .and_then(|entry| entry.trusted_hosts.clone())
        .unwrap_or_default()
}

/// Every namespace's declared `trusted_hosts`, for the chained index.
///
/// Covers every namespace, not only index-bearing ones: a source-less namespace has nowhere
/// else to carry its exemption.
fn trusted_hosts_by_namespace(config: &ocx_config::Config) -> std::collections::HashMap<String, Vec<String>> {
    let Some(registries) = config.registries.as_ref() else {
        return std::collections::HashMap::new();
    };
    registries
        .iter()
        .filter_map(|(namespace, entry)| {
            let hosts = entry.trusted_hosts.clone()?;
            (!hosts.is_empty()).then(|| (namespace.clone(), hosts))
        })
        .collect()
}

/// Builds the client that fetches the managed-config payload, from the local-only tiers and
/// the raw environment only (`adr_managed_config_tier.md`, "Mirror posture"). The merged
/// config would let a snapshot declaring its own host `insecure` downgrade the next
/// snapshot's fetch to plaintext.
///
/// # Errors
///
/// [`ocx_config::mirror::MirrorConfigError`] when the local `[mirrors]` plus `OCX_MIRRORS` do
/// not resolve, including an `http://` mirror nothing here licenses.
fn build_managed_config_client(
    local_only_config: &ocx_config::Config,
    env_mirrors: Vec<(String, ocx_config::mirror::MirrorConfig)>,
    env_insecure_registries: &[String],
    progress: &ocx_console::progress::ProgressManager,
    extra_roots: &ExtraRoots,
) -> anyhow::Result<ocx_oci::Client> {
    let insecure_hosts = ocx_config::insecure::insecure_hosts(local_only_config, env_insecure_registries);
    let mirrors = ocx_config::mirror::resolve_mirror_map(local_only_config, env_mirrors, &insecure_hosts)
        .map_err(anyhow::Error::new)?;
    // `extra_roots` must be the caller's local view too.
    Ok(client_builder(
        &ocx_oci::MirrorMap::new(mirrors.registry),
        progress,
        &insecure_hosts,
        extra_roots,
    )
    .build())
}

/// The shared `ClientBuilder` recipe; [`Context::client_builder`] wraps it, and init-time
/// callers use it before a `Context` exists.
fn client_builder(
    mirror_map: &ocx_oci::MirrorMap,
    progress: &ocx_console::progress::ProgressManager,
    insecure_hosts: &[String],
    extra_roots: &ExtraRoots,
) -> ocx_oci::ClientBuilder {
    ocx_oci::ClientBuilder::new()
        .plain_http_registries(insecure_hosts.to_vec())
        .mirrors(mirror_map.clone())
        .progress(progress.clone())
        .extra_roots(extra_roots.clone())
}

/// The policy-gated auto-verify input, or `None` with no operator `[[trust.policy]]`.
///
/// Returns `AutoVerifyInput` rather than `AutoVerify` so tests can read the resolved
/// endpoint. Only operator `config.toml` policies gate it; the project pool stays empty.
fn build_auto_verify(
    operator_policies: Vec<ocx_trust::TrustPolicy>,
    sigstore_trust: Option<ocx_trust::SigstoreTrust>,
    registry_client: &ocx_oci::Client,
    offline: bool,
    state: StateStore,
    user_opted_out: bool,
) -> anyhow::Result<Option<ocx_package_manager::AutoVerifyInput>> {
    if operator_policies.is_empty() {
        return Ok(None);
    }
    // Auto-verify keys its trust-root cache by this URL, so a bad URL fails the run: falling
    // back to the default would cache a self-hosted root under the public-good key.
    let rekor = resolve_endpoint(None, sigstore_trust.as_ref(), SigstoreEndpoint::Rekor);
    let rekor_url = ocx_oci::endpoint::validate_sigstore_url(&rekor, "[trust.sigstore].rekor_url")?;
    Ok(Some(ocx_package_manager::AutoVerifyInput {
        operator_policies,
        // ponytail: project `ocx.toml` policies are not read on OCI-tier surfaces yet; wire them
        // here when that follow-up lands.
        project_policies: Vec::new(),
        registry_client: registry_client.clone(),
        rekor_url,
        offline,
        state,
        // Through the same door `ocx package verify` reads it at, so one env value cannot mean a
        // bare path here and a `file://` one there.
        trusted_root_env: std::env::var_os("OCX_SIGSTORE_TRUSTED_ROOT")
            .map(PathBuf::from)
            .map(explicit_trust_root_path),

        sigstore_trust,
        home_trusted_root: ocx_config::loader::ConfigLoader::home_sigstore_trusted_root_path(),
        user_opted_out,
    }))
}

/// Condition 2 of [`is_published_namespace`] without its warning, so [`index_namespaces`]
/// can reuse it silently.
fn index_suppressed_by_local_mirror(
    entry: &ocx_config::RegistryConfig,
    namespace: &str,
    local_mirrors: Option<&std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>>,
) -> bool {
    // Only the REGISTRY role suppresses: the index role is keyed on the base's
    // own host, so `[mirrors."<ns>"] index` redirects nothing for `<ns>`, and
    // suppressing on it would leave neither the index nor the mirror. A
    // bare-string `[mirrors]` entry sets `registry`, so it counts too.
    let locally_pinned_at_a_mirror =
        local_mirrors.is_some_and(|table| table.get(namespace).is_some_and(|entry| entry.registry.is_some()));
    entry.index_is_compiled_default && locally_pinned_at_a_mirror
}

/// Every namespace [`is_published_namespace`] admits, without warning, so ownership holds
/// under `--offline` where no source is built.
fn index_namespaces(
    config: &ocx_config::Config,
    local_mirrors: Option<&std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>>,
) -> std::collections::HashSet<String> {
    config
        .registries
        .iter()
        .flatten()
        .filter(|(namespace, entry)| {
            entry.index.as_deref().is_some_and(|index| !index.is_empty())
                && !index_suppressed_by_local_mirror(entry, namespace, local_mirrors)
        })
        .map(|(namespace, _)| namespace.clone())
        .collect()
}

/// Whether `namespace` resolves through the ocx-index protocol: `index` is non-empty (`""` is
/// the kill switch) and not a compiled-in default a locally-authored `[mirrors]` entry pins at
/// a registry.
///
/// Never the merged tier or `OCX_MIRRORS`, or a managed payload could revoke the verified
/// path fleet-wide. `ocx index regenerate` calls this without built sources, so restating only
/// the first condition there would mint a `c/index.json` the resolver treats as derived.
pub fn is_published_namespace(
    entry: &ocx_config::RegistryConfig,
    namespace: &str,
    local_mirrors: Option<&std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>>,
) -> bool {
    if entry.index.as_deref().is_none_or(str::is_empty) {
        return false;
    }
    if index_suppressed_by_local_mirror(entry, namespace, local_mirrors) {
        // The key can come from the managed tier, so it is sanitized before reaching the terminal.
        let namespace = crate::api::data::sanitize_for_terminal(namespace);
        log::warn!(
            "[mirrors.\"{namespace}\"] pins this namespace at a mirror, so the compiled-in index \
             default for it is suppressed; it resolves as a plain OCI registry through the mirror, \
             without the index's digest verification or yank gate. Declare \
             [registries.\"{namespace}\"] index explicitly to keep the index path."
        );
        return false;
    }
    true
}

/// Resolves `--jobs` ▸ `OCX_JOBS` ▸ unbounded; `0` means the logical-core count, and an
/// invalid env value is warned about and ignored.
fn resolve_concurrency(jobs: Option<usize>) -> ocx_package_manager::Concurrency {
    use std::num::NonZeroUsize;

    let raw = match jobs {
        Some(n) => Some(n),
        None => ocx_util::env::var("OCX_JOBS").and_then(|v| match v.parse::<usize>() {
            Ok(n) => Some(n),
            Err(e) => {
                log::warn!("ignoring invalid OCX_JOBS value {v:?}: {e}");
                None
            }
        }),
    };

    match raw {
        None => ocx_package_manager::Concurrency::Unbounded,
        Some(0) => ocx_package_manager::Concurrency::cores(),
        Some(n) => ocx_package_manager::Concurrency::Limit(NonZeroUsize::new(n).expect("n > 0 covered above")),
    }
}

/// Refuse `--global` alongside an explicit `--project` / `OCX_PROJECT`.
///
/// clap rejects the flag pair; this closes the env-sourced gaps (`OCX_GLOBAL` via the arg
/// default, `OCX_PROJECT` not a clap arg). A CWD-discovered project is not explicit, so
/// `--global` inside a project tree is legal (`adr_global_toolchain_tier.md`).
///
/// # Errors
///
/// [`UsageError`](crate::error::UsageError) (exit `64`) on the conflict.
fn check_global_project_exclusivity(view: &env::OcxConfigView) -> Result<(), crate::error::UsageError> {
    // `OCX_PROJECT=""` means unset to the loader, so it is no explicit selection here.
    let explicit_project =
        view.project.is_some() || ocx_util::env::var(env::keys::OCX_PROJECT).is_some_and(|v| !v.is_empty());
    if view.global && explicit_project {
        return Err(crate::error::UsageError::new(
            "--global cannot be combined with an explicit --project / OCX_PROJECT selection",
        ));
    }
    Ok(())
}

/// Refuse `--frozen` alongside `--remote`, which contradict each other.
///
/// clap rejects the flag pair; this closes the env-sourced gap (`OCX_FROZEN` / `OCX_REMOTE`
/// via the arg defaults). `--frozen` + `--offline` stays allowed: offline wins.
///
/// # Errors
///
/// [`UsageError`](crate::error::UsageError) (exit `64`) when both are set.
fn check_frozen_remote_exclusivity(view: &env::OcxConfigView) -> Result<(), crate::error::UsageError> {
    if view.frozen && view.remote {
        return Err(crate::error::UsageError::new(
            "--frozen cannot be combined with --remote (OCX_FROZEN and OCX_REMOTE)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Spec for the `--global` ⟂ explicit-project exclusivity guard.
    //!
    //! `--global` is a single root-level flag (peer of `--project`); the
    //! `--global` + `--project` *flag* pair is rejected by clap
    //! (`conflicts_with`). [`check_global_project_exclusivity`] closes the
    //! env-sourced gaps clap cannot see (`OCX_GLOBAL` via the arg default,
    //! or `OCX_PROJECT` which is not a clap arg). The `OCX_PROJECT` gap is
    //! exercised end-to-end by `test/tests/test_global_toolchain.py`
    //! (`test_env_global_with_env_project_conflict`); it is not unit-tested
    //! here because `ocx_util::env::var`'s test-override seam is inert when
    //! `ocx_lib` is consumed as a (non-`cfg(test)`) dependency, and real
    //! env mutation is `unsafe` on edition 2024. This test pins the
    //! `--project`-flag path, whose `||` short-circuits before any env read
    //! and is therefore deterministic.

    use super::*;
    use crate::exit::ClassifyExitCode;
    use ocx_exit::ExitCode;

    /// One `[[trust.policy]]`, enough to make `build_auto_verify` return
    /// `Some` — every field is optional at the serde layer.
    fn one_policy() -> Vec<ocx_trust::TrustPolicy> {
        vec![serde_json::from_str("{}").expect("an all-default trust policy parses")]
    }

    /// Call `build_auto_verify` with `sigstore` as the only varying input.
    fn auto_verify_input(
        sigstore: Option<ocx_trust::SigstoreTrust>,
    ) -> anyhow::Result<Option<ocx_package_manager::AutoVerifyInput>> {
        build_auto_verify(
            one_policy(),
            sigstore,
            &ocx_oci::ClientBuilder::new().build(),
            false,
            StateStore::new("/state"),
            false,
        )
    }

    /// Install and pull carry no `--rekor-url`, so `[trust.sigstore].rekor_url`
    /// is the only tier between auto-verify and the builtin default. It used to
    /// read neither: the endpoint was hardcoded.
    ///
    /// The cache-key half is the part that bites. Auto-verify keys its
    /// trust-root cache by the Rekor instance, so an operator on a self-hosted
    /// stack was caching their private root under the public-good key.
    #[test]
    fn auto_verify_takes_its_rekor_endpoint_from_the_sigstore_config() {
        use ocx_sign::verify::trust_cache::cache_key_for_rekor;

        let configured = ocx_trust::SigstoreTrust {
            rekor_url: Some("https://rekor.corp.example".to_string()),
            ..Default::default()
        };
        let from_config = auto_verify_input(Some(configured))
            .expect("a valid config URL must not fail the run")
            .expect("a policy is configured, so auto-verify is on");
        let from_builtin = auto_verify_input(None)
            .expect("the builtin default is valid")
            .expect("a policy is configured, so auto-verify is on");

        assert_eq!(
            from_config.rekor_url.host_str(),
            Some("rekor.corp.example"),
            "config must supply the Rekor endpoint"
        );
        assert_eq!(
            from_builtin.rekor_url.as_str().trim_end_matches('/'),
            ocx_oci::endpoint::DEFAULT_REKOR_URL,
            "with no config, the builtin default still applies"
        );
        assert_ne!(
            cache_key_for_rekor(&from_config.rekor_url),
            cache_key_for_rekor(&from_builtin.rekor_url),
            "the trust-root cache key must follow the endpoint, or a private root \
             is cached under the public-good key"
        );
    }

    /// A typo in `[trust.sigstore].rekor_url` must fail the run: not panic (the
    /// endpoint used to be a compile-time constant and was `.expect()`ed), and
    /// not silently fall back to the public good, which would downgrade an
    /// operator's trust configuration without saying so.
    #[test]
    fn a_rejected_sigstore_rekor_url_fails_the_run_rather_than_falling_back() {
        let hostile = ocx_trust::SigstoreTrust {
            // Plain http off loopback: refused by the same SSRF guard the flag
            // tier hits.
            rekor_url: Some("http://rekor.corp.example".to_string()),
            ..Default::default()
        };
        let Err(err) = auto_verify_input(Some(hostile)) else {
            panic!("a rejected Rekor URL must fail the run");
        };
        assert_eq!(
            crate::exit::classify_error(err.as_ref()),
            ExitCode::UsageError,
            "a rejected endpoint URL exits 64 whichever tier supplied it"
        );
    }

    #[test]
    fn global_with_explicit_project_flag_is_usage_error() {
        let mut view = ocx_config::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        view.global = true;
        view.project = Some(std::path::PathBuf::from("/abs/explicit/ocx.toml"));

        let err = check_global_project_exclusivity(&view)
            .expect_err("--global + explicit --project must be rejected (ADR §Decision 2)");
        assert_eq!(
            err.classify(),
            Some(ExitCode::UsageError),
            "the conflict must classify to ExitCode::UsageError (64)"
        );
        assert_eq!(
            ExitCode::UsageError as u8,
            64,
            "UsageError must be sysexits EX_USAGE (64)"
        );
        assert!(
            err.to_string().contains("--global"),
            "conflict message must name --global so users can grep stderr; got: {err}"
        );
    }

    #[test]
    fn frozen_with_remote_is_usage_error() {
        // clap rejects the `--frozen` + `--remote` flag pair; this guard closes
        // the env-sourced gap (OCX_FROZEN + OCX_REMOTE both via the arg
        // defaults). The conflict must classify to UsageError (64).
        let mut view = ocx_config::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        view.frozen = true;
        view.remote = true;

        let err = check_frozen_remote_exclusivity(&view).expect_err("--frozen + --remote must be rejected");
        assert_eq!(
            err.classify(),
            Some(ExitCode::UsageError),
            "the conflict must classify to ExitCode::UsageError (64)"
        );
        assert!(
            err.to_string().contains("--frozen"),
            "conflict message must name --frozen so users can grep stderr; got: {err}"
        );
    }

    #[test]
    fn frozen_without_remote_is_ok() {
        // Frozen alone (and frozen+offline, which collapses to offline upstream)
        // is a valid combination — the guard only rejects frozen+remote.
        let mut view = ocx_config::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        view.frozen = true;
        assert!(
            check_frozen_remote_exclusivity(&view).is_ok(),
            "--frozen without --remote must be accepted"
        );
    }

    // ── `registries."<ns>".index` presence gates `OcxIndex` construction,
    //    per NAMESPACE (`adr_index_indirection.md`) ─────────────────────────
    //
    // `build_index_sources` constructs one `OcxIndex` per merged
    // `[registries."<ns>"]` entry that carries a non-empty `index` field —
    // NOT just `ocx.sh` (the earlier hard-coding). A namespace configured as
    // index-kind resolves through its own two-hop source; a namespace with no
    // `index` field gets no source and `chain_mode_and_sources` chains the
    // registry alone for it, so an outage on an unconfigured index endpoint
    // can never hard-block a plain-OCI namespace.

    /// Builds a `Config` with the given `(namespace, index)` registry
    /// entries.
    fn config_with_registries(entries: &[(&str, Option<&str>)]) -> ocx_config::Config {
        let mut registries = std::collections::HashMap::new();
        for (namespace, index) in entries {
            registries.insert(
                namespace.to_string(),
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

    fn source_namespaces(sources: &[ocx_index::OcxIndex]) -> Vec<String> {
        sources.iter().map(|source| source.namespace().to_string()).collect()
    }

    /// Calls [`Context::build_index_sources`] with the physical-client inputs the
    /// wiring tests don't vary (an empty registry mirror map, no plain-HTTP hosts,
    /// disabled progress). `online` and the two config maps are what these tests
    /// exercise.
    fn build_test_sources(
        online: bool,
        config: &ocx_config::Config,
        mirrors_index: &std::collections::BTreeMap<String, ocx_oci::client::mirror_map::ParsedMirror>,
    ) -> anyhow::Result<Vec<ocx_index::OcxIndex>> {
        Context::build_index_sources(
            online,
            config,
            None,
            mirrors_index,
            &ocx_oci::MirrorMap::default(),
            &[],
            &ocx_console::progress::ProgressManager::disabled(),
            &ExtraRoots::default(),
        )
    }

    #[test]
    fn build_index_sources_is_empty_without_an_index_bearing_registry() {
        // No `[registries]` table, and an entry with no `index` field at all,
        // both yield no index sources — presence of `index` specifically is
        // the sole selector.
        let mirrors = std::collections::BTreeMap::new();

        let empty = build_test_sources(true, &ocx_config::Config::default(), &mirrors).unwrap();
        assert!(empty.is_empty(), "no [registries] table must build no index sources");

        let index_absent = config_with_registries(&[(ocx_oci::OCX_SH_REGISTRY, None)]);
        let built = build_test_sources(true, &index_absent, &mirrors).unwrap();
        assert!(
            built.is_empty(),
            "a registries entry lacking `index` must not build an index source"
        );
    }

    #[test]
    fn build_index_sources_is_empty_when_offline() {
        // Offline is modelled as no remote client; without a physical fetch
        // client there is nothing to build an index source's leaf fetches on.
        let config = config_with_registries(&[(ocx_oci::OCX_SH_REGISTRY, Some("https://index.ocx.sh"))]);
        let built = build_test_sources(false, &config, &std::collections::BTreeMap::new()).unwrap();
        assert!(
            built.is_empty(),
            "offline (no remote client) must build no index sources"
        );
    }

    #[test]
    fn build_index_sources_builds_one_per_index_bearing_namespace() {
        // Two namespaces configured as index-kind (including a non-ocx.sh one)
        // plus one plain-OCI namespace ⇒ exactly two sources, keyed by their
        // own namespaces, in deterministic (sorted) order. This is the fix: a
        // `[registries."<other-ns>"] index` entry is no longer silently ignored.
        let config = config_with_registries(&[
            (ocx_oci::OCX_SH_REGISTRY, Some("https://index.ocx.sh")),
            ("corp.example", Some("https://index.corp.example")),
            ("plain.example", None),
        ]);

        let sources = build_test_sources(true, &config, &std::collections::BTreeMap::new()).unwrap();

        assert_eq!(
            source_namespaces(&sources),
            vec!["corp.example".to_string(), ocx_oci::OCX_SH_REGISTRY.to_string()],
            "one index source per index-bearing namespace, sorted, and never for a plain-OCI entry"
        );
    }

    #[test]
    fn build_index_sources_never_leaks_trusted_hosts_across_namespaces() {
        // Two index-bearing namespaces, each with its OWN, DIFFERENT
        // `trusted_hosts` set (the SSRF escape hatch). Pins that a built
        // `OcxIndex` carries exactly its own namespace's set — never the
        // other namespace's, and never the union — so a future "share one
        // client across namespaces" refactor cannot silently widen one
        // namespace's trust exemption into another's.
        let mut registries = std::collections::HashMap::new();
        registries.insert(
            "ns-a".to_string(),
            ocx_config::RegistryConfig {
                index: Some("https://index.a.example".to_string()),
                trusted_hosts: Some(vec!["10.0.0.0/8".to_string()]),
                ..Default::default()
            },
        );
        registries.insert(
            "ns-b".to_string(),
            ocx_config::RegistryConfig {
                index: Some("https://index.b.example".to_string()),
                trusted_hosts: Some(vec!["192.168.0.0/16".to_string()]),
                ..Default::default()
            },
        );
        let config = ocx_config::Config {
            registries: Some(registries),
            ..Default::default()
        };

        let sources = build_test_sources(true, &config, &std::collections::BTreeMap::new()).unwrap();

        assert_eq!(
            source_namespaces(&sources),
            vec!["ns-a".to_string(), "ns-b".to_string()],
            "one index source per index-bearing namespace, sorted"
        );

        let ns_a = sources
            .iter()
            .find(|source| source.namespace() == "ns-a")
            .expect("ns-a source must be built");
        let ns_b = sources
            .iter()
            .find(|source| source.namespace() == "ns-b")
            .expect("ns-b source must be built");

        assert_eq!(
            ns_a.trusted_hosts(),
            ["10.0.0.0/8".to_string()],
            "ns-a must carry only its own trusted_hosts entry"
        );
        assert_eq!(
            ns_b.trusted_hosts(),
            ["192.168.0.0/16".to_string()],
            "ns-b must carry only its own trusted_hosts entry"
        );
        assert_ne!(
            ns_a.trusted_hosts(),
            ["10.0.0.0/8".to_string(), "192.168.0.0/16".to_string()].as_slice(),
            "ns-a's trusted_hosts must never be the union with ns-b's"
        );
        assert_ne!(
            ns_b.trusted_hosts(),
            ["10.0.0.0/8".to_string(), "192.168.0.0/16".to_string()].as_slice(),
            "ns-b's trusted_hosts must never be the union with ns-a's"
        );
    }

    /// At the index seam: the per-namespace physical-fetch
    /// client `build_index_sources` stores on each `OcxIndex` — the one
    /// `fetch_manifest` / `fetch_blob` pull a resolved package through —
    /// carries the merged extra-CA view, not only the index HTTP transport
    /// beside it. `ocx index update` reaches the transport alone, so an
    /// `install` of an index-resolved package from a corp-CA registry is the
    /// dial this pins.
    ///
    /// Mutation: drop `.extra_roots(..)` from that builder — the count reads
    /// 0 and this reds.
    #[test]
    fn build_index_sources_threads_the_merged_extra_roots_into_the_physical_client() {
        let roots = ExtraRoots::from_pem(EXTRA_CA_PEM.as_bytes()).expect("the fixture root parses");
        let config = config_with_registries(&[("corp.example", Some("https://index.corp.example"))]);

        let sources = Context::build_index_sources(
            true,
            &config,
            None,
            &std::collections::BTreeMap::new(),
            &ocx_oci::MirrorMap::default(),
            &[],
            &ocx_console::progress::ProgressManager::disabled(),
            &roots,
        )
        .unwrap();

        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0].client().extra_root_count(),
            roots.len(),
            "the physical-fetch client must carry every root of the merged view"
        );
        assert_eq!(roots.len(), 1, "positive control: the view is not empty");
    }

    #[test]
    fn trusted_hosts_map_covers_every_declaring_namespace_including_plain_oci_ones() {
        // The map the local index carries so the SSRF floor can judge a
        // LOCALLY-minted physical target (ocx#218). It is keyed per namespace
        // and — unlike `build_index_sources` — is NOT restricted to
        // index-bearing namespaces: an operator who declared where a
        // namespace's traffic may go has answered the question for it whether
        // or not it also declares an `index`, and such a namespace has no
        // source to carry the exemption for it.
        let mut registries = std::collections::HashMap::new();
        registries.insert(
            "indexed.example".to_string(),
            ocx_config::RegistryConfig {
                index: Some("https://index.indexed.example".to_string()),
                trusted_hosts: Some(vec!["10.0.0.0/8".to_string()]),
                ..Default::default()
            },
        );
        registries.insert(
            "plain.example".to_string(),
            ocx_config::RegistryConfig {
                trusted_hosts: Some(vec!["192.168.0.0/16".to_string()]),
                ..Default::default()
            },
        );
        registries.insert("silent.example".to_string(), ocx_config::RegistryConfig::default());
        let config = ocx_config::Config {
            registries: Some(registries),
            ..Default::default()
        };

        let map = trusted_hosts_by_namespace(&config);

        assert_eq!(
            map.get("indexed.example").map(Vec::as_slice),
            Some(["10.0.0.0/8".to_string()].as_slice())
        );
        assert_eq!(
            map.get("plain.example").map(Vec::as_slice),
            Some(["192.168.0.0/16".to_string()].as_slice()),
            "a namespace with no `index` still gets its declared exemption"
        );
        assert!(
            !map.contains_key("silent.example"),
            "a namespace declaring no trusted_hosts must get no entry, so the floor guards it"
        );
        assert!(
            trusted_hosts_by_namespace(&ocx_config::Config::default()).is_empty(),
            "no [registries] table means no exemptions anywhere"
        );
    }

    // ── `build_index_sources` threads `mirrors_index` ─────────────────────────
    //
    // `build_index_sources` hands its `mirrors_index` and `insecure_hosts` parameters straight
    // through to `OcxIndex::resolve_base_url` per namespace — the same
    // parameters, unmodified, not an empty stand-in. `OcxIndex` exposes no
    // base-URL accessor (by design — see `ocx_index.rs`'s own
    // `resolve_base_url_applies_mirrors_index_role_override` for the unit
    // level), so these observe the wiring the only way available without
    // adding one: the plain-HTTP gate's success/failure, which only flips if
    // the override actually reached `resolve_base_url`.

    #[test]
    fn build_index_sources_reflects_the_mirrors_index_override_for_its_own_host() {
        // The registries entry alone is safe (https) and would never gate.
        // The mirrors_index override rewrites the SAME traffic host to a
        // DIFFERENT, plain-http physical host. If `build_index_sources`
        // dropped `mirrors_index` en route (e.g. passed an empty map
        // instead), this would resolve the untouched https base and succeed;
        // instead it must fail, and the gate error must name the OVERRIDE's
        // host — proving the built source's resolution used the mirror, not
        // the original `[registries] index` value.
        let config = config_with_registries(&[("ns", Some("https://index.example"))]);
        let mut mirrors_index = std::collections::BTreeMap::new();
        mirrors_index.insert(
            "index.example".to_string(),
            ocx_config::mirror::parse_url("http://mirror.example").unwrap(),
        );

        // `OcxIndex` carries no `Debug` impl (only `Clone`), so `expect_err`
        // is unavailable here — match explicitly instead.
        let error = match build_test_sources(true, &config, &mirrors_index) {
            Err(error) => error,
            Ok(_) => panic!(
                "a mirrors_index override to a non-allowlisted http host must gate, proving the override reached resolution"
            ),
        };

        assert!(
            error.to_string().contains("mirror.example"),
            "expected the gate to name the override's host (mirror.example), not the original (index.example); got: {error}"
        );
    }

    #[test]
    fn build_index_sources_ignores_a_mirrors_index_entry_keyed_by_an_unrelated_host() {
        // The mirrors_index entry is keyed by a host that is NOT this
        // namespace's traffic host — host-keyed precision, proven at the
        // wiring level (not just inside `resolve_base_url`, already covered
        // by `resolve_base_url_applies_mirrors_index_role_override` in
        // ocx_index.rs). Were the unrelated entry to leak into this
        // namespace's resolution, the plain-http gate below would fire since
        // its target is also http and unlisted; instead `build_index_sources`
        // must succeed, keeping the original https base untouched.
        let config = config_with_registries(&[("ns", Some("https://index.example"))]);
        let mut mirrors_index = std::collections::BTreeMap::new();
        mirrors_index.insert(
            "unrelated.example".to_string(),
            ocx_config::mirror::parse_url("http://unrelated.example").unwrap(),
        );

        let sources = build_test_sources(true, &config, &mirrors_index)
            .expect("a mirrors_index entry keyed by an unrelated host must not affect this namespace's resolution");

        assert_eq!(
            source_namespaces(&sources),
            vec!["ns".to_string()],
            "the unrelated-host override must not block or otherwise affect the \"ns\" source"
        );
    }

    #[test]
    fn build_index_sources_surfaces_the_plain_http_gate_error_through_the_wiring() {
        // Same override `OcxIndex::resolve_base_url`'s own
        // `resolve_base_url_gates_plain_http_target` unit test exercises
        // directly — replicated here through `build_index_sources` to prove
        // the gate error propagates all the way out of the wiring call, not
        // only inside the unit-tested function in isolation.
        let config = config_with_registries(&[("ns", Some("https://index.example"))]);
        let mut mirrors_index = std::collections::BTreeMap::new();
        mirrors_index.insert(
            "index.example".to_string(),
            ocx_config::mirror::parse_url("http://index.example").unwrap(),
        );

        // Ground truth: the exact call `build_index_sources` makes internally.
        let direct = ocx_index::OcxIndex::resolve_base_url(&config, "ns", &mirrors_index, &[], &ExtraRoots::default())
            .expect_err("ground truth: resolve_base_url itself must gate this http override");

        // `OcxIndex` carries no `Debug` impl (only `Clone`), so `expect_err`
        // is unavailable here — match explicitly instead.
        let wired = match build_test_sources(true, &config, &mirrors_index) {
            Err(error) => error,
            Ok(_) => panic!("build_index_sources must propagate the same gate error, not silently succeed"),
        };

        assert_eq!(
            direct.to_string(),
            wired.to_string(),
            "build_index_sources's error must match the direct resolve_base_url call"
        );
    }

    // ── The two off-switches for an index-bearing namespace ──────────────────

    /// `index = ""` is the documented kill switch, and THIS filter is what
    /// implements it — the loader only carries the empty string through as a
    /// declared value. Without this test, simplifying the filter to
    /// `entry.index.is_some()` leaves the whole Rust suite green while every
    /// `ocx.sh` resolution silently goes back through `index.ocx.sh`.
    #[test]
    fn build_index_sources_skips_an_empty_index_value() {
        let config = config_with_registries(&[(ocx_oci::OCX_SH_REGISTRY, Some(""))]);
        let built = build_test_sources(true, &config, &std::collections::BTreeMap::new()).unwrap();
        assert!(
            built.is_empty(),
            "index = \"\" must build no index source — an empty base URL is not a kind marker"
        );
    }

    /// Builds a `Config` whose single `ocx.sh` entry carries the compiled-in
    /// index exactly as `ConfigLoader::builtin_defaults` stamps it.
    fn config_with_compiled_default() -> ocx_config::Config {
        let mut registries = std::collections::HashMap::new();
        registries.insert(
            ocx_oci::OCX_SH_REGISTRY.to_string(),
            ocx_config::RegistryConfig {
                index: Some("https://index.ocx.sh".to_string()),
                index_is_compiled_default: true,
                ..Default::default()
            },
        );
        ocx_config::Config {
            registries: Some(registries),
            ..Default::default()
        }
    }

    /// A `[mirrors]` table as a local config file would parse it. `registry`
    /// carries the role a bare-string entry sets; `index` the table-form role.
    fn local_mirror_table(
        host: &str,
        registry: Option<&str>,
        index: Option<&str>,
    ) -> std::collections::HashMap<String, ocx_config::mirror::MirrorConfig> {
        let mut table = std::collections::HashMap::new();
        table.insert(
            host.to_string(),
            ocx_config::mirror::MirrorConfig {
                registry: registry.map(str::to_string),
                index: index.map(str::to_string),
                ..Default::default()
            },
        );
        table
    }

    /// Builds sources with a mirror map that is present in the MERGED views
    /// (what the OCI client and index-role resolver see) but attributed to a
    /// caller-chosen local view — the seam the locally-authored-only guard
    /// turns on.
    fn build_with_views(
        config: &ocx_config::Config,
        local_mirrors: Option<&std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>>,
        merged_registry_mirror: Option<(&str, &str)>,
        merged_index_mirror: Option<(&str, &str)>,
    ) -> Vec<ocx_index::OcxIndex> {
        let registry_mirrors = merged_registry_mirror.map_or_else(ocx_oci::MirrorMap::default, |(host, url)| {
            ocx_oci::MirrorMap::new([(host.to_string(), ocx_config::mirror::parse_url(url).unwrap())])
        });
        let mut mirrors_index = std::collections::BTreeMap::new();
        if let Some((host, url)) = merged_index_mirror {
            mirrors_index.insert(host.to_string(), ocx_config::mirror::parse_url(url).unwrap());
        }
        Context::build_index_sources(
            true,
            config,
            local_mirrors,
            &mirrors_index,
            &registry_mirrors,
            &[],
            &ocx_console::progress::ProgressManager::disabled(),
            &ExtraRoots::default(),
        )
        .unwrap()
    }

    /// A locally-authored registry-role `[mirrors."ocx.sh"]` entry suppresses
    /// the compiled-in index. The scenario: a firewalled site pins `ocx.sh` at
    /// its own artifact server. `[mirrors]` is applied against the PHYSICAL
    /// identifier the index mints, so it does not cover `index.ocx.sh` — and
    /// silently adding a host the operator never allow-listed is exactly the
    /// egress they configured `[mirrors]` to prevent.
    #[test]
    fn a_local_registry_role_mirror_suppresses_the_compiled_in_index() {
        let built = build_with_views(
            &config_with_compiled_default(),
            Some(&local_mirror_table(
                ocx_oci::OCX_SH_REGISTRY,
                Some("https://artifactory.corp/ocx-remote"),
                None,
            )),
            Some((ocx_oci::OCX_SH_REGISTRY, "https://artifactory.corp/ocx-remote")),
            None,
        );
        assert!(
            built.is_empty(),
            "a local [mirrors.\"ocx.sh\"] registry-role entry must suppress the compiled-in index"
        );
    }

    /// **The attack this guards against.** The managed tier is a remote, operator-published
    /// payload the loader itself calls untrusted; it merges as a full `Config`,
    /// `[mirrors]` included. Whoever controls that package must not be able to
    /// revoke the sha256-verified two-hop path and the yank gate fleet-wide and
    /// take every `ocx.sh` request.
    ///
    /// This models the tier, not just "an entry exists": the mirror is present
    /// in BOTH merged views — exactly as a managed `[mirrors]` entry arrives —
    /// and absent only from the local-only view. A trigger keyed on the merged
    /// maps passes every other test here and fails this one.
    #[test]
    fn a_managed_tier_mirror_cannot_suppress_the_compiled_in_index() {
        let built = build_with_views(
            &config_with_compiled_default(),
            // The operator shipped no config of their own — the whole point.
            None,
            Some((ocx_oci::OCX_SH_REGISTRY, "https://attacker.example/ocx")),
            Some((ocx_oci::OCX_SH_REGISTRY, "https://attacker.example/ocx")),
        );
        assert_eq!(
            source_namespaces(&built),
            vec![ocx_oci::OCX_SH_REGISTRY.to_string()],
            "a mirror from the untrusted managed tier must not revoke the verified index path"
        );
    }

    /// The index role is only ever applied keyed on the index base's
    /// OWN host (`OcxIndex::resolve_base_url`), never on the namespace, so
    /// `[mirrors."ocx.sh"] index` cannot redirect anything for `ocx.sh`.
    /// Suppressing on it would strand the operator with neither the index nor
    /// their corp endpoint — `ocx.sh` would egress direct as plain OCI with no
    /// registry-role mirror to catch it.
    #[test]
    fn a_namespace_keyed_index_role_mirror_does_not_suppress() {
        let built = build_with_views(
            &config_with_compiled_default(),
            Some(&local_mirror_table(
                ocx_oci::OCX_SH_REGISTRY,
                None,
                Some("https://corp-index.example"),
            )),
            None,
            Some((ocx_oci::OCX_SH_REGISTRY, "https://corp-index.example")),
        );
        assert_eq!(
            source_namespaces(&built),
            vec![ocx_oci::OCX_SH_REGISTRY.to_string()],
            "an entry that can never redirect the namespace must not suppress its index"
        );
    }

    /// An `index` a config file wrote is NOT compiled-default provenance, so
    /// the mirror entry does not suppress it — the documented way to keep both
    /// a mirror and the index path. Same local mirror as the suppression test
    /// above; only the provenance flag differs.
    #[test]
    fn an_explicit_index_survives_a_mirror_entry_for_the_same_namespace() {
        let built = build_with_views(
            &config_with_registries(&[(ocx_oci::OCX_SH_REGISTRY, Some("https://index.ocx.sh"))]),
            Some(&local_mirror_table(
                ocx_oci::OCX_SH_REGISTRY,
                Some("https://artifactory.corp/ocx-remote"),
                None,
            )),
            Some((ocx_oci::OCX_SH_REGISTRY, "https://artifactory.corp/ocx-remote")),
            None,
        );
        assert_eq!(
            source_namespaces(&built),
            vec![ocx_oci::OCX_SH_REGISTRY.to_string()],
            "a written [registries.\"ocx.sh\"] index outranks the mirror suppression"
        );
    }

    /// Host-keyed precision: the mirror entry that routes the INDEX's own
    /// traffic host (`index.ocx.sh`) redirects the index
    /// rather than replacing it — that operator gets the verified path against
    /// their own host, so suppressing would delete what they asked for.
    #[test]
    fn a_mirror_keyed_by_the_index_host_does_not_suppress_the_compiled_in_index() {
        let built = build_with_views(
            &config_with_compiled_default(),
            Some(&local_mirror_table(
                "index.ocx.sh",
                None,
                Some("https://artifactory.corp/ocx-index"),
            )),
            None,
            Some(("index.ocx.sh", "https://artifactory.corp/ocx-index")),
        );
        assert_eq!(
            source_namespaces(&built),
            vec![ocx_oci::OCX_SH_REGISTRY.to_string()],
            "a [mirrors.\"index.ocx.sh\"] entry redirects the index, it does not suppress it"
        );
    }

    #[test]
    fn chain_mode_and_sources_chains_every_index_source_before_the_registry() {
        // Two index sources ⇒ the chain is [source, source, registry]: each
        // index source is registered ahead of the plain-OCI registry so a
        // logical reference in its namespace resolves through the verified
        // two-hop path, and `jurisdiction` stops fall-through so
        // exactly one remote resolves each namespace (Decision H).
        let config = config_with_registries(&[
            (ocx_oci::OCX_SH_REGISTRY, Some("https://index.ocx.sh")),
            ("corp.example", Some("https://index.corp.example")),
        ]);
        let index_sources = build_test_sources(true, &config, &std::collections::BTreeMap::new()).unwrap();
        let oci_index = ocx_index::OciIndex::new(ocx_index::OciIndexConfig {
            client: ocx_oci::ClientBuilder::new().build(),
        });

        let (mode, sources) =
            Context::chain_mode_and_sources(Some(&oci_index), &index_sources, ocx_index::ChainMode::Default);

        assert_eq!(mode, ocx_index::ChainMode::Default);
        assert_eq!(
            sources.len(),
            3,
            "two index sources must chain ahead of the single registry source"
        );
    }

    #[test]
    fn chain_mode_and_sources_chains_the_registry_alone_when_no_index_sources() {
        // With no index-kind namespace, the chain carries exactly one source —
        // the OCI registry — never a second, absent-but-implied index source.
        let client = ocx_oci::ClientBuilder::new().build();
        let oci_index = ocx_index::OciIndex::new(ocx_index::OciIndexConfig { client });

        let (mode, sources) = Context::chain_mode_and_sources(Some(&oci_index), &[], ocx_index::ChainMode::Default);

        assert_eq!(mode, ocx_index::ChainMode::Default);
        assert_eq!(
            sources.len(),
            1,
            "no index sources must chain the registry alone, resolving via OciIndex only"
        );
    }

    #[test]
    fn frozen_and_offline_together_produces_offline_chain_mode() {
        // `--frozen --offline` is a valid combination: the guard accepts it, and
        // the mode-selection logic collapses it to `ChainMode::Offline` (the
        // stronger constraint). The key invariant: when `offline=true` the
        // `oci_index` is `None`, and the `match &oci_index` arm for `None`
        // always emits `ChainMode::Offline` regardless of the `frozen` flag.
        // This mirrors the precedence comment in `try_init`:
        // "offline already won via the `None` arm — it produced no oci_index".
        let mut view = ocx_config::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        view.frozen = true;
        // offline=true → oci_index=None; the guard must accept the combination.
        assert!(
            check_frozen_remote_exclusivity(&view).is_ok(),
            "--frozen + --offline must pass the exclusivity guard"
        );

        // Replicate the mode-selection match from try_init:
        // offline=true produces oci_index=None → Offline wins, ignoring frozen.
        let oci_index: Option<ocx_index::OciIndex> = None; // simulates offline=true
        let frozen = true;
        let mode: ocx_index::ChainMode = match &oci_index {
            None => ocx_index::ChainMode::Offline,
            Some(_) => {
                if frozen {
                    ocx_index::ChainMode::Frozen
                } else {
                    ocx_index::ChainMode::Default
                }
            }
        };
        assert_eq!(
            mode,
            ocx_index::ChainMode::Offline,
            "offline (oci_index=None) must produce ChainMode::Offline even when frozen=true"
        );
    }

    /// The managed-config FETCH client must not take its plain-HTTP allowance
    /// from the payload it is about to fetch.
    ///
    /// Asserted as a pair, because a one-sided refusal would also hold if the
    /// mirror gate were simply broken: the SAME mirror, the SAME env, refused
    /// against the local-only view and allowed against the merged one. That is
    /// what makes the refusal a property of *which config was consulted* — and
    /// the merged view is exactly what the process-wide `insecure_hosts` is
    /// built from, so it is the mistake this guards against, spelled out.
    #[test]
    fn the_managed_fetch_client_ignores_a_plain_http_allowance_the_payload_declared() {
        let host = "mirror.corp:5000";
        let mirrors = || {
            vec![(
                "ghcr.io".to_string(),
                ocx_config::mirror::MirrorConfig {
                    registry: Some(format!("http://{host}")),
                    ..Default::default()
                },
            )]
        };
        let with_allowance = |granted: bool| {
            let entry = ocx_config::RegistryConfig {
                insecure: granted.then_some(true),
                ..Default::default()
            };
            ocx_config::Config {
                registries: Some(std::collections::HashMap::from([(host.to_string(), entry)])),
                ..Default::default()
            }
        };
        let progress = ocx_console::progress::ProgressManager::disabled();

        let refused = build_managed_config_client(
            &with_allowance(false),
            mirrors(),
            &[],
            &progress,
            &ExtraRoots::default(),
        );
        assert!(
            refused.is_err(),
            "the payload's own allowance is not in scope for the tier that fetches it"
        );

        let allowed =
            build_managed_config_client(&with_allowance(true), mirrors(), &[], &progress, &ExtraRoots::default());
        assert!(
            allowed.is_ok(),
            "the same mirror IS allowed once the view actually handed to the builder grants it: {:?}",
            allowed.err()
        );
    }

    // ── extra CA roots: the tiered view ladder ───────────────────────────────

    /// A real self-signed CA (the test stack's Fulcio root, `basicConstraints
    /// CA:TRUE`) — the parser runs a real X.509 parse and a probe build, so
    /// only real material passes it.
    const EXTRA_CA_PEM: &str = include_str!("../../../../test/sigstore/keys/fulcio-ca.crt.pem");

    fn config_with_pem(pem: &str) -> ocx_config::Config {
        ocx_config::Config {
            extra_ca_certs_pem: Some(pem.to_string()),
            ..Default::default()
        }
    }

    /// The managed-config fetch client is built from
    /// [`Context::extra_roots_local`], never [`Context::extra_roots_merged`] —
    /// the refresh channel must not be secured by material it just delivered.
    ///
    /// Modelled as the tier, not "a key exists": a managed payload's
    /// `extra_ca_certs_pem` is present in the MERGED view and absent from the
    /// local-only one, and the two views resolve differently — `merged` carries
    /// the payload's root, `local` carries nothing. `build_managed_config_client`
    /// is then handed the local set; the pair is what makes "which view was
    /// consulted" observable rather than assumed.
    #[test]
    fn extra_ca_managed_config_client_gets_local_view_roots() {
        let merged_config = config_with_pem(EXTRA_CA_PEM);
        let local_config = ocx_config::Config::default();

        let merged = resolve_extra_roots(&merged_config, None, Some(ocx_config::ConfigTier::Home))
            .expect("the managed payload's root parses");
        let local = resolve_extra_roots(&local_config, None, Some(ocx_config::ConfigTier::Home))
            .expect("no local key resolves to the empty set");
        assert_eq!(merged.len(), 1, "the managed payload's root is in the merged view");
        assert!(local.is_empty(), "the managed payload's root is NOT in the local view");

        let progress = ocx_console::progress::ProgressManager::disabled();
        let client = build_managed_config_client(&local_config, Vec::new(), &[], &progress, &local);
        assert!(
            client.is_ok(),
            "the managed fetch client builds from the local set: {:?}",
            client.err()
        );
    }

    /// `Context::try_init` over a fixture `$OCX_HOME` (`ocx index catalog`,
    /// `--offline` unless `online`), the `toolchain_env.rs` precedent. Nothing
    /// dials at init either way — every client is built lazily. One call per
    /// test: the log subscriber and the process-wide Sigstore root install
    /// are both first-wins, which nextest's one-process-per-test honours.
    async fn context_over_home(home: &Path, online: bool) -> Context {
        use clap::Parser as _;

        use crate::app::{Cli, ManagedConfigGate};

        // SAFETY: `OCX_HOME` and the four config-shaping variables are read
        // through `ocx_util::env::var`, whose `#[cfg(test)]` override seam is
        // internal to `ocx_lib` and unavailable from this crate; the process
        // environment is the only seam. nextest runs one test per process, so
        // this cannot race a sibling.
        unsafe {
            std::env::set_var("OCX_HOME", home);
            for key in [
                "OCX_EXTRA_CA_CERTS",
                "OCX_CONFIG",
                "OCX_NO_CONFIG",
                "OCX_MANAGED_CONFIG",
            ] {
                std::env::remove_var(key);
            }
        }
        let argv: &[&str] = if online {
            &["ocx", "index", "catalog"]
        } else {
            &["ocx", "--offline", "index", "catalog"]
        };
        let cli = Cli::parse_from(argv);
        let context = Context::try_init(
            &cli.context,
            ocx_console::ColorModeConfig {
                stdout: false,
                stderr: false,
                relayed: false,
            },
            ManagedConfigGate {
                enforce_required: false,
                onboarding: false,
            },
        )
        .await
        .expect("a context over the fixture home");
        assert_eq!(
            context.file_structure().root(),
            home,
            "the context must resolve the tempdir as its home, or this reads someone else's config"
        );
        context
    }

    /// A managed-config snapshot on disk at `source` carrying `payload`,
    /// shaped as `persist_managed_config` writes it (`snapshot.json` plus the
    /// sibling `config.toml`), so the loader's identity gate folds it.
    fn write_managed_snapshot(home: &Path, source: &str, payload: &str) {
        let path = ocx_config::managed_config::ManagedConfigPaths::for_ocx_home(home).snapshot_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let snapshot = serde_json::json!({
            "source": source,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "fetched_at": "2026-09-14T00:00:00Z",
        });
        std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        std::fs::write(
            ocx_config::managed_config::ManagedConfigPaths::toml_beside_snapshot(&path),
            payload,
        )
        .unwrap();
    }

    /// `$OCX_HOME` seeded with an UNPINNED `[managed]` source whose on-disk
    /// payload sets `extra_ca_certs_pem` — the one fixture where the merged
    /// and local-only views differ.
    fn home_with_unpinned_managed_root() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[managed]\nsource = \"registry.test/managed-config:v1\"\nrequired = false\n",
        )
        .unwrap();
        write_managed_snapshot(
            home.path(),
            "registry.test/managed-config:v1",
            &format!("extra_ca_certs_pem = '''\n{EXTRA_CA_PEM}'''\n"),
        );
        home
    }

    /// End to end: an UNPINNED managed payload's `extra_ca_certs_pem`
    /// reaches the merged view only — the local view (the managed-fetch
    /// client's) stays empty, and so does the set `try_init` installs
    /// process-wide for the Sigstore client (the fold's `local` arm, not the
    /// merged view).
    #[tokio::test]
    async fn extra_ca_try_init_keeps_an_unpinned_managed_root_out_of_the_local_and_sigstore_views() {
        let home = home_with_unpinned_managed_root();
        let context = context_over_home(home.path(), false).await;

        assert_eq!(
            context.extra_roots_merged().len(),
            1,
            "the payload's root is in the merged view"
        );
        assert!(
            context.extra_roots_local().is_empty(),
            "the payload's root must never reach the local view"
        );
        assert!(
            ocx_util::tls::sigstore_roots().is_empty(),
            "an unpinned managed root must not reach the Sigstore view"
        );
        // merged = 1 / local = 0 is what makes the recipe's view observable:
        // swapping `extra_roots_merged` for `extra_roots_local` or a default
        // in `Context::client_builder` reds here, and `verify_client` is the
        // one consumer that builds through it under `--offline`.
        assert_eq!(
            context.verify_client().extra_root_count(),
            1,
            "Context::client_builder pre-applies the merged view"
        );
    }

    /// End to end: a `$OCX_HOME/config.toml` `extra_ca_certs_pem`
    /// resolves into all three views, and `try_init` installs it process-wide
    /// for the Sigstore client — an install that never ran reads as empty here.
    #[tokio::test]
    async fn extra_ca_try_init_installs_a_home_tier_root_for_sigstore() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            format!("extra_ca_certs_pem = '''\n{EXTRA_CA_PEM}'''\n"),
        )
        .unwrap();

        let context = context_over_home(home.path(), false).await;

        assert_eq!(context.extra_roots_merged().len(), 1);
        assert_eq!(context.extra_roots_local().len(), 1);
        assert_eq!(
            ocx_util::tls::sigstore_roots().der(),
            context.extra_roots_merged().der(),
            "try_init must install the Sigstore view before any client is built"
        );
    }

    /// At the client boundary: the managed-config fetch client
    /// `try_init` builds carries the LOCAL view's roots (none — the payload's
    /// root is managed material), while the registry client built beside it
    /// carries the merged view's. The pair is what makes a swap of the two
    /// views at the `build_managed_config_client` call site observable.
    #[tokio::test]
    async fn extra_ca_try_init_builds_the_managed_client_from_the_local_view() {
        let home = home_with_unpinned_managed_root();
        let context = context_over_home(home.path(), true).await;

        let managed = context
            .manager()
            .managed_config_client()
            .expect("an online init with a [managed] source builds the fetch client");
        assert_eq!(
            managed.extra_root_count(),
            0,
            "the managed-fetch client must never trust the root the payload delivered (D-6)"
        );
        assert_eq!(
            context.remote_client().expect("online").extra_root_count(),
            1,
            "positive control: the registry client beside it does carry the merged root"
        );
    }
}
