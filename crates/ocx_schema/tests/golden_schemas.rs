// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The seven published schemas are pinned byte-for-byte to committed goldens.
//!
//! `website/schema.taskfile.yml` writes each kind with
//! `cargo run -p ocx_schema --release -- <kind> > …/v1.json`; this test runs
//! the same [`ocx_schema::schema_for`] in-process and compares the bytes to
//! `tests/golden/<kind>.json`. Anything that changes the output — a field, a
//! `$defs` key, `$defs` *order* (schemars registers definitions in the order
//! it meets the types, so a type moving between crates can reorder them) —
//! reds here, in the commit that caused it, rather than in a downstream
//! consumer's next `taplo` run.
//!
//! Regenerate a golden only in a commit whose subject **names the schema
//! change**. The conventional type does not matter: a `refactor:` that moves
//! a type necessarily changes any doc link naming it, and splitting the
//! regeneration into its own commit would break the atomic extraction commit
//! C-049 requires and leave a bisectable tree carrying a moved type beside a
//! stale golden. The property this rule protects is that a golden never moves
//! silently, which the subject alone secures:
//!
//! ```sh
//! cargo run -p ocx_schema -- <kind> > crates/ocx_schema/tests/golden/<kind>.json
//! ```
//!
//! Trailing newline: `main.rs` prints with `println!`, so a golden captured
//! through the binary ends in `\n` while `schema_for` returns none. The
//! comparison appends one so the golden is exactly the file `task schema`
//! publishes.

use std::path::PathBuf;

fn golden_path(kind: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{kind}.json"))
}

fn assert_matches_golden(kind: &str) {
    let generated = ocx_schema::schema_for(kind).unwrap_or_else(|| panic!("schema_for({kind:?}) returned None")) + "\n";
    let path = golden_path(kind);
    let golden =
        std::fs::read(&path).unwrap_or_else(|e| panic!("golden {} is missing or unreadable: {e}", path.display()));

    if generated.as_bytes() == golden {
        return;
    }
    let offset = generated
        .bytes()
        .zip(golden.iter().copied())
        .position(|(a, b)| a != b)
        .unwrap_or(
            // One is a prefix of the other: the first differing byte is the
            // shorter one's length.
            generated.len().min(golden.len()),
        );
    let window = |bytes: &[u8]| {
        String::from_utf8_lossy(&bytes[offset.saturating_sub(40)..(offset + 40).min(bytes.len())]).into_owned()
    };
    panic!(
        "{kind} schema differs from its golden at byte {offset} ({} generated vs {} golden bytes)\n\
         generated: …{}…\n\
         golden:    …{}…\n\
         if the schema change is intended, regenerate with\n\
         cargo run -p ocx_schema -- {kind} > {}",
        generated.len(),
        golden.len(),
        window(generated.as_bytes()),
        window(&golden),
        path.display()
    );
}

macro_rules! golden_tests {
    ($($name:ident => $kind:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                assert_matches_golden($kind);
            }
        )*
    };
}

golden_tests! {
    metadata_schema_matches_golden => "metadata",
    config_schema_matches_golden => "config",
    project_schema_matches_golden => "project",
    project_lock_schema_matches_golden => "project-lock",
    patch_schema_matches_golden => "patch",
    reports_schema_matches_golden => "reports",
    execution_record_schema_matches_golden => "execution-record",
}
