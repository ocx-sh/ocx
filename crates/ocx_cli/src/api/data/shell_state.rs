// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx shell state` report (C-050).
//!
//! **Hard contract — the output is human-readable and must never be
//! eval-able.** `ocx self activate` and `--reconcile` emit shell source whose
//! entire purpose is to be `eval`'d; this command emits diagnostics whose
//! entire purpose is to be read. A surface where the two are confusable is one
//! copy-paste away from executing a diagnostic dump in a live shell. So: **no
//! line** of output is valid `export` / `set` / `$env.` syntax in **any** of
//! the ten arms, and a test asserts the two commands' outputs are never
//! interchangeable, for **every** enumerated inertness reason.
//!
//! # How the never-eval-able property is made structural, not incidental
//!
//! Two rules, both checkable by reading [`ShellStateReport::lines`]:
//!
//! 1. **Every line begins with a label from a fixed vocabulary this module
//!    owns** — a section heading, or two spaces then a label, or two spaces
//!    then the list marker `- `. No caller-, carrier- or filesystem-supplied
//!    byte is ever the first token of a line, so no line can *start* an
//!    assignment.
//! 2. **Every interpolated dynamic string passes through [`quoted`]**, which is
//!    `{:?}` — Rust's own escaping. That is the "quoted for a human, never for
//!    a shell" rule S-022 states, and it is what stops a ledger value carrying
//!    a literal LF from splitting itself into a second, unlabelled line. The
//!    carrier is untrusted input (C-007) and a forged one may hold any bytes at
//!    all in a key, a value, a `dir` or a source name.
//!
//! Rule 1 alone would be defeated by rule 2's hazard and vice versa; together
//! they leave no way for output to become shell source. The test derives the
//! forbidden prefixes from the shipped [`Shell`](ocx_lib::shell::Shell)
//! emitters rather than hard-coding them, so a new arm or a changed emitter
//! cannot silently widen what counts as "not shell source".

use std::path::{Path, PathBuf};

use ocx_lib::cli::DataInterface;
use ocx_lib::project::consent::Reason;
use ocx_lib::shell::coexistence::{Observation, Tool};
use ocx_lib::shell::reconcile::{CARRIER_KEY, Ledger, LedgerEntry, MAX_CARRIER_BYTES, Prior, ScopeId, Verdict};
use serde::Serialize;

use crate::api::Printable;

/// Render an untrusted string **quoted for a human, never for a shell**
/// (S-022).
///
/// `{:?}` escapes the two bytes that would otherwise break the report's
/// one-fact-per-line shape — LF and CR — along with `"` and `\`, and wraps the
/// result in quotes so the exact bytes are visible. It is deliberately *not* a
/// shell escaper: the point is that the result cannot be pasted into a shell
/// and mean anything.
fn quoted(text: &str) -> String {
    format!("{text:?}")
}

/// [`quoted`] over a path, which reaches us from the carrier, the environment
/// or the filesystem and is therefore subject to the same rule.
fn quoted_path(path: &Path) -> String {
    quoted(&path.display().to_string())
}

/// One member of the fingerprint watch set (C-019, A-13), as it stands on disk
/// right now.
///
/// Absent members are recorded too: a tier file that did not exist becoming
/// present is exactly the change the watch set must notice, which is why the
/// loader records candidates rather than survivors (A-13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchMember {
    /// The watched path.
    pub path: PathBuf,
    /// Whether it exists right now.
    pub present: bool,
    /// Size in bytes, when present.
    pub size: Option<u64>,
    /// Modification time as whole seconds since the Unix epoch, when present —
    /// the same granularity the per-prompt fast path compares.
    pub mtime: Option<u64>,
}

/// Whether the project scope still holds the prior for one constant it owns.
///
/// The priors are the one datum nothing can reconstruct (C-050), and the thing
/// C-012's `unset __OCX_ENV_STATE` repair gesture destroys — which is why this
/// is reported per constant rather than as a single yes/no.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PriorStatus {
    /// The constant's env key, as the carrier records it.
    pub key: String,
    /// Whether a [`Prior`] is still recorded for it.
    pub intact: bool,
}

/// The per-prompt hook's enablement, read from C-038's ladder rather than
/// re-derived.
///
/// `ocx shell state` declares no `--hook` / `--no-hook` pair of its own, so
/// rungs 1 and 2 are unreachable from here by construction and the answer comes
/// from rung 3 (`OCX_NO_HOOK`), rung 4 (`[shell] hook`) or rung 5 (auto).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookStatus {
    /// The deciding rung, rendered the way a user spells it.
    pub rung: String,
    /// The config tier that set it, when rung 4 decided (A-32 — the tier that
    /// **actually** decided, never a hard-coded "managed").
    pub tier: Option<String>,
    /// The resolved answer, or `None` on rung 5: "auto" is decided shell-side
    /// by the shim's interactivity probe, which a diagnostic cannot observe.
    pub enabled: Option<bool>,
}

/// A reason row that is not a member of [`Reason`] because it does not make the
/// shell inert on its own — it explains an answer the user would otherwise get
/// wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "note")]
pub enum Note {
    /// A-12 — the CWD walk skipped a symlinked `ocx.toml` candidate and
    /// promoted an ancestor. The loader's `log::warn!` never reaches the prompt
    /// (the hook discards the binary's stderr unconditionally, A-21), so this
    /// row is the user's only path to that answer.
    SymlinkedCandidateSkipped {
        /// The symlinked candidate that was skipped.
        candidate: PathBuf,
        /// The ancestor project activated in its place.
        ancestor: PathBuf,
    },
    /// A-26 — active via a `paths` grant, which is unconditional: source-set
    /// drift is **not** tracked for path grants, because nothing on the
    /// activation path writes a stamp to drift against.
    ActiveViaPathsGrant {
        /// The granting entry.
        entry: PathBuf,
    },
    /// QUAL-3 — a project file was reachable but could not be resolved (an
    /// unparseable `ocx.toml`, a canonicalization failure). Without this row
    /// the report says *"no project reachable from this directory"*, which is
    /// false for a user standing in a project whose config is broken — and this
    /// is the command whose product is the explanation.
    ProjectUnresolved {
        /// The resolution failure, rendered for a human.
        detail: String,
    },
    /// A-28 — a `paths` entry differing from the canonical directory only by
    /// ASCII case. Entries are compared as literal bytes after separator and
    /// trailing-slash normalization, so this is `Inert`; the row exists to pay
    /// off the support cost of that decision.
    PathsNearMiss {
        /// The entry that nearly matched.
        entry: PathBuf,
        /// The canonical project directory it was compared against.
        canonical: PathBuf,
    },
}

/// What `ocx shell state` reports — all derived, none of it mutating.
///
/// Read-only, absolutely: it never writes a stamp, never repairs a ledger,
/// never emits a plan (A-29 names it a non-member of the six-writer stamp
/// allowlist — a stamp written from here would consent to the very project it
/// is diagnosing). Repair is the `unset __OCX_ENV_STATE` gesture or a new
/// shell (C-012); this command is how a user checks the gesture worked.
#[derive(Debug, Serialize)]
pub struct ShellStateReport {
    /// `$OCX_HOME`, and whether it exists. A missing home is an ordinary state
    /// on a fresh install and exits 0; only a home that cannot be *read* is the
    /// single 74 path (C-051).
    pub ocx_home: PathBuf,
    /// Whether `$OCX_HOME` exists on disk.
    pub ocx_home_present: bool,

    /// Whether the `__OCX_ENV_STATE` carrier is set at all.
    ///
    /// This is what separates the two halves of C-050 reason 6: an absent
    /// carrier is the first prompt of a shell (nothing applied, nothing to
    /// repair); a carrier that is present and undecodable is a corrupt one (a
    /// scope was applied and its record is gone).
    pub carrier_present: bool,

    /// The carrier's encoded length in bytes, against [`MAX_CARRIER_BYTES`].
    pub carrier_bytes: usize,

    /// The decoded ledger, **rendered as fields, never as base64** — envelope
    /// tag, schema `v`, and the payload. `None` when the carrier is absent or
    /// [`Ledger::decode`] refused it (C-003, C-006).
    ///
    /// Carries what is applied per scope (`global` and `project` separately),
    /// whether `priors` are intact for each constant the project scope owns —
    /// the one datum nothing can reconstruct — and, through `over_cap`, the
    /// abandoned-scope marker (A-01: read from the marker, never inferred from
    /// an absent carrier).
    pub ledger: Option<Ledger>,

    /// Whether the ledger's recorded `fp` still matches the watch set as it
    /// stands on disk right now (C-019). `None` when there is no recorded
    /// fingerprint to compare against, **and** when no fold is available to
    /// compare with: the fingerprint is the reconciler's
    /// ([`ocx_lib::shell::reconcile::fingerprint`], which folds the raw
    /// `OCX_CONSENT_*` values, the recorded config-tier paths and the project's
    /// consent stamp — A-13), and a second fold defined here would produce a
    /// different string and report every fresh shell as stale.
    pub fingerprint_current: Option<bool>,

    /// The watch set, with each member's presence, size and mtime (C-050).
    pub watch_set: Vec<WatchMember>,

    /// The project the CWD walk resolved, canonicalized (A-30), and its
    /// 16-hex state key.
    pub project_dir: Option<PathBuf>,
    /// `ReferenceManager::name_for_path` of [`Self::project_dir`].
    pub project_key: Option<String>,
    /// Whether a usable consent stamp exists for that key (A-25: an unusable
    /// stamp is an absent stamp).
    pub project_stamped: bool,

    /// Prior intactness for each constant the ledger's project scope owns.
    pub priors: Vec<PriorStatus>,

    /// The hook's enablement and the rung that decided it (C-038).
    pub hook: HookStatus,

    /// Every coexisting tool observed live in this shell (C-049).
    ///
    /// A-37 — the two sentinels fire independently, so this is a list and the
    /// renderer prints **one line per observed tool**. [`Reason::YieldedTo`]
    /// carries a single [`Observation`], which is why the enumerated reason
    /// names the first and this field carries all of them.
    pub yielded_to: Vec<Observation>,

    /// Why the shell is not active, when it is not — **the command's reason to
    /// exist**. `None` when the project is active.
    pub inert_reason: Option<Reason>,

    /// Reason rows that explain an answer without being an inertness verdict of
    /// their own (A-12, A-26, A-28).
    pub notes: Vec<Note>,
}

impl ShellStateReport {
    /// Prior intactness for each constant the ledger's project scope owns.
    ///
    /// A constant with no recorded prior cannot be reverted: C-006 forbids
    /// guess-unsetting one, and "restore the recorded prior" has no operand
    /// without it.
    #[must_use]
    pub fn priors_for(ledger: Option<&Ledger>) -> Vec<PriorStatus> {
        let Some(project) = ledger.and_then(|ledger| ledger.scopes.project.as_ref()) else {
            return Vec::new();
        };
        project
            .applied
            .iter()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    ocx_lib::package::metadata::env::modifier::ModifierKind::Constant
                )
            })
            .map(|entry| PriorStatus {
                key: entry.key.clone(),
                intact: project.priors.contains_key(&entry.key),
            })
            .collect()
    }

    /// The whole report as the lines [`Printable::print_plain`] prints.
    ///
    /// Separated from the printing so the never-eval-able assertions can run
    /// over the exact bytes a user sees without capturing stdout.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!("ocx home: {}", quoted_path(&self.ocx_home)));
        out.push(format!(
            "  exists: {}",
            if self.ocx_home_present { "yes" } else { "no" }
        ));
        out.push(String::new());

        self.ledger_lines(&mut out);
        self.fingerprint_lines(&mut out);
        self.project_lines(&mut out);
        self.hook_lines(&mut out);
        self.activation_lines(&mut out);
        out
    }

    /// The decoded ledger, as fields — never as the base64 the carrier holds.
    fn ledger_lines(&self, out: &mut Vec<String>) {
        out.push("ledger:".to_owned());
        out.push(format!("  carrier: {}", quoted(CARRIER_KEY)));
        out.push(format!(
            "  present: {}",
            if self.carrier_present { "yes" } else { "no" }
        ));
        // A-38 — the cap bounds ocx's own contribution only. The combined
        // argv+envp size is an OS boundary: an E2BIG on execve degrades through
        // the ordinary spawn-failure path (74), with no ocx-side accounting.
        out.push(format!(
            "  bytes: {} of {} (ocx cap; total env-block size is an OS limit, not accounted here)",
            self.carrier_bytes, MAX_CARRIER_BYTES
        ));

        let Some(ledger) = self.ledger.as_ref() else {
            out.push(format!(
                "  decoded: no ({})",
                if self.carrier_present {
                    "truncated, or an unrecognised envelope tag"
                } else {
                    "the carrier is unset"
                }
            ));
            out.push(String::new());
            return;
        };

        out.push("  decoded: yes".to_owned());
        out.push("  envelope: 1".to_owned());
        out.push(format!("  schema v: {}", ledger.v));
        out.push(format!("  fingerprint: {}", quoted(&ledger.fp)));
        out.push(format!(
            "  verdict: {}",
            match ledger.verdict {
                Some(Verdict::Inert) => "inert",
                Some(Verdict::NoProject) => "no project",
                Some(Verdict::Activate) => "activate",
                None => "none recorded",
            }
        ));
        out.push(format!(
            "  over cap: {}",
            if ledger.over_cap.is_empty() {
                "none".to_owned()
            } else {
                ledger.over_cap.iter().map(scope_name).collect::<Vec<_>>().join(", ")
            }
        ));

        out.push("  applied, global scope:".to_owned());
        match ledger.scopes.global.as_ref() {
            Some(applied) if !applied.is_empty() => push_entries(out, applied),
            Some(_) => out.push("    - nothing".to_owned()),
            None => out.push("    - no record".to_owned()),
        }

        out.push("  applied, project scope:".to_owned());
        match ledger.scopes.project.as_ref() {
            Some(project) => {
                out.push(format!("    key: {}", quoted(&project.key)));
                out.push(format!("    dir: {}", quoted_path(&project.dir)));
                if project.applied.is_empty() {
                    out.push("    - nothing".to_owned());
                } else {
                    push_entries(out, &project.applied);
                }
                out.push("    priors:".to_owned());
                if self.priors.is_empty() {
                    out.push("      - no constants owned".to_owned());
                }
                for prior in &self.priors {
                    let recorded = match project.priors.get(&prior.key) {
                        Some(Prior::Unset) => "was unset".to_owned(),
                        Some(Prior::Value(value)) => format!("was {}", quoted(value)),
                        None => "lost".to_owned(),
                    };
                    out.push(format!(
                        "      - {} {} ({recorded})",
                        quoted(&prior.key),
                        if prior.intact { "intact" } else { "MISSING" }
                    ));
                }
            }
            None => out.push("    - no record".to_owned()),
        }
        out.push(String::new());
    }

    fn fingerprint_lines(&self, out: &mut Vec<String>) {
        out.push("fingerprint:".to_owned());
        out.push(format!(
            "  matches watch set: {}",
            match self.fingerprint_current {
                Some(true) => "yes",
                Some(false) => "no",
                None => "not compared",
            }
        ));
        out.push("  watch set:".to_owned());
        if self.watch_set.is_empty() {
            out.push("    - empty".to_owned());
        }
        for member in &self.watch_set {
            let detail = match (member.size, member.mtime) {
                (Some(size), Some(mtime)) => format!("present, {size} bytes, mtime {mtime}"),
                (Some(size), None) => format!("present, {size} bytes"),
                _ if member.present => "present".to_owned(),
                _ => "absent".to_owned(),
            };
            out.push(format!("    - {} ({detail})", quoted_path(&member.path)));
        }
        out.push(String::new());
    }

    fn project_lines(&self, out: &mut Vec<String>) {
        out.push("project:".to_owned());
        match self.project_dir.as_ref() {
            Some(dir) => {
                out.push(format!("  dir: {}", quoted_path(dir)));
                out.push(format!(
                    "  key: {}",
                    self.project_key.as_deref().map_or_else(|| "none".to_owned(), quoted)
                ));
                out.push(format!(
                    "  consent stamp: {}",
                    if self.project_stamped { "present" } else { "absent" }
                ));
            }
            None => out.push("  dir: none reachable from this directory".to_owned()),
        }
        out.push(String::new());
    }

    fn hook_lines(&self, out: &mut Vec<String>) {
        out.push("hook:".to_owned());
        out.push(format!(
            "  enabled: {}",
            match self.hook.enabled {
                Some(true) => "yes",
                Some(false) => "no",
                None => "auto (decided per shell at startup)",
            }
        ));
        out.push(format!("  deciding rung: {}", quoted(&self.hook.rung)));
        if let Some(tier) = self.hook.tier.as_deref() {
            out.push(format!("  deciding tier: {}", quoted(tier)));
        }
        out.push(String::new());
    }

    /// Whether a consented project scope is still waiting to be applied.
    ///
    /// True only on a ledger that **decoded**: an absent or corrupt carrier is
    /// [`Reason::LedgerUnreadable`]'s business, and a scope named in `over_cap`
    /// is [`Reason::LedgerOverCap`]'s.
    fn project_scope_pending(&self) -> bool {
        let Some(ledger) = self.ledger.as_ref() else {
            return false;
        };
        self.project_dir.is_some() && ledger.scopes.project.is_none() && !ledger.over_cap.contains(&ScopeId::Project)
    }

    /// The enumerated inertness reason — the command's reason to exist — plus
    /// the notes that explain an answer without being a verdict.
    fn activation_lines(&self, out: &mut Vec<String>) {
        out.push("activation:".to_owned());
        match self.inert_reason.as_ref() {
            Some(reason) => {
                out.push("  active: no".to_owned());
                self.reason_lines(reason, out);
            }
            // A project resolved, consent did not refuse, and the ledger
            // decoded but holds no project-scope record: the scope simply has
            // not been applied in this shell yet. Saying `yes` here would be as
            // untrue as calling a decoded carrier "unset" — and C-050 reason 6
            // enumerates exactly two carrier situations, so this third state
            // gets its own sentence rather than borrowing one of theirs.
            None if self.project_scope_pending() => {
                out.push("  active: not yet".to_owned());
                out.push("    the project scope is consented and not yet applied".to_owned());
                out.push("    the next prompt applies it; the carrier is intact".to_owned());
            }
            None => out.push("  active: yes".to_owned()),
        }
        for note in &self.notes {
            match note {
                Note::SymlinkedCandidateSkipped { candidate, ancestor } => {
                    out.push("  note: a symlinked ocx.toml candidate was skipped by the CWD walk".to_owned());
                    out.push(format!("    candidate: {}", quoted_path(candidate)));
                    out.push(format!("    activated instead: {}", quoted_path(ancestor)));
                    out.push("    opt in with: --project, or the OCX_PROJECT variable".to_owned());
                }
                Note::ActiveViaPathsGrant { entry } => {
                    out.push("  note: active via a paths grant".to_owned());
                    out.push(format!("    entry: {}", quoted_path(entry)));
                    out.push("    source-set drift is not tracked for path grants".to_owned());
                }
                Note::ProjectUnresolved { detail } => {
                    out.push("  note: a project file is reachable but could not be resolved".to_owned());
                    out.push(format!("    detail: {}", quoted(detail)));
                }
                Note::PathsNearMiss { entry, canonical } => {
                    out.push("  note: a paths entry differs only by ASCII case".to_owned());
                    out.push(format!("    entry: {}", quoted_path(entry)));
                    out.push(format!("    canonical dir: {}", quoted_path(canonical)));
                    out.push("    entries are compared as literal bytes, so this does not grant".to_owned());
                }
            }
        }
    }

    /// One arm per enumerated reason (C-050), each naming its own evidence.
    fn reason_lines(&self, reason: &Reason, out: &mut Vec<String>) {
        match reason {
            Reason::NoStampNoGrant {
                derived_sources,
                paths_tested,
                namespaces_tested,
            } => {
                out.push("  reason: no consent stamp, and no matching grant".to_owned());
                push_list(out, "derived sources", derived_sources.iter().map(String::as_str));
                push_list(
                    out,
                    "paths tested",
                    paths_tested.iter().map(|path| path.display().to_string()),
                );
                push_list(out, "namespaces tested", namespaces_tested.iter().map(String::as_str));
            }
            Reason::SourceSetDrift { new_sources } => {
                out.push("  reason: the lock's source set is not a subset of the stamp".to_owned());
                push_list(out, "new sources", new_sources.iter().map(String::as_str));
            }
            Reason::UncorroboratedNamespace {
                claimed_sources,
                verified_sources,
            } => {
                out.push("  reason: the namespace grant matches the lock's claim, not the store's record".to_owned());
                push_list(out, "claimed sources", claimed_sources.iter().map(String::as_str));
                match verified_sources {
                    // A `Some` that disagrees is the security-relevant half: a
                    // locked digest in the store came from a repository outside
                    // the granted namespace, and the lock renamed it.
                    Some(verified) => push_list(out, "verified sources", verified.iter().map(String::as_str)),
                    None => {
                        out.push("    verified sources: none recorded for this lock".to_owned());
                        out.push("    run `ocx pull` here once to record where each tool came from".to_owned());
                    }
                }
            }
            Reason::HookDisabled { rung, tier } => {
                out.push("  reason: the per-prompt hook is disabled".to_owned());
                out.push(format!("    deciding rung: {}", quoted(rung)));
                out.push(format!(
                    "    deciding tier: {}",
                    tier.as_deref().map_or_else(|| "not a config tier".to_owned(), quoted)
                ));
            }
            Reason::YieldedTo(first) => {
                out.push("  reason: yielded to another live per-prompt hook".to_owned());
                // A-37 — both sentinels fire independently, so this is one line
                // per observed tool, never an `elif` chain that suppresses the
                // second. `Reason::YieldedTo` carries only the first.
                let observed = if self.yielded_to.is_empty() {
                    std::slice::from_ref(first)
                } else {
                    self.yielded_to.as_slice()
                };
                for observation in observed {
                    out.push(format!(
                        "    live: {} (signal {})",
                        tool_name(observation.tool),
                        quoted(&observation.signal)
                    ));
                }
            }
            Reason::LedgerOverCap { scope } => {
                out.push("  reason: the ledger exceeded its cap and a scope was abandoned".to_owned());
                out.push(format!("    abandoned scope: {}", scope_name(scope)));
                out.push("    read from the over_cap marker the carrier still holds".to_owned());
            }
            Reason::LedgerUnreadable { first_prompt } => {
                if *first_prompt {
                    out.push("  reason: nothing has been applied in this shell yet".to_owned());
                    out.push("    the carrier is unset: this is the first prompt, not a fault".to_owned());
                } else {
                    out.push("  reason: the carrier is present but unreadable".to_owned());
                    out.push("    a scope was applied and its record is gone".to_owned());
                    out.push("    repair with: a new shell, or unset the carrier (which loses the priors)".to_owned());
                }
            }
            Reason::LockUnavailable => {
                out.push("  reason: ocx.lock is absent, unreadable or unparseable".to_owned());
                out.push("    the source-set predicate has nothing to quantify over".to_owned());
            }
        }
    }
}

/// One ledger entry per line, under the fixed `- ` list marker so no carrier
/// byte is ever the first token of a line.
fn push_entries(out: &mut Vec<String>, applied: &[LedgerEntry]) {
    for entry in applied {
        let separator = entry
            .separator
            .as_deref()
            .map_or_else(String::new, |sep| format!(" sep {}", quoted(sep)));
        out.push(format!(
            "    - {} {} {}{separator}",
            quoted(&entry.key),
            entry.kind,
            quoted(&entry.value)
        ));
    }
}

fn push_list(out: &mut Vec<String>, label: &str, items: impl IntoIterator<Item = impl AsRef<str>>) {
    out.push(format!("    {label}:"));
    let mut empty = true;
    for item in items {
        empty = false;
        out.push(format!("      - {}", quoted(item.as_ref())));
    }
    if empty {
        out.push("      - none".to_owned());
    }
}

fn scope_name(scope: &ScopeId) -> &'static str {
    match scope {
        ScopeId::Global => "global",
        ScopeId::Project => "project",
    }
}

fn tool_name(tool: Tool) -> &'static str {
    match tool {
        Tool::Direnv => "direnv",
        Tool::Mise => "mise",
    }
}

impl Printable for ShellStateReport {
    /// Human-readable, never-eval-able rendering of every enumerated reason
    /// (C-050).
    fn print_plain(&self, _data: &DataInterface) {
        for line in self.lines() {
            println!("{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use ocx_lib::package::metadata::env::modifier::ModifierKind;
    use ocx_lib::shell::Shell;
    use ocx_lib::shell::reconcile::{ProjectScope, Scopes};

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
        for line in lines.iter().flat_map(|line| line.split(['\n', '\r'])) {
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

    fn assert_never_eval_able(arm: &str, report: &ShellStateReport) {
        assert_lines_never_eval_able(arm, &report.lines());
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

        for line in report.lines().iter().flat_map(|line| line.split(['\n', '\r'])) {
            assert!(
                !emitted.contains(line.trim()),
                "arm `{arm}`: report line is byte-identical to emitted shell source: {line:?}"
            );
        }
    }

    fn ledger_with_project() -> Ledger {
        Ledger {
            v: 1,
            tiers: Vec::new(),
            fp: "a1b2c3d4".to_owned(),
            verdict: None,
            over_cap: Vec::new(),
            scopes: Scopes {
                global: Some(vec![LedgerEntry {
                    key: "OCX_HOME".to_owned(),
                    value: "/home/u/.ocx".to_owned(),
                    kind: ModifierKind::Constant,
                    separator: None,
                }]),
                global_priors: Default::default(),
                project: Some(ProjectScope {
                    key: "0123456789abcdef".to_owned(),
                    dir: PathBuf::from("/work/proj"),
                    applied: vec![
                        LedgerEntry {
                            key: "JAVA_HOME".to_owned(),
                            value: "/work/proj/.ocx/jdk".to_owned(),
                            kind: ModifierKind::Constant,
                            separator: None,
                        },
                        LedgerEntry {
                            key: "MAVEN_OPTS".to_owned(),
                            value: "-Xmx1g".to_owned(),
                            kind: ModifierKind::Constant,
                            separator: None,
                        },
                        LedgerEntry {
                            key: "PATH".to_owned(),
                            value: "/work/proj/.ocx/bin".to_owned(),
                            kind: ModifierKind::Path,
                            separator: None,
                        },
                    ],
                    priors: BTreeMap::from([("JAVA_HOME".to_owned(), Prior::Unset)]),
                }),
            },
        }
    }

    fn base(reason: Option<Reason>) -> ShellStateReport {
        let ledger = ledger_with_project();
        let priors = ShellStateReport::priors_for(Some(&ledger));
        ShellStateReport {
            ocx_home: PathBuf::from("/home/u/.ocx"),
            ocx_home_present: true,
            carrier_present: true,
            carrier_bytes: 412,
            ledger: Some(ledger),
            fingerprint_current: None,
            watch_set: vec![
                WatchMember {
                    path: PathBuf::from("/etc/ocx/config.toml"),
                    present: false,
                    size: None,
                    mtime: None,
                },
                WatchMember {
                    path: PathBuf::from("/home/u/.ocx/config.toml"),
                    present: true,
                    size: Some(1290),
                    mtime: Some(1_756_000_000),
                },
            ],
            project_dir: Some(PathBuf::from("/work/proj")),
            project_key: Some("0123456789abcdef".to_owned()),
            project_stamped: false,
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

        let mut over_cap = base(Some(Reason::LedgerOverCap {
            scope: ScopeId::Project,
        }));
        if let Some(ledger) = over_cap.ledger.as_mut() {
            ledger.over_cap = vec![ScopeId::Project];
            ledger.scopes.project = None;
        }
        over_cap.priors = ShellStateReport::priors_for(over_cap.ledger.as_ref());
        arms.push(("ledger_over_cap", over_cap));

        let mut absent = base(Some(Reason::LedgerUnreadable { first_prompt: true }));
        absent.carrier_present = false;
        absent.carrier_bytes = 0;
        absent.ledger = None;
        absent.priors = Vec::new();
        arms.push(("ledger_absent_first_prompt", absent));

        let mut corrupt = base(Some(Reason::LedgerUnreadable { first_prompt: false }));
        corrupt.ledger = None;
        corrupt.priors = Vec::new();
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
        if let Some(ledger) = pending.ledger.as_mut() {
            ledger.scopes.project = None;
        }
        pending.priors = ShellStateReport::priors_for(pending.ledger.as_ref());
        arms.push(("project_scope_pending", pending));

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
            Reason::LedgerOverCap { .. } => "ledger_over_cap",
            Reason::LedgerUnreadable { .. } => "ledger_unreadable",
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
                "ledger_over_cap",
                "ledger_unreadable",
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
            ]),
            "every Note variant must be exercised by an arm in `every_arm()`"
        );

        // `LedgerUnreadable` is two contractual situations behind one variant
        // (C-006), so the tag set alone would not prove both are covered.
        let first_prompt: BTreeSet<bool> = arms
            .iter()
            .filter_map(|(_, report)| match report.inert_reason.as_ref() {
                Some(Reason::LedgerUnreadable { first_prompt }) => Some(*first_prompt),
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
        if let Some(project) = report.ledger.as_mut().and_then(|l| l.scopes.project.as_mut()) {
            project.applied.push(LedgerEntry {
                key: "$env.EVIL".to_owned(),
                value: "x\nexport OCX_EVIL=1\n".to_owned(),
                kind: ModifierKind::Constant,
                separator: None,
            });
            project.dir = PathBuf::from("/work\nexport OCX_EVIL=2");
        }
        report.priors = ShellStateReport::priors_for(report.ledger.as_ref());
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
        let text = report.lines().join("\n");
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
        let text = report.lines().join("\n");
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
            let text = report.lines().join("\n");
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
        let lines = report.lines();
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
        let mut report = base(Some(Reason::LedgerOverCap {
            scope: ScopeId::Project,
        }));
        if let Some(ledger) = report.ledger.as_mut() {
            ledger.over_cap = vec![ScopeId::Project];
            ledger.scopes.project = None;
        }
        report.priors = ShellStateReport::priors_for(report.ledger.as_ref());
        let text = report.lines().join("\n");
        assert!(text.contains("over cap: project"), "{text}");
        assert!(text.contains("abandoned scope: project"), "{text}");
        // The carrier is still decodable — that is the whole point of A-01.
        assert!(text.contains("decoded: yes"), "{text}");
    }

    /// C-050 reason 6 + C-006 — an absent carrier (first prompt) and a corrupt
    /// one are **different** reasons, not one.
    #[test]
    fn c050_c006_absent_and_corrupt_ledgers_read_differently() {
        let mut absent = base(Some(Reason::LedgerUnreadable { first_prompt: true }));
        absent.carrier_present = false;
        absent.ledger = None;
        absent.priors = Vec::new();
        let absent_text = absent.lines().join("\n");

        let mut corrupt = base(Some(Reason::LedgerUnreadable { first_prompt: false }));
        corrupt.ledger = None;
        corrupt.priors = Vec::new();
        let corrupt_text = corrupt.lines().join("\n");

        assert!(
            absent_text.contains("this is the first prompt, not a fault"),
            "{absent_text}"
        );
        assert!(absent_text.contains("the carrier is unset"), "{absent_text}");
        assert!(
            corrupt_text.contains("a scope was applied and its record is gone"),
            "{corrupt_text}"
        );
        assert!(
            corrupt_text.contains("truncated, or an unrecognised envelope tag"),
            "{corrupt_text}"
        );
        assert_ne!(absent_text, corrupt_text);
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
        let text = report.lines().join("\n");
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
        let text = report.lines().join("\n");
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
        let text = report.lines().join("\n");
        assert!(text.contains("/Users/u/Repo"), "{text}");
        assert!(text.contains("/Users/u/repo"), "{text}");
        assert!(text.contains("does not grant"), "{text}");
    }

    /// A-38 — the combined env-block size is an OS boundary, reported as such:
    /// the 16 KiB cap bounds ocx's own contribution and nothing else.
    #[test]
    fn a038_the_env_block_boundary_is_reported_as_the_os_limit() {
        let text = base(None).lines().join("\n");
        assert!(text.contains(&format!("of {MAX_CARRIER_BYTES}")), "{text}");
        assert!(text.contains("is an OS limit, not accounted here"), "{text}");
    }

    /// C-050 — the ledger renders as **fields**, never as the base64 the
    /// carrier holds, and the two scopes are reported separately.
    #[test]
    fn c050_the_ledger_renders_as_fields_and_scopes_are_separate() {
        let ledger = ledger_with_project();
        let encoded = ledger.encode().expect("the fixture ledger encodes");
        let report = base(None);
        let text = report.lines().join("\n");

        assert!(!text.contains(&encoded), "the report must not carry the base64 carrier");
        assert!(text.contains("schema v: 1"), "{text}");
        assert!(text.contains("envelope: 1"), "{text}");
        assert!(text.contains("applied, global scope:"), "{text}");
        assert!(text.contains("applied, project scope:"), "{text}");
        assert!(text.contains("\"JAVA_HOME\" constant"), "{text}");
        assert!(text.contains("\"PATH\" path"), "{text}");
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
        let text = report.lines().join("\n");
        assert!(text.contains("\"JAVA_HOME\" intact (was unset)"), "{text}");
        assert!(text.contains("\"MAVEN_OPTS\" MISSING"), "{text}");
    }

    /// C-050 — the watch set reports each member's presence, size and mtime,
    /// including members that do not exist (A-13: a tier file becoming present
    /// is itself the change).
    #[test]
    fn c050_a013_watch_set_reports_absent_members_too() {
        let text = base(None).lines().join("\n");
        assert!(text.contains("\"/etc/ocx/config.toml\" (absent)"), "{text}");
        assert!(
            text.contains("\"/home/u/.ocx/config.toml\" (present, 1290 bytes, mtime 1756000000)"),
            "{text}"
        );
    }

    /// SPEC-2 — a ledger that **decoded** is never described as an unset
    /// carrier. The report says `not yet`, and says nothing that contradicts
    /// the `present: yes` / `decoded: yes` lines above it.
    #[test]
    fn c050_c006_a_decoded_ledger_is_never_reported_as_an_unset_carrier() {
        let mut pending = base(None);
        if let Some(ledger) = pending.ledger.as_mut() {
            ledger.scopes.project = None;
        }
        pending.priors = ShellStateReport::priors_for(pending.ledger.as_ref());
        assert!(pending.project_scope_pending());

        let text = pending.lines().join("\n");
        assert!(text.contains("present: yes"), "{text}");
        assert!(text.contains("decoded: yes"), "{text}");
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
        // `LedgerUnreadable`'s business, not this one's.
        let mut absent = base(None);
        absent.carrier_present = false;
        absent.ledger = None;
        assert!(!absent.project_scope_pending());
    }

    /// QUAL-3 — a project that is reachable but unresolvable gets its own row
    /// rather than being reported as "no project reachable".
    #[test]
    fn qual3_an_unresolvable_project_is_not_reported_as_no_project() {
        let mut report = base(None);
        report.project_dir = None;
        report.notes = vec![Note::ProjectUnresolved {
            detail: "invalid TOML in '/work/proj/ocx.toml'".to_owned(),
        }];
        let text = report.lines().join("\n");
        assert!(
            text.contains("a project file is reachable but could not be resolved"),
            "{text}"
        );
        assert!(text.contains("invalid TOML"), "{text}");
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
            let lines = report.lines();
            assert!(
                lines.iter().any(|line| line == expected),
                "fingerprint_current={value:?} must render {expected:?}; got {lines:#?}"
            );
        }
    }

    /// The never-eval-able assertion's own red state. A green result is
    /// evidence only if a red one was reachable: this shows the assertion
    /// firing on exactly the injection the fault-injection run uses.
    #[test]
    #[should_panic(expected = "is valid shell source")]
    fn the_never_eval_able_assertion_can_go_red() {
        let mut lines = base(None).lines();
        lines.push("  export OCX_INJECTED=1".to_owned());
        assert_lines_never_eval_able("injected", &lines);
    }

    /// SEC-1's red state, locked in permanently: one `Vec` element carrying an
    /// embedded LF is two physical lines on stdout, and the assertion must see
    /// the second one. Before the split-on-newline fix this passed.
    #[test]
    #[should_panic(expected = "is valid shell source")]
    fn the_never_eval_able_assertion_sees_an_embedded_newline() {
        let mut lines = base(None).lines();
        lines.push("    dir: /work\nexport OCX_EVIL=2".to_owned());
        assert_lines_never_eval_able("embedded_newline", &lines);
    }

    /// And the bare-assignment half of it, which no emitter prefix covers.
    #[test]
    #[should_panic(expected = "is a bare shell assignment")]
    fn the_bare_assignment_assertion_can_go_red() {
        let mut lines = base(None).lines();
        lines.push("  OCX_INJECTED=1".to_owned());
        assert_lines_never_eval_able("injected", &lines);
    }
}
