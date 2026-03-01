use std::sync::LazyLock;

use regex::Regex;

pub trait StringExt {
    /// Strict slug: only `[a-zA-Z0-9]` are kept, everything else becomes `_`.
    fn to_slug(&self) -> String;

    /// Relaxed slug: keeps `[a-zA-Z0-9._-]`, replaces the rest with `_`.
    ///
    /// Suitable for filesystem path components derived from OCI identifiers
    /// (registry names, repository names, tags).  Preserves dots (needed for
    /// domain names like `ghcr.io` and semantic versions like `3.28.1`) and
    /// hyphens (common in package names).
    fn to_relaxed_slug(&self) -> String;
}

static SLUG_TRANSFORM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9]").expect("Invalid slug regex!"));

static RELAXED_SLUG_TRANSFORM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9._-]").expect("Invalid relaxed slug regex!"));

impl<T: AsRef<str>> StringExt for T {
    fn to_slug(&self) -> String {
        SLUG_TRANSFORM.replace_all(self.as_ref(), "_").to_string()
    }

    fn to_relaxed_slug(&self) -> String {
        RELAXED_SLUG_TRANSFORM
            .replace_all(self.as_ref(), "_")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_slug_replaces_non_alphanumeric() {
        assert_eq!("hello_world", "hello world".to_slug());
        assert_eq!("foo_bar_baz", "foo-bar.baz".to_slug());
    }

    #[test]
    fn to_relaxed_slug_preserves_dots_and_hyphens() {
        assert_eq!("ghcr.io", "ghcr.io".to_relaxed_slug());
        assert_eq!("my-package", "my-package".to_relaxed_slug());
        assert_eq!("3.28.1", "3.28.1".to_relaxed_slug());
    }

    #[test]
    fn to_relaxed_slug_replaces_colons() {
        assert_eq!("localhost_5000", "localhost:5000".to_relaxed_slug());
    }

    #[test]
    fn to_relaxed_slug_replaces_spaces_and_special_chars() {
        assert_eq!("hello_world", "hello world".to_relaxed_slug());
        assert_eq!("a_b_c", "a@b/c".to_relaxed_slug());
    }
}
