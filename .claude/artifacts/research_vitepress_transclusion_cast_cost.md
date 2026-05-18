# Research: VitePress Transclusion + asciinema Cast Cost Model

> Date: 2026-05-17. Grounds the decoupling-boundary ADR for the tested
> website command-example mechanism. Findings grounded against local infra.

## Axis 1 — VitePress `<<<` snippet import

- `<<<` transclusion is **build-time** (`fs.readFileSync` in markdown-it
  plugin), not runtime. Content inlined into static HTML at `vitepress build`.
- `@` alias = the VitePress **srcDir**. `website/.vitepress/config.mts` sets
  `srcDir: "src"` → `@` = `website/src/`.
- Syntax: `<<< @/path.sh`, line ranges `{3,7}`, lang `{sh}`, line-numbers,
  and VS Code `#region name` / `#endregion name` blocks (no `#snippet`).
- **Conflict (load-bearing for ADR):** `<<<` can only read files **under
  `website/src/`**. `public/` IS under srcDir → `<<< @/public/x.sh` works.
  But scripts authored in `test/` are **outside srcDir → not transcludable**.
  Resolution: a build step copies scripts into `website/src/_scripts/`
  (underscore prefix → VitePress does not route it as a page). `public/`
  also works for serving but is the asset dir; `_scripts/` is cleaner for
  transclusion-only source.
- No `.md` under `website/src/docs/` currently uses `<<<`. Feature available,
  unused. Install scripts today are inline code strings, not transcluded.

| Asset | Location | `<<<`-able? |
|---|---|---|
| `install.sh` | `website/src/public/install.sh` | yes |
| recording scripts | `test/recordings/scripts/*.sh` | **no** (outside srcDir) |
| `.cast` files | `website/src/public/casts/*.cast` | yes but useless (binary) |

## Axis 2 — asciinema cast + GIF cost

- Per recording test: setup (real OCI pushes to `registry:2`) + PTY command
  execution (real OCX ops) dominate. Cast write itself ≈ 1 ms.
- **"Tested-only is cheaper because no cast" is false at per-test level** —
  setup + exec run regardless. Cast-optional remains justified for *other*
  reasons: not every snippet needs a visual GIF; agg/gifski conversion +
  human visual-review burden + GIF artifact churn.
- Serial recordings ≈ 4–15 min (22 scripts); `pytest -n auto` ≈ 1–3 min;
  registry throughput caps useful parallelism ~4–8 workers.
- GIF step (`agg`/gifski via `ProcessPoolExecutor`) ≈ 20–60 s total wall.
- **Real cost lever = pipeline placement.** Recordings are a `website:build`
  dependency, NOT in `task verify`. The expensive Docker+binary+cast chain
  only matters where it is wired. Decoupling cast generation from drift
  gating is the win, not per-script cast skipping.

## Implications for ADR

1. Drift gating must run where `task verify` runs → acceptance path
   (`test/tests/`, `test:parallel`), not the recordings pipeline.
2. Cast generation is a *separate, additive* concern layered on opt-in
   scripts inside the existing `website:build` recordings pipeline.
3. Transcludable script copies must land under `website/src/` (e.g.
   `website/src/_scripts/`), not `test/`; published copy is the decoupling
   seam (website never reads `test/`).
4. Reframe tenet #3 rationale: cast-optional is about GIF/review burden and
   not-every-snippet-is-a-demo, not per-test CPU.

## Sources

- VitePress: Markdown Extensions (import code snippets), Asset Handling,
  Site Config `srcDir`; `vuejs/vitepress` `markdownToVue.ts`.
- asciinema `agg` manual + `asciinema/agg` GitHub.
- Local: `website/.vitepress/config.mts`, `website/recordings.taskfile.yml`,
  `test/recordings/{cast_recorder,cast_to_gif,conftest}.py`,
  `test/recordings/scripts/*.sh`, `test/recordings/setups.py`.
