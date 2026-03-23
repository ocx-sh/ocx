<script setup lang="ts">
import { computed, reactive } from 'vue'
import {
  CollapsibleRoot,
  CollapsibleTrigger,
  CollapsibleContent,
  PopoverRoot,
  PopoverTrigger,
  PopoverPortal,
  PopoverContent,
} from 'reka-ui'
import TagBadge from './TagBadge.vue'
import { buildVersionTable } from '../utils/version'
import type { VariantRow } from '../utils/version'

const props = defineProps<{
  tags: string[]
  qualifiedName: string
}>()

const table = computed(() => buildVersionTable(props.tags))


// Track open state of minor popovers so we can close on copy
const openPopovers = reactive(new Map<string, boolean>())

function isPopoverOpen(key: string): boolean {
  return openPopovers.get(key) ?? false
}


// Track popovers that are closing (for exit animation)
const closingPopovers = reactive(new Set<string>())

function closePopover(key: string) {
  closingPopovers.add(key)
  setTimeout(() => {
    openPopovers.set(key, false)
  }, 200)
}

// Clean up closing state when popover actually closes
function handlePopoverUpdate(key: string, open: boolean) {
  openPopovers.set(key, open)
  if (open) closingPopovers.delete(key)
}

function isClosing(key: string): boolean {
  return closingPopovers.has(key)
}

function remainingCount(row: VariantRow): number {
  let count = 0
  for (const mg of row.majorGroups) {
    if (mg.majorTag) count++
    for (const minor of mg.minorGroups) {
      count += 1 + minor.children.length
    }
  }
  return count
}

function hasRemaining(row: VariantRow): boolean {
  return row.majorGroups.length > 0
}
</script>

<template>
  <div class="version-table">
    <CollapsibleRoot
      v-for="row in table.rows"
      :key="row.label"
      class="variant-row"
    >
      <div class="variant-row-header">
        <span class="variant-label" :class="{ default: row.isDefault }">
          <template v-if="row.isDefault">(default)</template>
          <template v-else>{{ row.label }}</template>
        </span>
        <div class="key-tags">
          <TagBadge
            v-for="tag in row.keyTags"
            :key="tag"
            :tag="tag"
            :qualified-name="qualifiedName"
            variant="rolling"
          />
        </div>
        <CollapsibleTrigger v-if="hasRemaining(row)" class="expand-toggle">
          <span class="expand-count">+{{ remainingCount(row) }}</span>
          <svg class="chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9" /></svg>
        </CollapsibleTrigger>
      </div>

      <CollapsibleContent v-if="hasRemaining(row)" class="variant-row-detail">
        <div class="major-groups">
          <div
            v-for="mg in row.majorGroups"
            :key="mg.major"
            class="major-group"
          >
            <!-- Major version tag as row header -->
            <div class="major-header">
              <TagBadge
                v-if="mg.majorTag"
                :tag="mg.majorTag"
                :qualified-name="qualifiedName"
                variant="rolling"
              />
              <span v-else class="major-number">{{ mg.major }}</span>
            </div>
            <!-- Minor groups for this major -->
            <div v-if="mg.minorGroups.length" class="minor-groups">
              <div
                v-for="minor in mg.minorGroups"
                :key="minor.minorTag"
                class="minor-group"
              >
                <TagBadge
                  :tag="minor.minorTag"
                  :qualified-name="qualifiedName"
                  variant="minor"
                />
                <PopoverRoot
                  v-if="minor.children.length"
                  :open="isPopoverOpen(minor.minorTag)"
                  @update:open="handlePopoverUpdate(minor.minorTag, $event)"
                >
                  <PopoverTrigger class="expand-toggle minor-toggle">
                    <svg class="chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9" /></svg>
                    <span class="expand-count">{{ minor.children.length }}</span>
                  </PopoverTrigger>
                  <PopoverPortal>
                    <PopoverContent class="minor-popover" :class="{ closing: isClosing(minor.minorTag) }" side="bottom" align="start" :side-offset="4">
                      <div class="minor-children">
                        <TagBadge
                          v-for="child in minor.children"
                          :key="child"
                          :tag="child"
                          :qualified-name="qualifiedName"
                          variant="child"
                          @copied="closePopover(minor.minorTag)"
                        />
                      </div>
                    </PopoverContent>
                  </PopoverPortal>
                </PopoverRoot>
              </div>
            </div>
          </div>
        </div>
      </CollapsibleContent>
    </CollapsibleRoot>

    <!-- Unknown tags row -->
    <CollapsibleRoot
      v-if="table.unknownTags.length"
      class="variant-row unknown-row"
    >
      <div class="variant-row-header">
        <span class="variant-label other">other</span>
        <div class="key-tags">
          <TagBadge
            v-for="tag in table.unknownTags.slice(0, 5)"
            :key="tag"
            :tag="tag"
            :qualified-name="qualifiedName"
          />
        </div>
        <CollapsibleTrigger
          v-if="table.unknownTags.length > 5"
          class="expand-toggle"
        >
          <span class="expand-count">+{{ table.unknownTags.length - 5 }}</span>
          <svg class="chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9" /></svg>
        </CollapsibleTrigger>
      </div>
      <CollapsibleContent v-if="table.unknownTags.length > 5" class="variant-row-detail">
        <div class="detail-section">
          <div class="tag-row">
            <TagBadge
              v-for="tag in table.unknownTags.slice(5)"
              :key="tag"
              :tag="tag"
              :qualified-name="qualifiedName"
            />
          </div>
        </div>
      </CollapsibleContent>
    </CollapsibleRoot>
  </div>
</template>

<style scoped>
/* Tag rows */
.tag-row {
  display: flex;
  flex-wrap: wrap;
  gap: 0.4rem;
  align-items: center;
}

/* Table layout */
.version-table {
  display: flex;
  flex-direction: column;
}

/* Variant row */
.variant-row {
  border-bottom: 1px solid var(--vp-c-divider);
}

.variant-row:last-child {
  border-bottom: none;
}

.variant-row-header {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  padding: 0.5rem 0;
  min-height: 2.25rem;
}

.variant-label {
  font-family: var(--vp-font-family-mono);
  font-size: 0.75rem;
  font-weight: 600;
  color: var(--vp-c-text-1);
  min-width: 5rem;
  flex-shrink: 0;
}

.variant-label.default {
  color: var(--vp-c-text-3);
  font-style: italic;
  font-weight: 400;
}

.variant-label.other {
  color: var(--vp-c-text-3);
  font-style: italic;
  font-weight: 400;
}

.key-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 0.35rem;
  align-items: center;
  flex: 1;
  min-width: 0;
}

/* Expand toggle (variant row + minor group) */
.expand-toggle {
  display: inline-flex;
  align-items: center;
  gap: 0.15rem;
  padding: 0.2rem 0.4rem;
  border: none;
  border-radius: 4px;
  background: transparent;
  color: var(--vp-c-text-3);
  cursor: pointer;
  font-size: 0.7rem;
  font-family: var(--vp-font-family-mono);
  flex-shrink: 0;
  transition: all 0.15s;
}

.expand-toggle:hover {
  background: var(--vp-c-bg-soft);
  color: var(--vp-c-text-2);
}

.chevron {
  transition: transform 0.2s ease;
}

.expand-toggle[data-state='open'] .chevron {
  transform: rotate(180deg);
}

.expand-count {
  opacity: 0.8;
}

/* Expanded detail */
.variant-row-detail {
  overflow: hidden;
}

.variant-row-detail[data-state='open'] {
  animation: row-open 200ms ease-out;
}

.variant-row-detail[data-state='closed'] {
  animation: row-close 200ms ease-in;
}

@keyframes row-open {
  from { height: 0; opacity: 0; }
  to { height: var(--reka-collapsible-content-height); opacity: 1; }
}

@keyframes row-close {
  from { height: var(--reka-collapsible-content-height); opacity: 1; }
  to { height: 0; opacity: 0; }
}

.detail-section {
  padding: 0 0 0.5rem 5.75rem;
}

/* Major groups */
.major-groups {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  padding: 0.25rem 0 0.5rem 5.75rem;
}

.major-group {
  display: flex;
  align-items: flex-start;
  gap: 0.5rem;
}

.major-header {
  flex-shrink: 0;
}

.major-number {
  font-family: var(--vp-font-family-mono);
  font-size: 0.75rem;
  font-weight: 600;
  color: var(--vp-c-text-2);
  padding: 0.2rem 0.4rem;
}

/* Minor version groups */
.minor-groups {
  display: flex;
  flex-wrap: wrap;
  gap: 0.4rem;
  align-items: flex-start;
}

.minor-group {
  display: flex;
  align-items: center;
  gap: 0.25rem;
}

.minor-toggle {
  padding: 0.1rem 0.25rem;
}

/* Minor children popover */
.minor-children {
  display: flex;
  flex-wrap: wrap;
  gap: 0.35rem;
}

/* Responsive */
@media (max-width: 640px) {
  .variant-label {
    min-width: 3.5rem;
    font-size: 0.7rem;
  }

  .variant-row-header {
    gap: 0.5rem;
  }

  .major-groups,
  .detail-section {
    padding-left: 4.25rem;
  }
}
</style>

<style>
/* Popover — unscoped because it renders in a portal */
.minor-popover {
  max-width: 360px;
  padding: 0.5rem;
  background: var(--vp-c-bg);
  border: 1px solid var(--vp-c-brand-soft);
  border-radius: 8px;
  z-index: 100;
  animation: popover-in 150ms ease-out;
}

.minor-popover.closing {
  animation: popover-out 200ms ease-in forwards;
}

@keyframes popover-in {
  from { opacity: 0; transform: translateY(-4px); }
  to { opacity: 1; transform: translateY(0); }
}

@keyframes popover-out {
  from { opacity: 1; transform: translateY(0); }
  to { opacity: 0; transform: translateY(-4px); }
}
</style>
