# cursor-byok 文档站

基于 [Fumadocs](https://fumadocs.dev) 和 Next.js 的独立内容应用。

## 目录

```text
apps/docs/
├── app/
│   ├── (home)/        # Hero 首页与开发者博客
│   └── docs/          # 用户文档布局和页面
├── components/        # MDX 组件
├── content/
│   ├── docs/          # 用户文档
│   └── blog/          # 开发者博客
└── lib/               # 内容源、站点信息与布局配置
```

## 本地开发

先安装依赖：

```bash
npm --prefix apps/docs install
```

从仓库根目录启动：

```bash
make dev-docs
```

浏览器打开 <http://localhost:3000>。

## 修改内容

- 在 `content/docs` 中维护面向使用者的 `.mdx` 文档。
- 在 `content/docs/meta.json` 中维护文档侧边栏标题与页面顺序。
- 在 `content/blog` 中维护开发者文章，文件名以 `YYYY-MM-DD-` 开头用于排序和展示日期。
- 在 `lib/shared.ts` 中维护站点名称、仓库和发布地址。

## 检查与构建

```bash
npm --prefix apps/docs run check
```

生产构建输出由 Next.js 管理。部署环境可通过 `NEXT_PUBLIC_SITE_URL` 设置站点公开地址；未设置时使用 `https://docs.leokun.cn`。
