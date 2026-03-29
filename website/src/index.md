---
# https://vitepress.dev/reference/default-theme-home-page
layout: home

hero:
  name: "ocx"
  text: "The Simple Package Manager"
  tagline: Turn any OCI registry into a cross-platform binary distribution platform. Zero extra infrastructure.
  image: /logo.svg
  actions:
    - theme: brand
      text: Get Started
      link: /docs/getting-started
    - theme: alt
      text: Install
      link: /docs/installation
    - theme: alt
      text: User Guide
      link: /docs/user-guide

features:
  - title: OCI-Native
    details: Your Docker Hub, GHCR, ECR, or Harbor is the package server. No new infrastructure, no new credentials, no new licenses.
    icon:
      src: /licensed/icons/feature-oci.svg

  - title: Cross-Platform
    details: One identifier resolves to the right binary on Linux, macOS, Windows, or FreeBSD. OCI multi-platform manifests handle the rest.
    icon:
      src: /licensed/icons/feature-platform.svg

  - title: Reproducible
    details: Content-addressed storage deduplicates automatically — if two tags resolve to the same build, they share one copy on disk. A local index snapshot locks every resolution. Same command, same result — today, tomorrow, offline.
    icon:
      src: /licensed/icons/feature-lock.svg

  - title: Automation-First
    details: JSON output, clean environments, composable commands. Designed from the ground up for CI pipelines and build systems.
    icon:
      src: /licensed/icons/feature-automation.svg
---

<div class="quick-start-header">
  <h2>Quick Install</h2>
  <p>One command. Any platform. No root required.</p>
</div>

<div class="quick-start-body">

::: code-group
```sh [Shell]
curl -fsSL https://ocx.sh/install.sh | sh
```
```ps1 [PowerShell]
irm https://ocx.sh/install.ps1 | iex
```
:::

Open a new terminal and run `ocx install uv:0.10` to install your first package.

</div>

<div class="feature-sections-header">
  <h2>How it works</h2>
  <p>A closer look at what makes ocx different.</p>
</div>

<div class="feature-sections">

<FeatureSection title="Your Registry is the Package Server">
  <template #text>

Install packages directly from any OCI-compliant registry. No taps, no formulas, no Artifactory. The same infrastructure you already run for container images now serves your development tools.

One command installs and activates a package. Switch versions instantly. Every install is content-addressed by SHA-256 digest — what you download is what you run.

  </template>

  <img src="/licensed/images/feature-registry.svg" alt="Install from any OCI registry" class="feature-img" />
</FeatureSection>

<FeatureSection title="Same Command on Every Platform" flip>
  <template #text>

Think `docker pull` — but for standalone binaries. You name the package, OCI multi-platform manifests resolve the right build for your OS and architecture. No platform conditionals, no filename guessing, no architecture mapping tables.

Write `ocx install uv:0.10` once. It works on your Mac, your CI runner's Linux, and your colleague's Windows machine — the tools you already use, distributed the way containers taught us.

  </template>

  <img src="/licensed/images/feature-platform.svg" alt="Cross-platform resolution" class="feature-img" />
</FeatureSection>

<FeatureSection title="Composable Environments">
  <template #text>

Every package declares its own environment variables. `ocx exec` layers them on top of your current shell, and with `--clean` it strips everything back to only what the packages provide — no host pollution, no PATH conflicts, no stale state.

Compose multiple packages in a single invocation. Each one contributes its variables. Pass `--clean` when you need a hermetic, reproducible scope — ideal for CI pipelines and build systems.

  </template>

  <img src="/licensed/images/feature-env.svg" alt="Clean environment execution" class="feature-img" />
</FeatureSection>

<FeatureSection title="Content-Addressed and Deduplicated" flip>
  <template #text>

Every object in the store is identified by its SHA-256 digest — a cryptographic fingerprint of its contents. If `uv:0.10` and `uv:latest` resolve to the same build, they share one directory on disk. Storage scales with distinct builds, not with the number of tags pointing at them.

This also means verification is built in. A path under `sha256:…/` never changes its contents. Pin any package to a digest and you have a lockfile-free guarantee — no registry queries, no index lookups, just the hash.

  </template>

  <img src="/licensed/images/feature-reproducible.svg" alt="Content-addressed deduplication" class="feature-img" />
</FeatureSection>

<FeatureSection title="Built for Automation">
  <template #text>

Every command returns structured JSON with `--format json`. Exit codes are meaningful. Environment variables compose cleanly. There is no interactive prompt, no "press Y to continue", no color code that breaks your parser.

`ocx exec` runs commands with package-declared variables. `ocx env` prints them for your build system. `ocx ci export` writes them directly into GitHub Actions or GitLab CI runtime files. The entire CLI is designed to be called by other tools, not typed by humans.

  </template>

  <img src="/licensed/images/feature-automation.svg" alt="Automation-first design" class="feature-img" />
</FeatureSection>

<FeatureSection title="Relocatable and Air-Gap Ready" flip>
  <template #text>

Every install is a self-contained directory — no global state, no registry database, no fragile symlinks into system paths. Copy the store to a USB drive, zip it into a CI cache artifact, or `scp` it to a machine behind a firewall.

The local index snapshot plus the content-addressed object store is everything `ocx --offline` needs. Bundle a toolchain once, redistribute it to air-gapped hosts, and `ocx install --offline` resolves it without ever touching the network.

  </template>

  <img src="/licensed/images/feature-offline.svg" alt="Offline air-gapped package redistribution" class="feature-img" />
</FeatureSection>

</div>

<div class="cta-header">
  <h2>Still reading?</h2>
  <p>Join the community and help shape what comes next.</p>
</div>

<div class="cta-cards">
  <a href="/docs/roadmap" class="cta-card">
    <img src="/licensed/icons/cta-roadmap.svg" alt="" class="cta-card-icon cta-icon-roadmap" />
    <div class="cta-card-text">
      <strong>Roadmap</strong>
      <span>See what we're building and what's coming next.</span>
    </div>
  </a>
  <a href="/docs/catalog/" class="cta-card">
    <img src="/licensed/icons/cta-catalog.svg" alt="" class="cta-card-icon cta-icon-catalog" />
    <div class="cta-card-text">
      <strong>Package Catalog</strong>
      <span>Browse available packages on the public registry.</span>
    </div>
  </a>
  <a href="https://discord.gg/BuRhhAYy9r" target="_blank" rel="noreferrer" class="cta-card">
    <img src="/licensed/icons/cta-discord.svg" alt="" class="cta-card-icon cta-icon-discord" />
    <div class="cta-card-text">
      <strong>Discord</strong>
      <span>Ask questions, share feedback, and follow development.</span>
    </div>
  </a>
  <a href="https://github.com/ocx-sh/ocx" target="_blank" rel="noreferrer" class="cta-card">
    <img src="/licensed/icons/cta-github.svg" alt="" class="cta-card-icon cta-icon-github" />
    <div class="cta-card-text">
      <strong>GitHub</strong>
      <span>Star the repo, file issues, or contribute code.</span>
    </div>
  </a>
</div>

<div class="closing">
  <p class="closing-tagline"><strong>ocx</strong> — your registry, your binaries, no extra infrastructure — free to use, fork, and extend.</p>
</div>

<style>
/*
 * Feature card icon tinting via CSS filters.
 * Icons are black SVGs rendered as <img>. We use filter chains to
 * colorize them, keyed to VitePress palette colors.
 *
 * Technique: brightness(0) saturate(100%) → pure black baseline,
 * then invert + sepia + saturate + hue-rotate to reach target hue.
 */

/* OCI-Native — indigo/brand */
.VPImage[src*="feature-oci"] {
  filter: invert(24%) sepia(79%) saturate(1742%) hue-rotate(216deg) brightness(92%) contrast(94%);
}
/* Cross-Platform — purple */
.VPImage[src*="feature-platform"] {
  filter: invert(27%) sepia(51%) saturate(3264%) hue-rotate(253deg) brightness(88%) contrast(93%);
}
/* Reproducible — green */
.VPImage[src*="feature-lock"] {
  filter: invert(37%) sepia(62%) saturate(592%) hue-rotate(113deg) brightness(92%) contrast(89%);
}
/* Automation-First — yellow/amber */
.VPImage[src*="feature-automation"] {
  filter: invert(35%) sepia(56%) saturate(764%) hue-rotate(348deg) brightness(93%) contrast(89%);
}

/* Dark mode: brighter variants matching VitePress dark palette */
.dark .VPImage[src*="feature-oci"] {
  filter: invert(72%) sepia(40%) saturate(1059%) hue-rotate(197deg) brightness(104%) contrast(101%);
}
.dark .VPImage[src*="feature-platform"] {
  filter: invert(76%) sepia(30%) saturate(1148%) hue-rotate(230deg) brightness(103%) contrast(97%);
}
.dark .VPImage[src*="feature-lock"] {
  filter: invert(70%) sepia(52%) saturate(498%) hue-rotate(106deg) brightness(96%) contrast(91%);
}
.dark .VPImage[src*="feature-automation"] {
  filter: invert(76%) sepia(54%) saturate(684%) hue-rotate(338deg) brightness(101%) contrast(96%);
}

/* Quick install — centered header, left-aligned body */
.quick-start-header {
  text-align: center;
  margin: 48px auto 8px;
  max-width: 1152px;
  padding: 0 24px;
}

.quick-start-header h2 {
  font-size: 28px;
  font-weight: 700;
  color: var(--vp-c-text-1);
  margin: 0 0 8px;
  border: none;
  padding: 0;
}

.quick-start-header p {
  font-size: 16px;
  color: var(--vp-c-text-2);
  margin: 0;
}

.quick-start-body {
  max-width: 1152px;
  margin: 0 auto;
  padding: 0 24px;
}

@media (min-width: 640px) {
  .quick-start-body {
    padding: 0 48px;
  }
}

@media (min-width: 960px) {
  .quick-start-body {
    padding: 0 64px;
  }
}

.quick-start-body p {
  font-size: 14px;
  color: var(--vp-c-text-3);
  margin: 12px 0 0;
}

/* Feature sections container — vertical stack with card spacing */
.feature-sections {
  display: flex;
  flex-direction: column;
  gap: 24px;
  max-width: 1152px;
  margin: 0 auto;
  padding: 0 24px;
}

/* Section header between feature cards and expanded sections */
.feature-sections-header {
  text-align: center;
  margin: 48px auto 8px;
  max-width: 1152px;
  padding: 0 24px;
}

.feature-sections-header h2 {
  font-size: 28px;
  font-weight: 700;
  color: var(--vp-c-text-1);
  margin: 0 0 8px;
  border: none;
  padding: 0;
}

.feature-sections-header p {
  font-size: 16px;
  color: var(--vp-c-text-2);
  margin: 0;
}

/* CTA section — community outro */
.cta-header {
  text-align: center;
  margin: 64px auto 8px;
  max-width: 1152px;
  padding: 0 24px;
}

.cta-header h2 {
  font-size: 28px;
  font-weight: 700;
  color: var(--vp-c-text-1);
  margin: 0 0 8px;
  border: none;
  padding: 0;
}

.cta-header p {
  font-size: 16px;
  color: var(--vp-c-text-2);
  margin: 0;
}

.cta-cards {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 16px;
  max-width: 1152px;
  margin: 24px auto 64px;
  padding: 0 24px;
}

@media (min-width: 640px) {
  .cta-cards {
    padding: 0 48px;
  }
}

@media (min-width: 960px) {
  .cta-cards {
    padding: 0 64px;
  }
}

@media (max-width: 768px) {
  .cta-cards {
    grid-template-columns: repeat(2, 1fr);
  }
}

@media (max-width: 480px) {
  .cta-cards {
    grid-template-columns: 1fr;
  }
}

.cta-card,
.cta-card:hover {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 24px 16px;
  border-radius: 12px;
  border: 1px solid var(--vp-c-divider);
  background: var(--vp-c-bg-soft);
  text-decoration: none !important;
  text-align: center;
  transition: border-color 0.25s ease, box-shadow 0.25s ease;
  color: inherit;
}

.cta-card:hover {
  border-color: var(--vp-c-brand-1);
  box-shadow: 0 2px 12px rgba(0, 0, 0, 0.08);
}

.cta-card-icon {
  width: 48px;
  height: 48px;
}

/* CTA icon tinting — matching feature card icon style */
/* Roadmap — indigo/brand */
.cta-icon-roadmap {
  filter: invert(24%) sepia(79%) saturate(1742%) hue-rotate(216deg) brightness(92%) contrast(94%);
}
/* Catalog — green */
.cta-icon-catalog {
  filter: invert(37%) sepia(62%) saturate(592%) hue-rotate(113deg) brightness(92%) contrast(89%);
}
/* Discord — purple */
.cta-icon-discord {
  filter: invert(27%) sepia(51%) saturate(3264%) hue-rotate(253deg) brightness(88%) contrast(93%);
}
/* GitHub — amber */
.cta-icon-github {
  filter: invert(35%) sepia(56%) saturate(764%) hue-rotate(348deg) brightness(93%) contrast(89%);
}

/* Dark mode CTA icons */
.dark .cta-icon-roadmap {
  filter: invert(72%) sepia(40%) saturate(1059%) hue-rotate(197deg) brightness(104%) contrast(101%);
}
.dark .cta-icon-catalog {
  filter: invert(70%) sepia(52%) saturate(498%) hue-rotate(106deg) brightness(96%) contrast(91%);
}
.dark .cta-icon-discord {
  filter: invert(76%) sepia(30%) saturate(1148%) hue-rotate(230deg) brightness(103%) contrast(97%);
}
.dark .cta-icon-github {
  filter: invert(76%) sepia(54%) saturate(684%) hue-rotate(338deg) brightness(101%) contrast(96%);
}

.cta-card-text strong {
  display: block;
  font-size: 14px;
  font-weight: 600;
  color: var(--vp-c-text-1);
  margin-bottom: 4px;
  text-decoration: none;
}

.cta-card-text span {
  font-size: 13px;
  color: var(--vp-c-text-2);
  line-height: 1.5;
  text-decoration: none;
}

.cta-card:hover .cta-card-text strong,
.cta-card:hover .cta-card-text span {
  text-decoration: none;
}

/* Closing statement */
.closing {
  text-align: center;
  margin: 0 auto 64px;
  padding: 0 24px;
}

.closing-tagline {
  font-size: 18px;
  color: var(--vp-c-text-2);
  margin: 0;
}

/* Feature section images */
.feature-img {
  width: 100%;
  max-width: 240px;
  height: auto;
}
</style>
