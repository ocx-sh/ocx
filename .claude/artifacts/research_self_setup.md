# Research: `ocx self setup` — implementation gaps

**Date:** 2026-06-04
**Scope:** Targeted supplement to [`adr_self_setup.md`](./adr_self_setup.md) (decisions 1B+2A+3D+4C locked). The broad peer-tool sweep lives in the ADR's Industry Context; this fills four narrow implementation gaps.

## Gap 1 — conda fenced-block parse/replace (prior art for Decision 3D)

conda `core/initialize.py`:

```python
CONDA_INITIALIZE_RE_BLOCK = (
    r"^# >>> conda initialize >>>(?:\n|\r\n)"
    r"([\s\S]*?)"
    r"# <<< conda initialize <<<(?:\n|\r\n)?"
)
```

- Explicit CRLF: `(?:\n|\r\n)` on both fences; trailing `?` tolerates EOF without newline.
- `[\s\S]*?` non-greedy dot-all; `^` anchor requires MULTILINE.
- **Duplicate blocks NOT collapsed** — conda replaces each block with a placeholder then substitutes new content at every placeholder → identical duplicates. Acknowledged TODO in conda (lines 1342/1463/1541). **OCX should collapse to one from the start.**
- **Diff-gate:** write only if `new_content != original_content` (in-memory string equality, no stored hash). OCX's embedded fence hash is strictly better (dirty-detect without full re-read; survives version change).
- **User edits inside fence are silently overwritten** (body is `[\s\S]*?`). Document "managed block — do not edit" at top.
- **Footguns:** mixed CRLF/LF can match one block and miss another. **Mitigation: normalize all line endings to `\n` before matching, write back `\n`.**

## Gap 2 — Rust marker + hash

- **`<hash8>`: `sha2` (already in tree for OCI digests), `hex::encode(&Sha256::digest(body)[..4])`** → 8 hex chars = 32-bit fingerprint. Non-cryptographic dirty-detect; git uses 7 chars. No new crate (`crc32fast`/`blake3` would add one for a cold path — boring tech wins).
- **Block find/replace: manual line-scan, NOT regex** for the multi-line block (CRLF + multiline + non-greedy is a regex config hazard). O(n) over lines, trivially unit-testable, naturally collapses duplicates. Use `regex` only for single-line hash extraction from the opener: `# >>> ocx v1 ([0-9a-f]{8}) >>>`.

## Gap 3 — PowerShell `$PROFILE` detection

Paths differ by version + may be OneDrive/GPO-redirected — **never hardcode**:

| Version | CurrentUserAllHosts |
|---|---|
| WinPS 5.1 | `$HOME\Documents\WindowsPowerShell\Profile.ps1` |
| pwsh 7 (Win) | `$HOME\Documents\PowerShell\Profile.ps1` |
| pwsh 7 (Linux/macOS) | `~/.config/powershell/profile.ps1` |

- **Target `CurrentUserAllHosts`** (`Profile.ps1`), not CurrentUserCurrentHost — affects every host (Windows Terminal, VS Code, ISE). Matches rustup. Microsoft docs endorse for cross-host items.
- **Detect at runtime via subprocess:** try `pwsh` then `powershell` with `-NoProfile -NonInteractive -Command "$PROFILE.CurrentUserAllHosts"`. Path is a runtime value.
- **Execution-policy caveat:** fresh Windows = `Restricted` blocks `.ps1` profiles. Detect (`Get-ExecutionPolicy -Scope CurrentUser`), print instruction to run `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned`. **Do not auto-set** (user security decision).

## Gap 4 — profile-indirection scope (known limitation, document — not a bug)

Sourcing `env.ps1` from `$PROFILE` is legitimate, privilege-free, no registry, no reboot — same as conda/pyenv-win/mise. It does NOT cover what `HKCU\Environment` would:

- `cmd.exe` windows, Start-menu/taskbar GUI apps, non-PowerShell CI steps don't see the PATH.
- `HKCU\Environment` would survive reboot + be visible to any new process — but carries the `REG_SZ`-vs-`REG_EXPAND_SZ` corruption footgun (rustup #261) and needs `WM_SETTINGCHANGE` broadcast.

**Plan note:** OCX deliberately uses profile-indirection (consistent cross-platform, reversible, no privilege). Tradeoff: `ocx` on PATH only inside PowerShell sessions. Users needing system-wide PATH add the bin dir manually. Document in setup completion message + `website/src/docs/reference/environment.md`.

## Sources

- conda `core/initialize.py` (regex, diff-gate, duplicate TODO); conda#9922 (CRLF)
- Microsoft Learn `about_Profiles` (5.1 + 7.6) — path table, CurrentUserAllHosts guidance
- rustup `src/cli/self_update/shell.rs` (CurrentUserAllHosts rationale); rustup#261 (REG_SZ footgun)
- `regex` `RegexBuilder` CRLF docs; `sha2` crate
