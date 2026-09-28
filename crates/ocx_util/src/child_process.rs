// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Turns a finished child's exit status into an exit code; platform conditionals stay inside each helper.

use std::process::ExitStatus;

/// Maps a child [`ExitStatus`] to an exit code: its own code, `128 + signum` for a signal death, `1` when neither is
/// known.
#[cfg(unix)]
pub fn exit_code_from_status(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        code
    } else {
        status.signal().map(|s| 128 + s).unwrap_or(1)
    }
}

#[cfg(not(unix))]
pub fn exit_code_from_status(status: ExitStatus) -> i32 {
    // Full `i32`: truncating to 8 bits loses Windows `STATUS_*` codes.
    status.code().unwrap_or(1)
}

/// Convert a child [`ExitStatus`] into a [`std::process::ExitCode`], saturating codes above 255.
pub fn propagate_exit_code(status: ExitStatus) -> std::process::ExitCode {
    // Saturate, not truncate: a wrapped code can turn a failure into `0`.
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
