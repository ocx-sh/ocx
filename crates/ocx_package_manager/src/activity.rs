// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! What one command did to packages, collected for the caller that reports it.
//!
//! The manager appends one entry per package set up (never per waiter) and the caller drains the
//! log once when the command is over. A manager without a log appends nothing.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ocx_oci::{PinnedPackageRef, Platform};

/// What happened to a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Installed with a candidate link.
    Installed,
    /// Pulled into the store.
    Pulled,
    /// Removed.
    Removed,
}

/// A package's part in the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Named by the command.
    Root,
    /// Pulled in by a root.
    Dependency,
}

/// One package's part in a command.
#[derive(Debug, Clone)]
pub struct PackageActivity {
    /// The package at its digest.
    pub pinned: PinnedPackageRef,
    /// The platform it was set up for, when known.
    pub platform: Option<Platform>,
    /// What happened.
    pub action: Action,
    /// Root or dependency.
    pub role: Role,
    /// The package was already in the store.
    pub store_hit: bool,
    /// Descriptor sizes of the layers this package fetched as layer leader that were not in the layer
    /// store; `0` when it fetched none, and for a store hit.
    pub download_size: u64,
    /// Wall time of `PackageManager::resolve`; `None` when the setup ran no resolve.
    pub resolve: Option<Duration>,
    /// Wall time of the whole layer phase (fetch, unpack, and waiting on layers other packages lead),
    /// set only when this package fetched at least one layer itself.
    pub download: Option<Duration>,
    /// Wall time from layer assembly through the move into the store, excluding dependency setup; set
    /// when the package was not a store hit.
    pub extract: Option<Duration>,
}

#[derive(Default)]
struct State {
    entries: Vec<PackageActivity>,
    roots: HashSet<PinnedPackageRef>,
}

/// The entries of one command, shared by every clone of the manager that carries it.
///
/// The lock is held only to push or take, never across an `.await`.
#[derive(Clone, Default)]
pub struct ActivityLog {
    state: Arc<Mutex<State>>,
}

impl ActivityLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one entry.
    pub fn record(&self, entry: PackageActivity) {
        self.lock().entries.push(entry);
    }

    /// Records the packages an `_all` entry point was asked for, apart from the setup entries, so
    /// a root that lost the singleflight race to its own dependency path is still a root.
    pub fn record_roots(&self, roots: impl IntoIterator<Item = PinnedPackageRef>) {
        self.lock().roots.extend(roots);
    }

    /// Takes every entry and empties the log, one record per `registry/repository@digest`.
    ///
    /// The role is `Root` when the digest is in the union of the recorded root sets or any of its
    /// entries says so; `Installed` wins over `Pulled`; the performance fields come from the entry
    /// that fetched (the first with `store_hit` false, else the first recorded), so an entry that
    /// only marks an action must carry `store_hit: true`. A `Removed` entry is its own record: an
    /// install and a removal of one digest in one run stay two records. The result is ordered by
    /// identifier string, removals after the other record of the same digest, because arrival order
    /// is a race between concurrent setups.
    pub fn drain(&self) -> Vec<PackageActivity> {
        let State { entries, roots } = std::mem::take(&mut *self.lock());
        let roots: HashSet<PinnedPackageRef> = roots.iter().map(PinnedPackageRef::strip_advisory).collect();

        let mut groups: HashMap<(PinnedPackageRef, bool), Vec<PackageActivity>> = HashMap::new();
        for entry in entries {
            let key = (entry.pinned.strip_advisory(), entry.action == Action::Removed);
            groups.entry(key).or_default().push(entry);
        }

        let mut merged: Vec<((String, bool), PackageActivity)> = groups
            .into_iter()
            .filter_map(|((identity, removed), group)| {
                let named_as_root = roots.contains(&identity);
                Some(((identity.to_string(), removed), merge(&group, named_as_root)?))
            })
            .collect();
        merged.sort_by(|a, b| a.0.cmp(&b.0));
        merged.into_iter().map(|(_, record)| record).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One record from every entry for one package; `None` only for an empty group.
fn merge(group: &[PackageActivity], named_as_root: bool) -> Option<PackageActivity> {
    let mut record = group.iter().find(|e| !e.store_hit).or(group.first())?.clone();
    if group.iter().any(|e| e.action == Action::Installed) {
        record.action = Action::Installed;
    }
    if named_as_root || group.iter().any(|e| e.role == Role::Root) {
        record.role = Role::Root;
    }
    if record.platform.is_none() {
        record.platform = group.iter().find_map(|e| e.platform.clone());
    }
    Some(record)
}

#[cfg(test)]
impl ActivityLog {
    /// How many entries are recorded, before `drain` merges them per package.
    pub(crate) fn recorded(&self) -> usize {
        self.lock().entries.len()
    }
}

#[cfg(test)]
mod tests {
    use ocx_oci::{Digest, PackageRef};

    use super::*;

    fn pinned(repository: &str, tag: Option<&str>, fill: &str) -> PinnedPackageRef {
        let mut package = PackageRef::new_registry(repository, "example.com");
        if let Some(tag) = tag {
            package = package.clone_with_tag(tag);
        }
        PinnedPackageRef::pin(&package, Digest::Sha256(fill.repeat(64)))
    }

    fn entry(pinned: PinnedPackageRef, action: Action, role: Role, store_hit: bool) -> PackageActivity {
        PackageActivity {
            pinned,
            platform: None,
            action,
            role,
            store_hit,
            download_size: 0,
            resolve: None,
            download: None,
            extract: None,
        }
    }

    /// A root that lost the singleflight race to its own dependency path is recorded once, as the
    /// dependency-role setup entry, yet stays a root.
    #[test]
    fn a_root_that_waited_is_still_root() {
        let log = ActivityLog::new();
        let tool = pinned("acme/tool", Some("1.0"), "a");
        log.record_roots([tool.clone()]);
        log.record(entry(tool.strip_advisory(), Action::Pulled, Role::Dependency, false));

        let drained = log.drain();

        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].role, Role::Root);
    }

    /// The store hit says nothing about the cost; the entry that fetched does.
    #[test]
    fn the_fetching_entry_keeps_its_performance_fields() {
        let log = ActivityLog::new();
        let tool = pinned("acme/tool", None, "a");
        let mut fetched = entry(tool.clone(), Action::Pulled, Role::Dependency, false);
        fetched.download_size = 4096;
        fetched.resolve = Some(Duration::from_millis(5));
        fetched.download = Some(Duration::from_millis(40));
        fetched.extract = Some(Duration::from_millis(7));
        log.record(entry(tool.clone(), Action::Pulled, Role::Dependency, true));
        log.record(fetched);

        let drained = log.drain();

        assert_eq!(drained.len(), 1);
        assert!(!drained[0].store_hit);
        assert_eq!(drained[0].download_size, 4096);
        assert_eq!(drained[0].download, Some(Duration::from_millis(40)));
        assert_eq!(drained[0].extract, Some(Duration::from_millis(7)));
    }

    #[test]
    fn installed_wins_over_pulled_and_keeps_the_fetching_fields() {
        let log = ActivityLog::new();
        let tool = pinned("acme/tool", None, "a");
        let mut fetched = entry(tool.clone(), Action::Pulled, Role::Root, false);
        fetched.download_size = 10;
        log.record(fetched);
        log.record(entry(tool, Action::Installed, Role::Root, true));

        let drained = log.drain();

        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].action, Action::Installed);
        assert_eq!(drained[0].download_size, 10);
        assert!(!drained[0].store_hit);
    }

    /// An install and a removal of one digest are two records; removals follow the other record.
    #[test]
    fn a_removal_is_its_own_record() {
        let log = ActivityLog::new();
        let tool = pinned("acme/tool", None, "a");
        log.record(entry(tool.clone(), Action::Removed, Role::Root, false));
        log.record(entry(tool, Action::Installed, Role::Root, true));

        let actions: Vec<Action> = log.drain().iter().map(|e| e.action).collect();

        assert_eq!(actions, [Action::Installed, Action::Removed]);
    }

    /// Arrival order is a race, so the drain orders by identifier; a second drain finds nothing.
    #[test]
    fn drain_orders_by_identifier_and_empties_the_log() {
        let log = ActivityLog::new();
        log.record(entry(pinned("zeta/tool", None, "b"), Action::Pulled, Role::Root, false));
        log.record(entry(
            pinned("alpha/tool", None, "c"),
            Action::Pulled,
            Role::Root,
            false,
        ));

        let repositories: Vec<String> = log.drain().iter().map(|e| e.pinned.repository().to_string()).collect();

        assert_eq!(repositories, ["alpha/tool", "zeta/tool"]);
        assert!(log.drain().is_empty());
    }

    /// Two tags of one digest are one package.
    #[test]
    fn entries_for_one_digest_under_two_tags_merge() {
        let log = ActivityLog::new();
        log.record(entry(
            pinned("acme/tool", Some("1"), "a"),
            Action::Pulled,
            Role::Root,
            false,
        ));
        log.record(entry(
            pinned("acme/tool", Some("1.0"), "a"),
            Action::Pulled,
            Role::Dependency,
            true,
        ));

        assert_eq!(log.drain().len(), 1);
    }
}
