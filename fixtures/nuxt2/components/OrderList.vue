<template>
  <ul><li v-for="o in orders" :key="o.id"><BaseCard>{{ o.number }}</BaseCard></li></ul>
</template>

<script>
import { mapState } from 'vuex'
export default {
  computed: { ...mapState('orders', ['orders']) },
}
</script>
