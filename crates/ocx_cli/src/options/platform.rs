// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Shared `--platform` / `-p` argument for commands that resolve against a
/// single platform.
///
/// Flatten into a command struct with `#[clap(flatten)]` to add the standard
/// `-p/--platform` argument. Absent is the signal for "use the current host
/// platform" — callers use `conventions::platform_or_default` to apply that
/// default.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct PlatformOption {
    /// Target platform to resolve packages against.
    ///
    /// The value is `os/arch[/variant][+feature[,feature...]]`,
    /// for example `linux/amd64`, `linux/arm64`, or `linux/amd64+libc.glibc`.
    /// The optional `+feature` suffix filters by `os.features`: OCX selects
    /// the manifest whose features are a subset of the value you pass, so
    /// `+libc.glibc` or `+libc.musl` forces a specific libc variant.
    /// WebAssembly targets are `wasip1/wasm` and `wasip2/wasm`; no host
    /// reports either, so they are reachable only by naming them here.
    /// Defaults to the auto-detected host platform. Details:
    /// <https://ocx.sh/docs/authoring/multi-platform>
    #[clap(short = 'p', long = "platform", value_name = "PLATFORM")]
    pub platform: Option<ocx_oci::Platform>,
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use crate::app::Cli;

    /// Every supported pair parses as `--platform` on a platform-taking
    /// command, so whatever the command fails with afterwards is resolution,
    /// never usage. `windows/arm64` parsed before the pair check existed and
    /// must keep parsing after it.
    #[test]
    // ported-from: test/tests/test_platform_pairs.py::test_supported_pair_reaches_resolution
    fn every_supported_pair_parses_as_a_platform_argument() {
        for platform in [
            "wasip1/wasm",
            "wasip2/wasm",
            "linux/amd64",
            "linux/arm64",
            "windows/arm64",
        ] {
            let argument = format!("--platform={platform}");
            let parsed = Cli::try_parse_from([
                "ocx",
                "package",
                "install",
                argument.as_str(),
                "localhost:5000/absent:1.0.0",
            ]);
            assert!(
                parsed.is_ok(),
                "{platform} is a supported pair and must parse; got a usage error: {}",
                parsed.err().map(|error| error.to_string()).unwrap_or_default()
            );
        }
    }
}

#[cfg(test)]
mod seam {
    use std::ffi::OsString;
    use std::process::ExitCode;

    use crate::app::seam::{Environment, run};

    /// An os and an arch that are each valid, paired into something nobody
    /// ships, is refused while parsing `--platform` with 64 — and the refusal
    /// lists the supported pairs, which is what tells "the pair check ran"
    /// apart from "the component was unknown" (both exit 64).
    #[tokio::test]
    // ported-from: test/tests/test_platform_pairs.py::test_unsupported_platform_pair_is_refused
    async fn an_unsupported_pair_is_refused_at_parse_naming_the_supported_pairs() {
        let root = tempfile::tempdir().expect("tempdir");
        let env = Environment {
            vars: [(
                OsString::from("OCX_HOME"),
                root.path().join("ocx-home").into_os_string(),
            )]
            .into(),
            cwd: root.path().to_owned(),
        };
        for platform in ["wasip1/amd64", "wasip2/arm64", "linux/wasm", "darwin/wasm"] {
            let argv: Vec<OsString> = [
                "ocx".to_owned(),
                "package".to_owned(),
                "install".to_owned(),
                format!("--platform={platform}"),
                "localhost:5000/absent:1.0.0".to_owned(),
            ]
            .map(OsString::from)
            .to_vec();
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run(&argv, &env, &mut out, &mut err).await;
            let err = String::from_utf8_lossy(&err);

            assert_eq!(code, ExitCode::from(64), "{platform} must exit 64: {err}");
            assert!(
                err.contains("wasip1/wasm"),
                "the refusal for {platform} must list the supported pairs: {err}"
            );
        }
    }
}
