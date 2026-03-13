<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'
import {
  StepperDescription,
  StepperIndicator,
  StepperItem,
  StepperRoot,
  StepperSeparator,
  StepperTitle,
  StepperTrigger,
} from 'reka-ui'

export interface Step {
  title: string
  description?: string
  status: 'complete' | 'current' | 'upcoming'
  /**
   * HTML string shown in the detail panel when this step is clicked.
   * Clicking the indicator or title of a step that has details opens the panel.
   * Clicking again collapses it.
   */
  details?: string
}

const props = withDefaults(defineProps<{
  steps: Step[]
  /** Layout direction. Defaults to vertical (best for roadmaps). */
  orientation?: 'horizontal' | 'vertical'
}>(), {
  orientation: 'vertical',
})

/** Radix Stepper uses a 1-indexed numeric model value for the active step. */
const modelValue = computed(() => {
  const idx = props.steps.findIndex(s => s.status === 'current')
  return idx >= 0 ? idx + 1 : props.steps.filter(s => s.status === 'complete').length + 1
})

const selectedIdx = ref<number | null>(null)
const hasAnyDetails = computed(() => props.steps.some(s => s.details))
const selectedStep = computed(() => selectedIdx.value !== null ? props.steps[selectedIdx.value] : null)

// Panel alignment — offset relative to StepperRoot (same grid column)
const stepperRootEl = ref<HTMLElement | null>(null)
const bodyEls: HTMLElement[] = []
const panelTop = ref(0)

async function toggle(i: number) {
  if (!props.steps[i]?.details) return
  selectedIdx.value = selectedIdx.value === i ? null : i
  if (selectedIdx.value !== null) {
    await nextTick()
    const body = bodyEls[selectedIdx.value]
    const root = stepperRootEl.value
    if (body && root) {
      panelTop.value = body.getBoundingClientRect().top - root.getBoundingClientRect().top
    }
  }
}
</script>

<template>
  <div class="vp-sw" :class="{ 'vp-sw--detail': hasAnyDetails }">
    <StepperRoot
      ref="stepperRootEl"
      :model-value="modelValue"
      :orientation="orientation"
      :linear="false"
      class="vp-stepper"
    >
      <StepperItem
        v-for="(step, i) in steps"
        :key="i"
        :step="i + 1"
        class="vp-stepper-item"
      >
        <StepperTrigger
          class="vp-stepper-trigger"
          :class="{ 'vp-stepper-trigger--clickable': step.details }"
          as="div"
          @click="toggle(i)"
        >
          <StepperIndicator
            class="vp-stepper-indicator"
            :class="{ 'vp-stepper-indicator--selected': selectedIdx === i }"
          >
            <svg v-if="step.status === 'complete'" width="13" height="13" viewBox="0 0 13 13" fill="none" aria-hidden="true">
              <path d="M2.5 6.5l3 3 5-5" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
            <span v-else>{{ i + 1 }}</span>
          </StepperIndicator>
        </StepperTrigger>

        <div
          :ref="(el) => { if (el) bodyEls[i] = el as HTMLElement }"
          class="vp-stepper-body"
          :class="{
            'vp-stepper-body--clickable': step.details,
            'vp-stepper-body--selected': selectedIdx === i,
          }"
          @click="toggle(i)"
        >
          <StepperTitle class="vp-stepper-title">{{ step.title }}</StepperTitle>
          <StepperDescription v-if="step.description" class="vp-stepper-desc">
            {{ step.description }}
          </StepperDescription>
        </div>

        <StepperSeparator v-if="i < steps.length - 1" class="vp-stepper-sep" />
      </StepperItem>
    </StepperRoot>

    <!-- Detail panel — shown to the right (or below on narrow screens) -->
    <Transition name="vp-detail">
      <div v-if="selectedStep" class="vp-stepper-panel" :style="{ marginTop: panelTop + 'px' }">
        <p class="vp-panel-title">{{ selectedStep.title }}</p>
        <!-- eslint-disable-next-line vue/no-v-html -->
        <div class="vp-panel-body" v-html="selectedStep.details" />
      </div>
    </Transition>
  </div>
</template>

<style scoped>
/* ── Wrapper ───────────────────────────────────────────────────────────── */
.vp-sw {
  display: block;
  padding: 8px 0;
}

.vp-sw--detail {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: 20px;
  align-items: start;
}

@media (max-width: 640px) {
  .vp-sw--detail {
    grid-template-columns: 1fr;
  }
}

/* ── Root ──────────────────────────────────────────────────────────────── */
.vp-stepper {
  display: flex;
}

.vp-stepper[data-orientation='vertical']   { flex-direction: column; }
.vp-stepper[data-orientation='horizontal'] { flex-direction: row; align-items: flex-start; }

/* ── Item ──────────────────────────────────────────────────────────────── */
.vp-stepper-item {
  position: relative;
  display: grid;
  grid-template-columns: 32px 1fr;
  grid-template-rows: auto 1fr;
  column-gap: 12px;
}

.vp-stepper[data-orientation='vertical']   .vp-stepper-item { padding-bottom: 24px; }
.vp-stepper[data-orientation='horizontal'] .vp-stepper-item {
  flex: 1;
  grid-template-columns: auto;
  grid-template-rows: auto auto 1fr;
  align-items: center;
  text-align: center;
  padding: 0 8px;
}

/* ── Trigger / indicator ───────────────────────────────────────────────── */
.vp-stepper-trigger {
  grid-column: 1;
  grid-row: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: default;
}

.vp-stepper-trigger--clickable { cursor: pointer; }

.vp-stepper[data-orientation='horizontal'] .vp-stepper-trigger { justify-self: center; }

.vp-stepper-indicator {
  width: 30px;
  height: 30px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 12px;
  font-weight: 600;
  border: 1px solid var(--vp-c-divider);
  background: var(--vp-c-bg);
  color: var(--vp-c-bg-alt);
  transition: background 0.2s, border-color 0.2s, color 0.2s, box-shadow 0.2s;
  flex-shrink: 0;
}

/* completed — deep saturated green in both modes */
.vp-stepper-item[data-state='completed'] .vp-stepper-indicator {
  background: var(--vp-c-green-1);
}

/* active — solid brand fill, consistent weight with 'complete' */
.vp-stepper-item[data-state='active'] .vp-stepper-indicator {
  background: var(--vp-c-brand);
}

/* inactive — solid brand fill, consistent weight with 'complete' */
.vp-stepper-item[data-state='inactive'] .vp-stepper-indicator {
  color: var(--vp-c-text-3);
}

/* ── Body ──────────────────────────────────────────────────────────────── */
.vp-stepper-body {
  grid-column: 2;
  grid-row: 1;
  padding-top: 4px;
  padding-left: 12px;
  border-radius: 6px;
  transition: background 0.15s;
}

.vp-stepper-body--clickable { cursor: pointer; }

.vp-stepper-body--selected {
  background: var(--vp-c-bg-soft);
  padding: 6px 12px;
  border-radius: 8px;
  border: 1px solid var(--vp-c-border);
}

.vp-stepper[data-orientation='horizontal'] .vp-stepper-body {
  grid-column: 1;
  grid-row: 2;
  padding-top: 8px;
}

.vp-stepper-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--vp-c-text-1);
  line-height: 1.4;
}

.vp-stepper-item[data-state='inactive'] .vp-stepper-title { color: var(--vp-c-text-3); }
/* don't dim when the detail panel is open for that step */
.vp-stepper-body--selected .vp-stepper-title { color: var(--vp-c-text-1) !important; }

.vp-stepper-desc {
  font-size: 13px;
  color: var(--vp-c-text-2);
  margin-top: 2px;
  line-height: 1.5;
}

.vp-stepper-item[data-state='inactive'] .vp-stepper-desc { color: var(--vp-c-text-3); }
.vp-stepper-body--selected .vp-stepper-desc { color: var(--vp-c-text-2) !important; }

/* ── Separator ─────────────────────────────────────────────────────────── */
.vp-stepper-sep {
  background: var(--vp-c-divider);
  transition: background 0.2s;
}

.vp-stepper-item[data-state='completed'] .vp-stepper-sep { background: var(--vp-c-green-1); }

.vp-stepper[data-orientation='vertical'] .vp-stepper-sep {
  position: absolute;
  left: 16px;
  transform: translateX(-50%);
  top: 34px;
  bottom: 0;
  width: 2px;
}

.vp-stepper[data-orientation='horizontal'] .vp-stepper-sep {
  grid-column: 1;
  grid-row: 3;
  position: absolute;
  top: 14px;
  left: calc(50% + 20px);
  right: calc(-50% + 20px);
  height: 2px;
}

/* ── Detail panel ──────────────────────────────────────────────────────── */
.vp-stepper-panel {
  border: 1px solid var(--vp-c-border);
  border-radius: 8px;
  background: var(--vp-c-bg-soft);
  padding: 16px 20px;
  align-self: start;
}

.vp-panel-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--vp-c-text-1);
  margin: 0 0 10px;
  padding-bottom: 8px;
  border-bottom: 1px solid var(--vp-c-divider);
}

.vp-panel-body {
  font-size: 13px;
  color: var(--vp-c-text-2);
  line-height: 1.6;
}

/* ── Transition ────────────────────────────────────────────────────────── */
.vp-detail-enter-active,
.vp-detail-leave-active {
  transition: opacity 0.18s ease, transform 0.18s ease;
}

.vp-detail-enter-from,
.vp-detail-leave-to {
  opacity: 0;
  transform: translateX(8px);
}
</style>
