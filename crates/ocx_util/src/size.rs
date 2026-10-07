// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The wire byte count, bounded to what a JSON consumer reads exactly.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A size in bytes; serialising one above [`ByteSize::MAX`] is an error, never a rounded number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize(u64);

impl ByteSize {
    /// 2^53 − 1: the largest integer an IEEE 754 double, and so every JSON parser, holds exactly.
    pub const MAX: u64 = (1 << 53) - 1;

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for ByteSize {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0 > Self::MAX {
            return Err(serde::ser::Error::custom(format!(
                "byte size {} exceeds 2^53 - 1",
                self.0
            )));
        }
        serializer.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u64::deserialize(deserializer)?;
        if value > Self::MAX {
            return Err(serde::de::Error::custom(format!("byte size {value} exceeds 2^53 - 1")));
        }
        Ok(Self(value))
    }
}

impl JsonSchema for ByteSize {
    fn schema_name() -> Cow<'static, str> {
        "ByteSize".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "ocx::ByteSize".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "integer",
            "minimum": 0,
            "maximum": Self::MAX,
            "description": "Size in bytes."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_as_a_bare_integer_up_to_max() {
        assert_eq!(
            serde_json::to_string(&ByteSize::from(ByteSize::MAX)).expect("serialises"),
            "9007199254740991"
        );
        assert_eq!(ByteSize::MAX, 9_007_199_254_740_991);
    }

    #[test]
    fn serialising_above_max_is_an_error() {
        assert!(serde_json::to_string(&ByteSize::from(ByteSize::MAX + 1)).is_err());
    }

    #[test]
    fn deserialising_above_max_is_an_error() {
        assert!(serde_json::from_str::<ByteSize>("9007199254740992").is_err());
        assert_eq!(serde_json::from_str::<ByteSize>("42").expect("parses").get(), 42);
    }

    #[test]
    fn schema_bounds_the_integer() {
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Holder {
            size: ByteSize,
        }
        let schema = serde_json::to_value(schemars::schema_for!(Holder)).expect("schema serialises");
        let def = &schema["$defs"]["ByteSize"];
        assert_eq!(def["type"], "integer");
        assert_eq!(def["minimum"], 0);
        assert_eq!(def["maximum"], 9_007_199_254_740_991_u64);
        assert_eq!(ByteSize::schema_id(), "ocx::ByteSize");
    }
}
