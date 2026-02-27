use std::sync::LazyLock;

use regex::Regex;

pub trait StringExt {
    fn to_slug(&self) -> String;
}

static SLUG_TRANSFORM: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"[^a-zA-Z0-9]",
            )
            .expect("Invalid slug regex!")
        });

impl<T: AsRef<str>> StringExt for T {
    fn to_slug(&self) -> String {
        SLUG_TRANSFORM.replace_all(self.as_ref(), "_").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_slug() {
        assert_eq!("hello_world", "hello world".to_slug());
        assert_eq!("foo_bar_baz", "foo-bar.baz".to_slug());
    }
}
