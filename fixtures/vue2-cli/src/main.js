import Vue from 'vue'
import Vuetify from 'vuetify'
import VueSignaturePad from 'vue-signature-pad'
import AppCard from 'Components/AppCard'
import { store } from 'Store/store'

Vue.use(Vuetify)
Vue.use(VueSignaturePad)
Vue.component('appCard', AppCard)

// Base components are registered globally by file name.
const base = require.context('./components/base', false, /Base[A-Z]\w+\.vue$/)
base.keys().forEach((file) => {
  const name = file.replace(/^\.\/(.*)\.\w+$/, '$1')
  Vue.component(name, base(file).default)
})

new Vue({ store, render: (h) => h('div') }).$mount('#app')
