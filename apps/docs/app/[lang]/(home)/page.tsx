import Link from 'next/link';
import {
  ArrowRight,
  BookOpen,
  Settings2,
  Star,
  Wrench,
} from 'lucide-react';
import { notFound } from 'next/navigation';
import { DesktopDemo } from '@/components/hero/DesktopDemo';
import { DownloadButton } from '@/components/hero/DownloadButton';
import { blogSource, formatBlogDate, sortBlogPages } from '@/lib/blog';
import { formatStars, getRepoStats } from '@/lib/github';
import { isLanguage, type Language } from '@/lib/i18n';
import { releaseUrl, repositoryUrl } from '@/lib/shared';

const copy: Record<
  Language,
  {
    pillReleased: (version: string) => string;
    pillFallback: string;
    title: string;
    subtitle: string;
    readDocs: string;
    flow: [string, string, string];
    docsEyebrow: string;
    docsTitle: string;
    viewAll: string;
    docs: { icon: typeof BookOpen; title: string; description: string; href: string }[];
    blogEyebrow: string;
    blogTitle: string;
  }
> = {
  zh: {
    pillReleased: (version) => `${version} 已发布 · 开源 · 本地运行`,
    pillFallback: '开源 · 本地运行 · 自由接入',
    title: 'Cursor 服务端的开源替代',
    subtitle:
      '在本机运行自己的模型网关，用自己的 API Key 接入 OpenAI、Anthropic 兼容服务或自定义端点，完整保留 Cursor Agent 的工具调用、Skills 和 MCP。',
    readDocs: '阅读文档',
    flow: ['Cursor 客户端', 'Cursor Byok 本地服务', '你的模型 API（OpenAI / Anthropic 兼容）'],
    docsEyebrow: 'DOCUMENTATION',
    docsTitle: '文档',
    viewAll: '查看全部',
    docs: [
      {
        icon: BookOpen,
        title: '快速开始',
        description: '完成安装、初始化和第一次模型调用。',
        href: '/docs',
      },
      {
        icon: Settings2,
        title: '模型配置',
        description: '配置协议、服务地址、凭据和生成参数。',
        href: '/docs/model-configuration',
      },
      {
        icon: Wrench,
        title: '常见问题',
        description: '回答常见问题。',
        href: '/docs/faq',
      },
    ],
    blogEyebrow: 'DEVELOPER BLOG',
    blogTitle: '开发者博客',
  },
  en: {
    pillReleased: (version) => `${version} released · Open source · Runs locally`,
    pillFallback: 'Open source · Runs locally · Any provider',
    title: "The open-source alternative to Cursor's backend",
    subtitle:
      'Run your own model gateway locally, connect OpenAI- and Anthropic-compatible services or custom endpoints with your own API keys, and keep Cursor Agent tool calling, Skills, and MCP intact.',
    readDocs: 'Read the docs',
    flow: ['Cursor client', 'Cursor Byok local service', 'Your model API (OpenAI / Anthropic compatible)'],
    docsEyebrow: 'DOCUMENTATION',
    docsTitle: 'Documentation',
    viewAll: 'View all',
    docs: [
      {
        icon: BookOpen,
        title: 'Quick Start',
        description: 'Install, initialize, and make your first model call.',
        href: '/docs',
      },
      {
        icon: Settings2,
        title: 'Model Configuration',
        description: 'Configure protocols, server addresses, credentials, and parameters.',
        href: '/docs/model-configuration',
      },
      {
        icon: Wrench,
        title: 'Frequently Asked Questions',
        description: 'Answer common questions.',
        href: '/docs/faq',
      },
    ],
    blogEyebrow: 'DEVELOPER BLOG',
    blogTitle: 'Developer Blog',
  },
};

export default async function HomePage(props: PageProps<'/[lang]'>) {
  const { lang } = await props.params;
  if (!isLanguage(lang)) notFound();

  const t = copy[lang];
  const prefix = `/${lang}`;
  const posts = sortBlogPages(blogSource.getPages(lang)).slice(0, 3);
  const { stars, version } = await getRepoStats();

  return (
    <main className="flex flex-1 flex-col">
      <section className="relative -mt-14 overflow-hidden border-b px-4 pb-16 pt-34 sm:px-6 sm:pb-24 sm:pt-42">
        <div aria-hidden className="pointer-events-none absolute inset-0 -z-10">
          <div className="absolute inset-0 bg-[linear-gradient(to_right,var(--color-fd-border)_1px,transparent_1px),linear-gradient(to_bottom,var(--color-fd-border)_1px,transparent_1px)] bg-[size:56px_56px] opacity-40 [mask-image:radial-gradient(ellipse_70%_60%_at_50%_0%,#000_20%,transparent_75%)]" />
          <div className="absolute left-1/2 top-[-14rem] h-[26rem] w-[44rem] -translate-x-1/2 rounded-full bg-fd-primary/10 blur-3xl" />
        </div>
        <div className="mx-auto max-w-6xl">
          <div className="mx-auto max-w-3xl text-center">
            <a
              href={releaseUrl}
              className="inline-flex items-center gap-2 rounded-full border bg-fd-card px-4 py-1.5 text-sm text-fd-muted-foreground transition-colors hover:text-fd-foreground"
            >
              <span className="relative flex size-2">
                <span className="absolute inline-flex size-full animate-ping rounded-full bg-fd-primary opacity-50" />
                <span className="relative inline-flex size-2 rounded-full bg-fd-primary" />
              </span>
              {version ? t.pillReleased(version) : t.pillFallback}
              <ArrowRight className="size-3.5" />
            </a>
            <h1 className="mt-6 text-4xl font-bold tracking-tight sm:text-6xl">{t.title}</h1>
            <p className="mx-auto mt-6 max-w-2xl text-lg leading-8 text-fd-muted-foreground">
              {t.subtitle}
            </p>
            <div className="mt-10 flex flex-wrap items-center justify-center gap-3">
              <DownloadButton lang={lang} />
              <Link
                href={`${prefix}/docs`}
                className="inline-flex items-center gap-2 rounded-lg border bg-fd-card px-5 py-3 font-medium transition-colors hover:bg-fd-accent"
              >
                {t.readDocs}
                <ArrowRight className="size-4" />
              </Link>
              <a
                href={repositoryUrl}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-2 rounded-lg border bg-fd-card px-5 py-3 font-medium transition-colors hover:bg-fd-accent"
              >
                <GitHubIcon className="size-4" />
                GitHub
                {stars !== null ? (
                  <span className="flex items-center gap-1 border-l pl-2.5 text-sm text-fd-muted-foreground">
                    <Star className="size-3.5" />
                    {formatStars(stars)}
                  </span>
                ) : null}
              </a>
            </div>
          </div>
          <DesktopDemo lang={lang} />
          <div className="mx-auto mt-10 flex flex-wrap items-center justify-center gap-x-3 gap-y-2 font-mono text-xs text-fd-muted-foreground sm:text-sm">
            <span>{t.flow[0]}</span>
            <ArrowRight className="size-3.5 shrink-0" />
            <span className="font-medium text-fd-foreground">{t.flow[1]}</span>
            <ArrowRight className="size-3.5 shrink-0" />
            <span>{t.flow[2]}</span>
          </div>
        </div>
      </section>

      <section className="border-b px-6 py-16 sm:py-20">
        <div className="mx-auto max-w-5xl">
          <div className="flex items-end justify-between gap-6">
            <div>
              <p className="font-mono text-sm font-medium text-fd-primary">{t.docsEyebrow}</p>
              <h2 className="mt-3 text-3xl font-bold tracking-tight">{t.docsTitle}</h2>
            </div>
            <Link
              href={`${prefix}/docs`}
              className="hidden items-center gap-2 text-sm font-medium sm:flex"
            >
              {t.viewAll}
              <ArrowRight className="size-4" />
            </Link>
          </div>

          <div className="mt-10 grid gap-4 md:grid-cols-3">
            {t.docs.map(({ icon: Icon, title, description, href }) => (
              <Link
                key={href}
                href={`${prefix}${href}`}
                className="group rounded-xl border bg-fd-card p-6 transition-colors hover:bg-fd-accent"
              >
                <Icon className="mb-5 size-5 text-fd-primary" />
                <h3 className="flex items-center justify-between font-semibold">
                  {title}
                  <ArrowRight className="size-4 transition-transform group-hover:translate-x-1" />
                </h3>
                <p className="mt-2 text-sm leading-6 text-fd-muted-foreground">{description}</p>
              </Link>
            ))}
          </div>
        </div>
      </section>

      {posts.length > 0 ? (
        <section className="px-6 py-16 sm:py-20">
          <div className="mx-auto max-w-5xl">
            <div className="flex items-end justify-between gap-6">
              <div>
                <p className="font-mono text-sm font-medium text-fd-primary">{t.blogEyebrow}</p>
                <h2 className="mt-3 text-3xl font-bold tracking-tight">{t.blogTitle}</h2>
              </div>
              <Link href={`${prefix}/blog`} className="flex items-center gap-2 text-sm font-medium">
                {t.viewAll}
                <ArrowRight className="size-4" />
              </Link>
            </div>

            <div className="mt-10 divide-y border-y">
              {posts.map((post) => (
                <Link
                  key={post.url}
                  href={post.url}
                  className="group grid gap-3 py-6 transition-colors hover:text-fd-primary sm:grid-cols-[10rem_1fr_auto] sm:items-center"
                >
                  <time className="text-sm text-fd-muted-foreground">
                    {formatBlogDate(post.path, lang)}
                  </time>
                  <div>
                    <h3 className="font-semibold">{post.data.title}</h3>
                    <p className="mt-1 text-sm text-fd-muted-foreground">
                      {post.data.description}
                    </p>
                  </div>
                  <ArrowRight className="hidden size-4 transition-transform group-hover:translate-x-1 sm:block" />
                </Link>
              ))}
            </div>
          </div>
        </section>
      ) : null}
    </main>
  );
}

function GitHubIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden className={className}>
      <path d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0 1 12 6.844a9.59 9.59 0 0 1 2.504.337c1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.02 10.02 0 0 0 22 12.017C22 6.484 17.522 2 12 2Z" />
    </svg>
  );
}
