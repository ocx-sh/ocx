// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The scope a `${self.env.KEY}` token resolves against.
//!
//! One package's vars declared **strictly earlier** than the one being
//! resolved, in declaration order (D6.1). Acyclicity is a property of that
//! shape rather than a check: a forward reference and a self reference are both
//! simply absent from the prefix, and there is no back-edge to detect (D6.3).

use std::collections::HashMap;
use std::sync::LazyLock;

use super::TemplateError;
use super::scanner;
use crate::package::metadata::env::entry::Entry;

/// One element of a `${self.env.KEY}` scope, seen as the key it declares.
///
/// The two enforcement points hold different things — the resolver a resolved
/// [`Entry`], the publish gate the bare key — and this is the whole of what
/// [`SelfEnvScope`] needs from either.
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

/// How many declared keys [`TemplateError::UndefinedSelfEnvRef`] lists before
/// eliding the rest.
///
/// A publisher who declared more vars than this does not need every one of them
/// echoed to find their typo — the first few establish which package and which
/// prefix of its `env` array the reference was resolved against, which is the
/// whole diagnostic. Sized like the byte cap in `scanner.rs`: comfortably past
/// what a real package declares, so an ordinary refusal never elides.
pub const MAX_LISTED_DECLARED_KEYS: usize = 16;

/// The vars declared strictly earlier than the one being resolved, in
/// declaration order, indexed by key as they arrive.
///
/// Generic over the element because the two enforcement points hold different
/// things (see [`DeclaredVar`]). They must agree on which references are legal
/// — a document the gate accepts is one every consumer then has to compose — so
/// the rule lives here once instead of on both sides.
///
/// **Why an index and not a walk.** The scope grows by one per declared var and
/// is searched once per `${self.env.*}` token, so a linear search is O(V×T) on
/// input a publisher controls: `load_object_data` caps a metadata blob at 4 MiB,
/// which still admits ~90k minimal vars. The index is maintained by [`push`],
/// which the callers already call once per var, so both the lookup and the
/// declared-twice test are O(1) and neither caller pays for the scope twice.
///
/// [`push`]: SelfEnvScope::push
pub struct SelfEnvScope<T> {
    /// Declaration order — what [`declared_keys_for_message`] projects for
    /// [`TemplateError::UndefinedSelfEnvRef`], and what makes acyclicity
    /// structural (D6.3).
    ///
    /// [`declared_keys_for_message`]: SelfEnvScope::declared_keys_for_message
    declared: Vec<T>,
    /// key → (index of its first declaration, how many declarations carry it).
    /// The count is the whole of the ambiguity test; the index is the answer.
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

    /// Extends the scope by one var, which is what makes the next var's scope
    /// the prefix strictly before it.
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
    /// [`TemplateError::AmbiguousSelfEnvRef`] when `key` is declared more than
    /// once — both candidates are legally visible and neither is privileged, so
    /// the reference is refused rather than resolved to an arbitrary one (D7).
    /// [`TemplateError::UndefinedSelfEnvRef`] when `key` is absent, naming the
    /// keys that were in scope. Walking the scope to name them is O(V) on a path
    /// that ends in an error, and it is the only place that walk still happens.
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

    /// The declared keys as [`TemplateError::UndefinedSelfEnvRef`] should print
    /// them: each escaped, the list bounded.
    ///
    /// Both halves are publisher-controlled and neither is validated anywhere
    /// upstream. A declared key is `metadata.json`'s `env.variables[].key`, a
    /// plain `String` — `is_valid_env_key` guards the `ocx.toml` parse path, and
    /// `ValidMetadata::try_from` asserts only modifier types and list entries,
    /// so a published document can carry ESC or newline bytes in a key
    /// (CWE-117/150). The shell and CI emitters *skip* an invalid key rather
    /// than refuse the document, which is exactly why one survives to be echoed
    /// here. The count is publisher-controlled too: one entry per declared var,
    /// unbounded, is a message as long as the author cares to make it.
    ///
    /// The sibling `key` field needs neither: it comes from a scanned
    /// `TokenShape::SelfEnv`, so `is_body_segment` has already confined it to
    /// `[A-Za-z0-9_-]`.
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
///
/// `HashMap::new` is not `const`, so this is a lazy static rather than a
/// constant — and a static rather than a per-resolver allocation, because every
/// caller outside env-value resolution has no self-env scope at all.
pub static EMPTY_SELF_ENV: LazyLock<SelfEnvScope<Entry>> = LazyLock::new(SelfEnvScope::new);
