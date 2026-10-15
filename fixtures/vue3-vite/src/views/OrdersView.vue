<script setup lang="ts">
import { createReusableTemplate } from '@vueuse/core'
import OrderCard from '@/components/OrderCard.vue'
import { useApi } from '@/composables/useApi'
import { useCartStore } from '@/stores/cart'

const { DefineRow, ReuseRow } = createReusableTemplate()
const cart = useCartStore()
const { loading } = useApi('/api/orders')
const { t } = useI18n()
const heading = t('message.orders.title')
const label = (s: string) => t(`message.orders.status.${s}`)
const oops = t('message.orders.refunded')
</script>

<template>
  <DefineRow v-slot="{ order }">
    <OrderCard :order="order" v-model:quantity="order.qty" @select="cart.add" />
  </DefineRow>
  <section>
    <i18n-t keypath="message.orders.empty" />
    <p>{{ $t('message.cart.add') }}</p>
    <ReuseRow v-for="o in cart.items" :key="o" :order="o" />
    <OrdersView v-if="false" />
    <UnknownThing />
  </section>
</template>
