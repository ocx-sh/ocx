// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `__OCX_ENV_STATE` carrier format ([ocx-sh/ocx#345](https://github.com/ocx-sh/ocx/issues/345)):
//! [`LedgerEntry`], [`ScopeId`], [`Verdict`], [`Prior`], [`ProjectScope`],
//! [`Scopes`], [`Ledger`], and the envelope codec
//! ([`Ledger::decode`]/[`Ledger::encode`]).
//!
//! The carrier is **untrusted input** (C-007): its only permitted effects are
//! naming the revert set and supplying the equality operand for the exit
//! guard. Nothing here constructs a path from it, re-grants consent, or
//! selects a value for a key it is not reverting.

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL;
use serde::{Deserialize, Serialize};

use super::{effective_separator, element_eq, is_never_constant, key_eq};
use crate::package::metadata::env::entry::Entry;
use crate::package::metadata::env::modifier::ModifierKind;

/// The private session carrier holding the encoded [`Ledger`].
///
/// C-012 documents `unset __OCX_ENV_STATE` as *the* repair gesture, which makes
/// this spelling user-facing contract — it sits inside the reserved `__OCX_*`
/// namespace [`crate::env::is_reserved_ocx_key`] gates, and C-036's resolver
/// gate exists so no package can declare it. One constant so the emitter
/// (`shell::hook`), the gate (`env`) and the reader here cannot drift.
pub const CARRIER_KEY: &str = "__OCX_ENV_STATE";

/// The schema version [`Ledger::empty`] writes and [`Ledger::decode`] accepts.
///
/// Describes the payload **shape**; the envelope tag describes the encoding
/// (C-003). Additive-only: a new field is optional and never moves this number
/// (A-04).
pub const LEDGER_VERSION: u8 = 1;

/// The size ceiling on the whole `__OCX_ENV_STATE` value, in bytes (C-003, C-004).
pub const MAX_CARRIER_BYTES: usize = 16 * 1024;

/// The ceiling on the recorded config-tier list ([`Ledger::tiers`]).
///
/// [`MAX_CARRIER_BYTES`] bounds the envelope, not the array inside it — and
/// `tiers` is the one field that turns array length into **per-prompt syscalls**:
/// every path in it becomes a `stat` in `watch_paths`' set, on every prompt, and
/// `activation::next_ledger` carries the list forward for the shell's whole life.
/// ~12 KiB of short JSON strings fits ~2400 of them inside the 16 KiB envelope,
/// so a carrier the user is invited to `unset` (C-012) — and can therefore also
/// hand-set — buys ~2400 `stat` calls per prompt, permanently.
///
/// Eight is headroom over the five `ConfigLoader::load_with_local_view` can
/// actually emit: the three candidate tiers (system, user, home) plus
/// `OCX_CONFIG` and `--config`. Nothing legitimate is ever truncated, so the
/// A-13 guarantee that a grant added to any tier expires the cached verdict is
/// untouched.
pub const MAX_RECORDED_TIERS: usize = 8;

/// The envelope tag naming encoder 1: base64url of compact JSON, uncompressed.
///
/// Private: nothing outside this file has any business writing an envelope.
const ENCODER_TAG: &str = "1";

/// One env-var binding the reconciler applied, recorded literally.
///
/// C-001 — the wire field is **`type`, never `kind`**: the spelling
/// `ocx_cli::api::data::env::EnvEntry` already emits and the nushell shim
/// already parses. Values are raw and unescaped (C-009, invariant L-2) and
/// byte-exact copies of what ocx wrote (C-008, invariant L-1).
///
/// A-08 — `separator` holds the **effective** separator, resolved once at
/// record time: always `Some` for [`ModifierKind::List`] (defaulting to
/// [`DEFAULT_SEPARATOR`]), and `None` reserved for path-kind, where it means
/// [`crate::env::PATH_SEPARATOR`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Environment-variable name.
    pub key: String,
    /// The exact string ocx wrote, byte for byte.
    pub value: String,
    /// How the value combines — re-derived from D for every key D declares
    /// (C-007 rule (b)); L's copy is used only for the revert set.
    #[serde(rename = "type")]
    pub kind: ModifierKind,
    /// The effective list separator; `None` is path-kind only (A-08).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
}

impl From<&Entry> for LedgerEntry {
    /// Infallible, and copies `value` byte for byte (C-008, C-009).
    fn from(entry: &Entry) -> Self {
        Self {
            key: entry.key.clone(),
            value: entry.value.clone(),
            kind: entry.kind.clone(),
            separator: match entry.kind {
                // A-08 resolves the default here, at record time, so no revert
                // path ever has to guess one back.
                ModifierKind::List => Some(effective_separator(entry)),
                ModifierKind::Path | ModifierKind::Constant => None,
            },
        }
    }
}

/// Which scope a ledger datum belongs to. Wire spelling `global` / `project`.
///
/// C-018 — exactly two slots. A project nested inside a project does not layer;
/// the inner one *replaces* the outer, so moving between them is a switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeId {
    /// The `--global` toolchain tier.
    Global,
    /// The project resolved by the CWD walk.
    Project,
}

/// The cached activation verdict (C-002).
///
/// **No positive verdict is ever written.** An `Activate` verdict is re-derived
/// every prompt and never read back from the carrier — caching it would make the
/// ledger a consent input, which C-007 forbids. The variant exists so the
/// vocabulary is total and the wire value is checkable. The two cached verdicts
/// are both *negative*: they can only ever cause ocx to do less, which is the
/// fail-safe direction, and the watch set expires both (A-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Never written to the carrier; present so the enum is total.
    Activate,
    /// The negative-consent cache, expired by the watch set (C-019, C-042).
    Inert,
    /// The walk resolved no project at all. Not consent-derived — there is no
    /// project to consent to — so caching it leaves C-007 untouched;
    /// `project_dir` is folded into the fingerprint, so entering any project
    /// expires it.
    ///
    /// Distinct from [`Verdict::Inert`] rather than folded into it: `inert`
    /// means *a project was resolved and consent refused it*, which
    /// `ocx shell state` reports and which a later grant must expire through the
    /// project's consent stamp — a stamp there is no key for when no project was
    /// resolved. Overloading one variant would make the two indistinguishable in
    /// the carrier and in that report.
    ///
    /// Wire spelling `"noproject"`. A binary predating this variant fails
    /// [`Ledger::decode`] on it and treats the ledger as absent (C-006, the
    /// fail-safe direction) — reachable only across a `self update` mid-session,
    /// and the very next prompt re-derives everything from scratch.
    NoProject,
}

/// What a scope's previous value was, for the constant revert path (C-015).
///
/// A-05 — capture reads set-ness through `std::env::var_os`, so a set-but-empty
/// variable is `Value("")` and **never** `Unset`. Reverting `Value("")` emits
/// that arm's `export_constant(key, "")`, never `Shell::unset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Prior {
    /// The variable did not exist before ocx set it; reverting removes it.
    Unset,
    /// The variable held this exact string; reverting restores it.
    Value(String),
}

/// The entries one scope applied, in emission order (C-021).
pub type Applied = Vec<LedgerEntry>;

/// Pre-apply values for one scope's constants, keyed by env key.
///
/// C-002 — a `BTreeMap` rather than an inline field on [`LedgerEntry`]: a prior
/// must survive a constant that is later retired out of `applied` (C-016), and
/// the sorted map keeps the encoded payload byte-stable for the fingerprint.
pub type Priors = BTreeMap<String, Prior>;

/// The project scope's record (C-002).
///
/// `key` and `dir` are **advisory identity labels** (C-007 rule (a), A-03):
/// both are re-derived from the CWD walk every prompt, neither may construct a
/// path, and `dir` never gates a revert — any value other than the walk's own
/// result means the scope has been left.
///
/// # The one exception
///
/// A-11's determinacy probe
/// ([`walk_is_indeterminate`](crate::activation::walk_is_indeterminate)) is the
/// single sanctioned use of `dir` as a path, and it is a *retention* of the
/// already-applied scope rather than a revert — the fail-safe direction. It is
/// bounded three ways: `dir` must be absolute (A-30 makes an honest one
/// canonical), it must be an ancestor-or-self of the live CWD, and the only
/// effect is one `symlink_metadata` on `<dir>/ocx.toml`. So a carrier can name
/// no path outside the process's own working-directory ancestry, and still
/// selects no value, re-grants no consent, and names no revert.
///
/// Everywhere else the rule above holds verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectScope {
    /// `ReferenceManager::name_for_path` of the canonical project directory.
    pub key: String,
    /// The canonical project directory, advisory only.
    pub dir: PathBuf,
    /// What this scope applied.
    pub applied: Applied,
    /// Pre-apply constant values, captured against the **post-global**
    /// environment (C-018), so reverting the project leaves the global scope
    /// standing.
    ///
    /// That is also why a prior here can hold a value the *global* scope owns
    /// rather than the user's own — [`Ledger::prior`] hops to
    /// [`Scopes::global_priors`] when it does.
    pub priors: Priors,
}

/// The two scope slots (C-018).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scopes {
    /// The global toolchain tier's applied entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<Applied>,
    /// Pre-apply values for the **global** scope's constants (R1).
    ///
    /// A **sibling** field rather than a `priors` member inside `global`,
    /// because turning that JSON array into an object would fail every live
    /// carrier's decode — the fleet-wide `priors` loss A-04 exists to forbid.
    /// As an optional additive field it bumps neither `v` nor the envelope tag
    /// (A-04), and an older binary simply ignores it.
    ///
    /// The design spec justified having no global priors with "the global tier
    /// is the user's own file and is never *left*". That conflates *the scope is
    /// never exited* with *a key is never removed from it*, and
    /// `ocx remove --global <pkg>` removes keys: without this map a retired
    /// global constant had no prior to restore and ocx's value stayed in the
    /// shell for its whole life, and a project constant shadowing a global one
    /// restored **global's** value on a two-scope retirement — the project prior
    /// is captured after global applied (C-018), so that is what it holds.
    ///
    /// Captured against the **pre-global** environment, which only the ledger's
    /// producer sees.
    #[serde(default, skip_serializing_if = "Priors::is_empty")]
    pub global_priors: Priors,
    /// The resolved project tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectScope>,
}

/// The decoded payload of `__OCX_ENV_STATE` (C-002).
///
/// `v` describes **shape**; the envelope tag describes **encoding** (C-003). A
/// change that is both bumps both. A-04 — `v` is additive-only, and a shape
/// break ships a `v-1` revert-read arm in the same release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// Schema version of the payload shape.
    pub v: u8,
    /// Watch-set fingerprint (C-019). A-13 folds the raw `OCX_CONSENT_*`
    /// values, the recorded config-tier paths and the project's consent stamp
    /// into it, which is what makes `verdict` expirable.
    pub fp: String,
    /// The cached negative verdict; [`Verdict::Inert`] or
    /// [`Verdict::NoProject`], never [`Verdict::Activate`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// The config-tier paths that were in effect at compose time (A-13).
    ///
    /// Recorded rather than re-derived, and that distinction is the whole
    /// point: the `--config` / `OCX_CONFIG` explicit tier is a
    /// **consent-bearing channel** (A-33), but the emitted hook body invokes
    /// `--reconcile` with no `--config`, so a per-prompt process re-deriving
    /// the list can never see it — and a grant added to that file would never
    /// expire the cached `inert` verdict. Seeded by the shell-start
    /// `ConfigLoader` pass, which is the one run that knows it, and carried
    /// forward unchanged from there.
    ///
    /// Additive (A-04): optional on the wire, so it moves neither `v` nor the
    /// envelope tag, and an absent list simply falls back to re-derivation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<PathBuf>,

    /// The **membership** of the watch set baked into the shell's emitted gate
    /// ([ocx-sh/ocx#347](https://github.com/ocx-sh/ocx/issues/347)).
    ///
    /// Not a second [`Ledger::fp`]. `fp` folds every member's presence, size and
    /// mtime and answers *"did anything move?"*; this folds the ordered path
    /// list alone and answers *"is the shell watching the right files?"*. The
    /// two are independent: a project's `ocx.lock` can move `fp` on every edit
    /// while `ws` never budges, and entering a project moves both.
    ///
    /// It exists because the gate is **baked at emission time**, not read per
    /// prompt: [`registration`] writes one newer-than term per watch path into
    /// the hook body, and that list then decides whether ocx is invoked at all.
    /// A reconcile recomputes the watch set every prompt, but the shell keeps
    /// gating on the list it was given — so a project entered mid-session was
    /// composed correctly and then never noticed again, and an `ocx add` inside
    /// it could not reach the next prompt.
    ///
    /// Written by, and only by, the emission that redefines the gate
    /// ([`redefinition`]); a value that ran ahead of the emission would describe
    /// a gate the shell does not have. [`next_ledger`] therefore carries it
    /// forward unchanged, exactly as it does [`Ledger::tiers`].
    ///
    /// Additive (A-04): optional on the wire, so it moves neither `v` nor the
    /// envelope tag. A carrier written by a binary that did not know the field
    /// decodes as empty, which no real membership digest equals — one redundant
    /// redefinition, never a missed one, which is the fail-safe direction.
    ///
    /// [`registration`]: crate::shell::hook::registration
    /// [`redefinition`]: crate::shell::hook::redefinition
    /// [`next_ledger`]: crate::activation::next_ledger
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ws: String,

    /// A digest of the deferred diagnostics the previous prompt printed (A-21).
    ///
    /// Deliberately **not** the messages themselves. The only question the next
    /// prompt asks is *"is this line new"*, and 16 hex characters answer it in
    /// the carrier budget the direnv-yield line alone would spend 70 bytes of —
    /// the `[env]` hint another 150.
    ///
    /// Without it, every message in [`Outcome::messages`] re-prints on **every**
    /// prompt for as long as its cause holds. For a directory direnv manages
    /// that is the shell's whole life, and neither of the two verdicts a message
    /// rides with is cacheable: an `Activate` verdict never is (C-007), and a
    /// yield hangs off an env sentinel `fp` does not fold. The summary line
    /// solved the same problem for itself by being a delta against the ledger;
    /// this is the rest of A-21's output getting the same treatment, in the same
    /// shape as `over_cap`'s "already announced on the prompt that did it" rule.
    ///
    /// Additive (A-04): optional on the wire, so it moves neither `v` nor the
    /// envelope tag, and a binary that does not know it re-prints once. It
    /// survives the over-cap marker alongside `fp`, or an over-cap shell would
    /// re-announce on every prompt for exactly the reason this field exists.
    ///
    /// [`Outcome::messages`]: crate::activation::Outcome::messages
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub messages_fp: String,

    /// Scopes the 16 KiB cap dropped (C-004, A-01). An additive field on this
    /// schema — it bumps neither `v` nor the envelope tag — and a scope it
    /// names is reconciled exactly as an absent scope. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub over_cap: Vec<ScopeId>,
    /// What each scope applied.
    pub scopes: Scopes,
}

impl Ledger {
    /// Parse the `<tag> "." <payload>` envelope (C-003).
    ///
    /// Encoder `1` is base64url of compact JSON, uncompressed, and is the only
    /// encoder this design defines. Returns `None` — meaning *treat the ledger
    /// as absent*, C-006 — for **every** failure, with no distinction at the
    /// type level: unrecognised tag, missing `.`, a tag that is not a single
    /// ASCII digit, a payload that is not valid base64url, JSON that does not
    /// match the schema, an unrecognised `v`, or a raw value over 16 KiB.
    ///
    /// A-02 — decode also discards any ledger recording `PATH` or `PATHEXT` as
    /// [`ModifierKind::Constant`], or carrying priors for either key.
    ///
    /// The excess of an over-long [`Ledger::tiers`] is **truncated, not
    /// rejected**: same direction as A-02's forged-constant strip, and for the
    /// same reason — the rest of the record is still usable, and refusing the
    /// whole carrier over one oversized field would discard a live shell's
    /// revert set. See [`MAX_RECORDED_TIERS`] for what the field costs per
    /// prompt.
    pub fn decode(raw: &str) -> Option<Ledger> {
        if raw.len() > MAX_CARRIER_BYTES {
            return None;
        }
        // `.` is absent from the base64url alphabet, so the first one is the
        // only one and the split needs no length prefix.
        let (tag, payload) = raw.split_once('.')?;
        if tag != ENCODER_TAG {
            return None;
        }
        let bytes = BASE64_URL.decode(payload).ok()?;
        let mut ledger: Ledger = serde_json::from_slice(&bytes).ok()?;
        if ledger.v != LEDGER_VERSION {
            return None;
        }
        ledger.tiers.truncate(MAX_RECORDED_TIERS);
        ledger.discard_forged_path_constants();
        Some(ledger)
    }

    /// Encode to `1.<base64url(compact json)>` (C-004).
    ///
    /// Over the 16 KiB cap this MUST NOT omit the variable: it emits a
    /// **marker-only** ledger — `{ v, fp, verdict, over_cap }` with both scope
    /// payloads dropped — which still carries the fingerprint and still
    /// decodes (A-01). `None` means *omit the variable entirely* and is
    /// reachable only when even the marker fails to encode.
    pub fn encode(&self) -> Option<String> {
        let full = envelope(self)?;
        if full.len() <= MAX_CARRIER_BYTES {
            return Some(full);
        }
        let marker = Ledger {
            v: self.v,
            fp: self.fp.clone(),
            // Survives the cap on `fp`'s reasoning inverted: dropping it would
            // make an over-cap shell redefine its gate on every single prompt,
            // for as long as the scope stays over the cap.
            ws: self.ws.clone(),
            verdict: self.verdict,
            // The tier list is what makes the next prompt's fingerprint
            // comparable at all, so it survives the cap alongside `fp`.
            tiers: self.tiers.clone(),
            // Same reasoning one field down: dropping it here would make an
            // over-cap shell re-announce every deferred diagnostic on every
            // prompt, which is the state the field exists to stop.
            messages_fp: self.messages_fp.clone(),
            over_cap: self.dropped_scopes(),
            scopes: Scopes::default(),
        };
        // One rule, not a ladder: the marker keeps `fp`, which is what stops the
        // next prompt recomposing and re-overflowing for the shell's whole life.
        envelope(&marker).filter(|encoded| encoded.len() <= MAX_CARRIER_BYTES)
    }

    /// The ledger the first prompt of a shell plans against (C-005).
    ///
    /// Required because [`Ledger::decode`] returns `Option` while [`plan`]
    /// takes `&Ledger` — without it the absent-ledger call is unrepresentable.
    pub fn empty() -> Ledger {
        Ledger {
            v: LEDGER_VERSION,
            fp: String::new(),
            ws: String::new(),
            verdict: None,
            tiers: Vec::new(),
            messages_fp: String::new(),
            over_cap: Vec::new(),
            scopes: Scopes::default(),
        }
    }

    /// Every scope this ledger carries a payload for, plus any already named
    /// over-cap, in emission order.
    fn dropped_scopes(&self) -> Vec<ScopeId> {
        let mut scopes = Vec::new();
        if self.scopes.global.is_some() || self.over_cap.contains(&ScopeId::Global) {
            scopes.push(ScopeId::Global);
        }
        if self.scopes.project.is_some() || self.over_cap.contains(&ScopeId::Project) {
            scopes.push(ScopeId::Project);
        }
        scopes
    }

    /// A-02's decode-side half: a carrier claiming `PATH`/`PATHEXT` as a
    /// constant, or carrying a prior for either, is stripped of exactly that
    /// claim. The rest of the record still acts.
    fn discard_forged_path_constants(&mut self) {
        let retain = |applied: &mut Applied| {
            applied.retain(|entry| !(matches!(entry.kind, ModifierKind::Constant) && is_never_constant(&entry.key)));
        };
        if let Some(global) = self.scopes.global.as_mut() {
            retain(global);
        }
        self.scopes.global_priors.retain(|key, _| !is_never_constant(key));
        if let Some(project) = self.scopes.project.as_mut() {
            retain(&mut project.applied);
            project.priors.retain(|key, _| !is_never_constant(key));
        }
    }

    /// The applied entries of both scopes in emission order — global first,
    /// project second (C-018).
    pub(super) fn applied_in_emission_order(&self) -> impl Iterator<Item = &LedgerEntry> {
        let global = self.scopes.global.iter().flatten();
        let project = self.scopes.project.iter().flat_map(|scope| scope.applied.iter());
        global.chain(project)
    }

    /// The recorded pre-apply value for `key` (R1).
    ///
    /// Two sources, and the project one does **not** simply win. A project prior
    /// is captured against the post-global environment (C-018), so where it
    /// holds the exact value the global scope recorded as its own constant for
    /// the same key, it is global's value and not the user's — restoring it on a
    /// two-scope retirement writes back a value no scope declares any more.
    /// In that one case the lookup **chains** to
    /// [`Scopes::global_priors`], which was captured against the pre-global
    /// environment and holds what the user actually had.
    ///
    /// Chaining is safe precisely because it is unreachable while global still
    /// declares the key: `retire_recorded_constant` returns early for a key
    /// `desired` still declares, so a project prior is only ever consulted once
    /// **both** scopes have stopped declaring it.
    ///
    /// Falls back to the global map outright when the project scope has no prior
    /// for the key — the retired-global-constant half of R1 — and to the project
    /// prior when the global scope recorded none, which is what an older
    /// carrier, written before `global_priors` existed, looks like.
    pub(super) fn prior(&self, key: &str) -> Option<&Prior> {
        let global = || self.scopes.global_priors.get(key);
        let Some(project) = self.scopes.project.as_ref().and_then(|scope| scope.priors.get(key)) else {
            return global();
        };
        if self.global_owns(key, project) {
            return global().or(Some(project));
        }
        Some(project)
    }

    /// Whether the global scope recorded `prior`'s value as its own constant for
    /// `key` — i.e. whether the project captured global's value rather than the
    /// user's.
    fn global_owns(&self, key: &str, prior: &Prior) -> bool {
        let Prior::Value(value) = prior else {
            // `Unset` means the key did not exist when the project captured it,
            // so global cannot have been holding it either.
            return false;
        };
        self.scopes.global.iter().flatten().any(|entry| {
            matches!(entry.kind, ModifierKind::Constant)
                && key_eq(&entry.key, key)
                && element_eq(&entry.value, value, &ModifierKind::Constant)
        })
    }
}

fn envelope(ledger: &Ledger) -> Option<String> {
    let json = serde_json::to_vec(ledger).ok()?;
    Some(format!("{ENCODER_TAG}.{}", BASE64_URL.encode(json)))
}

/// The carrier format's own safety net (C-003/C-004): the envelope codec, the
/// 16 KiB cap, and the forged-`PATH` discard.
///
/// These lived in `plan.rs`'s test module, behind a `pub(super) ENCODER_TAG`
/// widened so a sibling could reach it. That left `ledger.rs` — the file that
/// *owns* the format — with no tests of its own, so rewriting plan's test
/// module would have silently deleted the whole safety net for a wire format
/// every shell on the machine parses. The assertion bodies travelled verbatim;
/// only their address changed.
#[cfg(test)]
mod codec_tests {
    use std::path::PathBuf;

    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL;

    use super::*;
    use crate::package::metadata::env::entry::Entry;
    use crate::package::metadata::env::modifier::ModifierKind;

    fn entry(key: &str, value: &str, kind: ModifierKind, separator: Option<&str>) -> Entry {
        Entry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind,
            separator: separator.map(str::to_owned),
        }
    }

    fn path_entry(key: &str, value: &str) -> Entry {
        entry(key, value, ModifierKind::Path, None)
    }

    fn constant(key: &str, value: &str) -> Entry {
        entry(key, value, ModifierKind::Constant, None)
    }

    fn project(applied: Applied, priors: Priors) -> ProjectScope {
        ProjectScope {
            key: "acme-1a2b".to_owned(),
            dir: PathBuf::from("/p1"),
            applied,
            priors,
        }
    }

    fn ledger_with_project(applied: Applied, priors: Priors) -> Ledger {
        Ledger {
            scopes: Scopes {
                global: None,
                global_priors: Priors::new(),
                project: Some(project(applied, priors)),
            },
            ..Ledger::empty()
        }
    }

    /// EC-LEDGER-004 — an unrecognised tag, and a payload carrying no tag at
    /// all, are both absent. The discriminating fixtures are `2.<valid>` and a
    /// bare `<valid>`: the payload *would* decode, so a lenient reader that
    /// sniffed the JSON instead of splitting on the envelope's `.` would return
    /// `Some` for both. That split is what lets a future encoder `2` ship with
    /// no migration and no dual-read window.
    #[test]
    fn c003_s028_every_malformed_carrier_decodes_as_absent() {
        let valid = Ledger::empty().encode().expect("encode");
        let payload = valid.split_once('.').expect("envelope").1;
        assert!(!payload.contains('.'), "`.` is outside the base64url alphabet");
        let cases = vec![
            String::new(),
            "1".to_owned(),
            "1.".to_owned(),
            ".abc".to_owned(),
            "1.!!!not-base64!!!".to_owned(),
            format!("2.{payload}"),
            format!("11.{payload}"),
            format!("x.{payload}"),
            format!("1.{}", &payload[..payload.len() - 1]),
            payload.to_owned(),
        ];
        for case in cases {
            assert!(Ledger::decode(&case).is_none(), "expected absent for {case:?}");
        }
        assert!(
            Ledger::decode(&valid).is_some(),
            "the tagged spelling is the one that decodes"
        );
    }

    #[test]
    fn c003_a004_an_unrecognised_schema_version_decodes_as_absent() {
        let raw = format!("1.{}", BASE64_URL.encode(br#"{"v":99,"fp":"x","scopes":{}}"#));
        assert!(Ledger::decode(&raw).is_none());
    }

    /// EC-LEDGER-003 — a carrier clipped by a platform env-block limit decodes
    /// to nothing and lands in the same corrupt branch as any other garbage: no
    /// prefix of a valid payload is ever partially recovered.
    #[test]
    fn c003_truncating_a_valid_payload_at_any_byte_never_panics() {
        let mut ledger = Ledger::empty();
        ledger.fp = "abcdef0123456789".to_owned();
        ledger.scopes.global = Some(vec![LedgerEntry::from(&path_entry("PATH", "/opt/bin"))]);
        let encoded = ledger.encode().expect("encode");
        for cut in 0..encoded.len() {
            assert!(
                Ledger::decode(&encoded[..cut]).is_none(),
                "a payload clipped at byte {cut} must read as absent, never as a partial record"
            );
        }
    }

    #[test]
    fn c003_a_raw_value_over_the_cap_decodes_as_absent() {
        let oversized = format!("1.{}", "A".repeat(MAX_CARRIER_BYTES));
        assert!(Ledger::decode(&oversized).is_none());
    }

    #[test]
    fn c003_a004_an_unknown_field_inside_a_known_version_is_ignored() {
        let raw = format!(
            "1.{}",
            BASE64_URL.encode(br#"{"v":1,"fp":"x","scopes":{},"future_field":42}"#)
        );
        assert_eq!(Ledger::decode(&raw).expect("decode").fp, "x");
    }

    /// EC-LEDGER-015 — A-04's additive-only rule, asserted where it costs
    /// something: the live priors of a shell whose binary was swapped by
    /// `self update` in another terminal. Both directions must decode at the
    /// same `v` — a payload carrying a field this binary has never heard of,
    /// and one omitting every optional field this binary does know — because a
    /// `v` bump is read as "absent" and would drop the priors in every open
    /// terminal at once, the one direction the ADR only states for
    /// old-reads-new.
    #[test]
    fn c003_a004_an_additive_schema_change_keeps_live_priors_across_a_self_update() {
        let ledger = Ledger {
            fp: "before-update".to_owned(),
            verdict: Some(Verdict::Inert),
            ..ledger_with_project(
                vec![LedgerEntry::from(&constant("JAVA_HOME", "/p1/jdk"))],
                Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/user".to_owned()))]),
            )
        };
        let encoded = ledger.encode().expect("encode");
        let mut payload: serde_json::Value = serde_json::from_slice(
            &BASE64_URL
                .decode(encoded.split_once('.').expect("envelope").1)
                .expect("base64"),
        )
        .expect("json");
        let reseal = |payload: &serde_json::Value| {
            format!(
                "{ENCODER_TAG}.{}",
                BASE64_URL.encode(serde_json::to_vec(payload).expect("serialize"))
            )
        };
        let user = Prior::Value("/user".to_owned());

        // The new binary writes a field this one does not know.
        payload["future_field"] = serde_json::json!(42);
        let from_newer = Ledger::decode(&reseal(&payload)).expect("an added field never needs a `v` bump");
        assert_eq!(from_newer.prior("JAVA_HOME"), Some(&user));

        // The old binary wrote none of the optional fields this one knows.
        let object = payload.as_object_mut().expect("object");
        for optional in ["future_field", "verdict", "tiers", "over_cap"] {
            object.remove(optional);
        }
        let from_older = Ledger::decode(&reseal(&payload)).expect("an existing field is never made required");
        assert_eq!(from_older.prior("JAVA_HOME"), Some(&user));
        assert!(
            from_older.verdict.is_none(),
            "an absent verdict is not a decode failure"
        );
    }

    /// EC-LEDGER-005 — over the cap the carrier is a decodable marker, never an
    /// omitted variable and never a truncated one; the live priors go with the
    /// dropped scope, which is the `unset`-gesture cost reached without asking.
    ///
    /// EC-LEDGER-006 — and the marker is what keeps the *next* prompt able to
    /// tell over-cap from absent: `over_cap` names the abandoned scopes where an
    /// absent ledger names none, so `ocx shell state` can still print the reason.
    #[test]
    fn c004_a001_s027_over_cap_emits_a_decodable_marker_keeping_the_fingerprint() {
        let bulky: Applied = (0..600)
            .map(|index| {
                LedgerEntry::from(&path_entry(
                    "PATH",
                    &format!("/opt/a-very-long-package-directory/{index}/bin"),
                ))
            })
            .collect();
        let ledger = Ledger {
            fp: "deadbeefcafe".to_owned(),
            verdict: None,
            scopes: Scopes {
                global: Some(bulky.clone()),
                global_priors: Priors::new(),
                project: Some(project(
                    bulky,
                    Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/user".to_owned()))]),
                )),
            },
            ..Ledger::empty()
        };
        assert!(ledger.prior("JAVA_HOME").is_some(), "the prior exists before the cap");
        let encoded = ledger.encode().expect("the marker always encodes");
        assert!(encoded.len() <= MAX_CARRIER_BYTES);

        let decoded = Ledger::decode(&encoded).expect("the marker decodes");
        assert_eq!(decoded.fp, "deadbeefcafe");
        assert_eq!(decoded.over_cap, vec![ScopeId::Global, ScopeId::Project]);
        assert!(decoded.scopes.global.is_none());
        assert!(decoded.scopes.project.is_none());
        assert!(
            decoded.prior("JAVA_HOME").is_none(),
            "the priors go with the dropped scope — JAVA_HOME is stuck for the shell's life"
        );
        assert!(
            Ledger::empty().over_cap.is_empty(),
            "an absent ledger names no abandoned scope, so the two states stay distinguishable"
        );
    }

    #[test]
    fn c004_under_the_cap_the_whole_payload_survives() {
        let ledger = ledger_with_project(vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk"))], Priors::new());
        let decoded = Ledger::decode(&ledger.encode().expect("encode")).expect("decode");
        assert_eq!(decoded, ledger);
        assert!(decoded.over_cap.is_empty());
    }

    #[test]
    fn c007b_a002_decode_discards_a_forged_path_constant_and_its_prior() {
        let forged = ledger_with_project(
            vec![
                LedgerEntry {
                    key: "PATHEXT".to_owned(),
                    value: ".COM;.EXE".to_owned(),
                    kind: ModifierKind::Constant,
                    separator: None,
                },
                LedgerEntry::from(&constant("JAVA_HOME", "/jdk")),
            ],
            Priors::from([
                ("PATHEXT".to_owned(), Prior::Value("/attacker".to_owned())),
                ("JAVA_HOME".to_owned(), Prior::Unset),
            ]),
        );
        let decoded = Ledger::decode(&forged.encode().expect("encode")).expect("decode");
        let scope = decoded.scopes.project.expect("project scope survives");
        assert_eq!(scope.applied.len(), 1, "only the forged claim is discarded");
        assert_eq!(scope.applied[0].key, "JAVA_HOME");
        assert!(!scope.priors.contains_key("PATHEXT"));
        assert!(scope.priors.contains_key("JAVA_HOME"));
    }

    /// EC-LEDGER-012 — invariant L-2. The last fixture is a value already run
    /// through the POSIX single-quote escaper: the codec round-trips it byte
    /// for byte rather than unescaping it, which is how a pre-escaped value
    /// stays visible as the producer bug it is. Were one to reach `encode`, an
    /// inheriting shell would double-escape it on the POSIX arm and escape it
    /// correctly on none — a silent, per-value failure, because the arms'
    /// escapers differ. The companion half — that no escaper's output is
    /// *produced* here — is
    /// [`c009_the_encoded_payload_carries_no_shell_quoting`].
    ///
    /// EC-QUOTE-014 — the round-trip half of invariant L-2 (the payload holds
    /// keys, values and kinds, never shell text). The fixture set carries one
    /// value per escaper the arms actually differ on, because a value that
    /// survives one arm's quoting is not evidence about another's: `!` is the
    /// POSIX history-expansion case A-15 turned into a byte corruption once
    /// already, `(`/`)`/`$` are the nushell and PowerShell cases, an embedded
    /// LF is what would split one emitted statement into two, and a non-ASCII
    /// value is the base64url codec's own case.
    #[test]
    fn c008_c009_s024_s026_hostile_values_round_trip_byte_identically() {
        let hostile = [
            "/tmp/a';id;'b",
            "a\"b`c$d\\e%VAR%f",
            "line\u{1}one",
            "trailing\\",
            "$(rm -rf /)",
            "\u{00e9}\u{4e2d}\u{6587}",
            r"/tmp/a'\''b",
            "a!b",
            "/tmp/a(b)$c",
            "first\nsecond",
        ];
        let applied: Applied = hostile
            .iter()
            .enumerate()
            .map(|(index, value)| LedgerEntry::from(&constant(&format!("HOSTILE_{index}"), value)))
            .collect();
        let ledger = ledger_with_project(applied, Priors::new());

        let decoded = Ledger::decode(&ledger.encode().expect("encode")).expect("decode");
        let scope = decoded.scopes.project.expect("scope");
        for (index, value) in hostile.iter().enumerate() {
            assert_eq!(scope.applied[index].value, *value, "byte equality for {value:?}");
        }
    }

    /// EC-QUOTE-014, the other half: no arm's escaper output is ever an
    /// `encode` **input**. Encode one hostile value through
    /// [`crate::shell::escape::posix_single_quoted`] at any call site feeding
    /// the ledger and this reds — a pre-escaped value is double-escaped by an
    /// inheriting POSIX shell and correctly escaped by no arm at all, because
    /// the arms' escapers differ.
    #[test]
    fn c009_the_encoded_payload_carries_no_shell_quoting() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("HOSTILE", "/tmp/a';id;'b"))],
            Priors::new(),
        );
        let encoded = ledger.encode().expect("encode");
        let payload = String::from_utf8(
            BASE64_URL
                .decode(encoded.split_once('.').expect("envelope").1)
                .expect("base64"),
        )
        .expect("utf-8");
        assert!(payload.contains("/tmp/a';id;'b"), "the raw value is stored verbatim");
        assert!(
            !payload.contains("'\\''"),
            "no POSIX single-quote escaping reached the payload"
        );
    }

    #[test]
    fn c012_the_carrier_key_is_inside_the_reserved_namespace() {
        assert_eq!(CARRIER_KEY, "__OCX_ENV_STATE");
        assert!(crate::env::is_reserved_ocx_key(CARRIER_KEY));
        assert!(crate::env::is_valid_env_key(CARRIER_KEY));
    }

    #[test]
    fn c019_the_fingerprint_rides_inside_the_payload() {
        let ledger = Ledger {
            fp: "0a1b2c3d".to_owned(),
            verdict: Some(Verdict::Inert),
            ..Ledger::empty()
        };
        let decoded = Ledger::decode(&ledger.encode().expect("encode")).expect("decode");
        assert_eq!(decoded.fp, "0a1b2c3d");
        assert_eq!(decoded.verdict, Some(Verdict::Inert));
    }

    /// P2 — [`Verdict::NoProject`] round-trips through the carrier, and its wire
    /// spelling is `"noproject"` (the enum's `rename_all = "lowercase"`).
    ///
    /// The spelling is asserted against the decoded JSON rather than inferred,
    /// because it is the byte a binary predating the variant sees.
    #[test]
    fn p2_the_noproject_verdict_round_trips_and_spells_itself_lowercase() {
        let ledger = Ledger {
            fp: "0a1b2c3d".to_owned(),
            verdict: Some(Verdict::NoProject),
            ..Ledger::empty()
        };
        let encoded = ledger.encode().expect("encode");
        assert_eq!(
            Ledger::decode(&encoded).expect("decode").verdict,
            Some(Verdict::NoProject)
        );

        let (_, payload) = encoded.split_once('.').expect("envelope");
        let json = String::from_utf8(BASE64_URL.decode(payload).expect("base64")).expect("utf8");
        assert!(
            json.contains(r#""verdict":"noproject""#),
            "the wire spelling is the byte an older binary reads; got: {json}"
        );
    }

    /// P2 / C-006 — a binary that predates [`Verdict::NoProject`] treats the
    /// whole carrier as **absent**, which is the fail-safe direction: it
    /// recomposes from scratch and re-emits in its own vocabulary. Reachable
    /// only across a `self update` mid-session.
    ///
    /// Asserted by feeding `decode` a payload carrying a verdict this binary
    /// does not know either — the exact position an older binary is in — rather
    /// than by reasoning about one.
    #[test]
    fn p2_c006_a_verdict_an_older_binary_cannot_name_makes_the_ledger_absent() {
        let payload = format!(r#"{{"v":{LEDGER_VERSION},"fp":"x","verdict":"noproject","scopes":{{}}}}"#);
        let unknown = payload.replace("noproject", "somethingnewer");
        let envelope = |json: &str| format!("{ENCODER_TAG}.{}", BASE64_URL.encode(json.as_bytes()));

        assert!(
            Ledger::decode(&envelope(&payload)).is_some(),
            "the control: this binary does know `noproject`"
        );
        assert!(
            Ledger::decode(&envelope(&unknown)).is_none(),
            "an unnameable verdict is a decode failure, and C-006 reads that as no ledger"
        );
    }

    /// A-02 parity — `discard_forged_path_constants` strips `PATH`/`PATHEXT`
    /// from the new global map exactly as it does from the project's, so a
    /// forged carrier cannot make a revert write a whole `PATH`.
    ///
    /// Red state: drop the `global_priors.retain` line and the forged prior
    /// survives the decode.
    #[test]
    fn a002_r1_a_forged_global_prior_for_path_is_discarded_on_decode() {
        let ledger = Ledger {
            scopes: Scopes {
                global: Some(Vec::new()),
                global_priors: Priors::from([
                    ("PATH".to_owned(), Prior::Value("/attacker/bin".to_owned())),
                    ("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm".to_owned())),
                ]),
                project: None,
            },
            ..Ledger::empty()
        };
        let decoded = Ledger::decode(&ledger.encode().expect("encode")).expect("decode");

        assert_eq!(decoded.scopes.global_priors.get("PATH"), None, "A-02 strips it");
        assert_eq!(
            decoded.scopes.global_priors.get("JAVA_HOME"),
            Some(&Prior::Value("/usr/lib/jvm".to_owned())),
            "the rest of the record still acts"
        );
    }

    /// A-04 — `global_priors` is an **optional additive** field, so a carrier
    /// written before it existed still decodes at the same `v`, and one written
    /// with it decodes on a binary that ignores it. Neither direction bumps `v`
    /// or the envelope tag.
    ///
    /// The absent direction is asserted against a hand-built payload that omits
    /// the key entirely — the exact bytes an older binary emits — not against a
    /// struct with an empty map.
    #[test]
    fn a004_r1_a_carrier_without_global_priors_still_decodes() {
        let payload = format!(
            r#"{{"v":{LEDGER_VERSION},"fp":"x","scopes":{{"global":[{{"key":"JAVA_HOME","value":"/global/jdk","type":"constant"}}]}}}}"#
        );
        let raw = format!("{ENCODER_TAG}.{}", BASE64_URL.encode(payload.as_bytes()));

        let decoded = Ledger::decode(&raw).expect("an omitted additive field never needs a `v` bump");
        assert!(decoded.scopes.global_priors.is_empty());
        assert_eq!(
            decoded.prior("JAVA_HOME"),
            None,
            "no prior recorded, nothing to restore"
        );
    }

    /// The empty map is omitted from the wire, so adding the field costs a
    /// shell with no global constants nothing at all against the 16 KiB cap.
    #[test]
    fn a004_r1_an_empty_global_priors_map_is_omitted_from_the_wire() {
        let encoded = Ledger::empty().encode().expect("encode");
        let (_, payload) = encoded.split_once('.').expect("envelope");
        let json = String::from_utf8(BASE64_URL.decode(payload).expect("base64")).expect("utf8");
        assert!(!json.contains("global_priors"), "empty is omitted; got: {json}");
    }

    #[test]
    fn a038_the_cap_is_the_carriers_own_and_nothing_accounts_for_the_env_block() {
        assert_eq!(MAX_CARRIER_BYTES, 16 * 1024);
        let encoded = Ledger::empty().encode().expect("encode");
        assert!(encoded.len() < 64, "an empty ledger is tiny: {encoded}");
    }
}

#[cfg(test)]
mod decode_bounds_tests {
    use super::*;

    /// A carrier holding `count` tier paths, in the envelope shape
    /// [`Ledger::encode`] writes.
    ///
    /// Built through [`envelope`] rather than [`Ledger::encode`] on purpose:
    /// `encode` carries `tiers` into its over-cap marker too, so it refuses an
    /// oversized list outright and cannot produce the input this test is
    /// about. The threat here is not a carrier ocx wrote — it is one a user
    /// hand-set, which is the same door C-012's documented
    /// `unset __OCX_ENV_STATE` repair opens.
    fn carrier_with_tiers(count: usize) -> String {
        let ledger = Ledger {
            tiers: (0..count).map(|n| PathBuf::from(format!("/t/{n}"))).collect(),
            ..Ledger::empty()
        };
        envelope(&ledger).expect("the fixture ledger serializes")
    }

    /// The `tiers` array is bounded independently of the envelope, because it
    /// is the field that turns carrier length into per-prompt `stat` calls.
    ///
    /// Red state: widen the bound in [`Ledger::decode`] to
    /// `MAX_RECORDED_TIERS * 100` and the first assertion sees all 800.
    /// (Deleting the call outright is not the mutation to try — it leaves the
    /// constant dead and the lib fails to build under `-D warnings`, which is
    /// its own evidence that production has exactly one consumer for it.)
    #[test]
    fn a013_a_hand_set_carrier_cannot_grow_the_per_prompt_stat_set() {
        let raw = carrier_with_tiers(800);
        assert!(
            raw.len() <= MAX_CARRIER_BYTES,
            "the envelope cap must not be what bounds this, or the test proves nothing about `tiers`"
        );

        let decoded = Ledger::decode(&raw).expect("an over-long tier list is truncated, never rejected");
        assert_eq!(decoded.tiers.len(), MAX_RECORDED_TIERS);

        // The non-vacuity twin: a legitimate list — five is the most the
        // loader can emit — survives whole, so the cap is a ceiling and not a
        // blanket truncation.
        let decoded = Ledger::decode(&carrier_with_tiers(5)).expect("decodes");
        assert_eq!(
            decoded.tiers,
            (0..5).map(|n| PathBuf::from(format!("/t/{n}"))).collect::<Vec<_>>(),
            "every tier `ConfigLoader` can actually record must survive decode"
        );
    }
}
