// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The scope a `${self.env.KEY}` token resolves against: the package's vars declared
//! strictly earlier, so a forward or self reference is absent and no cycle can form.

use std::collections::HashMap;
use std::sync::LazyLock;

use super::TemplateError;
use super::scanner;
use crate::metadata::env::entry::Entry;

/// One `${self.env.KEY}` scope element, seen as the key it declares.
pub trait DeclaredVar {
    /// The env-var key this declaration binds.
    fn declared_key(&self) -> &str;
}

impl DeclaredVar for Entry {
    fn declared_key(&self) -> &str {
        &self.key
    }
}

impl DeclaredVar for &str {
    fn declared_key(&self) -> &str {
        self
    }
}

/// How many declared keys [`TemplateError::UndefinedSelfEnvRef`] lists before eliding the
/// rest; past what a real package declares.
pub const MAX_LISTED_DECLARED_KEYS: usize = 16;

/// The vars declared strictly earlier than the one being resolved, in order; shared by the
/// resolver and the publish gate so both agree on which references are legal.
// Indexed, not walked: a search per token is O(V×T) on publisher input (~90k vars in 4 MiB).
pub struct SelfEnvScope<T> {
    /// Declaration order.
    declared: Vec<T>,
    /// key → (index of its first declaration, declaration count).
    by_key: HashMap<String, (usize, usize)>,
}

impl<T: DeclaredVar> SelfEnvScope<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            declared: Vec::new(),
            by_key: HashMap::new(),
        }
    }

    /// Extends the scope by one var.
    pub fn push(&mut self, declared: T) {
        let key = declared.declared_key().to_owned();
        let next = self.declared.len();
        self.by_key
            .entry(key)
            .and_modify(|(_, count)| *count += 1)
            .or_insert((next, 1));
        self.declared.push(declared);
    }

    /// The one declaration `key` names.
    ///
    /// # Errors
    ///
    /// [`TemplateError::AmbiguousSelfEnvRef`] when `key` is declared more than once;
    /// [`TemplateError::UndefinedSelfEnvRef`] when absent, naming the keys in scope.
    pub fn lookup(&self, key: &str) -> Result<&T, TemplateError> {
        let Some(&(first, count)) = self.by_key.get(key) else {
            return Err(TemplateError::UndefinedSelfEnvRef {
                key: key.to_owned(),
                declared_before: self.declared_keys_for_message(),
            });
        };
        if count > 1 {
            return Err(TemplateError::AmbiguousSelfEnvRef { key: key.to_owned() });
        }
        Ok(&self.declared[first])
    }

    /// The declared keys for the error message, each escaped and the list capped.
    // Keys arrive unvalidated from `metadata.json` (ESC or newline bytes, CWE-117) and unbounded.
    fn declared_keys_for_message(&self) -> Vec<String> {
        let mut listed: Vec<String> = self
            .declared
            .iter()
            .take(MAX_LISTED_DECLARED_KEYS)
            .map(|declared| scanner::for_message(declared.declared_key()))
            .collect();
        if self.declared.len() > MAX_LISTED_DECLARED_KEYS {
            listed.push(scanner::TRUNCATION_MARKER.to_owned());
        }
        listed
    }
}

impl<T: DeclaredVar> Default for SelfEnvScope<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: DeclaredVar> FromIterator<T> for SelfEnvScope<T> {
    fn from_iter<I: IntoIterator<Item = T>>(declared: I) -> Self {
        let mut scope = Self::new();
        for one in declared {
            scope.push(one);
        }
        scope
    }
}

/// The scope a resolver built without `TemplateResolver::with_self_env` sees.
pub static EMPTY_SELF_ENV: LazyLock<SelfEnvScope<Entry>> = LazyLock::new(SelfEnvScope::new);
