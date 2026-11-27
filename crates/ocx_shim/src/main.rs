// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-shim`: native Windows launcher shim.
//!
//! One copy per entrypoint reads the `NAME.launch` record beside it and spawns `ocx` with the wire verb that record's
//! kind selects, via `CreateProcessW`, bypassing `cmd.exe` and its BatBadBut `%*` re-parse. Each failure is one `ocx-shim:` stderr line
//! and a sysexits-aligned exit code.
//! See `.claude/artifacts/adr_windows_exe_shim.md`, `system_design_windows_exe_shim.md`.

// The Win32 runtime is stubbed off Windows, and `cargo check` must stay green on the Linux CI host.
#![cfg_attr(not(windows), allow(dead_code))]

use std::process::exit;

// `target_abi = "llvm"` selects only the gnullvm targets; msvc/gnu get the probe from their own runtime.
#[cfg(all(target_os = "windows", target_env = "gnu", target_abi = "llvm"))]
mod chkstk;

/// Shim failure taxonomy (`adr_windows_exe_shim.md` § Error Taxonomy).
///
/// Job-object setup failure is deliberately no variant: the shim warns and spawns without the job.
#[derive(Debug)]
enum ShimError {
    /// `pkg_root` resolves outside the containment roots (defense in depth; `ocx launcher exec` holds the primary
    /// check). Exit 77 (`EX_NOPERM`).
    ContainmentViolation { path: String },
    /// `GetModuleFileNameW` failed or yielded no usable stem. Exit 74 (`EX_IOERR`).
    SelfPathFailure,
    /// The resolved absolute `ocx` is not there. Exit 69 (`EX_UNAVAILABLE`).
    ///
    /// A `PATH` walk that finds none is a 78 (`DirectRefused`), never this.
    OcxNotFound { program: String },
    /// `CreateProcessW` failed otherwise: exit 74, or 77 when `win32` is `ERROR_ACCESS_DENIED`.
    SpawnFailure { win32: u32, program: String },
    /// The launcher refused: its `.launch` record missing, unreadable or malformed, or no vendored root accepted. Exit 78.
    ///
    /// `reason` is the whole stderr line after the `ocx-shim:` prefix, so it names what the lookup tried.
    DirectRefused { reason: String },
    /// An argument cannot reach a batch target without the risk of command injection. Exit 64 (`EX_USAGE`).
    ///
    /// `reason` is the whole stderr line after the `ocx-shim:` prefix.
    DirectArgumentRefused { reason: String },
    /// The vendored tool could not be started. Exit 77 when permission was denied, else 74.
    #[cfg(unix)]
    DirectExecFailed {
        program: String,
        permission_denied: bool,
        reason: String,
    },
}

/// `ERROR_ACCESS_DENIED`; the non-Windows build mirrors the Win32 value so the exit-code mapping stays host-testable.
#[cfg(windows)]
const ERROR_ACCESS_DENIED_CODE: u32 = windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
#[cfg(not(windows))]
const ERROR_ACCESS_DENIED_CODE: u32 = 5;

/// `ERROR_NOT_SUPPORTED`, returned when `CreateProcessW` refuses the attribute list: the only code that permits the
/// degraded spawn retry.
#[cfg(windows)]
const ERROR_NOT_SUPPORTED_CODE: u32 = windows_sys::Win32::Foundation::ERROR_NOT_SUPPORTED;
#[cfg(not(windows))]
const ERROR_NOT_SUPPORTED_CODE: u32 = 50;

impl ShimError {
    /// Maps each failure to its exit code (ADR § Error Taxonomy).
    fn exit_code(&self) -> i32 {
        match self {
            ShimError::DirectRefused { .. } => 78,
            ShimError::DirectArgumentRefused { .. } => 64,
            #[cfg(unix)]
            ShimError::DirectExecFailed { permission_denied, .. } => {
                if *permission_denied {
                    77
                } else {
                    74
                }
            }
            ShimError::ContainmentViolation { .. } => 77,
            ShimError::SelfPathFailure => 74,
            ShimError::OcxNotFound { .. } => 69,
            ShimError::SpawnFailure { win32, .. } => {
                if *win32 == ERROR_ACCESS_DENIED_CODE {
                    77
                } else {
                    74
                }
            }
        }
    }

    /// One lowercase `ocx-shim:` stderr line, no trailing period; config errors append a recovery hint.
    fn stderr_message(&self) -> String {
        match self {
            ShimError::ContainmentViolation { path } => {
                format!("ocx-shim: package root outside OCX home: {path}")
            }
            ShimError::SelfPathFailure => "ocx-shim: cannot determine own path".to_string(),
            ShimError::OcxNotFound { program } => {
                format!(
                    "ocx-shim: pinned ocx not found: {} (OCX_BINARY_PIN points at a missing path)",
                    escape_controls(program)
                )
            }
            ShimError::SpawnFailure { win32, program } => {
                format!(
                    "ocx-shim: failed to start {}: win32 error {win32}",
                    escape_controls(program)
                )
            }
            ShimError::DirectRefused { reason } | ShimError::DirectArgumentRefused { reason } => {
                format!("ocx-shim: {}", escape_controls(reason))
            }
            #[cfg(unix)]
            ShimError::DirectExecFailed { program, reason, .. } => {
                format!("ocx-shim: failed to start {}: {reason}", escape_controls(program))
            }
        }
    }

    /// The clause a failed Windows spawn gains when a path-length limit is its plausible cause.
    ///
    /// An `ocx` that was not found carries no Win32 code; it is judged as `ERROR_PATH_NOT_FOUND`, the code a
    /// too-long path gives.
    fn length_limit_hint(&self, working_directory_units: usize) -> Option<String> {
        match self {
            ShimError::SpawnFailure { win32, program } => length_limit_hint(*win32, working_directory_units, program),
            ShimError::OcxNotFound { program } => length_limit_hint(3, working_directory_units, program),
            _ => None,
        }
    }
}

/// `text` with each control character escaped as `{:?}` writes it, so a program path cannot drive the terminal.
fn escape_controls(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            escaped.extend(c.escape_debug());
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// The clause naming a path-length limit as the plausible cause of Win32 error `win32` starting `program`.
///
/// `working_directory_units` is the inherited working directory's length in UTF-16 units.
fn length_limit_hint(win32: u32, working_directory_units: usize, program: &str) -> Option<String> {
    use ocx_env_apply::windows_path::{MAX_PATH, extended_length_path};

    // No prefix lifts this one: `CreateProcessW` refuses a working directory of MAX_PATH or more.
    if working_directory_units >= MAX_PATH && matches!(win32, 3 | 87 | 206 | 267) {
        return Some(format!(
            "the working directory is {working_directory_units} characters long and Windows starts no program \
             from one of {MAX_PATH} or more; start it from a shorter directory"
        ));
    }
    let program_units = program.encode_utf16().count();
    if program_units < MAX_PATH || !matches!(win32, 2 | 3 | 206) {
        return None;
    }
    let why = if ocx_env_apply::launch::is_batch_target(program) {
        "a batch file runs through cmd.exe, which has the same limit"
    } else if extended_length_path(program).is_none() {
        "a relative program path cannot take the \\\\?\\ prefix"
    } else {
        return None;
    };
    Some(format!(
        "the program path is {program_units} characters long, past the {MAX_PATH} Windows allows, and {why}; \
         move the directory the tree sits in to a shorter path"
    ))
}

/// The inherited working directory's length in UTF-16 units; 0 when it cannot be read.
#[cfg(windows)]
fn working_directory_units() -> usize {
    use std::os::windows::ffi::OsStrExt;
    std::env::current_dir().map_or(0, |dir| dir.as_os_str().encode_wide().count())
}

mod core;
mod direct;

/// Entry point; always diverges via [`std::process::exit`].
fn main() -> ! {
    // Before any other Win32 call, or a DLL planted beside the shim can load ahead of the System32 copy.
    #[cfg(windows)]
    harden_dll_search_path();

    match run() {
        Ok(code) => exit(code),
        Err(err) => {
            #[cfg_attr(
                not(windows),
                expect(unused_mut, reason = "only a Windows spawn gains a length hint")
            )]
            let mut message = err.stderr_message();
            #[cfg(windows)]
            if let Some(hint) = err.length_limit_hint(working_directory_units()) {
                message.push_str("; ");
                message.push_str(&hint);
            }
            eprintln!("{message}");
            exit(err.exit_code())
        }
    }
}

/// Restricts the DLL search path to System32 so a DLL planted beside the shim cannot load ahead of the system copy.
///
/// No `SetDllDirectoryW(NULL)` fallback: it leaves the application directory, the planted-DLL vector, searched.
#[cfg(windows)]
fn harden_dll_search_path() {
    use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};

    // SAFETY: takes one integer flag and no pointers. A zero return (unreachable on supported Windows) leaves the
    // default search order, so the result is ignored.
    let _ = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
}

/// Shim runtime (`adr_windows_exe_shim.md` § Contract 1).
#[cfg(windows)]
fn run() -> Result<i32, ShimError> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;

    // Grow the buffer until the length is below it: a fixed buffer silently truncates the path and picks the wrong
    // sidecar. `written == cap` means truncated even without `ERROR_INSUFFICIENT_BUFFER` (pre-Vista), so it grows too.
    let module_path = {
        let mut cap: usize = 512;
        loop {
            let mut buf = vec![0u16; cap];
            // SAFETY: `buf` holds `cap` u16s and `cap` is passed as `nsize`.
            let written = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), cap as u32) };
            if written == 0 {
                return Err(ShimError::SelfPathFailure);
            }
            let written = written as usize;
            if written < cap {
                buf.truncate(written);
                break std::ffi::OsString::from_wide(&buf);
            }
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
    let started = module_path.to_str().ok_or(ShimError::SelfPathFailure)?;
    direct::run_windows(started)
}

/// Starts `ocx` for a home-kind record: `home` picks the wire verb, `stem` is the tool name passed after `--`, `image`
/// is the launcher's image path.
#[cfg(windows)]
fn run_home(home: &core::HomeLaunch, image: &str, stem: &str) -> Result<i32, ShimError> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};

    let start = direct::prepare_home(home, image, true)?;
    let mut caller_args: Vec<String> = Vec::new();
    for argument in std::env::args_os().skip(1) {
        caller_args.push(argument.into_string().map_err(|_| ShimError::DirectArgumentRefused {
            reason: format!("an argument for {} is not valid Unicode", start.program),
        })?);
    }
    let command_line = core::build_child_command_line(&start.program, home, stem, &caller_args);
    let block = direct::environment_block(&start.environment);

    // The program is absolute and passed as `lpApplicationName`, so `CreateProcessW` neither parses it out of the
    // command line (CWE-428: `C:\Program Files\…` → `C:\Program.exe`) nor searches any directory for it (CWE-427).
    spawn_child(&start.program, &command_line, Some(&block)).map_err(|last| match last {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => ShimError::OcxNotFound {
            program: start.program.clone(),
        },
        other => ShimError::SpawnFailure {
            win32: other,
            program: start.program.clone(),
        },
    })
}

/// An anonymous job object that kills its processes when the last handle closes; null when setup failed.
///
/// Best-effort: on failure the shim warns and spawns without a job.
#[cfg(windows)]
fn create_kill_on_close_job() -> windows_sys::Win32::Foundation::HANDLE {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    // SAFETY: NULL security attrs + NULL name = an anonymous job object.
    let job: HANDLE = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        // SAFETY: no pointers; reads thread-local last-error.
        let last = unsafe { GetLastError() };
        eprintln!("ocx-shim: job object setup failed: {last}");
        return std::ptr::null_mut();
    }
    // SAFETY: all-zero is a valid `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `info` is initialised and the size argument matches its type.
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
        // SAFETY: `job` is a valid handle, not used again on this path.
        unsafe { CloseHandle(job) };
        return std::ptr::null_mut();
    }
    job
}

/// Starts the child with `CreateProcessW` inside a kill-on-close job and returns its exit code, or the Win32 error of
/// the failed spawn.
///
/// `application_name` is required: a NULL one makes `CreateProcessW` search the application directory and the working
/// directory for the program named on the command line (CWE-427). `environment` is a complete
/// `CREATE_UNICODE_ENVIRONMENT` block; `None` inherits this process's.
#[cfg(windows)]
fn spawn_child(application_name: &str, command_line: &str, environment: Option<&[u16]>) -> Result<i32, u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Threading::{
        CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, STARTUPINFOW, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    let mut command_line_w: Vec<u16> = std::ffi::OsStr::new(command_line)
        .encode_wide()
        .chain(Some(0))
        .collect();
    // A plain path past MAX_PATH fails with ERROR_PATH_NOT_FOUND (3); its `\\?\` form starts. The command line keeps
    // the path as given, so the child's `argv[0]` is unchanged.
    let app_name_w: Vec<u16> = {
        let name = ocx_env_apply::windows_path::extended_length_path(application_name);
        std::ffi::OsStr::new(name.as_deref().unwrap_or(application_name))
            .encode_wide()
            .chain(Some(0))
            .collect()
    };
    let app_name_ptr = app_name_w.as_ptr();
    let environment_ptr = environment.map_or(std::ptr::null(), |block| block.as_ptr().cast());
    let environment_flag = if environment.is_some() {
        CREATE_UNICODE_ENVIRONMENT
    } else {
        0
    };

    // Before `CreateProcessW`: the child is born running, and a Ctrl+C before install kills the shim before it can
    // wait, losing the exit code and job cleanup.
    install_console_ctrl_handler();

    // Created before `CreateProcessW` so the child is born inside it via `JOB_LIST`, with no suspend-assign-resume
    // race.
    let job = create_kill_on_close_job();

    // `bInheritHandles=TRUE` alone leaks every inheritable handle (CWE-403): `HANDLE_LIST` whitelists the std handles,
    // and `JOB_LIST` assigns the job at creation.
    // SAFETY: `GetStdHandle` returns a process-owned pseudo/real handle (or INVALID_HANDLE_VALUE); no pointers.
    let h_in: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let h_out: HANDLE = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let h_err: HANDLE = unsafe { GetStdHandle(STD_ERROR_HANDLE) };

    // A no-console parent has invalid std handles and the child must still launch, so only valid ones are listed.
    let valid = |h: HANDLE| !h.is_null() && h != INVALID_HANDLE_VALUE;
    let wire_std_handles = core::use_std_handles(valid(h_in), valid(h_out), valid(h_err));

    // De-duplicated: stdin and stdout are often one handle, and `HANDLE_LIST` must hold no duplicates.
    let mut unique_handles: Vec<HANDLE> = Vec::with_capacity(3);
    for h in [h_in, h_out, h_err] {
        if !valid(h) {
            continue;
        }
        if !unique_handles.contains(&h) {
            unique_handles.push(h);
        }
    }

    // `UpdateProcThreadAttribute` stores this pointer, not the value: it must outlive both `CreateProcessW` calls, or
    // the child is born outside the job (or the spawn fails on a stale handle).
    let job_handle = [job];

    // SAFETY: all-zero is the documented initial state of `STARTUPINFOEXW`.
    let mut startup_ex: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup_ex.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    if wire_std_handles {
        startup_ex.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup_ex.StartupInfo.hStdInput = h_in;
        startup_ex.StartupInfo.hStdOutput = h_out;
        startup_ex.StartupInfo.hStdError = h_err;
    }
    // SAFETY: all-zero is a valid `PROCESS_INFORMATION`, which `CreateProcessW` fills in.
    let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let want_handle_list = !unique_handles.is_empty();
    let want_job_list = !job.is_null();
    let attr_count = want_handle_list as u32 + want_job_list as u32;

    // `attr_list_buf` backs the list and must outlive `CreateProcessW`, which reads it; every path deletes it once.
    let mut attr_list_buf: Vec<u8> = Vec::new();
    let mut attr_list_ptr: LPPROC_THREAD_ATTRIBUTE_LIST = std::ptr::null_mut();
    let mut extended_present: u32 = 0;

    if attr_count > 0 {
        // SAFETY: a NULL list with a `usize` out-pointer is the documented sizing call.
        let mut size: usize = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), attr_count, 0, &mut size);
        }
        if size == 0 {
            eprintln!("ocx-shim: proc-thread attribute list sizing failed; spawning without handle/job scoping");
        } else {
            attr_list_buf = vec![0u8; size];
            attr_list_ptr = attr_list_buf.as_mut_ptr().cast();
            // SAFETY: `attr_list_ptr` is a `size`-byte buffer, sized by the call above for `attr_count` attributes.
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
                    // SAFETY: the list is initialised for `attr_count` entries, and `unique_handles` outlives
                    // `CreateProcessW`.
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
                    // SAFETY: the list is initialised for `attr_count` entries, and the function-scoped `job_handle`
                    // outlives `CreateProcessW`.
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
                    // SAFETY: `attr_list_ptr` was successfully initialised.
                    unsafe { DeleteProcThreadAttributeList(attr_list_ptr) };
                    attr_list_ptr = std::ptr::null_mut();
                    attr_list_buf = Vec::new();
                }
            }
        }
    }

    // SAFETY: `app_name_ptr` is a NUL-terminated buffer, `command_line_w` is mutable and NUL-terminated, and
    // `startup_ex`'s attribute list is backed by `attr_list_buf`, `unique_handles` and `job_handle`; all outlive it.
    let mut created = unsafe {
        CreateProcessW(
            app_name_ptr,
            command_line_w.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // bInheritHandles = TRUE (required for the whitelisted set)
            extended_present | environment_flag,
            environment_ptr,
            std::ptr::null(),
            std::ptr::addr_of!(startup_ex.StartupInfo),
            &mut process_info,
        )
    };

    let delete_attr_list = |ptr: LPPROC_THREAD_ATTRIBUTE_LIST| {
        if !ptr.is_null() {
            // SAFETY: `ptr` came from a successful InitializeProcThreadAttributeList and is deleted exactly once.
            unsafe { DeleteProcThreadAttributeList(ptr) };
        }
    };

    // Some hosts refuse the attribute list itself with ERROR_NOT_SUPPORTED (seen on GHA windows runners with piped
    // stdio): retry once plain, logged.
    // Only that code: any other would surrender handle and job scoping for a failure the list did not cause.
    // SAFETY: no pointers; reads thread-local last-error.
    let extended_error = if created == 0 { unsafe { GetLastError() } } else { 0 };
    if created == 0 && extended_present != 0 && extended_error == ERROR_NOT_SUPPORTED_CODE {
        let last = extended_error;
        eprintln!("ocx-shim: extended spawn failed: win32 error {last}; retrying without handle/job scoping");
        delete_attr_list(attr_list_ptr);
        attr_list_ptr = std::ptr::null_mut();
        attr_list_buf.clear();
        startup_ex.lpAttributeList = std::ptr::null_mut();
        // `cb` must match the `STARTUPINFOW` now passed, or a host that validates it fails the retry a second way.
        startup_ex.StartupInfo.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        // The failed call may have modified the command-line buffer; rebuild it.
        command_line_w = std::ffi::OsStr::new(&command_line)
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: as for the extended call, without the attribute list.
        created = unsafe {
            CreateProcessW(
                app_name_ptr,
                command_line_w.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                environment_flag,
                environment_ptr,
                std::ptr::null(),
                std::ptr::addr_of!(startup_ex.StartupInfo),
                &mut process_info,
            )
        };
    }

    if created == 0 {
        // SAFETY: no pointers; reads thread-local last-error.
        let last = unsafe { GetLastError() };
        delete_attr_list(attr_list_ptr);
        drop(attr_list_buf);
        if !job.is_null() {
            // SAFETY: valid job handle, not used again.
            unsafe { CloseHandle(job) };
        }
        return Err(last);
    }
    let child_process: HANDLE = process_info.hProcess;
    let child_thread: HANDLE = process_info.hThread;

    delete_attr_list(attr_list_ptr);
    drop(attr_list_buf);

    // SAFETY: `child_process` is a valid process handle.
    let wait = unsafe { WaitForSingleObject(child_process, INFINITE) };
    let mut exit_code: u32 = 1;
    if wait != WAIT_FAILED {
        // SAFETY: valid process handle + valid out-pointer to a local u32.
        unsafe {
            GetExitCodeProcess(child_process, &mut exit_code);
        }
    }

    // SAFETY: each handle came from a successful Win32 call and is not used again.
    unsafe {
        CloseHandle(child_thread);
        CloseHandle(child_process);
        if !job.is_null() && job != INVALID_HANDLE_VALUE {
            CloseHandle(job);
        }
    }

    // Full i32 passthrough, as `child_process::exit_code_from_status` does on Windows: never remap or truncate.
    Ok(exit_code as i32)
}

/// Installs a no-op console control handler so Ctrl+C / Ctrl+Break leave the shim alive to forward the exit code of
/// the child, which shares the console and handles the signal itself.
///
/// No `CREATE_NEW_PROCESS_GROUP`: it disables Ctrl+C in the child.
#[cfg(windows)]
fn install_console_ctrl_handler() {
    use windows_sys::Win32::Foundation::TRUE;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::core::BOOL;

    unsafe extern "system" fn handler(_ctrl_type: u32) -> BOOL {
        // TRUE suppresses the default terminate for the shim only; the child receives the same signal.
        TRUE
    }

    // SAFETY: `handler` has the `PHANDLER_ROUTINE` signature; failure only degrades Ctrl+C handling.
    unsafe {
        SetConsoleCtrlHandler(Some(handler), TRUE);
    }
}

/// The Unix launcher runs the vendored (`.launch`) kind alone.
#[cfg(unix)]
fn run() -> Result<i32, ShimError> {
    direct::run()
}

// ── Specification tests ─────────────────────────────────────────────────────
// Host-runnable on the Linux CI, since the Win32 syscalls are split out of `core::*`. They pin the error taxonomy,
// the wire-ABI parity matrix and the sidecar format contract. Each test spells out its own inputs and expected
// bytes/exit code rather than sharing builders, so a failure names the clause that regressed.
#[cfg(test)]
mod tests {
    use super::ShimError;
    use super::core::{HomeLaunch, ToolchainHome, build_child_command_line, derive_stem};
    use std::ffi::OsStr;

    /// A `package` record carrying `value` as its package root — the
    /// input every `build_child_command_line` row below assembles from.
    fn root(value: &str) -> HomeLaunch {
        HomeLaunch::PackageRoot(value.to_string())
    }

    /// A `deferred` record carrying `value` as its pinned identifier.
    fn shim_ref(value: &str) -> HomeLaunch {
        HomeLaunch::PinnedIdentifier(value.to_string())
    }

    /// A `toolchain` record naming a project home at `value`, with no
    /// baked `ocx`.
    fn exec_project(value: &str) -> HomeLaunch {
        HomeLaunch::ToolchainHome(ToolchainHome::Project(value.to_string()), None)
    }

    /// The same, carrying the baked absolute `ocx`.
    fn exec_project_baked(value: &str, ocx: &str) -> HomeLaunch {
        HomeLaunch::ToolchainHome(ToolchainHome::Project(value.to_string()), Some(ocx.to_string()))
    }

    /// A `toolchain` record naming the global home.
    fn exec_global() -> HomeLaunch {
        HomeLaunch::ToolchainHome(ToolchainHome::Global, None)
    }

    // ── ShimError → exit code mapping (ADR §Error Taxonomy table) ───────────
    //
    // E5/E6 cannot run host-side (they need a real `CreateProcessW` failure /
    // Win32 mock — acceptance-level), but the `exit_code()` mapping itself is
    // pure and is the unit-testable contract surface. These tests encode the
    // E1/E2→78, E3→77, E4→74, E5→69, E6→74, E6+ACCESS_DENIED→77 taxonomy.

    fn spawn_failure(win32: u32, program: &str) -> ShimError {
        ShimError::SpawnFailure {
            win32,
            program: program.to_string(),
        }
    }

    /// `C:\` plus `units - 3` characters: a drive path exactly `units` UTF-16 units long.
    fn drive_path(units: usize, suffix: &str) -> String {
        format!("C:\\{}{suffix}", "a".repeat(units - 3 - suffix.len()))
    }

    /// A long-path cause gets one clause naming the length, the limit and what to shorten; anything else gets none.
    #[test]
    fn length_limit_hint_names_the_working_directory_or_the_program_only_when_a_limit_is_the_cause() {
        let short_exe = "C:\\v\\bin\\tool.exe";
        for code in [3, 87, 206, 267] {
            let hint = spawn_failure(code, short_exe).length_limit_hint(260);
            let hint = hint.unwrap_or_else(|| panic!("error {code} with a 260-unit working directory has no hint"));
            assert!(
                hint.contains("working directory is 260") && hint.contains("260 or more"),
                "{hint}"
            );
        }
        assert_eq!(
            spawn_failure(267, short_exe).length_limit_hint(259),
            None,
            "under the limit: no hint"
        );
        assert_eq!(
            spawn_failure(5, short_exe).length_limit_hint(300),
            None,
            "access denied is not a length error"
        );

        let batch = drive_path(260, "\\run.cmd");
        let hint = spawn_failure(3, &batch)
            .length_limit_hint(10)
            .expect("a long batch path");
        assert!(
            hint.contains("program path is 260") && hint.contains("cmd.exe"),
            "{hint}"
        );
        let relative = format!("bin\\{}", "a".repeat(260));
        let hint = spawn_failure(206, &relative)
            .length_limit_hint(10)
            .expect("a long relative path");
        assert!(hint.contains("cannot take the \\\\?\\ prefix"), "{hint}");
        let pinned = ShimError::OcxNotFound { program: batch.clone() };
        assert!(
            pinned.length_limit_hint(10).is_some(),
            "a pinned ocx is judged as error 3"
        );

        // An absolute .exe past the limit takes the prefix, so its failure is not a length error.
        assert_eq!(
            spawn_failure(3, &drive_path(300, "\\tool.exe")).length_limit_hint(10),
            None
        );
        assert_eq!(
            spawn_failure(3, &drive_path(259, "\\run.cmd")).length_limit_hint(10),
            None
        );
        // Code 2 is a program-path code only; with a short working directory no code gives a clause.
        assert!(
            spawn_failure(2, &batch).length_limit_hint(10).is_some(),
            "error 2 on a long batch path"
        );
        assert_eq!(
            spawn_failure(2, short_exe).length_limit_hint(300),
            None,
            "error 2 is never a working-directory code"
        );
        for code in [5, 87] {
            assert_eq!(
                spawn_failure(code, &batch).length_limit_hint(10),
                None,
                "error {code} with a short working directory"
            );
        }
        let not_a_spawn = ShimError::DirectRefused { reason: batch };
        assert_eq!(not_a_spawn.length_limit_hint(300), None);
    }

    /// A refusal names paths from `argv[0]`, the working directory and runfiles manifests, so it cannot drive the
    /// terminal either.
    #[test]
    fn a_refusal_reaches_stderr_with_its_control_characters_escaped() {
        let refused = ShimError::DirectRefused {
            reason: "started path /v/to\u{1b}[2Jol is not the running image".to_string(),
        };
        assert_eq!(
            refused.stderr_message(),
            "ocx-shim: started path /v/to\\u{1b}[2Jol is not the running image"
        );
        let argument = ShimError::DirectArgumentRefused {
            reason: "argument 1 for C:\\t\r.cmd holds a line break".to_string(),
        };
        assert!(
            !argument.stderr_message().contains('\r'),
            "{:?}",
            argument.stderr_message()
        );
    }

    #[test]
    fn a_program_path_reaches_stderr_with_its_control_characters_escaped() {
        let err = spawn_failure(2, "C:\\v\\to\u{1b}[31mol.exe");
        assert_eq!(
            err.stderr_message(),
            "ocx-shim: failed to start C:\\v\\to\\u{1b}[31mol.exe: win32 error 2"
        );
        let pinned = ShimError::OcxNotFound {
            program: "C:\\o\ncx.exe".to_string(),
        };
        assert!(
            pinned.stderr_message().contains("C:\\o\\ncx.exe"),
            "{}",
            pinned.stderr_message()
        );
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
        assert_eq!(
            ShimError::OcxNotFound {
                program: "C:\\ocx.exe".to_string()
            }
            .exit_code(),
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

    // ── E5 OcxNotFound — the stderr line names the resolved program ─────────

    #[test]
    fn e5_ocx_not_found_pinned_names_program_and_drops_path_hint() {
        let err = ShimError::OcxNotFound {
            program: "C:\\tools\\ocx.exe".to_string(),
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

    /// The two positional grammars carry no baked program and never could —
    /// `HomeLaunch::baked_ocx` is `None` for both by construction, which is what
    /// keeps this fix scoped to the one sidecar that renders beside an `ocx`.
    #[test]
    fn only_the_exec_sidecar_can_carry_a_baked_ocx() {
        assert_eq!(root("C:\\pkg\\root").baked_ocx(), None);
        assert_eq!(shim_ref("ocx.sh/x@sha256:00").baked_ocx(), None);
        assert_eq!(exec_project("C:\\w\\proj").baked_ocx(), None, "the one-line form");
        assert_eq!(
            exec_project_baked("C:\\w\\proj", "C:\\baked\\ocx.exe").baked_ocx(),
            Some("C:\\baked\\ocx.exe"),
            "and the two-line form reports it — without this row the three above pass \
             against a `baked_ocx` that answers `None` unconditionally"
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
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "cmake", &[]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake",
            "no-whitespace program/pkg_root/stem are emitted unquoted (CommandLineToArgvW-correct); empty argv → nothing after the stem"
        );
    }

    #[test]
    fn build_child_command_line_pinned_program_quoted_safely() {
        let line = build_child_command_line(
            "C:\\tools\\ocx.exe",
            &root("C:\\pkg\\root"),
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
            &root("C:\\pkg\\root"),
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
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root\\"), "cmake", &[]);
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
        let line = build_child_command_line("ocx", &root("C:\\Program Files\\p\\"), "cmake", &[]);
        assert_eq!(
            line, "ocx launcher exec \"C:\\Program Files\\p\\\\\" -- cmake",
            "a spaced pkg_root ending in `\\` → trailing backslashes doubled before the closing quote (no quote escape, CWE-88 closed)"
        );
    }

    #[test]
    fn build_child_command_line_pkg_root_with_embedded_quote_is_escaped() {
        // Regression (B1): an embedded `"` in pkg_root (the sidecar is NOT a
        // trust boundary — the record's decoder accepts it) is escaped as
        // `\"` and the token is quoted, never breaking the argv boundary.
        let line = build_child_command_line("ocx", &root("C:\\p\"q\\r"), "cmake", &[]);
        assert_eq!(
            line, "ocx launcher exec \"C:\\p\\\"q\\r\" -- cmake",
            "an embedded `\"` in pkg_root is escaped as `\\\"` (CommandLineToArgvW), argv boundary intact"
        );
    }

    #[test]
    fn build_child_command_line_stem_with_embedded_quote_is_escaped() {
        // stem is derived at runtime from the on-disk filename — also not a
        // trust boundary; an embedded `"` must be neutralised the same way.
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "to\"ol", &[]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- \"to\\\"ol\"",
            "an embedded `\"` in stem is escaped + quoted (CommandLineToArgvW)"
        );
    }

    #[test]
    fn build_child_command_line_arg_with_spaces_is_quoted() {
        let line = build_child_command_line(
            "ocx",
            &root("C:\\pkg\\root"),
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
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "tool", &["& whoami".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- tool \"& whoami\"",
            "`& whoami` is a single quoted literal argument; never a cmd.exe metachar"
        );
    }

    #[test]
    fn build_child_command_line_embedded_double_quote_is_escaped() {
        // CommandLineToArgvW rule: an embedded `"` is backslash-escaped (`\"`)
        // and the argument is wrapped in quotes because it contains the quote.
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "tool", &["a\"b".to_string()]);
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
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "tool", &["%PATH%".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- tool %PATH%",
            "`%PATH%` forwarded verbatim — no cmd.exe variable expansion"
        );
    }

    #[test]
    fn build_child_command_line_unicode_is_verbatim() {
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "tool", &["café".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- tool café",
            "non-ASCII argv is forwarded verbatim (UTF-16 preserved end-to-end)"
        );
    }

    #[test]
    fn build_child_command_line_multiple_args_in_order() {
        let line = build_child_command_line(
            "ocx",
            &root("C:\\pkg\\root"),
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
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "cmake", &[String::new()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake \"\"",
            "an empty forwarded argv element must be emitted as `\"\"` (a real empty argument)"
        );
    }

    #[test]
    fn quoter_tab_arg_is_quoted() {
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "cmake", &["a\tb".to_string()]);
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
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "cmake", &["a\nb".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake \"a\nb\"",
            "an embedded LF must force quoting (amendment §3: any ASCII control byte)"
        );
    }

    #[test]
    fn quoter_carriage_return_and_vtab_and_formfeed_args_are_quoted() {
        // RED against current code for `\r`, `\x0b`, `\x0c` (none in the
        // current `' '|'\t'|'"'` predicate). Each must force quoting.
        let cr = build_child_command_line("ocx", &root("C:\\pkg\\root"), "t", &["a\rb".to_string()]);
        assert_eq!(
            cr, "ocx launcher exec C:\\pkg\\root -- t \"a\rb\"",
            "an embedded CR must force quoting (amendment §3)"
        );
        let vt = build_child_command_line("ocx", &root("C:\\pkg\\root"), "t", &["a\u{0b}b".to_string()]);
        assert_eq!(
            vt, "ocx launcher exec C:\\pkg\\root -- t \"a\u{0b}b\"",
            "an embedded vertical-tab (0x0B) must force quoting (amendment §3)"
        );
        let ff = build_child_command_line("ocx", &root("C:\\pkg\\root"), "t", &["a\u{0c}b".to_string()]);
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
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "cmake", &["a\\\\b c".to_string()]);
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
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "cmake", &["a\\\"b".to_string()]);
        assert_eq!(
            line, "ocx launcher exec C:\\pkg\\root -- cmake \"a\\\\\\\"b\"",
            "a backslash immediately before an embedded quote is doubled then the quote escaped (CommandLineToArgvW)"
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
             `crates/ocx_package_manager/src/launcher/body.rs` `.sh` \
             golden — the shim is the 2nd wire-ABI producer)"
        );
        // And it must appear, byte-for-byte, in the assembled child line in
        // the exact `launcher exec <pkg_root> --` shape the `.sh` body emits
        // (`ocx launcher exec '<pkg_root>' -- "${0##*/}" "$@"`).
        let line = build_child_command_line("ocx", &root("C:\\pkg\\root"), "cmake", &[]);
        assert!(
            line.contains("launcher exec C:\\pkg\\root -- cmake"),
            "shim child line must carry the `launcher exec <pkg_root> --` wire \
             shape shared with the `.sh` body: {line}"
        );
    }

    /// The SECOND wire verb's half of the same paired golden (C-018).
    ///
    /// `launcher shim` has exactly two producers — `ocx_lib`'s `.sh` shim body
    /// (`package_manager::launcher::body::unix_shim_body`) and this crate's
    /// [`super::core::WIRE_SUBCOMMAND_SHIM`] — and `ocx_lib` cannot depend on a
    /// binary crate, so nothing the compiler sees binds them. `body.rs`'s
    /// `launcher_shim_wire_token_is_bound_to_shim_producer` restates the token
    /// from that side; this restates it from here. A verb added on one side
    /// only drifts silently and forever, which is why this lands in the same
    /// commit as the token.
    #[test]
    fn shim_ref_wire_token_matches_sh_shim_body() {
        assert_eq!(
            super::core::WIRE_SUBCOMMAND_SHIM,
            "launcher shim",
            "the shim's wire subcommand token for a DEFERRED tool (any change \
             must also update `crates/ocx_package_manager/src/launcher/body.rs` \
             `unix_shim_body` — the native shim is the 2nd producer of this verb too)"
        );
        // And it must appear byte-for-byte in the assembled child line, in the
        // `launcher shim <pinned-id> --` shape the `.sh` shim body emits
        // (`ocx launcher shim '<pinned-id>' -- "${0##*/}" "$@"`).
        let pinned = "ocx.sh/tool/cmake:3.28@sha256:0000000000000000000000000000000000000000000000000000000000000001";
        let line = build_child_command_line("ocx", &shim_ref(pinned), "cmake", &[]);
        assert!(
            line.contains(&format!("launcher shim {pinned} -- cmake")),
            "shim child line must carry the `launcher shim <pinned-id> --` wire \
             shape shared with the `.sh` shim body: {line}"
        );
    }

    /// The drift the two paired goldens cannot catch on their own: a sidecar
    /// dispatched under the other sidecar's verb. Both are plausible wire
    /// tokens and both canaries above stay green, but `launcher exec` assumes
    /// the package already exists while `launcher shim` assumes it does not —
    /// so a crossed pair either fails on a missing package root or downloads a
    /// package that is already on disk.
    ///
    /// The pairing is enforced by [`HomeLaunch`]'s shape rather than by a
    /// convention, and this is the test that says so.
    #[test]
    fn each_sidecar_dispatches_only_its_own_verb() {
        let installed = build_child_command_line("ocx", &root("C:\\pkg\\root"), "cmake", &[]);
        let deferred = build_child_command_line("ocx", &shim_ref("ocx.sh/x@sha256:00"), "cmake", &[]);

        assert!(
            installed.contains("launcher exec"),
            "a `.shim` keeps its verb: {installed}"
        );
        assert!(
            !installed.contains("launcher shim"),
            "an installed package's sidecar must never carry the shim verb: {installed}"
        );
        assert!(
            deferred.contains("launcher shim"),
            "a `.shimref` keeps its verb: {deferred}"
        );
        assert!(
            !deferred.contains("launcher exec"),
            "a deferred tool's sidecar must never carry the exec verb: {deferred}"
        );
    }

    /// C-018's allow-list half, and C-011's trust-boundary half in one row:
    /// a `.shim` yields a path for the E3 containment check, a `.shimref`
    /// yields none — so a deferred tool bypasses the allow-list **by design**.
    ///
    /// Stated as an assertion rather than left to a doc comment because the
    /// bypass is a conceded gap, not a closed surface: if a later change makes
    /// `.shimref` produce a path here, that is a decision someone has to take
    /// deliberately, and this reds until they do.
    #[test]
    fn only_the_package_root_sidecar_is_subject_to_containment() {
        assert_eq!(
            root("C:\\pkg\\root").containment_path(),
            Some("C:\\pkg\\root"),
            "a `.shim` names a local path, so E3 applies to it"
        );
        assert_eq!(
            shim_ref("ocx.sh/x@sha256:00").containment_path(),
            None,
            "a `.shimref` names a registry identifier, not a path — there is \
             nothing for the pkg-root allow-list to compare against, and the \
             deferred tool's integrity rests on its digest-addressed fetch"
        );
        assert_eq!(
            exec_project("C:\\w\\proj").containment_path(),
            None,
            "a `.exec` names a LOCAL absolute path and still yields none — a \
             project directory is under none of the three allow-listed roots, so \
             checking it would refuse every legitimate trampoline (C-031); the \
             selector is validated by ocx's own project chain instead"
        );
        assert_eq!(
            exec_global().containment_path(),
            None,
            "…and the global home names no path"
        );
    }

    // ── E3 containment boundary (core::pkg_root_allowed) ───────────────────
    //
    // The boundary is a security control, so it lives in `core` as a pure
    // function precisely so it can be pinned here on the Linux CI host rather
    // than only on a Windows runner. The rows below are the allow-list: the
    // three admitted roots, and the two rejection classes that a raw string
    // prefix comparison would wrongly admit.

    /// The three roots `validate_launcher_pkg_root` allows are the three this
    /// admits — no more. `temp/<hash>` is the load-bearing negative: `temp/`
    /// also holds in-progress download directories, so admitting the whole
    /// subtree would let a tampered sidecar clear E3 while aiming the shim at
    /// un-assembled content.
    #[test]
    fn pkg_root_allowed_matches_the_cli_allow_list() {
        let home = std::path::Path::new("/ocx-home");
        let allowed = [
            "/ocx-home/packages/ocx.sh/sha256/ab/cdef/content",
            "/ocx-home/temp/test/pkg",
            "/ocx-home/temp/patch-test/pkg",
        ];
        for root in allowed {
            assert!(
                super::core::pkg_root_allowed(home, std::path::Path::new(root)),
                "{root} must be inside the containment boundary"
            );
        }

        let refused = [
            // `temp/` at large — download staging, not a launcher root.
            "/ocx-home/temp/0123456789abcdef0123456789abcdef/payload",
            "/ocx-home/temp/pkg",
            // Outside OCX_HOME entirely.
            "/elsewhere/packages/pkg",
        ];
        for root in refused {
            assert!(
                !super::core::pkg_root_allowed(home, std::path::Path::new(root)),
                "{root} must be refused by the containment boundary"
            );
        }
    }

    /// Sibling directories whose names merely share a prefix with an allowed
    /// root must not match. `Path::starts_with` is component-wise, so this
    /// holds by construction — the test exists because the obvious string
    /// implementation (`str::starts_with`) admits every row below.
    #[test]
    fn pkg_root_allowed_rejects_prefix_siblings() {
        let home = std::path::Path::new("/ocx-home");
        for root in [
            "/ocx-home/temp/test-evil/pkg",
            "/ocx-home/temp/patch-test-evil/pkg",
            "/ocx-home/packages-old/pkg",
            "/ocx-home-evil/packages/pkg",
        ] {
            assert!(
                !super::core::pkg_root_allowed(home, std::path::Path::new(root)),
                "{root} shares a name prefix with an allowed root but is a different directory"
            );
        }
    }

    // ── C-034: the trampoline half of the paired byte-exact golden ─────────

    /// The shim is the 2nd producer of the frozen trampoline wire ABI (C-032,
    /// C-034). `ocx_lib` cannot depend on this binary crate, so nothing the
    /// compiler sees binds these literals to
    /// `package_manager::launcher::body::unix_trampoline_body`. `body.rs`'s
    /// `trampoline_wire_tokens_are_bound_to_shim_producer` restates them from
    /// the `.sh` side; this restates them from here. Four tokens, not one — the
    /// root-flag-first ORDER and the `global` sidecar literal are wire ABI just
    /// as much as the verb is.
    #[test]
    fn trampoline_wire_tokens_match_the_sh_trampoline_body() {
        assert_eq!(
            super::core::WIRE_SUBCOMMAND_EXEC,
            "exec",
            "the shim's wire subcommand token for a TRAMPOLINE (any change must also \
             update `crates/ocx_package_manager/src/launcher/body.rs` \
             `unix_trampoline_body` — the native shim is the 2nd producer of this verb too)"
        );

        // The root flag comes FIRST, before the verb — the `.sh` body emits
        // `--project '<root>' exec -- "${0##*/}"`, this emits the same shape
        // with the CommandLineToArgvW quoter in place of the single quotes.
        let project = build_child_command_line("ocx", &exec_project("C:\\w\\proj"), "cmake", &[]);
        assert!(
            project.contains("--project C:\\w\\proj exec -- cmake"),
            "shim child line must carry the `--project <root> exec --` root-flag-FIRST \
             shape shared with the `.sh` trampoline body: {project}"
        );

        let global = build_child_command_line("ocx", &exec_global(), "cmake", &[]);
        assert!(
            global.contains("--global exec -- cmake"),
            "the global selector emits NO value token between the flag and the verb — a \
             `\"\"` there would make ocx read `exec` as the flag's argument: {global}"
        );
    }

    /// The drift the paired goldens cannot catch on their own, extended to the
    /// third verb: a `.exec` dispatched under a positional grammar would emit
    /// `exec <root> -- <stem>`, which `ocx` parses as a *package* named by the
    /// root — a plausible-looking line that runs the wrong thing.
    #[test]
    fn the_exec_sidecar_never_emits_its_home_as_a_positional() {
        let line = build_child_command_line("ocx", &exec_project("C:\\w\\proj"), "cmake", &[]);
        assert!(
            !line.contains("exec C:\\w\\proj"),
            "the home is the ROOT FLAG's argument, never the verb's first positional: {line}"
        );
        assert!(
            !line.contains("launcher"),
            "a trampoline dispatches the project-tier `exec`, not a `launcher` verb: {line}"
        );
        for other in [root("C:\\pkg\\root"), shim_ref("ocx.sh/x@sha256:00")] {
            let positional = build_child_command_line("ocx", &other, "cmake", &[]);
            assert!(
                !positional.contains("--project") && !positional.contains("--global"),
                "the two positional grammars must never grow a root flag: {positional}"
            );
        }
    }

    #[test]
    fn trampoline_child_line_quotes_every_token_and_forwards_argv_in_order() {
        let argv = vec!["--build".to_string(), "a b".to_string(), "\"q\"".to_string()];
        let line = build_child_command_line("ocx", &exec_project("C:\\Program Files\\proj\\"), "cmake", &argv);
        assert!(
            line.contains("--project \"C:\\Program Files\\proj\\\\\""),
            "the root goes through the CommandLineToArgvW quoter unchanged — spaces quoted, \
             the trailing backslash doubled so it cannot escape the closing quote: {line}"
        );
        assert!(
            line.ends_with(" --build \"a b\" \"\\\"q\\\"\""),
            "forwarded argv keeps its order and its quoting: {line}"
        );
    }

    /// C-032 (E-47, E-48): forwarded argv on the **`.exec`** arm. The shipped
    /// rows cover an empty element and a trailing backslash for the two
    /// positional grammars and for the root; the root-flag arm is a separate
    /// `match` branch, so it needs its own rows.
    ///
    /// The three elements are the three distinct quoter outcomes, and the
    /// middle one is the easy thing to get wrong in a test rather than in the
    /// code: `C:\path\` needs **no** quotes and therefore needs **no**
    /// backslash doubling. `CommandLineToArgvW` treats a backslash specially
    /// only immediately before a `"`, so an unquoted trailing backslash
    /// round-trips as itself. Doubling it unconditionally would corrupt the
    /// argument. The CWE-88 doubling belongs to `a b\`, which the space forces
    /// into quotes.
    #[test]
    fn the_trampoline_child_line_quotes_an_empty_and_a_trailing_backslash_argv_element() {
        let argv = vec![String::new(), "C:\\path\\".to_string(), "a b\\".to_string()];

        for (label, sidecar, prefix) in [
            (
                "project home",
                exec_project("C:\\w\\proj"),
                "ocx --project C:\\w\\proj exec -- cmake",
            ),
            ("global home", exec_global(), "ocx --global exec -- cmake"),
        ] {
            let line = build_child_command_line("ocx", &sidecar, "cmake", &argv);
            assert_eq!(
                line,
                format!("{prefix} \"\" C:\\path\\ \"a b\\\\\""),
                "{label}: an empty argv element is emitted as `\"\"` (it would otherwise \
                 vanish); an unquoted trailing backslash stays single; a QUOTED trailing \
                 backslash is doubled so it cannot escape the closing quote (CWE-88); order \
                 is preserved"
            );
        }

        // The control for the `--global` half specifically: no value token
        // between the flag and the verb, even with an empty argv element that
        // could plausibly be mistaken for one.
        let global = build_child_command_line("ocx", &exec_global(), "cmake", &argv);
        assert!(
            !global.contains("--global \"\""),
            "an empty argv element must never land where the flag's value would be: {global}"
        );
    }

    // ── RUL-39: the `.exec`-only strip scope, as a pure predicate ───────────

    /// C-033 / RUL-14 / D-V20: the tier-selector strip is scoped to `.exec`.
    ///
    /// The decision used to live inline inside `#[cfg(windows)] run()`, where
    /// no test on this host compiles — a scope ruling with no observable
    /// state. `core::strips_tier_selectors` is the seam; this is the test that
    /// could not exist before it (RUL-39).
    ///
    /// Both directions are asserted. A predicate that answered `true` for
    /// everything would strip a tier the caller deliberately exported before a
    /// `launcher exec` / `launcher shim` dispatch; one that answered `false`
    /// for everything would leave every trampoline on that `PATH` exiting 64
    /// from `check_global_project_exclusivity`, for a flag nobody typed.
    #[test]
    fn only_a_trampoline_sidecar_strips_the_callers_tier_selectors() {
        use super::core::strips_tier_selectors;

        for (label, sidecar) in [
            ("project home", exec_project("C:\\w\\proj")),
            ("global home", exec_global()),
        ] {
            assert!(
                strips_tier_selectors(&sidecar),
                "{label}: a `.exec` sidecar bakes the ONLY selector, so the caller's must go"
            );
        }

        for (label, sidecar) in [
            ("installed package", root("C:\\pkg\\root")),
            ("deferred tool", shim_ref("ocx.sh/x@sha256:00ab")),
        ] {
            assert!(
                !strips_tier_selectors(&sidecar),
                "{label}: the two positional grammars bake no tier selector, so they have \
                 nothing to shadow — the caller's chosen tier must survive (D-V3's correction)"
            );
        }
    }
}
