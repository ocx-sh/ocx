<script setup lang="ts">
import { computed, nextTick, ref, useSlots } from 'vue'
import DescriptionComp from './Description.vue'

const slots = useSlots()

interface StepData {
  title: string
  description?: string
  status: 'complete' | 'current' | 'upcoming'
  hasDetails: boolean
}

/** Extract plain text from a <Description> VNode's default slot. */
function descText(vnode: any): string | undefined {
  const slot = vnode?.children?.default
  if (typeof slot !== 'function') return undefined
  const text = slot()
    .map((v: any) => (typeof v.children === 'string' ? v.children : ''))
    .join('')
    .trim()
  return text || undefined
}

/** True if this VNode carries real content (not just whitespace text). */
function isMeaningful(v: any): boolean {
  if (typeof v.children === 'string') return v.children.trim() !== ''
  return v.type != null
}

/** Pull step metadata + slot children from the <Step> VNodes without mounting them. */
const stepVnodes = computed(() =>
  (slots.default?.() ?? []).filter((v: any) => v?.props?.title)
)

const steps = computed<StepData[]>(() =>
  stepVnodes.value.map((v: any) => {
    const childVnodes =
      typeof v.children?.default === 'function' ? v.children.default() : []
    const descVnode = childVnodes.find((cv: any) => cv?.type === DescriptionComp)
    const detailVnodes = childVnodes.filter(
      (cv: any) => cv?.type !== DescriptionComp && isMeaningful(cv)
    )
    return {
      title: v.props.title as string,
      description: descVnode ? descText(descVnode) : (v.props.description as string | undefined),
      status: (v.props.status ?? 'upcoming') as StepData['status'],
      hasDetails: detailVnodes.length > 0,
    }
  })
)

const hasAnyDetails = computed(() => steps.value.some(s => s.hasDetails))

function dataState(status: StepData['status']) {
  if (status === 'complete') return 'completed'
  if (status === 'current') return 'active'
  return 'inactive'
}

// ── Selection & panel alignment ────────────────────────────────────────── //
const selectedIdx = ref<number | null>(null)
const stepperEl = ref<HTMLElement | null>(null)
const bodyEls: HTMLElement[] = []
const panelTop = ref(0)

const selectedStep = computed(() =>
  selectedIdx.value !== null ? steps.value[selectedIdx.value] : null
)

/**
 * Returns a render function that produces the selected step's slot VNodes,
 * with <Description> elements stripped out (they live in the sidebar, not the panel).
 */
const DetailsRenderer = computed<(() => any) | null>(() => {
  if (selectedIdx.value === null) return null
  const vnode = stepVnodes.value[selectedIdx.value]
  if (typeof vnode?.children?.default !== 'function') return null
  const detail = vnode.children
    .default()
    .filter((v: any) => v?.type !== DescriptionComp && isMeaningful(v))
  if (detail.length === 0) return null
  return () => detail
})

async function toggle(i: number) {
  if (!steps.value[i]?.hasDetails) return
  selectedIdx.value = selectedIdx.value === i ? null : i
  if (selectedIdx.value !== null) {
    await nextTick()
    const body = bodyEls[selectedIdx.value]
    const root = stepperEl.value
    if (body && root) {
      panelTop.value =
        body.getBoundingClientRect().top - root.getBoundingClientRect().top
    }
  }
}
</script>

<template>
  <div class="vp-sw" :class="{ 'vp-sw--detail': hasAnyDetails }">
    <div ref="stepperEl" class="vp-stepper" data-orientation="vertical">
      <div
        v-for="(step, i) in steps"
        :key="i"
        class="vp-stepper-item"
        :data-state="dataState(step.status)"
      >
        <!-- Indicator circle -->
        <div
          class="vp-stepper-trigger"
          :class="{ 'vp-stepper-trigger--clickable': step.hasDetails }"
          @click="toggle(i)"
        >
          <div
            class="vp-stepper-indicator"
            :class="{ 'vp-stepper-indicator--selected': selectedIdx === i }"
          >
            <svg v-if="step.status === 'complete'" width="13" height="13" viewBox="0 0 13 13" fill="none" aria-hidden="true">
              <path d="M2.5 6.5l3 3 5-5" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
            <span v-else>{{ i + 1 }}</span>
          </div>
        </div>

        <!-- Title + description -->
        <div
          :ref="(el) => { if (el) bodyEls[i] = el as HTMLElement }"
          class="vp-stepper-body"
          :class="{
            'vp-stepper-body--clickable': step.hasDetails,
            'vp-stepper-body--selected': selectedIdx === i,
          }"
          @click="toggle(i)"
        >
          <div class="vp-stepper-title">{{ step.title }}</div>
          <div v-if="step.description" class="vp-stepper-desc">{{ step.description }}</div>
        </div>

        <!-- Connector line -->
        <div v-if="i < steps.length - 1" class="vp-stepper-sep" />
      </div>
    </div>

    <!-- Detail panel — aligned with selected step via marginTop -->
    <Transition name="vp-detail">
      <div
        v-if="selectedStep"
        class="vp-stepper-panel"
        :style="{ marginTop: panelTop + 'px' }"
      >
        <p class="vp-panel-title">{{ selectedStep.title }}</p>
        <div class="vp-panel-body">
          <component :is="DetailsRenderer" />
        </div>
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
  .vp-sw--detail { grid-template-columns: 1fr; }
}

/* ── Stepper root ───────────────────────────────────────────────────────── */
.vp-stepper { display: flex; flex-direction: column; }

/* ── Item ──────────────────────────────────────────────────────────────── */
.vp-stepper-item {
  position: relative;
  display: grid;
  grid-template-columns: 32px 1fr;
  grid-template-rows: auto 1fr;
  column-gap: 12px;
  padding-bottom: 24px;
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
  color: var(--vp-c-text-3);
  transition: background 0.2s, border-color 0.2s, color 0.2s, box-shadow 0.2s;
  flex-shrink: 0;
}

.vp-stepper-item[data-state='completed'] .vp-stepper-indicator {
  background: var(--vp-c-green-1);
  border-color: var(--vp-c-green-1);
  color: var(--vp-c-bg);
}

.vp-stepper-item[data-state='active'] .vp-stepper-indicator {
  background: var(--vp-c-brand);
  border-color: var(--vp-c-brand);
  color: var(--vp-c-bg);
}

/* ── Body ──────────────────────────────────────────────────────────────── */
.vp-stepper-body {
  grid-column: 2;
  grid-row: 1;
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

.vp-stepper-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--vp-c-text-1);
  line-height: 1.4;
  margin: 0;
  padding: 0;
}
.vp-stepper-item[data-state='inactive'] .vp-stepper-title { color: var(--vp-c-text-3); }
.vp-stepper-body--selected .vp-stepper-title { color: var(--vp-c-text-1) !important; }

.vp-stepper-desc {
  font-size: 13px;
  color: var(--vp-c-text-2);
  margin: 2px 0 0;
  padding: 0;
  line-height: 1.5;
}
.vp-stepper-item[data-state='inactive'] .vp-stepper-desc { color: var(--vp-c-text-3); }
.vp-stepper-body--selected .vp-stepper-desc { color: var(--vp-c-text-2) !important; }

/* ── Separator ─────────────────────────────────────────────────────────── */
.vp-stepper-sep {
  position: absolute;
  left: 16px;
  transform: translateX(-50%);
  top: 48px;
  bottom: 0;
  width: 2px;
  background: var(--vp-c-divider);
  border-radius: 1px;
  transition: background 0.2s;
}
.vp-stepper-item[data-state='completed'] .vp-stepper-sep { background: var(--vp-c-green-1); }

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
.vp-panel-body :deep(p) { margin: 0 0 8px; }
.vp-panel-body :deep(p:last-child) { margin-bottom: 0; }
.vp-panel-body :deep(code) {
  font-size: 12px;
  background: var(--vp-c-bg-mute);
  padding: 1px 5px;
  border-radius: 4px;
}

/* ── Transition ────────────────────────────────────────────────────────── */
.vp-detail-enter-active,
.vp-detail-leave-active { transition: opacity 0.18s ease, transform 0.18s ease; }
.vp-detail-enter-from,
.vp-detail-leave-to { opacity: 0; transform: translateX(8px); }
</style>
