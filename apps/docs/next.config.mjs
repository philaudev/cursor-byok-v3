import { createMDX } from 'fumadocs-mdx/next';

const withMDX = createMDX();

/** @type {import('next').NextConfig} */
const config = {
  reactStrictMode: true,
  images: {
    // 站内图片均为已按展示尺寸截取的静态资源,直接由 CDN 提供,
    // 避免在 Cloudflare Workers 上依赖运行时图片优化服务。
    unoptimized: true,
  },
};

export default withMDX(config);
