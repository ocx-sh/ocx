// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! What each command prints under `--format json`, the version of that contract, and the value
//! grammar each argument parser stands for.

use std::any::TypeId;

use serde::Serialize;

use super::leaf::Leaf;
use crate::api::Printable;

/// A report root name, read from `Printable::ROOT` by [`OutputMode::report`] and
/// [`OutputMode::report_then_fail`]; the private field keeps any other code from spelling one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ReportRoot(&'static str);

impl ReportRoot {
    /// The root name as published in `cli.json` and `reports/v1.json`.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// One kind of stdout a command can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputMode {
    /// One JSON report of the named root, exit 0.
    Report {
        #[schemars(with = "String")]
        root: ReportRoot,
    },
    /// One JSON report of the named root, then a non-zero exit: the command reports what it did before failing.
    ReportThenFail {
        #[schemars(with = "String")]
        root: ReportRoot,
    },
    /// Nothing on stdout.
    Empty,
    /// The child process owns stdout.
    Passthrough,
    /// Shell or CI source text, never JSON.
    ShellStream,
    /// A fetched document's raw bytes, verbatim, never JSON.
    RawDocument,
}

impl OutputMode {
    /// One JSON report of `T`, exit 0; the root name is `T::ROOT`, so it cannot be misspelled.
    pub const fn report<T: Printable>() -> Self {
        Self::Report {
            root: ReportRoot(T::ROOT),
        }
    }

    /// One JSON report of `T`, then a non-zero exit.
    pub const fn report_then_fail<T: Printable>() -> Self {
        Self::ReportThenFail {
            root: ReportRoot(T::ROOT),
        }
    }
}

type Row = (&'static [&'static str], u32, &'static [OutputMode]);

/// Every command with its contract version and output modes, keyed by its path below `ocx`.
pub static CONTRACT: &[Row] = &rows();

const fn rows() -> [Row; Leaf::ALL.len()] {
    let mut rows: [Row; Leaf::ALL.len()] = [(&[], 0, &[]); Leaf::ALL.len()];
    let mut index = 0;
    while index < Leaf::ALL.len() {
        let leaf = Leaf::ALL[index];
        let (version, modes) = leaf.contract();
        rows[index] = (leaf.path(), version, modes);
        index += 1;
    }
    rows
}

/// The value grammar of an argument whose parser offers no fixed choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    String,
    Integer,
    Path,
    Identifier,
    Platform,
    Digest,
}

/// The [`ValueType`] a parser's output type stands for, or `None` for a type nobody classified yet.
pub fn value_type(parser: &clap::builder::ValueParser) -> Option<ValueType> {
    let id = parser.type_id();
    VALUE_TYPES
        .iter()
        .find(|(type_id, _)| id == type_id())
        .map(|(_, value_type)| *value_type)
}

type TypeOf = fn() -> TypeId;

/// Closed: an argument of any other type fails the export until a row names it.
static VALUE_TYPES: &[(TypeOf, ValueType)] = &[
    (TypeId::of::<String>, ValueType::String),
    (TypeId::of::<std::ffi::OsString>, ValueType::String),
    (TypeId::of::<(String, String)>, ValueType::String),
    (TypeId::of::<std::time::Duration>, ValueType::String),
    (TypeId::of::<ocx_setup::VersionSpec>, ValueType::String),
    (TypeId::of::<ocx_package::version::Version>, ValueType::String),
    (
        TypeId::of::<ocx_sign::attest::predicate::PredicateType>,
        ValueType::String,
    ),
    (TypeId::of::<ocx_announce::forge::RepoCoordinate>, ValueType::String),
    (TypeId::of::<ocx_oci::layer_ref::LayerRef>, ValueType::String),
    (TypeId::of::<u8>, ValueType::Integer),
    (TypeId::of::<u16>, ValueType::Integer),
    (TypeId::of::<u32>, ValueType::Integer),
    (TypeId::of::<u64>, ValueType::Integer),
    (TypeId::of::<usize>, ValueType::Integer),
    (TypeId::of::<i64>, ValueType::Integer),
    (TypeId::of::<std::path::PathBuf>, ValueType::Path),
    (TypeId::of::<crate::options::Identifier>, ValueType::Identifier),
    (TypeId::of::<ocx_oci::PinnedPackageRef>, ValueType::Identifier),
    (TypeId::of::<ocx_oci::PackageRef>, ValueType::Identifier),
    (TypeId::of::<ocx_oci::Platform>, ValueType::Platform),
    (TypeId::of::<ocx_oci::Digest>, ValueType::Digest),
];
