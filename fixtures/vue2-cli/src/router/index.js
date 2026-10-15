import Vue from 'vue'
import Router from 'vue-router'
import adminRoutes from './admin'

const Orders = () => import('Components/../views/Orders')

Vue.use(Router)

export default new Router({
  mode: 'history',
  routes: [
    { path: '/', redirect: { name: 'orders' } },
    { path: '/orders', name: 'orders', component: Orders, meta: { title: 'Orders' } },
    adminRoutes,
    helperRoute('/reports'),
  ],
})
