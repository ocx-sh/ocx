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
    ///
    /// Records a consent stamp for the project governing PATH (default: the
    /// current directory), so a new shell prompt applies that project's tools
    /// and environment. This is the same stamp `ocx add`, `ocx lock`,
    /// `ocx pull` and `ocx exec` write as a side effect - running a mutating
    /// command in a directory is itself consent; this is the way to record one
    /// on purpose.
    ///
    /// The stamp records the source set the project's `ocx.lock` resolves from
    /// at the time it is written. A tool added from a new registry or
    /// organisation invalidates it; run this again to consent to the wider set.
    ///
    /// Undo with `ocx shell revoke`. See which grant is in effect with
    /// `ocx shell state`. Grants that live in configuration instead - a
    /// directory under `[shell.consent] paths`, an organisation under
    /// `[shell.consent] namespaces` - are edited in `config.toml`, not here.
    ///
    /// Exits 64 when no `ocx.toml` governs PATH, and when PATH names the ocx
    /// home: the global toolchain is always consented and never carries a
    /// stamp.
    ///
    /// https://ocx.sh/docs/in-depth/shell-integration
    Allow(super::shell_allow::ShellAllow),
    /// Generate shell completion scripts.
    Completion(super::shell_completion::ShellCompletion),
    /// Withdraw a project's consent stamp.
    ///
    /// Removes the consent stamp for the project governing PATH (default: the
    /// current directory). The next shell prompt stops applying that project's
    /// tools and environment, unless a `[shell.consent]` grant still covers it;
    /// those live in `config.toml` and are removed by editing it.
    /// `ocx shell state` reports which grant is in effect.
    ///
    /// Revoking a project that carries no stamp is not an error: the result is
    /// the state you asked for either way.
    ///
    /// A later `ocx add`, `ocx lock`, `ocx pull`, `ocx exec`, `ocx remove` or
    /// `ocx update` in that directory writes the stamp again.
    ///
    /// Exits 64 when no `ocx.toml` governs PATH.
    ///
    /// https://ocx.sh/docs/in-depth/shell-integration
    Revoke(super::shell_revoke::ShellRevoke),
    /// Report the shell integration's state, and why it is inert when it is.
    ///
    /// Prints the decoded `__OCX_ENV_STATE` ledger as fields: what each scope
    /// applied, whether the watch-set fingerprint still matches, whether the
    /// project scope's constant priors are intact, and - above all - the
    /// enumerated reason the shell is not active. That reason is one of: no
    /// consent stamp and no matching grant, source-set drift naming the new
    /// source, the hook disabled naming the deciding rung and tier, yielded to
    /// direnv or mise naming the live signal observed, the ledger over cap, or
    /// the ledger absent versus corrupt.
    ///
    /// Read-only: it never writes a consent stamp, never repairs the ledger
    /// and never emits a plan. The repair gesture is `unset __OCX_ENV_STATE`
    /// (which destroys the priors, so a new shell is the cleaner floor); this
    /// command is how you check it worked.
    ///
    /// The output is diagnostics for a human to read and is never valid shell
    /// source - deliberately not interchangeable with `ocx self activate`.
    ///
    /// Exits 0 in every reportable state, including an inert shell: the reason
    /// is the payload, not a failure. Exits 74 only when `$OCX_HOME` cannot be
    /// read.
    ///
    /// https://ocx.sh/docs/in-depth/shell-integration
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
