<script setup lang="ts">
/**
 * Standalone roadmap page layout with hero, vertical timeline, and CTA.
 *
 * Timeline line: single element whose gradient is built from the actual
 * positions of the icon elements (.timeline-node-icon / .timeline-node-icon-img)
 * inside each RoadmapItem, plus each item's --accent color.
 *
 * Gradient structure:
 *   transparent → fade-in → [accent₁ at node₁] → [accent₂ at node₂] → … → fade-out → transparent
 *
 * A scroll-driven mask dims the line outside the viewport.
 */
import { ref, onMounted, onUnmounted, nextTick } from 'vue'

const timelineEl = ref<HTMLElement | null>(null)
const lineStyle = ref<Record<string, string>>({})

let raf = 0

function update() {
  if (!timelineEl.value) return
  const container = timelineEl.value
  const items = container.querySelectorAll<HTMLElement>('.timeline-item')
  if (items.length < 2) return

  const containerRect = container.getBoundingClientRect()
  const h = containerRect.height
  if (h <= 0) return

  // Collect node center positions and accent colors
  const nodes: { pct: number; color: string }[] = []
  for (const item of items) {
    const icon = item.querySelector('.timeline-node-icon, .timeline-node-icon-img')
    if (!icon) continue
    const iconRect = icon.getBoundingClientRect()
    const center = iconRect.top + iconRect.height / 2 - containerRect.top
    const pct = (center / h) * 100
    const color = getComputedStyle(item).getPropertyValue('--accent').trim() || '#888'
    nodes.push({ pct, color })
  }
  if (nodes.length < 2) return

  // Build gradient: fade-in → node colors at exact positions → fade-out
  const fadeLen = 3
  const stops: string[] = []
  stops.push(`transparent ${Math.max(0, nodes[0].pct - fadeLen).toFixed(1)}%`)
  for (const n of nodes) {
    stops.push(`${n.color} ${n.pct.toFixed(1)}%`)
  }
  stops.push(`transparent ${Math.min(100, nodes[nodes.length - 1].pct + fadeLen).toFixed(1)}%`)
  const gradient = `linear-gradient(to bottom, ${stops.join(', ')})`

  // Scroll-driven spotlight mask
  const lineEl = container.querySelector('.timeline-line') as HTMLElement
  if (!lineEl) return
  const lineRect = lineEl.getBoundingClientRect()
  const lineH = lineRect.height
  if (lineH <= 0) return

  const viewH = window.innerHeight
  const vpTop = Math.max(0, (0 - lineRect.top) / lineH)
  const vpBottom = Math.min(1, (viewH - lineRect.top) / lineH)

  const fade = 0.04
  const t1 = Math.max(0, vpTop - fade)
  const t2 = Math.min(1, vpTop + fade)
  const b1 = Math.max(0, vpBottom - fade)
  const b2 = Math.min(1, vpBottom + fade)
  const dim = 0.1

  const spotlightMask = `linear-gradient(to bottom, rgba(0,0,0,${dim}) ${(t1 * 100).toFixed(1)}%, black ${(t2 * 100).toFixed(1)}%, black ${(b1 * 100).toFixed(1)}%, rgba(0,0,0,${dim}) ${(b2 * 100).toFixed(1)}%)`
  const horizMask = 'linear-gradient(to right, transparent, black 30%, black 70%, transparent)'

  lineStyle.value = {
    background: gradient,
    maskImage: `${horizMask}, ${spotlightMask}`,
    WebkitMaskImage: `${horizMask}, ${spotlightMask}`,
  }
}

function onScroll() {
  cancelAnimationFrame(raf)
  raf = requestAnimationFrame(update)
}

onMounted(async () => {
  await nextTick()
  update()
  window.addEventListener('scroll', onScroll, { passive: true })
  window.addEventListener('resize', onScroll, { passive: true })
})

onUnmounted(() => {
  window.removeEventListener('scroll', onScroll)
  window.removeEventListener('resize', onScroll)
  cancelAnimationFrame(raf)
})
</script>

<template>
  <div class="roadmap-page">
    <!-- Hero -->
    <section class="roadmap-hero">
      <div class="roadmap-hero-bg" />
      <div class="roadmap-hero-content">
        <div class="roadmap-hero-badge">Roadmap</div>
        <h1 class="roadmap-hero-title">
          Where we're <span class="roadmap-hero-accent">headed</span>
        </h1>
        <p class="roadmap-hero-subtitle">
          OCX is under active development. Here's what we're building
          and what's coming next.
        </p>
        <div class="roadmap-hero-legend">
          <span class="legend-item legend-active">
            <span class="legend-dot" />In Progress
          </span>
          <span class="legend-item legend-planned">
            <span class="legend-dot" />Planned
          </span>
          <span class="legend-item legend-shipped">
            <span class="legend-dot" />Shipped
          </span>
        </div>
      </div>
    </section>

    <!-- Timeline -->
    <section ref="timelineEl" class="roadmap-timeline">
      <div
        class="timeline-line"
        :style="lineStyle"
      />
      <slot />
    </section>

    <!-- CTA -->
    <section class="roadmap-cta">
      <h2 class="roadmap-cta-title">Get Involved</h2>
      <p class="roadmap-cta-subtitle">
        OCX is open source. If any of these milestones matter to your workflow,
        we'd love to hear from you.
      </p>
      <div class="roadmap-cta-cards">
        <a href="https://discord.gg/BuRhhAYy9r" target="_blank" rel="noreferrer" class="cta-card">
          <img src="/licensed/icons/cta-discord.svg" alt="" class="cta-card-icon cta-icon-discord" />
          <div class="cta-card-text">
            <strong>Discord</strong>
            <span>Discuss priorities and share feedback on what to build next.</span>
          </div>
        </a>
        <a href="https://github.com/ocx-sh/ocx/issues" target="_blank" rel="noreferrer" class="cta-card">
          <img src="/licensed/icons/cta-github.svg" alt="" class="cta-card-icon cta-icon-github" />
          <div class="cta-card-text">
            <strong>Issues</strong>
            <span>Request features, report bugs, and vote on what matters.</span>
          </div>
        </a>
        <a href="https://github.com/ocx-sh/ocx" target="_blank" rel="noreferrer" class="cta-card">
          <img src="/licensed/icons/cta-contribute.svg" alt="" class="cta-card-icon cta-icon-contribute" />
          <div class="cta-card-text">
            <strong>Contribute</strong>
            <span>Open a pull request and help build these features.</span>
          </div>
        </a>
        <a href="/docs/changelog" class="cta-card">
          <img src="/licensed/icons/cta-changelog.svg" alt="" class="cta-card-icon cta-icon-changelog" />
          <div class="cta-card-text">
            <strong>Changelog</strong>
            <span>See what already shipped and what landed recently.</span>
          </div>
        </a>
      </div>
    </section>
  </div>
</template>

<style scoped>
/* ── Page ──────────────────────────────────────────────────────────────── */
.roadmap-page {
  --page-max: 960px;
  min-height: 100vh;
}

/* ── Hero ──────────────────────────────────────────────────────────────── */
.roadmap-hero {
  position: relative;
  padding: 80px 24px 64px;
  text-align: center;
}

.roadmap-hero-bg {
  position: absolute;
  inset: -40% -20% -60% -20%;
  background:
    radial-gradient(ellipse 60% 40% at 50% 20%, color-mix(in srgb, var(--vp-c-brand-1) 10%, transparent), transparent),
    radial-gradient(ellipse 40% 35% at 75% 50%, color-mix(in srgb, #8b5cf6 6%, transparent), transparent),
    radial-gradient(ellipse 35% 30% at 25% 60%, color-mix(in srgb, #06b6d4 5%, transparent), transparent);
  pointer-events: none;
}

.roadmap-hero-content {
  position: relative;
  max-width: var(--page-max);
  margin: 0 auto;
}

.roadmap-hero-badge {
  display: inline-block;
  padding: 4px 16px;
  border-radius: 100px;
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--vp-c-brand-1);
  background: color-mix(in srgb, var(--vp-c-brand-1) 10%, transparent);
  border: 1px solid color-mix(in srgb, var(--vp-c-brand-1) 20%, transparent);
  margin-bottom: 24px;
}

.roadmap-hero-title {
  font-size: 48px;
  font-weight: 800;
  line-height: 1.1;
  color: var(--vp-c-text-1);
  margin: 0 0 16px;
  letter-spacing: -0.02em;
}

.roadmap-hero-accent {
  background: linear-gradient(135deg, var(--vp-c-brand-1), #8b5cf6, #06b6d4);
  background-clip: text;
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
}

.roadmap-hero-subtitle {
  font-size: 18px;
  color: var(--vp-c-text-2);
  line-height: 1.6;
  margin: 0 auto 32px;
  max-width: 520px;
}

.roadmap-hero-legend {
  display: flex;
  justify-content: center;
  gap: 24px;
  flex-wrap: wrap;
}

.legend-item {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  font-weight: 500;
  color: var(--vp-c-text-2);
}

.legend-dot {
  width: 10px;
  height: 10px;
  border-radius: 50%;
}

.legend-active .legend-dot {
  background: var(--vp-c-brand-1);
  box-shadow: 0 0 8px color-mix(in srgb, var(--vp-c-brand-1) 50%, transparent);
}

.legend-planned .legend-dot {
  background: var(--vp-c-text-3);
}

.legend-shipped .legend-dot {
  background: var(--vp-c-green-1);
  box-shadow: 0 0 8px color-mix(in srgb, var(--vp-c-green-1) 50%, transparent);
}

/* ── Timeline ──────────────────────────────────────────────────────────── */
.roadmap-timeline {
  position: relative;
  max-width: var(--page-max);
  margin: 0 auto;
  padding: 120px 24px 80px;
  counter-reset: roadmap-item;
}

/* Timeline line — gradient and masks set dynamically via inline :style */
.timeline-line {
  position: absolute;
  left: 50%;
  top: 0;
  bottom: 0;
  width: 4px;
  transform: translateX(-50%);
  border-radius: 2px;
  opacity: 0.7;
  mask-composite: intersect;
  -webkit-mask-composite: source-in;
}

@media (max-width: 768px) {
  .roadmap-hero {
    padding: 48px 20px 40px;
  }

  .roadmap-hero-title {
    font-size: 32px;
  }

  .roadmap-hero-subtitle {
    font-size: 16px;
  }

  .roadmap-timeline {
    padding: 0 16px 48px;
  }

  .timeline-line {
    left: 28px;
  }
}

@media (max-width: 480px) {
  .roadmap-hero-title {
    font-size: 28px;
  }

  .roadmap-hero-legend {
    gap: 16px;
  }
}

/* ── CTA ───────────────────────────────────────────────────────────────── */
.roadmap-cta {
  text-align: center;
  padding: 0 24px 120px;
  max-width: var(--page-max);
  margin: 0 auto;
}

.roadmap-cta-title {
  font-size: 28px;
  font-weight: 700;
  color: var(--vp-c-text-1);
  margin: 0 0 20px;
  letter-spacing: -0.01em;
}

.roadmap-cta-subtitle {
  font-size: 16px;
  color: var(--vp-c-text-2);
  margin: 0 auto 40px;
  max-width: 480px;
  line-height: 1.6;
}

.roadmap-cta-cards {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 20px;
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

/* Icon tinting — matching index page style */
/* Discord — purple */
.cta-icon-discord {
  filter: invert(27%) sepia(51%) saturate(3264%) hue-rotate(253deg) brightness(88%) contrast(93%);
}
/* Issues — green */
.cta-icon-github {
  filter: invert(37%) sepia(62%) saturate(592%) hue-rotate(113deg) brightness(92%) contrast(89%);
}
/* Contribute — indigo/brand */
.cta-icon-contribute {
  filter: invert(24%) sepia(79%) saturate(1742%) hue-rotate(216deg) brightness(92%) contrast(94%);
}
/* Changelog — amber */
.cta-icon-changelog {
  filter: invert(35%) sepia(56%) saturate(764%) hue-rotate(348deg) brightness(93%) contrast(89%);
}

/* Dark mode */
.dark .cta-icon-discord {
  filter: invert(76%) sepia(30%) saturate(1148%) hue-rotate(230deg) brightness(103%) contrast(97%);
}
.dark .cta-icon-github {
  filter: invert(70%) sepia(52%) saturate(498%) hue-rotate(106deg) brightness(96%) contrast(91%);
}
.dark .cta-icon-contribute {
  filter: invert(72%) sepia(40%) saturate(1059%) hue-rotate(197deg) brightness(104%) contrast(101%);
}
.dark .cta-icon-changelog {
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

@media (max-width: 768px) {
  .roadmap-cta-cards {
    grid-template-columns: repeat(2, 1fr);
  }

  .roadmap-cta-title {
    font-size: 24px;
  }
}

@media (max-width: 480px) {
  .roadmap-cta-cards {
    grid-template-columns: 1fr;
  }
}
</style>
