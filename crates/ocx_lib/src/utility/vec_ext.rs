// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;
use std::hash::Hash;

pub trait VecExt<T>: Clone {
    fn sorted(self) -> Self
    where
        T: Ord;

    fn unique(&mut self)
    where
        T: Eq + Hash + Clone;

    /// In-place dedup that keeps the **last** occurrence of each element while
    /// preserving relative order, the mirror of [`Self::unique`] (keep-first).
    /// `[a, b, a, c]` becomes `[b, a, c]`.
    fn unique_last(&mut self)
    where
        T: Eq + Hash + Clone;

    fn unique_clone(&self) -> Self
    where
        T: Eq + Hash + Clone,
    {
        let mut clone = self.clone();
        clone.unique();
        clone
    }
}

impl<T> VecExt<T> for Vec<T>
where
    T: Clone,
{
    fn sorted(mut self) -> Self
    where
        T: Ord,
    {
        self.sort();
        self
    }

    fn unique(&mut self)
    where
        T: Eq + Hash + Clone,
    {
        let mut seen = HashSet::new();
        self.retain(|item| seen.insert(item.clone()));
    }

    fn unique_last(&mut self)
    where
        T: Eq + Hash + Clone,
    {
        self.reverse();
        self.unique();
        self.reverse();
    }
}
