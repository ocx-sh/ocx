<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'

/**
 * Chess-layout feature section: text on one side, visual on the other.
 * Set `flip` to put the visual on the left.
 *
 * Scroll reveal: cards start small and transparent, then scale up and
 * fade in as they approach the vertical center of the viewport.
 * Progress is a continuous 0→1 value based on distance from center.
 */
defineProps<{
  title: string
  flip?: boolean
}>()

const el = ref<HTMLElement | null>(null)
const progress = ref(0)
const peak = ref(0)
let raf = 0

function update() {
  if (!el.value) return
  const rect = el.value.getBoundingClientRect()
  const viewH = window.innerHeight
  const elCenter = rect.top + rect.height / 2

  // Distance from viewport center, normalized to 0–1
  const dist = Math.abs(elCenter - viewH / 2) / (viewH / 2)
  let raw = Math.max(0, Math.min(1, 1 - dist))

  // If we're at the bottom of the page and the element is fully visible,
  // treat it as fully revealed
  const atBottom = window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 2
  const fullyVisible = rect.top >= 0 && rect.bottom <= viewH
  if (atBottom && fullyVisible) raw = 1

  // Track the highest progress ever reached
  peak.value = Math.max(peak.value, raw)

  progress.value = raw
}

function onScroll() {
  cancelAnimationFrame(raf)
  raf = requestAnimationFrame(update)
}

onMounted(() => {
  window.addEventListener('scroll', onScroll, { passive: true })
  window.addEventListener('resize', onScroll, { passive: true })
  update()
})

onUnmounted(() => {
  window.removeEventListener('scroll', onScroll)
  window.removeEventListener('resize', onScroll)
  cancelAnimationFrame(raf)
})
</script>

<template>
  <section
    ref="el"
    class="feature-section"
    :class="{ 'feature-section--flip': flip }"
    :style="{
      opacity: 0.4 + peak * 0.6,
      transform: `scale(${0.92 + peak * 0.08})`,
      borderColor: `color-mix(in srgb, var(--vp-c-brand-2) ${Math.round(progress * progress * 100)}%, var(--vp-c-divider))`,
      boxShadow: `0 0 24px -6px color-mix(in srgb, var(--vp-c-brand-1) ${Math.round(progress * progress * 12)}%, transparent)`,
    }"
  >
    <div class="feature-text">
      <h2 class="feature-title">{{ title }}</h2>
      <div class="feature-body">
        <slot name="text" />
      </div>
    </div>
    <div class="feature-visual">
      <slot />
    </div>
  </section>
</template>

<style scoped>
.feature-section {
  display: grid;
  grid-template-columns: 3fr 2fr;
  gap: 48px;
  align-items: center;
  margin: 0 auto;
  padding: 40px 40px;
  border-radius: 12px;
  background: var(--vp-c-bg-soft);
  border: 1px solid var(--vp-c-divider);
  will-change: transform, opacity;
}

.feature-section--flip {
  grid-template-columns: 2fr 3fr;
}

.feature-section--flip .feature-visual {
  order: -1;
}

.feature-title {
  font-size: 24px;
  font-weight: 700;
  color: var(--vp-c-text-1);
  margin: 0 0 16px;
  line-height: 1.3;
  border: none;
  padding: 0;
}

.feature-body {
  font-size: 15px;
  color: var(--vp-c-text-2);
  line-height: 1.7;
}

.feature-body :deep(p) {
  margin: 0 0 12px;
}

.feature-body :deep(p:last-child) {
  margin-bottom: 0;
}

.feature-body :deep(code) {
  font-size: 13px;
  background: var(--vp-c-bg-mute);
  padding: 2px 6px;
  border-radius: 4px;
}

.feature-visual {
  display: flex;
  align-items: center;
  justify-content: center;
}

.feature-visual :deep(.terminal) {
  width: 100%;
}

.feature-visual :deep(.feature-img) {
  width: 100%;
  max-width: 240px;
  height: auto;
}

@media (max-width: 768px) {
  .feature-section {
    grid-template-columns: 1fr;
    gap: 32px;
    padding: 28px 24px;
  }

  .feature-section--flip .feature-visual {
    order: unset;
  }
}
</style>
