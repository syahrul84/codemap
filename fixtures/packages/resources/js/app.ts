import { createApp } from 'vue'
import axios from 'axios'
import type { AxiosInstance } from 'axios'

const http: AxiosInstance = axios.create({ baseURL: '/api' })

createApp({ data: () => ({ http }) }).mount('#app')
