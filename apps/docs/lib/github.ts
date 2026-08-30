import { gitConfig } from './shared';

export type RepoStats = {
  stars: number | null;
  version: string | null;
};

async function fetchJson(url: string): Promise<Record<string, unknown> | null> {
  try {
    // 构建期取一次并静态化;数据随每次部署更新,运行时无需 ISR 缓存设施
    const res = await fetch(url, {
      headers: { Accept: 'application/vnd.github+json' },
      cache: 'force-cache',
    });
    if (!res.ok) return null;
    return (await res.json()) as Record<string, unknown>;
  } catch {
    return null;
  }
}

export async function getRepoStats(): Promise<RepoStats> {
  const base = `https://api.github.com/repos/${gitConfig.user}/${gitConfig.repo}`;
  const [repo, release] = await Promise.all([
    fetchJson(base),
    fetchJson(`${base}/releases/latest`),
  ]);

  return {
    stars: typeof repo?.stargazers_count === 'number' ? repo.stargazers_count : null,
    version: typeof release?.tag_name === 'string' ? release.tag_name : null,
  };
}

export function formatStars(stars: number): string {
  return new Intl.NumberFormat('en', {
    notation: 'compact',
    maximumFractionDigits: 1,
  }).format(stars);
}
