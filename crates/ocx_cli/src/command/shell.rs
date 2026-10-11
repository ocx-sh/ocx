// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

/// Shell-integration commands.
///
/// Generate static shell completion scripts, consent to a project's shell
/// activation or withdraw that consent, and report the state of the per-prompt
/// environment integration. Login-profile activation is handled by
/// `$OCX_HOME/env.sh` (sourced from your shell profile), not here.
#[derive(Subcommand)]
pub enum Shell {
    /// Consent to a project's shell activation.
    #[command(long_about = "\
        Consent to a project's shell activation.\n\n\
        Records a consent stamp for the project governing PATH (default: the current \
        directory), so the next shell prompt applies that project's binaries and environment. \
        This is the same stamp `ocx init` writes for the project it creates. `ocx add`, \
        `ocx lock`, `ocx remove`, `ocx update`, `ocx upgrade`, `ocx pull` and `ocx exec` write it \
        as a side effect, because running a mutating command in a directory is itself consent. \
        `--no-consent` or `OCX_NO_CONSENT` suppresses the side effect, not this command.\n\n\
        The stamp records the source set the project's `ocx.lock` resolves from at the time it \
        is written. A binding added from a new registry or organisation invalidates it; run \
        this again to consent to the wider set.\n\n\
        Undo with `ocx shell revoke`. See which grant is in effect with `ocx shell state`.\n\n\
        Grants that live in configuration instead - a directory under `[shell.consent] paths`, \
        an organisation under `[shell.consent] namespaces` - are edited in `config.toml`, not \
        here.\n\n\
        Exits 64 when no `ocx.toml` governs PATH, and when PATH names the ocx home: the global \
        toolchain is always consented and never carries a stamp.\n\n\
        https://ocx.sh/docs/explanation/shell-integration/")]
    Allow(super::shell_allow::ShellAllow),
    /// Generate shell completion scripts.
    ///
    /// Prints the completion script for the shell named by `--shell` to stdout, for sourcing from a
    /// profile or writing into the shell's completion directory. With `--if-enabled` it prints
    /// nothing when completions are switched off for the session. Install recipes per shell:
    /// <https://ocx.sh/docs/troubleshooting/shell/#commands>
    Completion(super::shell_completion::ShellCompletion),
    /// Withdraw a project's consent stamp.
    #[command(long_about = "\
        Withdraw a project's consent stamp.\n\n\
        Removes the consent stamp for the project governing PATH (default: the current \
        directory). The next shell prompt stops applying that project's binaries and \
        environment, unless a `[shell.consent]` grant still covers it. Those grants live in \
        `config.toml` and are removed by editing it.\n\n\
        `ocx shell state` reports which grant is in effect.\n\n\
        Revoking a project that carries no stamp is not an error: the result is the state you \
        asked for either way.\n\n\
        A later `ocx add`, `ocx lock`, `ocx pull`, `ocx exec`, `ocx remove`, `ocx update` or \
        `ocx upgrade` in that directory writes the stamp again.\n\n\
        Exits 64 when no `ocx.toml` governs PATH.\n\n\
        https://ocx.sh/docs/explanation/shell-integration/")]
    Revoke(super::shell_revoke::ShellRevoke),
    /// Report the shell integration's state, and why it is inert when it is.
    #[command(long_about = "\
        Report the shell integration's state, and why it is inert when it is.\n\n\
        Prints the decoded `__OCX_ENV_STATE` ledger as fields. They show what each scope \
        applied, whether the watch-set fingerprint still matches, and whether the project \
        scope's constant priors are intact. Above all they show the enumerated reason the shell \
        is not active.\n\n\
        The shell is inert for one of these reasons, each with its remedy.\n\n\
        The hook is disabled by `--no-hook`, `OCX_NO_HOOK` or `[shell] hook`; the report names the \
        deciding rung and tier. Enable the hook.\n\n\
        The shell yielded to direnv or mise; the report names the live signal. Stop that tool or \
        accept the yield.\n\n\
        `ocx.lock` is unavailable: absent, unreadable or unparseable. Run `ocx lock`.\n\n\
        A `[shell.consent] namespaces` grant matches the lock's claim, but the package store does \
        not corroborate those sources. The report names both source sets. Consent with `ocx shell allow`.\n\n\
        The source set drifted from the stamp; the report names the new sources. Run \
        `ocx shell allow` again.\n\n\
        There is no consent stamp and no matching grant. Run `ocx shell allow`.\n\n\
        The ledger exceeded its cap. Declare less environment in the abandoned scope.\n\n\
        The ledger is absent. In a shell `ocx self setup` wired, that is the first prompt and \
        none is needed; the next prompt applies it. If setup never wired this shell, run \
        `ocx self setup` and start a new shell.\n\n\
        The ledger is present but corrupt. Start a new shell, or run `unset __OCX_ENV_STATE`.\n\n\
        A consented project the ledger does not yet show reports `active: not yet`, and applies at \
        the next prompt. If its lock is absent or stale, it reports `active: no`, names the \
        refusal and points at `ocx lock`.\n\n\
        Read-only: it never writes a consent stamp, never repairs the ledger and never emits a \
        plan. The repair gesture is `unset __OCX_ENV_STATE` (which destroys the priors, so a \
        new shell is the cleaner floor); this command is how you check it worked.\n\n\
        The output is diagnostics for a human to read and is never valid shell source. It is \
        deliberately not interchangeable with `ocx self activate`.\n\n\
        Exits 0 in every reportable state, including an inert shell: the reason is the \
        payload, not a failure. Exits 74 only when `$OCX_HOME` cannot be read.\n\n\
        The text output is the answer and nothing else. It shows where `$OCX_HOME` is, which \
        project is in effect, and whether the integration is active. When it is not active, it \
        also shows the reason and the line that says what to do. `--verbose` only adds the \
        evidence behind it. It is a rendering tier, not a payload, so `--format json` emits the \
        same complete report with or without it.\n\n\
        When a project is active the report names the grant that activated it: `grant` in JSON, \
        a `granted by:` line in text. The value is `stamp`, `path` or `namespace`.\n\n\
        A stamp outranks `[shell.consent]`, so a project can be active with nothing in \
        `config.toml` to explain it. A `namespace` grant authorizes the project's packages, never its own `[env]`.\n\n\
        Every text line starts with a fixed label and every interpolated value is quoted. No \
        line is valid export syntax in any supported shell dialect.\n\n\
        https://ocx.sh/docs/explanation/shell-integration/")]
    State(super::shell_state::ShellState),
}

impl Shell {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Shell::Allow(allow) => allow.execute(context).await,
            Shell::Completion(_) => {
                unreachable!("shell completion is handled in the static-command bypass in App::run")
            }
            Shell::Revoke(revoke) => revoke.execute(context).await,
            Shell::State(state) => state.execute(context).await,
        }
    }
}
