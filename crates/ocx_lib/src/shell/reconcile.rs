// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-prompt environment reconciler: the `__OCX_ENV_STATE` ledger, its
//! envelope codec, and the typed three-way [`plan`].
//!
//! The carrier is **untrusted input** (C-007): its only permitted effects are
//! naming the revert set and supplying the equality operand for the exit
//! guard. Nothing here constructs a path from it, re-grants consent, or
//! selects a value for a key it is not reverting.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL;
use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::log;
use crate::package::metadata::env::entry::Entry;
use crate::package::metadata::env::list::DEFAULT_SEPARATOR;
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

/// The structural version of [`Plan`]'s JSON wire shape (A-23).
///
/// Bumps on a breaking reshape, never on an added field. A consumer seeing an
/// absent or unrecognised `v` applies nothing that prompt and returns silently.
pub const PLAN_VERSION: u8 = 1;

/// The size ceiling on the whole `__OCX_ENV_STATE` value, in bytes (C-003, C-004).
pub const MAX_CARRIER_BYTES: usize = 16 * 1024;

/// The envelope tag naming encoder 1: base64url of compact JSON, uncompressed.
const ENCODER_TAG: &str = "1";

/// The keys no scope may ever declare [`ModifierKind::Constant`] for (A-02).
const NEVER_CONSTANT: [&str; 2] = ["PATH", "PATHEXT"];

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
            verdict: self.verdict,
            // The tier list is what makes the next prompt's fingerprint
            // comparable at all, so it survives the cap alongside `fp`.
            tiers: self.tiers.clone(),
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
            verdict: None,
            tiers: Vec::new(),
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
    fn applied_in_emission_order(&self) -> impl Iterator<Item = &LedgerEntry> {
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
    fn prior(&self, key: &str) -> Option<&Prior> {
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

/// The typed three-way diff a single prompt executes (C-011).
///
/// A-23 — the JSON wire shape carries a top-level `"v": 1` on the same envelope
/// discipline as the ledger: `v` is **structural only**, bumping on a breaking
/// reshape and never on an added field, and the nushell consumer applies one
/// rule — `v` absent or unrecognised ⇒ apply nothing this prompt and return
/// silently (C-048).
///
/// The wire shape is `{"v":1,"sets":[…LedgerEntry-shaped…],"removes":[[key,
/// element,sep|null],…],"restores":[[key,value|null],…]}`. `Plan` never
/// contains shell text (C-009) — per-shell rendering stays in `Shell`.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    /// Structural wire version (A-23).
    pub v: u8,
    /// Apply: constants and list/path contributions.
    // `Entry` derives no serde at all, so the wire shape borrows
    // `LedgerEntry`'s — which is also what makes `type` the field name here.
    #[serde(serialize_with = "serialize_sets")]
    pub sets: Vec<Entry>,
    /// Remove one element: `(key, element, separator)`. The separator rides
    /// per element — the whole point of C-014's signature.
    pub removes: Vec<(String, String, Option<String>)>,
    /// Restore a constant: `(key, prior)`, where `None` means unset it.
    pub restores: Vec<(String, Option<String>)>,
}

fn serialize_sets<S>(sets: &[Entry], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.collect_seq(sets.iter().map(LedgerEntry::from))
}

/// Diff desired against current, scoped by the ledger (C-010).
///
/// **Pure**: no I/O, no clock, no env reads beyond `current`, platform-neutral
/// and unit-testable. Called once per scope-stack pass, producing one [`Plan`]
/// covering both scopes in emission order — global first, project second for
/// all three kinds (C-018, A-07).
///
/// `owned_prefixes` is **required, not optional**: both the degradation rule
/// (C-006) and the ownership rule (C-016) make `plan` responsible for repairing
/// lists when the ledger is lost, which needs `$OCX_HOME`. Ownership is
/// component-wise, never a byte prefix (A-09), against the prefixes **as the
/// caller spells them** — A-09 also asked for a canonicalization pass, which is
/// not shipped: an element reached through a symlinked `$OCX_HOME` is foreign to
/// a prefix given in the unresolved spelling and is left alone rather than
/// retired ([ocx-sh/ocx#350](https://github.com/ocx-sh/ocx/issues/350)).
///
/// A-10 — before anything reaches [`Plan`] or the ledger, `plan` drops four
/// classes with one warn-once line each: a key failing
/// [`crate::env::is_valid_env_key`]; a path-kind value containing
/// [`crate::env::PATH_SEPARATOR`]; an empty list or path element; an element
/// containing LF or CR. A-02 — it also refuses [`ModifierKind::Constant`] for
/// `PATH` and `PATHEXT`, compared case-insensitively on Windows.
pub fn plan(desired: &[Entry], current: &Env, ledger: &Ledger, owned_prefixes: &[&Path]) -> Plan {
    let emittable = emittable(desired);
    // `declared` answers rule (b) — "what kind does D give this key *now*" —
    // and `contributed` answers "is this exact element still wanted". Both are
    // keyed by the platform's own key-equality so a Windows `Path`/`PATH` pair
    // is one slot, as `EnvKey` already treats it.
    let declared = declared_index(&emittable);
    let contributed = contributed_elements(&emittable);
    let recorded = recorded_index(ledger);

    let mut plan = Plan {
        v: PLAN_VERSION,
        sets: apply_set(&emittable, &recorded, current),
        removes: Vec::new(),
        restores: Vec::new(),
    };

    for entry in ledger.applied_in_emission_order() {
        match entry.kind {
            ModifierKind::Path | ModifierKind::List => {
                if let Some(removal) = retire_recorded_element(entry, &declared, &contributed) {
                    push_removal(&mut plan.removes, removal);
                }
            }
            ModifierKind::Constant => {
                if let Some(restore) = retire_recorded_constant(entry, &declared, current, ledger) {
                    plan.restores.push(restore);
                }
            }
        }
    }

    for removal in repair_owned_segments(&declared, &recorded, &contributed, current, owned_prefixes) {
        push_removal(&mut plan.removes, removal);
    }

    plan
}

/// Capture the pre-apply values a later revert restores (C-015 rules 3–4, A-05).
///
/// `applied` is the scope's own record and `current` the environment **this
/// scope is about to be applied to** — for the project scope that is the
/// post-global environment (C-018), so a prior captured at project entry holds
/// global's value and reverting the project leaves the global scope intact; for
/// the global scope it is the pre-global environment, which is what makes a
/// retired global constant restorable at all (R1).
///
/// `previous` is the same scope's record from the previous prompt: its applied
/// list and its priors map. Both halves are needed and neither is optional — the
/// applied list answers "is the live value still what ocx wrote", and only if it
/// is does the recorded prior carry forward. Anything else — a genuine `[env]`
/// change, a mid-session override, or a value the user typed that happens to
/// equal D — re-captures, so leaving never unsets a variable the user set by
/// hand. A tuple rather than a scope type because the two scopes store the pair
/// differently: [`ProjectScope`] nests them, [`Scopes`] keeps them as siblings.
///
/// Set-ness, never truthiness: an existing empty variable is [`Prior::Value`]
/// with an empty string and never [`Prior::Unset`] (A-05).
pub fn capture_priors(applied: &[LedgerEntry], current: &Env, previous: Option<(&[LedgerEntry], &Priors)>) -> Priors {
    let mut priors = Priors::new();
    for entry in applied {
        if !matches!(entry.kind, ModifierKind::Constant) {
            continue;
        }
        let observed = current.get(&entry.key).map(os_to_string);
        let carried = previous.and_then(|(previous_applied, previous_priors)| {
            let recorded = previous_applied.iter().find(|candidate| {
                key_eq(&candidate.key, &entry.key) && matches!(candidate.kind, ModifierKind::Constant)
            })?;
            let current_value = observed.as_deref()?;
            element_eq(&recorded.value, current_value, &ModifierKind::Constant)
                .then(|| previous_priors.get(&entry.key))?
                .cloned()
        });
        let prior = carried.unwrap_or(match observed {
            Some(value) => Prior::Value(value),
            None => Prior::Unset,
        });
        priors.insert(entry.key.clone(), prior);
    }
    priors
}

/// Fold the watch set into the ledger's `fp` (C-019, A-13, A-14).
///
/// The **only** definition of what makes an environment stale. `fp` is compared
/// against [`Ledger::fp`]; equal means nothing in the watch set moved, and —
/// together with a cached [`Verdict::Inert`] — that is what makes the per-prompt
/// path stat-only (C-042).
///
/// `watch_paths` is the recorded member list, in order, exactly as the emitted
/// hook body carries it (C-044): the project's `ocx.toml` and `ocx.lock`, the
/// global tier's pair, the managed-config snapshot, the config-tier paths the
/// last `ConfigLoader` pass discovered, and the project's consent stamp. Each is
/// folded with its **presence**, its size and its mtime, so a tier file that did
/// not exist becomes a change the moment it is created. `project_dir` is member
/// 7 — which project the CWD walk resolved — folded as identity, so moving
/// between two projects is a change even when no watched file was touched. The
/// binary version is folded from `CARGO_PKG_VERSION`, so `self update` moves it.
///
/// A-13 — `consent_paths` and `consent_namespaces` are the **raw**
/// `OCX_CONSENT_PATHS` / `OCX_CONSENT_NAMESPACES` values, passed in rather than
/// read here so the fold stays pure and unit-testable without a process-wide env
/// lock. Without them a grant exported from another terminal would never expire
/// the cached `inert` verdict until the shell restarted. Set-but-empty is a
/// distinct state from unset, on the same set-ness rule [`Prior`] follows.
///
/// A-14 — the mtime is the **full** `SystemTime`, never a seconds-truncated
/// value, so the named ceiling ("an unchanged `(mtime, size)` pair is
/// invisible") is the filesystem's own granularity and nothing coarser.
///
/// Blocking: one `stat` per member. Call it from a blocking context — the whole
/// point of C-044 is that this is cheaper than the exec that reaches it.
pub fn fingerprint(
    watch_paths: &[PathBuf],
    project_dir: Option<&Path>,
    consent_paths: Option<&str>,
    consent_namespaces: Option<&str>,
) -> String {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    fold(&mut hasher, "ocx", env!("CARGO_PKG_VERSION").as_bytes());
    fold_optional(
        &mut hasher,
        "dir",
        project_dir.map(|dir| dir.as_os_str().as_encoded_bytes()),
    );

    for path in watch_paths {
        fold(&mut hasher, "path", path.as_os_str().as_encoded_bytes());
        // `metadata`, not `symlink_metadata`: the shell-side newer-than test
        // this fold has to agree with follows symlinks too (C-044).
        match std::fs::metadata(path) {
            Ok(meta) => {
                fold(&mut hasher, "present", &[1]);
                fold(&mut hasher, "size", &meta.len().to_le_bytes());
                fold(&mut hasher, "mtime", &mtime_bytes(&meta));
            }
            // Presence is a member in its own right — an absent tier file that
            // appears must read as a change, not as "nothing to compare".
            Err(_) => fold(&mut hasher, "present", &[0]),
        }
    }

    fold_optional(&mut hasher, "consent_paths", consent_paths.map(str::as_bytes));
    fold_optional(&mut hasher, "consent_namespaces", consent_namespaces.map(str::as_bytes));

    hex::encode(hasher.finalize())
}

/// [`fingerprint`] over this process's own `OCX_CONSENT_*` environment.
///
/// The two env reads happen **here**, at the one seam every consumer shares —
/// `ocx self activate --reconcile`, which folds the fingerprint it records, and
/// `ocx shell state`, which folds the one it reports — so the fold itself stays
/// pure and unit-testable without a process-wide env lock (A-13). Forgetting one
/// would silently make the cached `inert` verdict unexpirable, and a second copy
/// of this wrapper would let exactly that happen in one consumer and not the
/// other: the reporter would then print a fingerprint the reconciler never
/// computes.
///
/// Blocking: [`fingerprint`]'s one `stat` per member. Call it from a blocking
/// context.
#[must_use]
pub fn current_fingerprint(watch_paths: &[PathBuf], project_dir: Option<&Path>) -> String {
    fingerprint(
        watch_paths,
        project_dir,
        crate::env::var(crate::config::shell::OCX_CONSENT_PATHS).as_deref(),
        crate::env::var(crate::config::shell::OCX_CONSENT_NAMESPACES).as_deref(),
    )
}

/// The watch set's member paths, in the order [`fingerprint`] folds them and
/// the emitted hook body carries them (C-019, C-044, A-13).
///
/// **Candidates, not survivors**: a path that does not exist is a member too,
/// because one becoming present is exactly the change the watch set must
/// notice. Discovery happens here — during the shell-start `ConfigLoader` pass
/// and again only when a recomposition is already due — so the per-prompt path
/// stats this recorded list and parses nothing (C-042).
///
/// One definition, deliberately: the emitted hook body, the fingerprint fold and
/// `ocx shell state`'s evidence table all read this list, and two definitions of
/// *"what makes the environment stale"* drift.
pub fn watch_paths(
    file_structure: &crate::file_structure::FileStructure,
    project_dir: Option<&Path>,
    project_key: Option<&str>,
    recorded_tiers: Option<&[PathBuf]>,
) -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(9);

    // Members 1-2 — the project tier. `[env]` applies on its own authority
    // independently of the lock, so watching locks alone would miss an
    // `[env]`-only edit.
    if let Some(dir) = project_dir {
        paths.push(dir.join("ocx.toml"));
        paths.push(dir.join("ocx.lock"));
    }

    // Members 3-5 — the global tier and the managed-config snapshot.
    let home = file_structure.root();
    paths.push(home.join("ocx.toml"));
    paths.push(home.join("ocx.lock"));
    paths.push(file_structure.state.managed_config_snapshot_file());

    // Member 6's *observable* half. `CARGO_PKG_VERSION` alone does not move on
    // this project's floating `<version>-dev` channel — `self update` swaps
    // `current` to a different binary carrying the same version string — so the
    // binary the `current` symlink resolves to is watched directly. Its mtime
    // and size move whenever the symlink is repointed.
    paths.push(
        file_structure
            .symlinks
            .current(&crate::oci::ocx_cli_identifier())
            .join("content")
            .join("bin"),
    );

    // Member 8 — the config tiers (A-13, A-33).
    //
    // The **recorded** list wins whenever there is one: it came from
    // `LoadedConfig::config_tier_paths`, which honours `OCX_NO_CONFIG` and
    // includes the `--config` overlay that a per-prompt process cannot see for
    // itself. Re-deriving is the fallback for the one run that has no record
    // yet, and it is deliberately the *same* arithmetic the loader uses.
    match recorded_tiers {
        Some(recorded) => paths.extend(recorded.iter().cloned()),
        None => {
            if !crate::env::flag("OCX_NO_CONFIG", false) {
                paths.push(crate::config::loader::ConfigLoader::system_path());
                paths.extend(crate::config::loader::ConfigLoader::user_path());
                paths.extend(crate::config::loader::ConfigLoader::home_path());
            }
            if let Some(explicit) = crate::env::var(crate::env::keys::OCX_CONFIG).filter(|value| !value.is_empty()) {
                paths.push(PathBuf::from(explicit));
            }
        }
    }

    // Member 9 — the project's consent stamp. Without it a grant written from
    // another terminal never expires the cached `inert` verdict.
    if let Some(key) = project_key {
        paths.push(file_structure.state.consent_stamp_file(key));
    }

    paths
}

/// Absorb one named member, length-prefixed so two different member lists can
/// never collide by concatenating to the same byte stream.
fn fold(hasher: &mut sha2::Sha256, tag: &str, bytes: &[u8]) {
    use sha2::Digest as _;
    hasher.update(tag.as_bytes());
    hasher.update([0u8]);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// [`fold`] with an explicit presence byte, so unset and set-but-empty differ.
fn fold_optional(hasher: &mut sha2::Sha256, tag: &str, bytes: Option<&[u8]>) {
    match bytes {
        Some(bytes) => {
            fold(hasher, tag, &[1]);
            fold(hasher, tag, bytes);
        }
        None => fold(hasher, tag, &[0]),
    }
}

/// The full modification time, sign byte first so a pre-epoch mtime folds
/// distinctly from its post-epoch mirror (A-14 — never seconds-truncated).
fn mtime_bytes(meta: &std::fs::Metadata) -> Vec<u8> {
    // A filesystem with no modification time contributes the empty member
    // rather than a fabricated one; presence and size still fold above.
    let Ok(modified) = meta.modified() else {
        return Vec::new();
    };
    let (sign, delta) = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(delta) => (1u8, delta),
        Err(before_epoch) => (0u8, before_epoch.duration()),
    };
    let mut bytes = Vec::with_capacity(13);
    bytes.push(sign);
    bytes.extend_from_slice(&delta.as_secs().to_le_bytes());
    bytes.extend_from_slice(&delta.subsec_nanos().to_le_bytes());
    bytes
}

// ---------------------------------------------------------------------------
// Planner internals
// ---------------------------------------------------------------------------

/// A-10's gate. Every emitter returns `None` for an invalid key, so without
/// this L would carry a key no arm can ever remove; `L ⊆ emittable(D)` is the
/// invariant it buys. Warned once per key so a repeated contribution does not
/// print per occurrence.
///
/// **One admission rule, in one place** (E6): the per-entry predicate is
/// [`crate::shell::is_emittable`], shared with `conventions::emit_lines` so the
/// export path and the reconciler cannot drift. Two copies of an admission rule
/// drift, and the export path had none at all — a `type = "path"` value
/// embedding the separator grew `PATH` without bound on ksh, dash and pwsh.
///
/// A-02's "`PATH`/`PATHEXT` are never constant-kind" stays **here**, deliberately.
/// It is a *revert*-shaped rule and not an emit-shaped one: every arm can
/// perfectly well emit `export PATH=...`, and the refusal exists because a
/// forged or mistaken constant claim on those two keys makes the whole variable
/// ocx's to overwrite. Only the ledger has that stake, so it is the reconciler's
/// rule, not the emitters'.
fn emittable(desired: &[Entry]) -> Vec<&Entry> {
    let mut warned: HashSet<(&str, &str)> = HashSet::new();
    let mut kept = Vec::with_capacity(desired.len());
    for entry in desired {
        match admits(entry) {
            Err(reason) => {
                if warned.insert((entry.key.as_str(), reason)) {
                    log::warn!("ignoring env entry '{}': {reason}", entry.key);
                }
            }
            Ok(()) => kept.push(entry),
        }
    }
    kept
}

/// The admission rule itself, so the two consumers cannot hold different ones.
fn admits(entry: &Entry) -> Result<(), &'static str> {
    if matches!(entry.kind, ModifierKind::Constant) && is_never_constant(&entry.key) {
        Err("PATH and PATHEXT are never constant-kind")
    } else {
        crate::shell::is_emittable(entry)
    }
}

/// [`emittable`] without its logging, for the ledger builder.
///
/// `L ⊆ emittable(D)` (A-10) is a property of the **ledger**, and the ledger is
/// built next to `plan`, not inside it — so the gate has a second consumer, and
/// that consumer must not print `plan`'s warn-once lines a second time on the
/// same prompt. The stake is the ledger's alone: an entry no arm emits, recorded
/// as applied, is a key ocx claims to own and can never remove — and for
/// `PATH`/`PATHEXT` a refused constant claim recorded anyway hands the whole
/// variable's restore to a prior that was never captured from an apply.
pub fn emittable_entries(desired: &[Entry]) -> Vec<&Entry> {
    desired.iter().filter(|entry| admits(entry).is_ok()).collect()
}

/// C-015 rules 0 and 1 — **nothing is set where applying it would change
/// nothing**, so a quiet prompt emits an empty plan and the reconciler has a
/// fixed point.
///
/// Two rules, because the two kinds settle against different things:
///
/// - **Rule 1, a constant**, compares against the *ledger*: ocx re-asserts it
///   only where the composed value moved since it last wrote it, so a
///   mid-session override survives every recompose of an unchanged value.
///   Comparing against the live environment instead would re-emit over exactly
///   that override.
/// - **Rule 0, a path or list key**, compares against the *live environment*,
///   through [`settled_keys`]. Its application is idempotent, so re-emitting is
///   harmless — but not free: each re-emitted entry is a `while`-loop of
///   in-shell string surgery over the user's whole `PATH`, on every prompt,
///   forever ([ocx-sh/ocx#342](https://github.com/ocx-sh/ocx/issues/342)).
///
/// This is what makes `plan` depend on `current` for path kinds. It costs the
/// lost-ledger repair (C-006) nothing: [`repair_owned_segments`] reads `current`
/// directly and is unaffected, and a key whose fold is already live needs no
/// repair by definition.
fn apply_set(emittable: &[&Entry], recorded: &BTreeMap<String, &LedgerEntry>, current: &Env) -> Vec<Entry> {
    let settled = settled_keys(emittable, current);
    emittable
        .iter()
        .filter(|entry| match entry.kind {
            ModifierKind::Constant => !recorded.get(&key_norm(&entry.key)).is_some_and(|previous| {
                matches!(previous.kind, ModifierKind::Constant)
                    && element_eq(&previous.value, &entry.value, &ModifierKind::Constant)
            }),
            ModifierKind::Path | ModifierKind::List => !settled.contains(&key_norm(&entry.key)),
        })
        .map(|entry| (*entry).clone())
        .collect()
}

/// The path/list keys whose whole fold is **already live**, under the
/// comparison rule the key's kind gives it ([`value_settled`]).
///
/// Answered by folding `emittable` into a copy of `current` with
/// [`Env::apply_entries`] and asking which keys came back unchanged — never by
/// re-deriving the ordering rule here. That is the whole point: `apply_entries`
/// is the same [`move_to_front`](crate::utility::path::move_to_front) /
/// [`append_unique`](crate::utility::list::append_unique) fold the emitted shell
/// arms are contracted to equal byte for byte, so "the in-process fold changes
/// nothing" *is* "the emitted lines would change nothing". A second copy of the
/// prepend-and-dedupe rule would be a second thing to drift.
///
/// Settling is **per key and all-or-nothing**: a key settles only when the whole
/// group of entries contributing to it is a no-op together, which is what makes
/// dropping them all safe. A key any scope declares [`ModifierKind::Constant`]
/// for never settles here — its rule is C-015 rule 1's ledger comparison in
/// [`apply_set`], and deciding it from `current` would clobber a mid-session
/// override.
fn settled_keys(emittable: &[&Entry], current: &Env) -> HashSet<String> {
    let mut candidates: BTreeMap<String, (&str, ModifierKind)> = BTreeMap::new();
    let mut constants: HashSet<String> = HashSet::new();
    for entry in emittable {
        let norm = key_norm(&entry.key);
        match entry.kind {
            ModifierKind::Constant => {
                constants.insert(norm);
            }
            ModifierKind::Path | ModifierKind::List => {
                let slot = candidates
                    .entry(norm)
                    .or_insert((entry.key.as_str(), entry.kind.clone()));
                // Where one key carries both kinds, the *narrower* rule decides
                // it: a list element is opaque and compares byte-exact on every
                // platform (A-19 E5), so comparing such a key segment-wise
                // could call two spellings settled that the emitter would have
                // rewritten. Refusing to settle only re-emits, which is
                // idempotent; over-settling silently drops a change.
                if matches!(entry.kind, ModifierKind::List) {
                    slot.1 = ModifierKind::List;
                }
            }
        }
    }
    if candidates.is_empty() {
        return HashSet::new();
    }

    let folded: Vec<Entry> = emittable.iter().map(|entry| (*entry).clone()).collect();
    let mut probe = current.clone();
    probe.apply_entries(&folded);

    candidates
        .into_iter()
        .filter(|(norm, (key, kind))| {
            !constants.contains(norm) && value_settled(probe.get(key), current.get(key), kind)
        })
        .map(|(norm, _)| norm)
        .collect()
}

/// Whether the folded value and the live one are the **same value under the
/// kind's own comparison rule** — never `==` on the raw whole value.
///
/// A byte-exact whole-value compare has no fixed point on Windows. The fold
/// re-joins segments that came out of `std::env::split_paths`, which unquotes
/// there — the premise [`element_eq`] already states — so a retained
/// `"C:\Program Files\x"` comes back stripped, the compare is false forever, and
/// [#342](https://github.com/ocx-sh/ocx/issues/342) stays unfixed on exactly the
/// platform that quotes. The emitted pwsh arm keeps such a segment byte for byte
/// and normalises only what it compares against, so the honest question is
/// A-19's, and [`element_eq`] is the one place A-19 lives.
fn value_settled(folded: Option<&OsStr>, live: Option<&OsStr>, kind: &ModifierKind) -> bool {
    match (folded, live) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        // The unquoting is per segment, so the comparison has to be too.
        (Some(folded), Some(live)) => match kind {
            ModifierKind::Path => path_segments_eq(folded, live),
            // A list element compares byte-exact on every platform, and joining
            // on one separator is injective — so the whole value under that rule
            // *is* `OsStr` equality, without a lossy round-trip through `str`.
            // A constant never reaches here (`settled_keys` excludes it); the
            // arm keeps the match total, and its rule is the same byte-exact one
            // for the whole value.
            ModifierKind::List | ModifierKind::Constant => folded == live,
        },
    }
}

/// Segment-wise [`element_eq`] under [`ModifierKind::Path`], over both values
/// split by the platform's own `PATH` splitter.
fn path_segments_eq(left: &OsStr, right: &OsStr) -> bool {
    let mut left = std::env::split_paths(left);
    let mut right = std::env::split_paths(right);
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(left), Some(right)) if path_segment_eq(&left, &right) => continue,
            // Unequal segments, or one value out of segments before the other.
            _ => return false,
        }
    }
}

fn path_segment_eq(left: &Path, right: &Path) -> bool {
    // Byte-identical first, so an unchanged non-UTF-8 segment still settles. A
    // *changed* one falls to `false`: a segment no arm can name is one no
    // comparison can normalise, and re-emitting is the harmless direction.
    left == right
        || match (left.to_str(), right.to_str()) {
            (Some(left), Some(right)) => element_eq(left, right, &ModifierKind::Path),
            _ => false,
        }
}

/// C-016/C-017 — an element L records and D no longer wants is removed. Rule
/// (b) re-derives the kind and separator from D wherever D still declares the
/// key, so L's copies decide nothing but membership of the revert set.
fn retire_recorded_element(
    entry: &LedgerEntry,
    declared: &BTreeMap<String, &Entry>,
    contributed: &HashSet<(String, String)>,
) -> Option<(String, String, Option<String>)> {
    let current = declared.get(&key_norm(&entry.key));
    // Rule (b) again, this time for the *comparison*: wherever D still declares
    // the key, D's kind decides how its elements compare, so the membership test
    // here and the entry `contributed_elements` recorded were normalised the
    // same way. Where D declares nothing, L's own kind is all there is — and a
    // kind switch therefore misses, which retires the old element and lets the
    // new kind apply its own, the safe direction.
    let kind = current.map_or(&entry.kind, |declared| &declared.kind);
    if contributed.contains(&(key_norm(&entry.key), element_norm(&entry.value, kind))) {
        return None;
    }
    let separator = match current {
        // A constant overwrite retires the whole variable; removing an element
        // of it first would be a no-op the emitters still have to render.
        Some(current) => match current.kind {
            ModifierKind::Constant => return None,
            ModifierKind::Path => None,
            ModifierKind::List => Some(effective_separator(current)),
        },
        None => match entry.kind {
            ModifierKind::Path => None,
            _ => Some(entry.separator.clone().unwrap_or_else(|| DEFAULT_SEPARATOR.to_owned())),
        },
    };
    Some((entry.key.clone(), entry.value.clone(), separator))
}

/// C-015 rule 2 + C-017 — a constant L records and D no longer declares is
/// reverted to its recorded prior, never discarded, and only while the current
/// value is still what ocx wrote. A key with no recorded prior is left alone:
/// C-006 forbids guess-unsetting a constant, and "restore the recorded prior"
/// has no operand without one.
fn retire_recorded_constant(
    entry: &LedgerEntry,
    declared: &BTreeMap<String, &Entry>,
    current: &Env,
    ledger: &Ledger,
) -> Option<(String, Option<String>)> {
    if is_never_constant(&entry.key) || declared.contains_key(&key_norm(&entry.key)) {
        return None;
    }
    let observed = current.get(&entry.key).map(os_to_string)?;
    if !element_eq(&observed, &entry.value, &ModifierKind::Constant) {
        return None;
    }
    match ledger.prior(&entry.key)? {
        Prior::Unset => Some((entry.key.clone(), None)),
        Prior::Value(value) => Some((entry.key.clone(), Some(value.clone()))),
    }
}

/// C-016's structural half, and the whole of the lost-ledger repair (C-006).
///
/// **Subtractive, and the wording is load-bearing**: remove every prefix-owned
/// segment of C that D does not want, rather than merely ensuring D's segments
/// are in front. The additive reading leaves both `…/packages/<old>/bin` and
/// `…/packages/<new>/bin` on PATH after a digest bump — different strings, so
/// move-to-front reorders rather than dedupes. Segments are enumerated as they
/// appear in C and named verbatim in the removal, so selection and removal
/// share one byte-exact operand (A-09).
fn repair_owned_segments(
    declared: &BTreeMap<String, &Entry>,
    recorded: &BTreeMap<String, &LedgerEntry>,
    contributed: &HashSet<(String, String)>,
    current: &Env,
    owned_prefixes: &[&Path],
) -> Vec<(String, String, Option<String>)> {
    let mut keys: BTreeSet<(String, &str)> = BTreeSet::new();
    for entry in declared.values() {
        if matches!(entry.kind, ModifierKind::Path) {
            keys.insert((key_norm(&entry.key), entry.key.as_str()));
        }
    }
    for entry in recorded.values() {
        if matches!(entry.kind, ModifierKind::Path) && !declared.contains_key(&key_norm(&entry.key)) {
            keys.insert((key_norm(&entry.key), entry.key.as_str()));
        }
    }

    let mut removals = Vec::new();
    for (norm, key) in keys {
        let Some(value) = current.get(key) else { continue };
        for segment in std::env::split_paths(value) {
            let segment = segment.into_os_string();
            if segment.is_empty() || !is_owned(&segment, owned_prefixes) {
                continue;
            }
            // A segment no arm can name is a segment no arm can remove; leaving
            // it is the only honest outcome.
            let Some(segment) = segment.to_str() else { continue };
            // Every key in this loop is path-kind by construction, so the
            // segment compares under A-19 and nothing else.
            if contributed.contains(&(norm.clone(), element_norm(segment, &ModifierKind::Path))) {
                continue;
            }
            removals.push((key.to_owned(), segment.to_owned(), None));
        }
    }
    removals
}

fn declared_index<'a>(emittable: &[&'a Entry]) -> BTreeMap<String, &'a Entry> {
    // Later wins, matching the emission order the caller hands in: project's
    // declaration of a key overrides global's.
    emittable.iter().map(|entry| (key_norm(&entry.key), *entry)).collect()
}

fn recorded_index(ledger: &Ledger) -> BTreeMap<String, &LedgerEntry> {
    ledger
        .applied_in_emission_order()
        .map(|entry| (key_norm(&entry.key), entry))
        .collect()
}

/// The elements D still wants, each normalised under **its own kind** — a list
/// element byte-exact, a path element under A-19.
fn contributed_elements(emittable: &[&Entry]) -> HashSet<(String, String)> {
    emittable
        .iter()
        .filter(|entry| matches!(entry.kind, ModifierKind::Path | ModifierKind::List))
        .map(|entry| (key_norm(&entry.key), element_norm(&entry.value, &entry.kind)))
        .collect()
}

/// Push a removal unless the same removal is already planned.
///
/// "The same" is key, element **and separator**: the separator is what names the
/// kind, so two removals that disagree on it render through different emitter
/// arms and neither can stand in for the other. Only once they agree is the
/// element comparison unambiguous — a `None` separator is path-kind, `Some` is
/// list-kind.
fn push_removal(removes: &mut Vec<(String, String, Option<String>)>, removal: (String, String, Option<String>)) {
    let kind = removal_kind(&removal.2);
    let seen = removes.iter().any(|(key, value, separator)| {
        key_eq(key, &removal.0) && *separator == removal.2 && element_eq(value, &removal.1, &kind)
    });
    if !seen {
        removes.push(removal);
    }
}

/// C-014's signature carries the kind in the separator: `None` is path-kind and
/// means the platform path separator, `Some` is list-kind.
fn removal_kind(separator: &Option<String>) -> ModifierKind {
    match separator {
        None => ModifierKind::Path,
        Some(_) => ModifierKind::List,
    }
}

// ---------------------------------------------------------------------------
// Comparison and ownership primitives
// ---------------------------------------------------------------------------

/// A-09 — component-wise, never a byte prefix, so `.ocx-backup` and `.ocxevil`
/// are foreign to an `$OCX_HOME` of `.ocx`.
fn is_owned(segment: &OsStr, owned_prefixes: &[&Path]) -> bool {
    let segment = Path::new(segment);
    owned_prefixes.iter().any(|prefix| segment.starts_with(prefix))
}

/// The comparison rule for a value, **selected by the kind that wrote it**.
///
/// The two rules are the emitters' own, and the planner must not be wider than
/// the arm that will render its decision — a comparison that calls two spellings
/// equal suppresses a removal the emitter would have applied byte-exact, and the
/// variable then accumulates both.
///
/// - [`ModifierKind::List`] — **byte-exact, case-sensitive on every platform**
///   ([`crate::shell::Shell::remove_list_element`], [`crate::shell::Shell::export_list`]).
///   A list element is an opaque option string: `-DFOO=1` and `-Dfoo=1` are
///   different options, and a `"` inside one is part of the option, never a
///   quoting artefact.
/// - [`ModifierKind::Path`] — A-19: segment-exact after stripping one
///   surrounding pair of `"` (`std::env::split_paths` unquotes on Windows, so
///   the operand ocx sees may carry a pair its own emit did not write),
///   case-sensitive on Unix and ASCII-case-insensitive on Windows.
/// - [`ModifierKind::Constant`] — the `C == L.applied` exit guard, which the ADR
///   pins to the same predicate as A-19 (ASCII-case-insensitive on Windows).
///
/// The stored string is never normalised — only the comparison is (C-008).
fn element_eq(left: &str, right: &str, kind: &ModifierKind) -> bool {
    match kind {
        ModifierKind::List => left == right,
        ModifierKind::Path | ModifierKind::Constant => {
            let (left, right) = (unquote(left), unquote(right));
            if cfg!(windows) {
                left.eq_ignore_ascii_case(right)
            } else {
                left == right
            }
        }
    }
}

/// The hashable form of [`element_eq`]'s equivalence class, under the same kind.
fn element_norm(value: &str, kind: &ModifierKind) -> String {
    match kind {
        ModifierKind::List => value.to_owned(),
        ModifierKind::Path | ModifierKind::Constant => {
            let value = unquote(value);
            if cfg!(windows) {
                value.to_ascii_lowercase()
            } else {
                value.to_owned()
            }
        }
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value)
}

/// Key equality as `EnvKey` already defines it: case-insensitive on Windows,
/// where `$env:Path` and `$env:PATH` are one variable, exact elsewhere.
fn key_eq(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn key_norm(key: &str) -> String {
    if cfg!(windows) {
        key.to_ascii_uppercase()
    } else {
        key.to_owned()
    }
}

fn is_never_constant(key: &str) -> bool {
    NEVER_CONSTANT.iter().any(|reserved| key_eq(key, reserved))
}

fn effective_separator(entry: &Entry) -> String {
    entry.separator.clone().unwrap_or_else(|| DEFAULT_SEPARATOR.to_owned())
}

fn os_to_string(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

fn envelope(ledger: &Ledger) -> Option<String> {
    let json = serde_json::to_vec(ledger).ok()?;
    Some(format!("{ENCODER_TAG}.{}", BASE64_URL.encode(json)))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn env_with(pairs: &[(&str, &str)]) -> Env {
        let mut env = Env::clean();
        for (key, value) in pairs {
            env.set(*key, *value);
        }
        env
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

    fn sep(value: &str) -> String {
        value.to_owned()
    }

    /// Apply a [`Plan`] to an env the way the in-process path does, so a tier-1
    /// assertion can be made against a resulting variable rather than against
    /// the plan's own shape.
    fn apply_in_process(env: &mut Env, plan: &Plan) {
        for (key, element, separator) in &plan.removes {
            let Some(existing) = env.get(key).map(std::ffi::OsString::from) else {
                continue;
            };
            let updated = match separator {
                None => crate::utility::path::remove_segment(&existing, OsStr::new(element.as_str())),
                Some(separator) => {
                    let existing = existing.to_string_lossy().into_owned();
                    let kept: Vec<&str> = existing
                        .split(separator.as_str())
                        .filter(|part| !element_eq(part, element, &ModifierKind::List))
                        .collect();
                    std::ffi::OsString::from(kept.join(separator))
                }
            };
            env.set(key.as_str(), updated);
        }
        for (key, value) in &plan.restores {
            match value {
                Some(value) => env.set(key.as_str(), value.as_str()),
                None => env.remove(key),
            }
        }
        env.apply_entries(&plan.sets);
    }

    fn path_segments(env: &Env) -> Vec<String> {
        env.get("PATH")
            .map(|value| {
                std::env::split_paths(value)
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    // -- C-001: LedgerEntry wire shape ------------------------------------

    #[test]
    fn c001_wire_field_is_type_and_separator_is_omitted_when_none() {
        let recorded = LedgerEntry::from(&path_entry("PATH", "/opt/bin"));
        let json: serde_json::Value = serde_json::to_value(&recorded).expect("serialize");
        let keys: Vec<&str> = json.as_object().expect("object").keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["key", "value", "type"]);
        assert_eq!(json["type"], "path");
    }

    #[test]
    fn c001_the_wrong_spelling_kind_fails_to_deserialize() {
        let raw = r#"{"key":"PATH","value":"/a","kind":"path"}"#;
        assert!(serde_json::from_str::<LedgerEntry>(raw).is_err());
    }

    #[test]
    fn c001_a008_list_separator_is_always_some_and_defaults_to_a_space() {
        let defaulted = LedgerEntry::from(&entry("GOFLAGS", "-mod=vendor", ModifierKind::List, None));
        assert_eq!(defaulted.separator.as_deref(), Some(DEFAULT_SEPARATOR));
        let declared = LedgerEntry::from(&entry("CLASSPATH", "/a.jar", ModifierKind::List, Some(":")));
        assert_eq!(declared.separator.as_deref(), Some(":"));
    }

    #[test]
    fn c001_a008_path_kind_separator_stays_none() {
        assert!(LedgerEntry::from(&path_entry("PATH", "/opt/bin")).separator.is_none());
        assert!(LedgerEntry::from(&constant("JAVA_HOME", "/jdk")).separator.is_none());
    }

    // -- C-003 / S-028: decode is total, every failure is "absent" ---------

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

    // -- C-004 / A-01 / S-027: the over-cap marker -------------------------

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
    fn c004_a001_an_over_cap_scope_is_reconciled_exactly_as_an_absent_one() {
        let marker = Ledger {
            fp: "fp".to_owned(),
            over_cap: vec![ScopeId::Project],
            ..Ledger::empty()
        };
        let current = env_with(&[("PATH", "/home/u/.ocx/packages/old/bin:/usr/bin")]);
        let ocx_home = Path::new("/home/u/.ocx");
        let desired = vec![path_entry("PATH", "/home/u/.ocx/packages/new/bin")];

        let planned = plan(&desired, &current, &marker, &[ocx_home]);
        assert_eq!(
            planned.removes,
            vec![("PATH".to_owned(), "/home/u/.ocx/packages/old/bin".to_owned(), None)]
        );
    }

    // -- C-005 / S-042: the empty ledger -----------------------------------

    #[test]
    fn c005_s042_the_first_prompt_plans_against_an_empty_ledger_and_reverts_nothing() {
        let empty = Ledger::empty();
        assert_eq!(empty.v, LEDGER_VERSION);
        assert!(empty.fp.is_empty());
        assert!(empty.verdict.is_none());
        assert!(empty.scopes.global.is_none() && empty.scopes.project.is_none());

        let current = env_with(&[("PATH", "/usr/bin")]);
        let desired = vec![
            path_entry("PATH", "/home/u/.ocx/packages/new/bin"),
            constant("JAVA_HOME", "/jdk"),
        ];
        let planned = plan(&desired, &current, &empty, &[Path::new("/home/u/.ocx")]);

        assert!(planned.removes.is_empty());
        assert!(planned.restores.is_empty());
        assert_eq!(planned.sets.len(), 2);
    }

    // -- C-006 / S-021: degradation leaves constants alone ------------------

    #[test]
    fn c006_s021_a_lost_ledger_repairs_lists_subtractively_and_leaves_constants() {
        let current = env_with(&[
            ("PATH", "/home/u/.ocx/packages/old/bin:/usr/bin"),
            ("JAVA_HOME", "/home/u/.ocx/packages/old"),
        ]);
        let desired = vec![path_entry("PATH", "/home/u/.ocx/packages/new/bin")];
        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/home/u/.ocx")]);

        assert!(planned.restores.is_empty(), "a repair never guess-unsets a constant");
        assert_eq!(
            planned.removes,
            vec![("PATH".to_owned(), "/home/u/.ocx/packages/old/bin".to_owned(), None)]
        );
    }

    // -- C-007 / A-02 / A-03 / A-06: the forgery rules ---------------------

    /// EC-LEDGER-007 — a forged `kind`: `PATH` claimed as a constant with an
    /// attacker prior, in a shell where **no** scope declares `PATH`. Rule (b)
    /// has no operand there, so A-02's producer rule is what closes it: ocx
    /// never writes `PATH`/`PATHEXT` as constant-kind, so the claim is
    /// inconsistent with its own producer and the restore never fires.
    #[test]
    fn c007b_a002_a_forged_path_constant_never_becomes_a_restore() {
        let forged = ledger_with_project(
            vec![LedgerEntry {
                key: "PATH".to_owned(),
                value: "/usr/bin".to_owned(),
                kind: ModifierKind::Constant,
                separator: None,
            }],
            Priors::from([("PATH".to_owned(), Prior::Value("/attacker/bin".to_owned()))]),
        );
        let current = env_with(&[("PATH", "/usr/bin")]);

        let planned = plan(&[], &current, &forged, &[Path::new("/home/u/.ocx")]);
        assert!(
            planned.restores.is_empty(),
            "PATH is never constant-kind on either direction"
        );
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

    #[test]
    fn c007a_a003_a_forged_dir_still_reverts_the_recorded_scope() {
        // `dir` is advisory: the caller has already left the scope (D no longer
        // names JAVA_HOME), so the revert set is L-scoped regardless of what
        // `dir` claims, and no path is ever built from it.
        let mut ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/p1/jdk"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm".to_owned()))]),
        );
        ledger.scopes.project.as_mut().expect("scope").dir = PathBuf::from("../../../etc");
        let current = env_with(&[("JAVA_HOME", "/p1/jdk")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.restores,
            vec![("JAVA_HOME".to_owned(), Some("/usr/lib/jvm".to_owned()))]
        );
    }

    /// EC-LEDGER-008 — a forged `key` such as `../../../../etc` is inert. Both
    /// identity labels are re-derived from the CWD walk every prompt and
    /// neither may reach a path constructor; the observable a pure planner
    /// offers is that its whole output is invariant under them, which is what
    /// reds the moment either field starts selecting anything.
    #[test]
    fn c007a_a003_a_forged_key_never_reaches_a_path_constructor() {
        let planned = |key: &str, dir: &str| {
            let mut ledger = ledger_with_project(
                vec![LedgerEntry::from(&constant("JAVA_HOME", "/p1/jdk"))],
                Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm".to_owned()))]),
            );
            let scope = ledger.scopes.project.as_mut().expect("scope");
            scope.key = key.to_owned();
            scope.dir = PathBuf::from(dir);
            let current = env_with(&[("JAVA_HOME", "/p1/jdk"), ("PATH", "/home/u/.ocx/packages/old/bin")]);
            serde_json::to_value(plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")])).expect("serialize")
        };

        let benign = planned("acme-1a2b", "/p1");
        assert_eq!(
            benign["restores"],
            serde_json::json!([["JAVA_HOME", "/usr/lib/jvm"]]),
            "the revert set is named by `applied`, never by the identity labels"
        );
        for forged in ["../../../../etc", "/etc/shadow", "..\\..\\windows", ""] {
            assert_eq!(
                planned(forged, forged),
                benign,
                "a carrier claiming key/dir {forged:?} must plan byte-identically"
            );
        }
    }

    /// EC-LEDGER-009 — the legitimate project switch. On **every** switch the
    /// walk yields `/p2` while `dir` records `/p1`, so reading that mismatch as
    /// "invalidate the scope" would discard the revert set and leak `/p1`'s
    /// constant. A-03 reads it the other way: a mismatch means the scope has
    /// been *left*, so its `applied` list **is** the revert set.
    #[test]
    fn c007a_a003_a_project_switch_reverts_before_the_new_scope_applies() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/p1/jdk"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Unset)]),
        );
        let current = env_with(&[("JAVA_HOME", "/p1/jdk")]);
        // The new scope declares a different variable, so JAVA_HOME is in L and
        // not in D — exactly what leaving /p1 for /p2 looks like.
        let desired = vec![constant("GRADLE_HOME", "/p2/gradle")];

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(planned.restores, vec![("JAVA_HOME".to_owned(), None)]);
        assert_eq!(planned.sets.len(), 1);
        assert_eq!(planned.sets[0].key, "GRADLE_HOME");
    }

    #[test]
    fn c007c_a006_the_documented_privilege_crossing_residual_is_pinned() {
        // A-06: across a privilege boundary rule (c) IS an arbitrary-value
        // primitive, because the revert set is L-scoped and never intersected
        // with D. Asserted so a future narrowing reds this deliberately.
        let forged = ledger_with_project(
            vec![LedgerEntry::from(&constant("LD_PRELOAD", "/tmp/x.so"))],
            Priors::from([("LD_PRELOAD".to_owned(), Prior::Value("/attacker/evil.so".to_owned()))]),
        );
        let current = env_with(&[("LD_PRELOAD", "/tmp/x.so")]);

        let planned = plan(&[], &current, &forged, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.restores,
            vec![("LD_PRELOAD".to_owned(), Some("/attacker/evil.so".to_owned()))]
        );
    }

    #[test]
    fn c007b_a_priors_restore_never_runs_for_a_key_d_declares_list_kind() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("GOFLAGS", "-mod=vendor"))],
            Priors::from([("GOFLAGS".to_owned(), Prior::Value("/attacker".to_owned()))]),
        );
        let current = env_with(&[("GOFLAGS", "-mod=vendor")]);
        let desired = vec![entry("GOFLAGS", "-tags=x", ModifierKind::List, Some(" "))];

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert!(planned.restores.is_empty());
    }

    // -- C-008 / C-009 / S-024 / S-026: literal, raw, unescaped ------------

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

    // -- C-010 / A-10: the emittability gate -------------------------------

    /// EC-REC-006 — a key failing `is_valid_env_key` (`2FOO`, `A-B`) is dropped
    /// **before** it can reach `Plan` or the ledger. Every emitter already
    /// returns `None` for one, so without this gate L would name something no
    /// arm can emit or remove and the revert path would carry it forever;
    /// `L ⊆ emittable(D)` is the invariant it buys.
    ///
    /// EC-LIST-010 — and the same gate is where a path-kind value embedding the
    /// platform separator dies. `export_path`'s precondition is a single
    /// directory, so letting one through would insert two segments on apply and
    /// remove neither on revert: permanent PATH pollution.
    ///
    /// A-10 puts a gate here **and** an independent one at the `[env]` parse
    /// boundary (`project::env::parse_env_value`, `EnvPathSeparatorInValue`,
    /// exit 65) — "independently" is the addendum's own word. Neither stands in
    /// for the other: the parse-boundary refusal is what `ocx run`/`ocx exec`
    /// and the `--shell`/`direnv export` emitters see, and none of them reach
    /// the reconciler; this gate is what a value arriving from anywhere else
    /// meets, and it is what keeps `L ⊆ emittable(D)` true.
    #[test]
    fn c010_a010_plan_drops_every_key_or_element_no_arm_can_emit() {
        let current = env_with(&[("PATH", "/usr/bin")]);
        let desired = vec![
            path_entry("2FOO", "/opt/bin"),
            path_entry("A-B", "/opt/bin"),
            path_entry("PATH", &format!("/a{}/b", crate::env::PATH_SEPARATOR)),
            path_entry("LD_LIBRARY_PATH", ""),
            entry("GOFLAGS", "", ModifierKind::List, Some(" ")),
            entry("CFLAGS", "-O2\n-g", ModifierKind::List, Some(" ")),
            path_entry("MANPATH", "/opt/man\r/etc"),
            path_entry("PATH", "/opt/bin"),
        ];

        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/home/u/.ocx")]);
        let kept: Vec<(&str, &str)> = planned
            .sets
            .iter()
            .map(|entry| (entry.key.as_str(), entry.value.as_str()))
            .collect();
        assert_eq!(kept, vec![("PATH", "/opt/bin")]);
    }

    #[test]
    fn c010_a002_plan_refuses_a_constant_declaration_of_path() {
        let current = env_with(&[("PATH", "/usr/bin")]);
        let desired = vec![constant("PATH", "/attacker/bin"), constant("PATHEXT", ".EXE")];

        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/home/u/.ocx")]);
        assert!(planned.sets.is_empty());
    }

    #[test]
    fn c010_plan_reads_no_env_beyond_current() {
        // Purity, asserted the only way a unit test can: an env that names
        // nothing the process has must still produce the same plan.
        let current = Env::clean();
        let desired = vec![path_entry("PATH", "/opt/bin")];
        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/home/u/.ocx")]);
        assert!(planned.removes.is_empty());
        assert_eq!(planned.sets.len(), 1);
    }

    // -- C-011 / A-23: the Plan wire shape ---------------------------------

    #[test]
    fn c011_a023_the_plan_json_carries_a_structural_v_and_ledger_entry_spelling() {
        let planned = Plan {
            v: PLAN_VERSION,
            sets: vec![
                path_entry("PATH", "/opt/bin"),
                entry("CFLAGS", "-O2", ModifierKind::List, None),
            ],
            removes: vec![
                ("PATH".to_owned(), "/old/bin".to_owned(), None),
                ("CFLAGS".to_owned(), "-g".to_owned(), Some(sep(" "))),
            ],
            restores: vec![
                ("JAVA_HOME".to_owned(), Some("/usr/lib/jvm".to_owned())),
                ("GRADLE_HOME".to_owned(), None),
            ],
        };
        let json = serde_json::to_value(&planned).expect("serialize");

        assert_eq!(json["v"], 1);
        assert_eq!(json["sets"][0]["type"], "path");
        assert!(json["sets"][0].get("separator").is_none());
        assert_eq!(json["sets"][1]["type"], "list");
        assert_eq!(json["sets"][1]["separator"], DEFAULT_SEPARATOR);
        assert_eq!(json["removes"][0], serde_json::json!(["PATH", "/old/bin", null]));
        assert_eq!(json["removes"][1], serde_json::json!(["CFLAGS", "-g", " "]));
        assert_eq!(json["restores"][0], serde_json::json!(["JAVA_HOME", "/usr/lib/jvm"]));
        assert_eq!(json["restores"][1], serde_json::json!(["GRADLE_HOME", null]));
    }

    // -- C-012 / S-021: the repair gesture ---------------------------------

    #[test]
    fn c012_the_carrier_key_is_inside_the_reserved_namespace() {
        assert_eq!(CARRIER_KEY, "__OCX_ENV_STATE");
        assert!(crate::env::is_reserved_ocx_key(CARRIER_KEY));
        assert!(crate::env::is_valid_env_key(CARRIER_KEY));
    }

    // -- C-013 / A-07: apply is routed per kind ----------------------------

    /// EC-LIST-007 — the removal side mirrors the apply side for a list with no
    /// declared separator, both while D still names the key and once D has gone
    /// entirely. A-08 is what makes that possible: the ledger records the
    /// **effective** separator at write time (asserted by
    /// [`c001_a008_list_separator_is_always_some_and_defaults_to_a_space`]), so
    /// `None` never has to be guessed back into one and stays reserved for
    /// path-kind.
    #[test]
    fn c013_a007_list_kind_keeps_the_whole_contribution_and_its_effective_separator() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&entry(
                "GOFLAGS",
                "-mod=vendor -tags=old",
                ModifierKind::List,
                None,
            ))],
            Priors::new(),
        );
        let current = env_with(&[("GOFLAGS", "-mod=vendor -tags=old")]);
        let desired = vec![entry("GOFLAGS", "-mod=vendor -tags=new", ModifierKind::List, None)];

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.removes,
            vec![(
                "GOFLAGS".to_owned(),
                "-mod=vendor -tags=old".to_owned(),
                Some(sep(DEFAULT_SEPARATOR))
            )],
            "the contribution is opaque and rides its effective separator"
        );

        // Leaving the scope outright: D declares nothing, so the separator can
        // only come from L's recorded *effective* value. A `None` here would
        // mean the platform PATH separator, and the element would be
        // permanently unremovable — joined on a space, split on `:`.
        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.removes,
            vec![(
                "GOFLAGS".to_owned(),
                "-mod=vendor -tags=old".to_owned(),
                Some(sep(DEFAULT_SEPARATOR))
            )],
            "with D gone, the removal still mirrors the apply side"
        );
    }

    #[test]
    fn c013_a008_a_non_default_separator_rides_the_removal() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&entry(
                "CLASSPATH",
                "/old.jar",
                ModifierKind::List,
                Some(":"),
            ))],
            Priors::new(),
        );
        let current = env_with(&[("CLASSPATH", "/old.jar")]);
        let desired = vec![entry("CLASSPATH", "/new.jar", ModifierKind::List, Some(":"))];

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(planned.removes[0].2.as_deref(), Some(":"));
    }

    #[test]
    fn c013_absence_is_not_an_error_when_the_element_is_already_gone() {
        let ledger = ledger_with_project(vec![LedgerEntry::from(&path_entry("PATH", "/gone/bin"))], Priors::new());
        let current = env_with(&[("PATH", "/usr/bin")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(planned.removes.len(), 1, "a delete-if-found removal is still planned");
    }

    // -- C-015 rule 0: the path/list apply gate, the fixed point ------------

    /// The property the whole per-prompt design rests on: applying a plan and
    /// re-planning against the result yields **nothing**. Without it every
    /// prompt re-runs the emitted `PATH` surgery forever
    /// ([ocx-sh/ocx#342](https://github.com/ocx-sh/ocx/issues/342)).
    #[test]
    fn c015_rule0_a_second_pass_over_an_applied_plan_emits_nothing() {
        let desired = vec![
            path_entry("PATH", "/ocx/a/bin"),
            path_entry("PATH", "/ocx/b/bin"),
            entry("PERL5LIB", "/ocx/a/lib", ModifierKind::List, Some(":")),
            constant("JAVA_HOME", "/ocx/jdk"),
        ];
        let mut current = env_with(&[("PATH", "/usr/bin:/bin"), ("PERL5LIB", "/site/lib")]);
        let owned = [Path::new("/ocx")];

        let first = plan(&desired, &current, &Ledger::empty(), &owned);
        assert_eq!(first.sets.len(), 4, "the first pass applies everything");
        apply_in_process(&mut current, &first);
        assert_eq!(
            path_segments(&current),
            vec!["/ocx/b/bin", "/ocx/a/bin", "/usr/bin", "/bin"]
        );

        // The ledger a real second prompt would carry.
        let ledger = ledger_with_project(
            desired.iter().map(LedgerEntry::from).collect(),
            Priors::from([("JAVA_HOME".to_owned(), Prior::Unset)]),
        );
        let second = plan(&desired, &current, &ledger, &owned);
        assert!(
            second.sets.is_empty() && second.removes.is_empty() && second.restores.is_empty(),
            "a settled prompt must emit nothing, got {second:?}"
        );
    }

    /// The red half of the pair above: a path key whose fold would move settles
    /// nothing, so a genuine change still applies.
    #[test]
    fn c015_rule0_a_path_key_whose_fold_would_move_still_applies() {
        let desired = vec![path_entry("PATH", "/ocx/a/bin"), path_entry("PATH", "/ocx/b/bin")];
        let ledger = ledger_with_project(desired.iter().map(LedgerEntry::from).collect(), Priors::new());

        // Emission order is a-then-b, so the settled head is b-then-a. Reversed
        // here — the user's `PATH` edit — which the next prompt must repair.
        let current = env_with(&[("PATH", "/ocx/a/bin:/ocx/b/bin:/usr/bin")]);
        let planned = plan(&desired, &current, &ledger, &[Path::new("/ocx")]);
        assert_eq!(planned.sets.len(), 2, "a moved head re-applies both entries");
    }

    /// Settling is per key: a `PATH` that is already folded does not silence a
    /// sibling list key that is not.
    #[test]
    fn c015_rule0_settling_is_per_key_not_per_plan() {
        let desired = vec![
            path_entry("PATH", "/ocx/a/bin"),
            entry("PERL5LIB", "/ocx/a/lib", ModifierKind::List, Some(":")),
        ];
        let ledger = ledger_with_project(desired.iter().map(LedgerEntry::from).collect(), Priors::new());
        let current = env_with(&[("PATH", "/ocx/a/bin:/usr/bin"), ("PERL5LIB", "/site/lib")]);

        let planned = plan(&desired, &current, &ledger, &[Path::new("/ocx")]);
        let keys: Vec<&str> = planned.sets.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(keys, vec!["PERL5LIB"]);
    }

    /// Rule 1's operand is the ledger, never the live value: with an empty
    /// ledger a constant is claimed even though C already equals D. This is the
    /// *ledger* half of the pair — rule 0's constant exclusion is asserted by
    /// `c015_rule0_a_constant_key_never_settles_from_the_live_environment`,
    /// which needs a path/list candidate on the same key to reach the guard.
    #[test]
    fn c015_rule1_a_constant_compares_against_the_ledger_not_the_live_environment() {
        let desired = vec![constant("JAVA_HOME", "/ocx/jdk")];
        // The live value already equals D, but the ledger says ocx never wrote
        // it — the user typed it. A-04's coincidence rule claims it once.
        let current = env_with(&[("JAVA_HOME", "/ocx/jdk")]);

        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/ocx")]);
        assert_eq!(planned.sets.len(), 1);
    }

    /// Rule 0's constant exclusion, reached where it can actually run: a key
    /// carrying **both** a list contribution and a constant, which `Env`'s own
    /// `apply_entries_mixed_kinds_on_one_key_follow_vector_order` pins as a real
    /// composition state. `current` is the exact sandwich the fold produces, so
    /// without `settled_keys`' constant guard the key settles from the live
    /// environment and both list entries vanish — the mid-session-override
    /// clobber rule 1's ledger comparison exists to prevent.
    ///
    /// The guard is unreachable without a path/list candidate on the same key:
    /// `settled_keys` returns early on an empty candidate set, which is why the
    /// single-constant test above cannot assert this.
    #[test]
    fn c015_rule0_a_constant_key_never_settles_from_the_live_environment() {
        let desired = vec![
            entry("OPTS", "-first", ModifierKind::List, Some(" ")),
            constant("OPTS", "-replaced"),
            entry("OPTS", "-last", ModifierKind::List, Some(" ")),
        ];
        // What folding `desired` into this env produces, byte for byte: the
        // constant clears what came before it, then `-last` appends. So the
        // whole-value compare says "settled" and only the guard says otherwise.
        let current = env_with(&[("OPTS", "-replaced -last")]);

        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/ocx")]);
        let values: Vec<&str> = planned.sets.iter().map(|entry| entry.value.as_str()).collect();
        assert_eq!(
            values,
            vec!["-first", "-replaced", "-last"],
            "a key any scope declares constant settles from the ledger, never from C"
        );
    }

    /// The settle compare is [`element_eq`]'s, applied per segment — **not**
    /// `==` on the raw whole value.
    ///
    /// Asserted on the operand pair Windows actually produces, so the rule is
    /// checkable on every host: `Env::apply_entries` re-joins segments that came
    /// out of `std::env::split_paths`, which unquotes on Windows, so the folded
    /// value is the unquoted spelling of a live value that still carries its
    /// quotes. Under a byte-exact compare that key never settles and #342 stays
    /// open there; under A-19's rule it settles, which is what the emitted pwsh
    /// arm — retaining `$_` byte for byte — would actually do.
    #[test]
    fn c015_rule0_the_settle_compare_is_a019s_not_byte_exact() {
        let folded = std::ffi::OsString::from(["/ocx/a/bin", "/program files/x"].join(crate::env::PATH_SEPARATOR));
        let live = std::ffi::OsString::from(["/ocx/a/bin", "\"/program files/x\""].join(crate::env::PATH_SEPARATOR));
        assert_ne!(folded, live, "the two spellings are not byte-equal");
        assert!(
            value_settled(Some(&folded), Some(&live), &ModifierKind::Path),
            "a retained segment that differs only by one quote pair is settled"
        );

        // The red half: a segment that genuinely moved is never settled, and a
        // list value keeps the byte-exact rule its opaque elements require (E5).
        let moved = std::ffi::OsString::from(["/program files/x", "/ocx/a/bin"].join(crate::env::PATH_SEPARATOR));
        assert!(!value_settled(Some(&moved), Some(&live), &ModifierKind::Path));
        assert!(!value_settled(Some(&folded), Some(&live), &ModifierKind::List));
        assert!(!value_settled(Some(&folded), None, &ModifierKind::Path));
        assert!(value_settled(None, None, &ModifierKind::Path));
    }

    /// The same defect end to end, on the platform that has it: a `PATH` whose
    /// retained segment is quoted settles, so a second prompt emits nothing.
    ///
    /// Windows-only because the asymmetry is: `std::env::split_paths` strips the
    /// quotes there and nowhere else. The rule the assertion rests on is
    /// asserted on every host by
    /// `c015_rule0_the_settle_compare_is_a019s_not_byte_exact`.
    #[cfg(windows)]
    #[test]
    fn c015_rule0_a_quoted_retained_segment_settles_on_windows() {
        let desired = vec![path_entry("PATH", r"C:\ocx\a\bin")];
        let ledger = ledger_with_project(desired.iter().map(LedgerEntry::from).collect(), Priors::new());
        let live = [r"C:\ocx\a\bin", r#""C:\Program Files\x""#].join(crate::env::PATH_SEPARATOR);
        let current = env_with(&[("PATH", live.as_str())]);

        let planned = plan(&desired, &current, &ledger, &[Path::new(r"C:\ocx")]);
        assert!(
            planned.sets.is_empty(),
            "the fold is already live; a quote pair on a retained segment is not a change, got {planned:?}"
        );
    }

    // -- C-015 / A-05: constant apply and revert ---------------------------

    #[test]
    fn c015_rule1_an_unchanged_constant_is_not_re_set() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Unset)]),
        );
        // The user overrode it mid-session; D is unchanged, so ocx leaves C alone.
        let current = env_with(&[("JAVA_HOME", "/my/own/jdk")]);
        let desired = vec![constant("JAVA_HOME", "/jdk")];

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert!(planned.sets.is_empty());
    }

    #[test]
    fn c015_rule1_a_changed_constant_is_set() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk17"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Unset)]),
        );
        let current = env_with(&[("JAVA_HOME", "/jdk17")]);
        let desired = vec![constant("JAVA_HOME", "/jdk21")];

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(planned.sets.len(), 1);
        assert_eq!(planned.sets[0].value, "/jdk21");
    }

    #[test]
    fn c015_rule2_the_exit_guard_refuses_to_clobber_a_mid_session_override() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm".to_owned()))]),
        );
        let current = env_with(&[("JAVA_HOME", "/my/own/jdk")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert!(planned.restores.is_empty());
    }

    /// EC-CONST-008 — the whole `export JAVA_HOME=` journey in one pass.
    /// Capture reads set-ness, so a set-but-empty variable is `Value("")` and
    /// never collapses into `Unset`; leaving therefore emits that arm's
    /// `export_constant(key, "")` and never an `unset` — the difference a bash
    /// `[ -z "${JAVA_HOME+x}" ]` sees. The cheapest place a `filter` or an
    /// `unwrap_or_default` on the read side gets it wrong.
    #[test]
    fn c015_a005_a_set_but_empty_prior_restores_the_empty_value_and_never_unsets() {
        let applied = vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk"))];
        let priors = capture_priors(&applied, &env_with(&[("JAVA_HOME", "")]), None);
        assert_eq!(
            priors.get("JAVA_HOME"),
            Some(&Prior::Value(String::new())),
            "a set-but-empty variable captures as Value(\"\"), never as Unset"
        );

        let ledger = ledger_with_project(applied, priors);
        let current = env_with(&[("JAVA_HOME", "/jdk")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(planned.restores, vec![("JAVA_HOME".to_owned(), Some(String::new()))]);
    }

    #[test]
    fn c015_a005_capture_reads_set_ness_never_truthiness() {
        let applied = vec![
            LedgerEntry::from(&constant("EMPTY_HOME", "/jdk")),
            LedgerEntry::from(&constant("ABSENT_HOME", "/jdk")),
        ];
        let current = env_with(&[("EMPTY_HOME", "")]);

        let priors = capture_priors(&applied, &current, None);
        assert_eq!(priors.get("EMPTY_HOME"), Some(&Prior::Value(String::new())));
        assert_eq!(priors.get("ABSENT_HOME"), Some(&Prior::Unset));
    }

    #[test]
    fn c015_rule3_a_prior_is_re_captured_when_the_current_value_is_not_ours() {
        let previous = project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk17"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Unset)]),
        );
        // The user set it by hand; the next compose must not later unset it.
        let current = env_with(&[("JAVA_HOME", "/my/own/jdk")]);
        let applied = vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk21"))];

        let priors = capture_priors(
            &applied,
            &current,
            Some((previous.applied.as_slice(), &previous.priors)),
        );
        assert_eq!(
            priors.get("JAVA_HOME"),
            Some(&Prior::Value("/my/own/jdk".to_owned())),
            "rule 3: C != L.applied re-captures the prior"
        );
    }

    #[test]
    fn c015_rule3_a_prior_survives_a_recompose_that_did_not_change_the_value() {
        let previous = project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk17"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm".to_owned()))]),
        );
        let current = env_with(&[("JAVA_HOME", "/jdk17")]);
        let applied = vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk17"))];

        let priors = capture_priors(
            &applied,
            &current,
            Some((previous.applied.as_slice(), &previous.priors)),
        );
        assert_eq!(priors.get("JAVA_HOME"), Some(&Prior::Value("/usr/lib/jvm".to_owned())));
    }

    /// EC-CONST-006 — re-capture and the coincidence rule compose, and the
    /// composition is permanent: a prior that was `Unset` before the shell ever
    /// entered the project becomes the value the user typed, so **leaving sets
    /// it** rather than removing it. Pinned deliberately so nobody later
    /// "fixes" the restore into an unset and reintroduces the clobber the rule
    /// exists to prevent.
    #[test]
    fn c015_rule4_a_coincidence_is_claimed_silently_with_the_typed_value_as_prior() {
        let previous = project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk17"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Unset)]),
        );
        // C != L, but the user typed exactly what D wants.
        let current = env_with(&[("JAVA_HOME", "/jdk21")]);
        let applied = vec![LedgerEntry::from(&constant("JAVA_HOME", "/jdk21"))];

        let priors = capture_priors(
            &applied,
            &current,
            Some((previous.applied.as_slice(), &previous.priors)),
        );
        assert_eq!(priors.get("JAVA_HOME"), Some(&Prior::Value("/jdk21".to_owned())));

        let planned = plan(
            &[],
            &current,
            &ledger_with_project(applied, priors),
            &[Path::new("/home/u/.ocx")],
        );
        assert_eq!(
            planned.restores,
            vec![("JAVA_HOME".to_owned(), Some("/jdk21".to_owned()))],
            "leaving sets the coincidence value; it never unsets what the user typed"
        );
    }

    #[test]
    fn c015_only_constants_get_priors() {
        let applied = vec![
            LedgerEntry::from(&path_entry("PATH", "/opt/bin")),
            LedgerEntry::from(&entry("CFLAGS", "-O2", ModifierKind::List, None)),
        ];
        assert!(capture_priors(&applied, &env_with(&[("PATH", "/usr/bin")]), None).is_empty());
    }

    #[test]
    fn c015_c006_a_constant_with_no_recorded_prior_is_left_in_place() {
        // "Restore its recorded prior" has no operand without one, and C-006
        // forbids guess-unsetting a constant. Both prior maps are empty here -
        // the shape an older carrier written before `global_priors` existed
        // decodes to - so this is the retirement outcome.
        let ledger = Ledger {
            scopes: Scopes {
                global: Some(vec![LedgerEntry::from(&constant("JAVA_HOME", "/global/jdk"))]),
                global_priors: Priors::new(),
                project: None,
            },
            ..Ledger::empty()
        };
        let current = env_with(&[("JAVA_HOME", "/global/jdk")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert!(planned.restores.is_empty());
    }

    // -- C-016 / A-09: the retirement rule ---------------------------------

    /// The named red state of §7.6's assigned fault injection: making the list
    /// repair additive leaves the stale digest directory on PATH.
    #[test]
    fn c016_a_digest_bump_leaves_zero_stale_package_directories_on_path() {
        let old = "/home/u/.ocx/packages/ghcr.io/acme/tool/aaaa/bin";
        let new = "/home/u/.ocx/packages/ghcr.io/acme/tool/bbbb/bin";
        let mut current = env_with(&[("PATH", &format!("{old}:/usr/bin"))]);
        let desired = vec![path_entry("PATH", new)];

        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/home/u/.ocx")]);
        apply_in_process(&mut current, &planned);

        let segments = path_segments(&current);
        assert_eq!(
            segments.iter().filter(|segment| segment.as_str() == old).count(),
            0,
            "the stale digest directory must not survive the repair"
        );
        assert_eq!(segments, vec![new.to_owned(), "/usr/bin".to_owned()]);
    }

    /// EC-LIST-009 — prefix ownership is `Path::starts_with` (component
    /// boundary), never `str::starts_with`. A byte-prefix test claims
    /// `/home/u/.ocx-backup/bin` as ocx's and deletes a foreign element: the
    /// sibling-typosquat class the trust-whitelist research names for path
    /// grants, reappearing inside the reconciler.
    #[test]
    fn c016_a009_ownership_is_component_wise_so_a_lookalike_prefix_survives() {
        let mut current = env_with(&[(
            "PATH",
            "/home/u/.ocx-backup/bin:/home/u/.ocxevil/bin:/home/u/.ocx/packages/old/bin:/usr/bin",
        )]);
        let desired = vec![path_entry("PATH", "/home/u/.ocx/packages/new/bin")];

        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/home/u/.ocx")]);
        apply_in_process(&mut current, &planned);

        let segments = path_segments(&current);
        assert!(segments.contains(&"/home/u/.ocx-backup/bin".to_owned()));
        assert!(segments.contains(&"/home/u/.ocxevil/bin".to_owned()));
        assert!(!segments.contains(&"/home/u/.ocx/packages/old/bin".to_owned()));
    }

    #[test]
    fn c016_a009_the_removal_operand_is_the_segment_as_it_appears_in_current() {
        // A trailing slash makes C's spelling differ from anything D or L knows;
        // naming the observed segment verbatim is what makes the removal land.
        let mut current = env_with(&[("PATH", "/home/u/.ocx/packages/old/bin/:/usr/bin")]);
        let desired = vec![path_entry("PATH", "/home/u/.ocx/packages/new/bin")];

        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/home/u/.ocx")]);
        assert_eq!(planned.removes[0].1, "/home/u/.ocx/packages/old/bin/");

        apply_in_process(&mut current, &planned);
        assert_eq!(
            path_segments(&current)
                .iter()
                .filter(|segment| segment.starts_with("/home/u/.ocx/packages/old"))
                .count(),
            0
        );
    }

    #[test]
    fn c016_an_arbitrary_element_is_retired_only_where_the_ledger_records_it() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&path_entry("PATH", "/opt/project/bin"))],
            Priors::new(),
        );
        let mut current = env_with(&[("PATH", "/opt/project/bin:/opt/foreign/bin:/usr/bin")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        apply_in_process(&mut current, &planned);

        let segments = path_segments(&current);
        assert!(!segments.contains(&"/opt/project/bin".to_owned()));
        assert!(segments.contains(&"/opt/foreign/bin".to_owned()));
    }

    #[test]
    fn c016_a_still_desired_element_is_never_retired() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&path_entry("PATH", "/opt/project/bin"))],
            Priors::new(),
        );
        let current = env_with(&[("PATH", "/opt/project/bin:/usr/bin")]);
        let desired = vec![path_entry("PATH", "/opt/project/bin")];

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert!(planned.removes.is_empty());
    }

    #[test]
    fn c016_a_global_element_is_retired_mid_session_under_a_live_project() {
        // `ocx remove --global foo` from another terminal: global's L entry
        // survives, D no longer names it, the project scope is untouched.
        let ledger = Ledger {
            scopes: Scopes {
                global: Some(vec![LedgerEntry::from(&path_entry(
                    "PATH",
                    "/home/u/.ocx/packages/global/bin",
                ))]),
                global_priors: Priors::new(),
                project: Some(project(
                    vec![LedgerEntry::from(&path_entry("PATH", "/home/u/.ocx/packages/proj/bin"))],
                    Priors::new(),
                )),
            },
            ..Ledger::empty()
        };
        let current = env_with(&[(
            "PATH",
            "/home/u/.ocx/packages/proj/bin:/home/u/.ocx/packages/global/bin:/usr/bin",
        )]);
        let desired = vec![path_entry("PATH", "/home/u/.ocx/packages/proj/bin")];

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.removes,
            vec![("PATH".to_owned(), "/home/u/.ocx/packages/global/bin".to_owned(), None)]
        );
    }

    // -- C-017: the revert set is L-scoped, never intersected with D --------

    /// EC-LEDGER-010 — the superseded Validation tier-1 bullet said an L key
    /// absent from D must be *discarded*; a test written to it would have
    /// asserted the `JAVA_HOME` leak as correct. A-03 restates it: an L
    /// constant absent from D is **reverted**, and the forgery bound is
    /// D ∪ L, enforced by "an L entry may only undo itself".
    #[test]
    fn c017_a_constant_absent_from_d_is_reverted_not_discarded() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/p1/jdk"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm".to_owned()))]),
        );
        let mut current = env_with(&[("JAVA_HOME", "/p1/jdk")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        apply_in_process(&mut current, &planned);
        assert_eq!(
            current.get("JAVA_HOME").map(os_to_string).as_deref(),
            Some("/usr/lib/jvm")
        );
    }

    #[test]
    fn c017_keys_outside_d_union_l_are_never_touched() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("JAVA_HOME", "/p1/jdk"))],
            Priors::from([("JAVA_HOME".to_owned(), Prior::Unset)]),
        );
        let current = env_with(&[("JAVA_HOME", "/p1/jdk"), ("EDITOR", "vim"), ("SSH_AUTH_SOCK", "/tmp/s")]);
        let desired = vec![constant("GRADLE_HOME", "/p1/gradle")];

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        let touched: BTreeSet<&str> = planned
            .sets
            .iter()
            .map(|entry| entry.key.as_str())
            .chain(planned.removes.iter().map(|(key, _, _)| key.as_str()))
            .chain(planned.restores.iter().map(|(key, _)| key.as_str()))
            .collect();
        assert_eq!(touched, BTreeSet::from(["GRADLE_HOME", "JAVA_HOME"]));
    }

    // -- C-018 / A-07: scope order ------------------------------------------

    /// EC-SCOPE-001 — emission order is global first, project second, asserted
    /// as the **order of the emitted statements** and only then as the resolved
    /// values: `composer.rs`'s inversion trap reaching a new consumer.
    ///
    /// EC-LIST-008 — and the resolved values are what show the apply rule is
    /// per kind, not one sentence covering all three (A-07). Path-kind
    /// prepends, so the later scope lands in front and project wins; list-kind
    /// **appends**, so the later scope lands last and a first-wins consumer
    /// would read global — "in front, in order" is wrong for lists and this is
    /// where that reds.
    #[test]
    fn c018_a007_emission_order_is_global_first_project_second_for_all_kinds() {
        let desired = vec![
            path_entry("PATH", "/global/bin"),
            entry("GOFLAGS", "-global", ModifierKind::List, None),
            constant("JAVA_HOME", "/global/jdk"),
            path_entry("PATH", "/project/bin"),
            entry("GOFLAGS", "-project", ModifierKind::List, None),
            constant("JAVA_HOME", "/project/jdk"),
        ];
        let mut current = env_with(&[("PATH", "/usr/bin")]);

        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/home/u/.ocx")]);
        let order: Vec<&str> = planned.sets.iter().map(|entry| entry.value.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "/global/bin",
                "-global",
                "/global/jdk",
                "/project/bin",
                "-project",
                "/project/jdk"
            ]
        );

        apply_in_process(&mut current, &planned);
        assert_eq!(
            path_segments(&current),
            vec![
                "/project/bin".to_owned(),
                "/global/bin".to_owned(),
                "/usr/bin".to_owned()
            ],
            "path-kind: each application prepends, so the later scope lands in front — project, then global"
        );
        assert_eq!(
            current.get("GOFLAGS").map(os_to_string).as_deref(),
            Some("-global -project"),
            "list-kind: project last into a last-wins consumer"
        );
        assert_eq!(
            current.get("JAVA_HOME").map(os_to_string).as_deref(),
            Some("/project/jdk"),
            "constant: later write wins"
        );
    }

    #[test]
    fn c018_prior_capture_after_globals_apply_holds_globals_value() {
        // The caller applies global first; capture then sees global's value, so
        // reverting the project restores global rather than tearing it down.
        let after_global = env_with(&[("JAVA_HOME", "/global/jdk")]);
        let priors = capture_priors(
            &[LedgerEntry::from(&constant("JAVA_HOME", "/project/jdk"))],
            &after_global,
            None,
        );
        assert_eq!(priors.get("JAVA_HOME"), Some(&Prior::Value("/global/jdk".to_owned())));
    }

    // -- C-019 / C-021: fingerprint carriage and iteration order ------------

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

    /// EC-REC-002 — the shim-slot contract, named as a contract rather than as
    /// an outcome: a deferred root's `entrypoints/` must resolve ahead of
    /// `bin/`, and `bin/` ahead of `shims/`. The reconciler is a new
    /// `Vec<Entry>` consumer, and consumers prepend, so a reader who assumes
    /// push order equals PATH order inverts it and makes the shim shadow the
    /// real binaries.
    #[test]
    fn c021_entry_iteration_order_is_preserved_into_the_plan() {
        // The composer's consumers prepend, so the last entry pushed is first
        // in PATH; the reconciler must not reorder them on the way through.
        let desired = vec![
            path_entry("PATH", "/pkg/shims"),
            path_entry("PATH", "/pkg/bin"),
            path_entry("PATH", "/pkg/entrypoints"),
        ];
        let mut current = env_with(&[("PATH", "/usr/bin")]);

        let planned = plan(&desired, &current, &Ledger::empty(), &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned
                .sets
                .iter()
                .map(|entry| entry.value.as_str())
                .collect::<Vec<_>>(),
            vec!["/pkg/shims", "/pkg/bin", "/pkg/entrypoints"]
        );

        apply_in_process(&mut current, &planned);
        assert_eq!(
            path_segments(&current),
            vec![
                "/pkg/entrypoints".to_owned(),
                "/pkg/bin".to_owned(),
                "/pkg/shims".to_owned(),
                "/usr/bin".to_owned()
            ]
        );
    }

    // -- A-19: one comparison rule ------------------------------------------

    #[test]
    fn a019_a_quoted_segment_compares_equal_to_its_unquoted_spelling() {
        assert!(element_eq("\"/opt/bin\"", "/opt/bin", &ModifierKind::Path));
        assert!(element_eq("/opt/bin", "\"/opt/bin\"", &ModifierKind::Path));
        assert!(
            !element_eq("\"/opt/bin", "/opt/bin", &ModifierKind::Path),
            "only a surrounding pair is stripped"
        );
        assert_eq!(unquote("\""), "\"");
        // E5 — the strip is A-19's, and A-19 is about PATH segments: a list
        // element is opaque, so its quotes are part of the option.
        assert!(!element_eq("\"-Dx\"", "-Dx", &ModifierKind::List));
    }

    /// The two comparison rules, asserted against `cfg!(windows)` rather than
    /// behind a `#[cfg]` so **both** arms execute on every platform: the ASCII
    /// fold is path-kind only, and a list element stays case-sensitive
    /// everywhere because `-DFOO=1` and `-Dfoo=1` are different options.
    #[test]
    fn a019_e5_the_ascii_fold_is_path_kind_only() {
        assert_eq!(
            element_eq("/opt/Bin", "/opt/bin", &ModifierKind::Path),
            cfg!(windows),
            "path-kind folds case on Windows and nowhere else"
        );
        assert_eq!(
            element_norm("/opt/Bin", &ModifierKind::Path) == element_norm("/opt/bin", &ModifierKind::Path),
            cfg!(windows)
        );
        assert!(
            !element_eq("-DFOO=1", "-Dfoo=1", &ModifierKind::List),
            "a list element is case-sensitive on every platform"
        );
        assert_ne!(
            element_norm("-DFOO=1", &ModifierKind::List),
            element_norm("-Dfoo=1", &ModifierKind::List)
        );
    }

    /// EC-CONST-009 — the `C == L.applied` compare keys through the same
    /// equality `EnvKey` uses: ASCII-case-insensitive on Windows, exact
    /// elsewhere. Asserted against `cfg!(windows)` rather than behind a
    /// `#[cfg]` so **both** arms execute on every platform — a case-sensitive
    /// map makes the exit guard always false under pwsh's `$env:Path` spelling
    /// and silently abandons every prior, while a case-insensitive one fuses
    /// two genuinely distinct variables on Unix.
    #[test]
    fn a019_key_equality_follows_the_platform_and_so_does_the_exit_guard() {
        assert!(key_eq("JAVA_HOME", "JAVA_HOME"));
        assert_eq!(key_eq("Path", "PATH"), cfg!(windows));
        assert_eq!(key_norm("Path") == key_norm("PATH"), cfg!(windows));
        // A-02's reserved-key test rides the same equality.
        assert_eq!(is_never_constant("Path"), cfg!(windows));

        // The guard itself: L recorded the pwsh spelling while the shell
        // reports the uppercase one. On Windows that is one variable and the
        // prior is restored; on Unix they are two and nothing is touched.
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&constant("Java_Home", "/p1/jdk"))],
            Priors::from([("Java_Home".to_owned(), Prior::Value("/usr/lib/jvm".to_owned()))]),
        );
        let current = env_with(&[("JAVA_HOME", "/p1/jdk")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        let expected = if cfg!(windows) {
            vec![("Java_Home".to_owned(), Some("/usr/lib/jvm".to_owned()))]
        } else {
            Vec::new()
        };
        assert_eq!(planned.restores, expected);
    }

    // -- R1: the global scope records priors too -----------------------------

    /// R1 — `ocx remove --global <pkg>` retires a global constant, and the
    /// user's own value comes back.
    ///
    /// Before [`Scopes::global_priors`] existed there was no operand to restore
    /// and ocx's value stayed in the shell for its whole life: C-006 forbids
    /// guess-unsetting it, so leaving it was the only honest outcome *given the
    /// ledger's shape* — and the shape was the bug.
    ///
    /// Red state: drop `global_priors` from the `Scopes` literal below (the
    /// `Default` leaves it empty) and `restores` is empty again.
    #[test]
    fn r1_a_retired_global_constant_restores_the_users_own_value() {
        let ledger = Ledger {
            scopes: Scopes {
                global: Some(vec![LedgerEntry::from(&constant("JAVA_HOME", "/global/jdk"))]),
                global_priors: Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm".to_owned()))]),
                project: None,
            },
            ..Ledger::empty()
        };
        let current = env_with(&[("JAVA_HOME", "/global/jdk")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.restores,
            vec![("JAVA_HOME".to_owned(), Some("/usr/lib/jvm".to_owned()))],
            "the global scope's own prior is what a retired global constant reverts to"
        );
    }

    /// R1's compounding half — the two-scope retirement, and the reason
    /// [`Ledger::prior`] **chains** rather than preferring the project map.
    ///
    /// A project constant shadows a global one. The project's prior was captured
    /// *after* global applied (C-018), so it holds **global's** value, not the
    /// user's. When both scopes retire in the same prompt, restoring the project
    /// prior verbatim writes back a value no scope declares any more and the
    /// user's original is unrecoverable. The chain hop is what makes the answer
    /// the user's own value.
    ///
    /// Red state: return the project prior unconditionally in `Ledger::prior`
    /// and this restores `/global/jdk`.
    #[test]
    fn r1_two_scopes_retiring_together_restore_the_users_value_not_globals() {
        let ledger = Ledger {
            scopes: Scopes {
                global: Some(vec![LedgerEntry::from(&constant("JAVA_HOME", "/global/jdk"))]),
                global_priors: Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm".to_owned()))]),
                project: Some(project(
                    vec![LedgerEntry::from(&constant("JAVA_HOME", "/project/jdk"))],
                    Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/global/jdk".to_owned()))]),
                )),
            },
            ..Ledger::empty()
        };
        let current = env_with(&[("JAVA_HOME", "/project/jdk")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.restores,
            vec![("JAVA_HOME".to_owned(), Some("/usr/lib/jvm".to_owned()))],
            "the project prior held global's value, so the revert hops to global's own prior"
        );
    }

    /// The chain must not fire on a *coincidence*: where the project prior's
    /// value merely happens to differ from what global recorded, the project
    /// prior is the user's own and is restored verbatim.
    ///
    /// This is the guard that stops the hop from becoming "global always wins",
    /// which would lose a genuine pre-project value on every two-scope
    /// retirement where the scopes disagreed.
    #[test]
    fn r1_the_chain_hop_only_fires_when_the_project_prior_is_globals_value() {
        let ledger = Ledger {
            scopes: Scopes {
                global: Some(vec![LedgerEntry::from(&constant("JAVA_HOME", "/global/jdk"))]),
                global_priors: Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm".to_owned()))]),
                project: Some(project(
                    vec![LedgerEntry::from(&constant("JAVA_HOME", "/project/jdk"))],
                    // Not global's value: the user set this by hand mid-session,
                    // after global had applied.
                    Priors::from([("JAVA_HOME".to_owned(), Prior::Value("/opt/hand-rolled".to_owned()))]),
                )),
            },
            ..Ledger::empty()
        };
        let current = env_with(&[("JAVA_HOME", "/project/jdk")]);

        let planned = plan(&[], &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.restores,
            vec![("JAVA_HOME".to_owned(), Some("/opt/hand-rolled".to_owned()))],
            "a project prior that is not global's recorded value is the user's own"
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

    // -- E5: the comparison rule splits by kind ------------------------------

    /// A list element is **opaque**: the quotes in `"-Dx"` are part of the
    /// option, so a desired `-Dx` is a different element and the recorded one
    /// still has to be retired. `shell.rs`'s `remove_list_element` and
    /// `export_list` both match byte-exact, so a planner that unquotes first
    /// suppresses the very removal the emitter was going to render — and
    /// `export_list` then appends the second spelling beside the first.
    #[test]
    fn e5_a_quoted_list_element_is_not_the_same_element_as_its_bare_spelling() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&entry(
                "CFLAGS",
                "\"-Dx\"",
                ModifierKind::List,
                Some(" "),
            ))],
            Priors::new(),
        );
        let desired = vec![entry("CFLAGS", "-Dx", ModifierKind::List, Some(" "))];
        let mut current = env_with(&[("CFLAGS", "\"-Dx\"")]);

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.removes,
            vec![("CFLAGS".to_owned(), "\"-Dx\"".to_owned(), Some(sep(" ")))],
            "the recorded spelling is retired byte-exact"
        );

        apply_in_process(&mut current, &planned);
        assert_eq!(
            current.get("CFLAGS").map(os_to_string).as_deref(),
            Some("-Dx"),
            "one element survives, not both spellings"
        );
    }

    /// `-DFOO=1` and `-Dfoo=1` are different options on **every** platform, so
    /// the recorded one is retired regardless of host. The ASCII fold belongs
    /// to A-19's path-kind rule; applying it to a list element left `-DFOO=1`
    /// in place on Windows while `export_list`'s ordinal `.Replace` appended
    /// `-Dfoo=1` beside it, which is the unbounded growth `export_list`'s doc
    /// says the ordinal calls exist to prevent.
    #[test]
    fn e5_a_list_element_comparison_is_case_sensitive_on_every_platform() {
        let ledger = ledger_with_project(
            vec![LedgerEntry::from(&entry(
                "CFLAGS",
                "-DFOO=1",
                ModifierKind::List,
                Some(" "),
            ))],
            Priors::new(),
        );
        let desired = vec![entry("CFLAGS", "-Dfoo=1", ModifierKind::List, Some(" "))];
        let mut current = env_with(&[("CFLAGS", "-DFOO=1")]);

        let planned = plan(&desired, &current, &ledger, &[Path::new("/home/u/.ocx")]);
        assert_eq!(
            planned.removes,
            vec![("CFLAGS".to_owned(), "-DFOO=1".to_owned(), Some(sep(" ")))],
            "case is significant in a list element on every platform"
        );

        apply_in_process(&mut current, &planned);
        assert_eq!(
            current.get("CFLAGS").map(os_to_string).as_deref(),
            Some("-Dfoo=1"),
            "one element survives, not both cases"
        );
    }

    #[test]
    fn a019_the_ledger_stores_what_was_written_never_the_normalised_form() {
        let recorded = LedgerEntry::from(&path_entry("PATH", "\"/opt/Bin\""));
        assert_eq!(
            recorded.value, "\"/opt/Bin\"",
            "storage is byte-exact; only comparison normalises"
        );
    }

    // -- A-38: the carrier cap bounds only the ledger ------------------------

    #[test]
    fn a038_the_cap_is_the_carriers_own_and_nothing_accounts_for_the_env_block() {
        assert_eq!(MAX_CARRIER_BYTES, 16 * 1024);
        let encoded = Ledger::empty().encode().expect("encode");
        assert!(encoded.len() < 64, "an empty ledger is tiny: {encoded}");
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use std::path::PathBuf;

    use super::*;

    /// Every member of the watch set, folded once, with no consent channel set.
    fn fold(paths: &[PathBuf], project_dir: Option<&Path>) -> String {
        fingerprint(paths, project_dir, None, None)
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write fixture");
        path
    }

    /// A-14 — force an mtime collision explicitly. `std::fs::FileTimes` is the
    /// stdlib seam for this, so the forced-collision fixtures need no
    /// dev-dependency of their own.
    fn set_mtime(path: &Path, time: std::time::SystemTime) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open the fixture for set_times");
        file.set_times(std::fs::FileTimes::new().set_modified(time))
            .expect("set the fixture mtime");
    }

    fn unix_time(seconds: u64, nanos: u32) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::new(seconds, nanos)
    }

    /// C-019 — the fold is deterministic: the same watch set folds to the same
    /// string, so an unchanged environment never reports itself stale.
    #[test]
    fn fingerprint_is_stable_for_an_unchanged_watch_set_c019() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let toml = write(dir.path(), "ocx.toml", "[tools]\n");
        let paths = vec![toml];

        assert_eq!(
            fold(&paths, Some(dir.path())),
            fold(&paths, Some(dir.path())),
            "an unchanged watch set must fold to the same fingerprint"
        );
    }

    /// C-019 member 8 — **presence** is folded, not only content: a tier file
    /// that did not exist becomes a change the moment it is created.
    #[test]
    fn fingerprint_changes_when_an_absent_member_appears_c019() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let absent = dir.path().join("config.toml");
        let paths = vec![absent.clone()];

        let before = fold(&paths, None);
        std::fs::write(&absent, "[shell]\n").expect("create the tier file");

        assert_ne!(
            before,
            fold(&paths, None),
            "creating a recorded config tier must change the fingerprint (C-019 member 8)"
        );
    }

    /// C-019 — size is folded, so an edit that keeps the mtime still moves the
    /// fingerprint when the length changes.
    #[test]
    fn fingerprint_changes_when_a_member_changes_size_c019() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let lock = write(dir.path(), "ocx.lock", "version = 3\n");
        let recorded = std::fs::metadata(&lock).expect("metadata").modified().expect("mtime");
        let paths = vec![lock.clone()];
        let before = fold(&paths, None);

        std::fs::write(&lock, "version = 3\n# a longer body\n").expect("rewrite");
        // A-14 — force the mtime collision explicitly rather than racing the
        // clock, so the assertion observes the *size* member and not the
        // filesystem's timestamp granularity.
        set_mtime(&lock, recorded);

        assert_ne!(before, fold(&paths, None), "a size change must move the fingerprint");
    }

    /// A-14 — the ceiling, stated as a test rather than discovered in the
    /// field: an unchanged `(mtime, size)` pair is invisible. Forced, never
    /// raced — the mtime is explicitly set back to the recorded value.
    /// EC-FS-001 — the same-second ceiling is an unchanged (mtime, size) pair, per A-14.
    #[test]
    fn fingerprint_ceiling_is_an_unchanged_mtime_size_pair_a14() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let lock = write(dir.path(), "ocx.lock", "aaaa");
        let recorded = std::fs::metadata(&lock).expect("metadata").modified().expect("mtime");
        let paths = vec![lock.clone()];
        let before = fold(&paths, None);

        std::fs::write(&lock, "bbbb").expect("rewrite, same length");
        set_mtime(&lock, recorded);

        assert_eq!(
            before,
            fold(&paths, None),
            "the named ceiling is that an unchanged (mtime, size) pair is invisible (A-14)"
        );
    }

    /// A-14 — the fold compares the **full** `SystemTime`, never a
    /// seconds-truncated value. Two mtimes inside the same second must produce
    /// different fingerprints.
    ///
    /// Skipped, with the probe asserted, on a filesystem that stores whole
    /// seconds. A-14 names FAT/exFAT (2 s) and NFS (1 s) as widening the ceiling
    /// and Windows as a first-class host for them, so asserting sub-second
    /// precision there would be a portability red about the *filesystem*, not
    /// about the fold. The probe is the skip's evidence: it reads the stored
    /// value back rather than assuming the write took.
    #[test]
    fn fingerprint_folds_subsecond_mtime_precision_a14() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let lock = write(dir.path(), "ocx.lock", "aaaa");
        let paths = vec![lock.clone()];

        set_mtime(&lock, unix_time(1_700_000_000, 0));
        let whole_second = fold(&paths, None);
        set_mtime(&lock, unix_time(1_700_000_000, 500_000_000));
        let stored = std::fs::metadata(&lock)
            .expect("metadata")
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("post-epoch");
        if stored.subsec_nanos() == 0 {
            // The filesystem discarded the sub-second half; there is nothing
            // for the fold to distinguish and the ceiling is the storage's.
            return;
        }

        assert_ne!(
            whole_second,
            fold(&paths, None),
            "a sub-second mtime difference must move the fingerprint — a seconds-truncated \
             fold would report these two as identical (A-14)"
        );
    }

    /// A-13 — the raw `OCX_CONSENT_PATHS` value folds in, so a grant exported
    /// from another terminal expires the cached `inert` verdict at the next
    /// prompt instead of waiting for a shell restart.
    #[test]
    fn fingerprint_folds_the_raw_consent_paths_value_a13() {
        let unset = fingerprint(&[], None, None, None);
        let granted = fingerprint(&[], None, Some("/work/proj"), None);

        assert_ne!(
            unset, granted,
            "OCX_CONSENT_PATHS must fold into fp (A-13) — without it the negative-consent \
             cache is unexpirable"
        );
    }

    /// A-13 — same for `OCX_CONSENT_NAMESPACES`, and set-but-empty is a third
    /// state distinct from unset.
    #[test]
    fn fingerprint_folds_the_raw_consent_namespaces_value_a13() {
        let unset = fingerprint(&[], None, None, None);
        let empty = fingerprint(&[], None, None, Some(""));
        let granted = fingerprint(&[], None, None, Some("ocx.sh/acme"));

        assert_ne!(unset, empty, "set-but-empty must not fold as unset");
        assert_ne!(empty, granted, "the namespace value itself must fold");
    }

    /// C-019 member 7 — which project the CWD walk resolved is part of the
    /// fingerprint, so `cd`-ing between two projects is a change even when
    /// every watched file is untouched.
    /// EC-CFG-013 — the resolved project directory is folded into the fingerprint, so a scope switch moves it.
    #[test]
    fn fingerprint_folds_the_resolved_project_directory_c019() {
        let first = fingerprint(&[], Some(Path::new("/work/one")), None, None);
        let second = fingerprint(&[], Some(Path::new("/work/two")), None, None);
        let none = fingerprint(&[], None, None, None);

        assert_ne!(first, second, "a different project directory must fold differently");
        assert_ne!(first, none, "no project at all is its own state");
    }

    /// C-019 — the watch set is ordered: the fold is over the recorded list, so
    /// two different lists never collide by concatenation.
    #[test]
    fn fingerprint_distinguishes_member_order_c019() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let one = write(dir.path(), "a", "x");
        let two = write(dir.path(), "b", "y");

        assert_ne!(
            fold(&[one.clone(), two.clone()], None),
            fold(&[two, one], None),
            "the fold must be length-prefixed per member, not a plain concatenation"
        );
    }
}

#[cfg(test)]
mod watch_path_tests {
    use super::*;
    use crate::file_structure::FileStructure;

    /// C-019 — the project tier contributes **both** `ocx.toml` and `ocx.lock`:
    /// `[env]` applies on its own authority independently of the lock, so
    /// watching locks alone would miss an `[env]`-only edit.
    #[test]
    fn watch_paths_carry_both_project_files_c019() {
        let file_structure = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));
        let project = Path::new("/work/proj");

        let paths = watch_paths(&file_structure, Some(project), None, None);

        assert!(
            paths.contains(&project.join("ocx.toml")),
            "project ocx.toml is member 1"
        );
        assert!(
            paths.contains(&project.join("ocx.lock")),
            "project ocx.lock is member 2"
        );
    }

    /// C-019 members 3-5 — the global tier's pair and the managed-config
    /// snapshot are members whether or not a project resolved.
    #[test]
    fn watch_paths_carry_the_global_tier_without_a_project_c019() {
        let file_structure = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));

        let paths = watch_paths(&file_structure, None, None, None);

        assert!(paths.contains(&PathBuf::from("/tmp/ocx_home/ocx.toml")));
        assert!(paths.contains(&PathBuf::from("/tmp/ocx_home/ocx.lock")));
        assert!(paths.contains(&file_structure.state.managed_config_snapshot_file()));
    }

    /// A-13 / A-33 — a **recorded** tier list is used verbatim, so the
    /// `--config` overlay a per-prompt process cannot re-derive still reaches
    /// the watch set. Without this the cached `inert` verdict is unexpirable
    /// for a grant made through that channel.
    ///
    /// Red state: drop the `Some(recorded)` arm so `watch_paths` always
    /// re-derives, and the explicit tier disappears from the watch set.
    #[test]
    fn watch_paths_use_the_recorded_tier_list_verbatim_a13() {
        let file_structure = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));
        let explicit = PathBuf::from("/etc/fleet/consent.toml");
        let recorded = vec![PathBuf::from("/etc/ocx/config.toml"), explicit.clone()];

        let with = watch_paths(&file_structure, None, None, Some(&recorded));
        let derived = watch_paths(&file_structure, None, None, None);

        assert!(
            with.contains(&explicit),
            "a recorded --config tier must reach the watch set (A-13, A-33); got: {with:?}"
        );
        assert!(
            !derived.contains(&explicit),
            "re-derivation structurally cannot see --config - that is why the list is recorded"
        );
    }

    /// A-13 member 9 — the consent stamp joins the watch set, which is what
    /// makes the cached `inert` verdict expirable by a grant written from
    /// another terminal.
    #[test]
    fn watch_paths_carry_the_consent_stamp_a13() {
        let file_structure = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));

        let without = watch_paths(&file_structure, None, None, None);
        let with = watch_paths(
            &file_structure,
            Some(Path::new("/work/proj")),
            Some("a1b2c3d4e5f60718"),
            None,
        );

        let stamp = file_structure.state.consent_stamp_file("a1b2c3d4e5f60718");
        assert!(with.contains(&stamp), "the consent stamp is a watch-set member (A-13)");
        assert!(!without.contains(&stamp), "no project key, no stamp member");
    }
}
