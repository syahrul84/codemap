<script setup lang="ts">
definePageMeta({ layout: 'default', middleware: ['auth'] })
const route = useRoute()
const { store } = useCart()
</script>

<template>
  <OrderSummary :order-id="Number(route.params.id)" />
  <NuxtPage />
</template>
