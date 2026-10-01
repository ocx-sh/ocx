// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::records::RecordsOptions;
use ocx_util::env::{flag, string, var};

/// Canonical names for `OCX_*` environment variables read or written by ocx.
pub mod keys {
    /// Absolute path to the running `ocx`, set on every spawn so a child ocx runs the
    /// same binary, not whatever `$PATH` resolves.
    pub const OCX_BINARY_PIN: &str = "OCX_BINARY_PIN";
    /// The OCX data root — `$OCX_HOME`, else `~/.ocx` ([`crate::home::default_ocx_root`]).
    ///
    /// Forwarded set-always as the resolved root, or an `ocx exec --clean` child (no `HOME`)
    /// resolves another root and misses the package the parent just materialized.
    pub const OCX_HOME: &str = "OCX_HOME";
    /// Boolean — disables network access when truthy. Mirrors `--offline`.
    pub const OCX_OFFLINE: &str = "OCX_OFFLINE";
    /// Boolean — a tag-only reference missing from the local index errors instead of
    /// being fetched; digest-pinned content still fetches. Mirrors `--frozen`.
    pub const OCX_FROZEN: &str = "OCX_FROZEN";
    /// Boolean — uses the remote index by default when truthy. Mirrors `--remote`.
    pub const OCX_REMOTE: &str = "OCX_REMOTE";
    /// Path to an explicit configuration file. Mirrors `--config`.
    pub const OCX_CONFIG: &str = "OCX_CONFIG";
    /// Boolean — skip the discovered config-tier chain; explicit `--config` / [`OCX_CONFIG`]
    /// paths still load and a system-scope lock survives. Forwarded to child ocx.
    pub const OCX_NO_CONFIG: &str = "OCX_NO_CONFIG";
    /// Path to an explicit project `ocx.toml`. Mirrors `--project`.
    pub const OCX_PROJECT: &str = "OCX_PROJECT";
    /// Boolean — skip the CWD walk and [`OCX_PROJECT`]; explicit `--project` paths still load.
    /// Forwarded only when this invocation resolved no project.
    pub const OCX_NO_PROJECT: &str = "OCX_NO_PROJECT";
    /// Boolean — commands that stamp shell-activation consent as a side effect write none;
    /// `--consent` / `--no-consent` outrank it and `ocx shell allow` ignores it.
    ///
    /// Forwarded, else a script run by `ocx exec` that calls `ocx pull` stamps after all.
    pub const OCX_NO_CONSENT: &str = "OCX_NO_CONSENT";
    /// Boolean — select the global toolchain (`$OCX_HOME/ocx.toml`). Mirrors `--global`.
    pub const OCX_GLOBAL: &str = "OCX_GLOBAL";
    /// Path to the local index directory. Mirrors `--index`.
    pub const OCX_INDEX: &str = "OCX_INDEX";
    /// The registry a bare identifier resolves under; outranks `[registry] default`, else
    /// `ocx.sh`. Empty is unset. Forwarded so every frame of a launch chain agrees.
    pub const OCX_DEFAULT_REGISTRY: &str = "OCX_DEFAULT_REGISTRY";
    /// Comma-separated `host[:port]` authorities that may be dialled over plain HTTP.
    ///
    /// Forwarded, or a child with a narrower set cannot reach a registry the parent just pulled from.
    pub const OCX_INSECURE_REGISTRIES: &str = "OCX_INSECURE_REGISTRIES";
    /// Boolean — allow a tag resolving to a yanked index entry. Forwarded to child ocx.
    pub const OCX_ALLOW_YANKED: &str = "OCX_ALLOW_YANKED";
    /// Path to the active patch snapshot file, whose pinned digests win over live tag lookups.
    /// Forwarded to child ocx.
    pub const OCX_PATCH_SNAPSHOT: &str = "OCX_PATCH_SNAPSHOT";
    /// JSON object mapping an upstream host to a mirror string or `{"registry"?, "index"?}`,
    /// the same union `[mirrors."<host>"]` accepts. Forwarded to child ocx.
    pub const OCX_MIRRORS: &str = "OCX_MIRRORS";
    /// JSON object encoding the resolved `[patches]` config; forwarded only when configured.
    pub const OCX_PATCHES: &str = "OCX_PATCHES";
    /// JSON envelope of the resolved project `[env]` entries plus `ocx exec --env` overrides,
    /// or a launcher re-entry silently reverts them.
    ///
    /// Decode fails closed on the whole envelope (reserved key, unknown `kind`): a forged entry
    /// can set a value, so [`OCX_PATCHES`]'s leniency must not be copied.
    pub const OCX_ENV: &str = "OCX_ENV";
    /// JSON map from a content digest to the `registry/repository[:tag]` names one composition
    /// resolved it under; read by `ocx launcher exec` to match targeted patch rules.
    ///
    /// Written by ocx only while a `[patches]` tier is in effect; reserved, so `[env]` and
    /// `--env` cannot set it. [`crate::env::Env::apply_ocx_config`] leaves it alone, or a launcher nested
    /// under another launcher loses the outer composition's names.
    pub const OCX_LAUNCH_IDENTITIES: &str = "OCX_LAUNCH_IDENTITIES";
    /// Boolean — `ocx self setup` modifies no shell profile. Mirrors `--no-modify-path`.
    pub const OCX_NO_MODIFY_PATH: &str = "OCX_NO_MODIFY_PATH";
    /// OCI reference overriding `[managed].source` for this invocation only; empty is unset,
    /// [`OCX_NO_CONFIG`] suppresses it. Forwarded to child ocx.
    pub const OCX_MANAGED_CONFIG: &str = "OCX_MANAGED_CONFIG";
    /// Boolean — kill switch for the managed-config background refresh; `ocx config update` still works.
    ///
    /// Forwarded, or a child's refresh replaces the managed tier with config the parent never saw.
    pub const OCX_NO_CONFIG_REFRESH: &str = "OCX_NO_CONFIG_REFRESH";
    /// Directory execution records are written to; absent or empty turns recording off.
    /// Forwarded so every frame of a launch chain records into the outermost sink.
    pub const OCX_RECORDS_DIR: &str = "OCX_RECORDS_DIR";
    /// Filename template for each execution record; forwarded alongside [`OCX_RECORDS_DIR`].
    ///
    /// No `OCX_RECORDS_REQUIRED` exists: fail-closed recording is config-file-only operator policy.
    pub const OCX_RECORDS_NAME: &str = "OCX_RECORDS_NAME";
    /// `LazyMode` wire value (`never` / `always`), the weakest tier of the `lazy-mode` ladder.
    ///
    /// Not forwarded: it changes when content materializes, never which digest resolves.
    pub const OCX_LAZY_MODE: &str = "OCX_LAZY_MODE";
    /// `LazyReport` wire value (`silent` / `progress`); not forwarded, like [`OCX_LAZY_MODE`].
    pub const OCX_LAZY_REPORT: &str = "OCX_LAZY_REPORT";
    /// `ActivateMode` wire value (`env` / `bin` / `none`), the weakest tier of the `activate`
    /// ladder (`adr_toolchain_activation.md`). Not forwarded, like [`OCX_LAZY_MODE`].
    pub const OCX_TOOLCHAIN_ACTIVATE: &str = "OCX_TOOLCHAIN_ACTIVATE";
    /// Boolean — composed paths pin to digest roots; the weakest tier of the `pinned` ladder.
    ///
    /// Never read via [`ocx_util::env::flag`], which collapses unset and `false` and hides an explicit `false`.
    pub const OCX_TOOLCHAIN_PINNED: &str = "OCX_TOOLCHAIN_PINNED";
    /// Root under which project toolchain homes render — the env tier of `toolchain_dir`.
    ///
    /// Forwarded set-or-remove; the containment refusals apply to it like to every tier.
    pub const OCX_TOOLCHAIN_DIR: &str = "OCX_TOOLCHAIN_DIR";
    /// Boolean — skip the policy-gated auto-verify on install/pull; `--no-verify` wins. Forwarded to child ocx.
    pub const OCX_NO_VERIFY: &str = "OCX_NO_VERIFY";

    /// Extra CA roots — a path or inline PEM (contains `-----BEGIN`); empty is unset.
    ///
    /// Not forwarded: an inline PEM can exceed Windows' 32,767-character per-variable limit.
    pub const OCX_EXTRA_CA_CERTS: &str = "OCX_EXTRA_CA_CERTS";

    /// OIDC bearer token for keyless signing, below `--identity-token-file`/`-stdin`. In [`CREDENTIAL_KEYS`].
    pub const OCX_IDENTITY_TOKEN: &str = "OCX_IDENTITY_TOKEN";

    /// Password for an encrypted signing key; never a flag, since `argv` is visible host-wide.
    /// In [`CREDENTIAL_KEYS`].
    pub const OCX_KEY_PASSWORD: &str = "OCX_KEY_PASSWORD";

    /// The signing key PEM itself, for `--key env://OCX_SIGNING_KEY`. In [`CREDENTIAL_KEYS`];
    /// any other `env://` name works but is not scrubbed from plugins.
    pub const OCX_SIGNING_KEY: &str = "OCX_SIGNING_KEY";

    /// The API half of the forge credential pair (a forge access token).
    ///
    /// Not in [`CREDENTIAL_KEYS`], so a plugin-dispatched `ocx-mirror` inherits it to announce.
    /// `ocx_cli::command::package_announce` declares a private copy, so a rename here leaves it reading the old name.
    pub const OCX_ANNOUNCE_TOKEN: &str = "OCX_ANNOUNCE_TOKEN";

    /// The push-only forge credential (the secret half of `git push`'s Basic pair). In [`CREDENTIAL_KEYS`].
    pub const OCX_ANNOUNCE_GIT_TOKEN: &str = "OCX_ANNOUNCE_GIT_TOKEN";

    /// Every env var carrying a bearer credential, stripped from child envs by
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config).
    ///
    /// `Command::envs` only adds, so any other spawn site inheriting the ambient env must remove
    /// each entry itself, or the credential leaks to the child.
    /// The `OCX_AUTH_<slug>_TOKEN` pattern cannot be listed here and is never scrubbed.
    pub const CREDENTIAL_KEYS: &[&str] = &[
        OCX_IDENTITY_TOKEN,
        OCX_KEY_PASSWORD,
        OCX_SIGNING_KEY,
        OCX_ANNOUNCE_GIT_TOKEN,
    ];
}

/// Resolution-affecting policy snapshot that `Env::apply_ocx_config` writes onto a child env.
///
/// Presentation flags (`--log-level`, `--format`, `--color`) stay out, or a generated launcher
/// stops being opaque to the surrounding tool.
#[derive(Debug, Clone)]
pub struct OcxConfigView {
    /// Absolute path to the running ocx executable.
    pub self_exe: PathBuf,
    pub offline: bool,
    pub remote: bool,
    /// Tag resolution may only consult the local index. Forwarded as [`keys::OCX_FROZEN`].
    pub frozen: bool,
    pub config: Option<PathBuf>,
    pub project: Option<PathBuf>,
    /// The global toolchain is the in-effect project file. Forwarded as [`keys::OCX_GLOBAL`];
    /// only `apply_ocx_config_sets_ocx_global_when_set` observes it, no acceptance test can.
    pub global: bool,
    /// This invocation's `--no-consent`, forwarded as [`keys::OCX_NO_CONSENT`].
    ///
    /// Suppression-only: `--consent` never clears an ambient `OCX_NO_CONSENT`, since it covers
    /// only this invocation's project, not everything the child touches.
    pub no_consent: bool,
    pub index: Option<PathBuf>,
    /// The resolved `toolchain_dir`; `None` is the in-project default.
    ///
    /// Every producer writes `None` today, but its remove arm strips a stale inherited export,
    /// so the field is not dead.
    pub toolchain_dir: Option<PathBuf>,
    /// Per-host mirrors from [`crate::mirror::ResolvedMirrors::merged`], forwarded as [`keys::OCX_MIRRORS`].
    pub mirrors: Vec<(String, crate::mirror::MirrorConfig)>,
    /// Resolved `[patches]` config, forwarded as [`keys::OCX_PATCHES`].
    pub patches: Option<crate::patch::ResolvedPatchConfig>,
    /// Active patch snapshot file, forwarded as [`keys::OCX_PATCH_SNAPSHOT`].
    pub patch_snapshot: Option<PathBuf>,
    /// The effective managed-config source (flag > env > seed), forwarded as [`keys::OCX_MANAGED_CONFIG`].
    pub managed_config_source: Option<String>,
    /// Ambient `OCX_NO_VERIFY`, forwarded as [`keys::OCX_NO_VERIFY`]; the `--no-verify` flag is not.
    pub no_verify: bool,
    /// The discovered config chain was skipped, forwarded as [`keys::OCX_NO_CONFIG`].
    ///
    /// A field, not an ambient read at the forwarding site, which could disagree with what the loader used.
    pub no_config: bool,
    /// The resolved `[records]` sink; only `dir` and `name` are forwarded, the rest a child re-reads.
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

/// Environment variable key, uppercased on Windows (case-insensitive there); an `OsString`
/// so a non-UTF-8 Unix name survives.
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

/// Uppercase a single UTF-16 code unit in the ASCII range, as the kernel folds env names.
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
    /// The `PATH` directories composed packages contributed, in `PATH` order, never ambient ones.
    ///
    /// Never emitted by [`Env::iter`] / `IntoIterator`: it is bookkeeping and must not reach a child.
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

    /// Prepends `value` to the path-style variable `key`, moving an existing occurrence to the front.
    pub fn add_path(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        let key = EnvKey::new(key);
        let value = value.into();
        let new_value = match self.vars.get(&key) {
            Some(existing) => ocx_util::path::move_to_front(existing, &value),
            None => value,
        };
        self.vars.insert(key, new_value);
    }

    /// Appends `value` to the list-style variable `key`, moving an existing occurrence to the back.
    ///
    /// An empty `value` is a no-op even on an absent key; the ambient value is read lossily.
    pub fn add_list(&mut self, key: impl Into<OsString>, value: &str, separator: &str) {
        if value.is_empty() {
            return;
        }
        let key = EnvKey::new(key);
        let existing = self.vars.get(&key).map(|v| v.to_string_lossy().into_owned());
        let folded = ocx_util::list::append_unique(existing.as_deref().unwrap_or_default(), value, separator);
        self.vars.insert(key, OsString::from(folded));
    }

    /// Borrowing iterator over `(key, value)` pairs, in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars.iter().map(|(k, v)| (k.0.as_os_str(), v.as_os_str()))
    }

    /// Removes the named key; no-op if absent.
    pub fn remove(&mut self, key: &str) {
        self.vars.remove(&EnvKey::new(key));
    }

    /// Records `dir` as a `PATH` directory a composed package contributed.
    ///
    /// Does not set `PATH`: called alone it records a directory the child cannot see.
    pub fn note_package_path(&mut self, dir: &OsStr) {
        self.package_path = ocx_util::path::move_to_front(&self.package_path, dir);
    }

    /// Writes resolution-affecting OCX configuration onto this env so a child ocx sees the
    /// parent's policy, and strips [`keys::CREDENTIAL_KEYS`]. Idempotent.
    ///
    /// Every key but the binary pin and `OCX_HOME` is set-or-remove, so a stale parent-shell
    /// export cannot beat the parsed state.
    pub fn apply_ocx_config(&mut self, cfg: &OcxConfigView) {
        for credential in keys::CREDENTIAL_KEYS {
            self.remove(credential);
        }
        self.set(keys::OCX_BINARY_PIN, cfg.self_exe.as_os_str());
        if let Some(root) = crate::home::default_ocx_root() {
            // Absolutized here: a child may run from another CWD and read another directory.
            let absolute = std::path::absolute(&root).unwrap_or_else(|error| {
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
        // Without the remove, a stale exported `OCX_TOOLCHAIN_DIR` makes two frames of one launch use two trees.
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
        // Each set-or-remove, or a defaulted template picks up the parent shell's pattern.
        match &cfg.records.dir {
            Some(path) => self.set(keys::OCX_RECORDS_DIR, path.as_os_str()),
            None => self.remove(keys::OCX_RECORDS_DIR),
        }
        match &cfg.records.name {
            Some(template) => self.set(keys::OCX_RECORDS_NAME, template.as_str()),
            None => self.remove(keys::OCX_RECORDS_NAME),
        }
        // Always cleared, or a stale shell value reaches the child; `apply_child_env` writes the real one after.
        self.remove(keys::OCX_ENV);
        // Without this a hermetic parent's child re-reads the full config chain and resolves differently.
        if cfg.no_config {
            self.set(keys::OCX_NO_CONFIG, "1");
        } else {
            self.remove(keys::OCX_NO_CONFIG);
        }
        // Ambient-only keys: without them a `--clean` child resolves against defaults the parent overrode.
        if flag(keys::OCX_ALLOW_YANKED, false) {
            self.set(keys::OCX_ALLOW_YANKED, "1");
        } else {
            self.remove(keys::OCX_ALLOW_YANKED);
        }
        // Gated on the view: `explicit_project` reads the prune before `OCX_PROJECT`, so forwarding
        // both makes the child discard the project.
        if cfg.project.is_none() && flag(keys::OCX_NO_PROJECT, false) {
            self.set(keys::OCX_NO_PROJECT, "1");
        } else {
            self.remove(keys::OCX_NO_PROJECT);
        }
        if flag(keys::OCX_NO_CONFIG_REFRESH, false) {
            self.set(keys::OCX_NO_CONFIG_REFRESH, "1");
        } else {
            self.remove(keys::OCX_NO_CONFIG_REFRESH);
        }
        // An empty value must not travel, or the child reads it as "no default registry at all".
        match var(keys::OCX_DEFAULT_REGISTRY).filter(|value| !value.is_empty()) {
            Some(registry) => self.set(keys::OCX_DEFAULT_REGISTRY, registry),
            None => self.remove(keys::OCX_DEFAULT_REGISTRY),
        }
        match var(keys::OCX_INSECURE_REGISTRIES).filter(|value| !value.is_empty()) {
            Some(authorities) => self.set(keys::OCX_INSECURE_REGISTRIES, authorities),
            None => self.remove(keys::OCX_INSECURE_REGISTRIES),
        }
        // `cfg.no_consent` is argv's only channel, else `--no-consent` fails open; the ambient read survives `--clean`.
        if cfg.no_consent || flag(keys::OCX_NO_CONSENT, false) {
            self.set(keys::OCX_NO_CONSENT, "1");
        } else {
            self.remove(keys::OCX_NO_CONSENT);
        }
    }

    /// Resolve a command name through this env's `PATH` and, on Windows, its own `PATHEXT`.
    ///
    /// A path-bearing `command` falls back to the bare value on a miss; a bare name never does,
    /// or `execvp` repeats the lookup on the ambient `PATH`. Never logs a miss, or callers print it twice.
    ///
    /// # Errors
    ///
    /// [`CommandResolutionError::NotFound`] for a bare name no `PATH` directory provides;
    /// [`CommandResolutionError::TrampolineRefused`] when the answer is an ocx launcher trampoline.
    pub fn resolve_command(&self, command: impl AsRef<OsStr>) -> Result<PathBuf, CommandResolutionError> {
        self.resolve_command_in(command.as_ref(), self.lookup_path())
    }

    /// The one lookup both public resolvers route through, over an already-prepared `PATH` copy.
    fn resolve_command_in(&self, command: &OsStr, path: Option<OsString>) -> Result<PathBuf, CommandResolutionError> {
        let cwd = std::env::current_dir().unwrap_or_else(|e| {
            log::debug!("Could not determine current directory: {}", e);
            PathBuf::new()
        });

        // `which_in` reads the process PATHEXT, so Windows probes the child's itself.
        // One binding, not two `#[cfg]` returns, or one platform can ship an answer the trampoline gate never saw.
        #[cfg(windows)]
        let found = self.resolve_command_windows(command, path.as_deref(), &cwd);
        #[cfg(not(windows))]
        let found = which::which_in(command, path.as_deref(), &cwd).ok();

        if let Some(found) = found {
            // A trampoline answer re-enters `ocx exec`; two homes on one `PATH` loop A → B → A forever.
            if is_ocx_trampoline(&found) {
                return Err(CommandResolutionError::TrampolineRefused {
                    command: command.to_string_lossy().into_owned(),
                    path: found,
                });
            }
            return Ok(found);
        }

        // Safe outside the gate: `execvp` does no `PATH` search for a path, and a real trampoline resolved above.
        if command_is_path(command) {
            // `{:?}`: the command is untrusted, and a raw newline forges a log line (CWE-117).
            log::warn!(
                "Could not resolve {:?} via PATH, falling back to OS lookup.",
                command.to_string_lossy()
            );
            return Ok(PathBuf::from(command));
        }

        Err(CommandResolutionError::NotFound {
            command: command.to_string_lossy().into_owned(),
            // Re-split the value `which_in` searched, or the report can disagree with the search.
            searched: path
                .as_deref()
                .map(|value| std::env::split_paths(value).collect())
                .unwrap_or_default(),
        })
    }

    /// A copy of `PATH` with empty segments dropped, since one means the CWD on Unix (CWE-426).
    ///
    /// Nothing to search is `None`, never `Some("")`, which Unix `which_in` stats against the CWD.
    /// Re-joined with [`std::env::join_paths`], since a manual join tears a quoted Windows segment.
    fn lookup_path(&self) -> Option<OsString> {
        let path = self.get("PATH")?;
        let joined =
            std::env::join_paths(std::env::split_paths(path).filter(|segment| !segment.as_os_str().is_empty())).ok()?;
        if joined.is_empty() {
            return None;
        }
        Some(joined)
    }

    /// [`Self::resolve_command`] with `excluded` removed (`adr_toolchain_activation.md`).
    ///
    /// Only the lookup copy loses `excluded`, or no descendant reaches a sibling tool through a
    /// trampoline; `ocx launcher shim` prunes its child's `PATH` instead — do not unify the two.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve_command`].
    pub fn resolve_command_excluding(
        &self,
        command: impl AsRef<OsStr>,
        excluded: &[PathBuf],
    ) -> Result<PathBuf, CommandResolutionError> {
        self.resolve_command_in(command.as_ref(), self.lookup_path_excluding(excluded))
    }

    /// [`Self::lookup_path`] with every directory in `excluded` removed (segment-exact).
    fn lookup_path_excluding(&self, excluded: &[PathBuf]) -> Option<OsString> {
        let path = self.lookup_path()?;
        if excluded.is_empty() {
            return Some(path);
        }

        let pruned = excluded.iter().fold(path, |value, dir| {
            ocx_util::path::remove_segment(&value, dir.as_os_str())
        });

        if pruned.is_empty() {
            // `Some("")` would search the CWD (CWE-426).
            return None;
        }

        Some(pruned)
    }

    /// Windows-only: probe `path` with each extension from this env's PATHEXT, in order.
    ///
    /// `path` is the caller's lookup copy; reading the stored `PATH` would bypass its filtering.
    #[cfg(windows)]
    fn resolve_command_windows(&self, command: &OsStr, path: Option<&OsStr>, cwd: &std::path::Path) -> Option<PathBuf> {
        for ext in self.pathext() {
            let mut candidate = command.to_os_string();
            candidate.push(&ext);
            if let Ok(found) = which::which_in(&candidate, path, cwd) {
                return Some(found);
            }
        }

        which::which_in(command, path, cwd).ok()
    }

    /// Windows-only: this env's PATHEXT (else the default), in order.
    ///
    /// `.EXE` is appended when absent, or a hardened `PATHEXT=.BAT;.CMD` hides every `<name>.exe` shim.
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

    /// Resolve a command for `ocx * test` / launcher re-entry, package-contributed `PATH` first,
    /// or a packaged `tool` lacking the exec bit is tested against the host's `tool` and passes.
    ///
    /// Then [`Self::resolve_command`] with a warning; a total miss falls through to the bare name.
    ///
    /// # Errors
    ///
    /// [`CommandResolutionError::NotExecutable`] (Unix) when the package ships the name but it is
    /// not executable; [`CommandResolutionError::TrampolineRefused`] from the host lookup, never
    /// mapped to the bare name, or `execvp` finds the same trampoline and loops.
    pub fn resolve_test_command(&self, command: impl AsRef<OsStr>) -> Result<PathBuf, CommandResolutionError> {
        let command = command.as_ref();
        if command_is_path(command) {
            return self.resolve_command(command);
        }

        // The bare name is a candidate only with a PATHEXT extension; an extensionless match
        // would trade a working host fallback for a doomed exec.
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

        // Remembered across the scan so an executable match in a later directory still wins.
        let mut blocked: Option<(PathBuf, u32)> = None;

        for dir in std::env::split_paths(&self.package_path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            for name in &candidates {
                let path = dir.join(name);
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

        // ponytail: warn, not error; strict upgrade swaps the total-miss arm for Err(OutsidePackage).
        // Four production callers need the fall-through (`bare_name_does_not_anchor_on_cwd` panics without it).
        let found = match self.resolve_command(command) {
            Ok(found) => found,
            // This function performs the fallback, so it owns the warning, or the line vanishes.
            Err(CommandResolutionError::NotFound { .. }) => {
                log::warn!(
                    "Could not resolve {:?} via PATH, falling back to OS lookup.",
                    command.to_string_lossy()
                );
                return Ok(PathBuf::from(command));
            }
            // Never a blanket fallback: `execvp` would find the same trampoline on the ambient `PATH`.
            Err(error) => return Err(error),
        };
        // `{:?}`: the values are untrusted, and a raw newline forges a log line (CWE-117).
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

/// True unless `command` is exactly one [`Component::Normal`](std::path::Component).
///
/// `Path::join` discards the base for a rooted or prefixed value, so a looser test (a separator
/// check misses `C:tool`) stats outside every package directory.
fn command_is_path(command: &OsStr) -> bool {
    let mut components = std::path::Path::new(command).components();
    !matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

/// `Ok(())` when the candidate can be executed, else `Err(mode)` with its permission bits.
// ponytail: duplicated 3-line POSIX bit test; sharing would invert env→package layering
#[cfg(unix)]
fn executable_verdict(metadata: &std::fs::Metadata) -> Result<(), u32> {
    use std::os::unix::fs::PermissionsExt;
    // Checks the bit only; correct because the caller already filtered to `is_file()`.
    let mode = metadata.permissions().mode();
    if mode & 0o111 != 0 { Ok(()) } else { Err(mode & 0o7777) }
}

/// Non-Unix: a candidate matched a PATHEXT extension, so it is executable.
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
    /// A bare command name resolves in no directory of the composed `PATH`.
    #[error("{command:?} does not resolve in the composed environment; searched: {searched:?}")]
    NotFound {
        /// The bare command name as invoked.
        command: String,
        /// The non-empty `PATH` directories that were searched, in order.
        searched: Vec<PathBuf>,
    },

    /// The resolution answer is an ocx launcher trampoline, which would re-enter `ocx exec`.
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
    /// Every variant renders with `{:?}`: its values are untrusted, and a raw newline forges log lines (CWE-117).
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

/// The marker on line two of every POSIX launcher trampoline, all [`is_ocx_trampoline`] matches.
///
/// It must stay on line two, ahead of the baked root, or a deep checkout pushes it past
/// [`TRAMPOLINE_PROBE_BYTES`] and silently disarms the check.
/// The shared `# Generated by ocx` header cannot serve: refusing package launchers breaks `ocx launcher exec`.
pub const TRAMPOLINE_MARKER: &str = "# ocx-toolchain-trampoline";

/// How many bytes of a candidate file [`is_ocx_trampoline`] reads: a shebang plus the marker,
/// in one `read(2)`, deliberately smaller than a whole trampoline body.
pub const TRAMPOLINE_PROBE_BYTES: usize = 256;

/// Whether an already-resolved command path is an ocx launcher trampoline.
///
/// Judges the file, not a directory list, so it catches two project homes on one `PATH`,
/// which no exclusion set can name. Any I/O error answers "not a trampoline".
pub fn is_ocx_trampoline(path: &std::path::Path) -> bool {
    trampoline_signal(path)
}

/// POSIX half of [`is_ocx_trampoline`]: [`TRAMPOLINE_MARKER`] as line two of the probed prefix.
#[cfg(not(windows))]
fn trampoline_signal(path: &std::path::Path) -> bool {
    use std::io::Read as _;

    // Before the open: opening a FIFO blocks forever on the path that runs before every `exec`.
    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    // Fails open: this runs before every exec, so failing closed refuses on a transient `EACCES`.
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    // A prefix read, never `read_bounded`, which errors past its cap and so disarms deep checkouts.
    let mut prefix = Vec::with_capacity(TRAMPOLINE_PROBE_BYTES);
    if file
        .take(TRAMPOLINE_PROBE_BYTES as u64)
        .read_to_end(&mut prefix)
        .is_err()
    {
        return false;
    }
    // Line two matched whole, never anywhere, or a baked path spelling the marker refuses an ordinary launcher.
    prefix
        .split(|byte| *byte == b'\n')
        .nth(1)
        .is_some_and(|line| line == TRAMPOLINE_MARKER.as_bytes())
}

/// Windows half of [`is_ocx_trampoline`]: a sibling `.exec` sidecar, or an `.exec` path itself.
///
/// No content check: trampolines and lazy shims hardlink one blob, so a content match refuses shims.
#[cfg(windows)]
fn trampoline_signal(path: &std::path::Path) -> bool {
    // A `PATHEXT` containing `.EXEC` makes the sidecar itself an answer.
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exec"))
    {
        return true;
    }
    std::fs::metadata(path.with_extension("exec")).is_ok_and(|sidecar| sidecar.is_file())
}

/// Parses [`keys::OCX_INSECURE_REGISTRIES`] into a list of registry hostnames.
pub fn insecure_registries() -> Vec<String> {
    string(keys::OCX_INSECURE_REGISTRIES, String::new())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Serializes mirrors into the [`keys::OCX_MIRRORS`] JSON object that [`mirrors`] parses back;
/// `None` for an empty list.
fn encode_mirrors(mirrors: &[(String, crate::mirror::MirrorConfig)]) -> Option<String> {
    if mirrors.is_empty() {
        return None;
    }
    let mut object = serde_json::Map::with_capacity(mirrors.len());
    for (host, entry) in mirrors {
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

/// Parses [`keys::OCX_MIRRORS`] into `(host, MirrorConfig)` pairs; absent or empty is an empty list.
///
/// A broken value is a hard error: degrading it would route reads to the blocked origin.
///
/// # Errors
///
/// [`MirrorConfigError::MalformedEnvJson`] for invalid JSON; [`MirrorConfigError::InvalidShape`] /
/// [`MirrorConfigError::NonStringRoleValue`] for an unrecognized per-host value.
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
        assert!(is_reserved_ocx_key(keys::OCX_LAUNCH_IDENTITIES));
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

    // ── `Env::add_list` ──────────────────────────────────────────────────

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

    /// A child env carries the home the parent resolved.
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
        // `--global` (item 2, `adr_global_toolchain_tier.md` § Decisions (binding)) is
        // resolution-affecting, so `apply_ocx_config` MUST forward it to a
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
        // `OCX_LAZY_MODE` / `OCX_LAZY_REPORT` are NOT
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

    /// The conventional `env://` key variable is on the credential list.
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

    /// `OCX_ANNOUNCE_GIT_TOKEN` is a credential; its sibling
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

    /// The two ambient booleans with no view field and no flag. Without the
    /// forward a `--clean` child re-adopts a project the parent pruned, and
    /// re-fetches the managed config the parent suppressed — resolving against
    /// configuration the frame that spawned it never saw.
    ///
    /// Red state: delete either arm of the `OCX_NO_PROJECT` or
    /// `OCX_NO_CONFIG_REFRESH` block in `apply_ocx_config`.
    #[test]
    fn apply_ocx_config_forwards_the_ambient_resolution_kill_switches() {
        let guard = ocx_util::env::overrides::lock();

        guard.set(keys::OCX_NO_PROJECT, "1");
        guard.set(keys::OCX_NO_CONFIG_REFRESH, "1");
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert_eq!(
            env.get(keys::OCX_NO_PROJECT).and_then(std::ffi::OsStr::to_str),
            Some("1"),
            "a truthy OCX_NO_PROJECT must forward, or the child walks up and adopts a project"
        );
        assert_eq!(
            env.get(keys::OCX_NO_CONFIG_REFRESH).and_then(std::ffi::OsStr::to_str),
            Some("1"),
            "a truthy OCX_NO_CONFIG_REFRESH must forward, or the child re-fetches the managed tier"
        );

        // Absent (or falsy) → cleared, so a stale parent-shell export cannot
        // beat what this process resolved with. The inherited value is seeded
        // onto the child map first, the way
        // `apply_ocx_config_overwrites_inherited_stale_values` does: `remove`
        // on a key `Env::clean()` never held is a no-op, so asserting absence
        // from an empty map would hold with the remove arms deleted — a green
        // indistinguishable from the check never running.
        guard.remove(keys::OCX_NO_PROJECT);
        guard.set(keys::OCX_NO_CONFIG_REFRESH, "0");
        let mut env = Env::clean();
        env.set(keys::OCX_NO_PROJECT, "1");
        env.set(keys::OCX_NO_CONFIG_REFRESH, "1");
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_NO_PROJECT).is_none(),
            "an absent OCX_NO_PROJECT must clear an inherited value, not leave it standing"
        );
        assert!(
            env.get(keys::OCX_NO_CONFIG_REFRESH).is_none(),
            "a falsy OCX_NO_CONFIG_REFRESH must clear an inherited value, not leave it standing"
        );
    }

    /// The prune and an explicit project are two answers to one question, and
    /// the child reads the prune first (`ConfigLoader::explicit_project`). So
    /// an invocation carrying `--project` must forward the path and NOT the
    /// prune, or the child discards the path written beside it.
    ///
    /// Red state: drop the `cfg.project.is_none() &&` guard.
    #[test]
    fn apply_ocx_config_suppresses_the_project_prune_when_a_project_is_explicit() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(keys::OCX_NO_PROJECT, "1");

        let mut cfg = view("/abs/ocx");
        cfg.project = Some(std::path::PathBuf::from("/abs/repo/ocx.toml"));
        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);

        assert_eq!(
            env.get(keys::OCX_PROJECT).map(std::path::Path::new),
            Some(std::path::Path::new("/abs/repo/ocx.toml")),
            "the explicit project must reach the child"
        );
        assert!(
            env.get(keys::OCX_NO_PROJECT).is_none(),
            "the prune must not travel beside a project path the child would then discard"
        );
    }

    /// The two ambient strings. `OCX_DEFAULT_REGISTRY` decides what repository
    /// a bare identifier names and `OCX_INSECURE_REGISTRIES` decides what a
    /// dial may downgrade to, so a child resolving either differently reaches
    /// a different registry than the frame that spawned it.
    ///
    /// Empty is unset on the read side, so it must clear rather than travel.
    ///
    /// Red state: delete either arm of either `match` in `apply_ocx_config`.
    #[test]
    fn apply_ocx_config_forwards_the_ambient_registry_settings() {
        let guard = ocx_util::env::overrides::lock();

        guard.set(keys::OCX_DEFAULT_REGISTRY, "registry.example:5000");
        guard.set(keys::OCX_INSECURE_REGISTRIES, "registry.example:5000,other.example");
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert_eq!(
            env.get(keys::OCX_DEFAULT_REGISTRY).and_then(std::ffi::OsStr::to_str),
            Some("registry.example:5000"),
            "the default registry must forward, or a bare identifier names another repository"
        );
        assert_eq!(
            env.get(keys::OCX_INSECURE_REGISTRIES).and_then(std::ffi::OsStr::to_str),
            Some("registry.example:5000,other.example"),
            "the insecure authorities must forward verbatim, comma-joined as the parser reads them"
        );

        // Empty and absent are the same state to `ocx_util::env::string`, so
        // both clear — an empty value travelling onward would read as a
        // deliberate "no default registry" the parent never resolved. Seeded
        // onto the child map first for the reason the sibling above states:
        // `remove` on a key `Env::clean()` never held cannot fail, so the
        // assertion would hold with the remove arms deleted.
        guard.set(keys::OCX_DEFAULT_REGISTRY, "");
        guard.remove(keys::OCX_INSECURE_REGISTRIES);
        let mut env = Env::clean();
        env.set(keys::OCX_DEFAULT_REGISTRY, "stale.example");
        env.set(keys::OCX_INSECURE_REGISTRIES, "stale.example:5000");
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_DEFAULT_REGISTRY).is_none(),
            "an empty OCX_DEFAULT_REGISTRY must clear the inherited value, not forward either one"
        );
        assert!(
            env.get(keys::OCX_INSECURE_REGISTRIES).is_none(),
            "an absent OCX_INSECURE_REGISTRIES must clear an inherited value, not leave it standing"
        );
    }

    /// The consent opt-out survives the hop into a child ocx.
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

    /// An `ocx exec --no-consent` reaches the nested ocx a
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
    /// Inverted from the earlier assertion this replaces, following
    /// `interface_shim_names_refuses_the_literal_ocx_name`: the old test
    /// asserted exactly the fallback this contract deletes, so keeping it
    /// would have pinned the escape the contract exists to close.
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

    // ── `OCX_TOOLCHAIN_DIR` on the child env ────────────────────────────

    /// A resolved `toolchain_dir` travels to a child ocx, because it
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

    /// The `None` arm is **load-bearing**, not symmetry for its own
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

    // ── resolve_command / trampoline-refusal fixtures ───────────────────────

    /// An executable POSIX body carrying [`TRAMPOLINE_MARKER`] on its second
    /// line, exactly where a real generated trampoline puts it.
    #[cfg(unix)]
    fn write_trampoline(dir: &std::path::Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(
            &path,
            // The five-line shape the launcher generator actually emits,
            // including its single-quoted `__ocx_binary` assignment. A four-line
            // fixture spelling `${OCX_BINARY_PIN:-ocx}` would be a body this
            // codebase no longer produces, and every trampoline test here would
            // then be measured against a shape that cannot occur on disk.
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

    // ── the fallible `resolve_command` ──────────────────────────────────────

    /// A bare name a composed `PATH` directory provides resolves to that
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

    /// The `NotFound` error **names the search space it walked**, so a
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

    /// A **path-bearing** command keeps today's behaviour — including
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

    /// No `PATH` key at all is an empty search space, never an ambient
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

    /// A `PATH` of nothing but empty segments is
    /// **`None`**, never `Some("")` (CWE-426).
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

    /// An **empty `PATH` segment** means the current directory
    /// on Unix, and it is dropped from the lookup copy before the search (CWE-426).
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

    /// Three things on `PATH` that carry the right *name* but cannot be
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

    // ── `resolve_command_excluding` ──────────────────────────────────────────
    //
    // Every test below was written against the exclusion contract before the
    // implementation existed: while `Env::lookup_path_excluding` was still an
    // `unimplemented!()` stub each of them was an honest panic-red, so none of
    // them can have been shaped to fit whatever the body turned out to do.

    /// The excluded directory is not consulted, and the answer comes
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

    /// **The load-bearing invariant**: `excluded` is removed from the
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

    /// An empty exclusion set behaves exactly as
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

    /// Excluding a directory that is not on `PATH` is a no-op, never an
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

    /// Excluding **every** segment is `NotFound` — never a panic and
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

    /// The belt the two-guard design rests on: a directory that
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

    // ── the trampoline-identity refusal ─────────────────────────────────────

    /// **The input where only this guard defends**: a *foreign*
    /// home's trampoline on the lookup `PATH`.
    ///
    /// The caller's exclusion set can only ever name *this* invocation's own
    /// homes, so a second project's `<home>/toolchain/active/bin` on the same
    /// `PATH` is a directory the exclusion structurally cannot name. Under the
    /// exclusion alone the invocation loops A → B → A forever, one full compose
    /// per hop, with no error and no depth counter.
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

    /// A **symlink** on `PATH` pointing at a trampoline is
    /// refused, and the refusal judges the *link* path.
    ///
    /// `which` answers with the uncanonicalized path it walked, so the
    /// predicate is handed a symlink; `std::fs::metadata` and `File::open` both
    /// follow it, which is why it fires today. Swapping either for
    /// `symlink_metadata` — the natural "harden this against link tricks" edit
    /// — makes the answer "not a regular file" and silently disarms this predicate for
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

    /// A body **longer than [`TRAMPOLINE_PROBE_BYTES`]**
    /// is still refused when the marker sits inside the probed head — the deep
    /// checkout case, where a trampoline's baked absolute project root pushes
    /// the body past 256 bytes.
    ///
    /// `#[cfg(unix)]` like every other body-content row here: the probe window
    /// is the POSIX `trampoline_signal` only. The Windows half is the `.exec`
    /// sidecar and has no content check at all, so a shell body there
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

    /// The other half of the previous test: a marker sitting **beyond** the
    /// probe window is **not** refused.
    ///
    /// This is the row that pins the **bound**, so the fixture has to be one
    /// only the bound can answer: line *one* outruns
    /// [`TRAMPOLINE_PROBE_BYTES`], and the marker sits on line two — the exact
    /// position a real trampoline emits it at and the anchor accepts. The probed prefix
    /// therefore holds no `\n` at all, `nth(1)` is `None`, and the file is not
    /// a trampoline. Widen the constant and this test goes red, which is the
    /// whole point of it.
    ///
    /// A fixture that pushed the marker onto line *three* instead would be
    /// refused by the **anchor** at every bound — green with the window
    /// widened to a gigabyte, green with the bounded read deleted outright —
    /// and would pin nothing here.
    ///
    /// Recorded as the constraint the trampoline body generator must honour
    /// when it emits the body: the marker has to land within the first
    /// [`TRAMPOLINE_PROBE_BYTES`] bytes, which is why it puts the marker on line
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
            "the probe reads a bounded prefix; the trampoline body must emit the marker inside it"
        );
    }

    /// The positive control for the row above: the same anchor
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
    /// future re-ordering of the body disarms the guard loudly.
    ///
    /// `#[cfg(unix)]`: the line-two anchor is the POSIX signal. On Windows the
    /// signal is the `.exec` sidecar and no body is read, so the three
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

    /// A file that merely *contains* the marker somewhere in its body is
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

    /// A 0-byte file is not a trampoline, and the probe does not panic
    /// on it.
    #[test]
    fn a_zero_byte_file_is_not_a_trampoline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, b"").unwrap();

        assert!(!is_ocx_trampoline(&path), "an empty file carries no marker");
    }

    /// An unreadable file is **not** refused — the predicate fails
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

    /// A **FIFO** on the resolution path must not block.
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

    /// Windows: the sibling `.shim` sidecar of a lazy shim
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

    /// Windows: a sibling `.exec` sidecar **is** the trampoline signal.
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

    /// Windows: a resolved path whose **own** extension is
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
