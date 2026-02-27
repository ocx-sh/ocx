use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Shell {
    Env(super::shell_env::ShellEnv),
    Completion(super::shell_completion::ShellCompletion),
}

impl Shell {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Shell::Env(env) => env.execute(context).await,
            Shell::Completion(completion) => completion.execute().await,
        }
    }
}
