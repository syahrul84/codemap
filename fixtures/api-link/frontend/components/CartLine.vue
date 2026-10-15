<script setup lang="ts">
const props = defineProps<{ merchantId: number }>()
const item = ref<any>(null)
async function add() {
  const res = await useApi()(`/carts/${props.merchantId}/items`, { method: 'POST', body: {} })
  item.value = res.data
}
</script>

<template>
  <input type="number" :value="item.quantity" :max="item.max_quantity ?? 1" @change="add" />
</template>
