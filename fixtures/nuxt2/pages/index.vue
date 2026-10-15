<template>
  <OrderList />
</template>

<script>
export default {
  layout: 'admin',
  middleware: 'auth',
  async fetch() {
    await this.$store.dispatch('orders/fetch')
  },
}
</script>
