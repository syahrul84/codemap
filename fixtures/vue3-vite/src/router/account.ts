import type { RouteRecordRaw } from 'vue-router'

export const accountRoutes: RouteRecordRaw[] = [
  {
    path: '/account',
    component: () => import('@/views/OrdersView.vue'),
    children: [{ path: 'orders/:id', name: 'account.order', component: () => import('@/components/OrderCard.vue') }],
  },
]
