// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use ocx::app;

#[tokio::main]
async fn main() -> ExitCode {
    // The error line and the exit code come from `app::finish`, the one
    // boundary this binary and the in-process test seam share.
    app::finish(app::run().await)
}

#[cfg(test)]
mod tests {
    /// `main` keeps no error boundary of its own: a second copy beside
    /// `app::finish` is one that drifts from it (and from the seam), and an
    /// unsanitised copy nullifies every print-site sanitizer downstream of it.
    /// `app::finish` pins its own contents.
    ///
    /// Single tokens, counted: a formatter can rewrap a call, but it cannot
    /// split `error!` or `:#}`.
    #[test]
    fn main_exits_through_the_shared_error_boundary() {
        let code: String = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(code.matches("error!").count(), 0, "main logs no error line itself");
        assert_eq!(code.matches(":#}").count(), 0, "nor renders a cause chain");
        assert_eq!(
            code.matches("classify_error").count(),
            0,
            "nor classifies the exit code"
        );
        assert_eq!(
            code.matches("app::finish(").count(),
            1,
            "it exits through the shared boundary"
        );
    }
}
