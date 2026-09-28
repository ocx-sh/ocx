// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_console::DataInterface;

use crate::options;

pub mod data;
pub mod junit;

/// An API data type that renders itself as plain text or JSON; override `print_json` only
/// when the JSON form needs more than `Serialize`.
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
    /// Set once a report reaches stdout, shared across clones: the error-envelope wrapper reads it,
    /// or a report-then-fail command would put two JSON documents on stdout.
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

    /// Whether any report reached stdout; survives the `Context` move into `Command::execute`.
    pub fn reported_handle(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.reported)
    }

    /// Renders `item` to stdout in the configured format; quiet mode suppresses it.
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

    use ocx_console::Printer;

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
