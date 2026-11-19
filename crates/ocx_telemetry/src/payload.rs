// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The record inputs and the payload built from them.

use std::time::{Duration, SystemTime};

use opentelemetry::InstrumentationScope;
use opentelemetry::logs::{AnyValue, LogRecord, Logger, LoggerProvider, Severity};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{SdkLogRecord, SdkLoggerProvider};
use serde_json::{Map, Value, json};

use crate::keys;
use crate::resource::{self, ResourceInputs};

/// The event name of the one command record.
const COMMAND_EVENT: &str = "sh.ocx.command";
/// The event name of each package record.
const PACKAGE_EVENT: &str = "sh.ocx.package";
/// `error.type` when a command failed without a classified `error.detail` slug.
const UNCLASSIFIED_ERROR: &str = "_OTHER";

/// What the command did, as the CLI knows it when the outcome is final.
#[derive(Debug, Clone)]
pub struct CommandOutcome {
    /// The command path joined by spaces, for example `package install`.
    pub name: String,
    /// Entry of the run to outcome known.
    pub duration: Duration,
    /// The process exit code.
    pub exit_code: u8,
    /// Whether the command returned an error; a child's non-zero exit code is not one.
    pub result: CommandResult,
}

/// How the command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    /// No error was returned.
    Succeeded,
    /// The command returned an error.
    Failed {
        /// The error's `error.detail` slug; `None` when no slug classified it.
        detail: Option<String>,
    },
}

/// What happened to a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageAction {
    /// Installed with a candidate link.
    Installed,
    /// Pulled into the store.
    Pulled,
    /// Removed.
    Removed,
}

/// A package's part in the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageRole {
    /// Named by the command.
    Root,
    /// Pulled in by a root.
    Dependency,
}

/// One package record's inputs.
#[derive(Debug, Clone)]
pub struct PackageRecordInput {
    /// The `pkg:oci` purl.
    pub purl: String,
    /// What happened.
    pub action: PackageAction,
    /// Root or dependency.
    pub role: PackageRole,
    /// The package was already in the store.
    pub store_hit: bool,
    /// Layer bytes this package fetched; `None` for a store hit.
    pub download_size: Option<u64>,
    /// Time spent resolving.
    pub resolve: Option<Duration>,
    /// Layer phase wall time.
    pub download: Option<Duration>,
    /// Assembly time.
    pub extract: Option<Duration>,
}

/// The records of one export, as one JSON document: the debug line is this value printed and the
/// wire records are built from it, so the two cannot disagree. Keys are inserted in sorted order.
#[derive(Debug, Clone)]
pub(crate) struct Payload {
    document: Value,
    timestamp: SystemTime,
}

impl Payload {
    /// The document on one line, keys sorted, durations in seconds, without the shared timestamp.
    pub(crate) fn to_json_line(&self) -> String {
        self.document.to_string()
    }

    /// The line the debug log carries: `telemetry payload <json>`.
    pub(crate) fn audit_line(&self) -> String {
        format!("telemetry payload {}", self.to_json_line())
    }

    /// The records and resource in the OpenTelemetry SDK's types, ready for one export. Read back
    /// from the same document as [`Payload::to_json_line`], every record stamped with the
    /// timestamp taken when the payload was built.
    pub(crate) fn to_wire(&self) -> WireRecords {
        let scope = {
            let scope = &self.document["scope"];
            let builder = InstrumentationScope::builder(scope["name"].as_str().unwrap_or("ocx").to_owned());
            match scope["version"].as_str() {
                Some(version) => builder.with_version(version.to_owned()),
                None => builder,
            }
            .build()
        };
        // A provider without processors only mints records: nothing is emitted or exported through it.
        let logger = SdkLoggerProvider::builder().build().logger_with_scope(scope.clone());
        let records = self.document["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|record| {
                let mut wire = logger.create_log_record();
                wire.set_event_name(match record["event_name"].as_str()? {
                    COMMAND_EVENT => COMMAND_EVENT,
                    PACKAGE_EVENT => PACKAGE_EVENT,
                    _ => return None,
                });
                wire.set_timestamp(self.timestamp);
                wire.set_observed_timestamp(self.timestamp);
                wire.set_severity_number(Severity::Info);
                wire.set_severity_text("INFO");
                for (key, value) in record["attributes"].as_object()? {
                    if let Some(value) = any_value(value) {
                        wire.add_attribute(key.clone(), value);
                    }
                }
                Some(wire)
            })
            .collect();
        let attributes = self.document["resource"]
            .as_object()
            .map(|map| map.iter().map(|(key, value)| (key.clone(), value.clone())).collect())
            .unwrap_or_default();
        WireRecords {
            resource: resource::to_otel(&attributes),
            scope,
            records,
        }
    }
}

fn any_value(value: &Value) -> Option<AnyValue> {
    Some(match value {
        Value::String(text) => AnyValue::from(text.clone()),
        Value::Bool(flag) => AnyValue::from(*flag),
        Value::Number(number) => match number.as_i64() {
            Some(int) => AnyValue::from(int),
            None => AnyValue::from(number.as_f64()?),
        },
        _ => return None,
    })
}

/// The SDK form of a [`Payload`]. Owns the records, the scope and the resource, so a batch can
/// borrow from it for as long as the export runs.
#[derive(Debug)]
pub(crate) struct WireRecords {
    resource: Resource,
    scope: InstrumentationScope,
    records: Vec<SdkLogRecord>,
}

impl WireRecords {
    /// The resource every record shares; hand it to the exporter's `set_resource`.
    pub(crate) fn resource(&self) -> &Resource {
        &self.resource
    }

    /// The pairs `LogBatch::new` takes. The result borrows `self`: keep this value alive until the
    /// export future resolves.
    pub(crate) fn batch_data(&self) -> Vec<(&SdkLogRecord, &InstrumentationScope)> {
        self.records.iter().map(|record| (record, &self.scope)).collect()
    }
}

/// The command record followed by the package records in input order, with the resource every
/// record shares. `installation_id` is `None` when the id file could not be read or created.
pub(crate) fn build(
    resource: &ResourceInputs,
    installation_id: Option<&str>,
    outcome: &CommandOutcome,
    packages: &[PackageRecordInput],
) -> Payload {
    let records: Vec<Value> = std::iter::once(command_record(outcome))
        .chain(packages.iter().map(package_record))
        .collect();
    let resource_attributes: Map<String, Value> = resource::attributes(resource, installation_id).into_iter().collect();
    let document = json!({
        "records": records,
        "resource": resource_attributes,
        "scope": { "name": "ocx", "version": resource.version },
    });
    Payload {
        document,
        timestamp: SystemTime::now(),
    }
}

fn record(event: &'static str, attributes: Map<String, Value>) -> Value {
    json!({ "attributes": attributes, "event_name": event, "severity": "INFO" })
}

fn command_record(outcome: &CommandOutcome) -> Value {
    let mut attributes = Map::new();
    if let CommandResult::Failed { detail } = &outcome.result {
        attributes.insert(
            "error.type".to_owned(),
            detail.as_deref().unwrap_or(UNCLASSIFIED_ERROR).into(),
        );
    }
    attributes.insert("process.exit.code".to_owned(), outcome.exit_code.into());
    attributes.insert(
        "sh.ocx.command.duration".to_owned(),
        outcome.duration.as_secs_f64().into(),
    );
    attributes.insert("sh.ocx.command.name".to_owned(), outcome.name.as_str().into());
    record(COMMAND_EVENT, attributes)
}

fn package_record(input: &PackageRecordInput) -> Value {
    let (action, has_store_state) = match input.action {
        PackageAction::Installed => ("installed", true),
        PackageAction::Pulled => ("pulled", true),
        PackageAction::Removed => ("removed", false),
    };
    let role = match input.role {
        PackageRole::Root => "root",
        PackageRole::Dependency => "dependency",
    };
    let mut attributes = Map::new();
    attributes.insert(keys::PACKAGE.to_owned(), input.purl.as_str().into());
    attributes.insert("sh.ocx.package.action".to_owned(), action.into());
    // A removal has no store state and no phases: only the three identity keys.
    if has_store_state {
        let missed = !input.store_hit;
        let seconds = |duration: Duration| Value::from(duration.as_secs_f64());
        if let Some(download) = input.download {
            attributes.insert("sh.ocx.package.download.duration".to_owned(), seconds(download));
        }
        if missed {
            attributes.insert(
                "sh.ocx.package.download.size".to_owned(),
                input.download_size.unwrap_or(0).into(),
            );
            if let Some(extract) = input.extract {
                attributes.insert("sh.ocx.package.extract.duration".to_owned(), seconds(extract));
            }
        }
        if let Some(resolve) = input.resolve {
            attributes.insert("sh.ocx.package.resolve.duration".to_owned(), seconds(resolve));
        }
        attributes.insert("sh.ocx.package.store_hit".to_owned(), input.store_hit.into());
    }
    attributes.insert(keys::ROLE.to_owned(), role.into());
    record(PACKAGE_EVENT, attributes)
}

#[cfg(test)]
impl WireRecords {
    /// The records, the command record first.
    fn records(&self) -> &[SdkLogRecord] {
        &self.records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::CiEnvironment;

    #[test]
    fn golden_payload() {
        let resource = ResourceInputs {
            version: "0.0.0-test".to_owned(),
            pid: 4242,
            parent_pid: Some(4241),
            host_name: Some("ci-host".to_owned()),
            os_type: Some("linux".to_owned()),
            arch: Some("amd64".to_owned()),
            interactive: false,
            ci: CiEnvironment {
                github_actions: true,
                ci: true,
                github_run_id: Some("11".to_owned()),
                github_job: Some("build".to_owned()),
                ..CiEnvironment::default()
            },
            ..ResourceInputs::default()
        };
        let outcome = CommandOutcome {
            name: "package install".to_owned(),
            duration: Duration::from_millis(3500),
            exit_code: 79,
            result: CommandResult::Failed {
                detail: Some("manifest_not_found".to_owned()),
            },
        };
        let packages = [
            PackageRecordInput {
                purl: "pkg:oci/cmake?repository_url=ocx.sh/tools&arch=amd64".to_owned(),
                action: PackageAction::Installed,
                role: PackageRole::Root,
                store_hit: false,
                download_size: Some(1_048_576),
                resolve: Some(Duration::from_millis(250)),
                download: Some(Duration::from_millis(1500)),
                extract: Some(Duration::from_millis(500)),
            },
            PackageRecordInput {
                purl: "pkg:oci/ninja?repository_url=ocx.sh/tools&arch=amd64".to_owned(),
                action: PackageAction::Pulled,
                role: PackageRole::Dependency,
                store_hit: true,
                download_size: None,
                resolve: Some(Duration::from_millis(125)),
                download: None,
                extract: None,
            },
        ];

        let line = build(
            &resource,
            Some("0b0e6c3e-5d5c-4b3a-8a0e-2d7f3c9e1a11"),
            &outcome,
            &packages,
        )
        .to_json_line();

        let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/payload.json");
        let golden = std::fs::read_to_string(&golden_path).expect("read the golden payload");
        // The file ends in the newline editors insist on; the debug line has none.
        assert_eq!(line, golden.trim_end_matches('\n'));
    }

    fn base_outcome() -> CommandOutcome {
        CommandOutcome {
            name: "package install".to_owned(),
            duration: Duration::from_millis(1500),
            exit_code: 0,
            result: CommandResult::Succeeded,
        }
    }

    fn package(action: PackageAction) -> PackageRecordInput {
        PackageRecordInput {
            purl: "pkg:oci/cmake".to_owned(),
            action,
            role: PackageRole::Root,
            store_hit: false,
            download_size: Some(10),
            resolve: Some(Duration::from_millis(500)),
            download: Some(Duration::from_millis(250)),
            extract: Some(Duration::from_millis(125)),
        }
    }

    fn document(outcome: &CommandOutcome, packages: &[PackageRecordInput]) -> Value {
        let payload = build(&ResourceInputs::default(), None, outcome, packages);
        serde_json::from_str(&payload.to_json_line()).expect("the debug line is JSON")
    }

    fn attribute_keys(record: &Value) -> Vec<&str> {
        record["attributes"]
            .as_object()
            .expect("record attributes")
            .keys()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn command_record_fields() {
        let ok = document(&base_outcome(), &[]);
        let records = ok["records"].as_array().expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["event_name"], "sh.ocx.command");
        assert_eq!(records[0]["severity"], "INFO");
        assert_eq!(records[0]["attributes"]["sh.ocx.command.name"], "package install");
        assert_eq!(records[0]["attributes"]["sh.ocx.command.duration"], 1.5);
        assert_eq!(records[0]["attributes"]["process.exit.code"], 0);
        assert!(
            records[0]["attributes"].get("error.type").is_none(),
            "success has no error.type"
        );

        // A child's non-zero exit code is not an error of ocx.
        let child_exit = CommandOutcome {
            exit_code: 3,
            ..base_outcome()
        };
        assert!(
            document(&child_exit, &[])["records"][0]["attributes"]
                .get("error.type")
                .is_none()
        );

        let unclassified = CommandOutcome {
            exit_code: 1,
            result: CommandResult::Failed { detail: None },
            ..base_outcome()
        };
        let attributes = &document(&unclassified, &[])["records"][0]["attributes"];
        assert_eq!(attributes["error.type"], "_OTHER");
        assert_eq!(attributes["process.exit.code"], 1);
    }

    #[test]
    fn package_record_presence() {
        let keys = |input: &PackageRecordInput| {
            let doc = document(&base_outcome(), std::slice::from_ref(input));
            attribute_keys(&doc["records"][1])
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let all = [
            "sh.ocx.package",
            "sh.ocx.package.action",
            "sh.ocx.package.download.duration",
            "sh.ocx.package.download.size",
            "sh.ocx.package.extract.duration",
            "sh.ocx.package.resolve.duration",
            "sh.ocx.package.store_hit",
            "sh.ocx.role",
        ];

        // Fetched: every key.
        assert_eq!(keys(&package(PackageAction::Installed)), all);

        // Store hit: no download size, no extract time; a layer-less package has no download time.
        let hit = PackageRecordInput {
            store_hit: true,
            download_size: None,
            download: None,
            extract: None,
            ..package(PackageAction::Pulled)
        };
        assert_eq!(
            keys(&hit),
            [
                "sh.ocx.package",
                "sh.ocx.package.action",
                "sh.ocx.package.resolve.duration",
                "sh.ocx.package.store_hit",
                "sh.ocx.role"
            ]
        );

        // A miss that fetched no layer reports zero bytes and no download time.
        let empty = PackageRecordInput {
            download_size: None,
            download: None,
            ..package(PackageAction::Installed)
        };
        let doc = document(&base_outcome(), &[empty]);
        assert_eq!(doc["records"][1]["attributes"]["sh.ocx.package.download.size"], 0);
        assert!(
            doc["records"][1]["attributes"]
                .get("sh.ocx.package.download.duration")
                .is_none()
        );

        // Removed: identity keys only, however the inputs are filled.
        let removed = package(PackageAction::Removed);
        assert_eq!(
            keys(&removed),
            ["sh.ocx.package", "sh.ocx.package.action", "sh.ocx.role"]
        );
        let doc = document(&base_outcome(), &[removed]);
        assert_eq!(doc["records"][1]["attributes"]["sh.ocx.package.action"], "removed");
    }

    #[test]
    fn wire_records_come_from_the_same_document() {
        let payload = build(
            &ResourceInputs {
                version: "9.9.9".to_owned(),
                pid: 5,
                ..ResourceInputs::default()
            },
            Some("0b0e6c3e-5d5c-4b3a-8a0e-2d7f3c9e1a11"),
            &base_outcome(),
            &[package(PackageAction::Installed)],
        );
        let wire = payload.to_wire();

        assert_eq!(wire.records().len(), 2);
        let events: Vec<_> = wire.records().iter().map(|record| record.event_name()).collect();
        assert_eq!(events, [Some("sh.ocx.command"), Some("sh.ocx.package")]);
        let stamps: Vec<_> = wire.records().iter().map(|record| record.timestamp()).collect();
        assert!(
            stamps[0].is_some() && stamps[0] == stamps[1],
            "one shared timestamp: {stamps:?}"
        );
        for record in wire.records() {
            assert_eq!(record.severity_number(), Some(Severity::Info));
            assert_eq!(record.severity_text(), Some("INFO"));
            assert!(record.body().is_none());
        }
        let command_keys: Vec<String> = wire.records()[0]
            .attributes_iter()
            .map(|(key, _)| key.as_str().to_owned())
            .collect();
        assert_eq!(
            command_keys,
            ["process.exit.code", "sh.ocx.command.duration", "sh.ocx.command.name"]
        );
        let pairs = wire.batch_data();
        assert!(
            pairs
                .iter()
                .all(|(_, scope)| scope.name() == "ocx" && scope.version() == Some("9.9.9"))
        );
        assert!(
            wire.resource()
                .iter()
                .any(|(key, _)| key.as_str() == "sh.ocx.installation.id")
        );
        // The debug line carries no timestamp.
        assert!(!payload.to_json_line().contains("timestamp"));
        assert!(payload.audit_line().starts_with("telemetry payload {"));
    }
}
