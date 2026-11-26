// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The vendored (`.direct`) launcher: decoding its sidecar and finding the vendor root it ships in.
//!
//! Every filesystem question goes through [`LookupFs`], so the lookup rules run in unit tests without a disk.

use super::ShimError;
use super::core;
use ocx_env_apply::direct::DirectSidecar;
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
    fn is_executable_file(&self, path: &str) -> bool;
    /// Whether `path` has the device and inode of the running image.
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
    /// The started path, absolute and unresolved.
    pub started: String,
    /// The vendor root: absolute, lexically joined, no symlink resolved.
    pub root: String,
}

fn refuse(reason: String) -> ShimError {
    ShimError::DirectRefused { reason }
}

/// `O_NONBLOCK` without a `libc` edge: a blocking `open` of a FIFO waits for a writer and would hang the launcher.
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(all(unix, not(target_os = "linux")))]
const O_NONBLOCK: i32 = 0x4;

/// Reads the file at `path`: opened non-blocking, required to be a regular file, at most [`MAX_SIDECAR_BYTES`].
pub(super) fn read_regular_bounded(path: &Path) -> Result<Option<Vec<u8>>, ReadError> {
    use std::io::Read;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(O_NONBLOCK);
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
/// The root is the first candidate, in C-012 order, whose own `<self>.direct` is byte-identical to the sidecar
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
                return Ok(Probed::Direct(Located {
                    sidecar,
                    started: started.to_string(),
                    root,
                }));
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
        executables: HashSet<String>,
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
        fn is_executable_file(&self, path: &str) -> bool {
            self.executables.contains(path)
        }
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

    // ── V-52: kinds per platform ────────────────────────────────────────────

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

    // ── C-012 candidates ────────────────────────────────────────────────────

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

    /// V-59: a `RUNFILES_DIR` that does not hold the started path is never consulted, even when its entry matches.
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

    // ── C-013 / V-68: root shape ────────────────────────────────────────────

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

    // ── started path (Unix) and V-61 ────────────────────────────────────────

    #[test]
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

    /// V-61: the `PATH` hit is a different file than the running image.
    #[test]
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

    #[test]
    fn a_sidecar_the_fs_refuses_exits_78() {
        let mut fs = FakeFs::default();
        fs.unreadable.insert("/vendor/bin/cmake.direct".to_string());
        let message = refusal(locate("/vendor/bin/cmake", &unix(None), &fs));
        assert!(message.contains("not a regular file"), "{message}");
    }

    // ── real reads: V-60 ────────────────────────────────────────────────────

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

    /// V-60: a FIFO at the sidecar path must not block the launcher; the read runs on a thread so a hang fails the test.
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
