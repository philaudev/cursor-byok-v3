import { RootProvider } from 'fumadocs-ui/provider/next';
import { i18nProvider } from 'fumadocs-ui/i18n';
import { notFound } from 'next/navigation';
import type { Metadata } from 'next';
import { htmlLang, i18n, isLanguage } from '@/lib/i18n';
import { translations } from '@/lib/layout.shared';
import '../global.css';

const siteUrl = process.env.NEXT_PUBLIC_SITE_URL ?? 'https://docs.leokun.cn';

export async function generateMetadata(props: LayoutProps<'/[lang]'>): Promise<Metadata> {
  const { lang } = await props.params;

  if (lang === 'en') {
    return {
      metadataBase: new URL(siteUrl),
      title: {
        default: 'Cursor Byok Docs',
        template: '%s | Cursor Byok',
      },
      description: 'Installation, model configuration, and frequently asked questions for Cursor Byok.',
    };
  }

  return {
    metadataBase: new URL(siteUrl),
    title: {
      default: 'Cursor Byok 文档',
      template: '%s | Cursor Byok',
    },
    description: '安装、模型配置与常见问题指南。',
  };
}

export function generateStaticParams() {
  return i18n.languages.map((lang) => ({ lang }));
}

export default async function Layout({ params, children }: LayoutProps<'/[lang]'>) {
  const { lang } = await params;
  if (!isLanguage(lang)) notFound();

  return (
    <html lang={htmlLang[lang]} suppressHydrationWarning>
      <body className="flex min-h-screen flex-col">
        <RootProvider i18n={i18nProvider(translations, lang)}>{children}</RootProvider>
      </body>
    </html>
  );
}
