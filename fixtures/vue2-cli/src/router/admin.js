import AdminLayout from '../views/admin/AdminLayout.vue'

export default {
  path: '/admin',
  component: AdminLayout,
  children: [
    { path: '', name: 'admin.home', component: () => import('../views/admin/Dashboard.vue') },
    { path: 'users/:id', name: 'admin.user', component: () => import('../views/admin/Missing.vue') },
  ],
}
