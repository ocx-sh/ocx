use crate::compression;

#[derive(Default)]
pub struct ExtractOptions {
    pub algorithm: Option<compression::CompressionAlgorithm>,
    pub strip_components: usize,
}
