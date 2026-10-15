import { mount } from '@vue/test-utils'
import OrderCard from '@/components/OrderCard.vue'

it('renders', () => {
  expect(mount(OrderCard, { props: { order: { id: 1 } } }).html()).toContain('article')
})
