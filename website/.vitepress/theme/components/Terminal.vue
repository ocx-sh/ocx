<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, useSlots } from 'vue'
import 'asciinema-player/dist/bundle/asciinema-player.css'

const props = withDefaults(defineProps<{
  /** Path to a .cast file (alternative to inline <Frame> children). */
  src?: string
  /** Title shown in the terminal chrome title bar. */
  title?: string
  /** Terminal width in columns. */
  cols?: number
  /** Terminal height in rows. Auto-calculated from frame count if omitted. */
  rows?: number
  /** Start playback automatically. Defaults to true for inline frames, false for src. */
  autoPlay?: boolean
  /** Playback speed multiplier. */
  speed?: number
  /** Compress pauses longer than this many seconds. */
  idleTimeLimit?: number
  /** Loop playback. */
  loop?: boolean
  /** How the player scales: 'width', 'height', 'both', or 'none'. */
  fit?: 'width' | 'height' | 'both' | 'none'
}>(), {
  speed: 1,
  idleTimeLimit: 2,
  loop: false,
  fit: 'width',
})

// ── VNode introspection ────────────────────────────────────────────────── //

const slots = useSlots()

interface FrameData {
  at: number
  text: string
}

/** Extract plain text from a VNode's default slot (matches descText pattern from Steps.vue). */
function vnodeText(vnode: any): string {
  if (typeof vnode.children === 'string') return vnode.children
  const slot = vnode?.children?.default
  if (typeof slot === 'function') {
    return slot()
      .map((v: any) => (typeof v.children === 'string' ? v.children : ''))
      .join('')
  }
  return ''
}

/** Parse <Frame> VNodes into FrameData array. */
const frames = computed<FrameData[]>(() => {
  const vnodes = slots.default?.() ?? []
  return vnodes
    .filter((v: any) => v?.props && 'at' in v.props)
    .map((v: any) => ({
      at: Number(v.props.at),
      text: vnodeText(v).trim(),
    }))
    .filter(f => f.text.length > 0)
})

// ── Cast generation ────────────────────────────────────────────────────── //

const computedRows = computed(() => {
  if (props.rows != null) return props.rows
  if (frames.value.length > 0) return Math.min(frames.value.length + 1, 50)
  return undefined // src mode: let player use cast file header
})

/** Build asciicast v2 string from frames. */
const castData = computed(() => {
  if (props.src || frames.value.length === 0) return null

  const header = JSON.stringify({
    version: 2,
    width: props.cols ?? 80,
    height: computedRows.value,
  })

  const events = frames.value.map(f =>
    JSON.stringify([f.at, 'o', f.text + '\r\n'])
  )

  return header + '\n' + events.join('\n')
})

// ── Player lifecycle (client-side only) ────────────────────────────────── //

const containerEl = ref<HTMLElement | null>(null)
let player: any = null

onMounted(async () => {
  if (!containerEl.value) return

  const AsciinemaPlayer = await import('asciinema-player')

  const src = props.src ?? { data: castData.value }
  const autoPlay = props.autoPlay ?? !props.src

  player = AsciinemaPlayer.create(src, containerEl.value, {
    ...(props.cols != null && { cols: props.cols }),
    ...(computedRows.value != null && { rows: computedRows.value }),
    autoPlay,
    speed: props.speed,
    idleTimeLimit: props.idleTimeLimit,
    loop: props.loop,
    fit: props.fit === 'none' ? false : props.fit,
    theme: 'vitepress',
    terminalFontFamily: 'var(--vp-font-family-mono)',
  })
})

onBeforeUnmount(() => {
  player?.dispose()
  player = null
})
</script>

<template>
  <div class="vp-terminal">
    <div class="vp-terminal-chrome">
      <span class="vp-terminal-dot vp-terminal-dot--red" />
      <span class="vp-terminal-dot vp-terminal-dot--yellow" />
      <span class="vp-terminal-dot vp-terminal-dot--green" />
      <span v-if="title" class="vp-terminal-title">{{ title }}</span>
    </div>
    <div ref="containerEl" class="vp-terminal-player" />
  </div>
</template>

<style scoped>
.vp-terminal {
  border: 1px solid var(--vp-c-border);
  border-radius: 8px;
  overflow: hidden;
  margin: 16px 0;
}

.vp-terminal-chrome {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 10px 14px;
  background: var(--vp-c-bg-soft);
  border-bottom: 1px solid var(--vp-c-border);
}

.vp-terminal-dot {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  flex-shrink: 0;
}

.vp-terminal-dot--red { background: var(--vp-c-red-1); }
.vp-terminal-dot--yellow { background: var(--vp-c-yellow-1); }
.vp-terminal-dot--green { background: var(--vp-c-green-1); }

.vp-terminal-title {
  flex: 1;
  text-align: center;
  font-size: 12px;
  color: var(--vp-c-text-3);
  font-family: var(--vp-font-family-mono);
  margin-right: 48px;
}

.vp-terminal-player :deep(.ap-wrapper) {
  border-radius: 0;
  margin: 0;
}
</style>

<style>
/* Custom asciinema theme using VitePress CSS variables.
   Maps ANSI 0-15 to VitePress semantic color tokens.
   Auto dark-mode via VitePress var switching. */
.asciinema-player-theme-vitepress {
  --term-color-foreground: var(--vp-c-text-1);
  --term-color-background: var(--vp-code-block-bg);

  /* Standard ANSI colors (0-7) */
  --term-color-0: var(--vp-c-gray-1);
  --term-color-1: var(--vp-c-red-1);
  --term-color-2: var(--vp-c-green-1);
  --term-color-3: var(--vp-c-yellow-1);
  --term-color-4: var(--vp-c-indigo-1);
  --term-color-5: var(--vp-c-purple-1);
  --term-color-6: var(--vp-c-green-2);
  --term-color-7: var(--vp-c-text-2);

  /* Bright ANSI colors (8-15) */
  --term-color-8: var(--vp-c-gray-2);
  --term-color-9: var(--vp-c-red-2);
  --term-color-10: var(--vp-c-green-2);
  --term-color-11: var(--vp-c-yellow-2);
  --term-color-12: var(--vp-c-indigo-2);
  --term-color-13: var(--vp-c-purple-2);
  --term-color-14: var(--vp-c-green-3);
  --term-color-15: var(--vp-c-text-1);
}
</style>
