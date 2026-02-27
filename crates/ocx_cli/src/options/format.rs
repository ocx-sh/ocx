use clap::ValueEnum;

#[derive(Clone, Copy, Debug, ValueEnum, Default)]
pub enum Format {
    Json,
    #[default]
    Plain,
}
