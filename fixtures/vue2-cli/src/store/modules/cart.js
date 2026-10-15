const state = () => ({ items: [], total: 0 })

const getters = {
  count: (s) => s.items.length,
}

export default {
  namespaced: true,
  state,
  getters,
  mutations: {
    SET_ITEMS(s, items) { s.items = items },
  },
  actions: {
    async load({ commit }) { commit('SET_ITEMS', []) },
  },
}
