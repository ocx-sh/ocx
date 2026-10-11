# Research: Reference vs explanation in developer-tool docs

## Metadata

**Date:** 2026-10-11
**Domain:** cli
**Triggered by:** `.agents/discussions/docs-next-level.md` — reference-page bloat
**Expires:** 2027-04-11

## Direct Answer

Diátaxis: reference = austere facts mirroring product structure; explanation,
instruction, opinion move out and are linked. Practice: the uniform, field-shaped
references (uv settings, mise settings, kubectl, docker) are source-generated with a
regenerate command and a CI drift check (`--check` / dry-run); hand-written ones
(bun, terraform, git-config, cargo env) interleave rationale. Common outcome is
hybrid: machine facts (name, type, default, env) generated; long descriptions and
examples hand-authored as sidecar text in the source. Task-first docs: qualitative
evidence only (Meng et al. 2019; NPR One WTD talk), no head-to-head study; every
source keeps reference beside tasks. Starlight: no JSON-Schema or CLI-reference
plugin in the official list; routes are Astro content loaders (custom `load()` +
Zod) or committed generated MDX with a `--check` mode.

## Sources

- https://diataxis.fr/reference/ · https://diataxis.fr/explanation/ · https://diataxis.fr/how-to-guides/
- uv: https://docs.astral.sh/uv/reference/environment/ · https://docs.astral.sh/uv/reference/settings/ (`cargo dev generate-all`)
- mise: https://raw.githubusercontent.com/jdx/mise/main/settings.toml (`mise run render`)
- docker: https://pkg.go.dev/github.com/docker/cli-docs-tool · kubectl generated pages
- cargo env (hand-written): https://doc.rust-lang.org/cargo/reference/environment-variables.html
- clap-markdown: https://docs.rs/crate/clap-markdown/latest
- Against generation (opinion): https://devrelbook.substack.com/p/why-i-never-met-an-auto-generated
- Task-first: Meng et al. 2019 (mangold-international.com PDF); WTD 2017 Sarah Hersh talk
- Starlight plugins: https://starlight.astro.build/resources/plugins/ · loaders: https://docs.astro.build/en/reference/content-loader-reference/

## Leads

1. How uv/Ruff attach examples and long text to option metadata in Rust; their CI drift check.
2. Any Starlight site rendering reference from JSON Schema via a content loader; build/Pagefind cost.
3. Docker / cargo-mdman: hand-written long descriptions beside generated flags.
