use clap::ValueEnum;
use ocx_lib::compression;

#[derive(Clone, Copy, Debug, ValueEnum, Default)]
pub enum CompressionLevel {
    Fast,
    Best,
    #[default]
    Default,
}

impl From<CompressionLevel> for compression::CompressionLevel {
    fn from(val: CompressionLevel) -> Self {
        match val {
            CompressionLevel::Fast => compression::CompressionLevel::Fast,
            CompressionLevel::Best => compression::CompressionLevel::Best,
            CompressionLevel::Default => compression::CompressionLevel::Default,
        }
    }
}
