<script setup lang="ts">
import type { Order } from '@/types'

interface Props {
  /** The order to show */
  order: Order
  compact?: boolean
  size?: 'sm' | 'lg'
}

const props = withDefaults(defineProps<Props>(), { compact: false, size: 'sm' })
const emit = defineEmits<{
  (e: 'select', id: number): void
  (e: 'remove'): void
}>()
const quantity = defineModel<number>('quantity', { default: 1 })
defineSlots<{ default(): any; actions(props: { order: Order }): any }>()
</script>

<template>
  <article :class="size" @click="emit('select', props.order.id)">
    <slot />
    <slot name="actions" :order="order" />
    <input v-model.number="quantity" />
  </article>
</template>
