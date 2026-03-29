---
layout: page
title: Package Catalog
---

<div class="catalog-page">
  <div class="catalog-header">
    <h1>Catalog</h1>
    <p>Browse available packages on the <a href="https://ocx.sh">ocx.sh</a> registry.</p>
  </div>

  <PackageCatalog />

  <div class="catalog-cta">
    <h2>Didn't find your package?</h2>
    <p>Request it, discuss it, or contribute a mirror configuration.</p>
    <div class="catalog-cta-cards">
      <a href="https://github.com/ocx-sh/ocx/issues/new?template=package_request.yml" target="_blank" rel="noreferrer" class="catalog-cta-card">
        <img src="/licensed/icons/cta-github.svg" alt="" class="catalog-cta-icon catalog-icon-issues" />
        <div class="catalog-cta-text">
          <strong>Request</strong>
          <span>Open an issue to request a new package.</span>
        </div>
      </a>
      <a href="https://discord.gg/BuRhhAYy9r" target="_blank" rel="noreferrer" class="catalog-cta-card">
        <img src="/licensed/icons/cta-discord.svg" alt="" class="catalog-cta-icon catalog-icon-discord" />
        <div class="catalog-cta-text">
          <strong>Discord</strong>
          <span>Ask the community for help or suggestions.</span>
        </div>
      </a>
      <a href="https://github.com/ocx-sh/ocx" target="_blank" rel="noreferrer" class="catalog-cta-card">
        <img src="/licensed/icons/cta-contribute.svg" alt="" class="catalog-cta-icon catalog-icon-contribute" />
        <div class="catalog-cta-text">
          <strong>Contribute</strong>
          <span>Add a mirror config and publish your own.</span>
        </div>
      </a>
    </div>
  </div>
</div>

<style>
.catalog-page {
  max-width: 1152px;
  margin: 0 auto;
  padding: 48px 24px 80px;
}

.catalog-header {
  text-align: center;
  margin-bottom: 40px;
}

.catalog-header h1 {
  font-size: 32px;
  font-weight: 800;
  color: var(--vp-c-text-1);
  margin: 0 0 20px;
  letter-spacing: -0.01em;
}

.catalog-header p {
  font-size: 16px;
  color: var(--vp-c-text-2);
  margin: 0;
}

.catalog-header a {
  color: var(--vp-c-brand-1);
  text-decoration: underline;
  text-underline-offset: 2px;
}

@media (min-width: 640px) {
  .catalog-page {
    padding: 64px 48px 80px;
  }
}

@media (min-width: 960px) {
  .catalog-page {
    padding: 64px 64px 80px;
  }
}

/* ── CTA ───────────────────────────────────────────────────────────────── */
.catalog-cta {
  text-align: center;
  margin-top: 80px;
  padding-top: 48px;
  border-top: 1px solid var(--vp-c-divider);
}

.catalog-cta h2 {
  font-size: 24px;
  font-weight: 700;
  color: var(--vp-c-text-1);
  margin: 0 0 12px;
}

.catalog-cta p {
  font-size: 15px;
  color: var(--vp-c-text-2);
  margin: 0 0 32px;
}

.catalog-cta-cards {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 16px;
}

.catalog-cta-card,
.catalog-cta-card:hover {
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

.catalog-cta-card:hover {
  border-color: var(--vp-c-brand-1);
  box-shadow: 0 2px 12px rgba(0, 0, 0, 0.08);
}

.catalog-cta-icon {
  width: 40px;
  height: 40px;
}

.catalog-icon-issues {
  filter: invert(37%) sepia(62%) saturate(592%) hue-rotate(113deg) brightness(92%) contrast(89%);
}
.catalog-icon-discord {
  filter: invert(27%) sepia(51%) saturate(3264%) hue-rotate(253deg) brightness(88%) contrast(93%);
}
.catalog-icon-contribute {
  filter: invert(24%) sepia(79%) saturate(1742%) hue-rotate(216deg) brightness(92%) contrast(94%);
}

.dark .catalog-icon-issues {
  filter: invert(70%) sepia(52%) saturate(498%) hue-rotate(106deg) brightness(96%) contrast(91%);
}
.dark .catalog-icon-discord {
  filter: invert(76%) sepia(30%) saturate(1148%) hue-rotate(230deg) brightness(103%) contrast(97%);
}
.dark .catalog-icon-contribute {
  filter: invert(72%) sepia(40%) saturate(1059%) hue-rotate(197deg) brightness(104%) contrast(101%);
}

.catalog-cta-text strong {
  display: block;
  font-size: 14px;
  font-weight: 600;
  color: var(--vp-c-text-1);
  margin-bottom: 4px;
}

.catalog-cta-text span {
  font-size: 13px;
  color: var(--vp-c-text-2);
  line-height: 1.5;
}

@media (max-width: 768px) {
  .catalog-cta-cards {
    grid-template-columns: 1fr;
  }
}
</style>
