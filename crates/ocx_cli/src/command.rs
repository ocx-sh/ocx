// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Subcommand;

pub mod about;
pub mod add;
pub mod clean;
pub mod config;
pub mod config_push;
pub mod config_setup;
pub mod config_test;
pub mod config_update;
pub mod contract;
pub mod deprecated;
pub mod deps;
pub mod deselect;
pub mod direnv;
pub mod direnv_export;
pub mod direnv_init;
pub mod env;
pub mod exec;
pub mod index;
pub mod index_catalog;
pub mod index_common;
pub mod index_list;
pub mod index_regenerate;
pub mod index_sync;
pub mod index_update;
pub mod init;
pub mod inspect;
pub mod install;
pub mod launcher;
pub mod leaf;
pub mod lock;
pub mod login;
pub mod logout;
pub mod package;
pub mod package_announce;
pub mod package_attest;
pub mod package_cascade;
pub mod package_cascade_check;
pub mod package_cascade_repair;
pub mod package_claim;
pub mod package_copy;
pub mod package_create;
pub mod package_description;
pub mod package_description_pull;
pub mod package_description_push;
pub mod package_inspect;
pub mod package_prune;
pub mod package_pull;
pub mod package_push;
pub mod package_receipt;
pub mod package_sbom;
pub mod package_sign;
pub mod package_sign_common;
pub mod package_test;
pub mod package_verify;
pub mod patch;
pub mod patch_common;
pub mod patch_freeze;
pub mod patch_publish;
pub mod patch_sync;
pub mod patch_test;
pub mod patch_why;
pub mod pull;
pub mod remove;
pub mod script_runner;
pub mod select;
pub mod self_group;
pub mod shell;
pub mod shell_allow;
pub mod shell_completion;
pub mod shell_revoke;
pub mod shell_state;
pub mod status;
pub mod toolchain_env;
pub mod toolchain_exec;
pub mod uninstall;
pub mod update;
pub mod upgrade;
pub mod version;
pub mod which;

pub use contract::{CONTRACT, OutputMode};

#[derive(Subcommand)]
pub enum Command {
    /// Compose and print the toolchain environment.
    #[command(long_about = "\
        Compose and print the toolchain environment.\n\n\
        Reads the in-scope `ocx.toml` + `ocx.lock`, or the global toolchain under `--global`. \
        Defaults to a plain table; `ocx --format json env` emits JSON.\n\n\
        `--shell[=NAME]` is the only eval-safe form.\n\n\
        Under `--global` an unusable toolchain (none configured, or a corrupt or stale lock) \
        exits 0 with an empty environment.\n\n\
        With `--format json` the document also carries `binaries`, `entrypoints` and \
        `integrations` arrays beside `items`, plus an `advisories` array for any deferred package \
        in the composition. A package missing from the object store is installed as part of \
        composition. `--no-pull` skips that and never contacts the registry: an unmaterialized \
        package is warned about on stderr and omitted, and the exit code stays 0. One a `[[trust.policy]]` covers is \
        signature-verified first. Opt out with `OCX_NO_VERIFY`.\n\n\
        `--shell` needs the equals form (`--shell=bash`) so an unquoted positional token cannot \
        inject shell.")]
    Env(toolchain_env::ToolchainEnv),
    /// Add one or more package bindings to ocx.toml.
    #[command(long_about = "\
        Add one or more package bindings to ocx.toml.\n\n\
        Walks up from the working directory to the nearest `ocx.toml` and fails with exit 64 \
        when there is none; it never creates one (run `ocx init` first). Resolves only the new \
        bindings, carries every existing `ocx.lock` entry forward unchanged, and installs the \
        new packages.\n\n\
        Re-renders the project toolchain into `<project>/.ocx/toolchain/` - or under the \
        `toolchain_dir` root, when one is configured. That directory carries its own \
        `.gitignore`, so `git status` stays clean.\n\n\
        Several identifiers may be given at once. They are staged together and committed \
        atomically: if one is invalid, or its name is bound to a different identifier, nothing \
        is written.\n\n\
        `--group` applies to every identifier in the batch, and `--platform` only picks which \
        platform's leaf to fetch - the lock stays host-agnostic.\n\n\
        Each binding is named after the repository basename, so two packages with the same \
        basename collide. Prefix an identifier with `NAME=` to choose the name yourself: `ocx \
        add glab=ocx.sh/gitlab/cli`.\n\n\
        A bare identifier defaults to `:latest`, and the `ocx.toml` entry is written explicitly. \
        A group name and a `NAME` match `^[A-Za-z0-9][A-Za-z0-9._-]*$` and are at most 64 bytes.\n\n\
        Adding a binding the target group already declares with the same identifier leaves \
        `ocx.toml` byte-identical, including when one batch repeats the identifier. Such a \
        binding is re-locked only when `ocx.lock` has no entry for it; an existing pin is \
        never re-resolved, so run `ocx update <name>` to move one.\n\n\
        The packages are downloaded either way, so re-running after a failed download finishes \
        the install.\n\n\
        Each already-declared binding prints `Unchanged: <name> is already added` on stderr. \
        Fails with exit 64 when the binding name is already bound to a different identifier.\n\n\
        Run `ocx remove <name>` and add it again, or `ocx update <name>` to move the pin. The \
        same name in a different group is legal, so a project can carry different versions of \
        one package in different groups.\n\n\
        Fails with exit 65 when `ocx.toml` drifted from `ocx.lock` before this add (run `ocx \
        lock` to reconcile), or exit 78 when a carried entry can no longer be migrated (run `ocx \
        update`).")]
    Add(add::Add),
    /// Remove unreferenced objects from the local object store.
    #[command(long_about = "\
        Remove unreferenced objects from the local object store.\n\n\
        An object is unreferenced when no candidate or current symlink points to it and no \
        installed package depends on it. A registered project's `ocx.lock` also pins objects, \
        patch companions included. Projects register in the `$OCX_HOME/projects/` ledger when \
        `ocx lock` or `ocx add` writes a lockfile.\n\n\
        Dependencies of a removed package become unreferenced and are collected in the same pass.\n\n\
        With `--format json`, each item carries `kind`, `dry_run`, `path` and `held_by`. \
        A dry run adds a `Held By` column naming the projects that pin an object it keeps.\n\n\
        The rendered project toolchain under `<home>/toolchain/` is derived state that `ocx pull` \
        rewrites from `ocx.lock`; `clean` never walks or collects from it. Every run also removes \
        the consent stamp of any project whose directory no longer exists, and `--dry-run` lists \
        those stamps without removing them.\n\n\
        Do not run `clean` while other ocx commands are running: a concurrent install may \
        reference an object that `clean` is about to remove.")]
    Clean(clean::Clean),
    /// Manage the corporate managed-configuration tier.
    #[command(subcommand)]
    Config(config::ConfigGroup),
    /// direnv integration (init writes .envrc; export emits the env block).
    ///
    /// Bare `ocx direnv` is shorthand for `ocx direnv init`, the once-per-project setup that writes
    /// a `.envrc`. The generated `.envrc` evaluates `ocx direnv export` on every directory entry.
    Direnv(direnv::Direnv),
    /// Inspect the package index and refresh the local copy.
    ///
    /// None of these subcommands writes to a forge: proposing a namespace or a
    /// new version into the shared index is `ocx package claim` and
    /// `ocx package announce`.
    #[command(subcommand)]
    Index(index::Index),
    /// Print ocx version, registry, platform, shell, and home directory.
    #[command(long_about = "\
        Print ocx version, registry, platform, shell, and home directory.\n\n\
        Also lists the detected libc family on Linux. When build provenance was baked in at \
        compile time, two more rows appear: `Commit` (short SHA and clean or dirty status) and \
        `Channel` (for example `dev`). Both are absent on local builds and on stable releases.\n\n\
        The `Libc` row is absent on non-Linux hosts and on hosts with no readable dynamic loader. \
        Non-FHS layouts such as Gentoo Prefix and Homebrew on Linux are detected from a system \
        binary's loader path. NixOS is detected only while nix-ld is active.\n\n\
        The `Platform` row is the bare `os/arch` of the host.\n\n\
        `--format json` emits `platforms`, `features`, `libc`, `shell` and `home`, plus the `commit`, \
        `build` and `ci` blocks when baked in. `platforms[0]` is the host as an OCI platform \
        object with its `os.features`.\n\n\
        `features` lists those `os.features`, and `libc` is its libc subset. A package runs on the \
        host when its offered features are a subset of `features`.\n\n\
        In a terminal the table renders beside an isometric logo; piped output without `--color \
        always` falls back to plain key-value lines.")]
    About(about::About),
    /// Create a minimal ocx.toml in the current directory.
    #[command(long_about = "\
        Create a minimal ocx.toml in the current directory.\n\n\
        Writes a non-interactive skeleton - a `#:schema` directive and an empty `[tools]` table - \
        into the directory `--project` or `OCX_PROJECT` selects (else the current directory), or \
        into `$OCX_HOME` under `--global`. Use `ocx add` afterwards, or edit the file directly; \
        comments and declaration order survive every mutation.\n\n\
        If `ocx.toml` (or a symlink at that path) already exists the command exits 64 and \
        overwrites nothing. A `--project` path that does not exist exits 79 rather than being \
        created.\n\n\
        Records a consent stamp for the new project, with an empty source set because there is no \
        lock yet. The next shell prompt in that directory applies it, and the first `ocx add` \
        re-records it. `--no-consent` skips the stamp (consent later with `ocx shell allow`) and \
        `OCX_NO_CONSENT` makes that the default; the flag outranks the variable.\n\n\
        Under `--global` no stamp is written, since the ocx home toolchain is always consented. \
        `ocx --global add` and its siblings also create the global file, so `ocx --global init` \
        is for scaffolding it ahead of the first binding.")]
    Init(init::Init),
    /// Resolve package tags to digests and write ocx.lock.
    #[command(long_about = "\
        Resolve package tags to digests and write ocx.lock.\n\n\
        Resolves every binding in the nearest `ocx.toml` to per-platform leaf digests and writes \
        `ocx.lock` beside it. It is a whole-file reconcile. When the lock is current, every pin \
        is carried forward verbatim and nothing moves, even if a moving tag has moved upstream. \
        A lock is current when its `declaration_hash` over the canonicalized `ocx.toml` \
        matches.\n\n\
        When the config drifted, every declared tag is re-resolved and a moving tag may advance. \
        To force-advance pins on a current lock, use `ocx update`.\n\n\
        Each binding records its registry and repository plus a table of every platform the \
        publisher ships, whatever OS the command runs on. A lock committed on Linux is therefore \
        complete for macOS and Windows runners. The command is transactional: either every \
        binding resolves and the file is rewritten atomically, or nothing is written.\n\n\
        When no binding's resolved content changed, `generated_at` is preserved so diffs stay \
        minimal. After a successful write it prints a one-line stderr advisory when \
        `.gitattributes` does not declare `ocx.lock merge=union`.\n\n\
        Re-renders the project toolchain into `<project>/.ocx/toolchain/`, or under the \
        `toolchain_dir` root when one is configured. The `links/<group>/<entry>` tree and \
        the `shells/default/bin` launchers reached through the `active` link match the lock just \
        written. That directory carries its own `.gitignore`, so `git status` stays clean.")]
    Lock(lock::Lock),
    /// Authenticate to a registry and persist credentials.
    #[command(long_about = "\
        Authenticate to a registry and persist credentials.\n\n\
        Credentials go to the same `~/.docker/config.json` that `docker login` and `oras login` \
        write, so a credential written by any of the three is readable by the others. The \
        registry defaults to `OCX_DEFAULT_REGISTRY` (`ocx.sh`) when omitted.\n\n\
        Storage tiers, highest priority first: `credHelpers[REGISTRY]` (a per-registry helper), \
        `credsStore` (the global default helper), then a plaintext base64 `auths[REGISTRY].auth` \
        entry, which needs `--allow-insecure-store`.\n\n\
        `--auth-type` is reserved for a future OIDC or browser flow and exits 64 today. Plain \
        HTTP is not a login flag. The probe uses whatever scheme the rest of the binary would: \
        HTTPS unless the host is named by `[registries.<name>] insecure` or \
        `OCX_INSECURE_REGISTRIES`.\n\n\
        `--format json` reports `registry` and `username`. `registry` is the bare `host[:port]`, so \
        `https://ghcr.io/v1/` reports `ghcr.io`; a value with no parseable `host[:port]` exits 64.")]
    Login(login::Login),
    /// Remove credentials for a registry.
    ///
    /// Exits 0 whether or not the registry was logged in, matching `docker logout`, `oras logout`
    /// and `helm registry logout`. A CI cleanup script therefore does not fail when an earlier step
    /// already removed the credentials. The registry defaults to `OCX_DEFAULT_REGISTRY` when
    /// omitted.
    Logout(logout::Logout),
    /// Re-resolve declared tags against the registry; whole file or a subset.
    #[command(long_about = "\
        Re-resolve declared tags against the registry; whole file or a subset.\n\n\
        Resolves declared tags live against the registry by default (the root `--remote` flag \
        is redundant here, but accepted). It records the result in `ocx.lock` only and never modifies the local \
        index tag snapshot. A moving tag (`:latest`, `:3`) advances to \
        wherever it points today.\n\n\
        Under `--frozen`, resolution is capped at the local index snapshot (an unsnapshotted \
        tag exits 81); `--offline` forbids network access. Pass binding names or `-g/--group` \
        to advance only part of the toolchain and freeze the rest: `ocx update ripgrep` \
        advances one binding, `ocx update -g ci` advances a whole group.\n\n\
        A scoped update needs an existing `ocx.lock` (exit 78), refuses a drifted `ocx.toml` \
        (exit 65), and rejects an unknown group or name (exit 64). With no arguments every \
        declared tag is re-resolved, even when the lock is current, and an unchanged result \
        rewrites the lock byte-identically.\n\n\
        The command is transactional: any resolution failure writes nothing. It moves only \
        what a declared tag points to and never edits the declaration; to move the tag itself, \
        use `ocx upgrade`.\n\n\
        `--format json` emits `changes`, `unchanged` and `metadata_changed`; `--verbose` lists one \
        row per platform. See https://ocx.sh/docs/guides/update-tools/#project-update-report.\n\n\
        Because it holds both the old lock and the new one, it reports what moved. A table lists \
        the bindings whose pull identifier changed. Those that held still are summarized \
        behind a hint line unless `--verbose` lists them too. Unlike `ocx lock`, which \
        resolves through the local index and only when `ocx.toml` drifted, `ocx update` \
        consults the registry every time.\n\n\
        Re-renders the project toolchain into `<project>/.ocx/toolchain/`, or under the \
        `toolchain_dir` root when one is configured. The `links/<group>/<entry>` tree and \
        the `shells/default/bin` launchers reached through the `active` link follow the advanced \
        lock. That directory carries its own `.gitignore`, so `git status` stays clean.")]
    Update(update::Update),
    /// Move declared tags in ocx.toml to the newest published release.
    #[command(long_about = "\
        Move declared tags in ocx.toml to the newest published release.\n\n\
        `ocx update` moves the lock to where a tag points now; `ocx upgrade` moves the tag \
        itself. A binding keeps its variant and precision and stays within its major: \
        `cmake:3.28` moves to `cmake:3.29`, and `cmake:3` has nothing to move to until \
        `--major` lets it cross to `cmake:4`.\n\n\
        A newer major is listed as a hint either way. A binding that cannot move is left alone \
        and reported with one reason: `digest_pinned`, `latest`, `not_a_version`, \
        `prerelease_or_build` or `up_to_date`.\n\n\
        The retagged bindings are then re-locked and the project toolchain re-rendered, as \
        `ocx update` does.\n\n\
        Pass binding names or `-g/--group` to upgrade only part of the toolchain. `--check` \
        writes nothing and exits 65 when a tag would move.\n\n\
        Needs an existing `ocx.lock` (exit 78), refuses a drifted `ocx.toml` (exit 65), \
        rejects an unknown group or name (exit 64), and is refused under `--frozen` or \
        `--offline` (exit 81).\n\n\
        Each binding keeps its variant: a `debug-3.28` binding considers only `debug-*` tags, \
        and the new tag must be strictly newer than the declared one. A binding that cannot \
        move is a report row with its reason, never an error.\n\n\
        Comments and formatting in `ocx.toml` survive the edit. Tag listings come from the \
        registry every time and never touch the local index. Only the retagged bindings are \
        re-resolved and every other pin carries forward. When nothing would move, neither file \
        is written.\n\n\
        After writing, the new pins are materialized for the host platform, as `ocx update` \
        does.")]
    Upgrade(upgrade::Upgrade),
    /// Internal subcommands used by generated entry-point launchers (hidden).
    #[command(subcommand)]
    Launcher(launcher::Launcher),
    /// Operations related to packages (e.g. bundling or deploying)
    #[command(subcommand)]
    Package(package::Package),
    /// Manage patch overlays for a project.
    #[command(subcommand)]
    Patch(patch::PatchGroup),
    /// Pre-warm the object store, then render the project toolchain.
    ///
    /// Every package comes from the digest-pinned `ocx.lock`, never from the index, which makes
    /// this the primitive for reproducible CI setups; use `ocx package pull` for OCI identifiers.
    /// `ocx pull` never changes the lock's content: re-resolution is `ocx update`, rewriting from the
    /// config is `ocx lock`. After a successful pull it re-saves the file to advance its mtime.
    #[command(long_about = "\
        Pre-warm the object store, then render the project toolchain.\n\n\
        Fetches every digest-pinned entry the project `ocx.lock` declares into the local \
        object store. Then it renders the project toolchain into `<project>/.ocx/toolchain/`, or \
        under the `toolchain_dir` root when one is configured. That is one link per \
        `links/<group>/<entry>`, plus the `shells/default/bin` launchers for the default \
        group, reached through the `active` link. That directory carries its own `.gitignore`, \
        so `git status` stays clean.\n\n\
        This is the primary way a project toolchain is rendered; `--dry-run` reports the delta \
        and writes nothing.\n\n\
        The package store's own `candidate` and `current` symlinks are left alone: those move \
        only when you install or select a package.\n\n\
        Every package comes from the digest-pinned `ocx.lock`, never from the index, which makes \
        this the primitive for reproducible CI setups; use `ocx package pull` for OCI \
        identifiers. It never changes the lock's content: re-resolution is `ocx update`, rewriting \
        from the config is `ocx lock`. After a successful pull it re-saves the file to advance \
        its mtime.")]
    Pull(pull::Pull),
    /// Remove one or more package bindings from ocx.toml.
    #[command(long_about = "\
        Remove one or more package bindings from ocx.toml.\n\n\
        Each argument is a bare binding name (`cmake`), a name with a tag (`kitware/cmake:3.28`) \
        or a fully-qualified identifier (`ocx.sh/kitware/cmake:3.28`). An identifier is reduced \
        to its repository basename and matched against the `ocx.toml` map key. The tag and \
        registry only locate the entry. A binding added under \
        an explicit name (`ocx add glab=...`) is matched only by that name.\n\n\
        Fails with exit 79 when an argument matches no binding; the removals are staged together, \
        so `ocx.toml` is left unchanged.\n\n\
        When one name appears in more than one group (for example in `[tools]` and `[group.ci]`), \
        the command cannot tell which to drop and exits 64. Pass `--group` to name the target.\n\n\
        Drops each binding from `ocx.lock` as well. Then it re-renders the project toolchain into \
        `<project>/.ocx/toolchain/`, or under the `toolchain_dir` root when one is configured. The removed tools leave the `links/<group>/<entry>` tree and the `shells/default/bin` \
        launchers reached through the `active` link. That directory carries its own `.gitignore`, \
        so `git status` stays clean.\n\n\
        Only the reference is removed; `ocx clean`, or `--purge` right away, deletes a package \
        nothing else references.")]
    #[command(visible_alias = "rm")]
    Remove(remove::Remove),
    /// Run a command with the composed environment from the project toolchain.
    #[command(long_about = "\
        Run a command with the composed environment from the project toolchain.\n\n\
        Loads the nearest `ocx.toml` above the current directory (the walk `--project` describes) \
        and its `ocx.lock`, composes the environment of the selected bindings, and runs the \
        command in it. Only that file's lock is composed. Inside a nested project, the \
        enclosing project's packages are not part of the child environment. Name that project \
        with `ocx --project <dir> exec`.\n\n\
        Names are binding names from `ocx.toml`, not OCI identifiers; for those use `ocx package \
        exec`.\n\n\
        `--` is mandatory. Optional binding names come before it. The command and its arguments, at least one token forwarded unchanged, come after it. A missing `--` or an empty command \
        exits 64.\n\n\
        With names, only those bindings must resolve and install. An unrelated binding that \
        ships no leaf for this host (exit 78) does not block the run. Without names, every \
        binding in scope is composed.\n\n\
        A binding missing from the object store is installed during composition, and one a \
        `[[trust.policy]]` covers is signature-verified first (opt out with `OCX_NO_VERIFY`). \
        Entries compose in `-g` selection order after `all` expansion, then alphabetically by \
        binding name within a group.\n\n\
        Later entries are prepended, so the last group listed has its `bin/` searched first on \
        `PATH`.\n\n\
        The command is looked up once, before the child starts. Only the composed packages' \
        own `PATH` contributions are searched, never the ambient `PATH` or the working directory. A \
        match that is itself an ocx launcher is refused. `--env PATH=...` replaces the composed \
        `PATH` outright; write `--env PATH:path=...` to prepend.")]
    #[command(visible_alias = "x")]
    Exec(toolchain_exec::ToolchainExec),
    /// Deprecated spelling of `ocx exec`; removed in 0.7.
    #[command(name = "run", hide = true)]
    DeprecatedRun(toolchain_exec::ToolchainExec),
    #[command(subcommand)]
    Shell(shell::Shell),
    /// Manage the OCX installation itself (PATH activation, completions, self-update).
    #[command(name = "self", subcommand)]
    Self_(self_group::SelfGroup),
    /// Show what ocx.toml and ocx.lock declare, without resolving anything.
    #[command(long_about = "\
        Show what ocx.toml and ocx.lock declare, without resolving anything.\n\n\
        Offline and read-only: no registry is contacted, no package metadata is read, and the JSON \
        report selects no platform. No relative `path` value is anchored to the project root. \
        It answers on projects other commands refuse. A missing `ocx.lock` (exit 78 for `ocx \
        pull` and `ocx exec`), a drifted one (exit 65) and an unparseable one are reported as \
        payload, and the command still exits 0.\n\n\
        There is no `-g` and no NAME argument: the report is a keyed object a caller narrows \
        itself. `--offline` is accepted and inert.\n\n\
        Plain output marks a binding only where something needs attention: `not locked`, \
        `not locked for this platform`, `ambiguous for this platform` or `orphaned in ocx.lock`. \
        Group headings appear once the project declares more than one group.\n\n\
        The JSON report carries `lock` (with `current` and `declaration_hash`), \
        `groups.<name>.tools` and `package_settings`; the published reports schema describes it.\n\n\
        For what the declarations resolve to on this host, use `ocx inspect`.")]
    Status(status::Status),
    /// Inspect what the project toolchain resolves to, without installing.
    #[command(long_about = "\
        Inspect what the project toolchain resolves to, without installing.\n\n\
        The toolchain-tier counterpart to `ocx package inspect`: the same report and flags, keyed \
        by `ocx.toml` binding instead of raw identifier. It adds the project's composed `env` as an \
        ordered array: `[env]`, then each selected group's `[env]` in `-g` order, then `--env`. Every declaration is kept, so one key can repeat. Selects the bindings in the requested \
        groups, narrows them to any NAMEs given, and reports each.\n\n\
        Nothing is installed and neither project file is written.\n\n\
        By default each binding lists the platform candidates `ocx.lock` pins. The report is a \
        pure projection of the two project files: no registry is contacted, no host leaf is \
        chosen, and `-p` is inert. `--resolve` selects a platform exactly as on the OCI-tier \
        command.\n\n\
        A cold `--closure` costs one config-blob fetch per selected binding; a warm one works \
        under `--offline`. Needs a current lock: exit 78 when absent, 65 when it no longer \
        matches `ocx.toml`; use `ocx status` for those states.\n\n\
        Under `--resolve`, an unnamed sibling that ships no leaf for this host does not block the \
        report.")]
    Inspect(inspect::Inspect),
    /// Print the version of ocx
    ///
    /// Without flags it prints a bare `major.minor.patch` string for scripts. `--verbose` prints a
    /// multi-line build provenance summary.
    ///
    /// JSON output always carries the populated subset of provenance fields, whether or not
    /// `--verbose` is given.
    Version(version::Version),
    /// External subcommand: dispatched to an `ocx-<name>` binary discovered on PATH.
    // See `adr_cli_plugin_pattern.md` and `app::plugin_dispatch`.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

impl Command {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Command::Env(env) => env.execute(context).await,
            Command::Add(add) => add.execute(context).await,
            Command::Clean(clean) => clean.execute(context).await,
            Command::Config(config_group) => config_group.execute(context).await,
            Command::Direnv(direnv) => direnv.execute(context).await,
            Command::Index(index) => index.execute(context).await,
            Command::About(about) => about.execute(context).await,
            Command::Init(init) => init.execute(context).await,
            Command::Lock(lock) => lock.execute(context).await,
            Command::Login(login) => login.execute(context).await,
            Command::Logout(logout) => logout.execute(context).await,
            Command::Update(update) => update.execute(context).await,
            Command::Upgrade(upgrade) => upgrade.execute(context).await,
            Command::Launcher(launcher) => launcher.execute(context).await,
            Command::Package(package) => package.execute(context).await,
            Command::Patch(patch_group) => patch_group.execute(context).await,
            Command::Pull(pull) => pull.execute(context).await,
            Command::Remove(remove) => remove.execute(context).await,
            Command::Exec(exec) => exec.execute(context).await,
            Command::DeprecatedRun(exec) => {
                deprecated::warn_renamed(&context, &deprecated::RUN);
                exec.execute(context).await
            }
            Command::Shell(shell) => shell.execute(context).await,
            Command::Self_(group) => group.execute(context).await,
            Command::Status(status) => status.execute(context).await,
            Command::Inspect(inspect) => inspect.execute(context).await,
            Command::Version(_) => unreachable!("Version is handled in the static-command bypass in App::run"),
            Command::External(_) => {
                unreachable!("Command::External must be handled in App::run before reaching execute()")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Subcommand as _;

    /// The `ocx index` group, as clap builds it.
    fn index_group() -> clap::Command {
        super::Command::augment_subcommands(clap::Command::new("ocx"))
            .find_subcommand("index")
            .expect("ocx index is a command group")
            .clone()
    }

    /// The whole help text an operator sees for the group — the long form when
    /// there is one, else the short.
    fn index_help() -> String {
        let group = index_group();
        group
            .get_long_about()
            .or_else(|| group.get_about())
            .expect("the index group carries help text")
            .to_string()
    }

    /// S-035: no forge write may land under `ocx index`.
    ///
    /// **The reachable half of S-035, and a deliberate narrowing of it**
    /// (DX-54). The scenario asks for a clap did-you-mean hint pointing
    /// `ocx index claim` at `ocx package claim`; clap 4 emits none, because its
    /// candidate set is one command's **own** subcommand list and `claim` is
    /// within edit distance of none of `catalog`, `list`, `update`, `sync`,
    /// `regenerate`. Measured, not reasoned: `ocx index catalo` does print
    /// `tip: a similar subcommand exists: 'catalog'`, so the mechanism is live
    /// and only similarity-gated, and clap exposes no cross-command hook. The
    /// one thing that would emit a pointer is a hidden `claim` arm on
    /// `enum Index`, which makes clap **parse** rather than suggest — so
    /// `ocx index claim --help` would render a help page for a command that does
    /// not exist, and this very assertion would invert.
    ///
    /// So the property actually protected is the one that matters for the life
    /// of the repository: a later contributor must not put a forge write under
    /// a group whose help says nothing writes to a forge.
    ///
    /// **Residual, recorded rather than hidden:** the pointer to
    /// `ocx package claim` is one `--help` away, not inline — which is exactly
    /// where clap's own `For more information, try '--help'` sends the operator.
    ///
    /// **Green on arrival, and no red is reachable from this package's file
    /// set:** the mutation lives in `command/index.rs`, which no work package
    /// owns. Control: the reviewer adds a `Claim` arm to `enum Index`, observes
    /// this red, and reverts.
    #[test]
    fn index_group_declares_no_claim_subcommand() {
        assert!(
            index_group().find_subcommand("claim").is_none(),
            "ocx index must declare no claim subcommand — claiming a namespace writes to a forge"
        );
    }

    /// S-035: the group's own help states that no index subcommand writes to a
    /// forge, and names `ocx package claim`.
    ///
    /// Read off the built `Command` rather than off this file's source text: a
    /// source scan would live in the file it scans, which measures itself
    /// (`quality-core.md` § Unchecked Green). The two clauses are asserted
    /// separately so dropping either one reds.
    ///
    /// **Green on arrival** — the stub already wrote the sentence.
    /// Mutation: restore the doc comment to
    /// `Operations related to the package index`.
    #[test]
    fn index_group_help_states_no_forge_write() {
        let help = index_help();
        assert!(
            help.contains("writes to a forge"),
            "the index group's help must state that none of its subcommands writes to a forge; got: {help}"
        );
        assert!(
            help.contains("ocx package claim"),
            "and must name the command that does; got: {help}"
        );
    }

    /// `quality-cli-help.md` § Forbidden: no incorrect statement of behaviour.
    ///
    /// The group's long help must not tell an operator that its subcommands
    /// refresh the local copy from the registry. Three of the five do not:
    /// `catalog` lists repositories **in the registry**, `list` only reads, and
    /// `regenerate` re-derives `c/index.json` from the local `p/` walk and
    /// consults no source at all.
    ///
    /// Red at the stub: the sentence is present verbatim (DX-60).
    /// Mutation once implemented: restore it.
    #[test]
    fn index_group_help_does_not_claim_every_subcommand_refreshes() {
        let help = index_help();
        assert!(
            !help.contains("refresh it from the registry"),
            "three of the five index subcommands contact no registry; got: {help}"
        );
    }
}
