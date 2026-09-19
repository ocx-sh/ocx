// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the project, activation and lock error family — the `ocx_project` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_project::ProjectErrorKind;
use ocx_project::registry::ProjectRegistryErrorKind;

use ocx_package_manager::activation::SessionError;
use ocx_project::LockCurrency;
use ocx_project::error::Error as ProjectError;
use ocx_project::registry::error::Error as ProjectRegistryError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for SessionError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // 78 for an absent lock, 65 for a stale one — the mapping lives
            // with the wording it belongs to.
            Self::Lock(currency) => currency.classify(),
            // Defer to the wrapped library error's own classification, which
            // the chain walk reaches through `source()`.
            Self::Library(_) | Self::ListSeparator(_) => None,
        }
    }
}

impl ClassifyExitCode for ProjectError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Project(e) => match &e.kind {
                ProjectErrorKind::Io(_) => ExitCode::IoError,
                ProjectErrorKind::TomlParse(_)
                | ProjectErrorKind::TomlSerialize(_)
                | ProjectErrorKind::ReservedGroupName { .. }
                | ProjectErrorKind::InvalidToolchainNameCharset { .. }
                | ProjectErrorKind::ShellSectionInProject
                | ProjectErrorKind::UnsupportedDeclarationHashVersion { .. }
                | ProjectErrorKind::FileTooLarge { .. }
                | ProjectErrorKind::ToolValueMissingRegistry { .. }
                | ProjectErrorKind::ToolValueInvalid { .. }
                | ProjectErrorKind::PackageKeyMissingRegistry { .. }
                | ProjectErrorKind::PackageKeyInvalid { .. }
                // Schema-shape faults in `[group.*]` / `[env]`: the file is
                // valid TOML but not a valid ocx.toml. Same class as a
                // reserved group name, and the same remedy — edit the file.
                | ProjectErrorKind::GroupHoldsDirectBinding { .. }
                | ProjectErrorKind::UnknownGroupSection { .. }
                | ProjectErrorKind::EnvReservedKey { .. }
                | ProjectErrorKind::EnvInvalidKey { .. }
                | ProjectErrorKind::EnvUnknownModifier { .. }
                | ProjectErrorKind::EnvInvalidValue { .. }
                | ProjectErrorKind::EnvUnknownValueField { .. }
                | ProjectErrorKind::EnvSeparatorOnNonList { .. }
                | ProjectErrorKind::EnvInvalidSeparator { .. }
                | ProjectErrorKind::EnvSeparatorEdgedValue { .. }
                | ProjectErrorKind::LockRepositoryNotBare { .. }
                // The file on disk is valid TOML by the serde parser's reckoning
                // but not an editable document — same class, same remedy.
                | ProjectErrorKind::ManifestEditParse(_) => ExitCode::ConfigError,
                // 65, not the 78 its `[env]` siblings carry: the file is a valid
                // ocx.toml and the entry's *shape* is legal — the value itself is
                // the malformed datum, the same class the wire form already
                // refuses at 65. A-10 names the code.
                ProjectErrorKind::EnvPathSeparatorInValue { .. } => ExitCode::DataError,
                // Not a config fault: the format-preserving writer could not
                // express the staged mutation. Nothing the user can edit their
                // way out of, so no sysexits code would tell the truth.
                ProjectErrorKind::ManifestEditDiverged => ExitCode::Failure,
                ProjectErrorKind::EmptyGroupFilter
                | ProjectErrorKind::UnknownGroup { .. }
                | ProjectErrorKind::DuplicateToolAcrossSelectedGroups { .. }
                | ProjectErrorKind::BindingAmbiguous { .. } => ExitCode::UsageError,
                ProjectErrorKind::Locked => ExitCode::TempFail,
                ProjectErrorKind::TagNotFound { .. } => ExitCode::NotFound,
                ProjectErrorKind::AuthFailure { .. } => ExitCode::AuthError,
                // Exactly one cause may override the default, and it is the
                // transient one: the announced contract for this variant is
                // "75 when the resolver gives up on a transient fault".
                // `project_err_from_client` boxes the `ClientError` verbatim
                // for that downcast. Every other cause keeps 69 — deferring to
                // a typed non-transient cause would let it re-code the
                // lock-resolve interface (a `DigestMismatch` exhaustion
                // reaching a caller as 65), and a cause that is not a
                // `ClientError` at all has no classification to defer to.
                ProjectErrorKind::RegistryUnreachable { source, .. } => source
                    .downcast_ref::<ocx_oci::client::error::ClientError>()
                    .filter(|client| {
                        matches!(client, ocx_oci::client::error::ClientError::RegistryTransient(_))
                    })
                    .and_then(ClassifyExitCode::classify)
                    .unwrap_or(ExitCode::Unavailable),
                // A per-tool deadline on a registry interaction: nothing was
                // answered, so a rerun can genuinely succeed. Same rule the
                // transport applies to a request timeout one layer down.
                ProjectErrorKind::ResolveTimeout { .. } => ExitCode::TempFail,
                ProjectErrorKind::LockMissing => ExitCode::ConfigError,
                // The lock was written by an unsupported version — the user
                // must regenerate it, same remedy shape as a config mismatch.
                ProjectErrorKind::UnsupportedLockVersion { .. } => ExitCode::ConfigError,
                // The locked version ships no leaf for the host platform —
                // a pre-network config-state condition; the remedy is a
                // whole-file re-resolve (`ocx update`).
                ProjectErrorKind::NoHostLeaf { .. } => ExitCode::ConfigError,
                // Two or more leaves tie at the host platform's maximum D1
                // score — malformed/ambiguous *selection*, not a missing
                // entry (`ocx update` cannot fix a tie): same classification
                // as the fresh-resolve `SelectionAmbiguous` case (DataError).
                ProjectErrorKind::AmbiguousHostLeaf { .. } => ExitCode::DataError,
                ProjectErrorKind::ToolNotInConfig { .. } => ExitCode::NotFound,
                ProjectErrorKind::BindingAlreadyExists { .. } => ExitCode::UsageError,
                ProjectErrorKind::BindingNotFound { .. } => ExitCode::NotFound,
                ProjectErrorKind::ConfigAlreadyExists { .. } => ExitCode::UsageError,
                ProjectErrorKind::InvalidGroupName { .. } => ExitCode::UsageError,
                ProjectErrorKind::InvalidBindingName { .. } => ExitCode::UsageError,
                // Stale predecessor on partial-resolve: the caller's lock
                // snapshot is out of date with the live config. Same
                // classification as the read-side staleness gate
                // ([`LockCurrency::Stale`](ocx_project::LockCurrency) → DataError 65) so
                // wrappers and scripts get a single signal regardless of
                // which resolver path detected the mismatch.
                ProjectErrorKind::StaleLockOnPartial { .. } => ExitCode::DataError,
                // A dup-key collision is a structural integrity violation in
                // the resolved leaf map — classify as malformed data (65).
                ProjectErrorKind::DuplicatePlatformKey { .. } => ExitCode::DataError,
                // A noncanonical platform-map key is malformed on-disk data —
                // same classification as a dup-key collision.
                ProjectErrorKind::NoncanonicalPlatformKey { .. } => ExitCode::DataError,
                // Offline / frozen refused an unpinned-tag resolve during lock
                // building — same category as the index-layer policy block.
                ProjectErrorKind::PolicyBlocked { .. } => ExitCode::PolicyBlocked,
            },
            // The four variants the project tier minted at WP-33, when it
            // stopped borrowing `ocx_lib::Error` for failures it raises itself.
            // Each reproduces what that borrowed variant classified to, so the
            // narrowing moves no exit code (DEC-23):
            //
            //   ocx_lib::Error::OciClient(e)    => e.classify()
            //   ocx_lib::Error::OciIndex(e)     => e.classify()
            //   ocx_lib::Error::Config(e)       => e.classify()
            //   ocx_lib::Error::InternalFile(..) => IoError  (now ocx_util::error::FileError)
            //
            // Delegation rather than a flat code for the first three on
            // purpose: `ClientError` and the index error both distinguish
            // transient from terminal, and flattening would collapse 69 and 74.
            //
            // An early `return` rather than restructuring the `match` to
            // yield `Option`, so the `Self::Project` arm above keeps the exact
            // token stream `classify_baseline_7adaea62.json` records — the
            // baseline is token-exact by design and must never be rekeyed
            // (DEC-46/64). `return` and not `?` because `STANDS_IN_FOR`'s
            // `SameDelegation` normalises away a leading `return` and a binder
            // and nothing else: `e.classify()?` is the same code but a
            // different shape, and would read as a drift that is not one.
            Self::OciClient(e) => return e.classify(),
            Self::OciIndex(e) => return e.classify(),
            Self::Config(e) => return e.classify(),
            Self::InternalFile(_, _) => ExitCode::IoError,
        })
    }
}

impl ClassifyExitCode for LockCurrency {
    fn classify(&self) -> Option<ExitCode> {
        use ExitCode;
        match self {
            // Project exists but is not locked — a configuration gap.
            Self::Missing { .. } => Some(ExitCode::ConfigError),
            // Lock exists but disagrees with `ocx.toml` — stale on-disk data.
            Self::Stale { .. } => Some(ExitCode::DataError),
        }
    }
}

impl ClassifyExitCode for ProjectRegistryError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Registry(e) => match &e.kind {
                ProjectRegistryErrorKind::Io(_) => ExitCode::IoError,
            },
        })
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, SessionError);
    downcast_arm!(cause, ProjectError);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ocx_project::activate::ActivateMode;

    use ocx_project::config::ProjectConfig;

    // ── moved from ocx_project::config with the impl ──

    /// S8: a tool binding declared directly under `[group.<name>]` (the
    /// removed flat form) is a parse error naming the group and pointing at
    /// `[group.<name>.tools]`, classified `ExitCode::ConfigError` (78) — the
    /// error message IS the migration story for the handful of files
    /// written against the old shape.
    #[test]
    fn group_direct_binding_is_parse_error_naming_group_and_tools() {
        let toml_str = r#"
[group.ci]
bar = "ocx.sh/bar:1"
"#;
        let err = ProjectConfig::from_toml_str(toml_str).expect_err("direct binding under [group.ci] must reject");

        let code = <ocx_project::Error as ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(ExitCode::ConfigError),
            "S8 break must classify as ConfigError (exit 78); got {code:?}"
        );

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("[group.ci.tools]"),
            "message must point at [group.ci.tools]; got {rendered:?}"
        );

        #[allow(irrefutable_let_patterns)]
        let ocx_project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        let ProjectErrorKind::GroupHoldsDirectBinding { group, binding } = &pe.kind else {
            panic!("expected GroupHoldsDirectBinding, got {:?}", pe.kind);
        };
        assert_eq!(group, "ci");
        assert_eq!(binding, "bar");
    }

    /// A checked-in `ocx.toml` cannot declare the trust-sensitive `OCX_*`
    /// variables — not in `[env]`, not in a group's — because both sit in the
    /// namespace `ocx_util::env::is_reserved_ocx_key` reserves.
    ///
    /// `OCX_NO_VERIFY` turns off the policy-gated auto-verify on install/pull
    /// and is forwarded to every child ocx; `OCX_IDENTITY_TOKEN` is a bearer
    /// credential. Either one declarable from a project file would let a
    /// repository silently disable signature verification for everyone who runs
    /// a tool out of it.
    ///
    /// Built from the `keys::` constants rather than string literals: the gate
    /// matches on the `OCX_` prefix, so respelling either variable outside that
    /// prefix moves it out of the gate's reach without touching the gate. That
    /// is the failure this test exists to catch, alongside the gate itself
    /// being weakened.
    #[test]
    fn project_env_cannot_declare_trust_sensitive_ocx_keys() {
        for key in [
            ocx_config::env::keys::OCX_NO_VERIFY,
            ocx_config::env::keys::OCX_IDENTITY_TOKEN,
        ] {
            for scope in ["env", "group.ci.env"] {
                let toml_str = format!("[{scope}]\n{key} = \"1\"\n");
                let Err(err) = ProjectConfig::from_toml_str(&toml_str) else {
                    panic!("[{scope}] must reject the reserved key {key}");
                };
                assert_eq!(
                    <ocx_project::Error as ClassifyExitCode>::classify(&err),
                    Some(ExitCode::ConfigError),
                    "a reserved-key declaration is a config fault (exit 78)"
                );

                #[allow(irrefutable_let_patterns)]
                let ocx_project::Error::Project(project_error) = err else {
                    panic!("expected Error::Project");
                };
                let ProjectErrorKind::EnvReservedKey {
                    scope: reported_scope,
                    key: reported_key,
                } = &project_error.kind
                else {
                    panic!("expected EnvReservedKey, got {:?}", project_error.kind);
                };
                assert_eq!(reported_scope, scope, "the error must name the table to edit");
                assert_eq!(reported_key, key);
            }
        }
    }

    /// Refuse `toml`, returning the structured error together with the exit
    /// code the CLI derives from it.
    ///
    /// Variant and exit code travel together at every call site below: a
    /// mis-classified `ClassifyExitCode` arm hands the user exit 70 with a
    /// perfectly correct message, and a test asserting only the variant stays
    /// green straight through it.
    fn refuse_ocx_toml(toml: &str) -> (ocx_project::error::ProjectError, Option<ExitCode>) {
        let err = ProjectConfig::from_toml_str(toml).expect_err("this ocx.toml must be refused");
        let code = ClassifyExitCode::classify(&err);
        #[allow(irrefutable_let_patterns)]
        let ocx_project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        (pe, code)
    }

    /// C-012 (E49): the global `$OCX_HOME/ocx.toml` is read by the same
    /// loader, so it gets the same rules and the same exit 78.
    ///
    /// `ProjectConfig::from_path` is the entry point both tiers use
    /// (`command/toolchain_env.rs` reads the global file through it), and the
    /// parser is path-blind by construction — this asserts the loader itself
    /// applies C-012 rather than only the string constructor the tests above
    /// use.
    #[tokio::test]
    async fn c012_the_global_ocx_toml_is_parsed_by_the_same_loader_with_the_same_refusal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.toml");

        tokio::fs::write(&path, "activate = \"always\"\n")
            .await
            .expect("write global ocx.toml");
        let err = ProjectConfig::from_path(&path)
            .await
            .expect_err("an unknown `activate` must be refused wherever the file lives");
        assert_eq!(
            ClassifyExitCode::classify(&err),
            Some(ExitCode::ConfigError),
            "the global tier refuses at the same exit 78"
        );

        tokio::fs::write(&path, "activate = \"bin\"\npinned = true\n")
            .await
            .expect("write global ocx.toml");
        let config = ProjectConfig::from_path(&path).await.expect("valid keys parse");
        assert_eq!(config.activate, Some(ActivateMode::Bin));
        assert_eq!(config.pinned, Some(true));
    }

    // ── moved from ocx_project::error with the impl ──

    // ── moved from ocx_project::resolve with the impl ──

    // ── moved from ocx_project::config with the impl ──

    /// C-014 (E13) — the trap. A BARE `python3.13 = "ocx.sh/py:1"` never
    /// reaches the validator: TOML reads a bare dotted key as nesting, so the
    /// value deserialises as `{tools: {python3: {13: …}}}` and
    /// `RawProjectConfig.tools: BTreeMap<String, String>` refuses a table
    /// value on the FIRST pass.
    ///
    /// Its quoted twin `"python3.13"` is the only spelling that reaches C-014,
    /// and it is admitted (E14) — the ADR's own motivating example. Both halves
    /// are asserted here so a reader does not conclude from the refusal that
    /// dots are illegal.
    #[test]
    fn c014_a_bare_dotted_tools_key_is_toml_nesting_and_never_reaches_the_validator() {
        let (pe, code) = refuse_ocx_toml("[tools]\npython3.13 = \"ocx.sh/py:1\"\n");
        assert!(
            matches!(&pe.kind, ProjectErrorKind::TomlParse(_)),
            "a bare dotted key is a TOML shape fault, not a charset fault; got {:?}",
            pe.kind
        );
        assert_eq!(code, Some(ExitCode::ConfigError), "still exit 78");

        let config = ProjectConfig::from_toml_str("[tools]\n\"python3.13\" = \"ocx.sh/py:1\"\n")
            .expect("the quoted spelling is the ADR's own motivating example and must parse");
        assert!(
            config.tools.contains_key("python3.13"),
            "the quoted key must land verbatim; got {:?}",
            config.tools.keys().collect::<Vec<_>>()
        );
    }

    /// C-014 / item 17: the validator fires at ALL THREE declaration sites,
    /// and each refusal names its own scope.
    ///
    /// One validator, three call sites, is the contract — so a wiring that
    /// covered only `[tools]` would leave a group name unchecked, and a group
    /// name is a path component exactly as a tool name is.
    #[test]
    fn c014_the_charset_fires_at_all_three_declaration_sites() {
        let cases = [
            ("tools", "[tools]\n\"a b\" = \"ocx.sh/x:1\"\n"),
            ("group", "[group.\"a b\"]\n"),
            ("group.ci.tools", "[group.ci.tools]\n\"a b\" = \"ocx.sh/x:1\"\n"),
        ];
        for (expected_scope, toml) in cases {
            let (pe, code) = refuse_ocx_toml(toml);
            let ProjectErrorKind::InvalidToolchainNameCharset { scope, name } = &pe.kind else {
                panic!(
                    "expected InvalidToolchainNameCharset at {expected_scope}; got {:?}",
                    pe.kind
                );
            };
            assert_eq!(scope, expected_scope, "the refusal must name the site it fired at");
            assert_eq!(name, "a b", "the refusal must echo the offending name");
            assert_eq!(code, Some(ExitCode::ConfigError), "C-014 is exit 78");
        }
    }

    /// C-015 / RUL-1 (E28–E30): `[group.default]` and `[group.all]` are
    /// refused in EVERY ASCII case, not only lowercase.
    ///
    /// `[group.Default]` silently coexisting with the `default` group derived
    /// from `[tools]` is the collision the reservation exists to stop — and a
    /// byte-exact `contains_key` lets it through. Lowercase rows are the
    /// regression control on the shipped behaviour.
    ///
    /// NOTE for the reader of a red: this case returns from the `default` /
    /// `all` guards, which sit BEFORE `validate_toolchain_name`. It therefore
    /// passes against the stub, and its red is the fold mutation, not an
    /// `unimplemented!()` panic.
    #[test]
    fn c015_the_reserved_group_keywords_are_refused_in_every_ascii_case() {
        for spelling in ["default", "Default", "DEFAULT", "dEfAuLt", "all", "All", "ALL", "aLl"] {
            let toml = format!("[group.{spelling}.tools]\ncmake = \"ocx.sh/cmake:3.28\"\n");
            let (pe, code) = refuse_ocx_toml(&toml);
            let ProjectErrorKind::ReservedGroupName { name, hint } = &pe.kind else {
                panic!("expected ReservedGroupName for [group.{spelling}]; got {:?}", pe.kind);
            };
            assert_eq!(
                name, spelling,
                "the message must echo the user's own spelling so they can find the line"
            );
            assert!(!hint.is_empty(), "the shipped per-keyword hint must survive the fold");
            assert_eq!(code, Some(ExitCode::ConfigError), "still exit 78");
        }
    }

    /// C-015 / RUL-1, regression control: the two lowercase spellings keep
    /// their SHIPPED hint text, byte-unchanged.
    ///
    /// The fold widens which names are refused; it must not rewrite what the
    /// user is told. Each keyword keeps its own hint — `default` points at
    /// `[tools]`, `all` explains the expansion keyword — so a fold implemented
    /// with one shared message would red here.
    #[test]
    fn c015_the_folded_refusal_keeps_each_keywords_own_shipped_hint() {
        let (default_err, _) = refuse_ocx_toml("[group.Default]\n");
        let rendered = format!("{:#}", default_err.kind);
        assert!(
            rendered.contains("[group.Default] is reserved"),
            "the message must name the group as written; got {rendered:?}"
        );
        assert!(
            rendered.contains("put tools in the top-level [tools] table"),
            "`default` keeps its own shipped hint; got {rendered:?}"
        );

        let (all_err, _) = refuse_ocx_toml("[group.ALL]\n");
        let rendered = format!("{:#}", all_err.kind);
        assert!(
            rendered.contains("[group.ALL] is reserved"),
            "the message must name the group as written; got {rendered:?}"
        );
        assert!(
            rendered.contains("reserved keyword"),
            "`all` keeps its own shipped hint; got {rendered:?}"
        );
    }

    /// Ordering (E37): with an off-charset group and `[group.default]` in one
    /// file, the shipped `default` guard reports first — it runs before the
    /// group loop that validates names.
    ///
    /// The competing name is `"a b"`, not the former `bin`: C-073 makes
    /// `[group.bin]` legal, so a `bin` probe would leave one refusal in the
    /// file and stop testing the ordering it names.
    #[test]
    fn ordering_the_reserved_default_guard_precedes_the_group_name_validator() {
        let (pe, _) = refuse_ocx_toml("[group.\"a b\"]\n\n[group.default]\n");
        assert!(
            matches!(&pe.kind, ProjectErrorKind::ReservedGroupName { name, .. } if name == "default"),
            "the `default` guard runs before the group-name validator; got {:?}",
            pe.kind
        );
    }

    /// Ordering (E38): with an off-charset `[tools]` key and `[group.bin]` in
    /// one file, the `[tools]` refusal reports first — `parse_tool_map` runs
    /// before the group loop.
    #[test]
    fn ordering_the_tools_table_is_validated_before_any_group() {
        let (pe, _) = refuse_ocx_toml("[tools]\n\"a b\" = \"ocx.sh/x:1\"\n\n[group.\"c d\"]\n");
        let ProjectErrorKind::InvalidToolchainNameCharset { scope, name } = &pe.kind else {
            panic!("[tools] is walked before the groups; got {:?}", pe.kind);
        };
        assert_eq!(scope, "tools");
        assert_eq!(name, "a b");
    }

    /// C-012 + C-014 (E52): the two new keys do not short-circuit the name
    /// validator — a file that declares both AND an off-charset tool key is
    /// still refused.
    ///
    /// The probe key is `"a b"`, not the former `bin`: C-073 deleted that
    /// refusal, so a `bin` probe would assert nothing about the validator
    /// running at all.
    #[test]
    fn c012_activate_and_pinned_do_not_short_circuit_the_name_validator() {
        let (pe, code) = refuse_ocx_toml("activate = \"bin\"\npinned = true\n\n[tools]\n\"a b\" = \"ocx.sh/x:1\"\n");
        assert!(
            matches!(&pe.kind, ProjectErrorKind::InvalidToolchainNameCharset { scope, name }
                if scope == "tools" && name == "a b"),
            "the name validator still runs beside the two new keys; got {:?}",
            pe.kind
        );
        assert_eq!(code, Some(ExitCode::ConfigError));
    }

    // ── moved from ocx_project::error with the impl ──

    /// `ManifestEditDiverged` (the format-preserving writer produced a
    /// document that no longer describes the staged configuration) is a
    /// fail-closed writer-side guard, not something the user can fix by
    /// editing `ocx.toml` — pin it to `Failure` (1), distinct from the
    /// `ConfigError` (78) class above.
    #[test]
    fn manifest_edit_diverged_classifies_as_failure() {
        let err = ocx_project::Error::Project(ocx_project::ProjectError::new(
            PathBuf::from("/tmp/ocx.toml"),
            ProjectErrorKind::ManifestEditDiverged,
        ));
        assert_eq!(err.classify(), Some(ExitCode::Failure));
    }

    /// `ManifestEditParse` (the on-disk `ocx.toml` failed to parse as an
    /// editable document during a format-preserving write-back) is a config
    /// fault the user can edit their way out of — pin it to `ConfigError`
    /// (78), same class as a `TomlParse` failure.
    #[test]
    fn manifest_edit_parse_classifies_as_config_error() {
        let err = ocx_project::Error::Project(ocx_project::ProjectError::new(
            PathBuf::from("/tmp/ocx.toml"),
            ProjectErrorKind::ManifestEditParse("[tools\n".parse::<toml_edit::DocumentMut>().unwrap_err()),
        ));
        assert_eq!(err.classify(), Some(ExitCode::ConfigError));
    }
}
