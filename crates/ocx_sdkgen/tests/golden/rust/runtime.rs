// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The client: [`Ocx`] resolves the binary once, refuses a command whose contract version differs from the one this
//! SDK was generated for before it spawns anything, and decodes what the process printed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::commands::GlobalOptions;
use super::contract;
use super::env;
use super::spawn;
use super::types::{ContractVersions, ErrorDocument, ExitCode, VersionData};
use super::wire::Argv;

/// Why a call failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// No `ocx` binary was found: the pin names none and none is on `PATH`.
    BinaryNotFound,
    /// The process could not be started, waited on or its binary resolved, or its output stayed open after it exited.
    Spawn(std::io::Error),
    /// `ocx` failed and printed an error document: typed exit code, category and detail.
    Ocx(Box<ErrorDocument>),
    /// `ocx` failed without an error document, or a signal ended it.
    Exited { status: Option<i32>, stderr: String },
    /// The contract version of `subject` differs from the one this SDK was generated for.
    ContractMismatch { subject: String, expected: u32, found: u32 },
    /// The binary predates the machine-interface contract.
    Unsupported { found: String, minimum: &'static str },
    /// An argument was refused before spawning.
    InvalidArgument { arg: &'static str, reason: &'static str },
    /// The output is not the document the contract describes.
    Decode(serde_json::Error),
    /// Stdout or stderr passed its limit; the process was killed.
    OutputTooLarge,
    /// The call was cancelled; the process was interrupted, then killed when it did not exit within the grace period.
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BinaryNotFound => write!(formatter, "no ocx binary found"),
            Self::Spawn(error) => write!(formatter, "cannot run ocx: {error}"),
            Self::Ocx(document) => write!(formatter, "ocx failed: {}", document.error.message),
            Self::Exited { status, stderr } => match status {
                Some(status) => write!(formatter, "ocx exited with status {status}: {stderr}"),
                None => write!(formatter, "ocx was ended by a signal: {stderr}"),
            },
            Self::ContractMismatch {
                subject,
                expected,
                found,
            } => write!(
                formatter,
                "{subject} is at contract version {found}, expected {expected}"
            ),
            Self::Unsupported { found, minimum } => {
                write!(
                    formatter,
                    "ocx {found} predates the contract, which starts at {minimum}"
                )
            }
            Self::InvalidArgument { arg, reason } => write!(formatter, "argument `{arg}`: {reason}"),
            Self::Decode(error) => write!(formatter, "unexpected ocx output: {error}"),
            Self::OutputTooLarge => write!(formatter, "ocx output passed its limit"),
            Self::Cancelled => write!(formatter, "cancelled"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::Decode(error) => Some(error),
            _ => None,
        }
    }
}

/// What a command that can print a report and still fail returns.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome<R> {
    Success(R),
    /// A report on stdout and a non-zero exit.
    Failed {
        report: R,
        exit_code: ExitCode,
    },
}

/// What a command returns when its stdout is not one report: the child's own output, or one of several documents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raw {
    pub exit_code: ExitCode,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Raw {
    /// Decodes stdout as the report root `root`, checking its `schema_version` first.
    ///
    /// # Errors
    ///
    /// [`Error::ContractMismatch`] on another `schema_version`, [`Error::Decode`] when stdout is not that report.
    pub fn decode<R: DeserializeOwned>(&self, root: &'static str) -> Result<R, Error> {
        decode_report(root, &self.stdout)
    }

    /// Reads the run as a command that can print a report and still fail: a report and exit 0 is
    /// [`Outcome::Success`], a report and a non-zero exit [`Outcome::Failed`], and anything else is the error the
    /// failure stands for.
    ///
    /// # Errors
    ///
    /// [`Error::Ocx`] for an error document, [`Error::Exited`] for a failure that printed nothing the contract
    /// describes, [`Error::ContractMismatch`] and [`Error::Decode`] as for [`Raw::decode`].
    pub fn into_outcome<R: DeserializeOwned>(self, root: &'static str) -> Result<Outcome<R>, Error> {
        if self.exit_code.is_success() {
            return decode_report(root, &self.stdout).map(Outcome::Success);
        }
        if parse_error_document(&self.stdout)?.is_none() && !self.stdout.is_empty() {
            let report = decode_report(root, &self.stdout)?;
            return Ok(Outcome::Failed {
                report,
                exit_code: self.exit_code,
            });
        }
        Err(failure(self))
    }
}

/// How much output a call may collect before its process is killed, and how long a cancelled one may wind down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_stdout: usize,
    pub max_stderr: usize,
    /// How long a cancelled process has to exit after the interrupt before it is killed, and how long its output may
    /// stay open once it has exited. Windows has no interrupt to send, so a cancelled process there is killed at
    /// once.
    pub cancel_grace: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_stdout: 64 * 1024 * 1024,
            max_stderr: 4 * 1024 * 1024,
            cancel_grace: Duration::from_secs(5),
        }
    }
}

/// Ends the calls it is attached to: a running process is interrupted, killed when it outlasts
/// [`Limits::cancel_grace`], and the call returns [`Error::Cancelled`].
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// What the running binary reports about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Handshake {
    pub version: String,
    pub contract: ContractVersions,
}

/// One `ocx` binary and how to run it.
#[derive(Clone)]
pub struct Ocx {
    binary: PathBuf,
    globals: GlobalOptions,
    environment: Vec<(OsString, OsString)>,
    limits: Limits,
    cancel: Option<CancelToken>,
    handshake: OnceLock<Handshake>,
}

impl std::fmt::Debug for Ocx {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let environment: Vec<_> = self
            .environment
            .iter()
            .map(|(key, value)| {
                let secret = key.to_str().is_some_and(env::is_secret);
                (
                    key,
                    if secret {
                        OsString::from("<redacted>")
                    } else {
                        value.clone()
                    },
                )
            })
            .collect();
        formatter
            .debug_struct("Ocx")
            .field("binary", &self.binary)
            .field("globals", &self.globals)
            .field("environment", &environment)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Ocx {
    /// Uses the binary at `binary`, resolved to an absolute path now so a later `PATH` change cannot swap it.
    ///
    /// # Errors
    ///
    /// [`Error::Spawn`] when the path cannot be resolved or names no file.
    pub fn new(binary: impl Into<PathBuf>) -> Result<Self, Error> {
        let binary = std::path::absolute(binary.into()).map_err(Error::Spawn)?;
        if !binary.metadata().map_err(Error::Spawn)?.is_file() {
            return Err(Error::Spawn(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the ocx binary is not a file",
            )));
        }
        Ok(Self {
            binary,
            globals: GlobalOptions::default(),
            environment: Vec::new(),
            limits: Limits::default(),
            cancel: None,
            handshake: OnceLock::new(),
        })
    }

    /// Finds the binary: `OCX_BINARY_PIN` first, then the first `ocx` on `PATH` (`ocx.exe` on Windows).
    ///
    /// # Errors
    ///
    /// [`Error::BinaryNotFound`] when neither names a file.
    pub fn discover() -> Result<Self, Error> {
        let name = if cfg!(windows) { "ocx.exe" } else { "ocx" };
        locate(&spawn::parent_environment(), name).and_then(Self::new)
    }

    /// Sets a variable in every child's environment, after the inherited ones are scrubbed.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }

    /// The options placed before every command.
    #[must_use]
    pub fn with_globals(mut self, globals: GlobalOptions) -> Self {
        self.globals = globals;
        self
    }

    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Ends every call of this client when `cancel` fires.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Asks the binary for its version and contract (`ocx --format json version`); the answer is kept, so every
    /// command after the first reuses it.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when the binary predates the contract, and the errors of running it.
    pub fn handshake(&self) -> Result<Handshake, Error> {
        if let Some(handshake) = self.handshake.get() {
            return Ok(handshake.clone());
        }
        let fetched = self.fetch_handshake()?;
        Ok(self.handshake.get_or_init(|| fetched).clone())
    }

    fn fetch_handshake(&self) -> Result<Handshake, Error> {
        let mut argv = Argv::new();
        argv.word("--format=json");
        argv.word("version");
        let done = self.execute(argv)?;
        if !done.exit_code.is_success() {
            return Err(failure(done));
        }
        let document: Value = serde_json::from_slice(&done.stdout).map_err(Error::Decode)?;
        let version = document
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        if document.get("contract").is_none() {
            return Err(Error::Unsupported {
                found: version,
                minimum: contract::MINIMUM_OCX,
            });
        }
        check_version(
            "report `VersionData`",
            contract::report_version("VersionData"),
            &document,
        )?;
        let report: VersionData = serde_json::from_value(document).map_err(Error::Decode)?;
        Ok(Handshake {
            version: report.version,
            contract: report.contract,
        })
    }

    /// The argv head of every command: the JSON format, the global options and the command's words.
    pub fn argv(&self, words: &[&str]) -> Argv {
        let mut argv = Argv::new();
        argv.word("--format=json");
        self.globals.push(&mut argv);
        for word in words {
            argv.word(word);
        }
        argv
    }

    /// Refuses a command before it runs when the binary's contract version of the command, of an output root or of
    /// the error document is not the one this SDK was generated for.
    fn verify(&self, command: &'static str, roots: &[&'static str]) -> Result<(), Error> {
        let handshake = self.handshake()?;
        let versions = &handshake.contract;
        expect_version("the error document", contract::ERRORS, Some(versions.errors))?;
        let found = versions.commands.get(command).copied();
        expect_version(
            &format!("command `{command}`"),
            contract::command_version(command),
            found,
        )?;
        for root in roots {
            let found = versions.reports.get(*root).copied();
            expect_version(&format!("report `{root}`"), contract::report_version(root), found)?;
        }
        Ok(())
    }

    fn execute(&self, argv: Argv) -> Result<Raw, Error> {
        let invocation = argv.finish()?;
        let environment = spawn::child_environment(spawn::parent_environment(), &self.environment, &self.binary);
        let output = spawn::run(
            &self.binary,
            &invocation,
            environment,
            &self.limits,
            self.cancel.as_ref(),
        )?;
        let Some(status) = output.status else {
            return Err(Error::Exited {
                status: None,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        };
        Ok(Raw {
            exit_code: ExitCode::from_value(i64::from(status)),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    /// A command that prints one report and exits 0.
    pub fn call<R: DeserializeOwned>(&self, command: &'static str, root: &'static str, argv: Argv) -> Result<R, Error> {
        self.verify(command, &[root])?;
        let done = self.execute(argv)?;
        if done.exit_code.is_success() {
            decode_report(root, &done.stdout)
        } else {
            Err(failure(done))
        }
    }

    /// A command that can print a report and still exit non-zero.
    pub fn call_outcome<R: DeserializeOwned>(
        &self,
        command: &'static str,
        root: &'static str,
        argv: Argv,
    ) -> Result<Outcome<R>, Error> {
        self.verify(command, &[root])?;
        self.execute(argv)?.into_outcome(root)
    }

    /// A command that prints nothing on success.
    pub fn call_empty(&self, command: &'static str, argv: Argv) -> Result<(), Error> {
        self.verify(command, &[])?;
        let done = self.execute(argv)?;
        if done.exit_code.is_success() {
            Ok(())
        } else {
            Err(failure(done))
        }
    }

    /// A command whose stdout the caller interprets.
    pub fn call_raw(&self, command: &'static str, roots: &[&'static str], argv: Argv) -> Result<Raw, Error> {
        self.verify(command, roots)?;
        self.execute(argv)
    }
}

/// The error a non-zero exit becomes: the error document when stdout is one, the bare exit otherwise.
fn failure(done: Raw) -> Error {
    match parse_error_document(&done.stdout) {
        Ok(Some(document)) => Error::Ocx(Box::new(document)),
        Ok(None) => Error::Exited {
            status: i32::try_from(done.exit_code.value()).ok(),
            stderr: String::from_utf8_lossy(&done.stderr).into_owned(),
        },
        Err(error) => error,
    }
}

/// `Ok(None)` when `stdout` is not an error document at all.
fn parse_error_document(stdout: &[u8]) -> Result<Option<ErrorDocument>, Error> {
    let Ok(document) = serde_json::from_slice::<Value>(stdout) else {
        return Ok(None);
    };
    if !["command", "exit_code", "error"]
        .iter()
        .all(|key| document.get(key).is_some())
    {
        return Ok(None);
    }
    check_version("the error document", contract::ERRORS, &document)?;
    serde_json::from_value(document).map(Some).map_err(Error::Decode)
}

/// Decodes `stdout` as the report root `root`, after its `schema_version` matches the generated one.
pub fn decode_report<R: DeserializeOwned>(root: &'static str, stdout: &[u8]) -> Result<R, Error> {
    let document: Value = serde_json::from_slice(stdout).map_err(Error::Decode)?;
    check_version(&format!("report `{root}`"), contract::report_version(root), &document)?;
    serde_json::from_value(document).map_err(Error::Decode)
}

/// Refuses `document` when its `schema_version` is not `expected`.
pub fn check_version(subject: &str, expected: u32, document: &Value) -> Result<(), Error> {
    expect_version(
        subject,
        expected,
        document.get("schema_version").and_then(Value::as_i64),
    )
}

fn expect_version(subject: &str, expected: u32, found: Option<i64>) -> Result<(), Error> {
    if found == Some(i64::from(expected)) {
        return Ok(());
    }
    Err(Error::ContractMismatch {
        subject: subject.to_owned(),
        expected,
        found: found.and_then(|found| u32::try_from(found).ok()).unwrap_or(0),
    })
}

/// The binary `environment` names: the file `OCX_BINARY_PIN` holds, else the first `executable` on an absolute `PATH`
/// entry. Only the exact file name is looked for, so `ocx.exe` on Windows never matches a bare `ocx`.
///
/// # Errors
///
/// [`Error::BinaryNotFound`] when neither names a file.
pub fn locate(environment: &[(OsString, OsString)], executable: &str) -> Result<PathBuf, Error> {
    let lookup = |name: &str| {
        environment
            .iter()
            .find(|(key, _)| key.to_str().is_some_and(|key| key.eq_ignore_ascii_case(name)))
            .map(|(_, value)| value.clone())
    };
    if let Some(pin) = lookup("OCX_BINARY_PIN").filter(|pin| !pin.is_empty()) {
        return Ok(PathBuf::from(pin));
    }
    let path = lookup("PATH").ok_or(Error::BinaryNotFound)?;
    // An empty or relative entry means the current directory, where a planted `ocx` would be run in place of the
    // real one.
    std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(executable))
        .find(|candidate| candidate.is_file())
        .ok_or(Error::BinaryNotFound)
}
