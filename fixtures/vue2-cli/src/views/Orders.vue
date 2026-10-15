<template>
  <app-card heading="Orders" :closeable="false">
    <v-btn @click="reload">Reload</v-btn>
    <BaseButton kind="primary" />
    <order-row v-for="o in items" :key="o.id" :order="o" />
    <x-help />
    <VueSignaturePad ref="pad" />
    <missing-widget />
    <ghost-panel />
  </app-card>
</template>

<script>
import { mapState, mapGetters } from 'vuex'
import OrderRow from 'Components/OrderRow'
import MissingWidget from './MissingWidget'
import formMixin from '../mixins/form'

export default {
  name: 'OrdersPage',
  mixins: [formMixin],
  components: {
    OrderRow,
    MissingWidget,
    'x-help': { props: ['topic'], render: (h) => h('span') },
  },
  computed: {
    ...mapState('cart', ['items']),
    ...mapGetters('cart', ['count']),
  },
  methods: {
    reload() {
      this.$store.dispatch('cart/load')
    },
  },
}
</script>
