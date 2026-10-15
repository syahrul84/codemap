import { createRouter, createWebHistory } from 'vue-router'
import OrdersView from '@/views/OrdersView.vue'
import { accountRoutes } from './account'

const routes = [
  { path: '/', name: 'orders', component: OrdersView },
  ...accountRoutes,
]

export const router = createRouter({ history: createWebHistory(), routes })
