// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use ocx_lib::package::metadata::env::entry::Entry;

/// The repeatable `--env KEY[:TYPE[:SEP]]=VALUE` per-invocation override.
///
/// Flatten into a command with `#[clap(flatten)]` to add the flag, then read
/// the resolved entries through [`EnvOverride::entries`] — never the raw
/// strings. The flag belongs to both CLI tiers: it is a per-invocation
/// override, not project configuration, so adding it to an OCI-tier command
/// does not make that command read `ocx.toml`.
///
/// The entries are the highest-precedence stage of the composition order, so a
/// caller appends them last, after the package-composed set and (project tier)
/// after the project and group `[env]` tables.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct EnvOverride {
    /// Set an environment variable for this invocation, as `KEY[:TYPE[:SEP]]=VALUE`.
    ///
    /// Repeatable; a later `--env` for the same key wins. The value is split
    /// on the FIRST `=`, so `--env FOO=a=b` sets `FOO` to `a=b`. Values are
    /// literal - no interpolation.
    ///
    /// `TYPE` is `constant` (replace the existing value), `path` (prepend to
    /// it) or `list` (append to it), and defaults to `constant`. A relative
    /// `path` value resolves against the current directory, so
    /// `--env PATH:path=node_modules/.bin` puts a project-local directory in
    /// front of the composed `PATH`. Without `:path` a `PATH` override
    /// replaces the composed value outright, dropping every package directory.
    ///
    /// `SEP` is the string a `list` contribution is joined to the existing
    /// value with, as in `--env GODEBUG:list:,=gctrace=1`. It accepts any
    /// text, including a colon. Omitted, the key takes whatever separator
    /// another contributor to it declared, or a single space if none did.
    ///
    /// Applied last, so it overrides every package-declared variable - and,
    /// for the project toolchain, the project's `[env]` and any selected
    /// group's `[env]` as well.
    ///
    /// A bare `--env FOO` (pass the ambient value through) is not accepted;
    /// keys matching `OCX_*` or `__OCX_*` are rejected; so is a `TYPE` outside
    /// `constant`, `path` and `list`, a `SEP` that is empty or that qualifies
    /// any type but `list`, and a `list` value starting or ending with its own
    /// separator. All exit 64.
    #[arg(long = "env", value_name = "KEY[:TYPE[:SEP]]=VALUE")]
    env: Vec<String>,
}

impl EnvOverride {
    /// Parse the collected `--env` arguments into resolved entries.
    ///
    /// Splits each argument on the FIRST `=` so a value may itself contain `=`.
    /// The segment before it is `KEY`, `KEY:TYPE` or `KEY:TYPE:SEP`, where
    /// `TYPE` names a [`ModifierKind`] — `constant` (replace), `path`
    /// (prepend) or `list` (append). Omitted, it is `constant`, so a plain
    /// `KEY=VALUE` means exactly what it always did. Values are literal, never
    /// interpolated. A later occurrence of a key overrides an earlier one.
    ///
    /// `SEP` is raw text, taken from the first `:` after the type to the end
    /// of the segment — so it may itself be or contain a colon, and
    /// `K:list::=x` declares the separator `:`. Omitting it leaves the entry's
    /// separator unset for
    /// [`reconcile_list_separators`](ocx_lib::env::reconcile_list_separators)
    /// to settle against the other contributors to the same key; that is why
    /// this parser cannot check an omitted separator against the value, and
    /// only rejects a value edged by a separator spelled out here.
    ///
    /// The grammar is unambiguous because [`ocx_lib::env::is_valid_env_key`]
    /// admits no `:` — a colon in the pre-`=` segment is therefore always the
    /// type marker or part of the separator, and a Windows value like
    /// `C:\tools\bin` is untouched because only that segment is inspected.
    ///
    /// A relative `path` value resolves against `cwd`, the directory the flag
    /// was typed in. This deliberately differs from the `ocx.toml` form, which
    /// anchors to the project root for cwd-independence: a checked-in file must
    /// mean the same thing from any subdirectory, whereas a flag is composed by
    /// whatever script is invoking ocx, and cwd is the one base such a script
    /// can compute. Both forms resolve to an absolute value before the entry
    /// exists, so an emitted export line means the same thing wherever it is
    /// evaluated.
    ///
    /// `cwd` is a parameter rather than a `current_dir()` call inside the body
    /// so the function stays pure and directly unit-testable.
    ///
    /// # Errors
    ///
    /// [`ocx_lib::cli::UsageError`] (exit 64) when an argument carries no `=`
    /// (bare `--env FOO` ambient pass-through is not accepted in v1: it has
    /// meaning only under `--clean`, and admitting it later is purely
    /// additive), when the declared `TYPE` names no modifier, when a `SEP`
    /// qualifies a type other than `list`, when a `SEP` is one the fold cannot
    /// use, when a `list` value starts or ends with its own separator, when
    /// the key is outside the POSIX environment-name grammar, or when a key
    /// matches the reserved `OCX_*` / `__OCX_*` namespace — the same gate the
    /// file form applies, so the flag cannot be the way in.
    ///
    /// [`ModifierKind`]: ocx_lib::package::metadata::env::modifier::ModifierKind
    pub fn entries(&self, cwd: &Path) -> Result<Vec<Entry>, ocx_lib::cli::UsageError> {
        use ocx_lib::package::metadata::env::list::{is_separator_edged, separator_is_valid};
        use ocx_lib::package::metadata::env::modifier::ModifierKind;

        if self.env.is_empty() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::with_capacity(self.env.len());
        for argument in &self.env {
            // First `=` only: everything after it is the value, verbatim. This
            // is what makes `--env FOO=a=b` set `FOO` to `a=b` rather than
            // erroring.
            let Some((qualified_key, value)) = argument.split_once('=') else {
                return Err(ocx_lib::cli::UsageError::new(format!(
                    "--env value '{argument}' is not KEY[:TYPE[:SEP]]=VALUE; passing an ambient variable through by name is not supported"
                )));
            };
            // Strip the qualifier BEFORE the key gates, so `PATH:path=/x` is
            // reported against `PATH` — running them on the raw segment would
            // reject a well-formed argument as an invalid variable name.
            let (key, kind, declared_separator) = match qualified_key.split_once(':') {
                None => (qualified_key, ModifierKind::Constant, None),
                Some((key, qualifier)) => {
                    // Exactly one more split: the separator is raw text to the
                    // end of the segment, so `K:list::=x` reads as the
                    // separator `:` rather than an empty one plus a stray.
                    let (declared_type, declared_separator) = match qualifier.split_once(':') {
                        None => (qualifier, None),
                        Some((declared_type, separator)) => (declared_type, Some(separator)),
                    };
                    let kind = declared_type
                        .parse::<ModifierKind>()
                        .map_err(|error| ocx_lib::cli::UsageError::new(format!("--env value '{argument}': {error}")))?;
                    (key, kind, declared_separator)
                }
            };
            let separator = match declared_separator {
                None => None,
                Some(_) if kind != ModifierKind::List => {
                    return Err(ocx_lib::cli::UsageError::new(format!(
                        "--env value '{argument}': a separator qualifies a `list` value only, not `{kind}`"
                    )));
                }
                // Through the same predicate the metadata and `ocx.toml`
                // surfaces use, so one rule cannot drift into three. Its `=`
                // half is unreachable here (a separator containing `=` would
                // have ended the qualifier at that `=`); the empty and
                // line-break halves are both reachable from argv.
                //
                // `{:?}` on the separator: it is refused for carrying
                // something unprintable, and echoing a raw newline would split
                // the error into two lines (CWE-117).
                Some(separator) if !separator_is_valid(separator) => {
                    return Err(ocx_lib::cli::UsageError::new(format!(
                        "--env value '{argument}': separator {separator:?} must be non-empty and free of '=', newline and carriage return"
                    )));
                }
                Some(separator) if is_separator_edged(value, separator) => {
                    return Err(ocx_lib::cli::UsageError::new(format!(
                        "--env value '{argument}': the value {value:?} starts or ends with its separator {separator:?}"
                    )));
                }
                Some(separator) => Some(separator.to_owned()),
            };
            if !ocx_lib::env::is_valid_env_key(key) {
                return Err(ocx_lib::cli::UsageError::new(format!(
                    "--env key '{key}' is not a valid environment variable name"
                )));
            }
            if ocx_lib::env::is_reserved_ocx_key(key) {
                return Err(ocx_lib::cli::UsageError::new(format!(
                    "--env key '{key}' is reserved; OCX_* and __OCX_* keys cannot be set"
                )));
            }
            entries.push(Entry {
                key: key.to_owned(),
                value: match kind {
                    // A list contribution is literal text like a constant — the
                    // cwd anchors directories, not option strings.
                    ModifierKind::Constant | ModifierKind::List => value.to_owned(),
                    // Same `join` resolution the file form uses: an absolute
                    // value replaces the base outright, a relative one lands
                    // under it, and a driveless-but-rooted POSIX-authored value
                    // gets anchored to a drive on Windows instead of following
                    // the process's.
                    ModifierKind::Path => cwd.join(value).to_string_lossy().into_owned(),
                },
                kind,
                separator,
            });
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cwd every `--env` unit test resolves a relative `:path` against.
    /// A distinctive absolute stand-in, so an assertion that a value is rooted
    /// here cannot pass by accident.
    fn parse_cwd() -> &'static Path {
        Path::new(if cfg!(windows) {
            r"C:\invocation\dir"
        } else {
            "/invocation/dir"
        })
    }

    /// Build the flatten struct directly from raw argument strings — the same
    /// shape clap produces — and resolve against [`parse_cwd`].
    fn parse(arguments: &[&str]) -> Result<Vec<Entry>, ocx_lib::cli::UsageError> {
        EnvOverride {
            env: arguments.iter().map(|argument| (*argument).to_string()).collect(),
        }
        .entries(parse_cwd())
    }

    /// Parse one `--env` argument, for the single-value cases that make up most
    /// of this module.
    fn parse_one(argument: &str) -> Result<Vec<Entry>, ocx_lib::cli::UsageError> {
        parse(&[argument])
    }

    /// No `--env` at all yields no entries — the flag is optional on every
    /// command that flattens this struct.
    #[test]
    fn env_override_absent_yields_no_entries() {
        assert!(parse(&[]).expect("an absent flag must parse").is_empty());
    }

    /// A plain `KEY=VALUE` becomes a constant entry — the unqualified form's
    /// meaning is unchanged by the optional `:TYPE` qualifier.
    #[test]
    fn env_override_parses_key_value_as_constant() {
        use ocx_lib::package::metadata::env::modifier::ModifierKind;

        let parsed = parse_one("FOO=bar").expect("KEY=VALUE must parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].key, "FOO");
        assert_eq!(parsed[0].value, "bar");
        assert_eq!(parsed[0].kind, ModifierKind::Constant);
    }

    /// L1: split on the FIRST `=` only, so a value may itself contain `=`.
    #[test]
    fn env_override_splits_on_first_equals_only() {
        let parsed = parse_one("FOO=a=b").expect("a value containing '=' must parse");
        assert_eq!(parsed[0].key, "FOO");
        assert_eq!(parsed[0].value, "a=b", "only the first '=' separates key from value");

        // An empty value is legitimate — `FOO=` sets FOO to the empty string.
        let parsed = parse_one("FOO=").expect("an empty value must parse");
        assert_eq!(parsed[0].value, "");
    }

    /// L2: a bare `--env FOO` (docker-style ambient pass-through) is a usage
    /// error in v1 — it has meaning only under `--clean`, and admitting it
    /// later is purely additive.
    #[test]
    fn env_override_rejects_bare_name() {
        let error = parse_one("FOO").expect_err("a bare name must be rejected");
        assert!(
            error.to_string().contains("KEY[:TYPE[:SEP]]=VALUE"),
            "the error must name the expected form; got: {error}"
        );
    }

    /// X1 applies to the flag, not just the file: `--env` must not be the way
    /// in for a key that reconfigures how ocx itself resolves.
    #[test]
    fn env_override_rejects_reserved_ocx_keys() {
        for reserved in ["OCX_DEFAULT_REGISTRY=evil.example.com", "__OCX_TESTING_X=1"] {
            let error = parse_one(reserved).expect_err("a reserved key must be rejected: {reserved}");
            assert!(
                error.to_string().contains("reserved"),
                "the error must say the key is reserved; got: {error}"
            );
        }
    }

    /// X2: keys route through the shared POSIX name validator, so the flag
    /// cannot inject through the key slot of an emitted assignment line.
    #[test]
    fn env_override_rejects_invalid_key_grammar() {
        for invalid in ["A B=1", "1A=1", "=1"] {
            assert!(
                parse_one(invalid).is_err(),
                "'{invalid}' must be rejected by the shared key validator"
            );
        }
    }

    /// Repeatable: every occurrence is kept, in order, so the last one wins at
    /// application time (`Env::apply_entries` replays the vector).
    #[test]
    fn env_override_keeps_every_occurrence_in_order() {
        let parsed = parse(&["FOO=first", "FOO=second"]).expect("ok");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].value, "first");
        assert_eq!(parsed[1].value, "second", "a later --env must be applied last");
    }

    // ── the optional `:TYPE` qualifier ────────────────────────────────────────

    /// Both spellings of the qualifier are accepted, and each selects its
    /// modifier. `:constant` exists for symmetry with the file's explicit
    /// `{ type = "constant", … }` form.
    #[test]
    fn env_override_qualifier_selects_the_modifier() {
        use ocx_lib::package::metadata::env::modifier::ModifierKind;

        let parsed = parse_one("JAVA_OPTS:constant=-Xmx2g").expect("an explicit :constant must parse");
        assert_eq!(parsed[0].key, "JAVA_OPTS", "the qualifier must not leak into the key");
        assert_eq!(parsed[0].value, "-Xmx2g");
        assert_eq!(parsed[0].kind, ModifierKind::Constant);

        let parsed = parse_one(if cfg!(windows) {
            r"PATH:path=C:\opt\tools\bin"
        } else {
            "PATH:path=/opt/tools/bin"
        })
        .expect("a :path must parse");
        assert_eq!(parsed[0].key, "PATH");
        assert_eq!(
            parsed[0].kind,
            ModifierKind::Path,
            "a :path entry must prepend, not replace"
        );
    }

    /// An absolute `:path` value passes through; a relative one anchors to the
    /// supplied cwd. This is the behavior that distinguishes the flag from the
    /// `ocx.toml` form, which anchors to the project root.
    #[test]
    fn env_override_path_resolves_relative_against_cwd() {
        let absolute = if cfg!(windows) {
            r"C:\opt\tools\bin"
        } else {
            "/opt/tools/bin"
        };
        let parsed = parse_one(&format!("PATH:path={absolute}")).expect("an absolute :path must parse");
        assert_eq!(
            Path::new(&parsed[0].value),
            Path::new(absolute),
            "an absolute value must pass through untouched"
        );

        let parsed = parse_one("PATH:path=node_modules/.bin").expect("a relative :path must parse");
        let resolved = Path::new(&parsed[0].value);
        assert!(
            resolved.is_absolute(),
            "a relative value must resolve absolute: {resolved:?}"
        );
        assert!(
            resolved.starts_with(parse_cwd()),
            "a relative value must anchor to the invocation directory, not the project root: {resolved:?}"
        );
    }

    /// The first-`=` split survives the qualifier: only the segment BEFORE the
    /// first `=` is inspected for a type marker.
    #[test]
    fn env_override_qualifier_preserves_first_equals_split() {
        let parsed = parse_one("FOO:constant=a=b").expect("a qualified value containing '=' must parse");
        assert_eq!(parsed[0].key, "FOO");
        assert_eq!(parsed[0].value, "a=b");
    }

    /// A Windows-shaped value carries its own colon. Because only the pre-`=`
    /// segment is split on `:`, the drive letter is never mistaken for a type
    /// marker — the invariant, asserted without pinning a platform-specific
    /// resolved literal.
    #[test]
    fn env_override_qualifier_ignores_colons_in_the_value() {
        let parsed = parse_one(r"PATH:path=C:\tools\bin").expect("a drive-lettered value must parse");
        assert_eq!(parsed[0].key, "PATH", "the drive letter must not be read as a type");
        assert!(
            parsed[0].value.ends_with(r"C:\tools\bin"),
            "the value must survive intact; got: {}",
            parsed[0].value
        );
    }

    /// A `TYPE` naming no modifier is CLI misuse (exit 64), not a config-shape
    /// fault — and the message must name what is accepted, since the user has
    /// no file to look at.
    #[test]
    fn env_override_rejects_unknown_and_empty_type() {
        for invalid in ["X:bogus=v", "X:=v"] {
            let error = parse_one(invalid).expect_err("'{invalid}' names no modifier");
            let message = error.to_string();
            assert!(
                message.contains("path") && message.contains("constant"),
                "the error must name the accepted types; got: {message}"
            );
        }

        let error = parse_one("X:bogus=v").expect_err("must reject");
        assert!(
            error.to_string().contains("bogus"),
            "the error must quote the offending type; got: {error}"
        );
    }

    /// The security-relevant case: the reserved-namespace gate runs on the key
    /// AFTER the qualifier is stripped, so `:path` is not a way past it.
    #[test]
    fn env_override_reserved_gate_survives_qualifier_stripping() {
        for reserved in [
            "OCX_DEFAULT_REGISTRY:path=evil.example.com",
            "__OCX_TESTING_X:constant=1",
        ] {
            let error = parse_one(reserved).expect_err("a qualified reserved key must still be rejected");
            assert!(
                error.to_string().contains("reserved"),
                "the error must say the key is reserved; got: {error}"
            );
        }
    }

    /// The grammar gate still applies to the key portion once the qualifier is
    /// removed — stripping must not become a way to smuggle an invalid name.
    #[test]
    fn env_override_key_grammar_applies_after_qualifier_stripping() {
        for invalid in ["A B:path=1", "1A:constant=1", ":path=1"] {
            assert!(
                parse_one(invalid).is_err(),
                "'{invalid}' must be rejected by the shared key validator"
            );
        }
    }

    // ── the optional `:SEP` qualifier (W-5) ──────────────────────────────────

    /// Neither of the pre-`list` forms carries a separator, and the absence is
    /// what the composer reads to mean "no opinion". Characterization: the
    /// `:SEP` slot must not change what `KEY=VALUE` and `KEY:path=VALUE` mean.
    #[test]
    fn env_override_unqualified_forms_declare_no_separator() {
        for argument in ["FOO=bar", "FOO:constant=bar", "PATH:path=bin"] {
            let parsed = parse_one(argument).expect("the pre-list forms must still parse");
            assert_eq!(parsed[0].separator, None, "'{argument}' must declare no separator");
        }
    }

    /// A `list` without `:SEP` is legal — the separator is settled at compose
    /// time from whatever else contributes to the key, so declaring one here
    /// is optional in a way the wire form does not allow.
    #[test]
    fn env_override_list_without_a_separator_leaves_it_unset() {
        use ocx_lib::package::metadata::env::modifier::ModifierKind;

        let parsed = parse_one("JDK_JAVA_OPTIONS:list=-Xmx2g").expect("a bare :list must parse");
        assert_eq!(parsed[0].key, "JDK_JAVA_OPTIONS");
        assert_eq!(parsed[0].kind, ModifierKind::List);
        assert_eq!(parsed[0].value, "-Xmx2g");
        assert_eq!(
            parsed[0].separator, None,
            "an omitted separator must stay unset, not become the default here"
        );
    }

    /// The separator is raw text through to the `=`: a colon is a legal
    /// separator (`K:list::=x`), and so is a multi-character one.
    #[test]
    fn env_override_list_separator_is_taken_verbatim() {
        for (argument, expected) in [
            ("GODEBUG:list:,=gctrace=1", ","),
            ("OPTS:list::=x", ":"),
            ("OPTS:list:; =x", "; "),
        ] {
            let parsed = parse_one(argument).unwrap_or_else(|error| panic!("'{argument}' must parse; got {error}"));
            assert_eq!(
                parsed[0].separator.as_deref(),
                Some(expected),
                "'{argument}' must declare the separator {expected:?}"
            );
        }
    }

    /// The first-`=` split is unaffected by the separator slot, so a list
    /// value may contain `=` — which `GODEBUG` entries routinely do.
    #[test]
    fn env_override_list_value_keeps_its_own_equals_signs() {
        let parsed = parse_one("GODEBUG:list:,=gctrace=1,madvdontneed=1").expect("a list value with '=' must parse");
        assert_eq!(parsed[0].key, "GODEBUG");
        assert_eq!(parsed[0].value, "gctrace=1,madvdontneed=1");
    }

    /// An empty separator is refused through the shared predicate: the fold's
    /// flank match degrades to a bare substring scan without one.
    #[test]
    fn env_override_rejects_an_empty_list_separator() {
        let error = parse_one("OPTS:list:=x").expect_err("an empty separator must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("non-empty"),
            "the error must name the separator rule; got: {message}"
        );
        assert!(
            message.contains("OPTS:list:=x"),
            "the error must echo the offending argument; got: {message}"
        );
    }

    /// A line break is not a separator on any surface, and the 64 surface must
    /// say so in the same words as the 65/78 ones — a rule spelled three ways
    /// reads as three rules.
    #[test]
    fn env_override_rejects_a_line_breaking_list_separator() {
        for (argument, escaped) in [("OPTS:list:\n=x", r#""\n""#), ("OPTS:list:\r=x", r#""\r""#)] {
            let message = parse_one(argument)
                .expect_err("a line-breaking separator must be rejected")
                .to_string();
            assert!(
                message.contains("newline and carriage return"),
                "the error must name the full separator rule; got: {message}"
            );
            // The separator renders escaped rather than raw, so the refused
            // byte is visible instead of breaking the line it is reported on.
            // (The `'{argument}'` echo is a separate, pre-existing raw site.)
            assert!(
                message.contains(escaped),
                "the separator must be debug-quoted; expected {escaped} in: {message:?}"
            );
        }
    }

    /// Only a `list` folds, so only a `list` has anything to fold with. A
    /// separator on any other type is a misunderstanding worth naming rather
    /// than a value to ignore.
    #[test]
    fn env_override_rejects_a_separator_on_a_non_list_type() {
        for argument in ["PATH:path:/=x", "JAVA_OPTS:constant:,=x"] {
            let error = parse_one(argument).expect_err("a separator outside :list must be rejected");
            let message = error.to_string();
            assert!(
                message.contains("`list`"),
                "the error must name the rule; got: {message}"
            );
            assert!(
                message.contains(argument),
                "the error must echo the offending argument; got: {message}"
            );
        }
    }

    /// A value edged by its own separator makes the append fold ambiguous —
    /// the wrapper the algorithm adds fuses with the value's own flank — so
    /// every surface refuses it, this one at exit 64.
    #[test]
    fn env_override_rejects_a_separator_edged_list_value() {
        for argument in ["GODEBUG:list:,=,gctrace=1", "GODEBUG:list:,=gctrace=1,"] {
            let error = parse_one(argument).expect_err("a separator-edged value must be rejected");
            let message = error.to_string();
            assert!(
                message.contains("separator \",\""),
                "the error must name the separator; got: {message}"
            );
            assert!(
                message.contains("gctrace=1"),
                "the error must name the offending value; got: {message}"
            );
        }
    }

    /// The flag parses through clap exactly as it does when constructed
    /// directly, including the repeatable form — the flatten wiring itself is
    /// part of the contract this struct owns.
    #[test]
    fn env_override_flattens_into_a_command() {
        use clap::Parser as _;

        #[derive(clap::Parser)]
        struct Harness {
            #[clap(flatten)]
            env: EnvOverride,
        }

        let harness = Harness::try_parse_from(["harness", "--env", "A=1", "--env", "B:constant=2"]).expect("parse");
        let parsed = harness.env.entries(parse_cwd()).expect("entries");
        assert_eq!(parsed.len(), 2, "both occurrences must reach the struct");
        assert_eq!(parsed[0].key, "A");
        assert_eq!(parsed[1].key, "B");
    }
}
