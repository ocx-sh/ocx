// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Applying resolved [`Entry`] vectors to a process [`Env`], and the
//! `OCX_ENV` forwarding envelope that carries the caller-contributed tail
//! across a launcher hop.

use std::collections::HashMap;
use std::ffi::OsStr;

use super::entry::Entry;
use super::list;
use super::modifier::ModifierKind;
use crate::launch::LaunchIdentities;
use ocx_config::env::{Env, EnvKey, OcxConfigView, keys};
use ocx_util::env::{is_reserved_ocx_key, is_valid_env_key, var};

/// The two entry slices [`EnvEntriesExt::apply_child_env`] needs, named so they cannot transpose.
pub struct ChildEnv<'a> {
    /// The full composed environment the child process runs with.
    pub composed: &'a [Entry],
    /// Only the caller-contributed tail (project/group `[env]`, `--env`); a
    /// launcher re-entry re-derives the package part, so forwarding it would apply it twice.
    pub forwarded: &'a [Entry],
    /// The names this composition resolved each package under, for a launcher's patch
    /// matching; `None` keeps whatever the process inherited.
    pub identities: Option<&'a LaunchIdentities>,
}

/// Folding resolved [`Entry`] vectors onto an [`Env`].
pub trait EnvEntriesExt {
    /// Applies resolved environment entries to this environment.
    fn apply_entries(&mut self, entries: &[Entry]);

    /// Builds the environment for a child process from the composed entries,
    /// the running ocx's resolution-affecting config and the forwarded payload.
    fn apply_child_env(&mut self, env: ChildEnv<'_>, config: &OcxConfigView);
}

impl EnvEntriesExt for Env {
    /// Applies resolved environment entries, noting each composed `PATH`
    /// directory via [`Env::note_package_path`].
    ///
    /// Reserved `OCX_*` keys are filtered upstream at `resolve_env*`, not here,
    /// because `emit_lines` feeds an eval'd shell stream from the same vector.
    fn apply_entries(&mut self, entries: &[Entry]) {
        let path_key = EnvKey::new("PATH");
        for entry in entries {
            match entry.kind {
                ModifierKind::Path => {
                    // Before the note, which derives from the value already on `PATH`.
                    self.add_path(&entry.key, &entry.value);
                    if EnvKey::new(entry.key.as_str()) == path_key {
                        self.note_package_path(OsStr::new(entry.value.as_str()));
                    }
                }
                ModifierKind::Constant => self.set(&entry.key, &entry.value),
                ModifierKind::List => {
                    let separator = entry.separator.as_deref().unwrap_or(list::DEFAULT_SEPARATOR);
                    self.add_list(&entry.key, &entry.value, separator);
                }
            }
        }
    }

    /// Builds a child process's environment: composed entries, the running
    /// ocx's config, then the forwarded payload.
    ///
    /// Every compose-then-spawn command must go through here, or a launcher
    /// re-entry silently reverts the caller's overrides.
    fn apply_child_env(&mut self, env: ChildEnv<'_>, config: &OcxConfigView) {
        self.apply_entries(env.composed);
        // Set-or-removes every `OCX_*` key it knows, so no ambient export or entry beats this ocx's parsed config.
        self.apply_ocx_config(config);
        // After `apply_ocx_config`, which strips `OCX_ENV`, or the payload is deleted.
        set_forwarded_env(self, env.forwarded);
        // Only under a `[patches]` tier, so a host without one sees no new variable.
        if config.patches.is_some()
            && let Some(identities) = env.identities
        {
            match identities.encode() {
                Some(json) => self.set(keys::OCX_LAUNCH_IDENTITIES, json),
                None => self.remove(keys::OCX_LAUNCH_IDENTITIES),
            }
        }
    }
}

/// Writes the forwarded payload as [`keys::OCX_ENV`] for a launcher re-entry;
/// an empty slice leaves the key absent.
fn set_forwarded_env(env: &mut Env, entries: &[Entry]) {
    match encode_forwarded_env(entries) {
        Some(json) => env.set(keys::OCX_ENV, json),
        None => env.remove(keys::OCX_ENV),
    }
}

/// A `list` key's separator agreement could not be settled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ListSeparatorError {
    /// Two entries for one key declare different separators.
    #[error(
        "env var '{key}' is contributed as a list with conflicting separators {first:?} and {second:?}; every contributor to one key must agree"
    )]
    Conflict {
        key: String,
        /// The separator established first, in composition order.
        first: String,
        second: String,
    },

    /// A value is edged by the separator settled on it.
    #[error("env var '{key}' has a list value starting or ending with its separator {separator:?}: {value:?}")]
    EdgedValue {
        key: String,
        separator: String,
        value: String,
    },
}

/// Settles the separator each `list` key folds with: the first explicit one is
/// inherited by later `None` entries, and a key nobody set keeps `None` (the default).
///
/// Call once, after the last contributor.
///
/// # Errors
///
/// [`ListSeparatorError::Conflict`] when one key has two explicit separators;
/// [`ListSeparatorError::EdgedValue`] when the settled separator edges a value.
pub fn reconcile_list_separators<'a>(
    entries: impl IntoIterator<Item = &'a mut Entry>,
) -> Result<(), ListSeparatorError> {
    let mut entries: Vec<&mut Entry> = entries.into_iter().collect();

    let mut established: HashMap<EnvKey, String> = HashMap::new();
    for entry in entries.iter() {
        if entry.kind != ModifierKind::List {
            continue;
        }
        let Some(separator) = entry.separator.as_deref() else {
            continue;
        };
        match established.get(&EnvKey::new(entry.key.as_str())) {
            Some(first) if first != separator => {
                return Err(ListSeparatorError::Conflict {
                    key: entry.key.clone(),
                    first: first.clone(),
                    second: separator.to_string(),
                });
            }
            Some(_) => {}
            None => {
                established.insert(EnvKey::new(entry.key.as_str()), separator.to_string());
            }
        }
    }

    for entry in entries.iter_mut() {
        if entry.kind != ModifierKind::List || entry.separator.is_some() {
            continue;
        }
        if let Some(separator) = established.get(&EnvKey::new(entry.key.as_str())) {
            entry.separator = Some(separator.clone());
        }
    }

    // Edge-checked here: an inherited or defaulted separator first exists at this point.
    for entry in entries.iter() {
        if entry.kind != ModifierKind::List {
            continue;
        }
        let separator = entry.separator.as_deref().unwrap_or(list::DEFAULT_SEPARATOR);
        if list::is_separator_edged(&entry.value, separator) {
            return Err(ListSeparatorError::EdgedValue {
                key: entry.key.clone(),
                separator: separator.to_string(),
                value: entry.value.clone(),
            });
        }
    }

    Ok(())
}

/// Failure modes of decoding the forwarded [`keys::OCX_ENV`] payload.
///
/// Each rejects the whole envelope: a partially applied payload matches neither
/// what the parent composed nor what the user declared.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ForwardedEnvError {
    /// The value was present but not valid JSON.
    #[error("malformed OCX_ENV env value")]
    MalformedJson {
        #[source]
        source: serde_json::Error,
    },

    /// The mandatory `entries` sentinel is absent or not an array.
    #[error("OCX_ENV 'entries' field is absent or not an array")]
    MissingEntries,

    /// An array element is not an object carrying string `key`, `value` and
    /// `type` fields.
    #[error("OCX_ENV entry at index {index} is not an object with string 'key', 'value' and 'type'")]
    InvalidEntry {
        /// Zero-based position in the `entries` array.
        index: usize,
    },

    /// An entry's `type` is not a recognized modifier; never lenient, since a
    /// misread modifier sets a value with the wrong combination semantics.
    #[error("OCX_ENV entry '{key}' has unrecognized modifier type '{found}'")]
    UnknownKind {
        /// The entry's env-var name.
        key: String,
        /// The unrecognized `type` value as received.
        found: String,
    },

    /// An entry's key fails [`is_valid_env_key`].
    #[error("OCX_ENV entry '{key}' is not a valid environment variable name")]
    InvalidKey {
        /// The offending env-var name.
        key: String,
    },

    /// An entry's key is reserved ([`is_reserved_ocx_key`]).
    #[error("OCX_ENV entry '{key}' is reserved; OCX_* and __OCX_* keys cannot be forwarded")]
    ReservedKey {
        /// The offending env-var name.
        key: String,
    },

    /// A `list` entry has no separator; never defaulted, since the parent always
    /// writes one and a guessed space would re-join a comma list.
    #[error("OCX_ENV entry '{key}' is a list without a separator")]
    MissingSeparator {
        /// The entry's env-var name.
        key: String,
    },

    /// A `list` entry's separator fails [`list::separator_is_valid`].
    #[error("OCX_ENV entry '{key}' has unusable list separator {separator:?}")]
    InvalidSeparator {
        /// The entry's env-var name.
        key: String,
        /// The unusable separator as received.
        separator: String,
    },

    /// A `list` entry's value starts or ends with its own separator.
    #[error("OCX_ENV entry '{key}' has a list value starting or ending with its separator {separator:?}: {value:?}")]
    SeparatorEdgedValue {
        key: String,
        separator: String,
        value: String,
    },
}

/// Serializes entries into the [`keys::OCX_ENV`] envelope; `None` for an empty slice.
fn encode_forwarded_env(entries: &[Entry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let array: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let mut object = serde_json::json!({
                "key":   entry.key,
                "value": entry.value,
                "type":  entry.kind.to_string(),
            });
            // Defaulted here, never on the far side: the decoder refuses a missing separator.
            if entry.kind == ModifierKind::List
                && let Some(fields) = object.as_object_mut()
            {
                let separator = entry.separator.as_deref().unwrap_or(list::DEFAULT_SEPARATOR);
                fields.insert(
                    "separator".to_string(),
                    serde_json::Value::String(separator.to_string()),
                );
            }
            object
        })
        .collect();
    match serde_json::to_string(&serde_json::json!({ "entries": array })) {
        Ok(json) => Some(json),
        Err(error) => {
            log::warn!("failed to encode OCX_ENV: {error}");
            None
        }
    }
}

/// Parses [`keys::OCX_ENV`] back into the forwarded entries; absent or empty yields none.
///
/// The payload is untrusted: the reserved-key gate keeps a forged `OCX_*` key
/// out of a grandchild ocx, since [`Env::apply_ocx_config`] overwrites only keys it knows.
///
/// # Errors
///
/// [`ForwardedEnvError`] for any malformed entry.
pub fn forwarded_env() -> Result<Vec<Entry>, ForwardedEnvError> {
    let Some(raw) = var(keys::OCX_ENV) else {
        return Ok(Vec::new());
    };
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let envelope = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw)
        .map_err(|source| ForwardedEnvError::MalformedJson { source })?;

    let array = envelope
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or(ForwardedEnvError::MissingEntries)?;

    let mut out = Vec::with_capacity(array.len());
    for (index, element) in array.iter().enumerate() {
        let Some(object) = element.as_object() else {
            return Err(ForwardedEnvError::InvalidEntry { index });
        };
        let (Some(key), Some(value), Some(kind)) = (
            object.get("key").and_then(serde_json::Value::as_str),
            object.get("value").and_then(serde_json::Value::as_str),
            object.get("type").and_then(serde_json::Value::as_str),
        ) else {
            return Err(ForwardedEnvError::InvalidEntry { index });
        };
        if !is_valid_env_key(key) {
            return Err(ForwardedEnvError::InvalidKey { key: key.to_string() });
        }
        if is_reserved_ocx_key(key) {
            return Err(ForwardedEnvError::ReservedKey { key: key.to_string() });
        }
        let kind = kind
            .parse::<ModifierKind>()
            .map_err(|error| ForwardedEnvError::UnknownKind {
                key: key.to_string(),
                found: error.found,
            })?;

        let separator = if kind == ModifierKind::List {
            let Some(separator) = object.get("separator").and_then(serde_json::Value::as_str) else {
                return Err(ForwardedEnvError::MissingSeparator { key: key.to_string() });
            };
            if !list::separator_is_valid(separator) {
                return Err(ForwardedEnvError::InvalidSeparator {
                    key: key.to_string(),
                    separator: separator.to_string(),
                });
            }
            if list::is_separator_edged(value, separator) {
                return Err(ForwardedEnvError::SeparatorEdgedValue {
                    key: key.to_string(),
                    separator: separator.to_string(),
                    value: value.to_string(),
                });
            }
            Some(separator.to_string())
        } else {
            None
        };

        out.push(Entry {
            key: key.to_string(),
            value: value.to_string(),
            kind,
            separator,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    // Both are reached only from `#[cfg(unix)]` helpers and tests; the one
    // `#[cfg(windows)]` test below uses neither.
    #[cfg(unix)]
    use std::path::PathBuf;

    use super::*;
    #[cfg(unix)]
    use ocx_config::env::CommandResolutionError;

    /// Writes `name` into `dir` with the given mode, returning its path.
    #[cfg(unix)]
    fn write_binary(dir: &std::path::Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\ntrue\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    /// An executable POSIX body carrying `TRAMPOLINE_MARKER` on its second
    /// line, exactly where C-028's generated trampoline puts it.
    #[cfg(unix)]
    fn write_trampoline(dir: &std::path::Path, name: &str) -> PathBuf {
        use ocx_config::env::TRAMPOLINE_MARKER;
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n{TRAMPOLINE_MARKER}\nunset OCX_GLOBAL OCX_PROJECT\n\
                 __ocx_binary='/home/ocx/bin/ocx'\n\
                 exec \"${{OCX_BINARY_PIN:-$__ocx_binary}}\" --project '/p' exec -- \"${{0##*/}}\" \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// macOS puts `TempDir` under the `/tmp` -> `/private/tmp` symlink, so a
    /// raw tempdir path never equals a resolved one. Compare canonical forms.
    ///
    /// Gated like every one of its callers: they are all `#[cfg(unix)]`, so on
    /// Windows this is uncallable and `dead_code` refuses the build.
    #[cfg(unix)]
    fn same_file(left: &std::path::Path, right: &std::path::Path) -> bool {
        match (dunce::canonicalize(left), dunce::canonicalize(right)) {
            (Ok(l), Ok(r)) => l == r,
            _ => left == right,
        }
    }

    // ── OCX_ENV forwarding (R1 / R1a) ────────────────────────────────────

    fn entry(
        key: &str,
        value: &str,
        kind: crate::metadata::env::modifier::ModifierKind,
    ) -> crate::metadata::env::entry::Entry {
        crate::metadata::env::entry::Entry {
            key: key.to_string(),
            value: value.to_string(),
            kind,
            separator: None,
        }
    }

    /// A list entry carrying its separator, for the forwarding round-trip.
    fn list_entry(key: &str, value: &str, separator: &str) -> crate::metadata::env::entry::Entry {
        crate::metadata::env::entry::Entry {
            key: key.to_string(),
            value: value.to_string(),
            kind: crate::metadata::env::modifier::ModifierKind::List,
            separator: Some(separator.to_string()),
        }
    }

    /// The forwarded payload survives encode → env → decode with both modifier
    /// kinds and vector order intact. Order IS precedence, so it is part of the
    /// contract, not an implementation detail.
    #[test]
    fn forwarded_env_round_trips_both_kinds_in_order() {
        use crate::metadata::env::modifier::ModifierKind;

        let guard = ocx_util::env::overrides::lock();
        let original = vec![
            entry("CI", "1", ModifierKind::Constant),
            entry("PATH", "/project/node_modules/.bin", ModifierKind::Path),
            entry("RUSTFLAGS", "-C target-cpu=native", ModifierKind::Constant),
        ];

        let mut child = Env::clean();
        set_forwarded_env(&mut child, &original);
        let encoded = child
            .get(keys::OCX_ENV)
            .expect("a non-empty payload must set OCX_ENV")
            .to_str()
            .expect("OCX_ENV must be valid UTF-8")
            .to_string();
        guard.set(keys::OCX_ENV, encoded);

        let parsed = forwarded_env().expect("a self-encoded payload must decode");
        assert_eq!(parsed.len(), original.len());
        for (got, want) in parsed.iter().zip(&original) {
            assert_eq!(got.key, want.key);
            assert_eq!(got.value, want.value);
            assert_eq!(got.kind, want.kind, "modifier kind must survive the round-trip");
        }
    }

    // ── W-7: `list` entries survive the launcher hop ─────────────────────

    /// A comma list must reach the far side of the launcher hop with its
    /// separator intact. Losing it would silently re-join the value with
    /// spaces, and `GODEBUG` ignores settings it cannot parse.
    #[test]
    fn forwarded_env_round_trips_a_list_entry_with_its_separator() {
        let guard = ocx_util::env::overrides::lock();
        let original = vec![
            list_entry("GODEBUG", "gctrace=1", ","),
            list_entry("JDK_JAVA_OPTIONS", "-ea", " "),
        ];

        let mut child = Env::clean();
        set_forwarded_env(&mut child, &original);
        let encoded = child
            .get(keys::OCX_ENV)
            .expect("a non-empty payload must set OCX_ENV")
            .to_str()
            .expect("OCX_ENV must be valid UTF-8")
            .to_string();
        guard.set(keys::OCX_ENV, encoded);

        let parsed = forwarded_env().expect("a self-encoded list payload must decode");
        assert_eq!(parsed.len(), original.len());
        for (got, want) in parsed.iter().zip(&original) {
            assert_eq!(got.key, want.key);
            assert_eq!(got.value, want.value);
            assert_eq!(got.kind, want.kind);
            assert_eq!(got.separator, want.separator, "the separator must survive the hop");
        }
    }

    /// A list entry nobody established a separator for is forwarded with the
    /// *effective* one, not with the field omitted. The human surfaces let an
    /// author omit it, the decoder refuses a list without one, and the parent
    /// is the last process that knows what "omitted" resolved to — without
    /// this, `GODEBUG = { type = "list", value = "x" }` in `ocx.toml` would
    /// exit 65 the moment `ocx exec` went through an entrypoint launcher.
    #[test]
    fn forwarded_env_fills_in_the_effective_separator_for_a_list_without_one() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let guard = ocx_util::env::overrides::lock();
        let mut child = Env::clean();
        set_forwarded_env(
            &mut child,
            &[Entry {
                key: "JDK_JAVA_OPTIONS".to_string(),
                value: "-ea".to_string(),
                kind: ModifierKind::List,
                separator: None,
            }],
        );
        let encoded = child
            .get(keys::OCX_ENV)
            .expect("payload present")
            .to_str()
            .expect("UTF-8")
            .to_string();
        guard.set(keys::OCX_ENV, encoded);

        let parsed = forwarded_env().expect("a separator-less list entry must still decode");
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].separator.as_deref(),
            Some(crate::metadata::env::list::DEFAULT_SEPARATOR)
        );
    }

    /// Only list entries carry the field — a constant has no separator to
    /// resolve, and writing one would invite a decoder to act on it.
    #[test]
    fn forwarded_env_omits_the_separator_for_non_list_entries() {
        use crate::metadata::env::modifier::ModifierKind;

        let mut child = Env::clean();
        set_forwarded_env(&mut child, &[entry("CI", "1", ModifierKind::Constant)]);
        let encoded = child
            .get(keys::OCX_ENV)
            .expect("payload present")
            .to_str()
            .expect("UTF-8")
            .to_string();
        assert!(
            !encoded.contains("separator"),
            "a constant entry must not carry a separator field; got: {encoded}"
        );
    }

    /// A `list` entry without its separator fails the whole payload closed.
    /// Defaulting here would re-join a comma list with spaces on the child
    /// side, which is the silent wrong environment the envelope exists to
    /// prevent.
    #[test]
    fn forwarded_env_rejects_a_list_entry_without_a_separator() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(
            keys::OCX_ENV,
            r#"{"entries":[{"key":"GODEBUG","value":"gctrace=1","type":"list"}]}"#,
        );
        let error = forwarded_env().expect_err("a list without a separator must not decode");
        assert!(
            matches!(&error, ForwardedEnvError::MissingSeparator { key } if key == "GODEBUG"),
            "expected MissingSeparator naming the key; got: {error}"
        );
    }

    /// An unusable separator is refused rather than folded with: an empty one
    /// degrades the flank match to a bare substring scan.
    #[test]
    fn forwarded_env_rejects_an_unusable_list_separator() {
        let guard = ocx_util::env::overrides::lock();
        for separator in ["", "="] {
            guard.set(
                keys::OCX_ENV,
                format!(r#"{{"entries":[{{"key":"OPTS","value":"-ea","type":"list","separator":"{separator}"}}]}}"#),
            );
            let error = forwarded_env().expect_err("an unusable separator must not decode");
            assert!(
                matches!(&error, ForwardedEnvError::InvalidSeparator { key, .. } if key == "OPTS"),
                "expected InvalidSeparator naming the key; got: {error}"
            );
        }
    }

    /// The decoded values are already resolved, so the separator-edge check
    /// `EnvResolver` runs post-resolution applies here verbatim.
    #[test]
    fn forwarded_env_rejects_a_separator_edged_list_value() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(
            keys::OCX_ENV,
            r#"{"entries":[{"key":"GODEBUG","value":",gctrace=1","type":"list","separator":","}]}"#,
        );
        let error = forwarded_env().expect_err("an edged value must not decode");
        assert!(
            matches!(&error, ForwardedEnvError::SeparatorEdgedValue { key, .. } if key == "GODEBUG"),
            "expected SeparatorEdgedValue naming the key; got: {error}"
        );
    }

    /// An empty payload removes the key rather than writing an empty envelope,
    /// and an absent key decodes to an empty vector — a launcher invoked
    /// directly (no `ocx exec` parent) is the normal no-payload case.
    #[test]
    fn forwarded_env_empty_payload_removes_key_and_decodes_empty() {
        let guard = ocx_util::env::overrides::lock();

        let mut child = Env::clean();
        child.set(
            keys::OCX_ENV,
            r#"{"entries":[{"key":"STALE","value":"1","type":"constant"}]}"#,
        );
        set_forwarded_env(&mut child, &[]);
        assert!(
            child.get(keys::OCX_ENV).is_none(),
            "an empty payload must remove OCX_ENV, not write an empty envelope"
        );

        guard.remove(keys::OCX_ENV);
        assert!(forwarded_env().expect("absent OCX_ENV is not an error").is_empty());
        guard.set(keys::OCX_ENV, "");
        assert!(forwarded_env().expect("empty OCX_ENV is not an error").is_empty());
    }

    /// Malformed JSON is a hard error, never a silent empty decode.
    #[test]
    fn forwarded_env_rejects_malformed_json() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(keys::OCX_ENV, "not json {{{");
        assert!(matches!(forwarded_env(), Err(ForwardedEnvError::MalformedJson { .. })));
    }

    /// The `entries` sentinel is mandatory: valid JSON without it was not
    /// produced by our encoder, so it is corrupted or injected.
    #[test]
    fn forwarded_env_rejects_missing_entries_sentinel() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(keys::OCX_ENV, r#"{"env":[{"key":"CI","value":"1","type":"constant"}]}"#);
        assert!(matches!(forwarded_env(), Err(ForwardedEnvError::MissingEntries)));
        // Present but not an array is the same fault.
        guard.set(keys::OCX_ENV, r#"{"entries":"CI=1"}"#);
        assert!(matches!(forwarded_env(), Err(ForwardedEnvError::MissingEntries)));
    }

    /// An unrecognized modifier `kind` is a hard error — the one place
    /// `OCX_PATCHES`' defaulting leniency must NOT be copied. A misread kind
    /// would replace where it should prepend and produce a wrong environment
    /// with no signal.
    #[test]
    fn forwarded_env_rejects_unknown_modifier_kind() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(
            keys::OCX_ENV,
            r#"{"entries":[{"key":"CI","value":"1","type":"append"}]}"#,
        );
        let error = forwarded_env().expect_err("an unknown modifier type must not decode");
        assert!(
            matches!(&error, ForwardedEnvError::UnknownKind { key, found } if key == "CI" && found == "append"),
            "expected UnknownKind naming the key and the value; got: {error}"
        );
        // An absent `type` is equally refused — never defaulted to constant.
        guard.set(keys::OCX_ENV, r#"{"entries":[{"key":"CI","value":"1"}]}"#);
        assert!(matches!(
            forwarded_env(),
            Err(ForwardedEnvError::InvalidEntry { index: 0 })
        ));
        // `kind` is NOT an accepted alias: it is the `EntrySource` discriminant
        // on the JSON env surface, so accepting both spellings here would be
        // two vocabularies for one concept.
        guard.set(
            keys::OCX_ENV,
            r#"{"entries":[{"key":"CI","value":"1","kind":"constant"}]}"#,
        );
        assert!(matches!(
            forwarded_env(),
            Err(ForwardedEnvError::InvalidEntry { index: 0 })
        ));
    }

    /// A reserved key fails the WHOLE payload closed. Filtering the bad entry
    /// and keeping the rest would let an attacker shape the surviving set.
    #[test]
    fn forwarded_env_reserved_key_fails_the_whole_payload_closed() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(
            keys::OCX_ENV,
            r#"{"entries":[{"key":"CI","value":"1","type":"constant"},
                          {"key":"OCX_DEFAULT_REGISTRY","value":"evil.example.com","type":"constant"}]}"#,
        );
        let error = forwarded_env().expect_err("a reserved key must reject the payload");
        assert!(
            matches!(&error, ForwardedEnvError::ReservedKey { key } if key == "OCX_DEFAULT_REGISTRY"),
            "expected ReservedKey naming the offender; got: {error}"
        );

        // `__OCX_*` is gated identically.
        guard.set(
            keys::OCX_ENV,
            r#"{"entries":[{"key":"__OCX_TESTING_INSTALL_BINARY","value":"/tmp/x","type":"constant"}]}"#,
        );
        assert!(matches!(forwarded_env(), Err(ForwardedEnvError::ReservedKey { .. })));
    }

    /// A key outside the POSIX name grammar is refused by the same shared
    /// validator the shell emitters and CI flavors use.
    #[test]
    fn forwarded_env_rejects_invalid_key_grammar() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(
            keys::OCX_ENV,
            r#"{"entries":[{"key":"A\nB","value":"1","type":"constant"}]}"#,
        );
        assert!(matches!(forwarded_env(), Err(ForwardedEnvError::InvalidKey { .. })));
    }

    fn patched_view() -> OcxConfigView {
        let mut view = OcxConfigView::new("/abs/ocx");
        view.patches = Some(ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: "corp.example.com/patches".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        });
        view
    }

    const IDENTITIES: &str =
        r#"{"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa":["ocx.sh/plantuml:1"]}"#;

    fn identities() -> crate::launch::LaunchIdentities {
        let guard = ocx_util::env::overrides::lock();
        guard.set(keys::OCX_LAUNCH_IDENTITIES, IDENTITIES);
        crate::launch::LaunchIdentities::from_env().unwrap().unwrap()
    }

    fn child_env_with(identities: Option<&crate::launch::LaunchIdentities>, view: &OcxConfigView) -> Env {
        let mut child = Env::clean();
        child.set(keys::OCX_LAUNCH_IDENTITIES, "inherited");
        child.apply_child_env(
            ChildEnv {
                composed: &[],
                forwarded: &[],
                identities,
            },
            view,
        );
        child
    }

    /// Under a `[patches]` tier the composing parent's map replaces an inherited one.
    #[test]
    fn apply_child_env_writes_launch_identities_under_a_patch_tier() {
        let identities = identities();
        let child = child_env_with(Some(&identities), &patched_view());
        assert_eq!(
            child.get(keys::OCX_LAUNCH_IDENTITIES).and_then(|value| value.to_str()),
            Some(IDENTITIES)
        );
    }

    /// An empty map clears the inherited one: the parent is authoritative for its child tree.
    #[test]
    fn apply_child_env_clears_inherited_identities_for_an_empty_map() {
        let child = child_env_with(Some(&crate::launch::LaunchIdentities::default()), &patched_view());
        assert!(child.get(keys::OCX_LAUNCH_IDENTITIES).is_none());
    }

    /// No `[patches]` tier means no change, and `None` keeps a nested launch's inherited map.
    #[test]
    fn apply_child_env_leaves_launch_identities_alone_without_a_tier_or_a_map() {
        let identities = identities();
        for child in [
            child_env_with(Some(&identities), &OcxConfigView::new("/abs/ocx")),
            child_env_with(None, &patched_view()),
        ] {
            assert_eq!(
                child.get(keys::OCX_LAUNCH_IDENTITIES).and_then(|value| value.to_str()),
                Some("inherited")
            );
        }
    }

    #[test]
    fn apply_entries_constant() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut env = Env::clean();
        let entries = vec![Entry {
            key: "JAVA_HOME".to_string(),
            value: "/opt/java".to_string(),
            kind: ModifierKind::Constant,
            separator: None,
        }];
        env.apply_entries(&entries);
        assert_eq!(env.get("JAVA_HOME").unwrap(), "/opt/java");
    }

    #[test]
    fn apply_entries_path_prepends() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut env = Env::clean();
        env.set("PATH", "/usr/bin");
        let entries = vec![Entry {
            key: "PATH".to_string(),
            value: "/opt/bin".to_string(),
            kind: ModifierKind::Path,
            separator: None,
        }];
        env.apply_entries(&entries);
        let path = env.get("PATH").unwrap().to_str().unwrap();
        assert!(path.starts_with("/opt/bin"));
        assert!(path.ends_with("/usr/bin"));
    }

    #[test]
    fn apply_entries_mixed() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut env = Env::clean();
        let entries = vec![
            Entry {
                key: "HOME".to_string(),
                value: "/home/user".to_string(),
                kind: ModifierKind::Constant,
                separator: None,
            },
            Entry {
                key: "PATH".to_string(),
                value: "/opt/bin".to_string(),
                kind: ModifierKind::Path,
                separator: None,
            },
        ];
        env.apply_entries(&entries);
        assert_eq!(env.get("HOME").unwrap(), "/home/user");
        assert_eq!(env.get("PATH").unwrap(), "/opt/bin");
    }

    // ── W-3: `apply_entries` on list entries ──────────────────────────────

    #[test]
    fn apply_entries_list_folds_with_the_entry_separator() {
        let mut env = Env::clean();
        env.set("GODEBUG", "madvdontneed=1");
        env.apply_entries(&[list_entry("GODEBUG", "gctrace=1", ",")]);
        assert_eq!(env.get("GODEBUG").unwrap(), "madvdontneed=1,gctrace=1");
    }

    /// A separator that survived reconciliation as `None` means nobody
    /// established one, which reads as the human-surface default.
    #[test]
    fn apply_entries_list_without_a_separator_folds_with_a_space() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut env = Env::clean();
        env.set("JDK_JAVA_OPTIONS", "-Xmx2g");
        env.apply_entries(&[Entry {
            key: "JDK_JAVA_OPTIONS".to_string(),
            value: "-ea".to_string(),
            kind: ModifierKind::List,
            separator: None,
        }]);
        assert_eq!(env.get("JDK_JAVA_OPTIONS").unwrap(), "-Xmx2g -ea");
    }

    /// Mixed kinds on one key resolve in vector order, which is composition
    /// order: a constant lands, then a later list appends to what it set. The
    /// rendered value is the sandwich, not a merge.
    #[test]
    fn apply_entries_mixed_kinds_on_one_key_follow_vector_order() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut env = Env::clean();
        env.apply_entries(&[
            list_entry("OPTS", "-first", " "),
            Entry {
                key: "OPTS".to_string(),
                value: "-replaced".to_string(),
                kind: ModifierKind::Constant,
                separator: None,
            },
            list_entry("OPTS", "-last", " "),
        ]);
        assert_eq!(
            env.get("OPTS").unwrap(),
            "-replaced -last",
            "the constant clears what came before it; the later list appends to it"
        );
    }

    // ── W-11: per-key separator agreement ─────────────────────────────────

    /// The first explicit separator establishes the key's; a later entry that
    /// omits one inherits it. Without this a package's comma list plus a
    /// project entry with no separator would render space-joined and the
    /// consumer would silently ignore the contribution.
    #[test]
    fn reconcile_lets_a_later_entry_inherit_the_established_separator() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut entries = vec![
            list_entry("GODEBUG", "gctrace=1", ","),
            Entry {
                key: "GODEBUG".to_string(),
                value: "madvdontneed=1".to_string(),
                kind: ModifierKind::List,
                separator: None,
            },
        ];
        reconcile_list_separators(&mut entries).expect("one explicit separator is agreement");
        assert_eq!(entries[1].separator.as_deref(), Some(","));

        let mut env = Env::clean();
        env.apply_entries(&entries);
        assert_eq!(env.get("GODEBUG").unwrap(), "gctrace=1,madvdontneed=1");
    }

    /// An entry that omits the separator can also come *first*: the explicit
    /// one still establishes the key, wherever it sits.
    #[test]
    fn reconcile_establishes_from_a_later_explicit_separator() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut entries = vec![
            Entry {
                key: "GODEBUG".to_string(),
                value: "madvdontneed=1".to_string(),
                kind: ModifierKind::List,
                separator: None,
            },
            list_entry("GODEBUG", "gctrace=1", ","),
        ];
        reconcile_list_separators(&mut entries).expect("one explicit separator is agreement");
        assert_eq!(entries[0].separator.as_deref(), Some(","));
    }

    /// Nobody established one → the entries keep `None`, and the fold applies
    /// the default. Filling in a space here would erase that distinction from
    /// everything downstream, including the forwarding envelope.
    #[test]
    fn reconcile_leaves_none_when_no_contributor_declared_one() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut entries = vec![Entry {
            key: "JDK_JAVA_OPTIONS".to_string(),
            value: "-ea".to_string(),
            kind: ModifierKind::List,
            separator: None,
        }];
        reconcile_list_separators(&mut entries).expect("no declaration is not a conflict");
        assert_eq!(entries[0].separator, None);
    }

    /// Two explicit separators that disagree fail closed, naming the key and
    /// both spellings. First-wins would silently corrupt the loser's
    /// contribution for the consuming tool.
    #[test]
    fn reconcile_rejects_conflicting_explicit_separators() {
        let mut entries = vec![
            list_entry("GODEBUG", "gctrace=1", ","),
            list_entry("GODEBUG", "madvdontneed=1", ";"),
        ];
        let error = reconcile_list_separators(&mut entries).expect_err("two separators cannot both apply");
        let ListSeparatorError::Conflict { key, first, second } = &error else {
            panic!("expected a conflict; got {error:?}");
        };
        assert_eq!(key, "GODEBUG");
        assert_eq!(first, ",");
        assert_eq!(second, ";");
        let message = error.to_string();
        assert!(message.contains("GODEBUG") && message.contains(',') && message.contains(';'));
    }

    /// Agreement is per key: two keys with different separators is the normal
    /// case, not a conflict. A `Path` entry sharing a key with a list is
    /// likewise none of this function's business.
    #[test]
    fn reconcile_is_scoped_to_one_key() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut entries = vec![
            list_entry("GODEBUG", "gctrace=1", ","),
            list_entry("JDK_JAVA_OPTIONS", "-ea", " "),
            Entry {
                key: "GODEBUG".to_string(),
                value: "/opt/bin".to_string(),
                kind: ModifierKind::Path,
                separator: None,
            },
        ];
        reconcile_list_separators(&mut entries).expect("different keys never conflict");
        assert_eq!(entries[0].separator.as_deref(), Some(","));
        assert_eq!(entries[1].separator.as_deref(), Some(" "));
        assert_eq!(entries[2].separator, None, "a non-list entry inherits nothing");
    }

    /// Agreement has to span the caller's *whole* composition, which is not
    /// one vector: `ocx exec` composes the package entries and keeps the
    /// project entries separate so it can forward only the latter. Chaining
    /// two `iter_mut()`s is the shape that has to work.
    #[test]
    fn reconcile_spans_two_disjoint_vectors() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut composed = [list_entry("GODEBUG", "gctrace=1", ",")];
        let mut project = [Entry {
            key: "GODEBUG".to_string(),
            value: "madvdontneed=1".to_string(),
            kind: ModifierKind::List,
            separator: None,
        }];

        reconcile_list_separators(composed.iter_mut().chain(project.iter_mut()))
            .expect("one explicit separator across both vectors is agreement");
        assert_eq!(project[0].separator.as_deref(), Some(","));

        let mut conflicting = [list_entry("GODEBUG", "asyncpreemptoff=1", ";")];
        let error = reconcile_list_separators(composed.iter_mut().chain(conflicting.iter_mut()))
            .expect_err("a conflict across two vectors is still a conflict");
        assert!(matches!(&error, ListSeparatorError::Conflict { key, .. } if key == "GODEBUG"));
    }

    /// A value the author never edged becomes edged once it inherits a
    /// separator. `--env "GODEBUG:list=,a"` is legal at parse — the effective
    /// separator is not known there — so this is the only place it can be
    /// caught, and an edged value fuses with the fold's wrapper.
    #[test]
    fn reconcile_rejects_a_value_edged_by_its_inherited_separator() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut entries = vec![
            list_entry("GODEBUG", "gctrace=1", ","),
            Entry {
                key: "GODEBUG".to_string(),
                value: ",madvdontneed=1".to_string(),
                kind: ModifierKind::List,
                separator: None,
            },
        ];
        let error = reconcile_list_separators(&mut entries).expect_err("an inherited separator must still be checked");
        assert!(
            matches!(&error, ListSeparatorError::EdgedValue { key, separator, value }
                if key == "GODEBUG" && separator == "," && value == ",madvdontneed=1"),
            "expected EdgedValue naming key, separator and value; got: {error}"
        );
    }

    /// Nobody declared a separator, so the key folds with the default — and a
    /// value edged by *that* is just as ambiguous.
    #[test]
    fn reconcile_rejects_a_value_edged_by_the_default_separator() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        for value in [" -ea", "-ea "] {
            let mut entries = vec![Entry {
                key: "JDK_JAVA_OPTIONS".to_string(),
                value: value.to_string(),
                kind: ModifierKind::List,
                separator: None,
            }];
            let error =
                reconcile_list_separators(&mut entries).expect_err("the default separator is an effective separator");
            assert!(
                matches!(&error, ListSeparatorError::EdgedValue { separator, .. } if separator == " "),
                "expected EdgedValue naming the default separator; got: {error}"
            );
        }
    }

    /// A value carrying the separator in its interior is one opaque
    /// contribution, not an edged one — the check must not reject it.
    #[test]
    fn reconcile_accepts_values_that_merely_contain_their_separator() {
        use crate::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut entries = vec![
            list_entry("GODEBUG", "gctrace=1,madvdontneed=1", ","),
            Entry {
                key: "GODEBUG".to_string(),
                value: "asyncpreemptoff=1".to_string(),
                kind: ModifierKind::List,
                separator: None,
            },
        ];
        reconcile_list_separators(&mut entries).expect("an interior separator is not an edge");
        assert_eq!(entries[1].separator.as_deref(), Some(","));
    }

    /// Repeating the same separator is agreement, not a conflict.
    #[test]
    fn reconcile_accepts_repeated_agreement() {
        let mut entries = vec![
            list_entry("GODEBUG", "gctrace=1", ","),
            list_entry("GODEBUG", "madvdontneed=1", ","),
        ];
        reconcile_list_separators(&mut entries).expect("agreeing entries are not a conflict");
    }

    // ── `resolve_test_command` — the #268 shadow rule ──────────────────────

    /// Builds a `PATH`-kind entry for `dir`, as the composer would.
    fn path_entry(key: &str, dir: &std::path::Path) -> crate::metadata::env::entry::Entry {
        crate::metadata::env::entry::Entry {
            separator: None,
            key: key.to_string(),
            value: dir.to_str().unwrap().to_string(),
            kind: crate::metadata::env::modifier::ModifierKind::Path,
        }
    }

    /// The #268 regression: a package that ships `tool` without the executable
    /// bit must fail loudly, never fall through to the host's copy.
    ///
    /// The composition mirrors the real one — the package entry prepends onto
    /// an inherited PATH — so `resolve_command`'s answer here is exactly what
    /// `ocx package test` used to execute.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_errors_when_package_copy_not_executable() {
        let package_dir = tempfile::tempdir().unwrap();
        let ambient_dir = tempfile::tempdir().unwrap();
        let shipped = write_binary(package_dir.path(), "tool", 0o644);
        let decoy = write_binary(ambient_dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", ambient_dir.path());
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        // Anchor: the old resolver silently picked the host copy. If this ever
        // stops holding, the test below no longer discriminates.
        assert!(
            same_file(&env.resolve_command("tool").unwrap(), &decoy),
            "precondition: resolve_command must still return the host decoy"
        );

        let error = env
            .resolve_test_command("tool")
            .expect_err("a non-executable package copy must not fall through to the host");
        let message = error.to_string();
        assert!(
            message.contains(shipped.to_str().unwrap()),
            "error must name the package path, got: {message}"
        );
        assert!(message.contains("0644"), "error must name the mode, got: {message}");
    }

    /// Happy path is unchanged: an executable the package ships wins, and the
    /// answer is the one `resolve_command` already gave.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_prefers_package_dir_over_ambient() {
        let package_dir = tempfile::tempdir().unwrap();
        let ambient_dir = tempfile::tempdir().unwrap();
        let shipped = write_binary(package_dir.path(), "tool", 0o755);
        write_binary(ambient_dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", ambient_dir.path());
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        let resolved = env.resolve_test_command("tool").unwrap();
        assert!(
            same_file(&resolved, &shipped),
            "package copy must win; got {}",
            resolved.display()
        );
        assert!(same_file(&resolved, &env.resolve_command("tool").unwrap()));
    }

    /// A name the package does not ship still resolves on the host PATH — the
    /// `sh` / `grep` case every test script relies on.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_falls_back_when_package_ships_nothing() {
        let package_dir = tempfile::tempdir().unwrap();
        let ambient_dir = tempfile::tempdir().unwrap();
        write_binary(package_dir.path(), "other", 0o755);
        let host_tool = write_binary(ambient_dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", ambient_dir.path());
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        let resolved = env.resolve_test_command("tool").unwrap();
        assert!(
            same_file(&resolved, &host_tool),
            "an unshipped name must resolve on the host PATH; got {}",
            resolved.display()
        );
    }

    /// A dependency's `bin/` is a package directory too — the scan covers every
    /// composed PATH entry, not just the root package's.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_searches_dependency_dirs() {
        let root_dir = tempfile::tempdir().unwrap();
        let dependency_dir = tempfile::tempdir().unwrap();
        let shipped = write_binary(dependency_dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.apply_entries(&[
            path_entry("PATH", dependency_dir.path()),
            path_entry("PATH", root_dir.path()),
        ]);

        let resolved = env.resolve_test_command("tool").unwrap();
        assert!(
            same_file(&resolved, &shipped),
            "a dependency's bin dir must be searched; got {}",
            resolved.display()
        );
    }

    /// A blocked hit does not veto an executable copy a *later* package
    /// directory ships. The scan remembers the block and keeps going; the
    /// error is only reported when the whole scan turned up nothing runnable.
    ///
    /// This is the case that makes `blocked` a remembered `Option` rather than
    /// an early return — a dependency shipping a stray 0644 `tool` must not
    /// fail a package that ships a working one.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_later_executable_beats_an_earlier_blocked_copy() {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        write_binary(first_dir.path(), "tool", 0o644);
        let executable = write_binary(second_dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        // `apply_entries` prepends each PATH value, so applying second then
        // first leaves `first_dir` ahead of `second_dir` in the scan order.
        env.apply_entries(&[
            path_entry("PATH", second_dir.path()),
            path_entry("PATH", first_dir.path()),
        ]);

        let resolved = env
            .resolve_test_command("tool")
            .expect("an executable copy in a later dir must win over an earlier blocked one");
        assert!(
            same_file(&resolved, &executable),
            "expected the executable copy in the second dir; got {}",
            resolved.display()
        );
    }

    /// Two executable copies: scan order decides, and scan order is PATH
    /// order — the same answer `resolve_command` gives.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_first_executable_in_path_order_wins() {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let first = write_binary(first_dir.path(), "tool", 0o755);
        write_binary(second_dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.apply_entries(&[
            path_entry("PATH", second_dir.path()),
            path_entry("PATH", first_dir.path()),
        ]);

        let resolved = env.resolve_test_command("tool").unwrap();
        assert!(
            same_file(&resolved, &first),
            "the first package dir in PATH order must win; got {}",
            resolved.display()
        );
        assert!(
            same_file(&resolved, &env.resolve_command("tool").unwrap()),
            "package-scan order must agree with what the OS would run"
        );
    }

    /// Only `PATH` contributes executables. A `LD_LIBRARY_PATH` entry pointing
    /// at a directory that happens to hold a same-named non-executable file
    /// must not turn a legitimate host lookup into an error.
    #[cfg(unix)]
    #[test]
    fn apply_entries_records_only_path_dirs() {
        let library_dir = tempfile::tempdir().unwrap();
        let ambient_dir = tempfile::tempdir().unwrap();
        write_binary(library_dir.path(), "tool", 0o644);
        let host_tool = write_binary(ambient_dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", ambient_dir.path());
        env.apply_entries(&[path_entry("LD_LIBRARY_PATH", library_dir.path())]);

        let resolved = env
            .resolve_test_command("tool")
            .expect("a LD_LIBRARY_PATH dir contributes no executables");
        assert!(same_file(&resolved, &host_tool));
    }

    /// A path-bearing name addresses a file directly — no package-versus-host
    /// question — so it must behave exactly as `resolve_command` does.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_delegates_path_bearing_names() {
        let package_dir = tempfile::tempdir().unwrap();
        let shipped = write_binary(package_dir.path(), "tool", 0o644);

        let mut env = Env::clean();
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        for name in [shipped.to_str().unwrap(), "./tool", "..", "a/b"] {
            assert_eq!(
                env.resolve_test_command(name).unwrap(),
                env.resolve_command(name).unwrap(),
                "path-bearing '{name}' must delegate unchanged"
            );
        }
    }

    /// On Windows a package-shipped `<name>.exe` is found through the child
    /// env's PATHEXT, and a name that already carries the extension resolves
    /// through the bare-name candidate appended after them.
    #[cfg(windows)]
    #[test]
    fn resolve_test_command_finds_package_exe_via_pathext() {
        let package_dir = tempfile::tempdir().unwrap();
        let shipped = package_dir.path().join("tool.exe");
        std::fs::write(&shipped, b"MZ").unwrap();

        let mut env = Env::clean();
        env.set("PATHEXT", ".EXE;.CMD");
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        // `tool` matches via the `.EXE` candidate; `tool.exe` only matches
        // because the bare name is probed too — without it a `-- tool.exe`
        // invocation would miss the package scan entirely and fall to the host.
        for invoked in ["tool", "tool.exe"] {
            let resolved = env
                .resolve_test_command(invoked)
                .unwrap_or_else(|e| panic!("'{invoked}' must resolve inside the package: {e}"));
            assert_eq!(
                resolved.file_name().unwrap().to_str().unwrap().to_ascii_lowercase(),
                "tool.exe",
                "'{invoked}' must resolve to the package's tool.exe"
            );
        }
    }

    // ── C-011: `resolve_test_command`, one case per named caller ───────────
    //
    // Four production callers depend on this method's error semantics, and
    // C-009 must not change any of them. Each test below is named for the
    // caller whose dependency it pins.

    /// C-011, `crates/ocx_cli/src/command/launcher/exec.rs` — every installed
    /// launcher's re-entry. The package's own executable copy wins, and the
    /// signature still returns that path.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_answers_the_launcher_re_entry_with_the_package_copy() {
        let package_dir = tempfile::tempdir().unwrap();
        let ambient_dir = tempfile::tempdir().unwrap();
        let shipped = write_binary(package_dir.path(), "tool", 0o755);
        write_binary(ambient_dir.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", ambient_dir.path());
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        let resolved = env.resolve_test_command("tool").expect("the package ships the name");
        assert!(same_file(&resolved, &shipped), "got {}", resolved.display());
    }

    /// C-011, `crates/ocx_cli/src/command/package_test.rs` — a package that
    /// ships the name without the executable bit is still a hard
    /// `NotExecutable`, never a silent fall-through to a host copy.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_keeps_not_executable_terminal_for_package_test() {
        let package_dir = tempfile::tempdir().unwrap();
        let shipped = write_binary(package_dir.path(), "tool", 0o600);

        let mut env = Env::clean();
        env.set("PATH", "");
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        let error = env
            .resolve_test_command("tool")
            .expect_err("a non-executable package copy is terminal");
        let CommandResolutionError::NotExecutable { path, mode, .. } = &error else {
            panic!("expected NotExecutable, got {error:?}");
        };
        assert_eq!(path, &shipped);
        assert_eq!(*mode, 0o600, "the error names the permission bits it found");
    }

    /// C-011, `crates/ocx_cli/src/command/patch_test.rs` — a path-bearing
    /// command still delegates to [`Env::resolve_command`] unchanged, so the
    /// package-versus-host question is never asked of a value that names a file.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_delegates_a_path_bearing_name_for_patch_test() {
        let package_dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        write_binary(package_dir.path(), "tool", 0o755);
        let direct = write_binary(elsewhere.path(), "tool", 0o755);

        let mut env = Env::clean();
        env.set("PATH", "");
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        let resolved = env
            .resolve_test_command(direct.as_os_str())
            .expect("an absolute path names a file directly");
        assert!(
            same_file(&resolved, &direct),
            "a path-bearing value must not be redirected to the package copy; got {}",
            resolved.display()
        );
    }

    /// C-011, `crates/ocx_script/src/ocx_module.rs` — the Starlark host
    /// behind `ocx package test --script`, and the canary for this contract.
    ///
    /// A **total miss** — the package ships nothing and the host provides
    /// nothing — falls through to the bare name, never an `Err`. That
    /// fall-through is deliberately retained across C-009, and
    /// `ocx_module.rs`'s shipped `bare_name_does_not_anchor_on_cwd` panics the
    /// moment it becomes an error.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_falls_through_to_the_bare_name_for_the_starlark_host() {
        let package_dir = tempfile::tempdir().unwrap();
        write_binary(package_dir.path(), "other", 0o755);

        let mut env = Env::clean();
        env.set("PATH", "");
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        let resolved = env
            .resolve_test_command("__ocx_wp2_absent_tool__")
            .expect("a total miss falls through to the bare name, never an error");
        assert_eq!(
            resolved,
            PathBuf::from("__ocx_wp2_absent_tool__"),
            "the bare name is handed back for the OS to answer"
        );
    }

    /// C-011 ∧ C-069, Block A: the fall-through is scoped to the **total
    /// miss**, so a trampoline refusal propagates as itself instead of becoming
    /// a bare name.
    ///
    /// The bare name is the whole defect: `execvp` performs its own lookup
    /// against the **ambient** `PATH` and finds the same trampoline, so the
    /// A → B → A re-entry C-069 exists to stop is restored one level up, with
    /// no error and no depth counter. Reachable from
    /// `crates/ocx_cli/src/command/launcher/exec.rs`'s `run_with_env`, which
    /// builds `Env::new()` — the ambient `PATH`, carrying a `bin`-mode
    /// toolchain's trampolines — prunes nothing, and calls this method for
    /// every installed launcher's re-entry.
    #[cfg(unix)]
    #[test]
    fn resolve_test_command_propagates_a_trampoline_refusal_instead_of_the_bare_name() {
        let package_dir = tempfile::tempdir().unwrap();
        let foreign_home = tempfile::tempdir().unwrap();
        // The package ships something, just not the name under test — so the
        // scan misses and the host lookup is reached.
        write_binary(package_dir.path(), "other", 0o755);
        let trampoline = write_trampoline(foreign_home.path(), "cmake");

        let mut env = Env::clean();
        env.set("PATH", foreign_home.path());
        env.apply_entries(&[path_entry("PATH", package_dir.path())]);

        let error = env
            .resolve_test_command("cmake")
            .expect_err("a refused trampoline is not a total miss and must not fall through");
        let CommandResolutionError::TrampolineRefused { command, path } = &error else {
            panic!("expected TrampolineRefused — a bare name here re-arms the loop, got {error:?}");
        };
        assert_eq!(command, "cmake");
        assert!(
            same_file(path, &trampoline),
            "the refusal must name the trampoline it found; got {}",
            path.display()
        );
    }
}
