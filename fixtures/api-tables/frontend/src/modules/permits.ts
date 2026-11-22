import { api } from '../services/http'

const movingContract = { endpoint: 'permits/moving', title: 'Moving' }
const deliveryContract = { endpoint: 'permits/delivery', title: 'Delivery' }
const renovationContract = { endpoint: 'permits/renovation', title: 'Renovation' }

const permitContracts = {
  moving: movingContract,
  delivery: deliveryContract,
  renovation: renovationContract
}

const setupContracts = {
  approver: { endpoint: 'setup/approvers' }
}

function extensionEndpoint(permitId: number, id?: number) {
  const base = renovationContract.endpoint
  return `${base}/${permitId}/extension${id ? `/${id}` : ''}`
}

export const visitorEndpoints = {
  list: 'visitors',
  detail: (id: number | string) => `visitors/${id}`
}

export const permitsApi = {
  list(kind: string) {
    return api.get(permitContracts[kind].endpoint)
  },
  get(kind: string, id: number) {
    return api.get(`${permitContracts[kind].endpoint}/${id}`)
  },
  remove(kind: string, id: number) {
    const contract = permitContracts[kind]
    return api.delete(`${contract.endpoint}/${id}`)
  },
  save(kind: string, endpoint: string, body: object) {
    return api.post(endpoint || permitContracts[kind].endpoint, body)
  },
  approvers() {
    return api.get(setupContracts.approver.endpoint)
  },
  extension(permitId: number, id?: number) {
    return api.get(extensionEndpoint(permitId, id))
  },
  visitor(id: number) {
    return api.get(visitorEndpoints.detail(id))
  },
  saveVisitor(id: number | 'new', body: object) {
    const endpoint = id === 'new' ? 'visitors' : `visitors/${id}`
    return api.post(endpoint, { ...body, ...(id === 'new' ? {} : { _method: 'PATCH' }) })
  }
}
