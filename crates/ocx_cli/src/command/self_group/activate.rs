// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use ocx_lib::activate::ActivateMode;
use ocx_lib::activation::{self, Outcome, ProjectIdentity, SessionError, SessionInput};
use ocx_lib::cli::{ColorModeConfig, Theme};
use ocx_lib::env::Env;
use ocx_lib::file_structure::FileStructure;
use ocx_lib::setup::ocx_install_bin_path;
use ocx_lib::shell::reconcile::{self, CARRIER_KEY, Ledger, Plan, ScopeId};
use ocx_lib::shell::{escape, hook};
use ocx_lib::{ConfigInputs, ConfigLoader, ShellConfig, log, shell::Shell};

use crate::app::ContextOptions;
use crate::conventions::resolve_shell_arg;
use crate::options;

/// Emit shell activation lines for the current OCX installation.
///
/// Prints eval-safe shell lines to stdout that:
/// - Prepend the absolute resolved `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin`
///   path to `PATH`.
/// - Inject shell completions (unless `OCX_NO_COMPLETIONS=1`).
/// - Evaluate the global toolchain env (`ocx --global env --shell=NAME`).
///
/// Intended to be sourced from `$OCX_HOME/env.sh` at shell startup.
/// `env.sh` sets `OCX_HOME` before invoking `ocx self activate`, so the
/// absolute path is resolved from the binary's perspective — no shell-level
/// `$OCX_HOME` variable reference is emitted.
///
/// ```sh
/// if command -v ocx >/dev/null 2>&1; then
///     eval "$(ocx self activate --shell=sh)"
/// fi
/// ```
#[derive(Parser)]
pub struct SelfActivate {
    /// Target shell for activation output.
    ///
    /// Legal values: `bash`, `zsh`, `fish`, `ash`, `dash`, `ksh`, `sh`,
    /// `pwsh`, `elvish`, `nushell`, `batch` (`sh` == `dash`, POSIX alias).
    ///
    /// Must be supplied with `=` (`--shell=bash`). Bare `--shell` (no `=`)
    /// triggers autodetection from `$SHELL`/parent process; exit 64 if
    /// undetectable - pass `--shell=NAME` explicitly to override.
    ///
    /// When absent, defaults to autodetect (same as bare `--shell`).
    #[arg(
        long,
        value_enum,
        value_name = "SHELL",
        num_args = 0..=1,
        require_equals = true
    )]
    shell: Option<Option<Shell>>,

    /// Shell-completion injection policy.
    ///
    /// `--completion` forces completions on, `--no-completion` off; with
    /// neither, completions load only for an interactive session, and every
    /// shim states that shell-side with `--interactive`/`--no-interactive`
    /// rather than leaving the binary to probe a descriptor it has redirected.
    #[clap(flatten)]
    completion: options::Completion,

    /// Per-prompt hook installation policy.
    ///
    /// `--hook` forces the hook on, `--no-hook` off; with neither, the hook
    /// loads for an interactive session unless `[shell] hook` or
    /// `OCX_NO_HOOK` says otherwise. Decided once, at shell start.
    #[clap(flatten)]
    hook: options::hook::Hook,

    /// Session interactivity, as the calling shell measured it.
    ///
    /// Feeds the auto rung of both policies above, and nothing else — a shim
    /// that spelled its answer as `--hook` would take rung 2 and revoke
    /// `OCX_NO_HOOK` and `[shell] hook` for every shell it starts. With neither
    /// flag the binary falls back to its own terminal probe.
    #[clap(flatten)]
    interactive: options::Interactive,

    /// Reconcile this shell's environment for the current prompt.
    ///
    /// The per-prompt entry point the emitted hook body invokes. Hidden: it is
    /// machine surface, not something to type.
    // The `hide = true` precedent is flag-level, as on `command/login.rs:42`.
    #[clap(long = "reconcile", hide = true)]
    reconcile: bool,
}

impl SelfActivate {
    /// Context-free execution path — called from `app.rs` before `Context::try_init`.
    ///
    /// `self activate` runs on every shell startup and must not pay the full
    /// `Context::try_init` cost (ConfigLoader file walk, OCI client, OciIndex,
    /// PackageManager). It only needs a `FileStructure` to resolve the absolute
    /// `$OCX_HOME/symlinks/…/bin` path. `FileStructure::new()` reads `OCX_HOME`
    /// from the environment and is cheap to construct — no I/O beyond the env
    /// lookup.
    pub async fn execute(&self, options: &ContextOptions, color_config: ColorModeConfig) -> anyhow::Result<ExitCode> {
        if self.reconcile {
            // C-051 — the hook path exits 0, always. Malformed state degrades,
            // logs once at debug, and the prompt renders. Nothing reaches the
            // binary's stderr that a user would see anyway: the emitted body
            // discards it (A-21).
            let carrier = ocx_lib::env::var(CARRIER_KEY);
            if let Err(error) = self
                .run_reconcile(options, color_config, &FileStructure::new(), carrier.as_deref())
                .await
            {
                log::debug!("per-prompt reconcile degraded, emitting nothing: {error:#}");
            }
            return Ok(ExitCode::SUCCESS);
        }
        self.run_startup(options).await
    }

    /// Resolve the target shell.
    ///
    /// Bare `--shell` (`Some(None)`) or absent (`None`) both trigger
    /// autodetect; explicit `--shell=bash` (`Some(Some)`) passes through. The
    /// `self activate` bare-absent case differs from `ocx env` / `ocx package
    /// env`, where absent means "use the default format path".
    ///
    /// `self.shell.or(Some(None))` collapses both "absent" and "bare" into
    /// `Some(_)`, so `resolve_shell_arg` always returns `Some` here. The
    /// `.expect()` documents that invariant — `unreachable!()` masked it as an
    /// arm of the outer match (Q-W3).
    fn target_shell(&self) -> anyhow::Result<Shell> {
        let shell_arg = self.shell.or(Some(None));
        Ok(resolve_shell_arg(shell_arg)?
            .expect("resolve_shell_arg(Some(_)) always returns Some; see shell_arg remap above"))
    }

    /// The shell-start path: emit the activation stream, and nothing else.
    ///
    /// **No diagnostic of any kind is emitted here** (A-21). Not a suppressed
    /// one — the channel does not exist on this path. The summary line, the
    /// inert-project hint, the over-cap line, the direnv/mise yield line and the
    /// managed-strip reason all ride the first `--reconcile` run, which the
    /// first prompt of every shell always performs (C-051): layer 2's fast path
    /// has no recorded fingerprint to compare against, and "no record" counts as
    /// changed.
    async fn run_startup(&self, options: &ContextOptions) -> anyhow::Result<ExitCode> {
        let shell = self.target_shell()?;

        // Resolve the absolute install bin path from the OCX CLI symlink.
        // `env.sh` guarantees OCX_HOME is set before running `ocx self activate`,
        // so `FileStructure::new()` already knows the correct root via OCX_HOME.
        // Constructing FileStructure directly avoids the full Context::try_init
        // overhead (OCI client, OciIndex, PackageManager) on every shell startup.
        let file_structure = FileStructure::new();
        let bin_path = ocx_install_bin_path(&file_structure);
        // C-001 — the store owns the spelling; never a literal `toolchain/bin`
        // join here.
        let toolchain_bin = file_structure.toolchain.bin();

        // A-12's other half. The per-prompt hook has resolved this ladder since
        // 2026-09-06 (`global_prompt_entries`); the login stream did not, so a
        // global `activate = "bin"` still composed a full env envelope on every
        // shell start — the very symptom A-12 records as closed. The gate sits
        // here, at the emitter, for A-12's own reason: the resolver's other
        // caller is `ocx --global env` / `ocx --global exec`, an explicit user
        // request that composes under any mode.
        //
        // One small TOML parse on the startup path, next to the `ConfigLoader`
        // pass `load_shell_config` already makes. It cannot fail: a missing,
        // unreadable or unparseable `$OCX_HOME/ocx.toml` leaves the file tier
        // absent and falls through to `OCX_TOOLCHAIN_ACTIVATE` and then the
        // `env` floor, so a corrupt file still composes and no login ever
        // breaks over it.
        let activate = global_activate_mode(file_structure.root()).await;

        // C-042 Option C — `[shell]` is read **once**, here, through one
        // `ConfigLoader` pass. Not baked into the shim (byte-identical across
        // installs), not exported as `OCX_NO_HOOK=1` (an exported toggle leaks
        // into every child), not flags-and-env-only (that fails
        // `self setup --[no-]hook`).
        let (shell_config, tiers) = load_shell_config(options).await;
        // C-038 rung 5's input, in one place for both ladders: the shim's own
        // `--interactive`/`--no-interactive`, and only failing that the probe.
        let interactive = self.interactive.resolve_probed();

        // Generate the shell-completion script to emit inline when completions
        // are enabled for this session (see `Completion::enabled`). The auto
        // signal is `interactive` above, when neither `--completion` nor
        // `--no-completion` is passed.
        let load_completions = self
            .completion
            .enabled(interactive, shell_config.as_ref().and_then(|shell| shell.completions));
        let completion = load_completions.then(|| generate_completion_inline(shell)).flatten();

        let hook_enabled = self
            .hook
            .enabled(interactive, shell_config.as_ref().and_then(|shell| shell.hook));
        // Discovery happens here and is recorded into the emitted body: the
        // per-prompt path stats this list and parses nothing (C-019, C-042).
        let watch = if hook_enabled {
            // No ledger exists at shell start, so the determinacy distinction
            // cannot arise here: there is no scope to retain.
            let project = match resolve_walk(options, &Ledger::empty()).await {
                Walk::Resolved(project) => Some(project),
                Walk::Determinate | Walk::Indeterminate => None,
            };
            reconcile::watch_paths(
                &file_structure,
                project.as_ref().map(|project| project.config_path.as_path()),
                project.as_ref().map(|project| project.key.as_str()),
                Some(&tiers),
            )
        } else {
            Vec::new()
        };

        emit_activation(&LoginStream {
            shell,
            bin_path: &bin_path,
            toolchain_bin: &toolchain_bin,
            activate,
            completion: completion.as_deref(),
            hook_binary: hook_enabled.then(|| ocx_binary_path(&bin_path)).as_deref(),
            wrapper_binary: hook_enabled
                .then(|| wrapper_binary_path(&file_structure, &bin_path))
                .as_deref(),
            watch: &watch,
            seed: hook_enabled.then(|| seed_carrier(&tiers, &watch)).flatten().as_deref(),
        });
        Ok(ExitCode::SUCCESS)
    }
}

/// The per-prompt path.
///
/// Two properties are contractual and easy to lose:
///
/// - **`--reconcile` bypasses [`options::Hook::enabled`] entirely** (C-041). It
///   runs in a fresh process with no `configured` value, and reading one would
///   violate C-042's zero-config rule. Consequence, stated so it is discovered
///   here and not in a bug report: `OCX_NO_HOOK=1` exported mid-session takes
///   effect at the **next shell start**, not the next prompt.
/// - **The fast path reads no config at all** — flags, env, the ledger's
///   fingerprint and watch-set stats only. `ConfigLoader` runs again only once
///   the fingerprint has already decided a recomposition is needed.
impl SelfActivate {
    /// `file_structure` and `carrier` are parameters rather than ambient reads
    /// so the C-042 ordering is observable from a test: the short-circuit has to
    /// be provably *reached before* `Context::try_init`, and a test cannot force
    /// that through `$OCX_HOME` and `$__OCX_ENV_STATE` without mutating the
    /// process environment out from under every other test in the binary.
    async fn run_reconcile(
        &self,
        options: &ContextOptions,
        color_config: ColorModeConfig,
        file_structure: &FileStructure,
        carrier: Option<&str>,
    ) -> anyhow::Result<()> {
        let shell = self.target_shell()?;

        // The carrier is untrusted input (C-007): decoding it names the revert
        // set and supplies the equality operand. Nothing below builds a path
        // from it.
        let ledger = carrier.and_then(Ledger::decode).unwrap_or_else(Ledger::empty);

        // C-028 — the only project bytes read before consent is established are
        // the CWD walk's `stat` calls and (below) the `ocx.lock` parse the
        // source-set predicate requires. `ProjectConfig` deserialization comes
        // after, inside `Context::try_init`'s composition.
        let walk = resolve_walk(options, &ledger).await;
        // A-11 — an indeterminate walk retains the scope and emits nothing. The
        // alternative is tearing down a correctly-applied environment because a
        // `.git` probe returned `EACCES` for one prompt, and then rebuilding it
        // on the next: PATH flapping, which is the failure class this whole
        // design exists to remove. Emitting nothing also leaves the stamp
        // unrefreshed, so the next prompt retries rather than latching.
        if matches!(walk, Walk::Indeterminate) {
            log::debug!("the project walk was indeterminate; retaining the recorded scope unchanged");
            // Deliberately no checkpoint: the stamp stays stale so the next
            // prompt retries rather than latching on a transient probe error.
            return Ok(());
        }
        let project = match walk {
            Walk::Resolved(project) => Some(*project),
            Walk::Determinate | Walk::Indeterminate => None,
        };
        let watch = reconcile::watch_paths(
            file_structure,
            project.as_ref().map(|project| project.config_path.as_path()),
            project.as_ref().map(|project| project.key.as_str()),
            // A-13 — the recorded list, seeded by the shell-start pass that saw
            // `--config`. Empty means no record yet; `watch_paths` re-derives.
            (!ledger.tiers.is_empty()).then_some(ledger.tiers.as_slice()),
        );
        let project_dir = project.as_ref().map(|project| project.dir.clone());
        // Pure — the ordered path list, no `stat` — so it costs nothing on the
        // stat-only path below, which returns before ever reading it.
        let watch_fingerprint = reconcile::watch_set_fingerprint(&watch);
        let fingerprint = {
            let watch = watch.clone();
            let project_dir = project_dir.clone();
            // One blocking hop for the whole watch set rather than a
            // `spawn_blocking` per `stat`: the point of C-044 is that this is
            // cheaper than the exec that reached it.
            tokio::task::spawn_blocking(move || reconcile::current_fingerprint(&watch, project_dir.as_deref())).await?
        };

        // C-042 — the negative-consent cache. A fresh clone with no grant would
        // otherwise pay a full loader pass plus a lock parse on **every**
        // prompt. Only the negative verdict is cached: an `Activate` verdict is
        // always re-derived, because caching it would make the ledger a consent
        // input, which C-007 forbids.
        if activation::is_stat_only(&ledger, &fingerprint) {
            // Nothing to apply, but the guard did fire, so the stamp still has
            // to advance or every prompt execs (C-044).
            emit(hook::checkpoint(shell));
            return Ok(());
        }

        let context = crate::app::Context::try_init(
            options,
            color_config,
            crate::app::ManagedConfigGate {
                // The prompt is not the place to refuse over a missing managed
                // snapshot, and it is not the place to adopt a new one either.
                enforce_required: false,
                onboarding: false,
            },
        )
        .await?;

        // A-44 — the ocx home toolchain is ALWAYS consented, so it is resolved
        // here, ahead of the session: before the walk's project, before the lock
        // read, before `evaluate_with_stamp`. It stays in this crate because the
        // login exporter owns it — `--env` overrides and group selection are
        // argv concerns a prompt never has.
        let target = crate::conventions::platform_or_default(None);
        let global = global_prompt_entries(&context, &target).await?;
        let session = activation::session(SessionInput {
            global,
            manager: context.manager(),
            local_index: context.local_index(),
            concurrency: context.concurrency(),
            file_structure: context.file_structure(),
            config: context.config(),
            target: &target,
            project: project.as_ref(),
        })
        .await;
        let outcome = match session {
            Ok(outcome) => outcome,
            // A-21 — these two are the user's to fix, and propagating them
            // exits 65 with an empty stdout and a stderr the emitted hook
            // discards: edit `ocx.toml`, forget `ocx lock`, and from then on
            // every prompt silently stops tracking the project with no line
            // printed. Report them on the prompt instead.
            Err(error @ SessionError::Lock(_)) => {
                for line in refusal_lines(shell, &ledger, &error) {
                    println!("{line}");
                }
                // No checkpoint, exactly as the indeterminate walk above: the
                // stamp stays stale so the next prompt retries rather than
                // latching on a lock the user is about to regenerate.
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        // `plan_for` reaches `std::fs::canonicalize`, once per owned prefix
        // (`owned_spellings`) — blocking work on the runtime thread, and not a
        // GC path. It hops, like the fingerprint fold above. The three values
        // travel in and back out rather than being cloned: `Env`, `Ledger` and
        // `Outcome` all deep-copy, and every one of them is read again below.
        let owned_root = file_structure.root().to_path_buf();
        let (plan, ledger, outcome, current) = tokio::task::spawn_blocking(move || {
            let current = Env::new();
            // C-063 — the owned set is `$OCX_HOME` plus the consented, in-scope
            // project's own toolchain home, and nothing else. `owned_prefixes` is
            // read by `repair_owned_segments` as a **deletion authority over a
            // live shell's PATH**, so a prefix listed here is a promise that
            // every segment under it either comes from this prompt's desired set
            // or is removed. `Outcome::owned_home` is `None` on every arm that
            // did not reach a `ConsentProof`, which is what keeps the widening
            // strictly after consent rather than merely usually after it.
            let plan = {
                let mut owned: Vec<&Path> = vec![owned_root.as_path()];
                owned.extend(outcome.owned_home.as_deref());
                activation::plan_for(&ledger, &outcome, &owned, &current)
            };
            (plan, ledger, outcome, current)
        })
        .await?;

        if matches!(options.format.mode(), options::FormatMode::Json) {
            // C-048 — nushell's channel: the `Plan` itself, not shell text. It
            // carries no carrier and no gate: nushell's hook is inlined in its
            // shim and has no baked watch set to go stale (A-24).
            emit_plan_json(&plan);
            return Ok(());
        }

        // #347 — the gate is baked at emission time, so a watch set that grew a
        // member (entering a project is the headline case) leaves the shell
        // gating on the list it was handed at startup. Compared here, redefined
        // below, and recorded as `ws` in the same breath by `reconcile_lines` —
        // a `ws` that advanced without the emission would describe a gate the
        // shell does not have, and the staleness would then be permanent.
        let regate = watch_fingerprint != ledger.ws;
        let gate = regate
            .then(|| hook::redefinition(shell, &ocx_binary_path(&ocx_install_bin_path(file_structure)), &watch))
            .flatten();
        // The summary line's ink is decided here and passed down resolved, so
        // `reconcile::summary` asks the environment nothing (A-21).
        //
        // `relayed`, not `stderr`: this process does not print that line. It
        // writes shell source on a stdout the hook captures in a command
        // substitution, and the *shell* prints the line on its own stderr when
        // it evaluates it — while this process's stderr is `/dev/null`, because
        // every emitted hook redirects it (`shell::hook`, POSIX / fish / pwsh).
        // So `Auto`'s per-stream tty fallback answers `false` in every real
        // terminal, and gating on it would ship a decision that can never be
        // told from never having run. `options::Interactive::resolve_probed`
        // records the same trap costing the same mistake one module over.
        //
        // What makes the terminal-bound default *right* rather than merely
        // convenient: the hook's own existence is the interactivity proof.
        // `Hook::enabled`'s last rung is the shell's own answer, stated by the
        // shim through `--interactive` / `--no-interactive`, and a
        // non-interactive shell never registers the per-prompt hook. If
        // `--reconcile` is running at all, its output is bound for an
        // interactive shell's stderr by construction — stronger evidence than
        // any descriptor this process can stat. Every deliberate refusal still
        // decides: `--color never`, `NO_COLOR`, `CLICOLOR=0`, `TERM=dumb`.
        let theme = Theme::new(color_config.relayed);
        for line in reconcile_lines(
            shell,
            &ledger,
            &fingerprint,
            regate.then_some(watch_fingerprint.as_str()),
            &outcome,
            &plan,
            &current,
            &theme,
        ) {
            println!("{line}");
        }
        // After the plan and the carrier, before the checkpoint: the redefined
        // body is what the *next* prompt runs, and the checkpoint below is what
        // makes that next prompt quiet.
        emit(gate);
        // Last, and only on the path that got here: a degraded run returns
        // early above and emits no checkpoint, so its stamp stays stale and the
        // next prompt retries (D2 — every prompt re-converges).
        emit(hook::checkpoint(shell));
        Ok(())
    }
}

/// What a prompt emits when the consenting project's lock is absent or stale
/// (A-21).
///
/// **No plan, and that is the whole design.** `session`'s contract for an `Err`
/// is "emit nothing at all", which is the fail-safe outcome and not a degraded
/// one: a lock that is momentarily stale mid-`git checkout` must retain the
/// project scope rather than tear it down, and a plan built from a `desired`
/// missing that scope would revert it — the direction A-11 refuses for an
/// indeterminate walk, for the same reason. So the environment is left exactly
/// as it stands and only the sentence changes.
///
/// The carrier is re-emitted **unchanged but for `messages_fp`**: it records
/// that the line has been said without moving the fingerprint, the verdict or
/// either scope. Without it the message would print before every prompt until
/// the user runs `ocx lock`, which is the noise `messages_fp` exists to stop; a
/// fresh shell has an empty carrier and so is told once on its own first prompt.
fn refusal_lines(shell: Shell, previous: &Ledger, error: &SessionError) -> Vec<String> {
    let message = format!("ocx: {error}");
    let announced = activation::announcing(previous, std::slice::from_ref(&message));
    let mut lines = ledger_lines(shell, previous, &announced);
    if announced.messages_fp != previous.messages_fp {
        lines.extend(shell.emit_message(message));
    }
    lines
}

/// Every line one reconcile emits, in order (C-011, A-21).
///
/// Built as a `Vec` rather than printed inline so the emission order is a
/// contract a unit test can read — the ordering is the whole product here.
#[expect(
    clippy::too_many_arguments,
    reason = "the emission's inputs: where it goes, what it replaces, and what it renders"
)]
fn reconcile_lines(
    shell: Shell,
    previous: &Ledger,
    fingerprint: &str,
    regated: Option<&str>,
    outcome: &Outcome,
    plan: &Plan,
    current: &Env,
    theme: &Theme,
) -> Vec<String> {
    // C-018's capture ordering lives inside `next_ledger`, which is handed the
    // pre-global environment and derives the post-global one itself.
    let mut next = activation::next_ledger(previous, fingerprint, outcome, current);
    // #347 — `Some` exactly when the caller is emitting a redefined gate for
    // this membership. `next_ledger` carries the old value forward, so a caller
    // that reconciled without touching the gate records the gate the shell
    // still has.
    if let Some(membership) = regated {
        next.ws = membership.to_owned();
    }
    let mut lines = plan_lines(shell, plan);
    lines.extend(ledger_lines(shell, previous, &next));
    lines.extend(message_lines(shell, outcome, plan, previous, &next, theme));
    lines
}

/// Render the `Plan` as shell code.
///
/// **`restores` precedes `removes`, and the order is load-bearing.** The three
/// sets are key-disjoint only pairwise from `sets`; `removes` and `restores` can
/// name the **same key** whenever the two scopes disagreed about its kind — a
/// global path-kind entry and a project constant sharing a key, both retiring in
/// one prompt. The project's prior was captured *after* global applied (C-018),
/// so it still contains global's element. Emitting the removal first runs it
/// against the constant's live value, where the element is not present, and the
/// restore then writes the retired element straight back. Restoring first puts
/// the prior in place and lets the removal do its job on it.
///
/// `sets` still leads: it shares no key with either of the other two, because
/// `plan` only retires an element or a constant `desired` no longer declares.
fn plan_lines(shell: Shell, plan: &Plan) -> Vec<String> {
    let mut lines: Vec<String> = plan.sets.iter().filter_map(|entry| set_line(shell, entry)).collect();
    lines.extend(plan.restores.iter().filter_map(|(key, prior)| match prior {
        // A-05 — `Some("")` is a set-but-empty prior and restores through
        // `export_constant`, never `unset`.
        Some(value) => shell.export_constant(key, value),
        None => shell.unset(key),
    }));
    lines.extend(
        plan.removes
            .iter()
            .filter_map(|(key, element, separator)| shell.remove_list_element(key, element, separator.as_deref())),
    );
    lines
}

/// One apply line for one desired entry.
///
/// Deliberately not `conventions::emit_lines`: that helper prints, and its
/// `None` arms write `# ocx:` diagnostics to a stderr the emitted hook body
/// discards unconditionally (A-21). Every entry reaching here has already passed
/// `plan`'s A-10 gate, so those arms are unreachable on this path anyway.
fn set_line(shell: Shell, entry: &ocx_lib::package::metadata::env::entry::Entry) -> Option<String> {
    use ocx_lib::package::metadata::env::list::DEFAULT_SEPARATOR;
    use ocx_lib::package::metadata::env::modifier::ModifierKind;

    match entry.kind {
        ModifierKind::Path => shell.export_path(&entry.key, &entry.value),
        ModifierKind::Constant => shell.export_constant(&entry.key, &entry.value),
        ModifierKind::List => shell.export_list(
            &entry.key,
            &entry.value,
            entry.separator.as_deref().unwrap_or(DEFAULT_SEPARATOR),
        ),
    }
}

/// Emit `__OCX_ENV_STATE` for the next prompt, plus A-01's over-cap marker line.
///
/// A-01 wants **one line per transition into the over-cap state**, not one per
/// prompt: without the comparison against `previous`, every `cd` inside an
/// over-cap project reprints an abandonment the user was told about on entry.
/// The previous state is the decoded carrier's own `over_cap` list — the
/// marker `encode` wrote last prompt — so the transition is observed from the
/// record rather than inferred.
fn ledger_lines(shell: Shell, previous: &Ledger, next: &Ledger) -> Vec<String> {
    let Some(encoded) = next.encode() else {
        // Even the marker failed to encode: omit the variable entirely rather
        // than carry a value the next prompt cannot decode (C-006 then treats
        // the ledger as absent, which is the fail-safe direction).
        return shell.unset(CARRIER_KEY).into_iter().collect();
    };

    let mut lines: Vec<String> = shell.export_constant(CARRIER_KEY, &encoded).into_iter().collect();
    // A-01 — the abandoned scope is read back from the marker the carrier still
    // carries, never inferred from an absent carrier.
    for scope in Ledger::decode(&encoded)
        .map(|ledger| ledger.over_cap)
        .unwrap_or_default()
    {
        if previous.over_cap.contains(&scope) {
            // Already announced on the prompt that abandoned it. Re-announcing
            // is not new information, and a prompt is the one place a repeated
            // line is read as a new event.
            continue;
        }
        let name = match scope {
            ScopeId::Global => "global",
            ScopeId::Project => "project",
        };
        lines.extend(shell.emit_message(format!(
            "ocx: the {name} scope is too large to record; it will not be reverted"
        )));
    }
    lines
}

/// Every deferred diagnostic, as shell code that prints to stderr when eval'd
/// (A-21).
///
/// Each rides that arm's own value escaper and travels as a `printf` **format
/// argument, never the format string** — `Shell::emit_message` is the primitive
/// that guarantees both. `Batch` hosts no hook and returns `None` for all of
/// them.
fn message_lines(
    shell: Shell,
    outcome: &Outcome,
    plan: &Plan,
    previous: &Ledger,
    next: &Ledger,
    theme: &Theme,
) -> Vec<String> {
    let mut messages: Vec<String> = Vec::new();
    // The same "already announced on the prompt that did it" rule `ledger_lines`
    // applies to the over-cap marker, and for the same reason: a prompt is the
    // one place a repeated line is read as a new event. Every one of these
    // states a *condition* — direnv manages this directory, the `[env]` channel
    // is withheld, the managed tier dropped a consent payload — and a condition
    // that has not moved since the last prompt is not news. Without the
    // comparison the direnv line prints before every prompt for as long as the
    // shell sits in a direnv directory, which is the shell's whole life.
    if next.messages_fp != previous.messages_fp {
        messages.extend(outcome.messages.iter().cloned());
    }
    // The summary is exempt: it is already a delta between the two ledgers and
    // is `None` on a prompt that changed nothing.
    messages.extend(reconcile::summary(plan, previous, next, theme));
    messages
        .into_iter()
        .filter_map(|message| shell.emit_message(message))
        .collect()
}

/// The global toolchain tier's contribution to one prompt, gated on the global
/// toolchain's own `activate` mode (ADR `adr_toolchain_activation.md` D-3).
///
/// D-3 makes the user's own `$OCX_HOME/ocx.toml` the control for the clean-shell
/// case, so `activate` governs this tier exactly as a project's own file governs
/// the project tier.
///
/// # The gate is here, not in the resolver
///
/// [`resolve_global_pinned_env`](crate::command::toolchain_env::resolve_global_pinned_env)
/// has a second caller — `ocx --global env` / `ocx --global exec`, an **explicit
/// user request**. `activate` decides how a toolchain reaches a shell
/// *automatically*; it never decides what a command the user typed prints. So
/// the gate sits at this call site, the automatic one, and the resolver keeps
/// both its second caller and its never-fail posture unchanged.
///
/// # `bin` and `none` are the same `PATH` at this tier
///
/// C-059 makes the two global session directories
/// ([`ocx_install_bin_path`] and `$OCX_HOME/toolchain/bin`) desired
/// unconditionally, in every mode — they are session-level facts, and dropping
/// them would have `repair_owned_segments` delete the registration `ocx self
/// setup` just wrote. So at the **global** tier `bin` and `none` both compose
/// nothing and produce an identical `PATH`: under either, a global tool resolves
/// through its trampoline in `$OCX_HOME/toolchain/bin`. The two modes differ
/// only at the project tier. That is a consequence of C-059, not a gap.
///
/// # Cost
///
/// Skipping the resolver also skips the global lock read, the offline manager
/// clone and the per-tool `find`, so a `bin`/`none` prompt is strictly cheaper
/// than an `env` one.
async fn global_prompt_entries(
    context: &crate::app::Context,
    target: &ocx_lib::oci::Platform,
) -> anyhow::Result<Vec<ocx_lib::package::metadata::env::entry::Entry>> {
    // The `env` arm parses `$OCX_HOME/ocx.toml` a second time, inside
    // `resolve_global_pinned_env`, and that is deliberate. Threading this
    // parsed config into that resolver would change its signature and its
    // never-fail contract — the two things that keep the explicit-request path
    // (`ocx --global env` / `ocx --global exec`) and this gate's regression
    // guard honest about where the gate sits. The `bin`/`none` arm meanwhile
    // *removes* a lock read, an offline manager clone and the per-tool `find`,
    // so the net per-prompt cost is favourable; one more small TOML parse is
    // noise against `RECONCILE_BUDGET_MS`. Do not "optimise" this away.
    match global_activate_mode(context.file_structure().root()).await {
        ocx_lib::activate::ActivateMode::Env => {
            Ok(
                // No CLI tier: the per-prompt hook is not an invocation of
                // `ocx --global env`, so it has no `--pinned`/`--no-pinned` to
                // pass. `None` is "neither flag was given", which leaves the
                // global `ocx.toml`'s `pinned` key and `OCX_TOOLCHAIN_PINNED`
                // deciding the lane exactly as they did before the resolver
                // took the argument at all.
                crate::command::toolchain_env::resolve_global_pinned_env(context, target, &[], &[], None)
                    .await?
                    .map_or_else(Vec::new, |(entries, ..)| entries),
            )
        }
        ocx_lib::activate::ActivateMode::Bin | ocx_lib::activate::ActivateMode::None => Ok(Vec::new()),
    }
}

/// Resolve the `activate` ladder over `$OCX_HOME/ocx.toml` — the global tier's
/// own file, read through the one resolver both tiers share.
///
/// [`activation::activate_mode`] is called rather than a re-spelled
/// `Ladder { .. }.resolve(ACTIVATE_FLOOR)`: `ocx_lib::activate`'s module doc
/// names a second spelling of the floor as drift, and one resolver with two
/// callers is the point.
///
/// # Never fails
///
/// A missing, unreadable or unparseable global `ocx.toml` yields
/// [`ProjectConfig::default`], whose `activate` is `None` — so the **file tier
/// is absent** and the ladder falls through to `OCX_TOOLCHAIN_ACTIVATE` and then
/// to `ACTIVATE_FLOOR` (`env`), so a corrupt global file still composes — the
/// same posture `resolve_global_pinned_env` takes over the very same bytes
/// (`Err(_) =>` empty). This is the per-prompt path: it must not turn a
/// malformed file into an error, a warning on the prompt, or a silent `none`.
async fn global_activate_mode(home: &Path) -> ocx_lib::activate::ActivateMode {
    let config =
        ocx_lib::project::ProjectConfig::from_path(&ocx_lib::project::ProjectConfig::global_manifest_path(home))
            .await
            .unwrap_or_default();
    activation::activate_mode(&config)
}

/// Print one optional emitted line.
fn emit(line: Option<String>) {
    if let Some(line) = line {
        println!("{line}");
    }
}

/// Serialize the `Plan` for the nushell channel (C-011, C-048).
fn emit_plan_json(plan: &Plan) {
    match serde_json::to_string(plan) {
        Ok(json) => println!("{json}"),
        // A prompt never fails over a serialization error; nushell's applier
        // reads an absent `v` as "apply nothing this prompt" (A-23).
        Err(error) => log::debug!("could not serialize the reconcile plan: {error}"),
    }
}

/// What one CWD walk decided (A-11, C-019).
///
/// **Three outcomes, not two.** `ConfigLoader::project_path` collapses a
/// non-`NotFound` candidate error, a fail-closed `.git` boundary and a genuine
/// miss into the same `Ok(None)`, so the walk's return value cannot tell them
/// apart — and reverting a correctly-applied project scope because a `.git`
/// probe momentarily returned `EACCES` is exactly the PATH-flapping failure this
/// design exists to prevent. A-11 therefore requires one determinacy check
/// before any revert.
enum Walk {
    /// The walk resolved a project. Determinate by construction: the walk
    /// demonstrably worked, so a *different* hit is a genuine switch (C-018)
    /// and needs no further check.
    Resolved(Box<ProjectIdentity>),
    /// No hit, and the previously recorded scope is genuinely gone — a real
    /// leave, a real deletion, or `OCX_NO_PROJECT=1`. Revert normally.
    Determinate,
    /// No hit, but the previously recorded scope still looks alive. The walk's
    /// answer is **indeterminate**: retain the scope unchanged and emit nothing.
    Indeterminate,
}

/// Resolve the project tier's identity **without deserializing `ocx.toml`**
/// (C-028), classifying a miss by A-11's determinacy rule.
///
/// `ConfigLoader::project_path` is the CWD walk and nothing else — `stat` calls
/// against the precedence chain. Any failure degrades at debug level, never to a
/// broken prompt (C-051).
async fn resolve_walk(options: &ContextOptions, ledger: &Ledger) -> Walk {
    let cwd = match ocx_lib::env::current_dir() {
        Ok(cwd) => Some(cwd),
        // A-11 — the CWD itself was unlinked. Degrade to "no project resolved
        // this prompt", log at debug, and **never** fall back to a cached CWD.
        // With no CWD the ancestor test cannot be evaluated at all, which is the
        // definition of indeterminate.
        Err(error) => {
            log::debug!("the working directory is unreadable, so no project resolved this prompt: {error}");
            None
        }
    };

    let resolved = match cwd.as_deref() {
        Some(cwd) => match ConfigLoader::project_path(Some(cwd), options.project.as_deref()).await {
            Ok(path) => path,
            Err(error) => {
                log::debug!("project discovery degraded, treating this prompt as project-free: {error}");
                None
            }
        },
        None => None,
    };

    if let Some(config_path) = resolved {
        match ProjectIdentity::resolve(config_path).await {
            Ok(identity) => return Walk::Resolved(Box::new(identity)),
            // The walk found an `ocx.toml` but the filesystem would not resolve
            // it. That is a transient failure about a project that demonstrably
            // exists, so it is the indeterminate case, not a leave.
            Err(error) => log::debug!("the resolved project has no usable identity this prompt: {error}"),
        }
    }

    // A-11's determinacy probe reaches `std::fs::symlink_metadata`, so it hops
    // like every other filesystem read on this path. Only the recorded
    // directory crosses, not the whole ledger — the predicate reads nothing
    // else, and a `Ledger` clone would deep-copy both scopes on every revert.
    let recorded_dir = ledger.scopes.project.as_ref().map(|scope| scope.dir.clone());
    let has_recorded = recorded_dir.is_some();
    let indeterminate = match tokio::task::spawn_blocking(move || {
        activation::walk_is_indeterminate(recorded_dir.as_deref(), cwd.as_deref())
    })
    .await
    {
        Ok(indeterminate) => indeterminate,
        // Unknowable is the fail-safe answer, exactly as for an unreadable CWD:
        // retain a recorded scope rather than tear it down over a join error.
        Err(error) => {
            log::debug!("the determinacy probe task failed: {error}");
            has_recorded
        }
    };
    if indeterminate {
        Walk::Indeterminate
    } else {
        Walk::Determinate
    }
}

/// Read `[shell]` — and the config-tier path list — through the one
/// `ConfigLoader` pass C-042 allows.
///
/// Shared with `ocx shell completion --if-enabled`, which resolves the same
/// completions ladder in its own process: the POSIX and elvish shims inject
/// completions through a second invocation rather than this stream, and that
/// invocation needs rung 4 from the same place this one reads it.
///
/// This is the **only** run that knows the `--config` overlay: the emitted hook
/// body invokes `--reconcile` with no `--config`, so the tier list has to be
/// recorded here or the explicit consent channel (A-33) is invisible from every
/// prompt after this one.
///
/// A malformed ambient config must not break a shell start: `self activate` is
/// dispatched before `Context::try_init` precisely so it survives one, and this
/// pass keeps that property by degrading to "no tier set the rung" (which is
/// rung 5, auto) rather than propagating.
pub(crate) async fn load_shell_config(options: &ContextOptions) -> (Option<ShellConfig>, Vec<PathBuf>) {
    let cwd = ocx_lib::env::current_dir().ok();
    let loaded = ConfigLoader::load_with_local_view(ConfigInputs {
        explicit_path: options.config.as_deref(),
        explicit_project_path: options.project.as_deref(),
        cwd: cwd.as_deref(),
    })
    .await;
    match loaded {
        Ok(loaded) => (loaded.merged.shell, loaded.config_tier_paths),
        Err(error) => {
            log::debug!("could not read [shell]; falling back to the auto rung: {error}");
            (None, Vec::new())
        }
    }
}

/// The seed carrier the startup path exports, carrying **only** the config-tier
/// list (A-13, A-33).
///
/// The explicit `--config` tier is a consent-bearing channel, and this process
/// is the only one that ever sees it: the emitted hook body invokes
/// `--reconcile` with no `--config`. Handing the list forward through the
/// carrier is what lets every later prompt stat it, so a grant added to that
/// file expires the cached `inert` verdict at the next prompt instead of at the
/// next shell start.
///
/// It is env-setting shell code, not a diagnostic, so A-21's "no message on the
/// startup path" is untouched. It also does not make the first prompt skip its
/// reconcile: `fp` is empty, so it can never equal a real fingerprint, and the
/// shell-side guard still fires on the unset `__ocx_pwd`. `Ledger::decode` of
/// this value yields empty scopes, so the first prompt plans against exactly
/// what C-005 specifies.
///
/// `None` when there is nothing worth recording, so no carrier is exported.
fn seed_carrier(tiers: &[PathBuf], watch: &[PathBuf]) -> Option<String> {
    if tiers.is_empty() {
        return None;
    }
    Ledger {
        tiers: tiers.to_vec(),
        // The membership this very stream is about to bake into the gate
        // (#347). Without it the first prompt of every shell would compare
        // against an empty `ws`, decide the gate was stale, and redefine a hook
        // body byte-identical to the one just emitted.
        ws: reconcile::watch_set_fingerprint(watch),
        ..Ledger::empty()
    }
    .encode()
}

/// The resolved absolute `ocx` binary inside `bin_path`.
///
/// A-34 — resolution is through `current`, unconditionally: `OCX_BINARY_PIN` has
/// no effect on the emitted hook. The pin's three consumers are all re-entrant
/// invocations where a running ocx pins a child back to its own `current_exe()`;
/// the interactive shell's own top-level resolution is upstream of that
/// mechanism and structurally cannot consult it.
fn ocx_binary_path(bin_path: &Path) -> PathBuf {
    bin_path.join(if cfg!(windows) { "ocx.exe" } else { "ocx" })
}

/// The `ocx` the emitted **wrapper function** calls.
///
/// A global toolchain may pin `ocx` — D-4 removed the refusal that used to stop
/// it — and after C-060's re-ordering `$OCX_HOME/toolchain/bin` sits in front of
/// the installed binary, so a bare `ocx` on `PATH` resolves the pin. The wrapper
/// is a shell *function* of the same name, and a function shadows `PATH`
/// outright: left on the install path it would quietly re-introduce, for exactly
/// the interactive shells the pin is typed in, the precedence the ordering just
/// removed.
///
/// Resolved once at login rather than per call. C-045 requires every emitted
/// invocation to name an absolute binary, and the alternative — a
/// `PATH`-resolving `command ocx` written four times, once per shell dialect —
/// trades that contract away. A pin added after the shell started therefore
/// lands at the next login, like every other decision this stream bakes.
///
/// Falls back to the installed binary whenever the toolchain renders no `ocx`,
/// which is the ordinary case.
fn wrapper_binary_path(file_structure: &FileStructure, install_bin: &Path) -> PathBuf {
    let pinned = ocx_binary_path(&file_structure.toolchain.bin());
    if pinned.is_file() {
        pinned
    } else {
        ocx_binary_path(install_bin)
    }
}

/// Everything one login stream is built from.
///
/// A borrowed request struct rather than eight positional parameters, the same
/// shape and for the same reason as
/// [`RenderRequest`](ocx_lib::package_manager::tasks::render_toolchain::RenderRequest):
/// a positional list of two paths, two `Option<&str>`s, an `Option<&Path>`, a
/// slice and a mode reads as nothing at the call site, and `clippy::too_many_arguments`
/// says so at seven.
struct LoginStream<'a> {
    /// The shell whose idiom every emitted line is written in.
    shell: Shell,

    /// The installed `ocx`'s own `bin` directory — [`ocx_install_bin_path`]'s
    /// answer, prepended first so it lands **behind** the toolchain trampolines
    /// on `PATH`: it is the floor a bare `ocx` falls back to, not a lid over a
    /// toolchain that pins one (C-060).
    bin_path: &'a Path,

    /// The global rendered toolchain's trampolines, `$OCX_HOME/toolchain/bin`
    /// — [`ToolchainStore::bin`](ocx_lib::file_structure::ToolchainStore::bin)'s
    /// answer, never a literal join (C-001). Prepended last, so it lands
    /// frontmost of the two.
    toolchain_bin: &'a Path,

    /// The **global** tier's resolved mode. Gates exactly one line, the global
    /// env eval; neither `PATH` prepend consults it.
    activate: ActivateMode,

    /// The generated completion script to emit inline, or `None` when
    /// completions are disabled, the session is non-interactive, or the shell
    /// has no completion backend (see [`generate_completion_inline`]).
    completion: Option<&'a str>,

    /// The absolute binary the per-prompt hook body invokes, or `None` when the
    /// hook is disabled for this session.
    ///
    /// Deliberately **not** [`Self::wrapper_binary`]: the hook runs
    /// `self activate --reconcile` on every prompt, speaking the private
    /// `__OCX_ENV_STATE` ledger protocol, so a toolchain-pinned `ocx` of another
    /// version — or a broken one — would break the prompt of every shell rather
    /// than one command.
    hook_binary: Option<&'a Path>,

    /// The absolute binary the emitted `ocx` **wrapper function** invokes, or
    /// `None` when the hook is disabled (the wrapper is emitted only with it).
    ///
    /// See [`wrapper_binary_path`] for why this follows the toolchain pin where
    /// [`Self::hook_binary`] does not.
    wrapper_binary: Option<&'a Path>,

    /// The watch set the emitted hook body stats on each prompt.
    watch: &'a [PathBuf],

    /// The encoded tier-list carrier the first prompt reads, when there is one.
    seed: Option<&'a str>,
}

/// Emit all activation lines to stdout.
fn emit_activation(request: &LoginStream<'_>) {
    for line in activation_lines(request) {
        println!("{line}");
    }
}

/// The activation stream, in emission order.
///
/// Built as a `Vec` rather than printed inline: the order **is** the contract
/// here, and a test that reads it needs the stream as a value.
///
/// `activate` is the **global** tier's resolved mode, and it gates exactly one
/// line: the global-env eval. `toolchain_bin` is emitted in every mode — see
/// the two blocks below for why the asymmetry is the contract rather than an
/// oversight.
fn activation_lines(request: &LoginStream<'_>) -> Vec<String> {
    let &LoginStream {
        shell,
        bin_path,
        toolchain_bin,
        activate,
        completion,
        hook_binary,
        wrapper_binary,
        watch,
        seed,
    } = request;
    let mut lines = Vec::new();
    // ── Shell completions (emitted FIRST) ────────────────────────────────────
    // The completion block must lead the stream: clap_complete's PowerShell
    // output opens with `using namespace`, which `Invoke-Expression` (the pwsh
    // shim's loader) accepts only as the *first* statement — any earlier line
    // makes pwsh reject the whole script. Other shells are order-insensitive
    // here, and no completion block references `ocx` at definition time, so
    // emitting it before the PATH prepend is safe.
    if let Some(script) = completion {
        lines.push(script.to_owned());
    }

    // ── PATH prepend ─────────────────────────────────────────────────────────
    // Use the absolute resolved path — no $VAR references, so Shell::export_path
    // is safe here: each arm's own value escaper leaves a path carrying no shell
    // metacharacters byte-identical.
    //
    // Two session directories, in the order C-059/C-060 give them: the install
    // bin first so the later `toolchain_bin` prepend lands in front of it,
    // exactly as the reconciler's desired fold leaves them
    // (`SessionPath::install_bin` is "backmost of the three on PATH, so it is
    // emitted first"). A global toolchain that pins `ocx` therefore wins over
    // the installed binary, which is the whole point of being allowed to pin it
    // (D-4). Both are **unconditional in every mode** — dropping either in
    // `bin`/`none` would leave a login shell with no global tools at all
    // wherever the OS session-registration channel `ocx self setup` writes is
    // absent or ignored (a container, a host without systemd user env,
    // `--no-modify-path`, a non-interactive `sh` that never renders a prompt to
    // be repaired at).
    lines.extend(path_prepend_line(shell, bin_path));
    lines.extend(path_prepend_line(shell, toolchain_bin));

    // ── Global toolchain env ─────────────────────────────────────────────────
    // Evaluate the global toolchain env — in `env` mode only. `bin` and `none`
    // compose nothing here and reach their tools through the trampolines in the
    // `toolchain/bin` prepended above, which is the whole point of the mode; the
    // per-prompt arm has drawn this same line since A-12 (`global_prompt_entries`),
    // and a login stream that composed anyway is what made a global
    // `activate = "bin"` look inert for a whole session.
    //
    // In `env` mode the eval carries NO `OCX_ACTIVATED` state guard, and the
    // gate above is not one: it is a property of the toolchain's own manifest,
    // not of shell state, so it is identical for every shell of a session. The
    // emitted env lines never duplicate on a re-source — PATH uses idempotent
    // move-to-front (cmd included, via substring-delete; see
    // `Shell::export_path`), constants are absolute sets — and an *exported*
    // guard would leak into child processes (e.g. a VS Code Remote server whose
    // terminals inherit it) and wrongly suppress activation in a shell that
    // needs it. Re-sourcing still picks up a changed global toolchain.
    if activate == ActivateMode::Env {
        lines.push(format_global_env_eval(shell, &ocx_binary_path(bin_path)));
    }

    // ── Per-prompt hook + wrapper (emitted LAST) ─────────────────────────────
    // Last on purpose, and the ordering is load-bearing in both directions: the
    // hook body invokes the resolved absolute binary, so it must not run before
    // the PATH prepend has settled the stream it shares, and pwsh's
    // `using namespace` must still be the first statement of the whole stream.
    //
    // Registration is append-only and idempotent — re-sourcing an activation
    // stream registers nothing twice — and every arm that hosts no prompt hook
    // (batch, elvish, the strict-POSIX family, nushell) returns `None` here and
    // is a silent no-op.
    if let Some(binary) = hook_binary {
        // Before the registration: the hook body reads the carrier, so the seed
        // has to be in place by the time the first prompt fires.
        if let Some(seed) = seed {
            lines.extend(shell.export_constant(CARRIER_KEY, seed));
        }
        lines.extend(hook::registration(shell, binary, watch));
        // C-045 — a latency optimization for same-command-line chaining, never
        // the correctness floor. Every way of escaping the function name
        // degrades to next-prompt correctness rather than breaking.
        //
        // `wrapper_binary`, not `binary`: the wrapper is a shell function named
        // `ocx`, so it shadows `PATH` outright and has to follow the same
        // precedence `PATH` now gives — see [`wrapper_binary_path`].
        lines.extend(hook::wrapper(shell, wrapper_binary.unwrap_or(binary)));
    }

    lines
}

/// The PATH prepend line for the given shell using an absolute `bin_path`.
///
/// Uses [`Shell::export_path`] since the value is an absolute filesystem path
/// containing no shell variable references or metacharacters.
/// The PATH prepend line, or `None` when the arm cannot express it.
fn path_prepend_line(shell: Shell, bin_path: &std::path::Path) -> Option<String> {
    shell.export_path("PATH", &*bin_path.to_string_lossy())
}

/// Maps a shell to its `clap_complete` backend, or `None` when the shell has no
/// completion generator (ash/ksh/dash/batch/nushell).
fn completion_clap_shell(shell: Shell) -> Option<clap_complete::Shell> {
    use clap_complete::Shell as Clap;
    Some(match shell {
        Shell::Bash => Clap::Bash,
        Shell::Zsh => Clap::Zsh,
        Shell::Fish => Clap::Fish,
        Shell::Elvish => Clap::Elvish,
        Shell::PowerShell => Clap::PowerShell,
        // ash/ksh/dash/batch/nushell have no clap_complete backend.
        _ => return None,
    })
}

/// Generate the shell-completion script to emit inline into the activation
/// stream, or `None` when the shell has no completion backend.
///
/// Emitted directly into the eval'd activation stream rather than written to a
/// file: the stream is already `eval`/`Invoke-Expression`'d by the shim, so a
/// `complete -F` / `compdef` / `Register-ArgumentCompleter` block installs
/// completions with no file to manage, version-stamp, or read.
///
/// Delegates to [`crate::command::shell_completion::render_completion_script`]
/// — the single generator shared with `ocx shell completion`, which adds the
/// zsh `compinit` guard so the script registers wherever it is sourced. The one
/// activation-specific concern is order: PowerShell's `using namespace` must be
/// the first statement of the stream, which [`emit_activation`] guarantees by
/// emitting this block before the PATH prepend (verified on Windows PowerShell
/// 5.1 and PowerShell 7).
fn generate_completion_inline(shell: Shell) -> Option<String> {
    let clap_shell = completion_clap_shell(shell)?;
    let mut cmd = crate::app::Cli::command();
    let cmd_name = cmd.get_name().to_string();
    Some(crate::command::shell_completion::render_completion_script(
        &mut cmd, &cmd_name, clap_shell,
    ))
}

/// Format the global-env-eval line for `shell` without emitting.
///
/// On every shell this runs `<binary> --global env` guarded only by an
/// existence probe — PATH uses idempotent move-to-front (cmd included, via
/// substring-delete) and constants are absolute sets, so there is no
/// `OCX_ACTIVATED` state guard (an exported guard would leak into child shells,
/// e.g. a VS Code Remote server whose terminals inherit it, and suppress
/// activation where it is needed).
///
/// **C-045 — every invocation names the resolved absolute `binary`, and the
/// probe is a path test, never a name lookup.** The wrapper this same stream
/// emits is a shell *function* named `ocx`, and `command -v ocx` / `type -q ocx`
/// / `Get-Command ocx` all find functions — so a bare call inside the `$(…)`
/// here would execute the wrapper, capture its output into the env stream, and
/// eval it. A probe that consults the function table is the same defect as the
/// call, which is why both halves move together.
///
/// Two arms keep the bare form because neither can host the hazard:
/// [`Shell::Batch`] (cmd.exe has no functions at all) and [`Shell::Nushell`]
/// (hosts no wrapper — `shell::hook::wrapper` returns `None`, so no `ocx`
/// definition ever exists to shadow the call).
///
/// [`Shell::Elvish`] was in that list on a premise that stopped being true the
/// moment it gained a wrapper: `edit:add-var ocx~` puts a *function* named `ocx`
/// in the REPL namespace, and `has-external ocx` is a name lookup. Every shim
/// runs this stream unconditionally on each shell start, so the second source of
/// a session would have found the wrapper and captured its output into the env
/// stream — the exact defect C-045 names, arriving through a wrapper the arm did
/// not have when the exemption was written.
fn format_global_env_eval(shell: Shell, binary: &Path) -> String {
    let shell_name = shell_name_for_eval(shell);
    let path = binary.to_string_lossy();
    match shell {
        Shell::Ash | Shell::Ksh | Shell::Dash | Shell::Bash | Shell::Zsh => {
            let quoted = escape::posix_single_quoted(&path);
            format!(r#"if [ -x '{quoted}' ]; then eval "$('{quoted}' --global env --shell={shell_name})"; fi"#)
        }
        Shell::Fish => {
            let quoted = escape::fish_single_quoted(&path);
            format!("if test -x '{quoted}'; '{quoted}' --global env --shell={shell_name} | source; end")
        }
        // Capture the exporter output into a variable and only evaluate it when
        // non-empty. `& ocx …` yields `$null` when the command emits nothing
        // (no global toolchain yet), and `Invoke-Expression $null` throws
        // "Cannot bind argument to parameter 'Command' because it is null".
        // `| Out-String` also collapses the multi-line export output into one
        // string with newlines preserved — passing the raw object array to
        // `Invoke-Expression` would join lines with spaces and corrupt the
        // script. Works in both Windows PowerShell 5.1 and PowerShell 7+.
        Shell::PowerShell => {
            let quoted = escape::single_quoted_doubled(&path);
            format!(
                "if (Test-Path -LiteralPath '{quoted}' -PathType Leaf) {{ $__ocx_global_env = (& '{quoted}' --global env --shell={shell_name} | Out-String); if ($__ocx_global_env) {{ Invoke-Expression $__ocx_global_env }} }}"
            )
        }
        // CMD: `FOR /F` evaluates each line of the subprocess output. `delims=`
        // is required so `%i` is the WHOLE line — the default whitespace split
        // would leave `%i` as just the first token (`SET`), executing a bare
        // `SET` and dropping the assignment. Batch PATH emission is now idempotent
        // move-to-front (single-statement substring-delete, see
        // `Shell::export_path`), so like every other shell it runs unguarded —
        // no `OCX_ACTIVATED` session guard, no marker.
        Shell::Batch => {
            format!("FOR /F \"usebackq delims=\" %i IN (`ocx --global env --shell={shell_name}`) DO @%i")
        }
        // Elvish — capture the exporter output and pass it to `eval` as a
        // POSITIONAL argument: `eval (… | slurp)`. The older pipe form
        // `… | slurp | eval` gives `eval` zero positional args and raises
        // "arity mismatch: arguments must be 1 value, but is 0 values" on every
        // start (so the global toolchain env never applied). Empty output is
        // safe: `eval ""` is a no-op.
        Shell::Elvish => {
            // `?(test -x …)` and not `has-external`: the probe must be a path
            // test for the same reason the call must be a path call. `?(…)`
            // turns the external's non-zero exit into a falsey value instead of
            // an exception, the idiom the shipped `env.elv` shim already uses.
            let quoted = escape::single_quoted_doubled(&path);
            format!("if ?(test -x '{quoted}') {{ eval ('{quoted}' --global env --shell={shell_name} | slurp) }}")
        }
        // Nushell has no string `eval` and `source` needs a parse-time-const path
        // (it reads the file at PARSE time), so global env output cannot be
        // evaluated the way every other shell does. Apply it as DATA instead:
        // parse `--format json` and apply each entry by modifier type via the
        // shared `NU_ENV_APPLY_LOOP` (path -> move-to-front prepend, constant ->
        // replace). `--shell=nushell` output is deliberately NOT used here. The
        // loop is shared verbatim with `$OCX_HOME/env.nu` so the two cannot drift;
        // `ocx` is already on PATH (the preceding prepend), guarded by `which ocx`.
        // No `{shell_name}` interpolation: the global env is read as JSON via the
        // root `--format json` flag, not a `--shell=NAME` channel.
        Shell::Nushell => [
            "if (which ocx | length) > 0 { try { let _ocx_json = (ocx --format json --global env | from json); ",
            ocx_lib::setup::shims::NU_ENV_APPLY_LOOP,
            " } catch { } }",
        ]
        .concat(),
    }
}

/// Returns the short shell name suitable for use in `--shell=NAME` arguments.
fn shell_name_for_eval(shell: Shell) -> &'static str {
    match shell {
        Shell::Ash => "ash",
        Shell::Ksh => "ksh",
        Shell::Dash => "sh",
        Shell::Bash => "bash",
        Shell::Elvish => "elvish",
        Shell::Fish => "fish",
        Shell::Batch => "batch",
        Shell::PowerShell => "pwsh",
        Shell::Zsh => "zsh",
        Shell::Nushell => "nushell",
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use ocx_lib::shell::Shell;

    use ocx_lib::setup::ocx_install_bin_path;

    use super::{completion_clap_shell, format_global_env_eval, generate_completion_inline, path_prepend_line};

    /// The resolved absolute binary every emitted invocation must name (C-045).
    fn test_binary() -> PathBuf {
        PathBuf::from("/tmp/ocx_home/symlinks/ocx_sh/ocx/cli/current/content/bin/ocx")
    }

    /// Every shell variant — the activation surface is generic over the full set.
    const ALL_SHELLS: [Shell; 10] = [
        Shell::Ash,
        Shell::Ksh,
        Shell::Dash,
        Shell::Bash,
        Shell::Zsh,
        Shell::Fish,
        Shell::PowerShell,
        Shell::Batch,
        Shell::Elvish,
        Shell::Nushell,
    ];

    // ── PATH prepend: absolute path via Shell::export_path ──────────────────
    //
    // `emit_path_prepend` must use the resolved absolute path — no `$VAR`
    // references in the value. The emitted line must contain the absolute path
    // string and NOT contain `${OCX_HOME}` or `%OCX_HOME%`.

    #[test]
    fn path_prepend_bash_contains_absolute_path() {
        let path = PathBuf::from("/tmp/known/.ocx/symlinks/ocx_sh/ocx/cli/current/content/bin");
        // Use Shell::export_path directly to mirror what emit_path_prepend emits.
        let line = Shell::Bash
            .export_path("PATH", path.to_string_lossy())
            .expect("valid env-var name");
        assert!(
            line.contains("/tmp/known"),
            "PATH prepend must contain the absolute path; got: {line:?}"
        );
        assert!(
            !line.contains("${OCX_HOME}"),
            "PATH prepend must not contain ${{OCX_HOME}} variable reference; got: {line:?}"
        );
        assert!(
            !line.contains("%OCX_HOME%"),
            "PATH prepend must not contain %OCX_HOME% variable reference; got: {line:?}"
        );
    }

    #[test]
    fn path_prepend_batch_absolute_path() {
        let path = PathBuf::from(r"C:\Users\test\.ocx\symlinks\ocx_sh\ocx\cli\current\content\bin");
        let line = Shell::Batch
            .export_path("PATH", path.to_string_lossy())
            .expect("valid env-var name");
        assert!(
            line.contains("ocx_sh"),
            "CMD PATH prepend must contain the absolute path; got: {line:?}"
        );
        assert!(
            !line.contains("%OCX_HOME%"),
            "CMD PATH prepend must not contain %OCX_HOME% variable reference; got: {line:?}"
        );
    }

    /// Smoke-test that `path_prepend_line` does not panic for any shell variant.
    #[test]
    fn emit_path_prepend_does_not_panic_for_any_shell() {
        let path = PathBuf::from("/tmp/known/.ocx/symlinks/ocx_sh/ocx/cli/current/content/bin");
        let shells = [
            Shell::Ash,
            Shell::Ksh,
            Shell::Dash,
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Nushell,
            Shell::Elvish,
            Shell::Batch,
        ];
        for shell in shells {
            let path_clone = path.clone();
            std::thread::spawn(move || path_prepend_line(shell, &path_clone))
                .join()
                .unwrap_or_else(|_| panic!("path_prepend_line panicked for {shell:?}"));
        }
    }

    // ── No OCX_ACTIVATED guard (idempotent activation, no cross-process leak) ─

    /// No shell's global-env-eval line may carry an `OCX_ACTIVATED` state guard —
    /// Batch included now that its PATH emission is idempotent move-to-front
    /// (substring-delete). An exported guard leaks into child processes (e.g. a
    /// VS Code Remote server whose terminals inherit it) and suppresses
    /// activation in a shell that needs it — the exact SSH-vs-VS-Code divergence
    /// this removal fixes. Idempotent env emission (PATH move-to-front, constants
    /// absolute) makes the guard unnecessary for correctness.
    #[test]
    fn global_env_eval_has_no_activated_guard_for_any_shell() {
        for shell in ALL_SHELLS {
            let line = format_global_env_eval(shell, &test_binary());
            assert!(
                !line.contains("OCX_ACTIVATED"),
                "{shell:?} eval line must not reference OCX_ACTIVATED; got: {line:?}"
            );
        }
    }

    /// Every shell still invokes `ocx … global env` — removing the guard must
    /// strip only the state gate, never the eval itself. Nushell is the one
    /// exception to the `--shell=NAME` channel: it has no string `eval`, so it
    /// consumes the structured `--format json --global env` output and applies it
    /// with `load-env` instead of evaluating shell code.
    #[test]
    fn global_env_eval_invokes_global_env_for_any_shell() {
        for shell in ALL_SHELLS {
            let line = format_global_env_eval(shell, &test_binary());
            let invokes = if shell == Shell::Nushell {
                line.contains("--format json --global env")
            } else {
                line.contains("--global env --shell=")
            };
            assert!(invokes, "{shell:?} eval line must invoke ocx global env; got: {line:?}");
        }
    }

    /// Elvish must pass the exporter output to `eval` as a POSITIONAL argument
    /// (`eval (… | slurp)`), not pipe it (`… | slurp | eval`) — the pipe form
    /// gives `eval` zero args and raises an arity mismatch on every shell start,
    /// so the global toolchain env never applied.
    #[test]
    fn global_env_eval_elvish_uses_capture_not_pipe_to_eval() {
        let line = format_global_env_eval(Shell::Elvish, &test_binary());
        let binary = test_binary().to_string_lossy().into_owned();
        assert!(
            line.contains(&format!("eval ('{binary}' --global env --shell=elvish | slurp)")),
            "elvish must capture-then-eval, naming the absolute binary (C-045); got: {line:?}"
        );
        assert!(
            !line.contains("slurp | eval"),
            "elvish must NOT pipe to eval (arity mismatch); got: {line:?}"
        );
    }

    /// Nushell must apply the global env as DATA (`from json` + `load-env`),
    /// never via the no-op `nu -c $in` subprocess (which mutates only a child's
    /// env, so the global toolchain never reached the parent shell). The apply
    /// body is the shared `NU_ENV_APPLY_LOOP`, embedded verbatim so this line and
    /// `$OCX_HOME/env.nu` cannot drift, and it dispatches on the entry modifier
    /// type (so a non-PATH `type:path` var prepends, not overwrites).
    #[test]
    fn global_env_eval_nushell_applies_json_via_load_env() {
        let line = format_global_env_eval(Shell::Nushell, &test_binary());
        assert!(
            line.contains("from json"),
            "nushell must parse JSON global env; got: {line:?}"
        );
        assert!(
            line.contains("load-env"),
            "nushell must apply constants via load-env; got: {line:?}"
        );
        assert!(
            line.contains(ocx_lib::setup::shims::NU_ENV_APPLY_LOOP),
            "nushell eval line must embed the shared apply loop verbatim (drift guard); got: {line:?}"
        );
        assert!(
            line.contains(r#"$_ocx_e.type == "path""#),
            "nushell must dispatch on the entry modifier type, not the key name; got: {line:?}"
        );
        assert!(
            !line.contains("nu -c"),
            "nushell must NOT shell out to a child `nu -c` (no parent env effect); got: {line:?}"
        );
    }

    /// The eval stays gated on an `ocx`-existence probe (not a state guard) for
    /// every shell that has one, so a shell with ocx uninstalled is a clean
    /// no-op rather than an error. Batch has no probe — `FOR /F` runs ocx and a
    /// missing binary simply emits nothing.
    #[test]
    fn global_env_eval_probes_ocx_existence() {
        // C-045 — the five wrapper-hosting families probe the resolved **path**,
        // never the command name: `command -v` / `type -q` / `Get-Command` all
        // find the shell function named `ocx` that this same stream defines, so
        // a name probe would report "present" for the wrapper rather than for
        // the binary. The three arms that host no wrapper keep their name probe.
        let binary = test_binary();
        let binary = binary.display();
        let probes = [
            (Shell::Bash, format!("[ -x '{binary}' ]")),
            (Shell::Zsh, format!("[ -x '{binary}' ]")),
            (Shell::Dash, format!("[ -x '{binary}' ]")),
            (Shell::Ash, format!("[ -x '{binary}' ]")),
            (Shell::Ksh, format!("[ -x '{binary}' ]")),
            (Shell::Fish, format!("test -x '{binary}'")),
            (
                Shell::PowerShell,
                format!("Test-Path -LiteralPath '{binary}' -PathType Leaf"),
            ),
            // C-045 — a path test, not a name lookup: elvish hosts an `ocx`
            // wrapper function, and `has-external` would find it.
            (Shell::Elvish, format!("?(test -x '{binary}')")),
            (Shell::Nushell, "which ocx".to_owned()),
        ];
        for (shell, probe) in probes {
            let line = format_global_env_eval(shell, &test_binary());
            assert!(
                line.contains(&probe),
                "{shell:?} eval line must probe ocx via `{probe}`; got: {line:?}"
            );
        }
    }

    /// The pwsh eval line must capture the exporter output, collapse it with
    /// `Out-String`, and only `Invoke-Expression` it when non-empty — otherwise
    /// `Invoke-Expression $null` throws on every shell start with no toolchain
    /// (regression: TODO "First" bug).
    #[test]
    fn global_env_eval_powershell_guards_against_null_output() {
        let line = format_global_env_eval(Shell::PowerShell, &test_binary());
        assert!(
            line.contains("Out-String"),
            "pwsh eval line must pipe exporter output through Out-String; got: {line:?}"
        );
        assert!(
            !line.contains("Invoke-Expression (&"),
            "pwsh eval line must NOT pass `(& ocx …)` directly to Invoke-Expression \
             (yields $null when empty → bind error); got: {line:?}"
        );
        // The Invoke-Expression must be guarded by a non-empty check on the
        // captured variable.
        assert!(
            line.contains("if ($__ocx_global_env) { Invoke-Expression $__ocx_global_env }"),
            "pwsh eval line must guard Invoke-Expression on non-empty output; got: {line:?}"
        );
    }

    // ── Inline completion generation ────────────────────────────────────────

    /// Shells with a clap_complete backend produce a non-empty inline script;
    /// the rest opt out (no backend → no completion emitted).
    #[test]
    fn generate_completion_inline_covers_clap_shells_only() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::Elvish, Shell::PowerShell] {
            let script =
                generate_completion_inline(shell).unwrap_or_else(|| panic!("{shell:?} must produce a completion"));
            assert!(!script.is_empty(), "{shell:?} completion must be non-empty");
        }
        for shell in [Shell::Ash, Shell::Ksh, Shell::Dash, Shell::Batch, Shell::Nushell] {
            assert!(
                completion_clap_shell(shell).is_none() && generate_completion_inline(shell).is_none(),
                "{shell:?} has no clap_complete backend and must opt out"
            );
        }
    }

    /// The zsh completion self-loads `compinit` before clap's trailing
    /// `compdef`, so it works even when sourced before the user's own
    /// `compinit` (e.g. from `.zprofile`).
    #[test]
    fn zsh_completion_guards_compinit_before_compdef() {
        let script = generate_completion_inline(Shell::Zsh).expect("zsh has a backend");
        let guard = script
            .find("autoload -Uz compinit")
            .expect("zsh completion must self-load compinit");
        let compdef = script
            .rfind("compdef _ocx ocx")
            .expect("zsh completion must call compdef");
        assert!(guard < compdef, "the compinit guard must precede the compdef call");
    }

    /// The PowerShell completion must lead with `using namespace` so that, when
    /// emitted first into the activation stream, `Invoke-Expression` accepts it
    /// (valid only as the first statement; verified on WinPS 5.1 and pwsh 7).
    #[test]
    fn powershell_completion_leads_with_using_namespace() {
        let script = generate_completion_inline(Shell::PowerShell).expect("pwsh has a backend");
        let first = script.trim_start().lines().next().unwrap_or_default();
        assert!(
            first.starts_with("using namespace"),
            "pwsh completion must lead with `using namespace`; got: {first:?}"
        );
    }

    /// No completion output may contain non-ASCII bytes (ASCII guard, G4).
    ///
    /// clap embeds CLI help text into every completion script. A stray Unicode
    /// character (e.g. `→`) is invisible on UTF-8 shells but corrupts on Windows
    /// PowerShell 5.1, which decodes the captured activation stream with the
    /// console codepage — turning `→` into mojibake whose stray quote byte breaks
    /// the surrounding single-quoted PowerShell string. Keeping help ASCII makes
    /// the inline stream robust on every shell. If this fails, find the offending
    /// `#[arg]`/`#[command(about=...)]` help text and replace the non-ASCII
    /// character (e.g. `→` -> `->`).
    #[test]
    fn completion_output_is_ascii_for_all_shells() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::Elvish, Shell::PowerShell] {
            let script = generate_completion_inline(shell).expect("backend present");
            let bad = script.bytes().position(|byte| !byte.is_ascii());
            assert!(
                bad.is_none(),
                "{shell:?} completion must be ASCII-only; first non-ASCII byte at offset {:?}. \
                 Find the help text with a non-ASCII char and replace it (e.g. `→` -> `->`).",
                bad
            );
        }
    }

    // ── Completion interactivity gate ──────────────────────────────────────
    //
    // The gate decision (flags + `OCX_NO_COMPLETIONS` + interactivity) now lives
    // in `options::Completion::enabled` and is unit-tested there. `execute` just
    // honours the resolved boolean before calling `generate_completion_inline`.

    /// `ocx_install_bin_path` must return a path ending with `current/content/bin`.
    #[test]
    fn ocx_install_bin_path_structure() {
        use ocx_lib::file_structure::FileStructure;
        let fs = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));
        let bin_path = ocx_install_bin_path(&fs);
        assert!(
            bin_path.ends_with(Path::new("current/content/bin")),
            "bin path must end with current/content/bin; got: {bin_path:?}"
        );
        assert!(
            bin_path.starts_with("/tmp/ocx_home/symlinks"),
            "bin path must be rooted under $OCX_HOME/symlinks; got: {bin_path:?}"
        );
    }
}

#[cfg(test)]
mod reconcile_tests {
    use std::path::{Path, PathBuf};

    use clap::{CommandFactory as _, Parser as _};
    use ocx_lib::cli::Theme;
    use ocx_lib::env::Env;
    use ocx_lib::package::metadata::env::entry::Entry;
    use ocx_lib::package::metadata::env::modifier::ModifierKind;
    use ocx_lib::project::LockCurrency;
    use ocx_lib::shell::Shell;
    use ocx_lib::shell::coexistence::{Observation, Tool, Yield};
    use ocx_lib::shell::reconcile::{CARRIER_KEY, Ledger, Prior, Verdict};

    use ocx_lib::activation::{
        Outcome, ProjectIdentity, SessionError, SessionPath, is_stat_only, next_ledger, plan_for, yield_messages,
    };

    use super::{ActivateMode, SelfActivate, activation_lines, ocx_binary_path, reconcile_lines};

    /// The uncoloured theme. These assertions are about the shell statements
    /// the stream emits, and an escape sequence inside an expected string would
    /// only make them harder to read; the coloured summary line is proved where
    /// it is built, in `shell::reconcile::plan`.
    fn plain() -> Theme {
        Theme::new(false)
    }

    const ALL_SHELLS: [Shell; 10] = [
        Shell::Ash,
        Shell::Ksh,
        Shell::Dash,
        Shell::Bash,
        Shell::Zsh,
        Shell::Fish,
        Shell::PowerShell,
        Shell::Batch,
        Shell::Elvish,
        Shell::Nushell,
    ];

    fn constant(key: &str, value: &str) -> Entry {
        Entry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind: ModifierKind::Constant,
            separator: None,
        }
    }

    fn identity() -> ProjectIdentity {
        ProjectIdentity {
            config_path: PathBuf::from("/work/acme/ocx.toml"),
            dir: PathBuf::from("/work/acme"),
            key: "a1b2c3d4e5f60718".to_owned(),
        }
    }

    fn outcome(global: Vec<Entry>, project: Vec<Entry>, slot: Option<ProjectIdentity>) -> Outcome {
        Outcome {
            // C-059's two session directories, spelled as literals here on
            // purpose: these tests own no `FileStructure`, and what they assert
            // is the *rendering* of an outcome, never how one is minted.
            session: SessionPath {
                global_bin: PathBuf::from("/tmp/ocx_home/toolchain/bin"),
                project_bin: None,
                install_bin: bin_dir(),
            },
            global,
            project,
            owned_home: None,
            resolved: slot.is_some(),
            slot,
            inert: false,
            messages: Vec::new(),
        }
    }

    fn bin_dir() -> PathBuf {
        PathBuf::from("/tmp/ocx_home/symlinks/ocx_sh/ocx/cli/current/content/bin")
    }

    /// The other of C-059's two session directories — `$OCX_HOME/toolchain/bin`,
    /// the global rendered toolchain's trampolines.
    fn toolchain_bin_dir() -> PathBuf {
        PathBuf::from("/tmp/ocx_home/toolchain/bin")
    }

    /// The `PATH` a shell holds once C-059's two session directories are
    /// already applied — i.e. every prompt after a session's first, and what
    /// `ocx self setup` leaves behind at session level.
    ///
    /// Spelled in the order the desired set's fold leaves them
    /// (`$OCX_HOME/toolchain/bin` frontmost, C-060/RUL-62), because that is what
    /// makes the fold a genuine no-op: `plan`'s settling test asks whether
    /// applying the desired set to this environment changes it, and an
    /// environment holding the same two directories in the other order still
    /// moves.
    fn settled_env() -> Env {
        let mut env = Env::clean();
        env.set(
            "PATH",
            std::env::join_paths([toolchain_bin_dir().as_os_str(), bin_dir().as_os_str()]).expect("the seed joins"),
        );
        env
    }

    /// The login stream in the ladder's floor mode, which is what every
    /// assertion written before the `activate` gate existed assumed.
    fn startup_stream(shell: Shell, hook: bool) -> Vec<String> {
        startup_stream_in(shell, hook, ActivateMode::Env)
    }

    /// The login stream under a stated global `activate` mode.
    fn startup_stream_in(shell: Shell, hook: bool, activate: ActivateMode) -> Vec<String> {
        let bin = bin_dir();
        let binary = hook.then(|| ocx_binary_path(&bin));
        activation_lines(&super::LoginStream {
            shell,
            bin_path: &bin,
            toolchain_bin: &toolchain_bin_dir(),
            activate,
            completion: super::generate_completion_inline(shell).as_deref(),
            hook_binary: binary.as_deref(),
            wrapper_binary: None,
            watch: &[PathBuf::from("/work/acme/ocx.lock")],
            seed: None,
        })
    }

    // ── C-041: grammar ──────────────────────────────────────────────────────

    /// C-041 — the flag surface parses: `--reconcile` (hidden), and the paired
    /// `--hook` / `--no-hook`, which are POSIX last-wins rather than an error.
    #[test]
    fn c041_the_flag_surface_parses_with_posix_last_wins() {
        let reconcile = SelfActivate::parse_from(["self-activate", "--shell=bash", "--reconcile"]);
        assert!(reconcile.reconcile, "--reconcile must set the per-prompt entry point");

        let plain = SelfActivate::parse_from(["self-activate", "--shell=bash"]);
        assert!(!plain.reconcile, "--reconcile is opt-in, never the default");

        // Combining both is not an error (the `git --[no-]verify` idiom); the
        // last one wins. Resolution itself is `options::hook`'s and is tested
        // there — this asserts only that the pair is accepted here.
        SelfActivate::parse_from(["self-activate", "--shell=bash", "--no-hook", "--hook"]);
        SelfActivate::parse_from(["self-activate", "--shell=bash", "--hook", "--no-hook"]);
    }

    /// C-041 — `--reconcile` is hidden at flag level, following the shipped
    /// `command/login.rs` precedent. It is machine surface: the emitted hook
    /// body types it, a user never does.
    #[test]
    fn c041_reconcile_is_hidden_from_help() {
        let hidden = SelfActivate::command()
            .get_arguments()
            .find(|argument| argument.get_id() == "reconcile")
            .expect("the --reconcile flag must exist")
            .is_hide_set();
        assert!(hidden, "--reconcile must be hidden (C-041)");
    }

    /// C-038 rung 5, end to end from argv: the flag the shims emit reaches the
    /// auto rung's input, and the probe still decides when no flag arrives.
    ///
    /// The struct-level ladder is pinned in `options/interactive.rs`; what this
    /// adds is the wiring — a pair that parsed but was never read, or was read
    /// with the probe's value substituted, would pass there and fail here. The
    /// probe operand is supplied as a literal in both directions, so neither
    /// case can be satisfied by the ambient terminal state of the test runner.
    #[test]
    fn c038_the_interactivity_pair_decides_the_auto_rung_over_the_probe() {
        for (argument, probed, expected) in [("--interactive", false, true), ("--no-interactive", true, false)] {
            let parsed = SelfActivate::parse_from(["self-activate", "--shell=bash", argument]);
            assert_eq!(
                parsed.interactive.resolve(probed),
                expected,
                "{argument} must decide the auto rung over a probe answering {probed}"
            );
        }

        // No flag — the not-yet-refreshed shim, and a direct invocation at a
        // prompt. Rung 5 falls back to the probe, in both directions.
        let bare = SelfActivate::parse_from(["self-activate", "--shell=bash"]);
        assert_eq!(
            (bare.interactive.resolve(true), bare.interactive.resolve(false)),
            (true, false),
            "with no flag the probe must decide, both ways"
        );

        // Last-wins, as with every paired toggle here.
        let last_wins =
            SelfActivate::parse_from(["self-activate", "--shell=bash", "--no-interactive", "--interactive"]);
        assert!(
            last_wins.interactive.resolve(false),
            "`--no-interactive --interactive` must resolve interactive (POSIX last-wins)"
        );
    }

    /// C-038 — both interactivity flags are hidden: machine surface the shims
    /// emit, never something a user types.
    #[test]
    fn c038_the_interactivity_pair_is_hidden_from_help() {
        for id in ["interactive", "no_interactive"] {
            assert!(
                SelfActivate::command()
                    .get_arguments()
                    .find(|argument| argument.get_id() == id)
                    .unwrap_or_else(|| panic!("the {id} flag must exist"))
                    .is_hide_set(),
                "{id} must be hidden"
            );
        }
    }

    // ── C-043/C-046: startup emission order ─────────────────────────────────

    /// C-043 — the completion block must **lead** the stream: clap_complete's
    /// PowerShell output opens with `using namespace`, which
    /// `Invoke-Expression` accepts only as the first statement.
    #[test]
    fn c043_the_completion_block_leads_the_stream() {
        let stream = startup_stream(Shell::PowerShell, true);
        assert!(
            stream[0].trim_start().starts_with("using namespace"),
            "the pwsh completion block must be the first statement; got: {:?}",
            stream[0].lines().next()
        );
    }

    /// C-043 — order across the whole stream: completion, then the PATH
    /// prepend, then the global env eval, then the per-prompt hook, then the
    /// wrapper. The hook body invokes the resolved binary, so it must not
    /// precede the PATH prepend it shares a stream with.
    #[test]
    fn c043_startup_emission_order_is_completion_path_global_hook_wrapper() {
        let stream = startup_stream(Shell::Bash, true);
        let index = |needle: &str| {
            stream
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no emitted line contains {needle:?}; stream: {stream:#?}"))
        };

        let completion = index("complete -F");
        let path = index("export PATH=");
        let global = index("--global env --shell=bash");
        let hook = index("__ocx_prompt_hook() {");
        // `\nocx() {` and not `ocx() {`: clap's bash completion defines
        // `_ocx() {`, which the shorter needle matches first — a false green
        // that would have reported the wrapper as leading the stream.
        let wrapper = index("\nocx() {");

        assert!(completion < path, "completions lead the stream");
        assert!(path < global, "the PATH prepend precedes the global env eval");
        assert!(global < hook, "the hook registers after the env is in place");
        assert!(hook < wrapper, "the wrapper follows its own hook");
    }

    /// C-038 — with the hook disabled, the activation stream carries neither a
    /// registration nor a wrapper, and the rest of the stream is unchanged.
    #[test]
    fn c038_a_disabled_hook_emits_no_registration_and_no_wrapper() {
        let with = startup_stream(Shell::Bash, true);
        let without = startup_stream(Shell::Bash, false);

        assert!(
            with.iter().any(|line| line.contains("__ocx_prompt_hook")),
            "the enabled stream must register the hook"
        );
        assert!(
            !without.iter().any(|line| line.contains("__ocx_prompt_hook")),
            "a disabled hook must emit no registration"
        );
        assert!(
            !without.iter().any(|line| line.contains("\nocx() {")),
            "a disabled hook must emit no wrapper - it would have no stamp to refresh"
        );
        assert_eq!(
            with.len() - 2,
            without.len(),
            "disabling the hook must drop exactly the registration and the wrapper"
        );
    }

    // ── A-21: the startup path emits no diagnostics at all ─────────────────

    /// A-21 — **no** message rides the startup path, on any arm. Not a
    /// conditionally suppressed one: the channel does not exist there. p10k
    /// treats any console output during zsh initialisation as an error, and
    /// pwsh's `$ErrorActionPreference = 'Stop'` is the same class; deferring
    /// every message by one prompt removes the class rather than sniffing one
    /// consumer of it.
    #[test]
    fn a021_the_startup_stream_carries_no_message_on_any_arm() {
        for shell in ALL_SHELLS {
            let stream = startup_stream(shell, true).join("\n");
            // The needle is **derived from that arm's own `emit_message`**, not
            // a hardcoded list: `Shell::emit_message` has six distinct forms and
            // a three-needle list silently exempts elvish (`echo '…' >&2`), so a
            // startup diagnostic added on that arm alone would pass green.
            // Batch returns `None` — it hosts no hook and no message channel.
            let Some(probe) = shell.emit_message("__ocx_probe__") else {
                continue;
            };
            // Compare on the statement's shape, not its payload: strip the
            // probe text and require the surrounding form to be absent.
            let form = probe.replace("__ocx_probe__", "");
            let (prefix, _) = form.split_once("''").unwrap_or((form.as_str(), ""));
            assert!(
                !prefix.is_empty(),
                "{shell:?}: the derived needle must not be empty, or this sweep is vacuous"
            );
            assert!(
                !stream.contains(prefix),
                "{shell:?} startup stream must carry no diagnostic ({prefix:?} found) - A-21 \
                 deletes the startup channel outright rather than suppressing it"
            );
        }
    }

    // ── C-041 / A-34: cross-version safety ─────────────────────────────────

    /// C-041 — the emitted hook **probe-guards the binary** and **discards the
    /// reconcile call's stderr**. A rollback to a pre-hook ocx (S-030) then
    /// prints nothing at all instead of a clap usage error once per prompt in
    /// every open terminal, and a deleted binary (S-029) is a silent no-op.
    #[test]
    fn c041_s029_s030_the_emitted_hook_probe_guards_and_discards_stderr() {
        let stream = startup_stream(Shell::Bash, true).join("\n");
        assert!(
            stream.contains("self activate --reconcile"),
            "the hook must invoke the per-prompt entry point"
        );
        assert!(
            stream.contains("2>/dev/null"),
            "the reconcile call's stderr must be discarded (S-030)"
        );
        let binary = ocx_binary_path(&bin_dir());
        assert!(
            stream.contains(&format!("[ -x '{}' ]", binary.display())),
            "the hook must probe-guard the resolved binary (S-029); got: {stream}"
        );
    }

    /// A-34 — the hook resolves through `current`, unconditionally. No emitted
    /// line reads `OCX_BINARY_PIN`: the pin's consumers are all re-entrant
    /// invocations, and the interactive shell's own top-level resolution is
    /// upstream of that mechanism.
    #[test]
    fn a034_no_emitted_line_reads_the_binary_pin() {
        for shell in ALL_SHELLS {
            let stream = startup_stream(shell, true).join("\n");
            assert!(
                !stream.contains("OCX_BINARY_PIN"),
                "{shell:?} must not read OCX_BINARY_PIN in an emitted body (A-34)"
            );
        }
    }

    /// C-041 — the hook invokes the **resolved absolute** binary, never the
    /// bare name: the wrapper is named `ocx` and `command -v ocx` finds
    /// functions, so a bare call inside the emitted stream would run the
    /// wrapper inside a command substitution (C-045).
    #[test]
    fn c045_the_hook_and_wrapper_call_the_resolved_absolute_binary() {
        let binary = ocx_binary_path(&bin_dir());
        let stream = startup_stream(Shell::Bash, true).join("\n");
        assert!(
            stream.contains(&*binary.to_string_lossy()),
            "the emitted hook must name the resolved absolute binary; got: {stream}"
        );
    }

    // ── C-042: the negative-consent cache ──────────────────────────────────

    /// C-042 — an unchanged fingerprint **and** a cached `inert` verdict make
    /// the prompt stat-only. A fresh clone with no grant would otherwise pay a
    /// full loader pass plus a lock parse on every prompt.
    #[test]
    fn c042_an_unchanged_fingerprint_with_a_cached_inert_verdict_is_stat_only() {
        let mut ledger = Ledger::empty();
        ledger.fp = "fp-1".to_owned();
        ledger.verdict = Some(Verdict::Inert);

        assert!(is_stat_only(&ledger, "fp-1"), "unchanged + inert must short-circuit");
        assert!(
            !is_stat_only(&ledger, "fp-2"),
            "a moved fingerprint must expire the cache (A-13)"
        );
    }

    /// C-042 / C-007 — **only** the negative verdict is cached. An `Activate`
    /// verdict is re-derived every prompt; reading one back from the carrier
    /// would make the ledger a consent input.
    /// EC-FP-007 — the verdict is recomputed every prompt; nothing caches it across runs.
    #[test]
    fn c042_c007_an_activate_verdict_is_never_cached() {
        let mut ledger = Ledger::empty();
        ledger.fp = "fp-1".to_owned();

        ledger.verdict = None;
        assert!(
            !is_stat_only(&ledger, "fp-1"),
            "a ledger with no cached verdict must re-derive consent"
        );

        ledger.verdict = Some(Verdict::Activate);
        assert!(
            !is_stat_only(&ledger, "fp-1"),
            "an Activate verdict must never short-circuit the consent read (C-007)"
        );

        // And nothing this command writes ever puts one there.
        let active = next_ledger(
            &Ledger::empty(),
            "fp-1",
            &outcome(Vec::new(), Vec::new(), Some(identity())),
            &Env::clean(),
        );
        assert_eq!(active.verdict, None, "an active project caches no verdict");

        let mut refused = outcome(Vec::new(), Vec::new(), None);
        refused.inert = true;
        let refused = next_ledger(&Ledger::empty(), "fp-1", &refused, &Env::clean());
        assert_eq!(
            refused.verdict,
            Some(Verdict::Inert),
            "the refusal is the one verdict the ledger caches"
        );
    }

    /// A-10 — `L ⊆ emittable(D)`: the ledger records only what an arm can emit.
    ///
    /// `plan` refuses these entries, so nothing about them ever reaches the
    /// shell. A ledger that recorded them anyway would claim ocx owns a key it
    /// can never remove — and for `PATH` (A-02) it would hand the whole
    /// variable's restore to a prior captured from an apply that never happened.
    ///
    /// Red state: build `global`/`applied` from `outcome.global` /
    /// `outcome.project` directly, and every row below appears in the ledger.
    #[test]
    fn a010_the_ledger_records_only_what_an_arm_can_emit() {
        let refused = vec![
            // A-02 — a forged or mistaken constant claim on the one variable
            // whose restore would overwrite everything the shell holds.
            constant("PATH", "/only/mine"),
            // The four `is_emittable` classes: an invalid key, a path-kind value
            // carrying the separator, an empty element, an element with a newline.
            constant("2FOO", "x"),
            Entry {
                key: "TOOLS".to_owned(),
                value: format!("/a{}/b", ocx_lib::env::PATH_SEPARATOR),
                kind: ModifierKind::Path,
                separator: None,
            },
            Entry {
                key: "OPTS".to_owned(),
                value: String::new(),
                kind: ModifierKind::List,
                separator: Some(" ".to_owned()),
            },
            Entry {
                key: "NOTES".to_owned(),
                value: "a\nb".to_owned(),
                kind: ModifierKind::List,
                separator: Some(" ".to_owned()),
            },
        ];
        let kept = constant("JAVA_HOME", "/opt/jdk");

        let mut global = refused.clone();
        global.push(kept.clone());
        let mut project = refused.clone();
        project.push(kept.clone());

        let ledger = next_ledger(
            &Ledger::empty(),
            "fp-1",
            &outcome(global, project, Some(identity())),
            &Env::clean(),
        );

        let recorded: Vec<&str> = ledger
            .scopes
            .global
            .as_deref()
            .unwrap_or_default()
            .iter()
            .chain(ledger.scopes.project.as_ref().map_or(&[][..], |scope| &scope.applied))
            .map(|entry| entry.key.as_str())
            .collect();
        assert_eq!(
            recorded,
            ["JAVA_HOME", "JAVA_HOME"],
            "only the emittable entry may reach L, in both scopes: {recorded:?}"
        );
    }

    /// P2 — the walk resolving **no project** is the second cached negative
    /// verdict, and it is the overwhelmingly common `cd`.
    ///
    /// Measured on a real bash with the real emitted hook: 21.3 ms per `cd` in a
    /// non-project directory against 4.5 ms in an inert one, the whole gap being
    /// `Context::try_init` + `resolve_global_pinned_env` + a full plan that
    /// composes nothing but the global tier — which the fingerprint already
    /// proves unchanged.
    ///
    /// Red state: `verdict: outcome.inert.then_some(Verdict::Inert)`, the
    /// two-way form, which leaves `verdict == None` here.
    #[test]
    fn p2_a_walk_that_resolved_no_project_caches_a_noproject_verdict() {
        let none = next_ledger(
            &Ledger::empty(),
            "fp-1",
            &outcome(Vec::new(), Vec::new(), None),
            &Env::clean(),
        );
        assert_eq!(
            none.verdict,
            Some(Verdict::NoProject),
            "no project resolved is cacheable and is not consent-derived"
        );
        assert!(
            is_stat_only(&none, "fp-1"),
            "and the next prompt in the same directory answers from stats alone"
        );
        assert!(
            !is_stat_only(&none, "fp-2"),
            "`project_dir` is folded into the fingerprint, so entering any project expires it"
        );
    }

    /// P2's boundary — a project that *was* resolved but yielded to direnv/mise
    /// leaves `slot: None` exactly as the no-project walk does, and must **not**
    /// be cached: the yield is decided by an env sentinel
    /// (`DIRENV_DIR`/`MISE_SHELL`) that `fingerprint` does not fold, so a cached
    /// verdict would survive the sentinel going away.
    ///
    /// This is the whole reason `Outcome` carries `resolved` rather than reading
    /// `slot.is_some()`.
    #[test]
    fn p2_a_yielded_project_is_never_cached() {
        let mut yielded = outcome(Vec::new(), Vec::new(), None);
        yielded.resolved = true;
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &yielded, &Env::clean());
        assert_eq!(
            ledger.verdict, None,
            "a yield is not expirable by the watch set, so it caches nothing"
        );
        assert!(!is_stat_only(&ledger, "fp-1"));
    }

    // ── C-049 / A-37: the yield behaviour ──────────────────────────────────

    /// C-049 + A-37, **renderer half only** — one info line per observed tool.
    ///
    /// This hand-builds its `Yield` and never calls `coexistence::detect`, so it
    /// pins exactly one property: the renderer fans out over every observation
    /// rather than reporting only the first. Its red state is `.take(1)` in
    /// `yield_messages`.
    ///
    /// It does **not** and cannot detect an `elif` between the two sentinel
    /// checks — that lives across a seam this test does not cross, and is
    /// guarded by `coexistence::tests::detect_both_sentinels_fire_independently_a37`
    /// (`coexistence.rs:227`), which sets both env sentinels for real.
    #[test]
    fn a037_both_yield_sentinels_produce_one_line_each() {
        let yielded = Yield {
            observed: vec![
                Observation {
                    tool: Tool::Direnv,
                    signal: "DIRENV_DIR=/work/acme".to_owned(),
                },
                Observation {
                    tool: Tool::Mise,
                    signal: "MISE_SHELL=bash".to_owned(),
                },
            ],
        };

        let messages = yield_messages(&yielded);

        assert_eq!(
            messages.len(),
            2,
            "both live tools must each get their own line (A-37); got: {messages:#?}"
        );
        assert!(
            messages[0].contains("direnv") && messages[0].contains("DIRENV_DIR=/work/acme"),
            "the direnv line names the tool and the signal observed; got: {:?}",
            messages[0]
        );
        assert!(
            messages[1].contains("mise") && messages[1].contains("MISE_SHELL=bash"),
            "the mise line names the tool and the signal observed; got: {:?}",
            messages[1]
        );
    }

    /// C-049 — yielding narrows `desired` to the **global** scope and reverts
    /// the project scope already applied. No new planner arm is needed:
    /// narrowing `desired` and leaving the slot empty makes C-016's retirement
    /// rule retire the project's recorded constants subtractively.
    #[test]
    fn c049_a_yield_narrows_desired_to_global_and_reverts_the_project_scope() {
        // Prompt 1: global sets JAVA_HOME; the project additionally sets a key
        // the global scope never declares, so its revert is a genuine revert
        // rather than a re-apply of global's own value.
        let applied = outcome(
            vec![constant("JAVA_HOME", "/global/java")],
            vec![constant("CARGO_HOME", "/proj/cargo")],
            Some(identity()),
        );
        let mut before = Env::clean();
        before.set("CARGO_HOME", "/home/u/.cargo");
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &before);

        // Prompt 2: direnv is live for this directory, so only the global scope
        // is desired and the project slot retires.
        let mut yielded = outcome(applied.global.clone(), Vec::new(), None);
        yielded.messages = yield_messages(&Yield {
            observed: vec![Observation {
                tool: Tool::Direnv,
                signal: "DIRENV_DIR=/work/acme".to_owned(),
            }],
        });

        let mut current = Env::clean();
        current.set("JAVA_HOME", "/global/java");
        current.set("CARGO_HOME", "/proj/cargo");
        let plan = plan_for(&ledger, &yielded, &[Path::new("/tmp/ocx_home")], &current);

        assert_eq!(
            plan.restores,
            vec![("CARGO_HOME".to_owned(), Some("/home/u/.cargo".to_owned()))],
            "narrowing desired to the global scope must revert the project's own constant"
        );
        assert!(
            plan.sets.iter().all(|entry| entry.key != "CARGO_HOME"),
            "a yielded prompt must not re-apply the project scope"
        );
        let lines = reconcile_lines(Shell::Bash, &ledger, "fp-2", None, &yielded, &plan, &current, &plain());
        assert!(
            lines.iter().any(|line| line.contains("direnv manages this directory")),
            "the yield line rides the reconcile run (A-21); got: {lines:#?}"
        );
    }

    /// A-21 — a deferred diagnostic is announced on the prompt that produced
    /// it and **not** on the ones that merely still hold the same condition.
    ///
    /// Run over all three of the reconciler's message sources, because they
    /// reach `message_lines` through one field and a fix for one is a fix for
    /// all of them: the direnv/mise yield line, the withheld-`[env]` hint, and
    /// the managed tier's dropped-consent-payload reason. None of the three
    /// rides a cacheable verdict — an `Activate` verdict never is (C-007) and a
    /// yield hangs off an env sentinel `fp` does not fold — so the reconciler
    /// genuinely recomposes and re-derives every one of them every prompt. This
    /// repo's own `.envrc` puts the first case in front of the owner.
    ///
    /// Red state: drop the `next.messages_fp != previous.messages_fp` guard in
    /// `message_lines` and the second prompt repeats the line verbatim.
    #[test]
    fn a021_a_message_is_announced_once_and_not_on_every_later_prompt() {
        let cases: [(&str, String); 3] = [
            (
                "direnv manages this directory",
                yield_messages(&Yield {
                    observed: vec![Observation {
                        tool: Tool::Direnv,
                        signal: "DIRENV_DIR=/work/acme".to_owned(),
                    }],
                })
                .remove(0),
            ),
            (
                "its [env] is not applied",
                "ocx: /work/acme: its [env] is not applied - a namespaces grant covers packages only; run `ocx pull` \
                 here once, or add the directory to `[shell.consent] paths`"
                    .to_owned(),
            ),
            (
                "[shell.consent] payload",
                "ocx: the managed tier dropped a [shell.consent] payload".to_owned(),
            ),
        ];

        for (label, message) in cases {
            let mut steady = outcome(vec![constant("JAVA_HOME", "/global/java")], Vec::new(), None);
            steady.messages = vec![message.clone()];

            // Prompt 1 — the condition is new, so the line is owed.
            let current = Env::clean();
            let first = plan_for(&Ledger::empty(), &steady, &[Path::new("/tmp/ocx_home")], &current);
            let announced = reconcile_lines(
                Shell::Bash,
                &Ledger::empty(),
                "fp-1",
                None,
                &steady,
                &first,
                &current,
                &plain(),
            );
            assert!(
                announced.iter().any(|line| line.contains(label)),
                "{label}: the prompt that produced the condition must say so; got: {announced:#?}"
            );
            let recorded = next_ledger(&Ledger::empty(), "fp-1", &steady, &current);

            // Prompt 2 — same condition, same message, nothing else moved.
            let mut settled = Env::clean();
            settled.set("JAVA_HOME", "/global/java");
            let second = plan_for(&recorded, &steady, &[Path::new("/tmp/ocx_home")], &settled);
            let repeated = reconcile_lines(
                Shell::Bash,
                &recorded,
                "fp-2",
                None,
                &steady,
                &second,
                &settled,
                &plain(),
            );
            assert!(
                !repeated.iter().any(|line| line.contains(label)),
                "{label}: an unchanged condition is not a new event; got: {repeated:#?}"
            );

            // The non-vacuity twin: the suppression is keyed on the message
            // set, not on "the second prompt is always quiet". A *different*
            // message on the same condition is announced again.
            let mut changed = outcome(vec![constant("JAVA_HOME", "/global/java")], Vec::new(), None);
            changed.messages = vec![format!("{message} (and mise too)")];
            let third = plan_for(&recorded, &changed, &[Path::new("/tmp/ocx_home")], &settled);
            let announced_again = reconcile_lines(
                Shell::Bash,
                &recorded,
                "fp-2",
                None,
                &changed,
                &third,
                &settled,
                &plain(),
            );
            assert!(
                announced_again.iter().any(|line| line.contains("and mise too")),
                "{label}: a message that changed is news again; got: {announced_again:#?}"
            );
        }
    }

    /// A-21 — a stale or absent project lock is **reported**, not exited on,
    /// and reporting it does not tear the applied scope down.
    ///
    /// Both halves matter and neither implies the other. Propagating the error
    /// exits 65 with an empty stdout and a stderr the emitted hook discards, so
    /// the environment stops tracking the project with nothing said — but
    /// recovering by planning against an outcome that has no project scope
    /// would *revert* it, which is the mid-`git checkout` teardown A-11 refuses
    /// for an indeterminate walk. So: the line is emitted, the carrier comes
    /// back with the project scope intact, and no plan line is written at all.
    ///
    /// Red state for the first assertion: restore `session(...).await?` and the
    /// refusal never reaches an emitter. For the second: build the announced
    /// ledger with `next_ledger` instead of `announcing` and the project scope
    /// disappears from the carrier.
    #[test]
    fn a021_a_stale_lock_is_reported_and_the_applied_scope_is_retained() {
        let applied = outcome(
            vec![constant("JAVA_HOME", "/global/java")],
            vec![constant("CARGO_HOME", "/proj/cargo")],
            Some(identity()),
        );
        let mut before = Env::clean();
        before.set("CARGO_HOME", "/home/u/.cargo");
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &before);
        assert!(
            ledger.scopes.project.is_some(),
            "the fixture must have an applied project scope, or the retention assertion is vacuous"
        );

        let error = SessionError::from(LockCurrency::Stale {
            lock_path: PathBuf::from("/work/acme/ocx.lock"),
        });
        let lines = super::refusal_lines(Shell::Bash, &ledger, &error);

        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocx.lock is stale") && line.contains("run `ocx lock`")),
            "the prompt must say what broke and what fixes it; got: {lines:#?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("unset ") || line.contains("CARGO_HOME=")),
            "a refusal emits no plan at all — reverting here is the teardown the fail-safe exists to stop; got: \
             {lines:#?}"
        );

        // The carrier still carries the project scope, so the *next* prompt can
        // still revert it. Read back from the emitted line rather than from the
        // in-memory ledger: the emitted value is what the shell will hand back.
        let carrier = lines
            .iter()
            .find(|line| line.contains(CARRIER_KEY))
            .and_then(|line| line.split('\'').nth(1))
            .expect("the refusal re-emits the carrier");
        let decoded = Ledger::decode(carrier).expect("the re-emitted carrier decodes");
        assert_eq!(
            decoded.scopes.project.as_ref().map(|scope| scope.applied.clone()),
            ledger.scopes.project.as_ref().map(|scope| scope.applied.clone()),
            "the applied project scope must survive a refusal verbatim"
        );
        assert_eq!(decoded.fp, ledger.fp, "a refusal moves no fingerprint");

        // Second prompt, same broken lock: the line has been said.
        let repeated = super::refusal_lines(Shell::Bash, &decoded, &error);
        assert!(
            !repeated.iter().any(|line| line.contains("ocx.lock is stale")),
            "an unchanged refusal is not a new event; got: {repeated:#?}"
        );
    }

    // ── C-015/C-018: prior capture ordering ────────────────────────────────

    /// C-018 — the ordering is apply global, capture the project's priors,
    /// apply project. A prior captured at project entry therefore holds
    /// **global's** value; capturing before global's apply would silently tear
    /// down global's constants when leaving a project that never owned them.
    #[test]
    fn c018_the_project_prior_is_captured_after_globals_apply() {
        let applied = outcome(
            vec![constant("JAVA_HOME", "/global/java")],
            vec![constant("JAVA_HOME", "/proj/java")],
            Some(identity()),
        );
        // The **pre-global** environment goes in; `next_ledger` owns the apply,
        // so this asserts the ordering rather than performing it.
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &Env::clean());

        assert_eq!(
            ledger
                .scopes
                .project
                .as_ref()
                .expect("the project slot")
                .priors
                .get("JAVA_HOME"),
            Some(&Prior::Value("/global/java".to_owned())),
            "the recorded prior must be global's value, not the pre-global environment"
        );
    }

    // ── R1: the global scope records priors too ────────────────────────────

    /// R1 — the global scope's prior is captured against the **pre-global**
    /// environment, which is the only place the user's own value is visible.
    /// Only the ledger's producer holds one, which is why this half could not
    /// live in `reconcile.rs`.
    ///
    /// Red state: pass `after_global` to `capture_priors` for the global scope
    /// (the ordering the project scope needs) and the prior becomes ocx's own
    /// `/global/java` — a value that reverts to nothing.
    #[test]
    fn r1_the_global_prior_is_captured_before_globals_own_apply() {
        let applied = outcome(vec![constant("JAVA_HOME", "/global/java")], Vec::new(), None);
        let mut current = Env::clean();
        current.set("JAVA_HOME", "/usr/lib/jvm");

        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &current);

        assert_eq!(
            ledger.scopes.global_priors.get("JAVA_HOME"),
            Some(&Prior::Value("/usr/lib/jvm".to_owned())),
            "the user's own value, not the one global is about to write"
        );
    }

    /// C-015 rules 3-4 for the global scope: on every later prompt the live
    /// value **is** ocx's own, so a re-capture would overwrite the user's value
    /// with ocx's and make the revert a no-op. The recorded prior carries
    /// forward instead, exactly as the project scope's does.
    ///
    /// Red state: pass `None` as `capture_priors`' `previous` for the global
    /// scope and prompt 2 records `/global/java`.
    #[test]
    fn r1_a_global_prior_carries_forward_while_the_value_is_still_ocxs() {
        let applied = outcome(vec![constant("JAVA_HOME", "/global/java")], Vec::new(), None);
        let mut current = Env::clean();
        current.set("JAVA_HOME", "/usr/lib/jvm");
        let first = next_ledger(&Ledger::empty(), "fp-1", &applied, &current);

        // Prompt 2: the shell now holds what ocx wrote.
        let mut current = Env::clean();
        current.set("JAVA_HOME", "/global/java");
        let second = next_ledger(&first, "fp-2", &applied, &current);

        assert_eq!(
            second.scopes.global_priors.get("JAVA_HOME"),
            Some(&Prior::Value("/usr/lib/jvm".to_owned())),
            "the prior survives every prompt in which the value is still ocx's"
        );

        // And a mid-session override by hand re-captures, so leaving never
        // unsets a variable the user set themselves.
        let mut typed = Env::clean();
        typed.set("JAVA_HOME", "/opt/typed-by-hand");
        let third = next_ledger(&first, "fp-3", &applied, &typed);
        assert_eq!(
            third.scopes.global_priors.get("JAVA_HOME"),
            Some(&Prior::Value("/opt/typed-by-hand".to_owned()))
        );
    }

    /// R1 end to end, through the shipped producer and planner: the global tier
    /// stops declaring a constant (`ocx remove --global <pkg>`) and the user's
    /// own value comes back into the shell.
    ///
    /// The reconciler-side unit hand-builds its ledger; this one only ever
    /// builds a ledger the way a real prompt does, so a producer that recorded
    /// nothing would show up here even with `Ledger::prior` perfect.
    #[test]
    fn r1_removing_a_global_package_restores_the_users_constant_end_to_end() {
        // Prompt 1: the user had JAVA_HOME; the global tier overrides it.
        let applied = outcome(vec![constant("JAVA_HOME", "/global/java")], Vec::new(), None);
        let mut before = Env::clean();
        before.set("JAVA_HOME", "/usr/lib/jvm");
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &before);

        // Prompt 2: `ocx remove --global` ran in another terminal, so the tier
        // declares nothing at all.
        let removed = outcome(Vec::new(), Vec::new(), None);
        let mut current = Env::clean();
        current.set("JAVA_HOME", "/global/java");
        let plan = plan_for(&ledger, &removed, &[Path::new("/tmp/ocx_home")], &current);

        assert_eq!(
            plan.restores,
            vec![("JAVA_HOME".to_owned(), Some("/usr/lib/jvm".to_owned()))],
            "a retired global constant reverts to the user's own value"
        );
        let lines = super::plan_lines(Shell::Bash, &plan);
        assert!(
            lines.iter().any(|line| line.contains("/usr/lib/jvm")),
            "and the emitted stream carries it; got: {lines:#?}"
        );
    }

    /// The compounding half, end to end: a project constant shadows a global
    /// one and both retire in the same prompt. The project prior holds
    /// **global's** value by construction (C-018), so restoring it verbatim
    /// would leave a value no scope declares any more.
    #[test]
    fn r1_two_scopes_retiring_together_restore_the_users_value_end_to_end() {
        let applied = outcome(
            vec![constant("JAVA_HOME", "/global/java")],
            vec![constant("JAVA_HOME", "/proj/java")],
            Some(identity()),
        );
        let mut before = Env::clean();
        before.set("JAVA_HOME", "/usr/lib/jvm");
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &before);

        // The reachability half: the producer really did record global's value
        // as the project's prior, which is what makes the chain hop necessary.
        assert_eq!(
            ledger
                .scopes
                .project
                .as_ref()
                .expect("the project slot")
                .priors
                .get("JAVA_HOME"),
            Some(&Prior::Value("/global/java".to_owned()))
        );

        // Prompt 2: the CWD left the project *and* the global tier dropped the
        // package, in one prompt.
        let gone = outcome(Vec::new(), Vec::new(), None);
        let mut current = Env::clean();
        current.set("JAVA_HOME", "/proj/java");
        let plan = plan_for(&ledger, &gone, &[Path::new("/tmp/ocx_home")], &current);

        assert_eq!(
            plan.restores,
            vec![("JAVA_HOME".to_owned(), Some("/usr/lib/jvm".to_owned()))],
            "not /global/java, which no scope declares any more"
        );
    }

    /// C-015 rules 3-4 — leaving a project restores the value the user's own
    /// environment held before ocx touched it. This is the assertion the
    /// `capture_priors` fault injection must flip: without the captured prior
    /// there is nothing to restore, and the user's `PYENV_ROOT` is lost for the
    /// shell's whole life.
    #[test]
    fn c015_leaving_a_project_restores_the_constants_captured_prior() {
        // Prompt 1: the user already had PYENV_ROOT; the project overrides it.
        let mut before = Env::clean();
        before.set("PYENV_ROOT", "/home/u/.pyenv");
        let applied = outcome(
            Vec::new(),
            vec![constant("PYENV_ROOT", "/proj/pyenv")],
            Some(identity()),
        );
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &before);

        // Prompt 2: the CWD left the project, so the slot retires.
        let left = outcome(Vec::new(), Vec::new(), None);
        let mut current = Env::clean();
        current.set("PYENV_ROOT", "/proj/pyenv");
        let plan = plan_for(&ledger, &left, &[Path::new("/tmp/ocx_home")], &current);

        assert_eq!(
            plan.restores,
            vec![("PYENV_ROOT".to_owned(), Some("/home/u/.pyenv".to_owned()))],
            "leaving the project must restore the constant's captured prior"
        );
    }

    /// A-05 — a variable that did not exist before ocx set it reverts by
    /// **unset**, and set-ness is read through the environment, never
    /// truthiness.
    #[test]
    fn a005_an_absent_prior_reverts_by_unset() {
        let applied = outcome(
            Vec::new(),
            vec![constant("PYENV_ROOT", "/proj/pyenv")],
            Some(identity()),
        );
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &Env::clean());

        let left = outcome(Vec::new(), Vec::new(), None);
        let mut current = Env::clean();
        current.set("PYENV_ROOT", "/proj/pyenv");
        let plan = plan_for(&ledger, &left, &[Path::new("/tmp/ocx_home")], &current);

        assert_eq!(plan.restores, vec![("PYENV_ROOT".to_owned(), None)]);
        let lines = super::plan_lines(Shell::Bash, &plan);
        assert!(
            lines.contains(&"unset PYENV_ROOT".to_owned()),
            "an absent prior emits an unset; got: {lines:#?}"
        );
    }

    /// R2 — a restored prior must not re-introduce an element the same plan is
    /// retiring.
    ///
    /// `removes` and `restores` are **not** key-disjoint: a global path-kind
    /// entry and a project constant can share a key and retire in one prompt.
    /// The project's prior was captured after global applied (C-018), so it
    /// still contains global's element. With `removes` emitted first, the
    /// removal runs against the constant's live value — where the element is
    /// absent, so it is a no-op — and the restore then writes the retired
    /// element straight back into the shell.
    ///
    /// The first two assertions are the reachability proof: they establish that
    /// the shipped planner really does produce a `removes` and a `restores` for
    /// one key, so the ordering assertion is not guarding a shape that cannot
    /// occur.
    ///
    /// Red state: swap the `restores` and `removes` blocks in `plan_lines` back
    /// and the ordering assertion flips.
    #[test]
    fn r2_a_restored_prior_never_reintroduces_an_element_the_same_plan_retires() {
        let path_entry = |key: &str, value: &str| Entry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind: ModifierKind::Path,
            separator: None,
        };

        // A `PATH`-style value joined the way the platform spells it. A `:`
        // literal is one opaque segment to `std::env::split_paths` on Windows,
        // so move-to-front finds no existing occurrence and prepends a
        // duplicate — the fixture, not the planner, is what differs there.
        let joined = |segments: &[&str]| {
            std::env::join_paths(segments)
                .expect("fixture segments carry no path separator")
                .to_string_lossy()
                .into_owned()
        };
        let global_then_usr = joined(&["/global/bin", "/usr/bin"]);

        // Prompt 1: global contributes a path element, the project shadows the
        // same key with a constant. The prior is captured against the
        // post-global environment, so it holds global's element.
        let mut after_global = Env::clean();
        after_global.set("TOOLPATH", &global_then_usr);
        let applied = outcome(
            vec![path_entry("TOOLPATH", "/global/bin")],
            vec![constant("TOOLPATH", "/proj/fixed")],
            Some(identity()),
        );
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &after_global);
        assert_eq!(
            ledger
                .scopes
                .project
                .as_ref()
                .expect("the project slot")
                .priors
                .get("TOOLPATH"),
            Some(&Prior::Value(global_then_usr.clone())),
            "the prior must carry global's element, or this test is not the two-scope case"
        );

        // Prompt 2: both scopes retire in one prompt.
        let left = outcome(Vec::new(), Vec::new(), None);
        let mut current = Env::clean();
        current.set("TOOLPATH", "/proj/fixed");
        let plan = plan_for(&ledger, &left, &[Path::new("/tmp/ocx_home")], &current);

        assert!(
            plan.removes
                .iter()
                .any(|(key, element, _)| key == "TOOLPATH" && element == "/global/bin"),
            "the planner must retire global's element; got {:?}",
            plan.removes
        );
        assert!(
            plan.restores
                .iter()
                .any(|(key, prior)| key == "TOOLPATH" && prior.as_deref() == Some(global_then_usr.as_str())),
            "the planner must restore the project's prior; got {:?}",
            plan.restores
        );

        let lines = super::plan_lines(Shell::Bash, &plan);
        let restore = lines
            .iter()
            .position(|line| line.contains(&global_then_usr))
            .unwrap_or_else(|| panic!("no restore line emitted; got: {lines:#?}"));
        let remove = lines
            .iter()
            .position(|line| line.contains("TOOLPATH") && !line.contains(&global_then_usr))
            .unwrap_or_else(|| panic!("no removal line emitted; got: {lines:#?}"));

        assert!(
            restore < remove,
            "the restore must land before the removal, or the removal is a no-op against the \
             constant and the retired element survives in the prior; got: {lines:#?}"
        );
    }

    /// R3 / A-01 — one abandonment line per **transition into** the over-cap
    /// state, not one per prompt.
    ///
    /// Without the comparison against the previous ledger, every `cd` inside an
    /// over-cap project reprints a line about something that has not changed
    /// since the user was told about it — and at a prompt a repeated line reads
    /// as a new event.
    ///
    /// Red state: drop the `previous.over_cap.contains(&scope)` guard in
    /// `ledger_lines` and the second reconcile prints the line again.
    #[test]
    fn r3_a01_the_over_cap_line_is_emitted_once_per_transition_not_per_prompt() {
        // Comfortably past the 16 KiB cap, so `encode` falls back to the
        // marker-only ledger that carries `over_cap`.
        let bulky: Vec<Entry> = (0..400)
            .map(|index| constant(&format!("OCX_BULK_{index}"), &"x".repeat(64)))
            .collect();
        let applied = outcome(bulky.clone(), bulky, Some(identity()));

        let first_previous = Ledger::empty();
        let next = next_ledger(&first_previous, "fp-1", &applied, &Env::clean());
        let encoded = next.encode().expect("the marker ledger must still encode");
        let marker = Ledger::decode(&encoded).expect("the marker must decode");
        assert!(
            !marker.over_cap.is_empty(),
            "the fixture must actually exceed the cap, or both assertions below are vacuous"
        );

        let entry_lines = super::ledger_lines(Shell::Bash, &first_previous, &next);
        let announced = |lines: &[String]| lines.iter().filter(|line| line.contains("too large to record")).count();
        assert_eq!(
            announced(&entry_lines),
            marker.over_cap.len(),
            "entering the over-cap state announces each abandoned scope once; got: {entry_lines:#?}"
        );

        // The next prompt in the same project: same outcome, same cap, and the
        // previous ledger is the marker the line above exported.
        let steady_lines = super::ledger_lines(Shell::Bash, &marker, &next);
        assert_eq!(
            announced(&steady_lines),
            0,
            "a second consecutive over-cap reconcile must announce nothing; got: {steady_lines:#?}"
        );
        assert!(
            steady_lines.iter().any(|line| line.contains(CARRIER_KEY)),
            "the carrier is still exported - only the diagnostic is suppressed"
        );
    }

    // ── C-002/C-004: the carrier ───────────────────────────────────────────

    /// C-002 — every reconcile emits the next ledger, and it round-trips: what
    /// the prompt exports is what the next prompt decodes.
    #[test]
    fn c002_the_emitted_carrier_round_trips() {
        let applied = outcome(
            vec![constant("JAVA_HOME", "/global/java")],
            vec![constant("CARGO_HOME", "/proj/cargo")],
            Some(identity()),
        );
        let plan = plan_for(&Ledger::empty(), &applied, &[Path::new("/tmp/ocx_home")], &Env::clean());
        let lines = reconcile_lines(
            Shell::Bash,
            &Ledger::empty(),
            "fp-1",
            None,
            &applied,
            &plan,
            &Env::clean(),
            &plain(),
        );

        let carrier = lines
            .iter()
            .find(|line| line.contains(CARRIER_KEY))
            .unwrap_or_else(|| panic!("no carrier line emitted; got: {lines:#?}"));
        let encoded = carrier
            .split_once('=')
            .expect("an export line")
            .1
            .trim_matches(|c| c == '\'' || c == ' ');
        let decoded = Ledger::decode(encoded).expect("the emitted carrier must decode");

        assert_eq!(decoded.fp, "fp-1");
        assert_eq!(
            decoded.scopes.project.expect("the project slot").dir,
            PathBuf::from("/work/acme")
        );
    }

    /// The resolved colour reaches the emitted stream, in both directions.
    ///
    /// `shell::reconcile::plan` proves what the marks are inked *with*; this
    /// proves the theme this command resolves is the one that gets there — a
    /// summary built with a coloured theme carries escapes into the emitted
    /// statement, one built with the uncoloured theme carries none, and the two
    /// are the same statement once the escapes are stripped.
    ///
    /// Without it the threading could be wired to a theme nothing consults and
    /// every other assertion here would still pass.
    #[test]
    fn a021_the_resolved_colour_reaches_the_emitted_summary() {
        let applied = outcome(
            vec![constant("JAVA_HOME", "/global/java")],
            vec![constant("CARGO_HOME", "/proj/cargo")],
            Some(identity()),
        );
        let plan = plan_for(&Ledger::empty(), &applied, &[Path::new("/tmp/ocx_home")], &Env::clean());
        let emitted = |theme: &Theme| {
            reconcile_lines(
                Shell::Bash,
                &Ledger::empty(),
                "fp-1",
                None,
                &applied,
                &plan,
                &Env::clean(),
                theme,
            )
            .into_iter()
            // `ocx:` without the trailing space: under colour the theme closes
            // the dimmed prefix with a reset *before* the space, so a needle
            // spanning it matches only the uncoloured half — and this test
            // would then fail for a reason that is not the one it exists to
            // catch.
            .find(|line| line.contains("ocx:"))
            .unwrap_or_else(|| panic!("the fixture must emit a summary line"))
        };

        let inked = emitted(&Theme::new(true));
        let bare = emitted(&plain());
        assert!(
            inked.contains('\u{1b}'),
            "the resolved colour never reached the emitted summary: {inked:?}"
        );
        assert!(
            !bare.contains('\u{1b}'),
            "the uncoloured theme still painted the emitted summary: {bare:?}"
        );
        assert_eq!(
            console::strip_ansi_codes(&inked),
            bare,
            "colour changed the emitted statement, not just the escapes"
        );
    }

    /// **The eval-safety invariant for the coloured line (A-21).** No escape
    /// may reach the emitted stream except inside a message statement's own
    /// quoted literal.
    ///
    /// This stream is `eval`'d. An SGR sequence anywhere but inside a quoted
    /// argument is bytes the shell parses as source, so "the colour is safe"
    /// is a claim about *where* it sits, not about the escaper alone —
    /// `shell::reconcile::plan` proves the escaper preserves it, and this
    /// proves nothing bypasses the escaper on the way out.
    ///
    /// The needle is derived from each arm's own `emit_message`, never
    /// hard-coded, so a changed emitter cannot silently widen what counts as
    /// quoted. `Batch` hosts no hook and emits no message at all, so its stream
    /// must carry no escape by any route.
    ///
    /// Red state: drop the `emit_message` wrap in `message_lines` and push the
    /// message text straight into `lines`.
    #[test]
    fn a021_no_escape_reaches_the_stream_outside_a_quoted_message() {
        const PROBE: &str = "__ocx_colour_probe__";

        let applied = outcome(
            vec![constant("JAVA_HOME", "/global/java")],
            vec![constant("CARGO_HOME", "/proj/cargo")],
            Some(identity()),
        );
        let plan = plan_for(&Ledger::empty(), &applied, &[Path::new("/tmp/ocx_home")], &Env::clean());
        for shell in ALL_SHELLS {
            let lines = reconcile_lines(
                shell,
                &Ledger::empty(),
                "fp-1",
                None,
                &applied,
                &plan,
                &Env::clean(),
                &Theme::new(true),
            );
            let inked: Vec<&String> = lines.iter().filter(|line| line.contains('\u{1b}')).collect();

            let Some(probe) = shell.emit_message(PROBE) else {
                assert!(
                    inked.is_empty(),
                    "{shell:?} emits no message, so no escape may reach its stream: {inked:?}"
                );
                continue;
            };
            let prefix = probe
                .split(PROBE)
                .next()
                .filter(|prefix| !prefix.is_empty())
                .unwrap_or_else(|| panic!("{shell:?}: the probe must ride inside a statement"));
            assert_eq!(
                inked.len(),
                1,
                "{shell:?}: exactly the summary line carries colour; got {inked:?}"
            );
            for line in inked {
                assert!(
                    line.starts_with(prefix),
                    "{shell:?}: an escape reached the stream outside a message statement: {line:?}"
                );
            }
        }
    }

    /// #347 — the recorded membership must move **only** with the emission that
    /// redefines the gate, and must move **with** it.
    ///
    /// Both halves matter and fail in opposite directions. Recording it without
    /// emitting leaves the shell gating on a stale list it will never be told
    /// about again — the shipped defect. Emitting without recording redefines a
    /// byte-identical hook body on every prompt for the shell's whole life.
    #[test]
    fn the_recorded_membership_moves_only_with_the_gate_emission() {
        let applied = outcome(Vec::new(), Vec::new(), None);
        let plan = plan_for(&Ledger::empty(), &applied, &[Path::new("/tmp/ocx_home")], &Env::clean());
        let previous = Ledger {
            ws: "0123456789abcdef".to_owned(),
            ..Ledger::empty()
        };

        let carried = decoded_carrier(&reconcile_lines(
            Shell::Bash,
            &previous,
            "fp-2",
            None,
            &applied,
            &plan,
            &Env::clean(),
            &plain(),
        ));
        assert_eq!(
            carried.ws, previous.ws,
            "a reconcile that did not redefine the gate must record the gate the shell still has"
        );

        let regated = decoded_carrier(&reconcile_lines(
            Shell::Bash,
            &previous,
            "fp-2",
            Some("fedcba9876543210"),
            &applied,
            &plan,
            &Env::clean(),
            &plain(),
        ));
        assert_eq!(
            regated.ws, "fedcba9876543210",
            "the prompt that redefines the gate must record the membership it emitted"
        );
    }

    /// Read the emitted carrier back out of a reconcile stream.
    fn decoded_carrier(lines: &[String]) -> Ledger {
        let carrier = lines
            .iter()
            .find(|line| line.contains(CARRIER_KEY))
            .unwrap_or_else(|| panic!("no carrier line emitted; got: {lines:#?}"));
        let encoded = carrier
            .split_once('=')
            .expect("an export line")
            .1
            .trim_matches(|c| c == '\'' || c == ' ');
        Ledger::decode(encoded).expect("the emitted carrier must decode")
    }

    /// C-051 / A-21 — a reconcile that changed nothing emits no summary line;
    /// the summary exists to explain a change, not to narrate a no-op.
    ///
    /// The live environment is [`settled_env`], not `Env::clean()`: C-059 makes
    /// the two session directories part of the desired set on **every** prompt,
    /// so a shell that does not already hold them is one this reconcile
    /// genuinely changes — the premise the assertion needs is "nothing left to
    /// do", and an empty `PATH` is not that shell.
    #[test]
    fn a021_a_no_op_reconcile_emits_no_summary() {
        let empty = outcome(Vec::new(), Vec::new(), None);
        let current = settled_env();
        let plan = plan_for(&Ledger::empty(), &empty, &[Path::new("/tmp/ocx_home")], &current);
        let lines = reconcile_lines(
            Shell::Bash,
            &Ledger::empty(),
            "fp-1",
            None,
            &empty,
            &plan,
            &current,
            &plain(),
        );

        assert!(
            !lines.iter().any(|line| line.contains("printf '%s\\n'")),
            "a no-op prompt says nothing; got: {lines:#?}"
        );
    }
}

/// C-045 — no emitted snippet may ever call bare `ocx`.
///
/// The hazard is concrete and this WP created it: the activation stream defines
/// a shell **function** named `ocx`, and `command -v ocx` / `type -q ocx` /
/// `Get-Command ocx` all find functions. A bare call inside the stream's own
/// `$(…)` would therefore execute the wrapper, capture its output — including
/// the reconcile run's env-setting lines — and eval that as the global env.
#[cfg(test)]
mod bare_ocx_tests {
    use std::path::{Path, PathBuf};

    use clap::ValueEnum as _;
    use ocx_lib::shell::{Shell, escape};

    use super::{ActivateMode, activation_lines, format_global_env_eval, generate_completion_inline, ocx_binary_path};

    /// The arms whose activation stream defines a wrapper named `ocx`, and
    /// which therefore cannot contain a bare invocation.
    ///
    /// **Derived from `shell::hook::wrapper`, never hardcoded.** The exemption
    /// is not "these three shells are inconvenient" — it is that the hazard is
    /// structurally absent where no wrapper exists: batch (cmd.exe has no
    /// functions at all), elvish, nushell and the strict-POSIX family all get
    /// `None` from `wrapper`, so no `ocx` definition ever exists in those
    /// streams to shadow a call. Deriving the set means giving a new arm a
    /// wrapper without fixing its emitter fails this test, rather than silently
    /// widening the exemption.
    fn wrapper_arms() -> Vec<Shell> {
        let binary = ocx_binary_path(&bin_dir());
        let arms: Vec<Shell> = Shell::value_variants()
            .iter()
            .copied()
            .filter(|shell| ocx_lib::shell::hook::wrapper(*shell, &binary).is_some())
            .collect();
        assert!(
            !arms.is_empty(),
            "some arm must host a wrapper, or this guard is vacuous"
        );
        arms
    }

    fn bin_dir() -> PathBuf {
        PathBuf::from("/tmp/ocx_home/symlinks/ocx_sh/ocx/cli/current/content/bin")
    }

    /// C-059's other session directory — the global toolchain's trampolines.
    fn toolchain_bin_dir() -> PathBuf {
        PathBuf::from("/tmp/ocx_home/toolchain/bin")
    }

    /// The binary path as `shell` spells it inside its own quoting.
    ///
    /// `Path::join` produces a `\` on Windows, and fish is the one wrapper arm
    /// whose escapers double it — both `fish_single_quoted` and
    /// `fish_double_quoted` do, so the site's quoting context does not matter
    /// here. The doubling is correct: inside fish quotes a literal backslash
    /// must be written `\\` or fish eats it, so the needle has to carry it too.
    /// On POSIX the path holds no backslash and this is the identity.
    ///
    /// Escaper *correctness* is `escape.rs`'s own contract; what C-045 guards
    /// is that an invocation names the absolute binary, never a bare `ocx`.
    fn quoted_binary(shell: Shell, binary: &Path) -> String {
        let path = binary.to_string_lossy();
        match shell {
            Shell::Fish => escape::fish_single_quoted(&path),
            _ => path.into_owned(),
        }
    }

    /// Tier 1 — every ocx invocation in a wrapper-hosting stream is preceded by
    /// the resolved absolute path.
    ///
    /// The **positive** form, matching `shell::hook`'s own sibling guard: a
    /// denylist over the token `ocx` is useless here, because clap's completion
    /// block legitimately contains the word hundreds of times (`_ocx`,
    /// `complete -F _ocx ocx`, help text). So each *invocation* is located by
    /// its flag and required to carry the path immediately before it.
    #[test]
    fn c045_every_invocation_in_a_wrapper_arm_uses_the_absolute_path() {
        let binary = ocx_binary_path(&bin_dir());

        for shell in wrapper_arms() {
            let path = quoted_binary(shell, &binary);
            let stream = activation_lines(&super::LoginStream {
                shell,
                bin_path: &bin_dir(),
                toolchain_bin: &toolchain_bin_dir(),
                activate: ActivateMode::Env,
                completion: generate_completion_inline(shell).as_deref(),
                hook_binary: Some(&binary),
                wrapper_binary: None,
                watch: &[PathBuf::from("/work/acme/ocx.lock")],
                seed: None,
            })
            .join("\n");

            // The reconcile needle carries `--offline` because the emitted call
            // does: `--offline` is a root flag on `ContextOptions` and is not
            // declared `global`, so `hook`'s three appliers spell it *before*
            // the subcommand. Anchoring on the bare `self activate --reconcile`
            // would find the flag one token late and read `--offline ` as the
            // preceding text, which is exactly what this guard exists to reject.
            for flag in ["--global env", "--offline self activate --reconcile"] {
                let mut found = 0;
                for (at, _) in stream.match_indices(flag) {
                    found += 1;
                    let before = &stream[..at];
                    // Elvish's hook body rides inside a single-quoted `eval`
                    // string, so the path's own quotes are doubled a second
                    // time. Same rule, one more layer of quoting — spelled out
                    // rather than relaxed to a substring search, which would
                    // also accept an unquoted path.
                    let doubled = shell == Shell::Elvish && before.ends_with(&format!("{path}'' "));
                    assert!(
                        doubled || before.ends_with(&format!("{path}' ")) || before.ends_with(&format!("{path}\" ")),
                        "{shell:?}: `{flag}` at byte {at} is not preceded by the quoted absolute \
                         binary - a bare `ocx` here executes the wrapper this same stream defines. \
                         Preceding text: {:?}",
                        &before[before.len().saturating_sub(80)..]
                    );
                }
                assert!(found > 0, "{shell:?}: the stream must actually invoke `{flag}`");
            }
        }
    }

    /// The same rule for the standalone global-env-eval line, asserted against
    /// the emitter directly so a future caller that forgets to thread the binary
    /// cannot hide behind the stream assembly above.
    #[test]
    fn c045_the_global_env_eval_names_the_binary_on_every_wrapper_arm() {
        let binary = ocx_binary_path(&bin_dir());
        for shell in wrapper_arms() {
            let line = format_global_env_eval(shell, &binary);
            assert!(
                line.contains(&quoted_binary(shell, &binary)),
                "{shell:?} global env eval must name the resolved binary; got: {line:?}"
            );
        }
    }

    /// C-060 / D-4 — the wrapper follows a global `ocx` pin; without one it is
    /// the installed binary.
    ///
    /// The wrapper is a shell *function* named `ocx`, so it shadows `PATH`
    /// outright: left on the install path it would re-introduce, at exactly the
    /// interactive prompt the pin is typed at, the precedence C-060's re-order
    /// removed everywhere else.
    ///
    /// Both branches in one test, because either alone is satisfiable by a
    /// constant — a body that always answers the trampoline passes the pinned
    /// row, and today's shipped body passes the unpinned one.
    #[test]
    fn c060_the_wrapper_follows_a_global_ocx_pin_and_falls_back_to_the_install() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let file_structure = ocx_lib::file_structure::FileStructure::with_root(home.path().to_path_buf());
        let install_bin = ocx_lib::setup::ocx_install_bin_path(&file_structure);

        assert_eq!(
            super::wrapper_binary_path(&file_structure, &install_bin),
            ocx_binary_path(&install_bin),
            "with no rendered `ocx` trampoline the wrapper must name the installed binary"
        );

        let toolchain_bin = file_structure.toolchain.bin();
        std::fs::create_dir_all(&toolchain_bin).expect("mkdir toolchain/bin");
        let trampoline = ocx_binary_path(&toolchain_bin);
        std::fs::write(&trampoline, b"#!/bin/sh\n").expect("write trampoline");

        assert_eq!(
            super::wrapper_binary_path(&file_structure, &install_bin),
            trampoline,
            "a rendered `ocx` trampoline is what a bare `ocx` resolves on PATH, so the wrapper \
             must name it too"
        );
    }

    /// Live proof, on the shells this host can run: with a function named `ocx`
    /// defined **before** the stream is evaluated, the stream must not execute
    /// it.
    ///
    /// This is the assertion the bare-`ocx` regression flips. The function
    /// writes a marker file; a bare `$(ocx --global env …)` in the emitted
    /// stream runs it, so the marker appears. With the absolute path it cannot.
    #[cfg(unix)]
    #[test]
    fn c045_a_user_function_named_ocx_is_never_executed_by_the_stream() {
        use std::process::Command;

        for (argv, define) in [
            (["bash", "-c"], "ocx() { echo shadowed >> \"$MARKER\"; }"),
            (["zsh", "-c"], "ocx() { echo shadowed >> \"$MARKER\"; }"),
            (["fish", "-c"], "function ocx; echo shadowed >> $MARKER; end"),
        ] {
            let home = tempfile::TempDir::new().expect("tempdir");
            let marker = home.path().join("shadowed");
            let shell = match argv[0] {
                "bash" => Shell::Bash,
                "zsh" => Shell::Zsh,
                _ => Shell::Fish,
            };
            // Only the global-env-eval line is evaluated: it is the one emitted
            // statement that runs ocx at *stream evaluation* time. The hook and
            // wrapper bodies only run later, from a prompt.
            let line = format_global_env_eval(shell, &ocx_binary_path(&bin_dir()));
            let set_marker = if argv[0] == "fish" {
                format!("set -gx MARKER '{}'", marker.display())
            } else {
                format!("export MARKER='{}'", marker.display())
            };
            let script = format!("{set_marker}\n{define}\n{line}\n");

            let output = match Command::new(argv[0]).arg(argv[1]).arg(&script).output() {
                Ok(output) => output,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => panic!("failed to spawn {}: {error}", argv[0]),
            };
            assert!(
                output.status.success(),
                "{argv:?} exited {} on:\n{script}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !marker.exists(),
                "{argv:?}: the emitted stream executed the user's `ocx` function (C-045). \
                 A bare invocation would also capture the wrapper's output into the env stream."
            );
        }
    }
}

/// The two orderings this file carries that nothing else can guard.
///
/// Both are properties of *statement order* inside `run_reconcile` and
/// `compose`, so an outcome assertion cannot see them: swapping two lines leaves
/// every other test in the diff green. Both are closed here — one behaviourally,
/// one at compile time.
#[cfg(test)]
mod ordering_tests {
    use std::path::{Path, PathBuf};

    use clap::Parser as _;
    use ocx_lib::cli::ColorModeConfig;
    use ocx_lib::file_structure::FileStructure;
    use ocx_lib::project::consent::{self, Decision, Grant, Reason};
    use ocx_lib::shell::reconcile::{self, Ledger, Verdict};

    use ocx_lib::activation::{ConsentProof, authorized_project_env, is_stat_only, walk_is_indeterminate};

    use super::SelfActivate;
    use crate::app::Cli;

    /// C-042 — the per-prompt fast path reaches its short-circuit **before**
    /// `Context::try_init`, so a stat-only prompt reads no config at all.
    ///
    /// Observable because the config it would read is deliberately unparseable:
    /// reaching `Context::try_init` returns `Err`, short-circuiting returns
    /// `Ok`. That is the whole discrimination — a test asserting on emitted
    /// output cannot see this, because both orders emit nothing.
    ///
    /// Red state: swap `is_stat_only`'s early return with the
    /// `Context::try_init` call and this returns `Err`.
    #[tokio::test]
    async fn c042_a_stat_only_prompt_never_reaches_the_config_loader() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        // A project the walk resolves by explicit `--project`, so the test does
        // not depend on the runner's working directory.
        let project_dir = home.path().join("acme");
        std::fs::create_dir_all(&project_dir).expect("mkdir project");
        let project_file = project_dir.join("ocx.toml");
        std::fs::write(&project_file, "[tools]\n").expect("write ocx.toml");

        // The tripwire: any `ConfigLoader` pass over this file fails the run.
        let broken = home.path().join("broken.toml");
        std::fs::write(&broken, "this is not = = valid toml\n").expect("write the broken config");

        // Reproduce the ledger a previous inert prompt would have written: the
        // real fold over the real watch set, so the fast path's `fp` comparison
        // is the production one and not a stub.
        let canonical = consent::canonical_project_dir(&project_file).expect("canonicalize");
        let key = ocx_lib::reference_manager::ReferenceManager::name_for_path(&canonical);
        let watch = reconcile::watch_paths(&file_structure, Some(&project_file), Some(&key), None);
        let mut ledger = Ledger::empty();
        ledger.fp = reconcile::current_fingerprint(&watch, Some(&canonical));
        ledger.verdict = Some(Verdict::Inert);
        let carrier = ledger.encode().expect("the ledger must encode");

        let cli = Cli::parse_from([
            "ocx",
            "--config",
            broken.to_str().expect("utf8"),
            "--project",
            project_file.to_str().expect("utf8"),
            "self",
            "activate",
            "--shell=bash",
            "--reconcile",
        ]);
        let activate = SelfActivate::parse_from(["self-activate", "--shell=bash", "--reconcile"]);

        let outcome = activate
            .run_reconcile(
                &cli.context,
                ColorModeConfig {
                    stdout: false,
                    stderr: false,
                    relayed: false,
                },
                &file_structure,
                Some(&carrier),
            )
            .await;

        assert!(
            outcome.is_ok(),
            "an unchanged fingerprint with a cached `inert` verdict must short-circuit before any \
             config read; reaching the loader surfaces the deliberately-unparseable --config as: {:?}",
            outcome.err()
        );

        // The tripwire must be live: the same call with a moved fingerprint has
        // to reach the loader and fail. Without this the assertion above passes
        // for a build that never reads config on either path, which is
        // indistinguishable from the check never running.
        ledger.fp = "moved".to_owned();
        let stale = ledger.encode().expect("the ledger must encode");
        let reached = activate
            .run_reconcile(
                &cli.context,
                ColorModeConfig {
                    stdout: false,
                    stderr: false,
                    relayed: false,
                },
                &file_structure,
                Some(&stale),
            )
            .await;
        assert!(
            reached.is_err(),
            "a moved fingerprint must fall through to the config read - otherwise the assertion \
             above is vacuous and cannot tell a short-circuit from a build that reads nothing"
        );
    }

    /// C-028 — consent is evaluated before anything project-controlled is
    /// parsed, and the ordering is enforced by the type system rather than by
    /// statement order.
    ///
    /// `project_entries` takes a [`ConsentProof`], which only a `Decision` can
    /// mint. Hoisting the project parse above the consent evaluation is
    /// therefore a **compile error**, not a silent security regression — the
    /// mise-CVE ordering cannot be undone by moving a line.
    /// EC-CONSENT-009 — consent is decided before the project parses, so a refused project never mints the proof.
    #[test]
    fn c028_a_refused_project_cannot_mint_the_parse_proof() {
        assert!(
            ConsentProof::of(&Decision::Activate(Grant::Stamp)).is_some(),
            "an activating decision is what unlocks the project parse"
        );
        for refusal in [
            Reason::LockUnavailable,
            Reason::SourceSetDrift {
                new_sources: Default::default(),
            },
        ] {
            assert!(
                ConsentProof::of(&Decision::Inert(refusal.clone())).is_none(),
                "a refusal must not mint the proof `project_entries` requires ({refusal:?})"
            );
        }
    }

    /// S1 / CVE-2026-35533 (GHSA-436v-8fw5-4mj8) — a `namespaces` grant
    /// activates the **tool** channel and contributes **zero** `[env]` entries.
    ///
    /// The attack, end to end through the production chain: an attacker
    /// publishes a repository whose `ocx.lock` names only repositories inside a
    /// namespace the victim's fleet config granted — the packages need not
    /// exist, be pullable or be signed, it is one line of text the attacker
    /// writes — plus `ocx.toml` carrying
    /// `[env] PATH = { type = "path", value = "bin" }` and a malicious
    /// `bin/git`. A relative `path` value resolves against the project root, so
    /// on `cd` the clone's own `bin` would be PATH-front in the victim's live
    /// shell, with no stamp, no prompt and no network.
    ///
    /// This drives the real `ShellConsent` deserializer, the real
    /// `evaluate_with_stamp`, the real `ConsentProof` mint and the real
    /// `authorized_project_env` — the single seam `project_entries` builds
    /// `EnvScope::Project { env }` from.
    ///
    /// Red state: make `Grant::authorizes_project_env` return `true` for
    /// `Namespace` (the shipped behaviour) and the two zero-`[env]` assertions
    /// flip.
    #[test]
    fn s1_a_namespace_grant_activates_tools_but_contributes_no_project_env() {
        use ocx_lib::project::ProjectConfig;

        // The victim's documented fleet grant, built through the **shipped**
        // consent parser (`OCX_CONSENT_NAMESPACES`' own channel), so this
        // fixture cannot grant something production would have refused.
        let whitelist = ocx_lib::env_channel(None, Some("ocx.sh/acme-corp"));
        assert!(
            whitelist.namespaces.is_some() && whitelist.paths.is_empty(),
            "the fixture grants a namespace and no path, which is the attack's precondition"
        );

        // The attacker's project: a lock naming only the granted namespace, and
        // an `[env]` that puts the clone's own `bin` in front of PATH.
        let attacker = ProjectConfig::from_toml_str(concat!(
            "[tools]\n",
            "cmake = \"ocx.sh/acme-corp/cmake:1\"\n",
            "\n[env]\n",
            "PATH = { type = \"path\", value = \"bin\" }\n",
        ))
        .expect("the attacker's ocx.toml must parse - it is ordinary, valid input");
        let config_path = PathBuf::from("/work/clone/ocx.toml");
        let groups = vec![ocx_lib::project::DEFAULT_GROUP.to_owned()];

        // The declared set is non-empty, or "withheld nothing" would be
        // indistinguishable from "withheld everything".
        assert!(
            !ocx_lib::project::project_env_entries(&attacker, &config_path, &groups).is_empty(),
            "the fixture must declare an [env], or the assertions below are vacuous"
        );

        let sources: std::collections::BTreeSet<String> = ["ocx.sh/acme-corp".to_owned()].into_iter().collect();
        // No stamp, no `paths` entry: clause 2 is the whole of the grant. The
        // store's record is passed as corroborating, because this test is about
        // what clause 2 authorizes once it *does* hold — the separate question
        // of what makes it hold is `consent.rs`'s own suite. An attacker who
        // cannot corroborate never reaches this gate at all, which is strictly
        // narrower and would make these assertions vacuous.
        let decision = consent::evaluate_with_stamp(
            Path::new("/work/clone"),
            None,
            Some(&sources),
            Some(&sources),
            &whitelist,
        );
        assert_eq!(decision, Decision::Activate(Grant::Namespace));

        let proof = ConsentProof::of(&decision).expect(
            "clause 2 must still mint the proof - without it `compose` goes inert and the tool \
             channel it legitimately authorizes is lost too",
        );
        let (env, withheld) = authorized_project_env(proof, &attacker, &config_path, &groups);
        assert!(
            env.is_empty(),
            "a namespaces grant must contribute ZERO project [env] entries; got {env:?}"
        );
        assert!(withheld, "a withheld declaration owes the user one hint line");

        // The two explicit gestures naming this directory do authorize it.
        for granting in [Grant::Stamp, Grant::Path] {
            let proof = ConsentProof::of(&Decision::Activate(granting)).expect("a grant mints the proof");
            let (env, withheld) = authorized_project_env(proof, &attacker, &config_path, &groups);
            assert!(
                !env.is_empty(),
                "{granting:?} names this directory explicitly and must apply its [env]"
            );
            assert!(!withheld, "{granting:?} withholds nothing, so it owes no hint");
        }
    }

    /// A-13 / A-33 — a grant made through the `--config` overlay expires the
    /// cached `inert` verdict at the **next prompt**, not the next shell start.
    ///
    /// The explicit tier is a consent-bearing channel, and the emitted hook body
    /// invokes `--reconcile` with no `--config`, so a per-prompt process that
    /// re-derived its watch set could never see that file. The list is therefore
    /// recorded — seeded at shell start, carried in the ledger — and this walks
    /// the whole production chain: `watch_paths` -> `fingerprint` ->
    /// `is_stat_only`.
    ///
    /// Red state: drop `watch_paths`' `Some(recorded)` arm so it always
    /// re-derives, and the grant leaves the fingerprint unmoved — the prompt
    /// stays stat-only and the shell never activates.
    #[tokio::test]
    async fn a13_a_grant_through_the_config_overlay_expires_the_cached_inert_verdict() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let file_structure = FileStructure::with_root(home.path().to_path_buf());

        // The fleet's own tier, reachable only as `ocx --config …`.
        let overlay = home.path().join("fleet-consent.toml");
        std::fs::write(&overlay, "[shell]\n").expect("write the overlay");
        let tiers = vec![overlay.clone()];

        // The shell-start pass recorded it; a prompt has since gone inert.
        let watch = reconcile::watch_paths(&file_structure, None, None, Some(&tiers));
        let mut ledger = Ledger {
            tiers: tiers.clone(),
            ..Ledger::empty()
        };
        ledger.fp = reconcile::current_fingerprint(&watch, None);
        ledger.verdict = Some(Verdict::Inert);
        assert!(
            is_stat_only(&ledger, &reconcile::current_fingerprint(&watch, None)),
            "with nothing changed the prompt must be stat-only - otherwise the assertion below \
             passes for a build whose fingerprint never settles"
        );

        // Another terminal adds the grant.
        std::fs::write(&overlay, "[shell.consent]\npaths = [\"/work/acme\"]\n").expect("add the grant");

        assert!(
            !is_stat_only(&ledger, &reconcile::current_fingerprint(&watch, None)),
            "a grant written to the recorded --config overlay must expire the cached `inert` \
             verdict at the next prompt (A-13, A-33)"
        );
    }

    /// The seeding half: the startup stream hands the recorded tier list forward
    /// through the carrier, because that run is the only one that ever sees
    /// `--config`.
    #[test]
    fn a13_the_startup_stream_seeds_the_recorded_tier_list() {
        let bin = PathBuf::from("/tmp/ocx_home/symlinks/ocx_sh/ocx/cli/current/content/bin");
        let binary = super::ocx_binary_path(&bin);
        let overlay = PathBuf::from("/etc/fleet/consent.toml");
        let seed = super::seed_carrier(std::slice::from_ref(&overlay), &[]).expect("a non-empty tier list seeds");

        let stream = super::activation_lines(&super::LoginStream {
            shell: ocx_lib::shell::Shell::Bash,
            bin_path: &bin,
            toolchain_bin: &PathBuf::from("/tmp/ocx_home/toolchain/bin"),
            activate: super::ActivateMode::Env,
            completion: None,
            hook_binary: Some(&binary),
            wrapper_binary: None,
            watch: &[],
            seed: Some(&seed),
        });

        let carrier = stream
            .iter()
            .find(|line| line.contains(ocx_lib::shell::reconcile::CARRIER_KEY))
            .unwrap_or_else(|| panic!("the startup stream must export the seed; got: {stream:#?}"));
        let encoded = carrier
            .split_once('=')
            .expect("an export line")
            .1
            .trim_matches(|c| c == '\'' || c == ' ');
        let decoded = Ledger::decode(encoded).expect("the seed must decode");

        assert_eq!(decoded.tiers, vec![overlay], "the seed carries the recorded tier list");
        assert!(
            decoded.fp.is_empty(),
            "the seed carries no fingerprint, so the first prompt still reconciles (C-005, A-21)"
        );
        assert!(
            decoded.scopes.global.is_none() && decoded.scopes.project.is_none(),
            "the seed applies nothing - it is a record, not a scope"
        );
    }

    /// A-11 — an indeterminate walk retains the recorded scope; a determinate
    /// one reverts it.
    ///
    /// Red state: make `walk_is_indeterminate` return `false` unconditionally
    /// (the shipped behaviour before this fix) and the first assertion flips —
    /// a transient `.git` probe error tears the project scope down.
    #[test]
    fn a011_an_indeterminate_walk_retains_the_scope_a_determinate_one_reverts() {
        use ocx_lib::shell::reconcile::{ProjectScope, Scopes};

        let home = tempfile::TempDir::new().expect("tempdir");
        let project_dir = home.path().join("acme");
        std::fs::create_dir_all(&project_dir).expect("mkdir project");
        std::fs::write(project_dir.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");

        let recorded = |dir: PathBuf| Ledger {
            scopes: Scopes {
                global: None,
                global_priors: Default::default(),
                project: Some(ProjectScope {
                    key: "a1b2c3d4e5f60718".to_owned(),
                    dir,
                    applied: Vec::new(),
                    priors: Default::default(),
                }),
            },
            ..Ledger::empty()
        };
        let ledger = recorded(project_dir.clone());
        let inside = project_dir.join("src");
        std::fs::create_dir_all(&inside).expect("mkdir src");

        /// What the caller hands the predicate: the recorded project directory
        /// and nothing else.
        fn recorded_dir(ledger: &Ledger) -> Option<&Path> {
            ledger.scopes.project.as_ref().map(|scope| scope.dir.as_path())
        }

        // Indeterminate: the CWD is still inside the recorded project and its
        // `ocx.toml` is still a regular file, so a walk that returned nothing
        // did so for a reason the walk itself cannot report.
        assert!(
            walk_is_indeterminate(recorded_dir(&ledger), Some(&inside)),
            "a live recorded scope under the CWD must be retained (A-11)"
        );
        assert!(
            walk_is_indeterminate(recorded_dir(&ledger), Some(&project_dir)),
            "ancestor-or-self includes self"
        );

        // Determinate: a genuine leave.
        assert!(
            !walk_is_indeterminate(recorded_dir(&ledger), Some(home.path())),
            "a CWD outside the recorded directory is a genuine leave and must revert"
        );

        // Determinate: a genuine deletion.
        std::fs::remove_file(project_dir.join("ocx.toml")).expect("delete ocx.toml");
        assert!(
            !walk_is_indeterminate(recorded_dir(&ledger), Some(&inside)),
            "a deleted ocx.toml is a genuine deletion and must revert"
        );

        // Nothing recorded: the distinction cannot arise.
        assert!(
            !walk_is_indeterminate(recorded_dir(&Ledger::empty()), Some(&inside)),
            "with no recorded scope there is nothing to retain"
        );
    }
}

// ---------------------------------------------------------------------------
// WP-9 — Specify phase: the emitted `PATH`, read out of a real shell
// ---------------------------------------------------------------------------

/// C-059, C-060, RUL-62, RUL-87 — the session directories as a live shell sees
/// them.
///
/// **The assertion is the `PATH` a real `bash` ends up holding**, not the
/// desired vector and not the plan. RUL-62 is explicit about why: `export_path`
/// prepends, so the desired vector runs backwards relative to `PATH`, and an
/// implementation that copied `setup::session_path_directories`' slice verbatim
/// satisfies every assertion made on the vector while emitting exactly the
/// order C-060 forbids. The only operand that cannot lie is the shell's own
/// answer.
///
/// `bash` is required rather than skipped-when-absent: it is present on every
/// unix cargo leg, and a skip here would make the C-059 red — a live defect on
/// `goat` — indistinguishable from a run that never happened.
#[cfg(test)]
#[cfg(unix)]
mod emitted_path_tests {
    use std::path::{Path, PathBuf};

    use ocx_lib::activation::{Outcome, ProjectIdentity, SessionPath, next_ledger, plan_for};
    use ocx_lib::env::Env;
    use ocx_lib::package::metadata::env::entry::Entry;
    use ocx_lib::package::metadata::env::modifier::ModifierKind;
    use ocx_lib::shell::Shell;
    use ocx_lib::shell::reconcile::Ledger;

    use super::plan_lines;

    const OCX_HOME: &str = "/tmp/ocx-home";
    const PROJECT_DIR: &str = "/work/acme";
    const PROJECT_HOME: &str = "/work/acme/.ocx/toolchain";

    fn install_bin() -> PathBuf {
        PathBuf::from(OCX_HOME).join("symlinks/ocx.sh/ocx/cli/current/content/bin")
    }

    fn global_bin() -> PathBuf {
        PathBuf::from(OCX_HOME).join("toolchain/bin")
    }

    fn project_bin() -> PathBuf {
        PathBuf::from(PROJECT_HOME).join("bin")
    }

    fn path_entry(value: &str) -> Entry {
        Entry {
            key: "PATH".to_owned(),
            value: value.to_owned(),
            kind: ModifierKind::Path,
            separator: None,
        }
    }

    fn identity() -> ProjectIdentity {
        ProjectIdentity {
            config_path: PathBuf::from(PROJECT_DIR).join("ocx.toml"),
            dir: PathBuf::from(PROJECT_DIR),
            key: "a1b2c3d4e5f60718".to_owned(),
        }
    }

    fn outcome(session: SessionPath, global: Vec<Entry>, project: Vec<Entry>) -> Outcome {
        Outcome {
            session,
            global,
            project,
            owned_home: Some(PathBuf::from(PROJECT_HOME)),
            resolved: true,
            slot: Some(identity()),
            inert: false,
            messages: Vec::new(),
        }
    }

    fn env_with_path(segments: &[&Path]) -> Env {
        let mut env = Env::clean();
        env.set(
            "PATH",
            std::env::join_paths(segments.iter().map(|path| path.as_os_str())).expect("the seed joins"),
        );
        env
    }

    /// Run `lines` under `bash` over a `PATH` seeded with `seed`, and answer the
    /// `PATH` segments the shell is left holding.
    ///
    /// The emitted statements are the product here, so they are executed rather
    /// than pattern-matched: `export_path`'s bash arm is a colon-sentinel
    /// dedupe loop whose *effect* is the contract, and a substring assertion on
    /// it would pass for a line that removed the segment it was supposed to
    /// prepend.
    fn path_after(seed: &[&Path], lines: &[String]) -> Vec<String> {
        let seeded = std::env::join_paths(seed.iter().map(|path| path.as_os_str())).expect("the seed joins");
        let script = format!(
            "PATH='{}'; export PATH\n{}\nprintf '%s' \"$PATH\"\n",
            ocx_lib::shell::escape::posix_single_quoted(&seeded.to_string_lossy()),
            lines.join("\n")
        );
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("bash must be present on every unix cargo leg; a skip here would hide the red");
        assert!(
            output.status.success(),
            "the emitted stream must be valid bash; stderr: {}\nscript:\n{script}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .split(':')
            .map(str::to_owned)
            .collect()
    }

    fn position(path: &[String], needle: &Path) -> usize {
        let needle = needle.to_string_lossy();
        path.iter()
            .position(|segment| segment.as_str() == &*needle)
            .unwrap_or_else(|| panic!("{needle} must be on PATH; got {path:#?}"))
    }

    /// **C-059 — the live defect on `goat`, driven through the shell.**
    ///
    /// `ocx self setup` registers `ocx_install_bin_path` at session level; the
    /// per-prompt reconciler owns `$OCX_HOME` and removes every segment under it
    /// the desired set does not contribute. Only the *startup* stream emits the
    /// install directory, so the **first prompt of any shell strips it** — the
    /// user's `ocx` stops resolving.
    ///
    /// Red state — today's `goat`: `desired_entries` returns `global ++ project`
    /// and never splices [`Outcome::session`], so the emitted stream carries a
    /// `remove_list_element` for the install directory and `bash` drops it.
    #[test]
    fn c059_the_first_prompt_of_a_shell_does_not_strip_the_install_directory() {
        let live = [
            install_bin(),
            global_bin(),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
        ];
        let seed: Vec<&Path> = live.iter().map(PathBuf::as_path).collect();
        let current = env_with_path(&seed);

        // `activate = "none"` — the arm that composes nothing for the project,
        // with one global package so `PATH` is a key the repair examines.
        let applied = outcome(
            SessionPath {
                global_bin: global_bin(),
                project_bin: None,
                install_bin: install_bin(),
            },
            vec![path_entry(&format!("{OCX_HOME}/packages/gg/global/content/bin"))],
            Vec::new(),
        );
        let ledger = next_ledger(&Ledger::empty(), "fp-1", &applied, &Env::clean());
        let owned: Vec<&Path> = vec![Path::new(OCX_HOME), Path::new(PROJECT_HOME)];
        let plan = plan_for(&ledger, &applied, &owned, &current);

        let path = path_after(&seed, &plan_lines(Shell::Bash, &plan));
        position(&path, &install_bin());
        position(&path, &global_bin());
    }

    /// **C-060, RUL-62, RUL-87 — the order, read off the shell's own `PATH`.**
    ///
    /// Front to back: the project's composed entries, then
    /// `ocx_install_bin_path`, then the project's `<home>/toolchain/bin`, then
    /// `$OCX_HOME/toolchain/bin`, then the global tier's composed entries.
    ///
    /// The last comparison is RUL-87's: a splice that put the global tier after
    /// the session block would let a globally installed tool shadow the
    /// project's own trampoline.
    ///
    /// Red state: emit the session directories in their documented `PATH` order
    /// rather than reversed — i.e. hand the splice
    /// `setup::session_path_directories`' slice.
    #[test]
    fn c060_rul62_rul87_the_shells_path_orders_the_five_contributors() {
        let global_tool = format!("{OCX_HOME}/packages/gg/global/content/bin");
        let project_tool = format!("{OCX_HOME}/packages/pp/project/content/bin");
        let seed = [Path::new("/usr/bin")];

        let applied = outcome(
            SessionPath {
                global_bin: global_bin(),
                project_bin: Some(project_bin()),
                install_bin: install_bin(),
            },
            vec![path_entry(&global_tool)],
            vec![path_entry(&project_tool)],
        );
        let plan = plan_for(
            &Ledger::empty(),
            &applied,
            &[Path::new(OCX_HOME), Path::new(PROJECT_HOME)],
            &env_with_path(&seed),
        );

        let path = path_after(&seed, &plan_lines(Shell::Bash, &plan));
        let composed_project = position(&path, Path::new(&project_tool));
        let install = position(&path, &install_bin());
        let project = position(&path, &project_bin());
        let global = position(&path, &global_bin());
        let composed_global = position(&path, Path::new(&global_tool));

        assert!(
            composed_project < project && project < global && global < install && install < composed_global,
            "C-060/RUL-87: project-composed ▸ project bin/ ▸ global toolchain bin ▸ install_bin ▸ \
             global-composed; got {path:#?}"
        );
    }

    /// C-059 — a shell whose `PATH` lost both session directories gets them
    /// back on the next prompt. The reconciler contributes them; it does not
    /// merely refrain from deleting them.
    ///
    /// Red state: make the splice conditional on anything at all.
    #[test]
    fn c059_a_shell_that_lost_the_session_directories_has_them_restored() {
        let seed = [Path::new("/usr/bin")];
        let applied = outcome(
            SessionPath {
                global_bin: global_bin(),
                project_bin: None,
                install_bin: install_bin(),
            },
            Vec::new(),
            Vec::new(),
        );
        let plan = plan_for(
            &Ledger::empty(),
            &applied,
            &[Path::new(OCX_HOME)],
            &env_with_path(&seed),
        );

        let path = path_after(&seed, &plan_lines(Shell::Bash, &plan));
        assert_eq!(
            position(&path, &install_bin()),
            position(&path, &global_bin()) + 1,
            "C-059/C-060: both come back, the toolchain bin in front; got {path:#?}"
        );
    }
}

/// D-3 — the global toolchain tier honours its own `activate` setting.
///
/// Two halves, split by what they need: the ladder over `$OCX_HOME/ocx.toml`
/// (pure, a path and a tempdir), and the gate that consumes it (a real
/// [`crate::app::Context`], so the resolver it skips is the production one).
#[cfg(test)]
mod global_activate_tests {
    use clap::Parser as _;
    use ocx_lib::activate::ActivateMode;
    use ocx_lib::cli::ColorModeConfig;

    use super::{global_activate_mode, global_prompt_entries};
    use crate::app::{Cli, Context, ManagedConfigGate};

    /// Write `$OCX_HOME/ocx.toml` and resolve the global tier's mode over it.
    async fn mode_for(home: &std::path::Path, body: Option<&str>) -> ActivateMode {
        let file = home.join("ocx.toml");
        match body {
            Some(body) => std::fs::write(&file, body).expect("write the global ocx.toml"),
            None => {
                let _ = std::fs::remove_file(&file);
            }
        }
        global_activate_mode(home).await
    }

    /// A global file that **states** a mode decides, for each of the three.
    ///
    /// Every case states a value, so the environment tier — the weakest — can
    /// never reach the result and this test needs no process-env mutation.
    ///
    /// Red state: collapse the `Bin | None` arm of `global_prompt_entries`'
    /// match into `Env`, or drop `config.activate` from
    /// `ocx_lib::activation::activate_mode`'s `Ladder`.
    #[tokio::test]
    async fn d3_a_global_file_that_states_a_mode_decides() {
        let home = tempfile::TempDir::new().expect("tempdir");
        for (body, expected) in [
            ("activate = \"env\"\n", ActivateMode::Env),
            ("activate = \"bin\"\n", ActivateMode::Bin),
            ("activate = \"none\"\n", ActivateMode::None),
        ] {
            assert_eq!(
                mode_for(home.path(), Some(body)).await,
                expected,
                "the global tier reads its own `activate` key: {body:?}"
            );
        }
    }

    /// The ladder's other three tiers on this tier: leniency, the floor, and
    /// `OCX_TOOLCHAIN_ACTIVATE` as the **weakest** rung.
    ///
    /// The only test in this module that touches `OCX_TOOLCHAIN_ACTIVATE`.
    /// A sibling below writes and removes `OCX_HOME`, so "the only test that
    /// mutates the environment" would be false; what holds — and what the
    /// ordering guarantee actually needs — is that no other test in this crate
    /// writes *this key* (precedent: `crate::options::hook`'s
    /// `each_ladder_reads_its_own_environment_key`).
    ///
    /// Red state: replace `.unwrap_or_default()` in `global_activate_mode` with
    /// `.map(|c| ...).unwrap_or(ActivateMode::None)` and the corrupt-file
    /// assertions flip — a malformed global file would silently stop composing.
    #[tokio::test]
    async fn d3_the_ladder_is_lenient_and_the_environment_tier_is_the_weakest() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let key = ocx_lib::env::keys::OCX_TOOLCHAIN_ACTIVATE;
        // SAFETY: `OCX_TOOLCHAIN_ACTIVATE` has no other writer in this crate —
        // the sibling below writes `OCX_HOME`, a different key — and nextest
        // runs one test per process, so no concurrent reader of this key exists
        // to race.
        unsafe { std::env::remove_var(key) };

        assert_eq!(
            mode_for(home.path(), None).await,
            ActivateMode::Env,
            "no global ocx.toml at all: the file tier is absent and the floor decides"
        );
        assert_eq!(
            mode_for(home.path(), Some("[tools]\n")).await,
            ActivateMode::Env,
            "a global file that states no `activate`: the floor decides"
        );
        assert_eq!(
            mode_for(home.path(), Some("this is not = = valid toml\n")).await,
            ActivateMode::Env,
            "leniency: an unparseable global ocx.toml leaves the file tier ABSENT and still \
             composes - a prompt must never fail, warn, or silently fall to `none` over it"
        );

        // SAFETY: see above.
        unsafe { std::env::set_var(key, "bin") };
        assert_eq!(
            mode_for(home.path(), Some("[tools]\n")).await,
            ActivateMode::Bin,
            "with no key in the global file, OCX_TOOLCHAIN_ACTIVATE decides"
        );
        assert_eq!(
            mode_for(home.path(), Some("this is not = = valid toml\n")).await,
            ActivateMode::Bin,
            "a corrupt file falls THROUGH to the environment tier, it does not short-circuit"
        );
        assert_eq!(
            mode_for(home.path(), Some("activate = \"env\"\n")).await,
            ActivateMode::Env,
            "and it loses to a global file that states a value - the weakest-tier rule holds on \
             this tier too"
        );

        // SAFETY: see above.
        unsafe { std::env::remove_var(key) };
    }

    /// The gate itself, against a real [`Context`]: `bin` composes nothing while
    /// the **explicit** `ocx --global env` path over the same home still
    /// composes.
    ///
    /// The observable is the global file's own `[env]`, which
    /// `resolve_global_pinned_env` reads on its own authority (Q2) — so this
    /// needs no lock, no registry and no materialised package.
    ///
    /// The `env`-mode re-read at the end is the positive control: without it an
    /// empty vector under `bin` is indistinguishable from a build that composes
    /// nothing in any mode.
    ///
    /// Red state: replace `global_prompt_entries`' `Bin | None` arm with the
    /// `Env` arm's body (i.e. move the gate away, or into the resolver) and the
    /// first assertion fails.
    #[tokio::test]
    async fn d3_bin_mode_gates_the_prompt_and_leaves_the_explicit_request_alone() {
        let home = tempfile::TempDir::new().expect("tempdir");
        // SAFETY: `OCX_HOME` is read through `ocx_lib::env::var`, whose
        // `#[cfg(test)]` override seam is internal to `ocx_lib` and therefore
        // unavailable from this crate; the process variable is the only seam.
        // nextest runs one test per process, so this cannot race a sibling.
        unsafe { std::env::set_var("OCX_HOME", home.path()) };

        let global_config = home.path().join("ocx.toml");
        std::fs::write(&global_config, "activate = \"bin\"\n[env]\nWP16 = \"composed\"\n").expect("write ocx.toml");

        let cli = Cli::parse_from(["ocx", "self", "activate", "--shell=bash", "--reconcile"]);
        let color = ColorModeConfig {
            stdout: false,
            stderr: false,
            relayed: false,
        };
        let context = Context::try_init(
            &cli.context,
            color,
            ManagedConfigGate {
                enforce_required: false,
                onboarding: false,
            },
        )
        .await
        .expect("a context over an empty ocx home");
        assert_eq!(
            context.file_structure().root(),
            home.path(),
            "the context must resolve the tempdir as its home, or this test reads someone else's \
             ocx.toml and proves nothing"
        );
        let target = crate::conventions::platform_or_default(None);

        let keys = |entries: &[ocx_lib::package::metadata::env::entry::Entry]| -> Vec<String> {
            entries.iter().map(|entry| entry.key.clone()).collect()
        };

        assert!(
            global_prompt_entries(&context, &target)
                .await
                .expect("the gate never fails on a well-formed home")
                .is_empty(),
            "D-3: with `activate = \"bin\"` the global tier contributes nothing to a prompt"
        );

        // The regression guard for the whole design: the gate is at the hook,
        // not in the shared resolver, so an EXPLICIT `ocx --global env` over the
        // very same home still composes. Moving the gate into
        // `resolve_global_pinned_env` reds here and nowhere else.
        let explicit = crate::command::toolchain_env::resolve_global_pinned_env(&context, &target, &[], &[], None)
            .await
            .expect("the explicit path never fails on a declared [env]")
            .expect("a declared [env] is a contribution")
            .0;
        assert_eq!(
            keys(&explicit),
            vec!["WP16".to_owned()],
            "`activate` governs automatic activation, never what a typed command prints"
        );

        // Positive control: the same home in `env` mode does compose, so the
        // empty vector above is the gate's doing and not a home that had
        // nothing to give.
        std::fs::write(&global_config, "activate = \"env\"\n[env]\nWP16 = \"composed\"\n").expect("rewrite ocx.toml");
        assert_eq!(
            keys(
                &global_prompt_entries(&context, &target)
                    .await
                    .expect("the gate never fails on a well-formed home")
            ),
            vec!["WP16".to_owned()],
            "`env` mode composes the global tier, exactly as it always has"
        );

        // SAFETY: see above.
        unsafe { std::env::remove_var("OCX_HOME") };
    }

    // ── A-12's other half: the LOGIN stream ─────────────────────────────────

    /// The install-bin directory a login stream is built over.
    fn bin_dir() -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/ocx_home/symlinks/ocx_sh/ocx/cli/current/content/bin")
    }

    /// C-059's other session directory — the global toolchain's trampolines.
    fn toolchain_bin_dir() -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/ocx_home/toolchain/bin")
    }

    /// The login stream under a stated global `activate` mode.
    ///
    /// Five shells, not all ten: the gate sits *outside* per-shell formatting,
    /// so exhaustiveness buys nothing here and the per-shell spelling of both
    /// emitted lines is covered by `format_global_env_eval`'s and
    /// `path_prepend_line`'s own tests. What this list does carry is every
    /// shape the two needles take — the POSIX `eval`, fish's `| source`, pwsh's
    /// `Invoke-Expression`, elvish's capture-then-`eval`, and nushell, the one
    /// arm that consumes `--format json --global env` as data.
    const GATE_SHELLS: [ocx_lib::shell::Shell; 5] = [
        ocx_lib::shell::Shell::Bash,
        ocx_lib::shell::Shell::Fish,
        ocx_lib::shell::Shell::PowerShell,
        ocx_lib::shell::Shell::Elvish,
        ocx_lib::shell::Shell::Nushell,
    ];

    fn login_stream(shell: ocx_lib::shell::Shell, activate: ActivateMode) -> Vec<String> {
        super::activation_lines(&super::LoginStream {
            shell,
            bin_path: &bin_dir(),
            toolchain_bin: &toolchain_bin_dir(),
            activate,
            completion: None,
            hook_binary: Some(&super::ocx_binary_path(&bin_dir())),
            wrapper_binary: None,
            watch: &[],
            seed: None,
        })
    }

    /// Does this stream ask `ocx` for the global environment?
    ///
    /// Nushell is the one arm off the `--shell=NAME` channel: it has no string
    /// `eval`, so it consumes the structured `--format json --global env`
    /// output. Same question, different spelling — the needle follows the arm
    /// exactly as `global_env_eval_invokes_global_env_for_any_shell` spells it.
    fn composes_global_env(stream: &[String], shell: ocx_lib::shell::Shell) -> bool {
        let needle = if shell == ocx_lib::shell::Shell::Nushell {
            "--format json --global env"
        } else {
            "--global env"
        };
        stream.iter().any(|line| line.contains(needle))
    }

    /// A-12's other half — the **login** stream composes the global env in
    /// `env` mode only.
    ///
    /// A-12 (2026-09-06) gated the per-prompt arm and recorded the gap as
    /// closed, but two emitters put a global environment into a shell and only
    /// one was gated: `$OCX_HOME/env.sh` runs `ocx self activate`, whose stream
    /// pushed the eval in every mode. A global `activate = "bin"` therefore
    /// looked inert — repaired at the first prompt in an interactive shell, and
    /// never in a script, an `ssh host cmd`, a git hook or a plain `sh`, which
    /// register no hook at all.
    ///
    /// The `env` arm is the positive control: without it, a stream that emitted
    /// the eval in no mode at all would pass the `bin`/`none` assertions.
    ///
    /// Red state: drop the `activate == ActivateMode::Env` guard around
    /// `format_global_env_eval` in `activation_lines`.
    #[test]
    fn a12_the_login_stream_composes_the_global_env_in_env_mode_only() {
        for shell in GATE_SHELLS {
            for mode in [ActivateMode::Bin, ActivateMode::None] {
                let stream = login_stream(shell, mode);
                assert!(
                    !composes_global_env(&stream, shell),
                    "{shell:?} in {mode} mode must not ask ocx for the global env - the tools come \
                     from the trampolines in toolchain/bin; got: {stream:#?}"
                );
            }

            let stream = login_stream(shell, ActivateMode::Env);
            assert!(
                composes_global_env(&stream, shell),
                "{shell:?} in env mode must still compose the global env; got: {stream:#?}"
            );
        }
    }

    /// C-059 — both session directories reach a login shell in **every** mode.
    ///
    /// `$OCX_HOME/toolchain/bin` is a session-level fact, not a composition:
    /// the reconciler holds it desired unconditionally
    /// (`SessionPath::global_bin`), and the login stream now emits it too, so
    /// `bin` mode still resolves its tools where the OS session-registration
    /// channel `ocx self setup` writes is absent or ignored — a container, a
    /// host without systemd user env, `--no-modify-path`, or a non-interactive
    /// shell that never renders a prompt to be repaired at.
    ///
    /// Asserted against `path_prepend_line`'s own answer rather than a
    /// substring: the directory's path also appears inside the hook body, so a
    /// `contains` would stay green with no prepend at all.
    ///
    /// Red state: delete the `path_prepend_line(shell, toolchain_bin)` line, or
    /// make it conditional on the mode.
    #[test]
    fn c059_the_login_stream_prepends_the_global_toolchain_bin_in_every_mode() {
        let mut asserted = 0;
        for shell in GATE_SHELLS {
            let Some(expected) = super::path_prepend_line(shell, &toolchain_bin_dir()) else {
                continue;
            };
            for mode in [ActivateMode::Env, ActivateMode::Bin, ActivateMode::None] {
                let stream = login_stream(shell, mode);
                assert!(
                    stream.contains(&expected),
                    "{shell:?} in {mode} mode must prepend the global toolchain bin dir; \
                     expected {expected:?} in: {stream:#?}"
                );
                asserted += 1;
            }
        }
        assert!(
            asserted >= 12,
            "the loop must have asserted something for nearly every shell and mode - a shell set \
             whose arms all decline to express a prepend would make this test vacuous; got \
             {asserted} assertions"
        );
    }

    /// The two session directories arrive in C-060's order: the global
    /// trampolines in front of the installed `ocx`, so a global toolchain that
    /// pins `ocx` is the one a login shell resolves (D-4).
    ///
    /// Red state: swap the two `path_prepend_line` calls in `activation_lines`.
    #[test]
    fn c060_the_login_stream_puts_the_toolchain_bin_in_front_of_the_install_bin() {
        for shell in GATE_SHELLS {
            let (Some(install), Some(toolchain)) = (
                super::path_prepend_line(shell, &bin_dir()),
                super::path_prepend_line(shell, &toolchain_bin_dir()),
            ) else {
                continue;
            };
            let stream = login_stream(shell, ActivateMode::Bin);
            let at = |needle: &String| {
                stream
                    .iter()
                    .position(|line| line == needle)
                    .unwrap_or_else(|| panic!("{shell:?}: the stream must carry {needle:?}; got {stream:#?}"))
            };
            assert!(
                at(&install) < at(&toolchain),
                "{shell:?}: the install bin is emitted FIRST so the toolchain bin lands in front of \
                 it on PATH (C-060); got {stream:#?}"
            );
        }
    }
}
