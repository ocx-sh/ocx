<script setup lang="ts">
/** Declarative marker for <Terminal>. Never rendered to DOM — Terminal reads its VNodes directly.
 *  Each <Frame> represents one line of terminal output at a given timestamp. */
defineProps<{
  /** Time in seconds when this line appears. Multiple frames at the same `at` appear simultaneously. */
  at: string | number
}>()
</script>

<template><slot /></template>
