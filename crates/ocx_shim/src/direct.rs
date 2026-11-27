// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `NAME.launch` launcher: reading its record, dispatching on the record's kind, and for kind `direct` finding the
//! vendor root it ships in.
//!
//! Every filesystem question goes through [`LookupFs`], so the lookup rules run in unit tests without a disk.

use super::ShimError;
use super::core;
use ocx_env_apply::launch::{
    DirectRecord, EnvRecord, LaunchRecord, MAX_RECORD_BYTES, RECORD_EXTENSION, VENDOR_DIR_TOKEN, is_absolute_any_host,
    is_fully_qualified, record_path_for,
};
use ocx_env_apply::{ApplyKind, KeyFold, apply_one};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// At most this many directory entries of a runfiles tree are examined, in name order.
const MAX_RUNFILES_ENTRIES: usize = 4096;

/// A runfiles manifest may hold this many bytes.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

/// A runfiles manifest line may hold this many bytes, its line feed not counted.
const MAX_MANIFEST_LINE: usize = 64 * 1024;

/// At most this many distinct roots are taken from the runfiles manifests, in file order.
const MAX_MANIFEST_CANDIDATES: usize = 4096;

/// The candidate records read from manifest roots add up to at most this many bytes.
const MAX_CANDIDATE_RECORD_BYTES: u64 = 16 * 1024 * 1024;

/// How many root candidates the "no root found" line names.
const NAMED_CANDIDATES: usize = 8;

/// Why a launch record read was refused.
#[derive(Debug)]
pub(super) enum ReadError {
    /// The path is not a regular file (a FIFO, a directory, a device).
    NotRegular,
    /// The file holds more than [`MAX_RECORD_BYTES`].
    TooLarge,
    /// A streamed file holds more bytes than the bound the caller named.
    StreamTooLarge,
    /// A streamed line is longer than the bound the caller named.
    LineTooLong,
    /// Any other I/O failure, `NotFound` excepted.
    Io(std::io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::NotRegular => f.write_str("not a regular file"),
            ReadError::TooLarge => write!(f, "larger than {MAX_RECORD_BYTES} bytes"),
            ReadError::StreamTooLarge => f.write_str("larger than the stream bound"),
            ReadError::LineTooLong => f.write_str("a line is longer than the line bound"),
            ReadError::Io(err) => write!(f, "{err}"),
        }
    }
}

/// The lines of a file, each without its line feed, read on demand.
pub(super) type LineStream = Box<dyn Iterator<Item = Result<Vec<u8>, ReadError>>>;

/// The filesystem questions the lookup asks.
pub(super) trait LookupFs {
    /// The kernel's record of the loaded image, resolved once; the refusal when the platform names none.
    fn image_path(&self) -> Result<String, ShimError>;
    /// The bytes of the launch record at `path`; `Ok(None)` when nothing is there.
    fn read_sidecar(&self, path: &str) -> Result<Option<Vec<u8>>, ReadError>;
    /// The lines of the file at `path`, read as a stream: `Ok(None)` when nothing is there. The stream yields
    /// `Err` once the file passes `max_bytes` or a line passes `max_line` bytes (the line feed is not part of a
    /// line), and ends after its first `Err`.
    fn read_lines(&self, path: &str, max_bytes: u64, max_line: usize) -> Result<Option<LineStream>, ReadError>;
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
    /// The working directory, used to make relative paths absolute; empty when it cannot be read, which only kind
    /// `direct` needs.
    pub cwd: &'a str,
    /// The launcher's own `RUNFILES_DIR`.
    pub runfiles_dir: Option<&'a str>,
    /// The launcher's own `RUNFILES_MANIFEST_FILE`; read only when it lists the started path.
    pub manifest_file: Option<&'a str>,
    /// The target platform: separators, case folding and the path-list separator.
    pub windows: bool,
}

/// What the record beside the launcher asks it to run.
#[derive(Debug)]
pub(super) enum Probed {
    /// A decoded `direct` record and the root it was found for.
    Direct(Located),
    /// A `package`, `deferred` or `toolchain` record.
    Home(HomeFound),
}

/// A home-kind record and the image path it was read beside: the launcher's own directory and `NAME` come from it.
#[derive(Debug)]
pub(super) struct HomeFound {
    pub home: core::HomeLaunch,
    pub image: String,
}

/// A decoded `.launch` record bound to its vendor root.
#[derive(Debug)]
pub(super) struct Located {
    pub sidecar: DirectRecord,
    /// The vendor root: absolute, lexically joined, no symlink resolved.
    pub root: String,
}

pub(super) fn refuse(reason: String) -> ShimError {
    ShimError::DirectRefused { reason }
}

/// Opens `path` non-blocking and requires a regular file; `Ok(None)` when nothing is there.
///
/// The check runs on the opened handle, so a path swapped for a FIFO after the open changes nothing.
fn open_regular(path: &Path) -> Result<Option<(std::fs::File, std::fs::Metadata)>, ReadError> {
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
    let metadata = file.metadata().map_err(ReadError::Io)?;
    if !metadata.is_file() {
        return Err(ReadError::NotRegular);
    }
    Ok(Some((file, metadata)))
}

/// Reads the file at `path`: opened non-blocking, required to be a regular file, at most [`MAX_RECORD_BYTES`].
pub(super) fn read_regular_bounded(path: &Path) -> Result<Option<Vec<u8>>, ReadError> {
    use std::io::Read;

    let Some((file, metadata)) = open_regular(path)? else {
        return Ok(None);
    };
    if metadata.len() > MAX_RECORD_BYTES {
        return Err(ReadError::TooLarge);
    }
    let mut bytes = Vec::new();
    // One byte past the bound: a file that grew after the `metadata` call still trips the size check.
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(ReadError::Io)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(ReadError::TooLarge);
    }
    Ok(Some(bytes))
}

/// The lines of `reader` split at `\n`, with the two bounds of [`LookupFs::read_lines`] enforced as it reads.
///
/// A line is capped while it is still being read, so a file with no line feed never grows a buffer past `max_line`.
pub(super) fn bounded_lines(reader: impl std::io::BufRead + 'static, max_bytes: u64, max_line: usize) -> LineStream {
    struct Lines<R> {
        reader: R,
        read: u64,
        max_bytes: u64,
        max_line: usize,
        done: bool,
    }

    impl<R: std::io::BufRead> Lines<R> {
        fn fail(&mut self, error: ReadError) -> Option<Result<Vec<u8>, ReadError>> {
            self.done = true;
            Some(Err(error))
        }
    }

    impl<R: std::io::BufRead> Iterator for Lines<R> {
        type Item = Result<Vec<u8>, ReadError>;

        fn next(&mut self) -> Option<Self::Item> {
            if self.done {
                return None;
            }
            let mut line = Vec::new();
            loop {
                let buffer = match self.reader.fill_buf() {
                    Ok(buffer) => buffer,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => return self.fail(ReadError::Io(err)),
                };
                if buffer.is_empty() {
                    self.done = true;
                    return (!line.is_empty()).then_some(Ok(line));
                }
                let feed = buffer.iter().position(|&byte| byte == b'\n');
                let take = feed.unwrap_or(buffer.len());
                let used = take + usize::from(feed.is_some());
                if line.len() + take > self.max_line {
                    return self.fail(ReadError::LineTooLong);
                }
                if self.read + used as u64 > self.max_bytes {
                    return self.fail(ReadError::StreamTooLarge);
                }
                line.extend_from_slice(&buffer[..take]);
                self.read += used as u64;
                self.reader.consume(used);
                if feed.is_some() {
                    return Some(Ok(line));
                }
            }
        }
    }

    Box::new(Lines {
        reader,
        read: 0,
        max_bytes,
        max_line,
        done: false,
    })
}

pub(super) fn is_absolute(path: &str, windows: bool) -> bool {
    if windows {
        is_absolute_any_host(path)
    } else {
        path.starts_with('/')
    }
}

pub(super) fn separators(windows: bool) -> &'static [char] {
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
pub(super) fn split_file_name(path: &str, windows: bool) -> (&str, &str) {
    match path.rfind(separators(windows)) {
        Some(at) => path.split_at(at + 1),
        None => ("", path),
    }
}

/// Folds a path for a lexical comparison: on Windows ASCII case and `\` fold, on Unix nothing does.
pub(super) fn fold(text: &str, windows: bool) -> String {
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

/// The record at `<root>/<self>.launch`, read under the same bounds as the one beside the started path.
fn candidate_record(
    root: &str,
    self_path: &str,
    windows: bool,
    fs: &dyn LookupFs,
) -> Result<Option<Vec<u8>>, ShimError> {
    let path = format!(
        "{}/{self_path}.{}",
        root.trim_end_matches(separators(windows)),
        RECORD_EXTENSION
    );
    fs.read_sidecar(&path).map_err(|err| {
        refuse(format!(
            "cannot read launch record {path}: {err}; check that the file is readable"
        ))
    })
}

/// Whether `<root>/<self>.launch` is there and byte-identical to the record beside the started path.
fn accepts(root: &str, self_path: &str, raw: &[u8], windows: bool, fs: &dyn LookupFs) -> Result<bool, ShimError> {
    Ok(candidate_record(root, self_path, windows, fs)?.is_some_and(|bytes| bytes == raw))
}

/// `line` split into its manifest key and target, both unescaped; `None` when it holds no space.
///
/// A line starting with a space is escaped (Bazel 7.4.0 and later): the key reads `\s`, `\n` and `\b`, the target
/// `\n` and `\b`, and any other `\x` stays two characters. An unescaped line keeps its backslashes and is borrowed.
fn split_manifest_line(line: &[u8]) -> Option<(Cow<'_, [u8]>, Cow<'_, [u8]>)> {
    fn unescape(text: &[u8], key: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len());
        let mut bytes = text.iter().copied();
        while let Some(byte) = bytes.next() {
            if byte != b'\\' {
                out.push(byte);
                continue;
            }
            match bytes.next() {
                Some(b's') if key => out.push(b' '),
                Some(b'n') => out.push(b'\n'),
                Some(b'b') => out.push(b'\\'),
                Some(other) => out.extend([b'\\', other]),
                None => out.push(b'\\'),
            }
        }
        out
    }

    let (escaped, body) = match line.strip_prefix(b" ") {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let space = body.iter().position(|&byte| byte == b' ')?;
    let (key, target) = (&body[..space], &body[space + 1..]);
    Some(if escaped {
        (Cow::Owned(unescape(key, true)), Cow::Owned(unescape(target, false)))
    } else {
        (Cow::Borrowed(key), Cow::Borrowed(target))
    })
}

/// The root a manifest `target` names for `<self>.launch`, or `None` when the key and target do not both end with
/// `suffix` (`/<self>.launch`). Only a match allocates.
///
/// On Windows both ends are compared ASCII case-insensitively and the target's `\` reads as `/`.
fn manifest_root_bytes(key: &[u8], target: &[u8], suffix: &[u8], windows: bool) -> Option<Vec<u8>> {
    let ends_with = |text: &[u8]| {
        text.len() >= suffix.len()
            && text[text.len() - suffix.len()..]
                .iter()
                .zip(suffix)
                .all(|(&byte, &wanted)| {
                    if windows {
                        let byte = if byte == b'\\' { b'/' } else { byte };
                        byte.eq_ignore_ascii_case(&wanted)
                    } else {
                        byte == wanted
                    }
                })
    };
    // The key is compared as written: a `\` in it never reads as `/`.
    let key_ends_with = |text: &[u8]| {
        text.len() >= suffix.len()
            && if windows {
                text[text.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
            } else {
                text.ends_with(suffix)
            }
    };
    if !key_ends_with(key) || !ends_with(target) {
        return None;
    }
    let root = &target[..target.len() - suffix.len()];
    Some(if windows {
        root.iter()
            .map(|&byte| if byte == b'\\' { b'/' } else { byte })
            .collect()
    } else {
        root.to_vec()
    })
}

/// Whether the manifest `target` is the started path (ASCII case and separators folded on Windows).
fn lists_started_path(target: &[u8], started: &str, windows: bool) -> bool {
    if windows {
        fold(&String::from_utf8_lossy(target), true) == fold(started, true)
    } else {
        target == started.as_bytes()
    }
}

/// A manifest root as a path string, or the name of the rejection when its shape is not one the launcher follows:
/// absolute, UTF-8, free of NUL, CR and LF, no `.` or `..` segment and no path-list separator.
fn checked_manifest_root(root: &[u8], windows: bool) -> Result<String, String> {
    let Ok(text) = std::str::from_utf8(root) else {
        return Err(String::from_utf8_lossy(root).into_owned());
    };
    let text = if text.is_empty() && !windows { "/" } else { text };
    let list_separator = if windows { ';' } else { ':' };
    let hygienic = is_fully_qualified(text, windows)
        && !text.contains(['\0', '\r', '\n', list_separator])
        && !text
            .split(separators(windows))
            .any(|segment| segment == "." || segment == "..");
    if hygienic {
        Ok(text.to_string())
    } else {
        Err(text.to_string())
    }
}

/// Distinct roots in the order they were first pushed, at most [`MAX_MANIFEST_CANDIDATES`].
#[derive(Default)]
struct RootSet {
    seen: BTreeSet<Vec<u8>>,
    count: usize,
}

impl RootSet {
    /// Whether `root` is new and within the cap, so the caller examines it.
    fn push(&mut self, root: &[u8]) -> bool {
        if self.count < MAX_MANIFEST_CANDIDATES && self.seen.insert(root.to_vec()) {
            self.count += 1;
            return true;
        }
        false
    }
}

/// The root candidates one manifest names, or none when `needs_listing` and it never lists `started`.
fn scan_manifest(
    path: &str,
    needs_listing: bool,
    started: &str,
    self_path: &str,
    windows: bool,
    fs: &dyn LookupFs,
) -> Result<Vec<Vec<u8>>, ShimError> {
    let refused = |reason: String| refuse(format!("{reason}: {path}"));
    let lines = match fs.read_lines(path, MAX_MANIFEST_BYTES, MAX_MANIFEST_LINE) {
        Ok(Some(lines)) => lines,
        Ok(None) => return Ok(Vec::new()),
        Err(err) => return Err(manifest_refusal(path, err)),
    };
    let suffix = format!("/{self_path}.{RECORD_EXTENSION}");
    let mut found: Vec<Vec<u8>> = Vec::new();
    let mut lists = !needs_listing;
    for line in lines {
        let line = line.map_err(|err| manifest_refusal(path, err))?;
        if line.contains(&0) {
            return Err(refused("runfiles manifest line holds a NUL byte".to_string()));
        }
        // Bazel on Windows writes UTF-8; a line that is not cannot name a path it wrote.
        if windows && std::str::from_utf8(&line).is_err() {
            continue;
        }
        let Some((key, target)) = split_manifest_line(&line) else {
            continue;
        };
        if target.is_empty() {
            continue;
        }
        lists = lists || lists_started_path(&target, started, windows);
        if found.len() < MAX_MANIFEST_CANDIDATES
            && let Some(root) = manifest_root_bytes(&key, &target, suffix.as_bytes(), windows)
            && !found.contains(&root)
        {
            found.push(root);
        }
    }
    Ok(if lists { found } else { Vec::new() })
}

/// The refusal for a manifest that cannot be read within its bounds.
fn manifest_refusal(path: &str, err: ReadError) -> ShimError {
    refuse(match err {
        ReadError::StreamTooLarge => format!("runfiles manifest larger than 64 MiB: {path}"),
        ReadError::LineTooLong => format!("runfiles manifest line longer than 64 KiB: {path}"),
        other => format!("cannot read runfiles manifest {path}: {other}"),
    })
}

/// The runfiles manifests candidate (d) reads, in order, each with whether it must list the started path:
/// `<started>.runfiles/MANIFEST`, `<started>.runfiles_manifest`, then `$RUNFILES_MANIFEST_FILE`.
///
/// A path that resolves to one already named is dropped, so a file that two names reach is read once.
fn manifest_sources(started: &str, context: &LookupContext<'_>) -> Vec<(String, bool)> {
    let windows = context.windows;
    let mut sources: Vec<(String, bool)> = Vec::new();
    let named = [
        (format!("{started}.runfiles/MANIFEST"), false),
        (format!("{started}.runfiles_manifest"), false),
    ]
    .into_iter()
    .chain(context.manifest_file.map(|path| (path.to_string(), true)));
    let mut resolved: Vec<String> = Vec::new();
    for (path, needs_listing) in named {
        let key = fold(&absolutize(&path, context.cwd, windows), windows);
        if !resolved.contains(&key) {
            resolved.push(key);
            sources.push((path, needs_listing));
        }
    }
    sources
}

/// The started path on Unix: `argv[0]` joined onto the working directory, or the first `PATH` hit for a bare name.
#[cfg(unix)]
pub(super) fn started_path_unix(
    argv0: &str,
    cwd: &str,
    path_var: Option<&str>,
    fs: &dyn LookupFs,
) -> Result<String, ShimError> {
    if argv0.contains('/') {
        return Ok(absolutize(argv0, cwd, false));
    }
    path_var
        .unwrap_or_default()
        .split(':')
        .map(|entry| join_path(&absolutize(entry, cwd, false), argv0, false))
        .find(|candidate| fs.is_executable_file(candidate))
        .ok_or(ShimError::SelfPathFailure)
}

/// The lookup from the process's own `argv[0]` and `PATH`; a missing `argv0` is the same own-path failure as a name
/// that resolves nowhere.
#[cfg(unix)]
pub(super) fn lookup_unix(
    argv0: Option<&str>,
    context: &LookupContext<'_>,
    path_var: Option<&str>,
    fs: &dyn LookupFs,
) -> Result<Probed, ShimError> {
    locate_with(
        || {
            if context.cwd.is_empty() {
                return Err(ShimError::SelfPathFailure);
            }
            started_path_unix(argv0.ok_or(ShimError::SelfPathFailure)?, context.cwd, path_var, fs)
        },
        context,
        fs,
    )
}

/// Reads `path` as a record file: `Ok(None)` when absent, the refusal naming `path` when it cannot be read.
fn read_record(path: &str, fs: &dyn LookupFs) -> Result<Option<Vec<u8>>, ShimError> {
    fs.read_sidecar(path).map_err(|err| {
        refuse(format!(
            "cannot read launch record {path}: {err}; check that the file is readable"
        ))
    })
}

/// [`locate_with`] over a started path that is already known.
#[cfg(test)]
pub(super) fn locate(started: &str, context: &LookupContext<'_>, fs: &dyn LookupFs) -> Result<Probed, ShimError> {
    locate_with(|| Ok(started.to_string()), context, fs)
}

/// The accepted root made absolute by a lexical join, refused when it holds the path-list separator.
fn bind_root(sidecar: DirectRecord, candidate: &str, context: &LookupContext<'_>) -> Result<Probed, ShimError> {
    let root = absolutize(candidate, context.cwd, context.windows);
    let list_separator = if context.windows { ';' } else { ':' };
    if root.contains(list_separator) {
        return Err(refuse(format!(
            "vendor root {root} contains the path-list separator '{list_separator}'"
        )));
    }
    Ok(Probed::Direct(Located { sidecar, root }))
}

/// Decides the kind from the record beside the loaded image, then runs the started-path rules for kind `direct`.
///
/// A record of a home kind beside the image runs from it, whatever path the launcher was started by, so `started` is
/// called only once the record is absent or of kind `direct` and its failure never reaches a home launcher. Kind `direct`
/// (no record beside the image, or one of that kind) keeps the started path, because a runfiles tree reaches the
/// launcher through links. Its root is the first candidate, in candidate order, whose own `<self>.launch` is
/// byte-identical to the record beside the started path, so a stale tree holding another `vendor_id` is never taken.
pub(super) fn locate_with(
    started: impl FnOnce() -> Result<String, ShimError>,
    context: &LookupContext<'_>,
    fs: &dyn LookupFs,
) -> Result<Probed, ShimError> {
    let windows = context.windows;
    let image = fs.image_path()?;
    let image_record = record_path_for(&image, windows);
    let beside_image = read_record(&image_record, fs)?;
    if let Some(raw) = &beside_image {
        let record = LaunchRecord::decode(raw, windows)
            .map_err(|err| refuse(format!("malformed launch record {image_record}: {err}; restore the launcher and its record from the same install")))?;
        if !matches!(record, LaunchRecord::Direct(_)) {
            return core::HomeLaunch::from_record(record)
                .map(|home| Probed::Home(HomeFound { home, image }))
                .ok_or_else(|| refuse(format!("malformed launch record {image_record}: not a runnable record; restore the launcher and its record from the same install")));
        }
    }

    let started = started()?;
    let started = started.as_str();
    let (_, file_name) = split_file_name(started, windows);
    if (if windows { strip_exe(file_name) } else { file_name }).is_empty() {
        return Err(ShimError::SelfPathFailure);
    }
    // The image serves kind `direct` for this check only: a home kind never compares a `stat` with a later `open`.
    #[cfg(unix)]
    if !fs.is_running_image(started) {
        return Err(refuse(format!(
            "started path {started} is not the file this launcher runs from; start the launcher by the path of its own file"
        )));
    }

    let record_path = record_path_for(started, windows);
    let raw = match if record_path == image_record {
        Ok(beside_image)
    } else {
        read_record(&record_path, fs)
    }? {
        Some(raw) => raw,
        None => {
            return Err(refuse(format!(
                "launch record not found: {record_path}; keep the launcher beside its NAME.launch record"
            )));
        }
    };
    let record = LaunchRecord::decode(&raw, windows).map_err(|err| {
        refuse(format!(
            "malformed launch record {record_path}: {err}; restore the launcher and its record from the same install"
        ))
    })?;
    let LaunchRecord::Direct(sidecar) = record else {
        return Err(refuse(format!(
            "launch record {record_path} does not describe a vendored tool; start the launcher from its own directory, not through a link to another launcher"
        )));
    };
    let mut rejected: Vec<String> = Vec::new();
    for stage in 0..3 {
        let roots = match stage {
            0 => own_root(started, &sidecar.self_path, windows).into_iter().collect(),
            1 => entry_roots(&format!("{started}.runfiles"), windows, fs),
            _ => environment_runfiles_roots(started, context, fs),
        };
        for candidate in roots {
            if accepts(&candidate, &sidecar.self_path, &raw, windows, fs)? {
                return bind_root(sidecar, &candidate, context);
            }
            rejected.push(candidate);
        }
    }

    // The manifest is read last: only a tree holds declared links, and a remote action has no usable manifest. A
    // manifest is read only when no root of an earlier one was accepted.
    let mut record_bytes = 0u64;
    let mut roots = RootSet::default();
    for (path, needs_listing) in manifest_sources(started, context) {
        for root in scan_manifest(&path, needs_listing, started, &sidecar.self_path, windows, fs)? {
            if !roots.push(&root) {
                continue;
            }
            let root = match checked_manifest_root(&root, windows) {
                Ok(root) if record_bytes < MAX_CANDIDATE_RECORD_BYTES => root,
                Ok(root) | Err(root) => {
                    rejected.push(root);
                    continue;
                }
            };
            let found = candidate_record(&root, &sidecar.self_path, windows, fs)?;
            record_bytes += found.as_ref().map_or(0, |bytes| bytes.len() as u64);
            if found.is_some_and(|bytes| bytes == raw) {
                return bind_root(sidecar, &root, context);
            }
            rejected.push(root);
        }
    }

    let named: Vec<&str> = rejected.iter().take(NAMED_CANDIDATES).map(String::as_str).collect();
    Err(refuse(format!(
        "no vendored root found for {}: {} candidate(s) rejected: {}; neither a runfiles tree nor a runfiles manifest names the launch record; keep the launcher inside its vendored directory and check that its runfiles tree or manifest is present",
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
    sidecar: &DirectRecord,
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

/// Assembles the launch of a located record over the caller's environment and arguments.
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

/// The image path from the target of `/proc/self/exe`; the kernel appends ` (deleted)` once the file is unlinked, and
/// the record beside a path that no longer exists would be another directory's.
#[cfg(any(target_os = "linux", test))]
fn checked_image_link(target: &str) -> Result<String, ShimError> {
    if target.ends_with(" (deleted)") {
        return Err(refuse(format!(
            "the launcher image {target} was deleted after it started"
        )));
    }
    Ok(target.to_string())
}

/// The real filesystem.
struct RealFs;

impl LookupFs for RealFs {
    fn image_path(&self) -> Result<String, ShimError> {
        #[cfg(target_os = "linux")]
        {
            let target = std::fs::read_link("/proc/self/exe").map_err(|_| ShimError::SelfPathFailure)?;
            checked_image_link(target.to_str().ok_or(ShimError::SelfPathFailure)?)
        }
        #[cfg(target_os = "macos")]
        {
            let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
            // SAFETY: `buffer` holds `buffer.len()` writable bytes and that length is passed as the size; the call
            // writes at most that many and returns the byte count of the path, or 0 on failure.
            let written =
                unsafe { libc::proc_pidpath(libc::getpid(), buffer.as_mut_ptr().cast(), buffer.len() as u32) };
            if written <= 0 {
                return Err(ShimError::SelfPathFailure);
            }
            buffer.truncate(written as usize);
            String::from_utf8(buffer).map_err(|_| ShimError::SelfPathFailure)
        }
        #[cfg(windows)]
        {
            mapped_image_path()
        }
    }

    fn read_sidecar(&self, path: &str) -> Result<Option<Vec<u8>>, ReadError> {
        read_regular_bounded(Path::new(path))
    }

    fn read_lines(&self, path: &str, max_bytes: u64, max_line: usize) -> Result<Option<LineStream>, ReadError> {
        Ok(open_regular(Path::new(path))?
            .map(|(file, _)| bounded_lines(std::io::BufReader::new(file), max_bytes, max_line)))
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

/// Why [`fill_wide`] gave up.
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
enum WideFillFailure {
    /// The call failed for a reason other than a short buffer; carries its Win32 error.
    Failed(u32),
    /// The name does not fit in the largest buffer tried.
    TooLong,
}

/// Win32 `ERROR_INSUFFICIENT_BUFFER` and `ERROR_MORE_DATA`: a call reporting either with a zero return wants a larger
/// buffer. The Win32 constants are not named here because the unit tests run on every host.
#[cfg(any(windows, test))]
const SHORT_BUFFER_ERRORS: [u32; 2] = [122, 234];

/// Fills a wide-character buffer through `call`, growing it until the name fits, up to `max_units`.
///
/// `call` receives the buffer and returns `(written, error)`, `error` being the thread's last error when `written` is 0
/// and ignored otherwise. A short buffer comes back in three forms and each grows it: a zero return with
/// [`SHORT_BUFFER_ERRORS`] (`GetModuleFileNameW` on some systems), a return equal to the
/// capacity (a truncated name), and a return above it (`GetFinalPathNameByHandleW`: the size, NUL included, to retry
/// with). A return of `capacity - 1` may be a name cut to keep room for the NUL, so it grows too, and the retry returns
/// the same length when the name did fit. A return below that is the name's length, NUL excluded.
#[cfg(any(windows, test))]
fn fill_wide(max_units: usize, mut call: impl FnMut(&mut [u16]) -> (usize, u32)) -> Result<Vec<u16>, WideFillFailure> {
    let mut capacity: usize = 512;
    loop {
        let mut buffer = vec![0u16; capacity];
        let (written, error) = call(&mut buffer);
        let wanted = if written == 0 {
            if !SHORT_BUFFER_ERRORS.contains(&error) {
                return Err(WideFillFailure::Failed(error));
            }
            capacity * 2
        } else if written + 1 < capacity {
            buffer.truncate(written);
            return Ok(buffer);
        } else {
            (capacity * 2).max(written + 1)
        };
        if wanted > max_units {
            return Err(WideFillFailure::TooLong);
        }
        capacity = wanted;
    }
}

/// What a `NtQueryVirtualMemory(MemoryMappedFilenameInformation)` status asks of the caller.
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
enum SectionQuery {
    /// The structure is filled.
    Ready,
    /// The buffer was short: retry with this many bytes.
    Grow(usize),
    /// The call failed for a reason other than a short buffer; carries its `NTSTATUS`.
    Failed(i32),
    /// The name does not fit in `max_bytes`.
    TooLong,
}

/// `STATUS_BUFFER_OVERFLOW`, `STATUS_INFO_LENGTH_MISMATCH` and `STATUS_BUFFER_TOO_SMALL`: the buffer is short and
/// `ReturnLength` may carry the size wanted. Named by value because the unit tests run on every host.
#[cfg(any(windows, test))]
const SHORT_SECTION_BUFFER: [i32; 3] = [0x8000_0005_u32 as i32, 0xC000_0004_u32 as i32, 0xC000_0023_u32 as i32];

/// Reads `status` of a query that wrote into a `capacity`-byte buffer and reported `returned` bytes wanted.
///
/// A short buffer grows to `returned` when that is larger, else doubles: a call may leave `returned` unset.
#[cfg(any(windows, test))]
fn classify_section_query(status: i32, returned: usize, capacity: usize, max_bytes: usize) -> SectionQuery {
    if status >= 0 {
        return SectionQuery::Ready;
    }
    if !SHORT_SECTION_BUFFER.contains(&status) {
        return SectionQuery::Failed(status);
    }
    let wanted = if returned > capacity {
        returned
    } else {
        capacity.saturating_mul(2)
    };
    if wanted > max_bytes {
        SectionQuery::TooLong
    } else {
        SectionQuery::Grow(wanted)
    }
}

/// The name a filled `MEMORY_SECTION_NAME` holds, as UTF-16 units, or `None` when the structure lies.
///
/// `bytes` is the whole query buffer; `length` (BYTES) and `name_ptr` are the `UNICODE_STRING` the kernel wrote at its
/// start, `header_len` its size. The kernel puts the text inside the buffer, so a pointer outside it, before the
/// header, misaligned for UTF-16, or a length that is odd, zero or reaches past the end is refused, never followed.
#[cfg(any(windows, test))]
fn section_name_units(bytes: &[u8], length: u16, name_ptr: usize, header_len: usize) -> Option<Vec<u16>> {
    let offset = name_ptr.checked_sub(bytes.as_ptr() as usize)?;
    let length = usize::from(length);
    let end = offset.checked_add(length)?;
    if length == 0 || length % 2 != 0 || offset % 2 != 0 || offset < header_len || end > bytes.len() {
        return None;
    }
    Some(
        bytes[offset..end]
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect(),
    )
}

/// The thread's last error when `written` is 0, else 0: read before anything else can overwrite it.
#[cfg(windows)]
fn last_error_if_zero(written: usize) -> u32 {
    if written == 0 {
        // SAFETY: no arguments; reads thread-local last-error.
        unsafe { windows_sys::Win32::Foundation::GetLastError() }
    } else {
        0
    }
}

/// The refusal for a [`fill_wide`] failure at `step`.
#[cfg(windows)]
fn wide_fill_refusal(failure: WideFillFailure, step: &str) -> ShimError {
    match failure {
        WideFillFailure::Failed(code) => refuse(format!(
            "cannot resolve the launcher image: {step} failed (error {code})"
        )),
        WideFillFailure::TooLong => refuse(format!("the launcher image path from {step} is longer than 64K units")),
    }
}

/// A wide string from `buffer[..length]` as UTF-8, or the refusal naming `step`.
#[cfg(windows)]
fn wide_to_string(buffer: &[u16], step: &str) -> Result<String, ShimError> {
    use std::os::windows::ffi::OsStringExt;

    OsString::from_wide(buffer)
        .into_string()
        .map_err(|_| refuse(format!("the launcher image path from {step} is not valid Unicode")))
}

/// The refusal for a failed Win32 step of the image path.
#[cfg(windows)]
fn image_step_failed(step: &str) -> ShimError {
    // SAFETY: no arguments; reads thread-local last-error.
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    refuse(format!(
        "cannot resolve the launcher image: {step} failed (error {code})"
    ))
}

/// The image path on Windows: the file mapped at the executable's module base, resolved by handle.
///
/// `GetModuleFileNameW` returns the path the process was started by, which is the `active` junction when the launcher
/// is reached through one. The mapped file is the one the loader opened, so its final path names the render
/// directory behind the junction; no `stat` is compared with a later `open`.
///
/// The mapped name comes from `NtQueryVirtualMemory` itself: `GetMappedFileNameW` wraps it with a `MAX_PATH` buffer
/// of its own, so it fails for every caller buffer once the image path passes 260 characters.
#[cfg(windows)]
fn mapped_image_path() -> Result<String, ShimError> {
    use windows_sys::Wdk::Storage::FileSystem::NtQueryVirtualMemory;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, UNICODE_STRING};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetFinalPathNameByHandleW, OPEN_EXISTING, VOLUME_NAME_DOS,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    const MAX_UNITS: usize = 64 * 1024;
    /// `MemoryMappedFilenameInformation`: the file name behind the mapping at an address.
    const MEMORY_MAPPED_FILENAME_INFORMATION: i32 = 2;
    const STEP_MAPPED: &str = "NtQueryVirtualMemory";
    const STEP_OPEN: &str = "opening the mapped file";
    const STEP_FINAL: &str = "GetFinalPathNameByHandleW";
    const HEADER: usize = std::mem::size_of::<UNICODE_STRING>();

    // SAFETY: a null name asks for the module the process was created from; the returned base is only an address.
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    if module.is_null() {
        return Err(image_step_failed("GetModuleHandleW"));
    }

    // The device path (`\Device\HarddiskVolumeN\…`), read from a `MEMORY_SECTION_NAME` in a `u64`-backed buffer
    // so the `UNICODE_STRING` at its start is aligned.
    let max_bytes = MAX_UNITS * 2 + HEADER;
    let mut capacity = 2048_usize;
    let mut device = loop {
        let mut backing = vec![0u64; capacity.div_ceil(8)];
        let mut returned = 0_usize;
        // SAFETY: `backing` holds `capacity.div_ceil(8) * 8` writable bytes, at least the `capacity` passed; `module`
        // lies inside this image; `returned` is a valid out pointer.
        let status = unsafe {
            NtQueryVirtualMemory(
                GetCurrentProcess(),
                module.cast(),
                MEMORY_MAPPED_FILENAME_INFORMATION,
                backing.as_mut_ptr().cast(),
                capacity,
                &mut returned,
            )
        };
        match classify_section_query(status, returned, capacity, max_bytes) {
            SectionQuery::Grow(wanted) => capacity = wanted,
            SectionQuery::TooLong => {
                return Err(refuse(format!(
                    "the launcher image path from {STEP_MAPPED} is longer than 64K units"
                )));
            }
            SectionQuery::Failed(code) => {
                return Err(refuse(format!(
                    "cannot resolve the launcher image: {STEP_MAPPED} failed (status {:#010x})",
                    code as u32
                )));
            }
            SectionQuery::Ready => {
                // SAFETY: `backing` is zero-initialised, 8-aligned and at least `HEADER` bytes by construction (`capacity`
                // starts at 2048 and only grows), so the read is in bounds whatever the kernel wrote.
                let section = unsafe { std::ptr::read(backing.as_ptr().cast::<UNICODE_STRING>()) };
                // SAFETY: the same allocation viewed as `capacity.div_ceil(8) * 8` initialized bytes.
                let bytes = unsafe { std::slice::from_raw_parts(backing.as_ptr().cast::<u8>(), backing.len() * 8) };
                break section_name_units(bytes, section.Length, section.Buffer as usize, HEADER).ok_or_else(|| {
                    refuse(format!(
                        "cannot resolve the launcher image: {STEP_MAPPED} returned an invalid name"
                    ))
                })?;
            }
        }
    };

    // `\\?\GLOBALROOT` + the device path names the object by its NT path, which `CreateFileW` accepts.
    let mut name: Vec<u16> = r"\\?\GLOBALROOT".encode_utf16().collect();
    name.append(&mut device);
    name.push(0);
    // SAFETY: `name` is NUL-terminated; zero access (attributes only) and full sharing keep this open from blocking
    // anything; the backup flag lets `CreateFileW` open a directory-like object.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(image_step_failed(STEP_OPEN));
    }

    let resolved = fill_wide(MAX_UNITS, |buffer| {
        // SAFETY: `handle` is open and `buffer` holds `buffer.len()` u16s, passed as `cchfilepath`.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        (length as usize, last_error_if_zero(length as usize))
    })
    .map_err(|failure| wide_fill_refusal(failure, STEP_FINAL))
    .and_then(|buffer| wide_to_string(&buffer, STEP_FINAL));
    // SAFETY: `handle` is the valid handle opened above and is closed once.
    unsafe { CloseHandle(handle) };

    // `\\?\C:\…` and `\\?\UNC\…` lose their verbatim prefix where an ordinary path names the same file, so a
    // comparison with a `PATH` entry is not defeated by the prefix.
    Ok(dunce::simplified(Path::new(&resolved?)).to_string_lossy().into_owned())
}

impl core::ProgramFs for RealFs {
    fn is_program_file(&self, path: &str) -> bool {
        #[cfg(unix)]
        {
            self.is_executable_file(path)
        }
        #[cfg(windows)]
        {
            self.is_regular_file(path)
        }
    }

    fn is_regular_file(&self, path: &str) -> bool {
        std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
    }
}

/// What a home launch starts: the absolute program and the complete child environment.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct HomeStart {
    pub program: String,
    /// Where `program` came from, so a missing file names the setting to fix.
    pub source: core::ProgramSource,
    pub environment: Vec<(OsString, OsString)>,
}

/// Decides what a home launch starts over `inherited`: the depth guard and the environment first, then the program.
///
/// Nothing starts here, so every refusal is a 78 before the first process of the chain.
pub(super) fn plan_home(
    home: &core::HomeLaunch,
    launcher_directory: &str,
    inherited: Vec<(OsString, OsString)>,
    windows: bool,
    fs: &dyn core::ProgramFs,
) -> Result<HomeStart, ShimError> {
    let environment = core::home_environment(inherited, home, windows)?;
    let pin = match core::environment_value(&environment, "OCX_BINARY_PIN", windows) {
        Some(value) => Some(
            value
                .to_str()
                .ok_or_else(|| refuse("OCX_BINARY_PIN is not valid Unicode".to_string()))?,
        ),
        None => None,
    };
    let path = core::environment_value(&environment, "PATH", windows).map(|value| value.to_string_lossy());
    let inputs = core::ProgramInputs {
        pin,
        baked: home.baked_ocx(),
        path: path.as_deref(),
        launcher_directory,
        windows,
    };
    let program = core::resolve_program(&inputs, fs)?;
    Ok(HomeStart {
        program,
        source: core::program_source(&inputs),
        environment,
    })
}

#[cfg(windows)]
fn canonicalize(path: &Path) -> std::io::Result<std::path::PathBuf> {
    dunce::canonicalize(path)
}

#[cfg(unix)]
fn canonicalize(path: &Path) -> std::io::Result<std::path::PathBuf> {
    std::fs::canonicalize(path)
}

/// Refuses a package root outside the containment roots of `OCX_HOME`; `launcher exec` holds the primary check.
///
/// The check runs only when `OCX_HOME` resolves. A root that fails to resolve under a good `OCX_HOME` is suspicious,
/// so it is delegated, with a line saying so.
fn check_containment(home: &core::HomeLaunch, ocx_home: Option<&OsStr>) -> Result<(), ShimError> {
    let (Some(pkg_root), Some(ocx_home)) = (home.containment_path(), ocx_home) else {
        return Ok(());
    };
    let Ok(canonical_home) = canonicalize(Path::new(ocx_home)) else {
        return Ok(());
    };
    match canonicalize(Path::new(pkg_root)) {
        Ok(canonical_root) if !core::pkg_root_allowed(&canonical_home, &canonical_root) => {
            Err(ShimError::ContainmentViolation {
                path: canonical_root.display().to_string(),
            })
        }
        Ok(_) => Ok(()),
        Err(err) => {
            eprintln!("{}", canonicalize_notice(pkg_root, &err.to_string()));
            Ok(())
        }
    }
}

/// The stderr line for a package root that cannot be canonicalized; the root comes from a record file.
pub(super) fn canonicalize_notice(pkg_root: &str, err: &str) -> String {
    format!(
        "ocx-shim: cannot canonicalize package root {} ({err}); delegating containment to `launcher exec`",
        super::escape_controls(pkg_root)
    )
}

/// The containment check and the plan of a home launch over the process environment; `image` is the launcher's
/// image path, whose directory the program search skips.
pub(super) fn prepare_home(home: &core::HomeLaunch, image: &str, windows: bool) -> Result<HomeStart, ShimError> {
    #[expect(clippy::disallowed_methods, reason = "no ocx_env edge: the shim's size budget")]
    let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    check_containment(home, core::environment_value(&inherited, "OCX_HOME", windows))?;
    plan_home(home, split_file_name(image, windows).0, inherited, windows, &RealFs)
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

/// Starts `ocx` for a home-kind record by replacing this process with it; returns only the failure.
#[cfg(unix)]
fn run_home(home: &core::HomeLaunch, image: &str, caller_args: Vec<OsString>) -> Result<i32, ShimError> {
    let start = prepare_home(home, image, false)?;
    let mut args: Vec<OsString> = home
        .arguments(split_file_name(image, false).1)
        .into_iter()
        .map(OsString::from)
        .collect();
    args.extend(caller_args);
    let error = exec_program(&start.program, &args, start.environment);
    Err(ShimError::DirectExecFailed {
        program: start.program,
        permission_denied: error.kind() == std::io::ErrorKind::PermissionDenied,
        reason: error.to_string(),
    })
}

/// The working directory as a path string; empty when it is gone, unreadable or not Unicode.
fn working_directory() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|dir| dir.into_os_string().into_string().ok())
        .unwrap_or_default()
}

/// The `.launch` launcher on Unix: reads the record, finds the vendor root, builds the environment and argv, and
/// `exec`s the tool.
///
/// Writes nothing to disk and sets no environment of its own, so it runs from a read-only directory.
#[cfg(unix)]
pub(super) fn run() -> Result<i32, ShimError> {
    #[expect(clippy::disallowed_methods, reason = "no ocx_env edge: the shim's size budget")]
    let variable = |name: &str| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned());

    let mut arguments = std::env::args_os();
    let argv0 = arguments.next().and_then(|argument| argument.into_string().ok());
    let cwd = working_directory();

    let runfiles_dir = variable("RUNFILES_DIR");
    let manifest_file = variable("RUNFILES_MANIFEST_FILE");
    let context = LookupContext {
        cwd: &cwd,
        runfiles_dir: runfiles_dir.as_deref(),
        manifest_file: manifest_file.as_deref(),
        windows: false,
    };
    let located = match lookup_unix(argv0.as_deref(), &context, variable("PATH").as_deref(), &RealFs)? {
        Probed::Direct(located) => located,
        Probed::Home(found) => return run_home(&found.home, &found.image, arguments.collect()),
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

/// The `.launch` launcher on Windows: reads the record, then starts a home kind's `ocx` or, for kind `direct`, finds the
/// vendor root and starts the tool.
///
/// A batch target goes through `std::process::Command`, whose batch-file escaping refuses what it cannot quote
/// safely (BatBadBut, CVE-2024-24576), and anything else through the existing `CreateProcessW` path.
#[cfg(windows)]
pub(super) fn run_windows(started: &str) -> Result<i32, ShimError> {
    #[expect(clippy::disallowed_methods, reason = "no ocx_env edge: the shim's size budget")]
    let runfiles_dir = std::env::var_os("RUNFILES_DIR").map(|value| value.to_string_lossy().into_owned());
    #[expect(clippy::disallowed_methods, reason = "no ocx_env edge: the shim's size budget")]
    let manifest_file = std::env::var_os("RUNFILES_MANIFEST_FILE").map(|value| value.to_string_lossy().into_owned());
    let cwd = working_directory();
    let context = LookupContext {
        cwd: &cwd,
        runfiles_dir: runfiles_dir.as_deref(),
        manifest_file: manifest_file.as_deref(),
        windows: true,
    };
    let started_path = || {
        if cwd.is_empty() {
            return Err(ShimError::SelfPathFailure);
        }
        Ok(started.to_string())
    };
    let located = match locate_with(started_path, &context, &RealFs)? {
        Probed::Direct(located) => located,
        Probed::Home(found) => {
            let stem = core::derive_stem(OsStr::new(&found.image))?;
            return super::run_home(&found.home, &found.image, &stem);
        }
    };

    #[expect(clippy::disallowed_methods, reason = "no ocx_env edge: the shim's size budget")]
    let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let launch = plan_launch(&located, inherited, std::env::args_os().skip(1).collect(), true);
    if ocx_env_apply::launch::is_batch_target(&launch.program) {
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
    super::spawn_child(&launch.program, &command_line, Some(&block)).map_err(|win32| ShimError::SpawnFailure {
        win32,
        program: launch.program.clone(),
    })
}

/// The `CREATE_UNICODE_ENVIRONMENT` block of `env`: `KEY=VALUE\0` entries, then one more `\0`.
#[cfg(windows)]
pub(super) fn environment_block(env: &[(OsString, OsString)]) -> Vec<u16> {
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

/// The line a batch target's non-zero exit earns when a path-length limit is its plausible cause; `None` otherwise.
///
/// cmd.exe limits every path to MAX_PATH, so a script path that long never runs and a non-zero exit is cmd.exe's own
/// refusal, never the script's.
#[cfg(any(windows, test))]
fn batch_failure_line(program: &str, code: i32) -> Option<String> {
    if code == 0 {
        return None;
    }
    let hint = super::length_limit_hint(3, 0, program)?;
    Some(format!(
        "ocx-shim: failed to start {} through cmd.exe: exit {code}; {hint}",
        super::escape_controls(program)
    ))
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
                // std refuses only a line break; name the argument by position, never its text, which may be a secret.
                let line_break = launch.args.iter().position(|argument| {
                    argument
                        .as_encoded_bytes()
                        .iter()
                        .any(|byte| matches!(byte, b'\r' | b'\n'))
                });
                ShimError::DirectArgumentRefused {
                    reason: match line_break {
                        Some(index) => format!(
                            "argument {} for {} cannot be passed to a batch file safely: it holds a line break",
                            index + 1,
                            launch.program
                        ),
                        None => format!(
                            "an argument for {} cannot be passed to a batch file safely",
                            launch.program
                        ),
                    },
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
        Ok(status) => {
            let code = status.code().unwrap_or(1);
            // cmd.exe limits every path to MAX_PATH, so a script path that long never runs and a non-zero exit is
            // cmd.exe's own refusal, never the script's.
            if let Some(line) = batch_failure_line(&launch.program, code) {
                eprintln!("{line}");
            }
            Ok(code)
        }
        Err(error) => Err(ShimError::SpawnFailure {
            win32: error.raw_os_error().map_or(0, |code| code as u32),
            program: launch.program.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_env_apply::launch::is_batch_target;
    use std::collections::{HashMap, HashSet};

    const SELF_PATH: &str = "selections/default/shells/default/bin/cmake";
    const VENDOR_A: &str = "sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a";
    /// The image of a `FakeFs` that sets none: nothing is recorded beside it.
    const NO_IMAGE: &str = "/nowhere/image";
    const VENDOR_B: &str = "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    fn sidecar_json(vendor_id: &str, self_path: &str) -> Vec<u8> {
        format!(
            r#"{{"kind":"direct","schema_version":1,"vendor_id":"{vendor_id}","self":"{self_path}","program":"@VENDOR_DIR@/packages/p/content/bin/cmake","args":[],"unset":[],"env":[]}}"#
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
        /// The loaded image; `None` is an image with no record beside it.
        image: Option<String>,
        /// Started paths that `stat` as the image (a symlink to it).
        #[cfg(unix)]
        links: HashSet<String>,
        /// Every path `read_lines` was asked for, in order.
        line_reads: std::cell::RefCell<Vec<String>>,
        /// How many times the lookup asked whether a started path is the running image.
        #[cfg(unix)]
        identity_checks: std::cell::Cell<usize>,
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
        fn image_path(&self) -> Result<String, ShimError> {
            Ok(self.image.clone().unwrap_or_else(|| NO_IMAGE.to_string()))
        }
        fn read_sidecar(&self, path: &str) -> Result<Option<Vec<u8>>, ReadError> {
            if self.unreadable.contains(path) {
                return Err(ReadError::NotRegular);
            }
            Ok(self.files.get(path).cloned())
        }
        fn read_lines(&self, path: &str, max_bytes: u64, max_line: usize) -> Result<Option<LineStream>, ReadError> {
            self.line_reads.borrow_mut().push(path.to_string());
            if self.unreadable.contains(path) {
                return Err(ReadError::NotRegular);
            }
            Ok(self
                .files
                .get(path)
                .map(|bytes| bounded_lines(std::io::Cursor::new(bytes.clone()), max_bytes, max_line)))
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
            self.identity_checks.set(self.identity_checks.get() + 1);
            match self.image.as_deref() {
                None => true,
                Some(image) => image == path || self.links.contains(path),
            }
        }
    }

    fn unix(runfiles_dir: Option<&str>) -> LookupContext<'_> {
        LookupContext {
            cwd: "/work",
            runfiles_dir,
            manifest_file: None,
            windows: false,
        }
    }

    fn direct(probed: Probed) -> Located {
        match probed {
            Probed::Direct(located) => located,
            Probed::Home(found) => panic!("expected a direct record, found {found:?}"),
        }
    }

    fn refusal(result: Result<Probed, ShimError>) -> String {
        let err = result.expect_err("the lookup must refuse");
        assert_eq!(err.exit_code(), 78, "{}", err.stderr_message());
        err.stderr_message()
    }

    #[test]
    fn v52_a_direct_sidecar_is_accepted_on_unix() {
        let started = format!("/vendor/{SELF_PATH}");
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default().with_file(&format!("{started}.launch"), &bytes);
        let located = direct(locate(&started, &unix(None), &fs).unwrap());
        assert_eq!(located.root, "/vendor");
        assert_eq!(located.sidecar.vendor_id, VENDOR_A);
    }

    // ── the launcher reads only NAME.launch ─────────────────────────

    fn windows_context() -> LookupContext<'static> {
        LookupContext {
            cwd: "C:\\work",
            runfiles_dir: None,
            manifest_file: None,
            windows: true,
        }
    }

    /// The started path, its stem path (no `.exe` on Windows) and the context of each platform.
    fn platforms() -> [(String, String, LookupContext<'static>); 2] {
        let windows_stem = format!("C:\\vendor\\{}", SELF_PATH.replace('/', "\\"));
        [
            (
                format!("/vendor/{SELF_PATH}"),
                format!("/vendor/{SELF_PATH}"),
                unix(None),
            ),
            (format!("{windows_stem}.exe"), windows_stem, windows_context()),
        ]
    }

    #[test]
    fn vl08_a_launcher_beside_only_an_old_sidecar_exits_78_naming_the_record() {
        for (started, stem_path, context) in platforms() {
            for extension in ["shim", "shimref", "exec", "direct"] {
                let fs = FakeFs::default().with_file(&format!("{stem_path}.{extension}"), b"/some/root\n");
                let message = refusal(locate(&started, &context, &fs));
                assert!(
                    message.contains("cmake.launch"),
                    "{extension} on windows={}: {message}",
                    context.windows
                );
            }
        }
    }

    #[test]
    fn vl08_control_a_record_beside_an_old_sidecar_is_the_one_read() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        for (started, stem_path, context) in platforms() {
            // The root candidate spells its record path with `/`, so the Windows fake needs that spelling too.
            let root_record = format!(
                "{}/{SELF_PATH}.launch",
                if context.windows { "C:\\vendor" } else { "/vendor" }
            );
            let fs = FakeFs::default()
                .with_file(&format!("{stem_path}.shim"), b"/some/root\n")
                .with_file(&format!("{stem_path}.launch"), &bytes)
                .with_file(&root_record, &bytes);
            let located = direct(locate(&started, &context, &fs).unwrap());
            assert_eq!(located.sidecar.vendor_id, VENDOR_A, "windows={}", context.windows);
        }
    }

    // ── Root candidates ────────────────────────────────────────────────────

    #[test]
    fn candidate_a_strips_the_launcher_path_from_the_started_path_on_windows() {
        let started = "C:\\Vendor\\Selections\\Default\\Shells\\Default\\BIN\\CMAKE.EXE";
        let sidecar_path = "C:\\Vendor\\Selections\\Default\\Shells\\Default\\BIN\\CMAKE.launch";
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file(sidecar_path, &bytes)
            .with_file(&format!("C:\\Vendor/{SELF_PATH}.launch"), &bytes);
        let context = LookupContext {
            cwd: "C:\\work",
            runfiles_dir: None,
            manifest_file: None,
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
            .with_file("/out/bin/tool.launch", &bytes)
            .with_dir("/out/bin/tool.runfiles", &["_main", "pkg"])
            .with_file(&format!("/out/bin/tool.runfiles/pkg/{SELF_PATH}.launch"), &bytes);
        let located = direct(locate(started, &unix(None), &fs).unwrap());
        assert_eq!(located.root, "/out/bin/tool.runfiles/pkg");
    }

    #[test]
    fn candidate_b_skips_an_entry_whose_sidecar_has_another_vendor_id() {
        let started = "/out/bin/tool";
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &sidecar_json(VENDOR_A, SELF_PATH))
            .with_dir("/out/bin/tool.runfiles", &["a_other", "b_mine"])
            .with_file(
                &format!("/out/bin/tool.runfiles/a_other/{SELF_PATH}.launch"),
                &sidecar_json(VENDOR_B, SELF_PATH),
            )
            .with_file(
                &format!("/out/bin/tool.runfiles/b_mine/{SELF_PATH}.launch"),
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
            .with_file("/out/bin/tool.launch", &bytes)
            .with_dir("/out/bin/tool.runfiles", &refs)
            .with_file(
                &format!("/out/bin/tool.runfiles/{}/{SELF_PATH}.launch", names[position]),
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
            .with_file("/out/bin/tool.launch", &bytes)
            .with_dir("/out/bin/tool.runfiles", &["zz", "aa"])
            .with_file(&format!("/out/bin/tool.runfiles/zz/{SELF_PATH}.launch"), &bytes)
            .with_file(&format!("/out/bin/tool.runfiles/aa/{SELF_PATH}.launch"), &bytes);
        let located = direct(locate("/out/bin/tool", &unix(None), &fs).unwrap());
        assert_eq!(located.root, "/out/bin/tool.runfiles/aa");
    }

    /// A `RUNFILES_DIR` that does not hold the started path is never consulted, even when its entry matches.
    #[test]
    fn v59_a_stale_runfiles_dir_is_not_used_when_the_started_path_is_outside_it() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/own/tool.launch", &bytes)
            .with_dir("/stale/runfiles", &["tree"])
            .with_file(&format!("/stale/runfiles/tree/{SELF_PATH}.launch"), &bytes);
        let message = refusal(locate("/own/tool", &unix(Some("/stale/runfiles")), &fs));
        assert!(message.contains("no vendored root"), "{message}");
    }

    /// The refusal never tells anyone to build with `--enable_runfiles`: the manifest fallback makes the flag needless.
    #[test]
    fn v45_no_root_names_the_missing_tree_and_manifest_on_every_host() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default().with_file("C:\\out\\bin\\tool.launch", &bytes);
        let context = LookupContext {
            cwd: "C:\\work",
            runfiles_dir: None,
            manifest_file: None,
            windows: true,
        };
        let message = refusal(locate("C:\\out\\bin\\tool.exe", &context, &fs));
        assert!(message.contains("no vendored root found"), "{message}");
        assert!(!message.contains("--enable_runfiles"), "{message}");
        assert!(
            message.contains("neither a runfiles tree nor a runfiles manifest"),
            "{message}"
        );

        let fs = FakeFs::default().with_file("/out/bin/tool.launch", &bytes);
        let message = refusal(locate("/out/bin/tool", &unix(None), &fs));
        assert!(message.contains("no vendored root found"), "{message}");
        assert!(!message.contains("--enable_runfiles"), "{message}");
        assert!(
            message.ends_with("check that its runfiles tree or manifest is present"),
            "{message}"
        );
    }

    #[test]
    fn a_missing_sidecar_message_names_the_remedy() {
        let message = refusal(locate("/own/tool", &unix(None), &FakeFs::default()));
        assert!(message.contains("launch record not found: "), "{message}");
        assert!(
            message.ends_with("keep the launcher beside its NAME.launch record"),
            "{message}"
        );
    }

    #[test]
    fn v59_control_the_runfiles_dir_is_used_when_the_started_path_is_under_it() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/runfiles/_main/tool.launch", &bytes)
            .with_dir("/runfiles", &["tree"])
            .with_file(&format!("/runfiles/tree/{SELF_PATH}.launch"), &bytes);
        let located = direct(locate("/runfiles/_main/tool", &unix(Some("/runfiles")), &fs).unwrap());
        assert_eq!(located.root, "/runfiles/tree");
    }

    #[test]
    fn v59_a_prefix_that_is_not_a_path_component_is_not_under_the_runfiles_dir() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/runfiles-old/tool.launch", &bytes)
            .with_dir("/runfiles", &["tree"])
            .with_file(&format!("/runfiles/tree/{SELF_PATH}.launch"), &bytes);
        refusal(locate("/runfiles-old/tool", &unix(Some("/runfiles")), &fs));
    }

    #[test]
    fn a_missing_root_names_the_first_eight_candidates_and_the_count() {
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let names: Vec<String> = (0..12).map(|i| format!("e{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &bytes)
            .with_dir("/out/bin/tool.runfiles", &refs);
        let message = refusal(locate("/out/bin/tool", &unix(None), &fs));
        assert!(message.contains("12 candidate"), "{message}");
        assert!(message.contains("e07") && !message.contains("e08"), "{message}");
    }

    #[test]
    fn a_missing_sidecar_names_the_path_it_tried() {
        let message = refusal(locate("/vendor/bin/cmake", &unix(None), &FakeFs::default()));
        assert!(message.contains("/vendor/bin/cmake.launch"), "{message}");
    }

    // ── Root shape ────────────────────────────────────────────

    fn root_under(dir: &str) -> Result<Probed, ShimError> {
        let started = format!("{dir}/{SELF_PATH}");
        let fs = FakeFs::default().with_file(&format!("{started}.launch"), &sidecar_json(VENDOR_A, SELF_PATH));
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
            manifest_file: None,
            windows: true,
        };
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        for (root, accepted) in [("C:\\a;b", false), ("C:\\ab", true)] {
            let started = format!("{root}\\{}.exe", SELF_PATH.replace('/', "\\"));
            let sidecar_path = format!("{root}\\{}.launch", SELF_PATH.replace('/', "\\"));
            let fs = FakeFs::default()
                .with_file(&sidecar_path, &bytes)
                .with_file(&format!("{root}/{SELF_PATH}.launch"), &bytes);
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

    /// A direct launcher started by a path that is not the loaded image.
    #[test]
    #[cfg(unix)]
    fn v61_a_direct_launcher_whose_started_path_is_not_the_image_is_refused() {
        let started = "/shadow/cmake";
        let fs = FakeFs {
            image: Some("/real/cmake".to_string()),
            ..FakeFs::default()
        }
        .with_file(&format!("{started}.launch"), &sidecar_json(VENDOR_A, SELF_PATH));
        let message = refusal(locate(started, &unix(None), &fs));
        assert!(message.contains(started), "{message}");
    }

    /// A home launcher needs no started path: `execve("/abs/launcher", ["cmake"])` with `cmake` off `PATH` runs it.
    #[test]
    #[cfg(unix)]
    fn a_home_launcher_started_by_a_bare_argv0_off_path_runs_its_record() {
        let image = "/store/render/bin/cmake";
        let fs = FakeFs {
            image: Some(image.to_string()),
            ..FakeFs::default()
        }
        .with_file(
            &format!("{image}.launch"),
            br#"{"kind":"package","schema_version":1,"package_root":"/p"}"#,
        );
        let found = found(lookup_unix(Some("cmake"), &unix(None), Some(""), &fs));
        assert_eq!(found.image, image);
    }

    /// A direct launcher still needs its started path, and refuses when the name resolves nowhere.
    #[test]
    #[cfg(unix)]
    fn a_direct_launcher_started_by_an_unresolvable_argv0_is_an_own_path_failure() {
        let fs = FakeFs::default().with_file("/store/render/bin/cmake.launch", &sidecar_json(VENDOR_A, SELF_PATH));
        let err = lookup_unix(Some("cmake"), &unix(None), Some("/nowhere"), &fs).unwrap_err();
        assert_eq!(err.exit_code(), 74);
        let err = lookup_unix(None, &unix(None), Some("/nowhere"), &fs).unwrap_err();
        assert_eq!(err.exit_code(), 74);
    }

    /// No working directory: a home record still runs, and a direct record, which joins paths onto it, is a 74.
    #[test]
    #[cfg(unix)]
    fn without_a_working_directory_only_a_direct_launcher_fails() {
        let image = "/store/render/bin/cmake";
        let no_cwd = LookupContext { cwd: "", ..unix(None) };
        let home = FakeFs {
            image: Some(image.to_string()),
            ..FakeFs::default()
        }
        .with_file(
            &format!("{image}.launch"),
            br#"{"kind":"package","schema_version":1,"package_root":"/p"}"#,
        );
        assert_eq!(found(lookup_unix(Some(image), &no_cwd, None, &home)).image, image);
        let started = format!("/vendor/{SELF_PATH}");
        let direct_fs = FakeFs::default().with_file(&format!("{started}.launch"), &sidecar_json(VENDOR_A, SELF_PATH));
        let err = lookup_unix(Some(&started), &no_cwd, None, &direct_fs).unwrap_err();
        assert_eq!(err.exit_code(), 74, "{}", err.stderr_message());
    }

    #[test]
    #[cfg(unix)]
    fn a_bare_argv0_with_no_path_hit_is_an_own_path_failure() {
        let err = started_path_unix("cmake", "/work", Some("/nowhere"), &FakeFs::default()).unwrap_err();
        assert_eq!(err.exit_code(), 74);
    }

    // ── record decoding and dispatch ────────────────────────────────────────

    /// What `locate` says of `raw` as the record beside `/vendor/bin/cmake`.
    fn locate_record(raw: &[u8], context: &LookupContext<'_>) -> Result<Probed, ShimError> {
        let started = if context.windows {
            "C:\\vendor\\bin\\cmake.exe"
        } else {
            "/vendor/bin/cmake"
        };
        let record_path = if context.windows {
            "C:\\vendor\\bin\\cmake.launch"
        } else {
            "/vendor/bin/cmake.launch"
        };
        let fs = FakeFs {
            image: Some(started.to_string()),
            ..FakeFs::default()
        };
        locate(started, context, &fs.with_file(record_path, raw))
    }

    #[test]
    fn a_record_that_does_not_decode_exits_78_naming_its_path() {
        let unknown_field = br#"{"kind":"direct","schema_version":1,"vendor_id":"v","self":"a","program":"@VENDOR_DIR@/a","args":[],"unset":[],"env":[],"extra":1}"#;
        let mut over_a_mebibyte = sidecar_json(VENDOR_A, SELF_PATH);
        over_a_mebibyte.resize(MAX_RECORD_BYTES as usize + 1, b' ');
        let nul = sidecar_with(r#"["a\u0000b"]"#, "[]", "[]");
        for (label, raw) in [
            ("an unknown field", unknown_field.to_vec()),
            ("over one mebibyte", over_a_mebibyte),
            ("a NUL in an argument", nul.into_bytes()),
        ] {
            let message = refusal(locate_record(&raw, &unix(None)));
            assert!(message.contains("cmake.launch"), "{label}: {message}");
        }
    }

    #[test]
    fn the_home_kinds_dispatch_to_their_own_verbs() {
        use super::core::{HomeLaunch, ToolchainHome};
        let pinned = "ocx.sh/tool/cmake:3.28@sha256:0000000000000000000000000000000000000000000000000000000000000001";
        let home = |json: String| match locate_record(json.as_bytes(), &unix(None)).unwrap() {
            Probed::Home(found) => found.home,
            Probed::Direct(_) => panic!("a home record located as direct"),
        };

        let package = home(r#"{"kind":"package","schema_version":1,"package_root":"/o/packages/p"}"#.to_string());
        assert!(
            matches!(&package, HomeLaunch::PackageRoot(root) if root == "/o/packages/p"),
            "{package:?}"
        );

        let deferred = home(format!(
            r#"{{"kind":"deferred","schema_version":1,"identifier":"{pinned}"}}"#
        ));
        assert!(
            matches!(&deferred, HomeLaunch::PinnedIdentifier(id) if id == pinned),
            "{deferred:?}"
        );

        let project = home(
            r#"{"kind":"toolchain","schema_version":1,"tier":"project","project_root":"/w/proj","ocx":"/o/ocx"}"#
                .to_string(),
        );
        assert!(
            matches!(&project, HomeLaunch::ToolchainHome(ToolchainHome::Project(root), Some(ocx)) if root == "/w/proj" && ocx == "/o/ocx"),
            "{project:?}"
        );

        let global = home(r#"{"kind":"toolchain","schema_version":1,"tier":"global"}"#.to_string());
        assert!(
            matches!(&global, HomeLaunch::ToolchainHome(ToolchainHome::Global, None)),
            "{global:?}"
        );
    }

    // ── home kinds read the record beside the loaded image ───────

    fn package_record(root: &str) -> Vec<u8> {
        format!(r#"{{"kind":"package","schema_version":1,"package_root":"{root}"}}"#).into_bytes()
    }

    fn found(probed: Result<Probed, ShimError>) -> HomeFound {
        match probed.expect("a home record") {
            Probed::Home(found) => found,
            Probed::Direct(located) => panic!("a home record located as direct: {located:?}"),
        }
    }

    fn package_root(found: &HomeFound) -> &str {
        match &found.home {
            core::HomeLaunch::PackageRoot(root) => root,
            other => panic!("not a package record: {other:?}"),
        }
    }

    #[test]
    fn vl15_a_home_launcher_started_through_a_link_reads_the_record_beside_its_image() {
        let image = "/store/render/bin/cmake";
        let started = "/links/elsewhere/alias";
        let fs = FakeFs {
            image: Some(image.to_string()),
            ..FakeFs::default()
        }
        .with_file(&format!("{image}.launch"), &package_record("/o/packages/p"));
        let found = found(locate(started, &unix(None), &fs));
        assert_eq!(found.image, image);
        assert_eq!(package_root(&found), "/o/packages/p");
        assert_eq!(split_file_name(&found.image, false), ("/store/render/bin/", "cmake"));
    }

    #[test]
    #[cfg(unix)]
    fn vl15_a_record_beside_the_started_link_alone_is_not_read_for_a_home_kind() {
        let fs = FakeFs {
            image: Some("/store/render/bin/cmake".to_string()),
            links: HashSet::from(["/links/elsewhere/alias".to_string()]),
            ..FakeFs::default()
        }
        .with_file("/links/elsewhere/alias.launch", &package_record("/o/packages/p"));
        let message = refusal(locate("/links/elsewhere/alias", &unix(None), &fs));
        assert!(message.contains("alias.launch"), "{message}");
    }

    #[test]
    fn vl15_a_windows_home_launcher_reads_the_record_beside_its_image() {
        let image = "C:\\store\\render\\bin\\cmake.exe";
        let fs = FakeFs {
            image: Some(image.to_string()),
            ..FakeFs::default()
        }
        .with_file(
            "C:\\store\\render\\bin\\cmake.launch",
            &package_record("C:/o/packages/p"),
        );
        let found = found(locate("C:\\links\\alias.exe", &windows_context(), &fs));
        assert_eq!(found.image, image);
    }

    /// Two render directories hold byte-identical launchers; `active` now names the other one.
    #[test]
    fn vl16_a_switched_active_link_does_not_change_the_record_the_loaded_image_reads() {
        let fs = FakeFs {
            image: Some("/home/shells/old/bin/cmake".to_string()),
            ..FakeFs::default()
        }
        .with_file("/home/shells/old/bin/cmake.launch", &package_record("/o/packages/old"))
        .with_file("/home/shells/new/bin/cmake.launch", &package_record("/o/packages/new"));
        let found = found(locate("/home/active/bin/cmake", &unix(None), &fs));
        assert_eq!(package_root(&found), "/o/packages/old");
    }

    /// A home kind is never paired with another directory by comparing a `stat` with a later `open`.
    #[test]
    #[cfg(unix)]
    fn vl17_a_home_kind_makes_no_identity_check_between_the_started_path_and_the_image() {
        let fs = FakeFs {
            image: Some("/home/shells/old/bin/cmake".to_string()),
            ..FakeFs::default()
        }
        .with_file("/home/shells/old/bin/cmake.launch", &package_record("/o/packages/old"));
        // `/home/active/bin/cmake` is not the image's file: an identity compare would refuse it.
        assert_eq!(
            package_root(&found(locate("/home/active/bin/cmake", &unix(None), &fs))),
            "/o/packages/old"
        );
        assert_eq!(fs.identity_checks.get(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn vl17_a_direct_kind_keeps_the_identity_check() {
        let started = format!("/vendor/{SELF_PATH}");
        let fs = FakeFs::default().with_file(&format!("{started}.launch"), &sidecar_json(VENDOR_A, SELF_PATH));
        direct(locate(&started, &unix(None), &fs).unwrap());
        assert_eq!(fs.identity_checks.get(), 1);
    }

    #[test]
    fn vl18_an_image_path_the_kernel_marks_deleted_is_refused() {
        let err = checked_image_link("/store/render/bin/cmake (deleted)").unwrap_err();
        assert_eq!(err.exit_code(), 78, "{}", err.stderr_message());
        assert!(err.stderr_message().contains("deleted"), "{}", err.stderr_message());
        assert_eq!(
            checked_image_link("/store/render/bin/cmake").unwrap(),
            "/store/render/bin/cmake"
        );
    }

    /// A runfiles tree reaches a vendored launcher through a link: kind `direct` keeps the started path.
    #[test]
    #[cfg(unix)]
    fn a_direct_launcher_reached_through_a_link_reads_the_record_at_the_started_path() {
        let started = format!("/runfiles/_main/{SELF_PATH}");
        let image = format!("/external/vendor/{SELF_PATH}");
        let bytes = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs {
            image: Some(image.clone()),
            links: HashSet::from([started.clone()]),
            ..FakeFs::default()
        }
        .with_file(&format!("{started}.launch"), &bytes)
        .with_file(&format!("{image}.launch"), &bytes);
        let located = direct(locate(&started, &unix(None), &fs).unwrap());
        assert_eq!(located.root, "/runfiles/_main");
    }

    /// A fake of the five short-buffer forms of [`fill_wide`]: `name` is what the API would write into a large enough
    /// buffer. `truncate-minus-one` copies `capacity - 1` units and returns that count, as a call that keeps room for a NUL.
    fn fake_call(name: &[u16], form: &str) -> impl FnMut(&mut [u16]) -> (usize, u32) {
        let name = name.to_vec();
        let form = form.to_string();
        move |buffer| {
            let room = if form == "truncate-minus-one" {
                buffer.len() - 1
            } else {
                buffer.len()
            };
            if name.len() < room {
                buffer[..name.len()].copy_from_slice(&name);
                return (name.len(), 0);
            }
            match form.as_str() {
                "truncate-minus-one" => {
                    let n = buffer.len() - 1;
                    buffer[..n].copy_from_slice(&name[..n]);
                    (n, 0)
                }
                "more-data" => (0, 234),
                "insufficient" => (0, 122),
                "truncate" => {
                    let n = buffer.len();
                    buffer.copy_from_slice(&name[..n]);
                    (n, 0)
                }
                _ => (name.len() + 1, 0),
            }
        }
    }

    #[test]
    fn a_wide_name_of_any_length_up_to_the_nt_limit_fills_in_every_short_buffer_form() {
        for form in [
            "more-data",
            "insufficient",
            "truncate",
            "truncate-minus-one",
            "required-size",
        ] {
            for units in [1usize, 511, 512, 513, 1024, 5000, 32767] {
                let name: Vec<u16> = (0..units).map(|i| 0x41 + (i % 26) as u16).collect();
                let filled = fill_wide(64 * 1024, fake_call(&name, form));
                assert_eq!(filled.as_deref(), Ok(name.as_slice()), "{form} {units}");
            }
        }
    }

    #[test]
    fn a_wide_fill_fails_on_another_error_and_stops_at_its_limit() {
        assert_eq!(fill_wide(64 * 1024, |_| (0, 5)), Err(WideFillFailure::Failed(5)));
        assert_eq!(fill_wide(64 * 1024, |_| (0, 0)), Err(WideFillFailure::Failed(0)));
        let name = vec![0x41u16; 2000];
        assert_eq!(
            fill_wide(1024, fake_call(&name, "more-data")),
            Err(WideFillFailure::TooLong)
        );
    }

    #[test]
    fn a_section_query_grows_on_a_short_buffer_status_and_stops_at_its_limit() {
        let max = 130_000;
        assert_eq!(classify_section_query(0, 0, 2048, max), SectionQuery::Ready);
        assert_eq!(
            classify_section_query(0x8000_0005_u32 as i32, 6000, 2048, max),
            SectionQuery::Grow(6000)
        );
        assert_eq!(
            classify_section_query(0xC000_0004_u32 as i32, 0, 2048, max),
            SectionQuery::Grow(4096)
        );
        assert_eq!(
            classify_section_query(0xC000_0023_u32 as i32, 100, 2048, max),
            SectionQuery::Grow(4096)
        );
        assert_eq!(
            classify_section_query(0xC000_0004_u32 as i32, max + 1, 2048, max),
            SectionQuery::TooLong
        );
        assert_eq!(
            classify_section_query(0xC000_0004_u32 as i32, 0, usize::MAX, max),
            SectionQuery::TooLong
        );
        let denied = 0xC000_0022_u32 as i32;
        assert_eq!(
            classify_section_query(denied, 9999, 2048, max),
            SectionQuery::Failed(denied)
        );
    }

    /// A query buffer of `total` bytes with `text` written at `offset`.
    fn section_buffer(total: usize, offset: usize, text: &[u16]) -> Vec<u8> {
        let mut bytes = vec![0u8; total];
        for (index, unit) in text.iter().enumerate() {
            bytes[offset + index * 2..offset + index * 2 + 2].copy_from_slice(&unit.to_ne_bytes());
        }
        bytes
    }

    #[test]
    fn a_section_name_is_read_past_max_path_and_hostile_structures_are_refused() {
        let text: Vec<u16> = r"\Device\HarddiskVolume3\".repeat(30).encode_utf16().collect();
        assert!(text.len() > 260);
        let bytes = section_buffer(16 + text.len() * 2 + 8, 16, &text);
        let base = bytes.as_ptr() as usize;
        let length = (text.len() * 2) as u16;
        assert_eq!(section_name_units(&bytes, length, base + 16, 16), Some(text.clone()));

        assert_eq!(section_name_units(&bytes, 0, base + 16, 16), None, "zero length");
        assert_eq!(
            section_name_units(&bytes, length - 1, base + 16, 16),
            None,
            "odd length"
        );
        assert_eq!(
            section_name_units(&bytes, length + 16, base + 16, 16),
            None,
            "past the end"
        );
        assert_eq!(section_name_units(&bytes, length, base + 17, 16), None, "misaligned");
        assert_eq!(
            section_name_units(&bytes, length, base + 8, 16),
            None,
            "inside the header"
        );
        assert_eq!(
            section_name_units(&bytes, length, base.wrapping_sub(2), 16),
            None,
            "before the buffer"
        );
        assert_eq!(
            section_name_units(&bytes, length, usize::MAX - 1, 16),
            None,
            "overflowing pointer"
        );
        assert_eq!(
            section_name_units(&bytes, u16::MAX - 1, base + 16, 16),
            None,
            "length beyond the buffer"
        );
    }

    /// A NUL in a value ends its `KEY=VALUE` entry in a Windows environment block, so the rest would be a new entry.
    #[test]
    fn a_nul_in_an_env_value_exits_78() {
        let value = r#"[{"key":"K","value":"x\u0000OCX_SIGNING_KEY=s","kind":"constant"}]"#;
        let raw = sidecar_with("[]", r#"["OCX_SIGNING_KEY"]"#, value);
        refusal(locate_record(raw.as_bytes(), &windows_context()));
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
        fs.unreadable.insert("/vendor/bin/cmake.launch".to_string());
        let message = refusal(locate("/vendor/bin/cmake", &unix(None), &fs));
        assert!(message.contains("not a regular file"), "{message}");
    }

    // ── Launch plan ───────────────────────────────────────────

    fn located(json: &str, root: &str) -> Located {
        Located {
            sidecar: match LaunchRecord::decode(json.as_bytes(), false).unwrap() {
                LaunchRecord::Direct(record) => record,
                other => panic!("a fixture of kind {:?}", other.kind()),
            },
            root: root.to_string(),
        }
    }

    fn sidecar_with(args: &str, unset: &str, env: &str) -> String {
        format!(
            r#"{{"kind":"direct","schema_version":1,"vendor_id":"{VENDOR_A}","self":"{SELF_PATH}","program":"@VENDOR_DIR@/p/bin/cmake","args":{args},"unset":{unset},"env":{env}}}"#
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

    // ── the kernel's image path, through a link ──────────────────

    /// What this test binary reports when the next test starts a copy of it through a directory link.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn probe_child_reports_its_image_and_started_paths() {
        let image = RealFs.image_path().expect("an image path");
        println!("IMAGE={image}");
        println!("STARTED={}", std::env::args_os().next().unwrap_or_default().display());
        println!("MODULE={}", std::env::current_exe().unwrap_or_default().display());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn make_directory_link(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        // A junction needs no privilege, unlike a symlink; it is what the `active` link is on Windows.
        #[cfg(windows)]
        {
            #[expect(clippy::disallowed_types, reason = "a test starting its own copy")]
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(output.status.success(), "mklink /J: {output:?}");
        }
    }

    /// The launcher is started through `active` and loads from the render directory behind it: the image path names
    /// the render directory, where the started path (and, on Windows, `GetModuleFileNameW`) names the link.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn vl19_an_image_started_through_a_link_names_the_directory_behind_it() {
        let base = canonicalize(&scratch_dir("image-link")).unwrap();
        let render = base.join("render");
        std::fs::create_dir(&render).unwrap();
        let name = if cfg!(windows) { "probe.exe" } else { "probe" };
        let binary = render.join(name);
        std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
        let link = base.join("active");
        make_directory_link(&render, &link);

        // A copy another test's concurrent fork still holds open for writing refuses to exec (ETXTBSY) until
        // that child execs; the hold is brief, so a bounded retry is enough.
        let output = (0..50)
            .find_map(|attempt| {
                #[expect(clippy::disallowed_types, reason = "a test starting its own copy")]
                let started = std::process::Command::new(link.join(name))
                    .args(["--nocapture", "probe_child_reports"])
                    .output();
                match started {
                    Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 49 => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        None
                    }
                    other => Some(other),
                }
            })
            .unwrap()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reported = |key: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or_else(|| panic!("no {key} line in {stdout}"))
                .to_string()
        };
        let (image, started) = (reported("IMAGE="), reported("STARTED="));

        let expected = binary.to_str().unwrap();
        assert!(
            if cfg!(windows) {
                image.eq_ignore_ascii_case(expected)
            } else {
                image == expected
            },
            "image path {image} is not {expected}"
        );
        assert!(
            started.contains("active"),
            "the control did not go through the link: {started}"
        );
        assert!(!image.contains("active"), "{image}");
        #[cfg(windows)]
        assert!(reported("MODULE=").contains("active"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_batch_start_failure_line_names_the_program_the_exit_and_the_length_limit() {
        let program = format!("C:\\{}\\run.cmd", "d".repeat(300));
        let line = batch_failure_line(&program, 1).expect("a batch path past MAX_PATH earns the line");
        assert!(
            line.starts_with(&format!(
                "ocx-shim: failed to start {program} through cmd.exe: exit 1; "
            )),
            "{line}"
        );
        assert!(
            line.ends_with(
                "and a batch file runs through cmd.exe, which has the same limit; \
                 move the directory the tree sits in to a shorter path"
            ),
            "{line}"
        );
        assert!(
            line.contains("the program path is 311 characters long, past the 260 Windows allows"),
            "{line}"
        );
    }

    #[test]
    fn a_batch_start_failure_line_is_absent_for_exit_zero_and_for_a_short_path() {
        let long = format!("C:\\{}\\run.cmd", "d".repeat(300));
        assert_eq!(batch_failure_line(&long, 0), None);
        assert_eq!(batch_failure_line("C:\\short\\run.cmd", 1), None);
    }

    #[test]
    fn a_regular_file_is_read_and_a_missing_one_is_none() {
        let dir = scratch_dir("regular");
        let file = dir.join("a.launch");
        std::fs::write(&file, b"{}").unwrap();
        assert_eq!(read_regular_bounded(&file).unwrap().as_deref(), Some(&b"{}"[..]));
        assert!(read_regular_bounded(&dir.join("missing.launch")).unwrap().is_none());
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
        let file = dir.join("big.launch");
        std::fs::write(&file, vec![b' '; MAX_RECORD_BYTES as usize + 1]).unwrap();
        assert!(matches!(read_regular_bounded(&file), Err(ReadError::TooLarge)));
        std::fs::write(&file, vec![b' '; MAX_RECORD_BYTES as usize]).unwrap();
        assert_eq!(
            read_regular_bounded(&file).unwrap().map(|b| b.len()),
            Some(MAX_RECORD_BYTES as usize)
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
        let fifo = dir.join("cmake.launch");
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

    /// Program files named by path; every other path is absent.
    struct Programs(&'static [&'static str]);

    impl core::ProgramFs for Programs {
        fn is_program_file(&self, path: &str) -> bool {
            self.0.contains(&path)
        }

        fn is_regular_file(&self, path: &str) -> bool {
            self.0.contains(&path)
        }
    }

    fn package_home() -> core::HomeLaunch {
        core::HomeLaunch::PackageRoot("/o/packages/p".to_string())
    }

    fn variables(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(key, value)| (key.into(), value.into())).collect()
    }

    #[test]
    fn a_home_plan_takes_the_pin_and_path_from_the_environment_it_was_given() {
        let programs = Programs(&["/real/ocx"]);
        let start = plan_home(
            &package_home(),
            "/launcher/",
            variables(&[("OCX_BINARY_PIN", ""), ("PATH", "/launcher:/real"), ("KEEP", "1")]),
            false,
            &programs,
        )
        .expect("an empty pin falls to the PATH walk");
        assert_eq!(start.program, "/real/ocx");
        assert!(
            start.environment.contains(&("__OCX_LAUNCHER_DEPTH".into(), "1".into())),
            "{:?}",
            start.environment
        );
        assert!(start.environment.contains(&("KEEP".into(), "1".into())));

        let pinned = plan_home(
            &package_home(),
            "/launcher/",
            variables(&[("OCX_BINARY_PIN", "/pin/ocx"), ("PATH", "/real")]),
            false,
            &programs,
        )
        .expect("a pin wins");
        assert_eq!(pinned.program, "/pin/ocx");
    }

    #[test]
    fn a_windows_plan_finds_its_variables_across_case() {
        let programs = Programs(&["D:\\real\\ocx.exe"]);
        let start = plan_home(
            &package_home(),
            "C:\\launcher\\",
            variables(&[("Path", "C:\\launcher;D:\\real"), ("ocx_binary_pin", "")]),
            true,
            &programs,
        )
        .expect("`Path` is `PATH` on Windows");
        assert_eq!(start.program, "D:\\real\\ocx.exe");
    }

    #[test]
    fn the_loop_guard_refuses_before_the_program_is_resolved() {
        let error = plan_home(
            &package_home(),
            "/launcher/",
            variables(&[("__OCX_LAUNCHER_DEPTH", "32"), ("OCX_BINARY_PIN", "relative")]),
            false,
            &Programs(&[]),
        )
        .expect_err("a loop");
        assert!(
            error.stderr_message().contains("launcher loop"),
            "{}",
            error.stderr_message()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pin_that_is_not_unicode_is_refused() {
        use std::os::unix::ffi::OsStringExt;

        let error = plan_home(
            &package_home(),
            "/launcher/",
            vec![(OsString::from("OCX_BINARY_PIN"), OsString::from_vec(vec![b'/', 0xFF]))],
            false,
            &Programs(&[]),
        )
        .expect_err("a pin that cannot be a path string");
        assert_eq!(error.exit_code(), 78);
    }

    // ── Runfiles manifest, the stage after the trees ───────────────────────

    const TOOL: &str = "/out/bin/tool";
    const WINDOWS_TOOL: &str = "C:\\out\\bin\\tool.exe";

    fn manifest_line(root: &str) -> Vec<u8> {
        format!("pkg/{SELF_PATH}.launch {root}/{SELF_PATH}.launch\n").into_bytes()
    }

    /// A started launcher with its record, no runfiles tree, `manifest` beside it and a record under `/vendor/set`.
    fn manifest_world(manifest: &[u8]) -> FakeFs {
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        FakeFs::default()
            .with_file("/out/bin/tool.launch", &record)
            .with_file("/out/bin/tool.runfiles/MANIFEST", manifest)
            .with_file(&format!("/vendor/set/{SELF_PATH}.launch"), &record)
    }

    fn manifest_root(fs: &FakeFs) -> String {
        direct(locate(TOOL, &unix(None), fs).unwrap()).root
    }

    fn manifest_refusal(fs: &FakeFs) -> String {
        refusal(locate(TOOL, &unix(None), fs))
    }

    /// The Windows counterpart of [`manifest_world`]: the record sits beside `tool.exe`, the root's record is at `root`.
    fn windows_manifest_world(manifest: &[u8], root: &str) -> FakeFs {
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        FakeFs::default()
            .with_file("C:\\out\\bin\\tool.launch", &record)
            .with_file("C:\\out\\bin\\tool.exe.runfiles/MANIFEST", manifest)
            .with_file(&format!("{root}/{SELF_PATH}.launch"), &record)
    }

    fn windows_root(fs: &FakeFs) -> String {
        direct(locate(WINDOWS_TOOL, &windows_context(), fs).unwrap()).root
    }

    #[test]
    fn vl43_a_plain_manifest_line_names_the_root() {
        let fs = manifest_world(&manifest_line("/vendor/set"));
        assert_eq!(manifest_root(&fs), "/vendor/set");
    }

    #[test]
    fn vl43_the_second_source_is_read_when_the_first_is_absent() {
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &record)
            .with_file("/out/bin/tool.runfiles_manifest", &manifest_line("/vendor/set"))
            .with_file(&format!("/vendor/set/{SELF_PATH}.launch"), &record);
        assert_eq!(manifest_root(&fs), "/vendor/set");
    }

    #[test]
    fn vl43_an_escaped_line_unescapes_its_key_and_target_and_splits_at_the_first_space() {
        // Key `dir one/...` (`\s`), target `/vendor/a b\c` (a raw space, `\b` for the backslash).
        let line = format!(" dir\\sone/{SELF_PATH}.launch /vendor/a b\\bc/{SELF_PATH}.launch\n");
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &record)
            .with_file("/out/bin/tool.runfiles/MANIFEST", line.as_bytes())
            .with_file(&format!("/vendor/a b\\c/{SELF_PATH}.launch"), &record);
        assert_eq!(manifest_root(&fs), "/vendor/a b\\c");
    }

    #[test]
    fn vl43_an_escaped_key_reads_backslash_s_as_a_space() {
        // The lookup only suffix-checks a key, so the key's `\s` shows at the splitter alone.
        assert_eq!(
            split_manifest_line(b" a\\sb/x t").map(|(key, target)| (key.into_owned(), target.into_owned())),
            Some((b"a b/x".to_vec(), b"t".to_vec()))
        );
    }

    #[test]
    fn vl43_an_escaped_line_keeps_an_unknown_escape_as_two_characters() {
        let line = format!(" pkg/{SELF_PATH}.launch /vendor/x\\qy/{SELF_PATH}.launch\n");
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &record)
            .with_file("/out/bin/tool.runfiles/MANIFEST", line.as_bytes())
            .with_file(&format!("/vendor/x\\qy/{SELF_PATH}.launch"), &record);
        assert_eq!(manifest_root(&fs), "/vendor/x\\qy");
    }

    #[test]
    fn vl43_an_unescaped_line_keeps_its_backslashes() {
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &record)
            .with_file("/out/bin/tool.runfiles/MANIFEST", &manifest_line("/vendor/we\\bird"))
            .with_file(&format!("/vendor/we\\bird/{SELF_PATH}.launch"), &record);
        assert_eq!(manifest_root(&fs), "/vendor/we\\bird");
    }

    #[test]
    fn vl43_a_line_with_an_empty_target_or_no_space_is_skipped() {
        let mut manifest = format!("pkg/{SELF_PATH}.launch \n").into_bytes();
        manifest.extend_from_slice(format!("pkg/{SELF_PATH}.launch\n").as_bytes());
        manifest.extend_from_slice(&manifest_line("/vendor/set"));
        assert_eq!(manifest_root(&manifest_world(&manifest)), "/vendor/set");
        let empty = format!("pkg/{SELF_PATH}.launch \n").into_bytes();
        assert!(manifest_refusal(&manifest_world(&empty)).contains("0 candidate"));
    }

    #[test]
    fn vl43_a_line_for_another_file_is_no_candidate() {
        let manifest =
            format!("pkg/other.txt /vendor/set/{SELF_PATH}.launch\npkg/{SELF_PATH}.launch /vendor/set/other\n");
        assert!(manifest_refusal(&manifest_world(manifest.as_bytes())).contains("0 candidate"));
    }

    #[test]
    fn vl43_windows_targets_with_either_separator_and_any_case_name_the_root() {
        let backslashes = SELF_PATH.replace('/', "\\");
        for (target, root) in [
            (format!("C:\\vendor\\set\\{backslashes}.launch"), "C:/vendor/set"),
            (format!("C:/vendor/set/{SELF_PATH}.launch"), "C:/vendor/set"),
            (
                format!("c:\\VENDOR\\Set\\{}.LAUNCH", backslashes.to_uppercase()),
                "c:/VENDOR/Set",
            ),
        ] {
            let manifest = format!("PKG/{}.Launch {target}\n", SELF_PATH.to_uppercase());
            let fs = windows_manifest_world(manifest.as_bytes(), root);
            assert_eq!(windows_root(&fs), root, "{target}");
        }
    }

    #[test]
    fn vl43_a_windows_line_that_is_not_utf8_is_skipped_and_a_utf8_root_is_decoded_once() {
        let mut manifest = format!("pkg/{SELF_PATH}.launch C:/bad").into_bytes();
        manifest.push(0xFF);
        manifest.extend_from_slice(format!("/{SELF_PATH}.launch\n").as_bytes());
        manifest.extend_from_slice(&manifest_line("C:/vendor/\u{fc}n\u{ef}"));
        let fs = windows_manifest_world(&manifest, "C:/vendor/\u{fc}n\u{ef}");
        assert_eq!(windows_root(&fs), "C:/vendor/\u{fc}n\u{ef}");
    }

    #[test]
    fn vl43_a_line_over_64_kib_is_refused_and_one_of_64_kib_is_not() {
        let filler = |length: usize| {
            let mut line = b"k ".to_vec();
            line.resize(length, b'x');
            line.push(b'\n');
            line
        };
        let mut allowed = filler(MAX_MANIFEST_LINE);
        allowed.extend_from_slice(&manifest_line("/vendor/set"));
        assert_eq!(manifest_root(&manifest_world(&allowed)), "/vendor/set");

        let mut over = filler(MAX_MANIFEST_LINE + 1);
        over.extend_from_slice(&manifest_line("/vendor/set"));
        let message = manifest_refusal(&manifest_world(&over));
        assert!(message.contains("longer than 64 KiB"), "{message}");
    }

    #[test]
    fn vl43_a_line_holding_nul_is_refused_even_when_it_names_no_candidate() {
        let mut manifest = b"a b\0c\n".to_vec();
        manifest.extend_from_slice(&manifest_line("/vendor/set"));
        let message = manifest_refusal(&manifest_world(&manifest));
        assert!(message.contains("NUL"), "{message}");
    }

    /// `valid` first, then filler lines that name nothing, up to `total` bytes.
    fn manifest_of_size(valid: &[u8], total: usize) -> Vec<u8> {
        let mut manifest = valid.to_vec();
        let start = manifest.len();
        manifest.resize(total, b'x');
        for at in (start + 1023..total).step_by(1024) {
            manifest[at] = b'\n';
        }
        manifest[total - 1] = b'\n';
        manifest
    }

    #[test]
    fn vl43_a_file_over_64_mib_is_refused_and_one_of_64_mib_is_not() {
        let valid = manifest_line("/vendor/set");
        let at_bound = manifest_of_size(&valid, MAX_MANIFEST_BYTES as usize);
        assert_eq!(manifest_root(&manifest_world(&at_bound)), "/vendor/set");

        let mut over = at_bound;
        over.push(b'x');
        let message = manifest_refusal(&manifest_world(&over));
        assert!(message.contains("larger than 64 MiB"), "{message}");
    }

    #[test]
    fn vl43_candidate_records_are_read_up_to_16_mib_together() {
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        let heavy = vec![b' '; MAX_RECORD_BYTES as usize];
        let world = |matching: usize| {
            let mut manifest = Vec::new();
            let mut fs = FakeFs::default()
                .with_file("/out/bin/tool.launch", &record)
                .with_file(&format!("/vendor/c{matching:02}/{SELF_PATH}.launch"), &record);
            for index in 0..=matching {
                manifest.extend_from_slice(&manifest_line(&format!("/vendor/c{index:02}")));
                if index != matching {
                    fs = fs.with_file(&format!("/vendor/c{index:02}/{SELF_PATH}.launch"), &heavy);
                }
            }
            fs.with_file("/out/bin/tool.runfiles/MANIFEST", &manifest)
        };
        assert_eq!(manifest_root(&world(15)), "/vendor/c15");
        let message = manifest_refusal(&world(16));
        assert!(message.contains("17 candidate"), "{message}");
    }

    #[test]
    fn vl43_duplicate_roots_are_deduplicated_before_the_cap_of_4096() {
        let mut manifest = Vec::new();
        for _ in 0..MAX_MANIFEST_CANDIDATES + 1 {
            manifest.extend_from_slice(&manifest_line("/vendor/none"));
        }
        manifest.extend_from_slice(&manifest_line("/vendor/set"));
        assert_eq!(manifest_root(&manifest_world(&manifest)), "/vendor/set");
    }

    #[test]
    fn vl43_at_most_4096_distinct_candidates_are_examined() {
        let world = |matching: usize| {
            let mut manifest = Vec::new();
            for index in 0..=matching {
                manifest.extend_from_slice(&manifest_line(&format!("/vendor/n{index}")));
            }
            let record = sidecar_json(VENDOR_A, SELF_PATH);
            FakeFs::default()
                .with_file("/out/bin/tool.launch", &record)
                .with_file("/out/bin/tool.runfiles/MANIFEST", &manifest)
                .with_file(&format!("/vendor/n{matching}/{SELF_PATH}.launch"), &record)
        };
        assert_eq!(
            manifest_root(&world(MAX_MANIFEST_CANDIDATES - 1)),
            format!("/vendor/n{}", MAX_MANIFEST_CANDIDATES - 1)
        );
        let message = manifest_refusal(&world(MAX_MANIFEST_CANDIDATES));
        assert!(message.contains("4096 candidate"), "{message}");
    }

    #[test]
    fn vl43_the_refusal_names_the_first_eight_candidates_and_names_the_missing_tree_and_manifest() {
        let mut manifest = Vec::new();
        for index in 0..10 {
            manifest.extend_from_slice(&manifest_line(&format!("/vendor/r{index}")));
        }
        let message = manifest_refusal(&manifest_world_without_root(&manifest));
        assert!(message.contains("10 candidate"), "{message}");
        assert!(
            message.contains("/vendor/r7") && !message.contains("/vendor/r8"),
            "{message}"
        );
        assert!(!message.contains("--enable_runfiles"), "{message}");

        let windows = refusal(locate(
            WINDOWS_TOOL,
            &windows_context(),
            &windows_manifest_world(b"", "C:/none"),
        ));
        assert!(!windows.contains("--enable_runfiles"), "{windows}");
        assert!(
            windows.contains("neither a runfiles tree nor a runfiles manifest"),
            "{windows}"
        );
    }

    /// A manifest reachable by two names is read once, and a manifest after an accepted root is not read at all.
    #[test]
    fn a_manifest_path_already_scanned_is_not_read_again() {
        let fs = manifest_world_without_root(b"");
        let same = "/out/bin/tool.runfiles/MANIFEST";
        let message = refusal(locate(TOOL, &with_inherited_manifest(same), &fs));
        assert!(message.contains("no vendored root"), "{message}");
        let reads = fs.line_reads.borrow();
        assert_eq!(
            reads.iter().filter(|path| path.as_str() == same).count(),
            1,
            "the same path named twice is read once: {reads:?}"
        );
        assert_eq!(
            reads.len(),
            2,
            "the second beside-name is the only other read: {reads:?}"
        );
    }

    #[test]
    fn a_manifest_after_the_accepted_root_is_not_read() {
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &record)
            .with_file("/out/bin/tool.runfiles/MANIFEST", &manifest_line("/vendor/set"))
            .with_file("/out/bin/tool.runfiles_manifest", &manifest_line("/vendor/set"))
            .with_file(&format!("/vendor/set/{SELF_PATH}.launch"), &record);
        let context = with_inherited_manifest("/out/bin/tool.runfiles_manifest");
        assert_eq!(direct(locate(TOOL, &context, &fs).unwrap()).root, "/vendor/set");
        assert_eq!(
            *fs.line_reads.borrow(),
            ["/out/bin/tool.runfiles/MANIFEST".to_string()],
            "the first manifest accepted a root, so no other is opened"
        );
    }

    fn manifest_world_without_root(manifest: &[u8]) -> FakeFs {
        FakeFs::default()
            .with_file("/out/bin/tool.launch", &sidecar_json(VENDOR_A, SELF_PATH))
            .with_file("/out/bin/tool.runfiles/MANIFEST", manifest)
    }

    // ── RUNFILES_MANIFEST_FILE ─────────────────────────────────────────────

    fn with_inherited_manifest(manifest_file: &str) -> LookupContext<'_> {
        LookupContext {
            manifest_file: Some(manifest_file),
            ..unix(None)
        }
    }

    #[test]
    fn vl44_an_inherited_manifest_that_does_not_list_the_started_path_is_ignored() {
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        let mut other_tree = manifest_line("/vendor/set");
        other_tree.extend_from_slice(b"tool /somewhere/else/tool\n");
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &record)
            .with_file("/outer/MANIFEST", &other_tree)
            .with_file(&format!("/vendor/set/{SELF_PATH}.launch"), &record);
        let message = refusal(locate(TOOL, &with_inherited_manifest("/outer/MANIFEST"), &fs));
        assert!(message.contains("0 candidate"), "{message}");
    }

    #[test]
    fn vl44_an_inherited_manifest_that_lists_the_started_path_is_used() {
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        let mut outer = manifest_line("/vendor/set");
        outer.extend_from_slice(format!("tool {TOOL}\n").as_bytes());
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &record)
            .with_file("/outer/MANIFEST", &outer)
            .with_file(&format!("/vendor/set/{SELF_PATH}.launch"), &record);
        let located = direct(locate(TOOL, &with_inherited_manifest("/outer/MANIFEST"), &fs).unwrap());
        assert_eq!(located.root, "/vendor/set");
    }

    #[test]
    fn vl44_the_started_path_is_compared_folded_on_windows_and_exactly_on_unix() {
        let mut outer = manifest_line("C:/vendor/set");
        outer.extend_from_slice(b"tool c:/OUT/bin/TOOL.exe\n");
        let fs = windows_manifest_world(b"", "C:/vendor/set").with_file("C:\\outer\\MANIFEST", &outer);
        let context = LookupContext {
            manifest_file: Some("C:\\outer\\MANIFEST"),
            ..windows_context()
        };
        assert_eq!(
            direct(locate(WINDOWS_TOOL, &context, &fs).unwrap()).root,
            "C:/vendor/set"
        );

        let mut unix_outer = manifest_line("/vendor/set");
        unix_outer.extend_from_slice(b"tool /OUT/bin/tool\n");
        let fs = manifest_world(b"").with_file("/outer/MANIFEST", &unix_outer);
        assert!(refusal(locate(TOOL, &with_inherited_manifest("/outer/MANIFEST"), &fs)).contains("0 candidate"));
    }

    #[test]
    fn vl44_an_inherited_manifest_that_is_not_a_regular_file_is_refused() {
        // No earlier manifest yields a root, so the inherited one is reached.
        let mut fs = manifest_world_without_root(b"");
        fs.unreadable.insert("/outer/fifo".to_string());
        let message = refusal(locate(TOOL, &with_inherited_manifest("/outer/fifo"), &fs));
        assert!(
            message.contains("/outer/fifo") && message.contains("not a regular file"),
            "{message}"
        );
    }

    /// A FIFO at a manifest path must not block the launcher; the read runs on a thread so a hang fails the test.
    #[cfg(unix)]
    #[test]
    fn vl44_a_fifo_at_a_manifest_path_does_not_hang() {
        use std::os::unix::ffi::OsStrExt;

        let dir = scratch_dir("manifest-fifo");
        let fifo = dir.join("tool.runfiles_manifest");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `name` is a NUL-terminated path that outlives the call; `mkfifo` only reads it. No child process is
        // forked here, so no other test's freshly written executable is held open by an inherited descriptor.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

        let (sender, receiver) = std::sync::mpsc::channel();
        let path = fifo.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            let _ = sender.send(
                RealFs
                    .read_lines(&path, MAX_MANIFEST_BYTES, MAX_MANIFEST_LINE)
                    .map(|_| ()),
            );
        });
        let outcome = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the open must return within a second");
        assert!(matches!(outcome, Err(ReadError::NotRegular)), "{outcome:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn vl44_a_real_manifest_streams_its_lines_and_reports_a_missing_one_as_absent() {
        let dir = scratch_dir("manifest-lines");
        let file = dir.join("MANIFEST");
        std::fs::write(&file, "a b\n\nlast").unwrap();
        let path = file.to_string_lossy().into_owned();
        let lines: Vec<Vec<u8>> = RealFs
            .read_lines(&path, MAX_MANIFEST_BYTES, MAX_MANIFEST_LINE)
            .unwrap()
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(lines, [b"a b".to_vec(), Vec::new(), b"last".to_vec()]);
        assert!(
            RealFs
                .read_lines(&format!("{path}.absent"), MAX_MANIFEST_BYTES, MAX_MANIFEST_LINE)
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ── Root hygiene ───────────────────────────────────────────────────────

    /// A manifest root that would be accepted if it were not rejected on its shape.
    fn hygiene_refusal(root: &str) -> String {
        let record = sidecar_json(VENDOR_A, SELF_PATH);
        let fs = FakeFs::default()
            .with_file("/out/bin/tool.launch", &record)
            .with_file("/out/bin/tool.runfiles/MANIFEST", &manifest_line(root))
            .with_file(&format!("{root}/{SELF_PATH}.launch"), &record);
        let message = manifest_refusal(&fs);
        assert!(message.contains("1 candidate(s) rejected"), "{root:?}: {message}");
        message
    }

    #[test]
    fn vl45_a_root_that_is_relative_or_holds_a_dot_segment_is_rejected() {
        for root in [
            "vendor/set",
            "./vendor/set",
            "/vendor/./set",
            "/vendor/../vendor/set",
            "/vendor/set/..",
        ] {
            hygiene_refusal(root);
        }
    }

    #[test]
    fn vl45_a_root_with_cr_or_the_list_separator_is_rejected() {
        for root in ["/vendor/s\rt", "/vendor/s:t", "/vendor/s\u{b}t:"] {
            hygiene_refusal(root);
        }
    }

    #[test]
    fn vl45_a_windows_root_with_a_semicolon_or_no_drive_is_rejected() {
        for root in ["C:/vendor/a;b", "vendor/set", "C:/vendor/../set"] {
            let manifest = manifest_line(root);
            let fs = windows_manifest_world(&manifest, root);
            let message = refusal(locate(WINDOWS_TOOL, &windows_context(), &fs));
            assert!(message.contains("1 candidate(s) rejected"), "{root:?}: {message}");
        }
    }

    /// A driveless root names a directory on the current drive, which follows the working directory.
    #[test]
    fn a_windows_manifest_root_on_the_current_drive_is_rejected() {
        for root in ["/vendor/set", "\\vendor\\set"] {
            let manifest = manifest_line(root);
            let fs = windows_manifest_world(&manifest, &root.replace('\\', "/"));
            let message = refusal(locate(WINDOWS_TOOL, &windows_context(), &fs));
            assert!(message.contains("1 candidate(s) rejected"), "{root:?}: {message}");
        }
    }

    #[test]
    fn vl45_a_root_that_is_not_utf8_is_rejected_on_unix() {
        let mut manifest = format!("pkg/{SELF_PATH}.launch /vendor/").into_bytes();
        manifest.push(0xFF);
        manifest.extend_from_slice(format!("/{SELF_PATH}.launch\n").as_bytes());
        let message = manifest_refusal(&manifest_world(&manifest));
        assert!(message.contains("1 candidate(s) rejected"), "{message}");
    }
}
