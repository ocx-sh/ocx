// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub trait ResultExt {
    fn ignore(self);
}

impl<T, E> ResultExt for Result<T, E> {
    fn ignore(self) {
        let _ = self;
    }
}
