// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The generated Rust SDK (`tests/golden/rust/`, crate root `lib.rs`) compiled in as a module and run: against a
//! fake child (`fixtures/fake_ocx.sh`) for the spawn, argv and outcome rules, and against the Bazel-built
//! `ocx`, whose path Bazel passes as the compile-time `SDK_TEST_OCX_BINARY`. The `real_binary` tests need it and
//! fail without it, so a build with no binary reports red instead of passing on nothing; `verify-deep.yml`'s
//! nextest legs, which have none, filter them out by that module name.

#![expect(
    dead_code,
    unused_imports,
    reason = "the generated SDK is compiled whole; the tests use a part of it"
)]

#[expect(
    unreachable_pub,
    reason = "the generated SDK is a published crate's public API, compiled here as a private module"
)]
#[expect(
    clippy::disallowed_types,
    reason = "the generated SDK spawns the caller's `ocx`, never runs inside ocx, and resolves no package"
)]
#[path = "golden/rust/lib.rs"]
mod sdk;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::{Value, json};
use tempfile::TempDir;

use sdk::contract;
use sdk::types::{ExitCode, SignatureReport, SignatureReportSweep, SweptStatus};
use sdk::wire::Invocation;
use sdk::{Error, Ocx, Outcome, Secret, Unknowns};

/// The binary the `real_binary` tests run, or none when this build was not given one.
const REAL_BINARY: Option<&str> = option_env!("SDK_TEST_OCX_BINARY");

fn binary_json(commands: &[(&str, u32)], reports: &[(&str, u32)], errors: u32) -> String {
    fn table<'a>(entries: &[(&'a str, u32)]) -> BTreeMap<&'a str, u32> {
        entries.iter().copied().collect()
    }
    json!({
        "schema_version": 1,
        "version": "0.6.4",
        "contract": { "commands": table(commands), "reports": table(reports), "errors": errors },
    })
    .to_string()
}

/// The handshake of a binary at exactly the contract this SDK was generated for.
fn handshake() -> String {
    binary_json(contract::COMMANDS, contract::REPORTS, contract::ERRORS)
}

mod argv_and_refusals {
    use super::*;

    #[test]
    fn an_identifier_that_reads_as_an_option_is_refused() {
        let mut argv = sdk::wire::Argv::new();
        sdk::PackageInstallArgs {
            packages: vec!["--env=X=1".to_owned()],
            ..Default::default()
        }
        .push(&mut argv);
        let refused = argv.finish().expect_err("a leading `-` identifier must not reach argv");
        assert!(
            matches!(refused, Error::InvalidArgument { arg: "packages", .. }),
            "{refused:?}"
        );
    }

    #[test]
    fn a_hyphen_value_passes_only_where_the_grammar_allows_it() {
        let allowed = |command: &str| {
            let mut argv = sdk::wire::Argv::new();
            sdk::PackageExecArgs {
                packages: vec!["cmake:3".to_owned()],
                command: vec![command.to_owned()],
                ..Default::default()
            }
            .push(&mut argv);
            argv.finish()
        };
        let invocation = allowed("--version").expect("`command` allows hyphen values");
        assert_eq!(
            invocation.arguments.last().and_then(|word| word.to_str()),
            Some("--version")
        );
        // The same token as the *package* positional, where the grammar does not allow it.
        let mut argv = sdk::wire::Argv::new();
        sdk::PackageExecArgs {
            packages: vec!["--version".to_owned()],
            command: vec!["true".to_owned()],
            ..Default::default()
        }
        .push(&mut argv);
        assert!(matches!(
            argv.finish(),
            Err(Error::InvalidArgument { arg: "packages", .. })
        ));
    }

    #[test]
    fn a_flag_value_is_one_token_and_never_an_option() {
        let mut argv = sdk::wire::Argv::new();
        sdk::PackageInstallArgs {
            platform: Some("--evil x".to_owned()),
            packages: vec!["cmake:3".to_owned()],
            ..Default::default()
        }
        .push(&mut argv);
        let invocation = argv.finish().expect("a flag value may start with `-`");
        assert_eq!(
            invocation.arguments,
            vec![OsString::from("--platform=--evil x"), OsString::from("cmake:3")]
        );
    }

    #[test]
    fn a_repeatable_flag_sends_every_value_as_its_own_occurrence() {
        let mut argv = sdk::wire::Argv::new();
        sdk::ExecArgs {
            env: vec!["A=1".to_owned(), "B=2".to_owned()],
            groups: vec!["ci".to_owned(), "lint".to_owned()],
            argv: vec!["true".to_owned()],
            ..Default::default()
        }
        .push(&mut argv);
        let invocation = argv.finish().expect("repeated values build");
        for flag in ["--env=A=1", "--env=B=2", "--group=ci", "--group=lint"] {
            assert!(
                invocation.arguments.contains(&OsString::from(flag)),
                "{flag} missing from {:?}",
                invocation.arguments
            );
        }
        let _: Vec<String> = sdk::PullArgs::default().groups;
    }

    #[test]
    fn a_terminated_positional_is_closed_with_the_terminator_before_the_next_one() {
        let mut argv = sdk::wire::Argv::new();
        sdk::PackageExecArgs {
            packages: vec!["cmake:3".to_owned()],
            command: vec!["--env=LD_PRELOAD=x".to_owned(), "--".to_owned(), "sh".to_owned()],
            ..Default::default()
        }
        .push(&mut argv);
        let invocation = argv.finish().expect("command values are data after the terminator");
        assert_eq!(
            invocation.arguments,
            ["cmake:3", "--", "--env=LD_PRELOAD=x", "--", "sh"].map(OsString::from)
        );
    }

    #[test]
    fn an_empty_flag_value_is_sent_and_an_absent_one_is_not() {
        let build = |platform: Option<&str>| {
            let mut argv = sdk::wire::Argv::new();
            sdk::PackageInstallArgs {
                platform: platform.map(str::to_owned),
                packages: vec!["cmake:3".to_owned()],
                ..Default::default()
            }
            .push(&mut argv);
            argv.finish().expect("builds").arguments
        };
        assert_eq!(build(Some("")), ["--platform=", "cmake:3"].map(OsString::from));
        assert_eq!(build(None), ["cmake:3"].map(OsString::from));
    }

    #[test]
    fn an_empty_command_word_passes_and_an_empty_identifier_is_refused() {
        let mut argv = sdk::wire::Argv::new();
        sdk::PackageExecArgs {
            packages: vec!["cmake:3".to_owned()],
            command: vec!["sh".to_owned(), String::new()],
            ..Default::default()
        }
        .push(&mut argv);
        let invocation = argv.finish().expect("an empty command argument is data");
        assert_eq!(invocation.arguments, ["cmake:3", "--", "sh", ""].map(OsString::from));

        let mut argv = sdk::wire::Argv::new();
        sdk::PackageInstallArgs {
            packages: vec![String::new()],
            ..Default::default()
        }
        .push(&mut argv);
        assert!(matches!(
            argv.finish(),
            Err(Error::InvalidArgument { arg: "packages", .. })
        ));
    }

    #[test]
    fn debug_of_an_args_struct_hides_the_secret() {
        let args = sdk::LoginArgs {
            username: Some("alice".to_owned()),
            password_stdin: Some(Secret::new("hunter2")),
            ..Default::default()
        };
        let shown = format!("{args:?}");
        assert!(shown.contains("<redacted>"), "{shown}");
        assert!(!shown.contains("hunter2") && !shown.contains("104"), "{shown}");
    }
}

mod discovery {
    use super::*;

    fn environment(path: &std::path::Path, pin: Option<&str>) -> Vec<(OsString, OsString)> {
        let mut environment = vec![(OsString::from("PATH"), path.as_os_str().to_owned())];
        environment.extend(pin.map(|pin| (OsString::from("OCX_BINARY_PIN"), OsString::from(pin))));
        environment
    }

    #[test]
    fn windows_resolves_ocx_exe_and_never_a_bare_ocx() {
        let directory = TempDir::new().expect("temp dir");
        std::fs::write(directory.path().join("ocx"), b"").expect("write");
        let bare_only = sdk::runtime::locate(&environment(directory.path(), None), "ocx.exe");
        assert!(matches!(bare_only, Err(Error::BinaryNotFound)), "{bare_only:?}");

        std::fs::write(directory.path().join("ocx.exe"), b"").expect("write");
        let found = sdk::runtime::locate(&environment(directory.path(), None), "ocx.exe").expect("ocx.exe is there");
        assert_eq!(found, directory.path().join("ocx.exe"));
    }

    #[test]
    fn the_pin_wins_over_path() {
        let directory = TempDir::new().expect("temp dir");
        std::fs::write(directory.path().join("ocx"), b"").expect("write");
        let found = sdk::runtime::locate(&environment(directory.path(), Some("/pinned/ocx")), "ocx");
        assert_eq!(found.expect("pinned"), PathBuf::from("/pinned/ocx"));
    }

    #[test]
    fn an_empty_or_relative_path_entry_is_never_searched() {
        // The planted file lives in a temp dir, reached by a relative entry that climbs out of the
        // working directory: the runfiles tree Bazel runs this from is read-only and may hold no file.
        // No relative path crosses Windows drives; CI checks out on D: while %TEMP% is on C:.
        let cwd = std::env::current_dir().expect("cwd");
        fn drive(path: &std::path::Path) -> Option<std::path::Prefix<'_>> {
            match path.components().next() {
                Some(std::path::Component::Prefix(prefix)) => Some(prefix.kind()),
                _ => None,
            }
        }
        let directory = if drive(&cwd) == drive(&std::env::temp_dir()) {
            TempDir::new()
        } else {
            TempDir::new_in(&cwd)
        }
        .expect("temp dir");
        std::fs::write(directory.path().join("ocx"), b"").expect("write");
        let mut relative = PathBuf::new();
        for _ in cwd.ancestors().skip(1) {
            relative.push("..");
        }
        relative.extend(
            directory
                .path()
                .components()
                .filter(|c| !matches!(c, std::path::Component::Prefix(_) | std::path::Component::RootDir)),
        );
        let search = |entries: &[&std::path::Path]| {
            let path = std::env::join_paths(entries).expect("join");
            sdk::runtime::locate(&environment(path.as_ref(), None), "ocx")
        };
        assert!(
            cwd.join(&relative).join("ocx").is_file(),
            "the relative entry reaches the file"
        );
        let found = search(&[directory.path()]).expect("an absolute entry finds the file");
        assert_eq!(found, directory.path().join("ocx"));
        let skipped = search(&[std::path::Path::new(""), std::path::Path::new("."), relative.as_path()]);
        assert!(matches!(skipped, Err(Error::BinaryNotFound)), "{skipped:?}");
    }

    #[test]
    fn the_first_path_entry_wins() {
        let (first, second) = (TempDir::new().expect("temp dir"), TempDir::new().expect("temp dir"));
        for directory in [&first, &second] {
            std::fs::write(directory.path().join("ocx"), b"").expect("write");
        }
        let path = std::env::join_paths([first.path(), second.path()]).expect("join");
        let found = sdk::runtime::locate(&environment(path.as_ref(), None), "ocx").expect("found");
        assert_eq!(found, first.path().join("ocx"));
    }
}

mod child_environment {
    use super::*;

    fn pair(key: &str, value: &str) -> (OsString, OsString) {
        (OsString::from(key), OsString::from(value))
    }

    #[test]
    fn no_ocx_variable_is_inherited_and_the_callers_stay() {
        let parent = vec![
            pair("PATH", "/bin"),
            pair("OCX_HOME", "/leak"),
            pair("__OCX_TESTING_X", "1"),
            pair("__ocx_plumbing", "1"),
            pair("ocx_log_level", "debug"),
            pair("OCX_BINARY_PIN", "/other/ocx"),
            pair("HOME", "/home/a"),
        ];
        let caller = vec![pair("OCX_HOME", "/chosen")];
        let child = sdk::spawn::child_environment(parent, &caller, std::path::Path::new("/the/ocx"));
        let keys: Vec<_> = child.iter().map(|(key, _)| key.to_str().expect("utf-8")).collect();
        assert_eq!(
            keys,
            ["PATH", "HOME", "OCX_HOME", "OCX_BINARY_PIN"]
                .map(String::from)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert!(child.contains(&pair("OCX_HOME", "/chosen")));
        assert!(child.contains(&pair("OCX_BINARY_PIN", "/the/ocx")));
    }
}

mod secret_names {
    use super::*;

    #[test]
    fn a_placeholder_name_matches_every_concrete_registry_and_only_those() {
        for secret in [
            "OCX_AUTH_ghcr_io_TOKEN",
            "OCX_AUTH_GHCR_IO_TOKEN",
            "ocx_auth_ghcr_io_token",
            "OCX_AUTH_a_TOKEN",
            "OCX_KEY_PASSWORD",
        ] {
            assert!(sdk::env::is_secret(secret), "{secret} leaks");
        }
        for open in [
            "OCX_AUTH__TOKEN",
            "OCX_AUTH_ghcr_io_USER",
            "OCX_AUTH_ghcr_io_TOKEN_EXTRA",
            "OCX_HOME",
            "NOT_OCX_AUTH_ghcr_io_TOKEN",
        ] {
            assert!(!sdk::env::is_secret(open), "{open} is redacted without being secret");
        }
    }

    #[test]
    fn debug_of_a_client_redacts_a_registry_token_whatever_the_registry() {
        let ocx = Ocx::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_ocx.sh"))
            .expect("the fake binary exists")
            .with_env("OCX_AUTH_ghcr_io_TOKEN", "hunter2")
            .with_env("OCX_HOME", "/visible/home");
        let shown = format!("{ocx:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("/visible/home"), "{shown}");
    }
}

#[cfg(unix)]
mod fake_child {
    use std::time::{Duration, Instant};

    use super::*;

    fn script() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_ocx.sh")
    }

    /// A client on the fake child at the SDK's own contract, logging every spawn to the returned file.
    fn client(directory: &TempDir) -> (Ocx, PathBuf) {
        let log = directory.path().join("spawns.log");
        let ocx = Ocx::new(script())
            .expect("the fake binary exists")
            .with_env("FAKE_HANDSHAKE", handshake())
            .with_env("FAKE_LOG", &log);
        (ocx, log)
    }

    fn spawns(log: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    #[expect(clippy::disallowed_types, reason = "probes a pid with `kill -0`; not a tool launch")]
    fn alive(pid: &str) -> bool {
        std::process::Command::new("/bin/sh")
            .args(["-c", "kill -0 \"$0\"", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn push_args() -> sdk::PackagePushArgs {
        sdk::PackagePushArgs::default()
    }

    #[test]
    fn a_command_whose_version_alone_changed_spawns_nothing_beyond_the_handshake() {
        let directory = TempDir::new().expect("temp dir");
        let (ocx, log) = client(&directory);
        let bumped: Vec<_> = contract::COMMANDS
            .iter()
            .map(|(name, version)| (*name, if *name == "package push" { version + 1 } else { *version }))
            .collect();
        let ocx = ocx.with_env(
            "FAKE_HANDSHAKE",
            binary_json(&bumped, contract::REPORTS, contract::ERRORS),
        );

        for _ in 0..2 {
            let refused = ocx.package_push(&push_args()).expect_err("the version differs");
            assert!(
                matches!(&refused, Error::ContractMismatch { subject, expected: 1, found: 2 } if subject.contains("package push")),
                "{refused:?}"
            );
        }
        assert_eq!(
            spawns(&log),
            ["--format=json version"],
            "one handshake, cached for the second call"
        );

        // The control: the same call at the matching contract does spawn the command, so the log counts spawns.
        let control = TempDir::new().expect("temp dir");
        let (matching, log) = client(&control);
        let _ = matching.package_push(&push_args());
        assert_eq!(spawns(&log).len(), 2, "{:?}", spawns(&log));
    }

    #[test]
    fn a_report_whose_version_changed_is_refused_before_the_command_runs() {
        let directory = TempDir::new().expect("temp dir");
        let (ocx, log) = client(&directory);
        let bumped: Vec<_> = contract::REPORTS
            .iter()
            .map(|(name, version)| (*name, if *name == "PushReport" { version + 1 } else { *version }))
            .collect();
        let ocx = ocx.with_env(
            "FAKE_HANDSHAKE",
            binary_json(contract::COMMANDS, &bumped, contract::ERRORS),
        );
        let refused = ocx.package_push(&push_args()).expect_err("the report version differs");
        assert!(
            matches!(&refused, Error::ContractMismatch { subject, .. } if subject.contains("PushReport")),
            "{refused:?}"
        );
        assert_eq!(spawns(&log).len(), 1);
    }

    #[test]
    fn a_refused_argument_never_reaches_the_child() {
        let directory = TempDir::new().expect("temp dir");
        let (ocx, log) = client(&directory);
        let args = sdk::PackageInstallArgs {
            packages: vec!["--env=X=1".to_owned()],
            ..Default::default()
        };
        let refused = ocx.package_install(&args).expect_err("refused");
        assert!(matches!(refused, Error::InvalidArgument { .. }), "{refused:?}");
        assert_eq!(spawns(&log), ["--format=json version"]);
    }

    #[test]
    fn a_hyphen_value_where_the_grammar_allows_it_reaches_the_child_verbatim() {
        let directory = TempDir::new().expect("temp dir");
        let (ocx, log) = client(&directory);
        let args = sdk::PackageExecArgs {
            packages: vec!["cmake:3".to_owned()],
            command: vec!["--version".to_owned()],
            ..Default::default()
        };
        ocx.package_exec(&args).expect("the fake child exits 0");
        let logged = spawns(&log);
        assert_eq!(
            logged.last().map(String::as_str),
            Some("--format=json package exec cmake:3 -- --version")
        );
    }

    #[test]
    fn the_child_environment_is_the_scrubbed_one_plus_what_the_caller_set() {
        let done = Ocx::new(script())
            .expect("fake")
            .with_env("FAKE_HANDSHAKE", handshake())
            .with_env("FAKE_MODE", "env")
            .with_env("OCX_KEPT", "1")
            .call_raw("exec", &[], sdk::wire::Argv::new())
            .expect("runs");
        let printed = String::from_utf8(done.stdout).expect("utf-8");
        let ocx_keys: Vec<_> = printed
            .lines()
            .filter_map(|line| line.split_once('=').map(|(key, _)| key))
            .filter(|key| key.starts_with("OCX_") || key.starts_with("__OCX_"))
            .collect();
        assert_eq!(
            ocx_keys.iter().copied().collect::<std::collections::BTreeSet<_>>(),
            ["OCX_BINARY_PIN", "OCX_KEPT"].into_iter().collect(),
            "{printed}"
        );
    }

    #[test]
    fn stdin_is_null_unless_a_secret_is_sent() {
        let run = |stdin: Option<Secret>| {
            let environment = sdk::spawn::child_environment(sdk::spawn::parent_environment(), &[], &script());
            let invocation = Invocation {
                arguments: vec![OsString::from("stdin")],
                stdin,
            };
            let done =
                sdk::spawn::run(&script(), &invocation, environment, &sdk::Limits::default(), None).expect("runs");
            String::from_utf8(done.stdout).expect("utf-8")
        };
        assert_eq!(run(None), "null\n");
        assert_eq!(run(Some(Secret::new("s3cret"))), "piped\ns3cret");
    }

    #[test]
    fn output_over_the_limit_is_an_error_and_the_child_is_gone() {
        let directory = TempDir::new().expect("temp dir");
        let pid_file = directory.path().join("pid");
        let (ocx, _) = client(&directory);
        let ocx = ocx
            .with_env("FAKE_MODE", "flood")
            .with_env("FAKE_PID_FILE", &pid_file)
            .with_limits(sdk::Limits {
                max_stdout: 64 * 1024,
                ..Default::default()
            });
        let failed = ocx.call_raw("exec", &[], sdk::wire::Argv::new());
        assert!(matches!(failed, Err(Error::OutputTooLarge)), "{failed:?}");
        let pid = std::fs::read_to_string(&pid_file).expect("the child recorded its pid");
        assert!(!alive(pid.trim()), "the child {pid} outlived the call");
    }

    #[test]
    fn a_zero_cancel_grace_does_not_fail_a_run_that_exited_normally() {
        let directory = TempDir::new().expect("temp dir");
        let (ocx, _) = client(&directory);
        let ocx = ocx.with_env("FAKE_MODE", "env").with_limits(sdk::Limits {
            cancel_grace: Duration::ZERO,
            ..Default::default()
        });
        for _ in 0..50 {
            let done = ocx.call_raw("exec", &[], sdk::wire::Argv::new());
            assert!(done.is_ok(), "{done:?}");
        }
    }

    /// Cancels the token once the child has said it is in its final state, then returns how long the call took and
    /// whether the child saw an interrupt.
    fn cancelled_call(mode: &str, grace: Duration) -> (Duration, bool) {
        let directory = TempDir::new().expect("temp dir");
        let pid_file = directory.path().join("pid");
        let (ocx, _) = client(&directory);
        let token = sdk::CancelToken::new();
        let ocx = ocx
            .with_env("FAKE_MODE", mode)
            .with_env("FAKE_PID_FILE", &pid_file)
            .with_cancel(token.clone())
            .with_limits(sdk::Limits {
                cancel_grace: grace,
                ..Default::default()
            });
        let watched = pid_file.clone();
        let canceller = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !watched.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let at = Instant::now();
            token.cancel();
            at
        });
        let result = ocx.call_raw("exec", &[], sdk::wire::Argv::new());
        let cancelled_at = canceller.join().expect("canceller");
        let elapsed = cancelled_at.elapsed();
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        let pid = std::fs::read_to_string(&pid_file).expect("pid");
        assert!(!alive(pid.trim()), "the child {pid} outlived the call");
        let interrupted = directory.path().join("pid.interrupted").exists();
        (elapsed, interrupted)
    }

    #[test]
    fn cancelling_interrupts_a_child_that_listens_without_waiting_out_the_grace() {
        let (elapsed, interrupted) = cancelled_call("graceful", Duration::from_secs(20));
        assert!(
            interrupted,
            "the child never saw the interrupt, so it was killed outright"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "took {elapsed:?}, so the grace was waited out"
        );
    }

    #[test]
    fn cancelling_kills_a_child_that_ignores_the_interrupt_after_the_grace() {
        let grace = Duration::from_secs(1);
        let (elapsed, interrupted) = cancelled_call("stubborn", grace);
        assert!(!interrupted);
        assert!(elapsed >= grace, "killed after {elapsed:?}, inside the grace");
        assert!(elapsed < grace + Duration::from_secs(3), "took {elapsed:?}");
    }

    /// A child that exits at once while a background process it started keeps its stdout open for a while.
    fn daemonised_call(grace: Duration, token: Option<sdk::CancelToken>) -> (Result<sdk::Raw, Error>, Duration) {
        let directory = TempDir::new().expect("temp dir");
        let (ocx, _) = client(&directory);
        let mut ocx = ocx.with_env("FAKE_MODE", "daemon").with_limits(sdk::Limits {
            cancel_grace: grace,
            ..Default::default()
        });
        if let Some(token) = token {
            ocx = ocx.with_cancel(token);
        }
        let started = Instant::now();
        let result = ocx.call_raw("exec", &[], sdk::wire::Argv::new());
        (result, started.elapsed())
    }

    #[test]
    fn output_held_open_by_a_grandchild_ends_the_call_at_the_bound() {
        let (result, elapsed) = daemonised_call(Duration::from_millis(300), None);
        assert!(matches!(result, Err(Error::Spawn(_))), "{result:?}");
        assert!(
            elapsed < Duration::from_secs(3),
            "the call waited {elapsed:?} for the grandchild"
        );
    }

    #[test]
    fn cancelling_returns_while_a_grandchild_still_holds_the_output() {
        let token = sdk::CancelToken::new();
        let canceller = {
            let token = token.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                token.cancel();
            })
        };
        let (result, elapsed) = daemonised_call(Duration::from_secs(30), Some(token));
        canceller.join().expect("canceller");
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        assert!(
            elapsed < Duration::from_secs(3),
            "the call waited {elapsed:?} for the grandchild"
        );
    }

    fn signature_report(failed_leg: bool) -> Value {
        let leg = |format: &str, error: Option<&str>| match error {
            Some(error) => json!({ "format": format, "error": error }),
            None => json!({ "format": format, "manifest_digest": format!("sha256:{}", "a".repeat(64)) }),
        };
        json!({
            "schema_version": 2,
            "identifier": "ocx.sh/cmake:3",
            "key_backend": "keyless",
            "legs": [leg("bundle", None), leg("simplesigning", failed_leg.then_some("registry refused"))],
            "signer": "keyless-fulcio",
            "subject_digest": format!("sha256:{}", "b".repeat(64)),
        })
    }

    fn sign(ocx: Ocx, stdout: &Value, exit: i32) -> Result<sdk::Raw, Error> {
        ocx.with_env("FAKE_STDOUT", stdout.to_string())
            .with_env("FAKE_EXIT", exit.to_string())
            .package_sign(&sdk::PackageSignArgs {
                identifier: "ocx.sh/cmake:3".to_owned(),
                ..Default::default()
            })
    }

    #[test]
    fn a_partial_sign_is_a_failed_outcome_carrying_its_report() {
        let directory = TempDir::new().expect("temp dir");
        let raw = sign(client(&directory).0, &signature_report(true), 65).expect("a failed exit is not an error yet");
        match raw
            .into_outcome::<SignatureReport>("SignatureReport")
            .expect("a report")
        {
            Outcome::Failed { report, exit_code } => {
                assert_eq!(exit_code, ExitCode::DataError);
                assert_eq!(report.legs.len(), 2);
            }
            other => panic!("{other:?}"),
        }
        let raw = sign(client(&directory).0, &signature_report(false), 0).expect("runs");
        assert!(matches!(
            raw.into_outcome::<SignatureReport>("SignatureReport"),
            Ok(Outcome::Success(_))
        ));
    }

    #[test]
    fn a_mixed_sweep_is_a_failed_outcome_and_its_unknown_status_is_not_success() {
        let sweep = json!({
            "schema_version": 2,
            "items": [
                { "tag": "1", "status": "completed", "report": signature_report(false) },
                { "tag": "2", "status": "failed", "kind": "io_error", "message": "boom" },
                { "tag": "3", "status": "quarantined" },
            ],
        });
        let directory = TempDir::new().expect("temp dir");
        let raw = sign(client(&directory).0, &sweep, 65).expect("runs");
        let Ok(Outcome::Failed { report, exit_code }) =
            raw.into_outcome::<SignatureReportSweep>("SweepReport<SignatureReport>")
        else {
            panic!("a sweep with a failed row and a non-zero exit is a failed outcome");
        };
        assert_eq!(exit_code, ExitCode::DataError);
        let statuses: Vec<_> = report.items.iter().map(|row| row.status.clone()).collect();
        assert_eq!(
            statuses,
            [
                SweptStatus::Completed,
                SweptStatus::Failed,
                SweptStatus::Unknown("quarantined".to_owned())
            ]
        );
        assert_eq!(
            statuses.iter().map(SweptStatus::is_success).collect::<Vec<_>>(),
            [true, false, false]
        );
        assert_eq!(report.unknown_pointers(), ["/items/2/status"]);
    }

    #[test]
    fn an_error_document_is_the_typed_error_whatever_the_command() {
        let directory = TempDir::new().expect("temp dir");
        let document = json!({
            "schema_version": contract::ERRORS,
            "command": "package sign",
            "exit_code": 79,
            "error": { "kind": "not_found", "message": "gone", "context": {} },
        });
        let failed = sign(client(&directory).0, &document, 79);
        assert!(matches!(failed, Err(Error::Ocx(_))), "{failed:?}");
    }

    #[test]
    fn a_report_command_that_exits_non_zero_without_its_report_is_an_error() {
        let directory = TempDir::new().expect("temp dir");
        let document = json!({
            "schema_version": contract::ERRORS,
            "command": "pull",
            "exit_code": 1,
            "error": { "kind": "internal", "message": "boom", "context": {} },
        });
        let pull = |stdout: &str| {
            client(&directory)
                .0
                .with_env("FAKE_STDOUT", stdout)
                .with_env("FAKE_EXIT", "1")
                .pull(&sdk::PullArgs::default())
        };
        let documented = pull(&document.to_string());
        assert!(matches!(documented, Err(Error::Ocx(_))), "{documented:?}");
        let bare = pull("");
        assert!(matches!(bare, Err(Error::Exited { status: Some(1), .. })), "{bare:?}");
    }

    fn error_document(command: &str) -> String {
        json!({
            "schema_version": contract::ERRORS,
            "command": command,
            "exit_code": 79,
            "error": { "kind": "not_found", "message": "gone", "context": {} },
        })
        .to_string()
    }

    #[test]
    fn a_command_that_runs_another_program_returns_its_exit() {
        let directory = TempDir::new().expect("temp dir");
        let exec = |stdout: &str, exit: &str| {
            client(&directory)
                .0
                .with_env("FAKE_STDOUT", stdout)
                .with_env("FAKE_EXIT", exit)
                .exec(&sdk::ExecArgs {
                    argv: vec!["false".to_owned()],
                    ..Default::default()
                })
        };
        let raw = exec("child output", "1").expect("the child's exit is the result, not an error");
        assert_eq!(raw.exit_code.value(), 1);
        assert_eq!(raw.stdout, b"child output");
        let refused = exec(&error_document("exec"), "79");
        assert!(matches!(refused, Err(Error::Ocx(_))), "ocx's own refusal: {refused:?}");
    }

    #[test]
    fn a_passthrough_child_that_prints_a_look_alike_error_document_keeps_its_output() {
        let directory = TempDir::new().expect("temp dir");
        let package_test = |stdout: &str| {
            client(&directory)
                .0
                .with_env("FAKE_STDOUT", stdout)
                .with_env("FAKE_EXIT", "1")
                .package_test(&sdk::PackageTestArgs::default())
        };
        // Three keys of ocx's error document, but not the document: no `schema_version`, no typed `error`.
        let loose = r#"{"command":"build","exit_code":1,"error":"failed"}"#;
        let raw = package_test(loose).expect("a child's own JSON is its output, not ocx's refusal");
        assert_eq!(raw.exit_code.value(), 1);
        assert_eq!(raw.stdout, loose.as_bytes());
        // A genuine ocx error document, but for another command: still the child's output.
        let other = error_document("exec");
        let raw = package_test(&other).expect("a document naming another command is not this call's refusal");
        assert_eq!(raw.stdout, other.as_bytes());
        // The control: the same document naming this command is ocx's refusal.
        let refused = package_test(&error_document("package test"));
        assert!(matches!(refused, Err(Error::Ocx(_))), "{refused:?}");
    }

    #[test]
    fn a_shell_stream_command_that_exits_non_zero_is_an_error() {
        let directory = TempDir::new().expect("temp dir");
        let completion = |stdout: &str| {
            client(&directory)
                .0
                .with_env("FAKE_STDOUT", stdout)
                .with_env("FAKE_EXIT", "79")
                .shell_completion(&sdk::ShellCompletionArgs::default())
        };
        let bare = completion("partial script");
        assert!(matches!(bare, Err(Error::Exited { status: Some(79), .. })), "{bare:?}");
        let documented = completion(&error_document("shell completion"));
        assert!(matches!(documented, Err(Error::Ocx(_))), "{documented:?}");
    }

    #[test]
    fn a_report_command_that_also_runs_a_child_keeps_the_childs_failed_output() {
        let directory = TempDir::new().expect("temp dir");
        let raw = client(&directory)
            .0
            .with_env("FAKE_STDOUT", "child output")
            .with_env("FAKE_EXIT", "1")
            .package_test(&sdk::PackageTestArgs::default())
            .expect("a failing child in passthrough mode is the result, not an error");
        assert_eq!(raw.exit_code.value(), 1);
        assert_eq!(raw.stdout, b"child output");
    }
}

mod decoding {
    use super::*;

    #[test]
    fn a_known_union_tag_missing_a_required_field_is_an_error_not_unknown() {
        let missing = serde_json::from_value::<sdk::types::Note>(json!({"type": "active_via_paths_grant"}));
        let error = missing.expect_err("`entry` is required by the known tag");
        assert!(error.to_string().contains("active_via_paths_grant"), "{error}");
        let unknown = serde_json::from_value::<sdk::types::Note>(json!({"type": "from_the_future", "x": 1}))
            .expect("a tag the SDK does not know decodes");
        assert_eq!(unknown.unknown_pointers(), [""]);
    }

    #[test]
    fn only_success_is_success_for_a_known_failure_and_an_unknown_code() {
        assert!(ExitCode::Success.is_success());
        assert!(!ExitCode::DataError.is_success());
        assert!(!ExitCode::Unknown(99).is_success());
    }
}

/// Everything here runs the real `ocx` and fails when the build gave it no binary.
mod real_binary {
    use super::*;

    fn client(home: &TempDir) -> Ocx {
        let binary =
            REAL_BINARY.expect("SDK_TEST_OCX_BINARY names the ocx binary under test; run through the Bazel target");
        Ocx::new(binary)
            .expect("the binary exists")
            .with_env("OCX_HOME", home.path())
            .with_globals(sdk::GlobalOptions {
                offline: true,
                ..Default::default()
            })
    }

    #[test]
    fn the_binary_speaks_the_contract_the_sdk_was_generated_for() {
        let home = TempDir::new().expect("temp dir");
        let reported = client(&home).handshake().expect("the handshake decodes");
        assert_eq!(reported.contract.errors, i64::from(contract::ERRORS));
        for (name, version) in contract::COMMANDS {
            assert_eq!(
                reported.contract.commands.get(*name),
                Some(&i64::from(*version)),
                "command {name}"
            );
        }
        for (name, version) in contract::REPORTS {
            assert_eq!(
                reported.contract.reports.get(*name),
                Some(&i64::from(*version)),
                "report {name}"
            );
        }
    }

    #[test]
    fn a_report_command_decodes_into_its_typed_report() {
        let home = TempDir::new().expect("temp dir");
        let about = client(&home).about().expect("`ocx about` succeeds");
        assert!(!about.version.is_empty());
    }

    #[test]
    fn a_multiword_exec_command_is_parsed_by_the_binary_not_rejected_as_usage() {
        let home = TempDir::new().expect("temp dir");
        for command in [vec!["sh", "-c", "true"], vec!["--env=LD_PRELOAD=x", "--", "sh"]] {
            let ran = client(&home).package_exec(&sdk::PackageExecArgs {
                packages: vec!["example.invalid/none:1".to_owned()],
                command: command.iter().map(|word| (*word).to_owned()).collect(),
                ..Default::default()
            });
            // ocx refuses the unresolvable package with its error document; a usage error would carry exit 64.
            let exit_code = match &ran {
                Ok(raw) => raw.exit_code.clone(),
                Err(Error::Ocx(document)) => document.exit_code.clone(),
                Err(other) => panic!("`{command:?}`: {other:?}"),
            };
            assert_ne!(
                exit_code,
                ExitCode::UsageError,
                "`{command:?}` was a usage error: {ran:?}"
            );
        }
    }

    #[test]
    fn a_failing_command_is_the_typed_error_document() {
        let home = TempDir::new().expect("temp dir");
        let failed = client(&home)
            .package_which(&sdk::PackageWhichArgs {
                packages: vec!["example.invalid/none:1".to_owned()],
                ..Default::default()
            })
            .expect_err("nothing is installed in an empty home");
        let Error::Ocx(document) = failed else {
            panic!("expected an error document, got {failed:?}");
        };
        assert!(!document.exit_code.is_success(), "{document:?}");
        assert_eq!(document.command, "package which");
    }
}
