import { api } from '../services/http'

export interface ReportContract {
  endpoint: string
  title: string
}

export const salesReport: ReportContract = { endpoint: 'reports/sales', title: 'Sales' }
export const stockReport: ReportContract = { endpoint: 'reports/stock', title: 'Stock' }

export const reportsApi = {
  run(contract: ReportContract, params: object) {
    return api.get(contract.endpoint, { params })
  },
  detail(contract: ReportContract, id: number) {
    return api.get(`${contract.endpoint}/${id}`)
  },
  open(source: { path: string }) {
    return api.get(source.path)
  }
}
