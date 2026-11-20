import { createI18n } from 'vue-i18n'
import existingZhCN from '@renderer/i18n/locales/zh-CN'
import existingEnUS from '@renderer/i18n/locales/en-US'

export type Locale = 'zh-CN' | 'en-US'

function savedLocale(): Locale {
  try {
    return localStorage.getItem('locale') === 'en-US' ? 'en-US' : 'zh-CN'
  } catch {
    return 'zh-CN'
  }
}

export function createAppI18n(locale: Locale = savedLocale()): ReturnType<typeof createI18n> {
  return createI18n({
    legacy: false,
    locale,
    fallbackLocale: 'en-US',
    messages: {
      'zh-CN': {
        menu: existingZhCN.menu,
        aiProvider: { roles: existingZhCN.aiProvider.roles },
        profile: {
          appearance: {
            languageZhCN: existingZhCN.profile.appearance.languageZhCN,
            languageEnUS: existingZhCN.profile.appearance.languageEnUS
          }
        },
        tauriPreview: existingZhCN.tauriPreview
      },
      'en-US': {
        menu: existingEnUS.menu,
        aiProvider: { roles: existingEnUS.aiProvider.roles },
        profile: {
          appearance: {
            languageZhCN: existingEnUS.profile.appearance.languageZhCN,
            languageEnUS: existingEnUS.profile.appearance.languageEnUS
          }
        },
        tauriPreview: existingEnUS.tauriPreview
      }
    }
  })
}
