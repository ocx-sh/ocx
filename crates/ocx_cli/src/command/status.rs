// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Toolchain-tier `ocx status` command.
//!
//! Reads `ocx.toml` and its sibling `ocx.lock` and reports what they say. No
//! network, no advisory lock, no staleness gate, no object-store probe — the
//! command has to answer on exactly the broken project you reach for it with,
//! so an absent or drifted lock is payload rather than an error.
//!
//! Deliberately NOT routed through `load_project_with_lock`: that helper exits
//! 78 on a missing lock and 65 on a stale one, which are the two states status
//! exists to describe.

use std::process::ExitCode;

use clap::Parser;
use ocx_project::{ProjectConfig, ProjectLock};

use crate::api::data::status::StatusReport;

/// Show what `ocx.toml` and `ocx.lock` declare, without resolving anything.
///
/// Reports every declared group with its bindings and `[env]` table, each
/// binding's locked platform digests, the `[package."<id>"]` settings, and
/// whether the lock is still current for the declaration.
///
/// Offline and read-only: no registry is contacted, nothing is installed, and
/// neither file is written. A missing or stale `ocx.lock` is reported as such
/// and still exits 0 - use `ocx lock --check` for the CI gate that fails on
/// exactly that condition, and `ocx inspect` for the resolved surface (what
/// each binding resolves to on this host, and what it would put on `PATH`).
///
/// Takes no group or name filter: the report is a keyed object a caller can
/// narrow itself, and a filter here would only hide rows rather than change
/// any answer.
///
/// Exits 64 when no `ocx.toml` is in scope.
#[derive(Parser)]
pub struct Status {}

impl Status {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let (config_path, lock_path) = crate::app::project_context::resolve_project_paths(&context, None).await?;

        let config = ProjectConfig::from_path(&config_path).await?;

        // `from_path` yields `None` for an absent lock. A parse failure (an
        // unsupported `lock_version`, a corrupt file) is caught rather than
        // propagated: it is one of the states this command exists to name, and
        // the declaration half of the report is still perfectly readable.
        let lock = match ProjectLock::from_path(&lock_path).await {
            Ok(lock) => Ok(lock),
            Err(error) => Err(format!("{error}")),
        };

        let report = StatusReport::new(
            &config_path,
            &config,
            lock.as_ref().map(Option::as_ref).map_err(String::clone),
        );
        context.api().report(&report)?;

        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `status` takes no selectors - a `-g` or a NAME must be a usage error,
    /// not a silently-ignored argument.
    #[test]
    fn status_rejects_selectors() {
        assert!(Status::try_parse_from(["status", "-g", "ci"]).is_err());
        assert!(Status::try_parse_from(["status", "go-task"]).is_err());
        assert!(Status::try_parse_from(["status"]).is_ok());
    }
}

/// `ocx status` driven in-process through the seam. A project here is written
/// by hand: `ocx.toml` as `ocx add` leaves it, and `ocx.lock` in the V3 shape
/// `ocx lock` writes, recording the declaration hash of the `ocx.toml` it sits
/// beside — which is all `status` reads.
#[cfg(test)]
mod seam {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use ocx_project::{LockCurrency, ProjectConfig, ProjectLock};

    use crate::app::project_context::ProjectContextError;
    use crate::app::seam::{Environment, run};

    const REPO: &str = "wp13-status-probe";
    const REGISTRY: &str = "localhost:5000";

    struct Outcome {
        code: ExitCode,
        out: String,
        err: String,
    }

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

        fn env(&self, cwd: &Path, extra: &[(&str, &Path)]) -> Environment {
            let mut vars: std::collections::BTreeMap<OsString, OsString> =
                [(OsString::from("OCX_HOME"), self.home.clone().into_os_string())].into();
            for (key, value) in extra {
                vars.insert(OsString::from(key), value.as_os_str().to_owned());
            }
            Environment {
                vars,
                cwd: cwd.to_owned(),
            }
        }

        /// A project directory holding `ocx.toml` with `body`.
        fn project(&self, body: &str) -> PathBuf {
            let project = self.root.join("project");
            std::fs::create_dir_all(&project).expect("mkdir project");
            std::fs::write(project.join("ocx.toml"), body).expect("write ocx.toml");
            project
        }
    }

    fn declared_tool() -> String {
        format!("[tools]\n{REPO} = \"{REGISTRY}/{REPO}:1.0.0\"\n")
    }

    fn digest(fill: char) -> String {
        format!("sha256:{}", fill.to_string().repeat(64))
    }

    /// Write `ocx.lock` beside `project/ocx.toml`, current for that file as it
    /// stands now, pinning `REPO` in the default group on `platforms`.
    async fn lock(project: &Path, platforms: &[(&str, char)]) {
        let config = ProjectConfig::from_path(&project.join("ocx.toml"))
            .await
            .expect("ocx.toml parses");
        let pins: String = platforms
            .iter()
            .map(|(platform, fill)| format!("\"{platform}\" = \"{}\"\n", digest(*fill)))
            .collect();
        let text = format!(
            "[metadata]\nlock_version = 3\ndeclaration_hash_version = 1\ndeclaration_hash = \"{}\"\n\
             generated_by = \"ocx 0.3.0\"\ngenerated_at = \"2026-04-19T00:00:00Z\"\n\n\
             [[tool]]\nname = \"{REPO}\"\ngroup = \"default\"\nrepository = \"{REGISTRY}/{REPO}\"\n\n\
             [tool.platforms]\n{pins}",
            config.declaration_hash_cached()
        );
        std::fs::write(project.join("ocx.lock"), text).expect("write ocx.lock");
    }

    async fn drive(argv: &[&str], env: &Environment) -> Outcome {
        let argv: Vec<OsString> = std::iter::once("ocx")
            .chain(argv.iter().copied())
            .map(OsString::from)
            .collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        // Boxed: the whole CLI's future is large in a debug build, and nested
        // under a test's own future it overflows libtest's 2 MiB thread stack.
        let code = Box::pin(run(&argv, env, &mut out, &mut err)).await;
        Outcome {
            code,
            out: String::from_utf8(out).expect("stdout is UTF-8"),
            err: String::from_utf8(err).expect("stderr is UTF-8"),
        }
    }

    /// `ocx --format json status`, which must exit 0.
    async fn status(env: &Environment) -> serde_json::Value {
        let outcome = drive(&["--format", "json", "status"], env).await;
        assert_eq!(outcome.code, ExitCode::SUCCESS, "status must exit 0: {}", outcome.err);
        serde_json::from_str(&outcome.out).expect("status prints one JSON document")
    }

    /// No `ocx.lock` is a state, not a failure: exit 0, `present: false`, every
    /// binding carrying `declared` and no `platforms`.
    #[tokio::test]
    // ported-from: test/tests/test_status.py::test_status_without_lock_reports_declared_only
    async fn status_without_lock_reports_declared_only() {
        let fixture = Fixture::new();
        let project = fixture.project(&declared_tool());

        let data = status(&fixture.env(&project, &[])).await;

        assert_eq!(data["lock"]["present"], false);
        assert!(
            data["lock"].get("current").is_none(),
            "an absent lock cannot be current or stale"
        );
        assert!(
            data["lock"]["declaration_hash_expected"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:")),
            "{data}"
        );
        let tool = &data["groups"]["default"]["tools"][REPO];
        assert!(
            tool["declared"]
                .as_str()
                .is_some_and(|declared| declared.ends_with(&format!("{REPO}:1.0.0"))),
            "{tool}"
        );
        assert!(
            tool.get("platforms").is_none(),
            "unlocked bindings carry no platforms key"
        );
    }

    /// A locked binding carries the FULL platform map, not the host leaf.
    #[tokio::test]
    // ported-from: test/tests/test_status.py::test_status_with_lock_reports_every_platform
    async fn status_with_lock_reports_every_platform() {
        let fixture = Fixture::new();
        let project = fixture.project(&declared_tool());
        lock(&project, &[("linux/amd64", '1'), ("linux/arm64", '2')]).await;

        let data = status(&fixture.env(&project, &[])).await;

        assert_eq!(data["lock"]["present"], true);
        assert_eq!(data["lock"]["current"], true);
        assert_eq!(
            data["lock"]["declaration_hash"],
            data["lock"]["declaration_hash_expected"]
        );
        assert!(
            data["lock"]["generated_by"]
                .as_str()
                .is_some_and(|generated_by| generated_by.starts_with("ocx ")),
            "{data}"
        );
        let platforms = data["groups"]["default"]["tools"][REPO]["platforms"]
            .as_object()
            .expect("a locked binding carries a platforms map");
        assert!(
            platforms.contains_key("linux/amd64") && platforms.contains_key("linux/arm64"),
            "{platforms:?}"
        );
        for digest in platforms.values() {
            assert!(
                digest.as_str().is_some_and(|digest| digest.starts_with("sha256:")),
                "{platforms:?}"
            );
        }
    }

    /// A drifted lock exits 0 with `current: false` and names WHICH binding
    /// drifted — while the staleness gate every other project-tier command runs
    /// refuses the same files with 65.
    ///
    /// Partial companion of `test/tests/test_status.py::test_status_reports_drift_instead_of_refusing`,
    /// not a port: the sibling's 65 is checked through `is_stale` and a hand-built
    /// error, not the gate `ocx pull` runs, so the acceptance case stays (WP-13 audit).
    #[tokio::test]
    async fn status_reports_drift_instead_of_refusing() {
        let fixture = Fixture::new();
        let project = fixture.project(&declared_tool());
        lock(&project, &[("linux/amd64", '1')]).await;
        let config_path = project.join("ocx.toml");
        let declared = std::fs::read_to_string(&config_path).expect("read ocx.toml");
        std::fs::write(
            &config_path,
            format!("{declared}undeclared-in-lock = \"{REGISTRY}/{REPO}:1.0.0\"\n"),
        )
        .expect("declare a second binding");

        // The gate a sibling command (`ocx pull`) routes through refuses outright...
        let config = ProjectConfig::from_path(&config_path).await.expect("ocx.toml parses");
        let lock_path = project.join("ocx.lock");
        let on_disk = ProjectLock::from_path(&lock_path)
            .await
            .expect("ocx.lock parses")
            .expect("ocx.lock present");
        assert!(
            ocx_project::lock::is_stale(&on_disk, &config),
            "the staleness gate must fire for a sibling command"
        );
        let refusal = ProjectContextError::from(LockCurrency::Stale { lock_path });
        assert_eq!(
            crate::exit::classify_error(&refusal),
            ocx_exit::ExitCode::DataError,
            "a sibling command refuses a stale lock with 65"
        );

        // ...while status answers.
        let data = status(&fixture.env(&project, &[])).await;
        assert_eq!(data["lock"]["current"], false);
        assert_ne!(
            data["lock"]["declaration_hash"],
            data["lock"]["declaration_hash_expected"]
        );
        let tools = &data["groups"]["default"]["tools"];
        assert!(
            tools["undeclared-in-lock"].get("platforms").is_none(),
            "the binding added since the last lock is the one without platforms: {tools}"
        );
        assert!(
            tools[REPO].get("platforms").is_some(),
            "the already-locked sibling keeps its pins: {tools}"
        );
    }

    /// A corrupt `ocx.lock` still exits 0, carries the unreadable state as
    /// `error`, and leaves the declaration half of the report intact.
    #[tokio::test]
    // ported-from: test/tests/test_status.py::test_status_reports_unreadable_lock
    async fn status_reports_unreadable_lock() {
        let fixture = Fixture::new();
        let project = fixture.project(&declared_tool());
        std::fs::write(project.join("ocx.lock"), "this is not valid TOML for a lock [[[\n").expect("corrupt ocx.lock");

        let data = status(&fixture.env(&project, &[])).await;

        assert_eq!(data["lock"]["present"], true);
        assert!(
            data["lock"]["error"].as_str().is_some_and(|error| !error.is_empty()),
            "the error key is the unreadable state; there is no separate boolean: {data}"
        );
        assert!(
            data["lock"].get("readable").is_none(),
            "a boolean that is only ever false repeats what `error` already says"
        );
        assert!(
            data["lock"].get("current").is_none(),
            "nothing was parsed, so nothing can be current"
        );
        assert!(
            data["groups"]["default"]["tools"][REPO]["declared"]
                .as_str()
                .is_some_and(|declared| declared.ends_with(&format!("{REPO}:1.0.0"))),
            "the declaration is still readable and must still be reported: {data}"
        );
    }

    /// `[env]` lands in `groups.default.env`, a group's env stays in its own
    /// scope, and a relative `path` value is reported verbatim.
    #[tokio::test]
    // ported-from: test/tests/test_status.py::test_status_reports_env_verbatim_per_scope
    async fn status_reports_env_verbatim_per_scope() {
        let fixture = Fixture::new();
        let project = fixture.project(
            "[tools]\n\n[env]\nCI = \"1\"\nNODE_BIN = { type = \"path\", value = \"node_modules/.bin\" }\n\
             \n[group.lint.env]\nSTRICT = \"yes\"\n",
        );

        let data = status(&fixture.env(&project, &[])).await;

        let default_env = &data["groups"]["default"]["env"];
        assert_eq!(
            default_env["CI"],
            serde_json::json!({"type": "constant", "value": "1"}),
            "{default_env}"
        );
        assert_eq!(
            default_env["NODE_BIN"],
            serde_json::json!({"type": "path", "value": "node_modules/.bin"}),
            "a relative path value must stay verbatim - anchoring it is composition"
        );
        assert_eq!(
            data["groups"]["lint"]["env"],
            serde_json::json!({"STRICT": {"type": "constant", "value": "yes"}})
        );
        assert!(
            data["groups"]["lint"]["env"].get("CI").is_none(),
            "env is per-scope, never merged"
        );
        assert!(
            data.get("env").is_none(),
            "there is no top-level env - root [env] IS group default's"
        );
    }

    /// `[package."<id>"]` is reported even though it is excluded from
    /// `declaration_hash`.
    #[tokio::test]
    // ported-from: test/tests/test_status.py::test_status_reports_package_settings
    async fn status_reports_package_settings() {
        let fixture = Fixture::new();
        let project = fixture.project("[tools]\n");
        let env = fixture.env(&project, &[]);
        let before = status(&env).await["lock"]["declaration_hash_expected"].clone();
        std::fs::write(
            project.join("ocx.toml"),
            "[tools]\n\n[package.\"ocx.sh/example:1\"]\nno-patches = true\n",
        )
        .expect("add package settings");

        let data = status(&env).await;

        assert_eq!(
            data["package_settings"],
            serde_json::json!({"ocx.sh/example:1": {"no_patches": true}})
        );
        assert_eq!(
            data["lock"]["declaration_hash_expected"], before,
            "a [package.*] edit must not move the declaration hash"
        );
    }

    /// No `ocx.toml` in scope is a usage error.
    #[tokio::test]
    // ported-from: test/tests/test_status.py::test_status_outside_a_project_exits_64
    async fn status_outside_a_project_exits_64() {
        let fixture = Fixture::new();
        let empty = fixture.root.join("empty");
        std::fs::create_dir_all(&empty).expect("mkdir empty");

        let outcome = drive(&["status"], &fixture.env(&empty, &[])).await;

        assert_eq!(outcome.code, ExitCode::from(64), "stderr: {}", outcome.err);
    }

    /// `--project <dir>` / `OCX_PROJECT=<dir>` naming a directory with no
    /// `ocx.toml` exits 64, and the error names *that* directory — not the
    /// working directory, and not a parent walk that never ran.
    #[tokio::test]
    // ported-from: test/tests/test_status.py::test_status_names_the_selected_directory_when_it_holds_no_manifest
    async fn status_names_the_selected_directory_when_it_holds_no_manifest() {
        for spelling in ["flag", "env"] {
            let fixture = Fixture::new();
            let selected = fixture.root.join("probe-empty");
            std::fs::create_dir_all(&selected).expect("mkdir selected");
            let elsewhere = fixture.root.join("cwd");
            std::fs::create_dir_all(&elsewhere).expect("mkdir cwd");
            let selected_text = selected.display().to_string();

            let outcome = if spelling == "flag" {
                drive(
                    &["--project", selected_text.as_str(), "status"],
                    &fixture.env(&elsewhere, &[]),
                )
                .await
            } else {
                drive(&["status"], &fixture.env(&elsewhere, &[("OCX_PROJECT", &selected)])).await
            };

            assert_eq!(outcome.code, ExitCode::from(64), "[{spelling}] stderr: {}", outcome.err);
            assert!(
                outcome.err.contains(&selected_text),
                "[{spelling}] the error must name the selected directory:\n{}",
                outcome.err
            );
            assert!(
                !outcome.err.contains(&elsewhere.display().to_string()),
                "[{spelling}] the error must not name the working directory:\n{}",
                outcome.err
            );
            assert!(
                !outcome.err.contains("any parent"),
                "[{spelling}] no walk happened, so none may be claimed:\n{}",
                outcome.err
            );
        }
    }

    /// `status` takes no `-g` and no NAME.
    #[tokio::test]
    // ported-from: test/tests/test_status.py::test_status_rejects_selectors
    async fn status_rejects_selectors() {
        let fixture = Fixture::new();
        let project = fixture.project("[tools]\n");
        let env = fixture.env(&project, &[]);

        assert_ne!(drive(&["status", "-g", "ci"], &env).await.code, ExitCode::SUCCESS);
        assert_ne!(drive(&["status", "some-binding"], &env).await.code, ExitCode::SUCCESS);
    }

    /// `--offline` changes nothing, and no install symlink appears for a
    /// declared but never-installed binding.
    #[tokio::test]
    // ported-from: test/tests/test_status.py::test_status_makes_no_network_or_store_writes
    async fn status_makes_no_network_or_store_writes() {
        let fixture = Fixture::new();
        let project = fixture.project(&declared_tool());
        lock(&project, &[("linux/amd64", '1')]).await;
        let env = fixture.env(&project, &[]);

        let online = drive(&["--format", "json", "status"], &env).await;
        let offline = drive(&["--offline", "--format", "json", "status"], &env).await;

        assert_eq!(offline.code, ExitCode::SUCCESS, "{}", offline.err);
        assert_eq!(offline.out, online.out, "status must not behave differently offline");
        let symlinks = fixture.home.join("symlinks");
        let leaked: Vec<PathBuf> = walk(&symlinks)
            .into_iter()
            .filter(|path| path.display().to_string().contains(REPO))
            .collect();
        assert!(leaked.is_empty(), "status must not create install symlinks: {leaked:?}");
    }

    /// Every path under `dir`, recursively; empty when `dir` does not exist.
    fn walk(dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .flat_map(|entry| {
                let path = entry.path();
                let mut below = walk(&path);
                below.push(path);
                below
            })
            .collect()
    }
}
