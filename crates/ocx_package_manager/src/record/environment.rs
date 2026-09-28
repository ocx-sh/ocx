// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Best-effort collection of the host, OS and process blocks.
//!
//! Omit, never guess: a failed lookup drops exactly its own key and never fails the launch.

use std::path::Path;

use super::execution_record::{Host, Os, ParentProcess, Process, User};
use ocx_oci::{Architecture, OperatingSystem};

/// Testing seam: comma-separated record key paths (e.g. `process.user.id`) whose probes fail.
#[cfg(any(test, feature = "__testing"))]
const FAIL_PROBES_VAR: &str = "__OCX_TESTING_RECORDS_FAIL_PROBES";

/// Collect the host block.
pub fn host() -> Host {
    Host {
        name: probe("host.name", sysinfo::System::host_name),
    }
}

/// Collect the operating-system block.
pub fn operating_system() -> Os {
    Os {
        os_type: probe("os.type", OperatingSystem::current),
    }
}

/// Collect the process block.
///
/// `pid` is passed in because on Windows it is the spawned child's, which exists only after the spawn.
pub fn process(pid: u32, executable: &Path) -> Process {
    Process {
        pid,
        parent: probe("process.parent.pid", parent_pid).map(|pid| ParentProcess { pid }),
        user: user(),
        arch: probe("process.arch", Architecture::current),
        executable: executable.to_path_buf(),
        working_directory: probe("process.working_directory", || ocx_util::env::current_dir().ok()),
    }
}

/// The invoking user, or `None` when neither field is determinable.
fn user() -> Option<User> {
    let user = User {
        id: probe("process.user.id", user_id),
        name: probe("process.user.name", user_name),
    };
    (user.id.is_some() || user.name.is_some()).then_some(user)
}

/// Run one best-effort probe, where `key` is its record key path, unless a test forced it to fail.
fn probe<T>(key: &str, lookup: impl FnOnce() -> Option<T>) -> Option<T> {
    if forced_to_fail(key) {
        return None;
    }
    lookup()
}

/// The effective user id; unlike [`user_name`], no environment variable can move it.
#[cfg(unix)]
fn user_id() -> Option<String> {
    // SAFETY: `geteuid` takes no arguments, touches no memory and always succeeds.
    Some(unsafe { libc::geteuid() }.to_string())
}

/// Omitted: a Windows SID needs a process-token query this launch-path probe does not make.
#[cfg(not(unix))]
fn user_id() -> Option<String> {
    None
}

/// The invoking user's account name: caller-controlled via the environment, so audits key on [`user_id`].
///
/// Not read from passwd, which costs a directory round-trip per launch on LDAP/SSSD-backed hosts.
fn user_name() -> Option<String> {
    #[cfg(windows)]
    {
        ocx_util::env::var("USERNAME")
    }
    #[cfg(not(windows))]
    {
        ocx_util::env::var("USER").or_else(|| ocx_util::env::var("LOGNAME"))
    }
}

/// The launching process id.
#[cfg(unix)]
fn parent_pid() -> Option<u32> {
    Some(std::os::unix::process::parent_id())
}

/// Omitted: Windows has no `getppid`, and every stand-in (`sysinfo` included) scans the whole process
/// table on the launch path.
#[cfg(not(unix))]
fn parent_pid() -> Option<u32> {
    None
}

/// Whether a test has forced the probe at `key` to fail.
#[cfg(any(test, feature = "__testing"))]
fn forced_to_fail(key: &str) -> bool {
    ocx_util::env::var(FAIL_PROBES_VAR).is_some_and(|forced| forced.split(',').any(|probe| probe.trim() == key))
}

#[cfg(not(any(test, feature = "__testing")))]
fn forced_to_fail(_key: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Every best-effort probe forced to fail drops exactly its own key, and
    /// nothing errors: an undetectable hostname must never cost an invocation.
    #[test]
    fn every_forced_probe_omits_its_key() {
        let env = ocx_util::env::overrides::lock();
        env.set(
            FAIL_PROBES_VAR,
            "host.name,os.type,process.arch,process.user.id,process.user.name,process.parent.pid,process.working_directory",
        );

        assert!(host().name.is_none(), "a forced host.name probe omits the key");
        assert!(
            operating_system().os_type.is_none(),
            "a forced os.type probe omits the key"
        );

        let process = process(42, &PathBuf::from("/opt/tool"));
        assert!(process.arch.is_none(), "a forced process.arch probe omits the key");
        assert!(
            process.user.is_none(),
            "with both user probes forced the whole block is omitted"
        );
        assert!(
            process.parent.is_none(),
            "a forced process.parent.pid probe omits the key"
        );
        assert!(
            process.working_directory.is_none(),
            "a forced process.working_directory probe omits the key"
        );

        // The load-bearing fields come from resolution, not the environment, so
        // no probe failure can drop them.
        assert_eq!(process.pid, 42);
        assert_eq!(process.executable, PathBuf::from("/opt/tool"));
    }

    /// The user block survives one half being undeterminable — which is the
    /// Windows case for the id and the scratch-container case for the name.
    #[test]
    fn the_user_block_survives_either_half_going_missing() {
        let env = ocx_util::env::overrides::lock();
        env.set(FAIL_PROBES_VAR, "process.user.id");
        env.set("USER", "auditor");
        env.set("USERNAME", "auditor");

        let user = process(1, &PathBuf::from("/bin/true"))
            .user
            .expect("a determinable name alone still records the user");
        assert!(user.id.is_none());
        assert_eq!(user.name.as_deref(), Some("auditor"));

        env.set(FAIL_PROBES_VAR, "process.user.name");
        let user = process(1, &PathBuf::from("/bin/true")).user;
        if cfg!(unix) {
            let user = user.expect("a determinable id alone still records the user");
            assert!(user.name.is_none());
            assert!(user.id.is_some(), "the effective uid is determinable on this platform");
        } else {
            assert!(
                user.is_none(),
                "a platform with no uid and the name gone has nothing left to identify the user with"
            );
        }
    }

    /// The account name is caller-controlled, so it must never be the field an
    /// audit keys on: the environment moves it, the id it cannot.
    #[cfg(unix)]
    #[test]
    fn the_environment_can_rename_the_user_but_not_reidentify_them() {
        let env = ocx_util::env::overrides::lock();
        env.set("USER", "root");
        env.set("LOGNAME", "root");

        let user = process(1, &PathBuf::from("/bin/true")).user.expect("user block");
        assert_eq!(user.name.as_deref(), Some("root"), "the name follows the environment");
        // SAFETY: `geteuid` reads the calling process's own credentials.
        let effective = unsafe { libc::geteuid() }.to_string();
        assert_eq!(
            user.id.as_deref(),
            Some(effective.as_str()),
            "the id comes from the kernel and the environment cannot move it",
        );
    }

    /// The seam is per-key, not all-or-nothing — otherwise a test asserting one
    /// missing key would pass against an implementation that dropped every key.
    #[test]
    fn a_forced_probe_leaves_its_siblings_alone() {
        let env = ocx_util::env::overrides::lock();
        env.set(FAIL_PROBES_VAR, "host.name");
        env.set("USER", "auditor");
        env.set("USERNAME", "auditor");

        assert!(host().name.is_none(), "host.name was forced to fail");

        let process = process(1, &PathBuf::from("/bin/true"));
        assert_eq!(
            process.arch,
            Architecture::current(),
            "process.arch was not forced and must still resolve"
        );
        assert_eq!(
            process.user.and_then(|user| user.name),
            Some("auditor".to_string()),
            "process.user.name was not forced and must still resolve"
        );
    }

    /// With no seam set, the probes that cannot fail on a normal host do resolve
    /// — the discriminator proving the tests above assert absence rather than an
    /// unimplemented collector.
    #[test]
    fn unforced_probes_resolve_on_an_ordinary_host() {
        let _env = ocx_util::env::overrides::lock();

        assert_eq!(operating_system().os_type, OperatingSystem::current());

        let process = process(7, &PathBuf::from("/bin/true"));
        assert_eq!(
            process.parent.is_some(),
            cfg!(unix),
            "a parent pid is determinable exactly where the platform offers `getppid`"
        );
        assert!(
            process.working_directory.is_some(),
            "the working directory is determinable while the process is running"
        );
    }
}
