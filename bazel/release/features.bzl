"""`testing_features`: the `crate_features` of a crate that has test-only features.

A release build (`--define=ocx_provenance=release`) must not carry test seams, so
every crate in the shipped binary's closure drops its `__testing` feature (and
`__test_scaffolding`, where it has one) under the same switch that stamps the
provenance. One list per crate, spelled once here: `rust_library` and its
`rust_doc_test` both call it, because `bazel:build:drift` compares the two.
"""

visibility(["//crates/..."])

def testing_features(*extra):
    """The features every non-release build compiles in; none under the release define.

    Args:
        *extra: further test-only features, sorted before `__testing`.

    Returns:
        A select over `//bazel/release:stamped`.
    """
    return select({
        "//bazel/release:stamped": [],
        "//conditions:default": list(extra) + ["__testing"],
    })
