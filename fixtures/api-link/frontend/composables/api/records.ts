// Generic record helpers: the URL is built from the arguments.
export const recordsApi = {
  async get(endpoint: string, id: string | number, suffix = '') {
    return (await useApi()(`${endpoint}/${encodeURIComponent(String(id))}${suffix}`)).data
  },
}
