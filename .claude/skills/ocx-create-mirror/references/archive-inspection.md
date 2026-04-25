# Archive Inspection — Layout Analysis & Platform Gotchas

Guidance for Phase 3 of mirror creation workflow: inspect archive contents, detect wrapper dirs, handle platform-specific layouts.

## Analyzing directory structure

Look for:

- **Top-level wrapper directory** (e.g. `cmake-3.28.0-linux-x86_64/bin/cmake`) — if ALL entries share common prefix dir, needs `strip_components: 1` in mirror YAML so rebundling strips it. Count depth of common prefix for correct value (usually 1).
- `bin/` directory with executables → `${installPath}/bin` for PATH
- Root-level executable (no `bin/` dir) → `${installPath}` for PATH
- Platform-specific layouts (e.g. macOS `.app` bundles → `${installPath}/Foo.app/Contents/bin`)
- `man/`, `share/man/` dirs → MANPATH entry
- `lib/`, `include/` dirs → note for user, no auto-add env vars
- Paths in metadata relative to content root **after stripping** (e.g. archive has `cmake-3.28/bin/cmake` + `strip_components: 1` → metadata PATH use `${installPath}/bin`).

**Important:** `strip_components` in mirror YAML controls rebundling (strip wrapper dir before OCX package creation). Separate from `strip_components` in metadata JSON, which tells OCX how to extract package after download. Typically mirror YAML needs `strip_components`, metadata does not (rebundled archive already clean).

## macOS zip archive gotchas

macOS zips common pitfall source. Always inspect one macOS archive alongside Linux one.

- **`__MACOSX/` directory**: macOS built-in zip utility often embeds `__MACOSX/` dir with `._` resource fork files. End up as junk in package after rebundling.
- **`.app` bundles**: macOS assets sometimes contain `.app` application bundles (e.g. `Foo.app/Contents/MacOS/foo`) instead of flat `bin/foo` layouts. Binary path completely different — `${installPath}/Foo.app/Contents/MacOS` vs `${installPath}/bin`. Requires **platform-specific metadata file** (e.g. `metadata-darwin.json`) with correct PATH.
- **`.ad` / auxiliary files**: Some macOS archives include ad-hoc signing files, `.dSYM` debug bundles, other auxiliary files not in Linux/Windows archives. Change effective layout.

If directory structures differ, use platform-specific metadata in mirror config:

```yaml
metadata:
  default: metadata.json
  platforms:
    darwin/amd64: metadata-darwin.json
    darwin/arm64: metadata-darwin.json
```

## Platform-specific `strip_components`

Beyond macOS, Windows may also have flat layouts while Linux uses `bin/`. If layouts differ, create platform-specific metadata files. Check `strip_components` consistency across platforms — if all platforms have top-level wrapper dir, single `strip_components` value works. If some platforms differ (e.g. Windows zips flat, tarballs have wrapper), use per-platform `strip_components`:

```yaml
# Per-platform strip_components (tarballs have wrapper dir, Windows zip is flat)
strip_components:
  default: 1
  platforms:
    windows/amd64: 0
```

Simple form `strip_components: 1` still works when all platforms share same value.

## Per-platform `asset_type` (archive vs binary mix)

When tool ships tarballs on Linux/macOS but raw `.exe` on Windows, spec must use full per-platform form — uniform `asset_type` cannot mix archive and binary:

```yaml
asset_type:
  default:
    type: archive
    strip_components: 0
  platforms:
    windows/amd64:
      type: binary
      name: lychee
```

Common for Rust CLI tools. See `mirrors/lychee/mirror.yml` for worked example.

## Raw binaries

For raw binaries metadata just needs PATH entry pointing to `${installPath}` (binary lands directly in content root).

## Verifying Linux musl binaries

If chosen Linux asset is musl variant (non-Rust-triple), extract binary and run `file <binary>`. If reports `dynamically linked, interpreter /lib/ld-musl-*`, switch to gnu/glibc asset variant instead. See `platform-detection.md` for full decision process.