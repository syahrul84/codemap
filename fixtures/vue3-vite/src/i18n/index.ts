import { createI18n } from 'vue-i18n'
import en from './locales/en'
import ms from './locales/ms'

export const i18n = createI18n({ locale: 'en', messages: { en: { message: en }, ms: { message: ms } } })
