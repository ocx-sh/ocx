// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Generic boundary between OCX and the exit status of the child processes it
//! spawns. Platform conditionals live inside each helper so callers stay
//! platform-blind.
//!
//! The spawn primitives themselves are **not** here: they live in
//! `launch/child_process.rs` as a private submodule of [`crate::launch`], so
//! the only way to start a tool is through the policy seam that decides what it
//! records. This module owns the other half — turning a finished child's
//! [`ExitStatus`] into an exit code — which has callers on both sides of that
//! seam and no policy of its own.
//!
//! [`exit_code_from_status`] is the single source of truth for the
//! `128 + signum` (Unix) and full-`i32` passthrough (Windows) mapping;
//! [`propagate_exit_code`] and the launch seam's diverging
//! `propagate_exit_status` both delegate to it.

use std::process::ExitStatus;

/// Single source of truth for the "child [`ExitStatus`] → `i32` exit code"
/// mapping.
///
/// * **Unix** — signal-aware: when `code()` is `None` the child was killed by
///   a signal; returns `128 + signum` (POSIX convention) so `$?` and scripts
///   see a meaningful code. Falls back to `1` when no signal number is
///   available.
/// * **non-Unix** — the full `i32` exit code is forwarded. Windows preserves
///   32-bit `STATUS_*` codes in single-process pipelines; truncating to 8 bits
///   would silently lose information (e.g. `STATUS_ACCESS_VIOLATION =
///   0xC0000005`). Callers that must map to `u8` (e.g. [`propagate_exit_code`])
///   saturate after calling this function.
#[cfg(unix)]
pub fn exit_code_from_status(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        code
    } else {
        // Child killed by signal — follow 128 + signum convention.
        status.signal().map(|s| 128 + s).unwrap_or(1)
    }
}

#[cfg(not(unix))]
pub fn exit_code_from_status(status: ExitStatus) -> i32 {
    // Windows preserves the full 32-bit exit code; pass it through so
    // PowerShell's `$LastExitCode` and cmd's `%ERRORLEVEL%` see what
    // the child actually returned.
    status.code().unwrap_or(1)
}

/// Convert a child [`ExitStatus`] into a [`std::process::ExitCode`].
///
/// On Unix: signal-aware — `128 + signum` when the child was killed by a
/// signal (POSIX convention). When `code()` is `Some(n)` the raw value is
/// passed through, saturated to `u8` (0–255 range) for compatibility with the
/// [`std::process::ExitCode`] type.
///
/// On Windows: the raw numeric exit code is forwarded via `u8` saturation
/// (Windows `STATUS_*` values are 32-bit; values > 255 saturate to 255).
/// The launch seam's diverging propagation passes the full `i32` directly;
/// this non-diverging variant must truncate to `u8` because
/// [`std::process::ExitCode::from`] only accepts `u8`. The contract for callers
/// is documented: codes ≤ 255 are preserved cross-platform; Windows-only high
/// codes use the diverging `launch::exec` path instead.
pub fn propagate_exit_code(status: ExitStatus) -> std::process::ExitCode {
    // Delegate to exit_code_from_status for the platform-aware i32 computation
    // (128 + signum on Unix, full i32 passthrough on Windows), then saturate to
    // u8 because std::process::ExitCode::from only accepts u8 (0–255 range).
    // Codes above 255 saturate to 255. Callers that need the full i32 (e.g.,
    // Windows STATUS_* codes) should use the exec path instead.
    let code = exit_code_from_status(status);
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(255))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signal-killed child takes `128 + signum`, and an ordinary exit does
    /// not.
    ///
    /// The pair is the point: asserting only the signal case would pass for an
    /// implementation that returned `128 + n` unconditionally, and every one of
    /// the five spawn sites folded onto this function depends on the two being
    /// told apart.
    ///
    /// Raw wait statuses are used rather than a real child: they name the exact
    /// case without spawning anything, and `ExitStatusExt::from_raw` is the only
    /// way to construct the signalled one at all.
    #[cfg(unix)]
    #[test]
    fn a_signalled_child_and_an_exited_child_take_different_codes() {
        use std::os::unix::process::ExitStatusExt as _;

        // Low 7 bits = terminating signal, no exit code: SIGKILL.
        assert_eq!(exit_code_from_status(ExitStatus::from_raw(9)), 137);
        // High byte = exit code, low byte zero: exited with 3.
        assert_eq!(exit_code_from_status(ExitStatus::from_raw(3 << 8)), 3);
        // And zero stays zero rather than becoming the `unwrap_or(1)` fallback.
        assert_eq!(exit_code_from_status(ExitStatus::from_raw(0)), 0);
    }

    /// `propagate_exit_code` saturates into `u8` but must not otherwise
    /// re-derive anything: the `128 + signum` value has to survive the
    /// conversion.
    #[cfg(unix)]
    #[test]
    fn propagation_preserves_the_signal_convention() {
        use std::os::unix::process::ExitStatusExt as _;

        assert_eq!(
            format!("{:?}", propagate_exit_code(ExitStatus::from_raw(9))),
            format!("{:?}", std::process::ExitCode::from(137u8))
        );
    }

    /// Windows forwards the full 32-bit code, including the `STATUS_*` values
    /// that would be destroyed by an 8-bit truncation here.
    ///
    /// **Unexercised on this repo's Linux CI** — compiled only on Windows.
    #[cfg(windows)]
    #[test]
    fn windows_forwards_the_full_status_code() {
        use std::os::windows::process::ExitStatusExt as _;

        assert_eq!(exit_code_from_status(ExitStatus::from_raw(3)), 3);
        // STATUS_ACCESS_VIOLATION, the case truncation would lose.
        assert_eq!(
            exit_code_from_status(ExitStatus::from_raw(0xC000_0005)),
            0xC000_0005_u32 as i32
        );
    }
}
