# Increment 06: Website Changelog Page + Sidebar Entry

**Status**: Not Started
**Completed**: —
**ADR Phase**: 3 (steps 9-10)
**Depends on**: Increment 02 (CHANGELOG.md must exist)

---

## Goal

Add a Changelog page to the VitePress website that includes the repository's `CHANGELOG.md` at build time.

## Tasks

### 1. Create `website/src/docs/changelog.md`

```markdown
---
outline: deep
---

# Changelog

<!--@include: ../../../CHANGELOG.md{3,}-->
```

The `{3,}` suffix includes CHANGELOG.md starting from line 3, skipping the `# Changelog` header to avoid duplication. VitePress resolves `@include` at build time, so the page is always in sync with the repo.

### 2. Add to VitePress sidebar

In `website/.vitepress/config.mts` (or `website/src/.vitepress/config.mts`), add a sidebar entry:

```typescript
{
  text: "Changelog",
  link: "/docs/changelog",
}
```

Place it at the end of the sidebar, after the Reference section.

### 3. Test locally

```bash
cd website && pnpm dev
```

Navigate to the changelog page and verify:
- The content renders correctly
- The page title is "Changelog" (not duplicated)
- The `outline: deep` frontmatter creates a right-hand TOC with version headers
- Links within the changelog (if any) work

## Verification

- [ ] `website/src/docs/changelog.md` exists with `@include` directive
- [ ] Sidebar in `config.mts` has the Changelog entry
- [ ] VitePress builds successfully: `cd website && pnpm build`
- [ ] Changelog content renders in dev server
- [ ] `task verify` still passes

## Files Changed

- `website/src/docs/changelog.md` (new)
- `website/.vitepress/config.mts` or `website/src/.vitepress/config.mts` (sidebar update)

## Notes

- The `@include` path is relative to the markdown file's location. Since `changelog.md` is at `website/src/docs/`, the path `../../../CHANGELOG.md` points to the repo root.
- If the CHANGELOG.md has fewer than 3 lines (edge case with no releases yet), the include will be empty — that's fine, it'll populate when releases happen.
