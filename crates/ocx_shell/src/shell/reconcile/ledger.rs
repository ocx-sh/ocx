// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `__OCX_ENV_STATE` carrier format and its envelope codec; the carrier is **untrusted input**.

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL;
use serde::{Deserialize, Serialize};

use super::{effective_separator, element_eq, is_never_constant, key_eq};
use ocx_package::metadata::env::entry::Entry;
use ocx_package::metadata::env::modifier::ModifierKind;

/// The private session carrier holding the encoded [`Ledger`].
///
/// The spelling is user-facing: `unset __OCX_ENV_STATE` is the documented repair gesture.
pub const CARRIER_KEY: &str = "__OCX_ENV_STATE";

/// The payload-shape version [`Ledger::empty`] writes and [`Ledger::decode`] accepts.
///
/// Additive-only: a new field is optional and never moves this number.
pub const LEDGER_VERSION: u8 = 1;

/// The size ceiling on the whole `__OCX_ENV_STATE` value, in bytes.
pub const MAX_CARRIER_BYTES: usize = 16 * 1024;

/// The ceiling on [`Ledger::tiers`], each a `stat` on every prompt; a hand-set carrier could list ~2400.
///
/// Must stay above the five tiers the config loader emits, or a grant in a truncated tier never
/// expires the cached verdict.
pub const MAX_RECORDED_TIERS: usize = 8;

/// The envelope tag naming encoder 1: base64url of compact JSON, uncompressed.
const ENCODER_TAG: &str = "1";

/// One env-var binding the reconciler applied, recorded literally.
///
/// Values are raw, unescaped, byte-exact copies of what ocx wrote.
///
/// `separator` holds the **effective** separator, resolved once at record time: always
/// present for `list` (defaulting to a single space), omitted otherwise; for `path` it
/// means the platform's `PATH` separator.
// `type`, not `kind`: the spelling `EnvEntry` emits and the nushell shim parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema, Deserialize)]
pub struct LedgerEntry {
    /// Environment-variable name.
    pub key: String,
    /// The exact string ocx wrote, byte for byte.
    pub value: String,
    /// How the value combines.
    // Only the revert set reads this copy; D's kind wins for every key D declares.
    #[serde(rename = "type")]
    pub kind: ModifierKind,
    /// The effective list separator; omitted unless `type` is `list`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
}

impl From<&Entry> for LedgerEntry {
    fn from(entry: &Entry) -> Self {
        Self {
            key: entry.key.clone(),
            value: entry.value.clone(),
            kind: entry.kind.clone(),
            separator: match entry.kind {
                // Resolved at record time so no revert path has to guess the default back.
                ModifierKind::List => Some(effective_separator(entry)),
                ModifierKind::Path | ModifierKind::Constant => None,
            },
        }
    }
}

/// Which scope a ledger datum belongs to: `global` or `project`.
///
/// Exactly two slots. A project nested inside a project does not layer; the inner
/// one *replaces* the outer, so moving between them is a switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeId {
    /// The `--global` toolchain tier.
    Global,
    /// The project resolved by the CWD walk.
    Project,
}

/// The cached activation verdict.
///
/// `activate` is never written; the watch set expires both cached verdicts, `inert` and
/// `noproject`.
// Only negative verdicts are cached, so a stale one can only make ocx do less.
// Never cache `Activate`, or the untrusted ledger becomes a consent input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Never written to the carrier.
    Activate,
    /// The negative-consent cache, expired by the watch set.
    Inert,
    // Kept apart from `Inert`, or carrier and report cannot tell refused consent from no project.
    /// The walk resolved no project at all; wire spelling `"noproject"`.
    ///
    /// Entering any project expires it.
    // Expiry relies on the project directory being folded into `fp`.
    NoProject,
}

/// What a scope's previous value was, for the constant revert path.
///
/// A set-but-empty variable is `value` with an empty string and **never** `unset`, so
/// reverting it restores the empty value rather than removing the variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Prior {
    /// The variable did not exist before ocx set it; reverting removes it.
    Unset,
    /// The variable held this exact string; reverting restores it.
    Value(String),
}

/// The entries one scope applied, in emission order.
pub type Applied = Vec<LedgerEntry>;

/// Pre-apply values for one scope's constants, keyed by env key.
///
/// Not a [`LedgerEntry`] field, since a prior must outlive its retired entry; sorted, so the payload stays byte-stable.
pub type Priors = BTreeMap<String, Prior>;

/// The project scope's record.
///
/// `key` and `dir` are **advisory identity labels**, re-derived from the CWD walk every
/// prompt: any `dir` other than the walk's own result means the scope has been left.
// Neither may build a path beyond one bounded probe of `<dir>/ocx.toml`, and `dir` never gates a revert.
// `walk_is_indeterminate` is the one use of `dir` as a path, and it can only retain a scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema, Deserialize)]
pub struct ProjectScope {
    /// The project key derived from the canonical project directory.
    pub key: String,
    /// The canonical project directory, advisory only.
    pub dir: PathBuf,
    /// What this scope applied.
    pub applied: Applied,
    /// Pre-apply constant values, captured against the **post-global** environment, so
    /// reverting the project leaves the global scope standing.
    // May hold global's value, not the user's; `Ledger::prior` then hops to `global_priors`.
    pub priors: Priors,
}

/// The two scope slots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, schemars::JsonSchema, Deserialize)]
pub struct Scopes {
    /// The global toolchain tier's applied entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<Applied>,
    // A sibling of `global`, not a member: reshaping that array fails every live carrier's decode.
    /// Pre-apply values for the **global** scope's constants.
    ///
    /// Captured against the **pre-global** environment. Omitted when empty.
    // Without it, a constant `ocx remove --global` retires has no prior and stays in the shell for its life.
    #[serde(default, skip_serializing_if = "Priors::is_empty")]
    pub global_priors: Priors,
    /// The resolved project tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectScope>,
}

/// The decoded payload of `__OCX_ENV_STATE`.
// `v` versions shape, the envelope tag encoding: a change that is both bumps both; `v` is additive-only.
// A shape break must ship a `v-1` revert-read arm in the same release, or live shells lose their revert set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema, Deserialize)]
pub struct Ledger {
    /// Schema version of the payload shape.
    pub v: u8,
    /// Watch-set fingerprint.
    ///
    /// Folds the raw `OCX_CONSENT_*` values, the recorded config-tier paths and the
    /// project's consent stamp; `verdict` expires when it changes.
    pub fp: String,
    /// The cached negative verdict: `inert` or `noproject`, never `activate`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    // Recorded, not re-derived: `--reconcile` never sees `--config`, so a grant there would never expire `inert`.
    /// The config-tier paths in effect at compose time.
    ///
    /// An absent list falls back to re-deriving the paths. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<PathBuf>,

    // The shell gates on the watch list baked into its hook body; without `ws` a stale list is never redefined.
    /// Digest of the ordered watch-path list baked into the shell's emitted gate.
    ///
    /// Omitted when empty.
    // Distinct from `fp`: an `ocx.lock` edit moves `fp` while the watched paths stay the same.
    // Absent decodes as empty, which no real digest equals: one redundant redefinition, never a missed one.
    // Written only by the emission that redefines the gate, or it describes a gate the shell does not have.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ws: String,

    // Without it every deferred message re-prints on every prompt while its cause holds.
    /// A digest of the deferred diagnostics the previous prompt printed.
    ///
    /// Kept alongside `fp` when the carrier is over cap. Omitted when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub messages_fp: String,

    /// Scopes the 16 KiB cap dropped; omitted when empty.
    ///
    /// A scope it names is reconciled exactly as an absent scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub over_cap: Vec<ScopeId>,
    /// What each scope applied.
    pub scopes: Scopes,
}

impl Ledger {
    /// Parse the `<tag> "." <payload>` envelope; `None` (treat as absent) on every failure.
    ///
    /// Strips `PATH`/`PATHEXT` constant claims and truncates [`Ledger::tiers`] rather than rejecting,
    /// since refusing the carrier discards a live shell's revert set.
    pub fn decode(raw: &str) -> Option<Ledger> {
        if raw.len() > MAX_CARRIER_BYTES {
            return None;
        }
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

    /// Encode to `1.<base64url(compact json)>`.
    ///
    /// Over the 16 KiB cap it emits a marker-only ledger with both scope payloads dropped;
    /// `None` (omit the variable) only when even the marker fails.
    pub fn encode(&self) -> Option<String> {
        let full = envelope(self)?;
        if full.len() <= MAX_CARRIER_BYTES {
            return Some(full);
        }
        let marker = Ledger {
            v: self.v,
            fp: self.fp.clone(),
            // Dropping it makes an over-cap shell redefine its gate on every prompt.
            ws: self.ws.clone(),
            verdict: self.verdict,
            // Without it the next prompt's fingerprint is not comparable.
            tiers: self.tiers.clone(),
            // Dropping it makes an over-cap shell re-announce every deferred message on every prompt.
            messages_fp: self.messages_fp.clone(),
            over_cap: self.dropped_scopes(),
            scopes: Scopes::default(),
        };
        // The marker keeps `fp`, or every prompt recomposes and re-overflows.
        envelope(&marker).filter(|encoded| encoded.len() <= MAX_CARRIER_BYTES)
    }

    /// The ledger the first prompt of a shell plans against.
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

    /// Scopes with a payload or already named over-cap, in emission order.
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

    /// Strip only the `PATH`/`PATHEXT` constant claims and priors; the rest of the record still acts.
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

    /// The applied entries of both scopes, global first.
    pub(super) fn applied_in_emission_order(&self) -> impl Iterator<Item = &LedgerEntry> {
        let global = self.scopes.global.iter().flatten();
        let project = self.scopes.project.iter().flat_map(|scope| scope.applied.iter());
        global.chain(project)
    }

    /// The recorded pre-apply value for `key`.
    ///
    /// A project prior equal to global's own constant is global's value, so the lookup chains to
    /// [`Scopes::global_priors`]; safe only because `retire_recorded_constant` skips keys `desired` still declares.
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

    /// Whether the project captured global's constant for `key` rather than the user's value.
    fn global_owns(&self, key: &str, prior: &Prior) -> bool {
        let Prior::Value(value) = prior else {
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

/// The carrier format's own safety net: the envelope codec, the
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
    use ocx_package::metadata::env::entry::Entry;
    use ocx_package::metadata::env::modifier::ModifierKind;

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
        assert!(ocx_util::env::is_reserved_ocx_key(CARRIER_KEY));
        assert!(ocx_util::env::is_valid_env_key(CARRIER_KEY));
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
