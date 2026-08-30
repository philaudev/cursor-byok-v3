'use client';

import Image from 'next/image';
import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { demoLocale, type Language } from '@/lib/i18n';
import styles from './DesktopDemo.module.css';

// 小屏或触屏设备内嵌交互体验差，只展示 poster 静态图
const embedQuery = '(min-width: 680px) and (hover: hover)';

const alt: Record<Language, string> = {
  zh: 'Cursor Byok 桌面端数据概览界面',
  en: 'Cursor Byok desktop overview dashboard',
};

function subscribeEmbedQuery(callback: () => void) {
  const media = window.matchMedia(embedQuery);
  media.addEventListener('change', callback);
  return () => media.removeEventListener('change', callback);
}

function subscribeTheme(callback: () => void) {
  const observer = new MutationObserver(callback);
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
  return () => observer.disconnect();
}

export function DesktopDemo({ lang }: { lang: Language }) {
  const stageRef = useRef<HTMLDivElement>(null);
  const [mounted, setMounted] = useState(false);
  const embeddable = useSyncExternalStore(
    subscribeEmbedQuery,
    () => window.matchMedia(embedQuery).matches,
    () => true,
  );
  const dark = useSyncExternalStore(
    subscribeTheme,
    () => document.documentElement.classList.contains('dark'),
    () => true,
  );

  useEffect(() => {
    if (!embeddable) return;
    const stage = stageRef.current;
    if (!stage) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setMounted(true);
          observer.disconnect();
        }
      },
      { rootMargin: '240px' },
    );
    observer.observe(stage);
    return () => observer.disconnect();
  }, [embeddable]);

  return (
    <div ref={stageRef} className={styles.stage}>
      <div className={styles.halo} aria-hidden />
      <DemoViewport
        // key 含挂载状态:视口变小卸载 iframe 时整体重置,poster 重新显示
        key={`${dark ? 'dark' : 'light'}-${lang}-${embeddable && mounted ? 'live' : 'poster'}`}
        dark={dark}
        lang={lang}
        mounted={embeddable && mounted}
      />
    </div>
  );
}

function DemoViewport({
  dark,
  lang,
  mounted,
}: {
  dark: boolean;
  lang: Language;
  mounted: boolean;
}) {
  const [frameLoaded, setFrameLoaded] = useState(false);
  const [revealed, setRevealed] = useState(false);
  const theme = dark ? 'default-dark' : 'default-light';
  const demoUrl = `/product-demo/demo/index.html?theme=${theme}&locale=${demoLocale[lang]}`;
  const posterUrl = `/images/product-demo-${dark ? 'dark' : 'light'}-${lang}.png`;

  useEffect(() => {
    if (!frameLoaded) return;
    // iframe onLoad 只代表文档加载完成，等内部应用渲染后再揭开 poster
    const timer = setTimeout(() => setRevealed(true), 900);
    return () => clearTimeout(timer);
  }, [frameLoaded]);

  return (
    <div className={styles.viewport} data-dark={dark || undefined}>
      {mounted ? (
        <iframe
          title={alt[lang]}
          src={demoUrl}
          className={styles.demo}
          onLoad={() => setFrameLoaded(true)}
        />
      ) : null}
      <Image
        src={posterUrl}
        alt={alt[lang]}
        fill
        priority
        unoptimized
        className={revealed ? `${styles.poster} ${styles.posterHidden}` : styles.poster}
      />
    </div>
  );
}
