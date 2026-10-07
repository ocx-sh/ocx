// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification tests of the `ocx_project` family. Its types declare their own codes with `#[derive(Classify)]`.

use ocx_project::ProjectErrorKind;

use ocx_package_manager::activation::SessionError;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ocx_exit::{ClassifyExitCode, ExitCode};

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

        let code = ClassifyExitCode::classify(&err);
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
    /// namespace `ocx_env::is_reserved_ocx_key` reserves.
    ///
    /// `OCX_NO_VERIFY` turns off the policy-gated auto-verify on install/pull
    /// and is forwarded to every child ocx; `OCX_IDENTITY_TOKEN` is a bearer
    /// credential. Either one declarable from a project file would let a
    /// repository silently disable signature verification for everyone who runs
    /// a tool out of it.
    ///
    /// Built from the registry's declarations rather than string literals: the gate
    /// matches on the `OCX_` prefix, so respelling either variable outside that
    /// prefix moves it out of the gate's reach without touching the gate. That
    /// is the failure this test exists to catch, alongside the gate itself
    /// being weakened.
    #[test]
    fn project_env_cannot_declare_trust_sensitive_ocx_keys() {
        for key in [
            ocx_env::OCX_NO_VERIFY.name,
            ocx_env::OCX_IDENTITY_TOKEN.declaration().name,
        ] {
            for scope in ["env", "group.ci.env"] {
                let toml_str = format!("[{scope}]\n{key} = \"1\"\n");
                let Err(err) = ProjectConfig::from_toml_str(&toml_str) else {
                    panic!("[{scope}] must reject the reserved key {key}");
                };
                assert_eq!(
                    ClassifyExitCode::classify(&err),
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
        (*pe, code)
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

    /// Reds on: a project slug, delegated or chosen by the cause's type, naming another cause than
    /// the exit code does.
    #[test]
    fn project_details_name_the_cause_that_decides_the_code() {
        use crate::exit::tests::assert_detail;

        let project =
            |kind| ocx_project::Error::from(ocx_project::ProjectError::new(PathBuf::from("/tmp/ocx.toml"), kind));
        assert_detail(
            &project(ProjectErrorKind::ManifestEditDiverged),
            "project_manifest_edit_diverged",
        );
        let unreachable = |source: Box<dyn std::error::Error + Send + Sync>| {
            project(ProjectErrorKind::RegistryUnreachable {
                identifier: Box::new(
                    ocx_oci::PackageRef::parse("registry.example/pkg:1.0").expect("a valid reference"),
                ),
                source,
            })
        };
        let transient =
            ocx_oci::client::error::ClientError::RegistryTransient(Box::new(std::io::Error::other("reset")));
        assert_detail(&unreachable(Box::new(transient)), "registry_transient");
        assert_detail(
            &unreachable(Box::new(std::io::Error::other("refused"))),
            "registry_unreachable",
        );

        let stale = || ocx_project::LockCurrency::Stale {
            lock_path: PathBuf::from("/tmp/ocx.lock"),
        };
        // No ladder rung downcasts `LockCurrency`, so it is checked directly; the wrappers below go through the ladder.
        let missing = || ocx_project::LockCurrency::Missing {
            path: PathBuf::from("/tmp/ocx.lock"),
        };
        assert_eq!(
            crate::exit::detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(&stale())),
            "lock_stale"
        );
        assert_eq!(ClassifyExitCode::classify(&stale()), Some(ExitCode::DataError));
        assert_eq!(
            crate::exit::detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(&missing())),
            "lock_missing"
        );
        assert_eq!(ClassifyExitCode::classify(&missing()), Some(ExitCode::ConfigError));
        assert_detail(&SessionError::Lock(stale()), "lock_stale");
        assert_detail(&SessionError::Lock(missing()), "lock_missing");
    }

    /// `ManifestEditDiverged` (the format-preserving writer produced a
    /// document that no longer describes the staged configuration) is a
    /// fail-closed writer-side guard, not something the user can fix by
    /// editing `ocx.toml` — pin it to `Failure` (1), distinct from the
    /// `ConfigError` (78) class above.
    #[test]
    fn manifest_edit_diverged_classifies_as_failure() {
        let err = ocx_project::Error::from(ocx_project::ProjectError::new(
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
        let err = ocx_project::Error::from(ocx_project::ProjectError::new(
            PathBuf::from("/tmp/ocx.toml"),
            ProjectErrorKind::ManifestEditParse("[tools\n".parse::<toml_edit::DocumentMut>().unwrap_err()),
        ));
        assert_eq!(err.classify(), Some(ExitCode::ConfigError));
    }
}
