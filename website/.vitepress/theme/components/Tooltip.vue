<script setup lang="ts">
import {
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
  TooltipContent,
  TooltipPortal,
  TooltipArrow,
} from 'reka-ui'

withDefaults(defineProps<{
  /** The trigger text shown inline with a dashed underline. */
  term: string
  side?: 'top' | 'bottom' | 'left' | 'right'
  delayDuration?: number
}>(), {
  side: 'top',
  delayDuration: 400,
})
</script>

<template>
  <TooltipProvider :delay-duration="delayDuration">
    <TooltipRoot>
      <TooltipTrigger as-child>
        <span class="vp-tt-trigger">{{ term }}</span>
      </TooltipTrigger>
      <TooltipPortal>
        <TooltipContent :side="side" :side-offset="8" class="vp-tt-content">
          <slot />
          <TooltipArrow class="vp-tt-arrow" :width="12" :height="6" />
        </TooltipContent>
      </TooltipPortal>
    </TooltipRoot>
  </TooltipProvider>
</template>

<style>
/* Not scoped — the portal renders in document.body outside this component's subtree. */

.vp-tt-trigger {
  border-bottom: 1px dashed var(--vp-c-text-3);
  cursor: help;
}

.vp-tt-content {
  /* Must sit above VitePress sidebar (~z-index 30) and nav (~z-index 100). */
  z-index: 9999;
  padding: 10px 14px;
  max-width: 300px;
  border-radius: 8px;
  background-color: var(--vp-c-bg);
  color: var(--vp-c-text-1);
  font-size: 13px;
  line-height: 1.6;
  /* Use drop-shadow so it wraps the arrow shape as well. */
  filter: drop-shadow(0 4px 12px rgba(0, 0, 0, 0.15)) drop-shadow(0 1px 3px rgba(0, 0, 0, 0.1));
  will-change: transform, opacity;
}

.dark .vp-tt-content {
  filter: drop-shadow(0 4px 12px rgba(0, 0, 0, 0.45)) drop-shadow(0 1px 3px rgba(0, 0, 0, 0.3));
}

.vp-tt-arrow {
  /* Arrow fills with the same background so it blends seamlessly. */
  fill: var(--vp-c-bg);
}

.vp-tt-content[data-state='delayed-open'][data-side='top']    { animation: vp-tt-in-top    120ms ease-out; }
.vp-tt-content[data-state='delayed-open'][data-side='bottom'] { animation: vp-tt-in-bottom 120ms ease-out; }
.vp-tt-content[data-state='delayed-open'][data-side='left']   { animation: vp-tt-in-left   120ms ease-out; }
.vp-tt-content[data-state='delayed-open'][data-side='right']  { animation: vp-tt-in-right  120ms ease-out; }

@keyframes vp-tt-in-top    { from { opacity:0; transform:translateY( 4px) } to { opacity:1; transform:translateY(0) } }
@keyframes vp-tt-in-bottom { from { opacity:0; transform:translateY(-4px) } to { opacity:1; transform:translateY(0) } }
@keyframes vp-tt-in-left   { from { opacity:0; transform:translateX( 4px) } to { opacity:1; transform:translateX(0) } }
@keyframes vp-tt-in-right  { from { opacity:0; transform:translateX(-4px) } to { opacity:1; transform:translateX(0) } }
</style>
