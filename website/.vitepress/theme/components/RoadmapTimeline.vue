<script setup lang="ts">
import { computed, ref, useSlots } from 'vue'

const slots = useSlots()

interface RoadmapEntry {
  title: string
  icon: string
  description: string
  features: string[]
}

/** Parse <RoadmapItem> VNodes into data. */
const entries = computed<RoadmapEntry[]>(() => {
  const vnodes = slots.default?.() ?? []
  return vnodes
    .filter((v: any) => v?.props?.title)
    .map((v: any) => {
      // Features come as a prop (JSON array parsed by Vue)
      const rawFeatures = v.props.features
      const features: string[] = Array.isArray(rawFeatures) ? rawFeatures : []

      return {
        title: v.props.title as string,
        icon: (v.props.icon as string) ?? '?',
        description: (v.props.description as string) ?? '',
        features,
      }
    })
})

const selectedIdx = ref<number | null>(null)
const selectedEntry = computed(() =>
  selectedIdx.value !== null ? entries.value[selectedIdx.value] : null
)

function toggle(i: number) {
  selectedIdx.value = selectedIdx.value === i ? null : i
}
</script>

<template>
  <div class="roadmap">
    <!-- Horizontal timeline -->
    <div class="roadmap-track">
      <div class="roadmap-line" />
      <div
        v-for="(entry, i) in entries"
        :key="i"
        class="roadmap-node"
        :class="{ 'roadmap-node--active': selectedIdx === i }"
        @click="toggle(i)"
      >
        <div class="roadmap-circle">
          <span class="roadmap-icon">{{ entry.icon }}</span>
        </div>
        <div class="roadmap-label">{{ entry.title }}</div>
      </div>
    </div>

    <!-- Detail panel -->
    <Transition name="roadmap-panel">
      <div v-if="selectedEntry" :key="selectedIdx ?? 0" class="roadmap-detail">
        <div class="roadmap-detail-header">
          <span class="roadmap-detail-icon">{{ selectedEntry.icon }}</span>
          <h3 class="roadmap-detail-title">{{ selectedEntry.title }}</h3>
        </div>
        <p class="roadmap-detail-desc">{{ selectedEntry.description }}</p>
        <ul v-if="selectedEntry.features.length" class="roadmap-detail-features">
          <li v-for="(feat, j) in selectedEntry.features" :key="j">
            {{ feat }}
          </li>
        </ul>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
/* ── Track ─────────────────────────────────────────────────────────────── */
.roadmap-track {
  position: relative;
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  padding: 24px 16px 0;
  min-height: 120px;
}

.roadmap-line {
  position: absolute;
  top: 47px;
  left: 48px;
  right: 48px;
  height: 3px;
  background: var(--vp-c-divider);
  border-radius: 2px;
  z-index: 0;
}

/* ── Node ──────────────────────────────────────────────────────────────── */
.roadmap-node {
  position: relative;
  z-index: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  cursor: pointer;
  flex: 1;
  min-width: 0;
}

.roadmap-circle {
  width: 48px;
  height: 48px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--vp-c-bg);
  border: 3px solid var(--vp-c-divider);
  transition: border-color 0.25s, background 0.25s, transform 0.2s, box-shadow 0.25s;
  flex-shrink: 0;
}

.roadmap-node:hover .roadmap-circle {
  border-color: var(--vp-c-brand-light);
  transform: scale(1.08);
}

.roadmap-node--active .roadmap-circle {
  border-color: var(--vp-c-brand);
  background: var(--vp-c-brand);
  transform: scale(1.12);
  box-shadow: 0 0 0 4px var(--vp-c-brand-dimm);
}

.roadmap-icon {
  font-size: 20px;
  line-height: 1;
  transition: filter 0.2s;
}

.roadmap-node--active .roadmap-icon {
  filter: brightness(1.6) saturate(0.3);
}

.roadmap-label {
  margin-top: 10px;
  font-size: 12px;
  font-weight: 600;
  color: var(--vp-c-text-2);
  text-align: center;
  line-height: 1.3;
  max-width: 100px;
  transition: color 0.2s;
}

.roadmap-node--active .roadmap-label {
  color: var(--vp-c-brand);
}

/* ── Detail panel ──────────────────────────────────────────────────────── */
.roadmap-detail {
  margin-top: 24px;
  padding: 20px 24px;
  border: 1px solid var(--vp-c-border);
  border-radius: 12px;
  background: var(--vp-c-bg-soft);
}

.roadmap-detail-header {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}

.roadmap-detail-icon {
  font-size: 28px;
  line-height: 1;
}

.roadmap-detail-title {
  font-size: 18px;
  font-weight: 700;
  color: var(--vp-c-text-1);
  margin: 0;
  border: none;
  padding: 0;
}

.roadmap-detail-desc {
  font-size: 14px;
  color: var(--vp-c-text-2);
  line-height: 1.6;
  margin: 0 0 16px;
}

.roadmap-detail-features {
  list-style: none;
  padding: 0;
  margin: 0;
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
  gap: 8px;
}

.roadmap-detail-features li {
  font-size: 13px;
  color: var(--vp-c-text-1);
  line-height: 1.5;
  padding: 8px 12px;
  background: var(--vp-c-bg);
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
  display: flex;
  align-items: flex-start;
  gap: 8px;
}

.roadmap-detail-features li::before {
  content: '';
  display: inline-block;
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--vp-c-brand);
  flex-shrink: 0;
  margin-top: 6px;
}

/* ── Transition ────────────────────────────────────────────────────────── */
.roadmap-panel-enter-active,
.roadmap-panel-leave-active {
  transition: opacity 0.22s ease, transform 0.22s ease;
}

.roadmap-panel-enter-from,
.roadmap-panel-leave-to {
  opacity: 0;
  transform: translateY(8px);
}

/* ── Mobile ────────────────────────────────────────────────────────────── */
@media (max-width: 640px) {
  .roadmap-track {
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px 4px;
    padding: 16px 8px 0;
  }

  .roadmap-line {
    display: none;
  }

  .roadmap-node {
    flex: 0 0 calc(33.33% - 8px);
  }

  .roadmap-circle {
    width: 42px;
    height: 42px;
  }

  .roadmap-icon {
    font-size: 18px;
  }

  .roadmap-label {
    font-size: 11px;
  }

  .roadmap-detail-features {
    grid-template-columns: 1fr;
  }
}
</style>
