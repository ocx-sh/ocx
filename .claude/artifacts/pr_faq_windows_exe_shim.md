# PR-FAQ: Windows native `.exe` launcher shim

**Status:** Draft | **Date:** 2026-05-18 | **Resolves:** ocx-sh/ocx#66

## Press Release

OCX now ships entrypoint launchers on Windows as native `.exe` shims. Tools
installed via `ocx install` resolve from any shell — `cmd.exe`, PowerShell,
Git Bash — without configuring `PATHEXT`, and report their real program name
to argv-introspecting tools. The shim eliminates the `cmd.exe` argument
re-parse surface (BatBadBut / CVE-2024-24576 class) that the prior `.cmd`
launcher could not fully close. The `.cmd` launcher stays as a fallback;
nothing already installed needs reinstalling.

## FAQ

**Q: What changed for users?**
A: After `ocx install --select`, `/entrypoints/` contains `<name>.exe` +
`<name>.shim` next to the existing `<name>.cmd`. Default Windows `PATHEXT`
resolves `.EXE` before `.CMD`, so the native shim is used automatically.

**Q: Why not just fix the `.cmd`?** A: `cmd.exe` re-parses `%*` through a
second shell pass; the Rust std team concluded pure-batch escaping is
intractable for adversarial input. A compiled shim bypasses `cmd.exe`
entirely (`CreateProcessW`, no shell layer).

**Q: Does this break installed launchers?** A: No. Launchers are
install-local and regenerated on next `ocx install`/`select`. The OCI
manifest/metadata compatibility guarantee is unaffected (launchers are not
published artifacts).

**Q: Is the `.exe` signed?** A: Phase 1 ships unsigned (the security fix is
the priority and must not block on signing onboarding). Phase 2 adds an
Authenticode signature via SignPath Foundation (free OSS tier). Verbatim
copy of the embedded bytes preserves the signature.

**Q: Performance?** A: Native `.exe` spawn is ~10× faster than `cmd.exe`
interpreter startup; `argv[0]` is the launcher, not `cmd.exe`; Ctrl+C
forwards correctly.

**Q: What's out of scope?** A: Migrating old `.cmd` launchers (additive
coexistence instead), replacing the Unix `.sh` launcher (no PATHEXT/argv
issue there), aarch64 Windows shim (x86_64 first; follow-on).
