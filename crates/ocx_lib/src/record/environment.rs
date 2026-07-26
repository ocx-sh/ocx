// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Best-effort collection of the ambient blocks: host, OS, and process.
//!
//! Every lookup here can fail on a hardened or unusual host, and none of them is
//! worth failing a launch over. The contract is **omit, never guess**: a field
//! that cannot be determined is absent from the record rather than filled with a
//! placeholder a consumer would have to learn to distrust. An absent key means
//! "not determinable here"; a present key is always true.
//!
//! Concretely: an unreadable hostname, an architecture outside the closed OCI
//! set, or a missing passwd entry each drop exactly one key. Consumers already
//! tolerate absent keys, so this costs them nothing. The contract also covers
//! facts a platform cannot answer at all: Windows has no parent-pid call and no
//! uid, so both keys are simply absent there.
//!
//! `process.pid` and `process.executable` are the exception — they are
//! load-bearing, in hand from the caller, and cannot fail.

use std::path::Path;

use super::execution_record::{Host, Os, ParentProcess, Process, User};
use crate::oci::{Architecture, OperatingSystem};

/// The testing seam that forces individual probes to fail.
///
/// Comma-separated **record key paths** — `host.name`, `os.type`,
/// `process.arch`, `process.user.id`, `process.user.name`,
/// `process.parent.pid`, `process.working_directory`. One variable rather than
/// one per probe, so the seam's vocabulary is the wire format's own and a test
/// names the key it expects to be missing.
#[cfg(any(test, feature = "__testing"))]
const FAIL_PROBES_VAR: &str = "__OCX_TESTING_RECORDS_FAIL_PROBES";

/// Collect the host block. Every field omits rather than guesses.
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
/// `pid` is supplied rather than read here because the two platforms learn it at
/// different moments: on Unix it is ocx's own pid, which the exec turns into the
/// tool; on Windows it is the spawned child's, which does not exist until after
/// the spawn.
pub fn process(pid: u32, executable: &Path) -> Process {
    Process {
        pid,
        parent: probe("process.parent.pid", parent_pid).map(|pid| ParentProcess { pid }),
        user: user(),
        // `Architecture::current` already returns `None` for anything outside the
        // closed OCI vocabulary, which is precisely "undeterminable here".
        arch: probe("process.arch", Architecture::current),
        executable: executable.to_path_buf(),
        working_directory: probe("process.working_directory", || crate::env::current_dir().ok()),
    }
}

/// The invoking user, or `None` when neither field is determinable.
///
/// The block is emitted whenever *either* field resolves, so a hardened host
/// that answers the uid but sets no `$USER` still records who ran the tool.
fn user() -> Option<User> {
    let user = User {
        id: probe("process.user.id", user_id),
        name: probe("process.user.name", user_name),
    };
    (user.id.is_some() || user.name.is_some()).then_some(user)
}

/// Run one best-effort probe, unless a test has forced it to fail.
///
/// `key` is the probe's key path in the record, which is also the vocabulary of
/// [`FAIL_PROBES_VAR`].
fn probe<T>(key: &str, lookup: impl FnOnce() -> Option<T>) -> Option<T> {
    if forced_to_fail(key) {
        return None;
    }
    lookup()
}

/// The effective user id the kernel reports.
///
/// The trustworthy half of the user block: `geteuid` answers from the process's
/// own credentials, so unlike [`user_name`] no environment variable can move it.
/// It cannot fail — the call has no error path — but it stays behind the probe
/// seam so a test can exercise the absent case the way Windows produces it.
#[cfg(unix)]
fn user_id() -> Option<String> {
    // SAFETY: `geteuid` reads the calling process's own credentials. It takes no
    // arguments, touches no memory, and is documented as always succeeding.
    Some(unsafe { libc::geteuid() }.to_string())
}

/// Windows identifies a user by SID rather than uid, and reading it means
/// opening the process token and querying it — work this launch-path probe does
/// not do. Absent means "not determinable here", which is exactly true.
#[cfg(not(unix))]
fn user_id() -> Option<String> {
    None
}

/// The invoking user's account name.
///
/// **Caller-controlled**: read from the environment, so a hostile invocation can
/// name any user it likes. [`user_id`] is the field an audit keys on; this one
/// exists because a name is what a human reading the record recognises.
///
/// Read from the environment rather than the passwd database: enumerating users
/// costs a directory round-trip on an LDAP/SSSD-backed host, which is exactly the
/// corporate host this feature exists for, and the field is best-effort either
/// way. A scratch container sets neither, so both approaches omit the key there.
fn user_name() -> Option<String> {
    #[cfg(windows)]
    {
        crate::env::var("USERNAME")
    }
    #[cfg(not(windows))]
    {
        crate::env::var("USER").or_else(|| crate::env::var("LOGNAME"))
    }
}

/// The launching process id, so a call tree (`make` → `ocx exec` → tool)
/// reconstructs from records alone.
#[cfg(unix)]
fn parent_pid() -> Option<u32> {
    Some(std::os::unix::process::parent_id())
}

/// Windows has no `getppid`, and the portable stand-ins all cost a process-table
/// walk: `sysinfo`'s pid-scoped refresh still calls
/// `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` and iterates every entry to
/// find the one asked for, filtering afterwards. That is a blocking scan of the
/// whole machine's process list on the launch path, to fill one best-effort
/// field — so the key is omitted instead, which the module's contract reads as
/// "not determinable here".
#[cfg(not(unix))]
fn parent_pid() -> Option<u32> {
    None
}

/// Whether a test has forced the probe at `key` to fail.
#[cfg(any(test, feature = "__testing"))]
fn forced_to_fail(key: &str) -> bool {
    crate::env::var(FAIL_PROBES_VAR).is_some_and(|forced| forced.split(',').any(|probe| probe.trim() == key))
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
        let env = crate::test::env::lock();
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
        let env = crate::test::env::lock();
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
        let env = crate::test::env::lock();
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
        let env = crate::test::env::lock();
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
        let _env = crate::test::env::lock();

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
