import Link from 'next/link';
import type { Metadata } from 'next';
import { notFound } from 'next/navigation';
import { ArrowRight } from 'lucide-react';
import { blogSource, formatBlogDate, sortBlogPages } from '@/lib/blog';
import { isLanguage, type Language } from '@/lib/i18n';

const copy: Record<Language, { title: string; description: string; intro: string; empty: string }> = {
  zh: {
    title: '开发者博客',
    description: 'Cursor Byok 的架构决策、协议实现与开发进展。',
    intro: '记录 Cursor Byok 的架构决策、协议实现和开发进展。',
    empty: '文章正在准备中。',
  },
  en: {
    title: 'Developer Blog',
    description: 'Architecture decisions, protocol work, and development progress of Cursor Byok.',
    intro: 'Notes on architecture decisions, protocol work, and development progress of Cursor Byok.',
    empty: 'Articles are being prepared.',
  },
};

export async function generateMetadata(props: PageProps<'/[lang]/blog'>): Promise<Metadata> {
  const { lang } = await props.params;
  if (!isLanguage(lang)) notFound();

  return {
    title: copy[lang].title,
    description: copy[lang].description,
  };
}

export default async function BlogPage(props: PageProps<'/[lang]/blog'>) {
  const { lang } = await props.params;
  if (!isLanguage(lang)) notFound();

  const t = copy[lang];
  const posts = sortBlogPages(blogSource.getPages(lang));

  return (
    <main className="mx-auto w-full max-w-5xl px-6 py-16 sm:py-24">
      <div className="max-w-2xl">
        <p className="font-mono text-sm font-medium text-fd-primary">DEVELOPER BLOG</p>
        <h1 className="mt-4 text-4xl font-bold tracking-tight">{t.title}</h1>
        <p className="mt-4 text-lg leading-8 text-fd-muted-foreground">{t.intro}</p>
      </div>

      {posts.length > 0 ? (
        <div className="mt-12 divide-y border-y">
          {posts.map((post) => (
            <Link
              key={post.url}
              href={post.url}
              className="group grid gap-3 py-7 transition-colors hover:text-fd-primary sm:grid-cols-[10rem_1fr_auto] sm:items-center"
            >
              <time className="text-sm text-fd-muted-foreground">
                {formatBlogDate(post.path, lang)}
              </time>
              <div>
                <h2 className="font-semibold">{post.data.title}</h2>
                <p className="mt-1 text-sm leading-6 text-fd-muted-foreground">
                  {post.data.description}
                </p>
              </div>
              <ArrowRight className="hidden size-4 transition-transform group-hover:translate-x-1 sm:block" />
            </Link>
          ))}
        </div>
      ) : (
        <p className="mt-12 border-y py-8 text-fd-muted-foreground">{t.empty}</p>
      )}
    </main>
  );
}
