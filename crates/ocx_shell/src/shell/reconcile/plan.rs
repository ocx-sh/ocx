// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The typed reconciliation planner: diffs desired (D) against current (C), scoped by the
//! execution state's session records, and produces the [`Plan`] a shell arm renders.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{effective_separator, element_eq, element_norm, is_never_constant, key_eq, key_norm};
use ocx_config::env::Env;
use ocx_config::exec_state::{
    AppliedValue, ExecState, Owner, Prior as RecordedPrior, RecordedValue, RevertOutcome, Scope, SessionSource,
    Truncation, ValueKind, revert,
};
use ocx_console::Theme;
use ocx_package::metadata::env::apply::EnvEntriesExt;
use ocx_package::metadata::env::entry::Entry;
use ocx_package::metadata::env::list::DEFAULT_SEPARATOR;
use ocx_package::metadata::env::modifier::ModifierKind;

#[cfg(test)]
use super::unquote;

/// The structural version of [`Plan`]'s JSON wire shape; bumps on a breaking reshape, never on an added field.
///
/// A consumer seeing an absent or unrecognised `v` applies nothing that prompt.
pub const PLAN_VERSION: u8 = 1;

/// The typed three-way diff a single prompt executes; it never contains shell text.
///
/// Wire shape: `{"v":1,"sets":[{key,value,type,separator?}…],"removes":[[key,element,sep|null],…],
/// "restores":[[key,value|null],…]}`.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub v: u8,
    /// Apply: constants and list/path contributions.
    // Serialized as `SetWire`, which is what makes `type` the wire field name.
    #[serde(serialize_with = "serialize_sets")]
    pub sets: Vec<Entry>,
    /// Remove one element: `(key, element, separator)`; the separator names the kind (`None` path, `Some` list).
    pub removes: Vec<(String, String, Option<String>)>,
    /// Restore a constant: `(key, prior)`, where `None` means unset it.
    pub restores: Vec<(String, Option<String>)>,
}

/// One `sets` element as live shells decode it: `type`, not `kind`.
///
/// `separator` holds the **effective** separator, always present for `list` and omitted otherwise.
#[derive(Serialize)]
struct SetWire<'a> {
    key: &'a str,
    value: &'a str,
    #[serde(rename = "type")]
    kind: &'a ModifierKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    separator: Option<String>,
}

fn serialize_sets<S>(sets: &[Entry], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.collect_seq(sets.iter().map(|entry| SetWire {
        key: &entry.key,
        value: &entry.value,
        kind: &entry.kind,
        separator: match entry.kind {
            ModifierKind::List => Some(effective_separator(entry)),
            ModifierKind::Path | ModifierKind::Constant => None,
        },
    }))
}

/// The human-facing change summary for one prompt, or `None` when nothing changed: the keys this shell's own session records hold.
///
/// Inherited records are the outer frame's, so a key only they hold is neither added nor removed here.
pub fn session_summary(plan: &Plan, previous: &ExecState, next: &ExecState, theme: &Theme) -> Option<String> {
    marked_summary(
        plan,
        &session_keys(previous),
        &session_keys(next),
        switched_session_project(previous, next),
        theme,
    )
}

fn marked_summary(
    plan: &Plan,
    before: &HashSet<String>,
    after: &HashSet<String>,
    switched: Option<String>,
    theme: &Theme,
) -> Option<String> {
    let mut marks: BTreeMap<String, String> = BTreeMap::new();
    let touched = plan
        .sets
        .iter()
        .map(|entry| entry.key.as_str())
        .chain(plan.removes.iter().map(|(key, ..)| key.as_str()))
        .chain(plan.restores.iter().map(|(key, _)| key.as_str()))
        .filter(|key| !is_private_ocx_key(key));
    for key in touched {
        let norm = key_norm(key);
        let mark = match (before.contains(&norm), after.contains(&norm)) {
            (false, true) => '+',
            (true, true) => '~',
            (true, false) => '-',
            // Reachable via the unrecorded session `PATH` dirs; `-` would claim ocx stopped setting a key it just set.
            (false, false) => '+',
        };
        marks.entry(norm).or_insert_with(|| ink(theme, mark, key));
    }

    if marks.is_empty() {
        return None;
    }
    let tokens = marks.into_values().collect::<Vec<String>>().join(" ");
    let prefix = theme.aside("ocx:");
    match switched {
        Some(name) => Some(format!("{prefix} {tokens} {}", theme.aside(format!("({name})")))),
        None => Some(format!("{prefix} {tokens}")),
    }
}

/// `__OCX_*` keys are ocx's own bookkeeping, not part of the user's toolchain.
fn is_private_ocx_key(key: &str) -> bool {
    key.get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case("__OCX_"))
}

/// One `+KEY` / `~KEY` / `-KEY` token, inked by what its mark means.
fn ink(theme: &Theme, mark: char, key: &str) -> String {
    let token = format!("{mark}{key}");
    match mark {
        '+' => theme.ok(token),
        '~' => theme.tag(token),
        _ => theme.alert(token),
    }
}

/// The new project's directory name, only when this prompt switched from one recorded project to another.
///
/// The hint comes from received state, so the final component passes `display_safe` before it is printed.
fn switched_session_project(previous: &ExecState, next: &ExecState) -> Option<String> {
    let entered = next.session.project.as_ref()?;
    let left = previous.session.project.as_ref()?;
    if left.dir == entered.dir {
        return None;
    }
    let name = entered.dir.file_name().unwrap_or(entered.dir.as_os_str());
    Some(ocx_config::exec_state::display_safe(&name.to_string_lossy()))
}

/// The normalized keys of the records this shell's session owns.
fn session_keys(state: &ExecState) -> HashSet<String> {
    state
        .applied
        .iter()
        .filter(|record| matches!(record.owner, Owner::Session { .. }))
        .map(|record| key_norm(&record.key))
        .collect()
}

/// The admission gate, warning once per refused key; without it the ledger records keys no arm can ever remove.
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

fn admits(entry: &Entry) -> Result<(), &'static str> {
    if matches!(entry.kind, ModifierKind::Constant) && is_never_constant(&entry.key) {
        Err("PATH and PATHEXT are never constant-kind")
    } else {
        crate::shell::is_emittable(entry)
    }
}

/// [`emittable`] without its logging, for callers that record what the planner admits.
fn emittable_entries(desired: &[Entry]) -> Vec<&Entry> {
    desired.iter().filter(|entry| admits(entry).is_ok()).collect()
}

/// The entries whose application would change something, so a quiet prompt emits an empty plan.
///
/// A constant compares against the ledger, not the live env, or it re-emits over a mid-session override.
fn apply_set(emittable: &[&Entry], constant_applied: impl Fn(&Entry) -> bool, current: &Env) -> Vec<Entry> {
    let settled = settled_keys(emittable, current);
    emittable
        .iter()
        .filter(|entry| match entry.kind {
            ModifierKind::Constant => !constant_applied(entry),
            ModifierKind::Path | ModifierKind::List => !settled.contains(&key_norm(&entry.key)),
        })
        .map(|entry| (*entry).clone())
        .collect()
}

/// The path/list keys whose whole fold via [`EnvEntriesExt::apply_entries`] is **already live**.
///
/// A key any scope declares constant never settles here, or a mid-session override is clobbered.
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
                // Mixed kinds take the byte-exact list rule; over-settling silently drops a change.
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
    let mut probe = Env::clean();
    for (key, _) in candidates.values() {
        if let Some(value) = current.get(key) {
            probe.set(*key, value);
        }
    }
    probe.apply_entries(&folded);

    candidates
        .into_iter()
        .filter(|(norm, (key, kind))| {
            !constants.contains(norm) && value_settled(probe.get(key), current.get(key), kind)
        })
        .map(|(norm, _)| norm)
        .collect()
}

/// Whether folded and live are equal under the kind's own rule, never raw `==`.
///
/// Raw `==` has no fixed point on Windows: `split_paths` unquotes, so a quoted segment never compares equal.
fn value_settled(folded: Option<&OsStr>, live: Option<&OsStr>, kind: &ModifierKind) -> bool {
    match (folded, live) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(folded), Some(live)) => match kind {
            ModifierKind::Path => path_segments_eq(folded, live),
            ModifierKind::List | ModifierKind::Constant => folded == live,
        },
    }
}

fn path_segments_eq(left: &OsStr, right: &OsStr) -> bool {
    let mut left = std::env::split_paths(left);
    let mut right = std::env::split_paths(right);
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(left), Some(right)) if path_segment_eq(&left, &right) => continue,
            _ => return false,
        }
    }
}

fn path_segment_eq(left: &Path, right: &Path) -> bool {
    // Byte-identical first, so an unchanged non-UTF-8 segment still settles.
    left == right
        || match (left.to_str(), right.to_str()) {
            (Some(left), Some(right)) => element_eq(left, right, &ModifierKind::Path),
            _ => false,
        }
}

/// The lost-ledger repair: remove every prefix-owned segment of C that D does not want.
///
/// Subtractive, not move-to-front, or a digest bump leaves both the old and new `packages/…/bin` on `PATH`.
fn repair_owned_segments(
    declared: &BTreeMap<String, &Entry>,
    recorded_path_keys: &[(String, &str)],
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
    for (norm, key) in recorded_path_keys {
        if !declared.contains_key(norm) {
            keys.insert((norm.clone(), *key));
        }
    }

    if keys.is_empty() {
        return Vec::new();
    }
    // Once per reconcile, never per segment: a `realpath` per `PATH` element busts the prompt budget.
    let owned = owned_spellings(owned_prefixes);

    let mut removals = Vec::new();
    for (norm, key) in keys {
        let Some(value) = current.get(key) else { continue };
        for segment in std::env::split_paths(value) {
            let segment = segment.into_os_string();
            if segment.is_empty() || !is_owned(&segment, &owned) {
                continue;
            }
            // A non-UTF-8 segment is one no arm can name, so none can remove it.
            let Some(segment) = segment.to_str() else { continue };
            if contributed.contains(&(norm.clone(), element_norm(segment, &ModifierKind::Path))) {
                continue;
            }
            removals.push((key.to_owned(), segment.to_owned(), None));
        }
    }
    removals
}

fn declared_index<'a>(emittable: &[&'a Entry]) -> BTreeMap<String, &'a Entry> {
    // Later wins, so project's declaration of a key overrides global's.
    emittable.iter().map(|entry| (key_norm(&entry.key), *entry)).collect()
}

/// The elements D still wants, each normalised under its own kind.
fn contributed_elements(emittable: &[&Entry]) -> HashSet<(String, String)> {
    emittable
        .iter()
        .filter(|entry| matches!(entry.kind, ModifierKind::Path | ModifierKind::List))
        .map(|entry| (key_norm(&entry.key), element_norm(&entry.value, &entry.kind)))
        .collect()
}

/// One removal's identity, the hashable form of the `key_eq`/`element_eq`/separator triple.
type RemovalIdentity = (String, String, Option<String>);

/// Push a removal unless the same key, element and separator is already planned.
///
/// A set, not a scan of `removes`: the carrier is user-writable, so a scan is quadratic in its length.
fn push_removal(removes: &mut Vec<RemovalIdentity>, seen: &mut HashSet<RemovalIdentity>, removal: RemovalIdentity) {
    let kind = removal_kind(&removal.2);
    let identity = (key_norm(&removal.0), element_norm(&removal.1, &kind), removal.2.clone());
    if seen.insert(identity) {
        removes.push(removal);
    }
}

fn removal_kind(separator: &Option<String>) -> ModifierKind {
    match separator {
        None => ModifierKind::Path,
        Some(_) => ModifierKind::List,
    }
}

/// Every owned prefix in the caller's spelling **and** its resolved one.
///
/// `$OCX_HOME` is often a symlink that `PATH` holds resolved; either spelling alone leaves stale segments behind.
fn owned_spellings(owned_prefixes: &[&Path]) -> Vec<PathBuf> {
    let mut spellings: Vec<PathBuf> = Vec::with_capacity(owned_prefixes.len());
    for prefix in owned_prefixes {
        spellings.push(prefix.to_path_buf());
        if let Ok(resolved) = std::fs::canonicalize(prefix)
            && !spellings.contains(&resolved)
        {
            spellings.push(resolved);
        }
    }
    spellings
}

/// Component-wise, never a byte prefix, or `.ocx-backup` counts as owned by an `$OCX_HOME` of `.ocx`.
fn is_owned(segment: &OsStr, owned: &[PathBuf]) -> bool {
    let segment = Path::new(segment);
    owned.iter().any(|prefix| segment.starts_with(prefix))
}

// -- The session planner: the hook over the inherited execution state -------------------------

/// The scopes one prompt composes, after the inherited layer has been subtracted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Composition {
    /// Compose the global tier.
    pub global: bool,
    /// Compose this project, by its dunce-canonical root.
    pub project: Option<PathBuf>,
}

/// The hook's desired composition under `FollowCwd`.
///
/// The hook never composes a scope `inherited_scopes` already holds; it only adds missing ones.
/// `project_root` is the root of the project the working directory resolves to, if any. It is
/// compared dunce-canonical against the roots as the state wrote them; those are never resolved here.
pub fn desired_composition(inherited_scopes: &[Scope], project_root: Option<&Path>) -> Composition {
    let root = project_root.map(canonical_root);
    let inside_inherited_project = root.as_ref().is_some_and(|root| {
        inherited_scopes
            .iter()
            .any(|scope| matches!(scope, Scope::Project { root: held } if held == root))
    });
    if inside_inherited_project {
        // The inherited layer stands, the global tier included.
        return Composition::default();
    }
    Composition {
        global: !inherited_scopes.contains(&Scope::Global),
        project: root,
    }
}

/// The canonical form of a project root as [`ExecState::add_scope`] stores it.
///
/// Asked of the state itself, so the planner and the writer share one spelling.
fn canonical_root(root: &Path) -> PathBuf {
    let mut probe = ExecState::default();
    probe.add_scope(Scope::Project {
        root: root.to_path_buf(),
    });
    match probe.inherited_scopes.pop() {
        Some(Scope::Project { root }) => root,
        _ => root.to_path_buf(),
    }
}

/// Whether the hook may strip owned-prefix elements it has no record of.
///
/// Only after the writer dropped every applied record (rung 2) and only while no inherited scope
/// is known: with one known, an unrecorded element may be an outer frame's. Never after rung 3, where
/// an empty list proves nothing because the list was lost with the payload.
pub fn may_repair_owned_prefixes(state: &ExecState) -> bool {
    state.truncated == Truncation::AllAppliedDropped && state.inherited_scopes.is_empty()
}

/// Diff the desired composition against the live environment, scoped by the state's records.
///
/// The hook writes only session data: it retires `Session` `Composed` records alone, never an element
/// an `Inherited` or `Invocation` record names, and never writes a key or element an `Invocation`
/// record owns. The received home and policy play no part. `owned_prefixes` drives the repair of
/// [`may_repair_owned_prefixes`].
pub fn plan_session(desired: &[Entry], current: &Env, state: &ExecState, owned_prefixes: &[&Path]) -> Plan {
    let emittable = emittable(desired);
    let declared = declared_index(&emittable);
    let contributed = contributed_elements(&emittable);
    let guard = RecordGuard::new(&state.applied);

    let writable: Vec<&Entry> = emittable
        .iter()
        .copied()
        .filter(|entry| !guard.invocation_owns(entry))
        .collect();
    let composed: Vec<&AppliedValue> = state.applied.iter().filter(|record| is_composed(record)).collect();

    let mut plan = Plan {
        v: PLAN_VERSION,
        sets: apply_set(&writable, |entry| session_constant_applied(&composed, entry), current),
        removes: Vec::new(),
        restores: Vec::new(),
    };

    // Reverse application order, so a layered constant unwinds to the value it shadowed.
    let mut scratch = current.clone();
    let mut planned_removals: HashSet<RemovalIdentity> = HashSet::new();
    for record in composed.iter().rev() {
        match record.kind {
            ValueKind::Path | ValueKind::List => {
                if let Some(removal) = retire_session_element(record, &declared, &contributed, &guard, &mut scratch) {
                    push_removal(&mut plan.removes, &mut planned_removals, removal);
                }
            }
            ValueKind::Constant => {
                if let Some(restore) = retire_session_constant(record, &declared, &mut scratch) {
                    plan.restores.push(restore);
                }
            }
        }
    }

    if may_repair_owned_prefixes(state) {
        let recorded_path_keys: Vec<(String, &str)> = composed
            .iter()
            .filter(|record| matches!(record.kind, ValueKind::Path))
            .map(|record| (key_norm(&record.key), record.key.as_str()))
            .collect();
        let removals = repair_owned_segments(&declared, &recorded_path_keys, &contributed, current, owned_prefixes);
        for removal in removals {
            let identity = (key_norm(&removal.0), element_norm(&removal.1, &ModifierKind::Path));
            if !guard.names(&identity) {
                push_removal(&mut plan.removes, &mut planned_removals, removal);
            }
        }
    }

    plan
}

/// The entries of `desired` the hook may write: the emittable ones that no `Invocation` record owns.
///
/// What a record-writing caller must record, so a refresh never claims a key or element
/// that `ocx shell --env` put there. [`plan_session`] applies the same filter to its sets.
pub fn session_writable(desired: &[Entry], state: &ExecState) -> Vec<Entry> {
    let guard = RecordGuard::new(&state.applied);
    emittable_entries(desired)
        .into_iter()
        .filter(|entry| !guard.invocation_owns(entry))
        .cloned()
        .collect()
}

/// A record the hook owns: composed from `ocx.toml` groups or the global tier, refreshed every prompt.
fn is_composed(record: &AppliedValue) -> bool {
    matches!(
        record.owner,
        Owner::Session {
            source: SessionSource::Composed,
            ..
        }
    )
}

fn modifier_kind(kind: ValueKind) -> ModifierKind {
    match kind {
        ValueKind::Path => ModifierKind::Path,
        ValueKind::List => ModifierKind::List,
        ValueKind::Constant => ModifierKind::Constant,
    }
}

/// The elements and keys records the hook does not own hold, which it must neither remove nor overwrite.
struct RecordGuard {
    /// `(key, element)` an `Inherited` or `Invocation` path or list record names.
    named: HashSet<(String, String)>,
    /// The same, for `Invocation` records alone.
    invoked_elements: HashSet<(String, String)>,
    /// Constant keys an `Invocation` record owns.
    invoked_keys: HashSet<String>,
}

impl RecordGuard {
    fn new(records: &[AppliedValue]) -> Self {
        let mut guard = Self {
            named: HashSet::new(),
            invoked_elements: HashSet::new(),
            invoked_keys: HashSet::new(),
        };
        for record in records {
            let invoked = matches!(
                record.owner,
                Owner::Session {
                    source: SessionSource::Invocation,
                    ..
                }
            );
            if !invoked && !matches!(record.owner, Owner::Inherited) {
                continue;
            }
            match (&record.value, record.kind) {
                (RecordedValue::Literal(element), ValueKind::Path | ValueKind::List) => {
                    let identity = (
                        key_norm(&record.key),
                        element_norm(element, &modifier_kind(record.kind)),
                    );
                    if invoked {
                        guard.invoked_elements.insert(identity.clone());
                    }
                    guard.named.insert(identity);
                }
                (_, ValueKind::Constant) if invoked => {
                    guard.invoked_keys.insert(key_norm(&record.key));
                }
                _ => {}
            }
        }
        guard
    }

    fn names(&self, identity: &(String, String)) -> bool {
        self.named.contains(identity)
    }

    /// Whether writing `entry` would overwrite what an `Invocation` record owns.
    fn invocation_owns(&self, entry: &Entry) -> bool {
        match entry.kind {
            ModifierKind::Constant => self.invoked_keys.contains(&key_norm(&entry.key)),
            ModifierKind::Path | ModifierKind::List => self
                .invoked_elements
                .contains(&(key_norm(&entry.key), element_norm(&entry.value, &entry.kind))),
        }
    }
}

/// Whether `entry`, a constant, is already what a `Composed` record put in the environment.
fn session_constant_applied(composed: &[&AppliedValue], entry: &Entry) -> bool {
    // The newest record for the key: a later scope's value shadows an earlier one's, and writing the earlier
    // one back is what leaving the later scope has to do.
    composed
        .iter()
        .rfind(|record| matches!(record.kind, ValueKind::Constant) && key_eq(&record.key, &entry.key))
        .is_some_and(|record| {
            let RecordedValue::Literal(value) = &record.value;
            element_eq(value, &entry.value, &ModifierKind::Constant)
        })
}

/// Remove an element a `Composed` record holds and D no longer wants.
///
/// Whether the removal is still safe is [`revert`]'s answer, asked of `scratch`, which each
/// reverted record also updates.
fn retire_session_element(
    record: &AppliedValue,
    declared: &BTreeMap<String, &Entry>,
    contributed: &HashSet<(String, String)>,
    guard: &RecordGuard,
    scratch: &mut Env,
) -> Option<RemovalIdentity> {
    let RecordedValue::Literal(element) = &record.value;
    let norm = key_norm(&record.key);
    let current = declared.get(&norm);
    let kind = current.map_or_else(|| modifier_kind(record.kind), |declared| declared.kind.clone());
    if contributed.contains(&(norm.clone(), element_norm(element, &kind))) {
        return None;
    }
    if guard.names(&(norm, element_norm(element, &modifier_kind(record.kind)))) {
        return None;
    }
    let separator = match current {
        Some(current) => match current.kind {
            ModifierKind::Constant => return None,
            ModifierKind::Path => None,
            ModifierKind::List => Some(effective_separator(current)),
        },
        None => match record.kind {
            ValueKind::Path => None,
            _ => Some(record.separator.clone().unwrap_or_else(|| DEFAULT_SEPARATOR.to_owned())),
        },
    };
    matches!(revert(scratch, record), RevertOutcome::Reverted).then(|| (record.key.clone(), element.clone(), separator))
}

/// Restore a constant a `Composed` record holds and D no longer declares, to the prior it recorded.
fn retire_session_constant(
    record: &AppliedValue,
    declared: &BTreeMap<String, &Entry>,
    scratch: &mut Env,
) -> Option<(String, Option<String>)> {
    if is_never_constant(&record.key) || declared.contains_key(&key_norm(&record.key)) {
        return None;
    }
    if !matches!(revert(scratch, record), RevertOutcome::Reverted) {
        return None;
    }
    let restored = match &record.prior {
        Some(RecordedPrior::Value(prior)) => Some(prior.clone()),
        _ => None,
    };
    Some((record.key.clone(), restored))
}

/// A `PATH`-style fixture value spelled the way the platform joins one.
///
/// A `:` literal is one opaque segment to `std::env::split_paths` on Windows,
/// where the separator is `;` — so a POSIX-spelled fixture asks a different
/// question there than the one it asks on Unix, and the planner's segment-wise
/// answer stops being observable. Building the value through the exact inverse
/// of the splitter production uses keeps both halves in one vocabulary; on
/// POSIX it is the identity.
#[cfg(test)]
fn joined(segments: &[&str]) -> String {
    std::env::join_paths(segments)
        .expect("a fixture segment must not carry the platform path separator")
        .to_string_lossy()
        .into_owned()
}

/// The session planner: which scopes the hook composes and which records it may touch.
#[cfg(test)]
mod session_tests {
    use super::*;

    use ocx_config::exec_state::{
        AppliedValue, Owner, Prior as RecordedPrior, ProjectScopeHint, RecordedValue, ScopeId,
    };
    use ocx_config::exec_state::{SessionSource, Truncation, ValueKind};

    const OWNED: &str = "/home/u/.ocx";

    fn p() -> PathBuf {
        PathBuf::from("/work/p")
    }

    fn q() -> PathBuf {
        PathBuf::from("/work/q")
    }

    fn project(root: PathBuf) -> Scope {
        Scope::Project { root }
    }

    fn compose(inherited: &[Scope], cwd_project: Option<PathBuf>) -> Composition {
        desired_composition(inherited, cwd_project.as_deref())
    }

    fn global_only() -> Composition {
        Composition {
            global: true,
            project: None,
        }
    }

    fn nothing() -> Composition {
        Composition::default()
    }

    // -- The desired-composition table ------------------------------------------------

    #[test]
    fn inside_an_inherited_project_nothing_is_composed_global_included() {
        assert_eq!(compose(&[project(p())], Some(p())), nothing());
    }

    #[test]
    fn inside_an_inherited_project_with_global_inherited_too_composes_nothing() {
        assert_eq!(compose(&[Scope::Global, project(p())], Some(p())), nothing());
    }

    #[test]
    fn an_uninherited_project_composes_global_and_itself() {
        assert_eq!(
            compose(&[], Some(q())),
            Composition {
                global: true,
                project: Some(q())
            }
        );
    }

    #[test]
    fn an_uninherited_project_leaves_out_an_inherited_global() {
        assert_eq!(
            compose(&[Scope::Global], Some(q())),
            Composition {
                global: false,
                project: Some(q())
            }
        );
    }

    #[test]
    fn an_uninherited_project_layers_over_another_inherited_project() {
        assert_eq!(
            compose(&[project(p())], Some(q())),
            Composition {
                global: true,
                project: Some(q())
            }
        );
    }

    #[test]
    fn outside_every_project_the_global_tier_is_composed() {
        assert_eq!(compose(&[], None), global_only());
    }

    #[test]
    fn outside_every_project_an_inherited_global_is_not_composed_again() {
        assert_eq!(compose(&[Scope::Global], None), nothing());
    }

    #[test]
    fn outside_every_project_an_inherited_project_does_not_hide_the_global_tier() {
        assert_eq!(compose(&[project(p())], None), global_only());
    }

    /// `PayloadDropped` loses the list with the payload, so an empty list re-applies every scope (C2 residual).
    #[test]
    fn an_empty_inherited_list_composes_every_scope() {
        assert_eq!(
            compose(&[], Some(p())),
            Composition {
                global: true,
                project: Some(p())
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_spelling_of_an_inherited_root_is_the_same_project() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let real = std::fs::canonicalize(dir.path()).expect("the temporary directory resolves");
        let link = real.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("a symlink to the project root");

        assert_eq!(compose(&[project(real)], Some(link)), nothing());
    }

    // -- The hook writes only session data ---------------------------------------------

    fn session(source: SessionSource) -> Owner {
        Owner::Session {
            scope: ScopeId::Project,
            source,
        }
    }

    fn composed() -> Owner {
        session(SessionSource::Composed)
    }

    fn invocation() -> Owner {
        session(SessionSource::Invocation)
    }

    fn record(key: &str, value: &str, kind: ValueKind, owner: Owner) -> AppliedValue {
        AppliedValue {
            key: key.to_owned(),
            value: RecordedValue::Literal(value.to_owned()),
            kind,
            separator: matches!(kind, ValueKind::List).then(|| ":".to_owned()),
            pre_existing: false,
            owner,
            prior: None,
        }
    }

    fn path_record(element: &str, owner: Owner) -> AppliedValue {
        record("PATH", element, ValueKind::Path, owner)
    }

    fn constant_record(key: &str, value: &str, prior: RecordedPrior, owner: Owner) -> AppliedValue {
        AppliedValue {
            prior: Some(prior),
            ..record(key, value, ValueKind::Constant, owner)
        }
    }

    fn state_with(applied: Vec<AppliedValue>) -> ExecState {
        ExecState {
            applied,
            ..ExecState::default()
        }
    }

    fn env_with(pairs: &[(&str, &str)]) -> Env {
        let mut env = Env::clean();
        for (key, value) in pairs {
            env.set(*key, *value);
        }
        env
    }

    fn live_path(segments: &[&str]) -> Env {
        env_with(&[("PATH", &joined(segments))])
    }

    fn owned() -> Vec<&'static Path> {
        vec![Path::new(OWNED)]
    }

    fn plan_for(desired: &[Entry], current: &Env, state: &ExecState) -> Plan {
        plan_session(desired, current, state, &owned())
    }

    fn removed_elements(plan: &Plan) -> Vec<&str> {
        plan.removes.iter().map(|(_, element, _)| element.as_str()).collect()
    }

    fn path_entry(element: &str) -> Entry {
        Entry {
            key: "PATH".to_owned(),
            value: element.to_owned(),
            kind: ModifierKind::Path,
            separator: None,
        }
    }

    fn constant_entry(key: &str, value: &str) -> Entry {
        Entry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind: ModifierKind::Constant,
            separator: None,
        }
    }

    fn is_quiet(plan: &Plan) -> bool {
        plan.sets.is_empty() && plan.removes.is_empty() && plan.restores.is_empty()
    }

    /// EC-LEDGER-009: a project switch retires the session's own layer before the new scope applies.
    #[test]
    fn only_session_composed_records_are_retired() {
        let state = state_with(vec![
            path_record("/outer/bin", Owner::Inherited),
            path_record("/invoked/bin", invocation()),
            path_record("/session/bin", composed()),
        ]);
        let current = live_path(&["/session/bin", "/invoked/bin", "/outer/bin"]);

        let planned = plan_for(&[], &current, &state);

        assert_eq!(removed_elements(&planned), vec!["/session/bin"]);
        assert!(planned.restores.is_empty());
    }

    #[test]
    fn an_inherited_constant_is_never_restored() {
        let state = state_with(vec![constant_record(
            "JAVA_HOME",
            "/jdk",
            RecordedPrior::Unset,
            Owner::Inherited,
        )]);

        let planned = plan_for(&[], &env_with(&[("JAVA_HOME", "/jdk")]), &state);

        assert!(is_quiet(&planned), "{planned:?}");
    }

    #[test]
    fn an_invocation_constant_is_never_restored() {
        let state = state_with(vec![constant_record(
            "JAVA_HOME",
            "/jdk",
            RecordedPrior::Unset,
            invocation(),
        )]);

        let planned = plan_for(&[], &env_with(&[("JAVA_HOME", "/jdk")]), &state);

        assert!(is_quiet(&planned), "{planned:?}");
    }

    #[test]
    fn a_composed_element_an_inherited_record_names_is_kept() {
        let state = state_with(vec![
            path_record("/shared/bin", Owner::Inherited),
            path_record("/shared/bin", composed()),
            path_record("/session/bin", composed()),
        ]);
        let current = live_path(&["/session/bin", "/shared/bin"]);

        let planned = plan_for(&[], &current, &state);

        assert_eq!(removed_elements(&planned), vec!["/session/bin"]);
    }

    #[test]
    fn a_composed_element_an_invocation_record_names_is_kept() {
        let state = state_with(vec![
            path_record("/shared/bin", invocation()),
            path_record("/shared/bin", composed()),
        ]);

        let planned = plan_for(&[], &live_path(&["/shared/bin"]), &state);

        assert!(planned.removes.is_empty(), "{planned:?}");
    }

    #[test]
    fn a_pre_existing_composed_element_is_kept() {
        let state = state_with(vec![AppliedValue {
            pre_existing: true,
            ..path_record("/user/bin", composed())
        }]);

        let planned = plan_for(&[], &live_path(&["/user/bin"]), &state);

        assert!(planned.removes.is_empty(), "{planned:?}");
    }

    #[test]
    fn a_composed_element_the_desired_set_still_wants_stays() {
        let state = state_with(vec![path_record("/session/bin", composed())]);

        let planned = plan_for(&[path_entry("/session/bin")], &live_path(&["/session/bin"]), &state);

        assert!(is_quiet(&planned), "{planned:?}");
    }

    /// EC-LIST-007: a list removal uses the record's own separator.
    #[test]
    fn a_list_element_is_removed_with_its_separator() {
        let mut list = record("OCX_LIST", "a", ValueKind::List, composed());
        list.separator = Some(",".to_owned());
        let state = state_with(vec![list]);

        let planned = plan_for(&[], &env_with(&[("OCX_LIST", "x,a")]), &state);

        assert_eq!(
            planned.removes,
            vec![("OCX_LIST".to_owned(), "a".to_owned(), Some(",".to_owned()))]
        );
    }

    /// EC-LEDGER-010: a recorded constant absent from the desired set is reverted, bounded by what it recorded.
    #[test]
    fn a_composed_constant_is_restored_to_its_prior() {
        let state = state_with(vec![
            constant_record("EDITOR", "ocx-edit", RecordedPrior::Value("vim".to_owned()), composed()),
            constant_record("JAVA_HOME", "/jdk", RecordedPrior::Unset, composed()),
        ]);
        let current = env_with(&[("EDITOR", "ocx-edit"), ("JAVA_HOME", "/jdk")]);

        let planned = plan_for(&[], &current, &state);

        let mut restores = planned.restores.clone();
        restores.sort();
        assert_eq!(
            restores,
            vec![
                ("EDITOR".to_owned(), Some("vim".to_owned())),
                ("JAVA_HOME".to_owned(), None)
            ]
        );
    }

    #[test]
    fn a_constant_changed_since_it_was_applied_is_left_alone() {
        let state = state_with(vec![constant_record(
            "EDITOR",
            "ocx-edit",
            RecordedPrior::Value("vim".to_owned()),
            composed(),
        )]);

        let planned = plan_for(&[], &env_with(&[("EDITOR", "emacs")]), &state);

        assert!(planned.restores.is_empty(), "{planned:?}");
    }

    #[test]
    fn a_withheld_prior_is_never_restored() {
        let state = state_with(vec![constant_record(
            "EDITOR",
            "ocx-edit",
            RecordedPrior::Withheld,
            composed(),
        )]);

        let planned = plan_for(&[], &env_with(&[("EDITOR", "ocx-edit")]), &state);

        assert!(planned.restores.is_empty(), "{planned:?}");
    }

    #[test]
    fn a_refresh_never_writes_an_element_an_invocation_record_owns() {
        let state = state_with(vec![path_record("/invoked/bin", invocation())]);
        let desired = [path_entry("/invoked/bin"), path_entry("/project/bin")];

        let planned = plan_for(&desired, &live_path(&["/invoked/bin"]), &state);

        let written: Vec<&str> = planned.sets.iter().map(|entry| entry.value.as_str()).collect();
        assert_eq!(written, vec!["/project/bin"]);
    }

    #[test]
    fn a_refresh_never_writes_a_key_an_invocation_record_owns() {
        let state = state_with(vec![constant_record(
            "MODE",
            "invoked",
            RecordedPrior::Unset,
            invocation(),
        )]);
        let desired = [constant_entry("MODE", "project"), constant_entry("OTHER", "1")];

        let planned = plan_for(&desired, &env_with(&[("MODE", "invoked")]), &state);

        let written: Vec<&str> = planned.sets.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(written, vec!["OTHER"]);
        assert!(planned.restores.is_empty(), "{planned:?}");
    }

    #[test]
    fn a_settled_prompt_plans_nothing() {
        let state = state_with(vec![
            path_record("/session/bin", composed()),
            constant_record("MODE", "project", RecordedPrior::Unset, composed()),
        ]);
        let desired = [path_entry("/session/bin"), constant_entry("MODE", "project")];
        let current = Env::clean();
        let mut current = current;
        current.set("PATH", joined(&["/session/bin"]));
        current.set("MODE", "project");

        let planned = plan_for(&desired, &current, &state);

        assert!(is_quiet(&planned), "{planned:?}");
    }

    #[test]
    fn a_secret_named_constant_already_recorded_is_not_applied_again() {
        let state = state_with(vec![constant_record(
            "API_TOKEN",
            "fixture-secret-value",
            RecordedPrior::Unset,
            composed(),
        )]);
        let same = [constant_entry("API_TOKEN", "fixture-secret-value")];
        let rotated = [constant_entry("API_TOKEN", "another-secret-value")];
        let live = env_with(&[("API_TOKEN", "fixture-secret-value")]);

        assert!(plan_for(&same, &live, &state).sets.is_empty());
        assert_eq!(plan_for(&rotated, &live, &state).sets.len(), 1);
    }

    /// A `PATH` constant record restores nothing, even where its value matches the live `PATH` byte for byte.
    #[test]
    fn a_session_constant_record_keyed_path_is_never_restored() {
        let live = joined(&["/usr/bin", "/bin"]);
        let state = state_with(vec![constant_record(
            "PATH",
            &live,
            RecordedPrior::Value("/attacker-chosen-dir".to_owned()),
            composed(),
        )]);

        let planned = plan_for(&[], &env_with(&[("PATH", &live)]), &state);

        assert!(planned.restores.is_empty(), "{planned:?}");
    }

    /// A record whose value is not the live one restores nothing: the user's value stands.
    #[test]
    fn a_record_on_another_value_than_the_live_one_never_restores() {
        let state = state_with(vec![constant_record(
            "API_TOKEN",
            "recorded-value",
            RecordedPrior::Value("attacker-chosen-value".to_owned()),
            composed(),
        )]);

        let planned = plan_for(&[], &env_with(&[("API_TOKEN", "fixture-secret-value")]), &state);

        assert!(planned.restores.is_empty(), "{planned:?}");
    }

    /// `cd` into a project whose root is inherited: the hook's own global layer goes, the outer one stands.
    #[test]
    fn entering_an_inherited_project_retires_only_the_sessions_own_layer() {
        let state = state_with(vec![
            path_record("/outer/p/bin", Owner::Inherited),
            path_record("/session/global/bin", composed()),
        ]);
        let current = live_path(&["/session/global/bin", "/outer/p/bin"]);

        let planned = plan_for(&[], &current, &state);
        let mut after = current.clone();
        let mut after_path = after.get("PATH").map(std::ffi::OsString::from).expect("PATH is set");
        for (_, element, _) in &planned.removes {
            after_path = ocx_util::path::remove_segment(&after_path, OsStr::new(element));
        }
        after.set("PATH", after_path);

        let remaining: Vec<String> = std::env::split_paths(after.get("PATH").expect("PATH is set"))
            .map(|segment| segment.to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining, vec!["/outer/p/bin"]);
    }

    // -- Owned-prefix repair ------------------------------------------------------------

    fn truncated(state: ExecState, truncation: Truncation) -> ExecState {
        ExecState {
            truncated: truncation,
            ..state
        }
    }

    fn stale_owned_path() -> Env {
        live_path(&["/home/u/.ocx/packages/stale/bin", "/usr/bin"])
    }

    #[test]
    fn repair_strips_owned_segments_after_all_applied_dropped_with_no_inherited_scope() {
        let state = truncated(ExecState::default(), Truncation::AllAppliedDropped);

        let planned = plan_for(
            &[path_entry("/home/u/.ocx/packages/fresh/bin")],
            &stale_owned_path(),
            &state,
        );

        assert_eq!(removed_elements(&planned), vec!["/home/u/.ocx/packages/stale/bin"]);
    }

    #[test]
    fn repair_waits_while_an_inherited_scope_is_known() {
        let mut state = truncated(ExecState::default(), Truncation::AllAppliedDropped);
        state.inherited_scopes = vec![project(p())];

        let planned = plan_for(
            &[path_entry("/home/u/.ocx/packages/fresh/bin")],
            &stale_owned_path(),
            &state,
        );

        assert!(planned.removes.is_empty(), "{planned:?}");
    }

    #[test]
    fn repair_never_runs_after_the_payload_was_dropped() {
        let state = truncated(ExecState::default(), Truncation::PayloadDropped);

        let planned = plan_for(
            &[path_entry("/home/u/.ocx/packages/fresh/bin")],
            &stale_owned_path(),
            &state,
        );

        assert!(planned.removes.is_empty(), "{planned:?}");
    }

    #[test]
    fn repair_never_runs_when_only_inherited_records_were_dropped() {
        let state = truncated(ExecState::default(), Truncation::InheritedDropped);

        let planned = plan_for(
            &[path_entry("/home/u/.ocx/packages/fresh/bin")],
            &stale_owned_path(),
            &state,
        );

        assert!(planned.removes.is_empty(), "{planned:?}");
    }

    /// EC-LIST-009: ownership is component-wise, so a directory that only shares a byte prefix survives.
    #[test]
    fn repair_keeps_a_lookalike_prefix_of_an_owned_directory() {
        let state = truncated(ExecState::default(), Truncation::AllAppliedDropped);
        let lookalike = format!("{OWNED}-backup/x/bin");
        let current = live_path(&[lookalike.as_str(), "/home/u/.ocx/packages/stale/bin", "/usr/bin"]);

        let planned = plan_for(&[path_entry("/home/u/.ocx/packages/fresh/bin")], &current, &state);

        assert_eq!(removed_elements(&planned), vec!["/home/u/.ocx/packages/stale/bin"]);
    }

    /// EC-LIST-011: a symlinked `$OCX_HOME` owns its resolved spelling too, so a stale element spelled through it is repaired.
    #[cfg(unix)]
    #[test]
    fn repair_strips_an_element_spelled_through_the_resolved_ocx_home() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let real = std::fs::canonicalize(dir.path())
            .expect("the temporary directory resolves")
            .join("real");
        std::fs::create_dir(&real).expect("the real home");
        let link = real.parent().expect("a parent").join("link");
        std::os::unix::fs::symlink(&real, &link).expect("a symlink to the real home");
        let stale = real.join("packages/old/bin");
        let state = truncated(ExecState::default(), Truncation::AllAppliedDropped);
        let current = live_path(&[stale.to_str().expect("utf-8"), "/usr/bin"]);

        let planned = plan_session(&[path_entry("/fresh/bin")], &current, &state, &[link.as_path()]);

        assert_eq!(removed_elements(&planned), vec![stale.to_str().expect("utf-8")]);
    }

    /// An absent state is a fresh follow-cwd session: an unrecorded element is an outer frame's value.
    #[test]
    fn an_absent_state_repairs_nothing() {
        let planned = plan_for(
            &[path_entry("/home/u/.ocx/packages/fresh/bin")],
            &stale_owned_path(),
            &ExecState::default(),
        );

        assert!(planned.removes.is_empty(), "{planned:?}");
    }

    #[test]
    fn repair_never_removes_an_element_an_inherited_record_names() {
        let state = truncated(
            state_with(vec![path_record("/home/u/.ocx/packages/outer/bin", Owner::Inherited)]),
            Truncation::AllAppliedDropped,
        );
        let current = live_path(&[
            "/home/u/.ocx/packages/outer/bin",
            "/home/u/.ocx/packages/stale/bin",
            "/usr/bin",
        ]);

        let planned = plan_for(&[path_entry("/home/u/.ocx/packages/fresh/bin")], &current, &state);

        assert_eq!(removed_elements(&planned), vec!["/home/u/.ocx/packages/stale/bin"]);
    }

    #[test]
    fn repair_keeps_what_the_desired_set_contributes() {
        let state = truncated(ExecState::default(), Truncation::AllAppliedDropped);
        let current = live_path(&["/home/u/.ocx/packages/fresh/bin", "/home/u/.ocx/packages/stale/bin"]);

        let planned = plan_for(&[path_entry("/home/u/.ocx/packages/fresh/bin")], &current, &state);

        assert_eq!(removed_elements(&planned), vec!["/home/u/.ocx/packages/stale/bin"]);
    }

    #[test]
    fn the_gate_is_exactly_all_applied_dropped_with_no_inherited_scope() {
        let allowed = |truncation, scopes: Vec<Scope>| {
            let mut state = truncated(ExecState::default(), truncation);
            state.inherited_scopes = scopes;
            may_repair_owned_prefixes(&state)
        };

        assert!(allowed(Truncation::AllAppliedDropped, Vec::new()));
        assert!(!allowed(Truncation::AllAppliedDropped, vec![Scope::Global]));
        assert!(!allowed(Truncation::PayloadDropped, Vec::new()));
        assert!(!allowed(Truncation::InheritedDropped, Vec::new()));
        assert!(!allowed(Truncation::None, Vec::new()));
    }

    /// A hook shell carries `OCX_HOME` from setup's files; the received `h` plays no part.
    #[test]
    fn a_received_home_changes_nothing() {
        let state = truncated(
            state_with(vec![path_record("/session/bin", composed())]),
            Truncation::AllAppliedDropped,
        );
        let hostile = ExecState {
            home: Some(PathBuf::from("/home/u")),
            ..state.clone()
        };
        let desired = [path_entry("/home/u/.ocx/packages/fresh/bin")];
        let current = live_path(&["/session/bin", "/home/u/.ocx/packages/stale/bin"]);

        let honest = plan_for(&desired, &current, &state);
        let plan_with_home = plan_for(&desired, &current, &hostile);

        assert_eq!(honest.removes, plan_with_home.removes);
        assert_eq!(honest.restores, plan_with_home.restores);
        assert_eq!(honest.sets.len(), plan_with_home.sets.len());
    }

    // -- the change summary over the execution state --------------------------------------------

    fn summary_plan(desired: &[Entry], current: &Env, state: &ExecState) -> Plan {
        plan_for(desired, current, state)
    }

    fn with_project_hint(mut state: ExecState, dir: &str) -> ExecState {
        state.session.project = Some(ProjectScopeHint {
            key: "0123456789abcdef".to_owned(),
            dir: PathBuf::from(dir),
        });
        state
    }

    /// A session record the previous prompt held and this one does not is `-`; a new one is `+`.
    #[test]
    fn the_session_summary_marks_added_and_removed_session_keys() {
        let previous = state_with(vec![constant_record(
            "OLD_KEY",
            "old",
            RecordedPrior::Unset,
            composed(),
        )]);
        let next = state_with(vec![constant_record(
            "NEW_KEY",
            "new",
            RecordedPrior::Unset,
            composed(),
        )]);
        let desired = [constant_entry("NEW_KEY", "new")];
        let current = env_with(&[("OLD_KEY", "old")]);
        let planned = summary_plan(&desired, &current, &previous);

        let line = session_summary(&planned, &previous, &next, &Theme::new(false)).expect("a change is summarised");

        assert_eq!(line, "ocx: +NEW_KEY -OLD_KEY");
    }

    /// An inherited record is the outer frame's: a key only it holds is new to this shell when the session sets it.
    #[test]
    fn the_session_summary_ignores_inherited_records() {
        let previous = state_with(vec![constant_record(
            "OUTER_KEY",
            "outer",
            RecordedPrior::Unset,
            Owner::Inherited,
        )]);
        let next = state_with(vec![
            constant_record("OUTER_KEY", "outer", RecordedPrior::Unset, Owner::Inherited),
            constant_record("OUTER_KEY", "mine", RecordedPrior::Unset, composed()),
        ]);
        let current = env_with(&[("OUTER_KEY", "outer")]);
        let planned = summary_plan(&[constant_entry("OUTER_KEY", "mine")], &current, &previous);
        assert!(
            !planned.sets.is_empty(),
            "the fixture must set the key, or the mark below is vacuous"
        );

        let line = session_summary(&planned, &previous, &next, &Theme::new(false)).expect("a change is summarised");

        assert_eq!(
            line, "ocx: +OUTER_KEY",
            "counting the inherited record would mark it '~'"
        );
    }

    /// A switch to another project names it; the directory comes from the state, so control characters go.
    #[test]
    fn the_session_summary_names_the_project_it_switched_to_and_strips_the_state_text() {
        let previous = with_project_hint(state_with(Vec::new()), "/work/one");
        let next = with_project_hint(state_with(Vec::new()), "/work/two\u{1b}[31mred");
        let desired = [constant_entry("KEY", "value")];
        let planned = summary_plan(&desired, &env_with(&[]), &previous);

        let line = session_summary(&planned, &previous, &next, &Theme::new(false)).expect("a change is summarised");

        assert_eq!(line, "ocx: +KEY (two[31mred)");
    }

    // -- a shadowed constant is applied again once its shadow retires ---------------------------

    fn scoped(scope: ScopeId) -> Owner {
        Owner::Session {
            scope,
            source: SessionSource::Composed,
        }
    }

    /// The project scope shadowed the global constant; leaving the project must write the global value back.
    #[test]
    fn leaving_a_project_that_shadowed_a_global_constant_writes_the_global_value_back() {
        let state = state_with(vec![
            constant_record("SHARED", "global", RecordedPrior::Unset, scoped(ScopeId::Global)),
            constant_record(
                "SHARED",
                "project",
                RecordedPrior::Value("global".to_owned()),
                scoped(ScopeId::Project),
            ),
        ]);
        let current = env_with(&[("SHARED", "project")]);

        let plan = plan_for(&[constant_entry("SHARED", "global")], &current, &state);

        assert_eq!(
            plan.sets.iter().map(|entry| entry.value.as_str()).collect::<Vec<_>>(),
            ["global"],
            "the newest record for the key says `project`, so `global` is not applied"
        );
        assert!(
            plan.restores.is_empty(),
            "the key is still declared, so nothing is restored"
        );
    }

    /// The control: when the newest record already holds the desired value nothing is written again.
    #[test]
    fn a_constant_the_newest_record_already_holds_is_not_written_again() {
        let state = state_with(vec![
            constant_record("SHARED", "global", RecordedPrior::Unset, scoped(ScopeId::Global)),
            constant_record(
                "SHARED",
                "project",
                RecordedPrior::Value("global".to_owned()),
                scoped(ScopeId::Project),
            ),
        ]);
        let current = env_with(&[("SHARED", "project")]);

        let plan = plan_for(&[constant_entry("SHARED", "project")], &current, &state);

        assert!(plan.sets.is_empty(), "got: {:?}", plan.sets);
    }
}

/// The `Plan` wire shape and the comparison rules the planner shares.
#[cfg(test)]
mod wire_tests {
    use super::*;

    fn entry(key: &str, value: &str, kind: ModifierKind, separator: Option<&str>) -> Entry {
        Entry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind,
            separator: separator.map(str::to_owned),
        }
    }

    #[test]
    fn the_plan_json_carries_a_structural_v_and_the_type_spelling() {
        let planned = Plan {
            v: PLAN_VERSION,
            sets: vec![
                entry("PATH", "/opt/bin", ModifierKind::Path, None),
                entry("CFLAGS", "-O2", ModifierKind::List, None),
            ],
            removes: vec![
                ("PATH".to_owned(), "/old/bin".to_owned(), None),
                ("CFLAGS".to_owned(), "-g".to_owned(), Some(" ".to_owned())),
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

    /// The settle compare is [`element_eq`]'s, applied per segment, not `==` on the raw whole value.
    #[test]
    fn the_settle_compare_is_not_byte_exact() {
        let folded = std::ffi::OsString::from(["/ocx/a/bin", "/program files/x"].join(ocx_util::path::PATH_SEPARATOR));
        let live =
            std::ffi::OsString::from(["/ocx/a/bin", "\"/program files/x\""].join(ocx_util::path::PATH_SEPARATOR));
        assert_ne!(folded, live, "the two spellings are not byte-equal");
        assert!(
            value_settled(Some(&folded), Some(&live), &ModifierKind::Path),
            "a retained segment that differs only by one quote pair is settled"
        );

        let moved = std::ffi::OsString::from(["/program files/x", "/ocx/a/bin"].join(ocx_util::path::PATH_SEPARATOR));
        assert!(!value_settled(Some(&moved), Some(&live), &ModifierKind::Path));
        assert!(!value_settled(Some(&folded), Some(&live), &ModifierKind::List));
        assert!(!value_settled(Some(&folded), None, &ModifierKind::Path));
        assert!(value_settled(None, None, &ModifierKind::Path));
    }

    #[test]
    fn a_quoted_segment_compares_equal_to_its_unquoted_spelling() {
        assert!(element_eq("\"/opt/bin\"", "/opt/bin", &ModifierKind::Path));
        assert!(element_eq("/opt/bin", "\"/opt/bin\"", &ModifierKind::Path));
        assert!(
            !element_eq("\"/opt/bin", "/opt/bin", &ModifierKind::Path),
            "only a surrounding pair is stripped"
        );
        assert_eq!(unquote("\""), "\"");
        assert!(!element_eq("\"-Dx\"", "-Dx", &ModifierKind::List));
    }

    /// Asserted against `cfg!(windows)` so both arms execute on every platform.
    #[test]
    fn the_ascii_fold_is_path_kind_only() {
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
}
