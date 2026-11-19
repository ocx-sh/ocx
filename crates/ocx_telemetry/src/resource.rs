// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The resource every record of one export shares.

use std::collections::BTreeMap;

use opentelemetry::{Key, KeyValue, Value};
use opentelemetry_sdk::Resource;
use percent_encoding::percent_decode_str;

use crate::ci::CiEnvironment;
use crate::keys;

/// Attribute key to JSON scalar; the sorted map is what makes the debug line byte-stable, and the
/// wire resource is converted from the same values.
pub(crate) type Attributes = BTreeMap<String, serde_json::Value>;

/// Plain data the resource is built from; the caller probes the process, this crate only maps.
#[derive(Debug, Clone, Default)]
pub struct ResourceInputs {
    /// The version `ocx version` prints.
    pub version: String,
    /// This process's id.
    pub pid: u32,
    /// The parent process id; `None` where it cannot be determined (Windows).
    pub parent_pid: Option<u32>,
    /// The host name, when known.
    pub host_name: Option<String>,
    /// `os.type` spelling, when known.
    pub os_type: Option<String>,
    /// `process.arch` spelling, when known.
    pub arch: Option<String>,
    /// Whether stderr is a terminal.
    pub interactive: bool,
    /// The CI variables.
    pub ci: CiEnvironment,
    /// `OTEL_RESOURCE_ATTRIBUTES`, unparsed.
    pub otel_resource_attributes: Option<String>,
    /// `OTEL_SERVICE_NAME`.
    pub otel_service_name: Option<String>,
    /// `service.name` from `[telemetry] service_name`.
    pub config_service_name: Option<String>,
    /// Extra attributes from `[telemetry] resource_attributes`.
    pub config_attributes: BTreeMap<String, String>,
}

impl ResourceInputs {
    /// Fills the fields that come from the environment: the CI variables, `OTEL_RESOURCE_ATTRIBUTES`
    /// and `OTEL_SERVICE_NAME`.
    pub fn read_environment(&mut self) {
        self.ci = CiEnvironment::from_env();
        self.otel_resource_attributes = ocx_env::OTEL_RESOURCE_ATTRIBUTES.get();
        self.otel_service_name = ocx_env::OTEL_SERVICE_NAME.get();
    }
}

/// The resource attributes, lowest source first and per key the last one wins: derived keys,
/// `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_SERVICE_NAME`, `[telemetry] resource_attributes`,
/// `[telemetry] service_name`. An operator can therefore override a derived key, ids included.
/// `installation_id` is `None` when the id file could not be read or created.
pub(crate) fn attributes(inputs: &ResourceInputs, installation_id: Option<&str>) -> Attributes {
    let mut map = Attributes::new();
    let mut put = |key: &str, value: serde_json::Value| {
        map.insert(key.to_owned(), value);
    };

    put("service.name", "ocx".into());
    put("service.version", inputs.version.as_str().into());
    put(keys::PROCESS_PID, inputs.pid.into());
    put("sh.ocx.interactive", inputs.interactive.into());
    for (key, value) in [
        (keys::OS_TYPE, &inputs.os_type),
        (keys::PROCESS_ARCH, &inputs.arch),
        (keys::HOST_NAME, &inputs.host_name),
        ("sh.ocx.installation.id", &installation_id.map(str::to_owned)),
    ] {
        if let Some(value) = value {
            put(key, value.as_str().into());
        }
    }
    if let Some(parent) = inputs.parent_pid {
        put(keys::PROCESS_PARENT_PID, parent.into());
    }
    for (key, value) in inputs.ci.attributes() {
        put(key, value.into());
    }

    if let Some(raw) = &inputs.otel_resource_attributes {
        for (key, value) in parse_operator_attributes(raw) {
            put(&key, value.into());
        }
    }
    if let Some(name) = inputs.otel_service_name.as_deref().filter(|name| !name.is_empty()) {
        put("service.name", name.into());
    }
    for (key, value) in &inputs.config_attributes {
        if !key.is_empty() {
            put(key, value.as_str().into());
        }
    }
    if let Some(name) = inputs.config_service_name.as_deref().filter(|name| !name.is_empty()) {
        put("service.name", name.into());
    }
    map
}

/// `k=v,k=v` with keys and values percent-decoded. A pair without `=`, with an empty key or with
/// bytes that do not decode to UTF-8 is skipped; the debug line names its position, never its text.
fn parse_operator_attributes(raw: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (index, pair) in raw.split(',').enumerate().filter(|(_, pair)| !pair.trim().is_empty()) {
        let decoded = pair.split_once('=').and_then(|(key, value)| {
            let key = percent_decode_str(key.trim()).decode_utf8().ok()?;
            let value = percent_decode_str(value.trim()).decode_utf8().ok()?;
            (!key.is_empty()).then(|| (key.into_owned(), value.into_owned()))
        });
        match decoded {
            Some(pair) => pairs.push(pair),
            None => {
                tracing::debug!(target: "ocx_telemetry", pair = index + 1, "OTEL_RESOURCE_ATTRIBUTES pair is malformed: skipped")
            }
        }
    }
    pairs
}

/// One JSON scalar as an OpenTelemetry attribute; `None` for a shape no table row uses.
pub(crate) fn key_value(key: &str, value: &serde_json::Value) -> Option<KeyValue> {
    let value = match value {
        serde_json::Value::String(text) => Value::from(text.clone()),
        serde_json::Value::Bool(flag) => Value::from(*flag),
        serde_json::Value::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(int), _) => Value::from(int),
            (None, Some(float)) => Value::from(float),
            (None, None) => return None,
        },
        _ => return None,
    };
    Some(KeyValue::new(Key::from(key.to_owned()), value))
}

/// The wire resource: no detectors, only the attributes above.
pub(crate) fn to_otel(attributes: &Attributes) -> Resource {
    Resource::builder_empty()
        .with_attributes(attributes.iter().filter_map(|(key, value)| key_value(key, value)))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> ResourceInputs {
        ResourceInputs {
            version: "1.2.3".to_owned(),
            pid: 7,
            ..ResourceInputs::default()
        }
    }

    fn service_name(inputs: &ResourceInputs) -> serde_json::Value {
        attributes(inputs, None)["service.name"].clone()
    }

    #[test]
    fn resource_precedence() {
        let mut inputs = inputs();
        // Derived values apply first.
        assert_eq!(service_name(&inputs), "ocx");
        assert_eq!(attributes(&inputs, None)["process.pid"], 7);

        // `OTEL_RESOURCE_ATTRIBUTES` overrides derived values, a derived key included.
        inputs.otel_resource_attributes = Some("service.name=r2,process.pid=r2,extra=r2".to_owned());
        assert_eq!(service_name(&inputs), "r2");
        assert_eq!(attributes(&inputs, None)["process.pid"], "r2");

        // `OTEL_SERVICE_NAME` overrides `OTEL_RESOURCE_ATTRIBUTES`.
        inputs.otel_service_name = Some("r3".to_owned());
        assert_eq!(service_name(&inputs), "r3");
        // An empty OTEL_SERVICE_NAME does not count.
        inputs.otel_service_name = Some(String::new());
        assert_eq!(service_name(&inputs), "r2");
        inputs.otel_service_name = Some("r3".to_owned());

        // Config attributes override both env sources per key; `OTEL_SERVICE_NAME` keeps `service.name` until config names it.
        inputs.config_attributes = BTreeMap::from([("extra".to_owned(), "r4".to_owned())]);
        assert_eq!(attributes(&inputs, None)["extra"], "r4");
        assert_eq!(service_name(&inputs), "r3");
        inputs
            .config_attributes
            .insert("service.name".to_owned(), "r4".to_owned());
        assert_eq!(service_name(&inputs), "r4");

        // The config service name overrides config attributes.
        inputs.config_service_name = Some("r5".to_owned());
        assert_eq!(service_name(&inputs), "r5");
    }

    #[test]
    fn resource_attributes_are_percent_decoded() {
        let mut inputs = inputs();
        inputs.otel_resource_attributes =
            Some("team%2Fname=a%20b%2Cc%3Dd, plain = v ,=nokey,novalue,bad=%FF,,k%C3%A9y=caf%C3%A9".to_owned());
        let map = attributes(&inputs, None);
        assert_eq!(map["team/name"], "a b,c=d");
        assert_eq!(map["plain"], "v");
        assert_eq!(map["kéy"], "café");
        for skipped in ["novalue", "bad", ""] {
            assert!(!map.contains_key(skipped), "{skipped:?} must be skipped");
        }
    }

    #[test]
    fn best_effort_keys_are_omitted_and_never_placeholders() {
        let map = attributes(&inputs(), None);
        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["process.pid", "service.name", "service.version", "sh.ocx.interactive"]
        );
    }

    #[test]
    fn wire_resource_carries_the_attributes_and_nothing_else() {
        let resource = to_otel(&attributes(&inputs(), Some("0b0e6c3e-5d5c-4b3a-8a0e-2d7f3c9e1a11")));
        // The SDK resource is hash-ordered; only the set is asserted.
        let mut wire: Vec<String> = resource.iter().map(|(key, _)| key.as_str().to_owned()).collect();
        wire.sort();
        assert_eq!(
            wire,
            [
                "process.pid",
                "service.name",
                "service.version",
                "sh.ocx.installation.id",
                "sh.ocx.interactive"
            ]
        );
    }
}
