import { defineI18n } from 'fumadocs-core/i18n';

export const i18n = defineI18n({
  defaultLanguage: 'zh',
  languages: ['zh', 'en'],
  // 所有语言显式带前缀;未带前缀的访问由中间件按浏览器 Accept-Language 协商后重定向
  hideLocale: 'never',
});

export type Language = (typeof i18n)['languages'][number];

export function isLanguage(value: string): value is Language {
  return i18n.languages.some((lang) => lang === value);
}

/** HTML `lang` attribute values. */
export const htmlLang: Record<Language, string> = {
  zh: 'zh-CN',
  en: 'en',
};

/** Locale ids understood by the desktop app / demo. */
export const demoLocale: Record<Language, string> = {
  zh: 'zh-CN',
  en: 'en-US',
};
