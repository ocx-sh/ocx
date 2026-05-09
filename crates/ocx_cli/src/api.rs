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
/// `print_json` has a default implementation that serializes `self` via
/// [`Printer::print_json`] (with optional syntax highlighting). Override it
/// only when the JSON representation needs special handling beyond `Serialize`.
pub trait Printable: serde::Serialize {
    fn print_plain(&self, printer: &Printer);

    fn print_json(&self, printer: &Printer) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        Ok(printer.print_json(self)?)
    }
}

#[derive(Clone)]
pub struct Api {
    format: options::Format,
    printer: Printer,
    quiet: bool,
}

impl Api {
    pub fn new(format: options::Format, printer: Printer, quiet: bool) -> Self {
        Self { format, printer, quiet }
    }

    pub fn printer(&self) -> &Printer {
        &self.printer
    }

    /// Renders `item` to stdout in the configured format, unless quiet mode is
    /// active — quiet suppresses every report type, leaving stderr (progress,
    /// errors, warnings) untouched.
    pub fn report(&self, item: &impl Printable) -> anyhow::Result<()> {
        if self.quiet {
            return Ok(());
        }
        match self.format {
            options::Format::Json => item.print_json(&self.printer)?,
            options::Format::Plain => item.print_plain(&self.printer),
        }
        Ok(())
    }

    pub fn is_json(&self) -> bool {
        matches!(self.format, options::Format::Json)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// Stub `Printable` whose `print_plain` / `print_json` flip thread-local-style
    /// counters so the test can assert whether `Api::report` invoked them.
    struct CallCounter {
        plain: Cell<u32>,
        json: Cell<u32>,
    }

    impl CallCounter {
        fn new() -> Self {
            Self {
                plain: Cell::new(0),
                json: Cell::new(0),
            }
        }
    }

    impl serde::Serialize for CallCounter {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_unit()
        }
    }

    impl Printable for CallCounter {
        fn print_plain(&self, _printer: &Printer) {
            self.plain.set(self.plain.get() + 1);
        }

        fn print_json(&self, _printer: &Printer) -> anyhow::Result<()> {
            self.json.set(self.json.get() + 1);
            Ok(())
        }
    }

    #[test]
    fn report_skips_render_when_quiet() {
        let api = Api::new(options::Format::Plain, Printer::new(false), true);
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 0);
        assert_eq!(counter.json.get(), 0);
    }

    #[test]
    fn report_renders_plain_when_not_quiet() {
        let api = Api::new(options::Format::Plain, Printer::new(false), false);
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 1);
        assert_eq!(counter.json.get(), 0);
    }

    #[test]
    fn report_skips_json_when_quiet() {
        let api = Api::new(options::Format::Json, Printer::new(false), true);
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 0);
        assert_eq!(counter.json.get(), 0);
    }
}
