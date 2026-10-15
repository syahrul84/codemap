<template>
  <button :class="kind" @click="$emit('click', $event)"><slot /></button>
</template>

<script>
export default {
  props: ['kind'],
}
</script>
