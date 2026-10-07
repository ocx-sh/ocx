// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The wire timestamp: an RFC 3339 instant in UTC, always spelled with `Z`.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, SubsecRound, Utc};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An instant at whole-second precision, so a value equals its own round-trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(DateTime<Utc>);

impl From<DateTime<Utc>> for Timestamp {
    fn from(value: DateTime<Utc>) -> Self {
        Self(value.trunc_subsecs(0))
    }
}

impl From<Timestamp> for DateTime<Utc> {
    fn from(value: Timestamp) -> Self {
        value.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_rfc3339_opts(SecondsFormat::Secs, true))
    }
}

impl FromStr for Timestamp {
    type Err = chrono::ParseError;

    /// Accepts any RFC 3339 offset and normalises it to UTC whole seconds.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        DateTime::parse_from_rfc3339(raw).map(|at| Self::from(at.with_timezone(&Utc)))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Timestamp {
    fn schema_name() -> Cow<'static, str> {
        "Timestamp".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "ocx::Timestamp".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "date-time",
            "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$",
            "description": "RFC 3339 instant in UTC (`Z`), whole seconds."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn instant() -> Timestamp {
        Timestamp::from(
            Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7)
                .single()
                .expect("valid instant"),
        )
    }

    #[test]
    fn serialises_rfc3339_with_z() {
        assert_eq!(
            serde_json::to_string(&instant()).expect("serialises"),
            r#""2026-03-04T05:06:07Z""#
        );
    }

    #[test]
    fn sub_second_input_round_trips_to_an_equal_value() {
        let precise = Utc
            .timestamp_opt(1_700_000_000, 123_456_789)
            .single()
            .expect("valid instant");
        let value = Timestamp::from(precise);
        let back: Timestamp =
            serde_json::from_str(&serde_json::to_string(&value).expect("serialises")).expect("parses");
        assert_eq!(back, value);
    }

    #[test]
    fn offset_input_normalises_to_utc() {
        let parsed: Timestamp = serde_json::from_str(r#""2026-03-04T07:06:07+02:00""#).expect("parses");
        assert_eq!(parsed, instant());
    }

    #[test]
    fn non_rfc3339_input_is_refused() {
        assert!(serde_json::from_str::<Timestamp>(r#""2026-03-04 05:06:07""#).is_err());
    }

    #[test]
    fn schema_def_pattern_accepts_the_emitted_form_and_refuses_an_offset() {
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Holder {
            created_at: Timestamp,
        }
        let schema = serde_json::to_value(schemars::schema_for!(Holder)).expect("schema serialises");
        let def = &schema["$defs"]["Timestamp"];
        assert_eq!(def["type"], "string");
        let pattern = regex::Regex::new(def["pattern"].as_str().expect("Timestamp $def carries a pattern"))
            .expect("pattern compiles");
        assert!(pattern.is_match(&instant().to_string()));
        assert!(!pattern.is_match("2026-03-04T05:06:07+00:00"));
        assert!(!pattern.is_match("2026-03-04T05:06:07"));
        assert_eq!(Timestamp::schema_id(), "ocx::Timestamp");
    }
}
