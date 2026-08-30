import Link from 'next/link';
import type { Metadata } from 'next';
import { notFound } from 'next/navigation';
import { ArrowLeft } from 'lucide-react';
import { InlineTOC } from 'fumadocs-ui/components/inline-toc';
import { createRelativeLink } from 'fumadocs-ui/mdx';
import { getMDXComponents } from '@/components/mdx';
import { blogSource, formatBlogDate } from '@/lib/blog';
import { i18n, isLanguage, type Language } from '@/lib/i18n';

const copy: Record<Language, { back: string; team: string }> = {
  zh: { back: '返回开发者博客', team: 'Cursor Byok 开发团队' },
  en: { back: 'Back to Developer Blog', team: 'The Cursor Byok team' },
};

export default async function BlogPostPage(props: PageProps<'/[lang]/blog/[slug]'>) {
  const { lang, slug } = await props.params;
  if (!isLanguage(lang)) notFound();

  const page = blogSource.getPage([slug], lang);
  if (!page) notFound();

  const t = copy[lang];
  const prefix = `/${lang}`;
  const MDX = page.data.body;

  return (
    <main className="mx-auto w-full max-w-3xl px-6 py-12 sm:py-20">
      <article>
        <Link
          href={`${prefix}/blog`}
          className="mb-10 inline-flex items-center gap-2 text-sm text-fd-muted-foreground transition-colors hover:text-fd-foreground"
        >
          <ArrowLeft className="size-4" />
          {t.back}
        </Link>

        <header className="border-b pb-8">
          <time className="text-sm text-fd-muted-foreground">
            {formatBlogDate(page.path, lang)}
          </time>
          <h1 className="mt-4 text-3xl font-bold tracking-tight sm:text-4xl">{page.data.title}</h1>
          <p className="mt-4 text-lg leading-8 text-fd-muted-foreground">{page.data.description}</p>
          <p className="mt-5 text-sm font-medium">{t.team}</p>
        </header>

        <div className="prose mt-10 min-w-0">
          <InlineTOC items={page.data.toc} />
          <MDX
            components={getMDXComponents({
              a: createRelativeLink(blogSource, page),
            })}
          />
        </div>
      </article>
    </main>
  );
}

export function generateStaticParams() {
  return i18n.languages.flatMap((lang) =>
    blogSource.getPages(lang).map((page) => ({ lang, slug: page.slugs[0] })),
  );
}

export async function generateMetadata(
  props: PageProps<'/[lang]/blog/[slug]'>,
): Promise<Metadata> {
  const { lang, slug } = await props.params;
  if (!isLanguage(lang)) notFound();

  const page = blogSource.getPage([slug], lang);
  if (!page) notFound();

  return {
    title: page.data.title,
    description: page.data.description,
  };
}
