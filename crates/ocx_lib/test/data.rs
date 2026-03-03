pub fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test")
        .join("data")
}

pub fn archive_dir() -> std::path::PathBuf {
    data_dir().join("archive")
}

pub fn archive_xz() -> std::path::PathBuf {
    archive_dir().with_added_extension("tar.xz")
}

macro_rules! include_str {
    ($file:expr) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/data/", $file))
    };
}
pub(crate) use include_str;
