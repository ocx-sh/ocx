// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx shell state` report, which must never be eval-able in any arm, coloured or not.
//!
//! Every line starts with a label this module owns, or a carrier byte could start an
//! assignment; every dynamic string goes through `quoted`, or a literal LF splits a new line.

use std::path::{Path, PathBuf};

use ocx_config::exec_state::policy::{FieldClass, field_classes};
use ocx_config::exec_state::{
    AppliedValue, ExecState, Inherited, MAJOR, MAX_ENCODED, Narrowing, Owner, Pinned, PinnedTier, Policy, Prior,
    RecordedValue, Scope, ScopeId, Selection, SessionSource, Truncation, UnreadableReason, ValueKind, display_safe,
};
use ocx_console::{DataInterface, Theme, human_bytes, human_instant, human_time};
use ocx_project::activate::ActivateMode;
use ocx_project::consent::{Grant, Reason};
use ocx_shell::shell::coexistence::Observation;
use ocx_util::fs::path::AbsolutePath;
use ocx_util::size::ByteSize;
use ocx_util::time::Timestamp;
use serde::Serialize;

use crate::api::Printable;

/// Quote an untrusted string with `{:?}` for a human, never a shell; a value it would leave unchanged stays bare.
// The predicate must stay a subset of what `escape_debug` leaves alone, or a
// control, C1 or bidi byte (CWE-150) reaches the terminal unescaped.
fn quoted(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|character| !character.is_whitespace() && character.escape_debug().count() == 1)
    {
        return text.to_owned();
    }
    format!("{text:?}")
}

fn quoted_path(path: &Path) -> String {
    quoted(&path.display().to_string())
}

/// A state-derived string for stdout: the strip set removed first, then quoted for a human.
// The state is inherited input; `quoted` alone would print a bidi or U+0080-U+009F control it does not escape.
fn state_text(text: &str) -> String {
    quoted(&display_safe(text))
}

fn state_path(path: &Path) -> String {
    state_text(&path.display().to_string())
}

/// [`human_bytes`] over a `u64`; a size past `i64::MAX` renders as `unknown`.
fn human_size(bytes: u64) -> String {
    human_bytes(i64::try_from(bytes).unwrap_or(-1))
}

/// An instant as an age and the instant it stands for.
fn instant_description(at: Timestamp) -> String {
    let at = chrono::DateTime::<chrono::Utc>::from(at);
    format!("{}, {}", human_time(at), human_instant(at))
}

/// One member of the fingerprint watch set, as it stands on disk right now.
///
/// Absent members are recorded too.
// Keep absent members: a tier file appearing is exactly the change the watch set must notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct WatchMember {
    /// The watched path.
    pub path: PathBuf,
    /// Whether it exists right now.
    pub present: bool,
    /// Size in bytes; absent when the member is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<ByteSize>,
    /// When it was last modified, at the whole-second granularity the per-prompt
    /// fast path compares; absent when the member is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<Timestamp>,
}

/// Whether the project scope still holds the prior for one constant it owns.
///
/// Nothing can reconstruct a prior, and the `unset __OCX_ENV_STATE` repair
/// gesture destroys them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct PriorStatus {
    /// The constant's env key, as the carrier records it.
    pub key: String,
    /// Whether a prior is still recorded for it.
    pub intact: bool,
}

/// The per-prompt hook's enablement, as its ladder resolved it.
///
/// The answer comes from rung 3 (`OCX_NO_HOOK`), rung 4 (`[shell] hook`) or
/// rung 5 (auto): `ocx shell state` takes no `--hook` / `--no-hook` flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct HookStatus {
    /// The deciding rung, rendered the way a user spells it.
    pub rung: String,
    /// The config tier that **actually** set it; present only when rung 4 decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// The resolved answer; absent on rung 5, where "auto" is decided shell-side
    /// by the shim's interactivity probe, which a diagnostic cannot observe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// A reason row that is not an inertness reason, because it does not make the
/// shell inert on its own: it explains an answer the user would otherwise get
/// wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Note {
    /// The CWD walk skipped a symlinked `ocx.toml` candidate and promoted an
    /// ancestor.
    SymlinkedCandidateSkipped {
        /// The symlinked candidate that was skipped.
        candidate: PathBuf,
        /// The ancestor project activated in its place.
        ancestor: PathBuf,
    },
    /// Active via a `paths` grant, which is unconditional: source-set drift is
    /// **not** tracked for path grants.
    ActiveViaPathsGrant {
        /// The granting entry.
        entry: PathBuf,
    },
    /// A project file was reachable but could not be resolved (an unparseable
    /// `ocx.toml`, a canonicalization failure).
    ProjectUnresolved {
        /// The resolution failure, rendered for a human.
        detail: String,
    },
    /// A `paths` entry that would grant `canonical` if the compare folded ASCII case.
    ///
    /// Exact or subtree entry alike. Entries are compared as literal bytes, so
    /// the project stays inert.
    // Pairs with the literal-bytes compare in `consent_path_matches`.
    PathsNearMiss {
        /// The entry that nearly matched.
        entry: PathBuf,
        /// The canonical project directory it was compared against.
        canonical: PathBuf,
    },
    /// A `[shell.consent] paths` entry that can never match any project by construction.
    ///
    /// Classified from the entry's own bytes, independent of which project is in
    /// view.
    // Classified by `consent_entry_defect`; unlike a `namespaces` pattern, a broken `paths` entry still parses.
    PathsDefect {
        /// The malformed entry.
        entry: PathBuf,
        /// What is wrong with the entry, rendered for a human.
        defect: String,
    },
    /// The `ocx.toml` that answers for the reported scope exists but will not parse.
    ///
    /// Its `activate` / `pinned` are then absent and the ladder answered from
    /// `OCX_TOOLCHAIN_*` and then the floors, so a typo'd `activate = "nnone"`
    /// fails **open**, to `env`, the most-composing mode.
    // The only report of this: the per-prompt path tolerates the bad file and the hook discards stderr.
    ToolchainManifestUnparsed {
        /// The `ocx.toml` that would not parse — `$OCX_HOME/ocx.toml` for the
        /// global tier, the project's own otherwise.
        manifest: PathBuf,
        /// The parse failure, rendered for a human.
        detail: String,
    },
    /// `__OCX_ENV_STATE` is set and cannot be read, so its payload is ignored.
    EnvStateUnreadable {
        /// The major this ocx reads.
        major: u32,
        /// The major the header carries, when the header parsed.
        #[serde(skip_serializing_if = "Option::is_none")]
        found_major: Option<u32>,
        /// Why the value was refused, as a stable slug.
        reason: String,
        /// The record or payload field that failed validation; present for `invalid` only.
        #[serde(skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    /// The state is unreadable and the shell's hook skips every prompt until the value changes.
    EnvStateStoodDown,
    /// The state dropped data to fit the size cap.
    EnvStateTruncated {
        /// What was dropped.
        truncated: TruncationReport,
    },
    /// The inherited state carries settings that relax a protection.
    RelaxingPolicy {
        /// The relaxing settings that are set, sorted.
        names: Vec<String>,
    },
    /// The pinned `ocx` binary named by `OCX_BINARY_PIN` does not exist.
    BinaryPinMissing {
        /// The pinned path.
        path: PathBuf,
    },
    /// The inherited state names a `config_path` whose file no longer exists, so this report was built without it.
    ///
    /// An ordinary command treats a missing config path as an error; the report is a finding instead.
    ReceivedConfigMissing {
        /// The config path the parent ocx recorded.
        path: PathBuf,
    },
}

/// How much of the report the plain rendering carries; `--format json` always gets all of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Home, project, verdict and, when inert, the reason and its fix.
    Answer,
    /// The answer plus its evidence: execution state, binary pin, watch set, state key and stamp, hook ladder.
    Diagnostics,
}

/// What `ocx shell state` reports: all derived, none of it mutating.
///
/// Read-only: it never writes a stamp, repairs the state or emits a plan.
/// Repair is `unset __OCX_ENV_STATE` or a new shell.
// Never a stamp writer: a stamp written here would consent to the project it diagnoses.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ShellStateReport {
    /// `$OCX_HOME`.
    ///
    /// A missing home is an ordinary state on a fresh install and exits 0; only
    /// a home that cannot be *read* exits 74.
    pub ocx_home: AbsolutePath,
    /// Whether `$OCX_HOME` exists on disk.
    pub ocx_home_present: bool,

    /// Whether `ocx self setup` has ever wired this machine's shell.
    ///
    /// Probed as the witness shim under `ocx_home`.
    // "The next prompt applies it" holds only once the rc/profile fence sources the shim; a bare binary has neither.
    // Not a `Reason` arm: "setup has not run" is not a consent state.
    pub shell_integration_installed: bool,

    /// The **resolved toolchain home**, a machine-readable contract field.
    ///
    /// The supported way for an IDE, a devcontainer feature or a CI step to find
    /// a project's tree when `config.toml` relocates it with `toolchain_dir`:
    /// `<project>/.ocx/toolchain` or `<toolchain_dir>/<project-key>/toolchain`
    /// for the project in effect, and `$OCX_HOME/toolchain` when no project
    /// resolves. **Always present, never `null`**: an invalid `toolchain_dir`
    /// fails config load with exit 78 instead.
    // Non-optional because once the configuration parses the home is always spellable, rendered or not.
    #[schemars(with = "AbsolutePath")]
    pub toolchain_home: PathBuf,

    /// The **PATH-facing trampoline directory** for the same tier, which a consumer outside ocx puts on `PATH`.
    ///
    /// `toolchain_home` answers *where the tree is*; this answers *what to
    /// export*, and the two are not one join apart, so never build this from
    /// `toolchain_home`. Always present. It
    /// names a directory that need not exist yet: a home that has never been
    /// pulled reports the path it *would* hold.
    pub toolchain_bin: PathBuf,

    /// The **effective** `activate` mode, past the whole ladder.
    ///
    /// The flag tier (absent here), then `ocx.toml`, then
    /// `OCX_TOOLCHAIN_ACTIVATE`, then the floor. The resolved answer, never a tier.
    pub activate: ocx_project::activate::ActivateMode,

    /// The **effective** `pinned` value, resolved through the same ladder.
    ///
    /// `true` means a composing emitter yields digest paths and consults no
    /// `links/<group>/<entry>` link; `false` means it follows the rendered links, so
    /// an `ocx update` takes effect with no re-render.
    pub pinned: bool,

    /// Why the project's `ocx.lock` refuses composition, when it does, as text.
    ///
    /// Set when consent activates over a lock composition refuses, such as a
    /// `paths` grant over an unreadable or stale lock. Every prompt then exits 65
    /// and the scope never reaches the state.
    // The only field telling this state apart from a genuine first prompt.
    // `LockCurrency`'s `Display`; not a `Reason` arm, since the consent predicate did not refuse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_refusal: Option<String>,

    /// Whether the `__OCX_ENV_STATE` carrier is set at all.
    ///
    /// This is what separates two inert states: an absent
    /// carrier is the first prompt of a shell (nothing applied, nothing to
    /// repair); a carrier that is present and undecodable is a corrupt one (a
    /// scope was applied and its record is gone).
    pub carrier_present: bool,

    /// The carrier's encoded length in bytes, measured against the carrier cap.
    pub carrier_bytes: ByteSize,

    /// The decoded execution state, **rendered as fields, never as base64**.
    ///
    /// Reported in every state: an absent, unreadable or truncated value is a finding, not an error.
    // Published through its own report shape: the carrier's spelling is a wire format live shells decode.
    pub env_state: EnvStateReport,

    /// The `ocx` binary generated launchers re-enter, and whether it is still there.
    pub binary_pin: BinaryPinReport,

    /// Whether the state's recorded `fp` still matches the watch set on disk now.
    ///
    /// Absent when there is no recorded fingerprint to compare against, **and**
    /// when no fold is available to compare with.
    // Compared with the reconciler's own `reconcile::fingerprint`; a second fold here reports every fresh shell stale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint_current: Option<bool>,

    /// The watch set, with each member's presence, size and modification time.
    pub watch_set: Vec<WatchMember>,

    /// The project the CWD walk resolved, canonicalized; absent when none resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<AbsolutePath>")]
    pub project_dir: Option<PathBuf>,
    /// The 16-hex state key of `project_dir`; absent with it.
    // `ReferenceManager::name_for_path` of `project_dir`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
    /// Whether a usable consent stamp exists for that key; an unusable stamp
    /// counts as absent.
    pub project_stamped: bool,

    /// **Which clause activated this project**; absent when it is inert.
    ///
    /// A consent stamp outranks the `[shell.consent]` table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<Grant>,

    /// When the consent stamp was written, when there is one.
    ///
    /// The stamp's own recorded instant, not its file time. Absent whenever
    /// `project_stamped` is `false`.
    // Taken from the same read as `project_stamped`, or the two can disagree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stamp_written_at: Option<Timestamp>,

    /// Prior intactness for each constant the project scope owns.
    pub priors: Vec<PriorStatus>,

    /// The hook's enablement and the rung that decided it.
    pub hook: HookStatus,

    /// Every coexisting tool observed live in this shell.
    ///
    /// One entry per observed tool; the `yielded_to` inert reason names only the
    /// first.
    pub yielded_to: Vec<Observation>,

    /// Why the shell is not active, when it is not — **the command's reason to
    /// exist**. Absent when the project is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inert_reason: Option<Reason>,

    /// Reason rows that explain an answer without being an inertness verdict of
    /// their own.
    pub notes: Vec<Note>,
}

impl ShellStateReport {
    /// Prior intactness for each constant the project scope owns.
    #[must_use]
    pub fn priors_for(env_state: &EnvStateReport) -> Vec<PriorStatus> {
        env_state
            .applied
            .iter()
            .filter(|record| {
                record.owner == OwnerReport::Session
                    && record.scope == Some(ScopeIdReport::Project)
                    && record.kind == ValueKindReport::Constant
            })
            .map(|record| PriorStatus {
                key: record.key.clone(),
                intact: record.prior.is_some(),
            })
            .collect()
    }

    /// The lines [`Printable::print_plain`] prints at `detail`, kept apart from printing so
    /// the never-eval-able test checks the exact bytes a user sees, coloured included.
    #[must_use]
    pub fn lines(&self, theme: &Theme, detail: Detail) -> Vec<String> {
        let mut out = Vec::new();
        self.summary_lines(theme, detail, &mut out);
        self.activation_lines(theme, &mut out);
        if detail == Detail::Diagnostics {
            out.push(String::new());
            self.env_state_lines(theme, &mut out);
            self.binary_pin_lines(theme, &mut out);
            self.fingerprint_lines(theme, &mut out);
            self.hook_lines(theme, &mut out);
        }
        while out.last().is_some_and(String::is_empty) {
            out.pop();
        }
        out
    }

    /// Where OCX lives and which project is in effect.
    fn summary_lines(&self, theme: &Theme, detail: Detail, out: &mut Vec<String>) {
        let home = if self.ocx_home_present {
            quoted_path(self.ocx_home.as_path())
        } else {
            format!("{} {}", quoted_path(self.ocx_home.as_path()), theme.alert("(absent)"))
        };
        out.push(format!("{} {home}", theme.label("ocx home:")));

        match self.project_dir.as_ref() {
            Some(dir) => {
                out.push(format!("{} {}", theme.label("project:"), quoted_path(dir)));
                if detail == Detail::Diagnostics {
                    out.push(theme.field(
                        "  ",
                        "key",
                        self.project_key.as_deref().map_or_else(|| "none".to_owned(), quoted),
                    ));
                    out.push(theme.field("  ", "consent stamp", self.stamp_description()));
                }
            }
            None => out.push(format!(
                "{} none reachable from this directory",
                theme.label("project:")
            )),
        }

        // Ungated: under `toolchain_dir` relocation nothing else names this directory.
        out.push(format!(
            "{} {}",
            theme.label("toolchain:"),
            quoted_path(&self.toolchain_home)
        ));
        out.push(theme.field("  ", "bin", quoted_path(&self.toolchain_bin)));
        out.push(theme.field("  ", "activate", self.activate.to_string()));
        out.push(theme.field("  ", "pinned", if self.pinned { "yes" } else { "no" }));

        out.push(format!(
            "{} {}",
            theme.label("env state:"),
            match self.env_state.status {
                EnvStateStatus::Unreadable => theme.alert(self.env_state.status.as_str()),
                status => status.as_str().to_owned(),
            }
        ));

        out.push(String::new());
    }

    /// The decoded execution state, as fields - never as the base64 the carrier holds.
    fn env_state_lines(&self, theme: &Theme, out: &mut Vec<String>) {
        let state = &self.env_state;
        out.push(theme.label("execution state:"));
        out.push(theme.field("  ", "carrier", quoted(ocx_env::__OCX_ENV_STATE.name)));
        out.push(theme.field("  ", "present", if self.carrier_present { "yes" } else { "no" }));
        out.push(theme.field(
            "  ",
            "bytes",
            format!(
                "{} {}",
                self.carrier_bytes.get(),
                theme.note(format!("of {MAX_ENCODED}"))
            ),
        ));
        out.push(theme.field(
            "  ",
            "status",
            format!(
                "{} {}",
                state.status.as_str(),
                theme.note(match state.status {
                    EnvStateStatus::Absent => "(the carrier is unset)",
                    EnvStateStatus::Readable => "(decoded)",
                    EnvStateStatus::Unreadable => "(the payload is ignored; the header is still honoured)",
                })
            ),
        ));
        if state.status == EnvStateStatus::Absent {
            out.push(String::new());
            return;
        }
        out.push(theme.field("  ", "major", state.major.to_string()));
        if let Some(minor) = state.minor {
            out.push(theme.field("  ", "minor", minor.to_string()));
        }
        if let Some(found) = state.found_major {
            out.push(theme.field("  ", "found major", found.to_string()));
        }
        let narrowing = state.narrowing.as_deref().unwrap_or_default();
        out.push(theme.field(
            "  ",
            "narrowing",
            if narrowing.is_empty() {
                "none".to_owned()
            } else {
                narrowing
                    .iter()
                    .map(|name| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ));
        if let Some(home) = state.home.as_deref() {
            out.push(theme.field("  ", "home", state_path(home)));
        }
        if let Some(truncated) = state.truncated {
            out.push(theme.field("  ", "truncated", theme.alert(truncated.as_str())));
        }
        if state.status == EnvStateStatus::Unreadable {
            out.push(String::new());
            return;
        }
        out.push(theme.field(
            "  ",
            "session claimed",
            if state.session_claimed { "yes" } else { "no" },
        ));
        if let Some(policy) = state.policy.as_ref() {
            out.push(theme.field("  ", "policy", ""));
            let set = policy.set_values();
            if set.is_empty() {
                out.push("    - none set".to_owned());
            }
            for (name, value) in set {
                out.push(format!("    - {name} {}", state_text(&value)));
            }
        }
        if !state.relaxing_policy.is_empty() {
            out.push(theme.field("  ", "relaxing policy", theme.alert(state.relaxing_policy.join(", "))));
        }
        out.push(theme.field("  ", "inherited scopes", ""));
        if state.inherited_scopes.is_empty() {
            out.push("    - none".to_owned());
        }
        for scope in &state.inherited_scopes {
            match scope {
                ScopeReport::Global => out.push("    - global".to_owned()),
                ScopeReport::Project { root } => out.push(format!("    - project {}", state_path(root))),
            }
        }
        out.push(theme.field("  ", "applied", ""));
        if state.applied.is_empty() {
            out.push("    - nothing".to_owned());
        }
        for record in &state.applied {
            let separator = record.separator.as_deref().map_or_else(String::new, |sep| {
                format!(" {}", theme.note(format!("sep {}", state_text(sep))))
            });
            let value = state_text(&record.value);
            let pre_existing = if record.pre_existing { " (pre-existing)" } else { "" };
            out.push(format!(
                "    - {} {} {value}{separator}{pre_existing} {}",
                state_text(&record.key),
                theme.note(record.kind.as_str()),
                theme.note(format!("({})", record.owner_description())),
            ));
        }
        if let Some(replay) = state.replay.as_ref() {
            out.push(theme.field(
                "  ",
                "replay",
                format!(
                    "{} env, {} launch identities",
                    replay.env_count, replay.launch_identity_count
                ),
            ));
        }
        if let Some(selection) = state.selection.as_ref() {
            out.push(theme.field("  ", "selection", ""));
            selection.push_lines(theme, out);
        }
        out.push(theme.field("  ", "priors", ""));
        if self.priors.is_empty() {
            out.push("    - no constants owned".to_owned());
        }
        for prior in &self.priors {
            let recorded = state
                .applied
                .iter()
                .find(|record| record.key == prior.key && record.prior.is_some())
                .and_then(|record| record.prior.as_ref())
                .map_or_else(|| "lost".to_owned(), PriorReport::description);
            out.push(format!(
                "    - {} {} {}",
                state_text(&prior.key),
                if prior.intact {
                    theme.ok("intact")
                } else {
                    theme.alert("MISSING")
                },
                theme.note(format!("({recorded})"))
            ));
        }
        out.push(String::new());
    }

    /// The pinned `ocx` binary generated launchers re-enter.
    fn binary_pin_lines(&self, theme: &Theme, out: &mut Vec<String>) {
        let pin = &self.binary_pin;
        out.push(theme.label("binary pin:"));
        out.push(theme.field(
            "  ",
            "path",
            pin.path.as_deref().map_or_else(|| "none".to_owned(), state_path),
        ));
        out.push(theme.field("  ", "exists", if pin.exists { "yes" } else { "no" }));
        out.push(theme.field(
            "  ",
            "matches running binary",
            if pin.matches_running { "yes" } else { "no" },
        ));
        out.push(String::new());
    }

    fn fingerprint_lines(&self, theme: &Theme, out: &mut Vec<String>) {
        out.push(theme.label("fingerprint:"));
        out.push(theme.field(
            "  ",
            "matches watch set",
            match self.fingerprint_current {
                Some(true) => theme.ok("yes"),
                Some(false) => theme.alert("no"),
                None => "not compared".to_owned(),
            },
        ));
        out.push(theme.field("  ", "watch set", ""));
        if self.watch_set.is_empty() {
            out.push("    - empty".to_owned());
        }
        for member in &self.watch_set {
            let detail = match (member.size.map(ByteSize::get), member.modified_at) {
                (Some(size), Some(modified_at)) => {
                    format!(
                        "present, {}, modified {}",
                        human_size(size),
                        instant_description(modified_at)
                    )
                }
                (Some(size), None) => format!("present, {}", human_size(size)),
                _ if member.present => "present".to_owned(),
                _ => "absent".to_owned(),
            };
            out.push(format!(
                "    - {} {}",
                quoted_path(&member.path),
                theme.note(format!("({detail})"))
            ));
        }
        out.push(String::new());
    }

    fn hook_lines(&self, theme: &Theme, out: &mut Vec<String>) {
        out.push(theme.label("hook:"));
        out.push(theme.field(
            "  ",
            "enabled",
            match self.hook.enabled {
                Some(true) => theme.ok("yes"),
                Some(false) => theme.alert("no"),
                None => format!("auto {}", theme.note("(decided per shell at startup)")),
            },
        ));
        out.push(theme.field("  ", "deciding rung", quoted(&self.hook.rung)));
        if let Some(tier) = self.hook.tier.as_deref() {
            out.push(theme.field("  ", "deciding tier", quoted(tier)));
        }
        out.push(String::new());
    }

    /// Whether a consented project scope awaits its first apply; false on an unreadable, absent or truncated state.
    fn project_scope_pending(&self) -> bool {
        let state = &self.env_state;
        self.project_dir.is_some()
            && state.status == EnvStateStatus::Readable
            && state.truncated.is_none()
            && !state
                .applied
                .iter()
                .any(|record| record.owner == OwnerReport::Session && record.scope == Some(ScopeIdReport::Project))
    }

    /// Whether a reachable project file could not be resolved; no [`Reason`] covers
    /// that, so without this the report prints `active: yes` over a broken `ocx.toml`.
    fn project_unresolved(&self) -> bool {
        self.notes
            .iter()
            .any(|note| matches!(note, Note::ProjectUnresolved { .. }))
    }

    /// The verdict, its reason and fix, and the notes.
    fn activation_lines(&self, theme: &Theme, out: &mut Vec<String>) {
        match self.inert_reason.as_ref() {
            Some(reason) => {
                out.push(format!("{} {}", theme.label("active:"), theme.alert("no")));
                self.reason_lines(theme, reason, out);
            }
            // The note below carries detail and fix; this arm only stops the verdict saying `yes`.
            None if self.project_unresolved() => {
                out.push(format!("{} {}", theme.label("active:"), theme.alert("no")));
            }
            // Only the resolved mode tells `activate = none` from a healthy env project.
            // After the reason arm: a consent-refused project never reaches its own activate key.
            None if self.project_dir.is_some() && self.activate == ActivateMode::None => {
                out.push(format!("{} {}", theme.label("active:"), theme.alert("no")));
                out.push(format!("{} {}", theme.alert("reason:"), "activate = none"));
                out.push("  this project composes no environment and puts nothing on PATH".to_owned());
                push_fix(
                    theme,
                    out,
                    "set `activate` to `env` or `bin` in this project's ocx.toml",
                );
            }
            None if self.project_scope_pending() => match self.lock_refusal.as_deref() {
                // Composition refuses, so the scope never reaches the state and "not yet" would never come true.
                Some(refusal) => {
                    out.push(format!("{} {}", theme.label("active:"), theme.alert("no")));
                    out.push(format!("{} {refusal}", theme.alert("reason:")));
                    out.push("  consent allows this project; its lock does not compose".to_owned());
                    push_fix(
                        theme,
                        out,
                        "run `ocx lock` here - every prompt until then applies nothing",
                    );
                }
                None => {
                    out.push(format!("{} not yet", theme.label("active:")));
                    out.push("  the project scope is consented and not yet applied".to_owned());
                    out.push("  the next prompt applies it; the carrier is intact".to_owned());
                }
            },
            None => out.push(format!("{} {}", theme.label("active:"), theme.ok("yes"))),
        }
        if let Some(grant) = self.grant {
            out.push(format!(
                "  {} {}",
                theme.label("granted by:"),
                self.grant_description(grant)
            ));
        }
        for note in &self.notes {
            match note {
                Note::SymlinkedCandidateSkipped { candidate, ancestor } => {
                    out.push(format!(
                        "{} a symlinked ocx.toml candidate was skipped by the CWD walk",
                        theme.label("note:")
                    ));
                    out.push(theme.field("  ", "candidate", quoted_path(candidate)));
                    out.push(theme.field("  ", "activated instead", quoted_path(ancestor)));
                    out.push("  opt in with: --project, or the OCX_PROJECT variable".to_owned());
                }
                Note::ActiveViaPathsGrant { entry } => {
                    out.push(format!("{} active via a paths grant", theme.label("note:")));
                    out.push(theme.field("  ", "entry", quoted_path(entry)));
                    out.push("  source-set drift is not tracked for path grants".to_owned());
                }
                Note::ProjectUnresolved { detail } => {
                    out.push(format!(
                        "{} a project file is reachable but could not be resolved",
                        theme.label("note:")
                    ));
                    out.push(theme.field("  ", "detail", quoted(detail)));
                    push_fix(
                        theme,
                        out,
                        "repair the ocx.toml the detail names - nothing composes from this directory until it parses",
                    );
                }
                Note::PathsNearMiss { entry, canonical } => {
                    out.push(format!(
                        "{} a paths entry would grant if ASCII case were folded",
                        theme.label("note:")
                    ));
                    out.push(theme.field("  ", "entry", quoted_path(entry)));
                    out.push(theme.field("  ", "canonical dir", quoted_path(canonical)));
                    out.push("  entries are compared as literal bytes, so this does not grant".to_owned());
                }
                Note::PathsDefect { entry, defect } => {
                    out.push(format!(
                        "{} a paths entry can never match any project",
                        theme.label("note:")
                    ));
                    out.push(theme.field("  ", "entry", quoted_path(entry)));
                    // No `fix:` line: every_inert_arm_names_a_fix_and_no_other_arm_does reserves it for blocking arms.
                    out.push(theme.field("  ", "problem", defect));
                }
                Note::ToolchainManifestUnparsed { manifest, detail } => {
                    out.push(format!(
                        "{} the toolchain manifest does not parse, so its activate and pinned are ignored",
                        theme.label("note:")
                    ));
                    out.push(theme.field("  ", "manifest", quoted_path(manifest)));
                    // The loader's message embeds the path: untrusted, so quoted.
                    out.push(theme.field("  ", "detail", quoted(detail)));
                    // No `fix:` line: every_inert_arm_names_a_fix_and_no_other_arm_does reserves it for blocking arms.
                    out.push(
                        "  repair the manifest - until it parses, activate and pinned fall to their floors".to_owned(),
                    );
                }
                Note::EnvStateUnreadable {
                    major,
                    found_major,
                    reason,
                    field,
                } => {
                    out.push(format!(
                        "{} the execution state cannot be read, so its payload is ignored",
                        theme.label("note:")
                    ));
                    out.push(theme.field("  ", "reads major", major.to_string()));
                    if let Some(found) = found_major {
                        out.push(theme.field("  ", "found major", found.to_string()));
                    }
                    out.push(theme.field("  ", "reason", state_text(reason)));
                    if let Some(field) = field {
                        out.push(theme.field("  ", "field", state_text(field)));
                    }
                    out.push("  start a new shell, or unset __OCX_ENV_STATE; the header is still honoured".to_owned());
                }
                Note::EnvStateStoodDown => {
                    out.push(format!(
                        "{} the hook stood down on an unreadable state and skips every prompt",
                        theme.label("note:")
                    ));
                    out.push("  to re-arm it, start a new shell or unset __OCX_ENV_STATE".to_owned());
                }
                Note::EnvStateTruncated { truncated } => {
                    out.push(format!(
                        "{} the execution state dropped data to fit its size cap",
                        theme.label("note:")
                    ));
                    out.push(theme.field("  ", "dropped", truncated.as_str()));
                    out.push("  declare less environment, or run the command from a clean shell".to_owned());
                }
                Note::RelaxingPolicy { names } => {
                    out.push(format!(
                        "{} the inherited state relaxes protections",
                        theme.label("note:")
                    ));
                    for name in names {
                        out.push(format!("    - {}", state_text(name)));
                    }
                    out.push("  run from a clean shell if the parent did not intend this".to_owned());
                }
                Note::BinaryPinMissing { path } => {
                    out.push(format!("{} the pinned ocx binary does not exist", theme.label("note:")));
                    out.push(theme.field("  ", "path", state_path(path)));
                    out.push("  launchers cannot re-enter ocx; run `ocx self setup` to restore it".to_owned());
                }
                Note::ReceivedConfigMissing { path } => {
                    out.push(format!(
                        "{} the config file the parent ocx passed down does not exist",
                        theme.label("note:")
                    ));
                    out.push(theme.field("  ", "path", state_path(path)));
                    out.push("  it was skipped; restore the file or run from a clean shell".to_owned());
                }
            }
        }
    }

    /// The consent stamp's presence and, when present, the instant it records.
    fn stamp_description(&self) -> String {
        match (self.project_stamped, self.stamp_written_at) {
            (true, Some(written)) => format!("present (written {})", instant_description(written)),
            (true, None) => "present".to_owned(),
            (false, _) => "absent".to_owned(),
        }
    }

    /// The granting clause as the user would look for it: a command for the stamp, a config key otherwise.
    fn grant_description(&self, grant: Grant) -> String {
        match grant {
            Grant::Stamp => format!(
                "a consent stamp, {} - revoke it with `ocx shell revoke`",
                self.stamp_description()
            ),
            Grant::Path => "[shell.consent] paths".to_owned(),
            Grant::Namespace => "[shell.consent] namespaces (packages only, not the project's own [env])".to_owned(),
        }
    }

    /// One arm per enumerated reason: its evidence, then its `fix:` line.
    fn reason_lines(&self, theme: &Theme, reason: &Reason, out: &mut Vec<String>) {
        let headline = |out: &mut Vec<String>, text: &str| out.push(format!("{} {text}", theme.alert("reason:")));
        match reason {
            Reason::NoStampNoGrant {
                derived_sources,
                paths_tested,
                namespaces_tested,
            } => {
                headline(out, "no consent stamp, and no matching grant");
                push_list(
                    theme,
                    out,
                    "derived sources",
                    derived_sources.iter().map(String::as_str),
                );
                push_list(
                    theme,
                    out,
                    "paths tested",
                    paths_tested.iter().map(|path| path.display().to_string()),
                );
                push_list(
                    theme,
                    out,
                    "namespaces tested",
                    namespaces_tested.iter().map(String::as_str),
                );
                push_fix(
                    theme,
                    out,
                    "run `ocx shell allow` here, or add this directory to [shell.consent] paths",
                );
            }
            Reason::SourceSetDrift { new_sources } => {
                headline(out, "the lock's source set is not a subset of the stamp");
                push_list(theme, out, "new sources", new_sources.iter().map(String::as_str));
                push_fix(theme, out, "run `ocx shell allow` here to re-stamp the new source set");
            }
            Reason::UncorroboratedNamespace {
                claimed_sources,
                verified_sources,
            } => {
                headline(
                    out,
                    "the namespace grant matches the lock's claim, not the store's record",
                );
                push_list(
                    theme,
                    out,
                    "claimed sources",
                    claimed_sources.iter().map(String::as_str),
                );
                match verified_sources {
                    Some(verified) => push_list(theme, out, "verified sources", verified.iter().map(String::as_str)),
                    None => out.push(theme.field("  ", "verified sources", "none recorded for this lock")),
                }
                push_fix(
                    theme,
                    out,
                    "run `ocx pull` here once to record where each tool came from, or `ocx shell allow` to stamp \
                     this directory outright",
                );
            }
            Reason::HookDisabled { rung, tier } => {
                headline(out, "the per-prompt hook is disabled");
                out.push(theme.field("  ", "deciding rung", quoted(rung)));
                out.push(theme.field(
                    "  ",
                    "deciding tier",
                    tier.as_deref().map_or_else(|| "not a config tier".to_owned(), quoted),
                ));
                push_fix(theme, out, "re-enable the hook at that rung, then start a new shell");
            }
            Reason::YieldedTo(first) => {
                headline(out, "yielded to another live per-prompt hook");
                // One line per observed tool: the sentinels fire independently and `YieldedTo` carries only the first.
                let observed = if self.yielded_to.is_empty() {
                    std::slice::from_ref(first)
                } else {
                    self.yielded_to.as_slice()
                };
                for observation in observed {
                    out.push(theme.field(
                        "  ",
                        "live",
                        format!(
                            "{} {}",
                            observation.tool.as_str(),
                            theme.note(format!("(signal {})", quoted(&observation.signal)))
                        ),
                    ));
                }
                push_fix(
                    theme,
                    out,
                    "none needed here - OCX yields for as long as that tool is live in this shell",
                );
            }
            Reason::StateOverCap { scope } => {
                headline(out, "the execution state exceeded its cap and a scope was abandoned");
                let scope = match scope {
                    ScopeId::Global => "global",
                    ScopeId::Project => "project",
                };
                out.push(theme.field("  ", "abandoned scope", scope));
                out.push("  read from the over_cap marker the carrier still holds".to_owned());
                push_fix(
                    theme,
                    out,
                    "declare less environment in this scope; its state payload does not fit the cap",
                );
            }
            Reason::StateUnreadable { first_prompt } => {
                if *first_prompt {
                    if self.shell_integration_installed {
                        headline(out, "nothing has been applied in this shell yet");
                        out.push("  the carrier is unset: this is the first prompt, not a fault".to_owned());
                        push_fix(theme, out, "none needed - the next prompt applies it");
                    } else {
                        // Same unset carrier, but in a never-wired shell no prompt will ever apply anything.
                        headline(out, "the shell integration has never been installed here");
                        out.push(format!(
                            "  no {} under {}: `ocx self setup` has not run",
                            quoted(ocx_setup::shims::WITNESS_SHIM),
                            quoted_path(self.ocx_home.as_path())
                        ));
                        push_fix(theme, out, "run `ocx self setup`, then start a new shell");
                    }
                } else {
                    headline(out, "the carrier is present but unreadable");
                    out.push("  a scope was applied and its record is gone".to_owned());
                    push_fix(
                        theme,
                        out,
                        "start a new shell, or unset the carrier (which loses the priors)",
                    );
                }
            }
            Reason::LockUnavailable => {
                headline(out, "ocx.lock is absent, unreadable or unparseable");
                out.push("  the source-set predicate has nothing to quantify over".to_owned());
                push_fix(theme, out, "run `ocx lock` here");
            }
        }
    }
}

fn push_list(theme: &Theme, out: &mut Vec<String>, label: &str, items: impl IntoIterator<Item = impl AsRef<str>>) {
    out.push(theme.field("  ", label, ""));
    let mut empty = true;
    for item in items {
        empty = false;
        out.push(format!("    - {}", quoted(item.as_ref())));
    }
    if empty {
        out.push("    - none".to_owned());
    }
}

/// The `fix:` line closing a reason; arms whose remedy is none still say so.
fn push_fix(theme: &Theme, out: &mut Vec<String>, fix: &str) {
    out.push(format!("{} {fix}", theme.label("fix:")));
}

impl Printable for ShellStateReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "ShellStateReport";

    /// The answer only ([`Detail::Answer`]).
    fn print_plain(&self, data: &DataInterface) {
        for line in self.lines(&data.theme(), Detail::Answer) {
            println!("{line}");
        }
    }
}

/// [`ShellStateReport`] with its diagnostics (`ocx shell state --verbose`).
///
/// Serializes as the inner report: `--verbose` changes the plain rendering, never the JSON.
pub struct VerboseShellState(pub ShellStateReport);

impl Printable for VerboseShellState {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "VerboseShellState";

    fn print_plain(&self, data: &DataInterface) {
        for line in self.0.lines(&data.theme(), Detail::Diagnostics) {
            println!("{line}");
        }
    }
}

impl Serialize for VerboseShellState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

// Must match the transparent `Serialize` above: the published schema is the inner report's.
impl schemars::JsonSchema for VerboseShellState {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VerboseShellState".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <ShellStateReport>::json_schema(generator)
    }
}

// ── The execution state as the report publishes it ──
// Mirrors of the wire types: the carrier spells them for live shells to decode, and the report
// spells them for programs, so neither may follow the other's attribute changes.

/// Whether `__OCX_ENV_STATE` is set, and whether it could be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnvStateStatus {
    /// The variable is unset or empty.
    Absent,
    /// The value decoded; its fields follow.
    Readable,
    /// The value is set and could not be read; only the header fields follow.
    Unreadable,
}

impl EnvStateStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Readable => "readable",
            Self::Unreadable => "unreadable",
        }
    }
}

/// A narrowing the header carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NarrowingName {
    /// No network.
    Offline,
    /// No index refresh.
    Frozen,
    /// No consent stamp is written.
    NoConsent,
    /// A credential helper child; implies offline.
    CredentialHelper,
}

impl NarrowingName {
    fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Frozen => "frozen",
            Self::NoConsent => "no_consent",
            Self::CredentialHelper => "credential_helper",
        }
    }
}

/// What the encoder dropped to fit the size cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TruncationReport {
    /// Records received from an outer frame were dropped.
    InheritedDropped,
    /// Every applied record was dropped.
    AllAppliedDropped,
    /// The whole payload was dropped; only the header remains.
    PayloadDropped,
}

impl TruncationReport {
    fn from_wire(truncation: Truncation) -> Option<Self> {
        match truncation {
            Truncation::None => None,
            Truncation::InheritedDropped => Some(Self::InheritedDropped),
            Truncation::AllAppliedDropped => Some(Self::AllAppliedDropped),
            Truncation::PayloadDropped => Some(Self::PayloadDropped),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::InheritedDropped => "inherited_dropped",
            Self::AllAppliedDropped => "all_applied_dropped",
            Self::PayloadDropped => "payload_dropped",
        }
    }
}

/// The inherited settings that are set; same names as the state's `policy`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct PolicyReport {
    /// `OCX_REMOTE`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<bool>,
    /// `OCX_NO_PROJECT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_project: Option<bool>,
    /// `OCX_NO_CONFIG_REFRESH`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_config_refresh: Option<bool>,
    /// `OCX_NO_UPDATE_CHECK`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_update_check: Option<bool>,
    /// `OCX_CONFIG`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<AbsolutePath>")]
    pub config_path: Option<PathBuf>,
    /// `OCX_NO_CONFIG`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_config: Option<bool>,
    /// `OCX_INDEX`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<AbsolutePath>")]
    pub index_dir: Option<PathBuf>,
    /// `OCX_TOOLCHAIN_DIR`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<AbsolutePath>")]
    pub toolchain_dir: Option<PathBuf>,
    /// `OCX_MIRRORS`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirrors: Option<String>,
    /// `OCX_PATCHES`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patches: Option<String>,
    /// `OCX_PATCH_SNAPSHOT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<AbsolutePath>")]
    pub patch_snapshot_path: Option<PathBuf>,
    /// `OCX_MANAGED_CONFIG`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_config: Option<String>,
    /// `OCX_NO_VERIFY`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_verify: Option<bool>,
    /// `OCX_RECORDS_DIR`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<AbsolutePath>")]
    pub records_dir: Option<PathBuf>,
    /// `OCX_RECORDS_NAME`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records_name: Option<String>,
    /// `OCX_ALLOW_YANKED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_yanked: Option<bool>,
    /// `OCX_DEFAULT_REGISTRY`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_registry: Option<String>,
    /// `OCX_INSECURE_REGISTRIES`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insecure_registries: Option<String>,
}

impl PolicyReport {
    fn from_wire(policy: &Policy) -> Self {
        // No `..`: a new `Policy` field must be reported here or this stops compiling.
        let Policy {
            remote,
            no_project,
            no_config_refresh,
            no_update_check,
            config_path,
            no_config,
            index_dir,
            toolchain_dir,
            mirrors,
            patches,
            patch_snapshot_path,
            managed_config,
            no_verify,
            records_dir,
            records_name,
            allow_yanked,
            default_registry,
            insecure_registries,
        } = policy.clone();
        Self {
            remote,
            no_project,
            no_config_refresh,
            no_update_check,
            config_path,
            no_config,
            index_dir,
            toolchain_dir,
            mirrors,
            patches,
            patch_snapshot_path,
            managed_config,
            no_verify,
            records_dir,
            records_name,
            allow_yanked,
            default_registry,
            insecure_registries,
        }
    }

    /// Each set field with its value spelled for a human, in declaration order.
    fn set_values(&self) -> Vec<(&'static str, String)> {
        let flag = |value: Option<bool>| value.map(|value| value.to_string());
        let path = |value: &Option<PathBuf>| value.as_ref().map(|value| value.display().to_string());
        [
            ("remote", flag(self.remote)),
            ("no_project", flag(self.no_project)),
            ("no_config_refresh", flag(self.no_config_refresh)),
            ("no_update_check", flag(self.no_update_check)),
            ("config_path", path(&self.config_path)),
            ("no_config", flag(self.no_config)),
            ("index_dir", path(&self.index_dir)),
            ("toolchain_dir", path(&self.toolchain_dir)),
            ("mirrors", self.mirrors.clone()),
            ("patches", self.patches.clone()),
            ("patch_snapshot_path", path(&self.patch_snapshot_path)),
            ("managed_config", self.managed_config.clone()),
            ("no_verify", flag(self.no_verify)),
            ("records_dir", path(&self.records_dir)),
            ("records_name", self.records_name.clone()),
            ("allow_yanked", flag(self.allow_yanked)),
            ("default_registry", self.default_registry.clone()),
            ("insecure_registries", self.insecure_registries.clone()),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect()
    }
}

/// A scope an outer frame composed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScopeReport {
    /// The global toolchain tier.
    Global,
    /// A project, by the root it was composed from.
    Project {
        /// The project directory as the state recorded it.
        root: PathBuf,
    },
}

/// Who applied a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OwnerReport {
    /// A shell instance of this session.
    Session,
    /// An outer frame; this process only inherits it.
    Inherited,
}

/// The scope a session record belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeIdReport {
    /// The global toolchain tier.
    Global,
    /// The project in effect.
    Project,
}

/// Where a session record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceReport {
    /// From `ocx.toml` groups or the global tier; refreshed each prompt.
    Composed,
    /// From `ocx shell --with` or `--env`; removed, never refreshed.
    Invocation,
}

/// How an applied value combines with the variable's existing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueKindReport {
    /// A path-list element, prepended.
    Path,
    /// A list element joined by a separator.
    List,
    /// The whole value.
    Constant,
}

impl ValueKindReport {
    fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::List => "list",
            Self::Constant => "constant",
        }
    }
}

/// What a variable held before ocx set it.
///
/// A set-but-empty variable is `value` with an empty string, never `unset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PriorReport {
    /// The variable did not exist; reverting removes it.
    Unset,
    /// The variable held a value; reverting restores it.
    Value {
        /// The exact string the variable held.
        value: String,
    },
    /// The prior was not UTF-8; it is never restored.
    Withheld,
}

impl PriorReport {
    fn description(&self) -> String {
        match self {
            Self::Unset => "was unset".to_owned(),
            Self::Value { value } => format!("was {}", state_text(value)),
            Self::Withheld => "withheld".to_owned(),
        }
    }
}

/// One value ocx applied to an environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct AppliedReport {
    /// Environment-variable name.
    pub key: String,
    /// The exact string ocx wrote.
    pub value: String,
    /// How the value combines.
    pub kind: ValueKindReport,
    /// The effective list separator; present only for a `list` record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
    /// The element was already present and was only moved; reverting keeps it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pre_existing: bool,
    /// Who applied it.
    pub owner: OwnerReport,
    /// The scope of a session record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeIdReport>,
    /// The origin of a session record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceReport>,
    /// What a constant held before; absent for a path or list record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior: Option<PriorReport>,
}

impl AppliedReport {
    fn from_wire(record: &AppliedValue) -> Self {
        let (owner, scope, source) = match &record.owner {
            Owner::Session { scope, source } => (
                OwnerReport::Session,
                Some(match scope {
                    ScopeId::Global => ScopeIdReport::Global,
                    ScopeId::Project => ScopeIdReport::Project,
                }),
                Some(match source {
                    SessionSource::Composed => SourceReport::Composed,
                    SessionSource::Invocation => SourceReport::Invocation,
                }),
            ),
            Owner::Inherited => (OwnerReport::Inherited, None, None),
        };
        let RecordedValue::Literal(value) = &record.value;
        Self {
            key: record.key.clone(),
            value: value.clone(),
            kind: match record.kind {
                ValueKind::Path => ValueKindReport::Path,
                ValueKind::List => ValueKindReport::List,
                ValueKind::Constant => ValueKindReport::Constant,
            },
            separator: record.separator.clone(),
            pre_existing: record.pre_existing,
            owner,
            scope,
            source,
            prior: record.prior.as_ref().map(|prior| match prior {
                Prior::Unset => PriorReport::Unset,
                Prior::Value(value) => PriorReport::Value { value: value.clone() },
                Prior::Withheld => PriorReport::Withheld,
            }),
        }
    }

    fn owner_description(&self) -> String {
        match (self.owner, self.scope, self.source) {
            (OwnerReport::Session, Some(scope), Some(source)) => {
                format!(
                    "session {} {}",
                    match scope {
                        ScopeIdReport::Global => "global",
                        ScopeIdReport::Project => "project",
                    },
                    match source {
                        SourceReport::Composed => "composed",
                        SourceReport::Invocation => "invocation",
                    }
                )
            }
            (OwnerReport::Session, ..) => "session".to_owned(),
            (OwnerReport::Inherited, ..) => "inherited".to_owned(),
        }
    }
}

/// How many replay records the state carries; the records themselves are not published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ReplayCounts {
    /// Environment values launchers re-apply.
    pub env_count: usize,
    /// Package identities launchers match patch rules by.
    pub launch_identity_count: usize,
}

/// What the invocation that wrote the state selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SelectionReport {
    /// Follow the working directory; nothing is pinned.
    FollowCwd,
    /// Pinned to a project's groups.
    PinnedProject {
        /// The project directory as the state recorded it.
        #[schemars(with = "AbsolutePath")]
        project_dir: PathBuf,
        /// The pinned groups.
        groups: Vec<String>,
        /// `--with` identifiers as typed.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        with_names: Vec<String>,
        /// `--env` keys; never their values.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        env_keys: Vec<String>,
    },
    /// Pinned to the global toolchain's groups.
    PinnedGlobal {
        /// The pinned groups.
        groups: Vec<String>,
    },
}

impl SelectionReport {
    fn from_wire(selection: &Selection) -> Self {
        match selection {
            Selection::FollowCwd => Self::FollowCwd,
            Selection::Pinned(Pinned {
                tier,
                groups,
                with_names,
                env_keys,
                ..
            }) => match tier {
                PinnedTier::Project { dir } => Self::PinnedProject {
                    project_dir: dir.clone(),
                    groups: groups.clone(),
                    with_names: with_names.clone(),
                    env_keys: env_keys.clone(),
                },
                PinnedTier::Global => Self::PinnedGlobal { groups: groups.clone() },
            },
        }
    }

    fn push_lines(&self, theme: &Theme, out: &mut Vec<String>) {
        let list = |names: &[String]| {
            if names.is_empty() {
                "none".to_owned()
            } else {
                names.iter().map(|name| state_text(name)).collect::<Vec<_>>().join(", ")
            }
        };
        match self {
            Self::FollowCwd => out.push("    - follows the working directory".to_owned()),
            Self::PinnedProject {
                project_dir,
                groups,
                with_names,
                env_keys,
            } => {
                out.push(theme.field("    ", "pinned project", state_path(project_dir)));
                out.push(theme.field("    ", "groups", list(groups)));
                out.push(theme.field("    ", "with", list(with_names)));
                out.push(theme.field("    ", "env keys", list(env_keys)));
            }
            Self::PinnedGlobal { groups } => {
                out.push("    - pinned to the global toolchain".to_owned());
                out.push(theme.field("    ", "groups", list(groups)));
            }
        }
    }
}

/// The decoded `__OCX_ENV_STATE`: what a parent handed this process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct EnvStateReport {
    /// Whether the value is unset, readable or unreadable.
    pub status: EnvStateStatus,
    /// The major version this ocx reads.
    pub major: u32,
    /// The minor version the state was written with; present when readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minor: Option<u32>,
    /// The major the header carries; present when unreadable and the header parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_major: Option<u32>,
    /// The narrowings the header carries; present when readable or unreadable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrowing: Option<Vec<NarrowingName>>,
    /// The `OCX_HOME` the header carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<AbsolutePath>")]
    pub home: Option<PathBuf>,
    /// What was dropped to fit the size cap; omitted when nothing was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<TruncationReport>,
    /// The inherited settings that are set; present when readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyReport>,
    /// The set settings that relax a protection, plus `home`, sorted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relaxing_policy: Vec<String>,
    /// Whether a shell instance claimed the session; the token itself is not published.
    pub session_claimed: bool,
    /// Scopes an outer frame composed, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inherited_scopes: Vec<ScopeReport>,
    /// What was applied, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<AppliedReport>,
    /// Counts of the records launchers replay; present when readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay: Option<ReplayCounts>,
    /// What the writing invocation selected; present when readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionReport>,
}

impl EnvStateReport {
    /// The report for what this process received, and the notes it owes.
    ///
    /// `hook_active` is whether the shell's per-prompt hook runs here: an unreadable state stands that
    /// hook down, and the stood-down copy lives only in the shell.
    #[must_use]
    pub fn from_inherited(inherited: &Inherited, hook_active: bool) -> (Self, Vec<Note>) {
        let mut notes = Vec::new();
        let report = match inherited {
            Inherited::Absent => Self::empty(EnvStateStatus::Absent),
            Inherited::Unreadable { header, reason } => {
                let (slug, field) = unreadable_slug(*reason);
                notes.push(Note::EnvStateUnreadable {
                    major: MAJOR,
                    found_major: header.major,
                    reason: slug.to_owned(),
                    field: field.map(str::to_owned),
                });
                if hook_active {
                    notes.push(Note::EnvStateStoodDown);
                }
                Self {
                    found_major: header.major,
                    narrowing: Some(narrowing_names(header.narrowing)),
                    home: header.home.clone(),
                    ..Self::empty(EnvStateStatus::Unreadable)
                }
            }
            Inherited::Readable(state) => {
                let report = Self::from_state(state);
                if let Some(truncated) = report.truncated {
                    notes.push(Note::EnvStateTruncated { truncated });
                }
                if !report.relaxing_policy.is_empty() {
                    notes.push(Note::RelaxingPolicy {
                        names: report.relaxing_policy.clone(),
                    });
                }
                report
            }
        };
        (report, notes)
    }

    fn empty(status: EnvStateStatus) -> Self {
        Self {
            status,
            major: MAJOR,
            minor: None,
            found_major: None,
            narrowing: None,
            home: None,
            truncated: None,
            policy: None,
            relaxing_policy: Vec::new(),
            session_claimed: false,
            inherited_scopes: Vec::new(),
            applied: Vec::new(),
            replay: None,
            selection: None,
        }
    }

    fn from_state(state: &ExecState) -> Self {
        let policy = PolicyReport::from_wire(&state.policy);
        let mut relaxing_policy: Vec<String> = field_classes()
            .into_iter()
            .filter(|(_, class)| *class == FieldClass::Relaxing)
            .map(|(name, _)| name)
            .filter(|name| match *name {
                "home" => state.home.is_some(),
                name => policy.set_values().iter().any(|(set, _)| *set == name),
            })
            .map(str::to_owned)
            .collect();
        relaxing_policy.sort();
        Self {
            minor: Some(state.minor),
            narrowing: Some(narrowing_names(state.narrowing)),
            home: state.home.clone(),
            truncated: TruncationReport::from_wire(state.truncated),
            policy: Some(policy),
            relaxing_policy,
            session_claimed: state.session.token.is_some(),
            inherited_scopes: state
                .inherited_scopes
                .iter()
                .map(|scope| match scope {
                    Scope::Global => ScopeReport::Global,
                    Scope::Project { root } => ScopeReport::Project { root: root.clone() },
                })
                .collect(),
            applied: state.applied.iter().map(AppliedReport::from_wire).collect(),
            replay: Some(ReplayCounts {
                env_count: state.replay.env.len(),
                launch_identity_count: state.replay.launch_identities.len(),
            }),
            selection: Some(SelectionReport::from_wire(&state.selection)),
            ..Self::empty(EnvStateStatus::Readable)
        }
    }
}

fn narrowing_names(narrowing: Narrowing) -> Vec<NarrowingName> {
    let Narrowing {
        offline,
        frozen,
        no_consent,
        credential_helper,
    } = narrowing;
    [
        (offline, NarrowingName::Offline),
        (frozen, NarrowingName::Frozen),
        (no_consent, NarrowingName::NoConsent),
        (credential_helper, NarrowingName::CredentialHelper),
    ]
    .into_iter()
    .filter_map(|(set, name)| set.then_some(name))
    .collect()
}

/// A stable slug per refusal, and the failing field for `invalid`.
fn unreadable_slug(reason: UnreadableReason) -> (&'static str, Option<&'static str>) {
    match reason {
        UnreadableReason::HeaderInvalid => ("header_invalid", None),
        UnreadableReason::OtherMajor => ("other_major", None),
        UnreadableReason::TooLong => ("too_long", None),
        UnreadableReason::NotUtf8 => ("not_utf8", None),
        UnreadableReason::Base64 => ("base64", None),
        UnreadableReason::Inflate => ("inflate", None),
        UnreadableReason::InflateLimit => ("inflate_limit", None),
        UnreadableReason::TrailingBytes => ("trailing_bytes", None),
        UnreadableReason::Json => ("json", None),
        UnreadableReason::RecordLimit => ("record_limit", None),
        UnreadableReason::TierLimit => ("tier_limit", None),
        UnreadableReason::ScopeLimit => ("scope_limit", None),
        UnreadableReason::Invalid { field } => ("invalid", Some(field)),
    }
}

/// The `ocx` binary generated launchers re-enter (`OCX_BINARY_PIN`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct BinaryPinReport {
    /// The pinned path; absent when no pin is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Whether the pinned path exists.
    pub exists: bool,
    /// Whether the pinned path and the running binary canonicalise to one file.
    pub matches_running: bool,
}

impl BinaryPinReport {
    /// Stats the pin and compares it with the running binary. Blocking: call it off the runtime.
    #[must_use]
    pub fn probe(path: Option<&Path>, running: Option<&Path>) -> Self {
        // An unreadable pin reads as missing: the report must not fail on the thing it diagnoses.
        let exists = path.is_some_and(Path::exists);
        let matches_running = match (path, running) {
            (Some(path), Some(running)) => matches!(
                (dunce::canonicalize(path), dunce::canonicalize(running)),
                (Ok(pinned), Ok(running)) if pinned == running
            ),
            _ => false,
        };
        Self {
            path: path.map(Path::to_path_buf),
            exists,
            matches_running,
        }
    }

    /// [`Note::BinaryPinMissing`] when a pin is set and nothing is there.
    #[must_use]
    pub fn note(&self) -> Option<Note> {
        match (&self.path, self.exists) {
            (Some(path), false) => Some(Note::BinaryPinMissing { path: path.clone() }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ocx_config::exec_state::ScopeId as AbandonedScope;
    use ocx_config::exec_state::{AppliedValue, HeaderView, SessionToken};
    use ocx_console::Theme;
    use ocx_shell::shell::Shell;
    use ocx_shell::shell::coexistence::Tool;

    use super::*;

    /// Every `Shell` arm, with an exhaustive `match` so a new arm is a compile
    /// error here rather than a silently unchecked emitter.
    const ALL_SHELLS: [Shell; 10] = [
        Shell::Ash,
        Shell::Ksh,
        Shell::Dash,
        Shell::Bash,
        Shell::Elvish,
        Shell::Fish,
        Shell::Batch,
        Shell::PowerShell,
        Shell::Zsh,
        Shell::Nushell,
    ];

    #[test]
    fn all_shells_is_exhaustive() {
        for shell in ALL_SHELLS {
            // A new `Shell` variant makes this match non-exhaustive, which is
            // the point: `ALL_SHELLS` must not silently miss an emitter.
            match shell {
                Shell::Ash
                | Shell::Ksh
                | Shell::Dash
                | Shell::Bash
                | Shell::Elvish
                | Shell::Fish
                | Shell::Batch
                | Shell::PowerShell
                | Shell::Zsh
                | Shell::Nushell => {}
            }
        }
    }

    /// A key no report line can legitimately contain, so the prefix extraction
    /// below cannot accidentally split on report text.
    const PROBE_KEY: &str = "OCXSTATEPROBEKEY";

    /// The leading token every shipped emitter puts in front of the key —
    /// `export `, `set -gx `, `$Env:`, `$env.`, `set "` — **derived from the
    /// emitters, never hard-coded**, so a changed emitter cannot widen what
    /// counts as "not shell source".
    fn assignment_prefixes() -> Vec<String> {
        let mut prefixes = Vec::new();
        let mut contributed: Vec<Shell> = Vec::new();
        for shell in ALL_SHELLS {
            for emitted in [
                shell.export_constant(PROBE_KEY, "probe"),
                shell.export_path(PROBE_KEY, "probe"),
                shell.unset(PROBE_KEY),
            ]
            .into_iter()
            .flatten()
            {
                for line in emitted.lines() {
                    if let Some((prefix, _)) = line.split_once(PROBE_KEY)
                        && !prefix.is_empty()
                    {
                        let prefix = prefix.trim_start();
                        prefixes.push(prefix.to_owned());
                        // Also the bare keyword: fish spells the exported form
                        // `set -gx K v` and the local form `set K v`, so
                        // extracting only the full `set -gx ` prefix would let
                        // a line opening with `set ` through. The first token of
                        // a keyword-led prefix is the unit that matters.
                        if let Some((keyword, _)) = prefix.split_once(' ')
                            && !keyword.is_empty()
                        {
                            prefixes.push(format!("{keyword} "));
                        }
                        contributed.push(shell);
                    }
                }
            }
        }
        prefixes.sort();
        prefixes.dedup();
        assert!(
            prefixes.iter().any(|p| p == "export "),
            "the POSIX arms must contribute an `export ` prefix; got {prefixes:?}"
        );
        // Every emitter returns `Option<String>`, so an arm that starts
        // returning `None` would silently drop its prefix and this check would
        // quietly stop testing that shell while still reporting green — the
        // "quietly does less" failure mode. Assert per shell instead.
        for shell in ALL_SHELLS {
            assert!(
                contributed.contains(&shell),
                "{shell:?} contributed no assignment prefix: the never-eval-able check would \
                 silently stop covering that arm. Prefixes seen: {prefixes:?}"
            );
        }
        prefixes
    }

    /// Whether `line` looks like a bare assignment — eval-able without an
    /// `export`/`set` keyword in front of it.
    ///
    /// Covers the POSIX spelling `KEY=value` and PowerShell's `$X = value` /
    /// `$env:X = value`. The PowerShell forms were added after the positive
    /// control in `assert_not_interchangeable_with_activate` demonstrated that
    /// `Shell::PowerShell::export_path` emits a compound statement opening with
    /// `$__ocx_p='…'` that neither predicate recognised.
    fn is_bare_assignment(line: &str) -> bool {
        let Some((key, _)) = line.split_once('=') else {
            return false;
        };
        let key = key.trim_end();
        let key = key.strip_prefix('$').unwrap_or(key);
        let key = key
            .strip_prefix("env:")
            .or_else(|| key.strip_prefix("Env:"))
            .unwrap_or(key);
        !key.is_empty()
            && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// **The never-eval-able assertion.** Named, and run per reason arm.
    ///
    /// `arm` names the enumerated reason under test so a failure says *which*
    /// arm regressed — an injection on one arm must not be masked by the other
    /// nine passing.
    fn assert_lines_never_eval_able(arm: &str, lines: &[String]) {
        let prefixes = assignment_prefixes();
        // `print_plain` does one `println!` per element, so an element holding
        // an embedded LF is ONE element and TWO physical lines on stdout.
        // Iterating elements would make this assertion blind to the only
        // failure mode it exists to catch — a dropped `quoted()` letting a
        // forged carrier's newline start a line of its own. Split first.
        for raw in lines.iter().flat_map(|line| line.split(['\n', '\r'])) {
            // Strip first: under colour the line opens with the theme's SGR
            // introducer, and every `starts_with` below would then be asked
            // about `\x1b[` instead of about the text.
            let line = console::strip_ansi_codes(raw);
            let trimmed = line.trim_start();
            for prefix in &prefixes {
                assert!(
                    !trimmed.starts_with(prefix.as_str()),
                    "arm `{arm}`: line is valid shell source (prefix {prefix:?}): {line:?}"
                );
            }
            assert!(
                !is_bare_assignment(trimmed),
                "arm `{arm}`: line is a bare shell assignment: {line:?}"
            );
        }
    }

    /// Colour-off and colour-on, at both detail tiers. Four renderings per
    /// arm, because each one is bytes a user can actually see and any of them
    /// could carry the injection the others do not.
    fn assert_never_eval_able(arm: &str, report: &ShellStateReport) {
        for theme in [plain_theme(), colour_theme()] {
            for detail in [Detail::Answer, Detail::Diagnostics] {
                let label = format!("{arm} (colour={}, {detail:?})", theme.color());
                assert_lines_never_eval_able(&label, &report.lines(&theme, detail));
            }
        }
    }

    /// The uncoloured theme — what a pipe, a file, or `--color never` gets.
    fn plain_theme() -> Theme {
        Theme::new(false)
    }

    /// The coloured theme — what an interactive terminal gets.
    fn colour_theme() -> Theme {
        Theme::new(true)
    }

    /// The default rendering: the answer, uncoloured.
    fn answer(report: &ShellStateReport) -> String {
        report.lines(&plain_theme(), Detail::Answer).join("\n")
    }

    /// The `--verbose` rendering: the answer plus its evidence, uncoloured.
    fn diagnostics(report: &ShellStateReport) -> String {
        report.lines(&plain_theme(), Detail::Diagnostics).join("\n")
    }

    /// The second half of the hard contract: the two commands' outputs are
    /// never interchangeable. No report line may equal anything the shipped
    /// emitters produce for the same data.
    fn assert_not_interchangeable_with_activate(arm: &str, report: &ShellStateReport) {
        let mut emitted: Vec<String> = Vec::new();
        for shell in ALL_SHELLS {
            for key in ["JAVA_HOME", "PATH", "OCX_HOME"] {
                emitted.extend(shell.export_constant(key, "/opt/x"));
                emitted.extend(shell.export_path(key, "/opt/x"));
                emitted.extend(shell.unset(key));
            }
        }
        let emitted: BTreeSet<&str> = emitted.iter().flat_map(|block| block.lines()).collect();

        // Positive control. Byte-inequality alone is a tautology of the fixed
        // label vocabulary — it can never go red, so on its own it is a green
        // indistinguishable from never having run. Prove the corpus is real and
        // the predicate discriminates: every emitted line MUST trip the
        // eval-ability predicate that no report line trips.
        assert!(!emitted.is_empty(), "the emitter corpus is empty; nothing was compared");
        let prefixes = assignment_prefixes();
        for line in &emitted {
            let trimmed = line.trim_start();
            assert!(
                prefixes.iter().any(|prefix| trimmed.starts_with(prefix.as_str())) || is_bare_assignment(trimmed),
                "the emitter corpus must consist of shell source; this line is not: {line:?}"
            );
        }

        for line in report
            .lines(&plain_theme(), Detail::Diagnostics)
            .iter()
            .flat_map(|line| line.split(['\n', '\r']))
        {
            assert!(
                !emitted.contains(line.trim()),
                "arm `{arm}`: report line is byte-identical to emitted shell source: {line:?}"
            );
        }
    }

    fn session(scope: ScopeId, source: SessionSource) -> Owner {
        Owner::Session { scope, source }
    }

    fn record(key: &str, value: &str, kind: ValueKind, owner: Owner, prior: Option<Prior>) -> AppliedValue {
        AppliedValue {
            key: key.to_owned(),
            value: RecordedValue::Literal(value.to_owned()),
            kind,
            separator: None,
            pre_existing: false,
            owner,
            prior,
        }
    }

    /// A readable state with a global record and a project scope holding two constants (one with a
    /// recorded prior, one without) and a path.
    fn state_with_project() -> ExecState {
        let global = session(ScopeId::Global, SessionSource::Composed);
        let project = session(ScopeId::Project, SessionSource::Composed);
        ExecState {
            applied: vec![
                record(
                    "OCX_HOME",
                    "/home/u/.ocx",
                    ValueKind::Constant,
                    global,
                    Some(Prior::Unset),
                ),
                record(
                    "JAVA_HOME",
                    "/work/proj/.ocx/jdk",
                    ValueKind::Constant,
                    project.clone(),
                    Some(Prior::Unset),
                ),
                record("MAVEN_OPTS", "-Xmx1g", ValueKind::Constant, project.clone(), None),
                record("PATH", "/work/proj/.ocx/bin", ValueKind::Path, project, None),
            ],
            ..ExecState::default()
        }
    }

    fn readable(state: ExecState) -> EnvStateReport {
        EnvStateReport::from_inherited(&Inherited::Readable(state), false).0
    }

    /// Replaces the report's state and re-derives the priors from it, as the command does.
    fn set_state(report: &mut ShellStateReport, state: ExecState) {
        report.env_state = readable(state);
        report.priors = ShellStateReport::priors_for(&report.env_state);
    }

    /// The state with every project-scope record removed: consent holds and nothing was applied yet.
    fn without_project_scope(report: &mut ShellStateReport) {
        let mut state = state_with_project();
        state.applied.retain(|record| {
            !matches!(
                record.owner,
                Owner::Session {
                    scope: ScopeId::Project,
                    ..
                }
            )
        });
        set_state(report, state);
    }

    /// The encoder dropped every applied record to fit the cap: the project scope was abandoned.
    fn over_cap_state(report: &mut ShellStateReport) {
        let state = ExecState {
            truncated: Truncation::AllAppliedDropped,
            ..ExecState::default()
        };
        set_state(report, state);
    }

    /// No state at all, as on a shell's first prompt.
    fn without_state(report: &mut ShellStateReport) {
        report.env_state = EnvStateReport::from_inherited(&Inherited::Absent, false).0;
        report.priors = Vec::new();
    }

    /// A set value the parent could not have written, as an unreadable state.
    fn unreadable_state(report: &mut ShellStateReport) {
        let header = HeaderView {
            major: Some(MAJOR + 1),
            ..HeaderView::default()
        };
        report.env_state = EnvStateReport::from_inherited(
            &Inherited::Unreadable {
                header,
                reason: UnreadableReason::OtherMajor,
            },
            false,
        )
        .0;
        report.priors = Vec::new();
    }

    /// The published report spells a record's kind `kind`, a prior as a `type`-tagged object, and a
    /// value verbatim, whatever the carrier spells them.
    #[test]
    fn published_state_spells_kind_tagged_priors_and_verbatim_values() {
        let mut report = base(None);
        let mut state = state_with_project();
        state.applied = vec![
            AppliedValue {
                separator: Some(" ".to_owned()),
                pre_existing: true,
                ..record(
                    "CFLAGS",
                    "-O2",
                    ValueKind::List,
                    session(ScopeId::Project, SessionSource::Invocation),
                    None,
                )
            },
            record(
                "GITHUB_TOKEN",
                "tok-sentinel",
                ValueKind::Constant,
                session(ScopeId::Project, SessionSource::Composed),
                Some(Prior::Withheld),
            ),
            record("JAVA_HOME", "/opt/jdk", ValueKind::Constant, Owner::Inherited, None),
            record(
                "EMPTY",
                "x",
                ValueKind::Constant,
                session(ScopeId::Global, SessionSource::Composed),
                Some(Prior::Value("-g".to_owned())),
            ),
        ];
        set_state(&mut report, state);

        let json = serde_json::to_value(&report).expect("the report serializes");
        let applied = &json["env_state"]["applied"];
        assert_eq!(
            applied[0],
            serde_json::json!({
                "key": "CFLAGS", "value": "-O2", "kind": "list", "separator": " ", "pre_existing": true,
                "owner": "session", "scope": "project", "source": "invocation"
            }),
            "an applied record carries `kind` and no `type`"
        );
        assert_eq!(
            applied[1],
            serde_json::json!({
                "key": "GITHUB_TOKEN", "value": "tok-sentinel", "kind": "constant",
                "owner": "session", "scope": "project", "source": "composed", "prior": {"type": "withheld"}
            }),
            "a record carries its value verbatim and no `withheld` field"
        );
        assert_eq!(
            applied[2],
            serde_json::json!({"key": "JAVA_HOME", "value": "/opt/jdk", "kind": "constant", "owner": "inherited"})
        );
        assert_eq!(applied[3]["prior"], serde_json::json!({"type": "value", "value": "-g"}));
    }

    fn base(reason: Option<Reason>) -> ShellStateReport {
        let env_state = readable(state_with_project());
        let priors = ShellStateReport::priors_for(&env_state);
        // An active project was granted by *something*, an inert one by
        // nothing — so the grant tracks the reason rather than being a third
        // independent knob a fixture could set incoherently. This is what puts
        // the `granted by:` line, and the untrusted timestamp it interpolates,
        // inside the never-eval-able corpus.
        let granted = reason.is_none();
        ShellStateReport {
            ocx_home: AbsolutePath::new(if cfg!(windows) { r"C:\u\.ocx" } else { "/home/u/.ocx" })
                .expect("host-absolute fixture"),
            ocx_home_present: true,
            shell_integration_installed: true,
            // A relocated home, not the in-project default: the never-eval-able
            // corpus only covers a rendering of this field if the fixture makes
            // it differ from a path already in the report.
            toolchain_home: PathBuf::from("/home/u/toolchains/0123456789abcdef/toolchain"),
            toolchain_bin: PathBuf::from("/home/u/toolchains/0123456789abcdef/toolchain/active/bin"),
            activate: ocx_project::activate::ActivateMode::Bin,
            pinned: false,
            lock_refusal: None,
            carrier_present: true,
            carrier_bytes: ByteSize::from(412),
            env_state,
            binary_pin: BinaryPinReport {
                path: Some(PathBuf::from(
                    "/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx",
                )),
                exists: true,
                matches_running: true,
            },
            fingerprint_current: None,
            watch_set: vec![
                WatchMember {
                    path: PathBuf::from("/etc/ocx/config.toml"),
                    present: false,
                    size: None,
                    modified_at: None,
                },
                WatchMember {
                    path: PathBuf::from("/home/u/.ocx/config.toml"),
                    present: true,
                    size: Some(ByteSize::from(1290)),
                    modified_at: Some("2025-08-24T01:46:40Z".parse().expect("valid instant")),
                },
            ],
            project_dir: Some(PathBuf::from("/work/proj")),
            project_key: Some("0123456789abcdef".to_owned()),
            project_stamped: granted,
            grant: granted.then_some(Grant::Stamp),
            stamp_written_at: granted.then(|| "2026-08-27T07:12:03Z".parse().expect("valid instant")),
            priors,
            hook: HookStatus {
                rung: "auto".to_owned(),
                tier: None,
                enabled: None,
            },
            yielded_to: Vec::new(),
            inert_reason: reason,
            notes: Vec::new(),
        }
    }

    /// Every enumerated inertness reason, each paired with the arm name the
    /// assertions report on failure. This is the corpus S-022 requires: the
    /// never-eval-able assertion runs **per arm**, not once.
    fn every_arm() -> Vec<(&'static str, ShellStateReport)> {
        // The arms decidable without extra fixture state, up front; the ones
        // below need a mutated `base(..)` and are pushed one at a time.
        //
        // Both halves of the uncorroborated refusal appear here: a record that
        // disagrees with the claim, and no record at all. They render different
        // advice, so one arm would leave half the renderer unquantified.
        let mut arms = vec![
            ("active", base(None)),
            (
                "no_stamp_no_grant",
                base(Some(Reason::NoStampNoGrant {
                    derived_sources: BTreeSet::from(["ocx.sh/acme".to_owned()]),
                    paths_tested: vec![PathBuf::from("/work/other")],
                    namespaces_tested: vec!["ocx.sh/other".to_owned()],
                })),
            ),
            (
                "source_set_drift",
                base(Some(Reason::SourceSetDrift {
                    new_sources: BTreeSet::from(["ghcr.io/evil".to_owned()]),
                })),
            ),
            (
                "uncorroborated_namespace_disagrees",
                base(Some(Reason::UncorroboratedNamespace {
                    claimed_sources: BTreeSet::from(["ocx.sh/acme".to_owned()]),
                    verified_sources: Some(BTreeSet::from(["ocx.sh/evil".to_owned()])),
                })),
            ),
            (
                "uncorroborated_namespace_unrecorded",
                base(Some(Reason::UncorroboratedNamespace {
                    claimed_sources: BTreeSet::from(["ocx.sh/acme".to_owned()]),
                    verified_sources: None,
                })),
            ),
        ];

        let mut env_rung = base(Some(Reason::HookDisabled {
            rung: "OCX_NO_HOOK".to_owned(),
            tier: None,
        }));
        env_rung.hook = HookStatus {
            rung: "OCX_NO_HOOK".to_owned(),
            tier: None,
            enabled: Some(false),
        };
        arms.push(("hook_disabled_env", env_rung));

        let mut managed_rung = base(Some(Reason::HookDisabled {
            rung: "[shell] hook".to_owned(),
            tier: Some("managed config".to_owned()),
        }));
        managed_rung.hook = HookStatus {
            rung: "[shell] hook".to_owned(),
            tier: Some("managed config".to_owned()),
            enabled: Some(false),
        };
        arms.push(("hook_disabled_managed_tier", managed_rung));

        let mut explicit_rung = base(Some(Reason::HookDisabled {
            rung: "[shell] hook".to_owned(),
            tier: Some("--config / OCX_CONFIG".to_owned()),
        }));
        explicit_rung.hook = HookStatus {
            rung: "[shell] hook".to_owned(),
            tier: Some("--config / OCX_CONFIG".to_owned()),
            enabled: Some(false),
        };
        arms.push(("hook_disabled_explicit_tier", explicit_rung));

        let direnv = Observation {
            tool: Tool::Direnv,
            signal: "DIRENV_DIR=/work/proj".to_owned(),
        };
        let mise = Observation {
            tool: Tool::Mise,
            signal: "MISE_SHELL=bash".to_owned(),
        };
        let mut yielded = base(Some(Reason::YieldedTo(direnv.clone())));
        yielded.yielded_to = vec![direnv.clone()];
        arms.push(("yielded_direnv", yielded));

        let mut yielded_mise = base(Some(Reason::YieldedTo(mise.clone())));
        yielded_mise.yielded_to = vec![mise.clone()];
        arms.push(("yielded_mise", yielded_mise));

        let mut yielded_both = base(Some(Reason::YieldedTo(direnv.clone())));
        yielded_both.yielded_to = vec![direnv, mise];
        arms.push(("yielded_both", yielded_both));

        let mut over_cap = base(Some(Reason::StateOverCap {
            scope: AbandonedScope::Project,
        }));
        over_cap_state(&mut over_cap);
        arms.push(("state_over_cap", over_cap));

        let mut absent = base(Some(Reason::StateUnreadable { first_prompt: true }));
        absent.carrier_present = false;
        absent.carrier_bytes = ByteSize::from(0);
        without_state(&mut absent);
        arms.push(("ledger_absent_first_prompt", absent));

        let mut never_set_up = base(Some(Reason::StateUnreadable { first_prompt: true }));
        never_set_up.shell_integration_installed = false;
        never_set_up.carrier_present = false;
        never_set_up.carrier_bytes = ByteSize::from(0);
        without_state(&mut never_set_up);
        arms.push(("ledger_absent_setup_never_ran", never_set_up));

        let mut lock_refused = base(None);
        lock_refused.lock_refusal = Some("ocx.lock is stale (it does not match ocx.toml); run `ocx lock`".to_owned());
        without_project_scope(&mut lock_refused);
        arms.push(("lock_refuses_composition", lock_refused));

        let mut corrupt = base(Some(Reason::StateUnreadable { first_prompt: false }));
        without_state(&mut corrupt);
        arms.push(("ledger_corrupt", corrupt));

        arms.push(("lock_unavailable", base(Some(Reason::LockUnavailable))));

        let mut symlinked = base(None);
        symlinked.notes = vec![Note::SymlinkedCandidateSkipped {
            candidate: PathBuf::from("/work/proj/ocx.toml"),
            ancestor: PathBuf::from("/work"),
        }];
        arms.push(("note_symlinked_candidate", symlinked));

        let mut granted = base(None);
        granted.notes = vec![Note::ActiveViaPathsGrant {
            entry: PathBuf::from("/work/proj"),
        }];
        arms.push(("note_active_via_paths_grant", granted));

        let mut near_miss = base(Some(Reason::NoStampNoGrant {
            derived_sources: BTreeSet::new(),
            paths_tested: vec![PathBuf::from("/Users/u/Repo")],
            namespaces_tested: Vec::new(),
        }));
        near_miss.notes = vec![Note::PathsNearMiss {
            entry: PathBuf::from("/Users/u/Repo"),
            canonical: PathBuf::from("/Users/u/repo"),
        }];
        arms.push(("note_paths_near_miss", near_miss));

        let mut paths_defect = base(None);
        paths_defect.notes = vec![Note::PathsDefect {
            entry: PathBuf::from("/w/*/tools"),
            defect: "a `*` may appear only as the entry's final path component".to_owned(),
        }];
        arms.push(("note_paths_defect", paths_defect));

        let mut manifest_unparsed = base(None);
        manifest_unparsed.notes = vec![Note::ToolchainManifestUnparsed {
            manifest: PathBuf::from("/home/u/.ocx/ocx.toml"),
            // The loader's own message, which embeds a project path: untrusted
            // bytes, and therefore quoted by the renderer.
            detail: "/home/u/.ocx/ocx.toml: invalid TOML".to_owned(),
        }];
        arms.push(("note_toolchain_manifest_unparsed", manifest_unparsed));

        let mut unresolved = base(None);
        unresolved.project_dir = None;
        unresolved.project_key = None;
        unresolved.notes = vec![Note::ProjectUnresolved {
            // A resolution failure carries the loader's own message, which
            // embeds a project path: untrusted bytes, and therefore quoted.
            detail: "invalid TOML in '/work/proj\nexport OCX_EVIL=1/ocx.toml'".to_owned(),
        }];
        arms.push(("note_project_unresolved", unresolved));

        // The third activation state SPEC-2 named: the carrier decoded and
        // holds no project scope, so the report says "not yet" rather than
        // borrowing the absent-carrier sentence.
        let mut pending = base(None);
        without_project_scope(&mut pending);
        arms.push(("project_scope_pending", pending));

        // The five notes the execution state owes, each through the constructor the command calls.
        let mut unreadable = base(Some(Reason::StateUnreadable { first_prompt: false }));
        let (env_state, notes) = EnvStateReport::from_inherited(
            &Inherited::Unreadable {
                header: HeaderView {
                    major: Some(MAJOR + 1),
                    home: Some(PathBuf::from("/h\nexport OCX_EVIL=5")),
                    ..HeaderView::default()
                },
                reason: UnreadableReason::Invalid { field: "applied.key" },
            },
            true,
        );
        unreadable.env_state = env_state;
        unreadable.notes = notes;
        arms.push(("note_env_state_unreadable_and_stood_down", unreadable));

        let mut truncated = base(None);
        let (env_state, notes) = EnvStateReport::from_inherited(
            &Inherited::Readable(ExecState {
                truncated: Truncation::InheritedDropped,
                ..state_with_project()
            }),
            true,
        );
        truncated.env_state = env_state;
        truncated.notes = notes;
        arms.push(("note_env_state_truncated", truncated));

        let mut relaxing = base(None);
        let (env_state, notes) = EnvStateReport::from_inherited(
            &Inherited::Readable(ExecState {
                policy: Policy {
                    no_verify: Some(true),
                    mirrors: Some("a=b\nexport OCX_EVIL=6".to_owned()),
                    ..Policy::default()
                },
                home: Some(PathBuf::from("/home/u/.ocx")),
                ..state_with_project()
            }),
            true,
        );
        relaxing.env_state = env_state;
        relaxing.notes = notes;
        arms.push(("note_relaxing_policy", relaxing));

        let mut pin_missing = base(None);
        pin_missing.binary_pin = BinaryPinReport {
            path: Some(PathBuf::from("/h/.ocx/ocx\nexport OCX_EVIL=7")),
            exists: false,
            matches_running: false,
        };
        pin_missing.notes = pin_missing.binary_pin.note().into_iter().collect();
        arms.push(("note_binary_pin_missing", pin_missing));

        let mut config_missing = base(None);
        config_missing.notes = vec![Note::ReceivedConfigMissing {
            path: PathBuf::from("/h/.ocx/config.toml\nexport OCX_EVIL=8"),
        }];
        arms.push(("note_received_config_missing", config_missing));

        arms
    }

    /// A stable tag per [`Reason`] variant.
    ///
    /// The `match` is exhaustive with no wildcard, so a new variant is a
    /// **compile error here** — mirroring `all_shells_is_exhaustive` for the
    /// enum the never-eval-able invariant is quantified over on the *report*
    /// side. `reason_lines` already forces a renderer arm; this forces a test
    /// arm, and the two together are what make "for **every** enumerated
    /// inertness reason" (S-022) structurally true rather than a count.
    fn reason_tag(reason: &Reason) -> &'static str {
        match reason {
            Reason::NoStampNoGrant { .. } => "no_stamp_no_grant",
            Reason::SourceSetDrift { .. } => "source_set_drift",
            Reason::UncorroboratedNamespace { .. } => "uncorroborated_namespace",
            Reason::HookDisabled { .. } => "hook_disabled",
            Reason::YieldedTo(_) => "yielded_to",
            Reason::StateOverCap { .. } => "state_over_cap",
            Reason::StateUnreadable { .. } => "state_unreadable",
            Reason::LockUnavailable => "lock_unavailable",
        }
    }

    /// [`reason_tag`]'s twin for [`Note`].
    fn note_tag(note: &Note) -> &'static str {
        match note {
            Note::SymlinkedCandidateSkipped { .. } => "symlinked_candidate_skipped",
            Note::ActiveViaPathsGrant { .. } => "active_via_paths_grant",
            Note::ProjectUnresolved { .. } => "project_unresolved",
            Note::PathsNearMiss { .. } => "paths_near_miss",
            Note::PathsDefect { .. } => "paths_defect",
            Note::ToolchainManifestUnparsed { .. } => "toolchain_manifest_unparsed",
            Note::EnvStateUnreadable { .. } => "env_state_unreadable",
            Note::EnvStateStoodDown => "env_state_stood_down",
            Note::EnvStateTruncated { .. } => "env_state_truncated",
            Note::RelaxingPolicy { .. } => "relaxing_policy",
            Note::BinaryPinMissing { .. } => "binary_pin_missing",
            Note::ReceivedConfigMissing { .. } => "received_config_missing",
        }
    }

    /// S-022's quantifier, enforced: **every** `Reason` variant and **every**
    /// `Note` variant appears in `every_arm()`, so the per-arm never-eval-able
    /// assertion below cannot run green over a corpus that is missing one.
    ///
    /// Adding a variant breaks the build in `reason_tag` / `note_tag`; adding
    /// the tag without adding the arm breaks this assertion. Both edits are
    /// forced.
    #[test]
    fn c050_s022_every_reason_and_note_variant_is_in_the_arm_corpus() {
        let arms = every_arm();
        let reasons: BTreeSet<&str> = arms
            .iter()
            .filter_map(|(_, report)| report.inert_reason.as_ref())
            .map(reason_tag)
            .collect();
        assert_eq!(
            reasons,
            BTreeSet::from([
                "no_stamp_no_grant",
                "source_set_drift",
                "uncorroborated_namespace",
                "hook_disabled",
                "yielded_to",
                "state_over_cap",
                "state_unreadable",
                "lock_unavailable",
            ]),
            "every Reason variant must be exercised by an arm in `every_arm()`"
        );

        let notes: BTreeSet<&str> = arms
            .iter()
            .flat_map(|(_, report)| report.notes.iter())
            .map(note_tag)
            .collect();
        assert_eq!(
            notes,
            BTreeSet::from([
                "symlinked_candidate_skipped",
                "active_via_paths_grant",
                "project_unresolved",
                "paths_near_miss",
                "paths_defect",
                "toolchain_manifest_unparsed",
                "env_state_unreadable",
                "env_state_stood_down",
                "env_state_truncated",
                "relaxing_policy",
                "binary_pin_missing",
                "received_config_missing",
            ]),
            "every Note variant must be exercised by an arm in `every_arm()`"
        );

        // `StateUnreadable` is two contractual situations behind one variant
        // (C-006), so the tag set alone would not prove both are covered.
        let first_prompt: BTreeSet<bool> = arms
            .iter()
            .filter_map(|(_, report)| match report.inert_reason.as_ref() {
                Some(Reason::StateUnreadable { first_prompt }) => Some(*first_prompt),
                _ => None,
            })
            .collect();
        assert_eq!(
            first_prompt,
            BTreeSet::from([true, false]),
            "both halves of C-050 reason 6 must be exercised"
        );
    }

    // ── C-050 / S-022: the load-bearing invariant, per arm ───────────────────

    /// S-022 — for **every** enumerated inertness reason, no output line is
    /// valid `export` / `set` / `set -gx` / `$env.` / `$Env:` syntax, and none
    /// is a bare `KEY=value` assignment.
    ///
    /// EC-REC-004, half one of two: `ocx shell state` is never eval-able. No
    /// live shell is needed to decide it and none is used — the prefixes are
    /// **derived from the shipped emitters** ([`assignment_prefixes`], which
    /// itself fails if any arm stops contributing one), so "is this line shell
    /// source" is answered by the same code that produces shell source rather
    /// than by a hardcoded list that could drift away from it.
    ///
    /// `--format json` needs no separate arm: it is a context, not a divergent
    /// command surface — the same `ShellStateReport` renders it, and a JSON
    /// document is not `export`/`set`/`$env.` syntax in any of the five shells
    /// under any encoding of these values.
    ///
    /// The other half — that the two commands' outputs are never
    /// interchangeable — is
    /// [`c050_s022_no_reason_arm_is_interchangeable_with_activate`].
    #[test]
    fn c050_s022_no_reason_arm_renders_eval_able_output() {
        for (arm, report) in every_arm() {
            assert_never_eval_able(arm, &report);
        }
    }

    /// S-022 — and the two commands' outputs are never interchangeable, for
    /// every arm.
    ///
    /// EC-REC-004, half two of two: no `ocx shell state` line is byte-identical
    /// to anything `ocx self activate` emits for the same data. The emitter
    /// corpus is asserted non-empty and every line in it is asserted to trip
    /// the eval-ability predicate no report line trips — without that positive
    /// control the comparison is a tautology of the fixed label vocabulary and
    /// could never go red.
    #[test]
    fn c050_s022_no_reason_arm_is_interchangeable_with_activate() {
        for (arm, report) in every_arm() {
            assert_not_interchangeable_with_activate(arm, &report);
        }
    }

    /// A carrier value carrying a literal LF must not split itself into an
    /// unlabelled second line — the one way rule 1 could be defeated.
    #[test]
    fn c007_c050_a_forged_carrier_cannot_inject_a_line() {
        let mut report = base(None);
        let mut state = state_with_project();
        state.applied.push(record(
            "$env.EVIL",
            "x\nexport OCX_EVIL=1\n",
            ValueKind::Constant,
            session(ScopeId::Project, SessionSource::Composed),
            Some(Prior::Value("y\nexport OCX_EVIL=3".to_owned())),
        ));
        state.inherited_scopes.push(Scope::Project {
            root: PathBuf::from("/work\nexport OCX_EVIL=2"),
        });
        state.home = Some(PathBuf::from("/h\nexport OCX_EVIL=4"));
        set_state(&mut report, state);
        assert_never_eval_able("forged_carrier", &report);
    }

    // ── C-050: every arm is individually reachable and says its own thing ────

    /// C-050 reason 1 — the derived source set and the grants it was tested
    /// against are named.
    #[test]
    fn c050_no_stamp_no_grant_names_sources_and_grants() {
        let report = base(Some(Reason::NoStampNoGrant {
            derived_sources: BTreeSet::from(["ocx.sh/acme".to_owned()]),
            paths_tested: vec![PathBuf::from("/work/other")],
            namespaces_tested: vec!["ocx.sh/other".to_owned()],
        }));
        let text = answer(&report);
        assert!(text.contains("no consent stamp, and no matching grant"), "{text}");
        assert!(text.contains("ocx.sh/acme"), "{text}");
        assert!(text.contains("/work/other"), "{text}");
        assert!(text.contains("ocx.sh/other"), "{text}");
    }

    /// C-050 reason 2 — the source that is **new** is named.
    #[test]
    fn c050_source_set_drift_names_the_new_source() {
        let report = base(Some(Reason::SourceSetDrift {
            new_sources: BTreeSet::from(["ghcr.io/evil".to_owned()]),
        }));
        let text = answer(&report);
        assert!(text.contains("not a subset of the stamp"), "{text}");
        assert!(text.contains("ghcr.io/evil"), "{text}");
    }

    /// C-050 reason 3 + A-32 — the deciding rung **and** the deciding tier by
    /// name, never a hard-coded "managed". The explicit tier is a possible
    /// answer.
    #[test]
    fn c050_a032_hook_disabled_names_the_deciding_rung_and_tier() {
        for (rung, tier, expected_tier) in [
            ("OCX_NO_HOOK", None, "not a config tier"),
            ("[shell] hook", Some("managed config"), "managed config"),
            ("[shell] hook", Some("--config / OCX_CONFIG"), "--config / OCX_CONFIG"),
        ] {
            let report = base(Some(Reason::HookDisabled {
                rung: rung.to_owned(),
                tier: tier.map(str::to_owned),
            }));
            let text = answer(&report);
            assert!(text.contains("the per-prompt hook is disabled"), "{text}");
            assert!(text.contains(rung), "{text}");
            assert!(text.contains(expected_tier), "{text}");
        }
    }

    /// C-050 reason 4 + A-37 — one line per observed tool, never an `elif`
    /// chain that suppresses the second.
    #[test]
    fn c050_a037_both_yield_sentinels_get_their_own_line() {
        let direnv = Observation {
            tool: Tool::Direnv,
            signal: "DIRENV_DIR=/work/proj".to_owned(),
        };
        let mise = Observation {
            tool: Tool::Mise,
            signal: "MISE_SHELL=bash".to_owned(),
        };
        let mut report = base(Some(Reason::YieldedTo(direnv.clone())));
        report.yielded_to = vec![direnv, mise];
        let lines = report.lines(&plain_theme(), Detail::Answer);
        assert_eq!(
            lines.iter().filter(|line| line.contains("live: direnv")).count(),
            1,
            "{lines:#?}"
        );
        assert_eq!(
            lines.iter().filter(|line| line.contains("live: mise")).count(),
            1,
            "{lines:#?}"
        );
        let text = lines.join("\n");
        assert!(text.contains("DIRENV_DIR=/work/proj"), "{text}");
        assert!(text.contains("MISE_SHELL=bash"), "{text}");
    }

    /// C-050 reason 5 + A-01 — the abandoned scope is read from the `over_cap`
    /// marker the carrier still carries, never inferred from an absent one.
    #[test]
    fn c050_a001_over_cap_is_read_from_the_marker() {
        let mut report = base(Some(Reason::StateOverCap {
            scope: AbandonedScope::Project,
        }));
        over_cap_state(&mut report);
        let text = diagnostics(&report);
        assert!(text.contains("truncated: all_applied_dropped"), "{text}");
        assert!(text.contains("abandoned scope: project"), "{text}");
        // The state is still decodable - the cap drops records, not the header.
        assert!(text.contains("status: readable"), "{text}");
    }

    /// C-050 reason 6 + C-006 — an absent carrier (first prompt) and a corrupt
    /// one are **different** reasons, not one.
    #[test]
    fn c050_c006_absent_and_corrupt_ledgers_read_differently() {
        let mut absent = base(Some(Reason::StateUnreadable { first_prompt: true }));
        absent.carrier_present = false;
        without_state(&mut absent);
        let absent_text = diagnostics(&absent);

        let mut corrupt = base(Some(Reason::StateUnreadable { first_prompt: false }));
        without_state(&mut corrupt);
        let corrupt_text = diagnostics(&corrupt);

        assert!(
            absent_text.contains("this is the first prompt, not a fault"),
            "{absent_text}"
        );
        assert!(absent_text.contains("the carrier is unset"), "{absent_text}");
        assert!(
            corrupt_text.contains("a scope was applied and its record is gone"),
            "{corrupt_text}"
        );
        assert!(absent_text.contains("status: absent"), "{absent_text}");
        assert_ne!(absent_text, corrupt_text);
    }

    /// Finding 90 — an unset carrier means two opposite things, and the report
    /// used to give the wrong remedy for the likelier one.
    ///
    /// The command's single most likely first use is *"I installed ocx, why
    /// isn't it working"*, and in that shell the carrier is unset because
    /// `ocx self setup` never ran. Answering "none needed - the next prompt
    /// applies it" is not merely unhelpful: **the fix never comes true**, since
    /// no prompt will apply anything until the rc fence exists.
    ///
    /// Red state: force `shell_integration_installed` to `true` in the first
    /// half (or delete the renderer branch) and it renders the converging arm —
    /// the promise this test exists to keep off an unwired shell. The second
    /// half is the twin that keeps the branch discriminating rather than always
    /// firing: the same reason, on a wired shell, still converges by itself.
    ///
    /// EC-REC-007 — the renderer half of the setup-never-run split.
    #[test]
    fn f090_an_unset_carrier_on_an_unwired_shell_names_setup_not_convergence() {
        let mut never_set_up = base(Some(Reason::StateUnreadable { first_prompt: true }));
        never_set_up.shell_integration_installed = false;
        never_set_up.carrier_present = false;
        without_state(&mut never_set_up);
        let unwired = diagnostics(&never_set_up);

        assert!(
            unwired.contains("the shell integration has never been installed here"),
            "{unwired}"
        );
        assert!(unwired.contains("ocx self setup"), "{unwired}");
        assert!(
            unwired.contains(ocx_setup::shims::WITNESS_SHIM),
            "the row must name the shim it probed, so the answer is checkable: {unwired}"
        );
        assert!(
            !unwired.contains("the next prompt applies it"),
            "the fix that can never come true must not survive: {unwired}"
        );

        let mut wired = base(Some(Reason::StateUnreadable { first_prompt: true }));
        wired.carrier_present = false;
        without_state(&mut wired);
        let converging = diagnostics(&wired);

        assert!(
            converging.contains("the next prompt applies it"),
            "a wired shell on its first prompt does converge by itself: {converging}"
        );
        assert_ne!(unwired, converging);
    }

    /// Finding 97 — `active: not yet` was a promise about a prompt that had
    /// already failed silently on every prompt before it.
    ///
    /// Edit `ocx.toml`, forget `ocx lock`, in a consented project: consent
    /// still says activate, composition refuses with `StaleLock`, the prompt
    /// exits 65 into a stderr the hook discards (A-21), and the project scope
    /// never reaches the ledger. The report then read the empty project scope
    /// as "consented, not applied *yet*" and told the user the next prompt
    /// would apply it — forever.
    ///
    /// Red state: delete the `Some(refusal)` arm from `activation_lines`, or
    /// clear `lock_refusal` in the first half below, and it renders the
    /// pending arm. The second half is the twin that keeps the branch
    /// discriminating: with no refusal, a genuinely pending scope still says
    /// `not yet`.
    ///
    /// EC-REC-008 — the renderer half of the lock-refusal split.
    #[test]
    fn f097_a_lock_that_refuses_composition_is_not_a_scope_that_is_merely_pending() {
        let refusal = "ocx.lock is stale (it does not match ocx.toml); run `ocx lock`";

        let mut refused = base(None);
        refused.lock_refusal = Some(refusal.to_owned());
        without_project_scope(&mut refused);
        assert!(
            refused.project_scope_pending(),
            "the fixture must reach the pending arm, or this asserts nothing about it"
        );
        let refused_text = diagnostics(&refused);

        assert!(refused_text.contains(refusal), "{refused_text}");
        assert!(
            refused_text.contains("run `ocx lock`"),
            "the fix must name the command that ends the silence: {refused_text}"
        );
        assert!(
            !refused_text.contains("the next prompt applies it"),
            "the promise that cannot come true must not survive: {refused_text}"
        );

        let mut pending = base(None);
        without_project_scope(&mut pending);
        let pending_text = diagnostics(&pending);

        assert!(
            pending_text.contains("the next prompt applies it"),
            "a pending scope with a composable lock does converge by itself: {pending_text}"
        );
        assert_ne!(refused_text, pending_text);
    }

    /// A-12 — a symlinked candidate promotes the ancestor, and the row names
    /// `--project` / `OCX_PROJECT` as the opt-in. The loader's `log::warn!`
    /// never reaches the prompt, so this row is the only path to that answer.
    #[test]
    fn a012_symlinked_candidate_row_names_the_ancestor_and_the_opt_in() {
        let mut report = base(None);
        report.notes = vec![Note::SymlinkedCandidateSkipped {
            candidate: PathBuf::from("/work/proj/ocx.toml"),
            ancestor: PathBuf::from("/work"),
        }];
        let text = answer(&report);
        assert!(text.contains("/work/proj/ocx.toml"), "{text}");
        assert!(text.contains("activated instead"), "{text}");
        assert!(text.contains("--project"), "{text}");
        assert!(text.contains("OCX_PROJECT"), "{text}");
    }

    /// A-26 — active via a `paths` grant, and source-set drift is not tracked
    /// for path grants. Truthful rather than phantom.
    #[test]
    fn a026_paths_grant_row_states_that_drift_is_not_tracked() {
        let mut report = base(None);
        report.notes = vec![Note::ActiveViaPathsGrant {
            entry: PathBuf::from("/work/proj"),
        }];
        let text = answer(&report);
        assert!(text.contains("active via a paths grant"), "{text}");
        assert!(
            text.contains("source-set drift is not tracked for path grants"),
            "{text}"
        );
    }

    /// A-28 — a `paths` near-miss differing only by ASCII case gets its own
    /// diagnostic, and the report still says it does not grant.
    #[test]
    fn a028_paths_near_miss_row_names_both_spellings() {
        let mut report = base(Some(Reason::NoStampNoGrant {
            derived_sources: BTreeSet::new(),
            paths_tested: vec![PathBuf::from("/Users/u/Repo")],
            namespaces_tested: Vec::new(),
        }));
        report.notes = vec![Note::PathsNearMiss {
            entry: PathBuf::from("/Users/u/Repo"),
            canonical: PathBuf::from("/Users/u/repo"),
        }];
        let text = answer(&report);
        assert!(text.contains("/Users/u/Repo"), "{text}");
        assert!(text.contains("/Users/u/repo"), "{text}");
        assert!(text.contains("does not grant"), "{text}");
    }

    /// A-38 — the combined env-block size is an OS boundary, reported as such:
    /// the 16 KiB cap bounds ocx's own contribution and nothing else.
    #[test]
    fn a038_the_env_block_boundary_is_reported_as_the_os_limit() {
        let text = diagnostics(&base(None));
        assert!(text.contains(&format!("of {MAX_ENCODED}")), "{text}");
        // The cap is reported; the *explanation* of what it does and does not
        // bound is documentation and lives on the docs page. A status line
        // that carries a paragraph is the thing this rendering removed, so the
        // assertion that used to require the paragraph now forbids it.
        assert!(
            !text.contains("OS limit"),
            "the env-block explanation belongs in the docs, not in a parenthesis on a status line: {text}"
        );
    }

    /// The state renders as **fields**, never as the base64 the carrier holds, and each record names
    /// its owner.
    #[test]
    fn c050_the_state_renders_as_fields_and_records_name_their_owner() {
        let encoded = ocx_config::exec_state::encode(&state_with_project())
            .value
            .expect("the fixture state encodes");
        let report = base(None);
        let text = diagnostics(&report);

        assert!(!text.contains(&encoded), "the report must not carry the base64 carrier");
        assert!(text.contains("status: readable"), "{text}");
        assert!(text.contains("major: 2"), "{text}");
        assert!(
            text.contains("OCX_HOME constant /home/u/.ocx (session global composed)"),
            "{text}"
        );
        assert!(
            text.contains("JAVA_HOME constant /work/proj/.ocx/jdk (session project composed)"),
            "{text}"
        );
        assert!(text.contains("PATH path /work/proj/.ocx/bin"), "{text}");
        assert!(text.contains("replay: 0 env, 0 launch identities"), "{text}");
        assert!(text.contains("follows the working directory"), "{text}");
    }

    /// C-050 — `priors` intactness is reported per constant the project scope
    /// owns. `MAVEN_OPTS` is applied as a constant with no recorded prior: that
    /// is the datum nothing can reconstruct, so it must read as missing.
    #[test]
    fn c050_c012_priors_intactness_is_per_constant() {
        let report = base(None);
        assert_eq!(
            report.priors,
            vec![
                PriorStatus {
                    key: "JAVA_HOME".to_owned(),
                    intact: true
                },
                PriorStatus {
                    key: "MAVEN_OPTS".to_owned(),
                    intact: false
                },
            ],
            "a path-kind entry owns no prior and must not appear"
        );
        let text = diagnostics(&report);
        assert!(text.contains("JAVA_HOME intact (was unset)"), "{text}");
        assert!(text.contains("MAVEN_OPTS MISSING"), "{text}");
    }

    /// S-022 — quoting is now conditional, so the condition is the guard.
    ///
    /// The relaxation is only safe if the bare arm is a strict subset of what
    /// `{:?}` would have left alone, which is what every hostile case below
    /// asserts: each one must still come back quoted **and** escaped, and the
    /// escape is checked by looking for the backslash form rather than for the
    /// raw byte, because a raw byte in the expectation would match a report
    /// that emitted it.
    ///
    /// Red state: make `quoted` return `text.to_owned()` unconditionally and
    /// every hostile case below fails; make it `format!("{text:?}")`
    /// unconditionally and the first case fails.
    #[test]
    fn s022_quoting_is_dropped_only_for_values_that_need_none() {
        assert_eq!(quoted("/home/u/.ocx/config.toml"), "/home/u/.ocx/config.toml");
        assert_eq!(quoted("ocx.sh/acme/tool:1.2.3"), "ocx.sh/acme/tool:1.2.3");

        for hostile in [
            "a\nforged: row",
            "a\rcarriage return",
            "a\u{1b}[2J screen wipe",
            "a\u{202e}bidi override",
            "[shell] hook",
            "a \"quote\"",
            "a\\backslash",
            "",
        ] {
            let rendered = quoted(hostile);
            assert!(
                rendered.starts_with('"') && rendered.ends_with('"'),
                "a value needing escapes must stay quoted: {hostile:?} -> {rendered}"
            );
            assert!(
                !rendered[1..rendered.len() - 1].chars().any(|c| c.is_control()),
                "no control byte may survive into the report: {hostile:?} -> {rendered}"
            );
        }
    }

    /// C-050 — the watch set reports each member's presence, size and modification time,
    /// including members that do not exist (A-13: a tier file becoming present
    /// is itself the change).
    #[test]
    fn c050_a013_watch_set_reports_absent_members_too() {
        let text = diagnostics(&base(None));
        assert!(text.contains("/etc/ocx/config.toml (absent)"), "{text}");
        // The age is relative to the wall clock, so only the two halves that
        // are not are pinned: pinning "53 weeks ago" would red on its own the
        // week after it was written.
        assert!(
            text.contains("/home/u/.ocx/config.toml (present, 1.26 KiB, modified "),
            "{text}"
        );
        assert!(text.contains(", 2025-08-24 01:46:40 UTC)"), "{text}");
    }

    /// SPEC-2 — a ledger that **decoded** is never described as an unset
    /// carrier. The report says `not yet`, and says nothing that contradicts
    /// the `present: yes` / `decoded: yes` lines above it.
    #[test]
    fn c050_c006_a_decoded_ledger_is_never_reported_as_an_unset_carrier() {
        let mut pending = base(None);
        without_project_scope(&mut pending);
        assert!(pending.project_scope_pending());

        let text = diagnostics(&pending);
        assert!(text.contains("present: yes"), "{text}");
        assert!(text.contains("status: readable"), "{text}");
        assert!(text.contains("active: not yet"), "{text}");
        assert!(
            text.contains("the project scope is consented and not yet applied"),
            "{text}"
        );
        assert!(
            !text.contains("the carrier is unset"),
            "a decoded carrier must never be described as unset: {text}"
        );
        assert!(!text.contains("active: yes"), "{text}");

        // And the state is not claimed when the carrier never decoded — that is
        // `StateUnreadable`'s business, not this one's.
        let mut absent = base(None);
        absent.carrier_present = false;
        without_state(&mut absent);
        assert!(!absent.project_scope_pending());
    }

    /// C-059 — `activate = none` is reported as inert, and the *same* fixture
    /// with a composing mode is not.
    ///
    /// Both halves are the test. `base(None)` is a consented project with a
    /// decoded ledger carrying an applied project scope: under `bin` it is
    /// genuinely active, and under `none` every field in the report is
    /// byte-identical except the mode. So an implementation that ignored
    /// `activate` — the one this replaces — answers `yes` to both, and one that
    /// hard-codes `no` fails the control. Only reading the mode passes.
    #[test]
    fn c059_activate_none_is_not_reported_as_active() {
        let mut composing = base(None);
        composing.activate = ActivateMode::Bin;
        let control = answer(&composing);
        assert!(
            control.contains("active: yes"),
            "the control arm must stay active, or the assertion below proves nothing: {control}"
        );

        let mut inert = base(None);
        inert.activate = ActivateMode::None;
        let text = answer(&inert);
        assert!(
            text.contains("active: no"),
            "`activate = none` composes nothing and puts nothing on PATH: {text}"
        );
        assert!(!text.contains("active: yes"), "{text}");
        assert!(
            !text.contains("the next prompt applies it"),
            "no prompt ever applies anything in `none` mode: {text}"
        );
        assert!(text.contains("reason: activate = none"), "{text}");
        assert!(
            text.lines().any(|line| line.starts_with("fix: ")),
            "the only user-facing refusal with no fix line is a dead end: {text}"
        );
    }

    /// QUAL-3 — a project that is reachable but unresolvable gets its own row
    /// rather than being reported as "no project reachable", the verdict above
    /// it says `no`, and the row carries a fix like every other refusal.
    ///
    /// The verdict is the load-bearing half. An unparseable `ocx.toml` composes
    /// nothing, so `active: yes` over it is a false statement made by the one
    /// command whose entire product is the explanation — and it is exactly what
    /// this rendered before, because no enumerated [`Reason`] fires for a
    /// project the consent predicate never got to see.
    #[test]
    fn qual3_an_unresolvable_project_is_not_reported_as_no_project() {
        let mut report = base(None);
        report.project_dir = None;
        report.notes = vec![Note::ProjectUnresolved {
            detail: "invalid TOML in '/work/proj/ocx.toml'".to_owned(),
        }];
        let text = answer(&report);
        assert!(
            text.contains("a project file is reachable but could not be resolved"),
            "{text}"
        );
        assert!(text.contains("invalid TOML"), "{text}");
        assert!(
            text.contains("active: no"),
            "a project whose config will not parse is not active: {text}"
        );
        assert!(
            !text.contains("active: yes"),
            "a broken project file must never report as healthy: {text}"
        );
        assert!(
            text.lines().any(|line| line.starts_with("fix: ")),
            "the only user-facing refusal with no fix line is a dead end: {text}"
        );
    }

    /// C-019 / SPEC-3 — the fingerprint line has three states and all three are
    /// rendered. `None` is the **deferral**, not the contract: when WP-11's
    /// fold lands, `Some(true)` / `Some(false)` become reachable in production
    /// and this test already pins their wording.
    #[test]
    fn c019_the_fingerprint_line_renders_all_three_states() {
        for (value, expected) in [
            (Some(true), "  matches watch set: yes"),
            (Some(false), "  matches watch set: no"),
            (None, "  matches watch set: not compared"),
        ] {
            let mut report = base(None);
            report.fingerprint_current = value;
            let lines = report.lines(&plain_theme(), Detail::Diagnostics);
            assert!(
                lines.iter().any(|line| line == expected),
                "fingerprint_current={value:?} must render {expected:?}; got {lines:#?}"
            );
        }
    }

    // -- The two rendering tiers, and what each one owes the reader ---------

    /// The default rendering answers the question and stops: where OCX is,
    /// which project is in effect, whether it is active, and - when it is not -
    /// the reason and the fix.
    ///
    /// Two-sided on purpose. Asserting only that the evidence is absent would
    /// pass just as well on a rendering that printed nothing at all, so each
    /// omitted section is asserted **present** at the other tier in the same
    /// breath, and the answer is asserted to lead at both.
    #[test]
    fn the_default_rendering_leads_with_the_answer_and_omits_the_evidence() {
        let report = base(Some(Reason::NoStampNoGrant {
            derived_sources: BTreeSet::from(["ocx.sh/acme".to_owned()]),
            paths_tested: vec![PathBuf::from("/work/other")],
            namespaces_tested: vec!["ocx.sh/other".to_owned()],
        }));
        let default = report.lines(&plain_theme(), Detail::Answer);
        let verbose = report.lines(&plain_theme(), Detail::Diagnostics);

        assert!(default[0].starts_with("ocx home:"), "{default:#?}");
        assert!(default[1].starts_with("project:"), "{default:#?}");
        for lead in ["active: no", "reason: ", "fix: "] {
            assert!(
                default.iter().any(|line| line.starts_with(lead)),
                "the default rendering must carry {lead:?}: {default:#?}"
            );
        }
        // Seventeen lines, counting the execution-state status: the resolved
        // toolchain home, the launcher directory and the two effective settings
        // are four lines the *answer* owes a user (a home a `toolchain_dir`
        // relocated is named nowhere else in the report, and the directory to
        // export is not one join from it), so the budget moves by exactly what
        // each contract added and by nothing else.
        assert!(
            default.len() <= 17,
            "the default rendering is the answer, not the state dump; got {} lines: {default:#?}",
            default.len()
        );

        // Each section the default drops, and the evidence rows inside them.
        for section in [
            "execution state:",
            "binary pin:",
            "fingerprint:",
            "hook:",
            "  watch set:",
            "  carrier:",
            "  bytes:",
        ] {
            assert!(
                verbose.iter().any(|line| line.starts_with(section)),
                "--verbose must still carry {section:?}: {verbose:#?}"
            );
            assert!(
                !default.iter().any(|line| line.starts_with(section)),
                "the default rendering must not carry {section:?}: {default:#?}"
            );
        }

        // And the answer leads at the verbose tier too - the reason is what
        // the reader came for, whichever tier they asked for.
        let reason_at = verbose
            .iter()
            .position(|line| line.starts_with("reason: "))
            .expect("the verbose rendering carries the reason");
        let state_at = verbose
            .iter()
            .position(|line| line == "execution state:")
            .expect("the verbose rendering carries the execution state");
        assert!(reason_at < state_at, "the answer must lead at both tiers: {verbose:#?}");
    }

    /// Every enumerated reason ends with the one line that says what to do —
    /// and so does the one refusal that is a [`Note`] rather than a [`Reason`],
    /// an unresolvable project file. Nothing that is not a refusal grows one.
    ///
    /// QUAL-3 puts `ProjectUnresolved` on the refusal side of that line: it is
    /// a refusal (nothing composed), and it is not a `Reason` only because no
    /// consent predicate ever ran over a project that never resolved. The two
    /// counts are summed rather than or'd, so a report carrying both — a broken
    /// `ocx.toml` in a first-prompt shell — is owed both fixes.
    #[test]
    fn every_inert_arm_names_a_fix_and_no_other_arm_does() {
        for (arm, report) in every_arm() {
            let lines = report.lines(&plain_theme(), Detail::Answer);
            let fixes = lines.iter().filter(|line| line.starts_with("fix: ")).count();
            // Three states owe a fix, in the order `activation_lines` matches
            // them: an enumerated reason, an unresolvable project file, and a
            // lock that refuses composition where consent did not. The third
            // is only owed when the two before it did not claim the arm —
            // `lock_refusal` renders nothing when a `Reason` already spoke.
            let lock_refused = report.inert_reason.is_none()
                && !report.project_unresolved()
                && report.project_scope_pending()
                && report.lock_refusal.is_some();
            let owed = usize::from(report.inert_reason.is_some())
                + usize::from(report.project_unresolved())
                + usize::from(lock_refused);
            assert_eq!(
                owed, fixes,
                "arm `{arm}`: a refusal without a fix is a dead end, and a fix without a refusal has nothing to fix: {lines:#?}"
            );
        }
    }

    /// Colour adds escapes and changes nothing else - which is what makes the
    /// redirected-to-a-file rendering and the terminal one the same report.
    ///
    /// The positive control matters: without it, a theme that painted nothing
    /// would satisfy the parity assertion on every arm, and this test would be
    /// green in exactly the state it exists to rule out.
    #[test]
    fn colour_changes_only_the_escapes_never_the_text() {
        let mut painted = 0_usize;
        for (arm, report) in every_arm() {
            for detail in [Detail::Answer, Detail::Diagnostics] {
                let plain = report.lines(&plain_theme(), detail);
                let coloured = report.lines(&colour_theme(), detail);
                assert_eq!(
                    plain.len(),
                    coloured.len(),
                    "arm `{arm}`: colour changed the line count"
                );
                for (bare, inked) in plain.iter().zip(coloured.iter()) {
                    if inked.contains('\u{1b}') {
                        painted += 1;
                    }
                    assert_eq!(
                        bare.as_str(),
                        &*console::strip_ansi_codes(inked),
                        "arm `{arm}`: colour changed the text, not just the escapes"
                    );
                }
            }
        }
        assert!(
            painted > 0,
            "no arm emitted a single escape sequence; the parity assertion above would pass over an unpainted report"
        );
    }

    /// The `--verbose` tier is an inked view, not a monochrome YAML dump.
    ///
    /// `colour_changes_only_the_escapes_never_the_text` above proves colour
    /// changes no text, and its `painted > 0` control proves *something* on
    /// *some* arm was painted. Neither can tell that the diagnostics block
    /// under the coloured answer is still entirely bare — which is the state
    /// this command shipped in, and the one the report of it named. Asserting
    /// per key means reverting a single `field` call site to a plain
    /// `format!` reds here and nowhere else.
    ///
    /// Each key is asserted twice: as bare text in the uncoloured rendering,
    /// so the list cannot rot into keys the report no longer emits and quietly
    /// stop testing anything, and in the coloured one as the exact token
    /// [`field`] builds.
    #[test]
    fn the_diagnostics_tier_inks_every_evidence_key() {
        let mut report = base(None);
        // The one field `base` leaves absent; every other key below is already
        // reachable from it.
        report.hook.tier = Some("managed config".to_owned());

        let plain = report.lines(&plain_theme(), Detail::Diagnostics).join("\n");
        let inked = report.lines(&colour_theme(), Detail::Diagnostics).join("\n");
        for key in [
            "key:",
            "consent stamp:",
            "carrier:",
            "present:",
            "bytes:",
            "status:",
            "major:",
            "narrowing:",
            "session claimed:",
            "inherited scopes:",
            "applied:",
            "replay:",
            "selection:",
            "priors:",
            "path:",
            "exists:",
            "matches running binary:",
            "matches watch set:",
            "watch set:",
            "enabled:",
            "deciding rung:",
            "deciding tier:",
        ] {
            assert!(
                plain.contains(key),
                "the diagnostics tier no longer emits `{key}`, so the ink assertion below would pass over its absence"
            );
            assert!(
                inked.contains(&colour_theme().aside(key)),
                "`{key}` is not inked; the diagnostics tier reads as a YAML dump:\n{inked}"
            );
        }
        // The values carry the semantics, not just the keys: a verdict whose
        // negative is already alerted must have a positive that is not bare.
        assert!(
            inked.contains(&colour_theme().ok("intact")),
            "an intact prior is not inked, while a MISSING one is alerted:\n{inked}"
        );
    }

    /// **The machine contract.** `--verbose` is a plain-rendering tier, and the
    /// structured report is complete at both - a `--format json` consumer never
    /// sees less because a human flag was absent.
    #[test]
    fn the_structured_report_is_complete_at_both_tiers() {
        // Every optional set, since an unset optional is absent from the document by contract.
        let complete = || ShellStateReport {
            fingerprint_current: Some(true),
            ..base(Some(Reason::LockUnavailable))
        };
        let bare = serde_json::to_value(complete()).expect("the report serializes");
        let verbose = serde_json::to_value(VerboseShellState(complete())).expect("the verbose wrapper serializes");
        assert_eq!(bare, verbose, "`--verbose` must not change the structured payload");

        // Every field the default *rendering* drops is still in the document.
        for key in [
            "env_state",
            "binary_pin",
            "watch_set",
            "carrier_present",
            "carrier_bytes",
            "priors",
            "hook",
            "project_key",
            "project_stamped",
            "fingerprint_current",
        ] {
            assert!(
                bare.get(key).is_some(),
                "the structured report must carry `{key}`: {bare}"
            );
        }
        assert!(
            bare.get("ledger").is_none(),
            "the retired ledger field must not reappear: {bare}"
        );
        assert!(bare["env_state"].is_object(), "the state must be a payload, not a null");
        assert!(
            bare["watch_set"].as_array().is_some_and(|set| !set.is_empty()),
            "the watch set must survive into the structured report: {bare}"
        );

        // The other half, so the assertion pair discriminates: those same facts
        // are genuinely gone from the human default.
        let default = answer(&base(Some(Reason::LockUnavailable)));
        // `key: <hex>`, not the bare hex: C-056 puts the resolved toolchain
        // home in the default rendering, and a `toolchain_dir`-relocated home
        // is keyed by that same 16 hex characters (`<root>/<key>/toolchain`).
        // The row this assertion is about is the verbose `key:` field, and
        // anchoring on the label is what keeps it about that row.
        for needle in ["watch set", "carrier:", "bytes:", "key: 0123456789abcdef", "modified"] {
            assert!(
                !default.contains(needle),
                "the default rendering must not carry {needle:?}: {default}"
            );
        }
    }

    /// **Finding 1.** An activated project names the clause that activated it,
    /// and a stamp grant names the file's own recorded instant plus the
    /// command that removes it.
    ///
    /// The three clauses are asserted together because the whole defect was
    /// that they were indistinguishable: a report that said `active: yes` for
    /// all three let a stamp masquerade as the `[shell.consent]` entry the
    /// user had actually written down.
    ///
    /// Red state: drop the `if let Some(grant)` push in `activation_lines` and
    /// every assertion below fails; return a constant from `grant_description`
    /// and the three arms stop discriminating.
    #[test]
    fn f001_an_active_project_names_the_clause_that_granted_it() {
        let stamped = answer(&base(None));
        assert!(
            stamped.contains("granted by: a consent stamp"),
            "a stamp grant must say so: {stamped}"
        );
        assert!(
            stamped.contains("2026-08-27 07:12:03 UTC"),
            "a stamp grant must carry the instant the stamp records: {stamped}"
        );
        assert!(
            stamped.contains("ocx shell revoke"),
            "an invisible grant with no revoke gesture is the defect: {stamped}"
        );

        let mut path_granted = base(None);
        path_granted.grant = Some(Grant::Path);
        path_granted.project_stamped = false;
        path_granted.stamp_written_at = None;
        let rendered = answer(&path_granted);
        assert!(
            rendered.contains("granted by: [shell.consent] paths"),
            "a paths grant names the config key: {rendered}"
        );
        assert!(
            !rendered.contains("consent stamp"),
            "a paths grant must not be reported as a stamp: {rendered}"
        );

        let mut namespace_granted = base(None);
        namespace_granted.grant = Some(Grant::Namespace);
        let rendered = answer(&namespace_granted);
        assert!(
            rendered.contains("granted by: [shell.consent] namespaces"),
            "a namespaces grant names the config key: {rendered}"
        );

        // The refusal half: an inert project has no grant to name, so the line
        // is absent rather than saying "none".
        let inert = answer(&base(Some(Reason::LockUnavailable)));
        assert!(
            !inert.contains("granted by"),
            "an inert project was granted by nothing: {inert}"
        );
    }

    /// The two new keys are additive on the `--format json` contract: present
    /// in the document at both detail tiers, like every other field.
    #[test]
    fn the_grant_and_its_provenance_reach_the_structured_report() {
        let active = serde_json::to_value(base(None)).expect("the report serializes");
        assert_eq!(active["grant"], serde_json::json!("stamp"));
        assert_eq!(active["stamp_written_at"], serde_json::json!("2026-08-27T07:12:03Z"));

        // Both are `null` rather than absent when inert, so a consumer can
        // index them unconditionally.
        let inert = serde_json::to_value(base(Some(Reason::LockUnavailable))).expect("the report serializes");
        assert!(inert.get("grant").is_none(), "an inert project names no grant: {inert}");
        assert!(inert.get("stamp_written_at").is_none(), "no stamp, no instant: {inert}");
    }

    /// The never-eval-able assertion's own red state. A green result is
    /// evidence only if a red one was reachable: this shows the assertion
    /// firing on exactly the injection the fault-injection run uses.
    #[test]
    #[should_panic(expected = "is valid shell source")]
    fn the_never_eval_able_assertion_can_go_red() {
        let mut lines = base(None).lines(&plain_theme(), Detail::Answer);
        lines.push("  export OCX_INJECTED=1".to_owned());
        assert_lines_never_eval_able("injected", &lines);
    }

    /// SEC-1's red state, locked in permanently: one `Vec` element carrying an
    /// embedded LF is two physical lines on stdout, and the assertion must see
    /// the second one. Before the split-on-newline fix this passed.
    #[test]
    #[should_panic(expected = "is valid shell source")]
    fn the_never_eval_able_assertion_sees_an_embedded_newline() {
        let mut lines = base(None).lines(&plain_theme(), Detail::Answer);
        lines.push("    dir: /work\nexport OCX_EVIL=2".to_owned());
        assert_lines_never_eval_able("embedded_newline", &lines);
    }

    /// The same red state **under colour**, which is where it could quietly
    /// stop being reachable: the theme's escape sits in front of the keyword,
    /// so an assertion reading the raw bytes would answer about `\x1b[` and
    /// pass on an injected `export` line forever.
    #[test]
    #[should_panic(expected = "is valid shell source")]
    fn the_never_eval_able_assertion_can_go_red_under_colour() {
        let theme = colour_theme();
        let mut lines = base(None).lines(&theme, Detail::Answer);
        lines.push(format!("  {}OCX_INJECTED=1", theme.label("export ")));
        assert_lines_never_eval_able("injected_under_colour", &lines);
    }

    /// And the bare-assignment half of it, which no emitter prefix covers.
    #[test]
    #[should_panic(expected = "is a bare shell assignment")]
    fn the_bare_assignment_assertion_can_go_red() {
        let mut lines = base(None).lines(&plain_theme(), Detail::Answer);
        lines.push("  OCX_INJECTED=1".to_owned());
        assert_lines_never_eval_able("injected", &lines);
    }

    // ── C-056 / RUL-51 — the resolved toolchain home and the two effective
    //    settings ──────────────────────────────────────────────────────────

    /// The fixture's relocated home, spelled once. Deliberately a
    /// `toolchain_dir`-style path rather than `<project>/.ocx/toolchain`, so a
    /// renderer that printed `project_dir` and called it a home would not
    /// accidentally satisfy the assertions below.
    fn fixture_toolchain_home() -> String {
        base(None).toolchain_home.display().to_string()
    }

    /// The report with its toolchain home replaced.
    fn with_home(home: &str) -> ShellStateReport {
        let mut report = base(None);
        report.toolchain_home = PathBuf::from(home);
        // The pair travels together: a fixture that moved one and left the
        // other would exercise a state the builder cannot produce.
        report.toolchain_bin = PathBuf::from(home).join("active").join("bin");
        report
    }

    /// **C-056 / RUL-51** — under `--format json` the resolved home is a
    /// contract field: **present in every reportable state, and never `null`**.
    ///
    /// Asserted over the whole arm corpus, because "always present" is a claim
    /// about every state the command can report, not about the happy one. A
    /// refused `toolchain_dir` cannot appear here at all — it exits 78 at config
    /// load — which is exactly why the field can be unconditional.
    ///
    /// Mutation that reds it: making the field `Option<PathBuf>`, the shape a
    /// reader who has not seen RUL-51 reaches for.
    #[test]
    fn the_json_report_always_carries_a_non_null_toolchain_home() {
        for (arm, report) in every_arm() {
            let value = serde_json::to_value(&report).expect("the report serializes");
            let home = value
                .get("toolchain_home")
                .unwrap_or_else(|| panic!("arm `{arm}`: `toolchain_home` must be a field of the JSON report"));
            assert!(!home.is_null(), "arm `{arm}`: RUL-51 — the resolved home is never null");
            let home = home
                .as_str()
                .unwrap_or_else(|| panic!("arm `{arm}`: `toolchain_home` must serialize as a path string, got {home}"));
            assert!(!home.is_empty(), "arm `{arm}`: an empty home names nothing");
        }
    }

    /// **G-1 / S-004** — `toolchain_bin` is a contract field on the same terms
    /// as `toolchain_home`: present in every reportable state, never `null`,
    /// and never equal to the home.
    ///
    /// The last clause is the one that matters. A field that answered the home
    /// itself, or that a consumer could reconstruct by appending `/bin`, would
    /// leave every published recipe exactly as broken as the concatenation this
    /// field replaces — and both would still exit 0.
    ///
    /// Mutation that reds it: populate it as `toolchain_home.join("bin")`, or
    /// make it `Option<PathBuf>`.
    #[test]
    fn the_json_report_always_carries_a_launcher_directory_the_home_does_not_yield() {
        for (arm, report) in every_arm() {
            let value = serde_json::to_value(&report).expect("the report serializes");
            let bin = value
                .get("toolchain_bin")
                .unwrap_or_else(|| panic!("arm `{arm}`: `toolchain_bin` must be a field of the JSON report"));
            assert!(
                !bin.is_null(),
                "arm `{arm}`: G-1 — the launcher directory is present whenever the home is, so no \
                 consumer has to branch"
            );
            let bin = bin
                .as_str()
                .unwrap_or_else(|| panic!("arm `{arm}`: `toolchain_bin` must serialize as a path string, got {bin}"));
            let home = value
                .get("toolchain_home")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("arm `{arm}`: the home is the field this one is paired with"));

            assert!(
                !bin.is_empty(),
                "arm `{arm}`: an empty launcher directory names nothing"
            );
            assert_ne!(bin, home, "arm `{arm}`: the launcher directory is not the home");
            assert_ne!(
                bin,
                format!("{home}/bin"),
                "arm `{arm}`: G-1 exists because `.toolchain_home + \"/bin\"` is not the answer; a field \
                 that reproduced it would leave every published recipe broken and still exit 0"
            );
            assert!(
                bin.starts_with(home),
                "arm `{arm}`: the launcher directory lives inside the home it belongs to; got {bin} \
                 under {home}"
            );
        }
    }

    /// C-056 — the two **effective** settings travel with it, also on every arm.
    #[test]
    fn the_json_report_always_carries_the_effective_activate_and_pinned() {
        for (arm, report) in every_arm() {
            let value = serde_json::to_value(&report).expect("the report serializes");
            let activate = value
                .get("activate")
                .unwrap_or_else(|| panic!("arm `{arm}`: `activate` must be a field of the JSON report"));
            assert!(
                ["env", "bin", "none"].contains(&activate.as_str().unwrap_or_default()),
                "arm `{arm}`: `activate` must be one of the three wire values, got {activate}"
            );
            let pinned = value
                .get("pinned")
                .unwrap_or_else(|| panic!("arm `{arm}`: `pinned` must be a field of the JSON report"));
            assert!(
                pinned.is_boolean(),
                "arm `{arm}`: `pinned` is the resolved boolean, not a tier, got {pinned}"
            );
        }
    }

    /// **C-056** — the human rendering names the resolved home at **both**
    /// verbosity tiers.
    ///
    /// Both, because `--verbose` is a rendering tier and not a payload: a user
    /// who has to pass a flag to learn where their toolchain lives has been told
    /// the answer is a diagnostic.
    ///
    /// Mutation that reds it: adding the field to the struct and not to
    /// `summary_lines` — the exact half-landed state this case exists to
    /// forbid, and the one the wire-only assertions above cannot see.
    #[test]
    fn both_verbosity_tiers_name_the_resolved_toolchain_home() {
        let report = base(None);
        let home = fixture_toolchain_home();
        assert!(
            answer(&report).contains(&home),
            "C-056 — the default rendering must name the resolved home ({home}):\n{}",
            answer(&report)
        );
        assert!(
            diagnostics(&report).contains(&home),
            "C-056 — the --verbose rendering must name it too ({home}):\n{}",
            diagnostics(&report)
        );
    }

    /// **G-1** — the human rendering names the launcher directory at **both**
    /// verbosity tiers, and names it as something the home does not already say.
    ///
    /// The default tier especially: the reader who is about to export a
    /// directory by hand is exactly the reader this field exists for, and the
    /// answer they used to reach for was `<home>/bin`.
    ///
    /// Mutation that reds it: drop the `bin` line from `toolchain_lines` — the
    /// half-landed state where the wire field ships and the text does not, which
    /// the JSON assertions above cannot see.
    #[test]
    fn both_verbosity_tiers_name_the_launcher_directory() {
        let report = base(None);
        let bin = report.toolchain_bin.display().to_string();
        assert_ne!(
            bin,
            fixture_toolchain_home(),
            "the fixture must differ from the home, or naming one would satisfy both assertions"
        );
        assert!(
            answer(&report).contains(&bin),
            "G-1 — the default rendering must name the launcher directory ({bin}):\n{}",
            answer(&report)
        );
        assert!(
            diagnostics(&report).contains(&bin),
            "G-1 — the --verbose rendering must name it too ({bin}):\n{}",
            diagnostics(&report)
        );
    }

    /// C-056 — the human rendering reports the **effective** `activate` and
    /// `pinned`, not a tier's raw value.
    ///
    /// Asserted by discrimination rather than by a format guess: three reports
    /// differing only in `activate` must render three different texts, and two
    /// differing only in `pinned` must differ too. A renderer that omitted
    /// either would produce identical output for inputs the user needs told
    /// apart — which is the whole complaint the command answers.
    #[test]
    fn both_effective_settings_are_visible_in_the_default_rendering() {
        let mut renderings = std::collections::BTreeSet::new();
        for mode in [
            ocx_project::activate::ActivateMode::Env,
            ocx_project::activate::ActivateMode::Bin,
            ocx_project::activate::ActivateMode::None,
        ] {
            let mut report = base(None);
            report.activate = mode;
            let rendered = answer(&report);
            assert!(
                rendered.contains(&mode.to_string()),
                "C-056 — the rendering must carry the effective mode's wire spelling `{mode}`:\n{rendered}"
            );
            renderings.insert(rendered);
        }
        assert_eq!(
            renderings.len(),
            3,
            "C-056 — three different effective modes must render three different reports"
        );

        let mut following = base(None);
        following.pinned = false;
        let mut pinned = base(None);
        pinned.pinned = true;
        assert_ne!(
            answer(&following),
            answer(&pinned),
            "C-056 — the effective `pinned` must be visible; two reports differing only in it must differ"
        );
    }

    /// C-056 — the resolved home never turns the report into shell source.
    ///
    /// A `toolchain_dir` is a user-supplied path, and on POSIX a path may carry
    /// a newline. Interpolated bare, `<root>` = `"/h/t\nexport OCX_HOME=/tmp/x"`
    /// would put a line into `ocx shell state`'s output that a careless
    /// `eval "$(ocx shell state)"` executes — the injection the whole
    /// never-eval-able corpus exists for.
    ///
    /// Two halves, and the second is what makes the first non-vacuous: the
    /// rendering must *carry* the home (so the corpus is judging a real
    /// rendering of it), and it must not gain a line from it.
    #[test]
    fn a_hostile_toolchain_home_cannot_forge_a_line() {
        let hostile = "/h/toolchains/x\nexport OCX_HOME=/tmp/evil\nalias ls='rm -rf /'";
        let report = with_home(hostile);

        // Non-vacuity: the home is rendered at all. Without this, a renderer
        // that simply ignores the field passes every assertion below.
        assert!(
            answer(&report).contains("toolchains"),
            "the hostile home must actually be rendered, or this case measures nothing:\n{}",
            answer(&report)
        );
        assert_eq!(
            answer(&report).lines().count(),
            answer(&with_home("/h/toolchains/x")).lines().count(),
            "CWE-117 — a home carrying newlines must not add lines to the report"
        );
        assert_never_eval_able("hostile toolchain home", &report);
        assert_not_interchangeable_with_activate("hostile toolchain home", &report);
    }

    // ── The execution state and the binary pin ────

    fn json_of(report: &ShellStateReport) -> serde_json::Value {
        serde_json::to_value(report).expect("the report serializes")
    }

    /// A readable state with every optional field populated.
    fn full_state() -> ExecState {
        ExecState {
            narrowing: Narrowing {
                offline: true,
                ..Narrowing::default()
            },
            home: Some(PathBuf::from("/home/u/.ocx")),
            minor: 3,
            policy: Policy {
                no_verify: Some(true),
                remote: Some(true),
                ..Policy::default()
            },
            inherited_scopes: vec![
                Scope::Global,
                Scope::Project {
                    root: PathBuf::from("/work/lib"),
                },
            ],
            selection: Selection::Pinned(Pinned {
                tier: PinnedTier::Project {
                    dir: PathBuf::from("/work/app"),
                },
                groups: vec!["default".to_owned()],
                with_names: Vec::new(),
                env_keys: vec!["CC".to_owned()],
                invocation_digest: "0".repeat(64),
                pinned_paths: None,
            }),
            session: ocx_config::exec_state::SessionCache {
                token: Some(SessionToken { pid: 7, started: None }),
                ..Default::default()
            },
            truncated: Truncation::InheritedDropped,
            ..state_with_project()
        }
    }

    #[test]
    fn c029_a_readable_state_reports_every_field() {
        let mut report = base(None);
        set_state(&mut report, full_state());
        let state = json_of(&report)["env_state"].clone();

        assert_eq!(state["status"], "readable");
        assert_eq!(state["major"], 2);
        assert_eq!(state["minor"], 3);
        assert_eq!(state["narrowing"], serde_json::json!(["offline"]));
        assert_eq!(state["home"], "/home/u/.ocx");
        assert_eq!(state["truncated"], "inherited_dropped");
        assert_eq!(state["policy"], serde_json::json!({"remote": true, "no_verify": true}));
        assert_eq!(state["relaxing_policy"], serde_json::json!(["home", "no_verify"]));
        assert_eq!(state["session_claimed"], true);
        assert_eq!(
            state["inherited_scopes"],
            serde_json::json!([{"type": "global"}, {"type": "project", "root": "/work/lib"}])
        );
        assert_eq!(state["applied"].as_array().map(Vec::len), Some(4));
        assert_eq!(
            state["replay"],
            serde_json::json!({"env_count": 0, "launch_identity_count": 0})
        );
        assert_eq!(
            state["selection"],
            serde_json::json!({
                "type": "pinned_project", "project_dir": "/work/app", "groups": ["default"], "env_keys": ["CC"]
            })
        );
        assert!(
            state.get("found_major").is_none(),
            "found_major is for unreadable states: {state}"
        );
    }

    #[test]
    fn c029_an_absent_state_reports_only_its_status() {
        let mut report = base(Some(Reason::StateUnreadable { first_prompt: true }));
        without_state(&mut report);
        assert_eq!(
            json_of(&report)["env_state"],
            serde_json::json!({"status": "absent", "major": 2, "session_claimed": false})
        );
    }

    #[test]
    fn c029_an_unreadable_state_keeps_the_header_and_names_the_refusal() {
        let (env_state, notes) = EnvStateReport::from_inherited(
            &Inherited::Unreadable {
                header: HeaderView {
                    major: Some(3),
                    narrowing: Narrowing {
                        frozen: true,
                        ..Narrowing::default()
                    },
                    home: Some(PathBuf::from("/h")),
                },
                reason: UnreadableReason::OtherMajor,
            },
            true,
        );
        let state = serde_json::to_value(&env_state).expect("serializes");
        assert_eq!(
            state,
            serde_json::json!({
                "status": "unreadable", "major": 2, "found_major": 3, "narrowing": ["frozen"],
                "home": "/h", "session_claimed": false
            })
        );
        assert_eq!(
            notes,
            vec![
                Note::EnvStateUnreadable {
                    major: 2,
                    found_major: Some(3),
                    reason: "other_major".to_owned(),
                    field: None
                },
                Note::EnvStateStoodDown
            ]
        );
        let (_, without_hook) = EnvStateReport::from_inherited(
            &Inherited::Unreadable {
                header: HeaderView::default(),
                reason: UnreadableReason::Json,
            },
            false,
        );
        assert_eq!(
            without_hook.len(),
            1,
            "the stood-down note needs an active hook: {without_hook:?}"
        );
    }

    #[test]
    fn c029_the_notes_are_tagged_and_exit_stays_zero_by_construction() {
        let (_, notes) = EnvStateReport::from_inherited(&Inherited::Readable(full_state()), false);
        let json = serde_json::to_value(&notes).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!([
                {"type": "env_state_truncated", "truncated": "inherited_dropped"},
                {"type": "relaxing_policy", "names": ["home", "no_verify"]}
            ])
        );
        let missing = BinaryPinReport {
            path: Some(PathBuf::from("/gone/ocx")),
            exists: false,
            matches_running: false,
        };
        assert_eq!(
            serde_json::to_value(missing.note()).expect("serializes"),
            serde_json::json!({"type": "binary_pin_missing", "path": "/gone/ocx"})
        );
        let (_, clean) = EnvStateReport::from_inherited(&Inherited::Readable(state_with_project()), false);
        assert!(clean.is_empty(), "nothing relaxing or truncated, no note: {clean:?}");
    }

    /// `relaxing_policy` reads the set fields through `field_classes`, so a relaxing field the
    /// report forgets to list is a silent omission; populating every field proves none is.
    #[test]
    fn c029_every_relaxing_field_is_reported_when_set() {
        let policy = PolicyReport {
            remote: Some(true),
            no_project: Some(true),
            no_config_refresh: Some(true),
            no_update_check: Some(true),
            config_path: Some(PathBuf::from("/c")),
            no_config: Some(true),
            index_dir: Some(PathBuf::from("/i")),
            toolchain_dir: Some(PathBuf::from("/t")),
            mirrors: Some("m".to_owned()),
            patches: Some("p".to_owned()),
            patch_snapshot_path: Some(PathBuf::from("/s")),
            managed_config: Some("g".to_owned()),
            no_verify: Some(true),
            records_dir: Some(PathBuf::from("/r")),
            records_name: Some("n".to_owned()),
            allow_yanked: Some(true),
            default_registry: Some("d".to_owned()),
            insecure_registries: Some("x".to_owned()),
        };
        let set: Vec<&str> = policy.set_values().into_iter().map(|(name, _)| name).collect();
        for (name, class) in field_classes() {
            if name != "home" {
                assert!(set.contains(&name), "`{name}` ({class:?}) is missing from the report");
            }
        }
    }

    #[test]
    fn c029_the_binary_pin_reports_path_existence_and_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pinned = dir.path().join("ocx");
        std::fs::write(&pinned, b"").expect("write");
        let other = dir.path().join("other");
        std::fs::write(&other, b"").expect("write");

        let same = BinaryPinReport::probe(Some(&pinned), Some(&pinned));
        assert!(same.exists && same.matches_running);
        let different = BinaryPinReport::probe(Some(&pinned), Some(&other));
        assert!(different.exists && !different.matches_running);
        let missing = BinaryPinReport::probe(Some(&dir.path().join("gone")), Some(&pinned));
        assert!(!missing.exists && !missing.matches_running);
        assert!(matches!(missing.note(), Some(Note::BinaryPinMissing { .. })));
        let unset = BinaryPinReport::probe(None, Some(&pinned));
        assert_eq!(
            serde_json::to_value(&unset).expect("serializes"),
            serde_json::json!({"exists": false, "matches_running": false}),
            "an unset pin omits `path`"
        );
        assert!(unset.note().is_none(), "no pin set is not a missing pin");
    }

    /// The strip set (ASCII controls, DEL, U+0080-U+009F including U+009B, the bidi controls) never reaches stdout from a
    /// state-derived string, at either tier. Red state: drop `display_safe` from `state_text` and every
    /// assertion below fails.
    #[test]
    fn c029_state_derived_strings_never_reach_stdout_with_a_control_character() {
        let hostile = |label: &str| format!("/{label}\u{1b}[31m\u{9b}31m\u{202e}rtl\u{2066}iso");
        let mut state = state_with_project();
        state.inherited_scopes.push(Scope::Project {
            root: PathBuf::from(hostile("scope")),
        });
        state.applied.push(record(
            "HOSTILE",
            &hostile("literal"),
            ValueKind::Constant,
            Owner::Inherited,
            None,
        ));
        state.selection = Selection::Pinned(Pinned {
            tier: PinnedTier::Project {
                dir: PathBuf::from(hostile("pinned")),
            },
            groups: vec![hostile("group")],
            with_names: Vec::new(),
            env_keys: Vec::new(),
            invocation_digest: "0".repeat(64),
            pinned_paths: None,
        });
        let mut report = base(None);
        set_state(&mut report, state);
        report.binary_pin = BinaryPinReport {
            path: Some(PathBuf::from(hostile("pin"))),
            exists: false,
            matches_running: false,
        };
        report.notes = report.binary_pin.note().into_iter().collect();

        for (tier, lines) in [
            ("answer", report.lines(&plain_theme(), Detail::Answer)),
            ("diagnostics", report.lines(&plain_theme(), Detail::Diagnostics)),
        ] {
            let text = lines.join("\n");
            // `quoted` would escape these to a backslash form; the contract is that they are gone.
            assert!(
                !text.contains("\\u{"),
                "{tier}: an escaped control character was printed:\n{text}"
            );
            for stripped in ['\u{1b}', '\u{9b}', '\u{202e}', '\u{2066}'] {
                assert!(
                    !text.contains(stripped),
                    "{tier}: U+{:04X} reached stdout:\n{text:?}",
                    stripped as u32
                );
            }
        }
        let verbose = report.lines(&plain_theme(), Detail::Diagnostics).join("\n");
        assert!(
            verbose.contains("scope"),
            "the sanitised root is still shown: {verbose}"
        );
        assert!(
            verbose.contains("literal"),
            "the sanitised literal is still shown: {verbose}"
        );
        assert!(
            verbose.contains("pinned"),
            "the sanitised pinned dir is still shown: {verbose}"
        );
    }

    #[test]
    fn c029_the_default_rendering_prints_the_status_and_each_note_with_its_fix() {
        let mut report = base(None);
        unreadable_state(&mut report);
        report.notes = EnvStateReport::from_inherited(
            &Inherited::Unreadable {
                header: HeaderView::default(),
                reason: UnreadableReason::Base64,
            },
            true,
        )
        .1;
        let text = answer(&report);
        assert!(text.contains("env state: unreadable"), "{text}");
        assert!(text.contains("reason: base64"), "{text}");
        assert!(text.contains("unset __OCX_ENV_STATE"), "{text}");
        assert!(text.contains("stood down"), "{text}");
    }

    #[test]
    fn c029_the_ledger_field_is_gone() {
        let json = json_of(&base(None));
        assert!(json.get("ledger").is_none(), "{json}");
        assert!(
            json["binary_pin"]["matches_running"].as_bool().unwrap_or(false),
            "{json}"
        );
    }
}
