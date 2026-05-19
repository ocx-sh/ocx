// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Stack-probe builtins for the `*-pc-windows-gnullvm` targets.
//!
//! When the shim is cross-compiled with `cargo-zigbuild` (hermetic Zig
//! toolchain, see `adr_shim_hermetic_zigbuild.md`), Rust's codegen for the
//! gnullvm targets emits calls to the stack-probe helpers `___chkstk_ms`
//! (x86_64) / `__chkstk` (aarch64), but Zig is invoked with `-nolibc` so
//! neither libgcc nor compiler-rt supplies them — the link fails with
//! `undefined symbol: ___chkstk_ms` / `__chkstk`.
//!
//! Rather than re-add a runtime dependency we provide the symbols inline.
//! These are the upstream implementations verbatim (libgcc
//! `config/i386/cygwin.asm` and LLVM compiler-rt
//! `lib/builtins/aarch64/chkstk.S`); the ABI is load-bearing for stack
//! safety, so they are copied exactly, not paraphrased.
//!
//! Scoped to `target_abi = "llvm"` (the discriminator that selects the
//! gnullvm targets) so msvc/gnu builds, which get the probe from their own
//! runtime, are untouched.

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    r#"
    .global ___chkstk_ms
___chkstk_ms:
    push   %rcx
    push   %rax
    cmp    $0x1000, %rax
    lea    24(%rsp), %rcx
    jb     2f
1:
    sub    $0x1000, %rcx
    orq    $0x0, (%rcx)
    sub    $0x1000, %rax
    cmp    $0x1000, %rax
    ja     1b
2:
    sub    %rax, %rcx
    orq    $0x0, (%rcx)
    pop    %rax
    pop    %rcx
    ret
"#,
    options(att_syntax)
);

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    r#"
    .p2align 2
    .global __chkstk
__chkstk:
    lsl    x16, x15, #4
    mov    x17, sp
1:
    sub    x17, x17, #4096
    subs   x16, x16, #4096
    ldr    xzr, [x17]
    b.gt   1b
    ret
"#
);
