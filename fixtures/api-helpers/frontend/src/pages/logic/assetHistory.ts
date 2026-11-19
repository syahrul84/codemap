import { assetsApi } from '../../modules/assets'

export function useAssetHistory(id: unknown) {
  const load = () => assetsApi.familyList('history', id)
  const categories = () => assetsApi.categories()
  return { load, categories }
}
