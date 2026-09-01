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
//!    owns** — a heading in the first column, or an indented label, or an
//!    indented `- ` list marker. No caller-, carrier- or filesystem-supplied
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
//!
//! **Colour does not exempt either rule.** A theme puts its own SGR introducer
//! in front of a heading, so the first byte of a *rendered* line is an escape
//! in every arm — and a `starts_with("export ")` over those bytes would answer
//! about the escape rather than about the text, passing unconditionally. The
//! assertions strip ANSI first, and pair that with a parity check
//! (stripped-coloured equals uncoloured) so the stripping cannot hide a
//! divergence of its own.

use std::path::{Path, PathBuf};

use ocx_lib::cli::{DataInterface, Theme, human_bytes, human_instant, human_time};
use ocx_lib::project::consent::{Grant, Reason};
use ocx_lib::shell::coexistence::{Observation, Tool};
use ocx_lib::shell::reconcile::{CARRIER_KEY, Ledger, LedgerEntry, MAX_CARRIER_BYTES, Prior, ScopeId, Verdict};
use serde::Serialize;

use crate::api::Printable;

/// Render an untrusted string **quoted for a human, never for a shell**
/// (S-022) — and only when it needs quoting.
///
/// `{:?}` escapes the two bytes that would otherwise break the report's
/// one-fact-per-line shape — LF and CR — along with `"` and `\`, and wraps the
/// result in quotes so the exact bytes are visible. It is deliberately *not* a
/// shell escaper: the point is that the result cannot be pasted into a shell
/// and mean anything.
///
/// Applied **unconditionally** it also quoted every ordinary path, which is
/// most of what this report prints, and a page of `"…"` reads as a serialized
/// document rather than a report. So a value that `{:?}` would emit unchanged
/// is emitted unchanged, and the quotes become a signal: this value carries
/// something a reader should look at twice.
///
/// The predicate is a **subset** of what the fallback leaves alone, which is
/// why it cannot let anything through that the unconditional form would have
/// escaped: `escape_debug` renders exactly one character for a character it
/// does not escape, so requiring that is requiring `{:?}` to be a no-op. It
/// covers the control range, the C1 introducers and the Unicode bidi controls
/// (CWE-150) without naming any of them. Whitespace is excluded separately —
/// `{:?}` passes a space through, but an unquoted value with spaces in it is
/// unreadable on a line whose fields are space-separated.
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

/// [`quoted`] over a path, which reaches us from the carrier, the environment
/// or the filesystem and is therefore subject to the same rule.
fn quoted_path(path: &Path) -> String {
    quoted(&path.display().to_string())
}

/// [`human_bytes`] over the report's `u64` sizes.
///
/// A size past `i64::MAX` cannot come off a real file; `human_bytes` renders a
/// negative as `unknown`, which is the honest answer for one that did.
fn human_size(bytes: u64) -> String {
    human_bytes(i64::try_from(bytes).unwrap_or(-1))
}

/// A watch member's mtime, as an age **and** the instant it stands for.
///
/// Both, because this line is asked both questions: *is this file newer than
/// the freshness stamp* (the age) and *which exact second does the fingerprint
/// fold* (the instant, A-14). The bare epoch integer answered neither without
/// arithmetic. A value that will not convert falls back to it.
fn modified_description(mtime: u64) -> String {
    let converted = i64::try_from(mtime)
        .ok()
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0));
    let Some(at) = converted else {
        return format!("mtime {mtime}");
    };
    format!("{}, {}", human_time(at), human_instant(at))
}

/// The consent stamp's recorded instant, as an age **and** the instant itself.
///
/// The stamp is a file on disk, so its string is untrusted — and parsing it is
/// a stronger answer to that than quoting it was: a value re-rendered from a
/// `DateTime` cannot carry a control sequence at all, whatever the file held.
/// Only an unparseable value reaches [`quoted`], which is where S-022 still
/// does the work.
fn written_description(written: &str) -> String {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(written) else {
        return quoted(written);
    };
    let at = at.with_timezone(&chrono::Utc);
    format!("{}, {}", human_time(at), human_instant(at))
}

/// One member of the fingerprint watch set (C-019, A-13), as it stands on disk
/// right now.
///
/// Absent members are recorded too: a tier file that did not exist becoming
/// present is exactly the change the watch set must notice, which is why the
/// loader records candidates rather than survivors (A-13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
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
    /// A-28 — a `paths` entry that would grant `canonical` if the compare
    /// folded ASCII case, exact or subtree entry alike. Entries are compared
    /// as literal bytes ([`ocx_lib::consent_path_matches`]), so this is
    /// `Inert`; the row exists to pay off the support cost of that decision.
    PathsNearMiss {
        /// The entry that nearly matched.
        entry: PathBuf,
        /// The canonical project directory it was compared against.
        canonical: PathBuf,
    },
    /// A `[shell.consent] paths` entry that can never match any project by
    /// construction — [`ocx_lib::consent_entry_defect`]'s classification of
    /// the entry's own bytes, independent of which project is in view.
    /// `namespaces`' sibling grant refuses a pattern this broken at parse
    /// time (A-27); a `paths` entry is an ordinary path and parses
    /// regardless of whether it can ever grant, so without this row it sits
    /// in the config forever, granting nothing and saying nothing.
    PathsDefect {
        /// The malformed entry.
        entry: PathBuf,
        /// [`ocx_lib::EntryDefect`]'s own rendering of what is wrong with it.
        defect: String,
    },
}

/// How much of the report the human rendering carries.
///
/// A tier of the **plain** rendering only: the structured report serializes
/// the whole [`ShellStateReport`] either way, so nothing a human flag hides is
/// hidden from `--format json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// The answer, and nothing else: where OCX is, which project is in effect,
    /// whether the integration is active, and — when it is not — the
    /// enumerated reason and the fix.
    Answer,
    /// The answer, plus the evidence behind it: the decoded ledger with its
    /// carrier accounting, the fingerprint watch set, the project's state key
    /// and stamp, and the hook ladder.
    Diagnostics,
}

/// What `ocx shell state` reports — all derived, none of it mutating.
///
/// Read-only, absolutely: it never writes a stamp, never repairs a ledger,
/// never emits a plan (A-29 names it a non-member of the six-writer stamp
/// allowlist — a stamp written from here would consent to the very project it
/// is diagnosing). Repair is the `unset __OCX_ENV_STATE` gesture or a new
/// shell (C-012); this command is how a user checks the gesture worked.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ShellStateReport {
    /// `$OCX_HOME`, and whether it exists. A missing home is an ordinary state
    /// on a fresh install and exits 0; only a home that cannot be *read* is the
    /// single 74 path (C-051).
    pub ocx_home: PathBuf,
    /// Whether `$OCX_HOME` exists on disk.
    pub ocx_home_present: bool,

    /// Whether `ocx self setup` has ever wired this machine's shell — probed
    /// as [`ocx_lib::setup::shims::WITNESS_SHIM`] under [`Self::ocx_home`].
    ///
    /// C-050 reason 6's first-prompt half tells the user "the next prompt
    /// applies it". That is true only once the rc/profile fence exists to
    /// source the shim, and on a bare-binary install neither does. Without
    /// this field the report answers the command's single most likely first
    /// use — *"I installed ocx, why isn't it working"* — with a fix that can
    /// never come true, so the renderer branches on it rather than promising
    /// convergence that nothing will perform.
    ///
    /// Deliberately **not** a [`Reason`] arm: "setup has not run" is not a
    /// consent state, and `consent::Reason` enumerates consent states.
    pub shell_integration_installed: bool,

    /// Why the project's `ocx.lock` refuses composition, when it does — the
    /// `Display` of [`ocx_lib::project::LockCurrency`], rendered as text the
    /// same way [`Note::ProjectUnresolved`] carries its detail.
    ///
    /// Consent can say *activate* over a lock that composition then refuses: a
    /// `paths` grant holds without a readable lock at all, and a stale one
    /// still parses. Every prompt then exits 65 with a stderr the hook
    /// discards (A-21), so the scope never reaches the ledger and the report
    /// answered `active: not yet` — "the next prompt applies it" — forever.
    /// Nothing else in the report could tell that state from a genuine first
    /// prompt.
    ///
    /// Deliberately **not** a [`Reason`] arm: the consent predicate did not
    /// refuse, so this is not one of its eight enumerated answers.
    pub lock_refusal: Option<String>,

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

    /// **Which clause activated this project**, or `None` when it is inert.
    ///
    /// The consent stamp is an independent grant kind that outranks the
    /// `[shell.consent]` table a user reads, and until this field existed the
    /// report could not tell the two apart: a project stamped by a past
    /// `ocx add` looked exactly like one the config granted, so the config and
    /// the behaviour disagreed with nothing to say so. Naming the granting
    /// clause is the whole fix — a user who sees `stamp` where they expected
    /// `paths` now knows which one to revoke.
    ///
    /// Serialized as the [`Grant`] discriminant: `"stamp"`, `"namespace"` or
    /// `"path"`.
    pub grant: Option<Grant>,

    /// When the consent stamp was written (RFC 3339, UTC), when there is one.
    ///
    /// The stamp's own recorded instant, not a `stat` — a stamp replaced in
    /// place would otherwise report the moment of the replacement. `None`
    /// whenever [`Self::project_stamped`] is `false`; the two come from one
    /// read, so they cannot disagree.
    pub stamp_written_at: Option<String>,

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

    /// The report as the lines [`Printable::print_plain`] prints, at `detail`.
    ///
    /// Separated from the printing so the never-eval-able assertions can run
    /// over the exact bytes a user sees without capturing stdout — and so they
    /// can run over the **coloured** bytes too, which is where a label's own
    /// escape sequence would otherwise sit in front of a line's real first
    /// token and make the assertion answer about the escape instead.
    ///
    /// The answer leads at both tiers: where OCX is, which project is in
    /// effect, whether the integration is active, and — when it is not — the
    /// enumerated reason and the fix. [`Detail::Diagnostics`] appends the
    /// evidence behind that answer; nothing is dropped from the structured
    /// report at either tier.
    #[must_use]
    pub fn lines(&self, theme: &Theme, detail: Detail) -> Vec<String> {
        let mut out = Vec::new();
        self.summary_lines(theme, detail, &mut out);
        self.activation_lines(theme, &mut out);
        if detail == Detail::Diagnostics {
            out.push(String::new());
            self.ledger_lines(theme, &mut out);
            self.fingerprint_lines(theme, &mut out);
            self.hook_lines(theme, &mut out);
        }
        // Each diagnostics section closes with its own blank separator, so the
        // last one would otherwise leave the report ending on an empty line.
        while out.last().is_some_and(String::is_empty) {
            out.pop();
        }
        out
    }

    /// Where OCX lives and which project is in effect — the two facts every
    /// other line is relative to.
    ///
    /// The state key and the stamp's presence are diagnostics: a lookup index
    /// and a file that exists or does not, neither of which is the answer to
    /// *"is it working"*. They join the block under [`Detail::Diagnostics`].
    fn summary_lines(&self, theme: &Theme, detail: Detail, out: &mut Vec<String>) {
        let home = if self.ocx_home_present {
            quoted_path(&self.ocx_home)
        } else {
            format!("{} {}", quoted_path(&self.ocx_home), theme.alert("(absent)"))
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
        out.push(String::new());
    }

    /// The decoded ledger, as fields — never as the base64 the carrier holds.
    fn ledger_lines(&self, theme: &Theme, out: &mut Vec<String>) {
        out.push(theme.label("ledger:"));
        out.push(theme.field("  ", "carrier", quoted(CARRIER_KEY)));
        out.push(theme.field("  ", "present", if self.carrier_present { "yes" } else { "no" }));
        // A-38 — the cap bounds ocx's own contribution only; the combined
        // argv+envp size is an OS boundary ocx does not account for. That
        // distinction is documentation, and it lives on the docs page rather
        // than in a parenthesis on a status line.
        out.push(theme.field(
            "  ",
            "bytes",
            format!(
                "{} {}",
                self.carrier_bytes,
                theme.note(format!("of {MAX_CARRIER_BYTES}"))
            ),
        ));

        let Some(ledger) = self.ledger.as_ref() else {
            out.push(theme.field(
                "  ",
                "decoded",
                format!(
                    "no {}",
                    theme.note(if self.carrier_present {
                        "(truncated, or an unrecognised envelope tag)"
                    } else {
                        "(the carrier is unset)"
                    })
                ),
            ));
            out.push(String::new());
            return;
        };

        out.push(theme.field("  ", "decoded", "yes"));
        out.push(theme.field("  ", "envelope", "1"));
        out.push(theme.field("  ", "schema v", ledger.v.to_string()));
        out.push(theme.field("  ", "fingerprint", quoted(&ledger.fp)));
        out.push(theme.field(
            "  ",
            "verdict",
            match ledger.verdict {
                Some(Verdict::Inert) => "inert",
                Some(Verdict::NoProject) => "no project",
                Some(Verdict::Activate) => "activate",
                None => "none recorded",
            },
        ));
        out.push(theme.field(
            "  ",
            "over cap",
            if ledger.over_cap.is_empty() {
                "none".to_owned()
            } else {
                theme.alert(ledger.over_cap.iter().map(scope_name).collect::<Vec<_>>().join(", "))
            },
        ));

        out.push(theme.field("  ", "applied, global scope", ""));
        match ledger.scopes.global.as_ref() {
            Some(applied) if !applied.is_empty() => push_entries(theme, out, applied),
            Some(_) => out.push("    - nothing".to_owned()),
            None => out.push("    - no record".to_owned()),
        }

        out.push(theme.field("  ", "applied, project scope", ""));
        match ledger.scopes.project.as_ref() {
            Some(project) => {
                out.push(theme.field("    ", "key", quoted(&project.key)));
                out.push(theme.field("    ", "dir", quoted_path(&project.dir)));
                if project.applied.is_empty() {
                    out.push("    - nothing".to_owned());
                } else {
                    push_entries(theme, out, &project.applied);
                }
                out.push(theme.field("    ", "priors", ""));
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
                        "      - {} {} {}",
                        quoted(&prior.key),
                        if prior.intact {
                            theme.ok("intact")
                        } else {
                            theme.alert("MISSING")
                        },
                        theme.note(format!("({recorded})"))
                    ));
                }
            }
            None => out.push("    - no record".to_owned()),
        }
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
            let detail = match (member.size, member.mtime) {
                (Some(size), Some(mtime)) => {
                    format!(
                        "present, {}, modified {}",
                        human_size(size),
                        modified_description(mtime)
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

    /// Whether a reachable project file could not be resolved.
    ///
    /// QUAL-3 — an unresolvable `ocx.toml` composes nothing, so the shell is
    /// not active, but no [`Reason`] says so: the consent predicate never ran,
    /// because there was no project to run it over. Without this the report
    /// prints `active: yes` over a broken project file — the one answer a
    /// command whose entire product is the explanation must never give.
    fn project_unresolved(&self) -> bool {
        self.notes
            .iter()
            .any(|note| matches!(note, Note::ProjectUnresolved { .. }))
    }

    /// The verdict, the enumerated inertness reason behind it, its fix, and the
    /// notes that explain an answer without being a verdict.
    fn activation_lines(&self, theme: &Theme, out: &mut Vec<String>) {
        match self.inert_reason.as_ref() {
            Some(reason) => {
                out.push(format!("{} {}", theme.label("active:"), theme.alert("no")));
                self.reason_lines(theme, reason, out);
            }
            // No enumerated reason, and none is owed: the project tier never
            // resolved, so nothing composed. The note below carries the detail
            // and the fix; this line only has to stop saying `yes`.
            None if self.project_unresolved() => {
                out.push(format!("{} {}", theme.label("active:"), theme.alert("no")));
            }
            // A project resolved, consent did not refuse, and the ledger
            // decoded but holds no project-scope record: the scope simply has
            // not been applied in this shell yet. Saying `yes` here would be as
            // untrue as calling a decoded carrier "unset" — and C-050 reason 6
            // enumerates exactly two carrier situations, so this third state
            // gets its own sentence rather than borrowing one of theirs.
            None if self.project_scope_pending() => match self.lock_refusal.as_deref() {
                // Consent said activate and composition refuses anyway, so the
                // scope will never reach the ledger: "not yet" is a promise
                // about a prompt that has already failed silently on every
                // one before this.
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
        // The provenance of the grant, at both detail tiers: it is the answer,
        // not the evidence behind it. `None` here is the inert case, where the
        // reason arms above already said why nothing granted.
        if let Some(grant) = self.grant {
            // Themed like `fix:` — its structural peer, a labelled line under
            // the verdict rather than one of the uncoloured evidence rows.
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
                    // No separate `fix:` line: unlike a `Reason` refusal, this
                    // note is not gated to an inert project (a defective entry
                    // is a config bug whether or not something else granted),
                    // and `every_inert_arm_names_a_fix_and_no_other_arm_does`
                    // holds `fix:` to exactly the arms that block activation.
                    // `EntryDefect::Display` already states the actionable
                    // rewrite, so the guidance is not lost.
                    out.push(theme.field("  ", "problem", defect));
                }
            }
        }
    }

    /// The consent stamp's presence and, when present, the instant it records.
    ///
    /// One sentence for both, because "present" without a date is the state
    /// that made the stamp feel like it appeared from nowhere.
    fn stamp_description(&self) -> String {
        match (self.project_stamped, self.stamp_written_at.as_deref()) {
            (true, Some(written)) => format!("present (written {})", written_description(written)),
            (true, None) => "present".to_owned(),
            (false, _) => "absent".to_owned(),
        }
    }

    /// The granting clause, spelled the way the user would go looking for it —
    /// a command for the stamp, a config key for the other two.
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

    /// One arm per enumerated reason (C-050), each naming its own evidence and
    /// closing with the one line that says what to do about it.
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
                    // A `Some` that disagrees is the security-relevant half: a
                    // locked digest in the store came from a repository outside
                    // the granted namespace, and the lock renamed it.
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
                // A-37 — both sentinels fire independently, so this is one line
                // per observed tool, never an `elif` chain that suppresses the
                // second. `Reason::YieldedTo` carries only the first.
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
                            tool_name(observation.tool),
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
            Reason::LedgerOverCap { scope } => {
                headline(out, "the ledger exceeded its cap and a scope was abandoned");
                out.push(theme.field("  ", "abandoned scope", scope_name(scope)));
                out.push("  read from the over_cap marker the carrier still holds".to_owned());
                push_fix(
                    theme,
                    out,
                    "declare less environment in this scope; its ledger payload does not fit the cap",
                );
            }
            Reason::LedgerUnreadable { first_prompt } => {
                if *first_prompt {
                    if self.shell_integration_installed {
                        headline(out, "nothing has been applied in this shell yet");
                        out.push("  the carrier is unset: this is the first prompt, not a fault".to_owned());
                        push_fix(theme, out, "none needed - the next prompt applies it");
                    } else {
                        // Same evidence, opposite remedy: an unset carrier in a
                        // shell that was never wired is not a first prompt on
                        // the way to converging, it is a shell where no prompt
                        // will ever apply anything.
                        headline(out, "the shell integration has never been installed here");
                        out.push(format!(
                            "  no {} under {}: `ocx self setup` has not run",
                            quoted(ocx_lib::setup::shims::WITNESS_SHIM),
                            quoted_path(&self.ocx_home)
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

/// One ledger entry per line, under the fixed `- ` list marker so no carrier
/// byte is ever the first token of a line.
fn push_entries(theme: &Theme, out: &mut Vec<String>, applied: &[LedgerEntry]) {
    for entry in applied {
        let separator = entry.separator.as_deref().map_or_else(String::new, |sep| {
            format!(" {}", theme.note(format!("sep {}", quoted(sep))))
        });
        out.push(format!(
            "    - {} {} {}{separator}",
            quoted(&entry.key),
            theme.note(entry.kind.to_string()),
            quoted(&entry.value)
        ));
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

/// The one line that says what to do about the reason above it.
///
/// Every arm has one, including the two whose remedy is *none* — a shell that
/// converges by itself is an answer, and leaving it unsaid reads as an
/// omission. The label is styled so a reader scanning for what to type finds
/// it without reading the evidence.
fn push_fix(theme: &Theme, out: &mut Vec<String>, fix: &str) {
    out.push(format!("{} {fix}", theme.label("fix:")));
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
    /// The answer: where OCX is, which project is in effect, whether the
    /// integration is active, and — when it is not — the enumerated reason and
    /// the fix. Never eval-able (C-050), coloured or not.
    fn print_plain(&self, data: &DataInterface) {
        for line in self.lines(&data.theme(), Detail::Answer) {
            println!("{line}");
        }
    }
}

/// [`ShellStateReport`] rendered with the diagnostics behind the answer —
/// `ocx shell state --verbose`.
///
/// Plain format: the same leading answer, then the decoded ledger, the
/// fingerprint watch set and the hook ladder.
///
/// JSON format: delegates to the inner [`ShellStateReport`] — **identical wire
/// shape whether verbose or not**, the same contract `VerboseVersionData`
/// keeps. `--verbose` is a human flag; a `--format json` consumer never sees
/// less for its absence.
pub struct VerboseShellState(pub ShellStateReport);

impl Printable for VerboseShellState {
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

// The `Serialize` impl above is transparent, so the published schema is the
// inner type's. Verbosity changes the plain rendering only.
impl schemars::JsonSchema for VerboseShellState {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VerboseShellState".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <ShellStateReport>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use ocx_lib::cli::Theme;
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

    fn ledger_with_project() -> Ledger {
        Ledger {
            v: 1,
            tiers: Vec::new(),
            fp: "a1b2c3d4".to_owned(),
            ws: "0f1e2d3c4b5a6978".to_owned(),
            verdict: None,
            messages_fp: String::new(),
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
        // An active project was granted by *something*, an inert one by
        // nothing — so the grant tracks the reason rather than being a third
        // independent knob a fixture could set incoherently. This is what puts
        // the `granted by:` line, and the untrusted timestamp it interpolates,
        // inside the never-eval-able corpus.
        let granted = reason.is_none();
        ShellStateReport {
            ocx_home: PathBuf::from("/home/u/.ocx"),
            ocx_home_present: true,
            shell_integration_installed: true,
            lock_refusal: None,
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
            project_stamped: granted,
            grant: granted.then_some(Grant::Stamp),
            stamp_written_at: granted.then(|| "2026-08-27T07:12:03Z".to_owned()),
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

        let mut never_set_up = base(Some(Reason::LedgerUnreadable { first_prompt: true }));
        never_set_up.shell_integration_installed = false;
        never_set_up.carrier_present = false;
        never_set_up.carrier_bytes = 0;
        never_set_up.ledger = None;
        never_set_up.priors = Vec::new();
        arms.push(("ledger_absent_setup_never_ran", never_set_up));

        let mut lock_refused = base(None);
        lock_refused.lock_refusal =
            Some("ocx.lock is stale (ocx.toml changed since last `ocx lock`); run `ocx lock`".to_owned());
        if let Some(ledger) = lock_refused.ledger.as_mut() {
            ledger.scopes.project = None;
        }
        lock_refused.priors = ShellStateReport::priors_for(lock_refused.ledger.as_ref());
        arms.push(("lock_refuses_composition", lock_refused));

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

        let mut paths_defect = base(None);
        paths_defect.notes = vec![Note::PathsDefect {
            entry: PathBuf::from("/w/*/tools"),
            defect: "a `*` may appear only as the entry's final path component".to_owned(),
        }];
        arms.push(("note_paths_defect", paths_defect));

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
            Note::PathsDefect { .. } => "paths_defect",
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
                "paths_defect",
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
        let mut report = base(Some(Reason::LedgerOverCap {
            scope: ScopeId::Project,
        }));
        if let Some(ledger) = report.ledger.as_mut() {
            ledger.over_cap = vec![ScopeId::Project];
            ledger.scopes.project = None;
        }
        report.priors = ShellStateReport::priors_for(report.ledger.as_ref());
        let text = diagnostics(&report);
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
        let absent_text = diagnostics(&absent);

        let mut corrupt = base(Some(Reason::LedgerUnreadable { first_prompt: false }));
        corrupt.ledger = None;
        corrupt.priors = Vec::new();
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
        assert!(
            corrupt_text.contains("truncated, or an unrecognised envelope tag"),
            "{corrupt_text}"
        );
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
        let mut never_set_up = base(Some(Reason::LedgerUnreadable { first_prompt: true }));
        never_set_up.shell_integration_installed = false;
        never_set_up.carrier_present = false;
        never_set_up.ledger = None;
        never_set_up.priors = Vec::new();
        let unwired = diagnostics(&never_set_up);

        assert!(
            unwired.contains("the shell integration has never been installed here"),
            "{unwired}"
        );
        assert!(unwired.contains("ocx self setup"), "{unwired}");
        assert!(
            unwired.contains(ocx_lib::setup::shims::WITNESS_SHIM),
            "the row must name the shim it probed, so the answer is checkable: {unwired}"
        );
        assert!(
            !unwired.contains("the next prompt applies it"),
            "the fix that can never come true must not survive: {unwired}"
        );

        let mut wired = base(Some(Reason::LedgerUnreadable { first_prompt: true }));
        wired.carrier_present = false;
        wired.ledger = None;
        wired.priors = Vec::new();
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
        let refusal = "ocx.lock is stale (ocx.toml changed since last `ocx lock`); run `ocx lock`";

        let mut refused = base(None);
        refused.lock_refusal = Some(refusal.to_owned());
        if let Some(ledger) = refused.ledger.as_mut() {
            ledger.scopes.project = None;
        }
        refused.priors = ShellStateReport::priors_for(refused.ledger.as_ref());
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
        if let Some(ledger) = pending.ledger.as_mut() {
            ledger.scopes.project = None;
        }
        pending.priors = ShellStateReport::priors_for(pending.ledger.as_ref());
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
        assert!(text.contains(&format!("of {MAX_CARRIER_BYTES}")), "{text}");
        // The cap is reported; the *explanation* of what it does and does not
        // bound is documentation and lives on the docs page. A status line
        // that carries a paragraph is the thing this rendering removed, so the
        // assertion that used to require the paragraph now forbids it.
        assert!(
            !text.contains("OS limit"),
            "the env-block explanation belongs in the docs, not in a parenthesis on a status line: {text}"
        );
    }

    /// C-050 — the ledger renders as **fields**, never as the base64 the
    /// carrier holds, and the two scopes are reported separately.
    #[test]
    fn c050_the_ledger_renders_as_fields_and_scopes_are_separate() {
        let ledger = ledger_with_project();
        let encoded = ledger.encode().expect("the fixture ledger encodes");
        let report = base(None);
        let text = diagnostics(&report);

        assert!(!text.contains(&encoded), "the report must not carry the base64 carrier");
        assert!(text.contains("schema v: 1"), "{text}");
        assert!(text.contains("envelope: 1"), "{text}");
        assert!(text.contains("applied, global scope:"), "{text}");
        assert!(text.contains("applied, project scope:"), "{text}");
        assert!(text.contains("JAVA_HOME constant"), "{text}");
        assert!(text.contains("PATH path"), "{text}");
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

    /// C-050 — the watch set reports each member's presence, size and mtime,
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
        if let Some(ledger) = pending.ledger.as_mut() {
            ledger.scopes.project = None;
        }
        pending.priors = ShellStateReport::priors_for(pending.ledger.as_ref());
        assert!(pending.project_scope_pending());

        let text = diagnostics(&pending);
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
        assert!(
            default.len() <= 12,
            "the default rendering is the answer, not the state dump; got {} lines: {default:#?}",
            default.len()
        );

        // Each section the default drops, and the evidence rows inside them.
        for section in [
            "ledger:",
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
        let ledger_at = verbose
            .iter()
            .position(|line| line == "ledger:")
            .expect("the verbose rendering carries the ledger");
        assert!(
            reason_at < ledger_at,
            "the answer must lead at both tiers: {verbose:#?}"
        );
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
            "decoded:",
            "envelope:",
            "schema v:",
            "fingerprint:",
            "verdict:",
            "over cap:",
            "applied, global scope:",
            "applied, project scope:",
            "dir:",
            "priors:",
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
        let bare = serde_json::to_value(base(Some(Reason::LockUnavailable))).expect("the report serializes");
        let verbose = serde_json::to_value(VerboseShellState(base(Some(Reason::LockUnavailable))))
            .expect("the verbose wrapper serializes");
        assert_eq!(bare, verbose, "`--verbose` must not change the structured payload");

        // Every field the default *rendering* drops is still in the document.
        for key in [
            "ledger",
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
        assert!(!bare["ledger"].is_null(), "the ledger must be a payload, not a null");
        assert!(
            bare["watch_set"].as_array().is_some_and(|set| !set.is_empty()),
            "the watch set must survive into the structured report: {bare}"
        );

        // The other half, so the assertion pair discriminates: those same facts
        // are genuinely gone from the human default.
        let default = answer(&base(Some(Reason::LockUnavailable)));
        for needle in ["watch set", "carrier:", "bytes:", "0123456789abcdef", "mtime"] {
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
        assert!(inert["grant"].is_null(), "an inert project names no grant: {inert}");
        assert!(inert["stamp_written_at"].is_null(), "no stamp, no instant: {inert}");
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
}
