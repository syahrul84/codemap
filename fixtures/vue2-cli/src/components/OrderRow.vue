<template>
  <tr><td>{{ order.number }}</td><td>{{ value }}</td></tr>
</template>

<script>
export default {
  props: { order: Object, value: Number },
  methods: {
    change(v) { this.$emit('input', v) },
  },
}
</script>
