// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The attribute keys a consumer joins telemetry on; the builders use these, so a rename is one edit.

/// Resource key: this process's id.
pub const PROCESS_PID: &str = "process.pid";
/// Resource key: the parent process id.
pub const PROCESS_PARENT_PID: &str = "process.parent.pid";
/// Resource key: the host name.
pub const HOST_NAME: &str = "host.name";
/// Resource key: the operating system type.
pub const OS_TYPE: &str = "os.type";
/// Resource key: the CPU architecture.
pub const PROCESS_ARCH: &str = "process.arch";
/// Package record key: the package `pkg:oci` purl.
pub const PACKAGE: &str = "sh.ocx.package";
/// Package record key: whether the package is a root or a dependency.
pub const ROLE: &str = "sh.ocx.role";

/// The resource keys an execution record shares with telemetry.
pub const RESOURCE_JOIN_KEYS: [&str; 5] = [PROCESS_PID, PROCESS_PARENT_PID, HOST_NAME, OS_TYPE, PROCESS_ARCH];
/// The package record keys an execution record shares with telemetry.
pub const RECORD_JOIN_KEYS: [&str; 2] = [PACKAGE, ROLE];
