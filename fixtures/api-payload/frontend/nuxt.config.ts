export default defineNuxtConfig({
  runtimeConfig: { public: { apiBase: process.env.NUXT_PUBLIC_API_BASE || 'http://localhost:8000/api/v1' } },
})
