// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

pub mod activate;
pub mod setup;
pub mod update;

/// Subcommands for managing the OCX installation itself.
#[derive(Subcommand)]
pub enum SelfGroup {
    /// Sourced from `$OCX_HOME/env.sh` at shell startup to activate ocx in the
    /// current shell. Prepends `$OCX_HOME/symlinks/.../bin` to `PATH`, injects
    /// completions (unless `OCX_NO_COMPLETION=1`) and evaluates the global
    /// toolchain env, idempotently, so a re-source never duplicates an entry.
    #[command(long_about = "\
        Sourced from `$OCX_HOME/env.sh` at shell startup to activate ocx in the current shell. \
        Prepends `$OCX_HOME/symlinks/.../bin` to `PATH`, injects completions (unless \
        `OCX_NO_COMPLETION=1`) and evaluates the global toolchain env, idempotently, so a \
        re-source never duplicates an entry.\n\n\
        Prints up to three blocks of shell code.\n\n\
        A completion script for the detected shell comes first, so PowerShell's `using namespace` \
        directives lead the stream. It is emitted only when completions are enabled. It is \
        skipped silently with `OCX_NO_COMPLETION=1`, with `--no-completion`, in a non-interactive \
        session, or for a shell without a completion backend.\n\n\
        Then two `PATH` prepends, in every `activate` mode. The first is the installed binary's \
        `bin` directory. The second is `<OCX_HOME>/toolchain/active/bin` in front of it, so a \
        pinned `ocx` wins. Both come from the binary's own `OCX_HOME` at run time, with no \
        variable reference emitted.\n\n\
        Last, a global env eval line, only when the global `activate` mode is `env` (the ladder's \
        floor). It is guarded by a path test against the resolved absolute binary. A `$PATH` \
        lookup would not do, because a function or an earlier entry could shadow it. Under `bin` \
        or `none` the global toolchain reaches the shell through the trampolines prepended \
        above.\n\n\
        `--hook` and `--no-hook` decide this session only and are never remembered. They are \
        rungs 1 and 2 of the five-rung ladder, above `OCX_NO_HOOK`, `[shell] hook` and the \
        interactivity probe. `ocx self setup --hook` writes the persistent key.\n\n\
        Completions load only in interactive sessions. Each shim states its interactivity \
        through a hidden flag pair. A direct `ocx self activate` with neither flag falls back to \
        whether stdin or stderr is a terminal.")]
    Activate(activate::SelfActivate),
    /// Create or refresh ocx shell integration.
    #[command(long_about = "\
        Create or refresh ocx shell integration.\n\n\
        Completes a bare-binary install. It installs the latest published ocx into the content \
        store and writes the per-shell env shims into `$OCX_HOME`. It also adds a managed \
        activation block to your shell profiles. Pass an optional VERSION to install a specific \
        release instead of the latest.\n\n\
        Setup also registers the installed `bin` directory and `$OCX_HOME/toolchain/active/bin` \
        on the session PATH. That is the Windows user environment, an `environment.d` drop-in on \
        Linux, and a login LaunchAgent on macOS. Processes started outside a shell find them \
        too, such as desktop launchers, IDEs and services.\n\n\
        Neither change reaches a session that is already running. Terminals that are already \
        open see nothing until they are restarted, and the session PATH reaches new processes \
        only after the next login.\n\n\
        A session-PATH location that cannot be written is reported and warned about, never \
        fatal: setup still exits 0, and re-running it retries. The one exception is an \
        `$OCX_HOME` this platform's PATH format cannot spell, which is refused before anything \
        is written (exit 78).\n\n\
        Safe to re-run: the shims and profile blocks are diff-gated, so an unchanged setup is \
        a no-op. Pass `--dry-run` to preview, `--no-modify-path` to skip the profiles and the \
        session PATH alike, and `--force` to overwrite a managed block you have edited by hand \
        (otherwise exit 81).\n\n\
        Pass `--no-profile` to skip only the profile blocks, or `--profile PATH` to target \
        specific files instead of the detected ones.\n\n\
        `--no-modify-path` and `--profile` / `--no-profile` persist as `[shell] modify_path` \
        and `[shell] profiles` in config.toml, so the choice applies to every later run too, \
        not only this one. A corporate CA can be handed in as OCX_EXTRA_CA_CERTS, as a path or \
        the PEM text. It is persisted as `extra_ca_certs_pem` in config.toml before the \
        bootstrap downloads anything. The bootstrap itself and every later command then \
        already trust it.\n\n\
        Phases run in a fixed order. First, persist `OCX_EXTRA_CA_CERTS`. A refused value stops \
        the run before anything is written. Second, bootstrap the install, which is a no-op \
        when that version is already installed and current. A failed bootstrap stops the run \
        before any shim, profile or managed-config write.\n\n\
        Third, adopt the managed config \
        (`--managed-config`, else `OCX_MANAGED_CONFIG`, else the existing seed). Last, write the \
        five `env.*` shims and the profile blocks.\n\n\
        Every write to `config.toml` is a read-modify-write under one cross-process lock, so \
        concurrent runs serialize and both edits land. A holder that outlives the 5 second wait \
        exits 75.\n\n\
        `--dry-run` takes no lock and creates no `$OCX_HOME`. A stale ocx-authored block is \
        rewritten in place and a legacy `# BEGIN ocx` block is migrated to the versioned \
        fence.\n\n\
        `ocx config setup` shares the managed-config phase but not the first.\n\n\
        The session PATH gets the global toolchain's `bin` first and the installed binary's \
        directory second, so a global toolchain that pins `ocx` is what a session resolves. \
        Registration is idempotent by presence test: a second run reports `unchanged`.\n\n\
        `--no-modify-path` or a truthy `OCX_NO_MODIFY_PATH` skips it along with the profile \
        blocks. There is no positive `--modify-path`, so `[shell] modify_path` is only ever \
        written `false` by this command.\n\n\
        VERSION names only the tag, digest or tag-plus-digest of `ocx.sh/ocx/cli`, never a \
        registry or repository. A tag starts with `[a-zA-Z0-9_]`, continues with \
        `[a-zA-Z0-9._-]` and is at most 128 characters (`+` is normalized to `_`). A digest is \
        sha256, sha384 or sha512 in lowercase hex. Anything else exits 64. `package sign` and \
        `package attest` refuse a sha384 or sha512 subject with exit 65 \
        (`subject_digest_unsupported`).\n\n\
        `TAG@DIGEST` asserts that the tag resolves to that digest. A different digest exits 65 and \
        names both; under `--frozen` the comparison uses the local index. The literal `latest` is an \
        ordinary tag that resolves only if the registry publishes it, so omit VERSION to install the \
        latest release.\n\n\
        A digest pin selects a platform-specific package, so pin CI matrices by tag. A pin \
        that is already installed and current exits 0 as `already_present`; a tag older than \
        the installed version warns on stderr and proceeds.\n\n\
        Under `--frozen`, a tag-only pin missing from the local index exits 81.\n\n\
        Defaults: profiles are autodetected, `--toolchain-activate` leaves `ocx.toml` untouched, and \
        `--managed-config` resolves from `OCX_MANAGED_CONFIG`, then the existing seed.\n\n\
        Under `--format json` the root `status` is `completed`, `no_op`, `skipped` or `migrated`. \
        `skipped` means a profile or the `[managed]` fence carried user edits and exits 81.\n\n\
        The report also carries `bootstrap`, `shims`, `profiles`, `session_path_stores`, \
        `managed_config` and `extra_ca_certs`. `reload_hint` is true when a PATH surface changed, \
        and `dirty_profiles` lists the profiles skipped for user edits when `status` is `skipped`. `bootstrap.status` is `already_present`, `pulled` or \
        `would_pull`.\n\n\
        A profile outcome is `completed`, `no_op`, `migrated` or `skipped_dirty`. A session PATH \
        store outcome is `written`, `unchanged`, `removed`, `skipped_opt_out`, \
        `skipped_unsupported` or `failed`.\n\n\
        `jq .status` returns the root status, not the bootstrap status. Read `jq .bootstrap.status` \
        for the install step.\n\n\
        `managed_config.status` is `not_configured`, `already_adopted`, `adopted`, `refreshed`, \
        `refresh_unavailable`, `cleared`, `dirty`, `would_adopt` or `would_refresh`. \
        `extra_ca_certs.status` is `not_configured`, `persisted`, `unchanged`, `would_persist` or \
        `system_locked`.\n\n\
        https://ocx.sh/docs/getting-started/install-ocx/#manual")]
    Setup(setup::SelfSetup),
    /// Update ocx itself to the latest released version. Without `--check`,
    /// downloads the newest release and activates it; with `--check`, reports
    /// the result without installing.
    #[command(long_about = "\
        Update ocx itself to the latest released version. Without `--check`, downloads the \
        newest release and activates it; with `--check`, reports the result without \
        installing.\n\n\
        Downloading and activating are two separate steps. The newest release is pulled first, \
        then the new binary activates itself by running its own `ocx self setup`. If that \
        inner setup cannot finish, for example because a shell profile carries edits it refuses \
        to overwrite, the download stands but nothing is activated. Exit 75 reports this as \
        pulled rather than installed, and running `ocx self setup` again finishes the job.\n\n\
        The latest version is looked up live from the published index rather than your local \
        index, so the freshest release is always found. `--offline` skips the check. Both forms \
        always bypass the auto-check throttle.\n\n\
        Only plain `major.minor.patch` release tags count: rolling tags (`1`, `1.2`), \
        build-tagged versions and pre-releases are filtered out. The command reports one of \
        three outcomes.\n\n\
        Already up to date: nothing was pulled. Installed: `current` now names the new binary. \
        This holds even when the hand-off setup did not finish cleanly. A stderr advisory then \
        says what is left to do, typically `ocx self setup`, or with `--force`.\n\n\
        Pulled: the release was downloaded but never activated, so `current` still names the \
        old binary. The hand-off pins the digest just pulled, heals shell profiles rather than \
        introducing new ones, and writes no `config.toml` key; its output is inherited.\n\n\
        With `--check` the lookup runs without installing. It exits 0 when it completes, \
        including \"already up to date\" and \"update available\". It exits 75 when the check is \
        skipped.\n\n\
        Under `--format json` the report's `status` is `up_to_date`, `update_available`, \
        `installed`, `pulled` or `skipped`. `installed` carries `from` and `to`; `pulled` carries \
        `to`; `update_available` carries `identifier`.\n\n\
        `handoff` appears on `installed` and `pulled` only when the hand-off setup did not finish \
        cleanly. Its `type` is `exited` with `exit_code`, `spawn_failed` with `detail`, or \
        `signalled` with `signal`.\n\n\
        A `skipped` report names `skipped_reason.type`: `bootstrap`, `offline`, `throttled`, \
        `registry_probe_failed`, `not_found`, `unparseable_current`, `unparseable_latest` or \
        `no_release_tag`. Treat an unknown value as an unrecognised skip.")]
    Update(update::SelfUpdate),
}

impl SelfGroup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            // Reached only if `App::run` stops dispatching Activate before `Context::try_init`.
            SelfGroup::Activate(_) => {
                unreachable!(
                    "SelfGroup::Activate is always dispatched via app.rs::App::run before Context::try_init — see should_check_for_update bypass list"
                )
            }
            SelfGroup::Setup(setup) => setup.execute(context).await,
            SelfGroup::Update(update) => update.execute(context).await,
        }
    }
}
