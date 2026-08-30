import { HomeLayout } from 'fumadocs-ui/layouts/home';
import { baseOptions } from '@/lib/layout.shared';
import { isLanguage, i18n } from '@/lib/i18n';

export default async function Layout({ params, children }: LayoutProps<'/[lang]'>) {
  const { lang } = await params;
  const base = baseOptions(isLanguage(lang) ? lang : i18n.defaultLanguage);

  return (
    <HomeLayout
      {...base}
      nav={{ ...base.nav, transparentMode: 'always' }}
      className="home-fullbleed-nav"
    >
      {children}
    </HomeLayout>
  );
}
