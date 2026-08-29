import { source } from '@/lib/source';
import { DocsLayout } from 'fumadocs-ui/layouts/docs';
import { baseOptions } from '@/lib/layout.shared';
import { i18n, isLanguage } from '@/lib/i18n';

export default async function Layout({ params, children }: LayoutProps<'/[lang]/docs'>) {
  const { lang } = await params;
  const language = isLanguage(lang) ? lang : i18n.defaultLanguage;

  return (
    <DocsLayout tree={source.getPageTree(language)} {...baseOptions(language)}>
      {children}
    </DocsLayout>
  );
}
