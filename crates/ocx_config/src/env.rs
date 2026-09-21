// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::records::RecordsOptions;
// The accessor moved to `ocx_util::env` (the future `ocx_util::env`); this
// module reads the environment through it like every other consumer.
use ocx_util::env::{flag, string, var};

/// Canonical names for `OCX_*` environment variables read or written by ocx.
///
/// Single source of truth so spawn helpers, config loaders, docs, and tests
/// reference the same string in one place.
pub mod keys {
    /// Absolute path to the running `ocx` executable. Set on every subprocess
    /// spawn so child ocx invocations (e.g. via generated entrypoint launchers)
    /// pin to the same binary instead of whatever `$PATH` happens to resolve.
    /// Named `_PIN` to make the pin semantics explicit — the value is
    /// the specific binary that was running when the package was installed.
    pub const OCX_BINARY_PIN: &str = "OCX_BINARY_PIN";
    /// The OCX data root every store is rooted at — `$OCX_HOME`, else
    /// `~/.ocx`. Resolved through [`crate::home::default_ocx_root`], the one
    /// definition of that fallback.
    ///
    /// Forwarded by [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) **set-always**,
    /// never removed — the second key here with that shape, beside
    /// [`OCX_BINARY_PIN`]. Unlike [`OCX_CONFIG`] and its siblings there is no
    /// "parent resolved none" state a stale inherited value could beat: a
    /// resolvable root *is* what the parent resolved, so the value written
    /// here is the home the parent's own stores used, and an ambient
    /// `OCX_HOME=""` is corrected to the absolute fallback rather than
    /// travelling onward as the empty string.
    ///
    /// Without that forwarding `ocx exec --clean` sent a child into a
    /// *different* home: `--clean` strips `HOME` too, so a generated
    /// entrypoint launcher's `ocx launcher exec` re-entry resolved `~/.ocx`
    /// from the passwd database, read another config chain and looked for the
    /// package the parent had just materialized in a store that did not have
    /// it (ocx-sh/ocx#488).
    pub const OCX_HOME: &str = "OCX_HOME";
    /// Boolean — disables network access when truthy. Mirrors `--offline`.
    pub const OCX_OFFLINE: &str = "OCX_OFFLINE";
    /// Boolean — freezes tag resolution to the local index when truthy:
    /// an unpinned (tag-only) reference missing from the local index errors
    /// instead of being fetched and committed. Digest-pinned content still
    /// fetches over the network. Mirrors `--frozen`.
    pub const OCX_FROZEN: &str = "OCX_FROZEN";
    /// Boolean — uses the remote index by default when truthy. Mirrors `--remote`.
    pub const OCX_REMOTE: &str = "OCX_REMOTE";
    /// Path to an explicit configuration file. Mirrors `--config`.
    pub const OCX_CONFIG: &str = "OCX_CONFIG";
    /// Boolean — when truthy, skip the discovered config-tier chain
    /// (system / user / `$OCX_HOME`). Explicit `--config` / [`OCX_CONFIG`]
    /// paths still load; a SYSTEM-scope lock survives regardless.
    /// Resolution-affecting → forwarded to child ocx processes via
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config), so a launcher re-entry stays as hermetic as
    /// the frame that spawned it instead of re-reading the whole chain.
    pub const OCX_NO_CONFIG: &str = "OCX_NO_CONFIG";
    /// Path to an explicit project `ocx.toml` (project-tier toolchain config).
    /// Mirrors `--project`.
    pub const OCX_PROJECT: &str = "OCX_PROJECT";
    /// Boolean — when truthy, skip the CWD walk and the [`OCX_PROJECT`]
    /// env var. Explicit `--project` paths still load.
    pub const OCX_NO_PROJECT: &str = "OCX_NO_PROJECT";
    /// Boolean — when truthy, the seven project-scoped commands that stamp
    /// shell-activation consent as a side effect (`add`, `remove`, `lock`,
    /// `update`, `pull`, `exec`, `init`) run without writing one.
    ///
    /// For a caller that is not a consenting human: build tooling drives
    /// `ocx --project <abs> pull` against a checkout it did not choose, and a
    /// stamp there grants `Grant::Stamp`, which authorizes the project's own
    /// `[env]` table on every later `cd` (ocx-sh/ocx#400).
    ///
    /// **Outranked by the flag.** `--consent` / `--no-consent` on the commands
    /// that carry the pair decides first; this speaks only where neither was
    /// given. Read at exactly one site,
    /// `app::project_context::record_activation_consent_over` — the point every
    /// automatic writer routes through.
    ///
    /// **`ocx shell allow` ignores it.** That command is the explicit gesture
    /// this variable exists to distinguish machine invocation *from*, and it
    /// calls `project::consent::record` directly, past the seam.
    ///
    /// Forwarded to child ocx processes via [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config), from
    /// either the ambient value or an `ocx exec --no-consent` on the frame that
    /// spawns them. `ocx exec` composes its child from `shell::reconcile::inherited_env` by
    /// default and from [`Env::clean`](crate::env::Env::clean) under `--clean`, so the ambient half
    /// earns its keep on that second arm; the flag half has no other channel at
    /// all, because argv does not cross a spawn. Without both, a script `ocx
    /// exec` runs that itself calls `ocx pull` would stamp after all.
    pub const OCX_NO_CONSENT: &str = "OCX_NO_CONSENT";
    /// Boolean — when truthy, select the global toolchain
    /// (`$OCX_HOME/ocx.toml`) as the in-effect project file instead of
    /// discovering one via the CWD walk. Mirrors `--global`.
    pub const OCX_GLOBAL: &str = "OCX_GLOBAL";
    /// Path to the local index directory. Mirrors `--index`.
    pub const OCX_INDEX: &str = "OCX_INDEX";
    /// Boolean — when truthy, a tag resolving to a yanked entry on a static-file
    /// index (`index.ocx.sh`) is allowed instead of refused. A yank is a
    /// publisher signal, not a delete, so the override is explicit and opt-in.
    /// Resolution-affecting → forwarded to child ocx processes via
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config).
    pub const OCX_ALLOW_YANKED: &str = "OCX_ALLOW_YANKED";
    /// Path to the active patch snapshot file (`patches.snapshot.json`).
    ///
    /// When set, the compose overlay prefers the snapshot's pinned digests
    /// over live tag lookups for companions — enabling reproducible builds
    /// without a network round-trip.  Written by `ocx patch freeze`.
    /// Resolution-affecting → forwarded to child ocx processes via
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) so launchers apply the same frozen tier.
    pub const OCX_PATCH_SNAPSHOT: &str = "OCX_PATCH_SNAPSHOT";
    /// JSON object mapping an upstream traffic host to its mirror value —
    /// either a bare string (both traffic roles) or a `{"registry"?, "index"?}`
    /// object (per-role split), the same F5b union `[mirrors."<host>"]`
    /// accepts in TOML (e.g. `{"ghcr.io":"https://artifactory.example.com/ghcr-remote",
    /// "index.ocx.sh":{"index":"https://artifactory.corp/ocx-index"}}`).
    /// Resolution-affecting → forwarded to child ocx processes. A single JSON
    /// object is used (not a comma/`=` list) because mirror values are
    /// structured URLs with no delimiter-safe separator.
    pub const OCX_MIRRORS: &str = "OCX_MIRRORS";
    /// JSON object encoding the resolved `[patches]` config
    /// (`ResolvedPatchConfig`). Resolution-affecting → forwarded to child ocx
    /// processes so launchers re-apply the same patch tier (C5 across process
    /// boundaries). Forwarded only when `[patches]` is configured; absent
    /// otherwise. Mirrors `OCX_MIRRORS` in forwarding semantics.
    pub const OCX_PATCHES: &str = "OCX_PATCHES";
    /// JSON envelope carrying the resolved project/group `[env]` entries plus
    /// any `ocx exec --env` overrides, forwarded so a generated entrypoint
    /// launcher's re-entry (`ocx launcher exec`) applies the same project env
    /// the parent composed.
    ///
    /// Without it the launcher silently loses those entries: it builds a fresh
    /// `Env` from the inherited environment, then re-applies the package's own
    /// entries on top — reverting exactly the overrides the project declared,
    /// with no signal to the user. A package that declares entrypoints
    /// resolves *through* the launcher on the ordinary `ocx exec` path, so this
    /// is the primary path, not a corner case.
    ///
    /// **Decode is untrusted input and fails closed on the whole payload.** An
    /// entry whose key is reserved (`OCX_*` / `__OCX_*`) or whose modifier
    /// `kind` is unrecognized rejects the entire envelope rather than being
    /// skipped: a misread `kind` would apply a value with the wrong
    /// combination semantics and silently produce a wrong environment. This is
    /// where [`OCX_PATCHES`]' leniency must NOT be copied — a forged
    /// `no_patches` can only suppress an overlay, whereas a forged entry here
    /// can set a value.
    ///
    /// Carries no version discriminator, matching [`OCX_PATCHES`]: the
    /// envelope is not where this can break across versions — an unknown
    /// modifier `kind` is, and rejecting that directly is strictly better than
    /// a version field an older binary could not act on anyway.
    pub const OCX_ENV: &str = "OCX_ENV";
    /// Boolean — when truthy, `ocx self setup` writes the env shims but does
    /// NOT modify any shell profile. Mirrors `--no-modify-path`. Not a
    /// resolution-affecting flag (not forwarded to child ocx); the opt-out is
    /// not remembered between runs.
    pub const OCX_NO_MODIFY_PATH: &str = "OCX_NO_MODIFY_PATH";
    /// OCI reference for the managed-config artifact (plain string, like
    /// [`OCX_CONFIG`]). Overrides `[managed].source` for this invocation only
    /// — never written back to disk; `ocx self setup --managed-config` is the
    /// only writer of the seed. Resolution-affecting → forwarded to child ocx
    /// processes via [`crate::env::Env::apply_ocx_config`]. Runtime
    /// `OCX_MANAGED_CONFIG=""` is treated as unset (matches [`OCX_CONFIG`]).
    /// Suppressed (read side) by [`OCX_NO_CONFIG`] — hermetic means hermetic.
    pub const OCX_MANAGED_CONFIG: &str = "OCX_MANAGED_CONFIG";
    /// Boolean — kill switch for the managed-config background refresh tick
    /// (both `notify` and `apply` postures). Distinct from
    /// `OCX_NO_UPDATE_CHECK` — an independently silenceable concern. Explicit
    /// `ocx config update` still works when this is set.
    pub const OCX_NO_CONFIG_REFRESH: &str = "OCX_NO_CONFIG_REFRESH";
    /// Directory execution records are written to — the env tier of `[records]
    /// dir` / `--records-dir`. Absent ⇒ recording is off.
    /// Resolution-affecting → forwarded to child ocx processes via
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config), so every frame of one launch chain (an
    /// `ocx exec` and the `ocx launcher exec` it spawns) records into the sink
    /// the outermost invocation resolved. Read back by
    /// `crate::record::RecordsOptions::from_env`;
    /// `OCX_RECORDS_DIR=""` is treated as unset (matches [`OCX_CONFIG`]).
    pub const OCX_RECORDS_DIR: &str = "OCX_RECORDS_DIR";
    /// Filename template for each execution record — the env tier of
    /// `[records] name` / `--records-name`. Forwarded alongside
    /// [`OCX_RECORDS_DIR`], because a collector that globs or parses filenames
    /// depends on the pattern exactly as much as on the directory.
    ///
    /// There is deliberately no `OCX_RECORDS_REQUIRED`. The fail-closed posture
    /// is an operator decision read from the config file at every tier, never
    /// from the environment or a flag.
    pub const OCX_RECORDS_NAME: &str = "OCX_RECORDS_NAME";
    /// `crate::lazy::LazyMode` wire value (`never` /
    /// `always`) — the least specific tier of the `lazy-mode` resolution
    /// ladder (`plan_lazy_package_loading.md` C-006), below `ocx.toml`'s
    /// toolchain/group/package tiers and the `--lazy-mode` CLI flag.
    ///
    /// **Not** resolution-affecting in the [`OcxConfigView`](crate::env::OcxConfigView) sense: it
    /// changes *when* a tool's content materializes, never *which* digest
    /// resolves, so it is deliberately absent from [`OcxConfigView`](crate::env::OcxConfigView) and
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) never forwards it to a child ocx process.
    pub const OCX_LAZY_MODE: &str = "OCX_LAZY_MODE";
    /// `crate::lazy::LazyReport` wire value (`silent` /
    /// `progress`) — whether a shim's first-invocation materialization
    /// renders progress.
    ///
    /// **Not** resolution-affecting and **not** forwarded, same rationale as
    /// [`OCX_LAZY_MODE`].
    pub const OCX_LAZY_REPORT: &str = "OCX_LAZY_REPORT";
    /// `crate::activate::ActivateMode` wire value (`env` / `bin` / `none`) —
    /// how a rendered toolchain home reaches a shell
    /// (`plan_toolchain_activation.md` C-006).
    ///
    /// **The weakest tier of the `activate` ladder, not an override.** It sits
    /// *below* `ocx.toml`'s `activate` key, so a project that states a value
    /// wins over an exported value here; this variable only decides the outcome
    /// for a project that states none. Read through
    /// `crate::activate::ActivateMode::from_env`, which folds ASCII case and
    /// warns-and-falls-through on an unrecognised value rather than erroring.
    ///
    /// **Not** resolution-affecting in the [`OcxConfigView`](crate::env::OcxConfigView) sense: it changes
    /// how the toolchain reaches `PATH`, never *which* digest resolves, so it
    /// is deliberately absent from [`OcxConfigView`](crate::env::OcxConfigView) and
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) never forwards it — same rationale as
    /// [`OCX_LAZY_MODE`].
    pub const OCX_TOOLCHAIN_ACTIVATE: &str = "OCX_TOOLCHAIN_ACTIVATE";
    /// Boolean — whether composed paths pin to digest roots instead of
    /// following the rendered `<group>/<entry>` links
    /// (`plan_toolchain_activation.md` C-007).
    ///
    /// **The weakest tier of the `pinned` ladder, not an override.** Below
    /// both `--pinned` and `ocx.toml`'s `pinned` key, so it decides only for an
    /// invocation where neither speaks. Read through
    /// `crate::activate::pinned_from_env`, which yields `Option<bool>` —
    /// **not** through [`ocx_util::env::flag`], which collapses "unset" and "false" and would
    /// make an explicit `OCX_TOOLCHAIN_PINNED=false` indistinguishable from
    /// absence.
    pub const OCX_TOOLCHAIN_PINNED: &str = "OCX_TOOLCHAIN_PINNED";
    /// Path to the root under which project toolchain homes are rendered —
    /// the environment tier of `config.toml`'s `toolchain_dir`
    /// (`plan_toolchain_activation.md` C-008 / C-016).
    ///
    /// **Resolution-affecting**: it moves where the `links/<group>/<entry>`
    /// links and the `shells/default/bin` trampolines live, so every composed
    /// path a child ocx emits changes with it. Carried on [`OcxConfigView`](crate::env::OcxConfigView) and
    /// forwarded by [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config), which **removes** any inherited value when
    /// the parent resolved none — otherwise a stale parent-shell export beats
    /// the outer ocx's parsed state in every child.
    ///
    /// The containment refusals (C-017 root must be under `$HOME` /
    /// `$OCX_HOME`, C-018 never a system prefix, C-019 owner-owned and not
    /// group- or world-writable) apply to the **resolved** root whatever tier
    /// produced it — this variable included. A value refused when written in
    /// `config.toml` is refused identically when exported here (finding R-W2;
    /// WP-4 owns the refusals themselves).
    pub const OCX_TOOLCHAIN_DIR: &str = "OCX_TOOLCHAIN_DIR";
    /// Boolean — when truthy, skip the policy-gated auto-verify on
    /// `ocx package install` / `ocx package pull`. Env mirror of the
    /// per-command `--no-verify` flag (the flag wins). Forwarded to child ocx
    /// processes via [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) so a CI-wide opt-out reaches a
    /// launcher-spawned child install.
    pub const OCX_NO_VERIFY: &str = "OCX_NO_VERIFY";

    /// Operator-supplied extra CA root material — a path, or inline PEM text
    /// (a value containing `-----BEGIN`), for the registry, index, forge and
    /// Sigstore TLS clients (ocx#448). `OCX_EXTRA_CA_CERTS=""` is treated as
    /// unset, matching [`OCX_CONFIG`].
    ///
    /// **Not** forwarded to child processes — deliberately absent from
    /// [`OcxConfigView`](crate::env::OcxConfigView) and [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config): an env-only CA does not
    /// survive `ocx exec --clean` or reach the launcher it spawns, though the
    /// `config.toml` form of the same setting does, because the child re-reads
    /// disk for itself. Forwarding an inline PEM would also exceed Windows'
    /// 32,767-character per-variable limit and land in every launched tool's
    /// environment; a CA is public material, so scrubbing it like a credential
    /// would be the wrong posture too. See `ocx_util::tls::ExtraRoots`
    /// for the validated value this resolves to.
    pub const OCX_EXTRA_CA_CERTS: &str = "OCX_EXTRA_CA_CERTS";

    /// Short-lived OIDC bearer token for keyless Sigstore signing, read once by
    /// `ocx package sign` (lowest precedence, after `--identity-token-file` /
    /// `--identity-token-stdin`). A bearer credential: read directly via
    /// `std::env::var` and never forwarded to child processes — see
    /// [`CREDENTIAL_KEYS`] and the credential exemption table in `subsystem-cli.md`.
    pub const OCX_IDENTITY_TOKEN: &str = "OCX_IDENTITY_TOKEN";

    /// Password for an encrypted signing key, read once by the file key backend
    /// (`oci::sign::key_backend::key_password`). Never a flag — a password in
    /// `argv` is visible to every process on the host.
    ///
    /// A bearer credential in exactly the sense [`OCX_IDENTITY_TOKEN`] is: only
    /// the signing path needs it, so it is read directly via `std::env::var` and
    /// never forwarded to child processes — see [`CREDENTIAL_KEYS`] and the
    /// credential exemption table in `subsystem-cli.md`.
    pub const OCX_KEY_PASSWORD: &str = "OCX_KEY_PASSWORD";

    /// The signing key PEM itself, for `--key env://OCX_SIGNING_KEY`, read once
    /// by `oci::sign::key_backend::PemKeyBackend::open_env` (and on the verify
    /// side by `ocx_trust::compile_key_reference`).
    ///
    /// The **value is the key**, not a path to one — the spelling for a runner
    /// with no writable disk, where writing the key out in order to sign with
    /// it is the thing being avoided. [`OCX_KEY_PASSWORD`] is unrelated and the
    /// two coexist: one names the envelope, the other opens it.
    ///
    /// The most sensitive member of [`CREDENTIAL_KEYS`] — a raw private key
    /// rather than a short-lived token. `env://` accepts **any** variable name,
    /// and a name ocx does not know cannot be scrubbed; this is the
    /// conventional one, so the documented case is covered. Choosing another
    /// name still works and is still inherited by plugins — the operator's
    /// knowing call, made once, not a silent default.
    pub const OCX_SIGNING_KEY: &str = "OCX_SIGNING_KEY";

    /// The API half of the forge credential pair — a forge personal, project or
    /// group access token — read by the forge credential ladder
    /// (`forge::credentials::ForgeCredentials::resolve`).
    ///
    /// **A documented non-member of [`CREDENTIAL_KEYS`]**, and the only
    /// constant here that is one: see `# Known non-members` below for why it
    /// stays out while its push-half sibling [`OCX_ANNOUNCE_GIT_TOKEN`] is in.
    /// It lives here regardless because three sites need the *name* — the
    /// ladder that reads it, the CLI refusal that tells an operator which
    /// variable to set, and this module's own prose — and a name spelled three
    /// times is a name that drifts.
    ///
    /// **One private copy survives**, and it is recorded rather than assumed
    /// away: `ocx_cli::command::package_announce` still declares its own
    /// `const OCX_ANNOUNCE_TOKEN` and reads the variable directly, so a rename
    /// here compiles clean and leaves announce reading the old name. WP-15
    /// deletes that copy when it moves announce onto the ladder. Not a scrub
    /// gap — this variable is a documented non-member of [`CREDENTIAL_KEYS`],
    /// so no `env_remove` loop depends on the spelling.
    pub const OCX_ANNOUNCE_TOKEN: &str = "OCX_ANNOUNCE_TOKEN";

    /// The push-only forge credential, read by the git write transport's
    /// credential ladder (`forge::credentials::ForgeCredentials::resolve`).
    ///
    /// A bearer credential in the plain sense: it is presented as the secret
    /// half of an HTTP Basic pair to a `git push`, so holding the string is
    /// enough to write to the index repository. Its user half,
    /// `OCX_ANNOUNCE_GIT_USERNAME`, is **not** a credential and is deliberately
    /// absent from [`CREDENTIAL_KEYS`].
    ///
    /// **Asymmetric against its own sibling**, and the asymmetry is deliberate:
    /// this variable is scrubbed from plugin child environments while
    /// [`OCX_ANNOUNCE_TOKEN`] is not (see `# Known non-members`), so a
    /// plugin-dispatched `ocx-mirror` inherits the API half and not the push
    /// half. Benign today because that plugin drives no git transport; any
    /// transport wiring there must pass the push credential explicitly rather
    /// than relying on inheritance.
    pub const OCX_ANNOUNCE_GIT_TOKEN: &str = "OCX_ANNOUNCE_GIT_TOKEN";

    /// Every env var that carries a **bearer credential** — the single source
    /// of truth for that property, and the set `apply_ocx_config` scrubs from
    /// any child env.
    ///
    /// # Membership rule
    ///
    /// A variable belongs here when possessing its value is enough to act as
    /// the operator: a token, a passphrase, a private key. Not "sensitive" in
    /// the vague sense — a registry hostname is configuration, and
    /// `OCX_CONSENT_PATHS` is policy. If holding the string authenticates you,
    /// it is a credential.
    ///
    /// # Adding one
    ///
    /// Four edits, in the same change, or the set is a lie somewhere:
    ///
    /// 1. Add the constant here and list it below.
    /// 2. If the new member has a sibling that stays **out**, update
    ///    `# Known non-members` so it explains the asymmetry rather than a
    ///    rationale the new member violates.
    /// 3. Add its row to the credential exemption table in
    ///    `.claude/rules/subsystem-cli.md` (the reviewer-facing list).
    /// 4. Document it in `website/src/docs/reference/environment.md`, stating
    ///    that it is never forwarded to child processes (the user-facing list).
    ///
    /// # Members
    ///
    /// | Variable | Carries | Read at |
    /// |---|---|---|
    /// | [`OCX_IDENTITY_TOKEN`] | Short-lived OIDC bearer token | the shared sign/attest token resolver |
    /// | [`OCX_KEY_PASSWORD`] | Passphrase for an encrypted signing key | `oci::sign::key_backend::key_password` |
    /// | [`OCX_SIGNING_KEY`] | The signing key PEM itself | `oci::sign::key_backend::PemKeyBackend::open_env` |
    /// | [`OCX_ANNOUNCE_GIT_TOKEN`] | The push half of the forge credential pair | `forge::credentials::ForgeCredentials::resolve` |
    ///
    /// # Known non-members
    ///
    /// Two variables satisfy the membership rule above and are **deliberately
    /// not** in the set; a third is listed because its name makes it look as
    /// though it should be. Recorded here so the next contributor does not
    /// re-derive the analysis, or add one without seeing what it costs.
    ///
    /// - [`OCX_ANNOUNCE_TOKEN`] (read by the forge credential ladder) — a forge
    ///   personal access token, so holding it authenticates you. **Open: a
    ///   cross-repo decision, not an oversight.** `ocx-mirror` announces from a
    ///   plugin process, and a plugin inherits the ambient environment, so
    ///   adding this entry would stop that working. The owner's call.
    ///
    ///   **This is now an asymmetry inside one family, not a blanket
    ///   exclusion.** Its push-half sibling [`OCX_ANNOUNCE_GIT_TOKEN`] **is** a
    ///   member and **is** scrubbed, so a plugin-dispatched `ocx-mirror`
    ///   inherits the API half and not the push half. Benign only because that
    ///   plugin drives no git write transport today; wiring one there means
    ///   passing the push credential explicitly rather than relying on
    ///   inheritance.
    /// - `OCX_ANNOUNCE_GIT_USERNAME` (read by the same ladder) — the **user**
    ///   half of the HTTP Basic pair, defaulting to `gitlab-ci-token`. It does
    ///   **not** satisfy the membership rule: holding it authenticates nobody,
    ///   and listing it would say that it did. Not open, and not a gap — a
    ///   decided no, recorded only because it sits one underscore away from a
    ///   member.
    /// - `OCX_AUTH_<slug>_TOKEN` (read by `ocx_oci::auth::get_env_auth`) — a name
    ///   *pattern*, not a name, so a `&[&str]` structurally cannot hold it. **Open: a gap
    ///   in the mechanism, not a missing row.** The repo already solves this
    ///   shape once — `script::ocx_module::is_reserved_env_key` masks the whole
    ///   `OCX_AUTH_` family from Starlark by prefix — so there are two
    ///   credential masks and only one of them handles patterns. Closing it
    ///   means teaching this one prefixes too. Same shape as `env://` under an
    ///   operator-chosen name: what ocx cannot name, it cannot scrub.
    ///
    /// # Why a set at all
    ///
    /// Forwarding a credential to every subprocess broadens the attack surface
    /// for no gain: the one command that needs it reads it directly. But *not
    /// forwarding* is not *not leaking* — `Command::envs` only adds and
    /// overrides, so a spawn site that inherits the ambient environment passes
    /// an inherited credential straight through. Every such site must
    /// `env_clear()` or `env_remove` each entry here; `app/plugin_dispatch.rs`
    /// is the one that inherits deliberately and therefore removes explicitly.
    pub const CREDENTIAL_KEYS: &[&str] = &[
        OCX_IDENTITY_TOKEN,
        OCX_KEY_PASSWORD,
        OCX_SIGNING_KEY,
        OCX_ANNOUNCE_GIT_TOKEN,
    ];
}

/// Resolution-affecting policy snapshot, taken from the running ocx's parsed
/// `ContextOptions`. `Env::apply_ocx_config` writes this onto a child env so a
/// spawned ocx subprocess sees the same policy the parent saw, even when the
/// child env was built via [`Env::clean`](crate::env::Env::clean).
///
/// Only carries flags that change *what* a child ocx resolves. Presentation
/// flags (`--log-level`, `--format`, `--color`) are intentionally absent —
/// generated launchers must remain opaque to the surrounding tool, so outer
/// presentation choices never propagate via env. See
/// `website/src/docs/reference/env-composition.md` for the full forwarding
/// rule.
#[derive(Debug, Clone)]
pub struct OcxConfigView {
    /// Absolute path to the running ocx executable.
    pub self_exe: PathBuf,
    pub offline: bool,
    pub remote: bool,
    /// When true, tag→digest resolution may only consult the local index; an
    /// unpinned (tag-only) miss errors instead of walking the source chain.
    /// Digest-pinned content still fetches. Resolution-affecting → forwarded
    /// as [`keys::OCX_FROZEN`] so a child ocx applies the same freeze.
    pub frozen: bool,
    pub config: Option<PathBuf>,
    pub project: Option<PathBuf>,
    /// When true, the global toolchain (`$OCX_HOME/ocx.toml`) is the
    /// in-effect project file. Resolution-affecting → forwarded as
    /// [`keys::OCX_GLOBAL`] so a child ocx selects the same tier.
    ///
    /// **Forwarding-coverage rationale (W2-P3, F6).** `OCX_GLOBAL` is
    /// forwarded by [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) like every other
    /// resolution-affecting flag, and that contract is pinned by the unit
    /// test `apply_ocx_config_sets_ocx_global_when_set` (the sole
    /// `OCX_*`-landing path). No *acceptance-level* end-to-end
    /// "child ocx inherits `OCX_GLOBAL`" test is exercised on purpose: under
    /// strict isolation (adr_global_toolchain_tier.md §Decision 4) the only
    /// path that re-enters `ocx` from a `run`/`exec` spawn is a generated
    /// entrypoint launcher (`ocx launcher exec`), an OCI-tier primitive that
    /// reads `metadata.json` and never consults `ocx.toml`/global project
    /// resolution. There is therefore no observable child-side resolution
    /// difference to assert; fabricating a recursive `ocx --global run`
    /// child to create one would test a contrived path strict isolation
    /// forbids by design. The block-tier "resolution flag forwarded"
    /// contract is fully pinned at the unit layer.
    pub global: bool,
    /// When true, the invocation's own `--no-consent` said this command must
    /// not stamp. Forwarded as [`keys::OCX_NO_CONSENT`] so a nested ocx a
    /// child process launches inherits the refusal.
    ///
    /// **Suppression-only, deliberately asymmetric.** A `--consent` leaves
    /// this `false` and therefore never *clears* an ambient
    /// `OCX_NO_CONSENT` from the child env: `--consent` is a statement about
    /// the one project this invocation targets, not a grant covering
    /// everything the child goes on to touch. Refusal inherits downward;
    /// permission does not (ocx-sh/ocx#400).
    pub no_consent: bool,
    pub index: Option<PathBuf>,
    /// Root under which project toolchain homes are rendered — the resolved
    /// `toolchain_dir` (C-008). `None` when no tier set one, which is the
    /// in-project `<project>/.ocx/toolchain` default.
    ///
    /// Resolution-affecting, and that is the whole reason it travels: it moves
    /// `<home>/toolchain/links/<group>/<entry>`, so a child ocx that resolved a
    /// different root would emit composed paths pointing at another tree.
    /// Forwarded as [`keys::OCX_TOOLCHAIN_DIR`], set-or-**remove** like
    /// [`keys::OCX_CONFIG`] and [`keys::OCX_INDEX`].
    ///
    /// Every producer in the tree writes `None` until WP-4 resolves
    /// `toolchain_dir` (plan finding R-W7) — which is the shape D-V10 invoked
    /// *Unchecked Green* to reject for C-002, and is justified differently
    /// here: the field is compile-forced (a struct literal cannot omit it),
    /// and its `None` arm in [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) does real work today by
    /// stripping a stale inherited export. Only the `Some` arm waits.
    pub toolchain_dir: Option<PathBuf>,
    /// Per-traffic-host mirrors, as `(host, MirrorConfig)` pairs — the
    /// merged-but-not-yet-role-parsed union entries from
    /// [`crate::mirror::ResolvedMirrors::merged`]. Resolution-
    /// affecting → forwarded to child ocx processes via
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) as the single JSON object [`keys::OCX_MIRRORS`].
    /// The resolved map merges `[mirrors]` config with the inherited
    /// `OCX_MIRRORS` env, env winning per-host key.
    pub mirrors: Vec<(String, crate::mirror::MirrorConfig)>,
    /// Resolved `[patches]` site-tier config. Resolution-affecting → forwarded
    /// to child ocx processes via [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) as a JSON object
    /// in [`keys::OCX_PATCHES`] so launchers apply the same patch tier (C5).
    /// `None` when no `[patches]` registry is configured.
    pub patches: Option<crate::patch::ResolvedPatchConfig>,
    /// Path to the active patch snapshot file. Resolution-affecting →
    /// forwarded to child ocx processes via [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) as
    /// [`keys::OCX_PATCH_SNAPSHOT`] so launchers resolve the same frozen
    /// companion digests. `None` when no snapshot is active.
    pub patch_snapshot: Option<PathBuf>,
    /// The effective managed-config source (flag > env > seed), forwarded to
    /// child ocx processes via [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) as
    /// [`keys::OCX_MANAGED_CONFIG`] so a launcher re-entry resolves the same
    /// managed tier. `None` when no managed-config source is in effect.
    pub managed_config_source: Option<String>,
    /// When true, the policy-gated auto-verify on install/pull is opted out
    /// (`OCX_NO_VERIFY` truthy in the parent env). Forwarded as
    /// [`keys::OCX_NO_VERIFY`] so a child ocx install inherits the same
    /// CI-wide opt-out. The per-command `--no-verify` flag is a one-shot user
    /// choice and is NOT forwarded.
    pub no_verify: bool,
    /// When true, the discovered config-tier chain (system / user /
    /// `$OCX_HOME`) is skipped for this invocation (`OCX_NO_CONFIG` truthy in
    /// the parent env). Forwarded as [`keys::OCX_NO_CONFIG`] so a launcher
    /// re-entry stays as hermetic as the frame that spawned it instead of
    /// re-reading the whole chain.
    ///
    /// A field rather than an ambient read inside
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config): every other resolution-affecting value the
    /// child inherits is resolved once at `Context::try_init` and travels on
    /// this view, and a value read at the forwarding site instead is a second
    /// source of truth that can disagree with the one the loader actually
    /// used.
    pub no_config: bool,
    /// The resolved `[records]` sink and filename template, forwarded to child
    /// ocx processes via [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) as
    /// [`keys::OCX_RECORDS_DIR`] / [`keys::OCX_RECORDS_NAME`].
    ///
    /// Only `dir` and `name` travel: they are the two fields with an
    /// environment peer. `required` is config-file-only at every tier and
    /// `system_locked` is loader-set provenance, so setting either here
    /// forwards nothing — a child re-reads both from the config chain it
    /// resolves for itself.
    pub records: RecordsOptions,
}

impl OcxConfigView {
    pub fn new(self_exe: impl Into<PathBuf>) -> Self {
        Self {
            self_exe: self_exe.into(),
            offline: false,
            remote: false,
            frozen: false,
            config: None,
            project: None,
            global: false,
            no_consent: false,
            index: None,
            toolchain_dir: None,
            mirrors: Vec::new(),
            patches: None,
            patch_snapshot: None,
            managed_config_source: None,
            no_verify: false,
            no_config: false,
            records: RecordsOptions::default(),
        }
    }
}

/// Case-normalizing wrapper for environment variable keys.
///
/// On Windows, environment variable names are case-insensitive, so we
/// normalize to uppercase using the native wide-char representation.
/// On other platforms, keys are left as-is.
///
/// Keys are stored as `OsString` to preserve non-UTF-8 environment
/// variable names (possible on Unix).
///
/// Crate-visible because `package::metadata::env::apply` groups list entries by
/// the same normalized key this env stores them under — one normalization rule,
/// not two that can drift.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EnvKey(OsString);

impl EnvKey {
    /// Build a key, case-folded on Windows so a lookup matches the kernel.
    pub fn new(key: impl Into<OsString>) -> Self {
        let key = key.into();
        #[cfg(windows)]
        let key = {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            let upper: Vec<u16> = key.encode_wide().map(|c| wide_to_upper(c)).collect();
            OsString::from_wide(&upper)
        };
        Self(key)
    }
}

/// Uppercase a single UTF-16 code unit in the ASCII range.
///
/// Full Unicode case-folding is not needed — Windows environment variable
/// names are conventionally ASCII. This matches the kernel behavior for
/// env var lookups (case-insensitive in the ASCII range).
#[cfg(windows)]
fn wide_to_upper(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) {
        c - 0x20
    } else {
        c
    }
}

#[derive(Clone)]
pub struct Env {
    vars: HashMap<EnvKey, OsString>,
    /// The `PATH` directories contributed by composed package entries, in the
    /// same order they occupy in `PATH` — never the ambient inherited ones.
    ///
    /// This is what makes [`Env::resolve_test_command`] able to answer "does
    /// the package under test ship this name?" separately from "is this name
    /// on PATH at all". Populated only through [`Env::note_package_path`],
    /// which the entry fold calls for every composed `PATH` entry, and
    /// deliberately never emitted by [`Env::iter`] / `IntoIterator`: it is
    /// resolution bookkeeping, not an environment variable, and must not reach
    /// a child process.
    package_path: OsString,
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    pub fn new() -> Self {
        Self {
            vars: std::env::vars_os().map(|(k, v)| (EnvKey::new(k), v)).collect(),
            package_path: OsString::new(),
        }
    }

    pub fn clean() -> Self {
        Self {
            vars: HashMap::new(),
            package_path: OsString::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.vars.get(&EnvKey::new(key)).map(|s| s.as_os_str())
    }

    pub fn set(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.vars.insert(EnvKey::new(key), value.into());
    }

    /// Prepends `value` to the path-style variable `key` with **move-to-front**
    /// semantics: any existing occurrence of `value` is removed and `value` is
    /// placed at the front, so re-applying the same value never duplicates an
    /// entry. Empty segments are dropped. See
    /// `ocx_util::path::move_to_front`.
    pub fn add_path(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        let key = EnvKey::new(key);
        let value = value.into();
        let new_value = match self.vars.get(&key) {
            Some(existing) => ocx_util::path::move_to_front(existing, &value),
            None => value,
        };
        self.vars.insert(key, new_value);
    }

    /// Appends `value` to the list-style variable `key` with **move-to-back**
    /// semantics: any existing occurrence of `value` is removed and `value` is
    /// placed at the back, joined by `separator`, so re-applying the same
    /// contribution never duplicates it. See
    /// `ocx_util::list::append_unique`
    /// for the pinned algorithm.
    ///
    /// An empty `value` is a no-op — on an absent key too, which is where this
    /// deliberately differs from [`Self::add_path`]: appending nothing must not
    /// bring a variable into existence.
    ///
    /// The ambient value is read through `to_string_lossy`. Option-list
    /// variables carry authored text, and the fold is defined on UTF-8; a
    /// non-UTF-8 ambient value would not survive the append with its bytes
    /// intact either way.
    pub fn add_list(&mut self, key: impl Into<OsString>, value: &str, separator: &str) {
        if value.is_empty() {
            return;
        }
        let key = EnvKey::new(key);
        let existing = self.vars.get(&key).map(|v| v.to_string_lossy().into_owned());
        let folded = ocx_util::list::append_unique(existing.as_deref().unwrap_or_default(), value, separator);
        self.vars.insert(key, OsString::from(folded));
    }

    /// Borrowing iterator over `(key, value)` pairs.
    ///
    /// Lets a caller feed this env to `Command::envs` without consuming or
    /// cloning the whole map (`IntoIterator` is by-value). Order is
    /// unspecified (backed by a `HashMap`).
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars.iter().map(|(k, v)| (k.0.as_os_str(), v.as_os_str()))
    }

    /// Removes the named key from this environment. No-op if absent.
    pub fn remove(&mut self, key: &str) {
        self.vars.remove(&EnvKey::new(key));
    }

    /// Records `dir` as a `PATH` directory a composed package contributed.
    ///
    /// The write the private `package_path` field exists for, and the only way
    /// to make it from outside this module. `EnvEntriesExt::apply_entries` is
    /// the sole caller: it owns the decision of *which* entries count (a `path`
    /// entry on the `PATH` key, never `LD_LIBRARY_PATH` and its siblings), and
    /// this owns the fold — the same
    /// [`move_to_front`](ocx_util::path::move_to_front) the real `PATH`
    /// gets, so [`Self::resolve_test_command`] searches the two in one order.
    ///
    /// Setting a value on `PATH` is **not** implied. This is bookkeeping about
    /// an environment the caller has already written; calling it alone records
    /// a directory the child process cannot see.
    pub fn note_package_path(&mut self, dir: &OsStr) {
        self.package_path = ocx_util::path::move_to_front(&self.package_path, dir);
    }

    /// Materializes resolution-affecting OCX configuration onto this env so a
    /// child ocx process sees the same policy the parent saw.
    ///
    /// [`keys::OCX_NO_CONSENT`] is the one member that is **not**
    /// resolution-affecting: it changes nothing a child resolves, only whether
    /// a child writes a consent stamp. It rides here because this is the one
    /// channel that survives a deliberately emptied child env
    /// ([`Env::clean`](crate::env::Env::clean)) — the reason at its own block below.
    ///
    /// Always sets [`keys::OCX_BINARY_PIN`] and — when a root resolves —
    /// [`keys::OCX_HOME`], so a child lands in the same store the parent did
    /// even on the `--clean` arm, which strips `HOME` along with everything
    /// else. Sets [`keys::OCX_OFFLINE`] /
    /// [`keys::OCX_REMOTE`] / [`keys::OCX_FROZEN`] / [`keys::OCX_GLOBAL`] /
    /// [`keys::OCX_NO_VERIFY`] / [`keys::OCX_NO_CONFIG`] only
    /// when the corresponding flag is true so the child env stays minimal. Sets
    /// [`keys::OCX_CONFIG`] /
    /// [`keys::OCX_INDEX`] / [`keys::OCX_TOOLCHAIN_DIR`] /
    /// [`keys::OCX_RECORDS_DIR`] /
    /// [`keys::OCX_RECORDS_NAME`] only when the parent had an explicit value;
    /// otherwise removes any inherited setting so a stale parent-shell export
    /// cannot beat the outer ocx's parsed state.
    ///
    /// Does **not** propagate presentation flags (`--log-level`, `--format`,
    /// `--color`) — those are user-facing surface and must not leak into a
    /// launcher's child stream. Idempotent.
    pub fn apply_ocx_config(&mut self, cfg: &OcxConfigView) {
        // Bearer credentials are intentionally NOT forwarded — strip any
        // inherited value before writing the resolution-affecting keys.
        for credential in keys::CREDENTIAL_KEYS {
            self.remove(credential);
        }
        self.set(keys::OCX_BINARY_PIN, cfg.self_exe.as_os_str());
        // Set-always, never removed, and the one value here that is *resolved*
        // rather than parsed from the view: `default_ocx_root()` is the single
        // definition of the root this process' own stores were opened at, so
        // reading it here is the same source the parent used, not a second one
        // (the shape `OCX_ALLOW_YANKED` established below). `None` means
        // neither `$OCX_HOME` nor a home directory resolved — the state that
        // produced it, and one no inherited value could improve on — so that
        // arm writes nothing and clears nothing.
        //
        // Absolutized against *this* process' working directory, which is the
        // only frame where a relative ambient `OCX_HOME` still means what its
        // author meant: a child is free to run somewhere else —
        // `package test --script` spawns from the scratch tree — and a
        // relative value travelling verbatim would name a different directory
        // there, silently. `std::path::absolute` is lexical (no filesystem
        // access, no symlink resolution), so an absolute value is returned
        // unchanged and nothing is canonicalized behind the operator's back.
        if let Some(root) = crate::home::default_ocx_root() {
            let absolute = std::path::absolute(&root).unwrap_or_else(|error| {
                // The documented failure is an empty path or a CWD this
                // process cannot read; the raw value is still better than no
                // value, and the child re-resolves it the same way this
                // process just did.
                log::debug!("could not absolutize OCX_HOME '{}': {error}", root.display());
                root.clone()
            });
            self.set(keys::OCX_HOME, absolute.as_os_str());
        }
        if cfg.offline {
            self.set(keys::OCX_OFFLINE, "1");
        } else {
            self.remove(keys::OCX_OFFLINE);
        }
        if cfg.remote {
            self.set(keys::OCX_REMOTE, "1");
        } else {
            self.remove(keys::OCX_REMOTE);
        }
        if cfg.frozen {
            self.set(keys::OCX_FROZEN, "1");
        } else {
            self.remove(keys::OCX_FROZEN);
        }
        if cfg.global {
            self.set(keys::OCX_GLOBAL, "1");
        } else {
            self.remove(keys::OCX_GLOBAL);
        }
        match &cfg.config {
            Some(path) => self.set(keys::OCX_CONFIG, path.as_os_str()),
            None => self.remove(keys::OCX_CONFIG),
        }
        match &cfg.project {
            Some(path) => self.set(keys::OCX_PROJECT, path.as_os_str()),
            None => self.remove(keys::OCX_PROJECT),
        }
        match &cfg.index {
            Some(path) => self.set(keys::OCX_INDEX, path.as_os_str()),
            None => self.remove(keys::OCX_INDEX),
        }
        // C-008. The `None` arm is load-bearing, not symmetry for its own sake:
        // without the remove, a stale `OCX_TOOLCHAIN_DIR` exported into the
        // parent shell survives into every child and beats the outer ocx's
        // parsed state, so the two frames of one launch render and read two
        // different toolchain trees.
        match &cfg.toolchain_dir {
            Some(path) => self.set(keys::OCX_TOOLCHAIN_DIR, path.as_os_str()),
            None => self.remove(keys::OCX_TOOLCHAIN_DIR),
        }
        match encode_mirrors(&cfg.mirrors) {
            Some(json) => self.set(keys::OCX_MIRRORS, json),
            None => self.remove(keys::OCX_MIRRORS),
        }
        match crate::patch::encode_patches(cfg.patches.as_ref()) {
            Some(json) => self.set(keys::OCX_PATCHES, json),
            None => self.remove(keys::OCX_PATCHES),
        }
        match &cfg.patch_snapshot {
            Some(path) => self.set(keys::OCX_PATCH_SNAPSHOT, path.as_os_str()),
            None => self.remove(keys::OCX_PATCH_SNAPSHOT),
        }
        match &cfg.managed_config_source {
            Some(source) => self.set(keys::OCX_MANAGED_CONFIG, source.as_str()),
            None => self.remove(keys::OCX_MANAGED_CONFIG),
        }
        if cfg.no_verify {
            self.set(keys::OCX_NO_VERIFY, "1");
        } else {
            self.remove(keys::OCX_NO_VERIFY);
        }
        // Sink and template forward independently, each set-or-remove: a
        // resolved sink with a defaulted template must not pick the parent
        // shell's pattern back up, or a collector's glob matches files the
        // operator never described.
        match &cfg.records.dir {
            Some(path) => self.set(keys::OCX_RECORDS_DIR, path.as_os_str()),
            None => self.remove(keys::OCX_RECORDS_DIR),
        }
        match &cfg.records.name {
            Some(template) => self.set(keys::OCX_RECORDS_NAME, template.as_str()),
            None => self.remove(keys::OCX_RECORDS_NAME),
        }
        // Unconditional clear, never a set: the forwarded project env is NOT a
        // resolution-affecting config field and deliberately does not live on
        // `OcxConfigView`. This half of the contract guarantees a stale
        // `OCX_ENV` — exported by a shell, or inherited from an unrelated
        // parent `ocx exec` — can never reach a child. The invocation that
        // genuinely has a payload writes it afterwards, through
        // `EnvEntriesExt::apply_child_env`.
        self.remove(keys::OCX_ENV);
        // Hermetic must stay hermetic across the hop. Without this the parent
        // prunes the discovered config chain and the child — a fresh process —
        // re-reads it in full, so the two frames of one launch resolve against
        // different configuration. The already-forwarded `OCX_RECORDS_*` do not
        // cover it: `required` has no environment peer at all, so an unlocked
        // `[records]` posture the parent never saw would still reach the child.
        // Taken from the view rather than read ambiently here: the loader seam
        // is where the value is authoritative, `Context::try_init` captures it
        // there once, and a second read at this site could disagree with the
        // chain this process actually loaded.
        if cfg.no_config {
            self.set(keys::OCX_NO_CONFIG, "1");
        } else {
            self.remove(keys::OCX_NO_CONFIG);
        }
        // Resolution-affecting, but a pure env opt-in with no `ContextOptions`
        // / CLI counterpart (unlike the flags above): its authoritative value
        // IS the ambient env, which the outer ocx read the same way at the
        // index-client seam. Forward the parsed bool so a child ocx resolving
        // an index-sourced yanked tag honours the identical override.
        if flag(keys::OCX_ALLOW_YANKED, false) {
            self.set(keys::OCX_ALLOW_YANKED, "1");
        } else {
            self.remove(keys::OCX_ALLOW_YANKED);
        }
        // Two independent refusals, one key: `cfg.no_consent` is what this
        // invocation's own `--no-consent` said, the ambient read is what the
        // environment said, and either one suppresses. Not a second spelling of
        // the flag > env > stamp ladder — that still resolves at exactly one
        // site, `project_context::record_activation_consent_over`; this only
        // ORs two refusals that have already been decided.
        //
        // The halves cover different hops. `ocx exec` builds its child from
        // `Env::inherited` by default, so the ambient half already survives
        // that hop untouched and earns its keep on the `--clean` arm
        // (`toolchain_exec.rs` takes `Env::clean` there), where the child map
        // starts empty. The flag half has no other channel at all: a
        // `--no-consent` lives in argv, which no child process ever sees, so
        // without it the explicit gesture would propagate less far than the
        // ambient one — failing OPEN on a security control.
        //
        // One-way on purpose: a `--consent` leaves `cfg.no_consent` false
        // and therefore never clears an inherited refusal. Refusal inherits
        // downward, permission does not (ocx-sh/ocx#400).
        //
        // Not resolution-affecting, unlike everything above it: it changes no
        // resolution, only whether a side-effect write happens. It rides here
        // because this is the one channel that crosses a clean env.
        if cfg.no_consent || flag(keys::OCX_NO_CONSENT, false) {
            self.set(keys::OCX_NO_CONSENT, "1");
        } else {
            self.remove(keys::OCX_NO_CONSENT);
        }
    }

    /// Resolve a command name to a full executable path using PATH
    /// (and PATHEXT on Windows).
    ///
    /// On Windows, `PATHEXT` is read from *this* environment rather than from
    /// the running process, so a clean child env still resolves the native
    /// `<name>.exe` launcher shim correctly before the child is spawned (the
    /// fallback default `.COM;.EXE;.BAT;.CMD` always advertises `.EXE`).
    ///
    /// # C-009 — the two arms differ, deliberately
    ///
    /// - A **path-bearing** `command` (`./hello`, `/abs/tool`, the Windows
    ///   drive-relative `C:tool`) names a file directly, so this method keeps
    ///   today's behaviour exactly, **including** falling back to the bare
    ///   value when the lookup misses: the value already *is* a path, and
    ///   handing it to the OS is a meaningful answer that four shipped tests
    ///   depend on.
    /// - A **bare** name that `PATH` cannot resolve is now
    ///   [`CommandResolutionError::NotFound`]. The old
    ///   `PathBuf::from(command)` fallback is deleted from this arm: it handed
    ///   an unresolved name to `execvp`, which then performs its **own**
    ///   ambient-`PATH` lookup — the exact escape from the composed
    ///   environment ocx exists to prevent. The `log::warn!` that stood beside
    ///   it is deleted with it; a function returning `Result` must not also log
    ///   its own failure, or every caller that handles the error prints twice.
    ///   [`Self::resolve_test_command`] deliberately keeps that warning,
    ///   because its own contract is to fall through.
    ///
    /// The whole discrimination is `command_is_path`, the shipped helper
    /// whose "exactly one `Component::Normal`" rule already handles the
    /// `C:tool` trap a separator test misses. There is no second separator
    /// test here.
    ///
    /// Empty `PATH` segments are dropped from the **lookup copy** before the
    /// search — see `Self::lookup_path`. This env's own `PATH` is untouched.
    ///
    /// # C-069 — the answer is refused if it is a trampoline
    ///
    /// A successful lookup is gated once more before it is returned: an answer
    /// that is itself an ocx launcher trampoline is
    /// [`CommandResolutionError::TrampolineRefused`], never `Ok`. The gate
    /// lives in `Self::resolve_command_in`, which both public resolvers route
    /// through, so `Self::resolve_command_excluding` cannot acquire a different
    /// posture by accident. WP-2 owns the refusal **and its call site**; the
    /// only part of the loop guard left to WP-8 is C-058 — deriving the
    /// exclusion set from `ToolchainStore::bin()` / `ToolchainHome::bin()` and
    /// handing it to `Self::resolve_command_excluding`.
    ///
    /// # Errors
    ///
    /// [`CommandResolutionError::NotFound`] when `command` is a bare name that
    /// no directory on this env's `PATH` provides, and
    /// [`CommandResolutionError::TrampolineRefused`] when the answer is an
    /// ocx-generated launcher trampoline (C-069).
    pub fn resolve_command(&self, command: impl AsRef<OsStr>) -> Result<PathBuf, CommandResolutionError> {
        self.resolve_command_in(command.as_ref(), self.lookup_path())
    }

    /// The one lookup both public resolvers route through, over an
    /// already-prepared `PATH` copy.
    ///
    /// # Why the search space is a parameter — the C-010/C-069 ordering
    ///
    /// C-010's exclusion **shapes the input** to this lookup and C-069's
    /// refusal **filters its output**, so the exclusion necessarily runs first
    /// and no edit inside this function can reorder the two: by the time the
    /// refusal has an answer to judge, the caller's `PATH` copy has already
    /// decided which directories could produce one. That is what lets item 22
    /// assert *which* guard caught a given input — the exclusion answers
    /// "never looked there", the refusal answers "looked, found a trampoline".
    ///
    /// `NotFound`'s `searched` is re-split from **`path` itself**, on the miss
    /// arm only: it must name the directories that were *actually* walked, and
    /// a second derivation from the stored `PATH` would disagree with the copy
    /// handed to `which_in` (finding S-1). Splitting the one value here also
    /// keeps the vector off the success path, where a composed `PATH` of 15–60
    /// segments would otherwise be allocated on every resolution for an error
    /// that usually does not happen.
    fn resolve_command_in(&self, command: &OsStr, path: Option<OsString>) -> Result<PathBuf, CommandResolutionError> {
        // cwd is only used by `which` when the command contains a path
        // separator (e.g. `./hello`).  For bare names it is ignored.
        let cwd = std::env::current_dir().unwrap_or_else(|e| {
            log::debug!("Could not determine current directory: {}", e);
            PathBuf::new()
        });

        // On Windows, `which_in` internally reads PATHEXT from the real
        // process environment via `RealSys::env_windows_path_ext()`, not from
        // our child `Env`. We therefore probe the child's PATHEXT ourselves so
        // the native `<name>.exe` launcher shim is found even when the running
        // process has a different PATHEXT.
        //
        // Bound to one variable rather than returned from two `#[cfg]` arms so
        // there is a **single** success point for the C-069 gate below to sit
        // on: a per-platform `return Ok(found)` would let an edit ship an
        // ungated answer on the platform the other build never compiles.
        #[cfg(windows)]
        let found = self.resolve_command_windows(command, path.as_deref(), &cwd);
        #[cfg(not(windows))]
        let found = which::which_in(command, path.as_deref(), &cwd).ok();

        if let Some(found) = found {
            // C-069, the only site that fires it. After the lookup, before the
            // `Ok` — a trampoline answer re-enters `ocx exec` against its own
            // home, and with two homes on one `PATH` that is an unbounded
            // A → B → A loop with no error and no depth counter.
            if is_ocx_trampoline(&found) {
                return Err(CommandResolutionError::TrampolineRefused {
                    command: command.to_string_lossy().into_owned(),
                    path: found,
                });
            }
            return Ok(found);
        }

        if command_is_path(command) {
            // Today's behaviour, unchanged: the value names a file, so hand it
            // to the OS. `execvp` performs no `PATH` search for a value
            // carrying a separator, so there is no ambient escape to close.
            //
            // Deliberately outside the C-069 gate: this arm is reached only
            // when the lookup *failed*, so the named file does not exist or
            // cannot be executed, and a file in that state cannot be the
            // working trampoline the refusal exists to stop. A path-bearing
            // command that does name a real trampoline resolves above and is
            // refused there.
            //
            // `{:?}` on the command: it arrives from package metadata, from
            // `ocx.lock` and from argv, and a raw newline in a log line forges
            // a second one (CWE-117).
            log::warn!(
                "Could not resolve {:?} via PATH, falling back to OS lookup.",
                command.to_string_lossy()
            );
            return Ok(PathBuf::from(command));
        }

        Err(CommandResolutionError::NotFound {
            command: command.to_string_lossy().into_owned(),
            // Re-split the very value `which_in` was handed, so the reported
            // search space cannot disagree with the searched one. No `PATH`
            // key, or a `PATH` of nothing but empty segments, is `None` here
            // and therefore an empty space — never an invented one.
            searched: path
                .as_deref()
                .map(|value| std::env::split_paths(value).collect())
                .unwrap_or_default(),
        })
    }

    /// The `PATH` value handed to `which_in`, with empty segments dropped.
    ///
    /// A copy: this env's own `PATH` is never rewritten, so the child process
    /// still inherits every segment its composition established.
    ///
    /// An empty `PATH` segment (`/a::/b`, a leading or trailing `:`) means
    /// *the current directory* on Unix — CWE-426, untrusted search path. It is
    /// not exotic: [`Self::inherited`] carries whatever the invoking shell
    /// exported, and an ocx invocation whose composed entries touch no `PATH`
    /// key passes it through verbatim. Dropping the segments here makes a
    /// resolution answer independent of where the user happened to `cd`.
    ///
    /// # `None` is the only spelling of "nothing to search"
    ///
    /// Returned both when the env carries no `PATH` key at all and when every
    /// segment it carries is empty (`""`, `":"`, `"::"` — the shape
    /// `PATH="$A:$B"` produces when both are unset). The empty *string* is not
    /// an empty search space: `split_paths("")` yields one empty segment, and
    /// `which` filters those on Windows only, so on Unix `which_in` would stat
    /// the bare candidate against the **process working directory** — exactly
    /// the CWE-426 probe this filter exists to remove. `None` is refused
    /// outright (`CannotGetCurrentDirAndPathListEmpty`) and has no ambient
    /// fallback, which is why the caller must never substitute `Some("")`.
    ///
    /// Re-joined with [`std::env::join_paths`], the exact inverse of the
    /// [`split_paths`](std::env::split_paths) that produced the segments. On
    /// Windows `split_paths` reads `"` as a quote, so a segment it yields
    /// *can* contain the separator — std's own example is
    /// `c:\foo;c:\som"e;di"r;c:\bar`, whose middle segment is `c:\some;dir`.
    /// `join_paths` re-quotes such a segment; a manual
    /// `push(PATH_SEPARATOR)` join would tear it into `c:\some` and `dir` and
    /// hand `which_in` a directory nobody put on `PATH` — a widening in the
    /// one function whose whole subject is narrowing the search space.
    ///
    /// Its rejection arm degrades to `None`, never to the value *unfiltered*:
    /// an unfiltered value is precisely the empty-segment search this function
    /// exists to prevent, whereas no search space at all is refused outright
    /// by `which_in`. The arm is unreachable in practice — `join_paths`
    /// rejects only what its own `split_paths` cannot emit (a `"` on Windows,
    /// a `:` on Unix) — so failing closed there costs nothing.
    fn lookup_path(&self) -> Option<OsString> {
        let path = self.get("PATH")?;
        let joined =
            std::env::join_paths(std::env::split_paths(path).filter(|segment| !segment.as_os_str().is_empty())).ok()?;
        if joined.is_empty() {
            return None;
        }
        Some(joined)
    }

    /// [`Self::resolve_command`] over a `PATH` from which `excluded` has been
    /// removed (`plan_toolchain_activation.md` C-010).
    ///
    /// # The invariant
    ///
    /// **`excluded` is removed from the lookup copy of `PATH` only. This env's
    /// own `PATH` must be byte-identical after the call.** The child process
    /// still receives every segment its composition established, so a tool that
    /// spawns a sibling tool still resolves that sibling through a trampoline.
    /// Rewriting the stored value instead would silently convert "do not let
    /// *this* lookup answer with a trampoline directory" into "no descendant
    /// process may ever use one" — a different, much larger decision, and one
    /// that would break the very re-entry the trampolines exist to provide.
    ///
    /// This is the opposite of what `ocx launcher shim` does with its own shim
    /// directory: that site prunes the **child's** `PATH`
    /// (`process_env.set("PATH", pruned)`) because a shim tree must not follow
    /// the process it materialised. Do not unify the two.
    ///
    /// # Not a containment check
    ///
    /// Segments are dropped with
    /// `ocx_util::path::remove_segment`,
    /// whose own doc comment is explicit that a segment naming the same
    /// directory by a different string — a trailing slash, a symlink alias,
    /// `$OCX_HOME` spelled one way by the composing process and another by this
    /// one — survives untouched, and that it "is therefore not a containment
    /// check and must not be read as one". C-069's
    /// [`is_ocx_trampoline`] re-check over the *resolved answer* is what closes
    /// that gap, and it is a second, independent guard: deleting either one
    /// alone must leave the other observably firing.
    ///
    /// C-058's caller derives `excluded` from the same resolver that produced
    /// the homes (`ToolchainStore::bin()` / `ToolchainHome::bin()`), never from
    /// a literal path join, so the set cannot drift from the tree shape. That
    /// derivation — passing the set in — is the **whole** of what C-069 leaves
    /// to WP-8; the refusal itself and its call site ship here, in the shared
    /// `Self::resolve_command_in` this method routes through.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve_command`]: [`CommandResolutionError::NotFound`] when
    /// a bare name resolves in no remaining directory, and
    /// [`CommandResolutionError::TrampolineRefused`] when a directory that
    /// survived the segment-exact exclusion answers with a trampoline anyway.
    pub fn resolve_command_excluding(
        &self,
        command: impl AsRef<OsStr>,
        excluded: &[PathBuf],
    ) -> Result<PathBuf, CommandResolutionError> {
        self.resolve_command_in(command.as_ref(), self.lookup_path_excluding(excluded))
    }

    /// [`Self::lookup_path`] with every directory in `excluded` removed
    /// (C-010).
    ///
    /// `None` means the same thing it means there, for the same reason: an
    /// exhausted search space is nothing to search, never `Some("")`.
    ///
    /// Split out from [`Self::resolve_command_excluding`] so the exclusion is
    /// an *input* to the shared lookup rather than a step inside it — see the
    /// ordering argument on `Self::resolve_command_in`.
    fn lookup_path_excluding(&self, excluded: &[PathBuf]) -> Option<OsString> {
        // Start from the copy `Self::lookup_path` already makes. Nothing below
        // writes back through `self`, so this env's own `PATH` stays
        // byte-identical — the invariant on `Self::resolve_command_excluding`.
        //
        // `None` propagates: no `PATH` key, or one carrying nothing but empty
        // segments, leaves nothing to prune, and inventing a search space the
        // env does not have is exactly what `lookup_path` refuses to do.
        let path = self.lookup_path()?;
        if excluded.is_empty() {
            // An empty exclusion set is `Self::resolve_command`, byte for byte,
            // because it *is* `lookup_path`'s value.
            return Some(path);
        }

        // Segment-exact removal, one excluded directory at a time. A directory
        // that is not on `PATH` simply matches nothing, which is why "exclude
        // something absent" is a no-op rather than an error.
        //
        // `remove_segment` splits, drops empty segments and re-joins on every
        // call, so it subsumes `lookup_path`'s CWE-426 empty-segment filter —
        // re-filtering here would be a second copy of the same rule, free to
        // drift from it.
        let pruned = excluded.iter().fold(path, |value, dir| {
            ocx_util::path::remove_segment(&value, dir.as_os_str())
        });

        if pruned.is_empty() {
            // Every segment was excluded — the `None` case of `lookup_path`,
            // reached by a different route and for the same CWE-426 reason.
            return None;
        }

        Some(pruned)
    }

    /// Windows-only: resolve `command` by probing `path` with each extension
    /// from this env's PATHEXT, in order.
    ///
    /// Returns `None` when the command cannot be found. `path` is the caller's
    /// **lookup copy** (`Self::lookup_path`), not this env's stored `PATH`:
    /// the empty-segment drop and C-010's exclusion both apply to the copy
    /// only, and reading the field here would silently bypass both. PATHEXT is
    /// still read from this `Env`, not the running process — that is the entire
    /// point of this method vs. delegating to `which_in` which reads
    /// `std::env::var_os`.
    #[cfg(windows)]
    fn resolve_command_windows(&self, command: &OsStr, path: Option<&OsStr>, cwd: &std::path::Path) -> Option<PathBuf> {
        // For each extension, ask `which_in` if `command + ext` is found.
        // `which_in` on a name-with-extension will not try to append further
        // extensions — it just probes the PATH directories for that exact name.
        for ext in self.pathext() {
            let mut candidate = command.to_os_string();
            candidate.push(&ext);
            if let Ok(found) = which::which_in(&candidate, path, cwd) {
                return Some(found);
            }
        }

        // Also try the bare name (covers fully-qualified paths and commands
        // that already carry an extension like `foo.exe`).
        which::which_in(command, path, cwd).ok()
    }

    /// Windows-only: the executable extensions to probe, in order.
    ///
    /// Read from *this* env's PATHEXT, falling back to the Windows default when
    /// absent so a bare `foo` can still resolve `foo.exe`.
    ///
    /// The OCX launcher shim is always `<name>.exe` post-`.cmd` cutover
    /// (adr_windows_exe_shim.md). A hardened or customized child PATHEXT may
    /// omit `.EXE` entirely (e.g. `PATHEXT=.BAT;.CMD`); the cutover removed the
    /// PATHEXT inject/warn safety net on the premise that `.EXE` is
    /// unconditionally resolvable, so it is guaranteed to be probed regardless
    /// of the child PATHEXT. Probe order still respects the user's listed
    /// extensions; `.EXE` is appended only when absent (case-insensitive).
    #[cfg(windows)]
    fn pathext(&self) -> Vec<String> {
        let pathext_str = self
            .get("PATHEXT")
            .and_then(|v| v.to_str())
            .unwrap_or(".COM;.EXE;.BAT;.CMD")
            .to_string();

        let mut extensions: Vec<String> = pathext_str
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        if !extensions.iter().any(|ext| ext.eq_ignore_ascii_case(".EXE")) {
            extensions.push(".EXE".to_string());
        }
        extensions
    }

    /// Resolve a command for `ocx * test` / launcher re-entry: never silently
    /// skip a same-named file the package under test ships.
    ///
    /// [`Self::resolve_command`] answers "what would the OS run", which walks
    /// the composed package directories straight on into the ambient PATH and
    /// skips a non-executable file without a word. For a test command that is
    /// the wrong question: a package shipping `tool` with the executable bit
    /// missing would silently be tested against the *host's* `tool`, and pass.
    ///
    /// So the package's own copy is decided first, and only a name the package
    /// does not ship at all is looked up on the host PATH:
    ///
    /// 1. Scan the package-contributed PATH directories, in PATH order; the
    ///    first executable match wins.
    /// 2. A match that is present but not executable is a hard error — never a
    ///    fall-through to a host copy.
    /// 3. A name the package does not ship resolves through
    ///    [`Self::resolve_command`], with a warning naming the directories that
    ///    were searched — and a name the host does not provide either falls
    ///    through to the bare name with its own warning, never an error. That
    ///    fall-through is **deliberately retained** across C-009 (which deleted
    ///    the equivalent one from [`Self::resolve_command`]): four production
    ///    callers, the Starlark host among them, treat a total miss as "let the
    ///    OS answer", and this method owns the warning that used to live one
    ///    level down.
    ///
    /// A path-bearing `command` (`./tool`, an absolute path, a Windows
    /// drive-relative `C:tool`) names a file directly, so there is no
    /// package-versus-host question to answer and it delegates unchanged.
    ///
    /// On Windows a candidate is reached only by matching a PATHEXT extension,
    /// which *is* the platform's definition of executable — there is no exec
    /// bit to fail, so [`CommandResolutionError::NotExecutable`] cannot occur.
    ///
    /// Synchronous, like [`Self::resolve_command`] (whose `which_in` stats the
    /// same directories): a handful of local `stat` calls, once per invocation,
    /// immediately before the process execs a child and stops doing anything
    /// else. Not worth an async seam the sibling resolver does not have.
    ///
    /// # Errors
    ///
    /// [`CommandResolutionError::NotExecutable`] when the package under test
    /// ships the name but the file cannot be executed, and
    /// [`CommandResolutionError::TrampolineRefused`] when the host lookup
    /// answers with an ocx launcher trampoline (C-069). The fall-through in
    /// step 3 is scoped to `NotFound` — the **total miss** C-011 describes —
    /// and to nothing else: a refusal is not a miss, and mapping it back to
    /// the bare name would hand that name to `execvp`, whose own ambient-`PATH`
    /// lookup finds the same trampoline again. That is the unbounded
    /// A → B → A re-entry C-069 exists to stop, restored one level up.
    pub fn resolve_test_command(&self, command: impl AsRef<OsStr>) -> Result<PathBuf, CommandResolutionError> {
        let command = command.as_ref();
        if command_is_path(command) {
            // C-009's path-bearing arm never errors, so this delegation carries
            // the same answer it always did.
            return self.resolve_command(command);
        }

        // The bare name comes last, and only for a command that already
        // carries a PATHEXT extension (`-- tool.exe`): without it the package
        // scan misses a file the package plainly ships. An extensionless bare
        // name is deliberately NOT a candidate — Windows cannot exec it, and
        // `resolve_command_windows` would never return one either, so matching
        // it here would trade a working host fallback for a doomed exec.
        #[cfg(windows)]
        let candidates: Vec<OsString> = {
            let pathext = self.pathext();
            let command_has_pathext_extension = std::path::Path::new(command)
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|extension| {
                    pathext
                        .iter()
                        .any(|known| known.trim_start_matches('.').eq_ignore_ascii_case(extension))
                });
            pathext
                .iter()
                .map(|ext| {
                    let mut candidate = command.to_os_string();
                    candidate.push(ext);
                    candidate
                })
                .chain(command_has_pathext_extension.then(|| command.to_os_string()))
                .collect()
        };
        #[cfg(not(windows))]
        let candidates: Vec<OsString> = vec![command.to_os_string()];

        // The first present-but-not-executable hit, remembered across the whole
        // scan so an executable match in a later directory still wins.
        let mut blocked: Option<(PathBuf, u32)> = None;

        for dir in std::env::split_paths(&self.package_path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            for name in &candidates {
                let path = dir.join(name);
                // `metadata` follows symlinks: a relative link inside the
                // package to a real binary resolves, a dangling one is simply
                // not a candidate.
                let Ok(metadata) = std::fs::metadata(&path) else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                match executable_verdict(&metadata) {
                    Ok(()) => return Ok(path),
                    Err(mode) => blocked.get_or_insert((path, mode)),
                };
            }
        }

        if let Some((path, mode)) = blocked {
            return Err(CommandResolutionError::NotExecutable {
                command: command.to_string_lossy().into_owned(),
                path,
                mode,
            });
        }

        // ponytail: warn, not error — strict upgrade = swap this arm for
        // Err(OutsidePackage) when the owner flips decision #1 on #268.
        //
        // C-011: the total-miss fall-through is kept **intact** and is now
        // detected by matching `resolve_command`'s `Result` directly, instead
        // of by comparing its answer against the bare input string. Four
        // production callers depend on the fall-through — including the
        // Starlark host in `script/ocx_module.rs`, whose
        // `bare_name_does_not_anchor_on_cwd` test panics if a total miss turns
        // into an `Err`.
        let found = match self.resolve_command(command) {
            Ok(found) => found,
            // The **total miss**, and only it. C-009 deleted the warning from
            // `resolve_command`'s bare arm along with the fallback it
            // described; this function still performs that fallback, so it
            // owns the line or it disappears from the product silently.
            Err(CommandResolutionError::NotFound { .. }) => {
                log::warn!(
                    "Could not resolve {:?} via PATH, falling back to OS lookup.",
                    command.to_string_lossy()
                );
                return Ok(PathBuf::from(command));
            }
            // C-069's `TrampolineRefused` above all. A blanket arm here hands
            // the bare name to `execvp`, which repeats the lookup against the
            // ambient `PATH` and finds the same trampoline — the guard fully
            // defeated, and reachable from every installed launcher's re-entry
            // (`launcher/exec.rs`), which prunes nothing and inherits the
            // ambient `PATH` a `bin`-mode toolchain puts its trampolines on.
            // Same rule, same spelling, as `update_check.rs`'s
            // `query_installed_version` and `launcher/shim.rs`'s `execute`.
            Err(error) => return Err(error),
        };
        // The host answered, so the composed packages did not ship the name —
        // structurally, because a total miss returned above.
        //
        // `split_paths("")` yields one empty PathBuf; the scan skips those, so
        // the message must too or a PATH-less package prints `[""]`. Every
        // interpolation goes through `{:?}`, which quotes and escapes: these
        // values arrive from `PATH`, from package metadata and from
        // `ocx.lock`, and a raw newline forges a log line (CWE-117).
        let searched: Vec<PathBuf> = std::env::split_paths(&self.package_path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .collect();
        log::warn!(
            "{:?} is not shipped by the composed packages; resolved to {:?} on the host PATH — expected it under one of: {:?}",
            command.to_string_lossy(),
            found,
            searched
        );
        Ok(found)
    }
}

/// True when `command` names a file directly rather than a bare name to look
/// up on PATH.
///
/// A bare name is *exactly one* [`Component::Normal`](std::path::Component) —
/// anything else is path-bearing. The scan joins the name onto each package
/// directory, and `Path::join` with a value carrying its own root or prefix
/// discards the base, so a looser test lets the join stat outside every
/// package directory and report the result as a copy the package ships. A
/// separator test misses the Windows drive-relative form (`C:tool` has no
/// separator, yet `join` keeps only the drive).
fn command_is_path(command: &OsStr) -> bool {
    let mut components = std::path::Path::new(command).components();
    !matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

/// `Ok(())` when the candidate can be executed, `Err(mode)` carrying its
/// permission bits when it cannot.
///
/// Mirrors the three-bit POSIX test in `package::bin_scan`'s
/// `unix_is_executable` — named in prose, not linked: this module is
/// `ocx_config` and the link would be the reach the split removes.
// ponytail: duplicated 3-line POSIX bit test; sharing would invert env→package layering
#[cfg(unix)]
fn executable_verdict(metadata: &std::fs::Metadata) -> Result<(), u32> {
    use std::os::unix::fs::PermissionsExt;
    // The caller already filtered to `is_file()`, so this checks the bit only —
    // a directory's `x` (traversable) can never reach here.
    let mode = metadata.permissions().mode();
    if mode & 0o111 != 0 { Ok(()) } else { Err(mode & 0o7777) }
}

/// Non-Unix hosts have no exec bit: a candidate is reached only by matching a
/// PATHEXT extension, which is the platform's own executability rule, so every
/// candidate that exists is executable.
#[cfg(not(unix))]
fn executable_verdict(_metadata: &std::fs::Metadata) -> Result<(), u32> {
    Ok(())
}

impl IntoIterator for Env {
    type Item = (OsString, OsString);
    type IntoIter = std::vec::IntoIter<(OsString, OsString)>;

    fn into_iter(self) -> Self::IntoIter {
        self.vars
            .into_iter()
            .map(|(k, v)| (k.0, v))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

/// Failure modes of [`Env::resolve_command`],
/// [`Env::resolve_command_excluding`] and [`Env::resolve_test_command`].
#[derive(Debug, thiserror::Error)]
pub enum CommandResolutionError {
    /// A bare command name resolves in no directory of the composed `PATH`
    /// (`plan_toolchain_activation.md` C-009).
    ///
    /// Terminal rather than a fall-through to the bare name: handing an
    /// unresolved name to `execvp` makes the kernel repeat the search against
    /// the **ambient** `PATH`, which is precisely the escape from the composed
    /// environment ocx exists to prevent. A path-bearing command never reaches
    /// this variant — it names a file, so there is no search to fail.
    ///
    /// `{command:?}` rather than `{command}`: a resolved value can carry a
    /// newline, and a raw one forges log lines (CWE-117). `searched` goes
    /// through `{:?}` for the same reason.
    #[error("{command:?} does not resolve in the composed environment; searched: {searched:?}")]
    NotFound {
        /// The bare command name as invoked.
        command: String,
        /// The non-empty `PATH` directories that were searched, in order.
        searched: Vec<PathBuf>,
    },

    /// The resolution answer is itself an ocx-generated launcher trampoline
    /// (`plan_toolchain_activation.md` C-069, divergence D-V1).
    ///
    /// A trampoline re-enters `ocx exec` against its own home. Executing one as
    /// the *answer* to a resolution performed by `ocx exec` is a self-reference:
    /// with two project homes on one `PATH`, each carrying a stale trampoline
    /// for the same name, the invocation loops A → B → A forever, one full
    /// compose per hop, with no error and no depth counter.
    ///
    /// A **distinct** kind from [`Self::NotFound`], and distinct from C-010's
    /// `PATH` exclusion, so a test can assert *which* guard caught a given
    /// input and each guard keeps its own reachable red state. C-010 removes
    /// the known trampoline directories from the lookup copy of `PATH`; this
    /// variant is what remains when a directory named by a different string
    /// survived that segment-exact comparison. Deleting either guard alone must
    /// leave the other observably firing.
    ///
    /// `{command:?}` **and `{path:?}`** for the CWE-117 reason above: the path
    /// is a `PATH` segment plus a resolved file name, neither of which this
    /// process chose, so `Path::display` would render an embedded newline raw.
    #[error(
        "{command:?} resolves to an ocx launcher trampoline at {path:?}; running it would re-enter ocx against itself"
    )]
    TrampolineRefused {
        /// The command name as invoked.
        command: String,
        /// The resolved path that was identified as a trampoline.
        path: PathBuf,
    },

    /// The package under test ships this name, but the file cannot be exec'd.
    ///
    /// Deliberately terminal: falling through to a same-named host binary
    /// would run the test against something the package does not contain and
    /// report a pass.
    ///
    /// `{command:?}` and `{path:?}`, the one spelling this enum uses (D-V15(e)
    /// named this variant): the command arrives from package metadata and the
    /// path from a composed `PATH` segment, so a raw render forges log lines
    /// (CWE-117).
    #[error(
        "{command:?} is present in the package under test at {path:?} but is not executable (mode {mode:04o}); \
         re-create the package with the executable bit set - ocx does not fall through to a host copy on PATH"
    )]
    NotExecutable {
        /// The bare command name as invoked.
        command: String,
        /// Absolute path of the non-executable file inside the package.
        path: PathBuf,
        /// POSIX permission bits, masked to `0o7777`.
        mode: u32,
    },
}

/// The marker every ocx-generated POSIX launcher **trampoline** carries, and
/// the sole POSIX signal of [`is_ocx_trampoline`]
/// (`plan_toolchain_activation.md` C-069, divergence D-V12).
///
/// Defined here, beside the predicate that consumes it, and **imported** by
/// the one emitter: `package_manager::launcher::body::unix_trampoline_body`
/// writes this constant rather than re-spelling it — one canonical spelling
/// with a producer and a consumer, the same split C-034's paired golden uses.
///
/// # The constraint on that emitter
///
/// The marker must be **exactly the second line** of the body — after the
/// shebang, ahead of every interpolated value — because that is the whole of
/// what [`is_ocx_trampoline`] matches. Not "somewhere in the head": line two,
/// compared whole.
///
/// Two failures that position rules out, one on each side:
///
/// - A root deep enough to push the marker past
///   [`TRAMPOLINE_PROBE_BYTES`] would disarm C-069 for any checkout roughly
///   146 characters deep while every shallow-`tmp_path` test stayed green.
///   `a_marker_beyond_the_probe_window_is_not_refused` pins that half.
/// - A `$OCX_HOME` whose **own path text** spells this marker would otherwise
///   land it inside the probed head of an ordinary package launcher — whose
///   body interpolates that path — and get a file refused that must keep
///   resolving (E-21). Line one and line two are the only bytes no baked value
///   can reach. `a_launcher_whose_baked_path_spells_the_marker_is_not_refused`
///   pins that half.
///
/// # Why not the shipped header
///
/// `# Generated by ocx at install time. Do not edit.` is emitted
/// **byte-identically** by `unix_launcher_body` and `unix_shim_body` as well,
/// both golden-pinned. A predicate keyed on it would refuse every ocx-generated
/// package launcher and every lazy shim — files that are not trampolines,
/// cannot start the two-home A → B → A loop, and must keep resolving. That
/// refusal would break `ocx launcher exec` and `ocx package exec` outright.
/// This marker is discriminating by construction.
pub const TRAMPOLINE_MARKER: &str = "# ocx-toolchain-trampoline";

/// How many bytes of a candidate file [`is_ocx_trampoline`] reads.
///
/// Enough to hold a shebang line plus [`TRAMPOLINE_MARKER`] with room to spare,
/// and small enough that the read costs one `read(2)` on any filesystem. The
/// bound is the point: see [`is_ocx_trampoline`].
///
/// It is deliberately **smaller than a whole trampoline body**. A C-028 body
/// carries an absolute project root, so it passes 256 bytes as soon as the
/// checkout is roughly 146 characters deep — which is why the probe reads a
/// *prefix* and the marker is emitted on the second line.
pub const TRAMPOLINE_PROBE_BYTES: usize = 256;

/// Whether `path` — an already-**resolved** command answer — is an
/// ocx-generated launcher trampoline (C-069).
///
/// Fires **after** C-010's `PATH` exclusion, never instead of it, so a test can
/// assert which of the two guards caught a given input and each keeps its own
/// reachable red state. It identifies the *file*, not a directory list, which
/// is what makes it independent of how many toolchain trees exist — the defect
/// D-V1 records is two project homes on one `PATH`, where any exclusion set can
/// only ever name this invocation's own two.
///
/// # POSIX signal
///
/// [`TRAMPOLINE_MARKER`] as **exactly the second line**, read out of a
/// **bounded prefix** of [`TRAMPOLINE_PROBE_BYTES`] — never `contains()`, and
/// never "anywhere in the prefix". Two mechanisms doing two different jobs:
///
/// - The **line-2 anchor** is the discriminator. It is what refuses a file, and
///   equally what stops any *non*-line-2 occurrence from counting — an ordinary
///   tool that merely embeds the string somewhere in its data
///   (`a_file_that_merely_contains_the_marker_later_in_its_body_is_not_refused`),
///   and a *baked* occurrence (E-21). The second case is the reachable one: WP-6
///   interpolates an operator-controlled absolute path into every generated
///   body, so a `$OCX_HOME` spelling the marker would otherwise put it inside
///   the probed head of an ordinary package launcher and refuse it — breaking
///   `ocx launcher exec` for that install, which is the exact class D-V12
///   excluded when it rejected keying on the shared header
///   (`a_launcher_whose_baked_path_spells_the_marker_is_not_refused`).
/// - The **bound** is a read cap, not a discriminator, and owns exactly two
///   effects the anchor does not. It makes the probe one `read(2)` rather than
///   an allocation of whatever binary `PATH` resolved — this runs before every
///   `exec`, including `/bin/sh`. And it decides that a marker which *is* the
///   whole of line two but **begins past** [`TRAMPOLINE_PROBE_BYTES`] does not
///   count, which is the one property that stops the bound being widened away:
///   `a_marker_beyond_the_probe_window_is_not_refused` pins it, and reds when it
///   is.
///
/// Nothing interpolated can reach line one (the shebang) or line two, so the
/// anchor is discriminating by position as well as by spelling.
///
/// # Not `ocx_util::fs::read_bounded`
///
/// The catalog advertises that helper as "read a whole file under a byte
/// ceiling, refusing anything that is not a regular file", which reads like an
/// exact match for the paragraph above and is the first thing a
/// search-before-writing reflex finds. It is the **wrong** helper here: it
/// *errors* when the file exceeds the cap, and a C-028 trampoline body passes
/// 256 bytes as soon as the project root is roughly 146 characters deep.
/// Folded into the fail-open arm below, that error becomes "not a trampoline"
/// — C-069 silently disarmed for exactly the deep-checkout case, with every
/// test rooted at a shallow `tmp_path` still green. This is a prefix read
/// (`File::open` + [`std::io::Read::take`]), where passing the cap is the
/// normal case and carries no verdict.
///
/// # Windows signal
///
/// A sibling `.exec` sidecar beside the resolved `<stem>.exe`, plus a refusal
/// of a resolved path whose *own* extension is `.exec`. C-069's original
/// blob-content clause is **struck** (D-V15): every trampoline `.exe` and every
/// lazy-shim-slot `.exe` is a hardlink of the one committed blob, so content
/// cannot discriminate them and a content match would refuse shim slots —
/// exactly the class D-V12 excluded on POSIX. The second half is not
/// belt-and-braces: `which` treats any file carrying an extension as executable
/// on Windows, so a `PATHEXT` containing `.EXEC` makes the sidecar *text file*
/// itself a resolution answer.
///
/// # Regular files only, and the order is load-bearing
///
/// `std::fs::metadata` decides `is_file()` **before** anything is opened.
/// Opening a FIFO for reading blocks until a writer appears — forever, on a
/// resolution path that runs before every `exec`. A non-regular file is
/// therefore "not a trampoline" without ever being opened.
///
/// # Fails open
///
/// Any I/O error answers *not a trampoline*. This is deliberately the opposite
/// posture from `ocx launcher shim`'s `resolves_inside`, and correctly so:
/// that predicate runs once, over one directory ocx itself created, where the
/// unresolvable case is genuinely suspicious. This one runs on **every**
/// resolution, including `/bin/sh`, so a fail-closed I/O arm would turn a
/// transient `EACCES` on an unrelated binary into a refusal to run an ordinary
/// command. The guard it backstops (C-010's exclusion) is still in force, and
/// an attacker who can make the file unreadable can equally make it absent.
///
/// # Why this has a body while its sibling C-010 exclusion does not
///
/// It sits on `Env::resolve_command`'s success arm, which every bare-name
/// resolution in the workspace reaches — including `/bin/sh`. A stub here is
/// not a deferral, it is a panic on the hot path.
///
/// # Which validation row measures this
///
/// Item **37** (trampoline re-entry) is the only gate that ever observes this
/// predicate's cost, because it is the only one that resolves a command.
/// WP-12e's `bin`-mode reconcile row does **not**: the reconciler puts a
/// directory on `PATH` and resolves nothing, so it measures the cheap half.
/// Reporting the reconcile number as "the cost of C-069" answers a question
/// nobody asked.
pub fn is_ocx_trampoline(path: &std::path::Path) -> bool {
    trampoline_signal(path)
}

/// POSIX half of [`is_ocx_trampoline`]: [`TRAMPOLINE_MARKER`] in the first
/// [`TRAMPOLINE_PROBE_BYTES`] bytes of a regular file.
///
/// Split per platform as two whole functions rather than two `#[cfg]` blocks
/// inside one: the signals share no code, and a `cfg`-gated block in tail
/// position is the shape that silently becomes `()` when someone edits it.
#[cfg(not(windows))]
fn trampoline_signal(path: &std::path::Path) -> bool {
    use std::io::Read as _;

    // Ordered before the open, and load-bearing: opening a FIFO for reading
    // blocks until a writer appears — forever, on the path that runs before
    // every `exec`.
    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut prefix = Vec::with_capacity(TRAMPOLINE_PROBE_BYTES);
    if file
        .take(TRAMPOLINE_PROBE_BYTES as u64)
        .read_to_end(&mut prefix)
        .is_err()
    {
        return false;
    }
    // Bytes, not `str`: an arbitrary file on `PATH` need not be UTF-8, and a
    // lossy conversion would both allocate and let a replacement character
    // land inside the needle. Same idiom as `shim::contains_version_resource`.
    //
    // The SECOND line, matched whole — not "somewhere in the prefix" (E-21).
    // WP-6 bakes an operator-controlled absolute path into every generated
    // body, so a `$OCX_HOME` whose own path text spells the marker would land
    // it inside the probed head of an ordinary *package launcher* and refuse a
    // file that must keep resolving. A position no baked value can occupy is
    // the discriminating one: line 1 is the shebang and line 2 is the marker,
    // both emitted before anything interpolated.
    //
    // `split` yields one element for a `\n`-free prefix, so `nth(1)` is `None`
    // there and the answer is false without a special case. A truncated line 2
    // cannot arise: the marker sits at bytes 10..36 of a 256-byte window.
    prefix
        .split(|byte| *byte == b'\n')
        .nth(1)
        .is_some_and(|line| line == TRAMPOLINE_MARKER.as_bytes())
}

/// Windows half of [`is_ocx_trampoline`]: the sibling `.exec` sidecar, and a
/// resolved path whose own extension is `.exec`.
///
/// No content check (D-V15): every trampoline `.exe` and every lazy-shim-slot
/// `.exe` is a hardlink of the one committed blob, so content cannot tell them
/// apart and a content match would refuse shim slots.
#[cfg(windows)]
fn trampoline_signal(path: &std::path::Path) -> bool {
    // The resolved path's *own* extension first: `which` treats any file
    // carrying an extension as executable on Windows, so a `PATHEXT`
    // containing `.EXEC` makes the sidecar text file itself an answer.
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exec"))
    {
        return true;
    }
    // `with_extension` replaces `<stem>.exe` with `<stem>.exec` — the sidecar
    // the trampoline generator writes beside the shim hardlink.
    std::fs::metadata(path.with_extension("exec")).is_ok_and(|sidecar| sidecar.is_file())
}

/// Parses `OCX_INSECURE_REGISTRIES` into a list of registry hostnames.
pub fn insecure_registries() -> Vec<String> {
    string("OCX_INSECURE_REGISTRIES", String::new())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Serializes a `(host, MirrorConfig)` mirror list into the single JSON
/// object written to [`keys::OCX_MIRRORS`].
///
/// Returns `None` for an empty list so [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) removes any
/// inherited value rather than setting an empty object. Each entry re-encodes
/// as the F5b union shape (a bare string when both roles carry the same URL,
/// a `{registry?, index?}` object otherwise) so a child ocx's [`mirrors`]
/// parses it back through the same [`crate::mirror::parse_mirror_value`]
/// shared branch as a `[mirrors]` TOML table entry.
fn encode_mirrors(mirrors: &[(String, crate::mirror::MirrorConfig)]) -> Option<String> {
    if mirrors.is_empty() {
        return None;
    }
    let mut object = serde_json::Map::with_capacity(mirrors.len());
    for (host, entry) in mirrors {
        // Collapse to the bare-string form when both roles carry the same
        // URL (or one role, mirrored — see doc: F5b "both roles"), matching
        // the shape `parse_mirror_value` produces for a bare string. A
        // `{registry?, index?}` object otherwise, carrying only the roles
        // actually declared.
        let value = if entry.registry.is_some() && entry.registry == entry.index {
            serde_json::Value::String(entry.registry.clone().unwrap_or_default())
        } else {
            let mut fields = serde_json::Map::new();
            if let Some(registry) = &entry.registry {
                fields.insert("registry".to_string(), serde_json::Value::String(registry.clone()));
            }
            if let Some(index) = &entry.index {
                fields.insert("index".to_string(), serde_json::Value::String(index.clone()));
            }
            serde_json::Value::Object(fields)
        };
        object.insert(host.clone(), value);
    }
    match serde_json::to_string(&serde_json::Value::Object(object)) {
        Ok(json) => Some(json),
        Err(error) => {
            log::warn!("failed to encode OCX_MIRRORS: {error}");
            None
        }
    }
}

/// Parses [`keys::OCX_MIRRORS`] (a JSON object of upstream-host → union
/// mirror value, F5b) into a list of `(host, MirrorConfig)` pairs.
///
/// An absent or empty value yields an empty list. A present-but-broken value is
/// a hard error: silently degrading a forwarded `OCX_MIRRORS` to an identity map
/// would route reads to the firewall-blocked origin instead of the mirror — the
/// exact failure mode replace semantics exist to prevent.
///
/// Each per-host JSON value is fed to
/// [`crate::mirror::parse_mirror_value`] — the same shared branch a
/// `[mirrors]` TOML table entry parses through
/// ([`crate::mirror::deserialize_mirrors_table`]) — so a string or
/// `{registry?, index?}` object value parses identically regardless of source
/// format.
///
/// # Errors
///
/// Returns [`MirrorConfigError::MalformedEnvJson`] when the value is not valid
/// JSON, and [`MirrorConfigError::InvalidShape`] / [`MirrorConfigError::NonStringRoleValue`]
/// (naming the offending host) when a per-host value has an unrecognized shape.
///
/// [`MirrorConfigError::MalformedEnvJson`]: crate::mirror::MirrorConfigError::MalformedEnvJson
/// [`MirrorConfigError::InvalidShape`]: crate::mirror::MirrorConfigError::InvalidShape
/// [`MirrorConfigError::NonStringRoleValue`]: crate::mirror::MirrorConfigError::NonStringRoleValue
pub fn mirrors() -> Result<Vec<(String, crate::mirror::MirrorConfig)>, crate::mirror::MirrorConfigError> {
    let Some(raw) = var(keys::OCX_MIRRORS) else {
        return Ok(Vec::new());
    };
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).map_err(|source| crate::mirror::MirrorConfigError::MalformedEnvJson { source })?;

    let mut result = Vec::with_capacity(map.len());
    for (host, value) in map {
        if let Some(config) = crate::mirror::parse_mirror_value(&host, &value)? {
            result.push((host, config));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Test-only here since the package-aware half left: the residual module
    // reads neither validator, but the grammar they define is still this
    // module's contract to state.
    use ocx_util::env::{PATH_SEPARATOR, is_reserved_ocx_key, is_valid_env_key};

    /// `OCX_NO_MODIFY_PATH` is read through `flag` (the same `BooleanString`
    /// path as `--remote`/`--offline`), so both `=1` and `=true` set it true and
    /// an unset var is the `false` default (contract 8, item 7).
    #[test]
    fn ocx_no_modify_path_flag_is_truthy_for_one_and_true() {
        let env = ocx_util::env::overrides::lock();

        env.set(keys::OCX_NO_MODIFY_PATH, "1");
        assert!(
            flag(keys::OCX_NO_MODIFY_PATH, false),
            "OCX_NO_MODIFY_PATH=1 must be true"
        );

        env.set(keys::OCX_NO_MODIFY_PATH, "true");
        assert!(
            flag(keys::OCX_NO_MODIFY_PATH, false),
            "OCX_NO_MODIFY_PATH=true must be true"
        );

        env.remove(keys::OCX_NO_MODIFY_PATH);
        assert!(
            !flag(keys::OCX_NO_MODIFY_PATH, false),
            "unset OCX_NO_MODIFY_PATH must fall back to the false default"
        );
    }

    // ── is_reserved_ocx_key (X1 namespace gate) ──────────────────────────

    #[test]
    fn is_reserved_ocx_key_matches_both_prefixes_case_insensitively() {
        assert!(is_reserved_ocx_key("OCX_OFFLINE"));
        assert!(is_reserved_ocx_key("OCX_DEFAULT_REGISTRY"));
        assert!(is_reserved_ocx_key("__OCX_TESTING_INSTALL_BINARY"));
        // Windows env names are case-insensitive, so a lowercase spelling
        // lands in the same slot and must be caught by the same gate.
        assert!(is_reserved_ocx_key("ocx_offline"));
        assert!(is_reserved_ocx_key("__ocx_testing_x"));
    }

    #[test]
    fn is_reserved_ocx_key_leaves_ordinary_keys_alone() {
        assert!(!is_reserved_ocx_key("CI"));
        assert!(!is_reserved_ocx_key("PATH"));
        assert!(!is_reserved_ocx_key("SOURCE_DATE_EPOCH"));
        // Prefix, not substring: a key that merely mentions ocx is fine.
        assert!(!is_reserved_ocx_key("MY_OCX_HOME"));
        assert!(!is_reserved_ocx_key("OCX"));
    }

    /// `apply_ocx_config` clears any inherited `OCX_ENV` so a stale shell
    /// export — or a payload inherited from an unrelated parent `ocx exec` —
    /// cannot leak into a child that has no project env of its own.
    #[test]
    fn apply_ocx_config_removes_stale_forwarded_env() {
        let mut env = Env::clean();
        env.set(
            keys::OCX_ENV,
            r#"{"entries":[{"key":"STALE","value":"1","type":"constant"}]}"#,
        );
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_ENV).is_none(),
            "apply_ocx_config must clear an inherited OCX_ENV"
        );
    }

    #[test]
    fn env_key_case_insensitive() {
        // On Windows: keys are normalized to uppercase, so "Path" == "PATH" == "path".
        // On Unix: keys are case-sensitive, so each is distinct.
        let lower = EnvKey::new("path");
        let upper = EnvKey::new("PATH");
        let mixed = EnvKey::new("Path");

        #[cfg(windows)]
        {
            assert_eq!(lower, upper);
            assert_eq!(lower, mixed);
            assert_eq!(upper, mixed);
        }

        #[cfg(not(windows))]
        {
            assert_ne!(lower, upper);
            assert_ne!(lower, mixed);
            assert_ne!(upper, mixed);
        }
    }

    // ── is_valid_env_key (shared key validator) ──────────────────────────

    #[test]
    fn is_valid_env_key_accepts_identifiers() {
        assert!(is_valid_env_key("FOO"));
        assert!(is_valid_env_key("_x"));
        assert!(is_valid_env_key("A1"));
        assert!(is_valid_env_key("_OCX_INTERNAL"));
        assert!(is_valid_env_key("PATH"));
    }

    #[test]
    fn is_valid_env_key_rejects_empty() {
        assert!(!is_valid_env_key(""));
    }

    #[test]
    fn is_valid_env_key_rejects_leading_digit() {
        assert!(!is_valid_env_key("1A"));
    }

    #[test]
    fn is_valid_env_key_rejects_space() {
        assert!(!is_valid_env_key("A B"));
    }

    #[test]
    fn is_valid_env_key_rejects_newline() {
        // A newline in the key slot is the CI key-injection vector
        // (GitHub `$GITHUB_ENV` second-variable injection, CWE-77).
        assert!(!is_valid_env_key("A\nB"));
        assert!(!is_valid_env_key("A\rB"));
    }

    #[test]
    fn is_valid_env_key_rejects_equals() {
        // `=` would split the key/value framing of an assignment line.
        assert!(!is_valid_env_key("A=B"));
    }

    #[test]
    fn env_get_set_roundtrip() {
        let mut env = Env::clean();
        env.set("MY_VAR", "hello");
        assert_eq!(env.get("MY_VAR").unwrap(), "hello");
    }

    #[test]
    fn env_add_path_prepends() {
        let mut env = Env::clean();
        env.set("PATH", "/usr/bin");
        env.add_path("PATH", "/opt/bin");
        let path = env.get("PATH").unwrap().to_str().unwrap();
        assert!(path.starts_with("/opt/bin"));
        assert!(path.ends_with("/usr/bin"));
        assert!(path.contains(PATH_SEPARATOR));
    }

    #[test]
    fn env_add_path_to_empty() {
        let mut env = Env::clean();
        env.add_path("PATH", "/opt/bin");
        assert_eq!(env.get("PATH").unwrap(), "/opt/bin");
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use std::os::windows::ffi::OsStringExt;

        #[test]
        fn env_key_ascii_uppercase_only() {
            // Non-ASCII characters are NOT uppercased — only a-z.
            // German ü (U+00FC) should stay as-is, not become Ü.
            let key = EnvKey::new("myVar_ü");
            let expected = EnvKey::new("MYVAR_ü");
            assert_eq!(key, expected);
        }

        #[test]
        fn env_key_preserves_unpaired_surrogates() {
            // Construct an OsString with an unpaired high surrogate (0xD800).
            // This is invalid Unicode but valid WTF-16, which Windows allows.
            let wide: Vec<u16> = vec![0xD800, b'a' as u16, b'b' as u16];
            let key_os = OsString::from_wide(&wide);

            let env_key = EnvKey::new(key_os);

            // The unpaired surrogate must survive; ASCII a/b become A/B.
            let expected_wide: Vec<u16> = vec![0xD800, b'A' as u16, b'B' as u16];
            let expected = EnvKey(OsString::from_wide(&expected_wide));
            assert_eq!(env_key, expected);
        }

        #[test]
        fn env_key_preserves_surrogate_pair() {
            // U+1F600 (😀) encoded as a surrogate pair: 0xD83D 0xDE00
            let wide: Vec<u16> = vec![b'h' as u16, 0xD83D, 0xDE00, b'i' as u16];
            let key_os = OsString::from_wide(&wide);

            let env_key = EnvKey::new(key_os);

            // Surrogate pair must survive intact; h→H, i→I.
            let expected_wide: Vec<u16> = vec![b'H' as u16, 0xD83D, 0xDE00, b'I' as u16];
            let expected = EnvKey(OsString::from_wide(&expected_wide));
            assert_eq!(env_key, expected);
        }

        #[test]
        fn env_key_bmp_non_ascii() {
            // CJK character U+4E16 (世) — single BMP code unit, not ASCII.
            let wide: Vec<u16> = vec![0x4E16, b'x' as u16];
            let key_os = OsString::from_wide(&wide);

            let env_key = EnvKey::new(key_os);

            // 世 stays as-is, x→X.
            let expected_wide: Vec<u16> = vec![0x4E16, b'X' as u16];
            let expected = EnvKey(OsString::from_wide(&expected_wide));
            assert_eq!(env_key, expected);
        }

        #[test]
        fn env_get_case_insensitive() {
            let mut env = Env::clean();
            env.set("Path", "C:\\Windows");
            assert_eq!(env.get("PATH").unwrap(), "C:\\Windows");
            assert_eq!(env.get("path").unwrap(), "C:\\Windows");
        }
    }

    // ── W-2: `Env::add_list` ──────────────────────────────────────────────

    #[test]
    fn add_list_appends_behind_the_existing_value() {
        let mut env = Env::clean();
        env.set("JDK_JAVA_OPTIONS", "-Xmx2g");
        env.add_list("JDK_JAVA_OPTIONS", "-ea", " ");
        assert_eq!(env.get("JDK_JAVA_OPTIONS").unwrap(), "-Xmx2g -ea");
    }

    #[test]
    fn add_list_on_an_absent_key_sets_the_bare_value() {
        let mut env = Env::clean();
        env.add_list("GODEBUG", "gctrace=1", ",");
        assert_eq!(env.get("GODEBUG").unwrap(), "gctrace=1");
    }

    /// Re-applying moves the contribution to the back instead of duplicating
    /// it, so a repeated `direnv` re-entry leaves the value byte-stable.
    #[test]
    fn add_list_is_idempotent_and_moves_to_the_back() {
        let mut env = Env::clean();
        env.set("GODEBUG", "gctrace=1,madvdontneed=1");
        env.add_list("GODEBUG", "gctrace=1", ",");
        assert_eq!(env.get("GODEBUG").unwrap(), "madvdontneed=1,gctrace=1");
        env.add_list("GODEBUG", "gctrace=1", ",");
        assert_eq!(
            env.get("GODEBUG").unwrap(),
            "madvdontneed=1,gctrace=1",
            "a second application must change nothing"
        );
    }

    /// Appending nothing must not bring a variable into existence — the
    /// deliberate difference from `add_path`'s empty-insert asymmetry.
    #[test]
    fn add_list_with_an_empty_value_is_a_no_op_on_an_absent_key() {
        let mut env = Env::clean();
        env.add_list("GODEBUG", "", ",");
        assert!(env.get("GODEBUG").is_none(), "an empty append must not create the key");

        env.set("GODEBUG", "gctrace=1");
        env.add_list("GODEBUG", "", ",");
        assert_eq!(env.get("GODEBUG").unwrap(), "gctrace=1", "nor change an existing one");
    }

    // ── apply_ocx_config ─────────────────────────────────────────────────

    fn view(self_exe: &str) -> OcxConfigView {
        OcxConfigView::new(std::path::PathBuf::from(self_exe))
    }

    #[test]
    fn apply_ocx_config_sets_binary_and_skips_unset_flags() {
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert_eq!(env.get(keys::OCX_BINARY_PIN).unwrap(), "/abs/ocx");
        assert!(env.get(keys::OCX_OFFLINE).is_none());
        assert!(env.get(keys::OCX_REMOTE).is_none());
        assert!(env.get(keys::OCX_CONFIG).is_none());
        assert!(env.get(keys::OCX_INDEX).is_none());
    }

    /// C-013 (#488): a child env carries the home the parent resolved.
    ///
    /// `--clean` strips `HOME` as well, so a child that inherits no `OCX_HOME`
    /// resolves `~/.ocx` from the passwd database — a different store than the
    /// one the parent just materialized the package into. The value is written
    /// unconditionally, and an ambient empty string is corrected rather than
    /// forwarded: an empty `OCX_HOME` is not a home, and passing it on would
    /// hand the child a root the parent never used either.
    #[test]
    fn apply_ocx_config_sets_ocx_home_from_the_resolved_root() {
        let guard = ocx_util::env::overrides::lock();
        let home = guard.isolate_project_home();

        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert_eq!(
            env.get(keys::OCX_HOME).map(std::path::Path::new),
            Some(home.path()),
            "a clean child env must carry the OCX_HOME the parent resolved"
        );

        // Ambient `OCX_HOME=""` is "unset" to `default_ocx_root`, so the child
        // gets the absolute fallback the parent's own stores used.
        guard.set(keys::OCX_HOME, "");
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        let fallback = ocx_util::env::home_dir().expect("a home directory").join(".ocx");
        assert_eq!(
            env.get(keys::OCX_HOME).map(std::path::Path::new),
            Some(fallback.as_path()),
            "an empty ambient OCX_HOME must become the absolute fallback, never the empty string"
        );

        // A relative ambient value means "relative to *this* process' working
        // directory". The child may run somewhere else entirely, so what
        // crosses the spawn is the absolutized form.
        guard.set(keys::OCX_HOME, "rel/dir");
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        let forwarded = std::path::Path::new(env.get(keys::OCX_HOME).expect("OCX_HOME is set"));
        assert!(
            forwarded.is_absolute(),
            "a relative ambient OCX_HOME must be absolutized before it crosses a spawn, got {forwarded:?}"
        );
        assert!(
            forwarded.ends_with("rel/dir"),
            "absolutizing must keep the operator's own path, got {forwarded:?}"
        );
    }

    #[test]
    fn apply_ocx_config_writes_resolution_flags_when_set() {
        let mut cfg = view("/abs/ocx");
        cfg.offline = true;
        cfg.remote = true;
        cfg.config = Some("/cfg.toml".into());
        cfg.index = Some("/idx".into());

        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);
        assert_eq!(env.get(keys::OCX_OFFLINE).unwrap(), "1");
        assert_eq!(env.get(keys::OCX_REMOTE).unwrap(), "1");
        assert_eq!(env.get(keys::OCX_CONFIG).unwrap(), "/cfg.toml");
        assert_eq!(env.get(keys::OCX_INDEX).unwrap(), "/idx");
    }

    #[test]
    fn apply_ocx_config_sets_ocx_frozen_when_set() {
        // `--frozen` is resolution-affecting, so `apply_ocx_config` MUST
        // forward it to a child ocx as `OCX_FROZEN=1` when set, and clear any
        // inherited value when unset so a stale parent-shell export cannot beat
        // the outer ocx's parsed state. Mirrors the OCX_OFFLINE/REMOTE/GLOBAL
        // contract.
        let mut cfg = view("/abs/ocx");
        cfg.frozen = true;
        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);
        assert_eq!(
            env.get(keys::OCX_FROZEN).unwrap(),
            "1",
            "cfg.frozen=true must forward OCX_FROZEN=1 to the child env"
        );

        // Unset: a stale inherited OCX_FROZEN must be cleared.
        let mut env = Env::clean();
        env.set(keys::OCX_FROZEN, "1");
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_FROZEN).is_none(),
            "cfg.frozen=false must clear any inherited OCX_FROZEN"
        );
    }

    #[test]
    fn apply_ocx_config_forwards_ocx_no_verify_when_set() {
        // The auto-verify opt-out is forwarded so a launcher-spawned child
        // install inherits the same CI-wide `OCX_NO_VERIFY`; unset clears a
        // stale inherited value. Mirrors the OCX_OFFLINE/FROZEN/GLOBAL contract.
        let mut cfg = view("/abs/ocx");
        cfg.no_verify = true;
        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);
        assert_eq!(
            env.get(keys::OCX_NO_VERIFY).unwrap(),
            "1",
            "cfg.no_verify=true must forward OCX_NO_VERIFY=1 to the child env"
        );

        let mut env = Env::clean();
        env.set(keys::OCX_NO_VERIFY, "1");
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_NO_VERIFY).is_none(),
            "cfg.no_verify=false must clear any inherited OCX_NO_VERIFY"
        );
    }

    #[test]
    fn apply_ocx_config_sets_ocx_global_when_set() {
        // W2-P3 (adr_global_toolchain_tier.md §Decision 2, C2.2): `--global`
        // is resolution-affecting, so `apply_ocx_config` MUST forward it to a
        // child ocx as `OCX_GLOBAL=1` when set, and remove any inherited
        // value when unset (so a stale parent-shell export cannot beat the
        // outer ocx's parsed state). This plumbing is REAL (not a stub) — the
        // guard PASSES now, pinning the contract against future regression.
        let mut cfg = view("/abs/ocx");
        cfg.global = true;
        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);
        assert_eq!(
            env.get(keys::OCX_GLOBAL).unwrap(),
            "1",
            "cfg.global=true must forward OCX_GLOBAL=1 to the child env"
        );
        // OCX_GLOBAL travels with the resolution-affecting set, never the
        // presentation set (which is never forwarded — it would leak into
        // entrypoint child streams).
        for presentation in ["OCX_LOG", "OCX_LOG_CONSOLE", "OCX_FORMAT", "OCX_COLOR"] {
            assert!(
                env.get(presentation).is_none(),
                "presentation key `{presentation}` must never ride along with OCX_GLOBAL"
            );
        }

        // Unset: a stale inherited OCX_GLOBAL must be cleared so the outer
        // ocx's parsed state (global=false) wins.
        let mut env = Env::clean();
        env.set(keys::OCX_GLOBAL, "1");
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_GLOBAL).is_none(),
            "cfg.global=false must clear any inherited OCX_GLOBAL"
        );
    }

    #[test]
    fn apply_ocx_config_never_writes_the_lazy_keys() {
        // C-006: `OCX_LAZY_MODE` / `OCX_LAZY_REPORT` are NOT
        // resolution-affecting — they change *when* content materializes,
        // never *which* digest resolves — so they are absent from
        // `OcxConfigView` and a child must not receive them as forwarded
        // config. `apply_ocx_config` therefore neither sets them (unlike
        // OCX_FROZEN) nor scrubs them (unlike OCX_ENV, whose stale value
        // would be a payload): it does not touch them at all.
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        // Positive control: the forwarding path really ran, so the two
        // absence assertions below are not passing vacuously.
        assert_eq!(env.get(keys::OCX_BINARY_PIN).unwrap(), "/abs/ocx");
        for key in [keys::OCX_LAZY_MODE, keys::OCX_LAZY_REPORT] {
            assert!(
                env.get(key).is_none(),
                "`{key}` must never be forwarded as resolution-affecting config"
            );
        }

        // An ambient value the caller already placed on the child env is left
        // alone — non-forwarded is not the same as scrubbed.
        let mut env = Env::clean();
        env.set(keys::OCX_LAZY_MODE, "always");
        env.set(keys::OCX_LAZY_REPORT, "progress");
        env.apply_ocx_config(&view("/abs/ocx"));
        assert_eq!(env.get(keys::OCX_LAZY_MODE).unwrap(), "always");
        assert_eq!(env.get(keys::OCX_LAZY_REPORT).unwrap(), "progress");
    }

    #[test]
    fn apply_ocx_config_overwrites_inherited_stale_values() {
        // Outer ocx parses with offline=false, remote=false; inherited env
        // carries stale OCX_OFFLINE=1 / OCX_REMOTE=1 from a prior shell
        // export. The outer's parsed state must win — child ocx must NOT see
        // the stale flags.
        let mut env = Env::clean();
        env.set(keys::OCX_OFFLINE, "1");
        env.set(keys::OCX_REMOTE, "1");
        env.set(keys::OCX_CONFIG, "/stale.toml");
        env.set(keys::OCX_INDEX, "/stale-idx");

        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_OFFLINE).is_none(),
            "stale OCX_OFFLINE must be cleared"
        );
        assert!(env.get(keys::OCX_REMOTE).is_none(), "stale OCX_REMOTE must be cleared");
        assert!(env.get(keys::OCX_CONFIG).is_none(), "stale OCX_CONFIG must be cleared");
        assert!(env.get(keys::OCX_INDEX).is_none(), "stale OCX_INDEX must be cleared");
    }

    /// C-036: the conventional `env://` key variable is on the credential list.
    ///
    /// The scrub test below iterates `CREDENTIAL_KEYS`, so it would stay green
    /// with this entry removed — it would simply test one variable fewer. The
    /// membership is therefore asserted by name: an `env://OCX_SIGNING_KEY`
    /// that a plugin can read is a raw private key handed to third-party code,
    /// which `--key file:<path>` never does.
    #[test]
    fn the_conventional_signing_key_variable_is_a_credential() {
        assert!(
            keys::CREDENTIAL_KEYS.contains(&keys::OCX_SIGNING_KEY),
            "OCX_SIGNING_KEY holds a private key PEM and must be scrubbed from every child env"
        );
    }

    /// C-066: `OCX_ANNOUNCE_GIT_TOKEN` is a credential; its sibling
    /// `OCX_ANNOUNCE_GIT_USERNAME` is not.
    ///
    /// Both polarities in **one** function, so a builder cannot ship half the
    /// rule. The membership rule is "if holding the string authenticates you":
    /// the push token does, the user half of the HTTP Basic pair does not, and
    /// putting a username on the credential list would say it did.
    ///
    /// Asserted **by name**, like `the_conventional_signing_key_variable_is_a_credential`
    /// above and for the same reason: `apply_ocx_config_never_forwards_credential_tokens`
    /// iterates `CREDENTIAL_KEYS`, so removing an entry leaves it green — it
    /// simply tests one variable fewer.
    ///
    /// The names are spelled as literals rather than read from a `keys`
    /// constant on purpose: a constant would be compared against itself, so a
    /// typo in its value would satisfy both sides. The literal is the contract's
    /// own spelling, quoted the way `exit_code_forge_capability_unavailable_is_86`
    /// quotes 86.
    ///
    /// Red at the stub: the constant is not in the set.
    /// Mutation once implemented: remove `OCX_ANNOUNCE_GIT_TOKEN` from the array
    /// (the positive reds and the scrub test above does not); add
    /// `OCX_ANNOUNCE_GIT_USERNAME` to it (the negative reds).
    #[test]
    fn credential_keys_contains_git_token_not_username() {
        assert!(
            keys::CREDENTIAL_KEYS.contains(&"OCX_ANNOUNCE_GIT_TOKEN"),
            "OCX_ANNOUNCE_GIT_TOKEN is a push credential and must be scrubbed from every child env"
        );
        assert!(
            !keys::CREDENTIAL_KEYS.contains(&"OCX_ANNOUNCE_GIT_USERNAME"),
            "OCX_ANNOUNCE_GIT_USERNAME is the user half of an HTTP Basic pair, not a credential"
        );
    }

    #[test]
    fn apply_ocx_config_never_forwards_credential_tokens() {
        // Credential exemption (see subsystem-cli.md): bearer-credential env
        // vars must NEVER be forwarded to a child env via apply_ocx_config.
        // Forwarding a short-lived OIDC token to every subprocess broadens the
        // attack surface unnecessarily — the value should be read once by the
        // CLI command that needs it, never propagated through OcxConfigView.
        //
        // This test guards the boundary: even when the parent env already has
        // OCX_IDENTITY_TOKEN set (e.g. inherited via Env::new()), the call to
        // apply_ocx_config must leave the child-env entry absent.
        let mut env = Env::clean();
        for credential in keys::CREDENTIAL_KEYS {
            env.set(*credential, "tok-secret");
        }
        env.apply_ocx_config(&view("/abs/ocx"));
        for credential in keys::CREDENTIAL_KEYS {
            assert!(
                env.get(credential).is_none(),
                "credential token `{credential}` must never be forwarded by apply_ocx_config",
            );
        }
    }

    #[test]
    fn apply_ocx_config_never_sets_presentation_keys() {
        // Presentation flags (--log-level, --format, --color) must not
        // propagate via env — they would leak into a launcher's child stream.
        // The view does not even carry them, but assert the corresponding
        // canonical keys are absent regardless. `OCX_LOG` and `OCX_LOG_CONSOLE`
        // are the real env vars consumed by the CLI's `LogSettings::build_env_filter`;
        // `OCX_FORMAT` / `OCX_COLOR` are the canonical names that would bind
        // to `--format` / `--color` if those ever gained env counterparts.
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        for forbidden in ["OCX_LOG", "OCX_LOG_CONSOLE", "OCX_FORMAT", "OCX_COLOR"] {
            assert!(
                env.get(forbidden).is_none(),
                "presentation key `{forbidden}` must never be set by apply_ocx_config",
            );
        }
    }

    /// A hermetic parent spawns a hermetic child. Without the forward the child
    /// re-reads the discovered config chain the parent pruned, and the two
    /// frames of one launch resolve `[records]` (and everything else) against
    /// different configuration.
    #[test]
    fn apply_ocx_config_forwards_no_config_optin_from_the_view() {
        let mut config = view("/abs/ocx");
        config.no_config = true;
        let mut env = Env::clean();
        env.apply_ocx_config(&config);
        assert_eq!(
            env.get(keys::OCX_NO_CONFIG).and_then(std::ffi::OsStr::to_str),
            Some("1"),
            "a view resolved hermetic must forward OCX_NO_CONFIG to the child env"
        );
    }

    /// The remove half of set-or-remove: the **view's** bool is the sole
    /// authority, so a value inherited on the child env is stripped when this
    /// frame resolved hermetic false. Without it a stale `OCX_NO_CONFIG=1`
    /// export makes a child hermetic that the parent never was.
    #[test]
    fn apply_ocx_config_removes_stale_no_config_when_the_view_says_false() {
        let mut env = Env::clean();
        env.set(keys::OCX_NO_CONFIG, "1");
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_NO_CONFIG).is_none(),
            "an inherited OCX_NO_CONFIG must be removed when the view resolved false"
        );
    }

    /// The discriminating case for reading the value off the view rather than
    /// the ambient env: this process's own `OCX_NO_CONFIG` must not decide what
    /// the child inherits. `Context::try_init` reads it once at the loader seam,
    /// and a second read here could contradict the chain the parent loaded —
    /// after `--config` pruning, or in any embedder that never consulted the
    /// env at all. Reds against the ambient-read form this replaced.
    #[test]
    fn apply_ocx_config_ignores_an_ambient_no_config_the_view_did_not_carry() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(keys::OCX_NO_CONFIG, "1");

        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_NO_CONFIG).is_none(),
            "the ambient OCX_NO_CONFIG is not the authority — the view is"
        );
    }

    #[test]
    fn apply_ocx_config_forwards_yanked_optin_from_ambient() {
        let guard = ocx_util::env::overrides::lock();

        // A truthy ambient opt-in forwards to the child env so a nested ocx
        // resolving an index-sourced yanked tag honours the same override.
        guard.set(keys::OCX_ALLOW_YANKED, "1");
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert_eq!(
            env.get(keys::OCX_ALLOW_YANKED).and_then(std::ffi::OsStr::to_str),
            Some("1"),
            "a truthy OCX_ALLOW_YANKED must forward to the child env"
        );

        // Absent (or falsy) → not set on the child env.
        guard.remove(keys::OCX_ALLOW_YANKED);
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_ALLOW_YANKED).is_none(),
            "an absent OCX_ALLOW_YANKED must not be set on the child env"
        );
    }

    /// ocx-sh/ocx#400 — the consent opt-out survives the hop into a child ocx.
    ///
    /// `ocx exec --clean` composes its child from [`Env::clean`](crate::env::Env::clean), so a script
    /// it runs that itself calls `ocx pull` sees only what this function wrote.
    /// Without the forward that inner frame stamps a consent the outer
    /// invocation was explicitly told not to record — the defect is invisible
    /// from the outer command's own behaviour, which is why it is asserted
    /// here. The sibling below covers the other half, an `ocx exec
    /// --no-consent` whose refusal lives in argv and reaches the child only
    /// through this key.
    ///
    /// Red state: delete either arm of the `OCX_NO_CONSENT` block in
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config).
    #[test]
    fn apply_ocx_config_forwards_no_consent_from_ambient() {
        let guard = ocx_util::env::overrides::lock();

        guard.set(keys::OCX_NO_CONSENT, "1");
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert_eq!(
            env.get(keys::OCX_NO_CONSENT).and_then(std::ffi::OsStr::to_str),
            Some("1"),
            "a truthy OCX_NO_CONSENT must forward to the child env"
        );

        // Absent (or falsy) → not set on the child env, so a stale export in
        // the parent shell cannot suppress a stamp the child should write.
        guard.remove(keys::OCX_NO_CONSENT);
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_NO_CONSENT).is_none(),
            "an absent OCX_NO_CONSENT must not be set on the child env"
        );
    }

    /// ocx-sh/ocx#400 — an `ocx exec --no-consent` reaches the nested ocx a
    /// child launches, and an `ocx exec --consent` does not clear an inherited
    /// refusal.
    ///
    /// The flag lives in argv, which no child process ever sees, so this key is
    /// its only channel. Both arms are asserted because they are the two
    /// directions of one deliberate asymmetry: refusal inherits downward,
    /// permission does not.
    ///
    /// Red state, first arm: delete the `cfg.no_consent ||` half of the
    /// `OCX_NO_CONSENT` block in [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) — the flag then
    /// stops at the process boundary and the nested `ocx pull` stamps.
    /// Red state, second arm: make the block mirror the flag both ways
    /// (`if cfg.no_consent { set } else { remove }`) — a `--consent` then wipes
    /// an ambient refusal on the way down.
    #[test]
    fn apply_ocx_config_forwards_no_consent_from_the_invocation_flag() {
        let guard = ocx_util::env::overrides::lock();

        // The flag refused, the ambient environment said nothing: the refusal
        // must still reach the child.
        guard.remove(keys::OCX_NO_CONSENT);
        let mut cfg = view("/abs/ocx");
        cfg.no_consent = true;
        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);
        assert_eq!(
            env.get(keys::OCX_NO_CONSENT).and_then(std::ffi::OsStr::to_str),
            Some("1"),
            "an invocation's own --no-consent must forward to the child env"
        );

        // The mirror image: no flag refusal, but the environment refused. A
        // `--consent` decides the one project this invocation targets and must
        // not grant anything the child goes on to touch.
        guard.set(keys::OCX_NO_CONSENT, "1");
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert_eq!(
            env.get(keys::OCX_NO_CONSENT).and_then(std::ffi::OsStr::to_str),
            Some("1"),
            "--consent must not clear an inherited OCX_NO_CONSENT from the child env"
        );
    }

    #[test]
    fn clean_env_carries_no_ocx_keys() {
        // `Env::clean()` returns an empty map; nothing inherited from the
        // running process. Authoritative source for OCX_* on a child env is
        // `apply_ocx_config`, not the parent shell.
        let env = Env::clean();
        for key in [
            keys::OCX_BINARY_PIN,
            keys::OCX_OFFLINE,
            keys::OCX_REMOTE,
            keys::OCX_CONFIG,
            keys::OCX_INDEX,
        ] {
            assert!(env.get(key).is_none(), "Env::clean must not contain `{key}`");
        }
    }

    // ── resolve_command ──────────────────────────────────────────────────

    // ── Step 3.1 specification tests: OCX_MIRRORS round-trip ──────────────────

    /// `mirrors()` parses what `apply_ocx_config`/`encode_mirrors` emits —
    /// a basic round-trip for the simplest case.
    ///
    /// Traces: plan Testing Strategy — "`OCX_MIRRORS` JSON round-trip
    /// (mirrors() parses what apply_ocx_config/encode_mirrors emits)"; ADR
    /// review A3.
    #[test]
    fn ocx_mirrors_json_roundtrip_basic() {
        let env = ocx_util::env::overrides::lock();
        let input = vec![(
            "ghcr.io".to_string(),
            crate::mirror::MirrorConfig {
                registry: Some("https://corp.jfrog.io/ghcr-remote".to_string()),
                index: None,
                registry_system_locked: false,
                index_system_locked: false,
            },
        )];
        // encode_mirrors is private; drive it through apply_ocx_config so we
        // test the public contract (encode → set in env → mirrors() parses).
        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.mirrors = input.clone();
        let mut child_env = Env::clean();
        child_env.apply_ocx_config(&cfg);

        // Retrieve the encoded value and inject it via the test env override
        // so mirrors() reads it.
        let encoded = child_env
            .get(keys::OCX_MIRRORS)
            .expect("OCX_MIRRORS must be set when mirrors is non-empty")
            .to_str()
            .expect("OCX_MIRRORS must be valid UTF-8")
            .to_string();
        env.set(keys::OCX_MIRRORS, encoded);

        let parsed = mirrors().expect("well-formed OCX_MIRRORS must parse");
        assert_eq!(parsed.len(), 1, "parsed mirrors must have one entry");
        assert_eq!(parsed[0].0, "ghcr.io");
        assert_eq!(
            parsed[0].1.registry.as_deref(),
            Some("https://corp.jfrog.io/ghcr-remote")
        );
    }

    /// `mirrors()` round-trip including a `localhost:5000` host key and a url
    /// with a query string — specifically tests the delimiter-safety guarantee.
    ///
    /// Traces: plan Testing Strategy — "including a `localhost:5000` host key
    /// and a url with a query string"; ADR review A3 (JSON not comma/`=`).
    #[test]
    fn ocx_mirrors_json_roundtrip_localhost_and_query_string() {
        let env = ocx_util::env::overrides::lock();
        let input = vec![
            (
                "localhost:5000".to_string(),
                crate::mirror::MirrorConfig {
                    registry: Some("https://corp.mirror.io/proxy?region=eu".to_string()),
                    index: None,
                    registry_system_locked: false,
                    index_system_locked: false,
                },
            ),
            (
                "ghcr.io".to_string(),
                crate::mirror::MirrorConfig {
                    registry: Some("https://corp.jfrog.io/ghcr-remote".to_string()),
                    index: None,
                    registry_system_locked: false,
                    index_system_locked: false,
                },
            ),
        ];
        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.mirrors = input.clone();
        let mut child_env = Env::clean();
        child_env.apply_ocx_config(&cfg);

        let encoded = child_env
            .get(keys::OCX_MIRRORS)
            .expect("OCX_MIRRORS must be set when mirrors is non-empty")
            .to_str()
            .expect("OCX_MIRRORS must be valid UTF-8")
            .to_string();
        env.set(keys::OCX_MIRRORS, encoded);

        let mut parsed = mirrors().expect("well-formed OCX_MIRRORS must parse");
        // Sort for deterministic comparison (HashMap iteration order may differ).
        parsed.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(parsed.len(), 2, "parsed mirrors must have two entries");

        // Find each entry regardless of order.
        let localhost = parsed.iter().find(|(h, _)| h == "localhost:5000");
        let ghcr = parsed.iter().find(|(h, _)| h == "ghcr.io");

        assert!(localhost.is_some(), "localhost:5000 entry must survive round-trip");
        assert_eq!(
            localhost.unwrap().1.registry.as_deref(),
            Some("https://corp.mirror.io/proxy?region=eu"),
            "url with query string must survive verbatim"
        );
        assert!(ghcr.is_some(), "ghcr.io entry must survive round-trip");
    }

    /// Empty mirrors list → `encode_mirrors` returns `None` → `apply_ocx_config`
    /// removes any stale `OCX_MIRRORS` value.
    ///
    /// Traces: ADR — empty list means no mirror configured, must remove the key.
    #[test]
    fn empty_mirrors_removes_ocx_mirrors_key() {
        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.mirrors = vec![];

        let mut env = Env::clean();
        // Pre-set a stale value to confirm it is removed.
        env.set(keys::OCX_MIRRORS, r#"{"ghcr.io":"https://old.corp/remote"}"#);
        env.apply_ocx_config(&cfg);

        assert!(
            env.get(keys::OCX_MIRRORS).is_none(),
            "empty mirrors must remove OCX_MIRRORS from the child env"
        );
    }

    /// Malformed JSON in `OCX_MIRRORS` → `mirrors()` is a HARD error.
    ///
    /// Silently degrading a forwarded mirror map to an identity map would route
    /// reads to the firewall-blocked origin instead of the mirror — the exact
    /// anti-goal replace semantics exist to prevent. So a present-but-broken
    /// value must abort, not warn-and-empty.
    ///
    /// Traces: review Cluster-1 fail-loud — malformed forwarded `OCX_MIRRORS`
    /// is a hard error on both parent and child paths.
    #[test]
    fn malformed_ocx_mirrors_is_hard_error() {
        use crate::mirror::MirrorConfigError;

        let env = ocx_util::env::overrides::lock();
        env.set(keys::OCX_MIRRORS, "this is not valid json {{{");

        let result = mirrors();
        assert!(
            matches!(result, Err(MirrorConfigError::MalformedEnvJson { .. })),
            "malformed OCX_MIRRORS must yield MalformedEnvJson, got: {result:?}"
        );
    }

    /// A per-host value in `OCX_MIRRORS` that is neither a string nor a
    /// `{registry?, index?}` object → `mirrors()` is a HARD error naming the
    /// offending host. A silent `filter_map` drop would degrade the map for
    /// that host, so the entry must abort instead.
    ///
    /// Traces: review Cluster-1 fail-loud — non-string env value names the
    /// host; F5b — a bare integer is not a recognized union shape.
    #[test]
    fn non_string_ocx_mirrors_value_is_hard_error() {
        use crate::mirror::MirrorConfigError;

        let env = ocx_util::env::overrides::lock();
        // ghcr.io maps to a number, not a string url or a {registry?, index?} object.
        env.set(keys::OCX_MIRRORS, r#"{"ghcr.io":42}"#);

        let result = mirrors();
        assert!(
            matches!(result, Err(MirrorConfigError::InvalidShape { ref upstream, .. }) if upstream == "ghcr.io"),
            "non-string OCX_MIRRORS value must yield InvalidShape naming ghcr.io, got: {result:?}"
        );
    }

    /// A plain JSON string per-host value stays valid under the F5b union —
    /// it sets both traffic roles, parity with a bare `[mirrors."<host>"]`
    /// TOML string.
    #[test]
    fn ocx_mirrors_plain_string_value_sets_both_roles() {
        let env = ocx_util::env::overrides::lock();
        env.set(keys::OCX_MIRRORS, r#"{"ghcr.io":"https://mirror.corp/both-roles"}"#);

        let parsed = mirrors().expect("a plain string per-host value must parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "ghcr.io");
        assert_eq!(parsed[0].1.registry.as_deref(), Some("https://mirror.corp/both-roles"));
        assert_eq!(parsed[0].1.index.as_deref(), Some("https://mirror.corp/both-roles"));
    }

    /// A `{registry?, index?}` object per-host value splits per role — parity
    /// with a `[mirrors."<host>"]` TOML table entry.
    #[test]
    fn ocx_mirrors_object_value_splits_per_role() {
        let env = ocx_util::env::overrides::lock();
        env.set(
            keys::OCX_MIRRORS,
            r#"{"index.ocx.sh":{"index":"https://artifactory.corp/ocx-index"}}"#,
        );

        let parsed = mirrors().expect("an object value must parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "index.ocx.sh");
        assert!(
            parsed[0].1.registry.is_none(),
            "an index-only object value must leave registry unset"
        );
        assert_eq!(parsed[0].1.index.as_deref(), Some("https://artifactory.corp/ocx-index"));
    }

    /// An absent `OCX_MIRRORS` yields an empty list, not an error.
    #[test]
    fn ocx_mirrors_absent_env_yields_empty_vec() {
        let env = ocx_util::env::overrides::lock();
        env.remove(keys::OCX_MIRRORS);

        let parsed = mirrors().expect("an absent OCX_MIRRORS must not error");
        assert!(parsed.is_empty(), "absent OCX_MIRRORS must yield an empty vec");
    }

    /// An explicit empty JSON object also yields an empty list.
    #[test]
    fn ocx_mirrors_empty_object_yields_empty_vec() {
        let env = ocx_util::env::overrides::lock();
        env.set(keys::OCX_MIRRORS, "{}");

        let parsed = mirrors().expect("an empty object must not error");
        assert!(parsed.is_empty(), "OCX_MIRRORS=\"{{}}\" must yield an empty vec");
    }

    // ── encode_mirrors ↔ mirrors() identity round trips (F5b) ────────────────

    /// When `registry == index` for a host, `encode_mirrors` collapses the
    /// entry to a bare JSON string (not an object) — and `mirrors()` parses
    /// that bare string back into a `MirrorConfig` with both roles set to the
    /// same URL, an identity round trip.
    #[test]
    fn ocx_mirrors_encode_roundtrip_string_form_collapses_to_bare_json_string() {
        let env = ocx_util::env::overrides::lock();
        let input = vec![(
            "ghcr.io".to_string(),
            crate::mirror::MirrorConfig {
                registry: Some("https://corp.jfrog.io/ghcr-remote".to_string()),
                index: Some("https://corp.jfrog.io/ghcr-remote".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        )];
        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.mirrors = input.clone();
        let mut child_env = Env::clean();
        child_env.apply_ocx_config(&cfg);

        let encoded = child_env
            .get(keys::OCX_MIRRORS)
            .expect("OCX_MIRRORS must be set")
            .to_str()
            .expect("OCX_MIRRORS must be valid UTF-8")
            .to_string();

        let value: serde_json::Value = serde_json::from_str(&encoded).expect("encoded OCX_MIRRORS must be valid JSON");
        assert!(
            value["ghcr.io"].is_string(),
            "when registry == index the entry must collapse to a bare JSON string, got: {value}"
        );

        env.set(keys::OCX_MIRRORS, encoded);
        let parsed = mirrors().expect("well-formed OCX_MIRRORS must parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].1, input[0].1,
            "the string-form entry must round-trip identically"
        );
    }

    /// A registry-only (split) entry round-trips without gaining an index
    /// value — encode/parse identity for the split form.
    #[test]
    fn ocx_mirrors_encode_roundtrip_split_form_preserves_registry_only() {
        let env = ocx_util::env::overrides::lock();
        let input = vec![(
            "index.ocx.sh".to_string(),
            crate::mirror::MirrorConfig {
                registry: Some("https://mirror.corp/registry-side".to_string()),
                index: None,
                registry_system_locked: false,
                index_system_locked: false,
            },
        )];
        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.mirrors = input.clone();
        let mut child_env = Env::clean();
        child_env.apply_ocx_config(&cfg);

        let encoded = child_env
            .get(keys::OCX_MIRRORS)
            .expect("OCX_MIRRORS must be set")
            .to_str()
            .expect("OCX_MIRRORS must be valid UTF-8")
            .to_string();
        env.set(keys::OCX_MIRRORS, encoded);

        let parsed = mirrors().expect("well-formed OCX_MIRRORS must parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "index.ocx.sh");
        assert_eq!(
            parsed[0].1.registry.as_deref(),
            Some("https://mirror.corp/registry-side")
        );
        assert!(
            parsed[0].1.index.is_none(),
            "a registry-only entry must round-trip without gaining an index value"
        );
    }

    /// A both-roles-differing entry (registry and index point at distinct
    /// URLs) round-trips with both values preserved distinctly.
    #[test]
    fn ocx_mirrors_encode_roundtrip_both_roles_differing_preserved() {
        let env = ocx_util::env::overrides::lock();
        let input = vec![(
            "index.ocx.sh".to_string(),
            crate::mirror::MirrorConfig {
                registry: Some("https://mirror.corp/registry-side".to_string()),
                index: Some("https://mirror.corp/index-side".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        )];
        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.mirrors = input.clone();
        let mut child_env = Env::clean();
        child_env.apply_ocx_config(&cfg);

        let encoded = child_env
            .get(keys::OCX_MIRRORS)
            .expect("OCX_MIRRORS must be set")
            .to_str()
            .expect("OCX_MIRRORS must be valid UTF-8")
            .to_string();
        env.set(keys::OCX_MIRRORS, encoded);

        let parsed = mirrors().expect("well-formed OCX_MIRRORS must parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].1.registry.as_deref(),
            Some("https://mirror.corp/registry-side")
        );
        assert_eq!(
            parsed[0].1.index.as_deref(),
            Some("https://mirror.corp/index-side"),
            "distinct role URLs must both survive the round trip"
        );
    }

    // ── OCX_PATCHES forwarding via apply_ocx_config ──────────────────────────

    /// `apply_ocx_config` with `patches = None` does NOT set `OCX_PATCHES` and
    /// removes any stale inherited value.
    ///
    /// Traces: Phase 1 — "No `[patches]` -> apply_ocx_config does NOT set
    /// OCX_PATCHES (no-op / byte-identical env)"; stub manifest — "forwarded
    /// ONLY when present (mirror OCX_MIRRORS exactly)".
    #[test]
    fn apply_ocx_config_does_not_set_ocx_patches_when_patches_none() {
        let cfg = view("/abs/ocx");
        // patches is None by default in OcxConfigView::new()
        assert!(cfg.patches.is_none());

        let mut env = Env::clean();
        // Pre-set a stale value to confirm it is removed.
        env.set(
            keys::OCX_PATCHES,
            r#"{"registry":"stale","path_template":"x","required":true}"#,
        );
        env.apply_ocx_config(&cfg);

        assert!(
            env.get(keys::OCX_PATCHES).is_none(),
            "patches=None must remove any stale OCX_PATCHES from the child env"
        );
    }

    /// `apply_ocx_config` with `patches = Some(resolved)` sets `OCX_PATCHES` to
    /// a non-empty JSON string.
    ///
    /// Traces: Phase 1 — "OCX_PATCHES round-trip: an OcxConfigView carrying
    /// resolved patches -> apply_ocx_config sets OCX_PATCHES to JSON".
    #[test]
    fn apply_ocx_config_sets_ocx_patches_when_patches_some() {
        let mut cfg = view("/abs/ocx");
        cfg.patches = Some(crate::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: "corp.example.com/patches".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        });

        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);

        let raw = env
            .get(keys::OCX_PATCHES)
            .expect("OCX_PATCHES must be set when patches is Some")
            .to_str()
            .expect("OCX_PATCHES must be valid UTF-8");
        // Must be parseable JSON containing the registry.
        assert!(
            raw.contains("corp.example.com/patches"),
            "OCX_PATCHES JSON must contain the registry; got: {raw}"
        );
    }

    /// OCX_PATCHES full round-trip through `apply_ocx_config` then
    /// `patches_from_env`: child env carries the same `ResolvedPatchConfig`.
    ///
    /// Traces: Phase 1 — "OCX_PATCHES round-trip: an OcxConfigView carrying
    /// resolved patches -> apply_ocx_config sets OCX_PATCHES to JSON -> parsing
    /// OCX_PATCHES back yields the same resolved patches"; block-tier requirement
    /// C5.
    #[test]
    fn apply_ocx_config_ocx_patches_round_trip_via_patches_from_env() {
        use crate::patch::patches_from_env;

        let env_guard = ocx_util::env::overrides::lock();

        let original = crate::patch::ResolvedPatchConfig {
            system_required: false,
            // Non-empty opt-out so this end-to-end wire test proves
            // `apply_ocx_config` → `patches_from_env` forwards the project
            // `no-patches` set across the process boundary (the launcher path).
            no_patches: ["ghcr.io/acme/cli".to_string()].into_iter().collect(),
            registry: "internal.company.com/ocx-patches".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };

        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.patches = Some(original.clone());

        let mut child_env = Env::clean();
        child_env.apply_ocx_config(&cfg);

        // Inject the encoded OCX_PATCHES from child_env into the test env override
        // so patches_from_env() can read it.
        let encoded = child_env
            .get(keys::OCX_PATCHES)
            .expect("OCX_PATCHES must be set when patches is Some")
            .to_str()
            .expect("OCX_PATCHES must be valid UTF-8")
            .to_string();
        env_guard.set(keys::OCX_PATCHES, encoded);

        let parsed = patches_from_env()
            .expect("well-formed OCX_PATCHES must parse")
            .expect("non-empty OCX_PATCHES must yield Some(resolved)");

        assert_eq!(
            parsed, original,
            "patches_from_env must recover the same ResolvedPatchConfig forwarded by apply_ocx_config"
        );
    }

    // ── OCX_PATCH_SNAPSHOT forwarding via apply_ocx_config ───────────────────

    /// `apply_ocx_config` with `patch_snapshot = Some(path)` must set
    /// `OCX_PATCH_SNAPSHOT` to that path on the child env.
    ///
    /// Traceability: Phase 5B spec test 3 — OCX_PATCH_SNAPSHOT forwarded when Some.
    ///
    /// NOTE: This test PASSES against the current code because `apply_ocx_config`
    /// already forwards `patch_snapshot` (the implementation is already in place).
    /// The test is included here as a pinning guard to prevent regression and to
    /// satisfy the spec test contract.
    #[test]
    fn apply_ocx_config_sets_ocx_patch_snapshot_when_some() {
        let mut cfg = view("/abs/ocx");
        cfg.patch_snapshot = Some(std::path::PathBuf::from("/project/patches.snapshot.json"));

        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);

        let value = env
            .get(keys::OCX_PATCH_SNAPSHOT)
            .expect("OCX_PATCH_SNAPSHOT must be set when patch_snapshot is Some");
        assert_eq!(
            value.to_str().unwrap(),
            "/project/patches.snapshot.json",
            "OCX_PATCH_SNAPSHOT must equal the configured path"
        );
    }

    /// `apply_ocx_config` with `patch_snapshot = None` must remove any stale
    /// `OCX_PATCH_SNAPSHOT` value from the child env, so the outer ocx's state
    /// (no snapshot) wins over any inherited value.
    ///
    /// Traceability: Phase 5B spec test 3 — OCX_PATCH_SNAPSHOT removed when None.
    ///
    /// NOTE: This test PASSES against the current code. Included as a regression guard.
    #[test]
    fn apply_ocx_config_removes_ocx_patch_snapshot_when_none() {
        // Build a view with patch_snapshot = None (the default).
        let cfg = view("/abs/ocx");
        assert!(cfg.patch_snapshot.is_none());

        let mut env = Env::clean();
        // Pre-set a stale value to confirm it is cleared.
        env.set(keys::OCX_PATCH_SNAPSHOT, "/stale/patches.snapshot.json");
        env.apply_ocx_config(&cfg);

        assert!(
            env.get(keys::OCX_PATCH_SNAPSHOT).is_none(),
            "patch_snapshot=None must remove any stale OCX_PATCH_SNAPSHOT from the child env"
        );
    }

    /// Writes `name` into `dir` with the given mode, returning its path.
    #[cfg(unix)]
    fn write_binary(dir: &std::path::Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\ntrue\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    /// macOS puts `TempDir` under the `/tmp` -> `/private/tmp` symlink, so a
    /// raw tempdir path never equals a resolved one. Compare canonical forms.
    ///
    /// Gated like every one of its callers: they are all `#[cfg(unix)]`, so on
    /// Windows this is uncallable and `dead_code` refuses the build.
    #[cfg(unix)]
    fn same_file(left: &std::path::Path, right: &std::path::Path) -> bool {
        match (dunce::canonicalize(left), dunce::canonicalize(right)) {
            (Ok(l), Ok(r)) => l == r,
            _ => left == right,
        }
    }

    /// Only a lone bare name may be joined onto a package directory.
    ///
    /// The package scan does `dir.join(command)`, and `join` with anything
    /// carrying its own root or prefix *replaces* the base — so a name that is
    /// not exactly one normal component would stat outside every package
    /// directory and be reported as a copy the package ships (CWE-22 class).
    /// `C:tool` is the Windows form that a separator test cannot see: it has
    /// no separator at all, yet `join` keeps only the drive.
    #[test]
    fn command_is_path_admits_only_a_single_normal_component() {
        for bare in ["tool", "tool.exe", "tool-1.2"] {
            assert!(
                !command_is_path(OsStr::new(bare)),
                "'{bare}' is a lone bare name and must be package-scanned"
            );
        }
        for bearing in ["..", "a/b", "./tool", "/abs/tool", ""] {
            assert!(
                command_is_path(OsStr::new(bearing)),
                "'{bearing}' must delegate to resolve_command, never be joined onto a package dir"
            );
        }
        #[cfg(windows)]
        for bearing in ["C:tool", "C:\\tool", "\\\\server\\share\\tool"] {
            assert!(
                command_is_path(OsStr::new(bearing)),
                "'{bearing}' carries a drive/prefix that would replace the package dir on join"
            );
        }
    }

    // ── OCX_RECORDS_* — env as input, and env as forwarded output ────────────

    /// The resolved sink and template forward to the child env, so every frame
    /// of one launch chain records into the same place under the same grammar.
    #[test]
    fn apply_ocx_config_forwards_records_dir_and_name_when_set() {
        let mut cfg = view("/abs/ocx");
        cfg.records.dir = Some(std::path::PathBuf::from("/var/log/ocx-records"));
        cfg.records.name = Some("{time}-{host}-{pid}.json".to_string());

        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);

        assert_eq!(
            env.get(keys::OCX_RECORDS_DIR).unwrap(),
            "/var/log/ocx-records",
            "a resolved sink must forward as OCX_RECORDS_DIR"
        );
        assert_eq!(
            env.get(keys::OCX_RECORDS_NAME).unwrap(),
            "{time}-{host}-{pid}.json",
            "the resolved template must forward as OCX_RECORDS_NAME"
        );
    }

    /// The remove half of set-or-remove: with no sink resolved, an inherited
    /// value is actively stripped rather than left alone. Without this, one
    /// operator's `OCX_RECORDS_DIR` export survives into a child that resolved
    /// recording off — records written under a policy nobody configured.
    #[test]
    fn apply_ocx_config_removes_inherited_records_dir_and_name_when_unset() {
        let cfg = view("/abs/ocx");
        assert!(cfg.records.dir.is_none(), "the default view resolves recording off");

        let mut env = Env::clean();
        env.set(keys::OCX_RECORDS_DIR, "/stale/sink");
        env.set(keys::OCX_RECORDS_NAME, "{time}-stale.json");
        env.apply_ocx_config(&cfg);

        assert!(
            env.get(keys::OCX_RECORDS_DIR).is_none(),
            "an inherited OCX_RECORDS_DIR must be removed, not left to survive"
        );
        assert!(
            env.get(keys::OCX_RECORDS_NAME).is_none(),
            "an inherited OCX_RECORDS_NAME must be removed, not left to survive"
        );
    }

    /// `name` is removed independently of `dir`: a sink with a defaulted
    /// template must not inherit a stale pattern from the parent shell, or the
    /// collector's glob silently matches files the operator never described.
    #[test]
    fn apply_ocx_config_removes_stale_records_name_even_with_a_sink() {
        let mut cfg = view("/abs/ocx");
        cfg.records.dir = Some(std::path::PathBuf::from("/var/log/ocx-records"));
        assert!(cfg.records.name.is_none());

        let mut env = Env::clean();
        env.set(keys::OCX_RECORDS_NAME, "{time}-stale.json");
        env.apply_ocx_config(&cfg);

        assert_eq!(env.get(keys::OCX_RECORDS_DIR).unwrap(), "/var/log/ocx-records");
        assert!(
            env.get(keys::OCX_RECORDS_NAME).is_none(),
            "an inherited OCX_RECORDS_NAME must be removed even when a sink is set"
        );
    }

    /// `resolve_command` must find a well-known binary that exists on PATH.
    #[cfg(unix)]
    #[test]
    fn resolve_command_finds_sh_on_unix() {
        let env = Env::new();
        let resolved = env.resolve_command("sh").unwrap();
        // On any Unix system `sh` must exist somewhere on PATH.
        assert!(
            resolved.exists(),
            "resolve_command(\"sh\") must find a real path; got {}",
            resolved.display()
        );
    }

    /// A bare name the composed PATH cannot resolve is an error, not the bare
    /// name handed back for `execvp` to look up against the **ambient** PATH.
    ///
    /// Inverted from the pre-C-009 assertion this replaces, following
    /// `interface_shim_names_refuses_the_literal_ocx_name`: the old test
    /// asserted exactly the fallback C-009 deletes, so keeping it would have
    /// pinned the escape the contract exists to close.
    #[test]
    fn resolve_command_errors_when_a_bare_name_does_not_resolve() {
        let mut env = Env::clean();
        // Empty PATH — nothing can be found.
        env.set("PATH", "");
        #[cfg(windows)]
        env.set("PATHEXT", ".EXE;.CMD");
        assert!(
            matches!(
                env.resolve_command("__ocx_definitely_missing_binary__"),
                Err(CommandResolutionError::NotFound { .. })
            ),
            "an unresolvable bare name must be NotFound, never the bare name back"
        );
    }

    /// On Windows, `resolve_command_windows` must consult PATHEXT from this
    /// env, not from the running process. We simulate the scenario by placing
    /// a `.exe`-named file (the native launcher shim's on-disk shape) in a
    /// temp directory and pointing PATH at it with a PATHEXT that includes
    /// `.EXE`.
    #[cfg(windows)]
    #[test]
    fn resolve_command_windows_uses_child_env_pathext() {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("my_tool.exe");
        fs::write(&launcher, b"MZ").unwrap();

        let mut env = Env::clean();
        env.set("PATH", dir.path().to_str().unwrap());
        // PATHEXT lists .EXE — must resolve `my_tool` → `my_tool.exe`.
        env.set("PATHEXT", ".EXE;.CMD");

        let resolved = env
            .resolve_command("my_tool")
            .expect("the shim resolves through the child env PATHEXT");
        assert_eq!(
            resolved.file_name().unwrap().to_str().unwrap().to_ascii_lowercase(),
            "my_tool.exe",
            "resolve_command must find my_tool.exe via child env PATHEXT"
        );
    }

    /// Regression: a hardened or customized child PATHEXT may omit `.EXE`
    /// entirely (e.g. `PATHEXT=.BAT;.CMD`). The native launcher shim is always
    /// `<name>.exe`, so `resolve_command_windows` must still probe `.exe` even
    /// when the child PATHEXT does not list it — otherwise an installed `.exe`
    /// entrypoint silently becomes "command not found" (the cutover removed the
    /// PATHEXT inject/warn net that previously masked this).
    #[cfg(windows)]
    #[test]
    fn resolve_command_windows_probes_exe_when_pathext_omits_it() {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("my_tool.exe");
        fs::write(&launcher, b"MZ").unwrap();

        let mut env = Env::clean();
        env.set("PATH", dir.path().to_str().unwrap());
        // PATHEXT deliberately omits .EXE — `my_tool` must still resolve to
        // `my_tool.exe` via the always-probed `.exe` fallback.
        env.set("PATHEXT", ".BAT;.CMD");

        let resolved = env
            .resolve_command("my_tool")
            .expect("the shim resolves through the child env PATHEXT");
        assert_eq!(
            resolved.file_name().unwrap().to_str().unwrap().to_ascii_lowercase(),
            "my_tool.exe",
            "resolve_command must probe .exe even when child PATHEXT omits it"
        );
    }

    // ── C-008: `OCX_TOOLCHAIN_DIR` on the child env ────────────────────────

    /// C-008: a resolved `toolchain_dir` travels to a child ocx, because it
    /// moves `<home>/toolchain/links/<group>/<entry>` and is therefore
    /// resolution-affecting.
    #[test]
    fn apply_ocx_config_sets_ocx_toolchain_dir_when_some() {
        let mut cfg = view("/abs/ocx");
        cfg.toolchain_dir = Some(std::path::PathBuf::from("/home/u/toolchains"));

        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);

        assert_eq!(
            env.get(keys::OCX_TOOLCHAIN_DIR)
                .expect("OCX_TOOLCHAIN_DIR must be set when toolchain_dir is Some"),
            "/home/u/toolchains",
            "the child must resolve the same toolchain root as the parent"
        );
    }

    /// C-008: the `None` arm is **load-bearing**, not symmetry for its own
    /// sake — without the remove, a stale `OCX_TOOLCHAIN_DIR` exported into the
    /// parent shell survives into every child and beats the outer ocx's parsed
    /// state, so the two frames of one launch read two different trees.
    ///
    /// Modelled on `apply_ocx_config_removes_ocx_patch_snapshot_when_none`.
    #[test]
    fn apply_ocx_config_removes_ocx_toolchain_dir_when_none() {
        let cfg = view("/abs/ocx");
        assert!(cfg.toolchain_dir.is_none(), "the default view carries no root");

        let mut env = Env::clean();
        env.set(keys::OCX_TOOLCHAIN_DIR, "/stale/toolchains");
        env.apply_ocx_config(&cfg);

        assert!(
            env.get(keys::OCX_TOOLCHAIN_DIR).is_none(),
            "toolchain_dir=None must strip a stale inherited OCX_TOOLCHAIN_DIR"
        );
    }

    // ── C-009 / C-069 fixtures ─────────────────────────────────────────────

    /// An executable POSIX body carrying [`TRAMPOLINE_MARKER`] on its second
    /// line, exactly where C-028's generated trampoline puts it.
    #[cfg(unix)]
    fn write_trampoline(dir: &std::path::Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(
            &path,
            // The five-line C-028 shape WP-6 actually emits, including RUL-13's
            // single-quoted `__ocx_binary` assignment. A four-line fixture
            // spelling `${OCX_BINARY_PIN:-ocx}` would be a body this codebase
            // no longer produces, and every C-069 row here would then be
            // measured against a shape that cannot occur on disk.
            format!(
                "#!/bin/sh\n{TRAMPOLINE_MARKER}\nunset OCX_GLOBAL OCX_PROJECT\n\
                 __ocx_binary='/home/ocx/bin/ocx'\n\
                 exec \"${{OCX_BINARY_PIN:-$__ocx_binary}}\" --project '/p' exec -- \"${{0##*/}}\" \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    // ── C-009: the fallible `resolve_command` ──────────────────────────────

    /// C-009: a bare name a composed `PATH` directory provides resolves to that
    /// file's absolute path — the happy path the fallible signature keeps.
    #[cfg(unix)]
    #[test]
    fn resolve_command_resolves_a_bare_name_to_an_absolute_path_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let tool = write_binary(dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", dir.path());

        let resolved = env.resolve_command("tool").expect("a name on PATH resolves");
        assert!(
            same_file(&resolved, &tool),
            "resolve_command must answer with the file on PATH; got {}",
            resolved.display()
        );
        assert!(resolved.is_absolute(), "the answer is a path, never the bare name back");
    }

    /// C-009: the `NotFound` error **names the search space it walked**, so a
    /// user can see which directories were actually consulted.
    #[cfg(unix)]
    #[test]
    fn resolve_command_not_found_names_the_directories_it_searched() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();

        let mut env = Env::clean();
        env.set(
            "PATH",
            std::env::join_paths([first.path(), second.path()]).expect("tempdir paths carry no separator"),
        );

        let error = env
            .resolve_command("__ocx_wp2_absent_tool__")
            .expect_err("a bare name no directory provides is an error, never the bare name back");
        let CommandResolutionError::NotFound { command, searched } = &error else {
            panic!("expected NotFound, got {error:?}");
        };
        assert_eq!(command, "__ocx_wp2_absent_tool__");
        assert_eq!(
            searched,
            &vec![first.path().to_path_buf(), second.path().to_path_buf()],
            "the reported search space must be the one that was searched, in PATH order"
        );
        let message = error.to_string();
        assert!(
            message.contains(&first.path().display().to_string()),
            "the message must name the searched directories, got: {message}"
        );
    }

    /// C-009: a **path-bearing** command keeps today's behaviour — including
    /// the fall-through when the lookup misses. Only the bare-name arm changed.
    ///
    /// Every fixture is absolute or a name that cannot exist, so the answer
    /// does not depend on the process working directory.
    #[cfg(unix)]
    #[test]
    fn resolve_command_keeps_todays_behaviour_for_a_path_bearing_command() {
        let dir = tempfile::tempdir().unwrap();
        let tool = write_binary(dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", "");

        let resolved = env
            .resolve_command(tool.as_os_str())
            .expect("an absolute path names a file directly");
        assert!(same_file(&resolved, &tool), "an absolute path resolves to itself");

        // The lookup misses, and a path-bearing value is still handed to the OS
        // rather than refused: `execvp` performs no PATH search for it, so
        // there is no ambient escape to close.
        for bearing in ["./__ocx_wp2_missing__", "..", "/nonexistent/__ocx_wp2_missing__"] {
            let resolved = env
                .resolve_command(bearing)
                .unwrap_or_else(|error| panic!("'{bearing}' must fall through, got {error:?}"));
            assert_eq!(
                resolved,
                PathBuf::from(bearing),
                "a path-bearing miss is handed to the OS unchanged"
            );
        }
    }

    /// C-009: no `PATH` key at all is an empty search space, never an ambient
    /// fallback and never a panic.
    ///
    /// The probe is **`sh`**, not a name that exists nowhere: this test's whole
    /// subject is whether `which_in(cmd, None, …)` really means "no search
    /// space", and an impossible name is `NotFound` either way — green whether
    /// `None` is refused or silently falls back to the ambient `PATH`.
    /// `resolve_command_finds_sh_on_unix` is the sibling that proves `sh` is on
    /// that ambient `PATH`, so a fallback would answer `Ok` here.
    #[cfg(unix)]
    #[test]
    fn resolve_command_errors_when_the_env_carries_no_path_at_all() {
        let env = Env::clean();
        assert!(env.get("PATH").is_none(), "precondition: this env has no PATH");

        let error = env
            .resolve_command("sh")
            .expect_err("a PATH-less env resolves no bare name, not even one the host provides");
        let CommandResolutionError::NotFound { searched, .. } = &error else {
            panic!("expected NotFound, got {error:?}");
        };
        assert!(
            searched.is_empty(),
            "a PATH-less env must not invent a search space, got {searched:?}"
        );
    }

    /// D-V15 (CWE-426), Block B: a `PATH` of nothing but empty segments is
    /// **`None`**, never `Some("")`.
    ///
    /// Asserted on `Env::lookup_path` directly — the test module is this
    /// module, so the private helper is callable and the property needs no
    /// `set_current_dir`, which is process-global and racy under nextest.
    ///
    /// `Some("")` is not a harmless spelling of the same thing: `which` filters
    /// empty segments on Windows only, so on Unix `which_in` stats the bare
    /// candidate against the **process working directory** — `PATH=":"` (what
    /// `PATH="$A:$B"` renders to when both are unset) plus a hostile clone
    /// containing `./cmake` is a resolution answer out of the CWD. `None` is
    /// refused outright by `which_in` and has no ambient fallback.
    #[test]
    fn an_all_empty_path_is_no_search_space_at_all() {
        let separator = PATH_SEPARATOR;
        for hostile in ["".to_string(), separator.to_string(), format!("{separator}{separator}")] {
            let mut env = Env::clean();
            env.set("PATH", &hostile);
            assert_eq!(
                env.lookup_path(),
                None,
                "PATH={hostile:?} names no directory; Some(\"\") would probe the working directory"
            );
        }

        // Discriminating control: one real segment beside two empties still
        // yields a search space, so the guard above drops empties rather than
        // refusing every `PATH` that has one.
        let dir = tempfile::tempdir().unwrap();
        let mut env = Env::clean();
        env.set("PATH", format!("{separator}{}{separator}", dir.path().display()));
        assert_eq!(
            env.lookup_path().as_deref(),
            Some(dir.path().as_os_str()),
            "a real segment survives the filter that drops its empty neighbours"
        );
    }

    /// The lookup copy is re-joined with [`std::env::join_paths`], so a
    /// segment that legally contains the separator survives as **one**
    /// segment.
    ///
    /// On Windows `split_paths` reads `"` as a quote and can therefore emit a
    /// segment containing `;` — std's own example is
    /// `c:\foo;c:\som"e;di"r;c:\bar`, whose middle segment is `c:\some;dir`.
    /// The manual `push(PATH_SEPARATOR)` join this replaced tore that back
    /// into two directories, handing `which_in` a `c:\some` nobody put on
    /// `PATH`: a search-path widening in the function whose subject is
    /// narrowing the search space.
    ///
    /// **On Unix this assertion proves nothing about quoting, and is not
    /// claimed to.** `split_paths` splits on every `:` there, so no Unix
    /// segment can contain the separator and the manual join was
    /// byte-identical to `join_paths` for every input — there is no
    /// Linux-observable red for the tearing. What runs here is the count
    /// round-trip: it reds on the manual join only where quoting exists, and
    /// on this platform guards only that the filter drops the empties and
    /// nothing else. It is unconditional rather than `#[cfg(windows)]`
    /// because `task rust:check:windows-cfg` is scoped to `ocx_shim`, so a
    /// Windows-gated test here would not even be compiled by the gate.
    #[test]
    fn the_lookup_copy_round_trips_its_segments_one_for_one() {
        let separator = PATH_SEPARATOR;
        let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let expected: Vec<PathBuf> = dirs.iter().map(|dir| dir.path().to_path_buf()).collect();

        // Leading, interior and trailing empties, so the round trip is
        // asserted against the *filtered* set rather than against a value the
        // filter never had to touch.
        let hostile = format!(
            "{separator}{}{separator}{separator}{}{separator}{}{separator}",
            expected[0].display(),
            expected[1].display(),
            expected[2].display()
        );

        let mut env = Env::clean();
        env.set("PATH", &hostile);

        let looked_up = env.lookup_path().expect("three real segments are a search space");
        assert_eq!(
            std::env::split_paths(&looked_up).collect::<Vec<PathBuf>>(),
            expected,
            "PATH={hostile:?} names three directories; a join that tears or drops one \
             changes the search space"
        );
    }

    /// D-V15 (CWE-426): an **empty `PATH` segment** means the current directory
    /// on Unix, and it is dropped from the lookup copy before the search.
    ///
    /// Asserted through `NotFound`'s `searched`, which the resolver derives
    /// from the very value it hands to `which_in` — so an unfiltered `PATH`
    /// shows up here as an empty segment in the reported search space. The
    /// behavioural half (planting a decoy in the process working directory)
    /// is **not** written: it needs `std::env::set_current_dir`, which is
    /// process-global and racy across `cargo test`'s threads.
    #[cfg(unix)]
    #[test]
    fn resolve_command_drops_empty_path_segments_from_the_lookup_copy() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();

        let mut env = Env::clean();
        // Leading, interior and trailing empties — the three spellings a shell
        // produces from `PATH="$PATH:"`, `PATH=":$PATH"` and an unset variable
        // interpolated between two colons.
        let hostile = format!(":{}::{}:", first.path().display(), second.path().display());
        env.set("PATH", &hostile);

        let error = env
            .resolve_command("__ocx_wp2_absent_tool__")
            .expect_err("the name exists in neither directory");
        let CommandResolutionError::NotFound { searched, .. } = &error else {
            panic!("expected NotFound, got {error:?}");
        };
        assert_eq!(
            searched,
            &vec![first.path().to_path_buf(), second.path().to_path_buf()],
            "every empty segment must be dropped from the lookup copy"
        );
        assert_eq!(
            env.get("PATH").unwrap(),
            OsStr::new(hostile.as_str()),
            "this env's own PATH is a copy's source, never rewritten by a lookup"
        );

        // A PATH that is nothing but empty segments searches nothing at all.
        let mut only_empties = Env::clean();
        only_empties.set("PATH", ":");
        let error = only_empties
            .resolve_command("__ocx_wp2_absent_tool__")
            .expect_err("a PATH of empty segments resolves nothing");
        let CommandResolutionError::NotFound { searched, .. } = &error else {
            panic!("expected NotFound, got {error:?}");
        };
        assert!(
            searched.is_empty(),
            "an all-empty PATH searches nothing, got {searched:?}"
        );
    }

    /// C-009: three things on `PATH` that carry the right *name* but cannot be
    /// executed — a directory, a non-executable file, and a broken symlink —
    /// are each `NotFound`, never an answer.
    #[cfg(unix)]
    #[test]
    fn resolve_command_refuses_a_directory_a_non_executable_and_a_broken_symlink() {
        let as_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(as_directory.path().join("tool")).unwrap();

        let as_plain_file = tempfile::tempdir().unwrap();
        write_binary(as_plain_file.path(), "tool", 0o644);

        let as_broken_link = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            as_broken_link.path().join("__ocx_wp2_no_such_target__"),
            as_broken_link.path().join("tool"),
        )
        .unwrap();

        for (what, dir) in [
            ("a directory", as_directory.path()),
            ("a non-executable file", as_plain_file.path()),
            ("a broken symlink", as_broken_link.path()),
        ] {
            let mut env = Env::clean();
            env.set("PATH", dir);
            let resolved = env.resolve_command("tool");
            assert!(
                matches!(resolved, Err(CommandResolutionError::NotFound { .. })),
                "{what} named `tool` must not resolve, got {resolved:?}"
            );
        }
    }

    // ── C-010: `resolve_command_excluding` ─────────────────────────────────
    //
    // Every test below was written from C-010 against the stub, not from the
    // implementation: while `Env::lookup_path_excluding` was WP-2's one
    // `unimplemented!()` each of them was an honest panic-red, so none of them
    // can have been shaped to fit whatever the body turned out to do.

    /// C-010: the excluded directory is not consulted, and the answer comes
    /// from elsewhere on `PATH`.
    #[cfg(unix)]
    #[test]
    fn resolve_command_excluding_does_not_consult_the_excluded_directory() {
        let excluded_dir = tempfile::tempdir().unwrap();
        let real_dir = tempfile::tempdir().unwrap();
        write_trampoline(excluded_dir.path(), "cmake");
        let real = write_binary(real_dir.path(), "cmake", 0o755);

        let mut env = Env::clean();
        env.set(
            "PATH",
            std::env::join_paths([excluded_dir.path(), real_dir.path()]).unwrap(),
        );

        let resolved = env
            .resolve_command_excluding("cmake", &[excluded_dir.path().to_path_buf()])
            .expect("the name resolves in the surviving directory");
        assert!(
            same_file(&resolved, &real),
            "the excluded directory must not answer; got {}",
            resolved.display()
        );
    }

    /// C-010, **the load-bearing invariant**: `excluded` is removed from the
    /// *lookup copy* of `PATH` only. This env's own `PATH` must be
    /// byte-identical after the call, because a tool that spawns a sibling tool
    /// still resolves it through a trampoline.
    #[cfg(unix)]
    #[test]
    fn resolve_command_excluding_leaves_this_env_s_own_path_byte_identical() {
        let excluded_dir = tempfile::tempdir().unwrap();
        let real_dir = tempfile::tempdir().unwrap();
        write_binary(real_dir.path(), "cmake", 0o755);

        let mut env = Env::clean();
        let original = std::env::join_paths([excluded_dir.path(), real_dir.path()]).unwrap();
        env.set("PATH", &original);

        let _ = env.resolve_command_excluding("cmake", &[excluded_dir.path().to_path_buf()]);

        assert_eq!(
            env.get("PATH").expect("PATH survives the call"),
            original.as_os_str(),
            "the child's PATH keeps every segment its composition established"
        );
    }

    /// C-010: an empty exclusion set behaves exactly as
    /// [`Env::resolve_command`].
    #[cfg(unix)]
    #[test]
    fn resolve_command_excluding_with_an_empty_exclusion_behaves_as_resolve_command() {
        let dir = tempfile::tempdir().unwrap();
        let tool = write_binary(dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", dir.path());

        let resolved = env
            .resolve_command_excluding("tool", &[])
            .expect("an empty exclusion excludes nothing");
        assert!(same_file(&resolved, &tool), "got {}", resolved.display());
    }

    /// C-010: excluding a directory that is not on `PATH` is a no-op, never an
    /// error.
    #[cfg(unix)]
    #[test]
    fn resolve_command_excluding_a_directory_that_is_not_on_path_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        let tool = write_binary(dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", dir.path());

        let resolved = env
            .resolve_command_excluding("tool", &[unrelated.path().to_path_buf()])
            .expect("excluding an absent directory changes nothing");
        assert!(same_file(&resolved, &tool), "got {}", resolved.display());
    }

    /// C-010: excluding **every** segment is `NotFound` — never a panic and
    /// never an ambient fallback to the bare name.
    ///
    /// The probe is **`sh`** for the reason
    /// `resolve_command_errors_when_the_env_carries_no_path_at_all` states: an
    /// exhausted space reaches `which_in` as `None`, and only a name the
    /// ambient `PATH` really does provide can tell "`None` means no search
    /// space" from "`None` silently falls back".
    #[cfg(unix)]
    #[test]
    fn resolve_command_excluding_every_segment_is_not_found() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        write_binary(first.path(), "sh", 0o755);
        write_binary(second.path(), "sh", 0o755);

        let mut env = Env::clean();
        env.set("PATH", std::env::join_paths([first.path(), second.path()]).unwrap());
        assert!(
            env.resolve_command("sh").is_ok(),
            "precondition: `sh` resolves before the exclusion empties the space"
        );

        let resolved = env.resolve_command_excluding("sh", &[first.path().to_path_buf(), second.path().to_path_buf()]);
        let Err(CommandResolutionError::NotFound { searched, .. }) = &resolved else {
            panic!("an exhausted search space is NotFound, got {resolved:?}");
        };
        assert!(
            searched.is_empty(),
            "an exhausted space names nothing, and the host's own `sh` is not an answer; got {searched:?}"
        );
    }

    /// C-010 ∧ C-069, the belt the two-guard design rests on: a directory that
    /// **survives** the segment-exact exclusion answers with a trampoline, and
    /// the refusal catches it.
    ///
    /// `remove_segment`'s own doc is explicit that a segment naming the same
    /// directory by a different string survives untouched, and a trailing slash
    /// is the cheapest such spelling. Both arms are asserted so the fixture
    /// cannot pass for the wrong reason: the exact spelling empties the space
    /// (`NotFound`, guard one), the slashed spelling does not (
    /// `TrampolineRefused`, guard two).
    #[cfg(unix)]
    #[test]
    fn resolve_command_excluding_refuses_a_trampoline_a_trailing_slash_left_on_path() {
        let bin = tempfile::tempdir().unwrap();
        let trampoline = write_trampoline(bin.path(), "cmake");

        let mut env = Env::clean();
        env.set("PATH", bin.path());

        let exact = env.resolve_command_excluding("cmake", &[bin.path().to_path_buf()]);
        assert!(
            matches!(exact, Err(CommandResolutionError::NotFound { .. })),
            "control: the exact spelling really is excluded, got {exact:?}"
        );

        let slashed = PathBuf::from(format!("{}/", bin.path().display()));
        let error = env
            .resolve_command_excluding("cmake", &[slashed])
            .expect_err("a surviving trampoline directory must not answer");
        let CommandResolutionError::TrampolineRefused { path, .. } = &error else {
            panic!("the second guard must be the one that fired, got {error:?}");
        };
        assert!(
            same_file(path, &trampoline),
            "the refusal names the trampoline the exclusion could not strip; got {}",
            path.display()
        );
    }

    // ── C-069: the trampoline-identity refusal ─────────────────────────────

    /// C-069 / D-V1, **the input where only this guard defends**: a *foreign*
    /// home's trampoline on the lookup `PATH`.
    ///
    /// C-010's exclusion set can only ever name *this* invocation's own homes,
    /// so a second project's `<home>/toolchain/active/bin` on the same `PATH` is a
    /// directory the exclusion structurally cannot name. Under C-010 alone the
    /// invocation loops A → B → A forever, one full compose per hop, with no
    /// error and no depth counter.
    ///
    /// Kept a **unit** case on purpose: the acceptance form of this is an
    /// unbounded re-exec loop that hangs rather than fails, because
    /// `test/src/runner.py` passes no `timeout=`.
    #[cfg(unix)]
    #[test]
    fn resolve_command_refuses_a_foreign_home_trampoline_the_exclusion_cannot_name() {
        let foreign = tempfile::tempdir().unwrap();
        let bin = foreign.path().join("project-b/.ocx/toolchain/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let trampoline = write_trampoline(&bin, "cmake");

        let mut env = Env::clean();
        env.set("PATH", &bin);

        let error = env
            .resolve_command("cmake")
            .expect_err("a trampoline answer must be refused, never returned as Ok");
        let CommandResolutionError::TrampolineRefused { command, path } = &error else {
            panic!("expected TrampolineRefused — the guard identity is the whole point, got {error:?}");
        };
        assert_eq!(command, "cmake");
        assert!(
            same_file(path, &trampoline),
            "the refusal must name the trampoline it found; got {}",
            path.display()
        );
    }

    /// C-069 / finding S-1: a **symlink** on `PATH` pointing at a trampoline is
    /// refused, and the refusal judges the *link* path.
    ///
    /// `which` answers with the uncanonicalized path it walked, so the
    /// predicate is handed a symlink; `std::fs::metadata` and `File::open` both
    /// follow it, which is why it fires today. Swapping either for
    /// `symlink_metadata` — the natural "harden this against link tricks" edit
    /// — makes the answer "not a regular file" and silently disarms C-069 for
    /// exactly the aliased-directory class `remove_segment` cannot strip. That
    /// is the defect [pyenv#2696](https://github.com/pyenv/pyenv/issues/2696)
    /// shipped, and nothing else in this file pins it.
    #[cfg(unix)]
    #[test]
    fn a_symlink_on_path_pointing_at_a_trampoline_is_refused() {
        let real = tempfile::tempdir().unwrap();
        let alias = tempfile::tempdir().unwrap();
        let trampoline = write_trampoline(real.path(), "cmake");
        let link = alias.path().join("cmake");
        std::os::unix::fs::symlink(&trampoline, &link).unwrap();

        let mut env = Env::clean();
        env.set("PATH", alias.path());

        let error = env
            .resolve_command("cmake")
            .expect_err("a symlinked trampoline is still a trampoline");
        let CommandResolutionError::TrampolineRefused { path, .. } = &error else {
            panic!("expected TrampolineRefused, got {error:?}");
        };
        assert_eq!(
            path, &link,
            "which answers with the link path it walked, uncanonicalized"
        );
        assert!(
            std::fs::symlink_metadata(path)
                .expect("the refused answer exists")
                .file_type()
                .is_symlink(),
            "the refusal must have judged a symlink — otherwise this fixture proves nothing"
        );
    }

    /// C-069 / finding W-1: a body **longer than [`TRAMPOLINE_PROBE_BYTES`]**
    /// is still refused when the marker sits inside the probed head — the deep
    /// checkout case, where C-028's baked absolute project root pushes the body
    /// past 256 bytes.
    ///
    /// `#[cfg(unix)]` like every other body-content row here: the probe window
    /// is the POSIX `trampoline_signal` only. The Windows half is the `.exec`
    /// sidecar and has no content check at all (D-V15), so a shell body there
    /// is not a trampoline no matter where its marker sits — the row would
    /// assert something the platform does not claim.
    #[cfg(unix)]
    #[test]
    fn a_marker_inside_the_probe_window_of_an_over_long_body_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cmake");
        let body = format!("#!/bin/sh\n{TRAMPOLINE_MARKER}\n{}\n", "x".repeat(4 * 1024));
        assert!(
            body.len() > TRAMPOLINE_PROBE_BYTES,
            "precondition: the body must exceed the probe window"
        );
        std::fs::write(&path, body).unwrap();

        assert!(
            is_ocx_trampoline(&path),
            "a bounded prefix read must not turn an over-long body into 'not a trampoline'"
        );
    }

    /// C-069 / finding W-1, the other half: a marker sitting **beyond** the
    /// probe window is **not** refused.
    ///
    /// This is the row that pins the **bound**, so the fixture has to be one
    /// only the bound can answer: line *one* outruns
    /// [`TRAMPOLINE_PROBE_BYTES`], and the marker sits on line two — the exact
    /// position C-028 emits it at and the anchor accepts. The probed prefix
    /// therefore holds no `\n` at all, `nth(1)` is `None`, and the file is not
    /// a trampoline. Widen the constant and this test goes red, which is the
    /// whole point of it.
    ///
    /// A fixture that pushed the marker onto line *three* instead would be
    /// refused by the **anchor** at every bound — green with the window
    /// widened to a gigabyte, green with the bounded read deleted outright —
    /// and would pin nothing here.
    ///
    /// Recorded as the constraint WP-6 must honour when it emits the
    /// trampoline body: the marker has to land within the first
    /// [`TRAMPOLINE_PROBE_BYTES`] bytes, which is why C-028 puts it on line
    /// two, ahead of the line carrying the absolute project root. Together with
    /// the test above, this is what stops a future swap to a whole-file read
    /// silently disarming the guard for deep project roots, and a future
    /// re-ordering of the body silently disarming it for everyone.
    #[test]
    fn a_marker_beyond_the_probe_window_is_not_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cmake");
        let first_line = format!("#!/bin/sh {}", "#".repeat(300));
        assert!(
            first_line.len() > TRAMPOLINE_PROBE_BYTES,
            "precondition: line one must outrun the probe window, or this test measures the anchor, not the bound"
        );
        std::fs::write(&path, format!("{first_line}\n{TRAMPOLINE_MARKER}\n")).unwrap();

        assert!(
            !is_ocx_trampoline(&path),
            "the probe reads a bounded prefix; WP-6 must emit the marker inside it"
        );
    }

    /// E-21 / T-12, the positive control for the row above: the same anchor
    /// still refuses a real trampoline whose baked root is **deep**.
    ///
    /// Without this, `a_launcher_whose_baked_path_spells_the_marker_is_not_refused`
    /// would also pass against a predicate that answered `false` for everything.
    /// The root exceeds 200 characters so the body runs past the probe window,
    /// which is the case the head bound has to keep answering.
    #[cfg(unix)]
    #[test]
    fn a_trampoline_with_a_root_past_the_probe_window_is_still_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cmake");
        let deep_root = format!("/w/{}", "d".repeat(240));
        assert!(deep_root.len() > 200, "precondition: the root must be deep");
        let body = format!(
            "#!/bin/sh\n{TRAMPOLINE_MARKER}\nunset OCX_GLOBAL OCX_PROJECT\n\
             __ocx_binary='ocx'\n\
             exec \"${{OCX_BINARY_PIN:-$__ocx_binary}}\" --project '{deep_root}' exec -- \"${{0##*/}}\" \"$@\"\n"
        );
        assert!(
            body.len() > TRAMPOLINE_PROBE_BYTES,
            "precondition: the body must exceed the probe window"
        );
        std::fs::write(&path, body).unwrap();

        assert!(
            is_ocx_trampoline(&path),
            "the marker is line two regardless of how deep the baked root is — the \
             anchor sits ahead of every interpolated value"
        );
    }

    /// The anchor is the SECOND line specifically, not "an early line". A
    /// marker on line one or line three is not a trampoline signal, so a
    /// future re-ordering of the C-028 body disarms the guard loudly.
    ///
    /// `#[cfg(unix)]`: the line-two anchor is the POSIX signal. On Windows the
    /// signal is the `.exec` sidecar and no body is read (D-V15), so the three
    /// negatives would pass vacuously and the control — which is what makes
    /// them mean anything — cannot.
    #[cfg(unix)]
    #[test]
    fn only_the_second_line_carries_the_marker_signal() {
        let dir = tempfile::tempdir().unwrap();
        for (label, body) in [
            ("line one", format!("{TRAMPOLINE_MARKER}\n#!/bin/sh\nexec ocx\n")),
            (
                "line three",
                format!("#!/bin/sh\nunset OCX_GLOBAL\n{TRAMPOLINE_MARKER}\n"),
            ),
            (
                "line two, but only as a prefix of it",
                format!("#!/bin/sh\n{TRAMPOLINE_MARKER} and more\nexec ocx\n"),
            ),
        ] {
            let path = dir.path().join(label.replace(' ', "_"));
            std::fs::write(&path, &body).unwrap();
            assert!(
                !is_ocx_trampoline(&path),
                "{label}: the signal is line two matched WHOLE, nothing looser"
            );
        }

        // The control: the exact shape IS refused, so the rows above are not
        // passing because the predicate answers false unconditionally.
        let path = dir.path().join("real");
        std::fs::write(&path, format!("#!/bin/sh\n{TRAMPOLINE_MARKER}\nexec ocx\n")).unwrap();
        assert!(
            is_ocx_trampoline(&path),
            "control: the marker as the whole of line two is the signal"
        );
    }

    /// C-069: a file that merely *contains* the marker somewhere in its body is
    /// not refused. An unbounded `contains()` would refuse any ordinary tool
    /// that happens to embed the string in its data — and would allocate a
    /// 200 MB binary on every resolution.
    #[test]
    fn a_file_that_merely_contains_the_marker_later_in_its_body_is_not_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grep");
        let mut body: Vec<u8> = b"\x7fELF".to_vec();
        body.resize(8 * 1024, 0);
        body.extend_from_slice(TRAMPOLINE_MARKER.as_bytes());
        std::fs::write(&path, body).unwrap();

        assert!(
            !is_ocx_trampoline(&path),
            "an ordinary binary embedding the string must keep resolving"
        );
    }

    /// C-069: a 0-byte file is not a trampoline, and the probe does not panic
    /// on it.
    #[test]
    fn a_zero_byte_file_is_not_a_trampoline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, b"").unwrap();

        assert!(!is_ocx_trampoline(&path), "an empty file carries no marker");
    }

    /// C-069: an unreadable file is **not** refused — the predicate fails
    /// *open*, the deliberate inverse of `ocx launcher shim`'s `resolves_inside`.
    ///
    /// This predicate runs on every resolution including `/bin/sh`, so a
    /// fail-closed I/O arm would turn a transient `EACCES` on an unrelated
    /// binary into a refusal to run an ordinary command.
    ///
    /// The fixture's content **is** a trampoline body, so the assertion can
    /// only pass because the read failed. When the read does not fail — a
    /// privileged uid bypasses the mode bits — the test's premise does not
    /// hold, and it says so having *observed* the successful open rather than
    /// having assumed a uid.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_not_a_trampoline() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = write_trampoline(dir.path(), "cmake");
        assert!(
            is_ocx_trampoline(&path),
            "precondition: while readable, this body is refused"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        if std::fs::File::open(&path).is_ok() {
            // Observed, not assumed: this uid bypasses the mode bits
            // (root / CAP_DAC_OVERRIDE), so "unreadable" is not reproducible
            // here and there is nothing for the fail-open arm to answer.
            return;
        }

        assert!(
            !is_ocx_trampoline(&path),
            "an unreadable file must fail open, never refuse an ordinary command"
        );
    }

    /// C-069 / D-V15(b): a **FIFO** on the resolution path must not block.
    ///
    /// `std::fs::metadata` decides `is_file()` before anything is opened, and
    /// that ordering is the whole guard: opening a FIFO for reading blocks
    /// until a writer appears — forever, on the path that runs before every
    /// `exec`. No writer is ever opened here, so a regression that opens first
    /// hangs; the test is deterministic without a timeout because the two
    /// outcomes are "returns false" and "never returns", and only the first can
    /// report green.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_not_a_trampoline_and_is_never_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cmake");
        ocx_test_support::fifo::mkfifo(&path);
        assert!(
            std::fs::metadata(&path).is_ok_and(|m| !m.is_file()),
            "precondition: the fixture really is a FIFO, not a regular file"
        );

        assert!(
            !is_ocx_trampoline(&path),
            "a non-regular file is 'not a trampoline' without ever being opened"
        );
    }

    /// C-069 / D-V15(a), Windows: the sibling `.shim` sidecar of a lazy shim
    /// slot is **not** a trampoline signal.
    ///
    /// Every trampoline `.exe` *and* every shim-slot `.exe` is a hardlink of
    /// the one committed blob, so content cannot discriminate them; `.exec` is
    /// the only signal, and `.shim` must keep resolving.
    #[cfg(windows)]
    #[test]
    fn a_sibling_shim_sidecar_is_not_a_trampoline() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("cmake.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        std::fs::write(dir.path().join("cmake.shim"), b"C:\\pkg\n").unwrap();

        assert!(
            !is_ocx_trampoline(&exe),
            "a lazy shim slot must keep resolving; only `.exec` marks a trampoline"
        );
    }

    /// C-069, Windows: a sibling `.exec` sidecar **is** the trampoline signal.
    #[cfg(windows)]
    #[test]
    fn a_sibling_exec_sidecar_is_a_trampoline() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("cmake.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        std::fs::write(dir.path().join("cmake.exec"), b"C:\\project\n").unwrap();

        assert!(
            is_ocx_trampoline(&exe),
            "the `.exec` sidecar is the Windows trampoline signal"
        );
    }

    /// C-069 / D-V15(a), Windows: a resolved path whose **own** extension is
    /// `.exec` is refused.
    ///
    /// `which` treats any file carrying an extension as executable on Windows,
    /// so a `PATHEXT` containing `.EXEC` makes the sidecar *text file* itself a
    /// resolution answer.
    #[cfg(windows)]
    #[test]
    fn a_resolved_path_whose_own_extension_is_exec_is_a_trampoline() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("cmake.exec");
        std::fs::write(&sidecar, b"C:\\project\n").unwrap();

        assert!(
            is_ocx_trampoline(&sidecar),
            "a `.exec` file is never a command answer, whatever PATHEXT says"
        );
        assert!(
            is_ocx_trampoline(&dir.path().join("CMAKE.EXEC")),
            "the extension comparison folds case, like every other reserved-name check"
        );
    }
}
