// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::prelude::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuthType {
    Anonymous,
    Basic,
    Token,
}

impl AuthType {
    pub fn valid_strings() -> Vec<&'static str> {
        vec!["anonymous", "basic", "token", "bearer"]
    }
}

impl std::fmt::Display for AuthType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthType::Anonymous => write!(f, "anonymous"),
            AuthType::Basic => write!(f, "basic"),
            AuthType::Token => write!(f, "token"),
        }
    }
}

impl TryFrom<String> for AuthType {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        match value.to_lowercase().as_str() {
            "anonymous" => Ok(AuthType::Anonymous),
            "basic" => Ok(AuthType::Basic),
            "token" | "bearer" => Ok(AuthType::Token),
            other => Err(super::AuthError::InvalidType(other.to_string()).into()),
        }
    }
}
