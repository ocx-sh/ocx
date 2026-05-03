<script setup lang="ts">
/**
 * Self-rendering roadmap timeline card with alternating left/right layout
 * (via CSS :nth-child). `progress` (0..1) drives glow, border, shadow, ring
 * scale, and title accent on a cosine bell curve centered on the viewport.
 * `isVisible` (one-shot via IntersectionObserver) drives the entry fade.
 * Icon can be an emoji string or a file path (starts with / or contains .).
 */
import { ref, onMounted, onUnmounted } from 'vue'

const props = defineProps<{
  title: string
  icon?: string
  accent?: string
}>()

const isImageIcon =
  props.icon && (props.icon.startsWith('/') || props.icon.includes('.'))

const el = ref<HTMLElement | null>(null)
const progress = ref(0)
const isVisible = ref(false)
let raf = 0
let observer: IntersectionObserver | null = null

function update() {
  if (!el.value) return
  const rect = el.value.getBoundingClientRect()
  const viewH = window.innerHeight
  const elCenter = rect.top + rect.height / 2

  // Bell curve via cosine: 1 at viewport center, 0 at edges.
  const dist = Math.abs(elCenter - viewH / 2) / (viewH / 2)
  const clamped = Math.max(0, Math.min(1, dist))
  progress.value = Math.cos(clamped * Math.PI / 2)
}

function onScroll() {
  cancelAnimationFrame(raf)
  raf = requestAnimationFrame(update)
}

onMounted(() => {
  window.addEventListener('scroll', onScroll, { passive: true })
  window.addEventListener('resize', onScroll, { passive: true })
  update()

  // IntersectionObserver fires asynchronously after layout, so initial
  // opacity:0 paints before --visible flips and the transition can run.
  // Plain CSS transition triggered in onMounted skips on hydration because
  // Vue commits the class change in the same task as the first paint.
  if (el.value) {
    observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          isVisible.value = true
          observer?.disconnect()
          observer = null
        }
      },
      { rootMargin: '0px 0px -10% 0px' },
    )
    observer.observe(el.value)
  }
})

onUnmounted(() => {
  window.removeEventListener('scroll', onScroll)
  window.removeEventListener('resize', onScroll)
  cancelAnimationFrame(raf)
  observer?.disconnect()
})
</script>

<template>
  <div
    ref="el"
    class="timeline-item"
    :class="{ 'timeline-item--visible': isVisible }"
    :style="{
      '--accent-raw': accent ?? 'var(--vp-c-brand-1)',
      '--accent': `color-mix(in srgb, ${accent ?? 'var(--vp-c-brand-1)'} ${Math.round(15 + progress * 85)}%, var(--vp-c-text-3))`,
      '--progress': progress,
    }"
  >
    <!-- Node on the center line -->
    <div class="timeline-node">
      <div
        class="timeline-node-ring"
        :style="{
          transform: `scale(${1 + progress * 0.12})`,
          boxShadow: `0 0 0 ${progress * 6}px color-mix(in srgb, var(--accent) ${Math.round(progress * 25)}%, transparent), 0 0 ${Math.round(progress * 24)}px color-mix(in srgb, var(--accent) ${Math.round(progress * 20)}%, transparent)`,
        }"
      >
        <span
          v-if="isImageIcon"
          class="timeline-node-icon-img"
          :style="{
            maskImage: `url(${icon})`,
            WebkitMaskImage: `url(${icon})`,
            backgroundColor: 'var(--accent)',
          }"
        />
        <span v-else class="timeline-node-icon">{{ icon ?? '?' }}</span>
      </div>
      <div
        v-if="progress > 0.7"
        class="timeline-node-pulse"
        :style="{ opacity: (progress - 0.7) / 0.3 }"
      />
    </div>

    <!-- Card -->
    <div class="timeline-card">
      <div
        class="timeline-card-glow"
        :style="{ opacity: progress * progress }"
      />
      <div
        class="timeline-card-inner"
        :style="{
          borderColor: `color-mix(in srgb, var(--accent) ${Math.round(progress * progress * 40)}%, var(--vp-c-divider))`,
          boxShadow: `0 4px 24px -8px color-mix(in srgb, var(--accent) ${Math.round(progress * progress * 18)}%, transparent)`,
        }"
      >
        <div class="timeline-card-header">
          <span class="timeline-number" />
        </div>
        <h3
          class="timeline-title"
          :style="{
            color: progress > 0.3
              ? `color-mix(in srgb, var(--accent) ${Math.round(progress * progress * 100)}%, var(--vp-c-text-1))`
              : undefined,
          }"
        >{{ title }}</h3>
        <slot />
      </div>
    </div>
  </div>
</template>

<style scoped>
/* ── Timeline Item ─────────────────────────────────────────────────────── */
.timeline-item {
  position: relative;
  display: flex;
  align-items: flex-start;
  margin-bottom: 48px;
  counter-increment: roadmap-item;

  /* Default: card on the left */
  padding-right: calc(50% + 48px);
  padding-left: 0;

  /* Scroll reveal */
  opacity: 0;
  transform: translateY(32px);
  transition: opacity 0.6s ease, transform 0.6s ease;
}

.timeline-item--visible {
  opacity: 1;
  transform: translateY(0);
}

/* Alternate right on even items */
.timeline-item:nth-child(even) {
  padding-right: 0;
  padding-left: calc(50% + 48px);
  flex-direction: row-reverse;
}

/* ── Node (on the center line) ─────────────────────────────────────────── */
.timeline-node {
  position: absolute;
  left: 50%;
  top: 20px;
  transform: translateX(-50%);
  z-index: 2;
}

.timeline-node-ring {
  width: 52px;
  height: 52px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--vp-c-bg);
  border: 3px solid var(--accent);
  will-change: transform, box-shadow;
}

.timeline-node-icon {
  font-size: 22px;
  line-height: 1;
}

.timeline-node-icon-img {
  display: inline-block;
  width: 28px;
  height: 28px;
  mask-size: contain;
  -webkit-mask-size: contain;
  mask-repeat: no-repeat;
  -webkit-mask-repeat: no-repeat;
  mask-position: center;
  -webkit-mask-position: center;
}


.timeline-node-pulse {
  position: absolute;
  inset: -4px;
  border-radius: 50%;
  border: 2px solid var(--accent);
  animation: pulse 2.5s ease-out infinite;
  /* opacity and animationPlayState controlled via inline :style */
}

@keyframes pulse {
  0% {
    transform: scale(1);
    opacity: 0.4;
  }
  100% {
    transform: scale(1.6);
    opacity: 0;
  }
}

/* ── Card ──────────────────────────────────────────────────────────────── */
.timeline-card {
  position: relative;
  flex: 1;
  border-radius: 16px;
  overflow: hidden;
}

.timeline-card-glow {
  position: absolute;
  inset: 0;
  border-radius: 16px;
  /* Left-side cards: glow from top-right (toward the timeline line) */
  background: linear-gradient(
    225deg,
    color-mix(in srgb, var(--accent) 12%, transparent),
    transparent 70%
  );
  pointer-events: none;
  will-change: opacity;
}

/* Right-side cards: glow from top-left (toward the timeline line) */
.timeline-item:nth-child(even) .timeline-card-glow {
  background: linear-gradient(
    135deg,
    color-mix(in srgb, var(--accent) 12%, transparent),
    transparent 70%
  );
}

.timeline-card-inner {
  padding: 24px;
  border-radius: 16px;
  border: 1px solid var(--vp-c-divider);
  background: var(--vp-c-bg-soft);
  will-change: border-color, box-shadow;
}

.timeline-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}

/* ── Number (CSS counter) ──────────────────────────────────────────────── */
.timeline-number::before {
  content: '#' counter(roadmap-item);
  font-size: 12px;
  font-weight: 600;
  color: var(--vp-c-text-3);
  font-variant-numeric: tabular-nums;
}

/* ── Title ─────────────────────────────────────────────────────────────── */
.timeline-title {
  font-size: 20px;
  font-weight: 700;
  color: var(--vp-c-text-1);
  margin: 0 0 8px;
  line-height: 1.3;
}

/* ── Mobile ────────────────────────────────────────────────────────────── */
@media (max-width: 768px) {
  .timeline-item,
  .timeline-item:nth-child(even) {
    padding-right: 0;
    padding-left: 68px;
    flex-direction: row;
  }

  .timeline-node {
    left: 28px;
  }

  .timeline-node-ring {
    width: 44px;
    height: 44px;
  }

  .timeline-node-icon {
    font-size: 18px;
  }

  .timeline-node-icon-img {
    width: 24px;
    height: 24px;
  }

  .timeline-item {
    margin-bottom: 32px;
  }

  .timeline-card-inner {
    padding: 18px;
  }

  .timeline-title {
    font-size: 17px;
  }
}
</style>
