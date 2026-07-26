// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one place a recording decision can be minted.
//!
//! [`RecordingPolicy`] has private fields and no public constructor, and
//! [`resolve_records`] lives in this module and nowhere else. A launching frame
//! therefore cannot fabricate "recording is off" — only configuration can say
//! that. Placing either item loosely in the `record` surface would degrade the
//! guarantee to "anything in `record` can mint".
//!
//! The guarantee is module-scoped by design: `#[non_exhaustive]` would be
//! useless here, since it blocks literal construction only across crates and
//! `ocx_lib` is the crate that matters.

use std::path::{Path, PathBuf};

use super::error::RecordsError;
use super::name_template::{DEFAULT_TEMPLATE, NameTemplate};
use super::options::RecordsOptions;

/// The resolved recording decision for one invocation.
///
/// "Configured off" is encoded *inside* the policy rather than as an `Option`
/// around it — an `Option` at this boundary would leave the common case with no
/// constructor and reintroduce a caller-visible `None` to pass.
#[derive(Debug, Clone)]
pub struct RecordingPolicy {
    /// Resolved sink directory, pinned to where it really was at resolve time.
    /// `None` is the configured-off state.
    dir: Option<PathBuf>,

    /// Resolved and validated name template, defaulted when unset. Held parsed
    /// so an unknown placeholder is a configuration error at resolve time rather
    /// than a surprise filename at write time.
    name: NameTemplate,

    /// Fail posture: abort the launch when a record cannot be written.
    required: bool,
}

impl RecordingPolicy {
    /// The sink directory, or `None` when recording is configured off.
    ///
    /// Resolved to its real location at [`resolve_records`], so this is the
    /// directory the operator designated rather than the spelling they typed —
    /// see [`pin_sink`].
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Whether this invocation writes a record at all.
    pub fn is_recording(&self) -> bool {
        self.dir.is_some()
    }

    /// The validated name template, with the default already applied.
    pub fn name(&self) -> &NameTemplate {
        &self.name
    }

    /// The two fields a child ocx cannot re-derive on its own, in the shape
    /// [`OcxConfigView`](crate::env::OcxConfigView) forwards.
    ///
    /// A child re-reads the config file and the environment for itself, so those
    /// two tiers would resolve identically without any forwarding — but the CLI
    /// tier is per-invocation, and `Env::apply_ocx_config` is set-or-**remove**,
    /// so a launching frame that resolved a flag must hand the result down or
    /// the child records somewhere else, or nowhere.
    ///
    /// `required` and `system_locked` are deliberately absent: the posture is a
    /// config-file decision at every tier and the lock is loader-set provenance,
    /// so both are re-derived by the child from the config chain it resolves.
    pub fn forwarded(&self) -> RecordsOptions {
        RecordsOptions {
            dir: self.dir.clone(),
            name: Some(self.name.as_str().to_string()),
            required: None,
            system_locked: false,
        }
    }

    /// Whether a failed record aborts the launch.
    ///
    /// `false` warns on stderr and runs anyway; `true` exits 74 before the child
    /// starts. A developer who typed `--records-dir` once and fat-fingered the
    /// path should not have their build die for a policy nobody set, which is
    /// why the fail-closed posture lives with the policy and not with the flag.
    pub fn required(&self) -> bool {
        self.required
    }
}

/// Fold the three configuration tiers into the invocation's recording decision.
///
/// Precedence is default, then config file, then environment, then CLI
/// arguments — one fold, every field, highest last.
///
/// The SYSTEM-scope clamp is binary and per-block, not per-field: with no
/// operator policy the sink is the caller's, filename pattern included; the
/// moment an operator declares one at SYSTEM scope the whole block is theirs,
/// because a collector downstream now depends on all of it. It lives as an early
/// return in [`RecordsOptions::merge`] rather than as a step here, so the file
/// tiers — which fold in the config loader and never reach this function — get
/// the same clamp from the same line.
///
/// `required` is read from the **config tier only**, at every level: it is
/// absent from [`RecordsOptions`] values built from the environment or CLI
/// flags, and this function must ignore it there rather than trusting the shape.
/// It defaults `false` unlocked and `true` when SYSTEM-locked.
///
/// # Errors
///
/// Returns [`RecordsError`] when a **configured** sink's name template is
/// malformed or cannot produce a distinct name per record. With no sink there is
/// no filename to validate and this function does not fail.
pub fn resolve_records(
    config: RecordsOptions,
    env: RecordsOptions,
    args: RecordsOptions,
) -> Result<RecordingPolicy, RecordsError> {
    let mut merged = config;
    // The SYSTEM clamp needs no guard here — `RecordsOptions::merge` early-returns
    // when the accumulator is locked, which is what makes it free for the file
    // tiers too. `required` does need one: the environment and the CLI have no
    // channel for it by design, so it is dropped from those two layers rather
    // than trusted to be absent.
    let system_locked = merged.system_locked;
    merged.merge(RecordsOptions { required: None, ..env });
    merged.merge(RecordsOptions { required: None, ..args });

    // Fail closed under an operator policy, warn otherwise. An operator who
    // disagrees writes `required = false` in the same system-scope file.
    let required = merged.required.unwrap_or(system_locked);

    // With no sink nothing renders a filename, so nothing validates one. Parsing
    // it anyway would let a machine-wide `OCX_RECORDS_NAME` with no `dir` exit 78
    // out of every `ocx exec` on the host — generated entrypoint launchers
    // included, which take no flags and so cannot opt out — and would contradict
    // the published contract that the variable has no effect without a sink.
    let Some(dir) = merged.dir else {
        // A tier that *wrote* `required = true` and named no sink asked for
        // something that cannot happen. The launch path early-returns on a
        // policy that records nothing, before any posture is applied, so
        // resolving this to "off" would silently disable recording on every
        // host that carries the file. Refused here rather than there, so the
        // operator hears about it before the work — the `SinkSymlink`
        // precedent, and a configuration fault by the same measure.
        //
        // The trigger is the *explicit* `Some(true)`, never the SYSTEM-locked
        // default `required` resolves to above: a locked `[records]` block with
        // no `dir` is plausibly an operator locking recording off for the host,
        // and that must keep working exactly as it does.
        if merged.required == Some(true) {
            return Err(RecordsError::RequiredWithoutSink);
        }
        return Ok(RecordingPolicy {
            dir: None,
            name: NameTemplate::default(),
            required,
        });
    };

    Ok(RecordingPolicy {
        dir: Some(pin_sink(dir)),
        // Parsed here rather than at write time, so a bad placeholder surfaces
        // while the operator is still looking at their config and before any
        // child starts.
        name: NameTemplate::parse(merged.name.as_deref().unwrap_or(DEFAULT_TEMPLATE))?,
        required,
    })
}

/// Resolve the operator's sink to the directory it really names, once.
///
/// This is the moment of designation, and the only filesystem touch in
/// resolution — one path walk per invocation, on a path an operator typed.
///
/// Refusing a sink for merely *containing* a symlink was the wrong guard: macOS
/// reaches `/var/log/ocx/records` through `/var` → `/private/var`, so it refused
/// an ordinary host on every launch, and under `required = true` refused it
/// permanently. The property worth keeping is that nobody redirects the trail
/// **after** the operator designated it — so the designation is fixed here and
/// [`super::sink`] refuses to write anywhere else.
///
/// A sink that cannot be resolved — the ordinary fat-fingered path, absent on
/// disk — keeps the spelling as configured, so the I/O error names what the
/// operator typed and the warn posture still merely warns.
fn pin_sink(dir: PathBuf) -> PathBuf {
    dunce::canonicalize(&dir).unwrap_or(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};

    fn options(dir: Option<&str>, name: Option<&str>, required: Option<bool>) -> RecordsOptions {
        RecordsOptions {
            dir: dir.map(PathBuf::from),
            name: name.map(str::to_string),
            required,
            system_locked: false,
        }
    }

    fn locked(dir: Option<&str>, name: Option<&str>, required: Option<bool>) -> RecordsOptions {
        let mut options = options(dir, name, required);
        options.lock_as_system();
        options
    }

    fn resolve(config: RecordsOptions, env: RecordsOptions, args: RecordsOptions) -> RecordingPolicy {
        resolve_records(config, env, args).expect("the fixture templates are valid")
    }

    // ── the configured-off state ─────────────────────────────────────────────

    /// An absent `dir` yields a policy that records nothing — encoded inside the
    /// policy, never as an `Option` around it, so a launching frame has no
    /// caller-visible `None` to fabricate.
    #[test]
    fn absent_dir_yields_a_configured_off_policy() {
        let policy = resolve(
            RecordsOptions::default(),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );
        assert!(!policy.is_recording(), "no sink configured means no recording");
        assert!(policy.dir().is_none());
    }

    /// A configured sink yields a recording policy.
    #[test]
    fn configured_dir_yields_a_recording_policy() {
        let policy = resolve(
            options(Some("/var/log/ocx/records"), None, None),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );
        assert!(policy.is_recording());
        assert_eq!(policy.dir(), Some(Path::new("/var/log/ocx/records")));
    }

    // ── the sink is pinned to where it really is ─────────────────────────────

    /// An OS alias in the sink path is ordinary, not an attack: macOS reaches
    /// `/var/log/ocx/records` through `/var` → `/private/var`. Refusing it made
    /// every macOS host a dead host under `required = true`, so the path is
    /// resolved once here instead.
    #[cfg(unix)]
    #[test]
    fn a_sink_reached_through_a_symlinked_ancestor_pins_to_the_real_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let root = dunce::canonicalize(root.path()).expect("a just-created tempdir resolves");
        let real = root.join("private");
        std::fs::create_dir(&real).expect("create the real directory");
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("alias the ancestor");
        let sink = real.join("records");
        std::fs::create_dir(&sink).expect("create the sink");

        let policy = resolve(
            options(alias.join("records").to_str(), None, None),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );

        assert_eq!(
            policy.dir(),
            Some(sink.as_path()),
            "the aliased sink must pin to the directory it really names"
        );
    }

    /// A sink that does not exist keeps the spelling the operator typed, so the
    /// I/O error names their path rather than something resolution invented.
    #[test]
    fn an_unresolvable_sink_keeps_the_configured_spelling() {
        let root = tempfile::tempdir().expect("tempdir");
        let absent = root.path().join("never-created");

        let policy = resolve(
            options(absent.to_str(), None, None),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );

        assert_eq!(policy.dir(), Some(absent.as_path()));
    }

    // ── precedence: default → config → env → args ────────────────────────────

    /// The name default is applied at resolve time, not at write time, so the
    /// policy always carries a parsed template.
    #[test]
    fn name_default_is_applied() {
        let policy = resolve(
            options(Some("/records"), None, None),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );
        assert_eq!(policy.name().as_str(), DEFAULT_TEMPLATE);
    }

    /// Each tier wins its own field at its own level, and a tier that says
    /// nothing does not clobber a lower one.
    #[test]
    fn each_tier_wins_the_field_it_speaks_for() {
        let policy = resolve(
            options(Some("/from-config"), Some("{pid}.json"), None),
            options(Some("/from-env"), None, None),
            options(None, Some("{time}-{rand}.json"), None),
        );
        assert_eq!(policy.dir(), Some(Path::new("/from-env")), "env beats config for dir");
        assert_eq!(
            policy.name().as_str(),
            "{time}-{rand}.json",
            "args beat config for name"
        );
    }

    /// CLI arguments are the highest tier.
    #[test]
    fn args_beat_env_and_config() {
        let policy = resolve(
            options(Some("/from-config"), None, None),
            options(Some("/from-env"), None, None),
            options(Some("/from-args"), None, None),
        );
        assert_eq!(policy.dir(), Some(Path::new("/from-args")));
    }

    // ── `required` is config-file-only, at every tier ────────────────────────

    /// Unlocked, with nobody asking: warn and run. A developer who fat-fingered
    /// `--records-dir` must not have their build die for a policy nobody set.
    #[test]
    fn required_defaults_false_when_unlocked() {
        let policy = resolve(
            options(Some("/records"), None, None),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );
        assert!(!policy.required(), "the unlocked default is warn-and-run");
    }

    /// An unlocked config file may still opt in to fail-closed — the clamp
    /// table's own unlocked row calls `required` "config only", which is a
    /// positive claim that it is settable there.
    #[test]
    fn required_is_settable_from_an_unlocked_config_file() {
        let policy = resolve(
            options(Some("/records"), None, Some(true)),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );
        assert!(policy.required(), "a project-tier `required = true` must take effect");
    }

    /// SYSTEM-locked defaults to fail-closed: an operator who declared a
    /// collection pipeline gets exit 74 rather than a silently missing record.
    #[test]
    fn required_defaults_true_when_system_locked() {
        let policy = resolve(
            locked(Some("/records"), None, None),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );
        assert!(policy.required(), "the locked default is fail-closed");
    }

    /// An operator who disagrees writes `required = false` in the same file.
    #[test]
    fn system_locked_operator_can_opt_out_of_fail_closed() {
        let policy = resolve(
            locked(Some("/records"), None, Some(false)),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );
        assert!(!policy.required(), "an explicit system-scope opt-out must be honoured");
    }

    /// The environment cannot set the posture. There is no `OCX_RECORDS_REQUIRED`
    /// — and this function must ignore the field rather than trust that the
    /// caller left it `None`.
    #[test]
    fn required_is_refused_from_the_environment() {
        let policy = resolve(
            options(Some("/records"), None, None),
            options(None, None, Some(true)),
            RecordsOptions::default(),
        );
        assert!(!policy.required(), "the environment must not be able to fail closed");
    }

    /// The CLI cannot set the posture either. There is no `--records-required`.
    #[test]
    fn required_is_refused_from_the_command_line() {
        let policy = resolve(
            options(Some("/records"), None, None),
            RecordsOptions::default(),
            options(None, None, Some(true)),
        );
        assert!(!policy.required(), "a flag must not be able to fail closed");
    }

    /// Neither tier can relax the posture either — the refusal is symmetric, so
    /// a locked fail-closed policy survives an env or CLI `required = false`.
    #[test]
    fn required_is_refused_from_env_and_cli_when_locked() {
        let policy = resolve(
            locked(Some("/records"), None, None),
            options(None, None, Some(false)),
            options(None, None, Some(false)),
        );
        assert!(policy.required(), "a locked fail-closed posture must not be relaxed");
    }

    /// And it is refused unlocked too — the rule is "config file only at every
    /// tier", not "only when locked".
    #[test]
    fn required_is_refused_from_env_and_cli_when_unlocked() {
        let policy = resolve(
            options(Some("/records"), None, Some(true)),
            options(None, None, Some(false)),
            options(None, None, Some(false)),
        );
        assert!(policy.required(), "the config file's posture stands");
    }

    // ── a fail-closed posture with nothing to write to ───────────────────────

    /// `required = true` with no sink anywhere is a configuration error, not a
    /// silent opt-out. It is the plainest way an operator expresses "recording
    /// is mandatory", and resolving it to a policy that records nothing made
    /// every child on the fleet run unrecorded with exit 0 and no warning.
    ///
    /// Both tiers that can write it are exercised: an ordinary config file and
    /// a SYSTEM-locked one.
    #[test]
    fn an_explicit_required_true_without_a_sink_is_refused() {
        for config in [options(None, None, Some(true)), locked(None, None, Some(true))] {
            let locked_tier = config.system_locked;
            let error = resolve_records(config, RecordsOptions::default(), RecordsOptions::default())
                .expect_err("`required = true` with no sink must not resolve to a policy that records nothing");
            assert!(
                matches!(error, RecordsError::RequiredWithoutSink),
                "locked={locked_tier}, got: {error:?}"
            );
            assert_eq!(
                error.classify(),
                Some(ExitCode::ConfigError),
                "an operator fixes this by editing a config file, not by clearing an I/O condition"
            );
        }
    }

    /// The discriminator for the rule above, and the reason it keys on an
    /// **explicit** `required = true` rather than on the resolved posture: a
    /// SYSTEM-locked `[records]` block carrying no `dir` is plausibly an
    /// operator locking recording *off* for the host, and must keep working.
    #[test]
    fn a_system_lock_with_no_sink_and_no_explicit_posture_still_resolves_off() {
        let policy = resolve(
            locked(None, None, None),
            options(Some("/tmp/env"), None, None),
            options(Some("/tmp/args"), None, None),
        );
        assert!(
            !policy.is_recording(),
            "a locked block with no sink locks recording off, and the clamp still ignores env and CLI"
        );
        assert!(policy.required(), "the locked default posture is unchanged");
    }

    /// The other two rows of the same table: a sink present, or the posture
    /// explicitly relaxed, both resolve exactly as before.
    #[test]
    fn required_true_with_a_sink_and_required_false_without_one_are_unaffected() {
        let with_sink = resolve(
            options(Some("/records"), None, Some(true)),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );
        assert!(with_sink.is_recording());
        assert!(with_sink.required());

        let opted_out = resolve(
            options(None, None, Some(false)),
            RecordsOptions::default(),
            RecordsOptions::default(),
        );
        assert!(!opted_out.is_recording());
        assert!(!opted_out.required());
    }

    // ── the SYSTEM clamp, which env and CLI do not route through ─────────────

    /// A SYSTEM-locked block owns `dir`, `name` and `required` together: a
    /// wrapper script cannot accidentally redirect records and silently break
    /// the operator's collection pipeline.
    #[test]
    fn system_locked_ignores_env_and_cli() {
        let policy = resolve(
            locked(Some("/var/log/ocx/records"), Some("{time}-{pid}.json"), None),
            options(Some("/tmp/env"), Some("{rand}.json"), None),
            options(Some("/tmp/args"), Some("{time}.json"), None),
        );
        assert_eq!(
            policy.dir(),
            Some(Path::new("/var/log/ocx/records")),
            "a locked sink must not be redirected"
        );
        assert_eq!(
            policy.name().as_str(),
            "{time}-{pid}.json",
            "a locked filename pattern must not be changed"
        );
        assert!(policy.required(), "and the locked default posture still applies");
    }

    /// With no operator policy the sink is the caller's, filename pattern
    /// included — a SYSTEM file carrying no `[records]` locks nothing.
    #[test]
    fn an_unlocked_config_tier_locks_nothing() {
        let policy = resolve(
            RecordsOptions::default(),
            options(Some("/tmp/env"), None, None),
            options(None, Some("{rand}.json"), None),
        );
        assert_eq!(policy.dir(), Some(Path::new("/tmp/env")));
        assert_eq!(policy.name().as_str(), "{rand}.json");
    }

    // ── template validation happens here, before the child starts ────────────

    /// The template is parsed at resolve time, so a bad placeholder fails while
    /// the operator is still looking at their config.
    #[test]
    fn unknown_placeholder_fails_at_resolve_time() {
        let error = resolve_records(
            options(Some("/records"), Some("{time}-{jobid}.json"), None),
            RecordsOptions::default(),
            RecordsOptions::default(),
        )
        .expect_err("an unknown placeholder must be refused before the child starts");
        assert!(
            matches!(&error, RecordsError::TemplateUnknownPlaceholder { placeholder } if placeholder == "jobid"),
            "got: {error:?}"
        );
    }

    /// A constant template is refused the same way.
    #[test]
    fn constant_template_fails_at_resolve_time() {
        let error = resolve_records(
            options(Some("/records"), Some("record.json"), None),
            RecordsOptions::default(),
            RecordsOptions::default(),
        )
        .expect_err("a constant template must be refused");
        assert!(matches!(error, RecordsError::TemplateNotUnique), "got: {error:?}");
    }

    /// With no sink nothing renders a filename, so a broken template is not a
    /// failure. A machine-wide `OCX_RECORDS_NAME` must not exit 78 out of every
    /// `ocx exec` on the host — generated entrypoint launchers included, which
    /// take no flags and so cannot opt out.
    #[test]
    fn a_broken_template_without_a_sink_is_not_an_error() {
        for name in ["{jobid}.json", "record.json", "sub/{pid}.json"] {
            let policy = resolve_records(
                RecordsOptions::default(),
                options(None, Some(name), None),
                RecordsOptions::default(),
            )
            .unwrap_or_else(|error| panic!("{name:?} with no sink must resolve clean, got: {error:?}"));
            assert!(!policy.is_recording(), "{name:?} must not turn recording on");
            assert_eq!(
                policy.name().as_str(),
                DEFAULT_TEMPLATE,
                "the unused template falls back to the default"
            );
        }
    }

    /// The discriminator for the rule above: the same template *with* a sink is
    /// still refused, so the short circuit skips validation rather than dropping
    /// it.
    #[test]
    fn the_same_broken_template_with_a_sink_is_an_error() {
        for name in ["{jobid}.json", "record.json", "sub/{pid}.json"] {
            resolve_records(
                options(Some("/records"), None, None),
                options(None, Some(name), None),
                RecordsOptions::default(),
            )
            .expect_err(&format!("{name:?} must be refused once a sink is configured"));
        }
    }

    /// A locked block's own template is what gets validated — a caller cannot
    /// slip a valid template past a broken locked one, nor break a good one.
    #[test]
    fn the_locked_template_is_the_one_validated() {
        let error = resolve_records(
            locked(Some("/records"), Some("{jobid}.json"), None),
            RecordsOptions::default(),
            options(None, Some("{rand}.json"), None),
        )
        .expect_err("the locked template is the effective one");
        assert!(
            matches!(error, RecordsError::TemplateUnknownPlaceholder { .. }),
            "got: {error:?}"
        );
    }
}
