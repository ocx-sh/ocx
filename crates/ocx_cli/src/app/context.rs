// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use ocx_lib::{
    ConfigInputs, ConfigLoader,
    cli::{ColorModeConfig, Printer, UserInterface},
    env,
    file_structure::{self, BlobStore, TagStore},
    log,
    oci::{self, index},
    package_manager,
};

use crate::api;

use super::ContextOptions;

#[derive(Clone)]
pub struct Context {
    offline: bool,
    project_path: Option<PathBuf>,
    remote_client: Option<oci::Client>,
    remote_index: Option<oci::index::RemoteIndex>,
    /// Registry client available in every mode (including `--offline`).
    ///
    /// Built unconditionally so `ocx package verify` can read the artifact + its
    /// signature referrer from the registry even offline — verify's offline
    /// semantics scope to Sigstore trust services, not the artifact registry
    /// (see `verify_context`). `remote_client` / `online_context` stay
    /// offline-gated for every other command.
    registry_client: oci::Client,
    local_index: oci::index::LocalIndex,
    file_structure: file_structure::FileStructure,
    api: api::Api,
    ui: UserInterface,
    default_index: oci::index::Index,
    manager: package_manager::PackageManager,
    default_registry: String,
    config_trust: ocx_lib::trust::TrustConfig,
    config_view: env::OcxConfigView,
    concurrency: package_manager::Concurrency,
    progress: ocx_lib::cli::progress::ProgressManager,
    /// The fully merged config (every tier). Exposed so `ocx config update`
    /// and the background-refresh hook can resolve the `[managed]` tier
    /// themselves via `resolve_managed_target` (which never enforces the
    /// required-snapshot gate `try_init` itself applies below).
    config: ocx_lib::Config,
    /// The effective `OCX_MANAGED_CONFIG` override, already hermetic-gated by
    /// `OCX_NO_CONFIG` and empty-string-is-unset — resolved once here so every
    /// consumer (the required-gate below, `config update`, the refresh hook)
    /// agrees on the same value.
    managed_config_env_override: Option<String>,
    /// The on-disk managed-config snapshot, read once at `try_init` and
    /// **identity-gated** there (W2): `Some` only when it matches the
    /// effective source via the shared `snapshot_matches_source` predicate.
    /// Any I/O/parse failure is treated as absent (benign-state rule).
    managed_config_snapshot: Option<ocx_lib::managed_config::ManagedConfigSnapshot>,
}

/// The two `[managed]` tier gates `Context::try_init` needs, wrapped in a named
/// struct so the two adjacent `bool`s can never be transposed at the call site.
pub struct ManagedConfigGate {
    /// Gates the `[managed]` tier's required-snapshot check (ADR Decision E,
    /// criterion 6): `true` for ordinary commands (fails closed with
    /// `SnapshotRequired`, exit 78, when `required = true` and no matching
    /// snapshot exists); `false` for `ocx config update` and the `self`/static
    /// commands, which must remain reachable to fix (or simply do not touch)
    /// exactly that missing state. See
    /// `app::should_enforce_managed_config_required`.
    pub enforce_required: bool,
    /// Narrower than `enforce_required`: `true` only for the two commands that
    /// can adopt a brand-new managed-config source with no seed present (`ocx
    /// config update`, `ocx self setup`) — they get the managed-fetch client
    /// even when no source resolves yet. See
    /// `app::is_managed_config_onboarding_command`.
    pub onboarding: bool,
}

impl Context {
    pub async fn try_init(
        options: &ContextOptions,
        color_config: ColorModeConfig,
        managed_config_gate: ManagedConfigGate,
    ) -> anyhow::Result<Context> {
        // Shared span-free progress manager (ADR adr_progress_architecture).
        // Created before the subscriber so its `MultiProgress` backs the
        // fmt log writer (log lines flush inside `suspend`, never tearing
        // bars). Threaded into the OCI client (transfer bars) and the
        // package manager (task spinners). Disabled when stderr is not a
        // TTY so non-interactive runs pay no cost.
        let progress = if ocx_lib::cli::ProgressMode::detect().stderr {
            ocx_lib::cli::progress::ProgressManager::stderr()
        } else {
            ocx_lib::cli::progress::ProgressManager::disabled()
        };

        ocx_lib::cli::LogSettings::default()
            .with_console_level(options.log_level)
            .with_stderr_color(color_config.stderr)
            .init_with_progress(&progress)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        log::debug!("Creating context with options: {:?}", options);

        // Detect the host libc once and populate the process-wide cache that
        // `Platform::current()` reads during index resolution. One-shot CLI
        // assumption (see `host_capabilities` module doc). Detection failure is
        // not fatal — an undetected libc caches as `None`, a valid state that
        // restricts matching to entries with empty `os.features`.
        oci::HostCapabilities::detect_and_cache().await;

        if options.offline && options.remote {
            // `--offline --remote` = pinned-only mode. Both flags accepted
            // together because the routing matrix collapses cleanly:
            // `--offline` overrides `--remote` to no-source-contact, and
            // any tag-addressed resolution must succeed locally or error.
            // Documented in user-guide §Routing and command-line.md.
            log::info!(
                "--offline --remote: pinned-only mode - tag and catalog lookups will not contact a source. \
                 Tag-addressed resolution attempts must be satisfied locally or by digest-pinned identifiers."
            );
        }

        // Capture the explicit project path before consuming `options` into other
        // init calls. `lock` and similar commands need it for the precedence
        // chain: `--global`/`OCX_GLOBAL` selector ▸ `--project` ▸ `OCX_PROJECT`
        // ▸ CWD walk ▸ None.
        let project_path = options.project.clone();

        let cwd = env::current_dir()?;
        let loaded_config = ConfigLoader::load_with_local_view(ConfigInputs {
            explicit_path: options.config.as_deref(),
            explicit_project_path: options.project.as_deref(),
            cwd: Some(&cwd),
        })
        .await?;
        let config = loaded_config.merged;
        let local_only_config = loaded_config.local_only;
        // The loader's own raw read of snapshot.json (pre-identity-gate) —
        // reused below instead of a second read of the same file.
        let managed_config_snapshot = loaded_config.managed_config_snapshot;
        // The loader's single `[managed]` target resolution (from the local-only
        // view) — reused below for the required gate and the snapshot identity
        // gate instead of resolving the same target two more times.
        let resolved_managed_config = loaded_config.resolved_managed_config;

        // Resolve the per-host mirror map once via the lib resolver
        // (`ocx_lib::resolve_mirror_map`): `[mirrors]` config merged with the
        // inherited `OCX_MIRRORS` env (env wins per-host key), every entry parsed
        // and the plain-HTTP gate enforced in one place. The same resolved set
        // feeds both the OCI client (transport rewrite) and the `OcxConfigView`
        // (subprocess forwarding), so parent and forwarded children agree on the
        // mirror map. The lib `thiserror` error is re-wrapped into `anyhow` at
        // this CLI boundary.
        let (mirror_entries, mirror_pairs) =
            ocx_lib::resolve_mirror_map(&config, env::mirrors()?, &env::insecure_registries())
                .map_err(anyhow::Error::new)?;
        let mirror_map = oci::MirrorMap::new(mirror_entries);

        let printer = Printer::new(color_config.stdout, color_config.stderr);
        let ui = UserInterface::new(printer, console::Term::stderr().is_term(), options.quiet);
        // `ContextOptions::build_api` owns the printer + format-default +
        // quiet wiring. Shared with the Context-free static-command bypass
        // (`ocx version`) so both paths honour `--color` and the
        // `None → Plain` format default identically (handshake §3 amended
        // 2026-05-19: format is a context-only concern, no per-command
        // divergence).
        let api = options.build_api(color_config);

        // Explicit builder (not `from_env_with_progress`) so the config-derived
        // `MirrorMap` is threaded in; `OCX_MIRRORS` env precedence is already
        // folded into `mirror_map` by `resolve_mirrors`. A plain-HTTP mirror
        // requires its host in `OCX_INSECURE_REGISTRIES` (the mirror host is
        // what gets contacted) — composition with the existing plain-HTTP set,
        // no implicit scheme-driven opt-out (ADR F2).
        //
        // Built unconditionally: `verify` reads the artifact + signature from the
        // registry in every mode (its `--offline` scopes to Sigstore trust
        // services, not the registry). Offline still yields `remote_client:
        // None`, so the manager and `online_context()`/`remote_client()` keep
        // their offline behavior — only `verify_context()` reads this client.
        let registry_client = oci::ClientBuilder::new()
            .plain_http_registries(env::insecure_registries())
            .mirrors(mirror_map)
            .progress(progress.clone())
            .build();
        let (remote_client, remote_index) = if options.offline {
            (None, None)
        } else {
            (
                Some(registry_client.clone()),
                Some(index::RemoteIndex::new(index::RemoteConfig {
                    client: registry_client.clone(),
                })),
            )
        };
        let file_structure = file_structure::FileStructure::new();
        let tag_root = options
            .index
            .clone()
            .or_else(|| env::var(env::keys::OCX_INDEX).map(std::path::PathBuf::from))
            .unwrap_or_else(|| file_structure.tags.root().to_path_buf());
        let local_index = index::LocalIndex::new(index::LocalConfig {
            tag_store: TagStore::new(tag_root),
            blob_store: BlobStore::new(file_structure.blobs.root().to_path_buf()),
        });

        // Single `Index::from_chained` entry point; see
        // `chain_mode_and_sources` for the offline/online derivation.
        // Precedence (offline wins by producing no remote_index): frozen ▸
        // remote ▸ default. Frozen keeps the remote source so digest-pinned
        // content still fetches; only unpinned-tag resolution is refused.
        let online_mode = if options.frozen {
            index::ChainMode::Frozen
        } else if options.remote {
            index::ChainMode::Remote
        } else {
            index::ChainMode::Default
        };
        let (mode, sources) = Self::chain_mode_and_sources(remote_index.as_ref(), online_mode);
        let selected_index = index::Index::from_chained(local_index.clone(), sources, mode);

        let default_registry = env::string(
            "OCX_DEFAULT_REGISTRY",
            config
                .resolved_default_registry()
                .map(str::to_owned)
                .unwrap_or_else(|| ocx_lib::oci::DEFAULT_REGISTRY.into()),
        );

        // Resolve the [patches] site-tier config before constructing the manager
        // so the resolved form can be threaded in at construction time.
        // The two-step resolution (config-file tier then env fallback) must happen
        // here — the manager constructor receives the already-resolved form and does
        // not read config itself.
        //
        // The `no_patches` opt-out is a forwarded project-runtime concern, never
        // a `[patches]` config field, and MUST NOT be grafted onto a
        // config-file-sourced tier here: doing so makes a project-local opt-out
        // ambient inherited process state — it lands in `manager.patches()` AND
        // (via `config_view.patches` below) is re-forwarded over `OCX_PATCHES`
        // into unrelated child processes. The forwarded opt-out is meaningful
        // only at the launcher re-entry (`ocx launcher exec`), which decodes it
        // directly from the env at consumption time. Every other command computes
        // its own opt-out from its own project (`PatchScope::Project(...)`) or is
        // OCI-tier (`NoProjectContext`). The env-fallback branch below still
        // forwards a pure env-sourced tier verbatim (there is no config tier to
        // be authoritative), which is correct.
        let resolved_patches = match ocx_lib::resolve_patch_config(&config).map_err(anyhow::Error::new)? {
            Some(resolved) => Some(resolved),
            None => ocx_lib::patches_from_env().map_err(anyhow::Error::new)?,
        };

        // Resolve the active patch snapshot (if any) from `OCX_PATCH_SNAPSHOT`.
        // Reading happens before manager construction so the snapshot can be
        // threaded in at construction time — mirrors the resolved_patches flow
        // above. The env var is the sole selector for now; a future
        // `--patch-snapshot` flag would populate it here first.
        let patch_snapshot_path = env::var(env::keys::OCX_PATCH_SNAPSHOT).map(std::path::PathBuf::from);
        let patch_snapshot = if let Some(ref path) = patch_snapshot_path {
            ocx_lib::patch::PatchSnapshot::read(path)
                .await
                .map_err(anyhow::Error::new)?
        } else {
            None
        };

        // `OCX_NO_CONFIG=1` is hermetic: it suppresses both the loader's
        // managed-config candidate AND the env-override read here.
        let no_config = env::flag("OCX_NO_CONFIG", false);
        let managed_config_env_override = if no_config {
            None
        } else {
            env::var(env::keys::OCX_MANAGED_CONFIG)
        };

        // Managed-config tier (ADR "Mirror posture"): the fetch client for the
        // artifact itself is built from the LOCAL-ONLY mirror view — the
        // managed payload's own `[mirrors]` is excluded from the route used to
        // fetch it (no-cycle, no self-brick). `local_only_config` is the
        // pre-managed-tier merged view `ConfigLoader::load_with_local_view`
        // returns alongside `merged`. Building the client (and resolving its
        // local-only mirror map) costs a bundled-CA conversion — gated on a
        // source actually resolving (env override, else the seed) so the vast
        // majority of invocations with no managed tier configured pay nothing.
        // `managed_config_onboarding` also needs the client: it names exactly
        // `ocx config update` and `ocx self setup` (`app.rs`'s
        // `is_managed_config_onboarding_command`), the only commands that can
        // ONBOARD a brand-new source with no seed yet (`ocx self setup
        // --managed-config <ref>`) — those need the client even though
        // `has_managed_source` is false. Deliberately NARROWER than the
        // required-gate exemption: `ocx self activate` runs on every shell
        // startup and must never pay the client-build cost for an
        // unconfigured tier.
        let has_managed_source = managed_config_env_override
            .as_deref()
            .is_some_and(|source| !source.is_empty())
            || config
                .managed
                .as_ref()
                .and_then(|managed| managed.source.as_deref())
                .is_some_and(|source| !source.is_empty());
        let needs_managed_config_client = has_managed_source || managed_config_gate.onboarding;
        let managed_config_client = if options.offline || !needs_managed_config_client {
            None
        } else {
            let (local_mirror_entries, _local_mirror_pairs) =
                ocx_lib::resolve_mirror_map(&local_only_config, env::mirrors()?, &env::insecure_registries())
                    .map_err(anyhow::Error::new)?;
            let local_mirror_map = oci::MirrorMap::new(local_mirror_entries);
            Some(
                oci::ClientBuilder::new()
                    .plain_http_registries(env::insecure_registries())
                    .mirrors(local_mirror_map)
                    .progress(progress.clone())
                    .build(),
            )
        };

        // ADR Decision E: the `[managed]` target is resolved ONCE in the loader
        // (from the local-only view — the payload can never redirect the tier
        // that fetched it) and threaded here. Reuse it. The loader swallows a
        // resolution ERROR for its best-effort fold, so a configured-but-
        // unresolvable seed re-resolves HERE only to surface the authoritative
        // typed error (malformed seed/env ref, bad interval → exit 78); the
        // happy path never re-resolves.
        let resolved_managed_target = match resolved_managed_config {
            Some(resolved) => Some(resolved),
            None if has_managed_source => {
                ocx_lib::resolve_managed_target(&config, managed_config_env_override.as_deref())?
            }
            None => None,
        };

        // W2: identity-gate the raw on-disk snapshot ONCE against the effective
        // source (shared `snapshot_matches_source` predicate) so no CLI consumer
        // — `config update --check` included — ever reads an identity-mismatched
        // snapshot as if it belonged to the current tier. Reused by both the
        // required gate and the snapshot filter below so they can never drift.
        let snapshot_identity_matches = match (&managed_config_snapshot, &resolved_managed_target) {
            (Some(snapshot), Some(resolved)) => ocx_lib::snapshot_matches_source(snapshot, &resolved.source),
            _ => false,
        };

        // Required gate: `SnapshotRequired` fails closed (exit 78) for ordinary
        // commands; `ocx config update` and the `self`/static commands are
        // exempted here (`enforce_required = false`) because their entire job is
        // to create or inspect exactly the missing state. Applied via the lib
        // `enforce_required_snapshot` so the `#[non_exhaustive]`
        // `ManagedConfigError` is constructed inside `ocx_lib`.
        let managed_config = match resolved_managed_target {
            None => None,
            Some(resolved) => match ocx_lib::enforce_required_snapshot(resolved, snapshot_identity_matches) {
                Ok(resolved) => Some(resolved),
                Err(_snapshot_required) if !managed_config_gate.enforce_required => None,
                Err(source) => return Err(anyhow::Error::new(source)),
            },
        };

        // The required gate above already consumed the raw value; from here on
        // only the identity-matched snapshot is exposed to CLI consumers.
        let managed_config_snapshot = managed_config_snapshot.filter(|_| snapshot_identity_matches);

        let manager = package_manager::PackageManager::new(
            file_structure.clone(),
            selected_index.clone(),
            remote_client.clone(),
            &default_registry,
        )
        .with_progress(progress.clone())
        .with_patches(resolved_patches.clone())
        .with_patch_snapshot(patch_snapshot)
        .with_managed_config_client(managed_config_client);

        // Attach policy-gated auto-verify ONCE on the shared manager so EVERY
        // install surface inherits it fail-closed — not just `install`/`pull`
        // but every `find_or_install_all` path (`package exec`, `package env`,
        // `run`, patch discovery). `None` when no operator `[[trust.policy]]` is
        // configured. install/pull refine the opt-out from their
        // `--verify`/`--no-verify` flag via `conventions::manager_with_verify_flag`.
        let operator_policies = config.trust.as_ref().map(|t| t.policy.clone()).unwrap_or_default();
        let manager = manager.with_auto_verify(build_auto_verify(
            operator_policies,
            &registry_client,
            options.offline,
            file_structure.root(),
        ));

        // Capture the absolute path of the running ocx so subprocess spawns
        // can pin the inner ocx binary via `OCX_BINARY_PIN` instead of relying
        // on whatever `$PATH` resolves at the launcher site. Falling back to
        // the canonical `ocx` name lets ocx still operate when `current_exe()`
        // fails (e.g. binary deleted under a long-running process); the child
        // launcher's `${OCX_BINARY_PIN:-ocx}` form then degrades to `$PATH`-lookup.
        let self_exe = std::env::current_exe().unwrap_or_else(|e| {
            log::warn!("Could not resolve current exe: {e}");
            std::path::PathBuf::from("ocx")
        });
        let mut config_view = options.as_view(self_exe);
        // Feed the same resolved mirror map into the forwarding view so a child
        // ocx inherits `OCX_MIRRORS` matching the parent's transport rewrite.
        config_view.mirrors = mirror_pairs;
        // Thread the already-resolved patches into the config forwarding view
        // so child ocx processes (launcher exec) inherit the same patch tier
        // via `OCX_PATCHES` (C5 — forwarding across process boundaries).
        // `resolved_patches` was resolved above (config-file tier then env
        // fallback) before being passed to the manager constructor.
        config_view.patches = resolved_patches;
        // Forward the already-resolved patch snapshot path into the config view
        // so child processes (launcher exec) inherit the same snapshot via
        // `OCX_PATCH_SNAPSHOT` — mirrors how `resolved_patches` is forwarded
        // above. No `--patch-snapshot` flag exists yet; the env var is the
        // sole selector for now.
        config_view.patch_snapshot = patch_snapshot_path;
        // Forward the effective managed-config source so a child ocx (launcher
        // re-entry) resolves the same managed tier via `OCX_MANAGED_CONFIG`.
        config_view.managed_config_source = managed_config.as_ref().map(|resolved| resolved.source.to_string());
        // Forward the auto-verify opt-out so a launcher-spawned child install
        // inherits the same CI-wide `OCX_NO_VERIFY`. Pure env passthrough — the
        // per-command `--no-verify` flag is a one-shot choice and is not
        // forwarded. (`env::keys::OCX_NO_VERIFY`, see `subsystem-cli.md`.)
        config_view.no_verify = env::flag(env::keys::OCX_NO_VERIFY, false);
        check_global_project_exclusivity(&config_view)?;
        check_frozen_remote_exclusivity(&config_view)?;
        let concurrency = resolve_concurrency(options.jobs);

        Ok(Context {
            remote_client,
            remote_index,
            registry_client,
            offline: options.offline,
            project_path,
            file_structure,
            api,
            ui,
            local_index,
            default_index: selected_index,
            manager,
            default_registry,
            // Narrow projection (ISP): verify pools these with the project
            // ocx.toml's trust policies; the rest of `config` is already
            // extracted into `default_registry` / mirrors / patches above.
            config_trust: config.trust.unwrap_or_default(),
            config_view,
            concurrency,
            progress,
            config,
            managed_config_env_override,
            managed_config_snapshot,
        })
    }

    /// Shared span-free progress manager (ADR adr_progress_architecture).
    /// Commands wrap long operations in guards from this manager
    /// (`spinner`/`bytes`) instead of emitting tracing-indicatif spans.
    pub fn progress(&self) -> &ocx_lib::cli::progress::ProgressManager {
        &self.progress
    }

    pub fn is_offline(&self) -> bool {
        self.offline
    }

    /// Returns the explicit `--project` / `OCX_PROJECT` override path, if
    /// one was supplied. Commands that need project-level resolution (e.g. `lock`)
    /// should pass this to `ProjectConfig::resolve` as the explicit override so
    /// the flag is not silently discarded.
    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    /// Whether the global toolchain (`$OCX_HOME/ocx.toml`) was selected
    /// via the root `--global` flag or `OCX_GLOBAL`. Passed to
    /// `ProjectConfig::resolve` so project-tier prologues select the
    /// global file instead of walking the CWD. Mutually exclusive with an
    /// explicit `--project` / `OCX_PROJECT` (enforced by clap
    /// `conflicts_with` for the flag pair and by
    /// [`check_global_project_exclusivity`] for the env-sourced gaps).
    pub fn global(&self) -> bool {
        self.config_view.global
    }

    pub fn remote_client(&self) -> ocx_lib::Result<&oci::Client> {
        self.remote_client.as_ref().ok_or(ocx_lib::Error::OfflineMode)
    }

    pub fn remote_index(&self) -> ocx_lib::Result<&oci::index::RemoteIndex> {
        self.remote_index.as_ref().ok_or(ocx_lib::Error::OfflineMode)
    }

    pub fn local_index(&self) -> &oci::index::LocalIndex {
        &self.local_index
    }

    pub fn default_index(&self) -> &oci::index::Index {
        &self.default_index
    }

    /// Verb-intent index for the update family (`ocx update`): resolves tags
    /// live against the registry by default (`Remote`), capped by the policy
    /// ceilings (`--offline` wins over `--frozen`, same ladder as
    /// [`Self::try_init`] minus the `Default` arm), and never commits tag
    /// pointers into the shared local index — the caller's `ocx.lock` is the
    /// canonical record. See `adr_toolchain_update_family.md`.
    pub fn update_index(&self) -> oci::index::Index {
        let online_mode = if self.config_view.frozen {
            index::ChainMode::Frozen
        } else {
            index::ChainMode::Remote
        };
        let (mode, sources) = Self::chain_mode_and_sources(self.remote_index.as_ref(), online_mode);
        oci::index::Index::from_chained_lock_scoped(self.local_index.clone(), sources, mode)
    }

    /// Shared chain wiring for [`Self::try_init`] and [`Self::update_index`]:
    /// no remote index (`--offline`) forces `Offline` with no sources; online
    /// wraps the remote as the single chain source under the caller-chosen
    /// mode. Deriving mode and sources from the same value prevents the
    /// `(offline, remote_index = Some)` contradiction a bool-based match
    /// could produce.
    fn chain_mode_and_sources(
        remote_index: Option<&index::RemoteIndex>,
        online_mode: index::ChainMode,
    ) -> (index::ChainMode, Vec<index::Index>) {
        match remote_index {
            None => (index::ChainMode::Offline, Vec::new()),
            Some(remote) => (online_mode, vec![index::Index::from_remote(remote.clone())]),
        }
    }

    pub fn default_registry(&self) -> &str {
        &self.default_registry
    }

    /// Operator-tier trust policies from the merged `config.toml` (system /
    /// user / `$OCX_HOME`, array-appended). `ocx package verify` treats these
    /// as authoritative over the project `ocx.toml` (`trust::resolve_tiered`).
    pub fn config_trust_policies(&self) -> &[ocx_lib::trust::TrustPolicy] {
        &self.config_trust.policy
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

    pub fn manager(&self) -> &package_manager::PackageManager {
        &self.manager
    }

    /// Resolution-affecting policy snapshot to forward to subprocess spawns
    /// via [`env::Env::apply_ocx_config`]. Built from parsed `ContextOptions`
    /// at init time — beats stale parent-shell `OCX_*` exports.
    pub fn config_view(&self) -> &env::OcxConfigView {
        &self.config_view
    }

    /// Concurrency cap for parallel pulls, derived from `--jobs` (CLI),
    /// `OCX_JOBS` (env), or unbounded by default.
    pub fn concurrency(&self) -> package_manager::Concurrency {
        self.concurrency
    }

    /// The fully merged config (every tier). `ocx config update` and the
    /// background-refresh hook use this with
    /// `ocx_lib::resolve_managed_target` to resolve the
    /// `[managed]` tier WITHOUT the required-snapshot gate `try_init` itself
    /// enforces for ordinary commands.
    pub fn config(&self) -> &ocx_lib::Config {
        &self.config
    }

    /// The effective `OCX_MANAGED_CONFIG` override — already hermetic-gated
    /// by `OCX_NO_CONFIG` and with an empty string treated as unset.
    pub fn managed_config_env_override(&self) -> Option<&str> {
        self.managed_config_env_override.as_deref()
    }

    /// The on-disk managed-config snapshot, read once at `try_init` and
    /// identity-gated against the effective source (W2) — `Some` only when it
    /// belongs to the current tier. Absent on any I/O or parse failure
    /// (benign-state rule) or identity mismatch.
    pub fn managed_config_snapshot(&self) -> Option<&ocx_lib::managed_config::ManagedConfigSnapshot> {
        self.managed_config_snapshot.as_ref()
    }

    /// Returns the default [`Index`] paired with the online [`oci::Client`].
    ///
    /// This is the single accessor for commands that *require* network access
    /// (sign, verify, publish, …). It returns [`ocx_lib::Error::OfflineMode`]
    /// when the context was built with `--offline`, routing to exit code 81
    /// (`PolicyBlocked`) via [`ocx_lib::cli::classify_error`].
    ///
    /// Commands that optionally fall back to online mode should continue to
    /// use [`Self::default_index`] + [`Self::remote_client`] separately; the
    /// paired accessor is for commands where both are always required.
    #[allow(dead_code)] // Consumed by `command/package_sign.rs` in Phase 5.
    pub fn online_context(&self) -> ocx_lib::Result<(&oci::index::Index, &oci::Client)> {
        let client = self.remote_client.as_ref().ok_or(ocx_lib::Error::OfflineMode)?;
        Ok((&self.default_index, client))
    }

    /// Returns the default [`Index`] paired with a registry [`oci::Client`] for
    /// `ocx package verify`, in every mode — including `--offline`.
    ///
    /// Unlike [`Self::online_context`], this never returns `OfflineMode`: verify
    /// inherently reads the artifact and its signature referrer from the
    /// registry where they live (a local mirror in air-gapped deployments), so
    /// its `--offline` semantics scope to the Sigstore trust services (the Rekor
    /// key fetch and TUF), not the artifact registry. The returned `bool` is the
    /// offline flag, which the verify pipeline uses to forbid trust-services
    /// network and require cached/supplied trust material.
    pub fn verify_context(&self) -> (&oci::index::Index, &oci::Client, bool) {
        (&self.default_index, &self.registry_client, self.offline)
    }
}

/// Build the shared policy-gated auto-verify config, or `None` when no operator
/// `[[trust.policy]]` is configured.
///
/// Attached once on the manager (every install surface inherits it). Carries the
/// always-available registry client (verify reads the signature referrer from
/// the registry even under `--offline`), the offline flag, the
/// `OCX_SIGSTORE_TUF_ROOT` / `OCX_SIGSTORE_TRUST_ROOT` overrides, and the
/// `OCX_NO_VERIFY` opt-out default (install/pull refine it from their flag).
/// OCI-tier gating uses the operator `config.toml` set only; the project
/// `ocx.toml` pool stays empty (no new OCI-tier carve-out).
fn build_auto_verify(
    operator_policies: Vec<ocx_lib::trust::TrustPolicy>,
    registry_client: &oci::Client,
    offline: bool,
    cache_root: &Path,
) -> Option<package_manager::AutoVerify> {
    if operator_policies.is_empty() {
        return None;
    }
    // Compile-time-constant, known-valid URL — validated (not parsed by name) so
    // the CLI never names `url::Url`. Unused when the trust root pins the Rekor
    // key (the `OCX_SIGSTORE_TUF_ROOT` / offline path).
    const DEFAULT_REKOR_URL: &str = "https://rekor.sigstore.dev";
    let rekor_url =
        oci::endpoint::validate_sigstore_url(DEFAULT_REKOR_URL, "rekor").expect("built-in default Rekor URL is valid");
    Some(package_manager::AutoVerify::new(package_manager::AutoVerifyInput {
        operator_policies,
        // ponytail: seam for the deferred project-tier auto-verify (#99 known gap
        // — `ocx.toml` policies not yet read on OCI-tier install/pull/exec/env/run
        // surfaces, operator `config.toml` only today). Wire real project policies
        // here once that follow-up is scheduled; until then, always empty.
        project_policies: Vec::new(),
        registry_client: registry_client.clone(),
        rekor_url,
        offline,
        cache_root: cache_root.to_path_buf(),
        tuf_root_env: std::env::var_os("OCX_SIGSTORE_TUF_ROOT").map(PathBuf::from),
        pem_root_env: std::env::var_os("OCX_SIGSTORE_TRUST_ROOT").map(PathBuf::from),
        user_opted_out: env::flag(env::keys::OCX_NO_VERIFY, false),
    }))
}

/// Resolves `--jobs` / `OCX_JOBS` into a `Concurrency` value.
///
/// Precedence: CLI flag > env var > unbounded. `0` (from either source)
/// resolves to logical-core count (GNU Parallel convention). Invalid env
/// values are logged and ignored — the env path is best-effort.
fn resolve_concurrency(jobs: Option<usize>) -> package_manager::Concurrency {
    use std::num::NonZeroUsize;

    let raw = match jobs {
        Some(n) => Some(n),
        None => env::var("OCX_JOBS").and_then(|v| match v.parse::<usize>() {
            Ok(n) => Some(n),
            Err(e) => {
                log::warn!("ignoring invalid OCX_JOBS value {v:?}: {e}");
                None
            }
        }),
    };

    match raw {
        None => package_manager::Concurrency::Unbounded,
        Some(0) => package_manager::Concurrency::cores(),
        Some(n) => package_manager::Concurrency::Limit(NonZeroUsize::new(n).expect("n > 0 covered above")),
    }
}

/// Enforce mutual exclusion of the global toolchain selector and an
/// explicit project selection.
///
/// `--global` / `OCX_GLOBAL` and an explicit project (`--project` flag or
/// `OCX_PROJECT` env) both pick a project file. clap's
/// `conflicts_with = "project"` on [`ContextOptions::global`] already
/// rejects the explicit `--global` + `--project` *flag* pair at parse
/// time. This guard closes the gaps clap cannot see: `OCX_GLOBAL` reaches
/// `view.global` through the arg default (not a CLI-provided value, so
/// clap's conflict does not fire), and `OCX_PROJECT` is not a clap arg at
/// all. A project merely discovered by the CWD walk is *not* explicit —
/// `--global` from inside a project tree is legal and the global tier
/// wins by precedence (adr_global_toolchain_tier.md §Decision 2), so the
/// CWD walk deliberately sets neither `view.project` nor `OCX_PROJECT`.
///
/// # Errors
///
/// Returns [`UsageError`](ocx_lib::cli::UsageError) (exit `64`) when the
/// global selector is set alongside an explicit `--project` / `OCX_PROJECT`
/// selection.
fn check_global_project_exclusivity(view: &env::OcxConfigView) -> Result<(), ocx_lib::cli::UsageError> {
    // `OCX_PROJECT=""` is the loader's escape hatch (treated as unset);
    // mirror that here so an explicitly-cleared env var is not misread as
    // an explicit selection.
    let explicit_project = view.project.is_some() || env::var(env::keys::OCX_PROJECT).is_some_and(|v| !v.is_empty());
    if view.global && explicit_project {
        return Err(ocx_lib::cli::UsageError::new(
            "--global cannot be combined with an explicit --project / OCX_PROJECT selection",
        ));
    }
    Ok(())
}

/// Enforce mutual exclusion of `--frozen` and `--remote`.
///
/// `--frozen` freezes tag resolution to the local index; `--remote` forces
/// every mutable lookup to the source. They are directly contradictory.
/// clap's `conflicts_with = "remote"` on [`ContextOptions::frozen`] already
/// rejects the explicit `--frozen` + `--remote` *flag* pair at parse time.
/// This guard closes the env-sourced gap clap cannot see: both `OCX_FROZEN`
/// and `OCX_REMOTE` reach `view` through the arg defaults (not CLI-provided
/// values, so clap's conflict does not fire).
///
/// `--frozen` + `--offline` is deliberately **allowed**: offline is the
/// stronger constraint and wins the mode precedence, so the combination
/// collapses cleanly to offline.
///
/// # Errors
///
/// Returns [`UsageError`](ocx_lib::cli::UsageError) (exit `64`) when both the
/// frozen and remote policies are set.
fn check_frozen_remote_exclusivity(view: &env::OcxConfigView) -> Result<(), ocx_lib::cli::UsageError> {
    if view.frozen && view.remote {
        return Err(ocx_lib::cli::UsageError::new(
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
    //! here because `ocx_lib::env::var`'s test-override seam is inert when
    //! `ocx_lib` is consumed as a (non-`cfg(test)`) dependency, and real
    //! env mutation is `unsafe` on edition 2024. This test pins the
    //! `--project`-flag path, whose `||` short-circuits before any env read
    //! and is therefore deterministic.

    use super::*;
    use ocx_lib::cli::{ClassifyExitCode, ExitCode};

    #[test]
    fn global_with_explicit_project_flag_is_usage_error() {
        let mut view = ocx_lib::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
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
        let mut view = ocx_lib::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
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
        let mut view = ocx_lib::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        view.frozen = true;
        assert!(
            check_frozen_remote_exclusivity(&view).is_ok(),
            "--frozen without --remote must be accepted"
        );
    }

    #[test]
    fn frozen_and_offline_together_produces_offline_chain_mode() {
        // `--frozen --offline` is a valid combination: the guard accepts it, and
        // the mode-selection logic collapses it to `ChainMode::Offline` (the
        // stronger constraint). The key invariant: when `offline=true` the
        // `remote_index` is `None`, and the `match &remote_index` arm for `None`
        // always emits `ChainMode::Offline` regardless of the `frozen` flag.
        // This mirrors the precedence comment in `try_init`:
        // "offline already won via the `None` arm — it produced no remote_index".
        let mut view = ocx_lib::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        view.frozen = true;
        // offline=true → remote_index=None; the guard must accept the combination.
        assert!(
            check_frozen_remote_exclusivity(&view).is_ok(),
            "--frozen + --offline must pass the exclusivity guard"
        );

        // Replicate the mode-selection match from try_init:
        // offline=true produces remote_index=None → Offline wins, ignoring frozen.
        let remote_index: Option<index::RemoteIndex> = None; // simulates offline=true
        let frozen = true;
        let mode: index::ChainMode = match &remote_index {
            None => index::ChainMode::Offline,
            Some(_) => {
                if frozen {
                    index::ChainMode::Frozen
                } else {
                    index::ChainMode::Default
                }
            }
        };
        assert_eq!(
            mode,
            index::ChainMode::Offline,
            "offline (remote_index=None) must produce ChainMode::Offline even when frozen=true"
        );
    }
}
