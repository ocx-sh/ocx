// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::DataInterface;

use crate::options;

pub mod data;

/// Implemented by API data types that know how to render themselves in either output format.
///
/// The `report` method on [`Api`] dispatches between JSON and plain text via
/// this trait, so each data type owns its own formatting logic rather than
/// delegating it to a giant match block in the API layer.
///
/// `print_json` has a default implementation that serializes `self` via
/// [`DataInterface::print_json`] (with optional syntax highlighting). Override it
/// only when the JSON representation needs special handling beyond `Serialize`.
pub trait Printable: serde::Serialize {
    fn print_plain(&self, data: &DataInterface);

    fn print_json(&self, data: &DataInterface) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        Ok(data.print_json(self)?)
    }
}

#[derive(Clone)]
pub struct Api {
    format: options::FormatMode,
    data: DataInterface,
    quiet: bool,
    /// Set once a report has actually been printed to stdout. Shared across
    /// clones so the app-level error-envelope wrapper can tell "this failure
    /// already produced the command's stdout document" (report-then-fail
    /// commands like `package push --announce-file`) from "stdout is empty and
    /// the envelope is the document" — stdout must carry exactly one JSON
    /// document either way.
    reported: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Api {
    pub fn new(format: options::FormatMode, data: DataInterface, quiet: bool) -> Self {
        Self {
            format,
            data,
            quiet,
            reported: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn data(&self) -> &DataInterface {
        &self.data
    }

    /// Shared handle answering whether any report reached stdout — survives
    /// the `Context` move into `Command::execute`.
    pub fn reported_handle(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.reported)
    }

    /// Renders `item` to stdout in the configured format, unless quiet mode is
    /// active — quiet suppresses every report type, leaving stderr (progress,
    /// errors, warnings) untouched.
    pub fn report(&self, item: &impl Printable) -> anyhow::Result<()> {
        if self.quiet {
            return Ok(());
        }
        match self.format {
            options::FormatMode::Json => item.print_json(&self.data)?,
            options::FormatMode::Plain => item.print_plain(&self.data),
        }
        self.reported.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn is_json(&self) -> bool {
        matches!(self.format, options::FormatMode::Json)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use ocx_lib::cli::Printer;

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
        fn print_plain(&self, _data: &DataInterface) {
            self.plain.set(self.plain.get() + 1);
        }

        fn print_json(&self, _data: &DataInterface) -> anyhow::Result<()> {
            self.json.set(self.json.get() + 1);
            Ok(())
        }
    }

    #[test]
    fn report_skips_render_when_quiet() {
        let api = Api::new(
            options::FormatMode::Plain,
            DataInterface::new(Printer::new(false, false)),
            true,
        );
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 0);
        assert_eq!(counter.json.get(), 0);
    }

    #[test]
    fn report_renders_plain_when_not_quiet() {
        let api = Api::new(
            options::FormatMode::Plain,
            DataInterface::new(Printer::new(false, false)),
            false,
        );
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 1);
        assert_eq!(counter.json.get(), 0);
    }

    #[test]
    fn report_skips_json_when_quiet() {
        let api = Api::new(
            options::FormatMode::Json,
            DataInterface::new(Printer::new(false, false)),
            true,
        );
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 0);
        assert_eq!(counter.json.get(), 0);
    }
}
