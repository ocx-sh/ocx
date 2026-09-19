// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: a `use` slice inside macro tokens that cannot re-lex as a `use`
//! statement — here a `:path` metavariable, whose value no scanner can know
//! before expansion. The harness refuses it loudly rather than dropping it,
//! and that refusal is what turns every un-enumerated shape from a silent
//! miss into a red. A slice that genuinely is not a reach earns a carve-out
//! in `use_trees_in_tokens` with its reason written down, never a silent skip.

macro_rules! import {
    ($p:path) => {
        use $p;
    };
}

import!(crate::project::api);
