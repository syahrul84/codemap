export const state = () => ({ orders: [] })
export const mutations = { SET(s, o) { s.orders = o } }
export const actions = {
  async fetch({ commit }) {
    commit('SET', await this.$axios.$get('/orders'))
  },
}
