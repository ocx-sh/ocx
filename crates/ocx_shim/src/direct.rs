// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The vendored (`.direct`) launcher: decoding its sidecar and finding the vendor root it ships in.
//!
//! Every filesystem question goes through [`LookupFs`], so the lookup rules run in unit tests without a disk.

use super::ShimError;
use super::core;
use ocx_env_apply::direct::{DirectSidecar, EnvRecord, VENDOR_DIR_TOKEN};
use ocx_env_apply::{ApplyKind, KeyFold, apply_one};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// A sidecar is at most this large; a larger file is refused unread past the bound.
pub(super) const MAX_SIDECAR_BYTES: u64 = 1024 * 1024;

/// At most this many directory entries of a runfiles tree are examined, in name order.
const MAX_RUNFILES_ENTRIES: usize = 4096;

/// How many root candidates the "no root found" line names.
const NAMED_CANDIDATES: usize = 8;

/// Why a sidecar read was refused.
#[derive(Debug)]
pub(super) enum ReadError {
    /// The path is not a regular file (a FIFO, a directory, a device).
    NotRegular,
    /// The file holds more than [`MAX_SIDECAR_BYTES`].
    TooLarge,
    /// Any other I/O failure, `NotFound` excepted.
    Io(std::io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::NotRegular => f.write_str("not a regular file"),
            ReadError::TooLarge => write!(f, "larger than {MAX_SIDECAR_BYTES} bytes"),
            ReadError::Io(err) => write!(f, "{err}"),
        }
    }
}

/// The filesystem questions the lookup asks.
pub(super) trait LookupFs {
    /// The bytes of the sidecar at `path`; `Ok(None)` when nothing is there.
    fn read_sidecar(&self, path: &str) -> Result<Option<Vec<u8>>, ReadError>;
    /// Whether anything exists at `path`.
    fn exists(&self, path: &str) -> bool;
    /// The entry names of directory `dir`; empty when it is missing or unreadable.
    fn list_dir(&self, dir: &str) -> Vec<String>;
    /// Whether `path` is a regular executable file.
    #[cfg(unix)]
    fn is_executable_file(&self, path: &str) -> bool;
    /// Whether `path` has the device and inode of the running image.
    #[cfg(unix)]
    fn is_running_image(&self, path: &str) -> bool;
}

/// What the lookup needs beyond the started path.
pub(super) struct LookupContext<'a> {
    /// The working directory, used to make relative paths absolute.
    pub cwd: &'a str,
    /// The launcher's own `RUNFILES_DIR`.
    pub runfiles_dir: Option<&'a str>,
    /// The target platform: separators, case folding and the path-list separator.
    pub windows: bool,
}

/// The sidecar kind the probe found.
#[derive(Debug)]
pub(super) enum Probed {
    /// An existing kind; only a Windows launcher can see one, and its own path handles it.
    Legacy(&'static str),
    /// A decoded `.direct` sidecar and the root it was found for.
    Direct(Located),
}

/// A decoded `.direct` sidecar bound to its vendor root.
#[derive(Debug)]
pub(super) struct Located {
    pub sidecar: DirectSidecar,
    /// The vendor root: absolute, lexically joined, no symlink resolved.
    pub root: String,
}

fn refuse(reason: String) -> ShimError {
    ShimError::DirectRefused { reason }
}

/// Reads the file at `path`: opened non-blocking, required to be a regular file, at most [`MAX_SIDECAR_BYTES`].
pub(super) fn read_regular_bounded(path: &Path) -> Result<Option<Vec<u8>>, ReadError> {
    use std::io::Read;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A blocking `open` of a FIFO waits for a writer and would hang the launcher.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        // Windows refuses to open a directory without backup semantics, so access denied there can mean "not a file".
        Err(err)
            if cfg!(windows)
                && err.kind() == std::io::ErrorKind::PermissionDenied
                && std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) =>
        {
            return Err(ReadError::NotRegular);
        }
        Err(err) => return Err(ReadError::Io(err)),
    };
    // The check runs on the opened handle, so a path swapped for a FIFO after the open changes nothing.
    let metadata = file.metadata().map_err(ReadError::Io)?;
    if !metadata.is_file() {
        return Err(ReadError::NotRegular);
    }
    if metadata.len() > MAX_SIDECAR_BYTES {
        return Err(ReadError::TooLarge);
    }
    let mut bytes = Vec::new();
    // One byte past the bound: a file that grew after the `metadata` call still trips the size check.
    file.take(MAX_SIDECAR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(ReadError::Io)?;
    if bytes.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(ReadError::TooLarge);
    }
    Ok(Some(bytes))
}

/// Decodes and validates a sidecar read from `path`.
pub(super) fn decode_sidecar(raw: &[u8], path: &str, windows: bool) -> Result<DirectSidecar, ShimError> {
    if raw.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(refuse(format!(
            "sidecar {path} is larger than {MAX_SIDECAR_BYTES} bytes"
        )));
    }
    let sidecar: DirectSidecar =
        serde_json::from_slice(raw).map_err(|err| refuse(format!("malformed sidecar {path}: {err}")))?;
    sidecar
        .validate(windows)
        .map_err(|err| refuse(format!("malformed sidecar {path}: {err}")))?;
    // A NUL ends a Windows environment-block entry early, so the rest of the value would become a variable of its own.
    let mut texts = sidecar.args.iter().chain(
        sidecar
            .env
            .iter()
            .flat_map(|record| std::iter::once(&record.value).chain(record.separator.as_ref())),
    );
    if texts.any(|text| text.contains('\0')) {
        return Err(refuse(format!(
            "malformed sidecar {path}: an argument or environment value contains NUL"
        )));
    }
    Ok(sidecar)
}

fn is_absolute(path: &str, windows: bool) -> bool {
    if windows {
        core::is_absolute_path(path)
    } else {
        path.starts_with('/')
    }
}

fn separators(windows: bool) -> &'static [char] {
    if windows { &['/', '\\'] } else { &['/'] }
}

fn join_path(dir: &str, name: &str, windows: bool) -> String {
    format!("{}/{name}", dir.trim_end_matches(separators(windows)))
}

/// Makes `path` absolute by a lexical join onto `cwd`: `.` and empty segments go, `..` and symlinks are left alone.
fn absolutize(path: &str, cwd: &str, windows: bool) -> String {
    if is_absolute(path, windows) {
        return path.to_string();
    }
    let mut joined = cwd.trim_end_matches(separators(windows)).to_string();
    for segment in path.split(separators(windows)).filter(|s| !s.is_empty() && *s != ".") {
        joined.push('/');
        joined.push_str(segment);
    }
    if joined.is_empty() { "/".to_string() } else { joined }
}

/// `path` minus one trailing `.exe`, ASCII case-insensitive.
fn strip_exe(path: &str) -> &str {
    match path
        .len()
        .checked_sub(4)
        .and_then(|cut| path.get(cut..).map(|tail| (cut, tail)))
    {
        Some((cut, tail)) if tail.eq_ignore_ascii_case(".exe") => &path[..cut],
        _ => path,
    }
}

/// `path` split after its last separator into the directory (separator kept) and the file name.
fn split_file_name(path: &str, windows: bool) -> (&str, &str) {
    match path.rfind(separators(windows)) {
        Some(at) => path.split_at(at + 1),
        None => ("", path),
    }
}

/// Folds a path for a lexical comparison: on Windows ASCII case and `\` fold, on Unix nothing does.
fn fold(text: &str, windows: bool) -> String {
    if windows {
        text.to_ascii_lowercase().replace('\\', "/")
    } else {
        text.to_string()
    }
}

/// Candidate (a): the started path minus `.exe`, when it ends with `/<self>`; the prefix is the root.
fn own_root(started: &str, self_path: &str, windows: bool) -> Option<String> {
    let stripped = strip_exe(started);
    let at = stripped.len().checked_sub(self_path.len() + 1)?;
    let tail = stripped.get(at..)?;
    let rest = tail.strip_prefix(separators(windows))?;
    if fold(rest, windows) != fold(self_path, windows) {
        return None;
    }
    let prefix = &stripped[..at];
    Some(if prefix.is_empty() {
        "/".to_string()
    } else {
        prefix.to_string()
    })
}

/// The entries of `dir`, in name order and capped, each joined onto it.
fn entry_roots(dir: &str, windows: bool, fs: &dyn LookupFs) -> Vec<String> {
    let mut names = fs.list_dir(dir);
    names.sort();
    names.truncate(MAX_RUNFILES_ENTRIES);
    names.iter().map(|name| join_path(dir, name, windows)).collect()
}

/// Candidate (c): the entries of `$RUNFILES_DIR`, only when the started path lies lexically under it.
fn environment_runfiles_roots(started: &str, context: &LookupContext<'_>, fs: &dyn LookupFs) -> Vec<String> {
    let Some(runfiles_dir) = context.runfiles_dir else {
        return Vec::new();
    };
    let directory = absolutize(runfiles_dir, context.cwd, context.windows);
    let prefix = format!(
        "{}/",
        fold(&directory, context.windows).trim_end_matches(separators(context.windows))
    );
    if !fold(started, context.windows).starts_with(&prefix) {
        return Vec::new();
    }
    entry_roots(&directory, context.windows, fs)
}

/// Whether `<root>/<self>.direct` is there and byte-identical to the sidecar beside the started path.
fn accepts(root: &str, self_path: &str, raw: &[u8], windows: bool, fs: &dyn LookupFs) -> Result<bool, ShimError> {
    let path = format!(
        "{}/{self_path}.{}",
        root.trim_end_matches(separators(windows)),
        core::DIRECT_EXTENSION
    );
    match fs.read_sidecar(&path) {
        Ok(found) => Ok(found.is_some_and(|bytes| bytes == raw)),
        Err(err) => Err(refuse(format!("cannot read sidecar {path}: {err}"))),
    }
}

/// The started path on Unix: `argv[0]` joined onto the working directory, or the first `PATH` hit for a bare name.
///
/// The result must be the running image: a `PATH` search can land on a file the process never started from.
#[cfg(unix)]
pub(super) fn started_path_unix(
    argv0: &str,
    cwd: &str,
    path_var: Option<&str>,
    fs: &dyn LookupFs,
) -> Result<String, ShimError> {
    let started = if argv0.contains('/') {
        absolutize(argv0, cwd, false)
    } else {
        path_var
            .unwrap_or_default()
            .split(':')
            .map(|entry| join_path(&absolutize(entry, cwd, false), argv0, false))
            .find(|candidate| fs.is_executable_file(candidate))
            .ok_or(ShimError::SelfPathFailure)?
    };
    if !fs.is_running_image(&started) {
        return Err(refuse(format!("started path {started} is not the running image")));
    }
    Ok(started)
}

/// Finds the sidecar beside `started` and the vendor root it belongs to.
///
/// The root is the first candidate, in candidate order, whose own `<self>.direct` is byte-identical to the sidecar
/// beside the started path, so a stale tree holding another `vendor_id` is never taken.
pub(super) fn locate(started: &str, context: &LookupContext<'_>, fs: &dyn LookupFs) -> Result<Probed, ShimError> {
    let windows = context.windows;
    let (directory, file_name) = split_file_name(started, windows);
    let stem = strip_exe(file_name);
    if stem.is_empty() {
        return Err(ShimError::SelfPathFailure);
    }

    let mut probed_paths = Vec::new();
    let mut found = None;
    for extension in core::probe_extensions(windows) {
        let path = format!("{directory}{stem}.{extension}");
        if extension != core::DIRECT_EXTENSION {
            if fs.exists(&path) {
                return Ok(Probed::Legacy(extension));
            }
        } else {
            match fs.read_sidecar(&path) {
                Ok(Some(raw)) => {
                    found = Some((path, raw));
                    break;
                }
                Ok(None) => {}
                Err(err) => return Err(refuse(format!("cannot read sidecar {path}: {err}"))),
            }
        }
        probed_paths.push(path);
    }
    let Some((sidecar_path, raw)) = found else {
        return Err(refuse(format!(
            "sidecar not found: none of {}",
            probed_paths.join(", ")
        )));
    };
    let sidecar = decode_sidecar(&raw, &sidecar_path, windows)?;

    let mut rejected: Vec<String> = Vec::new();
    for stage in 0..3 {
        let roots = match stage {
            0 => own_root(started, &sidecar.self_path, windows).into_iter().collect(),
            1 => entry_roots(&format!("{started}.runfiles"), windows, fs),
            _ => environment_runfiles_roots(started, context, fs),
        };
        for candidate in roots {
            if accepts(&candidate, &sidecar.self_path, &raw, windows, fs)? {
                let root = absolutize(&candidate, context.cwd, windows);
                let list_separator = if windows { ';' } else { ':' };
                if root.contains(list_separator) {
                    return Err(refuse(format!(
                        "vendor root {root} contains the path-list separator '{list_separator}'"
                    )));
                }
                return Ok(Probed::Direct(Located { sidecar, root }));
            }
            rejected.push(candidate);
        }
    }

    let named: Vec<&str> = rejected.iter().take(NAMED_CANDIDATES).map(String::as_str).collect();
    Err(refuse(format!(
        "no vendored root found for {}: {} candidate(s) rejected: {}",
        sidecar.self_path,
        rejected.len(),
        if named.is_empty() {
            "none".to_string()
        } else {
            named.join(", ")
        }
    )))
}

/// `text` with every vendor-directory token replaced by `root`.
fn expand(text: &str, root: &str) -> String {
    text.replace(VENDOR_DIR_TOKEN, root)
}

/// What the launcher starts: the program, its arguments after the baked ones, and the complete child environment.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Launch {
    pub program: String,
    /// The baked arguments, then the caller's, in the order `ocx exec` gives them.
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

/// The child environment: `inherited`, minus every `unset` key, then each record applied in order.
///
/// On Windows keys fold ASCII case, so `Path` and `PATH` are one variable and the inherited spelling survives.
fn build_env(
    inherited: Vec<(OsString, OsString)>,
    sidecar: &DirectSidecar,
    root: &str,
    windows: bool,
) -> Vec<(OsString, OsString)> {
    let fold = if windows {
        KeyFold::AsciiCaseInsensitive
    } else {
        KeyFold::Exact
    };
    let folded = |key: &OsStr| match fold {
        KeyFold::Exact => key.to_owned(),
        KeyFold::AsciiCaseInsensitive => key.to_ascii_uppercase(),
    };
    // Folded key to the spelling it was first seen under and its value.
    let mut env: BTreeMap<OsString, (OsString, OsString)> = BTreeMap::new();
    for (key, value) in inherited {
        env.insert(folded(&key), (key, value));
    }
    for key in &sidecar.unset {
        env.remove(&folded(OsStr::new(key)));
    }
    for record in &sidecar.env {
        let slot = folded(OsStr::new(&record.key));
        let existing = env.get(&slot);
        let value = apply_record(
            record,
            existing.map(|(_, value)| value.as_os_str()),
            root,
            fold,
            windows,
        );
        let spelling = existing.map_or_else(|| OsString::from(&record.key), |(spelling, _)| spelling.clone());
        env.insert(slot, (spelling, value));
    }
    env.into_values().collect()
}

fn apply_record(record: &EnvRecord, existing: Option<&OsStr>, root: &str, fold: KeyFold, windows: bool) -> OsString {
    let mut value = expand(&record.value, root);
    if windows && record.kind == ApplyKind::Path {
        value = value.replace('/', "\\");
    }
    apply_one(
        existing,
        record.kind,
        OsStr::new(&value),
        record.separator.as_deref(),
        fold,
    )
}

/// Assembles the launch of a located sidecar over the caller's environment and arguments.
///
/// `argv[0]` is the program path itself, which is what `ocx exec` passes for the same tool. On Windows `/` becomes
/// `\` in the root, the program and `path` values, as the native install path `ocx exec` composes spells them.
pub(super) fn plan_launch(
    located: &Located,
    inherited: Vec<(OsString, OsString)>,
    caller_args: Vec<OsString>,
    windows: bool,
) -> Launch {
    let root = if windows {
        located.root.replace('/', "\\")
    } else {
        located.root.clone()
    };
    let mut args: Vec<OsString> = located
        .sidecar
        .args
        .iter()
        .map(|arg| OsString::from(expand(arg, &root)))
        .collect();
    args.extend(caller_args);
    let mut program = expand(&located.sidecar.program, &root);
    if windows {
        program = program.replace('/', "\\");
    }
    Launch {
        program,
        args,
        env: build_env(inherited, &located.sidecar, &root, windows),
    }
}

/// The real filesystem.
struct RealFs;

impl LookupFs for RealFs {
    fn read_sidecar(&self, path: &str) -> Result<Option<Vec<u8>>, ReadError> {
        read_regular_bounded(Path::new(path))
    }

    fn exists(&self, path: &str) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    fn list_dir(&self, dir: &str) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(unix)]
    fn is_executable_file(&self, path: &str) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }

    #[cfg(unix)]
    fn is_running_image(&self, path: &str) -> bool {
        use std::os::unix::fs::MetadataExt;
        // `current_exe` is `/proc/self/exe` on Linux and `_NSGetExecutablePath` on macOS.
        let Ok(image) = std::env::current_exe().and_then(std::fs::metadata) else {
            return false;
        };
        std::fs::metadata(path).is_ok_and(|started| started.dev() == image.dev() && started.ino() == image.ino())
    }
}

/// Whether `program` is a batch file, which `CreateProcessW` would hand to `cmd.exe`.
///
/// Windows ignores trailing dots and spaces in a file name, so `tool.cmd. ` still runs as `tool.cmd`.
fn is_batch_target(program: &str) -> bool {
    let name = program.trim_end_matches(['.', ' ']);
    [".bat", ".cmd"].iter().any(|extension| {
        name.len()
            .checked_sub(extension.len())
            .and_then(|cut| name.get(cut..))
            .is_some_and(|tail| tail.eq_ignore_ascii_case(extension))
    })
}

/// The refusal for a batch target whose path holds `%`: `cmd.exe` expands `%NAME%` inside the quoted script path,
/// which std's batch escaping does not cover, so another script could run.
fn batch_program_refusal(program: &str) -> Option<ShimError> {
    program.contains('%').then(|| {
        refuse(format!(
            "batch target {program} contains '%', which cmd.exe would expand in its path"
        ))
    })
}

/// Replaces the process with `program`; returns only the failure.
#[cfg(unix)]
#[expect(
    clippy::disallowed_types,
    clippy::disallowed_methods,
    reason = "the launcher is not ocx: it has no ocx_package_manager edge, and replacing itself with the tool is its job"
)]
fn exec_program(program: &str, args: &[OsString], env: Vec<(OsString, OsString)>) -> std::io::Error {
    use std::os::unix::process::CommandExt;

    std::process::Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        .exec()
}

/// The `.direct` launcher on Unix: finds the vendor root, builds the environment and argv, and `exec`s the tool.
///
/// Writes nothing to disk and sets no environment of its own, so it runs from a read-only directory.
#[cfg(unix)]
pub(super) fn run() -> Result<i32, ShimError> {
    #[expect(clippy::disallowed_methods, reason = "no ocx_env edge: the shim's size budget")]
    let variable = |name: &str| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned());

    let mut arguments = std::env::args_os();
    let argv0 = arguments
        .next()
        .and_then(|argument| argument.into_string().ok())
        .ok_or(ShimError::SelfPathFailure)?;
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|dir| dir.into_os_string().into_string().ok())
        .ok_or(ShimError::SelfPathFailure)?;

    let started = started_path_unix(&argv0, &cwd, variable("PATH").as_deref(), &RealFs)?;
    let runfiles_dir = variable("RUNFILES_DIR");
    let context = LookupContext {
        cwd: &cwd,
        runfiles_dir: runfiles_dir.as_deref(),
        windows: false,
    };
    let located = match locate(&started, &context, &RealFs)? {
        Probed::Direct(located) => located,
        Probed::Legacy(kind) => return Err(refuse(format!("unexpected .{kind} sidecar on this platform"))),
    };

    #[expect(clippy::disallowed_methods, reason = "no ocx_env edge: the shim's size budget")]
    let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let launch = plan_launch(&located, inherited, arguments.collect(), false);
    let error = exec_program(&launch.program, &launch.args, launch.env);
    Err(ShimError::DirectExecFailed {
        program: launch.program,
        permission_denied: error.kind() == std::io::ErrorKind::PermissionDenied,
        reason: error.to_string(),
    })
}

/// The `.direct` launcher on Windows: finds the vendor root, then starts the tool.
///
/// A batch target goes through `std::process::Command`, whose batch-file escaping refuses what it cannot quote
/// safely (BatBadBut, CVE-2024-24576), and anything else through the existing `CreateProcessW` path.
#[cfg(windows)]
pub(super) fn run_windows(started: &str) -> Result<i32, ShimError> {
    #[expect(clippy::disallowed_methods, reason = "no ocx_env edge: the shim's size budget")]
    let runfiles_dir = std::env::var_os("RUNFILES_DIR").map(|value| value.to_string_lossy().into_owned());
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|dir| dir.into_os_string().into_string().ok())
        .ok_or(ShimError::SelfPathFailure)?;
    let context = LookupContext {
        cwd: &cwd,
        runfiles_dir: runfiles_dir.as_deref(),
        windows: true,
    };
    let located = match locate(started, &context, &RealFs)? {
        Probed::Direct(located) => located,
        Probed::Legacy(kind) => return Err(refuse(format!("unexpected .{kind} sidecar beside the direct launcher"))),
    };

    #[expect(clippy::disallowed_methods, reason = "no ocx_env edge: the shim's size budget")]
    let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let launch = plan_launch(&located, inherited, std::env::args_os().skip(1).collect(), true);
    if is_batch_target(&launch.program) {
        if let Some(refusal) = batch_program_refusal(&launch.program) {
            return Err(refusal);
        }
        return spawn_batch(&launch);
    }

    // shortcut: an argument that is not valid Unicode is refused here, because the quoter works on `str`;
    // `ocx exec` passes it through, so upgrade with a wide-character quoter when a tool needs one.
    let mut command_line = String::new();
    core::append_quoted_arg(&mut command_line, &launch.program);
    for argument in &launch.args {
        let text = argument.to_str().ok_or_else(|| ShimError::DirectArgumentRefused {
            reason: format!("an argument for {} is not valid Unicode", launch.program),
        })?;
        command_line.push(' ');
        core::append_quoted_arg(&mut command_line, text);
    }
    let block = environment_block(&launch.env);
    super::spawn_child(Some(&launch.program), &command_line, Some(&block)).map_err(|win32| ShimError::SpawnFailure {
        win32,
        program: launch.program.clone(),
    })
}

/// The `CREATE_UNICODE_ENVIRONMENT` block of `env`: `KEY=VALUE\0` entries, then one more `\0`.
#[cfg(windows)]
fn environment_block(env: &[(OsString, OsString)]) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    let mut block: Vec<u16> = Vec::new();
    for (key, value) in env {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    // An empty block is two terminators, or the child reads past it.
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// Starts a batch target with `std::process::Command` and holds it in a kill-on-close job until it exits.
#[cfg(windows)]
#[expect(
    clippy::disallowed_types,
    reason = "the launcher is not ocx: it has no ocx_package_manager edge, and std's batch-file escaping is the point"
)]
fn spawn_batch(launch: &Launch) -> Result<i32, ShimError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

    super::install_console_ctrl_handler();
    let job = super::create_kill_on_close_job();

    let mut child = match std::process::Command::new(&launch.program)
        .args(&launch.args)
        .env_clear()
        .envs(launch.env.iter().map(|(key, value)| (key, value)))
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            if !job.is_null() {
                // SAFETY: `job` is a valid handle, not used again.
                unsafe { CloseHandle(job) };
            }
            // std refuses a batch argument it cannot quote safely with `InvalidInput`.
            return Err(if error.kind() == std::io::ErrorKind::InvalidInput {
                ShimError::DirectArgumentRefused {
                    reason: format!(
                        "an argument for {} cannot be passed to a batch file safely",
                        launch.program
                    ),
                }
            } else {
                ShimError::SpawnFailure {
                    win32: error.raw_os_error().map_or(0, |code| code as u32),
                    program: launch.program.clone(),
                }
            });
        }
    };
    // The window between spawn and assignment is std's: it exposes no job-list attribute to start the child inside.
    // SAFETY: both handles are valid; `child` owns its process handle for the whole call.
    if !job.is_null() && unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) } == 0 {
        // SAFETY: no pointers; reads thread-local last-error.
        let last = unsafe { GetLastError() };
        eprintln!("ocx-shim: job object assignment failed: {last}");
    }
    let status = child.wait();
    if !job.is_null() {
        // SAFETY: `job` is a valid handle, not used again.
        unsafe { CloseHandle(job) };
    }
    match status {
        // Full i32 passthrough: never remap or truncate.
        Ok(status) => Ok(status.code().unwrap_or(1)),
        Err(error) => Err(ShimError::SpawnFailure {
            win32: error.raw_os_error().map_or(0, |code| code as u32),
            program: launch.program.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    const SELF_PATH: &str = "selections/default/shells/default/bin/cmake";
    const VENDOR_A: &str = "sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a";
    const VENDOR_B: &str = "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    fn sidecar_json(vendor_id: &str, self_path: &str) -> Vec<u8> {
        format!(
            r#"{{"schema_version":1,"vendor_id":"{vendor_id}","self":"{self_path}","program":"@VENDOR_DIR@/packages/p/content/bin/cmake","args":[],"unset":[],"env":[]}}"#
        )
        .into_bytes()
    }

    #[derive(Default)]
    struct FakeFs {
        files: HashMap<String, Vec<u8>>,
        unreadable: HashSet<String>,
        dirs: HashMap<String, Vec<String>>,
        #[cfg(unix)]
        executables: HashSet<String>,
        #[cfg(unix)]
        image: Option<String>,
    }

    impl FakeFs {
        fn with_file(mut self, path: &str, bytes: &[u8]) -> Self {
            self.files.insert(path.to_string(), bytes.to_vec());
            self
        }
        fn with_dir(mut self, dir: &str, names: &[&str]) -> Self {
            self.dirs
                .insert(dir.to_string(), names.iter().map(|n| n.to_string()).collect());
            self
        }
    }

    impl LookupFs for FakeFs {
        fn read_sidecar(&self, path: &str) -> Result<Option<Vec<u8>>, ReadError> {
            if self.unreadable.contains(path) {
                return Err(ReadError::NotRegular);
            }
            Ok(self.files.get(path).cloned())
        }
        fn exists(&self, path: &str) -> bool {
            self.files.contains_key(path)
        }
        fn list_dir(&self, dir: &str) -> Vec<String> {
            self.dirs.get(dir).cloned().unwrap_or_default()
        }
        #[cfg(unix)]
        fn is_executable_file(&self, path: &str) -> bool {
            self.executables.contains(path)
        }
        #[cfg(unix)]
        fn is_running_image(&self, path: &str) -> bool {
            self.image.as_deref() == Some(path)
        }
    }

    fn unix(runfiles_dir: Option<&str>) -> LookupContext<'_> {
        LookupContext {
            cwd: "/work",
            runfiles_dir,
            windows: false,
        }
    }

    fn direct(probed: Probed) -> Located {
        match probed {
            Probed::Direct(located) => located,
            Probed::Legacy(extension) => panic!("expected a direct sidecar, found legacy {extension}"),
        }
    }

    fn refusal(result: Result<Probed, ShimError>) -> String {
        let err = result.expect_err("the lookup must refuse");
        assert_eq!(err.exit_code(), 78, "{}", err.stderr_message());
        err.stderr_message()
    }

    // ── Kinds per platform ────────────────────────────────────────────

    #[test]
    fn v52_unix_probes_the_direct_kind_alone() {
        assert_eq!(core::probe_extensions(false), ["direct"]);
    }

    #[test]
    fn v52_windows_probes_the_direct_kind_after_the_three_existing_ones() {
        assert_eq!(core::probe_extensions(true), ["shim", "shimref", "exec", "direct"]);
    }

    #[test]
    fn v52_a_direct_sidecar_is_accepted_on_unix() {
        let started = format!("/vendor/{SELF_PATH}");
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default().with_file(&format!("{started}.direct"), &bytes);
        let located = direct(locate(&started, &unix(None), &fs).unwrap());
        assert_eq!(located.root, "/vendor");
        assert_eq!(located.sidecar.vendor_id, VENDOR_A);
    }

    #[test]
    fn v52_other_kinds_are_refused_on_unix() {
        let started = format!("/vendor/{SELF_PATH}");
        let mut fs = FakeFs::default();
        for extension in ["shim", "shimref", "exec"] {
            fs = fs.with_file(&format!("{started}.{extension}"), b"/some/root\n");
        }
        let message = refusal(locate(&started, &unix(None), &fs));
        assert!(message.contains("cmake.direct"), "{message}");
    }

    #[test]
    fn v52_an_existing_kind_wins_over_direct_on_windows() {
        let started = format!("C:\\vendor\\{}.exe", SELF_PATH.replace('/', "\\"));
        let stem_path = started.strip_suffix(".exe").unwrap();
        let fs = FakeFs::default()
            .with_file(&format!("{stem_path}.shim"), b"C:\\root\n")
            .with_file(&format!("{stem_path}.direct"), &sidecar_json(VENDOR_A, SELF_PATH));
        let context = LookupContext {
            cwd: "C:\\work",
            runfiles_dir: None,
            windows: true,
        };
        assert!(matches!(
            locate(&started, &context, &fs).unwrap(),
            Probed::Legacy("shim")
        ));
    }

    // ── Root candidates ────────────────────────────────────────────────────

    #[test]
    fn candidate_a_strips_the_launcher_path_from_the_started_path_on_windows() {
        let started = "C:\\Vendor\\Selections\\Default\\Shells\\Default\\BIN\\CMAKE.EXE";
        let sidecar_path = "C:\\Vendor\\Selections\\Default\\Shells\\Default\\BIN\\CMAKE.direct";
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file(sidecar_path, &bytes)
            .with_file(&format!("C:\\Vendor/{SELF_PATH}.direct"), &bytes);
        let context = LookupContext {
            cwd: "C:\\work",
            runfiles_dir: None,
            windows: true,
        };
        let located = direct(locate(started, &context, &fs).unwrap());
        assert_eq!(located.root, "C:\\Vendor");
    }

    #[test]
    fn candidate_b_takes_the_runfiles_entry_holding_the_same_sidecar() {
        let started = "/out/bin/tool";
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.direct", &bytes)
            .with_dir("/out/bin/tool.runfiles", &["_main", "pkg"])
            .with_file(&format!("/out/bin/tool.runfiles/pkg/{SELF_PATH}.direct"), &bytes);
        let located = direct(locate(started, &unix(None), &fs).unwrap());
        assert_eq!(located.root, "/out/bin/tool.runfiles/pkg");
    }

    #[test]
    fn candidate_b_skips_an_entry_whose_sidecar_has_another_vendor_id() {
        let started = "/out/bin/tool";
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.direct", &sidecar_json(VENDOR_A, SELF_PATH))
            .with_dir("/out/bin/tool.runfiles", &["a_other", "b_mine"])
            .with_file(
                &format!("/out/bin/tool.runfiles/a_other/{SELF_PATH}.direct"),
                &sidecar_json(VENDOR_B, SELF_PATH),
            )
            .with_file(
                &format!("/out/bin/tool.runfiles/b_mine/{SELF_PATH}.direct"),
                &sidecar_json(VENDOR_A, SELF_PATH),
            );
        let located = direct(locate(started, &unix(None), &fs).unwrap());
        assert_eq!(located.root, "/out/bin/tool.runfiles/b_mine");
    }

    fn runfiles_with_matching_entry_at(position: usize) -> FakeFs {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let names: Vec<String> = (0..5000).map(|i| format!("e{i:05}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        FakeFs::default()
            .with_file("/out/bin/tool.direct", &bytes)
            .with_dir("/out/bin/tool.runfiles", &refs)
            .with_file(
                &format!("/out/bin/tool.runfiles/{}/{SELF_PATH}.direct", names[position]),
                &bytes,
            )
    }

    #[test]
    fn candidate_b_examines_at_most_4096_entries_in_name_order() {
        let located = direct(locate("/out/bin/tool", &unix(None), &runfiles_with_matching_entry_at(4095)).unwrap());
        assert_eq!(located.root, "/out/bin/tool.runfiles/e04095");

        let message = refusal(locate(
            "/out/bin/tool",
            &unix(None),
            &runfiles_with_matching_entry_at(4096),
        ));
        assert!(message.contains("4096 candidate"), "{message}");
    }

    #[test]
    fn candidate_b_examines_entries_in_name_order_whatever_the_listing_order() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.direct", &bytes)
            .with_dir("/out/bin/tool.runfiles", &["zz", "aa"])
            .with_file(&format!("/out/bin/tool.runfiles/zz/{SELF_PATH}.direct"), &bytes)
            .with_file(&format!("/out/bin/tool.runfiles/aa/{SELF_PATH}.direct"), &bytes);
        let located = direct(locate("/out/bin/tool", &unix(None), &fs).unwrap());
        assert_eq!(located.root, "/out/bin/tool.runfiles/aa");
    }

    /// A `RUNFILES_DIR` that does not hold the started path is never consulted, even when its entry matches.
    #[test]
    fn v59_a_stale_runfiles_dir_is_not_used_when_the_started_path_is_outside_it() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/own/tool.direct", &bytes)
            .with_dir("/stale/runfiles", &["tree"])
            .with_file(&format!("/stale/runfiles/tree/{SELF_PATH}.direct"), &bytes);
        let message = refusal(locate("/own/tool", &unix(Some("/stale/runfiles")), &fs));
        assert!(message.contains("no vendored root"), "{message}");
    }

    #[test]
    fn v59_control_the_runfiles_dir_is_used_when_the_started_path_is_under_it() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/runfiles/_main/tool.direct", &bytes)
            .with_dir("/runfiles", &["tree"])
            .with_file(&format!("/runfiles/tree/{SELF_PATH}.direct"), &bytes);
        let located = direct(locate("/runfiles/_main/tool", &unix(Some("/runfiles")), &fs).unwrap());
        assert_eq!(located.root, "/runfiles/tree");
    }

    #[test]
    fn v59_a_prefix_that_is_not_a_path_component_is_not_under_the_runfiles_dir() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/runfiles-old/tool.direct", &bytes)
            .with_dir("/runfiles", &["tree"])
            .with_file(&format!("/runfiles/tree/{SELF_PATH}.direct"), &bytes);
        refusal(locate("/runfiles-old/tool", &unix(Some("/runfiles")), &fs));
    }

    #[test]
    fn a_missing_root_names_the_first_eight_candidates_and_the_count() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let names: Vec<String> = (0..12).map(|i| format!("e{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.direct", &bytes)
            .with_dir("/out/bin/tool.runfiles", &refs);
        let message = refusal(locate("/out/bin/tool", &unix(None), &fs));
        assert!(message.contains("12 candidate"), "{message}");
        assert!(message.contains("e07") && !message.contains("e08"), "{message}");
    }

    #[test]
    fn a_missing_sidecar_names_the_path_it_tried() {
        let message = refusal(locate("/vendor/bin/cmake", &unix(None), &FakeFs::default()));
        assert!(message.contains("/vendor/bin/cmake.direct"), "{message}");
    }

    // ── Root shape ────────────────────────────────────────────

    fn root_under(dir: &str) -> Result<Probed, ShimError> {
        let started = format!("{dir}/{SELF_PATH}");
        let fs = FakeFs::default().with_file(&format!("{started}.direct"), &sidecar_json(VENDOR_A, SELF_PATH));
        locate(&started, &unix(None), &fs)
    }

    #[test]
    fn v68_a_root_without_a_list_separator_is_accepted() {
        assert_eq!(direct(root_under("/tmp/ab/out").unwrap()).root, "/tmp/ab/out");
    }

    #[test]
    fn v68_a_root_with_a_list_separator_is_refused() {
        let message = refusal(root_under("/tmp/a:b/out"));
        assert!(message.contains("/tmp/a:b/out"), "{message}");
    }

    #[test]
    fn v68_a_windows_root_with_a_semicolon_is_refused_and_a_drive_colon_is_not() {
        let context = LookupContext {
            cwd: "C:\\work",
            runfiles_dir: None,
            windows: true,
        };
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        for (root, accepted) in [("C:\\a;b", false), ("C:\\ab", true)] {
            let started = format!("{root}\\{}.exe", SELF_PATH.replace('/', "\\"));
            let sidecar_path = format!("{root}\\{}.direct", SELF_PATH.replace('/', "\\"));
            let fs = FakeFs::default()
                .with_file(&sidecar_path, &bytes)
                .with_file(&format!("{root}/{SELF_PATH}.direct"), &bytes);
            let result = locate(&started, &context, &fs);
            assert_eq!(result.is_ok(), accepted, "{root}");
        }
    }

    // ── Started path (Unix) ────────────────────────────────────────

    #[test]
    #[cfg(unix)]
    fn a_relative_argv0_with_a_slash_is_joined_onto_the_working_directory() {
        let fs = FakeFs {
            image: Some("/work/bin/cmake".to_string()),
            ..FakeFs::default()
        };
        assert_eq!(
            started_path_unix("./bin/cmake", "/work", None, &fs).unwrap(),
            "/work/bin/cmake"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_bare_argv0_takes_the_first_path_entry_holding_an_executable() {
        let fs = FakeFs {
            executables: HashSet::from(["/second/cmake".to_string(), "/third/cmake".to_string()]),
            image: Some("/second/cmake".to_string()),
            ..FakeFs::default()
        };
        assert_eq!(
            started_path_unix("cmake", "/work", Some("/first::/second:/third"), &fs).unwrap(),
            "/second/cmake"
        );
    }

    /// The `PATH` hit is a different file than the running image.
    #[test]
    #[cfg(unix)]
    fn v61_a_path_hit_that_is_not_the_running_image_is_refused() {
        let fs = FakeFs {
            executables: HashSet::from(["/shadow/cmake".to_string()]),
            image: Some("/real/cmake".to_string()),
            ..FakeFs::default()
        };
        let err = started_path_unix("cmake", "/work", Some("/shadow"), &fs).unwrap_err();
        assert_eq!(err.exit_code(), 78, "{}", err.stderr_message());
    }

    #[test]
    #[cfg(unix)]
    fn a_bare_argv0_with_no_path_hit_is_an_own_path_failure() {
        let err = started_path_unix("cmake", "/work", Some("/nowhere"), &FakeFs::default()).unwrap_err();
        assert_eq!(err.exit_code(), 74);
    }

    // ── sidecar decoding ────────────────────────────────────────────────────

    #[test]
    fn a_sidecar_with_an_unknown_field_exits_78() {
        let raw = br#"{"schema_version":1,"vendor_id":"v","self":"a","program":"@VENDOR_DIR@/a","args":[],"unset":[],"env":[],"extra":1}"#;
        assert_eq!(decode_sidecar(raw, "/s.direct", false).unwrap_err().exit_code(), 78);
    }

    #[test]
    fn a_sidecar_with_another_schema_version_exits_78() {
        let raw = String::from_utf8(sidecar_json(VENDOR_A, SELF_PATH))
            .unwrap()
            .replace("\"schema_version\":1", "\"schema_version\":2");
        assert_eq!(
            decode_sidecar(raw.as_bytes(), "/s.direct", false)
                .unwrap_err()
                .exit_code(),
            78
        );
    }

    #[test]
    fn a_sidecar_that_breaks_a_validation_rule_exits_78() {
        let raw = String::from_utf8(sidecar_json(VENDOR_A, SELF_PATH))
            .unwrap()
            .replace("@VENDOR_DIR@/packages", "/packages");
        assert_eq!(
            decode_sidecar(raw.as_bytes(), "/s.direct", false)
                .unwrap_err()
                .exit_code(),
            78
        );
    }

    #[test]
    fn a_well_formed_sidecar_decodes() {
        let raw = sidecar_json(VENDOR_A, SELF_PATH);
        assert_eq!(decode_sidecar(&raw, "/s.direct", false).unwrap().self_path, SELF_PATH);
    }

    #[test]
    fn a_sidecar_over_one_mebibyte_exits_78_even_when_well_formed() {
        let mut raw = sidecar_json(VENDOR_A, SELF_PATH);
        raw.resize(MAX_SIDECAR_BYTES as usize + 1, b' ');
        assert_eq!(decode_sidecar(&raw, "/s.direct", false).unwrap_err().exit_code(), 78);
    }

    /// A NUL in a value ends its `KEY=VALUE` entry in a Windows environment block, so the rest would be a new entry.
    #[test]
    fn a_nul_in_an_env_value_or_baked_argument_exits_78() {
        let value = r#"[{"key":"K","value":"x\u0000OCX_SIGNING_KEY=s","kind":"constant"}]"#;
        let raw = sidecar_with("[]", r#"["OCX_SIGNING_KEY"]"#, value);
        assert_eq!(
            decode_sidecar(raw.as_bytes(), "/s.direct", true)
                .unwrap_err()
                .exit_code(),
            78
        );
        let raw = sidecar_with(r#"["a\u0000b"]"#, "[]", "[]");
        assert_eq!(
            decode_sidecar(raw.as_bytes(), "/s.direct", false)
                .unwrap_err()
                .exit_code(),
            78
        );
    }

    #[test]
    fn a_batch_target_whose_path_holds_a_percent_sign_is_refused() {
        assert!(batch_program_refusal("C:\\v\\x.cmd").is_none());
        for program in ["C:\\a%b\\x.cmd", "C:\\v\\%PATH%.bat"] {
            let err = batch_program_refusal(program).expect("refused");
            assert_eq!(err.exit_code(), 78, "{program}");
            assert!(err.stderr_message().contains(program), "{}", err.stderr_message());
        }
    }

    #[test]
    fn a_sidecar_the_fs_refuses_exits_78() {
        let mut fs = FakeFs::default();
        fs.unreadable.insert("/vendor/bin/cmake.direct".to_string());
        let message = refusal(locate("/vendor/bin/cmake", &unix(None), &fs));
        assert!(message.contains("not a regular file"), "{message}");
    }

    // ── Launch plan ───────────────────────────────────────────

    fn located(json: &str, root: &str) -> Located {
        Located {
            sidecar: decode_sidecar(json.as_bytes(), "/s.direct", false).unwrap(),
            root: root.to_string(),
        }
    }

    fn sidecar_with(args: &str, unset: &str, env: &str) -> String {
        format!(
            r#"{{"schema_version":1,"vendor_id":"{VENDOR_A}","self":"{SELF_PATH}","program":"@VENDOR_DIR@/p/bin/cmake","args":{args},"unset":{unset},"env":{env}}}"#
        )
    }

    fn pairs(entries: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        entries
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn the_program_and_baked_arguments_take_the_root_before_the_callers_arguments() {
        let located = located(
            &sidecar_with(r#"["-C","@VENDOR_DIR@/a/@VENDOR_DIR@"]"#, "[]", "[]"),
            "/v",
        );
        let launch = plan_launch(
            &located,
            Vec::new(),
            vec![OsString::from("--x"), OsString::from("@VENDOR_DIR@")],
            false,
        );
        assert_eq!(launch.program, "/v/p/bin/cmake");
        assert_eq!(
            launch.args,
            ["-C", "/v/a//v", "--x", "@VENDOR_DIR@"].map(OsString::from),
            "the callers arguments are never token-expanded"
        );
    }

    #[test]
    fn an_unset_key_is_removed_and_the_records_apply_over_the_rest() {
        let env = r#"[{"key":"LATE","value":"v","kind":"constant"}]"#;
        let located = located(&sidecar_with("[]", r#"["SECRET"]"#, env), "/v");
        let launch = plan_launch(
            &located,
            pairs(&[("SECRET", "s"), ("LATE", "old"), ("KEEP", "k")]),
            Vec::new(),
            false,
        );
        assert_eq!(launch.env, pairs(&[("KEEP", "k"), ("LATE", "v")]));
    }

    #[test]
    fn records_apply_in_order_so_a_later_record_sees_an_earlier_one() {
        let env = r#"[
            {"key":"PATH","value":"@VENDOR_DIR@/one","kind":"path"},
            {"key":"PATH","value":"@VENDOR_DIR@/two","kind":"path"},
            {"key":"FLAGS","value":"-b","kind":"list","separator":" "},
            {"key":"FLAGS","value":"-a","kind":"list","separator":" "}]"#;
        let located = located(&sidecar_with("[]", "[]", env), "/v");
        // A path list splits and joins on the host separator, `;` on Windows.
        let host_list = |segments: &[&str]| std::env::join_paths(segments).unwrap().into_string().unwrap();
        let inherited = host_list(&["/usr/bin", "/v/one"]);
        let launch = plan_launch(&located, pairs(&[("PATH", inherited.as_str())]), Vec::new(), false);
        let expected = host_list(&["/v/two", "/v/one", "/usr/bin"]);
        assert_eq!(launch.env, pairs(&[("FLAGS", "-b -a"), ("PATH", expected.as_str())]));
    }

    #[test]
    fn an_env_key_compares_byte_exact_on_unix() {
        let env = r#"[{"key":"Path","value":"/x","kind":"constant"}]"#;
        let located = located(&sidecar_with("[]", r#"["PATH"]"#, env), "/v");
        let launch = plan_launch(&located, pairs(&[("PATH", "/usr/bin")]), Vec::new(), false);
        assert_eq!(launch.env, pairs(&[("Path", "/x")]));
    }

    // ── Windows: key fold, separators, batch dispatch ────────

    #[test]
    fn v73_windows_unset_and_path_records_match_keys_case_insensitively() {
        let env = r#"[{"key":"PATH","value":"@VENDOR_DIR@/bin","kind":"path"}]"#;
        let located = located(&sidecar_with("[]", r#"["secret"]"#, env), "/v");
        let launch = plan_launch(
            &located,
            pairs(&[("SECRET", "s"), ("Path", "\\V\\BIN")]),
            Vec::new(),
            true,
        );
        assert_eq!(
            launch.env,
            pairs(&[("Path", "\\v\\bin")]),
            "one `Path` under its inherited spelling, its element equal to the record's under the fold, `SECRET` unset"
        );
    }

    #[test]
    fn v73_control_unix_keeps_a_differently_cased_key_apart() {
        let env = r#"[{"key":"PATH","value":"/v/bin","kind":"path"}]"#;
        let located = located(&sidecar_with("[]", "[]", env), "/v");
        let launch = plan_launch(&located, pairs(&[("Path", "/usr/bin")]), Vec::new(), false);
        assert_eq!(launch.env, pairs(&[("PATH", "/v/bin"), ("Path", "/usr/bin")]));
    }

    #[test]
    fn windows_turns_slashes_into_backslashes_in_the_root_program_and_path_values() {
        let env = r#"[{"key":"P","value":"@VENDOR_DIR@/a/b","kind":"path"},{"key":"C","value":"x/@VENDOR_DIR@/y","kind":"constant"}]"#;
        let located = located(&sidecar_with(r#"["@VENDOR_DIR@/z/q"]"#, "[]", env), "C:/v/w");
        let launch = plan_launch(&located, Vec::new(), Vec::new(), true);
        assert_eq!(launch.program, "C:\\v\\w\\p\\bin\\cmake");
        assert_eq!(
            launch.args,
            [OsString::from("C:\\v\\w/z/q")],
            "only the root is converted in an argument"
        );
        assert_eq!(
            launch.env,
            pairs(&[("C", "x/C:\\v\\w/y"), ("P", "C:\\v\\w\\a\\b")]),
            "a constant keeps its own slashes; the root inside it is native"
        );
    }

    #[test]
    fn a_target_is_a_batch_file_by_its_extension_after_trimming_trailing_dots_and_spaces() {
        for batch in [
            "C:\\v\\x.bat",
            "C:\\v\\x.BAT",
            "C:\\v\\x.Cmd",
            "C:\\v\\x.CMD. ",
            "C:\\v\\x.bat...",
            "C:\\v\\x.cmd  ",
            "C:\\v\\x.bat. . ",
        ] {
            assert!(is_batch_target(batch), "{batch:?} must take the batch path");
        }
        for other in [
            "C:\\v\\x.batx",
            "C:\\v\\x.cmd.exe",
            "C:\\v\\x.exe",
            "C:\\v\\x.com",
            "C:\\v\\xbat",
            "C:\\v\\x.ba",
            "C:\\v\\x.bat.txt",
            "",
            ".",
        ] {
            assert!(!is_batch_target(other), "{other:?} must take the CreateProcessW path");
        }
    }

    #[test]
    fn a_refused_batch_argument_exits_64_naming_the_program() {
        let err = ShimError::DirectArgumentRefused {
            reason: "an argument for C:\\v\\x.bat cannot be passed to a batch file safely".to_string(),
        };
        assert_eq!(err.exit_code(), 64);
        assert!(
            err.stderr_message().starts_with("ocx-shim: "),
            "{}",
            err.stderr_message()
        );
        assert!(err.stderr_message().contains("x.bat"));
    }

    // ── Real reads ────────────────────────────────────────────────────

    fn scratch_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("ocx-shim-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_regular_file_is_read_and_a_missing_one_is_none() {
        let dir = scratch_dir("regular");
        let file = dir.join("a.direct");
        std::fs::write(&file, b"{}").unwrap();
        assert_eq!(read_regular_bounded(&file).unwrap().as_deref(), Some(&b"{}"[..]));
        assert!(read_regular_bounded(&dir.join("missing.direct")).unwrap().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_in_place_of_the_sidecar_is_not_a_regular_file() {
        let dir = scratch_dir("dir");
        assert!(matches!(read_regular_bounded(&dir), Err(ReadError::NotRegular)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_over_the_bound_is_refused() {
        let dir = scratch_dir("big");
        let file = dir.join("big.direct");
        std::fs::write(&file, vec![b' '; MAX_SIDECAR_BYTES as usize + 1]).unwrap();
        assert!(matches!(read_regular_bounded(&file), Err(ReadError::TooLarge)));
        std::fs::write(&file, vec![b' '; MAX_SIDECAR_BYTES as usize]).unwrap();
        assert_eq!(
            read_regular_bounded(&file).unwrap().map(|b| b.len()),
            Some(MAX_SIDECAR_BYTES as usize)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A FIFO at the sidecar path must not block the launcher; the read runs on a thread so a hang fails the test.
    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_types,
        reason = "a test needs `mkfifo`; std has no FIFO constructor"
    )]
    fn v60_a_fifo_at_the_sidecar_path_is_refused_without_blocking() {
        let dir = scratch_dir("fifo");
        let fifo = dir.join("cmake.direct");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());

        let (sender, receiver) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let _ = sender.send(read_regular_bounded(&path));
        });
        let outcome = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the read must return within a second");
        assert!(matches!(outcome, Err(ReadError::NotRegular)), "{outcome:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
