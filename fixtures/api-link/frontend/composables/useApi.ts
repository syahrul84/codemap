export const useApi = () => $fetch.create({ baseURL: useRuntimeConfig().public.apiBase })
