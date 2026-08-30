import { defineCloudflareConfig } from '@opennextjs/cloudflare';

// 站点全部页面在构建期静态生成，无需增量缓存 / 队列等运行时缓存设施。
export default defineCloudflareConfig({});
