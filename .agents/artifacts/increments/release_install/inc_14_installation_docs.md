# Increment 14: Installation Page (Website) + README Update

**Status**: Not Started
**Completed**: —
**ADR Phase**: 7 (steps 20-21)
**Depends on**: Increments 07+08 (install scripts exist), Increment 10 (release pipeline exists)

---

## Goal

Write the full installation documentation on the website and add install one-liners to the README.

## Tasks

### 1. Update `website/src/docs/installation.md`

The current file is a stub. Replace with full content covering (per ADR Section 13a):

- **Quick Install**: one-liners for macOS/Linux and Windows
- **GitHub Releases**: download table for all 8 targets with platform/architecture columns
- **Updating**: `ocx install ocx --select` and re-run install script
- **Verify Installation**: `ocx version` and `ocx info`
- **Uninstalling**: remove `~/.ocx` + remove profile source line (per shell)

Follow `.claude/rules/documentation.md`:
- Narrative structure (idea → problem → solution)
- Short paragraphs, real-world examples
- Reference-style links at bottom of file
- Use `:::tip` for actionable advice, `:::warning` for caveats
- Link to relevant pages (environment variables, user guide)

### 2. Update `README.md`

Add an Installation section with:
- Shell one-liner: `curl -fsSL https://ocx.sh/install.sh | sh`
- PowerShell one-liner: `irm https://ocx.sh/install.ps1 | iex`
- Link to full installation guide: `https://ocx.sh/docs/installation`

Keep it brief — the README should drive people to the website for details.

### 3. Cross-references

Ensure the installation page links to:
- Environment variables page (for `OCX_HOME`, `OCX_NO_MODIFY_PATH`)
- User guide (for understanding the three-store architecture)
- Command-line reference (for `ocx version`, `ocx info`)

## Verification

- [ ] Installation page has all sections from ADR Section 13a
- [ ] Download table covers all 8 targets
- [ ] One-liners match the actual install script URLs
- [ ] README has install section with link to full docs
- [ ] VitePress builds: `cd website && bun run vitepress build`
- [ ] All internal links resolve (no broken anchors)
- [ ] Documentation follows `.claude/rules/documentation.md` guidelines
- [ ] `task verify` still passes

## Files Changed

- `website/src/docs/installation.md` (rewrite from stub)
- `README.md` (add installation section)

## Notes

- The installation page content is outlined in ADR Section 13a — use that as the template but apply documentation.md rules for narrative structure.
- The download table URLs won't work until the first release is published. Use placeholder format: `ocx-{target}.tar.xz`.
- The uninstall section is important — users need to know how to cleanly remove OCX.
