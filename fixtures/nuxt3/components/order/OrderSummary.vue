<script setup lang="ts">
const props = defineProps({ orderId: { type: Number, required: true } })
const { data } = await useFetch(`/api/orders/${props.orderId}`)
</script>

<template>
  <div>{{ data?.total }}</div>
</template>
