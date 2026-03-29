<script setup lang="ts">
/**
 * Feature row inside <RoadmapFeatures>.
 *
 * Renders as a lean table row: [status dot] [feature text] [github link icon]
 *
 * Props:
 *   status  — "shipped" | "active" | "planned" (default: "planned")
 *   issue   — GitHub issue number (links to ocx-sh/ocx/issues/{n})
 *   pr      — GitHub PR number (links to ocx-sh/ocx/pull/{n})
 */
defineProps<{
  status?: 'shipped' | 'active' | 'planned'
  issue?: string | number
  pr?: string | number
}>()

const repo = 'https://github.com/ocx-sh/ocx'

const statusTitles: Record<string, string> = {
  shipped: 'Shipped',
  active: 'In Progress',
  planned: 'Planned',
}
</script>

<template>
  <div class="feature-row" :class="`feature-row--${status ?? 'planned'}`">
    <span
      class="feature-dot"
      :title="statusTitles[status ?? 'planned']"
    />
    <span class="feature-text"><slot /></span>
    <a
      v-if="issue"
      :href="`${repo}/issues/${issue}`"
      target="_blank"
      rel="noreferrer"
      class="feature-link"
      :title="`#${issue}`"
      @click.stop
    >
      <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
        <path d="M8 9.5a1.5 1.5 0 100-3 1.5 1.5 0 000 3z" />
        <path d="M8 0a8 8 0 100 16A8 8 0 008 0zM1.5 8a6.5 6.5 0 1113 0 6.5 6.5 0 01-13 0z" />
      </svg>
    </a>
    <a
      v-if="pr"
      :href="`${repo}/pull/${pr}`"
      target="_blank"
      rel="noreferrer"
      class="feature-link"
      :title="`#${pr}`"
      @click.stop
    >
      <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
        <path d="M1.5 3.25a2.25 2.25 0 113 2.122v5.256a2.251 2.251 0 11-1.5 0V5.372A2.25 2.25 0 011.5 3.25zm5.677-.177L9.573.677A.25.25 0 0110 .854V2.5h1A2.5 2.5 0 0113.5 5v5.628a2.251 2.251 0 11-1.5 0V5a1 1 0 00-1-1h-1v1.646a.25.25 0 01-.427.177L7.177 3.427a.25.25 0 010-.354zM3.75 2.5a.75.75 0 100 1.5.75.75 0 000-1.5zm0 9.5a.75.75 0 100 1.5.75.75 0 000-1.5zm8.25.75a.75.75 0 10-1.5 0 .75.75 0 001.5 0z" />
      </svg>
    </a>
  </div>
</template>

<style scoped>
.feature-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 12px;
  font-size: 13px;
  color: var(--vp-c-text-2);
  background: color-mix(in srgb, var(--vp-c-bg) 50%, transparent);
}

.feature-row + .feature-row {
  border-top: 1px solid color-mix(in srgb, var(--vp-c-divider) 40%, transparent);
}

/* ── Status dot ────────────────────────────────────────────────────────── */
.feature-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  flex-shrink: 0;
}

.feature-row--planned .feature-dot {
  background: var(--vp-c-text-3);
}

.feature-row--active .feature-dot {
  background: var(--vp-c-brand-1);
  box-shadow: 0 0 4px var(--vp-c-brand-1);
}

.feature-row--shipped .feature-dot {
  background: var(--vp-c-green-1);
  box-shadow: 0 0 4px var(--vp-c-green-1);
}

/* ── Text ──────────────────────────────────────────────────────────────── */
.feature-text {
  flex: 1;
  line-height: 1.4;
}

/* ── GitHub link — color reflects feature status ───────────────────────── */
.feature-link {
  display: inline-flex;
  align-items: center;
  transition: color 0.2s, opacity 0.2s;
  text-decoration: none !important;
  flex-shrink: 0;
}

.feature-row--planned .feature-link {
  color: var(--vp-c-text-3);
  opacity: 0.5;
}

.feature-row--active .feature-link {
  color: var(--vp-c-brand-1);
  opacity: 0.7;
}

.feature-row--shipped .feature-link {
  color: var(--vp-c-green-1);
  opacity: 0.7;
}

.feature-link:hover {
  opacity: 1;
}
</style>
