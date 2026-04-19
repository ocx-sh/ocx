// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

pub mod accumulator;
pub mod conflict;
pub mod constant;
pub mod exporter;
pub mod modifier;
pub mod path;
pub mod var;

#[derive(Debug, Default, Clone)]
pub struct Env {
    variables: Vec<var::Var>,
}

impl Env {
    pub fn is_empty(&self) -> bool {
        self.variables.is_empty()
    }

    pub fn resolve_into_env(
        &self,
        install_path: impl AsRef<std::path::Path>,
        env: &mut crate::env::Env,
    ) -> crate::Result<()> {
        let empty_ctx = std::collections::HashMap::new();
        let mut resolver = accumulator::Accumulator::new(install_path, &empty_ctx, env);
        for var in &self.variables {
            resolver.add(var)?;
        }
        Ok(())
    }
}

impl IntoIterator for Env {
    type Item = var::Var;
    type IntoIter = std::vec::IntoIter<var::Var>;

    fn into_iter(self) -> Self::IntoIter {
        self.variables.into_iter()
    }
}

impl<'a> IntoIterator for &'a Env {
    type Item = &'a var::Var;
    type IntoIter = std::slice::Iter<'a, var::Var>;

    fn into_iter(self) -> Self::IntoIter {
        self.variables.iter()
    }
}

impl Serialize for Env {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.variables.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Env {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let variables = Vec::<var::Var>::deserialize(deserializer)?;
        Ok(Env { variables })
    }
}

impl schemars::JsonSchema for Env {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Env")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Env serializes as a flat array of Var objects
        <Vec<var::Var>>::json_schema(generator)
    }
}

pub struct EnvBuilder {
    variables: Vec<var::Var>,
}

impl Default for EnvBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvBuilder {
    pub fn new() -> Self {
        EnvBuilder { variables: Vec::new() }
    }

    pub fn add_var(&mut self, var: var::Var) -> &mut Self {
        self.variables.push(var);
        self
    }

    pub fn with_var(mut self, var: var::Var) -> Self {
        self.add_var(var);
        self
    }

    pub fn add_path(&mut self, name: impl ToString, value: impl ToString, required: bool) -> &mut Self {
        self.add_var(var::Var::new_path(name, value, required))
    }

    pub fn with_path(mut self, name: impl ToString, value: impl ToString, required: bool) -> Self {
        self.add_path(name, value, required);
        self
    }

    pub fn add_constant(&mut self, name: impl ToString, value: impl ToString) -> &mut Self {
        self.add_var(var::Var::new_constant(name, value))
    }

    pub fn with_constant(mut self, name: impl ToString, value: impl ToString) -> Self {
        self.add_constant(name, value);
        self
    }

    pub fn build(self) -> Env {
        Env {
            variables: self.variables,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_builder() {
        let env = EnvBuilder::new()
            .with_path("PATH", "${installPath}/bin", true)
            .with_constant("JAVA_HOME", "${installPath}")
            .build();
        let json = serde_json::to_string_pretty(&env).unwrap();
        println!("Serialized env: {}", json);
    }
}
