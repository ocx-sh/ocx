# Tech Strategy - Golden Paths

This is the **SINGLE SOURCE OF TRUTH** for technology choices in this project.

## Compliance

1. **Follow This File**: Use the technologies listed below
2. **No Deviations**: Do not suggest alternatives unless explicitly instructed
3. **Latest Stable**: Always use the latest stable version unless pinned

## Language Golden Paths

### Rust (Primary)

| Component | Choice |
|-----------|--------|
| Edition | Rust 2024 |
| Async | Tokio |
| Linker | Mold (dev) |

### TypeScript / JavaScript (Website)

| Component | Choice |
|-----------|--------|
| Runtime | Bun |
| Build | Vite (VitePress) |
| Package Manager | Bun |

### Python (Acceptance Tests)

| Component | Choice |
|-----------|--------|
| Runtime | Python 3.13+ |
| Tooling | uv (Manager), Ruff (Linter) |
| Testing | pytest |

## Infrastructure

| Component | Choice |
|-----------|--------|
| Static/Frontend | GitHub Pages |
| Edge/CDN | Cloudflare |
| Secrets | GitHub Secrets |

## CI/CD

| Component | Choice |
|-----------|--------|
| Platform | GitHub Actions |
| Auth | OIDC |
| Security | Trivy |
