// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `self::` inside macro tokens, which `syn` never parses into a
//! path — the token-scan arm of the same root.

pub mod shell;
