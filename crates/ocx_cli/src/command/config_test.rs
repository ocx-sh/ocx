// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::managed_config::{preview_managed_config, read_candidate_payload};

use crate::api::data::config_test::ConfigTestData;

/// Arguments for `ocx config test`.
#[derive(Parser)]
pub struct ConfigTestArgs {
    /// The config file to check.
    config: std::path::PathBuf,
}

impl ConfigTestArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // The same bounded read `config push` does: a FIFO or a device named
        // as the candidate is refused before the open, an oversize file at
        // the size gate rather than after being read whole.
        let bytes = read_candidate_payload(&self.config).await?;

        // The candidate takes the managed tier's place in this machine's own
        // fold: base tiers, then the candidate, then the explicit overlay.
        // Same order adoption produces, so the report is the configuration the
        // machine would really resolve — overlay included.
        let preview = preview_managed_config(&bytes, context.config_base().clone(), context.config_overlay())?;

        // The gates every ocx invocation applies to its own config, run against
        // the effective merge — a payload that parses can still be one no
        // machine can start under (a plain-HTTP mirror, an empty patch
        // registry). Catching that here is the point of previewing.
        // The plain-HTTP set comes from the CANDIDATE's own `[registries]`
        // entries unioned with this machine's env — a payload that declares
        // `insecure = true` for its own mirror host must pass its own gate.
        let insecure_hosts =
            ocx_config::insecure::insecure_hosts(&preview.effective, &ocx_config::env::insecure_registries());
        ocx_config::mirror::resolve_mirror_map(&preview.effective, ocx_config::env::mirrors()?, &insecure_hosts)?;
        // Reported, not just consumed: a `[registries.<name>]` key that is not
        // an exact `host[:port]` grants nothing and fails much later as an
        // opaque TLS error, so the one command whose job is previewing a
        // config has to answer "did my entry take effect?".
        // Same config-tier-then-env precedence `Context::try_init` uses, so the
        // report matches what the machine actually resolves.
        let patches = match ocx_config::patch::resolve_patch_config(&preview.effective)? {
            Some(resolved) => Some(resolved),
            None => ocx_config::patch::patches_from_env()?,
        };
        // The machine's own tier, never the candidate's — a payload declaring
        // `[managed]` was already refused above.
        let managed =
            ocx_config::managed::resolve_managed_target(context.config(), context.managed_config_env_override())?;

        context.api().report(&ConfigTestData::new(
            &self.config,
            preview,
            patches,
            managed.as_ref(),
            insecure_hosts,
        ))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// `ocx config test` driven in-process through the seam.
#[cfg(test)]
mod seam {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use crate::app::seam::{Environment, run};

    /// A real self-signed CA (the acceptance stack's Fulcio root): the
    /// positive case needs material the certificate parser accepts, not a
    /// placeholder string.
    const CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBxjCCAWugAwIBAgIURyQ7Q7tYlF6VJqChHK15YYsx6rowCgYIKoZIzj0EAwIw
MDERMA8GA1UECgwIb2N4IHRlc3QxGzAZBgNVBAMMEm9jeCB0ZXN0IGZ1bGNpbyBD
QTAeFw0yNjA4MTcyMjQ3NThaFw0zNjA4MTQyMjQ3NThaMDAxETAPBgNVBAoMCG9j
eCB0ZXN0MRswGQYDVQQDDBJvY3ggdGVzdCBmdWxjaW8gQ0EwWTATBgcqhkjOPQIB
BggqhkjOPQMBBwNCAASBW+lKkSOzTTd01x+u6hpjZ0OMSskAwa7tDmjpTVaDRpNI
gpQw2jrEJn+l3ZrgkQ/gPKzNThqM/BoN612vMlbyo2MwYTAdBgNVHQ4EFgQUZIaz
LdRfbq9gubTgszBLOl3+UUMwHwYDVR0jBBgwFoAUZIazLdRfbq9gubTgszBLOl3+
UUMwDwYDVR0TAQH/BAUwAwEB/zAOBgNVHQ8BAf8EBAMCAQYwCgYIKoZIzj0EAwID
SQAwRgIhAK2LeZQAzRi2rTlJPPij5DI4daYATctFhMTPCfhTzf3JAiEAwYycMsOd
tD+gerlwfxS92f+5NXGZz7kM/FSrXpr8Up4=
-----END CERTIFICATE-----
";

    const PATCHES_ENV: &str =
        r#"{"registry":"env.example.com/patches","path_template":"{registry}/{repository}","required":true}"#;

    struct Outcome {
        code: ExitCode,
        out: String,
        err: String,
    }

    impl Outcome {
        fn json(&self) -> serde_json::Value {
            assert_eq!(self.code, ExitCode::SUCCESS, "config test must exit 0: {}", self.err);
            serde_json::from_str(&self.out).expect("config test prints one JSON document")
        }
    }

    /// An isolated `$OCX_HOME` plus a scratch directory for candidates.
    struct Fixture {
        _root: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root_dir = tempfile::tempdir().expect("tempdir");
            let root = dunce::canonicalize(root_dir.path()).expect("canonical tempdir");
            let home = root.join("ocx-home");
            std::fs::create_dir_all(&home).expect("mkdir OCX_HOME");
            Self {
                _root: root_dir,
                root,
                home,
            }
        }

        /// `$OCX_HOME/config.toml` — the machine tier the candidate merges onto.
        fn home_config(&self, content: &str) -> PathBuf {
            let path = self.home.join("config.toml");
            std::fs::write(&path, content).expect("write $OCX_HOME/config.toml");
            path
        }

        /// A candidate payload, deliberately NOT named `config.toml`.
        fn candidate(&self, content: &str) -> PathBuf {
            let path = self.root.join("candidate.toml");
            std::fs::write(&path, content).expect("write candidate");
            path
        }

        async fn config_test(&self, json: bool, candidate: &Path, vars: &[(&str, &str)]) -> Outcome {
            let mut env: BTreeMap<OsString, OsString> =
                [(OsString::from("OCX_HOME"), self.home.clone().into_os_string())].into();
            for (key, value) in vars {
                env.insert(OsString::from(key), OsString::from(value));
            }
            let environment = Environment {
                vars: env,
                cwd: self.root.clone(),
            };
            let mut argv: Vec<OsString> = vec!["ocx".into()];
            if json {
                argv.extend(["--format".into(), "json".into()]);
            }
            argv.extend(["config".into(), "test".into(), candidate.as_os_str().to_owned()]);
            let (mut out, mut err) = (Vec::new(), Vec::new());
            // Boxed: the whole CLI's future is large in a debug build, and nested
            // under a test's own future it overflows libtest's 2 MiB thread stack.
            let code = Box::pin(run(&argv, &environment, &mut out, &mut err)).await;
            Outcome {
                code,
                out: String::from_utf8(out).expect("stdout is UTF-8"),
                err: String::from_utf8(err).expect("stderr is UTF-8"),
            }
        }

        async fn report(&self, candidate: &Path, vars: &[(&str, &str)]) -> serde_json::Value {
            self.config_test(true, candidate, vars).await.json()
        }
    }

    fn strings(value: &serde_json::Value) -> Vec<&str> {
        value
            .as_array()
            .unwrap_or_else(|| panic!("expected an array, got {value}"))
            .iter()
            .map(|item| item.as_str().expect("array of strings"))
            .collect()
    }

    // ── Effective-merge preview ──────────────────────────────────────────

    /// The report is the EFFECTIVE config: candidate values appear, and
    /// machine values the candidate does not override survive.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_previews_merge_of_candidate_onto_machine_tiers
    async fn previews_merge_of_candidate_onto_machine_tiers() {
        let fixture = Fixture::new();
        fixture.home_config(
            "[registry]\ndefault = \"machine.example\"\n\n[mirrors.\"ghcr.io\"]\nregistry = \"https://mirror.machine/ghcr\"\n",
        );
        let candidate = fixture.candidate(
            "[registries.\"corp.example.com\"]\nindex = \"https://index.corp.example.com\"\n\n\
             [mirrors.\"quay.io\"]\nregistry = \"https://mirror.corp/quay\"\n\n\
             [patches]\nregistry = \"corp.example.com/ocx-patches\"\nrequired = false\n",
        );

        let report = fixture.report(&candidate, &[]).await;

        assert_eq!(report["valid"], true);
        assert_eq!(
            Path::new(report["candidate"].as_str().expect("candidate is a string")).file_name(),
            Some(std::ffi::OsStr::new("candidate.toml"))
        );
        assert_eq!(
            report["registry_default"], "machine.example",
            "a machine value the candidate does not set must survive the merge"
        );
        assert!(
            strings(&report["registries"]).contains(&"corp.example.com"),
            "the candidate's registry entry must appear: {report}"
        );
        let mirrors = strings(&report["mirrors"]);
        assert!(
            mirrors.contains(&"ghcr.io") && mirrors.contains(&"quay.io"),
            "both the machine mirror and the candidate mirror must appear in the effective view: {mirrors:?}"
        );
        assert_eq!(report["patches"]["registry"], "corp.example.com/ocx-patches");
        assert_eq!(report["patches"]["required"], false);
        assert_eq!(
            report["patches"]["path_template"], "{registry}/{repository}",
            "an omitted path template must be reported as the default that would apply"
        );
    }

    /// Where candidate and machine tier both set a value, the candidate wins.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_candidate_overrides_machine_value
    async fn candidate_overrides_machine_value() {
        let fixture = Fixture::new();
        fixture.home_config("[registry]\ndefault = \"machine.example\"\n");
        let candidate = fixture.candidate("[registry]\ndefault = \"corp.example.com\"\n");

        let report = fixture.report(&candidate, &[]).await;

        assert_eq!(report["registry_default"], "corp.example.com");
    }

    /// An explicit `OCX_CONFIG` overlay outranks the candidate, as it outranks
    /// an adopted payload. Both keys are set on both sides.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_config_overlay_outranks_the_candidate
    async fn config_overlay_outranks_the_candidate() {
        let fixture = Fixture::new();
        fixture.home_config("[registry]\ndefault = \"machine.example\"\n");
        let overlay = fixture.root.join("overlay.toml");
        std::fs::write(
            &overlay,
            "[registry]\ndefault = \"overlay.example\"\n\n[patches]\nregistry = \"overlay.example/patches\"\n",
        )
        .expect("write overlay");
        let candidate = fixture.candidate(
            "[registry]\ndefault = \"corp.example.com\"\n\n[patches]\nregistry = \"corp.example.com/patches\"\n",
        );

        let report = fixture
            .report(&candidate, &[("OCX_CONFIG", overlay.to_str().expect("UTF-8 path"))])
            .await;

        assert_eq!(
            report["registry_default"], "overlay.example",
            "an explicit overlay outranks an adopted payload, so it must outrank the candidate"
        );
        assert_eq!(report["patches"]["registry"], "overlay.example/patches");
    }

    /// The overlay only outranks keys it actually sets.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_candidate_still_wins_where_the_overlay_is_silent
    async fn candidate_still_wins_where_the_overlay_is_silent() {
        let fixture = Fixture::new();
        let overlay = fixture.root.join("overlay.toml");
        std::fs::write(&overlay, "[registry]\ndefault = \"overlay.example\"\n").expect("write overlay");
        let candidate = fixture.candidate("[patches]\nregistry = \"corp.example.com/patches\"\n");

        let report = fixture
            .report(&candidate, &[("OCX_CONFIG", overlay.to_str().expect("UTF-8 path"))])
            .await;

        assert_eq!(report["registry_default"], "overlay.example");
        assert_eq!(report["patches"]["registry"], "corp.example.com/patches");
    }

    /// The reported tier posture is the machine's own; the tier defaults to
    /// fail-closed.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_reports_the_machines_managed_posture
    async fn reports_the_machines_managed_posture() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[registry]\ndefault = \"corp.example.com\"\n");

        let report = fixture
            .report(
                &candidate,
                &[("OCX_MANAGED_CONFIG", "corp.example.com/ocx-config:user")],
            )
            .await;

        assert_eq!(report["managed"]["source"], "corp.example.com/ocx-config:user");
        assert_eq!(report["managed"]["required"], true, "the tier defaults to fail-closed");
    }

    /// `required` is read from the machine's seed, not assumed.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_reports_an_opted_out_managed_posture
    async fn reports_an_opted_out_managed_posture() {
        let fixture = Fixture::new();
        fixture.home_config("[managed]\nsource = \"corp.example.com/ocx-config:user\"\nrequired = false\n");
        let candidate = fixture.candidate("[registry]\ndefault = \"corp.example.com\"\n");

        let report = fixture.report(&candidate, &[]).await;

        assert_eq!(report["managed"]["source"], "corp.example.com/ocx-config:user");
        assert_eq!(report["managed"]["required"], false);
    }

    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_reports_no_managed_tier_when_unconfigured
    async fn reports_no_managed_tier_when_unconfigured() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[registry]\ndefault = \"corp.example.com\"\n");

        let report = fixture.report(&candidate, &[]).await;

        assert!(report["managed"].is_null(), "{report}");
    }

    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_plain_output_is_a_field_value_table
    async fn plain_output_is_a_field_value_table() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[registry]\ndefault = \"corp.example.com\"\n");

        let result = fixture.config_test(false, &candidate, &[]).await;

        assert_eq!(result.code, ExitCode::SUCCESS, "{}", result.err);
        assert!(
            result.out.contains("Field") && result.out.contains("Value"),
            "{}",
            result.out
        );
        assert!(result.out.contains("corp.example.com"), "{}", result.out);
        assert!(
            !result.out.contains("Valid"),
            "reaching plain output at all is the verdict; a constant row is noise"
        );
    }

    /// Multi-valued fields label every row.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_plain_repeats_the_field_name_on_every_row
    async fn plain_repeats_the_field_name_on_every_row() {
        let fixture = Fixture::new();
        let candidate =
            fixture.candidate("[registries.\"corp.example.com\"]\nindex = \"https://index.corp.example.com\"\n");

        let result = fixture.config_test(false, &candidate, &[]).await;

        let registry_rows = result.out.lines().filter(|line| line.starts_with("Registries")).count();
        assert!(
            registry_rows >= 2,
            "both the candidate entry and the built-in ocx.sh entry must be labelled: {}",
            result.out
        );
    }

    // ── Rejections (exit 78) — shared with `ocx config push` ─────────────

    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_rejects_managed_section_exit_78
    async fn rejects_managed_section_exit_78() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[managed]\nsource = \"corp.example.com/ocx-config:user\"\n");

        let result = fixture.config_test(false, &candidate, &[]).await;

        assert_eq!(result.code, ExitCode::from(78), "{}", result.err);
        assert!(result.err.contains("[managed]"), "{}", result.err);
    }

    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_rejects_invalid_toml_exit_78
    async fn rejects_invalid_toml_exit_78() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("not = [valid\n");

        let result = fixture.config_test(false, &candidate, &[]).await;

        assert_eq!(result.code, ExitCode::from(78), "{}", result.err);
    }

    /// The 64 KiB cap is the one the consumer-side fetch enforces.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_rejects_oversize_payload_exit_78
    async fn rejects_oversize_payload_exit_78() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate(&"# padding\n".repeat(7_000));

        let result = fixture.config_test(false, &candidate, &[]).await;

        assert_eq!(result.code, ExitCode::from(78), "{}", result.err);
    }

    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_missing_candidate_exits_79
    async fn missing_candidate_exits_79() {
        let fixture = Fixture::new();

        let result = fixture.config_test(false, &fixture.root.join("absent.toml"), &[]).await;

        assert_eq!(result.code, ExitCode::from(79), "{}", result.err);
    }

    /// `extra_ca_certs` and `extra_ca_certs_pem` together are ambiguous. The
    /// path named here exists, so the refusal can only come from the
    /// ambiguity.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_refuses_both_extra_ca_certs_keys_78
    async fn refuses_both_extra_ca_certs_keys_78() {
        let fixture = Fixture::new();
        std::fs::write(fixture.root.join("corp-ca.pem"), CA_PEM).expect("write CA");
        let candidate = fixture.candidate(&format!(
            "extra_ca_certs = \"corp-ca.pem\"\nextra_ca_certs_pem = '''\n{CA_PEM}'''\n"
        ));

        let result = fixture.config_test(false, &candidate, &[]).await;

        assert_eq!(result.code, ExitCode::from(78), "{}", result.err);
        assert!(
            result.err.contains("extra_ca_certs"),
            "the refusal must name the ambiguous keys: {}",
            result.err
        );
    }

    /// A clean `extra_ca_certs_pem` payload is valid and names no unknown key.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_accepts_pem_payload
    async fn accepts_pem_payload() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate(&format!("extra_ca_certs_pem = '''\n{CA_PEM}'''\n"));

        let plain = fixture.config_test(false, &candidate, &[]).await;
        assert_eq!(plain.code, ExitCode::SUCCESS, "{}", plain.err);

        let report = fixture.report(&candidate, &[]).await;
        assert_eq!(report["valid"], true);
        assert_eq!(
            report["unknown_keys"],
            serde_json::json!([]),
            "a key this ocx understands must not be reported as unknown"
        );
    }

    // ── Resolution gates — a payload can parse and still be unusable ─────

    /// A plain-HTTP mirror without the host being allowed: no machine can
    /// start under it.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_rejects_plain_http_mirror_exit_78
    async fn rejects_plain_http_mirror_exit_78() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[mirrors.\"ghcr.io\"]\nregistry = \"http://mirror.corp\"\n");

        let result = fixture
            .config_test(false, &candidate, &[("OCX_INSECURE_REGISTRIES", "")])
            .await;

        assert_eq!(result.code, ExitCode::from(78), "{}", result.err);
        assert!(
            result.err.contains("OCX_INSECURE_REGISTRIES"),
            "the error must name the plain-HTTP gate: {:?}",
            result.err
        );
    }

    /// The gate's other side: allowed by the environment, the same payload
    /// passes.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_allowed_plain_http_mirror_passes
    async fn allowed_plain_http_mirror_passes() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[mirrors.\"ghcr.io\"]\nregistry = \"http://mirror.corp\"\n");

        let report = fixture
            .report(&candidate, &[("OCX_INSECURE_REGISTRIES", "mirror.corp")])
            .await;

        assert!(strings(&report["mirrors"]).contains(&"ghcr.io"), "{report}");
    }

    /// The candidate's own `[registries."<host>"] insecure = true` licenses
    /// its plain-HTTP mirror with the environment grant emptied.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_plain_http_mirror_allowed_by_candidate_registries_entry
    async fn plain_http_mirror_allowed_by_candidate_registries_entry() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate(
            "[mirrors.\"ghcr.io\"]\nregistry = \"http://mirror.corp\"\n[registries.\"mirror.corp\"]\ninsecure = true\n",
        );

        let report = fixture.report(&candidate, &[("OCX_INSECURE_REGISTRIES", "")]).await;

        assert!(strings(&report["mirrors"]).contains(&"ghcr.io"), "{report}");
        assert_eq!(report["plain_http"], serde_json::json!(["mirror.corp"]));
    }

    /// An empty `[patches] registry` is refused as a config error.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_rejects_empty_patch_registry_exit_78
    async fn rejects_empty_patch_registry_exit_78() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[patches]\nregistry = \"\"\n");

        let result = fixture.config_test(false, &candidate, &[]).await;

        assert_eq!(
            result.code,
            ExitCode::from(78),
            "a malformed [patches] tier must classify as a config error: {}",
            result.err
        );
    }

    // ── `[patches]` precedence ────────────────────────────────────────────

    /// No `[patches]` anywhere in config: the `OCX_PATCHES` tier is reported.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_falls_back_to_the_env_patch_tier
    async fn falls_back_to_the_env_patch_tier() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[registry]\ndefault = \"corp.example.com\"\n");

        let report = fixture.report(&candidate, &[("OCX_PATCHES", PATCHES_ENV)]).await;

        assert_eq!(report["patches"]["registry"], "env.example.com/patches");
    }

    /// Config tier beats the env tier.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_candidate_patches_outrank_the_env_tier
    async fn candidate_patches_outrank_the_env_tier() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[patches]\nregistry = \"corp.example.com/patches\"\n");

        let report = fixture.report(&candidate, &[("OCX_PATCHES", PATCHES_ENV)]).await;

        assert_eq!(report["patches"]["registry"], "corp.example.com/patches");
    }

    // ── Unknown-key warnings (exit 0) ─────────────────────────────────────

    /// A typo'd section or key is surfaced as a warning, never a failure.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_reports_unknown_keys_and_still_exits_zero
    async fn reports_unknown_keys_and_still_exits_zero() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate(
            "[patchs]\nregistry = \"corp.example.com/ocx-patches\"\n\n[registry]\ndefalt = \"corp.example.com\"\n",
        );

        let plain = fixture.config_test(false, &candidate, &[]).await;
        assert_eq!(plain.code, ExitCode::SUCCESS, "{}", plain.err);

        let report = fixture.report(&candidate, &[]).await;
        assert_eq!(report["valid"], true);
        let unknown = strings(&report["unknown_keys"]);
        assert!(
            unknown.contains(&"patchs"),
            "typo'd section must be listed: {unknown:?}"
        );
        assert!(
            unknown.contains(&"registry.defalt"),
            "a typo'd key must be listed by its full path: {unknown:?}"
        );
        assert!(
            report["registry_default"].is_null(),
            "the typo means nothing was actually set - the preview must not pretend otherwise"
        );
    }

    /// KNOWN LIMITATION PIN, not a desired behaviour: keys inside a
    /// `[mirrors."<host>"]` entry are not checked.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_does_not_check_keys_inside_a_mirrors_entry
    async fn does_not_check_keys_inside_a_mirrors_entry() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate(
            "[mirrors.\"ghcr.io\"]\nregistry = \"https://mirror.corp/ghcr\"\nregsitry = \"https://typo.example\"\n",
        );

        let report = fixture.report(&candidate, &[]).await;

        assert_eq!(
            report["unknown_keys"],
            serde_json::json!([]),
            "if this now reports the typo, the limitation is fixed - update the help text, \
             the unknown_keys field doc and this test"
        );
        assert!(
            strings(&report["mirrors"]).contains(&"ghcr.io"),
            "the correctly spelled role must still take effect"
        );
    }

    /// The warning list is empty for a payload with no typos.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_clean_payload_reports_no_unknown_keys
    async fn clean_payload_reports_no_unknown_keys() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[registry]\ndefault = \"corp.example.com\"\n");

        let report = fixture.report(&candidate, &[]).await;

        assert_eq!(report["unknown_keys"], serde_json::json!([]));
    }

    /// The same key the ordinary loader stays silent about is surfaced here.
    #[tokio::test]
    // ported-from: test/tests/test_config_test.py::test_config_test_finds_the_key_the_ordinary_command_ignores
    async fn finds_the_key_the_ordinary_command_ignores() {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("[registry]\ndefault = \"machine.example\"\ntimeuot = 30\n");

        let report = fixture.report(&candidate, &[]).await;

        assert!(
            strings(&report["unknown_keys"]).contains(&"registry.timeuot"),
            "the same key the loader ignores must be surfaced here: {report}"
        );
    }
}
