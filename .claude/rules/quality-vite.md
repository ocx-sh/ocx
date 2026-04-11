---
paths:
  - "**/vite.config.*"
  - "**/vitest.config.*"
  - "**/.vitepress/config.*"
---

# Vite Build Tool Quality

Vite-specific opinions for Vite 7/8 (2026). Not a cookbook — for config
examples and API reference, read [vite.dev](https://vite.dev/). This file
captures the **opinions** (what to avoid, what to prefer) that a senior
developer would insist on in code review.

Project-independent and shareable.

---

## Anti-Patterns

### Block (must fix before merge)

1. **Hardcoded credentials or secrets** in `vite.config.*` — use environment variables, never commit.
2. **`VITE_` prefix on server-only env vars** — exposes them to the client bundle at build time. Never prefix secrets with `VITE_`.
3. **Browser API at module scope** in components used in VitePress/SSR — `window`, `document`, `localStorage` accessed at import time crashes the pre-render. Guard with `import.meta.env.SSR` or use `<ClientOnly>` wrapper.
4. **`vite.config.ts` at root alongside `.vitepress/config.ts`** — VitePress ignores the root config; the split causes silent override bugs.
5. **`build.target: "es5"`** with Rolldown (Vite 8) — not supported. Rolldown targets modern JS only.
6. **Mixing SSR and client globals** in shared modules — pre-render crashes.
7. **Missing env var validation** at config load — validate with `zod` or `valibot`; fail the build on invalid env.

### Warn (should fix)

- Missing `base` option for non-root deployments
- `optimizeDeps.include` as a workaround instead of fixing the underlying ESM incompatibility
- Using `vite-plugin-*` wrappers that duplicate Vite 8 built-ins (e.g., `vite-tsconfig-paths` is now built-in)
- Missing `.vitepress/cache` in `.gitignore`
- Enabling experimental `fullBundleMode` in production — not stable
- Not specifying `build.outDir` explicitly — defaults differ between app mode and library mode
- `resolve.alias` with absolute paths — use `fileURLToPath(new URL(..., import.meta.url))` for portability

---

## Env Var Discipline

- **`VITE_*` prefix is the client-exposure switch** — anything with this prefix ends up in the browser bundle at build time
- **`.env.local` gitignored**; `.env` committed (NO secrets, only public defaults)
- **Validate env vars at config load** with `zod`/`valibot`; fail fast on missing/invalid values
- **Never read `process.env` at module scope in client code** — it's resolved at build time, not runtime

```ts
// Good: validate at config load
import { z } from "zod";
const env = z.object({
  VITE_API_URL: z.string().url(),
  VITE_PUBLIC_KEY: z.string().min(1),
}).parse(import.meta.env);
```

---

## Vite 7 → Vite 8 Migration (2026)

- **Rolldown replaces Rollup** for production builds in Vite 8 — 10-30x faster
- Internal plugins (`alias`, `resolve`) are now Rust-native via `nativePlugins: 'v1'`
- **`resolve.tsconfigPaths: true`** built-in — drop the `vite-tsconfig-paths` plugin
- **Node.js baseline**: 20.19+ / 22.12+ (Vite 7+); Node 18 is EOL for Vite — upgrade base images
- Default browser target changed: `'baseline-widely-available'` instead of `'modules'`
- `build.rollupOptions` maps directly to Rolldown options — same API surface
- Plugin API is Rollup-compatible — existing plugins work without modification

---

## VitePress-Specific Gotchas

1. **SSR compatibility is mandatory** — VitePress pre-renders in Node at build time. Any browser API accessed at import time crashes the build. Pattern: guard with `import.meta.env.SSR` or use dynamic imports in `mounted()` hooks.
2. **`<ClientOnly>` wrapper** for components that cannot be made SSR-safe (third-party charting libs, etc.).
3. **`defineClientComponent`**: VitePress helper for importing Vue components that use browser APIs — avoids dynamic-import boilerplate.
4. **Vite config lives in `.vitepress/config.ts`**, not at root. The `vite` key inside VitePress config accepts the same `UserConfig` shape. Do not create a separate `vite.config.ts` at root — VitePress ignores it.
5. **Full typing**: use `defineConfig` from `vitepress` — it catches malformed nav/sidebar/theme configs at author time.

---

## Config Structure Recommendations

```typescript
// vite.config.ts or .vitepress/config.ts — typed, minimal
import { defineConfig } from "vite";

export default defineConfig(({ command, isSsrBuild }) => ({
  build: {
    target: "baseline-widely-available", // Vite 7+ default; explicit for visibility
  },
}));
```

- Use the function form of `defineConfig` only when config needs to branch on `command` or `isSsrBuild`
- **Library mode**: set `build.lib` with `entry`, `formats`, `fileName`. App-mode config for libraries is wrong — tree-shaking and externalization behave differently
- **Extract reusable plugin arrays** to a local helper — do not duplicate between `vitest.config.ts` and `vite.config.ts`

---

## Code Review Checklist (Vite-Specific)

See `quality-core.md` for the universal review checklist. Vite-specific additions:

- [ ] No secrets or credentials in `vite.config.*`
- [ ] No `VITE_` prefix on server-only env vars
- [ ] Env vars validated at config load (zod/valibot)
- [ ] `base` option set for non-root deployments
- [ ] Browser API access guarded for SSR if VitePress
- [ ] No duplicate config files (root `vite.config.ts` + `.vitepress/config.ts`)
- [ ] `build.outDir` specified explicitly
- [ ] `.vitepress/cache` in `.gitignore`
- [ ] Target set to `'baseline-widely-available'` or explicit browserslist

---

## Sources

Authoritative references used in this rule:

- [Vite 8.0 announcement](https://vite.dev/blog/announcing-vite8)
- [Vite 7.0 announcement](https://vite.dev/blog/announcing-vite7)
- [Rolldown integration guide](https://v7.vite.dev/guide/rolldown)
- [VitePress SSR Compatibility](https://vitepress.dev/guide/ssr-compat)
- [VitePress Configuration reference](https://vitepress.dev/reference/site-config)
