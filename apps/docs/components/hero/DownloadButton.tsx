'use client';

import { useSyncExternalStore } from 'react';
import { Download } from 'lucide-react';
import type { Language } from '@/lib/i18n';
import { releaseUrl } from '@/lib/shared';

function detectPlatform(): string | null {
  const ua = navigator.userAgent;
  if (/Mac/i.test(ua)) return 'macOS';
  if (/Win/i.test(ua)) return 'Windows';
  if (/Linux|X11/i.test(ua)) return 'Linux';
  return null;
}

const labels: Record<Language, { platform: (platform: string) => string; fallback: string }> = {
  zh: { platform: (platform) => `下载 ${platform} 版`, fallback: '下载最新版' },
  en: { platform: (platform) => `Download for ${platform}`, fallback: 'Download' },
};

const subscribeNoop = () => () => {};

export function DownloadButton({ lang }: { lang: Language }) {
  // 平台只在客户端可知；SSR 用通用文案，水合后替换为平台文案
  const platform = useSyncExternalStore(subscribeNoop, detectPlatform, () => null);
  const label = labels[lang];

  return (
    <a
      href={releaseUrl}
      className="inline-flex items-center gap-2 rounded-lg bg-fd-primary px-5 py-3 font-medium text-fd-primary-foreground transition-opacity hover:opacity-90"
    >
      <Download className="size-4" />
      {platform ? label.platform(platform) : label.fallback}
    </a>
  );
}
