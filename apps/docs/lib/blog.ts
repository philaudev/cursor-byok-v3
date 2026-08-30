import { loader } from 'fumadocs-core/source';
import { pageSchema } from 'fumadocs-core/source/schema';
import { defineCollections } from 'fumadocs-mdx/macro';
import { htmlLang, i18n, type Language } from './i18n';

const blog = defineCollections({
  type: 'doc',
  dir: 'content/blog',
  schema: pageSchema,
});

export const blogSource = loader({
  baseUrl: '/blog',
  source: blog.toFumadocsSource(),
  i18n,
});

export function getBlogDate(path: string): Date {
  const fileName = path.split('/').at(-1) ?? '';
  const match = /^(\d{4}-\d{2}-\d{2})/.exec(fileName);
  return match ? new Date(`${match[1]}T00:00:00Z`) : new Date(0);
}

export function formatBlogDate(path: string, lang: Language = i18n.defaultLanguage): string {
  return new Intl.DateTimeFormat(htmlLang[lang], {
    year: 'numeric',
    month: 'long',
    day: 'numeric',
    timeZone: 'UTC',
  }).format(getBlogDate(path));
}

export function sortBlogPages<T extends { path: string }>(pages: T[]): T[] {
  return [...pages].sort((left, right) => {
    return getBlogDate(right.path).getTime() - getBlogDate(left.path).getTime();
  });
}
