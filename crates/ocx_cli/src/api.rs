// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Printer;

use crate::options;

pub mod data;

/// Implemented by API data types that know how to render themselves in either output format.
///
/// The `report` method on [`Api`] dispatches between JSON and plain text via
/// this trait, so each data type owns its own formatting logic rather than
/// delegating it to a giant match block in the API layer.
///
/// JSON rendering is handled by [`Api`] directly (with optional syntax
/// highlighting); types only need to implement `print_plain`.
pub trait Reportable: serde::Serialize {
    fn print_plain(&self, printer: &Printer);
}

#[derive(Clone)]
pub struct Api {
    format: options::Format,
    printer: Printer,
}

impl Api {
    pub fn new(format: options::Format, printer: Printer) -> Self {
        Self { format, printer }
    }

    pub fn printer(&self) -> &Printer {
        &self.printer
    }

    pub fn report(&self, item: &impl Reportable) -> anyhow::Result<()> {
        match self.format {
            options::Format::Json => self.print_json(item)?,
            options::Format::Plain => item.print_plain(&self.printer),
        }
        Ok(())
    }

    fn print_json(&self, item: &impl serde::Serialize) -> anyhow::Result<()> {
        if self.printer.color() {
            let value = serde_json::to_value(item)?;
            println!(
                "{}",
                colored_json::to_colored_json(&value, colored_json::ColorMode::On)?
            );
        } else {
            println!("{}", serde_json::to_string_pretty(item)?);
        }
        Ok(())
    }

    pub fn is_json(&self) -> bool {
        matches!(self.format, options::Format::Json)
    }
}
