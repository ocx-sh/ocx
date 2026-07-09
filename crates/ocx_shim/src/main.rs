// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-shim` — native Windows launcher shim.
//!
//! One copy is emitted per entrypoint at install time (`<name>.exe`) next to
//! a one-line `<name>.shim` sidecar carrying the package root. At runtime the
//! shim derives its own stem, reads the sidecar, and spawns
//! `ocx launcher exec "<pkg_root>" -- "<stem>" <argv>` directly via
//! `CreateProcessW` — bypassing `cmd.exe` and closing the residual
//! BatBadBut / `%*` re-parse surface that the `.cmd` launcher leaves open.
//!
//! The shim is a separate binary; it does not use OCX's `Error` enum. Each
//! failure maps to a process exit code aligned with `quality-rust-exit_codes.md`
//! (sysexits.h base 64). Diagnostics go to stderr, one lowercase line, no
//! trailing period, prefixed `ocx-shim:` (`quality-rust-errors.md` C-GOOD-ERR).
//!
//! See `.claude/artifacts/adr_windows_exe_shim.md` (error taxonomy E1–E8,
//! Contract 1) and `system_design_windows_exe_shim.md`.
//
// TODO(arch-review): confirm msvc vs gnu toolchain choice and DLL
// search-order hardening (SetDllDirectoryW(null) early in main on msvc, vs
// the gnu hermetic-launcher precedent). `dist-workspace.toml` lists
// windows-msvc targets, so `rust-toolchain.toml` pins the two msvc targets;
// revisit during architecture review (plan §1.1 gate-affecting decision).

// On non-Windows hosts the entire Win32 runtime is stubbed out; `cargo check`
// must stay green on the Linux CI host (the shim is check-only there).
#![cfg_attr(not(windows), allow(dead_code))]

use std::process::exit;

// Stack-probe builtins for the hermetic `*-pc-windows-gnullvm` cross-build
// (cargo-zigbuild, `-nolibc`). `target_abi = "llvm"` is the discriminator that
// uniquely selects the gnullvm targets — msvc/gnu builds get the probe from
// their own runtime and must not pick this up. See `chkstk.rs`.
#[cfg(all(target_os = "windows", target_env = "gnu", target_abi = "llvm"))]
mod chkstk;

/// Shim failure taxonomy. Mirrors the ADR §Error Taxonomy table.
///
/// E7 (job-object setup failure) is intentionally **not** a variant: it is
/// non-fatal — the shim logs a warning and proceeds without the job object
/// rather than failing (failing there would regress vs `.cmd`). E8 (child ran
/// and exited) is likewise not an error: the child's exit code is forwarded
/// transparently (full i32 passthrough, Windows semantics).
#[derive(Debug)]
enum ShimError {
    /// E1 — the `<stem>.shim` sidecar does not exist. Exit 78 (`EX_CONFIG`).
    SidecarNotFound { path: String },
    /// E2 — the sidecar is empty, too large, not UTF-8, not absolute, or
    /// contains an embedded NUL/CR/LF before the terminator. Exit 78.
    MalformedSidecar { reason: String },
    /// E3 — `pkg_root` resolves outside `<OCX_HOME>/packages/`
    /// (defense-in-depth; primary check stays in `ocx launcher exec`).
    /// Exit 77 (`EX_NOPERM`).
    ContainmentViolation { path: String },
    /// E4 — `GetModuleFileNameW` failed or yielded no usable stem.
    /// Exit 74 (`EX_IOERR`).
    SelfPathFailure,
    /// E5 — `ocx` could not be started because it was not found. Exit 69
    /// (`EX_UNAVAILABLE`). `pinned` carries the resolved program when
    /// `OCX_BINARY_PIN` was *defined* (so the stderr line names the missing
    /// pinned path instead of the misleading "add ocx to PATH" hint); `None`
    /// means the variable was unset and the literal `ocx` PATH search missed.
    OcxNotFound { pinned: Option<String> },
    /// E6 — `CreateProcessW` failed for any other reason. Exit 74, unless the
    /// Win32 error is `ERROR_ACCESS_DENIED` (5) → exit 77 (derived purely from
    /// `win32`; no redundant flag). `win32` carries the raw `GetLastError`
    /// code so the stderr line names it (ADR E6, plan F-5); `program` is the
    /// resolved program so the operator sees what failed.
    SpawnFailure { win32: u32, program: String },
}

/// `GetLastError` value for `ERROR_ACCESS_DENIED`. Named (not a bare `5`
/// literal) so the E6 → exit-code 77 derivation is self-documenting and
/// avoids the magic-numeric anti-pattern (`quality-rust-exit_codes.md`).
///
/// Bound to the `windows-sys` constant on the real Windows target; the
/// non-Windows host build (Linux CI, check/test-only — `windows-sys` is a
/// `cfg(windows)` dependency) mirrors the same stable Win32 value so the
/// pure exit-code mapping stays host-testable.
#[cfg(windows)]
const ERROR_ACCESS_DENIED_CODE: u32 = windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
#[cfg(not(windows))]
const ERROR_ACCESS_DENIED_CODE: u32 = 5;

impl ShimError {
    /// Maps each failure to its process exit code per the ADR §Error Taxonomy
    /// table.
    fn exit_code(&self) -> i32 {
        match self {
            // E1 / E2 — missing or unusable per-install config artifact.
            ShimError::SidecarNotFound { .. } | ShimError::MalformedSidecar { .. } => 78,
            // E3 — tamper / containment violation.
            ShimError::ContainmentViolation { .. } => 77,
            // E4 — OS-level failure obtaining own identity.
            ShimError::SelfPathFailure => 74,
            // E5 — required dependency unavailable (pinned or PATH miss).
            ShimError::OcxNotFound { .. } => 69,
            // E6 — spawn failure; ACCESS_DENIED is the permission subcase,
            // derived purely from the Win32 code (no redundant flag).
            ShimError::SpawnFailure { win32, .. } => {
                if *win32 == ERROR_ACCESS_DENIED_CODE {
                    77
                } else {
                    74
                }
            }
        }
    }

    /// One lowercase stderr line, no trailing period, `ocx-shim:` prefix
    /// (`quality-rust-errors.md` C-GOOD-ERR). Config-class errors (E1/E2)
    /// append a parenthetical recovery hint.
    fn stderr_message(&self) -> String {
        match self {
            ShimError::SidecarNotFound { path } => {
                // Recovery hint: re-running `ocx install` regenerates the
                // entrypoint (`.exe` + `.shim`) for the package.
                format!("ocx-shim: sidecar not found: {path} (re-run `ocx install` to regenerate the entrypoint)")
            }
            ShimError::MalformedSidecar { reason } => {
                format!("ocx-shim: malformed sidecar: {reason} (re-run `ocx install` to regenerate the entrypoint)")
            }
            ShimError::ContainmentViolation { path } => {
                format!("ocx-shim: package root outside OCX home: {path}")
            }
            ShimError::SelfPathFailure => "ocx-shim: cannot determine own path".to_string(),
            ShimError::OcxNotFound { pinned: None } => {
                "ocx-shim: ocx not found (set OCX_BINARY_PIN or add ocx to PATH)".to_string()
            }
            ShimError::OcxNotFound { pinned: Some(p) } => {
                // OCX_BINARY_PIN was defined but the path does not exist —
                // naming the pinned program is actionable; the generic
                // "add ocx to PATH" hint would be misleading (PATH was not
                // the resolution path here).
                format!("ocx-shim: pinned ocx not found: {p} (OCX_BINARY_PIN points at a missing path)")
            }
            ShimError::SpawnFailure { win32, program } => {
                // ADR E6 / plan F-5: name the Win32 error and the resolved
                // program. Clean C-GOOD-ERR line — no angle-bracket
                // placeholder (lowercase, no trailing period).
                format!("ocx-shim: failed to start {program}: win32 error {win32}")
            }
        }
    }
}

/// Pure (host-runnable) shim logic, split from the Win32 syscalls so the
/// wire-ABI assembler, sidecar parser, stem derivation, and program
/// resolution can be unit-tested on the Linux CI host (system_design §8
/// mandates the pure/Win32 split). See [`core`] module docs.
mod core;

/// Entry point. Always diverges via [`std::process::exit`] (the crate builds
/// with `panic = "abort"` in the size profile — see `Cargo.toml`).
fn main() -> ! {
    // DLL search-order hardening (plan F-3): on the chosen msvc target a
    // planted DLL in the application directory could be loaded ahead of the
    // System32 copy. Lock the search path to System32 *before any other Win32
    // call* (well before `CreateProcessW`/dependent DLL loads). msvc is the
    // chosen target; gnu is fallback-only.
    #[cfg(windows)]
    harden_dll_search_path();

    match run() {
        // E8 — child ran and exited; forward its code transparently.
        Ok(code) => exit(code),
        Err(err) => {
            eprintln!("{}", err.stderr_message());
            exit(err.exit_code())
        }
    }
}

/// Restricts the process DLL search path to System32 so a DLL planted next to
/// the shim cannot be loaded ahead of the system copy (plan F-3). Called as
/// the very first Win32 interaction in [`main`].
///
/// `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` is available on
/// every shipped Windows 8+/Win10+ msvc target the shim is built for, so its
/// failure is not a reachable state in practice. The earlier
/// `SetDllDirectoryW(NULL)` fallback was dropped (YAGNI): it is unreachable on
/// supported targets and is strictly *weaker* (it only drops the current
/// working directory from the search order, leaving the application directory
/// — the actual planted-DLL vector — in place). On the unreachable
/// API-missing path the documented degraded state is "default OS DLL search
/// order"; the shim does no further dependent `LoadLibrary` work before
/// `CreateProcessW`, so the residual exposure is minimal.
#[cfg(windows)]
fn harden_dll_search_path() {
    use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};

    // SAFETY: `SetDefaultDllDirectories` takes only an integer flag and has no
    // pointer parameters. `LOAD_LIBRARY_SEARCH_SYSTEM32` is a valid flag value.
    // A zero return is the documented (here unreachable) degraded state above.
    let _ = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
}

/// Shim runtime (ADR Contract 1, §Behavior).
#[cfg(windows)]
fn run() -> Result<i32, ShimError> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_PATH_NOT_FOUND, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_FAILED,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };

    // ── Step 1: own module path → stem (grow-buffer; no silent truncation) ──
    //
    // A fixed buffer would silently yield a *truncated* path → wrong stem and
    // wrong sidecar with no E4 (Codex-deferred self-path truncation). Retry on
    // ERROR_INSUFFICIENT_BUFFER, growing the buffer, until the OS reports a
    // length strictly less than the buffer (i.e. it fit, not truncated).
    //
    // Assumes Vista+ : only there does `GetModuleFileNameW` NUL-truncate the
    // buffer AND set `ERROR_INSUFFICIENT_BUFFER` on overflow. Pre-Vista has
    // neither (it returns `nSize` with no error), so the `written == cap`
    // check below is the pre-Vista safety net. Every shipped msvc target the
    // shim is built for is Vista+, so this is reality, not a hypothetical.
    let module_path = {
        let mut cap: usize = 512;
        loop {
            let mut buf = vec![0u16; cap];
            // SAFETY: `buf` is a valid, `cap`-element u16 allocation; we pass
            // its length as `nsize`. `GetModuleFileNameW(NULL, ...)` writes at
            // most `nsize` code units and returns the count written.
            let written = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), cap as u32) };
            if written == 0 {
                return Err(ShimError::SelfPathFailure);
            }
            let written = written as usize;
            if written < cap {
                buf.truncate(written);
                break std::ffi::OsString::from_wide(&buf);
            }
            // Path did not fit (return == cap). On Win Vista+ the buffer is
            // also NUL-truncated and GetLastError == ERROR_INSUFFICIENT_BUFFER.
            // SAFETY: no pointers; reads thread-local last-error.
            let last = unsafe { GetLastError() };
            if last != ERROR_INSUFFICIENT_BUFFER && written != cap {
                return Err(ShimError::SelfPathFailure);
            }
            cap = cap
                .checked_mul(2)
                .filter(|&c| c <= 64 * 1024)
                .ok_or(ShimError::SelfPathFailure)?;
        }
    };
    let stem = core::derive_stem(&module_path)?;

    // ── Step 2: sidecar path = <dir>/<stem>.shim, read fully ───────────────
    let module_dir = std::path::Path::new(&module_path)
        .parent()
        .ok_or(ShimError::SelfPathFailure)?
        .to_path_buf();
    let sidecar_path = module_dir.join(format!("{stem}.shim"));
    let raw = match std::fs::read(&sidecar_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ShimError::SidecarNotFound {
                path: sidecar_path.display().to_string(),
            });
        }
        Err(err) => {
            return Err(ShimError::MalformedSidecar {
                reason: format!("cannot read sidecar: {err}"),
            });
        }
    };

    // ── Step 3: validate sidecar → pkg_root ────────────────────────────────
    let pkg_root = core::parse_sidecar(&raw)?;

    // ── Step 4: optional E3 containment (only when OCX_HOME is readable) ────
    //
    // Two distinct canonicalize failure modes (do NOT collapse them):
    //  - `OCX_HOME` itself does not canonicalize → the shim cannot run the
    //    defense-in-depth check at all; this is the ADR-sanctioned delegate
    //    path (the authoritative `validate_package_root` runs inside
    //    `launcher exec`). Silent, expected.
    //  - `OCX_HOME` canonicalizes but `pkg_root` does NOT → suspicious: the
    //    sidecar points at a path that does not resolve while OCX home does.
    //    Still delegate (`launcher exec` is authoritative) but log to stderr
    //    so the operator sees it — never silently swallow this one.
    if let Some(ocx_home) = std::env::var_os("OCX_HOME") {
        let home = std::path::Path::new(&ocx_home);
        // OCX_HOME unresolvable → ADR-sanctioned silent delegate (the
        // authoritative `validate_package_root` runs inside `launcher exec`).
        if let Ok(canon_home) = dunce::canonicalize(home) {
            match dunce::canonicalize(&pkg_root) {
                Ok(canon_root) => {
                    // `packages/` holds installed candidates; `temp/` holds
                    // `ocx package test` materializations — the test env's
                    // synthesized entrypoints are launched from there, so it
                    // is inside the trust boundary too (still under OCX_HOME).
                    let packages = canon_home.join("packages");
                    let temp = canon_home.join("temp");
                    if !canon_root.starts_with(&packages) && !canon_root.starts_with(&temp) {
                        return Err(ShimError::ContainmentViolation {
                            path: canon_root.display().to_string(),
                        });
                    }
                }
                Err(err) => {
                    // OCX_HOME ok but pkg_root unresolvable — delegate to the
                    // authoritative `launcher exec` check, but surface it.
                    eprintln!(
                        "ocx-shim: cannot canonicalize package root {pkg_root} ({err}); delegating containment to `launcher exec`"
                    );
                }
            }
        }
    }

    // ── Step 5: resolve program (OCX_BINARY_PIN parity) ────────────────────
    // `pin_defined` mirrors the `.cmd` `IF DEFINED OCX_BINARY_PIN`: present at
    // all (even empty) → pin branch. A pinned program is resolved EXPLICITLY
    // via `lpApplicationName` (no command-line program parsing — CWE-428).
    let pin = std::env::var_os("OCX_BINARY_PIN").map(|v| v.to_string_lossy().into_owned());
    let pin_defined = pin.is_some();
    let program = core::resolve_program(pin.as_deref());

    // ── Step 6: build child command line (byte-exact wire ABI) ─────────────
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let command_line = core::build_child_command_line(&program, &pkg_root, &stem, &argv);
    let mut command_line_w: Vec<u16> = std::ffi::OsStr::new(&command_line)
        .encode_wide()
        .chain(Some(0))
        .collect();

    // SECURITY (B2 / CWE-428): a pinned program goes through
    // `lpApplicationName` as its OWN NUL-terminated UTF-16 buffer so
    // `CreateProcessW` does NO command-line program-name parsing (a pinned
    // `C:\Program Files\…\ocx.cmd` would otherwise mis-resolve to
    // `C:\Program.exe`). `lpApplicationName = NULL` is used ONLY for the
    // unset-pin → literal `ocx` case, which legitimately needs PATH/PATHEXT
    // search (the quoted leading `ocx` token drives it).
    let app_name_w: Option<Vec<u16>> = core::spawn_application_name(&program, pin_defined)
        .map(|p| std::ffi::OsStr::new(p).encode_wide().chain(Some(0)).collect());
    let app_name_ptr = app_name_w.as_ref().map_or(std::ptr::null(), |b| b.as_ptr());

    // ── Step 7: console control handler installed BEFORE the child can run ─
    //
    // Finding 4 (Ctrl+C race): the child is born running (no `CREATE_SUSPENDED`
    // — see step 9), so the no-op handler MUST be in place before
    // `CreateProcessW`. Installing it afterwards left a window where a Ctrl+C
    // hit the shim's default handler and killed it before it could wait →
    // lost exit-code propagation + job cleanup. Registration failure is an
    // explicit logged degraded mode (mirrors E7 best-effort), not silent.
    install_console_ctrl_handler();

    // ── Step 8: job object KILL_ON_JOB_CLOSE created FIRST (E7 best-effort) ─
    //
    // The job is created and configured BEFORE `CreateProcessW` so the child
    // can be born inside it atomically via `PROC_THREAD_ATTRIBUTE_JOB_LIST`
    // (step 9). This removes the old `CREATE_SUSPENDED`→`AssignProcessToJobObject`
    // →`ResumeThread` race window entirely. Per E7 the job is best-effort: if
    // any of create / configure fails we fall back to a plain spawn with no
    // job (logged), never failing the shim (that would regress vs `.cmd`).
    //
    // SAFETY: NULL security attrs + NULL name = an anonymous job object.
    let job: HANDLE = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    let job = if job.is_null() {
        // SAFETY: no pointers; reads thread-local last-error.
        let last = unsafe { GetLastError() };
        eprintln!("ocx-shim: job object setup failed: {last}");
        std::ptr::null_mut()
    } else {
        // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a plain C struct
        // whose all-zero bit pattern is a valid initial state; we then set
        // only the `LimitFlags` field before passing it to the OS.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a valid, correctly-sized, zero-initialised struct;
        // the size argument matches its type. Failure is non-fatal (E7).
        let set = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            // SAFETY: no pointers; reads thread-local last-error.
            let last = unsafe { GetLastError() };
            eprintln!("ocx-shim: job object setup failed: {last}");
            // SAFETY: `job` is a valid handle from a successful
            // CreateJobObjectW; not used again on this path.
            unsafe { CloseHandle(job) };
            std::ptr::null_mut()
        } else {
            job
        }
    };

    // ── Step 9: STARTUPINFOEXW + attribute list (HANDLE_LIST + JOB_LIST) ────
    //
    // Finding 1 (CWE-403, blanket handle inheritance): `bInheritHandles=TRUE`
    // alone makes the child inherit EVERY inheritable handle in the shim, not
    // just stdio. `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` whitelists exactly the
    // three standard handles so nothing else leaks; `STARTF_USESTDHANDLES`
    // wires those same three as the child's std streams (ADR Contract 1
    // postcondition: "the child writes directly to the real console").
    // `PROC_THREAD_ATTRIBUTE_JOB_LIST` assigns the job at creation so the
    // child is born inside it (no race window).
    //
    // No-console parent (Codex#1 regression vs the removed `.cmd` path):
    // when invoked detached / from a GUI process / a service, one or more std
    // handles are `NULL`/`INVALID_HANDLE_VALUE`. `STARTF_USESTDHANDLES` is set
    // ONLY when all three are valid (see `core::use_std_handles`); otherwise
    // the `hStd*` slots stay zeroed and the OS gives the child default
    // streams — the child must still launch. The HANDLE_LIST whitelist is
    // independent: it is built from the unique *valid* std handles, so a
    // no-console parent yields an empty list, which the attribute logic below
    // already tolerates (`want_handle_list` is then false).
    //
    // The valid std handles are also de-duplicated: a console process commonly
    // has stdin == stdout (the same console handle), and the HANDLE_LIST must
    // not contain duplicates.
    //
    // SAFETY: `GetStdHandle` returns a process-owned pseudo/real handle (or
    // INVALID_HANDLE_VALUE) for the given well-known id; no pointers.
    let h_in: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let h_out: HANDLE = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let h_err: HANDLE = unsafe { GetStdHandle(STD_ERROR_HANDLE) };

    let valid = |h: HANDLE| !h.is_null() && h != INVALID_HANDLE_VALUE;
    let wire_std_handles = core::use_std_handles(valid(h_in), valid(h_out), valid(h_err));

    let mut unique_handles: Vec<HANDLE> = Vec::with_capacity(3);
    for h in [h_in, h_out, h_err] {
        if !valid(h) {
            continue;
        }
        if !unique_handles.contains(&h) {
            unique_handles.push(h);
        }
    }

    // SAFETY: `STARTUPINFOEXW` / `PROCESS_INFORMATION` are plain C structs
    // whose all-zero bit pattern is a valid, documented initial state (the
    // Win32 convention is to zero them and set `cb`). No padding/niche
    // concerns.
    let mut startup_ex: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup_ex.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    // No-console gate: only claim explicit std handles when ALL THREE are
    // valid. A no-console parent leaves the flag unset and `hStd*` zeroed so
    // `CreateProcessW` provides the child default streams rather than wiring
    // a broken handle (Codex#1 — must still launch the child).
    if wire_std_handles {
        startup_ex.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup_ex.StartupInfo.hStdInput = h_in;
        startup_ex.StartupInfo.hStdOutput = h_out;
        startup_ex.StartupInfo.hStdError = h_err;
    }
    // SAFETY: see above — `PROCESS_INFORMATION` is an all-zeroes-valid output
    // struct CreateProcessW fills in.
    let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // Number of attributes we intend to set: HANDLE_LIST (always, when we
    // have ≥1 unique std handle) + JOB_LIST (only when the job exists).
    let want_handle_list = !unique_handles.is_empty();
    let want_job_list = !job.is_null();
    let attr_count = want_handle_list as u32 + want_job_list as u32;

    // RAII-ish guard: the attribute-list backing buffer + whether it was
    // initialised, so every early return and the success path delete it
    // exactly once. `attr_list_buf` keeps the allocation alive for the whole
    // `CreateProcessW` call (the OS reads it during process creation).
    let mut attr_list_buf: Vec<u8> = Vec::new();
    let mut attr_list_ptr: LPPROC_THREAD_ATTRIBUTE_LIST = std::ptr::null_mut();
    let mut extended_present: u32 = 0;

    if attr_count > 0 {
        // First call sizes the opaque list. SAFETY: NULL list + out-pointer
        // to a local `usize`; documented two-call sizing protocol.
        let mut size: usize = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), attr_count, 0, &mut size);
        }
        if size == 0 {
            // Cannot size the list → degrade to a plain (non-extended) spawn
            // with explicit std handles only. Logged best-effort (E7-class).
            eprintln!("ocx-shim: proc-thread attribute list sizing failed; spawning without handle/job scoping");
        } else {
            attr_list_buf = vec![0u8; size];
            attr_list_ptr = attr_list_buf.as_mut_ptr().cast();
            // SAFETY: `attr_list_ptr` is a `size`-byte allocation; `size` is
            // the value the sizing call returned for `attr_count` attributes.
            let init = unsafe { InitializeProcThreadAttributeList(attr_list_ptr, attr_count, 0, &mut size) };
            if init == 0 {
                // SAFETY: no pointers; reads thread-local last-error.
                let last = unsafe { GetLastError() };
                eprintln!(
                    "ocx-shim: proc-thread attribute list init failed: {last}; spawning without handle/job scoping"
                );
                attr_list_ptr = std::ptr::null_mut();
                attr_list_buf = Vec::new();
            } else {
                let mut ok = true;
                if want_handle_list {
                    // SAFETY: `attr_list_ptr` is an initialised list with room
                    // for `attr_count` attributes. `unique_handles` is a live
                    // slice of `HANDLE` (pointer-sized) that outlives the
                    // `CreateProcessW` call below.
                    let r = unsafe {
                        UpdateProcThreadAttribute(
                            attr_list_ptr,
                            0,
                            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                            unique_handles.as_ptr().cast(),
                            std::mem::size_of_val(unique_handles.as_slice()),
                            std::ptr::null_mut(),
                            std::ptr::null(),
                        )
                    };
                    if r == 0 {
                        // SAFETY: no pointers; reads thread-local last-error.
                        let last = unsafe { GetLastError() };
                        eprintln!("ocx-shim: handle-list attribute failed: {last}");
                        ok = false;
                    }
                }
                if ok && want_job_list {
                    // `PROC_THREAD_ATTRIBUTE_JOB_LIST` takes an array of job
                    // handles; we pass exactly one. `job_handle` outlives the
                    // `CreateProcessW` call.
                    let job_handle = [job];
                    // SAFETY: as above; `job_handle` is a live 1-element array
                    // of `HANDLE` that outlives `CreateProcessW`.
                    let r = unsafe {
                        UpdateProcThreadAttribute(
                            attr_list_ptr,
                            0,
                            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                            job_handle.as_ptr().cast(),
                            std::mem::size_of_val(&job_handle),
                            std::ptr::null_mut(),
                            std::ptr::null(),
                        )
                    };
                    if r == 0 {
                        // SAFETY: no pointers; reads thread-local last-error.
                        let last = unsafe { GetLastError() };
                        eprintln!("ocx-shim: job-list attribute failed: {last}; child not born in job");
                        ok = false;
                    }
                }
                if ok {
                    startup_ex.lpAttributeList = attr_list_ptr;
                    extended_present = EXTENDED_STARTUPINFO_PRESENT;
                } else {
                    // Any attribute set failed → drop the list and spawn
                    // without extended startup info (E7-class degrade).
                    // SAFETY: `attr_list_ptr` was successfully initialised.
                    unsafe { DeleteProcThreadAttributeList(attr_list_ptr) };
                    attr_list_ptr = std::ptr::null_mut();
                    attr_list_buf = Vec::new();
                }
            }
        }
    }

    // ── Step 10: CreateProcessW (no CREATE_SUSPENDED — child born running) ──
    //
    // `bInheritHandles=TRUE` is REQUIRED for the whitelisted HANDLE_LIST set
    // to be inherited; with the attribute list present only those three std
    // handles cross into the child (Finding 1). When the attribute list could
    // not be built we still pass TRUE but only the explicit STARTF_USESTDHANDLES
    // trio is meaningfully consumed — a strictly smaller surface than the
    // previous unconditional blanket inheritance, and the documented degraded
    // mode.
    //
    // SAFETY: `lpApplicationName` is either NULL (literal-`ocx` PATH search)
    // or a valid NUL-terminated UTF-16 buffer (`app_name_w`) that outlives
    // the call — never parsed from the command line for a pinned program.
    // `command_line_w` is a mutable, NUL-terminated buffer that outlives the
    // call. `startup_ex` (and its attribute list, kept alive by
    // `attr_list_buf`) outlives the call. All other pointer args are NULL
    // except `process_info`, a valid zero-initialised output struct.
    let created = unsafe {
        CreateProcessW(
            app_name_ptr,
            command_line_w.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // bInheritHandles = TRUE (required for the whitelisted set)
            extended_present,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::addr_of!(startup_ex.StartupInfo),
            &mut process_info,
        )
    };

    // The attribute list has done its job once CreateProcessW returns; delete
    // it on EVERY exit path (success and failure). `attr_list_buf` is dropped
    // naturally afterwards.
    let delete_attr_list = |ptr: LPPROC_THREAD_ATTRIBUTE_LIST| {
        if !ptr.is_null() {
            // SAFETY: `ptr` was produced by a successful
            // InitializeProcThreadAttributeList and is deleted exactly once.
            unsafe { DeleteProcThreadAttributeList(ptr) };
        }
    };

    if created == 0 {
        // SAFETY: no pointers; reads thread-local last-error.
        let last = unsafe { GetLastError() };
        delete_attr_list(attr_list_ptr);
        drop(attr_list_buf);
        if !job.is_null() {
            // SAFETY: valid job handle, not used again.
            unsafe { CloseHandle(job) };
        }
        return match last {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Err(ShimError::OcxNotFound {
                // Name the pinned program only when OCX_BINARY_PIN was
                // *defined* (IF DEFINED semantics); an unset pin → literal
                // `ocx` PATH miss keeps the original both-hints message.
                pinned: pin_defined.then(|| program.clone()),
            }),
            // Every non-file-not-found failure is the same SpawnFailure value;
            // the ACCESS_DENIED → exit-77 (vs 74) discrimination lives in
            // `ShimError::exit_code()`, derived purely from the carried Win32
            // code — so there is no separate match arm to build here.
            other => Err(ShimError::SpawnFailure {
                win32: other,
                program: program.clone(),
            }),
        };
    }
    let child_process: HANDLE = process_info.hProcess;
    let child_thread: HANDLE = process_info.hThread;

    // Success path: the OS has copied the attribute data; release it now.
    delete_attr_list(attr_list_ptr);
    drop(attr_list_buf);

    // ── Step 11: wait + GetExitCodeProcess ─────────────────────────────────
    // SAFETY: `child_process` is a valid process handle; INFINITE blocks until
    // the child exits.
    let wait = unsafe { WaitForSingleObject(child_process, INFINITE) };
    let mut exit_code: u32 = 1;
    if wait != WAIT_FAILED {
        // SAFETY: valid process handle + valid out-pointer to a local u32.
        unsafe {
            GetExitCodeProcess(child_process, &mut exit_code);
        }
    }

    // Best-effort handle cleanup before the transparent exit. SAFETY: each
    // handle was produced by a successful Win32 call and is not used again.
    unsafe {
        CloseHandle(child_thread);
        CloseHandle(child_process);
        if !job.is_null() && job != INVALID_HANDLE_VALUE {
            CloseHandle(job);
        }
    }

    // ── Step 12: full i32 passthrough (E8) ─────────────────────────────────
    // Mirrors the Windows branch of `child_process::exit_code_from_status`
    // (full i32 passthrough — no remap, no truncation).
    Ok(exit_code as i32)
}

/// Installs a no-op console control handler that returns `TRUE` so the shim
/// itself does not terminate on Ctrl+C / Ctrl+Break — the child (sharing the
/// console, no new process group) handles the signal and the shim propagates
/// its exit code (ADR Contract 1; `CREATE_NEW_PROCESS_GROUP` is deliberately
/// NOT used).
///
/// Finding 4 (Ctrl+C race): this is installed BEFORE `CreateProcessW` (which
/// no longer uses `CREATE_SUSPENDED` — the child is born running). A Ctrl+C
/// arriving in the previously-unguarded window between spawn and handler
/// install would otherwise hit the shim's default handler and kill it before
/// it could wait, losing exit-code propagation and job cleanup. Registration
/// failure only degrades Ctrl+C handling and is non-fatal (E7-class).
#[cfg(windows)]
fn install_console_ctrl_handler() {
    use windows_sys::Win32::Foundation::TRUE;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::core::BOOL;

    unsafe extern "system" fn handler(_ctrl_type: u32) -> BOOL {
        // Returning TRUE marks the signal "handled" so the default terminate
        // action does not fire for the shim. The child receives the same
        // console signal and decides how to react.
        TRUE
    }

    // SAFETY: `handler` is a valid `extern "system"` callback with the
    // PHANDLER_ROUTINE signature; passing TRUE adds it. A failure here only
    // degrades Ctrl+C handling and is non-fatal.
    unsafe {
        SetConsoleCtrlHandler(Some(handler), TRUE);
    }
}

/// Non-Windows builds never run the shim; this keeps `cargo check --workspace`
/// green on the Linux CI host without pulling in any Win32 surface.
#[cfg(not(windows))]
fn run() -> Result<i32, ShimError> {
    unimplemented!("ocx-shim has no non-Windows runtime")
}

// ───────────────────────────────────────────────────────────────────────────
//  Specification tests (contract-first TDD, Phase 3.2)
//
//  These pin the shim's pure logic against the ADR §Error Taxonomy (E1–E8),
//  the Wire-ABI Parity matrix (plan §"Wire-ABI Parity"), and the `.shim`
//  Sidecar Format Contract. They are host-runnable (Linux CI) because the
//  Win32 syscalls are split out of `core::*` (system_design §8).
//
//  Tests are DAMP/self-contained: each spells out its own inputs and the
//  exact expected bytes/exit code rather than sharing builders, so a failure
//  names the precise contract clause that regressed.
// ───────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::ShimError;
    use super::core::{build_child_command_line, derive_stem, parse_sidecar, resolve_program};
    use std::ffi::OsStr;

    // ── ShimError → exit code mapping (ADR §Error Taxonomy table) ───────────
    //
    // E5/E6 cannot run host-side (they need a real `CreateProcessW` failure /
    // Win32 mock — acceptance-level), but the `exit_code()` mapping itself is
    // pure and is the unit-testable contract surface. These tests encode the
    // E1/E2→78, E3→77, E4→74, E5→69, E6→74, E6+ACCESS_DENIED→77 taxonomy.

    #[test]
    fn e1_sidecar_not_found_exits_78() {
        let err = ShimError::SidecarNotFound {
            path: "C:\\x\\cmake.shim".to_string(),
        };
        assert_eq!(err.exit_code(), 78, "E1 missing sidecar → EX_CONFIG (78)");
    }

    #[test]
    fn e2_malformed_sidecar_exits_78() {
        let err = ShimError::MalformedSidecar {
            reason: "embedded newline".to_string(),
        };
        assert_eq!(err.exit_code(), 78, "E2 malformed sidecar → EX_CONFIG (78)");
    }

    #[test]
    fn e3_containment_violation_exits_77() {
        let err = ShimError::ContainmentViolation {
            path: "C:\\evil".to_string(),
        };
        assert_eq!(err.exit_code(), 77, "E3 pkg_root outside OCX home → EX_NOPERM (77)");
    }

    #[test]
    fn e4_self_path_failure_exits_74() {
        assert_eq!(
            ShimError::SelfPathFailure.exit_code(),
            74,
            "E4 GetModuleFileNameW failure → EX_IOERR (74)"
        );
    }

    #[test]
    fn e5_ocx_not_found_exits_69() {
        // Win32-mock / acceptance-level: the *trigger* (CreateProcessW
        // ERROR_FILE_NOT_FOUND) needs Windows, but the code mapping is pure.
        // Uses the new `OcxNotFound { pinned }` variant shape (amendment R2);
        // red-by-COMPILE until the builder adds the `pinned` field.
        assert_eq!(
            ShimError::OcxNotFound { pinned: None }.exit_code(),
            69,
            "E5 ocx not found → EX_UNAVAILABLE (69)"
        );
    }

    // ── E6 SpawnFailure — field-less shape (quality refactor, amendment R2) ─
    //
    // Spec-to-builder (field-shape change, red-by-COMPILE against current
    // code): `ShimError::SpawnFailure` MUST drop the redundant
    // `access_denied: bool` — it is fully derivable from `win32`
    // (`ERROR_ACCESS_DENIED` == 5). New shape pinned here:
    //
    //   SpawnFailure { win32: u32, program: String }
    //
    // `exit_code()` returns 77 iff `win32 == 5`, else 74. The stderr line is
    // unchanged (`failed to start <program>: win32 error <n>`). These three
    // tests construct the field-less form, so they FAIL TO COMPILE until the
    // builder removes `access_denied` — that compile failure IS the red proof
    // the refactor is pending (accepted for a type-shape contract).

    #[test]
    fn e6_spawn_failure_other_exits_74() {
        // Win32-mock / acceptance-level trigger; pure mapping pinned here.
        let err = ShimError::SpawnFailure {
            win32: 2,
            program: "ocx".to_string(),
        };
        assert_eq!(err.exit_code(), 74, "E6 generic CreateProcessW failure → EX_IOERR (74)");
    }

    #[test]
    fn e6_spawn_failure_access_denied_exits_77() {
        // ACCESS_DENIED (win32 == 5) is the permission subcase → 77, derived
        // purely from `win32` now that `access_denied` is gone.
        let err = ShimError::SpawnFailure {
            win32: 5,
            program: "ocx".to_string(),
        };
        assert_eq!(
            err.exit_code(),
            77,
            "E6 + win32 == 5 (ERROR_ACCESS_DENIED) → EX_NOPERM (77), not 74 (derived from win32, no access_denied field)"
        );
    }

    #[test]
    fn e6_spawn_failure_stderr_is_clean_and_names_program_and_code() {
        // Plan F-5 / ADR E6 + CLI-UX fix: the stderr line must name the Win32
        // error code AND the resolved program, as a clean C-GOOD-ERR line —
        // NO angle-bracket placeholder, lowercase, no trailing period. The
        // line is UNCHANGED by the field-shape refactor.
        let err = ShimError::SpawnFailure {
            win32: 1450,
            program: "C:\\Program Files\\ocx\\ocx.cmd".to_string(),
        };
        let msg = err.stderr_message();
        assert_eq!(
            msg, "ocx-shim: failed to start C:\\Program Files\\ocx\\ocx.cmd: win32 error 1450",
            "E6 stderr must be the clean `failed to start <program>: win32 error <n>` line (no `<...>` placeholder)"
        );
        assert!(
            !msg.contains('<') && !msg.contains('>'),
            "E6 stderr must not contain angle-bracket placeholders: {msg:?}"
        );
    }

    // ── E5 OcxNotFound — pinned-vs-unset stderr branch (amendment R2) ───────
    //
    // Spec-to-builder (variant-shape change, red-by-COMPILE + behavior):
    // `ShimError::OcxNotFound` MUST carry whether `OCX_BINARY_PIN` was
    // DEFINED so the stderr line can name the pinned program instead of the
    // misleading "add ocx to PATH" hint. Pinned shape pinned here:
    //
    //   OcxNotFound { pinned: Option<String> }
    //
    // - `pinned: None`  (env unset)        → message unchanged:
    //       "ocx-shim: ocx not found (set OCX_BINARY_PIN or add ocx to PATH)"
    // - `pinned: Some(p)` (env defined)    → names `p`, and MUST NOT contain
    //       "add ocx to PATH" (the pin was the resolution path, not PATH).
    //
    // exit_code() stays 69 (EX_UNAVAILABLE) for both. These tests construct
    // the new variant shape so they FAIL TO COMPILE until the builder adds
    // the `pinned` field — that is the accepted red for a variant-shape
    // contract. Builder: implement `OcxNotFound { pinned: Option<String> }`.

    #[test]
    fn e5_ocx_not_found_unset_pin_keeps_path_hint_and_exits_69() {
        let err = ShimError::OcxNotFound { pinned: None };
        assert_eq!(err.exit_code(), 69, "E5 ocx not found → EX_UNAVAILABLE (69)");
        assert_eq!(
            err.stderr_message(),
            "ocx-shim: ocx not found (set OCX_BINARY_PIN or add ocx to PATH)",
            "unset OCX_BINARY_PIN → the original hint mentioning both OCX_BINARY_PIN and PATH"
        );
    }

    #[test]
    fn e5_ocx_not_found_pinned_names_program_and_drops_path_hint() {
        let err = ShimError::OcxNotFound {
            pinned: Some("C:\\tools\\ocx.exe".to_string()),
        };
        assert_eq!(
            err.exit_code(),
            69,
            "E5 still EX_UNAVAILABLE (69) when pinned-but-not-found"
        );
        let msg = err.stderr_message();
        assert!(
            msg.contains("C:\\tools\\ocx.exe"),
            "a defined-but-unresolvable OCX_BINARY_PIN must name the pinned program: {msg:?}"
        );
        assert!(
            !msg.contains("add ocx to PATH"),
            "when pinned, the stderr must NOT say `add ocx to PATH` (the pin, not PATH, was the resolution path): {msg:?}"
        );
        assert!(
            msg.starts_with("ocx-shim: "),
            "still a clean C-GOOD-ERR `ocx-shim:` line: {msg:?}"
        );
    }

    // ── derive_stem — strip exactly one trailing `.exe`, case-insensitive ──

    #[test]
    fn derive_stem_strips_single_trailing_exe() {
        let stem = derive_stem(OsStr::new("C:\\pkg\\entrypoints\\cmake.exe"))
            .expect("well-formed module path must yield a stem");
        assert_eq!(stem, "cmake", "exactly the final `.exe` is stripped");
    }

    #[test]
    fn derive_stem_keeps_interior_dot() {
        // Parity matrix row: `clang-format.exe` → `clang-format` (only the
        // final `.exe` is stripped, the interior `-`/`.` is preserved).
        let stem = derive_stem(OsStr::new("C:\\pkg\\entrypoints\\clang-format.exe"))
            .expect("well-formed module path must yield a stem");
        assert_eq!(stem, "clang-format", "only the final `.exe` is stripped");
    }

    #[test]
    fn derive_stem_is_case_insensitive_on_extension() {
        let stem = derive_stem(OsStr::new("C:\\pkg\\entrypoints\\cmake.EXE"))
            .expect("uppercase extension must still be recognised");
        assert_eq!(stem, "cmake", "`.EXE` stripped case-insensitively");
    }

    #[test]
    fn derive_stem_without_exe_is_unchanged() {
        let stem = derive_stem(OsStr::new("C:\\pkg\\entrypoints\\cmake"))
            .expect("a name with no `.exe` is still a valid stem");
        assert_eq!(stem, "cmake", "no trailing `.exe` → name returned unchanged");
    }

    #[test]
    fn derive_stem_strips_only_one_exe_suffix() {
        // `tool.exe.exe` → `tool.exe`: exactly ONE trailing `.exe` removed.
        let stem = derive_stem(OsStr::new("C:\\pkg\\entrypoints\\tool.exe.exe"))
            .expect("double-suffix is still a valid module path");
        assert_eq!(stem, "tool.exe", "strip exactly one trailing `.exe`, not all");
    }

    // ── parse_sidecar — `.shim` Sidecar Format Contract (ADR / sysdesign §6)

    #[test]
    fn parse_sidecar_accepts_trailing_lf() {
        let pkg_root = parse_sidecar(b"C:\\Users\\ci\\.ocx\\packages\\ocx.sh\\sha256\\ab\\cd\n")
            .expect("a single trailing LF is the canonical generator form");
        assert_eq!(
            pkg_root, "C:\\Users\\ci\\.ocx\\packages\\ocx.sh\\sha256\\ab\\cd",
            "the single trailing LF is stripped; body == pkg_root only"
        );
    }

    #[test]
    fn parse_sidecar_accepts_trailing_crlf() {
        let pkg_root = parse_sidecar(b"C:\\pkg\\root\r\n").expect("a trailing CRLF is tolerated (tolerant reader)");
        assert_eq!(pkg_root, "C:\\pkg\\root", "the trailing `\\r\\n` is stripped");
    }

    #[test]
    fn parse_sidecar_accepts_no_terminator() {
        let pkg_root = parse_sidecar(b"C:\\pkg\\root").expect("no terminator is tolerated (tolerant reader)");
        assert_eq!(pkg_root, "C:\\pkg\\root", "no terminator → body used verbatim");
    }

    #[test]
    fn parse_sidecar_rejects_empty_after_strip() {
        let err = parse_sidecar(b"\n").expect_err("empty body after stripping the LF is malformed");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "empty-after-strip → E2 MalformedSidecar, got {err:?}"
        );
        assert_eq!(err.exit_code(), 78, "E2 → exit 78");
    }

    #[test]
    fn parse_sidecar_rejects_truly_empty_input() {
        let err = parse_sidecar(b"").expect_err("a zero-byte sidecar is malformed");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "empty input → E2, got {err:?}"
        );
    }

    #[test]
    fn parse_sidecar_rejects_interior_newline() {
        let err = parse_sidecar(b"C:\\pkg\nroot\n").expect_err("an interior LF before the terminator is malformed");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "interior `\\n` → E2, got {err:?}"
        );
    }

    #[test]
    fn parse_sidecar_rejects_interior_carriage_return() {
        let err = parse_sidecar(b"C:\\pkg\rroot\n").expect_err("an interior CR before the terminator is malformed");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "interior `\\r` → E2, got {err:?}"
        );
    }

    #[test]
    fn parse_sidecar_rejects_embedded_nul() {
        let err = parse_sidecar(b"C:\\pkg\0root\n").expect_err("an embedded NUL byte is malformed");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "embedded `\\0` → E2, got {err:?}"
        );
    }

    #[test]
    fn parse_sidecar_rejects_invalid_utf8() {
        // 0xFF is never valid UTF-8.
        let err = parse_sidecar(b"C:\\\xff\\root\n").expect_err("non-UTF-8 bytes are malformed");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "invalid UTF-8 → E2, got {err:?}"
        );
    }

    #[test]
    fn parse_sidecar_rejects_non_absolute_path() {
        let err = parse_sidecar(b"relative\\pkg\\root\n").expect_err("a non-absolute pkg_root is malformed");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "non-absolute path → E2, got {err:?}"
        );
    }

    #[test]
    fn parse_sidecar_rejects_over_32_kib() {
        // 32 KiB + 1 byte → over the cap (defends against a corrupt/huge file).
        let mut raw = vec![b'C', b':', b'\\'];
        raw.resize(32 * 1024 + 1, b'a');
        let err = parse_sidecar(&raw).expect_err("a sidecar larger than 32 KiB is malformed");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "input > 32 KiB → E2, got {err:?}"
        );
    }

    // ── resolve_program — `OCX_BINARY_PIN` parity (plan Wire-ABI matrix) ────

    #[test]
    fn resolve_program_unset_pin_falls_back_to_ocx() {
        assert_eq!(
            resolve_program(None),
            "ocx",
            "OCX_BINARY_PIN unset → literal `ocx` (PATH lookup), parity with `.cmd` ELSE branch"
        );
    }

    #[test]
    fn resolve_program_set_nonempty_pin_uses_value() {
        assert_eq!(
            resolve_program(Some("C:\\tools\\ocx.exe")),
            "C:\\tools\\ocx.exe",
            "OCX_BINARY_PIN set non-empty → that value, parity with `.cmd` IF DEFINED branch"
        );
    }

    #[test]
    fn resolve_program_set_empty_pin_takes_pin_branch_not_ocx() {
        // Reconciled ABI decision (ADR §Error Taxonomy note E5/E6; plan
        // parity matrix "set empty → take pin branch"). The shim mirrors the
        // Windows `.cmd` `IF DEFINED OCX_BINARY_PIN` semantics: *defined at
        // all* (even empty) → pin branch. It must NOT fall back to `ocx`
        // (that is the Unix `${VAR:-ocx}` behaviour, deliberately out of
        // scope here).
        assert_eq!(
            resolve_program(Some("")),
            "",
            "OCX_BINARY_PIN defined-but-empty → pin branch (empty value), NOT `ocx` fallback"
        );
        assert_ne!(
            resolve_program(Some("")),
            "ocx",
            "empty pin must not collapse to the unset/`ocx` branch (parity-by-decision)"
        );
    }

    // ── build_child_command_line — SAFE CommandLineToArgvW wire assembler ──
    //
    // Wire ABI: `<program> launcher exec <pkg_root> -- <stem> <argv...>`.
    // SECURITY (B1/B2): `program`, `pkg_root`, and `stem` are ALL routed
    // through the `CommandLineToArgvW` quoter — they are quoted ONLY when they
    // contain whitespace/`"`/are empty, and an embedded `"` / trailing `\` is
    // neutralised. The old goldens locked the *unsafe* hand-quoted form (a
    // trailing `\` in `pkg_root` escaped the closing quote → CWE-88); these
    // assert the corrected SAFE form. The shim NEVER routes through cmd.exe.

    #[test]
    fn build_child_command_line_empty_argv() {
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "cmake", &[]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake",
            "no-whitespace program/pkg_root/stem are emitted unquoted (CommandLineToArgvW-correct); empty argv → nothing after the stem"
        );
    }

    #[test]
    fn build_child_command_line_pinned_program_quoted_safely() {
        let line = build_child_command_line(
            "C:\\tools\\ocx.exe",
            "C:\\pkg\\root",
            "cmake",
            &["--version".to_string()],
        );
        assert_eq!(
            line, "C:\\tools\\ocx.exe launcher exec C:\\pkg\\root -- cmake --version",
            "a pinned program with no whitespace is the unquoted leading token (it is ALSO passed via lpApplicationName in run())"
        );
    }

    #[test]
    fn build_child_command_line_pinned_program_with_spaces_is_quoted() {
        // Regression (B2): a pinned `C:\Program Files\…\ocx.cmd` MUST be
        // quoted as the leading token so the command line is well-formed.
        // (run() ALSO passes it via lpApplicationName so CreateProcessW does
        // no program parsing — CWE-428 — but the leading token must still be
        // valid for the literal-`ocx` PATH-search case.)
        let line = build_child_command_line(
            "C:\\Program Files\\ocx\\ocx.cmd",
            "C:\\pkg\\root",
            "cmake",
            &["--version".to_string()],
        );
        assert_eq!(
            line, "\"C:\\Program Files\\ocx\\ocx.cmd\" launcher exec C:\\pkg\\root -- cmake --version",
            "a spaced pinned program is wrapped in one quote pair (single leading token preserved)"
        );
    }

    #[test]
    fn build_child_command_line_pkg_root_with_trailing_backslash_is_safe() {
        // Regression (B1 / CWE-88): a `pkg_root` ending in `\` previously
        // escaped the hand-written closing quote, collapsing the argv
        // boundary. With the CommandLineToArgvW quoter and a no-whitespace
        // path it is emitted UNQUOTED (a trailing `\` is then literal — no
        // quote to escape), so the boundary is intact.
        let line = build_child_command_line("ocx", "C:\\pkg\\root\\", "cmake", &[]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root\\ -- cmake",
            "a no-whitespace pkg_root with a trailing `\\` is unquoted → the trailing backslash is literal, argv boundary intact (CWE-88 closed)"
        );
    }

    #[test]
    fn build_child_command_line_pkg_root_with_space_and_trailing_backslash_doubles() {
        // A spaced pkg_root with a trailing `\` is quoted; the trailing
        // backslash run is doubled before the closing quote so it does NOT
        // escape it (CommandLineToArgvW rule). Argv boundary stays intact.
        let line = build_child_command_line("ocx", "C:\\Program Files\\p\\", "cmake", &[]);
        assert_eq!(
            line, "ocx launcher exec \"C:\\Program Files\\p\\\\\" -- cmake",
            "a spaced pkg_root ending in `\\` → trailing backslashes doubled before the closing quote (no quote escape, CWE-88 closed)"
        );
    }

    #[test]
    fn build_child_command_line_pkg_root_with_embedded_quote_is_escaped() {
        // Regression (B1): an embedded `"` in pkg_root (the sidecar is NOT a
        // trust boundary — parse_sidecar tolerantly accepts it) is escaped as
        // `\"` and the token is quoted, never breaking the argv boundary.
        let line = build_child_command_line("ocx", "C:\\p\"q\\r", "cmake", &[]);
        assert_eq!(
            line, "ocx launcher exec \"C:\\p\\\"q\\r\" -- cmake",
            "an embedded `\"` in pkg_root is escaped as `\\\"` (CommandLineToArgvW), argv boundary intact"
        );
    }

    #[test]
    fn build_child_command_line_stem_with_embedded_quote_is_escaped() {
        // stem is derived at runtime from the on-disk filename — also not a
        // trust boundary; an embedded `"` must be neutralised the same way.
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "to\"ol", &[]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- \"to\\\"ol\"",
            "an embedded `\"` in stem is escaped + quoted (CommandLineToArgvW)"
        );
    }

    #[test]
    fn build_child_command_line_arg_with_spaces_is_quoted() {
        let line = build_child_command_line(
            "ocx",
            "C:\\pkg\\root",
            "cmake",
            &["-DCMAKE_INSTALL_PREFIX=C:\\Program Files\\x".to_string()],
        );
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake \"-DCMAKE_INSTALL_PREFIX=C:\\Program Files\\x\"",
            "an arg containing spaces is wrapped in one set of double quotes (single argument preserved)"
        );
    }

    #[test]
    fn build_child_command_line_ampersand_is_literal_not_cmd_metachar() {
        // BatBadBut regression: `& whoami` must be forwarded as ONE literal
        // argument, never interpreted by cmd.exe.
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "tool", &["& whoami".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- tool \"& whoami\"",
            "`& whoami` is a single quoted literal argument; never a cmd.exe metachar"
        );
    }

    #[test]
    fn build_child_command_line_embedded_double_quote_is_escaped() {
        // CommandLineToArgvW rule: an embedded `"` is backslash-escaped (`\"`)
        // and the argument is wrapped in quotes because it contains the quote.
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "tool", &["a\"b".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- tool \"a\\\"b\"",
            "an embedded double quote is escaped as `\\\"` per CommandLineToArgvW rules"
        );
    }

    #[test]
    fn build_child_command_line_percent_is_literal() {
        // `%` has no special meaning to CreateProcessW (no cmd.exe = no
        // environment-variable expansion); forwarded verbatim, not quoted
        // (it contains no whitespace/quote).
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "tool", &["%PATH%".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- tool %PATH%",
            "`%PATH%` forwarded verbatim — no cmd.exe variable expansion"
        );
    }

    #[test]
    fn build_child_command_line_unicode_is_verbatim() {
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "tool", &["café".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- tool café",
            "non-ASCII argv is forwarded verbatim (UTF-16 preserved end-to-end)"
        );
    }

    #[test]
    fn build_child_command_line_multiple_args_in_order() {
        let line = build_child_command_line(
            "ocx",
            "C:\\pkg\\root",
            "cmake",
            &[
                "--build".to_string(),
                ".".to_string(),
                "--target".to_string(),
                "all".to_string(),
            ],
        );
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake --build . --target all",
            "multiple forwarded args keep their order, no reordering or dropping"
        );
    }

    // ── Quoter — ALL ASCII control chars force quoting (amendment 3) ───────
    //
    // Design record "Review-Fix amendments" §3: `append_quoted_arg`
    // quote-forces on the EMPTY string OR any of space / tab / `"` / ANY
    // ASCII control byte (0x00–0x1F, incl. `\n` `\r` `\x0b` `\x0c`) so a
    // forwarded argv newline/CR cannot be mis-split by a generic consumer.
    // The current predicate only checks `' ' | '\t' | '"'`, so the `\n`,
    // `\r`, `\x0b`, `\x0c` cases are RED until the builder widens it.
    // Exercised via the public `build_child_command_line` (the quoter is
    // module-private). Assert the FULL command line, byte-exact.

    #[test]
    fn quoter_empty_forwarded_argv_element_is_emitted_as_empty_quotes() {
        // An empty argv element must round-trip as a real empty argument:
        // `""` (CommandLineToArgvW: empty token requires quotes).
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "cmake", &[String::new()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake \"\"",
            "an empty forwarded argv element must be emitted as `\"\"` (a real empty argument)"
        );
    }

    #[test]
    fn quoter_tab_arg_is_quoted() {
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "cmake", &["a\tb".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake \"a\tb\"",
            "a tab inside an arg forces quoting (whitespace splits argv otherwise)"
        );
    }

    #[test]
    fn quoter_newline_arg_is_quoted() {
        // RED against current code: the predicate does NOT include `\n`, so
        // the arg is currently emitted UNQUOTED and a generic consumer could
        // mis-split it. Amendment §3 requires it quoted.
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "cmake", &["a\nb".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake \"a\nb\"",
            "an embedded LF must force quoting (amendment §3: any ASCII control byte)"
        );
    }

    #[test]
    fn quoter_carriage_return_and_vtab_and_formfeed_args_are_quoted() {
        // RED against current code for `\r`, `\x0b`, `\x0c` (none in the
        // current `' '|'\t'|'"'` predicate). Each must force quoting.
        let cr = build_child_command_line("ocx", "C:\\pkg\\root", "t", &["a\rb".to_string()]);
        assert_eq!(
            cr, "ocx launcher exec C:\\pkg\\root -- t \"a\rb\"",
            "an embedded CR must force quoting (amendment §3)"
        );
        let vt = build_child_command_line("ocx", "C:\\pkg\\root", "t", &["a\u{0b}b".to_string()]);
        assert_eq!(
            vt, "ocx launcher exec C:\\pkg\\root -- t \"a\u{0b}b\"",
            "an embedded vertical-tab (0x0B) must force quoting (amendment §3)"
        );
        let ff = build_child_command_line("ocx", "C:\\pkg\\root", "t", &["a\u{0c}b".to_string()]);
        assert_eq!(
            ff, "ocx launcher exec C:\\pkg\\root -- t \"a\u{0c}b\"",
            "an embedded form-feed (0x0C) must force quoting (amendment §3)"
        );
    }

    #[test]
    fn quoter_backslash_run_not_before_quote_is_emitted_undoubled_inside_quotes() {
        // CommandLineToArgvW rule: backslashes are doubled ONLY when they
        // immediately precede a `"` (or the closing quote). An interior
        // backslash run NOT followed by a quote stays literal even when the
        // arg is quoted (here it is quoted because of the space).
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "cmake", &["a\\\\b c".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake \"a\\\\b c\"",
            "an interior backslash run not before a quote is emitted un-doubled inside the quotes"
        );
    }

    #[test]
    fn quoter_backslash_immediately_before_embedded_quote_is_doubled_then_escaped() {
        // `a\"b` → the single backslash precedes the embedded `"`, so it is
        // doubled (→ `\\`) and the quote escaped (→ `\"`): emitted as
        // `"a\\\"b"`. The arg is quoted because it contains a `"`.
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "cmake", &["a\\\"b".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake \"a\\\\\\\"b\"",
            "a backslash immediately before an embedded quote is doubled then the quote escaped (CommandLineToArgvW)"
        );
    }

    // ── parse_sidecar — boundary gaps (amendment, contract tightening) ─────
    //
    // Only ONE trailing terminator is stripped: a second trailing newline is
    // an interior newline → E2. A lone trailing CR with no LF is NOT a
    // terminator (only `\r\n` or `\n` are) so the CR is interior → E2. The
    // size cap is `> MAX_LEN`, so exactly `MAX_LEN` bytes is accepted.

    #[test]
    fn parse_sidecar_rejects_double_trailing_lf() {
        // `C:\p\r\n\n`: strip one `\n`, leaving `C:\p\r\n` whose final `\n`
        // is now an interior newline → E2 MalformedSidecar.
        let err = parse_sidecar(b"C:\\p\\r\n\n").expect_err("a second trailing LF is an interior newline");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "double trailing LF → E2 MalformedSidecar, got {err:?}"
        );
        assert_eq!(err.exit_code(), 78, "E2 → exit 78");
    }

    #[test]
    fn parse_sidecar_rejects_lone_trailing_cr_without_lf() {
        // `C:\p\r\r`: no `\r\n`/`\n` suffix, so NOTHING is stripped; the
        // trailing `\r` is then an interior CR → E2.
        let err = parse_sidecar(b"C:\\p\\r\r").expect_err("a lone trailing CR (no LF) is not a terminator");
        assert!(
            matches!(err, ShimError::MalformedSidecar { .. }),
            "lone trailing CR without LF → E2 (CR is interior), got {err:?}"
        );
    }

    #[test]
    fn parse_sidecar_accepts_exactly_32_kib() {
        // The cap is `len > MAX_LEN` (32 KiB). A path of EXACTLY 32 KiB is at
        // the boundary and must be accepted (the `>` is strict).
        const MAX_LEN: usize = 32 * 1024;
        let mut raw = vec![b'C', b':', b'\\'];
        raw.resize(MAX_LEN, b'a');
        assert_eq!(raw.len(), MAX_LEN, "fixture must be exactly MAX_LEN bytes");
        let pkg_root = parse_sidecar(&raw).expect("exactly MAX_LEN bytes is at the boundary and accepted");
        assert_eq!(
            pkg_root.len(),
            MAX_LEN,
            "no terminator present → the full MAX_LEN body is the pkg_root verbatim"
        );
    }

    // ── derive_stem — error branches (amendment, contract tightening) ──────

    #[test]
    fn derive_stem_dot_exe_only_is_self_path_failure() {
        // `.exe` → file name is `.exe`; stripping the trailing `.exe` leaves
        // an empty stem → E4 SelfPathFailure (cannot determine identity).
        let err = derive_stem(OsStr::new(".exe")).expect_err("a name that is only `.exe` has no stem");
        assert!(
            matches!(err, ShimError::SelfPathFailure),
            "`.exe` (empty stem after strip) → E4 SelfPathFailure, got {err:?}"
        );
        assert_eq!(err.exit_code(), 74, "E4 → exit 74");
    }

    #[test]
    fn derive_stem_trailing_separator_is_self_path_failure() {
        // `C:\pkg\` → empty final path component → E4 SelfPathFailure.
        let err = derive_stem(OsStr::new("C:\\pkg\\")).expect_err("a trailing-separator path has no file name");
        assert!(
            matches!(err, ShimError::SelfPathFailure),
            "trailing-separator path (empty file name) → E4 SelfPathFailure, got {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn derive_stem_non_utf8_os_str_is_self_path_failure() {
        // Host CI is Linux; build a non-UTF-8 `OsStr` via the unix
        // extension. `derive_stem` requires UTF-8 (`to_str()`), so a
        // non-UTF-8 module path → E4 SelfPathFailure.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let bad = OsStr::from_bytes(&[0x66, 0xff]); // 'f' + invalid byte 0xFF
        let err = derive_stem(bad).expect_err("a non-UTF-8 module path cannot yield a stem");
        assert!(
            matches!(err, ShimError::SelfPathFailure),
            "non-UTF-8 OsStr → E4 SelfPathFailure, got {err:?}"
        );
    }

    // ── use_std_handles — no-console std-handle gating (amendment 1) ───────
    //
    // Spec-to-builder: ADD a NEW pure helper to `core.rs`:
    //
    //   pub(super) fn use_std_handles(
    //       stdin_valid: bool, stdout_valid: bool, stderr_valid: bool,
    //   ) -> bool
    //
    // Returns `true` ONLY when ALL THREE are valid. The Win32 `run()` then
    // sets `STARTF_USESTDHANDLES` only when this returns `true`; a no-console
    // parent (NULL / INVALID_HANDLE_VALUE for any std handle) MUST NOT set
    // it (regression vs the removed `.cmd` path otherwise — design record
    // "Review-Fix amendments" §1, Codex#1). Pure → host-runnable on Linux.
    // Red-by-COMPILE until the builder adds the helper.

    #[test]
    fn use_std_handles_true_only_when_all_three_valid() {
        assert!(
            super::core::use_std_handles(true, true, true),
            "all three std handles valid → STARTF_USESTDHANDLES may be set"
        );
    }

    #[test]
    fn use_std_handles_false_when_any_single_handle_invalid() {
        assert!(
            !super::core::use_std_handles(false, true, true),
            "invalid stdin → must NOT set STARTF_USESTDHANDLES (no-console parent regression vs `.cmd`)"
        );
        assert!(
            !super::core::use_std_handles(true, false, true),
            "invalid stdout → must NOT set STARTF_USESTDHANDLES"
        );
        assert!(
            !super::core::use_std_handles(true, true, false),
            "invalid stderr → must NOT set STARTF_USESTDHANDLES"
        );
    }

    #[test]
    fn use_std_handles_false_when_no_console_at_all() {
        assert!(
            !super::core::use_std_handles(false, false, false),
            "a fully no-console parent (all handles NULL/INVALID) → must NOT set STARTF_USESTDHANDLES"
        );
    }

    // ── spawn_application_name — B2 / CWE-428 program-resolution policy ─────

    #[test]
    fn spawn_application_name_unset_pin_is_null_for_path_search() {
        // Unset OCX_BINARY_PIN → literal `ocx`; lpApplicationName MUST be NULL
        // so CreateProcessW performs the PATH/PATHEXT search (the only case
        // where command-line program search is legitimate).
        assert_eq!(
            super::core::spawn_application_name("ocx", false),
            None,
            "unset pin → lpApplicationName = NULL (literal `ocx` PATH search)"
        );
    }

    #[test]
    fn spawn_application_name_pinned_is_explicit_never_parsed() {
        // A pinned program (even a spaced `C:\Program Files\…`) MUST be passed
        // explicitly via lpApplicationName so CreateProcessW does NO
        // command-line program parsing (CWE-428: `C:\Program.exe` misresolve).
        assert_eq!(
            super::core::spawn_application_name("C:\\Program Files\\ocx\\ocx.cmd", true),
            Some("C:\\Program Files\\ocx\\ocx.cmd"),
            "pinned program → explicit lpApplicationName (no command-line program search, CWE-428 closed)"
        );
    }

    #[test]
    fn spawn_application_name_empty_pin_still_explicit() {
        // Reconciled `.cmd` IF DEFINED parity: defined-but-empty pin still
        // takes the pin branch and resolves explicitly (an empty
        // lpApplicationName then fails the spawn deterministically rather than
        // silently parsing the command line).
        assert_eq!(
            super::core::spawn_application_name("", true),
            Some(""),
            "defined-but-empty pin → still explicit lpApplicationName (parity-by-decision; deterministic fail, no silent cmdline parse)"
        );
    }

    // ── shim wire token is bound to the `.sh` body (2nd canary) ───────────
    //
    // The shim is the 2nd producer of the frozen `launcher exec` wire ABI
    // (`.sh` ⇄ shim; the `.cmd` producer was removed in the Axis C
    // cutover). This cross-producer canary fails if the shim's emitted
    // subcommand vocabulary drifts from the `.sh` launcher body in
    // `ocx_lib`'s `package_manager::launcher::body` — equal standing to the
    // existing `body.rs::tests` One-Way-Door goldens
    // (`subsystem-package-manager.md` canary rule). It does NOT depend on
    // ocx_lib (ocx_shim has no internal deps); instead it locks the literal
    // the shim emits AND restates the `.sh` body's literal so any change to
    // the wire vocabulary must touch BOTH this constant and `body.rs`.

    #[test]
    fn shim_wire_token_matches_sh_body() {
        // The literal the shim emits (single source: core::WIRE_SUBCOMMAND).
        assert_eq!(
            super::core::WIRE_SUBCOMMAND,
            "launcher exec",
            "the shim's wire subcommand token (any change must also update \
             `crates/ocx_lib/src/package_manager/launcher/body.rs` `.sh` \
             golden — the shim is the 2nd wire-ABI producer)"
        );
        // And it must appear, byte-for-byte, in the assembled child line in
        // the exact `launcher exec <pkg_root> --` shape the `.sh` body emits
        // (`ocx launcher exec '<pkg_root>' -- "$(basename "$0")" "$@"`).
        let line = build_child_command_line("ocx", "C:\\pkg\\root", "cmake", &[]);
        assert!(
            line.contains("launcher exec C:\\pkg\\root -- cmake"),
            "shim child line must carry the `launcher exec <pkg_root> --` wire \
             shape shared with the `.sh` body: {line}"
        );
    }
}
