<template>
  <div class="card">
    <h3 v-if="heading">{{ heading }}</h3>
    <slot />
    <slot name="footer" />
    <base-button @click="$emit('close')">x</base-button>
  </div>
</template>

<script>
export default {
  name: 'AppCard',
  props: {
    heading: { type: String, default: '' },
    closeable: { type: Boolean, required: true },
  },
}
</script>
