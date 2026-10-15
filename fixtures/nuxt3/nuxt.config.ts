export default defineNuxtConfig({
  modules: ['@pinia/nuxt', '@nuxt/ui'],
  runtimeConfig: { public: { apiBase: process.env.NUXT_PUBLIC_API_BASE || '/api' } },
})
