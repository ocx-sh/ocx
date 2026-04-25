# Platform Detection Heuristics

Asset filenames encode platform info various ways. Common patterns ranked by reliability:

1. **Explicit os-arch**: `tool-linux-amd64.tar.gz` — highest confidence
2. **Explicit os_arch**: `tool-linux_amd64.tar.gz` — high confidence
3. **Rust triple**: `tool-x86_64-unknown-linux-gnu.tar.gz` — high confidence
4. **Go-style**: `tool_Linux_x86_64.tar.gz` — high confidence (note capital L)
5. **Loose match**: `tool-linux64.tar.gz` — medium confidence, confirm with user

## Platform mapping table

Match asset filenames to platforms via common naming conventions:

| Platform | Common substrings |
|----------|-------------------|
| `linux/amd64` | `linux-x86_64`, `linux-amd64`, `linux64`, `Linux-x86_64` |
| `linux/arm64` | `linux-aarch64`, `linux-arm64`, `Linux-aarch64` |
| `darwin/amd64` | `darwin-x86_64`, `macos-x86_64`, `macOS-x86_64`, `macos-universal`, `Darwin-x86_64`, `apple-darwin` |
| `darwin/arm64` | `darwin-arm64`, `macos-arm64`, `darwin-aarch64`, `macos-universal`, `apple-darwin` |
| `windows/amd64` | `windows-x86_64`, `win64`, `windows-amd64`, `win-x64`, `pc-windows` |
| `windows/arm64` | `windows-arm64`, `win-arm64` |

- `universal`/`any` macOS asset exists → map both `darwin/amd64` and `darwin/arm64`.
- Build regex per platform. Use `.*` for version segments, escape dots and special chars.
- Asset names changed between versions → add multiple patterns per platform (newest first).

## musl vs glibc decision process

Multiple assets match same platform (e.g. both `-gnu` and `-musl` Linux variants) → prefer **statically linked musl** — works on glibc distros (Ubuntu, Fedora) and musl distros (Alpine). But **not all musl binaries statically linked**. Some tools (e.g. Bun) produce dynamically linked musl binaries needing `/lib/ld-musl-*.so.1` at runtime, fail on glibc with misleading "No such file or directory" error.

1. **Rust triple** (`*-unknown-linux-musl`): safe prefer musl — Rust musl target produces statically linked binaries by convention.
2. **Non-Rust musl variants**: download musl asset during inspection, verify with `file <binary>`. Says `statically linked` → use musl. Says `dynamically linked, interpreter /lib/ld-musl-*` → use **gnu/glibc variant**.
3. **When in doubt**, prefer gnu/glibc — works on vast majority Linux systems (all major distros, CI runners, WSL, containers except Alpine).