# Archive Inspection — Layout Analysis & Platform Gotchas

Detailed guidance for Phase 3 of the mirror creation workflow: inspecting archive contents, detecting wrapper directories, and handling platform-specific layouts.

## Analyzing directory structure

Look for:

- **Top-level wrapper directory** (e.g. `cmake-3.28.0-linux-x86_64/bin/cmake`) — if ALL entries share a common prefix directory, this needs `strip_components: 1` in the mirror YAML so the rebundling process strips it. Count the depth of the common prefix to determine the correct value (usually 1).
- `bin/` directory containing executables → `${installPath}/bin` for PATH
- Root-level executable (no `bin/` dir) → `${installPath}` for PATH
- Platform-specific layouts (e.g. macOS `.app` bundles → `${installPath}/Foo.app/Contents/bin`)
- `man/`, `share/man/` directories → MANPATH entry
- `lib/`, `include/` directories → note for the user but don't auto-add env vars
- Paths in metadata should be relative to the content root **after stripping** (e.g. if the archive has `cmake-3.28/bin/cmake` and `strip_components: 1`, the metadata PATH should use `${installPath}/bin`).

**Important:** `strip_components` in the mirror YAML controls rebundling (stripping the wrapper directory before creating the OCX package). This is separate from `strip_components` in metadata JSON, which tells OCX how to extract the package after downloading. Typically, the mirror YAML needs `strip_components` but the metadata does not (since the rebundled archive is already clean).

## macOS zip archive gotchas

macOS zips are a common source of pitfalls. Always inspect at least one macOS archive alongside a Linux one.

- **`__MACOSX/` directory**: macOS's built-in zip utility often embeds a `__MACOSX/` directory with `._` resource fork files. These end up as junk in the package after rebundling.
- **`.app` bundles**: macOS assets sometimes contain `.app` application bundles (e.g. `Foo.app/Contents/MacOS/foo`) instead of flat `bin/foo` layouts. The binary path is completely different — `${installPath}/Foo.app/Contents/MacOS` vs `${installPath}/bin`. This requires a **platform-specific metadata file** (e.g. `metadata-darwin.json`) with the correct PATH.
- **`.ad` / auxiliary files**: Some macOS archives include ad-hoc signing files, `.dSYM` debug bundles, or other auxiliary files not present in the Linux/Windows archives. These change the effective layout.

If the directory structures differ, use platform-specific metadata in the mirror config:

```yaml
metadata:
  default: metadata.json
  platforms:
    darwin/amd64: metadata-darwin.json
    darwin/arm64: metadata-darwin.json
```

## Platform-specific `strip_components`

Beyond macOS, Windows may also have flat layouts while Linux uses `bin/`. If layouts differ, create platform-specific metadata files. Check `strip_components` consistency across platforms — if all platforms have a top-level wrapper directory, a single `strip_components` value works. If some platforms differ (e.g. Windows zips are flat while tarballs have a wrapper), use per-platform `strip_components`:

```yaml
# Per-platform strip_components (tarballs have wrapper dir, Windows zip is flat)
strip_components:
  default: 1
  platforms:
    windows/amd64: 0
```

The simple form `strip_components: 1` still works when all platforms share the same value.

## Per-platform `asset_type` (archive vs binary mix)

When a tool ships tarballs on Linux/macOS but a raw `.exe` on Windows, the spec must use the full per-platform form — a uniform `asset_type` cannot mix archive and binary:

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

Common for Rust CLI tools. See `mirrors/lychee/mirror.yml` for a worked example.

## Raw binaries

For raw binaries the metadata just needs a PATH entry pointing to `${installPath}` (the binary lands directly in the content root).

## Verifying Linux musl binaries

If the chosen Linux asset is a musl variant (non-Rust-triple), extract a binary and run `file <binary>`. If it reports `dynamically linked, interpreter /lib/ld-musl-*`, switch to the gnu/glibc asset variant instead. See `platform-detection.md` for the full decision process.
