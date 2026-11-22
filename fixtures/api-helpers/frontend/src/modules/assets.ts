import { api } from '../services/http'

const base = 'assets'
const contracts = { lookups: { categories: 'lookups/categories' } } as const

function familyPath(family: string, id: unknown) {
  return [base, id, family].filter(Boolean).join('/')
}

function assetForm(record: Record<string, unknown>, method?: 'PATCH') {
  const form = new FormData()
  Object.entries(record).forEach(([key, value]) => form.append(key, String(value)))
  if (method) form.append('_method', method)
  return form
}

export const assetsApi = {
  list(query: Record<string, unknown>) {
    return api.get(base, { params: query })
  },
  save(id: unknown, record: Record<string, unknown>) {
    return api.post(id === 'new' ? base : `${base}/${id}`, {
      ...record,
      ...(id === 'new' ? {} : { _method: 'PATCH' })
    })
  },
  saveForm(id: unknown, record: Record<string, unknown>) {
    return api.post(id === 'new' ? base : `${base}/${id}`, assetForm(record, id === 'new' ? undefined : 'PATCH'))
  },
  categories: () => api.get(contracts.lookups.categories),
  familyList(family: string, id: unknown) {
    return api.get(familyPath(family, id))
  }
}

export class AssetLabels {
  title(id: unknown) {
    return `Asset ${id}`
  }
}
