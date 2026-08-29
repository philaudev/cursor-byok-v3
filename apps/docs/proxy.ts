import { NextRequest, NextResponse, type NextFetchEvent } from 'next/server';
import { isMarkdownPreferred, rewritePath } from 'fumadocs-core/negotiation';
import { createI18nMiddleware } from 'fumadocs-core/i18n/middleware';
import { i18n, type Language } from '@/lib/i18n';
import { docsContentRoute, docsRoute } from '@/lib/shared';

const { rewrite: rewriteDocs } = rewritePath(
  `${docsRoute}{/*path}`,
  `${docsContentRoute}{/*path}/content.md`,
);
const { rewrite: rewriteSuffix } = rewritePath(
  `${docsRoute}{/*path}.md`,
  `${docsContentRoute}{/*path}/content.md`,
);

const i18nMiddleware = createI18nMiddleware(i18n);

/** Split the locale prefix (if any) from a pathname. */
function splitLocale(pathname: string): { locale: Language; rest: string } {
  for (const lang of i18n.languages) {
    if (pathname === `/${lang}`) return { locale: lang, rest: '/' };
    if (pathname.startsWith(`/${lang}/`)) {
      return { locale: lang, rest: pathname.slice(lang.length + 1) };
    }
  }
  return { locale: i18n.defaultLanguage, rest: pathname };
}

export default function proxy(request: NextRequest, event: NextFetchEvent) {
  const { locale, rest } = splitLocale(request.nextUrl.pathname);

  const suffixTarget = rewriteSuffix(rest);
  if (suffixTarget) {
    return NextResponse.rewrite(new URL(`/${locale}${suffixTarget}`, request.nextUrl));
  }

  if (isMarkdownPreferred(request)) {
    const target = rewriteDocs(rest);

    if (target) {
      return NextResponse.rewrite(new URL(`/${locale}${target}`, request.nextUrl), {
        // this URL has two representations, selected by `Accept`
        headers: { Vary: 'Accept' },
      });
    }
  }

  return i18nMiddleware(request, event);
}

export const config = {
  matcher: ['/((?!api|_next|favicon.ico|icon.png|images/|product-demo/|llms\\.txt|llms-full\\.txt).*)'],
};
