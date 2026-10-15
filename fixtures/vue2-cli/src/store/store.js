import Vue from 'vue'
import Vuex from 'vuex'
import cart from './modules/cart'
import auth from './modules/auth'

Vue.use(Vuex)

export const store = new Vuex.Store({
  modules: { cart, auth },
})
