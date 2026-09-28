// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx.*` host module exposed to test scripts.
//!
//! Every path arg and `ocx.run(cwd=…)` goes through a `guard` resolver and then the symlink re-check before the
//! syscall, or a script escapes the sandbox.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use starlark::environment::GlobalsBuilder;
use starlark::starlark_module;
use starlark::values::none::{NoneOr, NoneType};
use starlark::values::tuple::UnpackTuple;
use starlark::values::{Value, dict::DictRef};

use super::arch_value::ArchValue;
use super::guard;
use super::host;
use super::os_value::OsValue;
use super::platform_value::PlatformValue;
use super::run_result::{OUTPUT_CAP_BYTES, RunResult};
use super::sl_error::{fail, script_type};
use ocx_oci::platform::{Architecture, OperatingSystem};

const DEFAULT_READ_MAX_BYTES: i32 = 1_048_576;

/// Env keys an `ocx.run(env=...)` overlay may never override, or it changes which binary and OCX config the child
/// resolves.
const RESERVED_ENV_KEYS: &[&str] = &[
    "PATH",
    ocx_config::env::keys::OCX_HOME,
    ocx_config::env::keys::OCX_BINARY_PIN,
    ocx_config::env::keys::OCX_CONFIG,
    ocx_config::env::keys::OCX_PROJECT,
    ocx_config::env::keys::OCX_INDEX,
    ocx_config::env::keys::OCX_NO_CONFIG,
    ocx_config::env::keys::OCX_NO_PROJECT,
    ocx_config::env::keys::OCX_OFFLINE,
    ocx_config::env::keys::OCX_REMOTE,
];

/// Per-registry credential env vars; a script may neither read them from the inherited env nor set them on a child.
const CREDENTIAL_ENV_PREFIX: &str = "OCX_AUTH_";

/// Whether an env key is off-limits to scripts, for both the `ocx.run` overlay and `ocx.env` reads.
fn is_reserved_env_key(key: &str) -> bool {
    // Slice bytes, not the `str`: `key[..N]` panics inside a multibyte scalar of a script-supplied key.
    let credential = key
        .as_bytes()
        .get(..CREDENTIAL_ENV_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(CREDENTIAL_ENV_PREFIX.as_bytes()));
    RESERVED_ENV_KEYS.iter().any(|r| r.eq_ignore_ascii_case(key)) || credential
}

fn slash_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Spawns `program` on the composed env plus overlay, capturing capped output under the wall-clock deadline.
fn spawn_capture(
    program: &Path,
    args: &[String],
    base_env: &ocx_config::env::Env,
    overlay: &[(String, String)],
    cwd: &Path,
    stdin: Option<&str>,
    wall_clock: Duration,
) -> Result<RunResult, String> {
    use std::process::Stdio;

    let start = Instant::now();
    let handle = tokio::runtime::Handle::current();

    let stdin_cfg = if stdin.is_some() { Stdio::piped() } else { Stdio::null() };

    // The overlay's `.envs` must come second so its keys win.
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
            // `BrokenPipe` only means the child stopped reading, which is legal; the wait reports its outcome.
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
            // Or `engine::classify` reports this kill as `Failed`, not `Timeout`.
            super::host::note_timeout();
            Err(format!(
                "ocx.run child exceeded the {} ms wall-clock deadline and was killed",
                wall_clock.as_millis()
            ))
        }
        Err(WaitError::Io(e)) => Err(format!("ocx.run failed waiting for child: {e}")),
    }
}

/// Caps a captured stream at [`OUTPUT_CAP_BYTES`], returning lossy UTF-8 text and whether it was truncated.
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

enum WaitError {
    TimedOut,
    Io(std::io::Error),
}

/// A borrowing `wait_with_output`, which tokio's `Child` lacks.
trait ChildWaitExt {
    async fn wait_with_output_ref(&mut self) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), WaitError>;
    #[cfg(not(unix))]
    async fn wait_with_output_owned(&mut self) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), WaitError>;
}

impl ChildWaitExt for tokio::process::Child {
    async fn wait_with_output_ref(&mut self) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), WaitError> {
        use tokio::io::AsyncReadExt;
        // Cap while reading, or a GiB-producing child exhausts host memory; the `+ 1` lets `cap_stream` see overflow.
        const TAKE_LIMIT: u64 = OUTPUT_CAP_BYTES as u64 + 1;
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();
        let mut out = self.stdout.take();
        let mut err = self.stderr.take();
        let read_out = async {
            let Some(s) = out.as_mut() else {
                return Ok::<(), std::io::Error>(());
            };
            AsyncReadExt::take(&mut *s, TAKE_LIMIT)
                .read_to_end(&mut out_buf)
                .await?;
            // Drain the rest, or the child blocks on a full pipe and `join!` deadlocks.
            tokio::io::copy(&mut *s, &mut tokio::io::sink()).await?;
            Ok(())
        };
        let read_err = async {
            let Some(s) = err.as_mut() else {
                return Ok::<(), std::io::Error>(());
            };
            AsyncReadExt::take(&mut *s, TAKE_LIMIT)
                .read_to_end(&mut err_buf)
                .await?;
            tokio::io::copy(&mut *s, &mut tokio::io::sink()).await?;
            Ok(())
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

/// True if `program` carries a path separator; `\` counts on every platform, as elsewhere in the sandbox.
fn program_is_path(program: &str) -> bool {
    program.contains('/') || program.contains('\\')
}

/// Resolves a program name to the binary the child will execute.
///
/// A bare name resolves on the composed env's PATH only, never the overlay's. A path resolves against the guarded
/// `cwd` the child runs in, or the re-entrancy check inspects the wrong binary.
fn resolve_program(base_env: &ocx_config::env::Env, program: &str, cwd: &Path) -> Result<std::path::PathBuf, String> {
    if program_is_path(program) {
        let raw = Path::new(program);
        return Ok(if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            cwd.join(raw)
        });
    }
    base_env.resolve_test_command(program).map_err(|e| e.to_string())
}

/// True if `resolved` is an `ocx` binary, which is refused: a nested `ocx` would write the real `$OCX_HOME`.
fn is_ocx_binary(base_env: &ocx_config::env::Env, resolved: &Path) -> bool {
    let stem = resolved
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("ocx"))
        .unwrap_or(false);
    if stem {
        return true;
    }

    // Canonicalize both sides, or a symlink `foo -> ocx` passes both the stem check and a raw pin comparison.
    let canonical_resolved = std::fs::canonicalize(resolved).ok();
    let Some(canonical_resolved) = canonical_resolved else {
        return false;
    };

    let mut pins: Vec<std::path::PathBuf> = Vec::new();
    if let Some(pin) = base_env.get(ocx_config::env::keys::OCX_BINARY_PIN) {
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
        let argv: Vec<&str> = args
            .items
            .iter()
            .map(|v| v.unpack_str())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| script_type("ocx.run arguments must all be strings"))?;
        let (program, rest) = argv
            .split_first()
            .ok_or_else(|| fail("ocx.run requires at least a program"))?;

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
            // A rejected cwd falls back to scratch, never the process CWD.
            let cwd_for_resolve = cwd_path
                .as_ref()
                .map(PathBuf::as_path)
                .unwrap_or(s.scratch_root.as_path());
            let resolved = resolve_program(&s.env, program, cwd_for_resolve);
            (resolved, cwd_path, s.wall_clock)
        });

        let cwd_path = cwd_path.map_err(fail)?;
        let resolved = resolved.map_err(fail)?;

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
        // `None`, as if unset, or a script can exfiltrate inherited `OCX_AUTH_*` secrets or detect them.
        if is_reserved_env_key(name) {
            return Ok(NoneOr::None);
        }
        Ok(host::with(|s| match s.env.get(name) {
            Some(v) => NoneOr::Other(v.to_string_lossy().into_owned()),
            None => NoneOr::None,
        }))
    }

    /// `ocx.read_file(path, *, max_bytes=1048576) -> str`
    fn read_file(
        #[starlark(require = pos)] path: &str,
        #[starlark(require = named, default = DEFAULT_READ_MAX_BYTES)] max_bytes: i32,
    ) -> starlark::Result<String> {
        let cap = usize::try_from(max_bytes.max(0)).unwrap_or(0);
        let resolved = host::with(|s| {
            guard::resolve_read(path, &s.scratch_root, &s.content_root)
                .map_err(|e| e.to_string())
                .and_then(|p| {
                    // The content root, not the package root, or a bundle symlink reaches the store's `refs/`.
                    let root = if p.starts_with(&s.scratch_root) {
                        &s.scratch_root
                    } else {
                        &s.content_root
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
            guard::resolve_read(path, &s.scratch_root, &s.content_root)
                .map_err(|e| e.to_string())
                .and_then(|p| {
                    let root = if p.starts_with(&s.scratch_root) {
                        &s.scratch_root
                    } else {
                        &s.content_root
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

fn os_members(globals: &mut GlobalsBuilder) {
    for variant in OperatingSystem::VARIANTS {
        let value = OsValue(*variant);
        globals.set(value.starlark_name(), value);
    }
}

fn arch_members(globals: &mut GlobalsBuilder) {
    for variant in Architecture::VARIANTS {
        let value = ArchValue(*variant);
        globals.set(value.starlark_name(), value);
    }
}

/// Registers the `ocx` namespace with its per-run attributes, frozen from the host scope.
///
/// With no host scope installed, `target_platform` is `Platform::Any` and the roots are empty strings.
pub(super) fn ocx_module(globals: &mut GlobalsBuilder) {
    let platform = host::try_with(|s| PlatformValue::from_platform(&s.platform))
        .unwrap_or_else(|| PlatformValue::from_platform(&ocx_oci::Platform::Any));
    let package_root = host::try_with(|s| slash_path(&s.package_root)).unwrap_or_default();
    let content_root = host::try_with(|s| slash_path(&s.content_root)).unwrap_or_default();
    let scratch_root = host::try_with(|s| slash_path(&s.scratch_root)).unwrap_or_default();
    globals.namespace("ocx", |b| {
        ocx_members(b);
        b.namespace("os", os_members);
        b.namespace("arch", arch_members);
        b.set("target_platform", platform);
        b.set("package_root", package_root);
        b.set("content_root", content_root);
        b.set("scratch_root", scratch_root);
    });
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
        assert!(is_reserved_env_key(ocx_config::env::keys::OCX_BINARY_PIN));
        assert!(is_reserved_env_key(ocx_config::env::keys::OCX_CONFIG));
        assert!(is_reserved_env_key(ocx_config::env::keys::OCX_INDEX));
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

        let mut env = ocx_config::env::Env::clean();
        env.set(ocx_config::env::keys::OCX_BINARY_PIN, real_ocx.as_os_str());

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

        let mut env = ocx_config::env::Env::clean();
        env.set(ocx_config::env::keys::OCX_BINARY_PIN, pin.as_os_str());

        assert!(!is_ocx_binary(&env, &other), "an unrelated binary must not be refused");
    }

    #[test]
    fn stem_named_ocx_is_refused_fast() {
        // Fast pre-filter: a program whose stem reads `ocx` is refused without
        // touching the filesystem.
        let env = ocx_config::env::Env::clean();
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
        let env = ocx_config::env::Env::clean();
        let cwd = Path::new("/sandbox/subdir");
        assert_eq!(
            resolve_program(&env, "./tool", cwd).unwrap(),
            cwd.join("tool"),
            "`./tool` must bind to <cwd>/tool"
        );
        assert_eq!(
            resolve_program(&env, "bin/tool", cwd).unwrap(),
            cwd.join("bin/tool"),
            "`bin/tool` must bind under the validated cwd"
        );
    }

    #[test]
    fn absolute_program_is_left_untouched() {
        let env = ocx_config::env::Env::clean();
        let cwd = Path::new("/sandbox/subdir");
        let abs = if cfg!(windows) { r"C:\bin\tool" } else { "/usr/bin/tool" };
        assert_eq!(
            resolve_program(&env, abs, cwd).unwrap(),
            Path::new(abs),
            "an absolute program path must not be re-anchored on the cwd"
        );
    }

    #[test]
    fn bare_name_does_not_anchor_on_cwd() {
        // A PATH-only name keeps PATH-resolution behaviour: it must NOT be
        // joined onto the cwd (that would defeat PATH lookup entirely).
        let mut env = ocx_config::env::Env::clean();
        env.set("PATH", "");
        #[cfg(windows)]
        env.set("PATHEXT", ".EXE");
        let cwd = Path::new("/sandbox/subdir");
        let resolved = resolve_program(&env, "definitely_missing_bin_xyz", cwd).unwrap();
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
