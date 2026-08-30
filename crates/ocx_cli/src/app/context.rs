// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use ocx_lib::{
    ConfigInputs, ConfigLoader,
    cli::{ColorModeConfig, Printer, UserInterface},
    env,
    file_structure::{self, IndexStore, StateStore},
    log,
    oci::{self, index},
    package_manager,
};

use crate::api;
use crate::command::package_sign_common::{SigstoreEndpoint, explicit_trust_root_path, resolve_endpoint};

use super::ContextOptions;

#[derive(Clone)]
pub struct Context {
    offline: bool,
    project_path: Option<PathBuf>,
    remote_client: Option<oci::Client>,
    oci_index: Option<oci::index::OciIndex>,
    /// One `index.ocx.sh`-protocol source per index-bearing namespace, built
    /// when online from every merged `[registries."<ns>"]` entry that carries
    /// an `index` field (`adr_index_indirection.md` F5a — kind per NAMESPACE,
    /// see `build_index_sources`). Each is chained ahead of the plain-OCI
    /// `oci_index`; empty under `--offline` or when no namespace is configured
    /// as index-kind.
    index_sources: Vec<oci::index::OcxIndex>,
    /// Registry client available in every mode (including `--offline`), built
    /// lazily on first read via [`Self::verify_client`].
    ///
    /// Exists so `ocx package verify` can read the artifact + its signature
    /// referrer from the registry even offline — verify's offline semantics
    /// scope to Sigstore trust services, not the artifact registry (see
    /// `verify_client`). `remote_client` stays offline-gated for every other
    /// command. Deferred rather than built unconditionally in `try_init`
    /// because most `--offline` invocations never call `verify_client` and the
    /// TLS trust-store construction it costs (~7ms) is pure waste for them;
    /// `try_init` still forces it eagerly whenever a network client is needed
    /// anyway (online) or an operator trust policy demands auto-verify.
    /// `Arc` makes the cell cloneable alongside the rest of `Context`.
    registry_client_cell: std::sync::Arc<std::sync::OnceLock<oci::Client>>,
    /// Registry mirror map threaded into [`Self::registry_client_cell`]'s
    /// on-demand build — kept so `verify_client()` can construct the client
    /// without re-deriving it from `Config`.
    mirror_map: oci::MirrorMap,
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
    /// The two tiers a managed payload folds BETWEEN: `config_base` (built-in
    /// defaults, system, user, `$OCX_HOME`) and the explicit `OCX_CONFIG` /
    /// `--config` `config_overlay`. Kept from the one `load_with_local_view`
    /// call rather than re-loaded on demand — a second load would re-emit the
    /// loader's discovery warnings. `ocx config test` folds a candidate payload
    /// between them, reproducing the adoption order exactly.
    config_base: ocx_lib::Config,
    config_overlay: ocx_lib::Config,
    /// The **locally-authored** `[mirrors]` table (the loader's `local_only`
    /// view), kept because [`is_published_namespace`] needs it and
    /// `ocx index regenerate` asks that question after init. The merged view
    /// would be wrong here for the reason `build_index_sources` documents: a
    /// managed payload may redirect traffic, but it may not revoke the verified
    /// index path.
    local_mirrors: Option<std::collections::HashMap<String, ocx_lib::MirrorConfig>>,
    /// The effective `OCX_MANAGED_CONFIG` override, already hermetic-gated by
    /// `OCX_NO_CONFIG` and empty-string-is-unset — resolved once here so every
    /// consumer (the required-gate below, `config update`, the refresh hook)
    /// agrees on the same value.
    managed_config_env_override: Option<String>,
    /// Every host that may be contacted over plain HTTP: the union of
    /// `[registries."<name>"].insecure` and `OCX_INSECURE_REGISTRIES`, resolved
    /// once so the registry protocol, the mirror-role gate, the index base URL
    /// and `ocx login`'s probe cannot disagree about a host.
    insecure_hosts: Vec<String>,
    /// The on-disk managed-config snapshot, read once at `try_init` and
    /// **identity-gated** there (W2): `Some` only when it matches the
    /// effective source via the shared `snapshot_matches_source` predicate.
    /// Any I/O/parse failure is treated as absent (benign-state rule).
    managed_config_snapshot: Option<ocx_lib::managed_config::ManagedConfigSnapshot>,
}

/// The two `[managed]` tier gates `Context::try_init` needs, wrapped in a named
/// struct so the two adjacent `bool`s can never be transposed at the call site.
pub struct ManagedConfigGate {
    /// Gates the `[managed]` tier's required-snapshot check
    /// (`adr_managed_config_tier.md` Decision E,
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
        // `Platform::current()` reads during index resolution. Detection failure
        // is not fatal — an undetected libc caches as `None`, a valid state that
        // restricts matching to entries with empty `os.features`.
        //
        // Cheap on all but the first invocation per host per TTL: the answer is
        // recorded at `$OCX_HOME/state/host/capabilities.json` and re-read from
        // there, so the loader-discovery walk no longer runs per process. Left
        // ahead of the `options.offline` branch deliberately — detection reads
        // only the local filesystem and spawns only local loaders, so it is not
        // network work and offline has nothing to say about it. See the
        // `host_capabilities` module's "Cache lifecycle" note for what
        // invalidates the record.
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
        // The unmerged halves of `local_only_config`, kept for `ocx config test`
        // (see the field docs) — one load, no second discovery pass.
        let config_base = loaded_config.base;
        let config_overlay = loaded_config.overlay;
        // The loader's own raw read of snapshot.json (pre-identity-gate) —
        // reused below instead of a second read of the same file.
        let managed_config_snapshot = loaded_config.managed_config_snapshot;
        // The loader's single `[managed]` target resolution (from the local-only
        // view) — reused below for the required gate and the snapshot identity
        // gate instead of resolving the same target two more times.
        let resolved_managed_config = loaded_config.resolved_managed_config;
        // What that snapshot actually contributed to `config` — identity gate
        // AND payload parse, as observed by the tier that did the folding.
        let managed_snapshot_state = loaded_config.managed_snapshot_state;

        // Which hosts may be reached over plain HTTP, resolved ONCE: the union
        // of the `[registries."<name>"].insecure` entries and the inherited
        // `OCX_INSECURE_REGISTRIES`, less anything the system scope locked shut.
        // Every gate below is handed this same set — the OCI client's protocol
        // choice, the mirror-role gate, each index base URL, and `ocx login`'s
        // probe — so one resolution answers for all of them.
        //
        // They compare it against their own resolved host string, and those
        // strings are not all the same: see `insecure_hosts`' doc for the one
        // name (Docker Hub) where they diverge, and why that divergence is
        // closed in the safe direction.
        let insecure_hosts = ocx_lib::insecure_hosts(&config, &env::insecure_registries());

        // Resolve the per-host mirror map once via the lib resolver
        // (`ocx_lib::resolve_mirror_map`): `[mirrors]` config merged with the
        // inherited `OCX_MIRRORS` env (env wins per-host key), every entry parsed
        // and the plain-HTTP gate enforced in one place, split by traffic role.
        // The registry role feeds the OCI client (transport rewrite) ONLY; the
        // merged (pre-role-parse) union feeds the `OcxConfigView` (subprocess
        // forwarding), so parent and forwarded children agree on the mirror map
        // and a forwarded child re-parses + re-validates it the same way a
        // `[mirrors]` TOML entry would. The lib `thiserror` error is re-wrapped
        // into `anyhow` at this CLI boundary.
        let resolved_mirrors =
            ocx_lib::resolve_mirror_map(&config, env::mirrors()?, &insecure_hosts).map_err(anyhow::Error::new)?;
        let mirror_map = oci::MirrorMap::new(resolved_mirrors.registry.clone());

        let printer = Printer::new(color_config.stdout, color_config.stderr);
        let ui = UserInterface::new(printer, console::Term::stderr().is_term(), options.quiet);
        // `ContextOptions::build_api` owns the printer + format-default +
        // quiet wiring. Shared with the Context-free static-command bypass
        // (`ocx version`) so both paths honour `--color` and the
        // `None → Plain` format default identically (handshake §3 amended
        // 2026-05-19: format is a context-only concern, no per-command
        // divergence).
        let api = options.build_api(color_config);

        // Explicit builder so the config-derived
        // `MirrorMap` is threaded in; `OCX_MIRRORS` env precedence is already
        // folded into `mirror_map` by `resolve_mirrors`. A plain-HTTP mirror
        // requires its host declared insecure (the mirror host is what gets
        // contacted) — either `[registries."<host>"] insecure = true` or
        // `OCX_INSECURE_REGISTRIES`, unioned; composition with the existing
        // plain-HTTP set, no implicit scheme-driven opt-out
        // (`adr_oci_registry_mirror.md`, Implementation Plan step 3 "Auth + insecure").
        //
        // `verify` reads the artifact + signature from the registry in every
        // mode (its `--offline` scopes to Sigstore trust services, not the
        // registry) — see `registry_client_cell`'s field doc. Offline yields
        // `remote_client: None` (the cell stays unbuilt), so the manager and
        // `remote_client()` keep their offline behavior; online forces the
        // cell here since a network client is needed regardless.
        let registry_client_cell: std::sync::Arc<std::sync::OnceLock<oci::Client>> =
            std::sync::Arc::new(std::sync::OnceLock::new());
        let (remote_client, oci_index) = if options.offline {
            (None, None)
        } else {
            let client = registry_client_cell
                .get_or_init(|| build_registry_client(&mirror_map, &progress, &insecure_hosts))
                .clone();
            (
                Some(client.clone()),
                Some(index::OciIndex::new(index::OciIndexConfig { client })),
            )
        };
        let file_structure = file_structure::FileStructure::new();
        // Index home precedence (`adr_index_indirection.md` A1): `--index` ▸
        // `OCX_INDEX` ▸ `$OCX_HOME/index`. This redirects the whole
        // collection — every configured source's subtree (root documents +
        // dispatch-object CAS), not a single source in isolation.
        // A redirected index home may be a user-committed or read-only shipped
        // copy, so its cross-process locks stay machine-global under
        // `$OCX_HOME/locks` (never inside the redirected home).
        let index_store = options
            .index
            .clone()
            .or_else(|| env::var(env::keys::OCX_INDEX).map(std::path::PathBuf::from))
            .map(|home| IndexStore::new(home).with_locks_root(file_structure.locks.clone()))
            .unwrap_or_else(|| file_structure.index.clone());
        // The yanked opt-in (`OCX_ALLOW_YANKED`) gates OFFLINE status surfacing:
        // a committed root's yanked tag is refused on a local resolve unless it is
        // set (`adr_index_indirection.md` F3) — the offline counterpart to the
        // `OcxIndex` `allow_yanked` the index sources below read.
        let allow_yanked = env::flag(env::keys::OCX_ALLOW_YANKED, false);
        let local_index = index::LocalIndex::new(index::LocalConfig {
            index_store: index_store.clone(),
        })
        .with_allow_yanked(allow_yanked)
        // The SSRF exemption for physical pointers this local copy mints
        // (ocx#218). Threaded here, not at each chain construction, so every
        // chain built from this local index — default, lock-scoped `ocx update`,
        // `ocx patch test`'s scratch chain, `PackageManager::offline_view` —
        // reads the same operator config. `--offline` builds no sources at all,
        // so without this the floor would have no exemption to read there.
        .with_trusted_hosts(trusted_hosts_by_namespace(&config));

        // Single `Index::from_chained` entry point; see
        // `chain_mode_and_sources` for the offline/online derivation.
        // Precedence (offline wins by producing no oci_index): frozen ▸
        // remote ▸ default. Frozen keeps the remote source so digest-pinned
        // content still fetches; only unpinned-tag resolution is refused.
        let online_mode = Self::online_chain_mode(options.frozen, options.remote);
        let index_sources = Self::build_index_sources(
            remote_client.is_some(),
            &config,
            local_only_config.mirrors.as_ref(),
            &resolved_mirrors.index,
            &mirror_map,
            &insecure_hosts,
            &progress,
        )?;
        let (mode, sources) = Self::chain_mode_and_sources(oci_index.as_ref(), &index_sources, online_mode);
        // Attach the machine-global blob store so an installed tool's leaf
        // platform manifest (content, cached in `$OCX_HOME/blobs` at install —
        // never the local index, A3) resolves offline with zero network: an
        // an absent dispatch object is recovered from the blob store before any
        // source walk
        // (`adr_index_indirection.md` A3 step 2 / B2).
        let selected_index = index::Index::from_chained_with_content_store(
            local_index.clone(),
            sources,
            mode,
            file_structure.blobs.clone(),
        );

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
        // its own opt-out from its own project (`EnvScope::Project(...)`) or is
        // OCI-tier (`EnvScope::Package`). The env-fallback branch below still
        // forwards a pure env-sourced tier verbatim (there is no config tier to
        // be authoritative), which is correct.
        let resolved_patches = match ocx_lib::resolve_patch_config(&config).map_err(anyhow::Error::new)? {
            Some(resolved) => Some(resolved),
            None => ocx_lib::patches_from_env().map_err(anyhow::Error::new)?,
        };

        // Resolve the active patch snapshot (if any) from `OCX_PATCH_SNAPSHOT`.
        // Reading happens before manager construction so the snapshot can be
        // threaded in at construction time — mirrors the resolved_patches flow
        // above. The env var is the sole selector: adopting a snapshot is a
        // deliberate opt-in, orthogonal to `--frozen` (which scopes to the
        // package tier). A future `--patch-snapshot` flag would populate it
        // here first.
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
            Some(build_managed_config_client(
                &local_only_config,
                env::mirrors()?,
                &env::insecure_registries(),
                &progress,
            )?)
        };

        // `adr_managed_config_tier.md` Decision A (identity-gated merge): the
        // `[managed]` target is resolved ONCE in the loader, from the local-only
        // view — the payload can never redirect the tier that fetched it — and
        // threaded here. Reuse it. The loader swallows a
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

        // W2: the loader reports what the snapshot actually contributed ONCE
        // (identity gate plus payload parse) and both consumers below read that
        // single value, so no CLI consumer — `config update --check` included —
        // reads an identity-mismatched snapshot as if it belonged to the current
        // tier, and the required gate can never drift from the merge.
        let snapshot_identity_matches = managed_snapshot_state != ocx_lib::ManagedSnapshotState::Unmatched;

        // Required gate: fails closed (exit 78) for ordinary commands; `ocx
        // config update` and the `self`/static commands are exempted here
        // (`enforce_required = false`) because their entire job is to create or
        // inspect exactly the missing state. Applied via the lib
        // `enforce_required_snapshot` so the `#[non_exhaustive]`
        // `ManagedConfigError` is constructed inside `ocx_lib`. Identity alone
        // is NOT the gate: a snapshot whose payload failed to parse is on disk
        // but contributes nothing, and a `required` tier must not read as
        // satisfied by a payload none of whose settings are in force.
        let managed_config = match resolved_managed_target {
            None => None,
            Some(resolved) => match ocx_lib::enforce_required_snapshot(resolved, managed_snapshot_state) {
                Ok(resolved) => Some(resolved),
                Err(_snapshot_required) if !managed_config_gate.enforce_required => None,
                Err(source) => return Err(anyhow::Error::new(source)),
            },
        };

        // The required gate above already consumed the raw value; from here on
        // only the identity-matched snapshot is exposed to CLI consumers — an
        // unusable payload still surfaces, so `config update --check` can report
        // the state it is there to diagnose.
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
        .with_managed_config_client(managed_config_client)
        // Guaranteed-local companion / site-patch lookups must read the same
        // (`--index` / `OCX_INDEX` redirected) snapshot the main index uses.
        .with_index(index_store);

        // Attach policy-gated auto-verify ONCE on the shared manager so EVERY
        // install surface inherits it fail-closed — not just `install`/`pull`
        // but every `find_or_install_all` path (`package exec`, `package env`,
        // `run`, patch discovery). `None` when no operator `[[trust.policy]]` is
        // configured. install/pull refine the opt-out from their
        // `--verify`/`--no-verify` flag via `conventions::manager_with_verify_flag`.
        let operator_policies = config.trust_policies().to_vec();
        // Resolve the `OCX_NO_VERIFY` opt-out ONCE here; both the auto-verify
        // config below and the forwarding `config_view` further down read this
        // single value (the per-command `--verify`/`--no-verify` flag refines it
        // in `conventions::manager_with_verify_flag`).
        let no_verify_env = env::flag(env::keys::OCX_NO_VERIFY, false);
        // Only force the lazy registry client when a policy actually needs it —
        // an empty `operator_policies` must not pay the ~7ms TLS-store build cost
        // `build_auto_verify` would otherwise trigger by taking `&oci::Client`.
        let auto_verify = if operator_policies.is_empty() {
            None
        } else {
            let client = registry_client_cell
                .get_or_init(|| build_registry_client(&mirror_map, &progress, &insecure_hosts))
                .clone();
            build_auto_verify(
                operator_policies,
                config.trust.as_ref().and_then(|t| t.sigstore.clone()),
                &client,
                options.offline,
                file_structure.state.clone(),
                no_verify_env,
            )?
            .map(package_manager::AutoVerify::new)
        };
        let manager = manager.with_auto_verify(auto_verify);

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
        // Feed the same resolved (merged, pre-role-parse) mirror map into the
        // forwarding view so a child ocx inherits `OCX_MIRRORS` matching the
        // parent's transport rewrite.
        config_view.mirrors = resolved_mirrors.merged.into_iter().collect();
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
        config_view.no_verify = no_verify_env;
        check_global_project_exclusivity(&config_view)?;
        check_frozen_remote_exclusivity(&config_view)?;
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
            // Narrow projection (ISP): verify pools these with the project
            // ocx.toml's trust policies; the rest of `config` is already
            // extracted into `default_registry` / mirrors / patches above.
            config_trust: config.trust.clone().unwrap_or_default(),
            config_view,
            concurrency,
            progress,
            config,
            config_base,
            config_overlay,
            local_mirrors: local_only_config.mirrors.clone(),
            managed_config_env_override,
            insecure_hosts,
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

    pub fn oci_index(&self) -> ocx_lib::Result<&oci::index::OciIndex> {
        self.oci_index.as_ref().ok_or(ocx_lib::Error::OfflineMode)
    }

    pub fn local_index(&self) -> &oci::index::LocalIndex {
        &self.local_index
    }

    /// Every configured `index.ocx.sh`-protocol source — one per index-bearing
    /// `[registries."<ns>"]` namespace, when online. Empty under `--offline` or
    /// when no namespace is configured as index-kind. Used by `ocx index
    /// update` to route a package to its namespace's source and to sync each
    /// source's catalog.
    pub fn index_sources(&self) -> &[oci::index::OcxIndex] {
        &self.index_sources
    }

    pub fn default_index(&self) -> &oci::index::Index {
        &self.default_index
    }

    /// The default-mode resolution chain — every index source ahead of the
    /// plain-OCI registry — for callers that must build their own [`Index`]
    /// over a different content store than [`Self::default_index`]'s
    /// (`ocx patch test`'s scratch root).
    ///
    /// Sharing the wiring is the point: a scratch chain assembled from the
    /// registry alone would resolve an index-bearing namespace as plain OCI
    /// while every other resolution in the same invocation goes through its
    /// index, so one identifier could name two different artifacts.
    /// `Offline` with no sources when there is no remote index.
    ///
    /// Carries the invocation's own policy ceiling rather than assuming
    /// `Default`: now that this chain reaches every index-bearing namespace's
    /// source, a hardcoded `Default` would let `ocx --frozen patch test`
    /// resolve the BASE off an unindexed tag — dialling the index and then the
    /// physical host — which is exactly what `--frozen` refuses everywhere else
    /// (`ocx --frozen pull` on the same identifier exits 81). A companion is a
    /// different tier and is deliberately unaffected: `install_companion`
    /// resolves through `Index::remote_view`, which ignores this ceiling.
    pub fn chain_sources(&self) -> (index::ChainMode, Vec<index::Index>) {
        let online_mode = Self::online_chain_mode(self.config_view.frozen, self.config_view.remote);
        Self::chain_mode_and_sources(self.oci_index.as_ref(), &self.index_sources, online_mode)
    }

    /// The policy ceiling for an online chain: `--frozen` ▸ `--remote` ▸
    /// default. `--offline` is not an arm — it is applied upstream by leaving
    /// the remote index unbuilt, which [`Self::chain_mode_and_sources`] turns
    /// into `Offline` with no sources at all.
    fn online_chain_mode(frozen: bool, remote: bool) -> index::ChainMode {
        if frozen {
            index::ChainMode::Frozen
        } else if remote {
            index::ChainMode::Remote
        } else {
            index::ChainMode::Default
        }
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
        let (mode, sources) = Self::chain_mode_and_sources(self.oci_index.as_ref(), &self.index_sources, online_mode);
        oci::index::Index::from_chained_lock_scoped(self.local_index.clone(), sources, mode)
    }

    /// Shared chain wiring for [`Self::try_init`] and [`Self::update_index`]:
    /// no remote index (`--offline`) forces `Offline` with no sources; online
    /// wraps the remote as the single chain source under the caller-chosen
    /// mode. Deriving mode and sources from the same value prevents the
    /// `(offline, oci_index = Some)` contradiction a bool-based match
    /// could produce.
    fn chain_mode_and_sources(
        oci_index: Option<&index::OciIndex>,
        index_sources: &[index::OcxIndex],
        online_mode: index::ChainMode,
    ) -> (index::ChainMode, Vec<index::Index>) {
        match oci_index {
            None => (index::ChainMode::Offline, Vec::new()),
            Some(remote) => {
                let mut sources = Vec::with_capacity(index_sources.len() + 1);
                // Every index-bearing namespace's static-file source is
                // registered BEFORE the registry so a logical reference in that
                // namespace always resolves through the verified two-hop path
                // (root -> sha256-verified obs -> physical) and the yank gate —
                // never bypassed by a registry that happens to serve the same
                // name (`adr_index_indirection.md` F, Codex R3). `jurisdiction`
                // routes each source to its own namespace and stops fall-through,
                // so exactly one remote resolves any given namespace (Decision H).
                // A namespace nobody configured as index-kind is absent here and
                // chains the registry alone, so an index-site outage can never
                // hard-block it.
                for source in index_sources {
                    sources.push(index::Index::from_source(source.clone()));
                }
                sources.push(index::Index::from_remote(remote.clone()));
                (online_mode, sources)
            }
        }
    }

    /// Builds one `index.ocx.sh`-protocol source per index-bearing namespace,
    /// when online (`adr_index_indirection.md` F5a — kind per NAMESPACE).
    ///
    /// `[registries."<ns>"] index` presence is the sole protocol-kind marker
    /// (Decision H): every merged `[registries]` entry carrying a non-empty
    /// `index` field resolves via the ocx-index protocol against that base
    /// URL — not just `ocx.sh`. A namespace with no `index` field resolves as
    /// plain OCI and gets no source here, so [`Self::chain_mode_and_sources`]
    /// chains the registry alone for it and an index-site outage can never
    /// hard-block a namespace nobody configured as index-kind. Sources are
    /// returned sorted by namespace so the chain order is deterministic.
    ///
    /// Each source's base URL honours its `[registries."<ns>"] index` value
    /// plus the `[mirrors."<host>"] index` role override for that base's
    /// traffic host (F5c); the yanked opt-in reads
    /// [`OCX_ALLOW_YANKED`](env::keys::OCX_ALLOW_YANKED). This is the single
    /// place the index clients are minted. A plain-`http://` final target is
    /// refused unless its host is in `insecure_hosts`
    /// (`OCX_INSECURE_REGISTRIES`), the same gate the registry role applies.
    ///
    /// **Mirror suppression.** A namespace whose `index` came from the
    /// compiled-in defaults tier (`index_is_compiled_default`) is dropped when
    /// a **locally-authored** `[mirrors."<ns>"]` entry pins its REGISTRY role.
    /// `[mirrors]` is keyed by traffic host and applied against the PHYSICAL
    /// identifier the index mints, so it does not follow a namespace through
    /// the index path: a firewalled site that pinned `ocx.sh` at its own
    /// registry would otherwise start dialling `index.ocx.sh` — a host it
    /// never allow-listed — with no config change of its own. An operator who
    /// declared where a namespace's traffic goes has answered the question;
    /// one who wants both writes `[registries."<ns>"] index` explicitly, which
    /// clears the compiled-default provenance and so wins here.
    ///
    /// Two scoping rules keep the trigger honest, and both are load-bearing:
    ///
    /// - **Locally-authored only** (`local_mirrors`, the loader's `local_only`
    ///   view: compiled-in ▸ discovered ▸ `OCX_CONFIG`/`--config`). The merged
    ///   config also carries the managed tier — a remote, operator-published
    ///   payload the loader itself calls untrusted. Honouring a `[mirrors]`
    ///   entry from there would let whoever controls that package revoke the
    ///   sha256-verified two-hop path AND the yank gate fleet-wide, then take
    ///   every `ocx.sh` request. A remote payload may redirect traffic (that is
    ///   its job); it may not revoke the verified path. `OCX_MIRRORS` is
    ///   excluded for the same reason — a parent ocx folds the managed tier
    ///   into the map it forwards, so trusting the env would re-open the same
    ///   hole one process hop later. Site-wide mirror policy lives in
    ///   `/etc/ocx/config.toml`, which every process reads directly.
    /// - **Registry role only.** The index role is applied keyed on the index
    ///   base's OWN host (`OcxIndex::resolve_base_url`, e.g. `index.ocx.sh`),
    ///   never on the namespace, so `[mirrors."<ns>"] index` cannot redirect
    ///   anything for `<ns>` — suppressing on it would leave the operator with
    ///   neither the index nor their mirror. A `[mirrors."index.ocx.sh"]`
    ///   entry does not suppress either: that operator redirected the index
    ///   rather than replacing it, and gets the verified path against their
    ///   own host (F5c).
    ///
    /// This is the only seam that can see both halves: the compiled-in tier is
    /// folded in `ConfigLoader`, but the mirror map is not resolved until
    /// `try_init`.
    fn build_index_sources(
        online: bool,
        config: &ocx_lib::Config,
        local_mirrors: Option<&std::collections::HashMap<String, ocx_lib::MirrorConfig>>,
        mirrors_index: &std::collections::BTreeMap<String, ocx_lib::ParsedMirror>,
        registry_mirrors: &oci::MirrorMap,
        insecure_hosts: &[String],
        progress: &ocx_lib::cli::progress::ProgressManager,
    ) -> ocx_lib::Result<Vec<index::OcxIndex>> {
        // Offline or no `[registries]` table ⇒ no sources.
        if !online {
            return Ok(Vec::new());
        }
        let Some(registries) = config.registries.as_ref() else {
            return Ok(Vec::new());
        };

        // Deterministic chain order: sort namespaces so the built sources — and
        // therefore the resolution chain — are stable across runs.
        let mut namespaces: Vec<&String> = registries
            .iter()
            .filter(|(namespace, entry)| is_published_namespace(entry, namespace, local_mirrors))
            .map(|(namespace, _)| namespace)
            .collect();
        namespaces.sort();

        let allow_yanked = env::flag(env::keys::OCX_ALLOW_YANKED, false);
        let mut sources = Vec::with_capacity(namespaces.len());
        for namespace in namespaces {
            // Per-namespace physical-fetch client: same mirror + plain-HTTP +
            // progress config as the shared remote client, PLUS an SSRF
            // `GuardedResolver` seeded with THIS namespace's `trusted_hosts`
            // (X1-X3, ocx#218). The resolver pins the connect address to the one
            // `physical_identifier` validated, so a root `repository` host cannot
            // rebind to a forbidden range between validate and connect. The trust
            // set is per-namespace (never a union) so one namespace's exemption
            // can never widen another's.
            let trusted_hosts = registries
                .get(namespace)
                .and_then(|entry| entry.trusted_hosts.clone())
                .unwrap_or_default();
            let client = oci::ClientBuilder::new()
                .plain_http_registries(insecure_hosts.to_vec())
                .mirrors(registry_mirrors.clone())
                .progress(progress.clone())
                .ssrf_guard(trusted_hosts.clone())
                .build();
            // The base URL and its transport are one decision, taken inside
            // `resolve_base_url` — picking a transport here would re-derive the
            // scheme the gate there already settled.
            let base = index::OcxIndex::resolve_base_url(config, namespace, mirrors_index, insecure_hosts)?;
            sources.push(index::OcxIndex::new(index::OcxIndexConfig {
                transport: base.transport,
                base_url: base.url,
                namespace: namespace.clone(),
                client,
                allow_yanked,
                trusted_hosts,
            }));
        }
        Ok(sources)
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

    /// Operator-tier `[trust.sigstore]` from the merged `config.toml` — the
    /// self-hosted Fulcio/Rekor trust root a fleet ships instead of every
    /// machine carrying a file or an env var.
    pub fn config_trust_sigstore(&self) -> Option<&ocx_lib::trust::SigstoreTrust> {
        self.config_trust.sigstore.as_ref()
    }

    /// Hosts this invocation may contact over plain HTTP — the resolved union
    /// of `[registries."<name>"].insecure` and `OCX_INSECURE_REGISTRIES`.
    /// Commands that build their own registry connection take this rather than
    /// re-deriving it, so every path agrees on the same set.
    pub fn insecure_hosts(&self) -> &[String] {
        &self.insecure_hosts
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

    /// The discovered tiers alone (built-in defaults, system, user,
    /// `$OCX_HOME`) — what a managed payload folds ONTO. Paired with
    /// [`Self::config_overlay`] so `ocx config test` can reproduce the adoption
    /// order for a candidate payload.
    pub fn config_base(&self) -> &ocx_lib::Config {
        &self.config_base
    }

    /// The explicit `OCX_CONFIG` / `--config` tier alone — what merges ON TOP
    /// of a managed payload, and therefore on top of a previewed candidate.
    pub fn config_overlay(&self) -> &ocx_lib::Config {
        &self.config_overlay
    }

    /// The locally-authored `[mirrors]` table, for callers that must re-ask
    /// [`is_published_namespace`] after init — `ocx index regenerate`'s
    /// published-only guard is the only one today.
    pub fn local_mirrors(&self) -> Option<&std::collections::HashMap<String, ocx_lib::MirrorConfig>> {
        self.local_mirrors.as_ref()
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

    /// Returns the registry [`oci::Client`] `ocx package verify` reads through,
    /// in every mode — including `--offline`. Built on first call and cached
    /// (see [`Self::registry_client_cell`] doc) — never gated on
    /// `[[trust.policy]]` configuration, so an unconfigured trust set with
    /// explicit `--certificate-identity`/`--certificate-oidc-issuer` flags
    /// still gets a client under `--offline`.
    ///
    /// Unlike the offline-gated [`Self::remote_client`], this never fails on
    /// `--offline`: verify inherently reads the artifact and its signature
    /// referrer from the registry where they live (a local mirror in air-gapped
    /// deployments), so its `--offline` semantics scope to the Sigstore trust
    /// services (the Rekor key fetch and TUF), not the artifact registry. Pair
    /// it with [`Self::is_offline`], which the verify pipeline uses to forbid
    /// trust-services network and require cached/supplied trust material.
    pub fn verify_client(&self) -> &oci::Client {
        self.registry_client_cell
            .get_or_init(|| build_registry_client(&self.mirror_map, &self.progress, &self.insecure_hosts))
    }
}

/// Every namespace's `[registries."<ns>"].trusted_hosts` SSRF exemption
/// (ocx#218), keyed by namespace and skipping namespaces that declare none.
///
/// Config is the single source of truth for the exemption:
/// [`Context::build_index_sources`] reads the same field into each `OcxIndex`
/// and each per-namespace `ssrf_guard`, and this map is that same value read
/// once more for the chained index. It deliberately covers EVERY namespace, not
/// only the index-bearing ones `build_index_sources` builds a source for — an
/// operator who declared where a namespace's traffic may go has answered the
/// question for that namespace, whether or not it also declares an `index`, and
/// a namespace with no source has no other place to carry the exemption.
fn trusted_hosts_by_namespace(config: &ocx_lib::Config) -> std::collections::HashMap<String, Vec<String>> {
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

/// Builds the registry client backing [`Context::registry_client_cell`].
///
/// Extracted so the lazy on-demand build ([`Context::verify_client`]) and the
/// two eager call sites in [`Context::try_init`] that already need a network
/// client (the online `remote_client`/`oci_index` pair, and policy-gated
/// auto-verify) share one construction — no drift between "built lazily" and
/// "built eagerly" client shapes.
fn build_registry_client(
    mirror_map: &oci::MirrorMap,
    progress: &ocx_lib::cli::progress::ProgressManager,
    insecure_hosts: &[String],
) -> oci::Client {
    oci::ClientBuilder::new()
        .plain_http_registries(insecure_hosts.to_vec())
        .mirrors(mirror_map.clone())
        .progress(progress.clone())
        .build()
}

/// Builds the client that FETCHES the managed-config payload.
///
/// Every input is the LOCAL-ONLY view or the raw environment — the merged
/// config, and the process-wide `insecure_hosts` derived from it, are
/// deliberately out of scope here rather than merely unused.
/// `adr_managed_config_tier.md`, "Mirror posture (AMENDED)": the managed
/// fetch's route derives from local tiers only, so the payload can never
/// influence its own refresh route — and dropping TLS *is* a redirection on
/// the transport axis. A managed snapshot declaring
/// `[registries."<its own host>"] insecure = true` would otherwise downgrade
/// the fetch of the NEXT snapshot to plaintext, which is exactly the
/// self-authorization the mirror half of this client was already written to
/// prevent.
///
/// # Errors
///
/// Propagates [`ocx_lib::MirrorConfigError`] when the
/// local-only `[mirrors]` plus the forwarded `OCX_MIRRORS` do not resolve —
/// including an `http://` mirror this view licenses nothing for.
fn build_managed_config_client(
    local_only_config: &ocx_lib::Config,
    env_mirrors: Vec<(String, ocx_lib::MirrorConfig)>,
    env_insecure_registries: &[String],
    progress: &ocx_lib::cli::progress::ProgressManager,
) -> anyhow::Result<oci::Client> {
    let insecure_hosts = ocx_lib::insecure_hosts(local_only_config, env_insecure_registries);
    let mirrors =
        ocx_lib::resolve_mirror_map(local_only_config, env_mirrors, &insecure_hosts).map_err(anyhow::Error::new)?;
    Ok(oci::ClientBuilder::new()
        .plain_http_registries(insecure_hosts)
        .mirrors(oci::MirrorMap::new(mirrors.registry))
        .progress(progress.clone())
        .build())
}

/// Build the shared policy-gated auto-verify inputs, or `None` when no operator
/// `[[trust.policy]]` is configured.
///
/// Returns the `AutoVerifyInput` rather than the wrapped `AutoVerify` so the
/// resolved endpoint stays readable — `AutoVerify` keeps its fields private,
/// and a test that cannot see the Rekor URL cannot pin it.
///
/// Attached once on the manager (every install surface inherits it). Carries the
/// always-available registry client (verify reads the signature referrer from
/// the registry even under `--offline`), the offline flag, the
/// `OCX_SIGSTORE_TRUSTED_ROOT` override, the operator `[trust.sigstore]` block,
/// the `$OCX_HOME/sigstore/trusted-root.json` convention path, and the
/// `OCX_NO_VERIFY` opt-out default (install/pull refine it from their flag).
/// OCI-tier gating uses the operator `config.toml` set only; the project
/// `ocx.toml` pool stays empty (no new OCI-tier carve-out).
fn build_auto_verify(
    operator_policies: Vec<ocx_lib::trust::TrustPolicy>,
    sigstore_trust: Option<ocx_lib::trust::SigstoreTrust>,
    registry_client: &oci::Client,
    offline: bool,
    state: StateStore,
    user_opted_out: bool,
) -> anyhow::Result<Option<package_manager::AutoVerifyInput>> {
    if operator_policies.is_empty() {
        return Ok(None);
    }
    // Endpoint precedence, minus the flag tier install/pull do not have:
    // `[trust.sigstore].rekor_url` > builtin default. Auto-verify keys its
    // trust-root cache by this URL, so an operator on a self-hosted stack whose
    // auto-verify still said `rekor.sigstore.dev` cached their private root
    // under the public-good key.
    //
    // Validated (not parsed by name) so the CLI never names `url::Url`, and
    // fallible now that the value can come from config: a bad
    // `[trust.sigstore].rekor_url` must fail the run, not panic and not
    // silently fall back to the public good. Unused when the trust root pins
    // the Rekor key (the `OCX_SIGSTORE_TRUSTED_ROOT` / offline path).
    let rekor = resolve_endpoint(None, sigstore_trust.as_ref(), SigstoreEndpoint::Rekor);
    let rekor_url = oci::endpoint::validate_sigstore_url(&rekor, "[trust.sigstore].rekor_url")?;
    Ok(Some(package_manager::AutoVerifyInput {
        operator_policies,
        // ponytail: seam for the deferred project-tier auto-verify (#99 known gap
        // — `ocx.toml` policies not yet read on OCI-tier install/pull/exec/env/run
        // surfaces, operator `config.toml` only today). Wire real project policies
        // here once that follow-up is scheduled; until then, always empty.
        project_policies: Vec::new(),
        registry_client: registry_client.clone(),
        rekor_url,
        offline,
        state,
        // Through the same door `ocx package verify` reads it at, so one env
        // value cannot mean a bare path here and a `file://` one there.
        // Through the same door `ocx package verify` reads it at, so one env
        // value cannot mean a bare path here and a `file://` one there.
        trusted_root_env: std::env::var_os("OCX_SIGSTORE_TRUSTED_ROOT")
            .map(PathBuf::from)
            .map(explicit_trust_root_path),

        sigstore_trust,
        home_trusted_root: ocx_lib::ConfigLoader::home_sigstore_trusted_root_path(),
        user_opted_out,
    }))
}

/// Whether `<namespace>` resolves through the ocx-index protocol — the single
/// published/derived test, shared by every consumer that needs the answer.
///
/// Two conditions, and **both** are load-bearing:
///
/// 1. `index` present and non-empty. `index = ""` is the documented kill
///    switch: an empty base URL is not a kind marker, so the namespace resolves
///    as plain OCI.
/// 2. Not a compiled-in default that a **locally-authored** `[mirrors."<ns>"]`
///    entry pins at a registry. That operator declared where the namespace's
///    traffic goes, and honouring both would start dialling a host they never
///    allow-listed — see [`Context::build_index_sources`]'s doc for the full
///    scoping argument.
///
/// It is a free function rather than a closure inside `build_index_sources`
/// because `ocx index regenerate`'s published-only guard (C-010) must reach the
/// same verdict, and it cannot consult the built sources instead: those are
/// empty under `--offline`, which C-021 requires `regenerate` to permit. A
/// guard that restated only condition 1 would mint a `c/index.json` under a
/// mirror-pinned namespace the resolver routes as derived — the exact outcome
/// C-010 exists to prevent.
///
/// [`Context::build_index_sources`]: Context
pub fn is_published_namespace(
    entry: &ocx_lib::RegistryConfig,
    namespace: &str,
    local_mirrors: Option<&std::collections::HashMap<String, ocx_lib::MirrorConfig>>,
) -> bool {
    if entry.index.as_deref().is_none_or(str::is_empty) {
        return false;
    }
    // A bare-string `[mirrors]` entry sets `registry`, so it counts too.
    let locally_pinned_at_a_mirror =
        local_mirrors.is_some_and(|table| table.get(namespace).is_some_and(|entry| entry.registry.is_some()));
    if entry.index_is_compiled_default && locally_pinned_at_a_mirror {
        // `build_index_sources` feeds this from the MERGED config, so a
        // namespace key can come from the managed tier rather than from the
        // operator — neutralized for the same reason the report payloads are.
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

    /// One `[[trust.policy]]`, enough to make `build_auto_verify` return
    /// `Some` — every field is optional at the serde layer.
    fn one_policy() -> Vec<ocx_lib::trust::TrustPolicy> {
        vec![serde_json::from_str("{}").expect("an all-default trust policy parses")]
    }

    /// Call `build_auto_verify` with `sigstore` as the only varying input.
    fn auto_verify_input(
        sigstore: Option<ocx_lib::trust::SigstoreTrust>,
    ) -> anyhow::Result<Option<package_manager::AutoVerifyInput>> {
        build_auto_verify(
            one_policy(),
            sigstore,
            &oci::ClientBuilder::new().build(),
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
        use ocx_lib::oci::verify::trust_cache::cache_key_for_rekor;

        let configured = ocx_lib::trust::SigstoreTrust {
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
            ocx_lib::oci::endpoint::DEFAULT_REKOR_URL,
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
        let hostile = ocx_lib::trust::SigstoreTrust {
            // Plain http off loopback: refused by the same SSRF guard the flag
            // tier hits.
            rekor_url: Some("http://rekor.corp.example".to_string()),
            ..Default::default()
        };
        let Err(err) = auto_verify_input(Some(hostile)) else {
            panic!("a rejected Rekor URL must fail the run");
        };
        assert_eq!(
            crate::app::classify_error(err.as_ref()),
            ExitCode::UsageError,
            "a rejected endpoint URL exits 64 whichever tier supplied it"
        );
    }

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

    // ── `registries."<ns>".index` presence gates `OcxIndex` construction,
    //    per NAMESPACE (`adr_index_indirection.md` F5a) ──────────────────────
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
    fn config_with_registries(entries: &[(&str, Option<&str>)]) -> ocx_lib::Config {
        let mut registries = std::collections::HashMap::new();
        for (namespace, index) in entries {
            registries.insert(
                namespace.to_string(),
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

    fn source_namespaces(sources: &[index::OcxIndex]) -> Vec<String> {
        sources.iter().map(|source| source.namespace().to_string()).collect()
    }

    /// Calls [`Context::build_index_sources`] with the physical-client inputs the
    /// wiring tests don't vary (an empty registry mirror map, no plain-HTTP hosts,
    /// disabled progress). `online` and the two config maps are what these tests
    /// exercise.
    fn build_test_sources(
        online: bool,
        config: &ocx_lib::Config,
        mirrors_index: &std::collections::BTreeMap<String, ocx_lib::ParsedMirror>,
    ) -> ocx_lib::Result<Vec<index::OcxIndex>> {
        Context::build_index_sources(
            online,
            config,
            None,
            mirrors_index,
            &oci::MirrorMap::default(),
            &[],
            &ocx_lib::cli::progress::ProgressManager::disabled(),
        )
    }

    #[test]
    fn build_index_sources_is_empty_without_an_index_bearing_registry() {
        // No `[registries]` table, and an entry with no `index` field at all,
        // both yield no index sources — presence of `index` specifically is
        // the sole selector (ADR F5a).
        let mirrors = std::collections::BTreeMap::new();

        let empty = build_test_sources(true, &ocx_lib::Config::default(), &mirrors).unwrap();
        assert!(empty.is_empty(), "no [registries] table must build no index sources");

        let index_absent = config_with_registries(&[(oci::OCX_SH_REGISTRY, None)]);
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
        let config = config_with_registries(&[(oci::OCX_SH_REGISTRY, Some("https://index.ocx.sh"))]);
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
            (oci::OCX_SH_REGISTRY, Some("https://index.ocx.sh")),
            ("corp.example", Some("https://index.corp.example")),
            ("plain.example", None),
        ]);

        let sources = build_test_sources(true, &config, &std::collections::BTreeMap::new()).unwrap();

        assert_eq!(
            source_namespaces(&sources),
            vec!["corp.example".to_string(), oci::OCX_SH_REGISTRY.to_string()],
            "one index source per index-bearing namespace, sorted, and never for a plain-OCI entry"
        );
    }

    #[test]
    fn build_index_sources_never_leaks_trusted_hosts_across_namespaces() {
        // Two index-bearing namespaces, each with its OWN, DIFFERENT
        // `trusted_hosts` set (X2, the SSRF escape hatch). Pins that a built
        // `OcxIndex` carries exactly its own namespace's set — never the
        // other namespace's, and never the union — so a future "share one
        // client across namespaces" refactor cannot silently widen one
        // namespace's trust exemption into another's.
        let mut registries = std::collections::HashMap::new();
        registries.insert(
            "ns-a".to_string(),
            ocx_lib::RegistryConfig {
                index: Some("https://index.a.example".to_string()),
                trusted_hosts: Some(vec!["10.0.0.0/8".to_string()]),
                ..Default::default()
            },
        );
        registries.insert(
            "ns-b".to_string(),
            ocx_lib::RegistryConfig {
                index: Some("https://index.b.example".to_string()),
                trusted_hosts: Some(vec!["192.168.0.0/16".to_string()]),
                ..Default::default()
            },
        );
        let config = ocx_lib::Config {
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
            ocx_lib::RegistryConfig {
                index: Some("https://index.indexed.example".to_string()),
                trusted_hosts: Some(vec!["10.0.0.0/8".to_string()]),
                ..Default::default()
            },
        );
        registries.insert(
            "plain.example".to_string(),
            ocx_lib::RegistryConfig {
                trusted_hosts: Some(vec!["192.168.0.0/16".to_string()]),
                ..Default::default()
            },
        );
        registries.insert("silent.example".to_string(), ocx_lib::RegistryConfig::default());
        let config = ocx_lib::Config {
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
            trusted_hosts_by_namespace(&ocx_lib::Config::default()).is_empty(),
            "no [registries] table means no exemptions anywhere"
        );
    }

    // ── `build_index_sources` threads `mirrors_index` (F5c) ──────────────────
    //
    // Traces: mirror-invariant audit 2026-07-19, gap G8. `build_index_sources`
    // hands its `mirrors_index` and `insecure_hosts` parameters straight
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
            ocx_lib::parse_url("http://mirror.example").unwrap(),
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
            ocx_lib::parse_url("http://unrelated.example").unwrap(),
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
            ocx_lib::parse_url("http://index.example").unwrap(),
        );

        // Ground truth: the exact call `build_index_sources` makes internally.
        let direct = index::OcxIndex::resolve_base_url(&config, "ns", &mirrors_index, &[])
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
        let config = config_with_registries(&[(oci::OCX_SH_REGISTRY, Some(""))]);
        let built = build_test_sources(true, &config, &std::collections::BTreeMap::new()).unwrap();
        assert!(
            built.is_empty(),
            "index = \"\" must build no index source — an empty base URL is not a kind marker"
        );
    }

    /// Builds a `Config` whose single `ocx.sh` entry carries the compiled-in
    /// index exactly as `ConfigLoader::builtin_defaults` stamps it.
    fn config_with_compiled_default() -> ocx_lib::Config {
        let mut registries = std::collections::HashMap::new();
        registries.insert(
            oci::OCX_SH_REGISTRY.to_string(),
            ocx_lib::RegistryConfig {
                index: Some("https://index.ocx.sh".to_string()),
                index_is_compiled_default: true,
                ..Default::default()
            },
        );
        ocx_lib::Config {
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
    ) -> std::collections::HashMap<String, ocx_lib::MirrorConfig> {
        let mut table = std::collections::HashMap::new();
        table.insert(
            host.to_string(),
            ocx_lib::MirrorConfig {
                registry: registry.map(str::to_string),
                index: index.map(str::to_string),
                ..Default::default()
            },
        );
        table
    }

    /// Builds sources with a mirror map that is present in the MERGED views
    /// (what the OCI client and index-role resolver see) but attributed to a
    /// caller-chosen local view — the seam N1 turns on.
    fn build_with_views(
        config: &ocx_lib::Config,
        local_mirrors: Option<&std::collections::HashMap<String, ocx_lib::MirrorConfig>>,
        merged_registry_mirror: Option<(&str, &str)>,
        merged_index_mirror: Option<(&str, &str)>,
    ) -> Vec<index::OcxIndex> {
        let registry_mirrors = merged_registry_mirror.map_or_else(oci::MirrorMap::default, |(host, url)| {
            oci::MirrorMap::new([(host.to_string(), ocx_lib::parse_url(url).unwrap())])
        });
        let mut mirrors_index = std::collections::BTreeMap::new();
        if let Some((host, url)) = merged_index_mirror {
            mirrors_index.insert(host.to_string(), ocx_lib::parse_url(url).unwrap());
        }
        Context::build_index_sources(
            true,
            config,
            local_mirrors,
            &mirrors_index,
            &registry_mirrors,
            &[],
            &ocx_lib::cli::progress::ProgressManager::disabled(),
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
                oci::OCX_SH_REGISTRY,
                Some("https://artifactory.corp/ocx-remote"),
                None,
            )),
            Some((oci::OCX_SH_REGISTRY, "https://artifactory.corp/ocx-remote")),
            None,
        );
        assert!(
            built.is_empty(),
            "a local [mirrors.\"ocx.sh\"] registry-role entry must suppress the compiled-in index"
        );
    }

    /// **N1, the attack.** The managed tier is a remote, operator-published
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
            Some((oci::OCX_SH_REGISTRY, "https://attacker.example/ocx")),
            Some((oci::OCX_SH_REGISTRY, "https://attacker.example/ocx")),
        );
        assert_eq!(
            source_namespaces(&built),
            vec![oci::OCX_SH_REGISTRY.to_string()],
            "a mirror from the untrusted managed tier must not revoke the verified index path"
        );
    }

    /// **N2.** The index role is only ever applied keyed on the index base's
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
                oci::OCX_SH_REGISTRY,
                None,
                Some("https://corp-index.example"),
            )),
            None,
            Some((oci::OCX_SH_REGISTRY, "https://corp-index.example")),
        );
        assert_eq!(
            source_namespaces(&built),
            vec![oci::OCX_SH_REGISTRY.to_string()],
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
            &config_with_registries(&[(oci::OCX_SH_REGISTRY, Some("https://index.ocx.sh"))]),
            Some(&local_mirror_table(
                oci::OCX_SH_REGISTRY,
                Some("https://artifactory.corp/ocx-remote"),
                None,
            )),
            Some((oci::OCX_SH_REGISTRY, "https://artifactory.corp/ocx-remote")),
            None,
        );
        assert_eq!(
            source_namespaces(&built),
            vec![oci::OCX_SH_REGISTRY.to_string()],
            "a written [registries.\"ocx.sh\"] index outranks the mirror suppression"
        );
    }

    /// Host-keyed precision: the mirror entry that routes the INDEX's own
    /// traffic host (`index.ocx.sh`, the F5c override) redirects the index
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
            vec![oci::OCX_SH_REGISTRY.to_string()],
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
            (oci::OCX_SH_REGISTRY, Some("https://index.ocx.sh")),
            ("corp.example", Some("https://index.corp.example")),
        ]);
        let index_sources = build_test_sources(true, &config, &std::collections::BTreeMap::new()).unwrap();
        let oci_index = index::OciIndex::new(index::OciIndexConfig {
            client: oci::ClientBuilder::new().build(),
        });

        let (mode, sources) =
            Context::chain_mode_and_sources(Some(&oci_index), &index_sources, index::ChainMode::Default);

        assert_eq!(mode, index::ChainMode::Default);
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
        let client = oci::ClientBuilder::new().build();
        let oci_index = index::OciIndex::new(index::OciIndexConfig { client });

        let (mode, sources) = Context::chain_mode_and_sources(Some(&oci_index), &[], index::ChainMode::Default);

        assert_eq!(mode, index::ChainMode::Default);
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
        let mut view = ocx_lib::env::OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        view.frozen = true;
        // offline=true → oci_index=None; the guard must accept the combination.
        assert!(
            check_frozen_remote_exclusivity(&view).is_ok(),
            "--frozen + --offline must pass the exclusivity guard"
        );

        // Replicate the mode-selection match from try_init:
        // offline=true produces oci_index=None → Offline wins, ignoring frozen.
        let oci_index: Option<index::OciIndex> = None; // simulates offline=true
        let frozen = true;
        let mode: index::ChainMode = match &oci_index {
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
                ocx_lib::MirrorConfig {
                    registry: Some(format!("http://{host}")),
                    ..Default::default()
                },
            )]
        };
        let with_allowance = |granted: bool| {
            let entry = ocx_lib::RegistryConfig {
                insecure: granted.then_some(true),
                ..Default::default()
            };
            ocx_lib::Config {
                registries: Some(std::collections::HashMap::from([(host.to_string(), entry)])),
                ..Default::default()
            }
        };
        let progress = ocx_lib::cli::progress::ProgressManager::disabled();

        let refused = build_managed_config_client(&with_allowance(false), mirrors(), &[], &progress);
        assert!(
            refused.is_err(),
            "the payload's own allowance is not in scope for the tier that fetches it"
        );

        let allowed = build_managed_config_client(&with_allowance(true), mirrors(), &[], &progress);
        assert!(
            allowed.is_ok(),
            "the same mirror IS allowed once the view actually handed to the builder grants it: {:?}",
            allowed.err()
        );
    }
}
