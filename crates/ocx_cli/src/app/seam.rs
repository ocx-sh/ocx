// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The in-process CLI seam: drive `ocx <argv>` inside the test process.
//!
//! For a case whose contract *is* the CLI surface — flag grammar, the report a
//! verb prints, the exit code it returns — and which would otherwise need the
//! built binary and a subprocess. Everything else ports to the lowest crate
//! whose API exercises the behaviour; a case that needs a registry, a shell, a
//! pty or a process of its own stays an acceptance test.
//!
//! Test-only: the module is gated `cfg(any(test, feature = "__testing"))`, so a
//! release build does not contain it.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use clap::Parser as _;
use ocx_console::capture::{self, Stream};
use ocx_oci::client::network_refusal;
use tracing::instrument::WithSubscriber as _;

use super::{App, Cli};

/// The complete environment one [`run`] executes in. Nothing falls through to
/// the test process: a variable not in `vars` reads as unset, and
/// `ocx_util::env::current_dir` — where project discovery starts — answers
/// `cwd`. A non-UTF-8 key or value reads as unset, as `ocx_util::env::var`
/// treats one.
///
/// `cwd` is not the process working directory: that is a process global the
/// seam leaves alone. A relative path *argument* a command opens directly (the
/// `config test` candidate) therefore resolves against the test process's own
/// directory — pass absolute paths.
pub struct Environment {
    pub vars: BTreeMap<OsString, OsString>,
    pub cwd: PathBuf,
}

/// Run `ocx` with `argv` (program name first) inside this process and return
/// the exit code `main` would return; the command's stdout lands in `out`, its
/// stderr in `err`.
///
/// ```ignore
/// let home = tempfile::tempdir()?;
/// let env = Environment { vars: [("OCX_HOME".into(), home.path().into())].into(), cwd: project_dir };
/// let (mut out, mut err) = (Vec::new(), Vec::new());
/// let code = run(&["ocx", "--format", "json", "status"].map(OsString::from), &env, &mut out, &mut err).await;
/// assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
/// assert_eq!(serde_json::from_slice::<serde_json::Value>(&out)?["lock"]["present"], false);
/// ```
///
/// Contract, each pinned by a test in this module:
///
/// 1. **No ambient environment.** `env` is installed through
///    `ocx_util::env::overrides` in hermetic mode. Readers that bypass that
///    layer are caught by the poisoned `env` of the `ocx_cli_seam_test` Bazel
///    target, not by this function.
/// 2. **No network.** A registry transport is refused where it would be built
///    (`ocx_oci::client::network_refusal`), and so is an index fetch
///    (`ocx_index::ReqwestIndexTransport`); a run that tried returns 64. The
///    Sigstore client (sign/verify) and the forge clients (announce) are not
///    refused — their verbs are not admitted.
/// 3. **No process exit, exec or spawn.** Only the verbs in `ADMITTED` run;
///    any other is refused with 64 before dispatch, and `--help` renders into
///    `out` instead of exiting. An exit during a run fails the test process
///    (an `atexit` tripwire) rather than ending it with the command's status.
/// 4. **No global installs.** No global subscriber, no signal handler, no
///    `console` colour override; tracing goes to a subscriber scoped to the
///    call. `log`-facade records are not bridged — that bridge is itself a
///    process global — so a diagnostic logged through `log::` never reaches
///    `err`; the final error line does.
/// 5. **Output only through `out`/`err`.**
///
/// Runs on the caller's Tokio runtime. Serialised under `ocx_util`'s
/// `EnvLock`, which it acquires — a caller holding that lock deadlocks. The
/// future is not `Send` for the same reason.
pub async fn run(
    argv: &[OsString],
    env: &Environment,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> ExitCode {
    run_admitting(ADMITTED, argv, env, out, err).await
}

/// Canonical command names (`app::canonical_command_name`) a [`run`] may
/// dispatch — verbs whose whole effect is reading local state and printing a
/// report. Everything else is refused with 64 before dispatch.
const ADMITTED: &[&str] = &["status", "config test"];

/// The allowlist of the run in progress; `None` when no run is.
static ADMITTED_NOW: Mutex<Option<&'static [&'static str]>> = Mutex::new(None);

fn admitted_now() -> MutexGuard<'static, Option<&'static [&'static str]>> {
    ADMITTED_NOW.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn run_admitting(
    admitted: &'static [&'static str],
    argv: &[OsString],
    env: &Environment,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> ExitCode {
    // ponytail: serialised via EnvLock; per-invocation env threading when concurrency matters.
    // The guard is held across every `.await` below on purpose — it is what
    // serialises seam runs — so the future is `!Send` and nothing inside the
    // run may take the lock again.
    let lock = ocx_util::env::overrides::lock();
    for (key, value) in &env.vars {
        if let (Some(key), Some(value)) = (key.to_str(), value.to_str()) {
            lock.set(key, value);
        }
    }
    lock.hermetic(env.cwd.clone());

    let session = Session::begin(admitted);
    // Boxed: the whole CLI future is large enough that, moved through the
    // caller's poll frames in a debug build, one run needed ~1.9 MiB of a
    // 2 MiB test-thread stack and overflowed it on Windows. On the heap the
    // same run needs ~1.4 MiB (measured with `RUST_MIN_STACK` on Linux).
    let code = Box::pin(App::drive(argv)).with_subscriber(subscriber(argv)).await;
    let (stdout, mut stderr, refused) = session.finish();
    let code = if refused {
        stderr.extend_from_slice(b"ERROR network access refused: the in-process seam opens no network connection\n");
        ocx_exit::ExitCode::UsageError.into()
    } else {
        code
    };
    drop(lock);

    if out.write_all(&stdout).is_err() || err.write_all(&stderr).is_err() {
        return ocx_exit::ExitCode::IoError.into();
    }
    code
}

/// The process globals a run swaps in, undone on drop so a panicking command
/// cannot leave the next run armed.
struct Session;

impl Session {
    fn begin(admitted: &'static [&'static str]) -> Self {
        *admitted_now() = Some(admitted);
        network_refusal::arm();
        capture::begin();
        arm_exit_tripwire();
        IN_RUN.store(true, Ordering::SeqCst);
        Session
    }

    /// `(stdout, stderr, network refused)`.
    fn finish(self) -> (Vec<u8>, Vec<u8>, bool) {
        let (stdout, stderr) = capture::end();
        (stdout, stderr, network_refusal::disarm())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        IN_RUN.store(false, Ordering::SeqCst);
        *admitted_now() = None;
        network_refusal::disarm();
        capture::end();
    }
}

/// Whether a run is between [`Session::begin`] and its drop.
static IN_RUN: AtomicBool = AtomicBool::new(false);

/// A `process::exit` during a run ends the test binary with the status the
/// command chose — 0 for `--help` — and a harness reads a clean exit as a
/// pass. This `atexit` hook turns any exit during a run into a failure. An
/// `execvp` runs no exit hook; the admission check is what keeps it out.
#[cfg(unix)]
fn arm_exit_tripwire() {
    extern "C" fn tripwire() {
        if IN_RUN.load(Ordering::SeqCst) {
            // Best effort: at exit there is nowhere left to report a failure to.
            let _ = std::io::stderr().write_all(b"in-process seam: the command exited the test process\n");
            // SAFETY: `_exit` ends the process at once, without re-entering
            // the exit sequence this hook is running inside.
            unsafe { libc::_exit(1) };
        }
    }
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        // SAFETY: `tripwire` is an `extern "C" fn` with no captured state.
        // A non-zero return (allocation failure) leaves the tripwire unarmed,
        // which loses the diagnostic and nothing else.
        let _ = unsafe { libc::atexit(tripwire) };
    });
}

#[cfg(not(unix))]
fn arm_exit_tripwire() {}

/// A fmt subscriber writing into the capture's stderr, at the level `argv`
/// asks for. `OCX_LOG`/`RUST_LOG` are not consulted — the production filter
/// reads them from the process environment.
fn subscriber(argv: &[OsString]) -> impl tracing::Subscriber + Send + Sync {
    let level = Cli::try_parse_from(argv)
        .ok()
        .and_then(|cli| cli.context.log_level)
        .map_or(tracing_subscriber::filter::LevelFilter::INFO, Into::into);
    tracing_subscriber::fmt()
        .compact()
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_max_level(level)
        .with_writer(|| CapturedStderr)
        .finish()
}

struct CapturedStderr;

impl Write for CapturedStderr {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        capture::write(Stream::Stderr, bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl App {
    /// `main`, minus the process: the same boundary, logging through whatever
    /// subscriber is current.
    async fn drive(argv: &[OsString]) -> ExitCode {
        let (color_mode, color_config) = color(argv);
        super::boundary::finish(App::new().run_from(argv.to_vec(), color_mode, color_config).await)
    }
}

/// `--color` from `argv` rather than the process's own arguments. `auto`
/// resolves to off without probing a terminal — the capture is not one — and
/// the `console` crate's global colour state is left alone.
fn color(argv: &[OsString]) -> (ocx_console::ColorMode, ocx_console::ColorModeConfig) {
    use ocx_console::ColorMode;

    let mode = match ColorMode::from_argv(argv.iter().skip(1).map(|arg| arg.to_string_lossy().into_owned())) {
        ColorMode::Always => ColorMode::Always,
        ColorMode::Auto | ColorMode::Never => ColorMode::Never,
    };
    (mode, mode.config())
}

/// Whether a seam run is in progress — the switch `app::in_seam` reads.
pub(super) fn active() -> bool {
    IN_RUN.load(Ordering::SeqCst)
}

/// Refuse a verb the run in progress does not admit.
pub(super) fn admit(command: Option<&crate::command::Command>) -> anyhow::Result<()> {
    let name = command.map_or("", super::canonical_command_name);
    if admitted_now().is_some_and(|admitted| admitted.contains(&name)) {
        return Ok(());
    }
    Err(super::CommandError::new(
        format!("`ocx {name}` is not admitted by the in-process seam; test it end to end"),
        ocx_exit::ExitCode::UsageError,
    )
    .into())
}

/// `clap_parse::parse` without the process exit: help and version render into
/// the capture and return clap's own code (0, or 2 for help shown in place of
/// a missing subcommand); every other failure is 64, as in production.
pub(super) fn parse(cmd: clap::Command, argv: &[OsString]) -> Result<clap::ArgMatches, ExitCode> {
    use clap_builder::error::ErrorKind;

    cmd.try_get_matches_from(argv).map_err(|error| {
        let stream = if error.use_stderr() {
            Stream::Stderr
        } else {
            Stream::Stdout
        };
        capture::write(stream, error.render().to_string().as_bytes());
        match error.kind() {
            ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                u8::try_from(error.exit_code()).map_or(ExitCode::FAILURE, ExitCode::from)
            }
            _ => ocx_exit::ExitCode::UsageError.into(),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const USAGE: u8 = 64;

    struct Outcome {
        code: ExitCode,
        out: String,
        err: String,
    }

    fn environment(vars: &[(&str, &Path)], cwd: &Path) -> Environment {
        Environment {
            vars: vars
                .iter()
                .map(|(key, value)| (OsString::from(key), value.as_os_str().to_owned()))
                .collect(),
            cwd: cwd.to_owned(),
        }
    }

    async fn drive(argv: &[&str], env: &Environment) -> Outcome {
        let argv: Vec<OsString> = std::iter::once("ocx")
            .chain(argv.iter().copied())
            .map(OsString::from)
            .collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&argv, env, &mut out, &mut err).await;
        Outcome {
            code,
            out: String::from_utf8(out).expect("stdout is UTF-8"),
            err: String::from_utf8(err).expect("stderr is UTF-8"),
        }
    }

    /// A directory holding an empty-but-valid `ocx.toml`.
    fn project(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("create project dir");
        std::fs::write(dir.join("ocx.toml"), "[tools]\n").expect("write ocx.toml");
        dunce::canonicalize(&dir).expect("canonical project dir")
    }

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("stdout must be one JSON document ({error}): {text:?}"))
    }

    // ── Invariant 1: no ambient environment reads ───────────────────────────

    /// A variable set on the *test process* is invisible to the run, and the
    /// run's working directory is `Environment::cwd`, not the process's.
    ///
    /// Red states: `overrides::get` falling through to `std::env::var`
    /// (the ambient `OCX_PROJECT` wins), `current_dir` ignoring the hermetic
    /// cwd (the walk starts from the crate directory).
    #[tokio::test]
    async fn seam_hermetic_env_ignores_the_process_environment_and_cwd() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("ocx-home");
        let wanted = project(root.path(), "wanted");
        let ambient = project(root.path(), "ambient");
        // SAFETY: the seam suites run one test per process (nextest) or
        // serialised (`--test-threads=1` under Bazel); nothing else reads the
        // process environment concurrently.
        unsafe { std::env::set_var("OCX_PROJECT", ambient.join("ocx.toml")) };
        let outcome = drive(
            &["--format", "json", "status"],
            &environment(&[("OCX_HOME", &home)], &wanted),
        )
        .await;
        // SAFETY: as above.
        unsafe { std::env::remove_var("OCX_PROJECT") };

        assert_eq!(outcome.code, ExitCode::SUCCESS, "stderr: {}", outcome.err);
        assert_eq!(
            json(&outcome.out)["project"],
            wanted.join("ocx.toml").display().to_string(),
            "the run must resolve the project from Environment::cwd alone"
        );
    }

    /// The home variables answer from the hermetic table: both the
    /// `$OCX_HOME` default (`~/.ocx/config.toml`) and the user tier
    /// (`<config dir>/ocx/config.toml`, read through `dirs` in production)
    /// come from the fake home.
    ///
    /// Each OS names both through its own variables, so the fake home is
    /// seeded the way `dirs` and `std::env::home_dir` read it there: `HOME`
    /// with `~/.config` (Linux) or `~/Library/Application Support` (macOS);
    /// `USERPROFILE` with `%APPDATA%` (Windows, which reads no `HOME`).
    ///
    /// Red states: `home_dir` returning `std::env::home_dir()`, the loader's
    /// `user_path` returning `dirs::config_dir()` under the seam.
    #[tokio::test]
    async fn seam_hermetic_home_answers_both_home_derived_config_tiers() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let (config_dir, vars) = if cfg!(windows) {
            let appdata = home.join("AppData").join("Roaming");
            (
                appdata.clone(),
                vec![("USERPROFILE", home.clone()), ("APPDATA", appdata)],
            )
        } else if cfg!(target_os = "macos") {
            (
                home.join("Library").join("Application Support"),
                vec![("HOME", home.clone())],
            )
        } else {
            (home.join(".config"), vec![("HOME", home.clone())])
        };
        std::fs::create_dir_all(home.join(".ocx")).expect("mkdir ~/.ocx");
        std::fs::create_dir_all(config_dir.join("ocx")).expect("mkdir <config dir>/ocx");
        std::fs::write(
            home.join(".ocx").join("config.toml"),
            "[registries.\"home-tier.example\"]\ninsecure = true\n",
        )
        .expect("write $OCX_HOME tier");
        std::fs::write(
            config_dir.join("ocx").join("config.toml"),
            "[registry]\ndefault = \"user-tier.example\"\n",
        )
        .expect("write user tier");
        let candidate = root.path().join("candidate.toml");
        std::fs::write(&candidate, "").expect("write candidate");

        let vars: Vec<(&str, &Path)> = vars.iter().map(|(key, value)| (*key, value.as_path())).collect();
        let outcome = drive(
            &["--format", "json", "config", "test", &candidate.display().to_string()],
            &environment(&vars, root.path()),
        )
        .await;

        assert_eq!(outcome.code, ExitCode::SUCCESS, "stderr: {}", outcome.err);
        let report = json(&outcome.out);
        assert_eq!(
            report["registry_default"], "user-tier.example",
            "user tier from the fake HOME"
        );
        assert!(
            report["registries"]
                .as_array()
                .is_some_and(|registries| registries.contains(&"home-tier.example".into())),
            "$OCX_HOME tier from the fake HOME: {report}"
        );
    }

    /// Under the seam's Bazel target the poisoned environment is really there.
    ///
    /// Keyed on `TEST_TARGET` so the unpoisoned targets (cargo, and
    /// `ocx_cli_test`, which runs these tests too) pass it trivially. Red
    /// state: the `env` dict removed from `ocx_cli_seam_test` in
    /// `crates/ocx_cli/BUILD.bazel`.
    #[test]
    fn seam_poisoned_environment_is_present_under_its_bazel_target() {
        let under_seam_target = std::env::var("TEST_TARGET").is_ok_and(|target| target.ends_with(":ocx_cli_seam_test"));
        if !under_seam_target {
            return;
        }
        let missing: Vec<String> = poisoned_keys()
            .into_iter()
            .filter(|key| std::env::var_os(key).is_none_or(|value| !value.to_string_lossy().contains("poison")))
            .collect();
        assert!(
            missing.is_empty(),
            "keys not poisoned under the seam target: {missing:?}"
        );
        // A poisoned config tier must be something an ambient read FAILS on:
        // a directory where the loader expects `config.toml` (kept, then an
        // error to read). A path that does not resolve, or a symlink, is
        // skipped by the loader, which neutralises the read instead.
        let var = |key: &str| std::path::PathBuf::from(std::env::var_os(key).unwrap_or_default());
        for tier in [
            var("HOME").join(".config/ocx/config.toml"),
            var("XDG_CONFIG_HOME").join("ocx/config.toml"),
            var("OCX_HOME").join("config.toml"),
        ] {
            assert!(
                std::fs::symlink_metadata(&tier).is_ok_and(|meta| meta.file_type().is_dir()),
                "{} must be a real directory, so a config read of it fails",
                tier.display()
            );
        }
        let docker = var("DOCKER_CONFIG").join("config.json");
        let text = std::fs::read_to_string(&docker)
            .unwrap_or_else(|e| panic!("poisoned {} must be readable: {e}", docker.display()));
        assert!(
            serde_json::from_str::<serde_json::Value>(&text).is_err(),
            "{} must be invalid JSON",
            docker.display()
        );
    }

    /// Every key `ocx_cli_seam_test` must poison — the process-environment
    /// reads a third-party crate could make past `ocx_util::env`, plus the
    /// registry-auth family for the default registry.
    fn poisoned_keys() -> Vec<String> {
        use ocx_util::prelude::StringExt as _;

        let mut keys: Vec<String> = [
            "OCX_HOME",
            "HOME",
            "XDG_CONFIG_HOME",
            "DOCKER_CONFIG",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
        ]
        .map(str::to_owned)
        .to_vec();
        let slug = ocx_oci::DEFAULT_REGISTRY.to_slug();
        keys.extend(["TYPE", "USER", "TOKEN"].map(|suffix| format!("OCX_AUTH_{slug}_{suffix}")));
        keys
    }

    // ── Invariant 2: no network ─────────────────────────────────────────────

    /// A verb that reaches for a registry is refused with 64 before any
    /// request leaves the process.
    ///
    /// `index list` is not admitted; the test widens the allowlist for this
    /// one call because no admitted verb touches a registry at all. Red state:
    /// `Client::transport` building the real transport under the seam.
    #[tokio::test]
    async fn seam_refuses_a_registry_transport_with_64() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("ocx-home");
        let argv: Vec<OsString> = ["ocx", "--remote", "index", "list", "registry.invalid/seam/probe"]
            .map(OsString::from)
            .to_vec();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_admitting(
            &["index list"],
            &argv,
            &environment(&[("OCX_HOME", &home)], root.path()),
            &mut out,
            &mut err,
        )
        .await;
        let err = String::from_utf8_lossy(&err);
        assert_eq!(code, ExitCode::from(USAGE), "stderr: {err}");
        assert!(
            err.contains("network access refused"),
            "stderr names the refusal: {err}"
        );
    }

    /// An index fetch is refused the same way — the index has its own HTTP
    /// client, not the registry transport.
    ///
    /// The index base is `index.invalid` (RFC 2606), so the red state fails
    /// on name resolution rather than reaching a real host. Red state: the
    /// refusal removed from `ReqwestIndexTransport::get` — the run then fails
    /// on DNS without naming the refusal.
    #[tokio::test]
    async fn seam_refuses_an_index_fetch_with_64() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("ocx-home");
        std::fs::create_dir_all(&home).expect("mkdir OCX_HOME");
        std::fs::write(
            home.join("config.toml"),
            "[registries.\"ocx.sh\"]\nindex = \"https://index.invalid/\"\n",
        )
        .expect("write config");
        let argv: Vec<OsString> = ["ocx", "--remote", "index", "list", "ocx.sh/seam/probe"]
            .map(OsString::from)
            .to_vec();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_admitting(
            &["index list"],
            &argv,
            &environment(&[("OCX_HOME", &home)], root.path()),
            &mut out,
            &mut err,
        )
        .await;
        let err = String::from_utf8_lossy(&err);
        assert_eq!(code, ExitCode::from(USAGE), "stderr: {err}");
        assert!(
            err.contains("network access refused"),
            "stderr names the refusal: {err}"
        );
    }

    // ── Invariant 3: no process exit, no exec, no spawn ─────────────────────

    /// The process-replacing and process-owning verbs are refused before
    /// dispatch.
    ///
    /// Red state: the admission check removed — each of these then runs (and
    /// fails for its own reasons) without naming the refusal.
    #[tokio::test]
    async fn seam_refuses_the_process_owning_verbs_before_dispatch() {
        let root = tempfile::tempdir().expect("tempdir");
        let env = environment(&[("OCX_HOME", &root.path().join("ocx-home"))], root.path());
        let digest = format!("ocx.sh/seam@sha256:{}", "0".repeat(64));
        for argv in [
            vec!["exec", "--", "true"],
            vec!["package", "exec", "seam/probe:1", "--", "true"],
            vec!["launcher", "exec", "/nonexistent", "--", "true"],
            vec!["launcher", "shim", digest.as_str(), "--", "true"],
            vec!["self", "update"],
        ] {
            let outcome = drive(&argv, &env).await;
            assert_eq!(
                outcome.code,
                ExitCode::from(USAGE),
                "`ocx {}`: {}",
                argv.join(" "),
                outcome.err
            );
            assert!(
                outcome.err.contains("not admitted"),
                "`ocx {}` must be refused by the seam: {}",
                argv.join(" "),
                outcome.err
            );
        }
    }

    /// `--help` renders into `out` and returns 0 instead of exiting the test
    /// process.
    ///
    /// Red state: the seam parse delegating to `clap_parse::parse`, whose
    /// `err.exit()` ends the test binary mid-test (libtest then never prints
    /// this test's result line).
    #[tokio::test]
    async fn seam_help_renders_into_out_without_exiting() {
        let root = tempfile::tempdir().expect("tempdir");
        let env = environment(&[], root.path());

        for argv in [vec!["--help"], vec!["status", "--help"]] {
            let help = drive(&argv, &env).await;
            assert_eq!(help.code, ExitCode::SUCCESS, "`ocx {}`: {}", argv.join(" "), help.err);
            assert!(help.out.contains("Usage: ocx"), "help lands in out: {:?}", help.out);
        }
    }

    // ── Invariant 4: no global installs ─────────────────────────────────────

    /// A run installs no global tracing subscriber, and its diagnostics land
    /// in `err`.
    ///
    /// Red state: `Context::try_init` running `LogSettings::init_with_progress`
    /// under the seam.
    #[tokio::test]
    async fn seam_installs_no_global_subscriber_and_logs_to_err() {
        let root = tempfile::tempdir().expect("tempdir");
        let outcome = drive(
            &["status"],
            &environment(&[("OCX_HOME", &root.path().join("ocx-home"))], root.path()),
        )
        .await;

        assert_eq!(outcome.code, ExitCode::from(USAGE), "no ocx.toml in scope is 64");
        assert!(
            outcome.err.contains("ocx.toml"),
            "the error line lands in err: {:?}",
            outcome.err
        );
        // Outside any scoped default, `get_default` answers with the global
        // dispatcher — `NoSubscriber` until something installs one.
        // (`has_been_set` cannot tell: a scoped default sets it too.)
        assert!(
            tracing::dispatcher::get_default(|dispatch| dispatch.is::<tracing::subscriber::NoSubscriber>()),
            "the run installed a global tracing subscriber"
        );
    }

    // ── Invariant 5: output only through out/err ────────────────────────────

    /// The report lands in `out` — and so does the JSON error envelope.
    ///
    /// Red states: `Line::write` bypassing the capture sink (the report goes
    /// to the test process's stdout), the envelope printed with `println!`.
    #[tokio::test]
    async fn seam_report_and_error_envelope_land_in_out() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("ocx-home");
        let wanted = project(root.path(), "wanted");

        let report = drive(
            &["--format", "json", "status"],
            &environment(&[("OCX_HOME", &home)], &wanted),
        )
        .await;
        assert_eq!(report.code, ExitCode::SUCCESS, "stderr: {}", report.err);
        assert!(
            json(&report.out)["groups"].is_object(),
            "the report is in out: {:?}",
            report.out
        );
        assert!(!report.err.contains('{'), "no JSON in err: {:?}", report.err);

        let failure = drive(
            &["--format", "json", "status"],
            &environment(&[("OCX_HOME", &home)], root.path()),
        )
        .await;
        assert_eq!(failure.code, ExitCode::from(USAGE));
        assert_eq!(
            json(&failure.out)["command"],
            "status",
            "the envelope is in out: {:?}",
            failure.out
        );
    }
}
