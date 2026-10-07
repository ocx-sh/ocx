// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! One function per command, and the arguments each takes.

use std::path::PathBuf;

use super::runtime::{Error, Ocx, Outcome, Raw};
use super::types::{
    About, AnnounceReport, Catalog, ClaimReport, Clean, ConfigSetupData, ConfigTestData, ConfigUpdateData, CopyReport,
    InspectReport, Installs, LocatedPaths, LockReport, LoginResult, LogoutResult, PackageCascadeCheck,
    PackageCascadeRepair, PackageDescriptions, PackageReceipt, PatchFreezeReport, PatchPublishReport, PatchSyncReport,
    PatchWhyReport, Paths, PruneOutcome, PushReport, RegenerateReport, Removed, SelfSetupData, StatusReport, Tags,
    VerificationReport,
};
use super::wire::{Argv, Hyphen, Presence, Secret};

/// The options `ocx` takes before its subcommand. The SDK sets `--format` itself and never passes
/// `--quiet`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlobalOptions {
    /// Path to the ocx configuration file
    pub config: Option<PathBuf>,
    /// Project directory, or the project file itself (project-tier toolchain config)
    pub project: Option<PathBuf>,
    /// Select the global toolchain (`$OCX_HOME/ocx.toml`) as the project file in effect, instead of
    /// discovering a project by CWD walk
    pub global: bool,
    /// Route mutable lookups (tag list, catalog, tag->manifest) to the remote registry instead of
    /// the local index
    pub remote: bool,
    /// Disable all network access
    pub offline: bool,
    /// Freeze tag resolution to the local index; never fetch an unknown tag
    pub frozen: bool,
    /// Maximum number of root packages to pull concurrently
    pub jobs: Option<i64>,
    /// Overrides the local index home directory
    pub index: Option<PathBuf>,
    /// The log level to use
    pub log_level: Option<String>,
    /// When to use ANSI colors in output
    pub color: Option<String>,
}

impl GlobalOptions {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("config", &self.config, Presence::Optional, &[]);
        argv.flag("project", &self.project, Presence::Optional, &[]);
        argv.switch("global", self.global);
        argv.switch("remote", self.remote);
        argv.switch("offline", self.offline);
        argv.switch("frozen", self.frozen);
        argv.flag(
            "jobs",
            self.jobs.map(|number| number.to_string()),
            Presence::Optional,
            &[],
        );
        argv.flag("index", &self.index, Presence::Optional, &[]);
        argv.flag(
            "log-level",
            &self.log_level,
            Presence::Optional,
            &["trace", "debug", "info", "warn", "error", "off"],
        );
        argv.flag("color", &self.color, Presence::Optional, &["auto", "always", "never"]);
    }
}

/// The arguments of `ocx add`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddArgs {
    /// Named group to add the bindings to. Defaults to the implicit `\[tools\]` table when omitted
    pub group: Option<String>,
    /// Materialize resolved packages into the object store, installing on a local miss
    pub pull: bool,
    /// Skip materialization; resolve against local state only. Materialization is deferred to `ocx
    /// pull` or the first `ocx exec` / direnv hit
    pub no_pull: bool,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Tool identifiers to add, each optionally prefixed with `NAME=` (e.g. `ocx.sh/cmake:3.28`,
    /// `glab=ocx.sh/gitlab/cli`)
    pub identifiers: Vec<String>,
}

impl AddArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("group", &self.group, Presence::Optional, &[]);
        argv.switch("pull", self.pull);
        argv.switch("no-pull", self.no_pull);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.positional("identifiers", &self.identifiers, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx clean`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CleanArgs {
    /// Show what would be removed without actually removing anything
    pub dry_run: bool,
    /// Ignore the project registry and collect all unreferenced packages, including those held by
    /// other projects' `ocx.lock` files
    pub force: bool,
}

impl CleanArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("dry-run", self.dry_run);
        argv.switch("force", self.force);
    }
}

/// The arguments of `ocx config push`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigPushArgs {
    /// Identifier under which the config is published (e.g. `corp/ocx-config:user-1.4.2`)
    pub identifier: String,
    /// Update rolling variant tags derived from the version tag
    pub cascade: bool,
    /// Platform entry written into the package index. Defaults to `any`
    pub platform: Option<String>,
    /// The config file to publish (its content is staged as `config.toml`)
    pub config: PathBuf,
}

impl ConfigPushArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("identifier", [&self.identifier], Presence::Required, &[]);
        argv.switch("cascade", self.cascade);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.positional("config", [&self.config], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx config setup`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigSetupArgs {
    /// Adopt (or clear) this managed-config source
    pub managed_config: Option<String>,
    /// Report the intended actions without writing anything
    pub dry_run: bool,
    /// Overwrite a `\[managed\]` fence that carries user edits (the dirty state)
    pub force: bool,
}

impl ConfigSetupArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("managed-config", &self.managed_config, Presence::Optional, &[]);
        argv.switch("dry-run", self.dry_run);
        argv.switch("force", self.force);
    }
}

/// The arguments of `ocx config test`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigTestArgs {
    /// The config file to check
    pub config: PathBuf,
}

impl ConfigTestArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("config", [&self.config], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx config update`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigUpdateArgs {
    /// Version to sync: tag, `sha256:<hex>`, or `tag@sha256:<hex>`
    pub version: Option<String>,
    /// Report the managed-config tier's status without fetching or swapping
    pub check: bool,
    /// Pause the background refresh for a duration (e.g. `4h`, `3d`; max `7d`)
    pub pause: Option<String>,
    /// Clear an active pause and refresh immediately
    pub resume: bool,
}

impl ConfigUpdateArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("version", &self.version, Presence::Optional, Hyphen::Refuse);
        argv.switch("check", self.check);
        argv.flag("pause", &self.pause, Presence::Optional, &[]);
        argv.switch("resume", self.resume);
    }
}

/// The arguments of `ocx direnv export`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DirenvExportArgs {
    /// Restrict the composition to the named group(s)
    pub groups: Option<String>,
    /// Set an environment variable for this invocation, or pass one by name
    pub env: Option<String>,
    /// Materialize resolved packages into the object store, installing on a local miss
    pub pull: bool,
    /// Skip materialization; resolve against local state only. Materialization is deferred to `ocx
    /// pull` or the first `ocx exec` / direnv hit
    pub no_pull: bool,
    /// Control when a package's content downloads: now, or on first use
    pub lazy_mode: Option<String>,
}

impl DirenvExportArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("group", &self.groups, Presence::Optional, &[]);
        argv.flag("env", &self.env, Presence::Optional, &[]);
        argv.switch("pull", self.pull);
        argv.switch("no-pull", self.no_pull);
        argv.flag("lazy-mode", &self.lazy_mode, Presence::Optional, &["never", "always"]);
    }
}

/// The arguments of `ocx direnv init`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DirenvInitArgs {
    /// Overwrite an existing `.envrc` in the current directory
    pub force: bool,
}

impl DirenvInitArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("force", self.force);
    }
}

/// The arguments of `ocx env`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnvArgs {
    /// Restrict the composition to the named group(s)
    pub groups: Option<String>,
    /// Set an environment variable for this invocation, or pass one by name
    pub env: Option<String>,
    /// Target shell for eval-safe export lines
    pub shell: Option<String>,
    /// Write the composed environment into a CI system's persistence channel
    pub ci: Option<String>,
    /// Write the GitLab export to this file instead of stdout
    pub export_file: Option<PathBuf>,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Materialize resolved packages into the object store, installing on a local miss
    pub pull: bool,
    /// Skip materialization; resolve against local state only. Materialization is deferred to `ocx
    /// pull` or the first `ocx exec` / direnv hit
    pub no_pull: bool,
    /// Control when a package's content downloads: now, or on first use
    pub lazy_mode: Option<String>,
    /// Compose digest paths instead of the toolchain links
    pub pinned: bool,
    /// Compose through the toolchain links instead of digest paths
    pub no_pinned: bool,
    /// Annotate each entry with its origin package or companion identifier
    pub show_patches: bool,
}

impl EnvArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("group", &self.groups, Presence::Optional, &[]);
        argv.flag("env", &self.env, Presence::Optional, &[]);
        argv.flag(
            "shell",
            &self.shell,
            Presence::Optional,
            &[
                "ash",
                "ksh",
                "dash",
                "bash",
                "elvish",
                "fish",
                "batch",
                "powershell",
                "zsh",
                "nushell",
            ],
        );
        argv.flag("ci", &self.ci, Presence::Optional, &["github", "gitlab"]);
        argv.flag("export-file", &self.export_file, Presence::Optional, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.switch("pull", self.pull);
        argv.switch("no-pull", self.no_pull);
        argv.flag("lazy-mode", &self.lazy_mode, Presence::Optional, &["never", "always"]);
        argv.switch("pinned", self.pinned);
        argv.switch("no-pinned", self.no_pinned);
        argv.switch("show-patches", self.show_patches);
    }
}

/// The arguments of `ocx exec`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecArgs {
    /// Restrict the composition to the named group(s)
    pub groups: Option<String>,
    /// Start with a clean environment containing only the package variables, instead of inheriting
    /// the current shell environment
    pub clean: bool,
    /// Set an environment variable for this invocation, or pass one by name
    pub env: Option<String>,
    /// Control when a package's content downloads: now, or on first use
    pub lazy_mode: Option<String>,
    /// Compose digest paths instead of the toolchain links
    pub pinned: bool,
    /// Compose through the toolchain links instead of digest paths
    pub no_pinned: bool,
    /// Directory to write this invocation's execution record into
    pub dir: Option<PathBuf>,
    /// Filename template for the record, e.g. `{time}-{host}-{pid}.json`
    pub name: Option<String>,
    /// Record a consent stamp (the default unless OCX_NO_CONSENT is set)
    pub consent: bool,
    /// Run without consenting to this project's shell activation
    pub no_consent: bool,
    /// Binding names to compose into the child env. Each name must resolve unambiguously inside the
    /// selected scope. Only the named tools are resolved to a host leaf, so an unrelated tool in
    /// scope that ships no leaf for this host does not block the run. An empty list means "every
    /// binding in scope"; then every tool must resolve
    pub names: Vec<String>,
    /// Command to execute, with arguments. The command runs with the composed package env. `--` is
    /// mandatory and at least one argv token is required
    pub argv: Vec<String>,
}

impl ExecArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("group", &self.groups, Presence::Optional, &[]);
        argv.switch("clean", self.clean);
        argv.flag("env", &self.env, Presence::Optional, &[]);
        argv.flag("lazy-mode", &self.lazy_mode, Presence::Optional, &["never", "always"]);
        argv.switch("pinned", self.pinned);
        argv.switch("no-pinned", self.no_pinned);
        argv.flag("records-dir", &self.dir, Presence::Optional, &[]);
        argv.flag("records-name", &self.name, Presence::Optional, &[]);
        argv.switch("consent", self.consent);
        argv.switch("no-consent", self.no_consent);
        argv.positional("names", &self.names, Presence::Optional, Hyphen::Refuse);
        argv.operand("argv", &self.argv, Presence::Required);
    }
}

/// The arguments of `ocx index catalog`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexCatalogArgs {
    /// List tags for each repository in the catalog
    pub with_tags: bool,
    /// Registries to list repositories from (defaults to OCX_DEFAULT_REGISTRY)
    pub registries: Vec<String>,
}

impl IndexCatalogArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("with-tags", self.with_tags);
        argv.positional("registries", &self.registries, Presence::Optional, Hyphen::Refuse);
    }
}

/// The arguments of `ocx index list`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexListArgs {
    /// Shows which platforms are available for each package. Uses the tag from the identifier, or
    /// `latest` if none specified
    pub platforms: bool,
    /// Lists unique variant names found in the tags
    pub variants: bool,
    /// Package identifiers to list the available versions for
    pub packages: Vec<String>,
}

impl IndexListArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("platforms", self.platforms);
        argv.switch("variants", self.variants);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx index regenerate`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexRegenerateArgs {
    /// Registries whose local catalog is rebuilt from the package roots on disk
    pub registries: Vec<String>,
}

impl IndexRegenerateArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("registries", &self.registries, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx index sync`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexSyncArgs {
    /// Registries whose catalog names the packages to refresh
    pub registries: Vec<String>,
    /// Print the packages this would refresh, and refresh none of them
    pub dry_run: bool,
}

impl IndexSyncArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("registries", &self.registries, Presence::Required, Hyphen::Refuse);
        argv.switch("dry-run", self.dry_run);
    }
}

/// The arguments of `ocx index update`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexUpdateArgs {
    /// Packages to refresh; a bare name resolves against the default registry
    pub packages: Vec<String>,
}

impl IndexUpdateArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx init`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InitArgs {
    /// Record a consent stamp (the default unless OCX_NO_CONSENT is set)
    pub consent: bool,
    /// Run without consenting to this project's shell activation
    pub no_consent: bool,
}

impl InitArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("consent", self.consent);
        argv.switch("no-consent", self.no_consent);
    }
}

/// The arguments of `ocx inspect`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InspectArgs {
    /// Restrict the composition to the named group(s)
    pub groups: Option<String>,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Set an environment variable for this invocation, or pass one by name
    pub env: Option<String>,
    /// Select this host's leaf and emit its metadata plus the OCI resolution chain
    pub resolve: bool,
    /// Compute each binding's dependency closure from metadata alone, without installing
    pub closure: bool,
    /// Binding names to inspect; defaults to every binding in the selected groups
    pub names: Vec<String>,
}

impl InspectArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("group", &self.groups, Presence::Optional, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("env", &self.env, Presence::Optional, &[]);
        argv.switch("resolve", self.resolve);
        argv.switch("closure", self.closure);
        argv.positional("names", &self.names, Presence::Optional, Hyphen::Refuse);
    }
}

/// The arguments of `ocx launcher exec`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LauncherExecArgs {
    /// Absolute path to the installed package root (the directory containing `metadata.json`).
    /// Baked into the launcher at install time
    pub pkg_root: PathBuf,
    /// The launcher's own filename (argv0 passed after `--`), used to identify which entrypoint to
    /// dispatch
    pub argv: Vec<String>,
}

impl LauncherExecArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("pkg_root", [&self.pkg_root], Presence::Required, Hyphen::Refuse);
        argv.operand("argv", &self.argv, Presence::Required);
    }
}

/// The arguments of `ocx launcher shim`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LauncherShimArgs {
    /// Show progress while a deferred package downloads on first use
    pub lazy_report: Option<String>,
    /// Pinned identifier of the deferred tool, baked into the shim
    pub identifier: String,
    /// The shim's own filename (argv0 passed after `--`), then the user's arguments. The filename
    /// selects which of the tool's declared names was invoked
    pub argv: Vec<String>,
}

impl LauncherShimArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag(
            "lazy-report",
            &self.lazy_report,
            Presence::Optional,
            &["silent", "progress"],
        );
        argv.positional("identifier", [&self.identifier], Presence::Required, Hyphen::Refuse);
        argv.operand("argv", &self.argv, Presence::Required);
    }
}

/// The arguments of `ocx lock`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LockArgs {
    /// Verify `ocx.lock` is current relative to `ocx.toml` and exit
    pub check: bool,
    /// Materialize resolved packages into the object store, installing on a local miss
    pub pull: bool,
    /// Skip materialization; resolve against local state only. Materialization is deferred to `ocx
    /// pull` or the first `ocx exec` / direnv hit
    pub no_pull: bool,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
}

impl LockArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("check", self.check);
        argv.switch("pull", self.pull);
        argv.switch("no-pull", self.no_pull);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
    }
}

/// The arguments of `ocx login`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoginArgs {
    /// Username for the registry. Prompted when omitted on a TTY
    pub username: Option<String>,
    /// Read password/token from stdin. Required in non-interactive contexts
    pub password_stdin: Option<Secret>,
    /// Allow plaintext fallback to `~/.docker/config.json` `auths` when no native helper is
    /// configured. Default: refuse, exit `ConfigError(78)` with install hint
    pub allow_insecure_store: bool,
    /// Verify the operation against the registry before committing it (default)
    pub verify: bool,
    /// Skip verification
    pub no_verify: bool,
    /// Registry hostname (e.g. `ghcr.io`, `registry.example.com`). Optional - falls back to
    /// `OCX_DEFAULT_REGISTRY`
    pub registry: Option<String>,
}

impl LoginArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("username", &self.username, Presence::Optional, &[]);
        argv.secret("password-stdin", self.password_stdin.as_ref());
        argv.switch("allow-insecure-store", self.allow_insecure_store);
        argv.switch("verify", self.verify);
        argv.switch("no-verify", self.no_verify);
        argv.positional("registry", &self.registry, Presence::Optional, Hyphen::Refuse);
    }
}

/// The arguments of `ocx logout`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogoutArgs {
    /// Registry hostname (e.g. `ghcr.io`). Optional - falls back to `OCX_DEFAULT_REGISTRY`
    pub registry: Option<String>,
}

impl LogoutArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("registry", &self.registry, Presence::Optional, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package announce`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageAnnounceArgs {
    /// Package to announce, as `<namespace>/<package>` (e.g. `acme/widget`)
    pub package: Option<String>,
    /// Replace the curated tag set with this comma-separated list. A currently-committed tag that
    /// is not named here is dropped. So is a reserved tag named here: an `__ocx` tag (the keep tag
    /// included) or a legacy `sha256.<hex>` one is not a version, so the run still succeeds and
    /// reports the drops
    pub tags: Option<String>,
    /// Add, update or remove the tags listed in this file: a listed tag the registry no longer has
    /// is removed from the index. Rows the file does not list are left as they are
    pub tags_file: Option<PathBuf>,
    /// Add every tag the registry holds; remove ephemeral rows whose tag is gone, and report
    /// durable ones
    pub tags_from_registry: bool,
    /// Re-observe every committed tag; remove ephemeral rows whose tag is gone, and report durable
    /// ones
    pub refresh: bool,
    /// Mark the tags this run adds as removable without review
    pub ephemeral: bool,
    /// Index repository the request targets, as `\[HOST/\]NAMESPACE/PROJECT`
    pub index_repo: Option<String>,
    /// Which forge hosts the index repository
    pub forge: Option<String>,
    /// How the request is written
    pub transport: Option<String>,
    /// Open (or update) the request from this fork, as `\[HOST/\]NAMESPACE/PROJECT`
    pub fork: Option<String>,
    /// Write the rendered index entry under this directory instead of opening a request. Works
    /// without a credential
    pub output: Option<PathBuf>,
    /// Mark a tag as yanked. Repeat for multiple tags. Requires `--yank-reason`; only applies to a
    /// tag already in the curated set
    pub yank: Option<String>,
    /// Clear the yanked marker from a tag. Repeat for multiple tags
    pub unyank: Option<String>,
    /// Reason recorded on every tag named by `--yank` in this run
    pub yank_reason: Option<String>,
}

impl PackageAnnounceArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("package", &self.package, Presence::Optional, Hyphen::Refuse);
        argv.flag("tags", &self.tags, Presence::Optional, &[]);
        argv.flag("tags-file", &self.tags_file, Presence::Optional, &[]);
        argv.switch("tags-from-registry", self.tags_from_registry);
        argv.switch("refresh", self.refresh);
        argv.switch("ephemeral", self.ephemeral);
        argv.flag("index-repo", &self.index_repo, Presence::Optional, &[]);
        argv.flag("forge", &self.forge, Presence::Optional, &["github", "gitlab"]);
        argv.flag("transport", &self.transport, Presence::Optional, &["api", "git"]);
        argv.flag("fork", &self.fork, Presence::Optional, &[]);
        argv.flag("output", &self.output, Presence::Optional, &[]);
        argv.flag("yank", &self.yank, Presence::Optional, &[]);
        argv.flag("unyank", &self.unyank, Presence::Optional, &[]);
        argv.flag("yank-reason", &self.yank_reason, Presence::Optional, &[]);
    }
}

/// The arguments of `ocx package attest`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageAttestArgs {
    /// Narrow into one platform of an image index
    pub platform: Option<String>,
    /// Predicate document to attach (JSON)
    pub predicate: PathBuf,
    /// Predicate type: an alias or a full URI
    pub predicate_type: String,
    /// Fulcio CA endpoint (the keyless certificate issuer)
    pub fulcio_url: Option<String>,
    /// Rekor transparency-log endpoint
    pub rekor_url: Option<String>,
    /// Read the OIDC identity token from this file (highest precedence)
    pub identity_token_file: Option<PathBuf>,
    /// Read the OIDC identity token from stdin (second precedence)
    pub identity_token_stdin: Option<Secret>,
    /// Suppress the interactive browser OAuth fallback (CI / headless)
    pub no_tty: bool,
    /// Bypass the referrers-capability cache for this invocation
    pub no_cache: bool,
    /// Signature wire format: bundle (default), simplesigning, or both
    pub signature_format: Option<String>,
    /// Sign or verify with a key pair instead of keyless Sigstore
    pub key: Option<String>,
    /// Record the signature in the Rekor transparency log
    pub rekor_upload: bool,
    /// Skip the Rekor entry
    pub no_rekor_upload: bool,
    /// Tags to sweep. Repeatable, and accepts a comma-separated list
    pub tags: Option<String>,
    /// Read tags from a file, one per line or comma-separated
    pub tags_file: Option<PathBuf>,
    /// Package identifier to attest (`registry/repo:tag\[@digest\]`)
    pub identifier: String,
}

impl PackageAttestArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("predicate", [&self.predicate], Presence::Required, &[]);
        argv.flag("type", [&self.predicate_type], Presence::Required, &[]);
        argv.flag("fulcio-url", &self.fulcio_url, Presence::Optional, &[]);
        argv.flag("rekor-url", &self.rekor_url, Presence::Optional, &[]);
        argv.flag(
            "identity-token-file",
            &self.identity_token_file,
            Presence::Optional,
            &[],
        );
        argv.secret("identity-token-stdin", self.identity_token_stdin.as_ref());
        argv.switch("no-tty", self.no_tty);
        argv.switch("no-cache", self.no_cache);
        argv.flag(
            "signature-format",
            &self.signature_format,
            Presence::Optional,
            &["bundle", "simplesigning", "both"],
        );
        argv.flag("key", &self.key, Presence::Optional, &[]);
        argv.switch("rekor-upload", self.rekor_upload);
        argv.switch("no-rekor-upload", self.no_rekor_upload);
        argv.flag("tags", &self.tags, Presence::Optional, &[]);
        argv.flag("tags-file", &self.tags_file, Presence::Optional, &[]);
        argv.positional("identifier", [&self.identifier], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package cascade check`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageCascadeCheckArgs {
    /// Packages to audit
    pub packages: Vec<String>,
}

impl PackageCascadeCheckArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package cascade repair`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageCascadeRepairArgs {
    /// Compute and print the same plan without writing anything
    pub dry_run: bool,
    /// Append the rolling tags this run moved or created to this file, one per line, for `ocx
    /// package announce --tags-file`. Creates the file if absent and keeps the tags already in it.
    /// Takes one package per run, since the file names no package and `announce` publishes it
    /// against one.
    pub tags_file: Option<PathBuf>,
    /// Packages to repair
    pub packages: Vec<String>,
}

impl PackageCascadeRepairArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("dry-run", self.dry_run);
        argv.flag("tags-file", &self.tags_file, Presence::Optional, &[]);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package claim`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageClaimArgs {
    /// Index repository the request targets, as `\[HOST/\]NAMESPACE/PROJECT`
    pub index_repo: Option<String>,
    /// Which forge hosts the index repository
    pub forge: Option<String>,
    /// How the request is written
    pub transport: Option<String>,
    /// Open (or update) the request from this fork, as `\[HOST/\]NAMESPACE/PROJECT`
    pub fork: Option<String>,
    /// Write the rendered index entry under this directory instead of opening a request. Works
    /// without a credential
    pub output: Option<PathBuf>,
    /// The physical OCI repository the package's bytes live in, as `oci://HOST/PATH`
    pub repository: String,
    /// An owner of the package, as `LOGIN` or `LOGIN:ID`. Repeat for several, in the order they
    /// should be recorded
    pub owner: Option<String>,
    /// The upstream organization this package mirrors or repackages
    pub upstream_org: Option<String>,
    /// The upstream project's repository URL, as an absolute `http` or `https` URL carrying no
    /// embedded credentials
    pub upstream_repository_url: Option<String>,
    /// A disclaimer recorded on the index entry, for a package that is not operated by the upstream
    /// project
    pub upstream_disclaimer: Option<String>,
    /// Namespace and package to claim, as `<namespace>/<package>` (e.g. `acme/widget`)
    pub package: String,
}

impl PackageClaimArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("index-repo", &self.index_repo, Presence::Optional, &[]);
        argv.flag("forge", &self.forge, Presence::Optional, &["github", "gitlab"]);
        argv.flag("transport", &self.transport, Presence::Optional, &["api", "git"]);
        argv.flag("fork", &self.fork, Presence::Optional, &[]);
        argv.flag("output", &self.output, Presence::Optional, &[]);
        argv.flag("repository", [&self.repository], Presence::Required, &[]);
        argv.flag("owner", &self.owner, Presence::Optional, &[]);
        argv.flag("upstream-org", &self.upstream_org, Presence::Optional, &[]);
        argv.flag(
            "upstream-repository-url",
            &self.upstream_repository_url,
            Presence::Optional,
            &[],
        );
        argv.flag(
            "upstream-disclaimer",
            &self.upstream_disclaimer,
            Presence::Optional,
            &[],
        );
        argv.positional("package", [&self.package], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package copy`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageCopyArgs {
    /// Rewrite only the registry host, keeping the repository path and tag
    pub to: Option<String>,
    /// Full target reference, when the repository path or tag changes too
    pub identifier: Option<String>,
    /// Platform to copy. Repeatable
    pub platform: Option<String>,
    /// Recompute the rolling tags (`1.4`, `1`, `latest`) at the target
    pub cascade: bool,
    /// Write a `__ocx.keep.sha256-<hex>` tag for each platform manifest published (default)
    pub keep_tag: bool,
    /// Skip the keep tag
    pub no_keep_tag: bool,
    /// Carry the signatures, SBOMs and attestations anchored to each manifest (default)
    pub referrers: bool,
    /// Leave the signatures, SBOMs and attestations behind
    pub no_referrers: bool,
    /// Also copy the repository description (`__ocx.desc`): README and logo
    pub with_description: bool,
    /// Record an OCI annotation on the target's image index. Repeatable
    pub annotation: Option<String>,
    /// Report what would be copied and write nothing
    pub dry_run: bool,
    /// Package to copy: `registry/repository:tag` or `registry/repository@sha256:...`
    pub source: String,
}

impl PackageCopyArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("to", &self.to, Presence::Optional, &[]);
        argv.flag("identifier", &self.identifier, Presence::Optional, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.switch("cascade", self.cascade);
        argv.switch("keep-tag", self.keep_tag);
        argv.switch("no-keep-tag", self.no_keep_tag);
        argv.switch("referrers", self.referrers);
        argv.switch("no-referrers", self.no_referrers);
        argv.switch("with-description", self.with_description);
        argv.flag("annotation", &self.annotation, Presence::Optional, &[]);
        argv.switch("dry-run", self.dry_run);
        argv.positional("source", [&self.source], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package create`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageCreateArgs {
    /// Path to the package to bundle
    pub path: PathBuf,
    /// Identifier the bundle will be published under (e.g. `repo:2.0.0`)
    pub identifier: Option<String>,
    /// Platform of the package content (e.g. `linux/amd64`, or `any` for platform-agnostic content)
    pub platform: Option<String>,
    /// Output file or directory, if a directory is provided the filename will be inferred
    pub output: Option<PathBuf>,
    /// Force overwrite of output file if it already exists
    pub force: bool,
    /// Path to a `metadata.json` file to validate, resolve, and write alongside the output bundle
    pub metadata: Option<PathBuf>,
    /// Compression level to use for the package bundle
    pub compression_level: Option<String>,
    /// Number of compression threads (0 = auto-detect, 1 = single-threaded)
    pub threads: Option<i64>,
    /// Scan the content tree for executables the package puts on `PATH`, verifying a declared
    /// `binaries` claim or filling an absent one
    pub bin_scan: bool,
    /// Skip the executable scan; the `binaries` field passes through unchanged, whether declared or
    /// absent
    pub no_bin_scan: bool,
    /// Skip the libc check on the packaged binaries
    pub no_libc_lint: bool,
    /// Treat PATH as an archive and bundle what it extracts to
    pub extract: bool,
    /// Drop the leading N path components of every extracted entry
    pub strip_components: Option<i64>,
}

impl PackageCreateArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("path", [&self.path], Presence::Required, Hyphen::Refuse);
        argv.flag("identifier", &self.identifier, Presence::Optional, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("output", &self.output, Presence::Optional, &[]);
        argv.switch("force", self.force);
        argv.flag("metadata", &self.metadata, Presence::Optional, &[]);
        argv.flag(
            "compression-level",
            &self.compression_level,
            Presence::Optional,
            &["fast", "best", "default"],
        );
        argv.flag(
            "threads",
            self.threads.map(|number| number.to_string()),
            Presence::Optional,
            &[],
        );
        argv.switch("bin-scan", self.bin_scan);
        argv.switch("no-bin-scan", self.no_bin_scan);
        argv.switch("no-libc-lint", self.no_libc_lint);
        argv.switch("extract", self.extract);
        argv.flag(
            "strip-components",
            self.strip_components.map(|number| number.to_string()),
            Presence::Optional,
            &[],
        );
    }
}

/// The arguments of `ocx package deps`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageDepsArgs {
    /// Expose the package's full dep set, including private (self-only) edges. Affects `--flat`.
    /// See `ocx exec --help` for the full surface semantics
    pub self_view: bool,
    /// Show the flattened evaluation order instead of the tree
    pub flat: bool,
    /// Explain why a dependency is pulled in (matches by registry and repository; tag is ignored)
    pub why: Option<String>,
    /// Limit tree depth (default: unlimited). Only applies to tree view
    pub depth: Option<i64>,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Package identifiers to inspect
    pub packages: Vec<String>,
}

impl PackageDepsArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("self", self.self_view);
        argv.switch("flat", self.flat);
        argv.flag("why", &self.why, Presence::Optional, &[]);
        argv.flag(
            "depth",
            self.depth.map(|number| number.to_string()),
            Presence::Optional,
            &[],
        );
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package description pull`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageDescriptionPullArgs {
    /// Save the README to this file or directory (single package only)
    pub save_readme: Option<PathBuf>,
    /// Save the logo to this file or directory (single package only)
    pub save_logo: Option<PathBuf>,
    /// Package repositories to query
    pub packages: Vec<String>,
}

impl PackageDescriptionPullArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("save-readme", &self.save_readme, Presence::Optional, &[]);
        argv.flag("save-logo", &self.save_logo, Presence::Optional, &[]);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package description push`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageDescriptionPushArgs {
    /// Copy the whole description from another package repository
    pub from: Option<String>,
    /// Path to the README markdown file
    pub readme: Option<PathBuf>,
    /// Path to an optional logo image (PNG or SVG)
    pub logo: Option<PathBuf>,
    /// Short title for catalog display (sets org.opencontainers.image.title)
    pub title: Option<String>,
    /// One-line summary for catalog display (sets org.opencontainers.image.description)
    pub description: Option<String>,
    /// Comma-separated search keywords (sets sh.ocx.keywords)
    pub keywords: Option<String>,
    /// The package repository. Tag is ignored; always pushes to __ocx.desc
    pub identifier: String,
}

impl PackageDescriptionPushArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("from", &self.from, Presence::Optional, &[]);
        argv.flag("readme", &self.readme, Presence::Optional, &[]);
        argv.flag("logo", &self.logo, Presence::Optional, &[]);
        argv.flag("title", &self.title, Presence::Optional, &[]);
        argv.flag("description", &self.description, Presence::Optional, &[]);
        argv.flag("keywords", &self.keywords, Presence::Optional, &[]);
        argv.positional("identifier", [&self.identifier], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package deselect`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageDeselectArgs {
    /// Package identifiers to deselect
    pub packages: Vec<String>,
}

impl PackageDeselectArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package env`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageEnvArgs {
    /// Expose the package's full env, including private (self-only) entries. See `ocx exec --help`
    /// for full view semantics
    pub self_view: bool,
    /// Set an environment variable for this invocation, or pass one by name
    pub env: Option<String>,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Resolve the content path via the installed candidate symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/candidates/<tag>`)
    pub candidate: bool,
    /// Resolve the content path via the current-selected symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/current`)
    pub current: bool,
    /// Resolve the content path via a link written by `ocx package install --link <PATH>`
    pub link: Option<PathBuf>,
    /// Control when a package's content downloads: now, or on first use
    pub lazy_mode: Option<String>,
    /// Package identifiers to resolve the environment for
    pub packages: Vec<String>,
    /// Target shell for eval-safe export lines
    pub shell: Option<String>,
    /// Write the composed environment into a CI system's persistence channel
    pub ci: Option<String>,
    /// Write the GitLab export to this file instead of stdout
    pub export_file: Option<PathBuf>,
    /// Annotate each entry with its origin package or companion identifier
    pub show_patches: bool,
}

impl PackageEnvArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("self", self.self_view);
        argv.flag("env", &self.env, Presence::Optional, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.switch("candidate", self.candidate);
        argv.switch("current", self.current);
        argv.flag("link", &self.link, Presence::Optional, &[]);
        argv.flag("lazy-mode", &self.lazy_mode, Presence::Optional, &["never", "always"]);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
        argv.flag(
            "shell",
            &self.shell,
            Presence::Optional,
            &[
                "ash",
                "ksh",
                "dash",
                "bash",
                "elvish",
                "fish",
                "batch",
                "powershell",
                "zsh",
                "nushell",
            ],
        );
        argv.flag("ci", &self.ci, Presence::Optional, &["github", "gitlab"]);
        argv.flag("export-file", &self.export_file, Presence::Optional, &[]);
        argv.switch("show-patches", self.show_patches);
    }
}

/// The arguments of `ocx package exec`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageExecArgs {
    /// Start with a clean environment containing only the package variables, instead of inheriting
    /// the current shell environment
    pub clean: bool,
    /// Expose the package's full env, including its private (self-only) entries. Off by default:
    /// only public + interface entries are loaded (the consumer view). Generated launchers use `ocx
    /// launcher exec` which enables self-view internally
    pub self_view: bool,
    /// Remove the packages from the store once the command finishes
    pub rm: bool,
    /// Set an environment variable for this invocation, or pass one by name
    pub env: Option<String>,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Resolve the content path via the installed candidate symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/candidates/<tag>`)
    pub candidate: bool,
    /// Resolve the content path via the current-selected symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/current`)
    pub current: bool,
    /// Resolve the content path via a link written by `ocx package install --link <PATH>`
    pub link: Option<PathBuf>,
    /// Control when a package's content downloads: now, or on first use
    pub lazy_mode: Option<String>,
    /// Directory to write this invocation's execution record into
    pub dir: Option<PathBuf>,
    /// Filename template for the record, e.g. `{time}-{host}-{pid}.json`
    pub name: Option<String>,
    /// Package identifiers to layer environment from
    pub packages: Vec<String>,
    /// Command to execute, with arguments. The command will be executed with the environment with
    /// the packages
    pub command: Vec<String>,
}

impl PackageExecArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("clean", self.clean);
        argv.switch("self", self.self_view);
        argv.switch("rm", self.rm);
        argv.flag("env", &self.env, Presence::Optional, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.switch("candidate", self.candidate);
        argv.switch("current", self.current);
        argv.flag("link", &self.link, Presence::Optional, &[]);
        argv.flag("lazy-mode", &self.lazy_mode, Presence::Optional, &["never", "always"]);
        argv.flag("records-dir", &self.dir, Presence::Optional, &[]);
        argv.flag("records-name", &self.name, Presence::Optional, &[]);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
        argv.terminator();
        argv.positional("command", &self.command, Presence::Required, Hyphen::Allow);
    }
}

/// The arguments of `ocx package inspect`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageInspectArgs {
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Set an environment variable for this invocation, or pass one by name
    pub env: Option<String>,
    /// Platform-select through the index and emit the OCI resolution chain (pinned identifier and
    /// walk-order chain digests: index, manifest, config) alongside the metadata and layers.
    /// Without `--resolve` or `--closure`, an image-index reference lists its platform candidates
    /// instead
    pub resolve: bool,
    /// Compute the metadata-only dependency closure without installing
    pub closure: bool,
    /// Package identifiers to inspect (each a tag or `@digest`)
    pub packages: Vec<String>,
}

impl PackageInspectArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("env", &self.env, Presence::Optional, &[]);
        argv.switch("resolve", self.resolve);
        argv.switch("closure", self.closure);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package install`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageInstallArgs {
    /// Also set the installed version as current (creates the current symlink)
    pub select: bool,
    /// Also link the installed package at PATH, which keeps it from `ocx clean` while the link
    /// exists
    pub link: Option<PathBuf>,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Verify the package's Sigstore signature before installing (default)
    pub verify: bool,
    /// Skip Sigstore signature verification. Equivalent env var: `OCX_NO_VERIFY`
    pub no_verify: bool,
    /// Package identifiers to install
    pub packages: Vec<String>,
}

impl PackageInstallArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("select", self.select);
        argv.flag("link", &self.link, Presence::Optional, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.switch("verify", self.verify);
        argv.switch("no-verify", self.no_verify);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package prune`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackagePruneArgs {
    /// Select every build of this pre-release and its rolling tag, e.g. `0.5.0-canary`
    pub prerelease: Option<String>,
    /// With `--prerelease`: keep the newest N builds and the rolling tag (N >= 1)
    pub keep_builds: Option<i64>,
    /// Delete tags the index does not mark ephemeral, or with no index to ask
    pub force: bool,
    /// Append each gone tag the index lists, one per line, for `ocx package announce --tags-file`.
    /// Creates the file if absent and keeps the tags already in it. Written on every run that gets
    /// past selection, empty included, never on a dry run
    pub tags_file: Option<PathBuf>,
    /// Run the safeguard and report; delete nothing, write nothing
    pub dry_run: bool,
    /// Package whose tags to delete, e.g. `ocx.acme.example/acme/tool`
    pub package: String,
    /// Delete exactly these tags
    pub tags: Vec<String>,
}

impl PackagePruneArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("prerelease", &self.prerelease, Presence::Optional, &[]);
        argv.flag(
            "keep-builds",
            self.keep_builds.map(|number| number.to_string()),
            Presence::Optional,
            &[],
        );
        argv.switch("force", self.force);
        argv.flag("tags-file", &self.tags_file, Presence::Optional, &[]);
        argv.switch("dry-run", self.dry_run);
        argv.positional("package", [&self.package], Presence::Required, Hyphen::Refuse);
        argv.positional("tags", &self.tags, Presence::Optional, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package pull`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackagePullArgs {
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Verify the package's Sigstore signature before installing (default)
    pub verify: bool,
    /// Skip Sigstore signature verification. Equivalent env var: `OCX_NO_VERIFY`
    pub no_verify: bool,
    /// Package identifiers to pull
    pub packages: Vec<String>,
}

impl PackagePullArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.switch("verify", self.verify);
        argv.switch("no-verify", self.no_verify);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package push`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackagePushArgs {
    /// Will cascade rolling releases, ie. pushing 1.2.3 will also update 1.2, 1, etc
    pub cascade: bool,
    /// Let the pushed tag's variant also own the un-prefixed version track
    pub default: bool,
    /// Skip the keep tag
    pub no_keep_tag: bool,
    /// Append a UTC build-metadata segment to the published tag
    pub build_timestamp: Option<String>,
    /// Path to the package metadata JSON file. Defaults to a sibling of the first file layer (e.g.
    /// `pkg.tar.gz` -> `pkg-metadata.json`). Required when no file layers are provided
    pub metadata: Option<PathBuf>,
    /// Record an OCI annotation on the published image index. Repeatable
    pub annotation: Option<String>,
    /// Stamp OCI annotations from the CI environment
    pub ci_annotations: Option<String>,
    /// After a successful push, append the pushed tag and any cascade tags to this file, one per
    /// line (creating it if absent), so `ocx package announce --tags-file` can pick them up
    pub tags_file: Option<PathBuf>,
    /// After the push, attest this CycloneDX SBOM against the pushed manifest
    pub sbom: Option<PathBuf>,
    /// Sign each platform manifest this push writes, inline
    pub sign: bool,
    /// Fulcio CA endpoint (the keyless certificate issuer)
    pub fulcio_url: Option<String>,
    /// Rekor transparency-log endpoint
    pub rekor_url: Option<String>,
    /// Signature wire format: bundle (default), simplesigning, or both
    pub signature_format: Option<String>,
    /// Sign or verify with a key pair instead of keyless Sigstore
    pub key: Option<String>,
    /// Record the signature in the Rekor transparency log
    pub rekor_upload: bool,
    /// Skip the Rekor entry
    pub no_rekor_upload: bool,
    /// Target platform (e.g. `linux/amd64`, or `any` for platform-agnostic content)
    pub platform: Option<String>,
    /// Identifier under which the package is published (e.g. `repo:2.0.0`)
    pub identifier: Option<String>,
    /// Layers to push, in order (base layer first, top layer last)
    pub layers: Vec<String>,
    /// Write a `__ocx.keep.sha256-<hex>` tag for each platform manifest published (default)
    pub keep_tag: bool,
}

impl PackagePushArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("cascade", self.cascade);
        argv.switch("default", self.default);
        argv.switch("no-keep-tag", self.no_keep_tag);
        argv.flag(
            "build-timestamp",
            &self.build_timestamp,
            Presence::Optional,
            &["datetime", "date", "none"],
        );
        argv.flag("metadata", &self.metadata, Presence::Optional, &[]);
        argv.flag("annotation", &self.annotation, Presence::Optional, &[]);
        argv.flag(
            "ci-annotations",
            &self.ci_annotations,
            Presence::Optional,
            &["github", "gitlab"],
        );
        argv.flag("tags-file", &self.tags_file, Presence::Optional, &[]);
        argv.flag("sbom", &self.sbom, Presence::Optional, &[]);
        argv.switch("sign", self.sign);
        argv.flag("fulcio-url", &self.fulcio_url, Presence::Optional, &[]);
        argv.flag("rekor-url", &self.rekor_url, Presence::Optional, &[]);
        argv.flag(
            "signature-format",
            &self.signature_format,
            Presence::Optional,
            &["bundle", "simplesigning", "both"],
        );
        argv.flag("key", &self.key, Presence::Optional, &[]);
        argv.switch("rekor-upload", self.rekor_upload);
        argv.switch("no-rekor-upload", self.no_rekor_upload);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("identifier", &self.identifier, Presence::Optional, &[]);
        argv.positional("layers", &self.layers, Presence::Optional, Hyphen::Refuse);
        argv.switch("keep-tag", self.keep_tag);
    }
}

/// The arguments of `ocx package receipt`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageReceiptArgs {
    /// Path to the bundle `ocx package create -o` wrote
    pub bundle: PathBuf,
}

impl PackageReceiptArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("bundle", [&self.bundle], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package sbom`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageSbomArgs {
    /// Narrow into one platform of an image index
    pub platform: Option<String>,
    /// Write the SBOM document to PATH ("-" for stdout)
    pub output: Option<PathBuf>,
    /// Parse each SBOM and report component counts (CycloneDX 1.5-1.7 only)
    pub summary: bool,
    /// Restrict to one predicate type (for example cyclonedx or spdx)
    pub predicate_type: Option<String>,
    /// Expected certificate SAN (exact match)
    pub certificate_identity: Option<String>,
    /// Expected certificate OIDC issuer (exact match)
    pub certificate_oidc_issuer: Option<String>,
    /// Sign or verify with a key pair instead of keyless Sigstore
    pub key: Option<String>,
    /// Signature wire format: bundle (default), simplesigning, or both
    pub signature_format: Option<String>,
    /// Trust-root override: a Sigstore trusted-root JSON (or a directory holding
    /// trusted_root.json), named by a bare path or a file:// one
    pub trusted_root: Option<PathBuf>,
    /// Rekor transparency-log endpoint
    pub rekor_url: Option<String>,
    /// Bypass the referrers-capability cache for this invocation
    pub no_cache: bool,
    /// Require a verified signature; refuse unsigned attachments
    pub verify: bool,
    /// List documents without verifying anything
    pub no_verify: bool,
    /// Package identifier to read (`registry/repo:tag\[@digest\]`)
    pub identifier: String,
}

impl PackageSbomArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("output", &self.output, Presence::Optional, &[]);
        argv.switch("summary", self.summary);
        argv.flag("type", &self.predicate_type, Presence::Optional, &[]);
        argv.flag(
            "certificate-identity",
            &self.certificate_identity,
            Presence::Optional,
            &[],
        );
        argv.flag(
            "certificate-oidc-issuer",
            &self.certificate_oidc_issuer,
            Presence::Optional,
            &[],
        );
        argv.flag("key", &self.key, Presence::Optional, &[]);
        argv.flag(
            "signature-format",
            &self.signature_format,
            Presence::Optional,
            &["bundle", "simplesigning", "both"],
        );
        argv.flag("sigstore-trusted-root", &self.trusted_root, Presence::Optional, &[]);
        argv.flag("rekor-url", &self.rekor_url, Presence::Optional, &[]);
        argv.switch("no-cache", self.no_cache);
        argv.switch("verify", self.verify);
        argv.switch("no-verify", self.no_verify);
        argv.positional("identifier", [&self.identifier], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package select`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageSelectArgs {
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Package identifiers to select
    pub packages: Vec<String>,
}

impl PackageSelectArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package sign`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageSignArgs {
    /// Narrow into one platform of an image index
    pub platform: Option<String>,
    /// Fulcio CA endpoint (the keyless certificate issuer)
    pub fulcio_url: Option<String>,
    /// Rekor transparency-log endpoint
    pub rekor_url: Option<String>,
    /// Read the OIDC identity token from this file (highest precedence)
    pub identity_token_file: Option<PathBuf>,
    /// Read the OIDC identity token from stdin (second precedence)
    pub identity_token_stdin: Option<Secret>,
    /// Suppress the interactive browser OAuth fallback (CI / headless)
    pub no_tty: bool,
    /// Bypass the referrers-capability cache for this invocation
    pub no_cache: bool,
    /// Signature wire format: bundle (default), simplesigning, or both
    pub signature_format: Option<String>,
    /// Sign or verify with a key pair instead of keyless Sigstore
    pub key: Option<String>,
    /// Record the signature in the Rekor transparency log
    pub rekor_upload: bool,
    /// Skip the Rekor entry
    pub no_rekor_upload: bool,
    /// Tags to sweep. Repeatable, and accepts a comma-separated list
    pub tags: Option<String>,
    /// Read tags from a file, one per line or comma-separated
    pub tags_file: Option<PathBuf>,
    /// Package identifier to sign (`registry/repo:tag\[@digest\]`)
    pub identifier: String,
}

impl PackageSignArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("fulcio-url", &self.fulcio_url, Presence::Optional, &[]);
        argv.flag("rekor-url", &self.rekor_url, Presence::Optional, &[]);
        argv.flag(
            "identity-token-file",
            &self.identity_token_file,
            Presence::Optional,
            &[],
        );
        argv.secret("identity-token-stdin", self.identity_token_stdin.as_ref());
        argv.switch("no-tty", self.no_tty);
        argv.switch("no-cache", self.no_cache);
        argv.flag(
            "signature-format",
            &self.signature_format,
            Presence::Optional,
            &["bundle", "simplesigning", "both"],
        );
        argv.flag("key", &self.key, Presence::Optional, &[]);
        argv.switch("rekor-upload", self.rekor_upload);
        argv.switch("no-rekor-upload", self.no_rekor_upload);
        argv.flag("tags", &self.tags, Presence::Optional, &[]);
        argv.flag("tags-file", &self.tags_file, Presence::Optional, &[]);
        argv.positional("identifier", [&self.identifier], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package test`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageTestArgs {
    /// Path to the package metadata JSON file. Defaults to a sibling of the first file layer (e.g.
    /// `pkg.tar.gz` -> `pkg-metadata.json`). Required when no file layers are provided
    pub metadata: Option<PathBuf>,
    /// Target platform (e.g. `linux/amd64`). An explicit value is used as given. Omit it to take
    /// the platform the build receipt beside the bundle recorded; a usage error (exit 64) when
    /// neither names one. Parity with `package push`
    pub platform: Option<String>,
    /// Materialize into DIR instead of an auto-managed temp dir. DIR must not exist (created by
    /// ocx) or be empty. Implies keep - the dir is never deleted by ocx. Must reside on the same
    /// filesystem as `$OCX_HOME/layers/` - hardlink assembly does not fall back to copy
    pub output: Option<PathBuf>,
    /// Preserve the temp build directory after the command exits. Path is printed to stderr.
    /// Default temp root is `$OCX_HOME/temp/test/`
    pub keep: bool,
    /// Compose the package's private env surface (default: interface surface). Same semantics as
    /// `ocx exec --self` / `ocx env --self`
    pub self_view: bool,
    /// Strip ambient parent env before composing - only `OCX_*` config and composed package vars
    /// reach the child. Mirrors `ocx exec --clean`
    pub clean: bool,
    /// Set an environment variable for this invocation, or pass one by name
    pub env: Option<String>,
    /// Identifier under which the package is materialized. Tag form (`repo:tag`) only; an explicit
    /// `@digest` is rejected (the digest is computed locally during this command and supplying one
    /// would conflict)
    pub identifier: String,
    /// Layers, in order (base first, top last). Same syntax as `package push`: either a path to a
    /// `.tar.gz`/`.tar.xz`/`.tar.zst` archive, or `sha256:<hex>.<ext>` referring to a layer already
    /// present in the target registry. Digest refs are auto-pulled from the registry on demand; in
    /// `--offline`, missing digest blobs error with `PolicyBlocked`
    pub layers: Vec<String>,
    /// Path to a Starlark test script. Mutually exclusive with the trailing command. When given,
    /// the materialized package env is interpreted by the embedded engine instead of exec'ing a
    /// command
    pub script: Option<PathBuf>,
    /// Write a JUnit XML report for the scripted run to PATH
    pub junit: Option<PathBuf>,
    /// Command to execute inside the composed env, with arguments. Required unless `--script` is
    /// given (exactly one of the two forms must be supplied)
    pub command: Vec<String>,
}

impl PackageTestArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("metadata", &self.metadata, Presence::Optional, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("output", &self.output, Presence::Optional, &[]);
        argv.switch("keep", self.keep);
        argv.switch("self", self.self_view);
        argv.switch("clean", self.clean);
        argv.flag("env", &self.env, Presence::Optional, &[]);
        argv.flag("identifier", [&self.identifier], Presence::Required, &[]);
        argv.positional("layers", &self.layers, Presence::Optional, Hyphen::Refuse);
        argv.flag("script", &self.script, Presence::Optional, &[]);
        argv.flag("junit", &self.junit, Presence::Optional, &[]);
        argv.operand("command", &self.command, Presence::Optional);
    }
}

/// The arguments of `ocx package uninstall`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageUninstallArgs {
    /// Also remove the current symlink (equivalent to also running `ocx deselect`)
    pub deselect: bool,
    /// Delete the object from the store when no references remain after uninstall
    pub purge: bool,
    /// Package identifiers to uninstall
    pub packages: Vec<String>,
}

impl PackageUninstallArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("deselect", self.deselect);
        argv.switch("purge", self.purge);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package verify`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageVerifyArgs {
    /// Narrow into one platform of an image index
    pub platform: Option<String>,
    /// Expected certificate SAN (exact match)
    pub certificate_identity: Option<String>,
    /// Expected certificate OIDC issuer (exact match)
    pub certificate_oidc_issuer: Option<String>,
    /// Sign or verify with a key pair instead of keyless Sigstore
    pub key: Option<String>,
    /// Signature wire format: bundle (default), simplesigning, or both
    pub signature_format: Option<String>,
    /// Rekor transparency-log endpoint
    pub rekor_url: Option<String>,
    /// Verify a signed in-toto attestation instead of an artifact signature
    pub attestation: bool,
    /// Restrict to one predicate type (for example cyclonedx or spdx)
    pub predicate_type: Option<String>,
    /// Accept a keyless cosign sidecar that carries no transparency-log entry
    pub allow_unlogged_signature: bool,
    /// Bypass the referrers-capability cache for this invocation
    pub no_cache: bool,
    /// Trust-root override: a Sigstore trusted-root JSON (or a directory holding
    /// trusted_root.json), named by a bare path or a file:// one
    pub trusted_root: Option<PathBuf>,
    /// Package identifier to verify (`registry/repo:tag\[@digest\]`)
    pub identifier: String,
}

impl PackageVerifyArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag(
            "certificate-identity",
            &self.certificate_identity,
            Presence::Optional,
            &[],
        );
        argv.flag(
            "certificate-oidc-issuer",
            &self.certificate_oidc_issuer,
            Presence::Optional,
            &[],
        );
        argv.flag("key", &self.key, Presence::Optional, &[]);
        argv.flag(
            "signature-format",
            &self.signature_format,
            Presence::Optional,
            &["bundle", "simplesigning", "both"],
        );
        argv.flag("rekor-url", &self.rekor_url, Presence::Optional, &[]);
        argv.switch("attestation", self.attestation);
        argv.flag("type", &self.predicate_type, Presence::Optional, &[]);
        argv.switch("allow-unlogged-signature", self.allow_unlogged_signature);
        argv.switch("no-cache", self.no_cache);
        argv.flag("sigstore-trusted-root", &self.trusted_root, Presence::Optional, &[]);
        argv.positional("identifier", [&self.identifier], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx package which`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageWhichArgs {
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Resolve the content path via the installed candidate symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/candidates/<tag>`)
    pub candidate: bool,
    /// Resolve the content path via the current-selected symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/current`)
    pub current: bool,
    /// Resolve the content path via a link written by `ocx package install --link <PATH>`
    pub link: Option<PathBuf>,
    /// Control when a package's content downloads: now, or on first use
    pub lazy_mode: Option<String>,
    /// Package identifiers to resolve
    pub packages: Vec<String>,
}

impl PackageWhichArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.switch("candidate", self.candidate);
        argv.switch("current", self.current);
        argv.flag("link", &self.link, Presence::Optional, &[]);
        argv.flag("lazy-mode", &self.lazy_mode, Presence::Optional, &["never", "always"]);
        argv.positional("packages", &self.packages, Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx patch publish`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatchPublishArgs {
    /// Path to the patch descriptor JSON file to publish
    pub descriptor: PathBuf,
    /// Publish the descriptor as the global descriptor so it applies to every base. Stored at the
    /// reserved `global` repository in the patch registry. Mutually exclusive with a base
    /// identifier
    pub global: bool,
    /// Base identifier whose package-specific patch path receives the descriptor. Omit with
    /// `--global` for the global descriptor
    pub base: Option<String>,
    /// Patch registry to publish to, as `HOST/PATH` (e.g. registry.corp.example/ocx-patches)
    pub registry: Option<String>,
}

impl PatchPublishArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("descriptor", [&self.descriptor], Presence::Required, &[]);
        argv.switch("global", self.global);
        argv.positional("base", &self.base, Presence::Optional, Hyphen::Refuse);
        argv.flag("registry", &self.registry, Presence::Optional, &[]);
    }
}

/// The arguments of `ocx patch sync`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatchSyncArgs {
    /// Target platform to resolve packages against
    pub platform: Option<String>,
}

impl PatchSyncArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
    }
}

/// The arguments of `ocx patch test`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatchTestArgs {
    /// Path to the patch descriptor JSON file to compose
    pub descriptor: PathBuf,
    /// Target platform for composing the environment. Defaults to the host platform
    pub platform: Option<String>,
    /// Use the base's own surface, the one its launchers see, instead of its consumers'
    pub self_view: bool,
    /// Path to a local archive for a companion package, allowing the companion to be materialized
    /// without a registry round-trip. Repeatable
    pub companion_archives: Option<PathBuf>,
    /// Base identifier to compose the descriptor onto
    pub base: String,
    /// Patch registry to compose against, as `HOST/PATH` (e.g. registry.corp.example/ocx-patches)
    pub registry: Option<String>,
    /// Path to a Starlark test script to run in the composed environment. Mutually exclusive with a
    /// trailing command
    pub script: Option<PathBuf>,
    /// Set an environment variable for this invocation, or pass one by name
    pub env: Option<String>,
    /// Command to run in the composed environment, after `--`. Mutually exclusive with `--script`.
    /// When neither is given, the composed environment is printed
    pub command: Vec<String>,
}

impl PatchTestArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("descriptor", [&self.descriptor], Presence::Required, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.switch("self", self.self_view);
        argv.flag("companion-archive", &self.companion_archives, Presence::Optional, &[]);
        argv.positional("base", [&self.base], Presence::Required, Hyphen::Refuse);
        argv.flag("registry", &self.registry, Presence::Optional, &[]);
        argv.flag("script", &self.script, Presence::Optional, &[]);
        argv.flag("env", &self.env, Presence::Optional, &[]);
        argv.operand("command", &self.command, Presence::Optional);
    }
}

/// The arguments of `ocx patch why`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatchWhyArgs {
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Use the base's own surface, the one its launchers see, instead of its consumers'
    pub self_view: bool,
    /// Base identifier to trace patch provenance for
    pub base: String,
}

impl PatchWhyArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.switch("self", self.self_view);
        argv.positional("base", [&self.base], Presence::Required, Hyphen::Refuse);
    }
}

/// The arguments of `ocx pull`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PullArgs {
    /// Preview which locked packages are cached vs. would be fetched
    pub dry_run: bool,
    /// Restrict the pull to the named group(s)
    pub groups: Option<String>,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Control when a package's content downloads: now, or on first use
    pub lazy_mode: Option<String>,
    /// Record a consent stamp (the default unless OCX_NO_CONSENT is set)
    pub consent: bool,
    /// Run without consenting to this project's shell activation
    pub no_consent: bool,
}

impl PullArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("dry-run", self.dry_run);
        argv.flag("group", &self.groups, Presence::Optional, &[]);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("lazy-mode", &self.lazy_mode, Presence::Optional, &["never", "always"]);
        argv.switch("consent", self.consent);
        argv.switch("no-consent", self.no_consent);
    }
}

/// The arguments of `ocx remove`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoveArgs {
    /// Bindings to remove (binding name or fully-qualified identifier, e.g. `cmake` or
    /// `ocx.sh/cmake:3.28`)
    pub identifiers: Vec<String>,
    /// Target a specific group. Use `default` to target the implicit `\[tools\]` table, or a named
    /// group (e.g. `ci`) to target `\[group.ci\]`. Without this flag, all groups are searched; if
    /// the binding appears in more than one group an error is returned
    pub group: Option<String>,
}

impl RemoveArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("identifiers", &self.identifiers, Presence::Required, Hyphen::Refuse);
        argv.flag("group", &self.group, Presence::Optional, &[]);
    }
}

/// The arguments of `ocx self activate`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelfActivateArgs {
    /// Target shell for activation output
    pub shell: Option<String>,
    /// Force shell-completion injection on, regardless of session interactivity
    pub completion: bool,
    /// Force shell-completion injection off
    pub no_completion: bool,
    /// Force the per-prompt hook on, regardless of session interactivity
    pub hook: bool,
    /// Force the per-prompt hook off
    pub no_hook: bool,
}

impl SelfActivateArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.flag(
            "shell",
            &self.shell,
            Presence::Optional,
            &[
                "ash",
                "ksh",
                "dash",
                "bash",
                "elvish",
                "fish",
                "batch",
                "powershell",
                "zsh",
                "nushell",
            ],
        );
        argv.switch("completion", self.completion);
        argv.switch("no-completion", self.no_completion);
        argv.switch("hook", self.hook);
        argv.switch("no-hook", self.no_hook);
    }
}

/// The arguments of `ocx self setup`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelfSetupArgs {
    /// Turn the per-prompt shell hook on: writes `\[shell\] hook = true`
    pub hook: bool,
    /// Turn the per-prompt shell hook off: writes `\[shell\] hook = false`
    pub no_hook: bool,
    /// Turn shell completions on: writes `\[shell\] completions = true`
    pub completion: bool,
    /// Turn shell completions off: writes `\[shell\] completions = false`
    pub no_completion: bool,
    /// Record how a toolchain reaches PATH: writes `activate` to `$OCX_HOME/ocx.toml`
    pub toolchain_activate: Option<String>,
    /// Version to install: tag, `sha256:<hex>`, or `tag@sha256:<hex>`
    pub version: Option<String>,
    /// Write the env shims but touch neither a shell profile nor the session PATH
    pub no_modify_path: bool,
    /// Target an explicit profile file. Repeatable
    pub profile: Option<PathBuf>,
    /// Write no profile blocks at all
    pub no_profile: bool,
    /// Report the intended actions without writing anything
    pub dry_run: bool,
    /// Overwrite a managed block that carries user edits (the dirty state)
    pub force: bool,
    /// Adopt (or clear) the corporate managed-config tier
    pub managed_config: Option<String>,
}

impl SelfSetupArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("hook", self.hook);
        argv.switch("no-hook", self.no_hook);
        argv.switch("completion", self.completion);
        argv.switch("no-completion", self.no_completion);
        argv.flag(
            "toolchain-activate",
            &self.toolchain_activate,
            Presence::Optional,
            &["env", "bin", "none"],
        );
        argv.positional("version", &self.version, Presence::Optional, Hyphen::Refuse);
        argv.switch("no-modify-path", self.no_modify_path);
        argv.flag("profile", &self.profile, Presence::Optional, &[]);
        argv.switch("no-profile", self.no_profile);
        argv.switch("dry-run", self.dry_run);
        argv.switch("force", self.force);
        argv.flag("managed-config", &self.managed_config, Presence::Optional, &[]);
    }
}

/// The arguments of `ocx self update`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelfUpdateArgs {
    /// Check for a newer ocx version without installing it
    pub check: bool,
}

impl SelfUpdateArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("check", self.check);
    }
}

/// The arguments of `ocx shell allow`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellAllowArgs {
    /// The directory whose project to consent to (default: the current one)
    pub path: Option<PathBuf>,
}

impl ShellAllowArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("path", &self.path, Presence::Optional, Hyphen::Refuse);
    }
}

/// The arguments of `ocx shell completion`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellCompletionArgs {
    /// Print nothing when completions are switched off for this session
    pub if_enabled: bool,
    /// The shell to generate the completions for: bash, elvish, fish, powershell or zsh
    pub shell: Option<String>,
}

impl ShellCompletionArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("if-enabled", self.if_enabled);
        argv.flag(
            "shell",
            &self.shell,
            Presence::Optional,
            &[
                "ash",
                "ksh",
                "dash",
                "bash",
                "elvish",
                "fish",
                "batch",
                "powershell",
                "zsh",
                "nushell",
            ],
        );
    }
}

/// The arguments of `ocx shell revoke`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellRevokeArgs {
    /// The directory whose project to revoke (default: the current one)
    pub path: Option<PathBuf>,
}

impl ShellRevokeArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.positional("path", &self.path, Presence::Optional, Hyphen::Refuse);
    }
}

/// The arguments of `ocx shell state`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellStateArgs {
    /// Add the diagnostics behind the answer - the decoded ledger, the fingerprint watch set and
    /// the hook ladder
    pub verbose: bool,
}

impl ShellStateArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("verbose", self.verbose);
    }
}

/// The arguments of `ocx status`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusArgs {
    /// Show each binding's host digest and locked platform count
    pub verbose: bool,
}

impl StatusArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("verbose", self.verbose);
    }
}

/// The arguments of `ocx update`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpdateArgs {
    /// Verify the candidate lock would match the predecessor and exit
    pub check: bool,
    /// List the bindings that did not move as well as the ones that did
    pub verbose: bool,
    /// Materialize resolved packages into the object store, installing on a local miss
    pub pull: bool,
    /// Skip materialization; resolve against local state only. Materialization is deferred to `ocx
    /// pull` or the first `ocx exec` / direnv hit
    pub no_pull: bool,
    /// Target platform to resolve packages against
    pub platform: Option<String>,
    /// Advance every binding in the named group(s); freeze the rest
    pub groups: Option<String>,
    /// Binding names to advance; freeze every other pin
    pub names: Vec<String>,
}

impl UpdateArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("check", self.check);
        argv.switch("verbose", self.verbose);
        argv.switch("pull", self.pull);
        argv.switch("no-pull", self.no_pull);
        argv.flag("platform", &self.platform, Presence::Optional, &[]);
        argv.flag("group", &self.groups, Presence::Optional, &[]);
        argv.positional("names", &self.names, Presence::Optional, Hyphen::Refuse);
    }
}

/// The arguments of `ocx upgrade`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpgradeArgs {
    /// Report the tags that would move and exit without writing
    pub check: bool,
    /// Allow a tag to move to a newer major version
    pub major: bool,
    /// List the bindings that were left alone as well as the ones that moved
    pub verbose: bool,
    /// Upgrade every binding in the named group(s); leave the rest
    pub groups: Option<String>,
    /// Binding names to upgrade; leave every other binding as declared
    pub names: Vec<String>,
}

impl UpgradeArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("check", self.check);
        argv.switch("major", self.major);
        argv.switch("verbose", self.verbose);
        argv.flag("group", &self.groups, Presence::Optional, &[]);
        argv.positional("names", &self.names, Presence::Optional, Hyphen::Refuse);
    }
}

/// The arguments of `ocx version`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VersionArgs {
    /// Emit enriched build provenance - commit, dirty flag, build time, target, rustc, CI run URL.
    /// JSON output always includes the populated subset; this flag only affects plain text
    pub verbose: bool,
}

impl VersionArgs {
    /// Appends the arguments to `argv`.
    pub fn push(&self, argv: &mut Argv) {
        argv.switch("verbose", self.verbose);
    }
}

impl Ocx {
    /// Print ocx version, registry, platform, shell, and home directory
    ///
    /// Runs `ocx about`.
    pub fn about(&self) -> Result<About, Error> {
        let argv = self.argv(&["about"]);
        self.call("about", "About", argv)
    }

    /// Add one or more package bindings to ocx.toml
    ///
    /// Runs `ocx add`.
    pub fn add(&self, args: &AddArgs) -> Result<LockReport, Error> {
        let mut argv = self.argv(&["add"]);
        args.push(&mut argv);
        self.call("add", "LockReport", argv)
    }

    /// Remove unreferenced objects from the local object store
    ///
    /// Runs `ocx clean`.
    pub fn clean(&self, args: &CleanArgs) -> Result<Clean, Error> {
        let mut argv = self.argv(&["clean"]);
        args.push(&mut argv);
        self.call("clean", "Clean", argv)
    }

    /// Publish a config file as a managed-config package
    ///
    /// Runs `ocx config push`.
    pub fn config_push(&self, args: &ConfigPushArgs) -> Result<PushReport, Error> {
        let mut argv = self.argv(&["config", "push"]);
        args.push(&mut argv);
        self.call("config push", "PushReport", argv)
    }

    /// Adopt (or clear) the corporate managed-config tier
    ///
    /// Runs `ocx config setup`.
    pub fn config_setup(&self, args: &ConfigSetupArgs) -> Result<ConfigSetupData, Error> {
        let mut argv = self.argv(&["config", "setup"]);
        args.push(&mut argv);
        self.call("config setup", "ConfigSetupData", argv)
    }

    /// Check a config file and preview the configuration it would produce
    ///
    /// Runs `ocx config test`.
    pub fn config_test(&self, args: &ConfigTestArgs) -> Result<ConfigTestData, Error> {
        let mut argv = self.argv(&["config", "test"]);
        args.push(&mut argv);
        self.call("config test", "ConfigTestData", argv)
    }

    /// Refresh the managed-config snapshot from the registry
    ///
    /// Runs `ocx config update`.
    pub fn config_update(&self, args: &ConfigUpdateArgs) -> Result<ConfigUpdateData, Error> {
        let mut argv = self.argv(&["config", "update"]);
        args.push(&mut argv);
        self.call("config update", "ConfigUpdateData", argv)
    }

    /// Print stateless shell exports for the project toolchain (direnv entry point)
    ///
    /// Runs `ocx direnv export`.
    pub fn direnv_export(&self, args: &DirenvExportArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["direnv", "export"]);
        args.push(&mut argv);
        self.call_raw("direnv export", &[], argv)
    }

    /// Write a `.envrc` wiring `ocx direnv export` into direnv
    ///
    /// Runs `ocx direnv init`.
    pub fn direnv_init(&self, args: &DirenvInitArgs) -> Result<(), Error> {
        let mut argv = self.argv(&["direnv", "init"]);
        args.push(&mut argv);
        self.call_empty("direnv init", argv)
    }

    /// Compose and print the toolchain environment
    ///
    /// Runs `ocx env`.
    pub fn env(&self, args: &EnvArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["env"]);
        args.push(&mut argv);
        self.call_raw("env", &["EnvVars"], argv)
    }

    /// Run a command with the composed environment from the project toolchain
    ///
    /// Runs `ocx exec`.
    pub fn exec(&self, args: &ExecArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["exec"]);
        args.push(&mut argv);
        self.call_raw("exec", &[], argv)
    }

    /// List available repositories in the registry
    ///
    /// Runs `ocx index catalog`.
    pub fn index_catalog(&self, args: &IndexCatalogArgs) -> Result<Catalog, Error> {
        let mut argv = self.argv(&["index", "catalog"]);
        args.push(&mut argv);
        self.call("index catalog", "Catalog", argv)
    }

    /// List available versions of a package
    ///
    /// Runs `ocx index list`.
    pub fn index_list(&self, args: &IndexListArgs) -> Result<Tags, Error> {
        let mut argv = self.argv(&["index", "list"]);
        args.push(&mut argv);
        self.call("index list", "Tags", argv)
    }

    /// Rebuild a local index source's catalog from the root documents on disk
    ///
    /// Runs `ocx index regenerate`.
    pub fn index_regenerate(&self, args: &IndexRegenerateArgs) -> Result<RegenerateReport, Error> {
        let mut argv = self.argv(&["index", "regenerate"]);
        args.push(&mut argv);
        self.call("index regenerate", "RegenerateReport", argv)
    }

    /// Refresh every package one or more registries' catalogs list
    ///
    /// Runs `ocx index sync`.
    pub fn index_sync(&self, args: &IndexSyncArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["index", "sync"]);
        args.push(&mut argv);
        self.call_raw("index sync", &["CatalogPreview"], argv)
    }

    /// Refresh the local index for one or more packages
    ///
    /// Runs `ocx index update`.
    pub fn index_update(&self, args: &IndexUpdateArgs) -> Result<(), Error> {
        let mut argv = self.argv(&["index", "update"]);
        args.push(&mut argv);
        self.call_empty("index update", argv)
    }

    /// Create a minimal ocx.toml in the current directory
    ///
    /// Runs `ocx init`.
    pub fn init(&self, args: &InitArgs) -> Result<(), Error> {
        let mut argv = self.argv(&["init"]);
        args.push(&mut argv);
        self.call_empty("init", argv)
    }

    /// Inspect what the project toolchain resolves to, without installing
    ///
    /// Runs `ocx inspect`.
    pub fn inspect(&self, args: &InspectArgs) -> Result<Outcome<InspectReport>, Error> {
        let mut argv = self.argv(&["inspect"]);
        args.push(&mut argv);
        self.call_outcome("inspect", "InspectReport", argv)
    }

    /// Execute an installed package entrypoint from a generated launcher
    ///
    /// Runs `ocx launcher exec`.
    pub fn launcher_exec(&self, args: &LauncherExecArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["launcher", "exec"]);
        args.push(&mut argv);
        self.call_raw("launcher exec", &[], argv)
    }

    /// Materialize a deferred package and run one of its declared names
    ///
    /// Runs `ocx launcher shim`.
    pub fn launcher_shim(&self, args: &LauncherShimArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["launcher", "shim"]);
        args.push(&mut argv);
        self.call_raw("launcher shim", &[], argv)
    }

    /// Resolve package tags to digests and write ocx.lock
    ///
    /// Runs `ocx lock`.
    pub fn lock(&self, args: &LockArgs) -> Result<LockReport, Error> {
        let mut argv = self.argv(&["lock"]);
        args.push(&mut argv);
        self.call("lock", "LockReport", argv)
    }

    /// Authenticate to a registry and persist credentials
    ///
    /// Runs `ocx login`.
    pub fn login(&self, args: &LoginArgs) -> Result<LoginResult, Error> {
        let mut argv = self.argv(&["login"]);
        args.push(&mut argv);
        self.call("login", "LoginResult", argv)
    }

    /// Remove credentials for a registry
    ///
    /// Runs `ocx logout`.
    pub fn logout(&self, args: &LogoutArgs) -> Result<LogoutResult, Error> {
        let mut argv = self.argv(&["logout"]);
        args.push(&mut argv);
        self.call("logout", "LogoutResult", argv)
    }

    /// Observe an owner-curated set of registry tags and publish the rebuilt entry into the index
    ///
    /// Runs `ocx package announce`.
    pub fn package_announce(&self, args: &PackageAnnounceArgs) -> Result<AnnounceReport, Error> {
        let mut argv = self.argv(&["package", "announce"]);
        args.push(&mut argv);
        self.call("package announce", "AnnounceReport", argv)
    }

    /// Attach an in-toto attestation to a published package manifest
    ///
    /// Runs `ocx package attest`.
    pub fn package_attest(&self, args: &PackageAttestArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["package", "attest"]);
        args.push(&mut argv);
        self.call_raw(
            "package attest",
            &["AttestationReport", "SweepReport<AttestationReport>"],
            argv,
        )
    }

    /// Report where a package's rolling tags disagree with its versions
    ///
    /// Runs `ocx package cascade check`.
    pub fn package_cascade_check(&self, args: &PackageCascadeCheckArgs) -> Result<Outcome<PackageCascadeCheck>, Error> {
        let mut argv = self.argv(&["package", "cascade", "check"]);
        args.push(&mut argv);
        self.call_outcome("package cascade check", "PackageCascadeCheck", argv)
    }

    /// Re-point a package's rolling tags at the content its versions imply
    ///
    /// Runs `ocx package cascade repair`.
    pub fn package_cascade_repair(
        &self,
        args: &PackageCascadeRepairArgs,
    ) -> Result<Outcome<PackageCascadeRepair>, Error> {
        let mut argv = self.argv(&["package", "cascade", "repair"]);
        args.push(&mut argv);
        self.call_outcome("package cascade repair", "PackageCascadeRepair", argv)
    }

    /// Claim a package in the index so its tags can be announced
    ///
    /// Runs `ocx package claim`.
    pub fn package_claim(&self, args: &PackageClaimArgs) -> Result<ClaimReport, Error> {
        let mut argv = self.argv(&["package", "claim"]);
        args.push(&mut argv);
        self.call("package claim", "ClaimReport", argv)
    }

    /// Promote an already-published package to another registry or repository
    ///
    /// Runs `ocx package copy`.
    pub fn package_copy(&self, args: &PackageCopyArgs) -> Result<Outcome<CopyReport>, Error> {
        let mut argv = self.argv(&["package", "copy"]);
        args.push(&mut argv);
        self.call_outcome("package copy", "CopyReport", argv)
    }

    /// Creates an archive from a local package directory
    ///
    /// Runs `ocx package create`.
    pub fn package_create(&self, args: &PackageCreateArgs) -> Result<(), Error> {
        let mut argv = self.argv(&["package", "create"]);
        args.push(&mut argv);
        self.call_empty("package create", argv)
    }

    /// Show the dependency tree for one or more installed packages
    ///
    /// Runs `ocx package deps`.
    pub fn package_deps(&self, args: &PackageDepsArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["package", "deps"]);
        args.push(&mut argv);
        self.call_raw(
            "package deps",
            &["Dependencies", "DependenciesTrace", "FlatDependencies"],
            argv,
        )
    }

    /// Show description metadata (title, description, keywords) for one or more packages
    ///
    /// Runs `ocx package description pull`.
    pub fn package_description_pull(&self, args: &PackageDescriptionPullArgs) -> Result<PackageDescriptions, Error> {
        let mut argv = self.argv(&["package", "description", "pull"]);
        args.push(&mut argv);
        self.call("package description pull", "PackageDescriptions", argv)
    }

    /// Push or update description metadata for a package repository
    ///
    /// Runs `ocx package description push`.
    pub fn package_description_push(&self, args: &PackageDescriptionPushArgs) -> Result<(), Error> {
        let mut argv = self.argv(&["package", "description", "push"]);
        args.push(&mut argv);
        self.call_empty("package description push", argv)
    }

    /// Remove the current-version symlink for one or more packages
    ///
    /// Runs `ocx package deselect`.
    pub fn package_deselect(&self, args: &PackageDeselectArgs) -> Result<Removed, Error> {
        let mut argv = self.argv(&["package", "deselect"]);
        args.push(&mut argv);
        self.call("package deselect", "Removed", argv)
    }

    /// Print the resolved environment variables for one or more installed packages
    ///
    /// Runs `ocx package env`.
    pub fn package_env(&self, args: &PackageEnvArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["package", "env"]);
        args.push(&mut argv);
        self.call_raw("package env", &["EnvVars"], argv)
    }

    /// Runs installed packages
    ///
    /// Runs `ocx package exec`.
    pub fn package_exec(&self, args: &PackageExecArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["package", "exec"]);
        args.push(&mut argv);
        self.call_raw("package exec", &[], argv)
    }

    /// Inspect one or more package references (candidates, metadata, or resolution chain)
    ///
    /// Runs `ocx package inspect`.
    pub fn package_inspect(&self, args: &PackageInspectArgs) -> Result<Outcome<InspectReport>, Error> {
        let mut argv = self.argv(&["package", "inspect"]);
        args.push(&mut argv);
        self.call_outcome("package inspect", "InspectReport", argv)
    }

    /// Install packages from a local or remote index (no `ocx.toml` touched)
    ///
    /// Runs `ocx package install`.
    pub fn package_install(&self, args: &PackageInstallArgs) -> Result<Installs, Error> {
        let mut argv = self.argv(&["package", "install"]);
        args.push(&mut argv);
        self.call("package install", "Installs", argv)
    }

    /// Delete tags from a package's registry repository, guarded by the index
    ///
    /// Runs `ocx package prune`.
    pub fn package_prune(&self, args: &PackagePruneArgs) -> Result<Outcome<PruneOutcome>, Error> {
        let mut argv = self.argv(&["package", "prune"]);
        args.push(&mut argv);
        self.call_outcome("package prune", "PackagePrune", argv)
    }

    /// Downloads packages into the local object store without creating install symlinks
    ///
    /// Runs `ocx package pull`.
    pub fn package_pull(&self, args: &PackagePullArgs) -> Result<Paths, Error> {
        let mut argv = self.argv(&["package", "pull"]);
        args.push(&mut argv);
        self.call("package pull", "Paths", argv)
    }

    /// Publish a package's layers and metadata to a registry
    ///
    /// Runs `ocx package push`.
    pub fn package_push(&self, args: &PackagePushArgs) -> Result<Outcome<PushReport>, Error> {
        let mut argv = self.argv(&["package", "push"]);
        args.push(&mut argv);
        self.call_outcome("package push", "PushReport", argv)
    }

    /// Print the build receipt `ocx package create` wrote beside a bundle
    ///
    /// Runs `ocx package receipt`.
    pub fn package_receipt(&self, args: &PackageReceiptArgs) -> Result<PackageReceipt, Error> {
        let mut argv = self.argv(&["package", "receipt"]);
        args.push(&mut argv);
        self.call("package receipt", "PackageReceipt", argv)
    }

    /// List or extract the SBOM attestations a published package carries
    ///
    /// Runs `ocx package sbom`.
    pub fn package_sbom(&self, args: &PackageSbomArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["package", "sbom"]);
        args.push(&mut argv);
        self.call_raw("package sbom", &["SbomListingReport"], argv)
    }

    /// Set the current version of one or more packages
    ///
    /// Runs `ocx package select`.
    pub fn package_select(&self, args: &PackageSelectArgs) -> Result<Installs, Error> {
        let mut argv = self.argv(&["package", "select"]);
        args.push(&mut argv);
        self.call("package select", "Installs", argv)
    }

    /// Sign a published package's manifest (keyless Sigstore, via OCI Referrers)
    ///
    /// Runs `ocx package sign`.
    pub fn package_sign(&self, args: &PackageSignArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["package", "sign"]);
        args.push(&mut argv);
        self.call_raw(
            "package sign",
            &["SignatureReport", "SweepReport<SignatureReport>"],
            argv,
        )
    }

    /// Materialize a package locally (no registry round-trip) and run a command in its env
    ///
    /// Runs `ocx package test`.
    pub fn package_test(&self, args: &PackageTestArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["package", "test"]);
        args.push(&mut argv);
        self.call_raw("package test", &["ScriptRunReport"], argv)
    }

    /// Remove an installed candidate for one or more packages
    ///
    /// Runs `ocx package uninstall`.
    pub fn package_uninstall(&self, args: &PackageUninstallArgs) -> Result<Removed, Error> {
        let mut argv = self.argv(&["package", "uninstall"]);
        args.push(&mut argv);
        self.call("package uninstall", "Removed", argv)
    }

    /// Verify a published package's Sigstore signature (keyless, via OCI Referrers)
    ///
    /// Runs `ocx package verify`.
    pub fn package_verify(&self, args: &PackageVerifyArgs) -> Result<VerificationReport, Error> {
        let mut argv = self.argv(&["package", "verify"]);
        args.push(&mut argv);
        self.call("package verify", "VerificationReport", argv)
    }

    /// Resolve installed packages and print their package-root (or, with `--candidate`/`--current`,
    /// install-symlink) paths
    ///
    /// Runs `ocx package which`.
    pub fn package_which(&self, args: &PackageWhichArgs) -> Result<LocatedPaths, Error> {
        let mut argv = self.argv(&["package", "which"]);
        args.push(&mut argv);
        self.call("package which", "LocatedPaths", argv)
    }

    /// Freeze companion package digests to a snapshot for reproducible builds
    ///
    /// Runs `ocx patch freeze`.
    pub fn patch_freeze(&self) -> Result<PatchFreezeReport, Error> {
        let argv = self.argv(&["patch", "freeze"]);
        self.call("patch freeze", "PatchFreezeReport", argv)
    }

    /// Publish a patch descriptor to the patch registry
    ///
    /// Runs `ocx patch publish`.
    pub fn patch_publish(&self, args: &PatchPublishArgs) -> Result<PatchPublishReport, Error> {
        let mut argv = self.argv(&["patch", "publish"]);
        args.push(&mut argv);
        self.call("patch publish", "PatchPublishReport", argv)
    }

    /// Refresh patch descriptors and companion packages from the registry
    ///
    /// Runs `ocx patch sync`.
    pub fn patch_sync(&self, args: &PatchSyncArgs) -> Result<PatchSyncReport, Error> {
        let mut argv = self.argv(&["patch", "sync"]);
        args.push(&mut argv);
        self.call("patch sync", "PatchSyncReport", argv)
    }

    /// Compose a patch descriptor onto a base locally, without publishing
    ///
    /// Runs `ocx patch test`.
    pub fn patch_test(&self, args: &PatchTestArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["patch", "test"]);
        args.push(&mut argv);
        self.call_raw("patch test", &["PatchTestReport", "ScriptRunReport"], argv)
    }

    /// Show which companion contributes each patched env var to a base, and by which descriptor
    /// rule
    ///
    /// Runs `ocx patch why`.
    pub fn patch_why(&self, args: &PatchWhyArgs) -> Result<PatchWhyReport, Error> {
        let mut argv = self.argv(&["patch", "why"]);
        args.push(&mut argv);
        self.call("patch why", "PatchWhyReport", argv)
    }

    /// Pre-warm the object store, then render the project toolchain
    ///
    /// Runs `ocx pull`.
    pub fn pull(&self, args: &PullArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["pull"]);
        args.push(&mut argv);
        self.call_raw("pull", &["PullDryRun", "WarmedPaths"], argv)
    }

    /// Remove one or more package bindings from ocx.toml
    ///
    /// Runs `ocx remove`.
    pub fn remove(&self, args: &RemoveArgs) -> Result<LockReport, Error> {
        let mut argv = self.argv(&["remove"]);
        args.push(&mut argv);
        self.call("remove", "LockReport", argv)
    }

    /// Sourced from `$OCX_HOME/env.sh` at shell startup to activate ocx in the current shell.
    /// Prepends `$OCX_HOME/symlinks/.../bin` to `PATH`, injects completions (unless
    /// `OCX_NO_COMPLETION=1`), and evaluates the global toolchain env. Safe to re-source: the PATH
    /// updates are idempotent (move-to-front), so a re-source never duplicates an entry
    ///
    /// Runs `ocx self activate`.
    pub fn self_activate(&self, args: &SelfActivateArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["self", "activate"]);
        args.push(&mut argv);
        self.call_raw("self activate", &[], argv)
    }

    /// Create or refresh ocx shell integration
    ///
    /// Runs `ocx self setup`.
    pub fn self_setup(&self, args: &SelfSetupArgs) -> Result<SelfSetupData, Error> {
        let mut argv = self.argv(&["self", "setup"]);
        args.push(&mut argv);
        self.call("self setup", "SelfSetupData", argv)
    }

    /// Update ocx itself to the latest released version. Without `--check`, downloads the newest
    /// release and activates it. With `--check`, reports the result without installing
    ///
    /// Runs `ocx self update`.
    pub fn self_update(&self, args: &SelfUpdateArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["self", "update"]);
        args.push(&mut argv);
        self.call_raw("self update", &["SelfUpdateData", "UpdateCheckData"], argv)
    }

    /// Consent to a project's shell activation
    ///
    /// Runs `ocx shell allow`.
    pub fn shell_allow(&self, args: &ShellAllowArgs) -> Result<(), Error> {
        let mut argv = self.argv(&["shell", "allow"]);
        args.push(&mut argv);
        self.call_empty("shell allow", argv)
    }

    /// Generate shell completion scripts
    ///
    /// Runs `ocx shell completion`.
    pub fn shell_completion(&self, args: &ShellCompletionArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["shell", "completion"]);
        args.push(&mut argv);
        self.call_raw("shell completion", &[], argv)
    }

    /// Withdraw a project's consent stamp
    ///
    /// Runs `ocx shell revoke`.
    pub fn shell_revoke(&self, args: &ShellRevokeArgs) -> Result<(), Error> {
        let mut argv = self.argv(&["shell", "revoke"]);
        args.push(&mut argv);
        self.call_empty("shell revoke", argv)
    }

    /// Report the shell integration's state, and why it is inert when it is
    ///
    /// Runs `ocx shell state`.
    pub fn shell_state(&self, args: &ShellStateArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["shell", "state"]);
        args.push(&mut argv);
        self.call_raw("shell state", &["ShellStateReport", "VerboseShellState"], argv)
    }

    /// Show what ocx.toml and ocx.lock declare, without resolving anything
    ///
    /// Runs `ocx status`.
    pub fn status(&self, args: &StatusArgs) -> Result<StatusReport, Error> {
        let mut argv = self.argv(&["status"]);
        args.push(&mut argv);
        self.call("status", "StatusReport", argv)
    }

    /// Re-resolve declared tags against the registry; whole file or a subset
    ///
    /// Runs `ocx update`.
    pub fn update(&self, args: &UpdateArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["update"]);
        args.push(&mut argv);
        self.call_raw("update", &["UpdateReport", "VerboseUpdateReport"], argv)
    }

    /// Move declared tags in ocx.toml to the newest published release
    ///
    /// Runs `ocx upgrade`.
    pub fn upgrade(&self, args: &UpgradeArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["upgrade"]);
        args.push(&mut argv);
        self.call_raw("upgrade", &["UpgradeReport", "VerboseUpgradeReport"], argv)
    }

    /// Print the version of ocx
    ///
    /// Runs `ocx version`.
    pub fn version(&self, args: &VersionArgs) -> Result<Raw, Error> {
        let mut argv = self.argv(&["version"]);
        args.push(&mut argv);
        self.call_raw("version", &["VerboseVersionData", "VersionData"], argv)
    }
}
