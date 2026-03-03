use clap::Parser;
use ocx_lib::env;

use crate::options;

#[derive(Debug, Parser)]
pub struct ContextOptions {
    /// The path to the ocx configuration file
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<std::path::PathBuf>,

    /// Use the remote index by default instead of the local index.
    #[arg(long, default_value_t = env::flag("OCX_REMOTE", false))]
    pub remote: bool,

    /// Run in offline mode, which will not attempt to fetch any remote information.
    ///
    /// If a required package is not already installed, the command will fail.
    #[arg(long, default_value_t = env::flag("OCX_OFFLINE", false))]
    pub offline: bool,

    /// The format to use when outputting information.
    #[arg(long, value_enum, value_name = "FORMAT", default_value_t = Default::default())]
    pub format: options::Format,

    /// Alternative path to the local index directory (ignored if --remote is set)
    #[arg(long, value_name = "PATH")]
    pub index: Option<std::path::PathBuf>,

    /// The log level to use
    #[arg(short, long, value_enum)]
    pub log_level: Option<options::LogLevel>,
}
