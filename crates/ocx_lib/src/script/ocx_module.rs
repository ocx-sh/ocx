// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx.*` host module exposed to test scripts.
//!
//! `#[starlark_module]`-defined and registered under the `ocx` namespace.
//! Signatures (Starlark-facing):
//!
//! - `ocx.run(*args, *, env=None, cwd=None, stdin=None) -> RunResult` —
//!   positional varargs (first = program, rest = argv; splat a list with
//!   `ocx.run(*cmd)`); zero positional args → `Failed`. Own piped
//!   `tokio::process::Command` on the composed env (already through
//!   `Env::apply_ocx_config`); `env` overlay applied AFTER command resolution
//!   (resolution uses composed PATH only); reserved keys rejected; `cwd`
//!   defaults to scratch and is guard- + symlink-rechecked; `stdin` is a
//!   per-call child stdin string (independent of `--script -`);
//!   `kill_on_drop(true)` + SIGINT/SIGTERM forwarding (parity with
//!   `child_process::spawn_and_wait`); child awaited via
//!   `Handle::current().block_on(...)` under the per-child wall-clock kill
//!   deadline. Refuses to spawn a program resolving to an `ocx` binary in v1.
//! - `ocx.env(name) -> str | None` — read one var from the composed env.
//! - `ocx.platform() -> {"os":…, "arch":…}` — reflects the `-p` flag.
//! - `ocx.package_root() -> str` — read-only package root (`/`-normalized).
//! - `ocx.scratch_root() -> str` — read-write sandbox root (`/`-normalized).
//! - `ocx.read_file(path, *, max_bytes=1048576) -> str` — guarded read.
//! - `ocx.write_file(path, content)` — scratch-only guarded write.
//! - `ocx.exists(path) -> bool` — guarded existence check.
//! - `ocx.mkdir(path)` — scratch-only recursive idempotent `mkdir -p`.
//!
//! Every `path` arg AND `ocx.run(cwd=…)` goes through `guard::resolve_scratch`
//! (write/`cwd` side) or `guard::resolve_read` (read side) then the Codex C1
//! symlink re-check before the syscall.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use starlark::environment::GlobalsBuilder;
use starlark::starlark_module;
use starlark::values::none::{NoneOr, NoneType};
use starlark::values::tuple::UnpackTuple;
use starlark::values::{Value, dict::DictRef};

use super::guard;
use super::host;
use super::run_result::{OUTPUT_CAP_BYTES, RunResult};
use super::sl_error::{fail, script_type};

/// Default `ocx.read_file` size cap (1 MiB).
const DEFAULT_READ_MAX_BYTES: i32 = 1_048_576;

/// Env keys an `ocx.run(env=...)` overlay may never override (Codex C3). PATH
/// is reserved so an overlay can never change which binary resolves; the OCX
/// loader vars are reserved so the overlay cannot redirect a (refused, but
/// defence-in-depth) re-entrant ocx.
const RESERVED_ENV_KEYS: &[&str] = &[
    "PATH",
    "OCX_HOME",
    crate::env::keys::OCX_BINARY_PIN,
    crate::env::keys::OCX_CONFIG,
    crate::env::keys::OCX_PROJECT,
    crate::env::keys::OCX_INDEX,
    crate::env::keys::OCX_NO_CONFIG,
    crate::env::keys::OCX_NO_PROJECT,
    crate::env::keys::OCX_OFFLINE,
    crate::env::keys::OCX_REMOTE,
];

/// Prefix of the per-registry credential env vars OCX reads (see
/// `crate::auth`: `OCX_AUTH_<slug>_{TYPE,USER,TOKEN}`). A script must never be
/// able to read these out of the inherited host env (exfiltration when
/// `--clean` is not set), nor set them on a child via the `ocx.run` overlay.
const CREDENTIAL_ENV_PREFIX: &str = "OCX_AUTH_";

/// Single source of truth for "this env key is off-limits to scripts".
///
/// Covers (a) the resolution-affecting reserved keys an `ocx.run(env=...)`
/// overlay may not override (so the composed sandbox/loader policy is
/// authoritative) and (b) the `OCX_AUTH_*` credential family (so a script
/// cannot exfiltrate inherited host secrets via `ocx.env`). Both the overlay
/// rejection and the `ocx.env` read deny-list consult this one predicate —
/// the key set is not duplicated.
fn is_reserved_env_key(key: &str) -> bool {
    // Byte-boundary-safe prefix test: `key[..N]` panics when N falls inside a
    // multibyte UTF-8 scalar (a script may supply an arbitrary non-ASCII env
    // key). The credential-mask predicate must NEVER panic — slice the bytes,
    // not the `str`, and compare ASCII-case-insensitively.
    let credential = key
        .as_bytes()
        .get(..CREDENTIAL_ENV_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(CREDENTIAL_ENV_PREFIX.as_bytes()));
    RESERVED_ENV_KEYS.iter().any(|r| r.eq_ignore_ascii_case(key)) || credential
}

/// `/`-normalized string form of a path (portable across platforms).
fn slash_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Spawns `program` with `args` on the composed env (+ optional overlay),
/// capturing output with a cap, under the per-child wall-clock kill deadline.
///
/// Sync by contract (Fix#2): the surrounding `run_script` is sync and called
/// via `block_in_place`, so the child is awaited via
/// `Handle::current().block_on(...)`, NOT `.await` / `spawn_blocking`. The
/// child sets `kill_on_drop(true)` and (Unix) forwards SIGINT/SIGTERM —
/// parity with `child_process::spawn_and_wait` (Codex C4).
fn spawn_capture(
    program: &Path,
    args: &[String],
    base_env: &crate::env::Env,
    overlay: &[(String, String)],
    cwd: &Path,
    stdin: Option<&str>,
    wall_clock: Duration,
) -> Result<RunResult, String> {
    use std::process::Stdio;

    let start = Instant::now();
    let handle = tokio::runtime::Handle::current();

    let stdin_cfg = if stdin.is_some() { Stdio::piped() } else { Stdio::null() };

    // Feed the composed base env by borrowing iteration (no full `Env` clone
    // per `ocx.run`), then apply the small overlay delta as a second `.envs`
    // call — `Command` keeps a key→value map so the later call wins for any
    // overlapping key (reserved keys were already rejected upstream, so the
    // overlay only adds/overrides author-chosen vars).
    let spawn_res = tokio::process::Command::new(program)
        .args(args)
        .env_clear()
        .envs(base_env.iter())
        .envs(overlay.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(cwd)
        .stdin(stdin_cfg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();

    let mut child = spawn_res.map_err(|e| format!("failed to spawn '{}': {e}", program.display()))?;

    if let Some(input) = stdin {
        use tokio::io::AsyncWriteExt;
        if let Some(mut sink) = child.stdin.take() {
            let write_res: std::io::Result<()> = handle.block_on(async {
                sink.write_all(input.as_bytes()).await?;
                sink.shutdown().await
            });
            // A genuine stdin write failure must surface as an I/O error, not
            // be silently swallowed into a spurious child failure. `BrokenPipe`
            // is benign: it means the child closed stdin / exited before
            // reading all input, which is legal child behaviour, not a host
            // fault — let the normal wait path report the child's outcome.
            if let Err(e) = write_res
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                let _ = child.start_kill();
                return Err(format!("ocx.run failed writing child stdin: {e}"));
            }
        }
    }

    let output = handle.block_on(async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigint = signal(SignalKind::interrupt()).ok();
            let mut sigterm = signal(SignalKind::terminate()).ok();
            let deadline = tokio::time::sleep(wall_clock);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    out = child.wait_with_output_ref() => break out,
                    _ = &mut deadline => {
                        let _ = child.start_kill();
                        break Err(WaitError::TimedOut);
                    }
                    _ = async { sigint.as_mut().unwrap().recv().await }, if sigint.is_some() => {
                        let _ = child.start_kill();
                    }
                    _ = async { sigterm.as_mut().unwrap().recv().await }, if sigterm.is_some() => {
                        let _ = child.start_kill();
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            match tokio::time::timeout(wall_clock, child.wait_with_output_owned()).await {
                Ok(r) => r,
                Err(_) => Err(WaitError::TimedOut),
            }
        }
    });

    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    match output {
        Ok((status, stdout_raw, stderr_raw)) => {
            let (stdout, t1) = cap_stream(&stdout_raw);
            let (stderr, t2) = cap_stream(&stderr_raw);
            let exit_code = exit_code_of(&status);
            Ok(RunResult::new(exit_code, stdout, stderr, duration_ms, t1 || t2))
        }
        Err(WaitError::TimedOut) => {
            // Codex C4: record the typed timeout so `engine::classify` surfaces
            // `ScriptOutcomeKind::Timeout` instead of the generic `Failed`
            // bucket. The Timeout→exit-code mapping is unchanged (a separately
            // deferred decision) — only the status becomes observable.
            super::host::note_timeout();
            Err(format!(
                "ocx.run child exceeded the {} ms wall-clock deadline and was killed",
                wall_clock.as_millis()
            ))
        }
        Err(WaitError::Io(e)) => Err(format!("ocx.run failed waiting for child: {e}")),
    }
}

/// Caps a captured stream at [`OUTPUT_CAP_BYTES`], returning the (lossy UTF-8)
/// text and whether it was truncated.
fn cap_stream(raw: &[u8]) -> (String, bool) {
    if raw.len() > OUTPUT_CAP_BYTES {
        (String::from_utf8_lossy(&raw[..OUTPUT_CAP_BYTES]).into_owned(), true)
    } else {
        (String::from_utf8_lossy(raw).into_owned(), false)
    }
}

#[cfg(unix)]
fn exit_code_of(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| status.signal().map(|s| 128 + s).unwrap_or(1))
}

#[cfg(not(unix))]
fn exit_code_of(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// Internal wait error so the spawn helper distinguishes a timeout kill from a
/// genuine I/O failure.
enum WaitError {
    TimedOut,
    Io(std::io::Error),
}

/// Tokio's `Child` has no borrowing `wait_with_output`; this drains stdout +
/// stderr concurrently with the wait and returns the status + raw bytes.
trait ChildWaitExt {
    async fn wait_with_output_ref(&mut self) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), WaitError>;
    #[cfg(not(unix))]
    async fn wait_with_output_owned(&mut self) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), WaitError>;
}

impl ChildWaitExt for tokio::process::Child {
    async fn wait_with_output_ref(&mut self) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), WaitError> {
        use tokio::io::AsyncReadExt;
        // Bound EACH stream DURING streaming: a GiB-producing child must never
        // be fully buffered then capped after the fact (host OOM). Read at most
        // OUTPUT_CAP_BYTES + 1 — the extra byte lets `cap_stream` detect that
        // the producer exceeded the cap (→ truncated=true) without buffering
        // the rest. The kernel pipe back-pressures the child once we stop
        // reading. Both streams are bounded independently.
        const TAKE_LIMIT: u64 = OUTPUT_CAP_BYTES as u64 + 1;
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();
        let mut out = self.stdout.take();
        let mut err = self.stderr.take();
        // A stream-drain failure must surface as an I/O error, not be silently
        // dropped (W4 parity with the stdin path): swallowing it reports
        // partial output alongside an apparently-valid child status. The
        // streaming `take(TAKE_LIMIT)` cap (→ `truncated`) is preserved — only
        // the discarded `Result` is now propagated.
        let read_out = async {
            match out.as_mut() {
                Some(s) => s.take(TAKE_LIMIT).read_to_end(&mut out_buf).await.map(|_| ()),
                None => Ok(()),
            }
        };
        let read_err = async {
            match err.as_mut() {
                Some(s) => s.take(TAKE_LIMIT).read_to_end(&mut err_buf).await.map(|_| ()),
                None => Ok(()),
            }
        };
        let (status, out_res, err_res) = tokio::join!(self.wait(), read_out, read_err);
        let status = status.map_err(WaitError::Io)?;
        out_res.map_err(WaitError::Io)?;
        err_res.map_err(WaitError::Io)?;
        Ok((status, out_buf, err_buf))
    }

    #[cfg(not(unix))]
    async fn wait_with_output_owned(&mut self) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), WaitError> {
        self.wait_with_output_ref().await
    }
}

/// True if `program` carries a path separator (so it is a path, not a bare
/// PATH-looked-up name). `\` counts on every platform for portability parity
/// with the rest of the sandbox (which treats `\` as a separator everywhere).
fn program_is_path(program: &str) -> bool {
    program.contains('/') || program.contains('\\')
}

/// Resolves a program name to the binary the child will execute.
///
/// Codex C3: a PATH-only name resolves against the composed env's PATH only
/// (the overlay must not change resolution). A path-bearing `program`
/// (`./tool`, `bin/tool`) must resolve relative to the *validated* guarded
/// `cwd` — NOT the process CWD — because the child runs with `current_dir(cwd)`
/// applied; resolving it against the outer CWD would bind (and refuse-check)
/// the wrong binary.
fn resolve_program(base_env: &crate::env::Env, program: &str, cwd: &Path) -> std::path::PathBuf {
    if program_is_path(program) {
        let raw = Path::new(program);
        return if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            cwd.join(raw)
        };
    }
    base_env.resolve_command(program)
}

/// Returns true if `resolved` is (or resolves to) an `ocx` binary — refused in
/// v1 (Fix#6): a nested unsandboxed `ocx` would write the real `$OCX_HOME`.
fn is_ocx_binary(base_env: &crate::env::Env, resolved: &Path) -> bool {
    // Fast pre-filter: the file stem already reads as `ocx`.
    let stem = resolved
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("ocx"))
        .unwrap_or(false);
    if stem {
        return true;
    }

    // A PATH symlink `foo -> /usr/bin/ocx` defeats the stem check (stem is
    // `foo`) and a raw `==` against `OCX_BINARY_PIN` (the link path differs
    // from the pin path). Canonicalize BOTH sides so the symlink is resolved
    // to its real target before comparison.
    let canonical_resolved = std::fs::canonicalize(resolved).ok();
    let Some(canonical_resolved) = canonical_resolved else {
        return false;
    };

    // Refuse if the canonical target equals the pinned running binary or the
    // current executable, each canonicalized the same way.
    let mut pins: Vec<std::path::PathBuf> = Vec::new();
    if let Some(pin) = base_env.get(crate::env::keys::OCX_BINARY_PIN) {
        pins.push(Path::new(pin).to_path_buf());
    }
    if let Ok(exe) = std::env::current_exe() {
        pins.push(exe);
    }
    pins.iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .any(|p| p == canonical_resolved)
}

/// Members of the `ocx` namespace.
#[starlark_module]
fn ocx_members(globals: &mut GlobalsBuilder) {
    /// `ocx.run(*args, *, env=None, cwd=None, stdin=None) -> RunResult`
    fn run<'v>(
        #[starlark(args)] args: UnpackTuple<Value<'v>>,
        #[starlark(require = named)] env: Option<Value<'v>>,
        #[starlark(require = named)] cwd: Option<&str>,
        #[starlark(require = named)] stdin: Option<&str>,
        eval: &mut starlark::eval::Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        // Parse positional argv. Zero positional → Failed (R2).
        let argv: Vec<&str> = args
            .items
            .iter()
            .map(|v| v.unpack_str())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| script_type("ocx.run arguments must all be strings"))?;
        let (program, rest) = argv
            .split_first()
            .ok_or_else(|| fail("ocx.run requires at least a program"))?;

        // Overlay dict (after-resolution; reserved keys rejected — Codex C3).
        let overlay: Vec<(String, String)> = match env {
            None => Vec::new(),
            Some(v) => {
                let dict = DictRef::from_value(v).ok_or_else(|| script_type("ocx.run env= must be a dict"))?;
                let mut pairs = Vec::new();
                for (k, val) in dict.iter() {
                    let key = k
                        .unpack_str()
                        .ok_or_else(|| script_type("ocx.run env= keys must be strings"))?;
                    let value = val
                        .unpack_str()
                        .ok_or_else(|| script_type("ocx.run env= values must be strings"))?;
                    if is_reserved_env_key(key) {
                        return Err(fail(format!("ocx.run env= cannot override the reserved key '{key}'")));
                    }
                    pairs.push((key.to_string(), value.to_string()));
                }
                pairs
            }
        };

        let rest: Vec<String> = rest.iter().map(|s| s.to_string()).collect();

        let (resolved, cwd_path, wall_clock) = host::with(|s| {
            // cwd default = scratch root; guarded + symlink re-checked (C1).
            let cwd_path = match cwd {
                None => Ok(s.scratch_root.clone()),
                Some(c) => guard::resolve_scratch(c, &s.scratch_root)
                    .map_err(|e| e.to_string())
                    .and_then(|p| {
                        guard::verify_symlink_containment(&s.scratch_root, &p)
                            .map_err(|e| e.to_string())
                            .map(|()| p)
                    }),
            };
            // Codex C3: resolve a path-bearing `program` against the VALIDATED
            // cwd (the binary the child will actually exec), not the process
            // CWD. Falls back to the guarded scratch root when the cwd is
            // rejected so the (refused) re-entrant check still runs on a
            // deterministic path rather than the outer CWD.
            let cwd_for_resolve = cwd_path
                .as_ref()
                .map(PathBuf::as_path)
                .unwrap_or(s.scratch_root.as_path());
            let resolved = resolve_program(&s.env, program, cwd_for_resolve);
            (resolved, cwd_path, s.wall_clock)
        });

        let cwd_path = cwd_path.map_err(fail)?;

        // Refuse re-entrant ocx (Fix#6).
        let reentrant = host::with(|s| is_ocx_binary(&s.env, &resolved));
        if reentrant {
            return Err(fail(
                "re-entrant ocx is not supported in v1 (ocx.run target resolves to an ocx binary); awaits a follow-up ADR",
            ));
        }

        let result = host::with(|s| spawn_capture(&resolved, &rest, &s.env, &overlay, &cwd_path, stdin, wall_clock))
            .map_err(fail)?;

        host::with_mut(|s| s.last_run = Some(result.clone()));
        Ok(result.alloc(eval.heap()))
    }

    /// `ocx.env(name) -> str | None`
    fn env(#[starlark(require = pos)] name: &str) -> starlark::Result<NoneOr<String>> {
        // Credential / resolution-reserved keys are never readable: when
        // `--clean` is not set the composed env inherits the host's
        // `OCX_AUTH_*` secrets, and a script must not be able to exfiltrate
        // them (or probe the loader policy). Returns `None`, indistinguishable
        // from "unset", so a script cannot even detect their presence.
        if is_reserved_env_key(name) {
            return Ok(NoneOr::None);
        }
        Ok(host::with(|s| match s.env.get(name) {
            Some(v) => NoneOr::Other(v.to_string_lossy().into_owned()),
            None => NoneOr::None,
        }))
    }

    /// `ocx.platform() -> {"os":…, "arch":…}`
    fn platform<'v>(eval: &mut starlark::eval::Evaluator<'v, '_, '_>) -> starlark::Result<Value<'v>> {
        use starlark::values::dict::AllocDict;
        let (os, arch) = host::with(|s| {
            let segs = s.platform.segments();
            let os = segs.first().cloned().unwrap_or_else(|| "any".to_string());
            let arch = segs.get(1).cloned().unwrap_or_else(|| "any".to_string());
            (os, arch)
        });
        Ok(eval.heap().alloc(AllocDict([("os", os), ("arch", arch)])))
    }

    /// `ocx.package_root() -> str`
    fn package_root() -> starlark::Result<String> {
        Ok(host::with(|s| slash_path(&s.package_root)))
    }

    /// `ocx.scratch_root() -> str`
    fn scratch_root() -> starlark::Result<String> {
        Ok(host::with(|s| slash_path(&s.scratch_root)))
    }

    /// `ocx.read_file(path, *, max_bytes=1048576) -> str`
    fn read_file(
        #[starlark(require = pos)] path: &str,
        #[starlark(require = named, default = DEFAULT_READ_MAX_BYTES)] max_bytes: i32,
    ) -> starlark::Result<String> {
        let cap = usize::try_from(max_bytes.max(0)).unwrap_or(0);
        let resolved = host::with(|s| {
            guard::resolve_read(path, &s.scratch_root, &s.package_root)
                .map_err(|e| e.to_string())
                .and_then(|p| {
                    // Symlink re-check against the root that actually contains
                    // the resolved path (read side is NOT exempt — C1).
                    let root = if p.starts_with(&s.scratch_root) {
                        &s.scratch_root
                    } else {
                        &s.package_root
                    };
                    guard::verify_symlink_containment(root, &p)
                        .map_err(|e| e.to_string())
                        .map(|()| p)
                })
        })
        .map_err(fail)?;

        let bytes = std::fs::read(&resolved).map_err(|e| fail(format!("ocx.read_file failed for '{path}': {e}")))?;
        let slice = if bytes.len() > cap { &bytes[..cap] } else { &bytes[..] };
        match std::str::from_utf8(slice) {
            Ok(s) => Ok(s.to_string()),
            Err(_) => Err(script_type(format!("ocx.read_file: '{path}' is not valid UTF-8"))),
        }
    }

    /// `ocx.write_file(path, content)` — scratch-only.
    fn write_file(
        #[starlark(require = pos)] path: &str,
        #[starlark(require = pos)] content: &str,
    ) -> starlark::Result<NoneType> {
        let resolved = host::with(|s| {
            guard::resolve_scratch(path, &s.scratch_root)
                .map_err(|e| e.to_string())
                .and_then(|p| {
                    guard::verify_symlink_containment(&s.scratch_root, &p)
                        .map_err(|e| e.to_string())
                        .map(|()| p)
                })
        })
        .map_err(fail)?;

        std::fs::write(&resolved, content.as_bytes())
            .map_err(|e| fail(format!("ocx.write_file failed for '{path}': {e}")))?;
        Ok(NoneType)
    }

    /// `ocx.exists(path) -> bool`
    fn exists(#[starlark(require = pos)] path: &str) -> starlark::Result<bool> {
        let resolved = host::with(|s| {
            guard::resolve_read(path, &s.scratch_root, &s.package_root)
                .map_err(|e| e.to_string())
                .and_then(|p| {
                    let root = if p.starts_with(&s.scratch_root) {
                        &s.scratch_root
                    } else {
                        &s.package_root
                    };
                    guard::verify_symlink_containment(root, &p)
                        .map_err(|e| e.to_string())
                        .map(|()| p)
                })
        })
        .map_err(fail)?;
        Ok(resolved.exists())
    }

    /// `ocx.mkdir(path)` — recursive, idempotent (`mkdir -p`), scratch-only.
    fn mkdir(#[starlark(require = pos)] path: &str) -> starlark::Result<NoneType> {
        let resolved = host::with(|s| {
            guard::resolve_scratch(path, &s.scratch_root)
                .map_err(|e| e.to_string())
                .and_then(|p| {
                    guard::verify_symlink_containment(&s.scratch_root, &p)
                        .map_err(|e| e.to_string())
                        .map(|()| p)
                })
        })
        .map_err(fail)?;

        std::fs::create_dir_all(&resolved).map_err(|e| fail(format!("ocx.mkdir failed for '{path}': {e}")))?;
        Ok(NoneType)
    }
}

/// Registers the `ocx` namespace on the globals builder.
pub(super) fn ocx_module(globals: &mut GlobalsBuilder) {
    globals.namespace("ocx", ocx_members);
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── B1: ocx.env credential / reserved-key deny-list ──────────────────────
    //
    // `ocx.env(name)` must return None for any credential (`OCX_AUTH_*`) or
    // resolution-reserved key so a `.star` script cannot exfiltrate inherited
    // host secrets when `--clean` is not set. Single source of truth: the same
    // predicate the `ocx.run` env-overlay rejection uses.

    #[test]
    fn reserved_predicate_blocks_credential_keys() {
        assert!(is_reserved_env_key("OCX_AUTH_FOO_TOKEN"));
        assert!(is_reserved_env_key("OCX_AUTH_my_registry_USER"));
        // Case-insensitive (Windows env semantics + defence in depth).
        assert!(is_reserved_env_key("ocx_auth_foo_token"));
    }

    #[test]
    fn reserved_predicate_blocks_resolution_keys() {
        assert!(is_reserved_env_key("PATH"));
        assert!(is_reserved_env_key("OCX_HOME"));
        assert!(is_reserved_env_key(crate::env::keys::OCX_BINARY_PIN));
        assert!(is_reserved_env_key(crate::env::keys::OCX_CONFIG));
        assert!(is_reserved_env_key(crate::env::keys::OCX_INDEX));
    }

    #[test]
    fn reserved_predicate_allows_benign_keys() {
        // A benign package-exported var must remain readable by ocx.env.
        assert!(!is_reserved_env_key("CMAKE_ROOT"));
        assert!(!is_reserved_env_key("MY_TOOL_HOME"));
        // A key that merely contains (but does not start with) the prefix.
        assert!(!is_reserved_env_key("NOT_OCX_AUTH_FOO"));
        // The bare prefix-shorter key is not a credential key.
        assert!(!is_reserved_env_key("OCX_AUT"));
    }

    #[test]
    fn reserved_predicate_is_byte_boundary_safe() {
        // C-1: a script may supply an arbitrary non-ASCII env key. The
        // credential-mask predicate slices bytes, not the `str`, so a key
        // whose byte index 9 lands inside a multibyte scalar must NOT panic
        // and must report `false` (it is not an `OCX_AUTH_` credential key).
        assert!(!is_reserved_env_key("é"));
        // A short 1-byte key (shorter than the prefix) must not panic either.
        assert!(!is_reserved_env_key("x"));
        // A multibyte key longer than the prefix span — still no panic, still
        // not a credential key.
        assert!(!is_reserved_env_key("ééééé_TOKEN"));
        // ASCII-case-insensitive positive still holds after the rewrite.
        assert!(is_reserved_env_key("ocx_auth_x"));
        assert!(is_reserved_env_key("OcX_AuTh_FOO_TOKEN"));
    }

    // ── W1: re-entrant ocx symlink bypass ────────────────────────────────────
    //
    // A PATH symlink `foo -> .../ocx` must be refused: the stem check fails
    // (stem is `foo`), so `is_ocx_binary` must canonicalize both sides and
    // catch it via the OCX_BINARY_PIN comparison.

    #[test]
    #[cfg(unix)]
    fn symlink_to_ocx_pin_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        // A real file standing in for the pinned ocx binary.
        let real_ocx = dir.path().join("ocx-real");
        std::fs::write(&real_ocx, b"#!/bin/sh\n").unwrap();
        // A PATH symlink whose name is NOT `ocx` pointing at it.
        let link = dir.path().join("foo");
        std::os::unix::fs::symlink(&real_ocx, &link).unwrap();

        let mut env = crate::env::Env::clean();
        env.set(crate::env::keys::OCX_BINARY_PIN, real_ocx.as_os_str());

        // Stem is `foo` (fast pre-filter must NOT match), yet the canonicalized
        // target equals the canonicalized pin → refused.
        assert!(
            is_ocx_binary(&env, &link),
            "a non-`ocx`-named symlink resolving to the pinned ocx must be refused"
        );
    }

    #[test]
    #[cfg(unix)]
    fn unrelated_binary_is_not_refused() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("shtool");
        std::fs::write(&other, b"#!/bin/sh\n").unwrap();
        let pin = dir.path().join("ocx-real");
        std::fs::write(&pin, b"#!/bin/sh\n").unwrap();

        let mut env = crate::env::Env::clean();
        env.set(crate::env::keys::OCX_BINARY_PIN, pin.as_os_str());

        assert!(!is_ocx_binary(&env, &other), "an unrelated binary must not be refused");
    }

    #[test]
    fn stem_named_ocx_is_refused_fast() {
        // Fast pre-filter: a program whose stem reads `ocx` is refused without
        // touching the filesystem.
        let env = crate::env::Env::clean();
        assert!(is_ocx_binary(&env, Path::new("/usr/local/bin/ocx")));
        assert!(is_ocx_binary(&env, Path::new("/somewhere/OCX")));
    }

    // ── C-3: path-bearing program binds to the validated cwd ─────────────────
    //
    // A `program` carrying a path separator must resolve relative to the
    // guarded cwd the child will actually run in — not the process CWD —
    // otherwise `ocx.run("./tool", cwd="subdir")` would exec/refuse-check the
    // wrong binary.

    #[test]
    fn path_bearing_program_resolves_against_cwd() {
        let env = crate::env::Env::clean();
        let cwd = Path::new("/sandbox/subdir");
        assert_eq!(
            resolve_program(&env, "./tool", cwd),
            cwd.join("tool"),
            "`./tool` must bind to <cwd>/tool"
        );
        assert_eq!(
            resolve_program(&env, "bin/tool", cwd),
            cwd.join("bin/tool"),
            "`bin/tool` must bind under the validated cwd"
        );
    }

    #[test]
    fn absolute_program_is_left_untouched() {
        let env = crate::env::Env::clean();
        let cwd = Path::new("/sandbox/subdir");
        let abs = if cfg!(windows) { r"C:\bin\tool" } else { "/usr/bin/tool" };
        assert_eq!(
            resolve_program(&env, abs, cwd),
            Path::new(abs),
            "an absolute program path must not be re-anchored on the cwd"
        );
    }

    #[test]
    fn bare_name_does_not_anchor_on_cwd() {
        // A PATH-only name keeps PATH-resolution behaviour: it must NOT be
        // joined onto the cwd (that would defeat PATH lookup entirely).
        let mut env = crate::env::Env::clean();
        env.set("PATH", "");
        #[cfg(windows)]
        env.set("PATHEXT", ".EXE");
        let cwd = Path::new("/sandbox/subdir");
        let resolved = resolve_program(&env, "definitely_missing_bin_xyz", cwd);
        assert!(
            !resolved.starts_with(cwd),
            "a bare PATH name must not be anchored on the cwd, got {resolved:?}"
        );
    }

    // ── B2: per-stream output cap ────────────────────────────────────────────

    #[test]
    fn cap_stream_truncates_oversized_buffer() {
        // The streaming reader takes at most OUTPUT_CAP_BYTES + 1; a producer
        // that exceeds the cap yields a cap+1 buffer here. cap_stream must
        // report truncated and bound the returned text at the cap.
        let oversized = vec![b'x'; OUTPUT_CAP_BYTES + 1];
        let (text, truncated) = cap_stream(&oversized);
        assert!(truncated, "an over-cap stream must report truncated=true");
        assert!(
            text.len() <= OUTPUT_CAP_BYTES,
            "captured text must be bounded at the cap, got {}",
            text.len()
        );
    }

    #[test]
    fn cap_stream_passes_small_buffer_untouched() {
        let small = b"hello".to_vec();
        let (text, truncated) = cap_stream(&small);
        assert!(!truncated);
        assert_eq!(text, "hello");
    }
}
